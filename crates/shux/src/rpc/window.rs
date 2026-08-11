//! Window CRUD methods.

use std::path::PathBuf;
use std::sync::Arc;

use shux_rpc::{Policy, Sensitivity};
use tokio::sync::Mutex;

use crate::pane_command;
use crate::pane_io::PaneIoState;
use crate::pane_spawn::{spawn_failure, spawn_pane_pty};
use crate::rpc::convert::{graph_error_to_rpc, window_to_json};
use crate::rpc::params::{
    parse_expected_version, required_str, resolve_session_ref, resolve_window_ref,
};

/// Register window CRUD methods on the router builder.
pub(crate) fn register_window_methods(
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

    let io_create = io_state.clone();
    let io_ensure = io_state.clone();
    let io_kill = io_state;
    let cancel_create = cancel.clone();
    let cancel_ensure = cancel.clone();

    builder
        .register_with_policy(
            "window.create",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g1.clone();
                let io = io_create.clone();
                let ct = cancel_create.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let session_id_str = required_str(&params, "session_id")?;
                    let session_id = resolve_session_ref(&gh, session_id_str, "session_id")?;

                    let name = params.get("name").and_then(|v| v.as_str());

                    // Auto-generate window name if not provided
                    let title = match name {
                        Some(n) => n.to_string(),
                        None => {
                            let snap = gh.snapshot();
                            let session = snap.sessions.get(&session_id).ok_or_else(|| {
                                shux_rpc::RpcError::not_found("session", session_id_str)
                            })?;
                            let mut idx = session.windows.len();
                            loop {
                                let candidate = format!("{idx}");
                                if !snap.window_name_exists_in_session(&session_id, &candidate) {
                                    break candidate;
                                }
                                idx += 1;
                            }
                        }
                    };

                    let cwd = params
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .map(PathBuf::from)
                        .unwrap_or_else(|| {
                            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"))
                        });

                    let command = pane_command::parse_pane_command(&params)?;

                    // Creating a window focuses it, and destroying a window hands
                    // focus to the session's FIRST window — not the one that had
                    // it. A rollback therefore has to restore focus itself, or a
                    // failed create silently relocates the session (see the
                    // matching comment in `pane.split`).
                    let prior_active_window = gh
                        .snapshot()
                        .sessions
                        .get(&session_id)
                        .map(|s| s.active_window);

                    // `_with_command` persists the argv on the window's initial
                    // pane, so `pane list` and `PaneCreated` report what is really
                    // running instead of a blank (issue #125).
                    let window_id = gh
                        .create_window_with_command(session_id, title, cwd.clone(), command.clone())
                        .await
                        .map_err(graph_error_to_rpc)?;

                    let snap = gh.snapshot();
                    let window = snap
                        .windows
                        .get(&window_id)
                        .ok_or_else(|| shux_rpc::RpcError::internal("window not in snapshot"))?;
                    let session = snap
                        .sessions
                        .get(&window.session_id)
                        .ok_or_else(|| shux_rpc::RpcError::internal("session not in snapshot"))?;
                    let index = session
                        .windows
                        .iter()
                        .position(|id| *id == window_id)
                        .unwrap_or(0);
                    let is_active = session.active_window == window_id;
                    let pane_id = window.active_pane.to_string();

                    // Spawn PTY for the new pane. A window whose only pane
                    // never started is not a window — surface the failure and
                    // undo the create rather than returning success on a
                    // phantom (issue #125 follow-up).
                    if let Err(e) = spawn_pane_pty(
                        window.active_pane,
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
                        // Same compare-and-restore as `pane.split`: only put
                        // focus back if it is still on the window being undone.
                        let focus_is_still_ours = gh
                            .snapshot()
                            .sessions
                            .get(&session_id)
                            .map(|s| s.active_window)
                            == Some(window_id);

                        let _ = gh.destroy_window(window_id, None).await;

                        if focus_is_still_ours
                            && let Some(prev) = prior_active_window
                            && gh
                                .snapshot()
                                .sessions
                                .get(&session_id)
                                .map(|s| s.active_window)
                                != Some(prev)
                        {
                            let _ = gh.focus_window(prev, None).await;
                        }
                        return Err(spawn_failure(&e));
                    }

                    let mut result = window_to_json(window, index, is_active, &snap);
                    // Include pane_id at top level for convenience
                    result["pane_id"] = serde_json::Value::String(pane_id);

                    Ok(result)
                }
            },
        )
        .register_with_policy(
            "window.list",
            Policy::fixed(Sensitivity::Public),
            move |params: Option<serde_json::Value>| {
                let gh = g2.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let session_id_str = required_str(&params, "session_id")?;
                    let session_id = resolve_session_ref(&gh, session_id_str, "session_id")?;

                    let snap = gh.snapshot();
                    let session = snap
                        .sessions
                        .get(&session_id)
                        .ok_or_else(|| shux_rpc::RpcError::not_found("session", session_id_str))?;

                    let windows: Vec<serde_json::Value> = session
                        .windows
                        .iter()
                        .enumerate()
                        .filter_map(|(index, wid)| {
                            snap.windows.get(wid).map(|w| {
                                window_to_json(w, index, session.active_window == *wid, &snap)
                            })
                        })
                        .collect();

                    Ok(serde_json::json!(windows))
                }
            },
        )
        .register_with_policy(
            "window.ensure",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g3.clone();
                let io = io_ensure.clone();
                let ct = cancel_ensure.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let session_id_str = required_str(&params, "session_id")?;
                    let session_id = resolve_session_ref(&gh, session_id_str, "session_id")?;

                    let name = params
                        .get("name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'name' parameter")
                        })?
                        .to_string();

                    // Validate BEFORE the already-exists shortcut. Parsing after
                    // it made `window.ensure` the one spawning RPC that accepted
                    // `{"command": 42}` without complaint whenever the window
                    // happened to exist — the same input it rejects when the
                    // window does not (issue #125 follow-up). `session.ensure`
                    // has always parsed first; this matches it.
                    let command = pane_command::parse_pane_command(&params)?;

                    // Check if window with this name already exists
                    let snap = gh.snapshot();
                    if let Some(w) = snap.find_window_by_name(&session_id, &name) {
                        let session = snap.sessions.get(&session_id).ok_or_else(|| {
                            shux_rpc::RpcError::not_found("session", session_id_str)
                        })?;
                        let index = session
                            .windows
                            .iter()
                            .position(|id| *id == w.id)
                            .unwrap_or(0);
                        let is_active = session.active_window == w.id;
                        let mut result = window_to_json(w, index, is_active, &snap);
                        result["created"] = serde_json::Value::Bool(false);
                        return Ok(result);
                    }

                    let cwd = params
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .map(PathBuf::from)
                        .unwrap_or_else(|| {
                            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"))
                        });
                    let prior_active_window = gh
                        .snapshot()
                        .sessions
                        .get(&session_id)
                        .map(|s| s.active_window);
                    let window_id = gh
                        .create_window_with_command(session_id, name, cwd.clone(), command.clone())
                        .await
                        .map_err(graph_error_to_rpc)?;

                    let snap = gh.snapshot();
                    let window = snap
                        .windows
                        .get(&window_id)
                        .ok_or_else(|| shux_rpc::RpcError::internal("window not in snapshot"))?;

                    // Spawn PTY for the new pane. A window whose only pane
                    // never started is not a window — surface the failure and
                    // undo the create rather than returning success on a
                    // phantom (issue #125 follow-up).
                    if let Err(e) = spawn_pane_pty(
                        window.active_pane,
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
                        // Same compare-and-restore as `pane.split`: only put
                        // focus back if it is still on the window being undone.
                        let focus_is_still_ours = gh
                            .snapshot()
                            .sessions
                            .get(&session_id)
                            .map(|s| s.active_window)
                            == Some(window_id);

                        let _ = gh.destroy_window(window_id, None).await;

                        if focus_is_still_ours
                            && let Some(prev) = prior_active_window
                            && gh
                                .snapshot()
                                .sessions
                                .get(&session_id)
                                .map(|s| s.active_window)
                                != Some(prev)
                        {
                            let _ = gh.focus_window(prev, None).await;
                        }
                        return Err(spawn_failure(&e));
                    }

                    let session = snap
                        .sessions
                        .get(&session_id)
                        .ok_or_else(|| shux_rpc::RpcError::internal("session not in snapshot"))?;
                    let index = session
                        .windows
                        .iter()
                        .position(|id| *id == window_id)
                        .unwrap_or(0);
                    let is_active = session.active_window == window_id;
                    let mut result = window_to_json(window, index, is_active, &snap);
                    result["created"] = serde_json::Value::Bool(true);
                    Ok(result)
                }
            },
        )
        .register_with_policy(
            "window.rename",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g4.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let window_id = resolve_window_ref(&gh, required_str(&params, "id")?, "id")?;

                    let new_title = params
                        .get("name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'name' parameter")
                        })?
                        .to_string();

                    let expected_version = parse_expected_version(&params)?;

                    gh.rename_window(window_id, new_title, expected_version)
                        .await
                        .map_err(graph_error_to_rpc)?;

                    let snap = gh.snapshot();
                    let window = snap.windows.get(&window_id).ok_or_else(|| {
                        shux_rpc::RpcError::internal("window vanished after rename")
                    })?;
                    let session = snap
                        .sessions
                        .get(&window.session_id)
                        .ok_or_else(|| shux_rpc::RpcError::internal("session not in snapshot"))?;
                    let index = session
                        .windows
                        .iter()
                        .position(|id| *id == window_id)
                        .unwrap_or(0);
                    let is_active = session.active_window == window_id;
                    Ok(window_to_json(window, index, is_active, &snap))
                }
            },
        )
        .register_with_policy(
            "window.focus",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g5.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let window_id = resolve_window_ref(&gh, required_str(&params, "id")?, "id")?;

                    let expected_version = parse_expected_version(&params)?;

                    let previous = gh
                        .focus_window(window_id, expected_version)
                        .await
                        .map_err(graph_error_to_rpc)?;

                    let snap = gh.snapshot();
                    let window = snap.windows.get(&window_id).ok_or_else(|| {
                        shux_rpc::RpcError::internal("window vanished after focus")
                    })?;
                    let session = snap
                        .sessions
                        .get(&window.session_id)
                        .ok_or_else(|| shux_rpc::RpcError::internal("session not in snapshot"))?;
                    let index = session
                        .windows
                        .iter()
                        .position(|id| *id == window_id)
                        .unwrap_or(0);
                    let mut result = window_to_json(window, index, true, &snap);
                    result["previous_window_id"] = match previous {
                        Some(id) => serde_json::Value::String(id.to_string()),
                        None => serde_json::Value::Null,
                    };
                    Ok(result)
                }
            },
        )
        .register_with_policy(
            "window.reorder",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g6.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let window_id = resolve_window_ref(&gh, required_str(&params, "id")?, "id")?;

                    let new_index = params
                        .get("new_index")
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'new_index' parameter")
                        })? as usize;

                    let expected_version = parse_expected_version(&params)?;

                    gh.reorder_window(window_id, new_index, expected_version)
                        .await
                        .map_err(graph_error_to_rpc)?;

                    let snap = gh.snapshot();
                    let window = snap.windows.get(&window_id).ok_or_else(|| {
                        shux_rpc::RpcError::internal("window vanished after reorder")
                    })?;
                    let session = snap
                        .sessions
                        .get(&window.session_id)
                        .ok_or_else(|| shux_rpc::RpcError::internal("session not in snapshot"))?;
                    let index = session
                        .windows
                        .iter()
                        .position(|id| *id == window_id)
                        .unwrap_or(0);
                    let is_active = session.active_window == window_id;
                    Ok(window_to_json(window, index, is_active, &snap))
                }
            },
        )
        .register_with_policy(
            "window.kill",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g7.clone();
                let io = io_kill.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let window_id = resolve_window_ref(&gh, required_str(&params, "id")?, "id")?;

                    let expected_version = parse_expected_version(&params)?;

                    // Snapshot pane IDs BEFORE mutation so we can tear down IO
                    // after the destroy succeeds. Mutate the graph first so a
                    // stale `expected_version` (or LastWindow refusal) errors
                    // out without leaving the window with orphaned VTs/PTYs.
                    let pane_ids: Vec<_> = {
                        let snap = gh.snapshot();
                        snap.panes
                            .values()
                            .filter(|p| p.window_id == window_id)
                            .map(|p| p.id)
                            .collect()
                    };

                    gh.destroy_window(window_id, expected_version)
                        .await
                        .map_err(graph_error_to_rpc)?;

                    {
                        let mut state = io.lock().await;
                        let pulse = state.teardown_panes(&pane_ids, true);
                        drop(state);
                        pulse.notify_one();
                    }

                    Ok(serde_json::json!({ "killed": window_id.to_string() }))
                }
            },
        )
}
