//! `shux pane …` handlers.

use crate::style;

use super::{args::*, resolve::*, rpc::*};

/// Handle the `shux pane list` command.
pub async fn handle_pane_list(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let (session_id, window_id) = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let result = rpc_call(
        stream,
        "pane.list",
        serde_json::json!({"session_id": session_id, "window_id": window_id}),
    )
    .await?;

    // Resolve the window title for the header
    let window_title = {
        let win_result = rpc_call(
            stream,
            "window.list",
            serde_json::json!({"session_id": session_id}),
        )
        .await
        .ok();
        win_result
            .and_then(|r| {
                r.as_array().and_then(|windows| {
                    windows.iter().find_map(|w| {
                        let wid = w.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        if wid == window_id {
                            w.get("title").and_then(|v| v.as_str()).map(String::from)
                        } else {
                            None
                        }
                    })
                })
            })
            .unwrap_or_else(|| window_id.chars().take(8).collect())
    };

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
            );
        }
        OutputFormat::Text | OutputFormat::Plain => {
            let ctx = style::TerminalContext::detect(to_style_format(format));

            let pane_infos: Vec<style::PaneInfo> = result
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|p| {
                            let id = p
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?")
                                .to_string();
                            let cwd = p
                                .get("cwd")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            // The pane's title — what its border draws, and
                            // the only handle an operator has on it. It was
                            // read here for `--format json` and nowhere else,
                            // so neither human format ever named a pane
                            // (issue #135).
                            let title = p
                                .get("title")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            // `pane.list` returns `command` as a JSON
                            // ARRAY; reading it with `as_str()` made this
                            // column permanently blank (issue #104
                            // adversarial review). The argv is handed on
                            // whole — the renderer quotes it, so both human
                            // formats show the same argument boundaries
                            // (issue #135).
                            let command = p
                                .get("command")
                                .and_then(|v| v.as_array())
                                .map(|a| {
                                    a.iter()
                                        .filter_map(|x| x.as_str())
                                        .map(String::from)
                                        .collect::<Vec<_>>()
                                })
                                .unwrap_or_default();
                            let is_focused = p
                                .get("is_focused")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            let is_zoomed = p
                                .get("is_zoomed")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            style::PaneInfo {
                                id,
                                title,
                                cwd,
                                command,
                                is_focused,
                                is_zoomed,
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();

            style::render_pane_list(&ctx, session_name, &window_title, &pane_infos);
        }
    }

    Ok(())
}

/// Handle the `shux pane split` command.
#[allow(clippy::too_many_arguments)]
pub async fn handle_pane_split(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_spec: Option<&str>,
    direction: Option<&str>,
    ratio: Option<f64>,
    cmd: Option<String>,
    argv: Vec<String>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let (session_id, window_id) = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let mut params = serde_json::json!({
        "session_id": session_id,
        "window_id": window_id,
    });

    if let Some(pid) = pane_spec {
        params["pane_id"] = serde_json::Value::String(pid.to_string());
    }
    if let Some(dir) = direction {
        params["direction"] = serde_json::Value::String(dir.to_string());
    }
    if let Some(r) = ratio {
        params["ratio"] = serde_json::json!(r);
    }
    // Same two forms and the same precedence as `session create` /
    // `window create`: trailing argv is exec'd, `--cmd` is a shell command.
    // `pane.split` has always accepted `command`; only the CLI had no way to
    // say it (issue #125 follow-up).
    if !argv.is_empty() {
        params["command"] =
            serde_json::Value::Array(argv.into_iter().map(serde_json::Value::String).collect());
    } else if let Some(c) = cmd {
        params["command"] = serde_json::Value::String(c);
    }

    let result = rpc_call(stream, "pane.split", params).await?;

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
            );
        }
        OutputFormat::Text | OutputFormat::Plain => {
            let pane_id = result
                .get("pane")
                .and_then(|v| v.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let dir_label = direction.unwrap_or("vertical");
            crate::style::print_pane_split(pane_id, dir_label);
        }
    }

    Ok(())
}

/// Handle the `shux pane focus` command.
pub async fn handle_pane_focus(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_id: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    // Resolve window for validation, but pane.focus only needs pane_id
    let _ = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let result = rpc_call(
        stream,
        "pane.focus",
        serde_json::json!({"pane_id": pane_id}),
    )
    .await?;

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
            );
        }
        OutputFormat::Text | OutputFormat::Plain => {
            crate::style::print_pane_focused(pane_id);
        }
    }

    Ok(())
}

/// Handle the `shux pane focus-dir` command.
pub async fn handle_pane_focus_dir(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    direction: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let (session_id, window_id) = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let result = rpc_call(
        stream,
        "pane.focus_direction",
        serde_json::json!({
            "session_id": session_id,
            "window_id": window_id,
            "direction": direction,
        }),
    )
    .await?;

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
            );
        }
        OutputFormat::Text | OutputFormat::Plain => {
            let pane_id = result
                .get("pane_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            crate::style::print_pane_focused(pane_id);
        }
    }

    Ok(())
}

/// Handle the `shux pane resize` command.
#[allow(clippy::too_many_arguments)]
pub async fn handle_pane_resize(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_spec: Option<&str>,
    direction: &str,
    delta: Option<f64>,
    expected_version: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let (session_id, window_id) = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let mut params = serde_json::json!({
        "session_id": session_id,
        "window_id": window_id,
        "direction": direction,
    });

    if let Some(pid) = pane_spec {
        params["pane_id"] = serde_json::Value::String(pid.to_string());
    }
    if let Some(d) = delta {
        params["delta"] = serde_json::json!(d);
    }
    if let Some(ev) = expected_version {
        params["expected_version"] = serde_json::Value::from(ev);
    }

    let result = rpc_call(stream, "pane.resize", params).await?;

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
            );
        }
        OutputFormat::Text | OutputFormat::Plain => {
            let pane_id = result
                .get("pane_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            crate::style::print_pane_resized(pane_id);
        }
    }

    Ok(())
}

/// Handle the `shux pane zoom` command.
pub async fn handle_pane_zoom(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_spec: Option<&str>,
    expected_version: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let (session_id, window_id) = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let mut params = serde_json::json!({
        "session_id": session_id,
        "window_id": window_id,
    });

    if let Some(pid) = pane_spec {
        params["pane_id"] = serde_json::Value::String(pid.to_string());
    }
    if let Some(ev) = expected_version {
        params["expected_version"] = serde_json::Value::from(ev);
    }

    let result = rpc_call(stream, "pane.zoom", params).await?;

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
            );
        }
        OutputFormat::Text | OutputFormat::Plain => {
            let pane_id = result
                .get("pane_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let is_zoomed = result
                .get("is_zoomed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            crate::style::print_pane_zoomed(pane_id, is_zoomed);
        }
    }

    Ok(())
}

/// Handle the `shux pane swap` command.
pub async fn handle_pane_swap(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_id: &str,
    target_id: &str,
    expected_version: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    // Resolve window for validation
    let _ = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let mut params = serde_json::Map::new();
    params.insert(
        "pane_id".to_string(),
        serde_json::Value::String(pane_id.to_string()),
    );
    params.insert(
        "target_pane_id".to_string(),
        serde_json::Value::String(target_id.to_string()),
    );
    if let Some(ev) = expected_version {
        params.insert("expected_version".to_string(), serde_json::Value::from(ev));
    }
    let result = rpc_call(stream, "pane.swap", serde_json::Value::Object(params)).await?;

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
            );
        }
        OutputFormat::Text | OutputFormat::Plain => {
            crate::style::print_pane_swapped(pane_id, target_id);
        }
    }

    Ok(())
}

/// Handle `shux pane title` — set or clear a pane title.
///
/// `--title "..."` sets a manual override; `--clear` removes it.
/// `--auto` / `--no-auto` toggle whether OSC + command-derived
/// titles flow into the displayed title (orthogonal to the manual
/// override, so you can pin auto OFF without clearing your manual
/// title).
#[allow(clippy::too_many_arguments)]
pub async fn handle_pane_title(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_spec: Option<&str>,
    title: Option<&str>,
    clear: bool,
    auto: bool,
    no_auto: bool,
    format: OutputFormat,
) -> anyhow::Result<()> {
    if title.is_some() && clear {
        anyhow::bail!("--title and --clear are mutually exclusive");
    }
    if auto && no_auto {
        anyhow::bail!("--auto and --no-auto are mutually exclusive");
    }

    let (session_id, window_id) = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let mut params = serde_json::json!({
        "session_id": session_id,
        "window_id": window_id,
    });
    if let Some(pid) = pane_spec {
        params["pane_id"] = serde_json::Value::String(pid.to_string());
    }
    // Title intent: explicit `null` clears, string sets, omitted leaves
    // manual_title unchanged. clap can't directly emit that tri-state
    // for us — we synthesize it here.
    if clear {
        params["title"] = serde_json::Value::Null;
    } else if let Some(t) = title {
        params["title"] = serde_json::Value::String(t.to_string());
    }
    if auto {
        params["auto"] = serde_json::Value::Bool(true);
    } else if no_auto {
        params["auto"] = serde_json::Value::Bool(false);
    }

    let result = rpc_call(stream, "pane.set_title", params).await?;

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
            );
        }
        OutputFormat::Text | OutputFormat::Plain => {
            let pid = result
                .get("pane_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let displayed = result.get("title").and_then(|v| v.as_str()).unwrap_or("");
            crate::style::print_pane_title_set(pid, displayed);
        }
    }

    Ok(())
}

/// Handle `shux pane watch` — long-poll `pane.output.watch` and write
/// each chunk's bytes to stdout. Pipes cleanly into `tee log` etc.
/// PR 2c / data-plane consumer.
pub async fn handle_pane_watch(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    pane_id: &str,
    timeout_ms: u64,
    limit: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    use base64::Engine;
    use std::io::Write;

    // Validate the reference early so we fail fast on typos instead of
    // round-tripping to the daemon. `--session` is documented as validating
    // that the pane belongs to a live session, so actually do that — and take
    // the canonical id back, so the long-poll below is keyed on the full uuid
    // rather than whatever fragment the caller typed.
    shux_core::idref::parse_ref(shux_core::idref::RefKind::Pane, pane_id)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let pane_id = &validate_pane_belongs_to_session(stream, session_name, pane_id).await?;

    let mut next_seq: Option<u64> = None;
    let mut delivered: u64 = 0;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut warned_sampled = false;

    loop {
        let mut params = serde_json::json!({
            "pane_id": pane_id,
            "timeout_ms": timeout_ms,
            // 50 chunks per poll is plenty given the 10/s/pane source
            // rate; smaller bounds just mean more RPC round-trips.
            "limit": 50,
        });
        if let Some(s) = next_seq {
            params["from_seq"] = serde_json::json!(s);
        }
        let resp = rpc_call(stream, "pane.output.watch", params).await?;

        if let Some(arr) = resp.get("chunks").and_then(|v| v.as_array()) {
            for chunk in arr {
                let bytes_b64 = chunk.get("bytes").and_then(|v| v.as_str()).unwrap_or("");
                let sampled = chunk
                    .get("sampled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                match format {
                    OutputFormat::Json => {
                        let _ = writeln!(out, "{}", serde_json::to_string(chunk)?);
                    }
                    OutputFormat::Text | OutputFormat::Plain => {
                        if sampled && !warned_sampled {
                            eprintln!(
                                "{} sampled pane.output chunk — bytes were dropped before this chunk; use `shux pane record --to FILE` for lossless audits",
                                crate::style::warning("!"),
                            );
                            warned_sampled = true;
                        } else if !sampled {
                            warned_sampled = false;
                        }
                        if let Ok(raw) =
                            base64::engine::general_purpose::STANDARD.decode(bytes_b64.as_bytes())
                        {
                            let _ = out.write_all(&raw);
                        }
                    }
                }
                delivered += 1;
                if let Some(lim) = limit
                    && delivered >= lim
                {
                    let _ = out.flush();
                    return Ok(());
                }
            }
            let _ = out.flush();
        }
        if let Some(s) = resp.get("next_seq").and_then(|v| v.as_u64()) {
            next_seq = Some(s);
        }
        // `lagged`: surface to stderr so pipes stay clean.
        if resp
            .get("lagged")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            eprintln!(
                "{} subscriber lagged behind data plane — some chunks dropped",
                crate::style::warning("!"),
            );
        }
    }
}

/// Handle `shux pane record` — start a daemon-side lossless recorder, wait for
/// a bounded duration or Ctrl-C, then stop and report the byte count.
pub async fn handle_pane_record(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    pane_id: &str,
    to: &std::path::Path,
    force: bool,
    duration_ms: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    // Validate the reference early so typos don't create files. Resolving it
    // here also means the recorder is started against the canonical id rather
    // than whatever fragment the caller typed.
    shux_core::idref::parse_ref(shux_core::idref::RefKind::Pane, pane_id)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let pane_id = &validate_pane_belongs_to_session(stream, session_name, pane_id).await?;

    let path = if to.is_absolute() {
        to.to_path_buf()
    } else {
        std::env::current_dir()?.join(to)
    };

    let start = rpc_call(
        stream,
        "pane.record.start",
        serde_json::json!({
            "pane_id": pane_id,
            "path": path,
            "overwrite": force,
            "duration_ms": duration_ms,
        }),
    )
    .await?;
    let recording_id = start
        .get("recording_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("daemon did not return recording_id"))?
        .to_string();

    match duration_ms {
        Some(ms) => tokio::time::sleep(std::time::Duration::from_millis(ms)).await,
        None => {
            eprintln!(
                "{} recording lossless pane output; press Ctrl-C to stop",
                crate::style::muted("..."),
            );
            tokio::signal::ctrl_c().await?;
        }
    }

    let stopped = rpc_call(
        stream,
        "pane.record.stop",
        serde_json::json!({
            "recording_id": recording_id,
        }),
    )
    .await?;

    match format {
        OutputFormat::Json => println!(
            "{}",
            crate::style::json_safe(&serde_json::to_string_pretty(&stopped)?)
        ),
        OutputFormat::Plain => {
            let path = stopped.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            let bytes = stopped
                .get("bytes_written")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let status = stopped
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            println!("recording\t{status}\t{path}\t{bytes}");
        }
        OutputFormat::Text => {
            let path = stopped.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            let bytes = stopped
                .get("bytes_written")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let status = stopped
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            println!(
                "{} {} bytes to {} ({})",
                crate::style::success("✓ recorded"),
                crate::style::bold(&bytes.to_string()),
                crate::style::muted(path),
                status,
            );
        }
    }

    Ok(())
}

/// Handle the `shux pane kill` command.
pub async fn handle_pane_kill(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_id: &str,
    expected_version: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    // Resolve window for validation
    let _ = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let mut params = serde_json::Map::new();
    params.insert(
        "pane_id".to_string(),
        serde_json::Value::String(pane_id.to_string()),
    );
    if let Some(ev) = expected_version {
        params.insert("expected_version".to_string(), serde_json::Value::from(ev));
    }
    let result = rpc_call(stream, "pane.kill", serde_json::Value::Object(params)).await?;

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
            );
        }
        OutputFormat::Text | OutputFormat::Plain => {
            crate::style::print_pane_killed(pane_id);
        }
    }

    Ok(())
}

/// Handle the `shux pane send-keys` command.
pub async fn handle_pane_send_keys(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_spec: Option<&str>,
    text: Option<&str>,
    data: Option<&str>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let (session_id, window_id) = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let mut params = serde_json::json!({
        "session_id": session_id,
        "window_id": window_id,
    });

    if let Some(pid) = pane_spec {
        params["pane_id"] = serde_json::Value::String(pid.to_string());
    }

    if let Some(t) = text {
        params["text"] = serde_json::Value::String(t.to_string());
    } else if let Some(d) = data {
        params["data"] = serde_json::Value::String(d.to_string());
    } else {
        anyhow::bail!("either --text or --data must be provided");
    }

    let result = rpc_call(stream, "pane.send_keys", params).await?;

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
            );
        }
        OutputFormat::Text | OutputFormat::Plain => {
            let bytes = result
                .get("bytes_written")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let pane_id = result
                .get("pane_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            crate::style::print_send_keys(pane_id, bytes);
        }
    }

    Ok(())
}

/// Handle the `shux pane run` command.
#[allow(clippy::too_many_arguments)]
pub async fn handle_pane_run(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_spec: Option<&str>,
    command: &str,
    timeout: u64,
    is_async: bool,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let (session_id, window_id) = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let mut params = serde_json::json!({
        "session_id": session_id,
        "window_id": window_id,
        "command": command,
        "timeout": timeout,
        "async": is_async,
    });

    if let Some(pid) = pane_spec {
        params["pane_id"] = serde_json::Value::String(pid.to_string());
    }

    let result = rpc_call(stream, "pane.run_command", params).await?;

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
            );
        }
        OutputFormat::Text | OutputFormat::Plain => {
            crate::style::print_run_command(&result, is_async);
        }
    }

    Ok(())
}

/// Handle the `shux pane capture` command.
/// `shux pane snapshot` — rasterize a single pane (no chrome) via
/// `pane.snapshot` RPC. Snapshot dimensions come from the pane's
/// current VT grid size; use `pane.set_size` first to change them.
pub async fn handle_pane_snapshot(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_spec: Option<&str>,
    output: Option<std::path::PathBuf>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    use base64::Engine;

    let (session_id, window_id) = resolve_pane_window_id(stream, session_name, window_spec).await?;
    let mut params = serde_json::json!({
        "session_id": session_id,
        "window_id": window_id,
    });
    if let Some(pid) = pane_spec {
        params["pane_id"] = serde_json::Value::String(pid.to_string());
    }

    let result = rpc_call(stream, "pane.snapshot", params).await?;
    let b64 = result
        .get("png_base64")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("daemon response missing png_base64"))?;
    let png = base64::engine::general_purpose::STANDARD.decode(b64)?;

    match (output, format) {
        (Some(path), _) => {
            std::fs::write(&path, &png)?;
            if !matches!(format, OutputFormat::Json) {
                let w = result.get("width").and_then(|v| v.as_u64()).unwrap_or(0);
                let h = result.get("height").and_then(|v| v.as_u64()).unwrap_or(0);
                println!(
                    "{} {} ({}×{} px, {} bytes)",
                    crate::style::success("✓ snapshot →"),
                    crate::style::bold(path.display().to_string().as_str()),
                    w,
                    h,
                    png.len(),
                );
            } else {
                println!(
                    "{}",
                    crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
                );
            }
        }
        (None, OutputFormat::Json) => {
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
            );
        }
        (None, _) => {
            println!("{b64}");
        }
    }
    Ok(())
}

/// `shux pane set-size` — call `pane.set_size` RPC with absolute dims.
pub async fn handle_pane_set_size(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_spec: Option<&str>,
    cols: u16,
    rows: u16,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let (session_id, window_id) = resolve_pane_window_id(stream, session_name, window_spec).await?;
    let mut params = serde_json::json!({
        "session_id": session_id,
        "window_id": window_id,
        "cols": cols,
        "rows": rows,
    });
    if let Some(pid) = pane_spec {
        params["pane_id"] = serde_json::Value::String(pid.to_string());
    }

    let result = rpc_call(stream, "pane.set_size", params).await?;

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
            );
        }
        OutputFormat::Text | OutputFormat::Plain => {
            println!(
                "{} pane resized to {}×{}",
                crate::style::success("✓"),
                cols,
                rows,
            );
        }
    }
    Ok(())
}

pub async fn handle_pane_capture(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_spec: Option<&str>,
    lines: u64,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let (session_id, window_id) = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let mut params = serde_json::json!({
        "session_id": session_id,
        "window_id": window_id,
        "lines": lines,
    });

    if let Some(pid) = pane_spec {
        params["pane_id"] = serde_json::Value::String(pid.to_string());
    }

    let result = rpc_call(stream, "pane.capture", params).await?;

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&result)?)
            );
        }
        OutputFormat::Text | OutputFormat::Plain => {
            let text = result.get("text").and_then(|v| v.as_str()).unwrap_or("");
            print!("{text}");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::session::*;
    use crate::cli::test_support::*;

    #[tokio::test]
    async fn cli_pane_io_and_snapshot_handlers_forward_payloads() {
        let sid = "11111111-1111-4111-8111-111111111111";
        let wid = "22222222-2222-4222-8222-222222222222";
        let pane = "33333333-3333-4333-8333-333333333333";
        let session = || session_list_response(sid, wid);
        let windows = || window_list_response(wid, pane);
        let png_b64 = "iVBORw0KGgo=";
        let record_dir = tempfile::tempdir().unwrap();
        let record_path = record_dir.path().join("record.raw");
        let (mut client, requests, task) = spawn_rpc_script(vec![
            session(),
            windows(),
            serde_json::json!({"pane_id": pane, "bytes_written": 5}),
            session(),
            windows(),
            serde_json::json!({"command_id": "cmd-1", "state": "running"}),
            session(),
            windows(),
            serde_json::json!({"pane_id": pane, "text": "ready\n", "lines": 1}),
            serde_json::json!({"pane_id": pane, "matched": true, "elapsed_ms": 12, "absent": false}),
            session(),
            windows(),
            serde_json::json!({"pane_id": pane, "cols": 100, "rows": 30}),
            session(),
            windows(),
            serde_json::json!([{"id": pane, "cwd": "/tmp", "command": "bash", "is_focused": true, "is_zoomed": false}]),
            serde_json::json!({"recording_id": "55555555-5555-4555-8555-555555555555", "pane_id": pane, "path": record_path.display().to_string(), "duration_ms": 0, "lossless": true, "backpressure": true}),
            serde_json::json!({"recording_id": "55555555-5555-4555-8555-555555555555", "path": record_path.display().to_string(), "bytes_written": 0, "status": "complete", "lossless": true, "error": null}),
            session(),
            windows(),
            serde_json::json!({"png_base64": png_b64, "width": 10, "height": 10}),
            session(),
            serde_json::json!({"png_base64": png_b64, "width": 20, "height": 10}),
            session(),
            windows(),
            serde_json::json!({"png_base64": png_b64, "width": 30, "height": 10}),
        ]);

        handle_pane_send_keys(
            &mut client,
            "dev",
            Some("main"),
            Some(pane),
            Some("hello"),
            None,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_pane_run(
            &mut client,
            "dev",
            Some("main"),
            Some(pane),
            "make test",
            60,
            true,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_pane_capture(
            &mut client,
            "dev",
            Some("main"),
            Some(pane),
            5,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_wait_for(
            &mut client,
            Some("dev"),
            None,
            Some(pane),
            Some("ready"),
            None,
            false,
            20,
            1000,
            25,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_pane_set_size(
            &mut client,
            "dev",
            Some("main"),
            Some(pane),
            100,
            30,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_pane_record(
            &mut client,
            "dev",
            pane,
            &record_path,
            true,
            Some(0),
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_pane_snapshot(
            &mut client,
            "dev",
            Some("main"),
            Some(pane),
            None,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_snapshot(
            &mut client,
            Some("dev"),
            None,
            None,
            120,
            36,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        handle_snapshot(
            &mut client,
            Some("dev"),
            Some("main"),
            None,
            80,
            24,
            OutputFormat::Json,
        )
        .await
        .unwrap();

        let requests = finish_rpc_script(client, task, requests).await;
        assert!(
            requests
                .iter()
                .any(|r| r["method"] == "pane.send_keys" && r["params"]["text"] == "hello")
        );
        assert!(
            requests
                .iter()
                .any(|r| r["method"] == "pane.run_command" && r["params"]["async"] == true)
        );
        assert!(
            requests
                .iter()
                .any(|r| r["method"] == "pane.wait_for" && r["params"]["poll_ms"] == 25)
        );
        assert!(
            requests
                .iter()
                .any(|r| r["method"] == "pane.set_size" && r["params"]["cols"] == 100)
        );
        assert!(requests.iter().any(|r| {
            r["method"] == "pane.record.start"
                && r["params"]["pane_id"] == pane
                && r["params"]["path"] == record_path.display().to_string()
        }));
        assert!(requests.iter().any(|r| {
            r["method"] == "pane.list"
                && r["params"]["session_id"] == sid
                && r["params"]["window_id"] == wid
        }));
        assert!(requests.iter().any(|r| r["method"] == "pane.snapshot"));
        assert!(requests.iter().any(|r| r["method"] == "session.snapshot"));
        assert!(requests.iter().any(|r| r["method"] == "window.snapshot"));
    }
}
