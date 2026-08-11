//! `shux pane glance|wait-settled|checkpoint|diff` and `shux lens run`.
//!
//! The lens verbs are agent-facing, so each maps its RPC error to a distinct
//! documented exit code rather than a generic failure.

use super::{args::*, rpc::*};

/// Map a `pane.glance` RPC error code to its CLI exit code (lens PRD §10
/// exit-code table). `pane.glance`'s error surface is INVALID_PARAMS,
/// PANE_NOT_FOUND, PERMISSION_DENIED, PAYLOAD_TOO_LARGE — everything else
/// falls into the table's generic "any other RPC error" bucket.
pub fn lens_glance_exit_code(rpc_error_code: i64) -> i32 {
    match rpc_error_code {
        -32602 => 2, // INVALID_PARAMS
        -32005 => 4, // PERMISSION_DENIED
        -32013 => 5, // PAYLOAD_TOO_LARGE
        _ => 3,      // any other RPC error, incl. PANE_NOT_FOUND (-32004)
    }
}

/// `shux pane glance` — atomic {png, text, revision} of one pane via
/// `pane.glance` RPC (lens PRD §5, §10). No session/window resolution:
/// `pane` is always a raw pane UUID, mirroring the RPC's `pane_id` param.
#[allow(clippy::too_many_arguments)]
pub async fn handle_pane_glance(
    stream: &mut tokio::net::UnixStream,
    pane: &str,
    png_path: Option<std::path::PathBuf>,
    text_only: bool,
    no_cursor: bool,
    checkpoint: bool,
    include_cells: bool,
    cells_out: Option<std::path::PathBuf>,
    masks: Vec<(u16, u16, u16)>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    // clap's `conflicts_with` already rejects this combination at parse time
    // (exit 2, no RPC); this guard keeps the invariant for any programmatic
    // caller of the handler (greptile PR #89 P2).
    if text_only && png_path.is_some() {
        anyhow::bail!("--text-only and --png are mutually exclusive");
    }
    let mask_params: Vec<serde_json::Value> = masks
        .iter()
        .map(|(row, col, width)| serde_json::json!({"row": row, "col": col, "width": width}))
        .collect();
    let params = serde_json::json!({
        "pane_id": pane,
        "include_cursor": !no_cursor,
        "include_png": !text_only,
        "checkpoint": checkpoint,
        "include_cells": include_cells,
        "masks": mask_params,
    });

    match rpc_call(stream, "pane.glance", params).await {
        Ok(result) => {
            // Write the canonical `cells` envelope to disk when requested (task 080).
            if let Some(path) = &cells_out {
                let Some(cells) = result.get("cells") else {
                    anyhow::bail!("--cells-out given but the glance result has no cells field");
                };
                std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(cells)?))?;
            }
            if let Some(path) = &png_path {
                use base64::Engine;
                let b64 = result.get("png_base64").and_then(|v| v.as_str());
                let Some(b64) = b64 else {
                    anyhow::bail!(
                        "--png given but the glance result has no png_base64 \
                         (was --text-only also passed?)"
                    );
                };
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|e| anyhow::anyhow!("decode glance png: {e}"))?;
                std::fs::write(path, &bytes)?;
            }

            match format {
                OutputFormat::Json => {
                    // Deliberately the `{result|error}` envelope, NOT the bare
                    // result the sibling snapshot/capture handlers emit: the
                    // FROZEN lens harness (lens_common::cli_envelope, its doc
                    // comment reads §10 as "the raw RPC result envelope")
                    // parses `.get("result")/.get("error")` from every lens
                    // CLI verb's --format json output, giving byte-parity
                    // with `shux rpc call` (M9). Emitting the bare result
                    // breaks G1/G2/G2w CLI twins (verified empirically —
                    // codex P2 review minor 4 is DISPUTED with that
                    // evidence; changing shape requires a LENS-TEST-CHANGE
                    // to the frozen harness first).
                    let envelope = serde_json::json!({"result": result});
                    println!(
                        "{}",
                        crate::style::json_safe(&serde_json::to_string_pretty(&envelope)?)
                    );
                }
                OutputFormat::Text | OutputFormat::Plain => {
                    let revision = result.get("revision").and_then(|v| v.as_u64()).unwrap_or(0);
                    let cols = result.get("cols").and_then(|v| v.as_u64()).unwrap_or(0);
                    let rows = result.get("rows").and_then(|v| v.as_u64()).unwrap_or(0);
                    let cursor = result.get("cursor").cloned().unwrap_or_default();
                    let cursor_row = cursor.get("row").and_then(|v| v.as_u64()).unwrap_or(0);
                    let cursor_col = cursor.get("col").and_then(|v| v.as_u64()).unwrap_or(0);
                    let cursor_visible = cursor
                        .get("visible")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let alt_screen = result
                        .get("alt_screen")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let checkpointed = result
                        .get("checkpointed")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let evicted_revision = result.get("evicted_revision").and_then(|v| v.as_u64());
                    let text = result.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    let png_written = png_path.as_deref().map(|p| {
                        let len = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
                        (p, len)
                    });
                    crate::style::print_pane_glance(
                        pane,
                        revision,
                        cols,
                        rows,
                        cursor_row,
                        cursor_col,
                        cursor_visible,
                        alt_screen,
                        checkpointed,
                        evicted_revision,
                        text,
                        png_written,
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
            match format {
                OutputFormat::Json => {
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
                }
                OutputFormat::Text | OutputFormat::Plain => {
                    // Render `data.detail`, not just the generic code name:
                    // "invalid_params (code -32602)" does not tell someone who
                    // pasted a three-character id what to do about it
                    // (issue #120; same rule wait-settled already followed).
                    crate::style::print_error(&format!(
                        "glance failed: {} (code {code})",
                        rpc_display(code, &message, data.as_ref())
                    ));
                }
            }
            std::process::exit(lens_glance_exit_code(code));
        }
        Err(other) => Err(other.into()),
    }
}

/// Map a `pane.wait_settled` RPC error code to its CLI exit code (lens PRD
/// §10). A settle TIMEOUT is NOT an error — it is a `settled=false` RESULT
/// handled in the success arm below and mapped to exit 1 there. This maps only
/// genuine RPC errors: INVALID_PARAMS → 2, PERMISSION_DENIED → 4, everything
/// else (incl. PANE_NOT_FOUND) → 3.
pub fn lens_wait_settled_exit_code(rpc_error_code: i64) -> i32 {
    match rpc_error_code {
        -32602 => 2, // INVALID_PARAMS
        -32005 => 4, // PERMISSION_DENIED
        _ => 3,      // any other RPC error, incl. PANE_NOT_FOUND (-32004)
    }
}

/// `shux pane wait-settled` — block until a pane is quiet via
/// `pane.wait_settled` RPC (lens PRD §6, §10). `quiet`/`timeout` arrive here
/// already normalized to milliseconds by `parse_duration_ms`. Exit 0 when
/// settled, exit 1 on timeout (`settled=false`, a RESULT not an error).
#[allow(clippy::too_many_arguments)]
pub async fn handle_pane_wait_settled(
    stream: &mut tokio::net::UnixStream,
    pane: &str,
    quiet_ms: u64,
    timeout_ms: u64,
    hold_ms: u64,
    stable_frames: u32,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let params = serde_json::json!({
        "pane_id": pane,
        "quiet_ms": quiet_ms,
        "timeout_ms": timeout_ms,
        "hold_ms": hold_ms,
        "stable_frames": stable_frames,
    });

    match rpc_call(stream, "pane.wait_settled", params).await {
        Ok(result) => {
            let settled = result
                .get("settled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            match format {
                OutputFormat::Json => {
                    // §10: byte-identical to `shux rpc call` — the `{result}`
                    // envelope (the frozen lens harness parses this shape).
                    let envelope = serde_json::json!({ "result": result });
                    println!(
                        "{}",
                        crate::style::json_safe(&serde_json::to_string_pretty(&envelope)?)
                    );
                }
                OutputFormat::Text | OutputFormat::Plain => {
                    let revision = result.get("revision").and_then(|v| v.as_u64()).unwrap_or(0);
                    let waited_ms = result
                        .get("waited_ms")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    crate::style::print_pane_wait_settled(pane, settled, revision, waited_ms);
                }
            }
            // §10 CLI-only mapping: settled → exit 0, timeout → exit 1.
            if settled {
                Ok(())
            } else {
                std::process::exit(1);
            }
        }
        Err(RpcClientError::Rpc {
            code,
            message,
            data,
        }) => {
            match format {
                OutputFormat::Json => {
                    let mut err_obj = serde_json::json!({
                        "code": code,
                        "message": message,
                    });
                    if let Some(d) = data {
                        err_obj["data"] = d;
                    }
                    let envelope = serde_json::json!({ "error": err_obj });
                    println!(
                        "{}",
                        crate::style::json_safe(&serde_json::to_string_pretty(&envelope)?)
                    );
                }
                OutputFormat::Text | OutputFormat::Plain => {
                    // Surface the actionable `data.detail` (e.g. "hold_ms 5 out of range
                    // [10, 60000]"), not just the generic "invalid_params" message — a first-timer
                    // who mistypes --hold-ms/--stable-frames/--quiet must be told the range
                    // (dogfood 083: error actionability).
                    crate::style::print_error(&format!(
                        "wait-settled failed: {}",
                        rpc_display(code, &message, data.as_ref())
                    ));
                }
            }
            std::process::exit(lens_wait_settled_exit_code(code));
        }
        Err(other) => Err(other.into()),
    }
}

/// Map a `pane.checkpoint` RPC error code to its CLI exit code (lens PRD §10).
/// `pane.checkpoint` error surface: INVALID_PARAMS (bad/missing pane_id →
/// exit 2), PERMISSION_DENIED (exit 4), PANE_NOT_FOUND + anything else → exit 3.
pub fn lens_checkpoint_exit_code(rpc_error_code: i64) -> i32 {
    match rpc_error_code {
        -32602 => 2, // INVALID_PARAMS
        -32005 => 4, // PERMISSION_DENIED
        _ => 3,      // any other RPC error, incl. PANE_NOT_FOUND (-32004)
    }
}

/// Map a `pane.diff_since` RPC error code to its CLI exit code (lens PRD §10
/// exit-code table). STALE_REVISION / RESIZE_INVALIDATED / PAYLOAD_TOO_LARGE
/// map to exit 5 (diff-specific data errors); INVALID_PARAMS → 2,
/// PERMISSION_DENIED → 4, everything else (incl. PANE_NOT_FOUND) → 3.
pub fn lens_diff_exit_code(rpc_error_code: i64) -> i32 {
    match rpc_error_code {
        -32602 => 2,                   // INVALID_PARAMS
        -32005 => 4,                   // PERMISSION_DENIED
        -32010 | -32011 | -32013 => 5, // STALE / INVALIDATED / PAYLOAD_TOO_LARGE
        _ => 3,                        // any other RPC error, incl. PANE_NOT_FOUND
    }
}

/// Emit the `{error}` envelope (`--format json`) or a styled error line, then
/// exit with `exit_code`. Shared by the checkpoint/diff error arms — byte-
/// parity with `shux rpc call` (M9), the shape the frozen lens harness parses.
pub fn lens_emit_error_and_exit(
    format: OutputFormat,
    verb: &str,
    code: i64,
    message: &str,
    data: Option<serde_json::Value>,
    exit_code: i32,
) -> ! {
    match format {
        OutputFormat::Json => {
            let mut err_obj = serde_json::json!({ "code": code, "message": message });
            if let Some(d) = data {
                err_obj["data"] = d;
            }
            let envelope = serde_json::json!({ "error": err_obj });
            match serde_json::to_string_pretty(&envelope) {
                Ok(s) => println!("{s}"),
                Err(e) => eprintln!("failed to serialize error envelope: {e}"),
            }
        }
        OutputFormat::Text | OutputFormat::Plain => {
            // As above: the actionable text lives in `data.detail`.
            crate::style::print_error(&format!(
                "{verb} failed: {} (code {code})",
                rpc_display(code, message, data.as_ref())
            ));
        }
    }
    std::process::exit(exit_code);
}

/// `shux pane checkpoint` — capture a checkpoint via `pane.checkpoint` RPC
/// (lens PRD §7, §10). `pane` is a raw pane UUID, mirroring the RPC `pane_id`.
pub async fn handle_pane_checkpoint(
    stream: &mut tokio::net::UnixStream,
    pane: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let params = serde_json::json!({ "pane_id": pane });

    match rpc_call(stream, "pane.checkpoint", params).await {
        Ok(result) => {
            match format {
                OutputFormat::Json => {
                    // §10: the `{result}` envelope, byte-identical to
                    // `shux rpc call` (the frozen lens harness parses this).
                    let envelope = serde_json::json!({ "result": result });
                    println!(
                        "{}",
                        crate::style::json_safe(&serde_json::to_string_pretty(&envelope)?)
                    );
                }
                OutputFormat::Text | OutputFormat::Plain => {
                    let revision = result.get("revision").and_then(|v| v.as_u64()).unwrap_or(0);
                    let evicted = result.get("evicted_revision").and_then(|v| v.as_u64());
                    crate::style::print_pane_checkpoint(pane, revision, evicted);
                }
            }
            Ok(())
        }
        Err(RpcClientError::Rpc {
            code,
            message,
            data,
        }) => lens_emit_error_and_exit(
            format,
            "checkpoint",
            code,
            &message,
            data,
            lens_checkpoint_exit_code(code),
        ),
        Err(other) => Err(other.into()),
    }
}

/// `shux pane diff` — structured diff via `pane.diff_since` RPC (lens PRD §7,
/// §10). `--heat <path>` writes the heat PNG; `--no-row-text` drops the
/// per-row changed text. Exit 0 on any delta; exit 5 on stale/invalidated.
pub async fn handle_pane_diff(
    stream: &mut tokio::net::UnixStream,
    pane: &str,
    since: u64,
    heat_path: Option<std::path::PathBuf>,
    no_row_text: bool,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let params = serde_json::json!({
        "pane_id": pane,
        "since_revision": since,
        "changed_row_text": !no_row_text,
        // Only request the heat PNG when the caller wants a file for it.
        "heat_png": heat_path.is_some(),
    });

    match rpc_call(stream, "pane.diff_since", params).await {
        Ok(result) => {
            if let Some(path) = &heat_path {
                use base64::Engine;
                let b64 = result.get("heat_png_base64").and_then(|v| v.as_str());
                let Some(b64) = b64 else {
                    anyhow::bail!("--heat given but the diff result has no heat_png_base64");
                };
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|e| anyhow::anyhow!("decode heat png: {e}"))?;
                std::fs::write(path, &bytes)?;
            }

            match format {
                OutputFormat::Json => {
                    // §10: the `{result}` envelope, byte-identical to
                    // `shux rpc call` (the frozen lens harness parses this).
                    let envelope = serde_json::json!({ "result": result });
                    println!(
                        "{}",
                        crate::style::json_safe(&serde_json::to_string_pretty(&envelope)?)
                    );
                }
                OutputFormat::Text | OutputFormat::Plain => {
                    let from = result
                        .get("from_revision")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let to = result
                        .get("to_revision")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let cells = result
                        .get("cells_changed")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let cursor_moved = result
                        .get("cursor_moved")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let regions = result
                        .get("regions")
                        .and_then(|v| v.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    let truncated = result
                        .get("regions_truncated")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let heat_written = heat_path.as_deref().map(|p| {
                        let len = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
                        (p, len)
                    });
                    crate::style::print_pane_diff(
                        pane,
                        from,
                        to,
                        cells,
                        regions,
                        truncated,
                        cursor_moved,
                        heat_written,
                    );
                }
            }
            // Exit 0 on ANY delta — the diff is data, not a verdict (§10).
            Ok(())
        }
        Err(RpcClientError::Rpc {
            code,
            message,
            data,
        }) => lens_emit_error_and_exit(
            format,
            "diff",
            code,
            &message,
            data,
            lens_diff_exit_code(code),
        ),
        Err(other) => Err(other.into()),
    }
}

/// Map a `lens.run` RPC error code to its CLI exit code (lens PRD §10 exit
/// table): INVALID_PARAMS → 2, PERMISSION_DENIED → 4,
/// RESOURCE_EXHAUSTED/SPAWN_FAILED → 5 (setup failures BEFORE the child
/// starts — the child-exit-code precedence rule only applies once `wait`
/// has actually observed the process start), everything else → 3.
pub fn lens_run_exit_code(rpc_error_code: i64) -> i32 {
    match rpc_error_code {
        -32602 => 2,          // INVALID_PARAMS
        -32005 => 4,          // PERMISSION_DENIED
        -32012 | -32014 => 5, // RESOURCE_EXHAUSTED / SPAWN_FAILED
        _ => 3,
    }
}

/// `shux lens run` — spawn `argv` in a hidden scratch session via `lens.run`
/// RPC (lens PRD §8, §10). Async by default (prints `{session_id, pane_id,
/// revision}`); `--wait` blocks for completion, adds `exit_code`, and once
/// the child has started, the CLI process itself exits with the CHILD's
/// code — authoritatively, even if it collides with the exit table below
/// (§10's documented precedence rule; scripts needing certainty parse
/// `--format json`, where `exit_code` is present iff the child ran).
#[allow(clippy::too_many_arguments)]
pub async fn handle_lens_run(
    stream: &mut tokio::net::UnixStream,
    argv: &[String],
    size: (u16, u16),
    ttl_ms: u64,
    max_runtime_ms: u64,
    env: &[(String, String)],
    cwd: Option<&std::path::Path>,
    wait: bool,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let env_obj: serde_json::Map<String, serde_json::Value> = env
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    let mut params = serde_json::json!({
        "argv": argv,
        "cols": size.0,
        "rows": size.1,
        "env": serde_json::Value::Object(env_obj),
        "post_exit_ttl_ms": ttl_ms,
        "max_runtime_ms": max_runtime_ms,
        "wait": wait,
    });
    if let Some(c) = cwd {
        params["cwd"] = serde_json::Value::String(c.display().to_string());
    }

    match rpc_call(stream, "lens.run", params).await {
        Ok(result) => {
            match format {
                OutputFormat::Json => {
                    // §10: the `{result}` envelope, byte-identical to
                    // `shux rpc call` (the frozen lens harness parses this).
                    let envelope = serde_json::json!({ "result": result });
                    println!(
                        "{}",
                        crate::style::json_safe(&serde_json::to_string_pretty(&envelope)?)
                    );
                }
                OutputFormat::Text | OutputFormat::Plain => {
                    let session_id = result
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let pane_id = result.get("pane_id").and_then(|v| v.as_str()).unwrap_or("");
                    let revision = result.get("revision").and_then(|v| v.as_u64()).unwrap_or(0);
                    let exit_code = result.get("exit_code").and_then(|v| v.as_i64());
                    crate::style::print_lens_run(session_id, pane_id, revision, exit_code);
                }
            }
            // §10 precedence: once the child has started (wait=true and the
            // RPC returned normally — spawn already succeeded synchronously
            // per LENS-R-045), the CLI exits with the CHILD's code.
            if wait {
                let exit_code = result
                    .get("exit_code")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(-1);
                std::process::exit(exit_code as i32);
            }
            Ok(())
        }
        Err(RpcClientError::Rpc {
            code,
            message,
            data,
        }) => lens_emit_error_and_exit(
            format,
            "lens run",
            code,
            &message,
            data,
            lens_run_exit_code(code),
        ),
        Err(other) => Err(other.into()),
    }
}
