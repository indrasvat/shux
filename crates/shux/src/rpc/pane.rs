//! Pane lifecycle and layout methods.

use std::path::PathBuf;
use std::sync::Arc;

use shux_rpc::{Policy, Sensitivity};
use tokio::sync::Mutex;

use crate::pane_command;
use crate::pane_io::PaneIoState;
use crate::pane_spawn::{spawn_failure, spawn_pane_pty};
use crate::rpc::convert::{graph_error_to_rpc, pane_to_json};
use crate::rpc::params::{
    parse_expected_version, required_str, resolve_pane_id_from_params, resolve_pane_ref,
    resolve_pane_ref_named, resolve_window_id_from_params,
};

/// Register pane operation methods on the router builder.
pub(crate) fn register_pane_methods(
    builder: shux_rpc::RouterBuilder,
    graph: shux_core::graph::GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    cancel: tokio_util::sync::CancellationToken,
) -> shux_rpc::RouterBuilder {
    let g1 = graph.clone();
    let g2 = graph.clone();
    let g3 = graph.clone();
    let g4 = graph.clone();
    let g5 = graph.clone();
    let g6 = graph.clone();
    let g7 = graph.clone();
    let g8 = graph.clone();
    let g9 = graph.clone();

    let io_split = io_state.clone();
    let io_kill = io_state;
    let cancel_split = cancel;

    builder
        .register_with_policy("pane.list", Policy::fixed(Sensitivity::Public), move |params: Option<serde_json::Value>| {
            let gh = g1.clone();
            async move {
                let params = params.unwrap_or_default();

                // Resolve window_id — either provided directly or via session_id + active_window
                let window_id = resolve_window_id_from_params(&gh, &params)?;

                let snap = gh.snapshot();
                let window = snap
                    .windows
                    .get(&window_id)
                    .ok_or_else(|| shux_rpc::RpcError::not_found("window", &window_id.to_string()))?;

                let panes: Vec<serde_json::Value> = snap
                    .panes
                    .values()
                    .filter(|p| p.window_id == window_id)
                    .map(|p| pane_to_json(p, window))
                    .collect();

                Ok(serde_json::json!(panes))
            }
        })
        .register_with_policy("pane.split", Policy::fixed(Sensitivity::OwnedMutation), move |params: Option<serde_json::Value>| {
            let gh = g2.clone();
            let io = io_split.clone();
            let ct = cancel_split.clone();
            async move {
                let params = params.unwrap_or_default();

                // Resolve the target pane — either provided or active pane of window
                let pane_id = resolve_pane_id_from_params(&gh, &params)?;

                let direction = match params.get("direction").and_then(|v| v.as_str()) {
                    Some("horizontal") | Some("h") => shux_core::layout::Direction::Horizontal,
                    Some("vertical") | Some("v") => shux_core::layout::Direction::Vertical,
                    None | Some("auto") => shux_core::layout::Direction::Vertical, // default to vertical
                    Some(other) => {
                        return Err(shux_rpc::RpcError::invalid_params(&format!(
                            "invalid direction '{other}', expected 'horizontal', 'vertical', or 'auto'"
                        )));
                    }
                };

                // Validated BEFORE the cast, and before the graph grows a pane.
                // `rpc call pane.split` is a shipped surface — every subcommand
                // mirrors an RPC method 1:1 — so a guard that lives only in the
                // clap value parser leaves the documented range reachable by
                // anyone who types the method name instead of the verb.
                let ratio_f64 = params
                    .get("ratio")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.5);
                pane_command::check_ratio(ratio_f64)
                    .map_err(|e| shux_rpc::RpcError::invalid_params(&format!("ratio: {e}")))?;
                let ratio = ratio_f64 as f32;

                // Parse BEFORE splitting. A malformed `command` used to be
                // noticed after the graph had already grown a pane, leaving a
                // half-made split behind on an error path (issue #125).
                let command = pane_command::parse_pane_command(&params)?;
                let cwd = params
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"))
                    });

                // Splitting focuses the new pane, and destroying a pane hands
                // focus to whichever pane the layout tree yields first — NOT the
                // one that had it. So a rollback has to put focus back itself,
                // or a failed split silently moves the operator's cursor to an
                // unrelated pane and every later `-p`-less verb targets it.
                let (prior_active_pane, prior_zoom) = {
                    let snap = gh.snapshot();
                    let w = snap
                        .panes
                        .get(&pane_id)
                        .and_then(|p| snap.windows.get(&p.window_id));
                    (
                        w.map(|w| w.active_pane),
                        w.and_then(|w| w.layout.zoom.as_ref().map(|z| z.zoomed_pane)),
                    )
                };

                // `_with_command` persists the argv on the new pane, so
                // `pane list` and the `PaneCreated` event report what is
                // really running instead of a blank.
                let new_pane_id = gh
                    .split_pane_with_command(pane_id, direction, ratio, command.clone())
                    .await
                    .map_err(graph_error_to_rpc)?;

                // A PTY that never started is not a pane. Discarding this
                // error left a phantom in the graph — `pane list` showed it
                // with `exit_status: null`, every later verb answered "pane VT
                // not found", and the RPC had already returned success
                // (issue #125 follow-up).
                if let Err(e) = spawn_pane_pty(
                    new_pane_id,
                    cwd,
                    command,
                    shux_pty::handle::PtySize::default(),
                    Vec::new(),
                    false,
                    io,
                    ct,
                    gh.clone(),
                )
                .await
                {
                    // Restore focus ONLY if it is still on the pane being undone.
                    // Capturing before and writing after is a lost update: an
                    // operator who moved focus while the PTY was starting had
                    // their choice silently reverted, and measurably more often
                    // than before the restore existed. Their choice wins.
                    // One snapshot, read three times: `gh.snapshot()` hands back
                    // a temporary, so chaining lookups across two calls borrows
                    // from something already dropped.
                    let active_pane_of = |snap: &shux_core::graph::SessionGraphSnapshot,
                                          pane: shux_core::model::PaneId| {
                        snap.panes
                            .get(&pane)
                            .and_then(|p| snap.windows.get(&p.window_id))
                            .map(|w| w.active_pane)
                    };
                    let focus_is_still_ours =
                        active_pane_of(&gh.snapshot(), new_pane_id) == Some(new_pane_id);

                    let _ = gh.destroy_pane(new_pane_id, None).await;

                    if focus_is_still_ours
                        && let Some(prev) = prior_active_pane
                        // `destroy_pane` may already have landed on `prev`;
                        // focusing again fires a transition out of nowhere.
                        && active_pane_of(&gh.snapshot(), prev) != Some(prev)
                    {
                        let _ = gh.focus_pane(prev).await;
                    }
                    // A successful split legitimately clears zoom; an undone one
                    // must not leave the window un-zoomed.
                    if let Some(z) = prior_zoom {
                        let snap = gh.snapshot();
                        let zoomed_now = snap
                            .panes
                            .get(&z)
                            .and_then(|p| snap.windows.get(&p.window_id))
                            .is_some_and(|w| w.layout.is_zoomed());
                        drop(snap);
                        if !zoomed_now {
                            let _ = gh.zoom_pane(z, None).await;
                        }
                    }
                    return Err(spawn_failure(&e));
                }

                let snap = gh.snapshot();
                let new_pane = snap
                    .panes
                    .get(&new_pane_id)
                    .ok_or_else(|| shux_rpc::RpcError::internal("pane not in snapshot"))?;
                let window = snap
                    .windows
                    .get(&new_pane.window_id)
                    .ok_or_else(|| shux_rpc::RpcError::internal("window not in snapshot"))?;

                Ok(serde_json::json!({
                    "pane": pane_to_json(new_pane, window),
                    "split_from": pane_id.to_string(),
                }))
            }
        })
        .register_with_policy("pane.focus", Policy::fixed(Sensitivity::OwnedMutation), move |params: Option<serde_json::Value>| {
            let gh = g3.clone();
            async move {
                let params = params.unwrap_or_default();
                let pane_id = resolve_pane_ref(&gh, required_str(&params, "pane_id")?)?;

                let previous = gh
                    .focus_pane(pane_id)
                    .await
                    .map_err(graph_error_to_rpc)?;

                Ok(serde_json::json!({
                    "pane_id": pane_id.to_string(),
                    "previous_pane_id": previous.map(|id| id.to_string()),
                }))
            }
        })
        .register_with_policy(
            "pane.focus_direction",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g4.clone();
                async move {
                    let params = params.unwrap_or_default();

                    let window_id = resolve_window_id_from_params(&gh, &params)?;

                    let dir_str = params
                        .get("direction")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'direction' parameter")
                        })?;

                    let direction = match dir_str.to_lowercase().as_str() {
                        "up" => shux_core::layout::NavDirection::Up,
                        "down" => shux_core::layout::NavDirection::Down,
                        "left" => shux_core::layout::NavDirection::Left,
                        "right" => shux_core::layout::NavDirection::Right,
                        other => {
                            return Err(shux_rpc::RpcError::invalid_params(&format!(
                                "invalid direction '{other}', expected 'up', 'down', 'left', or 'right'"
                            )));
                        }
                    };

                    // Use a default viewport — the actual viewport would come from the TUI client
                    let viewport = shux_core::layout::Rect::new(0, 0, 120, 40);

                    let snap = gh.snapshot();
                    let window = snap
                        .windows
                        .get(&window_id)
                        .ok_or_else(|| {
                            shux_rpc::RpcError::not_found("window", &window_id.to_string())
                        })?;
                    let previous_pane = window.active_pane;

                    let target = gh
                        .focus_pane_direction(window_id, direction, viewport)
                        .await
                        .map_err(graph_error_to_rpc)?;

                    match target {
                        Some(pane_id) => Ok(serde_json::json!({
                            "pane_id": pane_id.to_string(),
                            "previous_pane_id": previous_pane.to_string(),
                        })),
                        None => Err(shux_rpc::RpcError::invalid_params(&format!(
                            "no neighbor pane in direction {dir_str}"
                        ))),
                    }
                }
            },
        )
        .register_with_policy("pane.resize", Policy::fixed(Sensitivity::OwnedMutation), move |params: Option<serde_json::Value>| {
            let gh = g5.clone();
            async move {
                let params = params.unwrap_or_default();
                let pane_id = resolve_pane_id_from_params(&gh, &params)?;

                let dir_str = params
                    .get("direction")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        shux_rpc::RpcError::invalid_params("missing 'direction' parameter")
                    })?;

                let direction = match dir_str.to_lowercase().as_str() {
                    "horizontal" | "h" => shux_core::layout::Direction::Horizontal,
                    "vertical" | "v" => shux_core::layout::Direction::Vertical,
                    other => {
                        return Err(shux_rpc::RpcError::invalid_params(&format!(
                            "invalid direction '{other}', expected 'horizontal' or 'vertical'"
                        )));
                    }
                };

                let delta = params
                    .get("delta")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.1) as f32;

                let expected_version = parse_expected_version(&params)?;

                gh.resize_pane(pane_id, direction, delta, expected_version)
                    .await
                    .map_err(graph_error_to_rpc)?;

                Ok(serde_json::json!({ "pane_id": pane_id.to_string() }))
            }
        })
        .register_with_policy("pane.zoom", Policy::fixed(Sensitivity::OwnedMutation), move |params: Option<serde_json::Value>| {
            let gh = g6.clone();
            async move {
                let params = params.unwrap_or_default();
                let pane_id = resolve_pane_id_from_params(&gh, &params)?;
                let expected_version = parse_expected_version(&params)?;

                let is_zoomed = gh
                    .zoom_pane(pane_id, expected_version)
                    .await
                    .map_err(graph_error_to_rpc)?;

                Ok(serde_json::json!({
                    "pane_id": pane_id.to_string(),
                    "is_zoomed": is_zoomed,
                }))
            }
        })
        .register_with_policy("pane.swap", Policy::fixed(Sensitivity::OwnedMutation), move |params: Option<serde_json::Value>| {
            let gh = g7.clone();
            async move {
                let params = params.unwrap_or_default();
                let pane_id_str = required_str(&params, "pane_id")?;
                let target_str = required_str(&params, "target_pane_id")?;

                let pane_a = resolve_pane_ref(&gh, pane_id_str)?;
                let pane_b = resolve_pane_ref_named(&gh, target_str, "target_pane_id")?;

                let expected_version = parse_expected_version(&params)?;

                gh.swap_panes(pane_a, pane_b, expected_version)
                    .await
                    .map_err(graph_error_to_rpc)?;

                Ok(serde_json::json!({
                    "pane_a": pane_a.to_string(),
                    "pane_b": pane_b.to_string(),
                }))
            }
        })
        .register_with_policy("pane.kill", Policy::fixed(Sensitivity::OwnedMutation), move |params: Option<serde_json::Value>| {
            let gh = g8.clone();
            let io = io_kill.clone();
            async move {
                let params = params.unwrap_or_default();
                let pane_id = resolve_pane_ref(&gh, required_str(&params, "pane_id")?)?;

                let expected_version = parse_expected_version(&params)?;

                // Order-of-operations matters: destroy_pane() can return
                // LastPane (refusing to remove the only pane in a window).
                // If we tear down writers/resizers/vts FIRST and then the
                // graph mutation fails, the pane stays in the graph but
                // its IO state is gone — the session ends up with an
                // active pane that has no PTY. Mutate the graph first;
                // only purge IO state on success. Same reason
                // expected_version is checked inside destroy_pane — a stale
                // version must error out BEFORE we touch IO state.
                gh.destroy_pane(pane_id, expected_version)
                    .await
                    .map_err(graph_error_to_rpc)?;
                {
                    let mut state = io.lock().await;
                    let pulse = state.teardown_panes(&[pane_id], true);
                    drop(state);
                    pulse.notify_one();
                }

                Ok(serde_json::json!({ "killed": pane_id.to_string() }))
            }
        })
        .register_with_policy(
            "pane.set_title",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g9.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;
                    // `title: null` clears the manual override, letting
                    // OSC + command-derived auto-titles flow back into
                    // the displayed pane title. `title: "text"` pins it.
                    // Omitted entirely leaves the manual title unchanged
                    // — useful when toggling only `auto`.
                    let title: Option<Option<String>> = match params.get("title") {
                        Some(serde_json::Value::Null) => Some(None),
                        Some(serde_json::Value::String(s)) => Some(Some(s.clone())),
                        Some(other) => {
                            return Err(shux_rpc::RpcError::invalid_params(&format!(
                                "'title' must be string or null, got {other}"
                            )));
                        }
                        None => None,
                    };
                    let auto = match params.get("auto") {
                        Some(serde_json::Value::Bool(b)) => Some(*b),
                        Some(serde_json::Value::Null) | None => None,
                        Some(other) => {
                            return Err(shux_rpc::RpcError::invalid_params(&format!(
                                "'auto' must be boolean or null, got {other}"
                            )));
                        }
                    };
                    // If neither was provided, the caller is asking us
                    // to do nothing — surface that as invalid_params
                    // rather than a silent success.
                    if title.is_none() && auto.is_none() {
                        return Err(shux_rpc::RpcError::invalid_params(
                            "must provide at least one of 'title' or 'auto'",
                        ));
                    }
                    // `title: None` (omitted) → don't touch manual_title.
                    // `title: Some(None)` (explicit null) → clear it.
                    // `title: Some(Some(...))` → set it.
                    let title_arg = title.unwrap_or_else(|| {
                        // Caller only set `auto`; leave manual_title alone.
                        // Re-read the current value so set_pane_title's
                        // unconditional set_manual_title doesn't wipe it.
                        gh.snapshot()
                            .panes
                            .get(&pane_id)
                            .and_then(|p| p.manual_title.clone())
                    });
                    gh.set_pane_title(pane_id, title_arg, auto)
                        .await
                        .map_err(graph_error_to_rpc)?;
                    let snap = gh.snapshot();
                    let pane = snap.panes.get(&pane_id).ok_or_else(|| {
                        shux_rpc::RpcError::internal("pane vanished after set_title")
                    })?;
                    Ok(serde_json::json!({
                        "pane_id": pane_id.to_string(),
                        "title": pane.title,
                        "auto_title": pane.auto_title,
                        "manual_title": pane.manual_title,
                        "osc_title": pane.osc_title,
                    }))
                }
            },
        )
}
