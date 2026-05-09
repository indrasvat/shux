//! M0 Integration Test Suite
//!
//! Verifies PRD §17 M0 "Done when" criteria by testing the full
//! CLI→UDS→RPC→SessionGraph pipeline. Each test gets its own ephemeral
//! daemon (RPC server + SessionGraph) for isolation.

use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use tokio::net::UnixStream;
use tokio_util::codec::Framed;

// ══════════════════════════════════════════════════════════════
// Test harness
// ══════════════════════════════════════════════════════════════

/// Map GraphError to appropriate RPC error codes (duplicated from main.rs).
fn graph_error_to_rpc(e: shux_core::graph::GraphError) -> shux_rpc::RpcError {
    use shux_core::graph::GraphError;
    match e {
        GraphError::SessionNotFound(_) => shux_rpc::RpcError::not_found("session", &e.to_string()),
        GraphError::WindowNotFound(_) => shux_rpc::RpcError::not_found("window", &e.to_string()),
        GraphError::PaneNotFound(_) => shux_rpc::RpcError::not_found("pane", &e.to_string()),
        GraphError::SessionNameExists(ref name) => {
            shux_rpc::RpcError::name_conflict("session", name)
        }
        GraphError::WindowNameConflict(ref name) => {
            shux_rpc::RpcError::name_conflict("window", name)
        }
        GraphError::EmptySessionName
        | GraphError::SessionNameTooLong(_)
        | GraphError::InvalidSessionName(_) => shux_rpc::RpcError::invalid_params(&e.to_string()),
        GraphError::EmptyWindowName | GraphError::WindowIndexOutOfRange { .. } => {
            shux_rpc::RpcError::invalid_params(&e.to_string())
        }
        GraphError::LastWindow | GraphError::LastPane => {
            shux_rpc::RpcError::invalid_params(&e.to_string())
        }
        GraphError::PaneSwapSelf | GraphError::PaneCrossWindow | GraphError::NoNeighbor(_) => {
            shux_rpc::RpcError::invalid_params(&e.to_string())
        }
        GraphError::LayoutError(_) => shux_rpc::RpcError::internal(&e.to_string()),
        GraphError::VersionConflict { expected, actual } => {
            shux_rpc::RpcError::version_conflict("resource", "?", expected, actual)
        }
        GraphError::Shutdown => shux_rpc::RpcError::internal(&e.to_string()),
    }
}

/// Build window info JSON from a Window.
fn window_to_json(
    w: &shux_core::model::Window,
    index: usize,
    is_active: bool,
    snap: &shux_core::graph::SessionGraphSnapshot,
) -> serde_json::Value {
    let pane_count = snap.panes.values().filter(|p| p.window_id == w.id).count();
    serde_json::json!({
        "id": w.id.to_string(),
        "session_id": w.session_id.to_string(),
        "title": w.title,
        "pane_count": pane_count,
        "active_pane_id": w.active_pane.to_string(),
        "index": index,
        "is_active": is_active,
        "version": w.version,
    })
}

/// Build session info JSON from a Session.
fn session_to_json(
    s: &shux_core::model::Session,
    snap: &shux_core::graph::SessionGraphSnapshot,
) -> serde_json::Value {
    let first_window_id = s.windows.first().map(|w| w.to_string());
    let first_pane_id = s
        .windows
        .first()
        .and_then(|wid| snap.windows.get(wid).map(|w| w.active_pane.to_string()));
    serde_json::json!({
        "id": s.id.to_string(),
        "name": s.name,
        "windows": s.windows.iter().map(|w| w.to_string()).collect::<Vec<_>>(),
        "window_count": s.windows.len(),
        "active_window_id": s.active_window.to_string(),
        "window_id": first_window_id,
        "pane_id": first_pane_id,
        "created_at": s.created_at.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
    })
}

/// Register session CRUD methods backed by a real GraphHandle.
fn register_session_methods(
    builder: shux_rpc::RouterBuilder,
    graph: shux_core::graph::GraphHandle,
) -> shux_rpc::RouterBuilder {
    let g1 = graph.clone();
    let g2 = graph.clone();
    let g3 = graph.clone();
    let g4 = graph.clone();
    let g5 = graph.clone();

    builder
        .register("session.list", move |_params: Option<serde_json::Value>| {
            let gh = g1.clone();
            async move {
                let snap = gh.snapshot();
                let mut sessions: Vec<_> = snap.sessions.values().collect();
                sessions.sort_by_key(|s| s.created_at);
                let sessions: Vec<serde_json::Value> =
                    sessions.iter().map(|s| session_to_json(s, &snap)).collect();
                Ok(serde_json::json!({ "sessions": sessions }))
            }
        })
        .register(
            "session.create",
            move |params: Option<serde_json::Value>| {
                let gh = g2.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let name = params
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let name = match name {
                        Some(n) => n,
                        None => {
                            let snap = gh.snapshot();
                            let mut idx = snap.sessions.len();
                            loop {
                                let candidate = format!("session-{idx}");
                                if !snap.session_name_exists(&candidate) {
                                    break candidate;
                                }
                                idx += 1;
                            }
                        }
                    };
                    let cwd = PathBuf::from("/tmp");
                    match gh.create_session(name, cwd).await {
                        Ok(session_id) => {
                            let snap = gh.snapshot();
                            if let Some(s) = snap.sessions.get(&session_id) {
                                Ok(session_to_json(s, &snap))
                            } else {
                                Ok(serde_json::json!({ "id": session_id.to_string() }))
                            }
                        }
                        Err(e) => Err(graph_error_to_rpc(e)),
                    }
                }
            },
        )
        .register("session.kill", move |params: Option<serde_json::Value>| {
            let gh = g3.clone();
            async move {
                let params = params.unwrap_or_default();
                let session_id = if let Some(id_str) = params.get("id").and_then(|v| v.as_str()) {
                    let parsed: shux_core::model::SessionId = id_str.parse().map_err(|_| {
                        shux_rpc::RpcError::invalid_params("invalid session ID format")
                    })?;
                    let snap = gh.snapshot();
                    if !snap.sessions.contains_key(&parsed) {
                        return Err(shux_rpc::RpcError::not_found("session", id_str));
                    }
                    parsed
                } else if let Some(name) = params.get("name").and_then(|v| v.as_str()) {
                    let snap = gh.snapshot();
                    let session = snap
                        .find_session_by_name(name)
                        .ok_or_else(|| shux_rpc::RpcError::not_found("session", name))?;
                    session.id
                } else {
                    return Err(shux_rpc::RpcError::invalid_params(
                        "missing 'name' or 'id' parameter",
                    ));
                };

                let snap = gh.snapshot();
                let name = snap
                    .sessions
                    .get(&session_id)
                    .map(|s| s.name.clone())
                    .unwrap_or_default();

                gh.destroy_session(session_id, None)
                    .await
                    .map_err(graph_error_to_rpc)?;

                Ok(serde_json::json!({ "killed": name }))
            }
        })
        .register(
            "session.ensure",
            move |params: Option<serde_json::Value>| {
                let gh = g4.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let name = params
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("default")
                        .to_string();
                    let snap = gh.snapshot();
                    if let Some(s) = snap.find_session_by_name(&name) {
                        let mut json = session_to_json(s, &snap);
                        json["created"] = serde_json::Value::Bool(false);
                        return Ok(json);
                    }
                    let cwd = PathBuf::from("/tmp");
                    match gh.create_session(name, cwd).await {
                        Ok(session_id) => {
                            let snap = gh.snapshot();
                            if let Some(s) = snap.sessions.get(&session_id) {
                                let mut json = session_to_json(s, &snap);
                                json["created"] = serde_json::Value::Bool(true);
                                Ok(json)
                            } else {
                                Ok(serde_json::json!({
                                    "id": session_id.to_string(),
                                    "created": true,
                                }))
                            }
                        }
                        Err(e) => Err(graph_error_to_rpc(e)),
                    }
                }
            },
        )
        .register(
            "session.rename",
            move |params: Option<serde_json::Value>| {
                let gh = g5.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let new_name = params
                        .get("new_name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'new_name' parameter")
                        })?
                        .to_string();

                    let session_id = if let Some(name) = params.get("name").and_then(|v| v.as_str())
                    {
                        let snap = gh.snapshot();
                        let session = snap
                            .find_session_by_name(name)
                            .ok_or_else(|| shux_rpc::RpcError::not_found("session", name))?;
                        session.id
                    } else if let Some(id_str) = params.get("id").and_then(|v| v.as_str()) {
                        id_str.parse().map_err(|_| {
                            shux_rpc::RpcError::invalid_params("invalid session ID format")
                        })?
                    } else {
                        return Err(shux_rpc::RpcError::invalid_params(
                            "missing 'name' or 'id' parameter",
                        ));
                    };

                    gh.rename_session(session_id, new_name, None)
                        .await
                        .map_err(graph_error_to_rpc)?;

                    let snap = gh.snapshot();
                    if let Some(s) = snap.sessions.get(&session_id) {
                        Ok(session_to_json(s, &snap))
                    } else {
                        Err(shux_rpc::RpcError::internal(
                            "session vanished after rename",
                        ))
                    }
                }
            },
        )
}

/// Register window CRUD methods backed by a real GraphHandle.
fn register_window_methods(
    builder: shux_rpc::RouterBuilder,
    graph: shux_core::graph::GraphHandle,
) -> shux_rpc::RouterBuilder {
    let g1 = graph.clone();
    let g2 = graph.clone();
    let g3 = graph.clone();
    let g4 = graph.clone();
    let g5 = graph.clone();
    let g6 = graph.clone();
    let g7 = graph.clone();

    builder
        .register("window.create", move |params: Option<serde_json::Value>| {
            let gh = g1.clone();
            async move {
                let params = params.unwrap_or_default();
                let session_id_str = params
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        shux_rpc::RpcError::invalid_params("missing 'session_id' parameter")
                    })?;
                let session_id: shux_core::model::SessionId = session_id_str
                    .parse()
                    .map_err(|_| shux_rpc::RpcError::invalid_params("invalid session_id format"))?;
                let name = params.get("name").and_then(|v| v.as_str());
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
                let cwd = PathBuf::from("/tmp");
                let window_id = gh
                    .create_window(session_id, title, cwd)
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
                let mut result = window_to_json(window, index, is_active, &snap);
                result["pane_id"] = serde_json::Value::String(pane_id);
                Ok(result)
            }
        })
        .register("window.list", move |params: Option<serde_json::Value>| {
            let gh = g2.clone();
            async move {
                let params = params.unwrap_or_default();
                let session_id_str = params
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        shux_rpc::RpcError::invalid_params("missing 'session_id' parameter")
                    })?;
                let session_id: shux_core::model::SessionId = session_id_str
                    .parse()
                    .map_err(|_| shux_rpc::RpcError::invalid_params("invalid session_id format"))?;
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
                        snap.windows
                            .get(wid)
                            .map(|w| window_to_json(w, index, session.active_window == *wid, &snap))
                    })
                    .collect();
                Ok(serde_json::json!(windows))
            }
        })
        .register("window.ensure", move |params: Option<serde_json::Value>| {
            let gh = g3.clone();
            async move {
                let params = params.unwrap_or_default();
                let session_id_str = params
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        shux_rpc::RpcError::invalid_params("missing 'session_id' parameter")
                    })?;
                let session_id: shux_core::model::SessionId = session_id_str
                    .parse()
                    .map_err(|_| shux_rpc::RpcError::invalid_params("invalid session_id format"))?;
                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'name' parameter"))?
                    .to_string();
                let snap = gh.snapshot();
                if let Some(w) = snap.find_window_by_name(&session_id, &name) {
                    let session = snap
                        .sessions
                        .get(&session_id)
                        .ok_or_else(|| shux_rpc::RpcError::not_found("session", session_id_str))?;
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
                let cwd = PathBuf::from("/tmp");
                let window_id = gh
                    .create_window(session_id, name, cwd)
                    .await
                    .map_err(graph_error_to_rpc)?;
                let snap = gh.snapshot();
                let window = snap
                    .windows
                    .get(&window_id)
                    .ok_or_else(|| shux_rpc::RpcError::internal("window not in snapshot"))?;
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
        })
        .register("window.rename", move |params: Option<serde_json::Value>| {
            let gh = g4.clone();
            async move {
                let params = params.unwrap_or_default();
                let window_id_str = params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'id' parameter"))?;
                let window_id: shux_core::model::WindowId = window_id_str
                    .parse()
                    .map_err(|_| shux_rpc::RpcError::invalid_params("invalid window id format"))?;
                let new_title = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'name' parameter"))?
                    .to_string();
                gh.rename_window(window_id, new_title)
                    .await
                    .map_err(graph_error_to_rpc)?;
                let snap = gh.snapshot();
                let window = snap
                    .windows
                    .get(&window_id)
                    .ok_or_else(|| shux_rpc::RpcError::internal("window vanished after rename"))?;
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
        })
        .register("window.focus", move |params: Option<serde_json::Value>| {
            let gh = g5.clone();
            async move {
                let params = params.unwrap_or_default();
                let window_id_str = params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'id' parameter"))?;
                let window_id: shux_core::model::WindowId = window_id_str
                    .parse()
                    .map_err(|_| shux_rpc::RpcError::invalid_params("invalid window id format"))?;
                let previous = gh
                    .focus_window(window_id)
                    .await
                    .map_err(graph_error_to_rpc)?;
                let snap = gh.snapshot();
                let window = snap
                    .windows
                    .get(&window_id)
                    .ok_or_else(|| shux_rpc::RpcError::internal("window vanished after focus"))?;
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
        })
        .register(
            "window.reorder",
            move |params: Option<serde_json::Value>| {
                let gh = g6.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let window_id_str =
                        params.get("id").and_then(|v| v.as_str()).ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'id' parameter")
                        })?;
                    let window_id: shux_core::model::WindowId =
                        window_id_str.parse().map_err(|_| {
                            shux_rpc::RpcError::invalid_params("invalid window id format")
                        })?;
                    let new_index = params
                        .get("new_index")
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'new_index' parameter")
                        })? as usize;
                    gh.reorder_window(window_id, new_index)
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
        .register("window.kill", move |params: Option<serde_json::Value>| {
            let gh = g7.clone();
            async move {
                let params = params.unwrap_or_default();
                let window_id_str = params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'id' parameter"))?;
                let window_id: shux_core::model::WindowId = window_id_str
                    .parse()
                    .map_err(|_| shux_rpc::RpcError::invalid_params("invalid window id format"))?;
                gh.destroy_window(window_id, None)
                    .await
                    .map_err(graph_error_to_rpc)?;
                Ok(serde_json::json!({ "killed": window_id_str }))
            }
        })
}

/// Build pane info JSON from a Pane.
fn pane_to_json(
    p: &shux_core::model::Pane,
    window: &shux_core::model::Window,
) -> serde_json::Value {
    let is_focused = window.active_pane == p.id;
    let is_zoomed = window.layout.is_zoomed()
        && window
            .layout
            .zoom
            .as_ref()
            .is_some_and(|z| z.zoomed_pane == p.id);
    serde_json::json!({
        "id": p.id.to_string(),
        "window_id": p.window_id.to_string(),
        "title": p.title,
        "cwd": p.cwd.to_string_lossy(),
        "command": p.command,
        "exit_status": p.exit_status,
        "is_focused": is_focused,
        "is_zoomed": is_zoomed,
        "version": p.version,
    })
}

/// Register pane operation methods on the router builder.
fn register_pane_methods(
    builder: shux_rpc::RouterBuilder,
    graph: shux_core::graph::GraphHandle,
) -> shux_rpc::RouterBuilder {
    let g1 = graph.clone();
    let g2 = graph.clone();
    let g3 = graph.clone();
    let g4 = graph.clone();
    let g5 = graph.clone();
    let g6 = graph.clone();
    let g7 = graph.clone();
    let g8 = graph.clone();

    builder
        .register("pane.list", move |params: Option<serde_json::Value>| {
            let gh = g1.clone();
            async move {
                let params = params.unwrap_or_default();
                let window_id = resolve_window_id_from_params(&gh, &params)?;
                let snap = gh.snapshot();
                let window = snap.windows.get(&window_id).ok_or_else(|| {
                    shux_rpc::RpcError::not_found("window", &window_id.to_string())
                })?;
                let panes: Vec<serde_json::Value> = snap
                    .panes
                    .values()
                    .filter(|p| p.window_id == window_id)
                    .map(|p| pane_to_json(p, window))
                    .collect();
                Ok(serde_json::json!(panes))
            }
        })
        .register("pane.split", move |params: Option<serde_json::Value>| {
            let gh = g2.clone();
            async move {
                let params = params.unwrap_or_default();
                let pane_id = resolve_pane_id_from_params(&gh, &params)?;
                let direction = match params.get("direction").and_then(|v| v.as_str()) {
                    Some("horizontal") | Some("h") => shux_core::layout::Direction::Horizontal,
                    Some("vertical") | Some("v") => shux_core::layout::Direction::Vertical,
                    None | Some("auto") => shux_core::layout::Direction::Vertical,
                    Some(other) => {
                        return Err(shux_rpc::RpcError::invalid_params(&format!(
                            "invalid direction '{other}'"
                        )));
                    }
                };
                let ratio = params.get("ratio").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
                let new_pane_id = gh
                    .split_pane(pane_id, direction, ratio)
                    .await
                    .map_err(graph_error_to_rpc)?;
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
        .register("pane.focus", move |params: Option<serde_json::Value>| {
            let gh = g3.clone();
            async move {
                let params = params.unwrap_or_default();
                let pane_id_str = params
                    .get("pane_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'pane_id'"))?;
                let pane_id: shux_core::model::PaneId = pane_id_str
                    .parse()
                    .map_err(|_| shux_rpc::RpcError::invalid_params("invalid pane_id"))?;
                let previous = gh.focus_pane(pane_id).await.map_err(graph_error_to_rpc)?;
                Ok(serde_json::json!({
                    "pane_id": pane_id.to_string(),
                    "previous_pane_id": previous.map(|id| id.to_string()),
                }))
            }
        })
        .register(
            "pane.focus_direction",
            move |params: Option<serde_json::Value>| {
                let gh = g4.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let window_id = resolve_window_id_from_params(&gh, &params)?;
                    let dir_str = params
                        .get("direction")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'direction'"))?;
                    let direction = match dir_str.to_lowercase().as_str() {
                        "up" => shux_core::layout::NavDirection::Up,
                        "down" => shux_core::layout::NavDirection::Down,
                        "left" => shux_core::layout::NavDirection::Left,
                        "right" => shux_core::layout::NavDirection::Right,
                        other => {
                            return Err(shux_rpc::RpcError::invalid_params(&format!(
                                "invalid direction '{other}'"
                            )));
                        }
                    };
                    let viewport = shux_core::layout::Rect::new(0, 0, 120, 40);
                    let snap = gh.snapshot();
                    let window = snap.windows.get(&window_id).ok_or_else(|| {
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
        .register("pane.resize", move |params: Option<serde_json::Value>| {
            let gh = g5.clone();
            async move {
                let params = params.unwrap_or_default();
                let pane_id = resolve_pane_id_from_params(&gh, &params)?;
                let dir_str = params
                    .get("direction")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'direction'"))?;
                let direction = match dir_str.to_lowercase().as_str() {
                    "horizontal" | "h" => shux_core::layout::Direction::Horizontal,
                    "vertical" | "v" => shux_core::layout::Direction::Vertical,
                    other => {
                        return Err(shux_rpc::RpcError::invalid_params(&format!(
                            "invalid direction '{other}'"
                        )));
                    }
                };
                let delta = params.get("delta").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
                gh.resize_pane(pane_id, direction, delta)
                    .await
                    .map_err(graph_error_to_rpc)?;
                Ok(serde_json::json!({ "pane_id": pane_id.to_string() }))
            }
        })
        .register("pane.zoom", move |params: Option<serde_json::Value>| {
            let gh = g6.clone();
            async move {
                let params = params.unwrap_or_default();
                let pane_id = resolve_pane_id_from_params(&gh, &params)?;
                let is_zoomed = gh.zoom_pane(pane_id).await.map_err(graph_error_to_rpc)?;
                Ok(serde_json::json!({
                    "pane_id": pane_id.to_string(),
                    "is_zoomed": is_zoomed,
                }))
            }
        })
        .register("pane.swap", move |params: Option<serde_json::Value>| {
            let gh = g7.clone();
            async move {
                let params = params.unwrap_or_default();
                let pane_id_str = params
                    .get("pane_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'pane_id'"))?;
                let target_str = params
                    .get("target_pane_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        shux_rpc::RpcError::invalid_params("missing 'target_pane_id'")
                    })?;
                let pane_a: shux_core::model::PaneId = pane_id_str
                    .parse()
                    .map_err(|_| shux_rpc::RpcError::invalid_params("invalid pane_id"))?;
                let pane_b: shux_core::model::PaneId = target_str
                    .parse()
                    .map_err(|_| shux_rpc::RpcError::invalid_params("invalid target_pane_id"))?;
                gh.swap_panes(pane_a, pane_b)
                    .await
                    .map_err(graph_error_to_rpc)?;
                Ok(serde_json::json!({
                    "pane_a": pane_a.to_string(),
                    "pane_b": pane_b.to_string(),
                }))
            }
        })
        .register("pane.kill", move |params: Option<serde_json::Value>| {
            let gh = g8.clone();
            async move {
                let params = params.unwrap_or_default();
                let pane_id_str = params
                    .get("pane_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'pane_id'"))?;
                let pane_id: shux_core::model::PaneId = pane_id_str
                    .parse()
                    .map_err(|_| shux_rpc::RpcError::invalid_params("invalid pane_id"))?;
                gh.destroy_pane(pane_id).await.map_err(graph_error_to_rpc)?;
                Ok(serde_json::json!({ "killed": pane_id_str }))
            }
        })
}

/// Resolve a pane_id from params: either explicit or active pane of resolved window.
fn resolve_pane_id_from_params(
    gh: &shux_core::graph::GraphHandle,
    params: &serde_json::Value,
) -> Result<shux_core::model::PaneId, shux_rpc::RpcError> {
    if let Some(pane_id_str) = params.get("pane_id").and_then(|v| v.as_str()) {
        return pane_id_str
            .parse()
            .map_err(|_| shux_rpc::RpcError::invalid_params("invalid pane_id format"));
    }
    let window_id = resolve_window_id_from_params(gh, params)?;
    let snap = gh.snapshot();
    let window = snap
        .windows
        .get(&window_id)
        .ok_or_else(|| shux_rpc::RpcError::not_found("window", &window_id.to_string()))?;
    Ok(window.active_pane)
}

/// Resolve a window_id from params: either explicit or session's active window.
fn resolve_window_id_from_params(
    gh: &shux_core::graph::GraphHandle,
    params: &serde_json::Value,
) -> Result<shux_core::model::WindowId, shux_rpc::RpcError> {
    if let Some(wid_str) = params.get("window_id").and_then(|v| v.as_str()) {
        return wid_str
            .parse()
            .map_err(|_| shux_rpc::RpcError::invalid_params("invalid window_id format"));
    }
    let session_id_str = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            shux_rpc::RpcError::invalid_params("missing 'pane_id' or 'window_id' or 'session_id'")
        })?;
    let session_id: shux_core::model::SessionId = session_id_str
        .parse()
        .map_err(|_| shux_rpc::RpcError::invalid_params("invalid session_id format"))?;
    let snap = gh.snapshot();
    let session = snap
        .sessions
        .get(&session_id)
        .ok_or_else(|| shux_rpc::RpcError::not_found("session", session_id_str))?;
    Ok(session.active_window)
}

/// Start a test server (RPC + SessionGraph) on an ephemeral UDS.
/// Returns (socket_path, cancel_token).
async fn start_test_server(
    dir: &std::path::Path,
) -> (PathBuf, tokio_util::sync::CancellationToken) {
    let socket_path = dir.join("m0-test.sock");
    let cancel = tokio_util::sync::CancellationToken::new();

    let (graph, state) = shux_core::graph::SessionGraph::new();
    let (graph_tx, graph_rx) = tokio::sync::mpsc::channel(256);
    let graph_handle = shux_core::graph::GraphHandle::new(graph_tx, state);

    let graph_cancel = cancel.clone();
    tokio::spawn(async move {
        shux_core::graph::run_graph_loop(graph, graph_rx, graph_cancel).await;
    });

    let router = register_pane_methods(
        register_window_methods(
            register_session_methods(
                shux_rpc::server::register_builtin_methods(shux_rpc::Router::builder()),
                graph_handle.clone(),
            ),
            graph_handle.clone(),
        ),
        graph_handle,
    )
    .build();

    let config = shux_rpc::ServerConfig {
        socket_path: socket_path.clone(),
        tcp_addr: String::new(),
        auth_token: None,
    };

    let server = shux_rpc::Server::new(config, router, cancel.clone());

    tokio::spawn(async move {
        let _ = server.run().await;
    });

    for _ in 0..20 {
        if UnixStream::connect(&socket_path).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    (socket_path, cancel)
}

/// Send a JSON-RPC request over a framed UDS connection and get the response.
async fn rpc_raw(
    socket_path: &std::path::Path,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let stream = UnixStream::connect(socket_path).await.unwrap();
    let mut framed = Framed::new(stream, shux_rpc::create_codec());

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": uuid::Uuid::new_v4().to_string(),
        "method": method,
        "params": params,
    });
    let payload = serde_json::to_vec(&request).unwrap();
    framed.send(Bytes::from(payload)).await.unwrap();

    let response_frame = framed.next().await.unwrap().unwrap();
    serde_json::from_slice(&response_frame).unwrap()
}

// ══════════════════════════════════════════════════════════════
// M0 "Done when" tests (PRD §17)
// ══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_m0_system_version() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let response = rpc_raw(&socket_path, "system.version", serde_json::json!({})).await;

    assert_eq!(response["jsonrpc"], "2.0");
    let version = response["result"]["version"].as_str().unwrap();
    assert!(!version.is_empty());
    assert!(version.contains('.'), "version should be semver: {version}");
    assert_eq!(response["result"]["name"], "shux");

    cancel.cancel();
}

#[tokio::test]
async fn test_m0_system_health() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let response = rpc_raw(&socket_path, "system.health", serde_json::json!({})).await;
    assert_eq!(response["result"]["status"], "ok");

    cancel.cancel();
}

#[tokio::test]
async fn test_m0_create_session() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let response = rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "test"}),
    )
    .await;

    assert!(
        response["result"]["id"].is_string(),
        "session.create should return id"
    );
    assert_eq!(response["result"]["name"], "test");

    cancel.cancel();
}

#[tokio::test]
async fn test_m0_list_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    // Create a session
    rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "list-test"}),
    )
    .await;

    // List sessions
    let response = rpc_raw(&socket_path, "session.list", serde_json::json!({})).await;
    let sessions = response["result"]["sessions"].as_array().unwrap();
    assert!(!sessions.is_empty(), "should have at least 1 session");

    let found = sessions
        .iter()
        .any(|s| s["name"].as_str() == Some("list-test"));
    assert!(found, "session 'list-test' should appear in list");

    cancel.cancel();
}

#[tokio::test]
async fn test_m0_session_kill() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "doomed"}),
    )
    .await;

    let kill_response = rpc_raw(
        &socket_path,
        "session.kill",
        serde_json::json!({"name": "doomed"}),
    )
    .await;
    assert!(
        kill_response["error"].is_null(),
        "kill should succeed: {kill_response}"
    );

    // Verify it is gone
    let list_response = rpc_raw(&socket_path, "session.list", serde_json::json!({})).await;
    let sessions = list_response["result"]["sessions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let found = sessions
        .iter()
        .any(|s| s["name"].as_str() == Some("doomed"));
    assert!(!found, "killed session should not appear in list");

    cancel.cancel();
}

#[tokio::test]
async fn test_m0_detach_reattach() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let create_resp = rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "persist-test"}),
    )
    .await;
    let session_id = create_resp["result"]["id"].as_str().unwrap().to_string();

    // "Detach" by dropping the connection
    {
        let _stream = UnixStream::connect(&socket_path).await.unwrap();
        // stream dropped here
    }

    tokio::time::sleep(Duration::from_millis(100)).await;

    // "Reattach" — the session should still exist
    let list_response = rpc_raw(&socket_path, "session.list", serde_json::json!({})).await;
    let sessions = list_response["result"]["sessions"].as_array().unwrap();
    let found = sessions
        .iter()
        .any(|s| s["id"].as_str() == Some(&session_id));
    assert!(found, "session should persist after client disconnect");

    cancel.cancel();
}

#[tokio::test]
async fn test_m0_multiple_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    for name in ["alpha", "beta", "gamma"] {
        rpc_raw(
            &socket_path,
            "session.create",
            serde_json::json!({"name": name}),
        )
        .await;
    }

    let response = rpc_raw(&socket_path, "session.list", serde_json::json!({})).await;
    let sessions = response["result"]["sessions"].as_array().unwrap();
    assert!(sessions.len() >= 3, "should have 3+ sessions");

    for name in ["alpha", "beta", "gamma"] {
        let found = sessions.iter().any(|s| s["name"].as_str() == Some(name));
        assert!(found, "session '{name}' should be in list");
    }

    cancel.cancel();
}

#[tokio::test]
async fn test_m0_invalid_method() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let response = rpc_raw(&socket_path, "nonexistent.method", serde_json::json!({})).await;
    assert!(response["error"].is_object(), "should return an error");
    assert_eq!(response["error"]["code"], -32601);

    cancel.cancel();
}

#[tokio::test]
async fn test_m0_concurrent_connections() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let mut handles = Vec::new();
    for i in 0..5 {
        let path = socket_path.clone();
        handles.push(tokio::spawn(async move {
            let response = rpc_raw(&path, "system.version", serde_json::json!({})).await;
            assert!(
                response["result"]["version"].is_string(),
                "concurrent request {i} should succeed"
            );
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    cancel.cancel();
}

#[tokio::test]
async fn test_m0_session_ensure() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    // First ensure: creates session
    let resp1 = rpc_raw(
        &socket_path,
        "session.ensure",
        serde_json::json!({"name": "ensure-test"}),
    )
    .await;
    assert_eq!(resp1["result"]["name"], "ensure-test");
    assert_eq!(resp1["result"]["created"], true);
    let id1 = resp1["result"]["id"].as_str().unwrap().to_string();

    // Second ensure: returns existing session
    let resp2 = rpc_raw(
        &socket_path,
        "session.ensure",
        serde_json::json!({"name": "ensure-test"}),
    )
    .await;
    assert_eq!(resp2["result"]["name"], "ensure-test");
    assert_eq!(resp2["result"]["created"], false);
    assert_eq!(resp2["result"]["id"].as_str().unwrap(), id1);

    cancel.cancel();
}

// ══════════════════════════════════════════════════════════════
// L2: PTY Integration Tests (crate-level)
// ══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_m0_pty_spawn_echo() {
    let config = shux_pty::PtyConfig::with_command(
        vec!["echo".into(), "SHUX_M0_PTY".into()],
        PathBuf::from("/tmp"),
    );

    let mut handle = shux_pty::PtyHandle::spawn(&config).unwrap();

    let mut output = Vec::new();
    let mut buf = [0u8; 4096];

    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match handle.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => output.extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
        }
    })
    .await;

    assert!(
        String::from_utf8_lossy(&output).contains("SHUX_M0_PTY"),
        "echo output should contain marker"
    );
}

#[tokio::test]
async fn test_m0_pty_exit_status() {
    for (cmd, expected_success) in [("true", true), ("false", false)] {
        let config = shux_pty::PtyConfig::with_command(vec![cmd.into()], PathBuf::from("/tmp"));
        let mut handle = shux_pty::PtyHandle::spawn(&config).unwrap();
        let status = handle.wait().await.unwrap();
        assert_eq!(
            status.success(),
            expected_success,
            "unexpected exit status for {cmd}"
        );
    }
}

// ══════════════════════════════════════════════════════════════
// CLI binary tests (uses the compiled shux binary)
// ══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_m0_cli_version_json() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shux"))
        .args([
            "--format",
            "json",
            "--socket",
            socket_path.to_str().unwrap(),
            "api",
            "system.version",
        ])
        .output()
        .await
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "shux api system.version should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("output should be valid JSON: {stdout}"));
    assert!(parsed["version"].is_string());

    cancel.cancel();
}

#[tokio::test]
async fn test_m0_cli_ls() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    // Create a session via RPC first
    rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "cli-ls-test"}),
    )
    .await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shux"))
        .args(["--socket", socket_path.to_str().unwrap(), "ls"])
        .output()
        .await
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("cli-ls-test"),
        "ls output should contain session name. Got: {stdout}"
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_m0_cli_new_detached() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shux"))
        .args([
            "--socket",
            socket_path.to_str().unwrap(),
            "new",
            "-s",
            "cli-new-test",
            "-d",
        ])
        .output()
        .await
        .unwrap();

    assert!(
        output.status.success(),
        "shux new should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify session exists via RPC
    let response = rpc_raw(&socket_path, "session.list", serde_json::json!({})).await;
    let sessions = response["result"]["sessions"].as_array().unwrap();
    let found = sessions
        .iter()
        .any(|s| s["name"].as_str() == Some("cli-new-test"));
    assert!(found, "session created via CLI should appear in list");

    cancel.cancel();
}

#[tokio::test]
async fn test_m0_cli_kill() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    // Create a session via RPC
    rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "cli-kill-test"}),
    )
    .await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shux"))
        .args([
            "--socket",
            socket_path.to_str().unwrap(),
            "kill",
            "-s",
            "cli-kill-test",
        ])
        .output()
        .await
        .unwrap();

    assert!(
        output.status.success(),
        "shux kill should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify session is gone
    let response = rpc_raw(&socket_path, "session.list", serde_json::json!({})).await;
    let sessions = response["result"]["sessions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let found = sessions
        .iter()
        .any(|s| s["name"].as_str() == Some("cli-kill-test"));
    assert!(!found, "killed session should be gone");

    cancel.cancel();
}

#[tokio::test]
async fn test_m0_cli_ls_json() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "json-test"}),
    )
    .await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shux"))
        .args([
            "--format",
            "json",
            "--socket",
            socket_path.to_str().unwrap(),
            "ls",
        ])
        .output()
        .await
        .unwrap();

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("ls --format json should return valid JSON: {stdout}"));
    assert!(
        parsed["sessions"].is_array(),
        "JSON ls output should contain sessions array"
    );

    cancel.cancel();
}

// ══════════════════════════════════════════════════════════════
// Task 013: Session CRUD tests
// ══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_013_session_rename() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let create_resp = rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "old-name"}),
    )
    .await;
    assert!(create_resp["error"].is_null(), "create should succeed");

    // Rename by name
    let rename_resp = rpc_raw(
        &socket_path,
        "session.rename",
        serde_json::json!({"name": "old-name", "new_name": "new-name"}),
    )
    .await;
    assert!(
        rename_resp["error"].is_null(),
        "rename should succeed: {rename_resp}"
    );
    assert_eq!(rename_resp["result"]["name"], "new-name");

    // Verify old name is gone
    let list_resp = rpc_raw(&socket_path, "session.list", serde_json::json!({})).await;
    let sessions = list_resp["result"]["sessions"].as_array().unwrap();
    let names: Vec<&str> = sessions.iter().filter_map(|s| s["name"].as_str()).collect();
    assert!(names.contains(&"new-name"));
    assert!(!names.contains(&"old-name"));

    cancel.cancel();
}

#[tokio::test]
async fn test_013_session_rename_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "first"}),
    )
    .await;
    rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "second"}),
    )
    .await;

    let rename_resp = rpc_raw(
        &socket_path,
        "session.rename",
        serde_json::json!({"name": "second", "new_name": "first"}),
    )
    .await;
    assert!(
        rename_resp["error"].is_object(),
        "rename to existing name should fail"
    );
    assert_eq!(
        rename_resp["error"]["code"],
        shux_rpc::ErrorCode::NameConflict.code()
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_013_session_name_validation_empty() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let resp = rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": ""}),
    )
    .await;
    // Explicit empty name should fail validation (auto-generate only when name is absent)
    assert!(
        resp["error"].is_object(),
        "empty name should fail validation: {resp}"
    );
    assert_eq!(
        resp["error"]["code"],
        shux_rpc::ErrorCode::InvalidParams.code()
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_013_session_name_validation_spaces() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let resp = rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "bad name"}),
    )
    .await;
    assert!(
        resp["error"].is_object(),
        "name with spaces should fail: {resp}"
    );
    assert_eq!(
        resp["error"]["code"],
        shux_rpc::ErrorCode::InvalidParams.code()
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_013_session_name_validation_too_long() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let long_name = "a".repeat(129);
    let resp = rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": long_name}),
    )
    .await;
    assert!(
        resp["error"].is_object(),
        "name too long should fail: {resp}"
    );
    assert_eq!(
        resp["error"]["code"],
        shux_rpc::ErrorCode::InvalidParams.code()
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_013_session_ensure_idempotency_triple() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let r1 = rpc_raw(
        &socket_path,
        "session.ensure",
        serde_json::json!({"name": "idem"}),
    )
    .await;
    let r2 = rpc_raw(
        &socket_path,
        "session.ensure",
        serde_json::json!({"name": "idem"}),
    )
    .await;
    let r3 = rpc_raw(
        &socket_path,
        "session.ensure",
        serde_json::json!({"name": "idem"}),
    )
    .await;

    assert_eq!(r1["result"]["id"], r2["result"]["id"]);
    assert_eq!(r2["result"]["id"], r3["result"]["id"]);
    assert_eq!(r1["result"]["created"], true);
    assert_eq!(r2["result"]["created"], false);
    assert_eq!(r3["result"]["created"], false);

    cancel.cancel();
}

#[tokio::test]
async fn test_013_session_kill_nonexistent() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let resp = rpc_raw(
        &socket_path,
        "session.kill",
        serde_json::json!({"name": "nonexistent"}),
    )
    .await;
    assert!(
        resp["error"].is_object(),
        "killing nonexistent session should fail"
    );
    assert_eq!(resp["error"]["code"], shux_rpc::ErrorCode::NotFound.code());

    cancel.cancel();
}

#[tokio::test]
async fn test_013_session_create_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "dupe"}),
    )
    .await;

    let resp = rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "dupe"}),
    )
    .await;
    assert!(resp["error"].is_object(), "duplicate name should fail");
    assert_eq!(
        resp["error"]["code"],
        shux_rpc::ErrorCode::NameConflict.code()
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_013_session_list_sorted_by_creation() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    for name in ["alpha", "beta", "gamma"] {
        rpc_raw(
            &socket_path,
            "session.create",
            serde_json::json!({"name": name}),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let resp = rpc_raw(&socket_path, "session.list", serde_json::json!({})).await;
    let sessions = resp["result"]["sessions"].as_array().unwrap();
    let names: Vec<&str> = sessions.iter().filter_map(|s| s["name"].as_str()).collect();
    assert_eq!(names, vec!["alpha", "beta", "gamma"]);

    cancel.cancel();
}

#[tokio::test]
async fn test_013_session_list_has_window_count() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "counted"}),
    )
    .await;

    let resp = rpc_raw(&socket_path, "session.list", serde_json::json!({})).await;
    let sessions = resp["result"]["sessions"].as_array().unwrap();
    assert_eq!(sessions[0]["window_count"], 1);
    assert!(sessions[0]["active_window_id"].is_string());

    cancel.cancel();
}

#[tokio::test]
async fn test_013_session_create_has_window_pane_ids() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let resp = rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "with-ids"}),
    )
    .await;

    assert!(
        resp["result"]["window_id"].is_string(),
        "create should return window_id"
    );
    assert!(
        resp["result"]["pane_id"].is_string(),
        "create should return pane_id"
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_013_session_auto_name() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    // Create with no name — should auto-generate
    let resp = rpc_raw(&socket_path, "session.create", serde_json::json!({})).await;
    assert!(resp["error"].is_null(), "auto-name create should succeed");
    let name = resp["result"]["name"].as_str().unwrap();
    assert!(
        name.starts_with("session-"),
        "auto-name should start with 'session-', got: {name}"
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_013_session_kill_by_id() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let create_resp = rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "kill-by-id"}),
    )
    .await;
    let session_id = create_resp["result"]["id"].as_str().unwrap().to_string();

    let kill_resp = rpc_raw(
        &socket_path,
        "session.kill",
        serde_json::json!({"id": session_id}),
    )
    .await;
    assert!(
        kill_resp["error"].is_null(),
        "kill by ID should succeed: {kill_resp}"
    );

    // Verify it is gone
    let list_resp = rpc_raw(&socket_path, "session.list", serde_json::json!({})).await;
    let sessions = list_resp["result"]["sessions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(sessions.is_empty());

    cancel.cancel();
}

#[tokio::test]
async fn test_013_cli_rename() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    // Create via RPC
    rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "cli-rename-old"}),
    )
    .await;

    // Rename via CLI
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shux"))
        .args([
            "--socket",
            socket_path.to_str().unwrap(),
            "rename",
            "-s",
            "cli-rename-old",
            "-n",
            "cli-rename-new",
        ])
        .output()
        .await
        .unwrap();

    assert!(
        output.status.success(),
        "shux rename should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Renamed") || stdout.contains("renamed"),
        "rename output should confirm: {stdout}"
    );

    // Verify via RPC
    let list_resp = rpc_raw(&socket_path, "session.list", serde_json::json!({})).await;
    let sessions = list_resp["result"]["sessions"].as_array().unwrap();
    let names: Vec<&str> = sessions.iter().filter_map(|s| s["name"].as_str()).collect();
    assert!(names.contains(&"cli-rename-new"));
    assert!(!names.contains(&"cli-rename-old"));

    cancel.cancel();
}

// ══════════════════════════════════════════════════════════════
// Task 014: Window CRUD tests
// ══════════════════════════════════════════════════════════════

/// Helper: create a session and return its ID.
async fn create_session(socket_path: &std::path::Path, name: &str) -> String {
    let resp = rpc_raw(
        socket_path,
        "session.create",
        serde_json::json!({"name": name}),
    )
    .await;
    assert!(
        resp["error"].is_null(),
        "session.create should succeed: {resp}"
    );
    resp["result"]["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn test_014_window_create() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let session_id = create_session(&socket_path, "win-test").await;

    let resp = rpc_raw(
        &socket_path,
        "window.create",
        serde_json::json!({"session_id": session_id, "name": "editor"}),
    )
    .await;
    assert!(
        resp["error"].is_null(),
        "window.create should succeed: {resp}"
    );
    assert_eq!(resp["result"]["title"], "editor");
    assert!(resp["result"]["id"].is_string());
    assert!(resp["result"]["is_active"].as_bool().unwrap());

    cancel.cancel();
}

#[tokio::test]
async fn test_014_window_create_auto_name() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let session_id = create_session(&socket_path, "auto-win").await;

    // Session already has window "0" (default). Creating without name should auto-name.
    let resp = rpc_raw(
        &socket_path,
        "window.create",
        serde_json::json!({"session_id": session_id}),
    )
    .await;
    assert!(
        resp["error"].is_null(),
        "auto-name create should succeed: {resp}"
    );
    let title = resp["result"]["title"].as_str().unwrap();
    // Auto-name is index-based; first window is "0", new one should be "1"
    assert!(!title.is_empty(), "auto-named window should have a title");

    cancel.cancel();
}

#[tokio::test]
async fn test_014_window_list() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let session_id = create_session(&socket_path, "list-win").await;

    // Create a second window
    rpc_raw(
        &socket_path,
        "window.create",
        serde_json::json!({"session_id": session_id, "name": "second"}),
    )
    .await;

    let resp = rpc_raw(
        &socket_path,
        "window.list",
        serde_json::json!({"session_id": session_id}),
    )
    .await;
    let windows = resp["result"].as_array().unwrap();
    assert_eq!(windows.len(), 2, "should have 2 windows");

    // Last created window should be active
    let active_count = windows
        .iter()
        .filter(|w| w["is_active"].as_bool().unwrap_or(false))
        .count();
    assert_eq!(active_count, 1, "exactly one window should be active");

    cancel.cancel();
}

#[tokio::test]
async fn test_014_window_list_missing_session() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let resp = rpc_raw(
        &socket_path,
        "window.list",
        serde_json::json!({"session_id": "00000000-0000-0000-0000-000000000000"}),
    )
    .await;
    assert!(
        resp["error"].is_object(),
        "listing windows for nonexistent session should fail"
    );
    assert_eq!(resp["error"]["code"], shux_rpc::ErrorCode::NotFound.code());

    cancel.cancel();
}

#[tokio::test]
async fn test_014_window_kill() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let session_id = create_session(&socket_path, "kill-win").await;

    // Create a second window so we can kill one
    let create_resp = rpc_raw(
        &socket_path,
        "window.create",
        serde_json::json!({"session_id": session_id, "name": "doomed"}),
    )
    .await;
    let window_id = create_resp["result"]["id"].as_str().unwrap().to_string();

    let kill_resp = rpc_raw(
        &socket_path,
        "window.kill",
        serde_json::json!({"id": window_id}),
    )
    .await;
    assert!(
        kill_resp["error"].is_null(),
        "window.kill should succeed: {kill_resp}"
    );

    // Verify only 1 window remains
    let list_resp = rpc_raw(
        &socket_path,
        "window.list",
        serde_json::json!({"session_id": session_id}),
    )
    .await;
    let windows = list_resp["result"].as_array().unwrap();
    assert_eq!(windows.len(), 1, "should have 1 window after kill");

    cancel.cancel();
}

#[tokio::test]
async fn test_014_window_kill_last_fails() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let session_id = create_session(&socket_path, "last-win").await;

    // Get the only window's ID
    let list_resp = rpc_raw(
        &socket_path,
        "window.list",
        serde_json::json!({"session_id": session_id}),
    )
    .await;
    let window_id = list_resp["result"][0]["id"].as_str().unwrap().to_string();

    // Try to kill the last window — should fail
    let kill_resp = rpc_raw(
        &socket_path,
        "window.kill",
        serde_json::json!({"id": window_id}),
    )
    .await;
    assert!(
        kill_resp["error"].is_object(),
        "killing last window should fail: {kill_resp}"
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_014_window_rename() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let session_id = create_session(&socket_path, "rename-win").await;

    let list_resp = rpc_raw(
        &socket_path,
        "window.list",
        serde_json::json!({"session_id": session_id}),
    )
    .await;
    let window_id = list_resp["result"][0]["id"].as_str().unwrap().to_string();

    let rename_resp = rpc_raw(
        &socket_path,
        "window.rename",
        serde_json::json!({"id": window_id, "name": "renamed-window"}),
    )
    .await;
    assert!(
        rename_resp["error"].is_null(),
        "window.rename should succeed: {rename_resp}"
    );
    assert_eq!(rename_resp["result"]["title"], "renamed-window");

    cancel.cancel();
}

#[tokio::test]
async fn test_014_window_focus() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let session_id = create_session(&socket_path, "focus-win").await;

    // Get the initial window
    let list_resp = rpc_raw(
        &socket_path,
        "window.list",
        serde_json::json!({"session_id": session_id}),
    )
    .await;
    let first_win_id = list_resp["result"][0]["id"].as_str().unwrap().to_string();

    // Create a second window (becomes active)
    rpc_raw(
        &socket_path,
        "window.create",
        serde_json::json!({"session_id": session_id, "name": "second"}),
    )
    .await;

    // Focus back to first window
    let focus_resp = rpc_raw(
        &socket_path,
        "window.focus",
        serde_json::json!({"id": first_win_id}),
    )
    .await;
    assert!(
        focus_resp["error"].is_null(),
        "window.focus should succeed: {focus_resp}"
    );
    assert!(focus_resp["result"]["is_active"].as_bool().unwrap());
    assert!(focus_resp["result"]["previous_window_id"].is_string());

    cancel.cancel();
}

#[tokio::test]
async fn test_014_window_reorder() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let session_id = create_session(&socket_path, "reorder-win").await;

    // Create 2 more windows (total 3)
    rpc_raw(
        &socket_path,
        "window.create",
        serde_json::json!({"session_id": session_id, "name": "w1"}),
    )
    .await;
    let create_resp = rpc_raw(
        &socket_path,
        "window.create",
        serde_json::json!({"session_id": session_id, "name": "w2"}),
    )
    .await;
    let w2_id = create_resp["result"]["id"].as_str().unwrap().to_string();

    // Move w2 to index 0
    let reorder_resp = rpc_raw(
        &socket_path,
        "window.reorder",
        serde_json::json!({"id": w2_id, "new_index": 0}),
    )
    .await;
    assert!(
        reorder_resp["error"].is_null(),
        "window.reorder should succeed: {reorder_resp}"
    );
    assert_eq!(reorder_resp["result"]["index"], 0);

    cancel.cancel();
}

#[tokio::test]
async fn test_014_window_ensure_create() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let session_id = create_session(&socket_path, "ensure-win").await;

    let resp1 = rpc_raw(
        &socket_path,
        "window.ensure",
        serde_json::json!({"session_id": session_id, "name": "ensured"}),
    )
    .await;
    assert!(resp1["error"].is_null(), "ensure should succeed: {resp1}");
    assert_eq!(resp1["result"]["title"], "ensured");
    assert_eq!(resp1["result"]["created"], true);

    // Second ensure returns existing
    let resp2 = rpc_raw(
        &socket_path,
        "window.ensure",
        serde_json::json!({"session_id": session_id, "name": "ensured"}),
    )
    .await;
    assert_eq!(resp2["result"]["created"], false);
    assert_eq!(resp2["result"]["id"], resp1["result"]["id"]);

    cancel.cancel();
}

#[tokio::test]
async fn test_014_window_new_becomes_active() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let session_id = create_session(&socket_path, "active-win").await;

    // Create a second window
    let create_resp = rpc_raw(
        &socket_path,
        "window.create",
        serde_json::json!({"session_id": session_id, "name": "new-active"}),
    )
    .await;
    assert!(create_resp["result"]["is_active"].as_bool().unwrap());

    // Verify via list
    let list_resp = rpc_raw(
        &socket_path,
        "window.list",
        serde_json::json!({"session_id": session_id}),
    )
    .await;
    let windows = list_resp["result"].as_array().unwrap();
    let active: Vec<_> = windows
        .iter()
        .filter(|w| w["is_active"].as_bool().unwrap_or(false))
        .collect();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0]["title"], "new-active");

    cancel.cancel();
}

// ══════════════════════════════════════════════════════════════
// Task 014: Window CLI tests
// ══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_014_cli_window_list() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let session_id = create_session(&socket_path, "cli-wl").await;
    rpc_raw(
        &socket_path,
        "window.create",
        serde_json::json!({"session_id": session_id, "name": "editor"}),
    )
    .await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shux"))
        .args([
            "--socket",
            socket_path.to_str().unwrap(),
            "window",
            "list",
            "-s",
            "cli-wl",
        ])
        .output()
        .await
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("editor"),
        "window list should show 'editor': {stdout}"
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_014_cli_window_new() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    create_session(&socket_path, "cli-wn").await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shux"))
        .args([
            "--socket",
            socket_path.to_str().unwrap(),
            "window",
            "new",
            "-s",
            "cli-wn",
            "-n",
            "cli-created",
        ])
        .output()
        .await
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("Created") || stdout.contains("created"),
        "should confirm creation: {stdout}"
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_014_cli_window_list_json() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    create_session(&socket_path, "cli-wlj").await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shux"))
        .args([
            "--format",
            "json",
            "--socket",
            socket_path.to_str().unwrap(),
            "window",
            "list",
            "-s",
            "cli-wlj",
        ])
        .output()
        .await
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("should be valid JSON: {stdout}"));
    assert!(parsed.is_array(), "JSON window list should be an array");

    cancel.cancel();
}

// ══════════════════════════════════════════════════════════════
// Task 015: Pane Operations
// ══════════════════════════════════════════════════════════════

/// Send a JSON-RPC request and extract the result, panicking on error.
async fn rpc_ok(
    socket_path: &std::path::Path,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let resp = rpc_raw(socket_path, method, params).await;
    assert!(resp["error"].is_null(), "{method} should succeed: {resp}");
    resp["result"].clone()
}

/// Helper: create a session and return (session_id, window_id, pane_id)
async fn create_session_full(
    socket_path: &std::path::Path,
    name: &str,
) -> (String, String, String) {
    let resp = rpc_ok(
        socket_path,
        "session.create",
        serde_json::json!({"name": name}),
    )
    .await;
    let session_id = resp["id"].as_str().unwrap().to_string();
    let window_id = resp["window_id"].as_str().unwrap().to_string();
    let pane_id = resp["pane_id"].as_str().unwrap().to_string();
    (session_id, window_id, pane_id)
}

#[tokio::test]
async fn test_015_pane_split_vertical() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let (session_id, window_id, _pane_id) = create_session_full(&socket_path, "psv").await;

    let resp = rpc_ok(
        &socket_path,
        "pane.split",
        serde_json::json!({"session_id": session_id, "window_id": window_id, "direction": "vertical"}),
    )
    .await;

    assert!(resp.get("pane").is_some(), "should return new pane");
    assert!(resp.get("split_from").is_some(), "should return split_from");

    // List panes — should be 2
    let list = rpc_ok(
        &socket_path,
        "pane.list",
        serde_json::json!({"session_id": session_id, "window_id": window_id}),
    )
    .await;
    assert_eq!(
        list.as_array().unwrap().len(),
        2,
        "should have 2 panes after split"
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_015_pane_split_horizontal() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let (session_id, window_id, _pane_id) = create_session_full(&socket_path, "psh").await;

    let resp = rpc_ok(
        &socket_path,
        "pane.split",
        serde_json::json!({"session_id": session_id, "window_id": window_id, "direction": "horizontal"}),
    )
    .await;

    assert!(resp.get("pane").is_some());
    let new_pane = &resp["pane"];
    assert!(new_pane.get("id").is_some());

    cancel.cancel();
}

#[tokio::test]
async fn test_015_pane_split_auto_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let (session_id, window_id, _pane_id) = create_session_full(&socket_path, "psa").await;

    // No direction → defaults to vertical
    let resp = rpc_ok(
        &socket_path,
        "pane.split",
        serde_json::json!({"session_id": session_id, "window_id": window_id}),
    )
    .await;

    assert!(resp.get("pane").is_some());

    cancel.cancel();
}

#[tokio::test]
async fn test_015_pane_list() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let (session_id, window_id, pane_id) = create_session_full(&socket_path, "pl").await;

    let list = rpc_ok(
        &socket_path,
        "pane.list",
        serde_json::json!({"session_id": session_id, "window_id": window_id}),
    )
    .await;

    let panes = list.as_array().unwrap();
    assert_eq!(panes.len(), 1, "new session has 1 pane");
    assert_eq!(panes[0]["id"].as_str().unwrap(), pane_id);
    assert!(panes[0]["is_focused"].as_bool().unwrap());

    cancel.cancel();
}

#[tokio::test]
async fn test_015_pane_focus() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let (session_id, window_id, original_pane) = create_session_full(&socket_path, "pf").await;

    // Split to get a second pane
    let split = rpc_ok(
        &socket_path,
        "pane.split",
        serde_json::json!({"session_id": session_id, "window_id": window_id}),
    )
    .await;
    let new_pane_id = split["pane"]["id"].as_str().unwrap().to_string();

    // Focus the original pane (split made the new one active)
    let resp = rpc_ok(
        &socket_path,
        "pane.focus",
        serde_json::json!({"pane_id": original_pane}),
    )
    .await;

    assert_eq!(resp["pane_id"].as_str().unwrap(), original_pane);
    assert_eq!(
        resp["previous_pane_id"].as_str().unwrap(),
        new_pane_id,
        "previous should be the split pane"
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_015_pane_focus_already_focused() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let (_session_id, _window_id, pane_id) = create_session_full(&socket_path, "pfaf").await;

    // Focus already-focused pane → previous should be null (same pane)
    let resp = rpc_ok(
        &socket_path,
        "pane.focus",
        serde_json::json!({"pane_id": pane_id}),
    )
    .await;

    assert_eq!(resp["pane_id"].as_str().unwrap(), pane_id);
    // Previous is null since it was already focused
    assert!(resp["previous_pane_id"].is_null());

    cancel.cancel();
}

#[tokio::test]
async fn test_015_pane_focus_direction() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let (session_id, window_id, left_pane) = create_session_full(&socket_path, "pfd").await;

    // Split vertically to get left/right panes
    let split = rpc_ok(
        &socket_path,
        "pane.split",
        serde_json::json!({"session_id": session_id, "window_id": window_id, "direction": "vertical"}),
    )
    .await;
    let right_pane = split["pane"]["id"].as_str().unwrap().to_string();

    // Now focused on right_pane (new pane is active after split)
    // Focus left
    let resp = rpc_ok(
        &socket_path,
        "pane.focus_direction",
        serde_json::json!({"session_id": session_id, "window_id": window_id, "direction": "left"}),
    )
    .await;

    assert_eq!(resp["pane_id"].as_str().unwrap(), left_pane);
    assert_eq!(resp["previous_pane_id"].as_str().unwrap(), right_pane);

    // Focus right (back to right_pane)
    let resp = rpc_ok(
        &socket_path,
        "pane.focus_direction",
        serde_json::json!({"session_id": session_id, "window_id": window_id, "direction": "right"}),
    )
    .await;

    assert_eq!(resp["pane_id"].as_str().unwrap(), right_pane);

    cancel.cancel();
}

#[tokio::test]
async fn test_015_pane_resize() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let (session_id, window_id, _pane_id) = create_session_full(&socket_path, "pr").await;

    // Split first (need 2 panes to resize)
    rpc_ok(
        &socket_path,
        "pane.split",
        serde_json::json!({"session_id": session_id, "window_id": window_id}),
    )
    .await;

    // Resize the active pane
    let resp = rpc_ok(
        &socket_path,
        "pane.resize",
        serde_json::json!({"session_id": session_id, "window_id": window_id, "direction": "vertical", "delta": 0.1}),
    )
    .await;

    assert!(
        resp.get("pane_id").is_some(),
        "resize should return pane_id"
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_015_pane_zoom_toggle() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let (session_id, window_id, _pane_id) = create_session_full(&socket_path, "pz").await;

    // Split to get 2 panes (zoom is meaningful with multiple panes)
    rpc_ok(
        &socket_path,
        "pane.split",
        serde_json::json!({"session_id": session_id, "window_id": window_id}),
    )
    .await;

    // Zoom in
    let resp = rpc_ok(
        &socket_path,
        "pane.zoom",
        serde_json::json!({"session_id": session_id, "window_id": window_id}),
    )
    .await;

    assert!(resp["is_zoomed"].as_bool().unwrap(), "should be zoomed");

    // Zoom out (toggle)
    let resp = rpc_ok(
        &socket_path,
        "pane.zoom",
        serde_json::json!({"session_id": session_id, "window_id": window_id}),
    )
    .await;

    assert!(!resp["is_zoomed"].as_bool().unwrap(), "should be unzoomed");

    cancel.cancel();
}

#[tokio::test]
async fn test_015_pane_swap() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let (session_id, window_id, pane_a) = create_session_full(&socket_path, "ps").await;

    let split = rpc_ok(
        &socket_path,
        "pane.split",
        serde_json::json!({"session_id": session_id, "window_id": window_id}),
    )
    .await;
    let pane_b = split["pane"]["id"].as_str().unwrap().to_string();

    let resp = rpc_ok(
        &socket_path,
        "pane.swap",
        serde_json::json!({"pane_id": pane_a, "target_pane_id": pane_b}),
    )
    .await;

    assert_eq!(resp["pane_a"].as_str().unwrap(), pane_a);
    assert_eq!(resp["pane_b"].as_str().unwrap(), pane_b);

    cancel.cancel();
}

#[tokio::test]
async fn test_015_pane_swap_self_fails() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let (_session_id, _window_id, pane_id) = create_session_full(&socket_path, "pssf").await;

    let resp = rpc_raw(
        &socket_path,
        "pane.swap",
        serde_json::json!({"pane_id": pane_id, "target_pane_id": pane_id}),
    )
    .await;

    assert!(
        resp.get("error").is_some(),
        "swapping pane with itself should error"
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_015_pane_kill() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let (session_id, window_id, _pane_id) = create_session_full(&socket_path, "pk").await;

    // Split to get a second pane
    let split = rpc_ok(
        &socket_path,
        "pane.split",
        serde_json::json!({"session_id": session_id, "window_id": window_id}),
    )
    .await;
    let new_pane_id = split["pane"]["id"].as_str().unwrap().to_string();

    // Kill the new pane
    let resp = rpc_ok(
        &socket_path,
        "pane.kill",
        serde_json::json!({"pane_id": new_pane_id}),
    )
    .await;

    assert_eq!(resp["killed"].as_str().unwrap(), new_pane_id);

    // Verify only 1 pane left
    let list = rpc_ok(
        &socket_path,
        "pane.list",
        serde_json::json!({"session_id": session_id, "window_id": window_id}),
    )
    .await;
    assert_eq!(list.as_array().unwrap().len(), 1);

    cancel.cancel();
}

#[tokio::test]
async fn test_015_pane_kill_last_fails() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let (_session_id, _window_id, pane_id) = create_session_full(&socket_path, "pklf").await;

    // Try to kill the only pane
    let resp = rpc_raw(
        &socket_path,
        "pane.kill",
        serde_json::json!({"pane_id": pane_id}),
    )
    .await;

    assert!(
        resp.get("error").is_some(),
        "killing last pane should error"
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_015_pane_kill_updates_focus() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let (session_id, window_id, original_pane) = create_session_full(&socket_path, "pkuf").await;

    // Split to get a second pane (which becomes active)
    let split = rpc_ok(
        &socket_path,
        "pane.split",
        serde_json::json!({"session_id": session_id, "window_id": window_id}),
    )
    .await;
    let new_pane_id = split["pane"]["id"].as_str().unwrap().to_string();

    // Kill the new active pane
    rpc_ok(
        &socket_path,
        "pane.kill",
        serde_json::json!({"pane_id": new_pane_id}),
    )
    .await;

    // Focus should now be back on the original pane — verify via list
    let list = rpc_ok(
        &socket_path,
        "pane.list",
        serde_json::json!({"session_id": session_id, "window_id": window_id}),
    )
    .await;
    let panes = list.as_array().unwrap();
    assert_eq!(panes.len(), 1);
    assert_eq!(panes[0]["id"].as_str().unwrap(), original_pane);
    assert!(panes[0]["is_focused"].as_bool().unwrap());

    cancel.cancel();
}
