//! Pane I/O methods: writes, capture, waits, recording and the lens reads.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use shux_rpc::{Policy, Sensitivity};
use tokio::sync::Mutex;

use crate::lens_render::{
    glance_row_text, lens_pixel_budget_check, parse_glance_masks, render_lens_heat_png,
};
use crate::pane_io::{PaneIoState, ResizeRequest, diff_lookup_checkpoint};
use crate::pane_record::{
    PANE_RECORD_COMPLETED_TTL, PaneRecordChunk, PaneRecordFormat, PaneRecordStatus, PaneRecorder,
    spawn_pane_recorder,
};
use crate::rpc::params::{
    required_str, resolve_pane_id_from_params, resolve_session_ref, resolve_window_id_from_params,
};
use crate::settle::{
    SettleWake, settle_decide, settle_is_quiet, settle_reacquire_watch, settle_remaining_quiet_ns,
    settle_u32_param, settle_u64_param, validate_stability_params, validate_wait_settled_params,
    wait_settled_frame_stability,
};
use crate::snapshot::{
    build_snapshot_rasterizer, parse_snapshot_dims, preview_for_log, snapshot_font_key,
    snapshot_window,
};
use crate::{lens_scratch, onboarding, pane_command, session_meta, statusbar_runner};

/// Register pane I/O methods (send_keys, run_command, command_status, command_cancel, capture).
#[allow(clippy::too_many_arguments)]
pub(crate) fn register_pane_io_methods(
    builder: shux_rpc::RouterBuilder,
    graph: shux_core::graph::GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    _cancel: tokio_util::sync::CancellationToken,
    config: shux_core::config::ConfigHandle,
    meta_cache: session_meta::SessionMetaCache,
    onboarding: onboarding::OnboardingHandle,
    segments: statusbar_runner::SegmentCache,
    lens_audit: Arc<lens_scratch::LensAuditLog>,
) -> shux_rpc::RouterBuilder {
    // LENS-R-052 audit handles for the three lens observation methods
    // (glance / checkpoint / diff). `caller` comes from
    // `shux_rpc::current_caller()` — the task-local the plugin dispatch
    // wrapper scopes to `plugin:<uuid>`; UDS requests default to "uds"
    // (P5 round-1 claude N3, adjudicated IMPLEMENT).
    let audit_glance = lens_audit.clone();
    let audit_checkpoint = lens_audit.clone();
    let audit_diff = lens_audit;

    let g1 = graph.clone();
    let g2 = graph.clone();
    let g5 = graph.clone();
    let g6 = graph.clone();
    let g7 = graph.clone();
    let g8 = graph.clone();
    let g9 = graph.clone();
    let g10 = graph.clone();
    let g12 = graph.clone(); // pane.glance (LENS-R-010..016)
    let g13 = graph.clone(); // pane.wait_settled (LENS-R-020..025)
    let g14 = graph.clone(); // pane.checkpoint (LENS-R-030/031)
    let g15 = graph.clone(); // pane.diff_since (LENS-R-033..038)
    let g11 = graph;

    let io1 = io_state.clone();
    let io2 = io_state.clone();
    let io3 = io_state.clone();
    let io4 = io_state.clone();
    let io5 = io_state.clone();
    let io6 = io_state.clone();
    let io7 = io_state.clone();
    let io8 = io_state.clone();
    let io9 = io_state.clone();
    let io10 = io_state.clone();
    let io11 = io_state.clone();
    let io13 = io_state.clone(); // pane.glance
    let io14 = io_state.clone(); // pane.wait_settled
    let io15 = io_state.clone(); // pane.checkpoint
    let io16 = io_state.clone(); // pane.diff_since
    let io12 = io_state;

    // Shared rasterizer for `pane.snapshot` / `window.snapshot` / `session.snapshot`.
    // Wrapped in an `ArcSwap` so the snapshot handlers can pick up
    // `appearance.font` changes via the existing config hot-reload
    // signal without a daemon restart. On reload failure (bad font
    // path, corrupt file) the last-good rasterizer is kept and the
    // error logged — snapshots never produce blank PNGs because of a
    // misconfiguration. PR #46.
    //
    // Race-window note (council review, PR #46): we capture the
    // build-time config snapshot ONCE here and pass it INTO the reload
    // task. The task starts from that exact same snapshot's font key
    // and re-checks the current config before entering its `notified`
    // loop. This closes the TOCTOU between (a) the initial build and
    // (b) the spawned task starting to await — without it, a config
    // change in that gap would be silently lost because
    // `ConfigHandle::replace` uses `notify_waiters()` which only wakes
    // tasks ALREADY parked on `notified()`.
    let build_snap = config.current();
    let initial_font_key = snapshot_font_key(&build_snap);
    let rasterizer: Arc<arc_swap::ArcSwap<shux_raster::Rasterizer>> =
        Arc::new(arc_swap::ArcSwap::from(Arc::new(
            build_snapshot_rasterizer(&build_snap).unwrap_or_else(|e| {
                tracing::warn!(
                    error = %e,
                    "appearance.font invalid at startup, falling back to bundled chain"
                );
                shux_raster::Rasterizer::new(14.0)
                    .expect("shux-raster: bundled font corrupt — should be unreachable")
            }),
        )));
    {
        let raster_handle = rasterizer.clone();
        let config_for_reload = config.clone();
        let notify = config_for_reload.change_notify();
        tokio::spawn(async move {
            let mut last_font_key = initial_font_key;
            // Catch any change that landed between the initial build
            // and this task taking its first scheduling slot. Without
            // this, the racing change is silently swallowed and the
            // user sees a stale rasterizer until they edit the config
            // again. Council review (PR #46).
            let bootstrap_snap = config_for_reload.current();
            let bootstrap_key = snapshot_font_key(&bootstrap_snap);
            if bootstrap_key != last_font_key {
                match build_snapshot_rasterizer(&bootstrap_snap) {
                    Ok(new_raster) => {
                        raster_handle.store(Arc::new(new_raster));
                        last_font_key = bootstrap_key;
                        tracing::info!(
                            "snapshot rasterizer caught a config change \
                             that raced the daemon startup"
                        );
                    }
                    Err(e) => tracing::warn!(
                        error = %e,
                        "snapshot rasterizer bootstrap reload failed; keeping initial"
                    ),
                }
            }
            loop {
                notify.notified().await;
                let cfg_snap = config_for_reload.current();
                let new_key = snapshot_font_key(&cfg_snap);
                if new_key == last_font_key {
                    continue;
                }
                match build_snapshot_rasterizer(&cfg_snap) {
                    Ok(new_raster) => {
                        raster_handle.store(Arc::new(new_raster));
                        last_font_key = new_key;
                        tracing::info!("snapshot rasterizer rebuilt after config change");
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "snapshot rasterizer rebuild failed; keeping last good"
                        );
                    }
                }
            }
        });
    }
    let rasterizer_pane = rasterizer.clone();
    let rasterizer_window = rasterizer.clone();
    let rasterizer_session = rasterizer.clone();
    let rasterizer_glance = rasterizer.clone();
    let rasterizer_diff = rasterizer.clone();

    builder
        .register_with_policy(
            "pane.send_keys",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g1.clone();
                let io = io1.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;

                    let bytes = if let Some(text) = params.get("text").and_then(|v| v.as_str()) {
                        text.as_bytes().to_vec()
                    } else if let Some(b64) = params.get("data").and_then(|v| v.as_str()) {
                        use base64::Engine;
                        base64::engine::general_purpose::STANDARD
                            .decode(b64)
                            .map_err(|e| {
                                shux_rpc::RpcError::invalid_params(&format!("invalid base64: {e}"))
                            })?
                    } else {
                        return Err(shux_rpc::RpcError::invalid_params(
                            "missing 'text' or 'data' parameter",
                        ));
                    };

                    let state = io.lock().await;
                    let writer = state
                        .writers
                        .get(&pane_id)
                        .ok_or_else(|| {
                            shux_rpc::RpcError::not_found("pane PTY", &pane_id.to_string())
                        })?
                        .clone();
                    drop(state);

                    let len = bytes.len();
                    writer
                        .send(bytes)
                        .await
                        .map_err(|_| shux_rpc::RpcError::internal("PTY write channel closed"))?;

                    Ok(serde_json::json!({
                        "pane_id": pane_id.to_string(),
                        "bytes_written": len,
                    }))
                }
            },
        )
        .register_with_policy(
            "pane.run_command",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g2.clone();
                let io = io2.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;

                    let command =
                        params
                            .get("command")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| {
                                shux_rpc::RpcError::invalid_params("missing 'command' parameter")
                            })?;

                    let args: Vec<String> = pane_command::parse_run_args(&params)?;

                    let timeout_secs = params.get("timeout").and_then(|v| v.as_u64()).unwrap_or(30);
                    let timeout = Duration::from_secs(timeout_secs);

                    let is_async = params
                        .get("async")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);

                    let (completion_tx, completion_rx) = if !is_async {
                        let (tx, rx) = tokio::sync::oneshot::channel();
                        (Some(tx), Some(rx))
                    } else {
                        (None, None)
                    };

                    let (command_id, pty_command) = {
                        let mut state = io.lock().await;
                        state.cmd_engine.start_command(
                            pane_id.0,
                            command,
                            &args,
                            timeout,
                            completion_tx,
                        )
                    };

                    // Write the PTY command
                    {
                        let state = io.lock().await;
                        let writer = state
                            .writers
                            .get(&pane_id)
                            .ok_or_else(|| {
                                shux_rpc::RpcError::not_found("pane PTY", &pane_id.to_string())
                            })?
                            .clone();
                        drop(state);

                        writer.send(pty_command.into_bytes()).await.map_err(|_| {
                            shux_rpc::RpcError::internal("PTY write channel closed")
                        })?;
                    }

                    if is_async {
                        return Ok(serde_json::json!({
                            "command_id": command_id.to_string(),
                            "state": "running",
                        }));
                    }

                    // Sync mode: wait for completion
                    let result = completion_rx
                        .unwrap() // safe: created above when !is_async
                        .await
                        .map_err(|_| {
                            shux_rpc::RpcError::internal("command completion channel dropped")
                        })?;

                    // Capture text from VT
                    let stdout = {
                        let state = io.lock().await;
                        state
                            .vts
                            .get(&pane_id)
                            .map(|vt| {
                                let text = vt.capture_text(Some(50));
                                shux_pty::strip_ansi(&text)
                            })
                            .unwrap_or_default()
                    };

                    Ok(serde_json::json!({
                        "command_id": result.command_id.to_string(),
                        "state": result.state,
                        "exit_code": result.exit_code,
                        "stdout": stdout,
                        "runtime_ms": result.runtime_ms,
                    }))
                }
            },
        )
        .register_with_policy(
            "pane.command_status",
            Policy::fixed(Sensitivity::ContentRead),
            move |params: Option<serde_json::Value>| {
                let io = io3.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let cmd_id_str = params
                        .get("command_id")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'command_id' parameter")
                        })?;

                    let command_id: uuid::Uuid = cmd_id_str.parse().map_err(|_| {
                        shux_rpc::RpcError::invalid_params("invalid command_id format")
                    })?;

                    let state = io.lock().await;
                    let result = state
                        .cmd_engine
                        .get_status(command_id)
                        .ok_or_else(|| shux_rpc::RpcError::not_found("command", cmd_id_str))?;

                    Ok(serde_json::json!({
                        "command_id": result.command_id.to_string(),
                        "state": result.state,
                        "exit_code": result.exit_code,
                        "runtime_ms": result.runtime_ms,
                    }))
                }
            },
        )
        .register_with_policy(
            "pane.command_cancel",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let io = io4.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let cmd_id_str = params
                        .get("command_id")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'command_id' parameter")
                        })?;

                    let command_id: uuid::Uuid = cmd_id_str.parse().map_err(|_| {
                        shux_rpc::RpcError::invalid_params("invalid command_id format")
                    })?;

                    let mut state = io.lock().await;
                    let pane_uuid = state
                        .cmd_engine
                        .cancel_command(command_id)
                        .ok_or_else(|| shux_rpc::RpcError::not_found("command", cmd_id_str))?;

                    // Send Ctrl-C to the pane
                    let pane_id = shux_core::model::PaneId::from_uuid(pane_uuid);
                    if let Some(writer) = state.writers.get(&pane_id) {
                        let _ = writer.send(vec![0x03]).await; // ETX = Ctrl-C
                    }

                    Ok(serde_json::json!({
                        "command_id": cmd_id_str,
                        "state": "cancelled",
                    }))
                }
            },
        )
        .register_with_policy(
            "pane.capture",
            Policy::fixed(Sensitivity::ContentRead),
            move |params: Option<serde_json::Value>| {
                let gh = g5.clone();
                let io = io5.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;

                    // None → entire visible viewport (iTerm2 get_screen_contents
                    // shape). Some(N) → tail N non-blank rows.
                    let lines = params
                        .get("lines")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as usize);

                    let state = io.lock().await;
                    let vt = state.vts.get(&pane_id).ok_or_else(|| {
                        shux_rpc::RpcError::not_found("pane VT", &pane_id.to_string())
                    })?;

                    let text = vt.capture_text(lines);
                    let clean = shux_pty::strip_ansi(&text);
                    let cursor = vt.cursor();
                    let cols = vt.grid().cols();
                    let rows = vt.grid().rows();

                    let mut body = serde_json::json!({
                        "pane_id": pane_id.to_string(),
                        "text": clean,
                        "lines": clean.lines().count(),
                        "cols": cols,
                        "rows": rows,
                        "cursor": {
                            "row": cursor.row,
                            "col": cursor.col,
                            "visible": cursor.visible,
                        },
                    });
                    if let Some(requested) = lines {
                        body["requested_lines"] = serde_json::Value::from(requested);
                    }
                    Ok(body)
                }
            },
        )
        .register_with_policy(
            "pane.wait_for",
            Policy::fixed(Sensitivity::ContentRead),
            move |params: Option<serde_json::Value>| {
                let gh = g10.clone();
                let io = io10.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;

                    let needle_text = params
                        .get("text")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    let needle_regex_raw = params.get("regex").and_then(|v| v.as_str());
                    let absent = params
                        .get("absent")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let lines =
                        params.get("lines").and_then(|v| v.as_u64()).unwrap_or(200) as usize;
                    let timeout_ms = params
                        .get("timeout_ms")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(10_000)
                        .min(60_000);
                    let poll_ms = params
                        .get("poll_ms")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(100)
                        .clamp(20, 1_000);

                    if needle_text.is_none() && needle_regex_raw.is_none() {
                        return Err(shux_rpc::RpcError::invalid_params(
                            "missing 'text' or 'regex' parameter",
                        ));
                    }
                    let needle_regex = match needle_regex_raw {
                        Some(r) => Some(regex::Regex::new(r).map_err(|e| {
                            shux_rpc::RpcError::invalid_params(&format!("invalid regex: {e}"))
                        })?),
                        None => None,
                    };

                    let start = std::time::Instant::now();
                    let deadline = start + std::time::Duration::from_millis(timeout_ms);
                    let mut last_capture;

                    loop {
                        last_capture = {
                            let state = io.lock().await;
                            let vt = state.vts.get(&pane_id).ok_or_else(|| {
                                shux_rpc::RpcError::not_found("pane VT", &pane_id.to_string())
                            })?;
                            let raw = vt.capture_text(Some(lines));
                            shux_pty::strip_ansi(&raw)
                        };

                        let hit = if let Some(re) = needle_regex.as_ref() {
                            re.is_match(&last_capture)
                        } else if let Some(t) = needle_text.as_ref() {
                            last_capture.contains(t.as_str())
                        } else {
                            false
                        };
                        let matched = if absent { !hit } else { hit };

                        if matched {
                            let elapsed = start.elapsed().as_millis() as u64;
                            return Ok(serde_json::json!({
                                "pane_id": pane_id.to_string(),
                                "matched": true,
                                "elapsed_ms": elapsed,
                                "absent": absent,
                                "text_preview": preview_for_log(&last_capture, 240),
                            }));
                        }

                        if std::time::Instant::now() >= deadline {
                            return Err(shux_rpc::RpcError::with_message_and_data(
                                shux_rpc::ErrorCode::NotFound,
                                "wait_for timed out".to_string(),
                                serde_json::json!({
                                    "pane_id": pane_id.to_string(),
                                    "absent": absent,
                                    "timeout_ms": timeout_ms,
                                    "elapsed_ms": start.elapsed().as_millis() as u64,
                                    "last_capture_preview": preview_for_log(&last_capture, 480),
                                }),
                            ));
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(poll_ms)).await;
                    }
                }
            },
        )
        .register_with_policy(
            "pane.record.start",
            Policy::fixed(Sensitivity::PluginsForbidden),
            move |params: Option<serde_json::Value>| {
                let gh = g11.clone();
                let io = io11.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;
                    let path_str =
                        params.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'path' parameter")
                        })?;
                    if path_str.trim().is_empty() {
                        return Err(shux_rpc::RpcError::invalid_params(
                            "'path' parameter must not be empty",
                        ));
                    }
                    let path = PathBuf::from(path_str);
                    let overwrite = params
                        .get("overwrite")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let duration_ms = params.get("duration_ms").and_then(|v| v.as_u64());
                    // Task 083: `format` selects the on-disk shape. "raw" (default) is the
                    // pre-083 lossless byte stream; "cast" emits asciinema v2 (timestamped output
                    // + honest resize events, UTF-8-safe). Unknown values fail closed.
                    let format_str = params
                        .get("format")
                        .and_then(|v| v.as_str())
                        .unwrap_or("raw");
                    let cast = match format_str {
                        "raw" => false,
                        "cast" => true,
                        other => {
                            return Err(shux_rpc::RpcError::invalid_params(&format!(
                                "unknown record format {other:?} (expected \"raw\" or \"cast\")"
                            )));
                        }
                    };
                    if !overwrite {
                        match tokio::fs::try_exists(&path).await {
                            Ok(true) => {
                                return Err(shux_rpc::RpcError::invalid_params(
                                    "record file already exists; pass overwrite=true to replace it",
                                ));
                            }
                            Ok(false) => {}
                            Err(e) => {
                                return Err(shux_rpc::RpcError::internal(&format!(
                                    "failed to inspect record file {}: {e}",
                                    path.display()
                                )));
                            }
                        }
                    }

                    // Verify the pane is live and not already recording, and (for cast) read its
                    // current geometry for the asciinema header — all in one critical section.
                    let record_format = {
                        let state = io.lock().await;
                        if !state.writers.contains_key(&pane_id) {
                            return Err(shux_rpc::RpcError::not_found(
                                "live pane PTY",
                                &pane_id.to_string(),
                            ));
                        }
                        if state.recorders.get(&pane_id).is_some_and(|recorders| {
                            recorders.iter().any(|r| {
                                r.outcome.lock().expect("record outcome poisoned").status
                                    == PaneRecordStatus::Recording
                            })
                        }) {
                            return Err(shux_rpc::RpcError::name_conflict(
                                "pane recording",
                                &pane_id.to_string(),
                            ));
                        }
                        if cast {
                            let vt = state.vts.get(&pane_id).ok_or_else(|| {
                                shux_rpc::RpcError::not_found("pane VT", &pane_id.to_string())
                            })?;
                            PaneRecordFormat::Cast {
                                cols: vt.grid().cols() as u16,
                                rows: vt.grid().rows() as u16,
                            }
                        } else {
                            PaneRecordFormat::Raw
                        }
                    };

                    let (sender, outcome, task) =
                        spawn_pane_recorder(path.clone(), overwrite, record_format)
                            .await
                            .map_err(|e| shux_rpc::RpcError::internal(&e))?;
                    let recording_id = uuid::Uuid::new_v4();
                    let mut state = io.lock().await;
                    if !state.writers.contains_key(&pane_id) {
                        drop(state);
                        let _ = sender
                            .send(PaneRecordChunk::Finish {
                                status: PaneRecordStatus::Aborted,
                            })
                            .await;
                        let _ = task.await;
                        return Err(shux_rpc::RpcError::not_found(
                            "live pane PTY",
                            &pane_id.to_string(),
                        ));
                    }
                    state
                        .recorders
                        .entry(pane_id)
                        .or_default()
                        .push(PaneRecorder {
                            id: recording_id,
                            path: path.clone(),
                            sender: sender.clone(),
                            outcome: outcome.clone(),
                            task,
                        });
                    drop(state);

                    if let Some(ms) = duration_ms {
                        let io_for_deadline = io.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                            let deadline_sender = {
                                let state = io_for_deadline.lock().await;
                                state.recorders.get(&pane_id).and_then(|recorders| {
                                    recorders
                                        .iter()
                                        .find(|r| r.id == recording_id)
                                        .and_then(|r| {
                                            let status = r
                                                .outcome
                                                .lock()
                                                .expect("record outcome poisoned")
                                                .status;
                                            (status == PaneRecordStatus::Recording)
                                                .then(|| r.sender.clone())
                                        })
                                })
                            };
                            if let Some(sender) = deadline_sender {
                                let _ = sender
                                    .send(PaneRecordChunk::Finish {
                                        status: PaneRecordStatus::Complete,
                                    })
                                    .await;
                            }
                            tokio::time::sleep(PANE_RECORD_COMPLETED_TTL).await;
                            let mut state = io_for_deadline.lock().await;
                            for recorders in state.recorders.values_mut() {
                                if let Some(pos) = recorders.iter().position(|r| {
                                    r.id == recording_id
                                        && r.outcome.lock().expect("record outcome poisoned").status
                                            != PaneRecordStatus::Recording
                                }) {
                                    recorders.remove(pos);
                                    break;
                                }
                            }
                            state.recorders.retain(|_, recorders| !recorders.is_empty());
                        });
                    }

                    Ok(serde_json::json!({
                        "recording_id": recording_id.to_string(),
                        "pane_id": pane_id.to_string(),
                        "path": path.display().to_string(),
                        "duration_ms": duration_ms,
                        "lossless": true,
                        "backpressure": true,
                    }))
                }
            },
        )
        .register_with_policy(
            "pane.record.stop",
            Policy::fixed(Sensitivity::PluginsForbidden),
            move |params: Option<serde_json::Value>| {
                let io = io12.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let recording_id_str = params
                        .get("recording_id")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'recording_id' parameter")
                        })?;
                    let recording_id: uuid::Uuid = recording_id_str.parse().map_err(|_| {
                        shux_rpc::RpcError::invalid_params("invalid recording_id format")
                    })?;

                    let recorder = {
                        let mut state = io.lock().await;
                        let mut found = None;
                        for recorders in state.recorders.values_mut() {
                            if let Some(pos) = recorders.iter().position(|r| r.id == recording_id) {
                                found = Some(recorders.remove(pos));
                                break;
                            }
                        }
                        state.recorders.retain(|_, recorders| !recorders.is_empty());
                        found
                    }
                    .ok_or_else(|| {
                        shux_rpc::RpcError::not_found("pane recording", recording_id_str)
                    })?;

                    let should_finish = recorder
                        .outcome
                        .lock()
                        .expect("record outcome poisoned")
                        .status
                        == PaneRecordStatus::Recording;
                    if should_finish {
                        let _ = recorder
                            .sender
                            .send(PaneRecordChunk::Finish {
                                status: PaneRecordStatus::Complete,
                            })
                            .await;
                    }
                    drop(recorder.sender);
                    let join_result =
                        tokio::time::timeout(std::time::Duration::from_secs(5), recorder.task)
                            .await;
                    match join_result {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            let mut outcome =
                                recorder.outcome.lock().expect("record outcome poisoned");
                            outcome.status = PaneRecordStatus::Error;
                            outcome.error = Some(format!("pane recorder task failed: {e}"));
                        }
                        Err(_) => {
                            let mut outcome =
                                recorder.outcome.lock().expect("record outcome poisoned");
                            if outcome.status == PaneRecordStatus::Recording {
                                outcome.status = PaneRecordStatus::Error;
                                outcome.error =
                                    Some("timed out while finalizing pane recording".to_string());
                            }
                        }
                    }
                    let result = recorder
                        .outcome
                        .lock()
                        .expect("record outcome poisoned")
                        .clone();

                    Ok(serde_json::json!({
                        "recording_id": recording_id.to_string(),
                        "path": recorder.path.display().to_string(),
                        "bytes_written": result.bytes_written,
                        "status": result.status.as_str(),
                        "lossless": result.status == PaneRecordStatus::Complete,
                        "error": result.error,
                    }))
                }
            },
        )
        .register_with_policy(
            "pane.snapshot",
            Policy::fixed(Sensitivity::ContentRead),
            move |params: Option<serde_json::Value>| {
                let gh = g6.clone();
                let io = io6.clone();
                let r = rasterizer_pane.load_full();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;

                    // Read visible dims FIRST, validate the pixel budget BEFORE
                    // any allocation, then clone only the visible viewport (not
                    // scrollback). Codex review (PR #16): cloning the full Grid
                    // under lock paid hundreds of MB of allocations even on
                    // snapshots that were about to be rejected by the cap,
                    // because the default 5000-line scrollback was being copied
                    // unconditionally.
                    let (cw, ch) = r.cell_size();
                    let (grid_snapshot, cursor_pos, snap_cols, snap_rows, default_colors) = {
                        let state = io.lock().await;
                        let vt = state.vts.get(&pane_id).ok_or_else(|| {
                            shux_rpc::RpcError::not_found("pane VT", &pane_id.to_string())
                        })?;
                        let cols = vt.grid().cols();
                        let rows = vt.grid().rows();
                        // 16 M output pixels (~64 MB RGBA, ~4000x4000 px).
                        let pixel_count = (cols as u64)
                            .saturating_mul(cw as u64)
                            .saturating_mul(rows as u64)
                            .saturating_mul(ch as u64);
                        const MAX_PIXELS: u64 = 16_000_000;
                        if pixel_count > MAX_PIXELS {
                            return Err(shux_rpc::RpcError::invalid_params(&format!(
                                "snapshot would be {pixel_count} pixels — exceeds cap of \
                            {MAX_PIXELS}; resize the pane first via pane.set_size"
                            )));
                        }
                        let cur = vt.cursor();
                        let cursor_pos = cur.visible.then_some((cur.row, cur.col, cur.shape));
                        let default_colors = vt.default_colors();
                        // Visible-only clone — does NOT copy scrollback.
                        let grid_clone = vt.grid().clone_visible();
                        (grid_clone, cursor_pos, cols, rows, default_colors)
                    };

                    // Rasterize + PNG-encode off the runtime worker. Both are
                    // pure-CPU and don't yield, so we route them to a blocking
                    // worker that won't starve other RPC handlers.
                    let (img, png_buf) = tokio::task::spawn_blocking(move || {
                        let opts = shux_raster::RasterOptions {
                            cursor: cursor_pos.map(|(row, col, _)| (row, col)),
                            cursor_shape: cursor_pos.map(|(_, _, shape)| shape).unwrap_or_default(),
                            cursor_color: default_colors.cursor,
                            fg_default: default_colors.fg.unwrap_or_else(|| {
                                shux_raster::RasterOptions::default().fg_default
                            }),
                            bg_default: default_colors.bg.unwrap_or_else(|| {
                                shux_raster::RasterOptions::default().bg_default
                            }),
                        };
                        let img = r.render(&grid_snapshot, &opts);
                        let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
                        {
                            use image::ImageEncoder;
                            let encoder = image::codecs::png::PngEncoder::new(&mut buf);
                            encoder
                                .write_image(
                                    img.as_raw(),
                                    img.width(),
                                    img.height(),
                                    image::ExtendedColorType::Rgba8,
                                )
                                .map_err(|e| format!("PNG encode failed: {e}"))?;
                        }
                        Ok::<_, String>((img, buf))
                    })
                    .await
                    .map_err(|e| shux_rpc::RpcError::internal(&format!("rasterize join: {e}")))?
                    .map_err(|e| shux_rpc::RpcError::internal(&e))?;

                    use base64::Engine;
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&png_buf);

                    Ok(serde_json::json!({
                        "pane_id": pane_id.to_string(),
                        "png_base64": b64,
                        "width": img.width(),
                        "height": img.height(),
                        "cell_width": cw,
                        "cell_height": ch,
                        "cols": snap_cols,
                        "rows": snap_rows,
                        "format": "png",
                    }))
                }
            },
        )
        .register_with_policy(
            "pane.glance",
            Policy::fixed(Sensitivity::ContentRead),
            move |params: Option<serde_json::Value>| {
                let gh = g12.clone();
                let io = io13.clone();
                let r = rasterizer_glance.load_full();
                let audit = audit_glance.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;

                    let include_cursor = params
                        .get("include_cursor")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    let include_png = params
                        .get("include_png")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    let want_checkpoint = params
                        .get("checkpoint")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    // Lens-gate capture emission (task 080): `include_cells` returns the
                    // canonical `FrameEnvelope` (078 schema) for the current viewport;
                    // `masks` are redaction rects applied BEFORE serialize/hash — and,
                    // when present, ALSO to the returned `text`/`png` so a secret never
                    // leaks (council D4). Default (no masks) leaves text/png byte-
                    // identical to today.
                    let include_cells = params
                        .get("include_cells")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let masks = parse_glance_masks(&params)?;

                    // LENS-R-010: ONE atomic clone under the pane's state
                    // lock — grid, cursor {row,col,visible}, size,
                    // alt_screen, dynamic default colors, ContentRevision —
                    // all read from the same critical section. Render + text
                    // extraction happen from THIS clone, outside the lock
                    // (LENS-R-011): same revision guaranteed for both.
                    let (cw, ch) = r.cell_size();
                    let (
                        revision,
                        cursor_row,
                        cursor_col,
                        cursor_visible,
                        cursor_shape,
                        alt_screen,
                        snap_cols,
                        snap_rows,
                        default_colors,
                        palette_overridden,
                        grid_snapshot,
                    ) = {
                        let state = io.lock().await;
                        let vt = state.vts.get(&pane_id).ok_or_else(|| {
                            shux_rpc::RpcError::not_found("pane VT", &pane_id.to_string())
                        })?;
                        let cols = vt.grid().cols();
                        let rows = vt.grid().rows();
                        // Pre-render pixel budget (codex PR #89 P1): reject
                        // over-budget panes BEFORE any clone/render/encode —
                        // same 16M-pixel cap as `pane.snapshot`, but mapped
                        // to PAYLOAD_TOO_LARGE (-32013) per §5.2. Without
                        // this, a 1000×1000 pane forced the daemon to
                        // allocate + encode hundreds of MB of RGBA before
                        // the post-encode 8 MiB check could fire. Text-only
                        // glances skip it: no PNG payload exists to cap.
                        // Shared with the diff heat path (PR #91 codex P1).
                        if include_png {
                            lens_pixel_budget_check(
                                cols,
                                rows,
                                cw,
                                ch,
                                "shrink the pane (pane.set_size) or set include_png=false",
                            )?;
                        }
                        let cur = vt.cursor();
                        let default_colors = vt.default_colors();
                        // Sticky OSC-4 override bit (task 080): read in the SAME critical
                        // section as the grid so the emitted `cells` envelope is
                        // revision-consistent (glance did not read it before — council Q5).
                        let palette_overridden = vt.palette_overridden();
                        // Visible-only clone — no scrollback (LENS-R-012).
                        let grid_clone = vt.grid().clone_visible();
                        (
                            vt.content_revision(),
                            cur.row,
                            cur.col,
                            cur.visible,
                            cur.shape,
                            vt.is_alternate_screen(),
                            cols,
                            rows,
                            default_colors,
                            palette_overridden,
                            grid_clone,
                        )
                    };

                    // Lens-gate capture (task 080): build the canonical envelope from
                    // the SAME clone when `cells` are requested OR masks must redact the
                    // emitted content. Masks are applied in `from_snapshot` — before any
                    // serialize/hash (council D4).
                    let capture_env = if include_cells || !masks.is_empty() {
                        Some(shux_vt::FrameEnvelope::from_snapshot(
                            &grid_snapshot,
                            cursor_row as u16,
                            cursor_col as u16,
                            cursor_visible,
                            cursor_shape,
                            default_colors,
                            alt_screen,
                            palette_overridden,
                            &masks,
                        ))
                    } else {
                        None
                    };

                    // When masks apply, present the MASKED reconstruction to text + PNG
                    // so a secret never reaches `text`/`png` either (council D4). Owns a
                    // Grid only on the masked path; the default path is byte-unchanged.
                    let masked_present = if !masks.is_empty() {
                        Some(capture_env.as_ref().expect("built when masked").to_grid())
                    } else {
                        None
                    };

                    // The cursor column to PRESENT (rendered PNG + response field): clamp
                    // to the mask origin when the cursor sits inside a redacted rect, so a
                    // masked secret's LENGTH does not leak via the drawn/reported cursor
                    // (council impl-review BLOCKER — the `cells` envelope clamps in
                    // `from_snapshot`, but the daemon's own render + response bypassed it).
                    // Checkpoints keep the RAW cursor (internal state, never a golden).
                    let present_cursor_col = masks
                        .cursor_redaction_col(cursor_row as u16, cursor_col as u16)
                        .map(|c| c as usize)
                        .unwrap_or(cursor_col);

                    // Text extraction (LENS-R-012), outside the lock: ANSI-free,
                    // full-width rows (no trim), joined by `\n`, no scrollback.
                    let text = match &masked_present {
                        Some(g) => g.glance_text(),
                        None => grid_snapshot.glance_text(),
                    };

                    // Checkpoints feed `pane.diff_since` (internal state, never a
                    // golden), so they store the REAL (unmasked) clone. Clone first so
                    // the render source can then MOVE the appropriate grid.
                    let checkpoint_grid = want_checkpoint.then(|| grid_snapshot.clone());
                    // Render source: the masked reconstruction when masks apply, else the
                    // raw clone (moved — checkpoint already cloned it if needed).
                    let render_grid: Option<shux_vt::Grid> = if include_png {
                        Some(masked_present.unwrap_or(grid_snapshot))
                    } else {
                        None
                    };

                    // PNG rendering (LENS-R-013): reuses shux-raster
                    // unchanged, cursor drawn iff visible AND include_cursor
                    // (default true) — identical policy to `pane.snapshot`.
                    //
                    // `default_colors` below comes from OSC 10/11/12 — the
                    // exact same wiring `pane.snapshot` already uses
                    // (vt.default_colors() → RasterOptions.{fg,bg,cursor}
                    // _default). Per the P2 re-adjudication of §4.2's OSC
                    // row, dynamic-default-color changes are Class A (they
                    // bump ContentRevision — revision tracks the PRESENTED
                    // frame), so a revision-watching caller can no longer
                    // miss a color-only repaint. Residual known limitation:
                    // OSC 4 palette redefinition remains Class B.
                    let png_base64 = if let Some(render_grid) = render_grid {
                        let render_cursor = include_cursor && cursor_visible;
                        let cursor_pos = render_cursor.then_some((cursor_row, present_cursor_col));
                        let cursor_shape = if render_cursor {
                            cursor_shape
                        } else {
                            shux_vt::CursorShape::default()
                        };
                        let opts = shux_raster::RasterOptions {
                            cursor: cursor_pos,
                            cursor_shape,
                            cursor_color: default_colors.cursor,
                            fg_default: default_colors.fg.unwrap_or_else(|| {
                                shux_raster::RasterOptions::default().fg_default
                            }),
                            bg_default: default_colors.bg.unwrap_or_else(|| {
                                shux_raster::RasterOptions::default().bg_default
                            }),
                        };
                        let png_buf = tokio::task::spawn_blocking(move || {
                            let img = r.render(&render_grid, &opts);
                            let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
                            {
                                use image::ImageEncoder;
                                let encoder = image::codecs::png::PngEncoder::new(&mut buf);
                                encoder
                                    .write_image(
                                        img.as_raw(),
                                        img.width(),
                                        img.height(),
                                        image::ExtendedColorType::Rgba8,
                                    )
                                    .map_err(|e| format!("PNG encode failed: {e}"))?;
                            }
                            Ok::<_, String>(buf)
                        })
                        .await
                        .map_err(|e| shux_rpc::RpcError::internal(&format!("rasterize join: {e}")))?
                        .map_err(|e| shux_rpc::RpcError::internal(&e))?;

                        // §5.2: PAYLOAD_TOO_LARGE at 8 MiB DECODED (before
                        // base64, which would inflate it further).
                        const MAX_PNG_BYTES: usize = 8 * 1024 * 1024;
                        if png_buf.len() > MAX_PNG_BYTES {
                            return Err(shux_rpc::RpcError::payload_too_large(
                                png_buf.len(),
                                MAX_PNG_BYTES,
                            ));
                        }

                        use base64::Engine;
                        Some(base64::engine::general_purpose::STANDARD.encode(&png_buf))
                    } else {
                        None
                    };

                    // Checkpoint storage (§7 LENS-R-030/031): a second, short
                    // lock acquisition — keyed by `revision`, the SAME value
                    // read alongside the clone above (not re-read here); that
                    // clone IS the checkpoint (LENS-R-014). store_checkpoint
                    // refuses if the pane was torn down between the two lock
                    // windows (codex P2 review major: no resurrection of
                    // checkpoint state for a dead pane); `checkpointed` then
                    // honestly reports false.
                    let (checkpointed, evicted_revision) = if let Some(grid) = checkpoint_grid {
                        let mut state = io.lock().await;
                        state.store_checkpoint(
                            pane_id,
                            revision,
                            grid,
                            (cursor_row, cursor_col, cursor_visible),
                            // The §5.1 clone's OSC defaults (LENS-R-038b) —
                            // read in the SAME critical section as the grid.
                            default_colors,
                        )
                    } else {
                        (false, None)
                    };

                    // LENS-R-052: audit the successful glance (P5 round-1
                    // codex M2a — the spec's field list: ts, caller, method,
                    // pane_id, revision(s), bytes_returned). bytes_returned
                    // counts the DECODED payload (viewport text + PNG bytes
                    // before base64).
                    let png_decoded_len = png_base64
                        .as_ref()
                        .map(|b64| lens_scratch::b64_decoded_len(b64))
                        .unwrap_or(0);
                    audit.append(serde_json::json!({
                        "ts": lens_scratch::iso_now(),
                        "caller": shux_rpc::current_caller(),
                        "method": "pane.glance",
                        "pane_id": pane_id.to_string(),
                        "revision": revision,
                        "bytes_returned": text.len() + png_decoded_len,
                    }));

                    let mut result = serde_json::json!({
                        "revision": revision,
                        "cols": snap_cols,
                        "rows": snap_rows,
                        "cursor": {
                            "row": cursor_row,
                            // Clamped col (BLOCKER): the reported cursor must not leak a
                            // masked secret's length either.
                            "col": present_cursor_col,
                            "visible": cursor_visible,
                        },
                        "alt_screen": alt_screen,
                        "text": text,
                        "png_base64": png_base64,
                        "checkpointed": checkpointed,
                        "evicted_revision": evicted_revision,
                    });
                    // Emit the canonical `FrameEnvelope` ONLY when requested (task 080);
                    // absent by default keeps the frozen glance response byte-stable.
                    if include_cells && let Some(env) = &capture_env {
                        result["cells"] =
                            serde_json::to_value(env).unwrap_or(serde_json::Value::Null);
                    }
                    Ok(result)
                }
            },
        )
        .register_with_policy(
            // `pane.wait_settled` (§6 SPEC-C, LENS-R-020..025): block until the
            // pane has been quiet for `quiet_ms`, or return `settled=false` on
            // the server-side timeout deadline. Pure observation — same read
            // sensitivity as glance.
            "pane.wait_settled",
            Policy::fixed(Sensitivity::ContentRead),
            move |params: Option<serde_json::Value>| {
                let gh = g13.clone();
                let io = io14.clone();
                async move {
                    let params = params.unwrap_or_default();

                    // LENS-R-025 + codex P3 M2: strict typing first (absent →
                    // default; wrong type → INVALID_PARAMS, never a silent
                    // default), then bounds. CLI maps -32602 to exit 2 (§10).
                    let quiet_ms = settle_u64_param(&params, "quiet_ms", 300)?;
                    let timeout_ms = settle_u64_param(&params, "timeout_ms", 10_000)?;
                    validate_wait_settled_params(quiet_ms, timeout_ms)?;
                    // Task 083 frame-stability opt-ins (default 0/1 = off ⇒ pure quiet mode,
                    // backward compatible). `masks` scopes the stability hash to the same
                    // masked domain the golden compare uses (council #4).
                    let hold_ms = settle_u64_param(&params, "hold_ms", 0)?;
                    let stable_frames = settle_u32_param(&params, "stable_frames", 1)?;
                    validate_stability_params(hold_ms, stable_frames, timeout_ms)?;
                    let stability_masks = parse_glance_masks(&params)?;

                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;

                    // LENS-R-003/021: subscribe to the pane's revision watch.
                    // `subscribe()` seeds the receiver with the CURRENT published
                    // value AND catches every value published after — no
                    // lost-edge race (this is why the substrate is a `watch`,
                    // not a `Notify`). Clone the receiver out UNDER the io lock,
                    // then drop the lock before any `.await` (never hold the io
                    // mutex across an await — the daemon's cardinal deadlock).
                    let mut rx = {
                        let state = io.lock().await;
                        state
                            .revisions
                            .get(&pane_id)
                            .map(|tx| tx.subscribe())
                            .ok_or_else(|| {
                                shux_rpc::RpcError::not_found("pane VT", &pane_id.to_string())
                            })?
                    };

                    // LENS-R-022: server-side monotonic deadline measured from
                    // request acceptance. `waited_ms` is elapsed against the
                    // SAME instant. LENS-R-023: `rx` and every sleep below live
                    // inside this future, so a client disconnect (the router
                    // drops the future) drops the waiter — no daemon growth.
                    let accept = tokio::time::Instant::now();
                    let timeout_deadline = accept + std::time::Duration::from_millis(timeout_ms);
                    let waited_ms = |now: tokio::time::Instant| -> u64 {
                        now.saturating_duration_since(accept)
                            .as_millis()
                            .min(u128::from(u32::MAX)) as u64
                    };

                    // Task 083: frame-CONTENT stability modes (hold_ms / stable_frames). Default
                    // quiet mode (both off) falls through to the UNCHANGED loop below so S1..S5
                    // stay byte-identical. In stability mode quiet_ms never independently settles
                    // (council #1); it is unused here — the criteria are the frame-hash hold and
                    // the contiguous-revision run.
                    if hold_ms > 0 || stable_frames >= 2 {
                        return wait_settled_frame_stability(
                            &io,
                            pane_id,
                            &mut rx,
                            &stability_masks,
                            hold_ms,
                            stable_frames,
                            accept,
                            timeout_deadline,
                        )
                        .await;
                    }

                    // Event-driven loop (LENS-R-021): no polling, and each sleep
                    // is exactly the remaining quiet interval (capped by the
                    // timeout), woken early only by a genuine Class-A revision.
                    //
                    // Every wake — sleep expiry, watch wake, or a late
                    // scheduler wake — re-enters the top of this loop, so the
                    // quiet condition is ALWAYS re-evaluated before the
                    // timeout can fire (`settle_decide` precedence — codex P3
                    // B1: timeout returns only when quiet is still false at
                    // the deadline).
                    loop {
                        // `borrow_and_update` copies the latest (revision, ns)
                        // and marks it seen, so the next `changed()` fires only
                        // on a strictly newer Class-A batch (Class-B never
                        // publishes — LENS-R-024, S5 comes free).
                        let rev = *rx.borrow_and_update();
                        let now_ns = shux_vt::monotonic_now_ns();
                        let quiet = settle_is_quiet(now_ns, rev.last_mutation_ns, quiet_ms);
                        // TOCTOU guard (claude P3 review): a revision published
                        // AFTER the snapshot above must restart the evaluation
                        // — returning `settled:true` from the stale snapshot
                        // would report a pane as still that has already
                        // mutated again.
                        let pending = match rx.has_changed() {
                            Ok(p) => p,
                            Err(_) => {
                                // codex P3 M1: sender dropped ⇒ pane torn down
                                // mid-wait → NOT_FOUND (never settle on a
                                // frozen value); re-subscribe if a publisher
                                // somehow lives again (defensive).
                                rx = settle_reacquire_watch(&io, pane_id).await?;
                                continue;
                            }
                        };
                        let past_timeout = tokio::time::Instant::now() >= timeout_deadline;
                        match settle_decide(quiet, past_timeout, pending) {
                            SettleWake::Settled => {
                                return Ok(serde_json::json!({
                                    "settled": true,
                                    "revision": rev.content_revision,
                                    "waited_ms": waited_ms(tokio::time::Instant::now()),
                                }));
                            }
                            SettleWake::TimedOut => {
                                return Ok(serde_json::json!({
                                    "settled": false,
                                    "revision": rev.content_revision,
                                    "waited_ms": waited_ms(tokio::time::Instant::now()),
                                }));
                            }
                            SettleWake::KeepWaiting => {}
                        }
                        if pending {
                            // Fresh revision already queued — restart on it
                            // immediately (no sleep, no select).
                            continue;
                        }

                        let remaining = std::time::Duration::from_nanos(settle_remaining_quiet_ns(
                            now_ns,
                            rev.last_mutation_ns,
                            quiet_ms,
                        ));
                        let quiet_deadline = tokio::time::Instant::now() + remaining;
                        let wake = quiet_deadline.min(timeout_deadline);

                        tokio::select! {
                            changed = rx.changed() => {
                                if changed.is_err() {
                                    // codex P3 M1 (same rule as above): pane
                                    // teardown mid-wait → NOT_FOUND.
                                    rx = settle_reacquire_watch(&io, pane_id).await?;
                                }
                                // Loop re-evaluates on the fresh value.
                            }
                            _ = tokio::time::sleep_until(wake) => {
                                // Loop re-evaluates: quiet first, then
                                // timeout (settle_decide precedence).
                            }
                        }
                    }
                }
            },
        )
        .register_with_policy(
            // `pane.checkpoint` (§7 SPEC-D, LENS-R-030/031, DEC-22): capture the
            // pane's current visible grid clone keyed by its `content_revision`
            // for a later `pane.diff_since`. Cap 4 per pane, FIFO by creation
            // revision; re-checkpointing the same revision is a no-op
            // (`evicted_revision: null`). Pure observation of pane content plus
            // bounded daemon-side storage — same read sensitivity as glance.
            "pane.checkpoint",
            Policy::fixed(Sensitivity::ContentRead),
            move |params: Option<serde_json::Value>| {
                let gh = g14.clone();
                let io = io15.clone();
                let audit = audit_checkpoint.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;

                    // One lock: verify the VT exists (PANE_NOT_FOUND otherwise),
                    // clone the current visible grid + cursor keyed by the
                    // revision read in the SAME critical section, and store.
                    // store_checkpoint dedups the same-revision no-op and evicts
                    // the FIFO-oldest past the cap (LENS-R-030/031).
                    let (revision, evicted) = {
                        let mut state = io.lock().await;
                        let (revision, grid, cursor, default_colors) = {
                            let vt = state.vts.get(&pane_id).ok_or_else(|| {
                                shux_rpc::RpcError::not_found("pane VT", &pane_id.to_string())
                            })?;
                            let cur = vt.cursor();
                            (
                                vt.content_revision(),
                                vt.grid().clone_visible(),
                                (cur.row, cur.col, cur.visible),
                                // OSC defaults at capture time (LENS-R-038b),
                                // same critical section as the grid clone.
                                vt.default_colors(),
                            )
                        };
                        let (_stored, evicted) =
                            state.store_checkpoint(pane_id, revision, grid, cursor, default_colors);
                        (revision, evicted)
                    };

                    // LENS-R-052: audit the checkpoint. A checkpoint returns
                    // no pane content, so bytes_returned is 0 by definition.
                    audit.append(serde_json::json!({
                        "ts": lens_scratch::iso_now(),
                        "caller": shux_rpc::current_caller(),
                        "method": "pane.checkpoint",
                        "pane_id": pane_id.to_string(),
                        "revision": revision,
                        "bytes_returned": 0,
                    }));

                    Ok(serde_json::json!({
                        "revision": revision,
                        "evicted_revision": evicted,
                    }))
                }
            },
        )
        .register_with_policy(
            // `pane.diff_since` (§7 SPEC-D, LENS-R-033..038): diff the pane's
            // current visible grid against a checkpointed revision. Existence
            // FIRST — a missing pane is PANE_NOT_FOUND before any checkpoint
            // lookup; then the LENS-R-033 rule (exact checkpoint → diff; else
            // ≤ invalidation marker → RESIZE_INVALIDATED -32011; else
            // STALE_REVISION -32010 with `available`). Pure observation.
            "pane.diff_since",
            Policy::fixed(Sensitivity::ContentRead),
            move |params: Option<serde_json::Value>| {
                let gh = g15.clone();
                let io = io16.clone();
                let r = rasterizer_diff.load_full();
                let audit = audit_diff.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;

                    // `since_revision` is required; strict typing (missing /
                    // wrong type → INVALID_PARAMS, CLI exit 2).
                    let since_revision = match params.get("since_revision") {
                        Some(v) => v.as_u64().ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params(
                                "since_revision must be a non-negative integer",
                            )
                        })?,
                        None => {
                            return Err(shux_rpc::RpcError::invalid_params(
                                "since_revision is required",
                            ));
                        }
                    };
                    let want_row_text = params
                        .get("changed_row_text")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    let want_heat = params
                        .get("heat_png")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);

                    // One lock: existence check, checkpoint lookup (LENS-R-033),
                    // and the atomic current-grid clone all in one critical
                    // section so `to_revision`/grid/cursor agree.
                    let (cw, ch) = r.cell_size();
                    let (
                        cp_grid,
                        cp_cursor,
                        cp_defaults,
                        cur_grid,
                        cur_cursor,
                        to_revision,
                        default_colors,
                    ) = {
                        let state = io.lock().await;
                        let vt = state.vts.get(&pane_id).ok_or_else(|| {
                            shux_rpc::RpcError::not_found("pane VT", &pane_id.to_string())
                        })?;
                        // Existence-first lookup runs AFTER the pane check.
                        let (cp_grid, cp_cursor, cp_defaults) =
                            diff_lookup_checkpoint(&state, &pane_id, since_revision)?;
                        // Pre-render pixel budget for the heat path (PR #91
                        // codex P1): the SAME 16M-pixel cap glance enforces,
                        // checked BEFORE any RGBA allocation/rasterization —
                        // a 1000×1000 pane (valid per pane.set_size) would
                        // otherwise allocate hundreds of MB in
                        // render_lens_heat_png before the post-encode 8 MiB
                        // check could fire. Runs AFTER the LENS-R-033 lookup
                        // so stale/invalidated (more actionable) wins over
                        // the payload error; heat-less diffs skip it — the
                        // cell-level diff never rasterizes.
                        if want_heat {
                            lens_pixel_budget_check(
                                vt.grid().cols(),
                                vt.grid().rows(),
                                cw,
                                ch,
                                "shrink the pane (pane.set_size) or set heat_png=false",
                            )?;
                        }
                        let cur = vt.cursor();
                        (
                            cp_grid,
                            cp_cursor,
                            cp_defaults,
                            vt.grid().clone_visible(),
                            (cur.row, cur.col, cur.visible),
                            vt.content_revision(),
                            vt.default_colors(),
                        )
                    };

                    // Diff computation outside the lock (LENS-R-034..036;
                    // LENS-R-038b: Default colors resolve against each
                    // side's own defaults — the checkpoint's captured
                    // defaults vs the pane's CURRENT defaults).
                    // Thin adapter over the shux-vt comparator (task 079). Both
                    // frames are the same pane at equal dims (resize/alt-screen
                    // invalidate the checkpoint first), and pane.diff_since has no
                    // palette field, so `palette_overridden` is false on both sides:
                    // `geometry_changed` / `palette_overridden_differs` stay false and
                    // are never serialized — output byte-identical to pre-refactor.
                    let diff = {
                        let cp_view = shux_vt::GridFrame::new(
                            &cp_grid,
                            cp_defaults,
                            shux_vt::CursorState {
                                row: cp_cursor.0,
                                col: cp_cursor.1,
                                visible: cp_cursor.2,
                            },
                            false,
                        );
                        let cur_view = shux_vt::GridFrame::new(
                            &cur_grid,
                            default_colors,
                            shux_vt::CursorState {
                                row: cur_cursor.0,
                                col: cur_cursor.1,
                                visible: cur_cursor.2,
                            },
                            false,
                        );
                        shux_vt::diff_frames(&cp_view, &cur_view)
                    };

                    let regions: Vec<serde_json::Value> = diff
                        .regions
                        .iter()
                        .map(|s| {
                            serde_json::json!({
                                "row": s.row,
                                "col_start": s.col_start,
                                "col_end": s.col_end,
                            })
                        })
                        .collect();

                    let changed_row_text = if want_row_text {
                        let mut map = serde_json::Map::new();
                        for &row in &diff.changed_rows {
                            map.insert(
                                row.to_string(),
                                serde_json::Value::String(glance_row_text(&cur_grid, row)),
                            );
                        }
                        serde_json::Value::Object(map)
                    } else {
                        serde_json::Value::Object(serde_json::Map::new())
                    };

                    // Heat PNG (LENS-R-037): render off the runtime worker.
                    // The base frame uses the pane's CURRENT defaults
                    // (`default_colors`, read in the lock above) — the heat
                    // map depicts the PRESENTED current frame, never the
                    // checkpoint's colors (LENS-R-038b test c). The mask is
                    // MOVED into the closure — nothing reads it afterwards
                    // (greptile PR #91: the clone was a needless heap copy
                    // of rows×cols booleans).
                    let heat_png_base64 = if want_heat {
                        let changed_mask = diff.changed_mask;
                        let (rows, cols) = (diff.rows, diff.cols);
                        let heat = tokio::task::spawn_blocking(move || {
                            render_lens_heat_png(
                                &r,
                                &cur_grid,
                                default_colors,
                                &changed_mask,
                                rows,
                                cols,
                            )
                        })
                        .await
                        .map_err(|e| {
                            shux_rpc::RpcError::internal(&format!("heat rasterize join: {e}"))
                        })?
                        .map_err(|e| shux_rpc::RpcError::internal(&e))?;

                        // §7.3 shares glance's 8 MiB decoded-PNG cap.
                        const MAX_PNG_BYTES: usize = 8 * 1024 * 1024;
                        if heat.len() > MAX_PNG_BYTES {
                            return Err(shux_rpc::RpcError::payload_too_large(
                                heat.len(),
                                MAX_PNG_BYTES,
                            ));
                        }
                        use base64::Engine;
                        Some(base64::engine::general_purpose::STANDARD.encode(&heat))
                    } else {
                        None
                    };

                    // LENS-R-052: audit the diff with BOTH revisions
                    // ("revision(s)" per the spec's field list).
                    // bytes_returned counts the decoded payload: changed row
                    // text + heat PNG bytes before base64.
                    let row_text_len: usize = changed_row_text
                        .as_object()
                        .map(|m| m.values().filter_map(|v| v.as_str()).map(str::len).sum())
                        .unwrap_or(0);
                    let heat_decoded_len = heat_png_base64
                        .as_ref()
                        .map(|b64| lens_scratch::b64_decoded_len(b64))
                        .unwrap_or(0);
                    audit.append(serde_json::json!({
                        "ts": lens_scratch::iso_now(),
                        "caller": shux_rpc::current_caller(),
                        "method": "pane.diff_since",
                        "pane_id": pane_id.to_string(),
                        "from_revision": since_revision,
                        "to_revision": to_revision,
                        "bytes_returned": row_text_len + heat_decoded_len,
                    }));

                    let (bb_rs, bb_cs, bb_re, bb_ce) = diff.bounding_box;
                    Ok(serde_json::json!({
                        "from_revision": since_revision,
                        "to_revision": to_revision,
                        "cells_changed": diff.cells_changed,
                        "cursor_moved": diff.cursor_moved,
                        "regions": regions,
                        "regions_truncated": diff.regions_truncated,
                        "bounding_box": {
                            "row_start": bb_rs,
                            "col_start": bb_cs,
                            "row_end": bb_re,
                            "col_end": bb_ce,
                        },
                        "changed_row_text": changed_row_text,
                        "heat_png_base64": heat_png_base64,
                    }))
                }
            },
        )
        .register_with_policy(
            "window.snapshot",
            Policy::fixed(Sensitivity::ContentRead),
            {
                let cfg = config.clone();
                let meta = meta_cache.clone();
                let onb = onboarding.clone();
                let segs = segments.clone();
                move |params: Option<serde_json::Value>| {
                    let gh = g8.clone();
                    let io = io8.clone();
                    let r = rasterizer_window.load_full();
                    let cfg = cfg.clone();
                    let meta = meta.clone();
                    let onb = onb.clone();
                    let segs = segs.clone();
                    async move {
                        let params = params.unwrap_or_default();
                        let window_id = resolve_window_id_from_params(&gh, &params)?;
                        let (cols, rows) = parse_snapshot_dims(&params)?;
                        let snap = gh.snapshot();
                        let (result, _revisions) = snapshot_window(
                            &snap,
                            &io,
                            window_id,
                            cols,
                            rows,
                            r,
                            &cfg,
                            &meta,
                            &onb,
                            &segs,
                            &[],
                        )
                        .await?;
                        Ok(result)
                    }
                }
            },
        )
        .register_with_policy(
            "session.snapshot",
            Policy::fixed(Sensitivity::ContentRead),
            {
                let cfg = config.clone();
                let meta = meta_cache.clone();
                let onb = onboarding.clone();
                let segs = segments.clone();
                move |params: Option<serde_json::Value>| {
                    let gh = g9.clone();
                    let io = io9.clone();
                    let r = rasterizer_session.load_full();
                    let cfg = cfg.clone();
                    let meta = meta.clone();
                    let onb = onb.clone();
                    let segs = segs.clone();
                    async move {
                        let params = params.unwrap_or_default();
                        let session_id_str = required_str(&params, "session_id")?;
                        let session_id = resolve_session_ref(&gh, session_id_str, "session_id")?;
                        let snap = gh.snapshot();
                        let session = snap.sessions.get(&session_id).ok_or_else(|| {
                            shux_rpc::RpcError::not_found("session", session_id_str)
                        })?;
                        let window_id = session.active_window;
                        // LENS-R-006 (P1): collect every pane in the session with
                        // its structural entity `version`. The `content_revision`
                        // (§4 substrate) is read from each pane's VT below. This is
                        // the ONLY public exposure of the counter until pane.glance
                        // ships in P2 — it is what lets G3/G4 go green.
                        let session_version: u64 = session.version;
                        let pane_meta: Vec<(shux_core::model::PaneId, u64)> = session
                            .windows
                            .iter()
                            .filter_map(|wid| snap.windows.get(wid))
                            .flat_map(|win| win.layout.tree.pane_ids())
                            .filter_map(|pid| snap.panes.get(&pid).map(|p| (pid, p.version)))
                            .collect();
                        let (cols, rows) = parse_snapshot_dims(&params)?;
                        // Council major 5: render from the SAME snapshot the
                        // session_version/panes[] metadata above came from —
                        // a second gh.snapshot() here could interleave with a
                        // concurrent structural mutation and tear the result.
                        //
                        // PR #87 bot P1 (codex + greptile): content_revisions
                        // are captured INSIDE snapshot_window's io-lock clone
                        // pass — the same critical section that clones the VT
                        // grids for rendering — so pixels and revisions are
                        // provably same-lock (a second lock read here let an
                        // old PNG pair with a newer revision). Plain reads:
                        // never touches DirtyState or render-consumed state
                        // (LENS-R-004).
                        let revision_pane_ids: Vec<shux_core::model::PaneId> =
                            pane_meta.iter().map(|(pid, _)| *pid).collect();
                        let (mut result, content_revs) = snapshot_window(
                            &snap,
                            &io,
                            window_id,
                            cols,
                            rows,
                            r,
                            &cfg,
                            &meta,
                            &onb,
                            &segs,
                            &revision_pane_ids,
                        )
                        .await?;
                        // A graph pane without a VT is REACHABLE via a
                        // snapshot/kill race (TOCTOU): the graph snapshot is a
                        // point-in-time copy, so a pane killed between
                        // `gh.snapshot()` and this PaneIoState read has its VT
                        // already removed. Skip is the correct behavior — OMIT
                        // the entry rather than emit content_revision: 0
                        // (LENS-R-001 starts the counter at 1, so 0 is a lie),
                        // matching snapshot_window's established filter_map
                        // handling of VT-less panes. No assert: panicking on a
                        // legitimate race is wrong (council claude-r2 major).
                        let panes_json: Vec<serde_json::Value> = pane_meta
                            .iter()
                            .filter_map(|(pid, version)| match content_revs.get(pid) {
                                Some(rev) => Some(serde_json::json!({
                                    "pane_id": pid.to_string(),
                                    "version": version,
                                    "content_revision": rev,
                                })),
                                None => {
                                    tracing::warn!(
                                        %pid,
                                        "session.snapshot: graph pane has no VT \
                                         (killed since snapshot); omitting from \
                                         panes[] (never emit revision 0)"
                                    );
                                    None
                                }
                            })
                            .collect();
                        if let Some(obj) = result.as_object_mut() {
                            obj.insert(
                                "session_version".to_string(),
                                serde_json::json!(session_version),
                            );
                            obj.insert("panes".to_string(), serde_json::json!(panes_json));
                        }
                        Ok(result)
                    }
                }
            },
        )
        .register_with_policy(
            "pane.set_size",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g7.clone();
                let io = io7.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;
                    // Validate in u64-space BEFORE narrowing — `as u16` silently
                    // wraps `cols=66536` to 1000 and lets it through (codex
                    // review). Sanity bounds: 4..=1000 cols, 2..=1000 rows.
                    let cols_u64 = params
                        .get("cols")
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'cols'"))?;
                    let rows_u64 = params
                        .get("rows")
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'rows'"))?;
                    if !(4..=1000).contains(&cols_u64) || !(2..=1000).contains(&rows_u64) {
                        return Err(shux_rpc::RpcError::invalid_params(&format!(
                            "rows/cols out of range (got rows={rows_u64} cols={cols_u64}; \
                        valid: 4..=1000 cols, 2..=1000 rows)"
                        )));
                    }
                    let cols = cols_u64 as u16;
                    let rows = rows_u64 as u16;
                    let pty_size = shux_pty::handle::PtySize { rows, cols };

                    // Construct a oneshot ack and await it (with a short timeout
                    // so a deadlocked PTY task can't hang the RPC). Synchronous
                    // semantics: when this RPC returns Ok, `vt.grid().cols/rows`
                    // already reflect the new size and a follow-up pane.snapshot
                    // will capture at the requested resolution.
                    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel::<()>();
                    let resizer = {
                        let state = io.lock().await;
                        state.resizers.get(&pane_id).cloned()
                    };
                    let resizer = resizer.ok_or_else(|| {
                        shux_rpc::RpcError::not_found("pane resizer", &pane_id.to_string())
                    })?;
                    resizer
                        .send(ResizeRequest {
                            size: pty_size,
                            ack: Some(ack_tx),
                        })
                        .await
                        .map_err(|_| shux_rpc::RpcError::internal("pane resize channel closed"))?;
                    tokio::time::timeout(std::time::Duration::from_secs(2), ack_rx)
                        .await
                        .map_err(|_| {
                            shux_rpc::RpcError::internal("pane resize ack timed out after 2s")
                        })?
                        .map_err(|_| {
                            shux_rpc::RpcError::internal("pane resize ack channel dropped")
                        })?;
                    Ok(serde_json::json!({
                        "pane_id": pane_id.to_string(),
                        "rows": rows,
                        "cols": cols,
                    }))
                }
            },
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane_io::PaneRevision;
    use crate::pane_record::tee_pane_recorders;
    use tokio_util::sync::CancellationToken;

    use crate::rpc::test_harness::{RpcHarness, dispatch_err, dispatch_ok};
    use tokio::sync::watch;

    /// codex PR #89 P1 — the glance pixel-budget guard must fire BEFORE any
    /// render/encode work (pane.snapshot's MAX_PIXELS equivalent, mapped to
    /// PAYLOAD_TOO_LARGE -32013), and a text-only glance on the same
    /// oversized pane must still succeed (no PNG payload exists to cap).
    #[tokio::test]
    async fn production_glance_rejects_over_budget_panes_before_render() {
        let harness = RpcHarness::new();
        let (_sid, _wid, pane_id) = harness.seed_session("glance-budget").await;
        let _writer_rx = harness.seed_io(pane_id, b"budget probe").await;

        // Grow the pane to pane.set_size's maximum: 1000x1000 cells is far
        // beyond the 16M-pixel raster budget at the bundled font's metrics.
        let resized = dispatch_ok(
            &harness.router,
            "pane.set_size",
            serde_json::json!({"pane_id": pane_id.to_string(), "cols": 1000, "rows": 1000}),
        )
        .await;
        assert_eq!(resized["cols"], 1000);

        let err = dispatch_err(
            &harness.router,
            "pane.glance",
            serde_json::json!({"pane_id": pane_id.to_string()}),
        )
        .await;
        assert_eq!(
            err.code,
            shux_rpc::ErrorCode::PayloadTooLarge.code(),
            "over-budget glance must map to PAYLOAD_TOO_LARGE (-32013)"
        );
        let data = err.data.expect("guard error carries data");
        assert!(data["pixels"].as_u64().unwrap() > data["max_pixels"].as_u64().unwrap());

        // Text-only glance on the SAME oversized pane succeeds — the guard
        // only protects the render path.
        let ok = dispatch_ok(
            &harness.router,
            "pane.glance",
            serde_json::json!({"pane_id": pane_id.to_string(), "include_png": false}),
        )
        .await;
        assert_eq!(ok["cols"], 1000);
        assert!(ok["png_base64"].is_null());
        assert!(ok["text"].as_str().unwrap().contains("budget probe"));

        harness.stop().await;
    }

    #[tokio::test]
    async fn pane_record_routes_capture_source_bytes_losslessly() {
        let harness = RpcHarness::new();
        let (_session_id, _window_id, pane_id) = harness.seed_session("record").await;
        let _writer_rx = harness.seed_io(pane_id, b"ready\n").await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("raw-output.bin");

        let started = dispatch_ok(
            &harness.router,
            "pane.record.start",
            serde_json::json!({
                "pane_id": pane_id.to_string(),
                "path": path.display().to_string(),
            }),
        )
        .await;
        assert_eq!(started["lossless"], true);
        assert_eq!(started["backpressure"], true);
        let recording_id = started["recording_id"].as_str().unwrap().to_string();

        let mut payload = Vec::new();
        for i in 0..8192u32 {
            payload.extend_from_slice(b"\x1b[2Jframe:");
            payload.extend_from_slice(i.to_string().as_bytes());
            payload.push(b'\n');
        }
        assert!(
            payload.len() > 64 * 1024,
            "payload should exceed sampled pane.output pending cap"
        );
        tee_pane_recorders(&harness.io, pane_id, &payload, &harness.cancel).await;

        let stopped = dispatch_ok(
            &harness.router,
            "pane.record.stop",
            serde_json::json!({
                "recording_id": recording_id,
            }),
        )
        .await;
        assert_eq!(stopped["status"], "complete");
        assert_eq!(stopped["lossless"], true);
        assert_eq!(stopped["bytes_written"], payload.len() as u64);
        let recorded = tokio::fs::read(dir.path().join("raw-output.bin"))
            .await
            .unwrap();
        assert_eq!(recorded, payload);

        harness.stop().await;
    }

    #[tokio::test]
    async fn pane_record_start_rejects_duplicate_active_recorder_for_pane() {
        let harness = RpcHarness::new();
        let (_session_id, _window_id, pane_id) = harness.seed_session("record-duplicate").await;
        let _writer_rx = harness.seed_io(pane_id, b"ready\n").await;
        let dir = tempfile::tempdir().unwrap();
        let first_path = dir.path().join("first.bin");
        let second_path = dir.path().join("second.bin");

        let started = dispatch_ok(
            &harness.router,
            "pane.record.start",
            serde_json::json!({
                "pane_id": pane_id.to_string(),
                "path": first_path.display().to_string(),
            }),
        )
        .await;
        let err = dispatch_err(
            &harness.router,
            "pane.record.start",
            serde_json::json!({
                "pane_id": pane_id.to_string(),
                "path": second_path.display().to_string(),
            }),
        )
        .await;
        assert_eq!(err.code, shux_rpc::ErrorCode::NameConflict.code());

        let _ = dispatch_ok(
            &harness.router,
            "pane.record.stop",
            serde_json::json!({
                "recording_id": started["recording_id"].as_str().unwrap(),
            }),
        )
        .await;
        harness.stop().await;
    }

    #[tokio::test]
    async fn pane_record_duration_stops_on_daemon_side() {
        let harness = RpcHarness::new();
        let (_session_id, _window_id, pane_id) = harness.seed_session("record-duration").await;
        let _writer_rx = harness.seed_io(pane_id, b"ready\n").await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("duration.bin");

        let started = dispatch_ok(
            &harness.router,
            "pane.record.start",
            serde_json::json!({
                "pane_id": pane_id.to_string(),
                "path": path.display().to_string(),
                "duration_ms": 25,
            }),
        )
        .await;
        let recording_id = started["recording_id"].as_str().unwrap().to_string();
        tee_pane_recorders(&harness.io, pane_id, b"before-deadline", &harness.cancel).await;
        tokio::time::sleep(std::time::Duration::from_millis(75)).await;
        tee_pane_recorders(&harness.io, pane_id, b"after-deadline", &harness.cancel).await;

        let stopped = dispatch_ok(
            &harness.router,
            "pane.record.stop",
            serde_json::json!({
                "recording_id": recording_id,
            }),
        )
        .await;
        assert_eq!(stopped["status"], "complete");
        assert_eq!(stopped["bytes_written"], "before-deadline".len() as u64);
        assert_eq!(
            tokio::fs::read(dir.path().join("duration.bin"))
                .await
                .unwrap(),
            b"before-deadline"
        );

        harness.stop().await;
    }

    #[tokio::test]
    async fn pane_record_start_refuses_existing_file_without_overwrite() {
        let harness = RpcHarness::new();
        let (_session_id, _window_id, pane_id) = harness.seed_session("record-exists").await;
        let _writer_rx = harness.seed_io(pane_id, b"ready\n").await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing.bin");
        tokio::fs::write(&path, b"keep").await.unwrap();

        let err = dispatch_err(
            &harness.router,
            "pane.record.start",
            serde_json::json!({
                "pane_id": pane_id.to_string(),
                "path": path.display().to_string(),
            }),
        )
        .await;
        assert_eq!(err.code, shux_rpc::ErrorCode::InvalidParams.code());
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"keep");

        harness.stop().await;
    }

    #[tokio::test]
    async fn production_pane_io_routes_cover_writes_capture_wait_resize_and_commands() {
        let harness = RpcHarness::new();
        let (session_id, window_id, pane_id) = harness.seed_session("io").await;
        let mut writer_rx = harness
            .seed_io(pane_id, b"boot complete\nagent-ready\n")
            .await;

        let sent = dispatch_ok(
            &harness.router,
            "pane.send_keys",
            serde_json::json!({"pane_id": pane_id.to_string(), "text": "echo hi\n"}),
        )
        .await;
        assert_eq!(sent["bytes_written"], 8);
        assert_eq!(writer_rx.recv().await.unwrap(), b"echo hi\n");

        let sent_b64 = dispatch_ok(
            &harness.router,
            "pane.send_keys",
            serde_json::json!({"pane_id": pane_id.to_string(), "data": "A03/"}),
        )
        .await;
        assert_eq!(sent_b64["bytes_written"], 3);
        assert_eq!(writer_rx.recv().await.unwrap(), vec![3, 77, 255]);

        let capture = dispatch_ok(
            &harness.router,
            "pane.capture",
            serde_json::json!({"session_id": session_id.to_string(), "lines": 2}),
        )
        .await;
        assert!(capture["text"].as_str().unwrap().contains("agent-ready"));
        assert_eq!(capture["requested_lines"], 2);

        let wait_text = dispatch_ok(
            &harness.router,
            "pane.wait_for",
            serde_json::json!({"window_id": window_id.to_string(), "text": "agent-ready", "timeout_ms": 40, "poll_ms": 20}),
        )
        .await;
        assert_eq!(wait_text["matched"], true);
        assert_eq!(wait_text["absent"], false);

        let wait_absent = dispatch_ok(
            &harness.router,
            "pane.wait_for",
            serde_json::json!({"pane_id": pane_id.to_string(), "regex": "panic|error", "absent": true, "timeout_ms": 40}),
        )
        .await;
        assert_eq!(wait_absent["matched"], true);
        assert_eq!(wait_absent["absent"], true);

        let timeout = dispatch_err(
            &harness.router,
            "pane.wait_for",
            serde_json::json!({"pane_id": pane_id.to_string(), "text": "never-happens", "timeout_ms": 20, "poll_ms": 20}),
        )
        .await;
        assert_eq!(timeout.code, shux_rpc::ErrorCode::NotFound.code());
        assert!(
            timeout.data.unwrap()["last_capture_preview"]
                .as_str()
                .unwrap()
                .contains("agent-ready")
        );

        let resized = dispatch_ok(
            &harness.router,
            "pane.set_size",
            serde_json::json!({"pane_id": pane_id.to_string(), "cols": 24, "rows": 4}),
        )
        .await;
        assert_eq!(resized["cols"], 24);
        assert_eq!(resized["rows"], 4);
        {
            let state = harness.io.lock().await;
            let vt = state.vts.get(&pane_id).unwrap();
            assert_eq!(vt.grid().cols(), 24);
            assert_eq!(vt.grid().rows(), 4);
        }

        let bad_size = dispatch_err(
            &harness.router,
            "pane.set_size",
            serde_json::json!({"pane_id": pane_id.to_string(), "cols": 1001, "rows": 4}),
        )
        .await;
        assert_eq!(bad_size.code, shux_rpc::ErrorCode::InvalidParams.code());

        let running = dispatch_ok(
            &harness.router,
            "pane.run_command",
            serde_json::json!({
                "pane_id": pane_id.to_string(),
                "command": "printf",
                "args": ["hello world"],
                "async": true,
                "timeout": 5,
            }),
        )
        .await;
        assert_eq!(running["state"], "running");
        let command_id = running["command_id"].as_str().unwrap().to_string();
        let pty_command = String::from_utf8(writer_rx.recv().await.unwrap()).unwrap();
        assert!(pty_command.contains("printf"));
        assert!(pty_command.contains("hello"));
        assert!(pty_command.contains("SHUX_MAR"));

        let status = dispatch_ok(
            &harness.router,
            "pane.command_status",
            serde_json::json!({"command_id": command_id}),
        )
        .await;
        assert_eq!(status["state"], "running");

        let cancelled = dispatch_ok(
            &harness.router,
            "pane.command_cancel",
            serde_json::json!({"command_id": command_id}),
        )
        .await;
        assert_eq!(cancelled["state"], "cancelled");
        assert_eq!(writer_rx.recv().await.unwrap(), vec![0x03]);

        let status = dispatch_ok(
            &harness.router,
            "pane.command_status",
            serde_json::json!({"command_id": command_id}),
        )
        .await;
        assert_eq!(status["state"], "cancelled");

        let bad_command = dispatch_err(
            &harness.router,
            "pane.run_command",
            serde_json::json!({"pane_id": pane_id.to_string()}),
        )
        .await;
        assert_eq!(bad_command.code, shux_rpc::ErrorCode::InvalidParams.code());

        harness.stop().await;
    }

    /// Deadline-bounded wait for the pane's settle-waiter count (the watch
    /// publisher's receiver_count — receivers exist ONLY while a
    /// `pane.wait_settled` handler is subscribed, so this IS the waiter
    /// registry). §16.1 permits deadline-bounded event waits.
    async fn wait_for_settle_waiters(
        io: &Arc<Mutex<PaneIoState>>,
        pane_id: shux_core::model::PaneId,
        expected: usize,
    ) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let count = {
                let state = io.lock().await;
                state
                    .revisions
                    .get(&pane_id)
                    .map(|tx| tx.receiver_count())
                    .unwrap_or(0)
            };
            if count == expected {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("settle waiter count never reached {expected} (last saw {count})");
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// P3 codex B2 lens-side proof (in-process half): a client that
    /// disconnects mid-`pane.wait_settled` must have its waiter DROPPED — the
    /// real observable is the pane's revision-watch receiver_count, which is
    /// exactly the set of live settle subscriptions (LENS-R-023). Runs the
    /// PRODUCTION router behind a REAL shux-rpc UDS server, so the
    /// connection-level cancellation path (serve_connection) is what drops
    /// the handler future. The black-box CLI-SIGKILL half lives in
    /// crates/shux/tests/settle_waiter_drop.rs.
    #[tokio::test]
    async fn production_settle_waiter_dropped_on_client_disconnect() {
        use futures::{SinkExt, StreamExt};

        let harness = RpcHarness::new();
        let (_sid, _wid, pane_id) = harness.seed_session("settle-drop").await;
        let _write_rx = harness.seed_io(pane_id, b"boot").await;
        // Seed the revision publisher the per-pane PTY task normally owns.
        // last_mutation_ns == now → the pane cannot become quiet within the
        // 60s quiet window, so the waiter lives until dropped.
        {
            let mut state = harness.io.lock().await;
            let (tx, rx0) = watch::channel(PaneRevision {
                content_revision: 1,
                last_mutation_ns: shux_vt::monotonic_now_ns(),
            });
            drop(rx0); // receiver_count now counts ONLY settle waiters
            state.revisions.insert(pane_id, tx);
        }

        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("settle-drop.sock");
        let server_cancel = CancellationToken::new();
        let server = shux_rpc::Server::new(
            shux_rpc::ServerConfig {
                socket_path: socket_path.clone(),
                tcp_addr: String::new(),
                auth_token: None,
            },
            harness.router.clone(),
            server_cancel.clone(),
        );
        let server_task = tokio::spawn(async move {
            server.run().await.unwrap();
        });

        // Connect (bounded retry — no fixed bind sleep) and park a waiter.
        let stream = {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                match tokio::net::UnixStream::connect(&socket_path).await {
                    Ok(s) => break s,
                    Err(e) => {
                        if tokio::time::Instant::now() >= deadline {
                            panic!("settle-drop server never bound: {e}");
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    }
                }
            }
        };
        let mut framed = tokio_util::codec::Framed::new(stream, shux_rpc::create_codec());
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "pane.wait_settled",
            "params": {
                "pane_id": pane_id.to_string(),
                "quiet_ms": 60_000,
                "timeout_ms": 600_000,
            },
        });
        framed
            .send(bytes::Bytes::from(serde_json::to_vec(&request).unwrap()))
            .await
            .unwrap();

        // The waiter subscribes: receiver_count 0 → 1.
        wait_for_settle_waiters(&harness.io, pane_id, 1).await;

        // Client disconnect (socket-level equivalent of SIGKILLing the CLI).
        drop(framed);

        // The waiter must be GONE — not parked until settle or the 600s cap.
        wait_for_settle_waiters(&harness.io, pane_id, 0).await;

        // Daemon healthy: a fresh connection serves a normal request.
        let stream2 = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
        let mut framed2 = tokio_util::codec::Framed::new(stream2, shux_rpc::create_codec());
        let list_req = serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "session.list", "params": {}
        });
        framed2
            .send(bytes::Bytes::from(serde_json::to_vec(&list_req).unwrap()))
            .await
            .unwrap();
        let response: serde_json::Value =
            serde_json::from_slice(&framed2.next().await.unwrap().unwrap()).unwrap();
        assert!(
            response["result"]["sessions"].is_array(),
            "daemon must stay responsive after the waiter drop: {response}"
        );

        server_cancel.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_task).await;
        harness.stop().await;
    }

    /// P3 codex M1: a pane killed while a settle waiter is parked on it must
    /// resolve that waiter with NOT_FOUND (-32004) — never `settled:true` on
    /// the frozen last value of a dead pane's channel. Teardown drops the
    /// revision publisher, the waiter's `changed()` errors, and the re-check
    /// finds the pane gone.
    #[tokio::test]
    async fn production_wait_settled_pane_killed_mid_wait_returns_not_found() {
        let harness = RpcHarness::new();
        let (_sid, _wid, pane_id) = harness.seed_session("settle-kill").await;
        let _write_rx = harness.seed_io(pane_id, b"boot").await;
        {
            let mut state = harness.io.lock().await;
            let (tx, rx0) = watch::channel(PaneRevision {
                content_revision: 1,
                // Fresh mutation stamp: cannot become quiet within 60s, so
                // the waiter is guaranteed parked when the pane dies.
                last_mutation_ns: shux_vt::monotonic_now_ns(),
            });
            drop(rx0);
            state.revisions.insert(pane_id, tx);
        }

        let router = harness.router.clone();
        let waiter = tokio::spawn(async move {
            router
                .dispatch(
                    "pane.wait_settled",
                    Some(serde_json::json!({
                        "pane_id": pane_id.to_string(),
                        "quiet_ms": 60_000,
                        "timeout_ms": 600_000,
                    })),
                )
                .await
        });

        // Deterministic: the waiter has subscribed (receiver_count 0 → 1).
        wait_for_settle_waiters(&harness.io, pane_id, 1).await;

        // Kill the pane exactly the way pane/window/session kill does:
        // teardown with remove_vts drops the VT AND the revision publisher.
        {
            let mut state = harness.io.lock().await;
            let _ = state.teardown_panes(&[pane_id], true);
        }

        let result = tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
            .await
            .expect("wait_settled must resolve promptly after pane teardown")
            .expect("waiter task must not panic");
        let err = result.expect_err("must error, not settle on a dead pane's frozen value");
        assert_eq!(
            err.code,
            shux_rpc::ErrorCode::NotFound.code(),
            "pane killed mid-wait must surface NOT_FOUND (-32004): {err:?}"
        );

        harness.stop().await;
    }

    /// P3 codex M2: mistyped `quiet_ms`/`timeout_ms` (string, float, null,
    /// negative) must surface INVALID_PARAMS (-32602) via the raw RPC path —
    /// never silently fall back to the defaults. Absent params still default.
    #[tokio::test]
    async fn production_wait_settled_rejects_mistyped_params() {
        let harness = RpcHarness::new();
        let (_sid, _wid, pane_id) = harness.seed_session("settle-types").await;
        let _write_rx = harness.seed_io(pane_id, b"boot").await;
        {
            let mut state = harness.io.lock().await;
            let (tx, rx0) = watch::channel(PaneRevision {
                content_revision: 1,
                // Ancient mutation stamp → the defaults-path call below
                // settles immediately instead of really waiting.
                last_mutation_ns: 1,
            });
            drop(rx0);
            state.revisions.insert(pane_id, tx);
        }

        for bad in [
            serde_json::json!("5ms"),
            serde_json::json!(5.5),
            serde_json::json!(null),
            serde_json::json!(-5),
        ] {
            let err = dispatch_err(
                &harness.router,
                "pane.wait_settled",
                serde_json::json!({ "pane_id": pane_id.to_string(), "quiet_ms": bad }),
            )
            .await;
            assert_eq!(
                err.code,
                shux_rpc::ErrorCode::InvalidParams.code(),
                "quiet_ms={bad} must be INVALID_PARAMS, got {err:?}"
            );
            let err = dispatch_err(
                &harness.router,
                "pane.wait_settled",
                serde_json::json!({ "pane_id": pane_id.to_string(), "timeout_ms": bad }),
            )
            .await;
            assert_eq!(
                err.code,
                shux_rpc::ErrorCode::InvalidParams.code(),
                "timeout_ms={bad} must be INVALID_PARAMS, got {err:?}"
            );
        }

        // Absent params → documented defaults (quiet 300 / timeout 10_000);
        // the ancient mutation stamp makes this an immediate settled return.
        let result = dispatch_ok(
            &harness.router,
            "pane.wait_settled",
            serde_json::json!({ "pane_id": pane_id.to_string() }),
        )
        .await;
        assert_eq!(result["settled"], serde_json::json!(true));

        harness.stop().await;
    }
}
