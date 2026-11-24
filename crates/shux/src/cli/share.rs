//! `shux share`: a read-only link to a session. Mirrors `share.*` RPC.
//!
//! Inside a shux pane, any pane, the link never reaches the screen: it goes
//! to the clipboard through a local command that writes no terminal bytes,
//! or the daemon refuses before minting it.

use std::path::PathBuf;
use std::time::Duration;

use tokio::io::AsyncWriteExt as _;

use crate::cli::rpc::RpcClientError;
use crate::cli::{OutputFormat, ShareCreateArgs, rpc_call, to_style_format};
use crate::style;

/// How long the clipboard may take before the link is revoked instead.
const COPY_WAIT: Duration = Duration::from_secs(10);

/// The pane this runs in, as `SHUX_PANE_ID` says: set at all, even to an id
/// no daemon knows, means inside shux.
pub fn caller_pane() -> Option<String> {
    std::env::var_os("SHUX_PANE_ID")
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string_lossy().into_owned())
}

/// A refusal made here, shaped as the daemon's are, for `--format json`.
fn refused(reason: &str, detail: &str) -> anyhow::Error {
    RpcClientError::Rpc {
        code: shux_rpc::ErrorCode::InvalidParams.code(),
        message: "invalid_params".into(),
        data: Some(serde_json::json!({ "reason": reason, "detail": detail })),
    }
    .into()
}

/// A clipboard command that takes the link on stdin and draws nothing.
fn clipboard() -> Option<(PathBuf, Vec<&'static str>)> {
    let on_path = |name: &str| {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    };
    if cfg!(target_os = "macos") {
        return on_path("pbcopy").map(|p| (p, vec![]));
    }
    if std::env::var_os("WAYLAND_DISPLAY").is_some()
        && let Some(p) = on_path("wl-copy")
    {
        return Some((p, vec![]));
    }
    if std::env::var_os("DISPLAY").is_some() {
        if let Some(p) = on_path("xclip") {
            return Some((p, vec!["-selection", "clipboard"]));
        }
        if let Some(p) = on_path("xsel") {
            return Some((p, vec!["--clipboard", "--input"]));
        }
    }
    None
}

async fn copy(cmd: &(PathBuf, Vec<&'static str>), text: &str) -> anyhow::Result<()> {
    let mut child = tokio::process::Command::new(&cmd.0)
        .args(&cmd.1)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes()).await?;
    }
    let status = child.wait().await?;
    anyhow::ensure!(status.success(), "{} exited with {status}", cmd.0.display());
    Ok(())
}

/// Copies `url` within [`COPY_WAIT`], unless Ctrl-C or a SIGTERM comes first.
async fn copy_or_give_up(
    clip: &(PathBuf, Vec<&'static str>),
    url: &str,
    interrupt: &mut tokio::signal::unix::Signal,
    terminate: &mut tokio::signal::unix::Signal,
) -> anyhow::Result<()> {
    tokio::select! {
        copied = tokio::time::timeout(COPY_WAIT, copy(clip, url)) => copied.unwrap_or_else(|_| {
            Err(anyhow::anyhow!("the clipboard took more than {}s", COPY_WAIT.as_secs()))
        }),
        _ = interrupt.recv() => Err(anyhow::anyhow!("interrupted")),
        _ = terminate.recv() => Err(anyhow::anyhow!("terminated")),
    }
}

fn print_json(v: &serde_json::Value) -> anyhow::Result<()> {
    println!("{}", style::json_safe(&serde_json::to_string_pretty(v)?));
    Ok(())
}

pub async fn handle_share_create(
    stream: &mut tokio::net::UnixStream,
    args: ShareCreateArgs,
    format: OutputFormat,
) -> anyhow::Result<()> {
    if args.qr && matches!(format, OutputFormat::Json) {
        return Err(refused(
            "qr_json",
            "--qr draws the link as text; it has no place in --format json",
        ));
    }
    let from_pane = caller_pane();
    let clip = from_pane.as_ref().and_then(|_| clipboard());
    // Ready before the link exists, so a Ctrl-C while it is being copied
    // revokes it instead of leaving it live and undelivered.
    let mut signals = match clip {
        Some(_) => {
            use tokio::signal::unix::{SignalKind, signal};
            Some((
                signal(SignalKind::interrupt())?,
                signal(SignalKind::terminate())?,
            ))
        }
        None => None,
    };
    let create = rpc_call(
        stream,
        "share.create",
        serde_json::json!({
            "session": args.session,
            "from_pane": from_pane,
            "ttl_secs": args.for_,
            "uses": args.uses,
            "label": args.label,
            "can_copy": clip.is_some(),
            "qr": args.qr,
        }),
    );
    // Interrupted before the link exists, the call is dropped: the daemon
    // ends a tunnel startup whose caller has gone.
    let result = match signals.as_mut() {
        Some((interrupt, terminate)) => tokio::select! {
            r = create => r?,
            _ = interrupt.recv() => anyhow::bail!("interrupted"),
            _ = terminate.recv() => anyhow::bail!("terminated"),
        },
        None => create.await?,
    };
    let url = result
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("share.create answered without a link"))?
        .to_string();
    let id = result
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if result.get("deliver").and_then(|v| v.as_str()) == Some("clipboard") {
        let (Some(clip), Some((interrupt, terminate))) = (clip, signals.as_mut()) else {
            anyhow::bail!("the daemon chose the clipboard, and there is none");
        };
        if let Err(e) = copy_or_give_up(&clip, &url, interrupt, terminate).await {
            // A link nobody holds is no use to anyone: end it.
            let revoked = rpc_call(stream, "share.revoke", serde_json::json!({ "id": id })).await;
            return Err(match revoked {
                Ok(_) => e.context("copying the link to the clipboard; the link was revoked"),
                Err(r) => e.context(format!(
                    "copying the link to the clipboard; revoking it failed too ({r}): \
                     run `shux share revoke {id}`"
                )),
            });
        }
        let mut shown = result.clone();
        shown["url"] = serde_json::Value::Null;
        if matches!(format, OutputFormat::Json) {
            return print_json(&shown);
        }
        return Ok(style::print_share_copied(&shown)?);
    }
    if matches!(format, OutputFormat::Json) {
        return print_json(&result);
    }
    match style::TerminalContext::detect(to_style_format(format)).format {
        style::OutputFormat::Plain => style::print_plain_line(&url)?,
        _ => style::print_share_created(&result)?,
    }
    if args.qr {
        style::print_share_qr(&url)?;
    }
    Ok(())
}

pub async fn handle_share_list(
    stream: &mut tokio::net::UnixStream,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let result = rpc_call(stream, "share.list", serde_json::json!({})).await?;
    if matches!(format, OutputFormat::Json) {
        return print_json(&result);
    }
    let links = result
        .get("links")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    match style::TerminalContext::detect(to_style_format(format)).format {
        style::OutputFormat::Plain => style::print_share_list_plain(&links)?,
        _ => style::print_share_list(&links, result.get("tunnel").and_then(|v| v.as_str()))?,
    }
    Ok(())
}

pub async fn handle_share_revoke(
    stream: &mut tokio::net::UnixStream,
    id: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let result = rpc_call(stream, "share.revoke", serde_json::json!({ "id": id })).await?;
    if matches!(format, OutputFormat::Json) {
        return print_json(&result);
    }
    let revoked: Vec<String> = result
        .get("revoked")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    match style::TerminalContext::detect(to_style_format(format)).format {
        style::OutputFormat::Plain => {
            for r in &revoked {
                style::print_plain_line(r)?;
            }
        }
        _ => style::print_share_revoked(&revoked)?,
    }
    Ok(())
}

pub async fn handle_share_kick(
    stream: &mut tokio::net::UnixStream,
    viewer: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let result = rpc_call(
        stream,
        "share.kick",
        serde_json::json!({ "viewer": viewer }),
    )
    .await?;
    if matches!(format, OutputFormat::Json) {
        return print_json(&result);
    }
    let kicked = result
        .get("kicked")
        .and_then(|v| v.as_str())
        .unwrap_or(viewer);
    match style::TerminalContext::detect(to_style_format(format)).format {
        style::OutputFormat::Plain => style::print_plain_line(kicked)?,
        _ => style::print_success("dropped viewer", kicked, None)?,
    }
    Ok(())
}

pub async fn handle_share_stop(
    stream: &mut tokio::net::UnixStream,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let result = rpc_call(stream, "share.stop", serde_json::json!({})).await?;
    if matches!(format, OutputFormat::Json) {
        return print_json(&result);
    }
    let n = result
        .get("links_ended")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    match style::TerminalContext::detect(to_style_format(format)).format {
        style::OutputFormat::Plain => style::print_plain_line(&n.to_string())?,
        _ => style::print_share_stopped(n)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_bad_duration_names_the_forms_it_takes() {
        for bad in ["soon", "5x", "1.5h"] {
            let err = crate::cli::parse_share_duration(bad).unwrap_err();
            assert!(err.contains("30m") && err.contains("1d"), "{bad}: {err}");
        }
        assert_eq!(crate::cli::parse_share_duration("1d"), Ok(86_400));
        assert_eq!(crate::cli::parse_share_duration("90"), Ok(90));
    }
}
