//! `shux api` and `shux version`.

use super::{args::*, rpc::*};

/// Handle the `shux rpc call <method> --params ...` command.
pub async fn handle_api(
    stream: &mut tokio::net::UnixStream,
    method: &str,
    params_str: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let params: serde_json::Value = serde_json::from_str(params_str)
        .map_err(|e| anyhow::anyhow!("Invalid JSON params: {e}"))?;

    // PR 3b: surface RPC errors as part of the JSON-RPC envelope on
    // stdout, not as a human-readable anyhow error on stderr. Callers
    // of `shux rpc call` are debug tools / agents that expect to parse the
    // raw `{result | error}` shape — including bounded `data` fields
    // like `expected_version` / `actual_version` for retry loops.
    match rpc_call(stream, method, params).await {
        Ok(result) => {
            let envelope = serde_json::json!({"result": result});
            match format {
                OutputFormat::Json | OutputFormat::Text | OutputFormat::Plain => {
                    println!(
                        "{}",
                        crate::style::json_safe(&serde_json::to_string_pretty(&envelope)?)
                    );
                }
            }
            Ok(())
        }
        Err(RpcClientError::Rpc {
            code,
            message,
            data,
        }) => {
            let mut err_obj = serde_json::json!({
                "code": code,
                "message": message,
            });
            if let Some(d) = data {
                err_obj["data"] = d;
            }
            let envelope = serde_json::json!({"error": err_obj});
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&envelope)?)
            );
            // Non-zero exit so shell pipelines can branch, but the
            // structured error is still on stdout for parsers.
            std::process::exit(2);
        }
        Err(other) => Err(other.into()),
    }
}

/// Report a command refused because the daemon is another build that may hold
/// sessions, and exit [`crate::client::DAEMON_IN_USE_EXIT`]. With
/// `--format json` the refusal is the JSON document on stdout.
pub fn exit_daemon_in_use(refusal: &crate::client::DaemonInUse, format: OutputFormat) -> ! {
    use std::io::Write as _;
    match format {
        OutputFormat::Json => match serde_json::to_string_pretty(&refusal.to_json()) {
            Ok(s) => println!("{}", crate::style::json_safe(&s)),
            Err(e) => crate::style::print_error(&format!("{refusal} ({e})")),
        },
        OutputFormat::Text | OutputFormat::Plain => {
            crate::style::print_error(&refusal.to_string());
        }
    }
    let _ = std::io::stdout().flush();
    std::process::exit(crate::client::DAEMON_IN_USE_EXIT)
}

/// Handle the `shux version` command: this CLI's build, and the daemon's
/// beside it when one serves `socket_path`. Never starts a daemon.
pub async fn handle_version(
    socket_path: &std::path::Path,
    format: OutputFormat,
) -> anyhow::Result<()> {
    use crate::client::DaemonAnswer;

    let cli = (env!("CARGO_PKG_VERSION"), shux_rpc::GIT_SHA);
    // `daemon`: `None` when no daemon serves the socket, `Err` when one is
    // there but did not say which build. `mismatch`: whether it is not this
    // build, as far as it said.
    let (daemon, mismatch) = match crate::client::ask_daemon_version(socket_path).await {
        DaemonAnswer::Absent => (None, None),
        DaemonAnswer::Answered(dv) => {
            let stale = !dv.is_current();
            (Some(Ok(dv)), Some(stale))
        }
        DaemonAnswer::Unreadable => (
            Some(Err((
                "unreadable",
                "it did not say which build it is".to_string(),
            ))),
            Some(true),
        ),
        DaemonAnswer::Unresponsive => (
            Some(Err((
                "unresponsive",
                format!(
                    "it did not answer within {:?}; it may be busy, stopped or wedged",
                    crate::patience::peer_patience()
                ),
            ))),
            None,
        ),
        DaemonAnswer::Unreachable(e) => (Some(Err(("unreachable", e.to_string()))), None),
    };

    match format {
        OutputFormat::Json => {
            let daemon = match &daemon {
                None => serde_json::Value::Null,
                Some(Ok(dv)) => serde_json::json!({
                    "status": "running",
                    "version": dv.version,
                    "git_sha": dv.git_sha,
                }),
                Some(Err((status, error))) => serde_json::json!({
                    "status": status,
                    "version": null,
                    "git_sha": null,
                    "error": error,
                }),
            };
            let envelope = serde_json::json!({
                "name": "shux",
                "version": cli.0,
                "git_sha": cli.1,
                "cli": {"version": cli.0, "git_sha": cli.1},
                "daemon": daemon,
                "mismatch": mismatch,
            });
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&envelope)?)
            );
        }
        OutputFormat::Plain => {
            let (status, found) = match &daemon {
                None => ("not-running", None),
                Some(Ok(dv)) => ("running", Some((dv.version.as_str(), dv.git_sha.as_str()))),
                Some(Err((status, _))) => (*status, None),
            };
            crate::style::print_version_plain(cli, status, found, mismatch)?;
        }
        OutputFormat::Text => {
            crate::style::print_version(
                cli.0,
                Some(cli.1),
                daemon.is_none().then_some("daemon not running"),
            )?;
            // What the next command does, as `client::ensure_daemon_running_at`
            // decides it: only an empty shux daemon of another build is replaced.
            let next = if mismatch != Some(true) {
                String::new()
            } else if crate::daemon_boot::our_live_daemon(socket_path).is_none() {
                "no shux daemon process serves this socket, so shux commands \
                 refuse to run until whatever serves it is stopped"
                    .to_string()
            } else {
                let held = match &daemon {
                    Some(Ok(_)) => crate::client::count_sessions(socket_path).await,
                    _ => None,
                };
                let matching = match &daemon {
                    Some(Ok(dv)) => format!("the shux {} ({}) CLI", dv.version, dv.git_sha),
                    _ => "the shux CLI that started it".to_string(),
                };
                match held {
                    Some(0) => "it holds no sessions, so the next shux command replaces it \
                                with this build"
                        .to_string(),
                    held => format!(
                        "{}, so shux commands refuse to run against it: `shux daemon stop` \
                         ends them and starts this build, or use {matching}",
                        match held {
                            Some(1) => "it holds 1 session".to_string(),
                            Some(n) => format!("it holds {n} sessions"),
                            None => "it may hold sessions".to_string(),
                        }
                    ),
                }
            };
            let next = || next.as_str();
            match &daemon {
                None => {}
                Some(Ok(dv)) => {
                    let note = (mismatch == Some(true))
                        .then(|| format!("differs from this CLI; {}", next()));
                    crate::style::print_daemon_version(
                        Some((&dv.version, &dv.git_sha)),
                        note.as_deref(),
                    )?;
                }
                Some(Err((_, error))) => {
                    let note = if mismatch == Some(true) {
                        format!("{error}; {}", next())
                    } else {
                        error.clone()
                    };
                    crate::style::print_daemon_version(None, Some(&note))?;
                }
            }
        }
    }
    Ok(())
}
