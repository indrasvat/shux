use std::path::PathBuf;
use std::sync::Arc;

use clap::{CommandFactory, FromArgMatches};
use tokio::sync::{Notify, mpsc};
use tracing_subscriber::EnvFilter;

mod cli;
mod client;
mod daemon;
mod style;

use cli::{Cli, Command, OutputFormat, WindowCommand};

fn main() -> anyhow::Result<()> {
    let cmd = Cli::command().before_help(style::banner());
    let matches = cmd.get_matches();
    let args = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());

    // Internal daemon subcommand — called by auto-start
    if matches!(args.command, Some(Command::__daemon)) {
        return run_daemon();
    }

    // Normal CLI client mode
    run_client(args)
}

/// Daemon entry point.
///
/// 1. Daemonize (double-fork) — BEFORE tokio runtime
/// 2. Create tokio runtime
/// 3. Set up CancellationToken tree
/// 4. Start signal handlers
/// 5. Bind UDS
/// 6. Run daemon state loop
fn run_daemon() -> anyhow::Result<()> {
    // Step 1: Daemonize BEFORE tokio
    if !daemon::daemonize()? {
        // We are the parent — exit cleanly
        return Ok(());
    }

    // Step 2: Now we are the daemon process — create tokio runtime
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        // Initialize tracing (to file, since stdio is /dev/null)
        // TODO: Set up file-based tracing subscriber
        tracing_subscriber::fmt()
            .with_env_filter("shux=info")
            .with_target(false)
            .init();

        let tokens = shux_core::daemon::ShutdownTokens::new();
        let config_reload_notify = Arc::new(Notify::new());
        let (cmd_tx, cmd_rx) = mpsc::channel(64);

        // Start signal handler
        daemon::spawn_signal_handler(cmd_tx.clone(), tokens.clone()).await?;

        // Ensure runtime dir and clean up stale socket
        daemon::ensure_runtime_dir()?;
        daemon::remove_socket_file()?;

        // Set up SessionGraph + graph loop
        let sock_path = daemon::socket_path()?;
        let cancel = tokens.root.clone();
        run_rpc_server(sock_path, cancel.clone()).await?;

        // Run the daemon state loop (blocks until shutdown)
        shux_core::daemon::run_daemon_state_loop(cmd_rx, tokens.clone(), config_reload_notify)
            .await;

        // Cleanup
        daemon::remove_pid_file()?;
        daemon::remove_socket_file()?;
        tracing::info!("Daemon shut down cleanly");

        Ok::<(), anyhow::Error>(())
    })?;

    Ok(())
}

/// Client entry point — parse CLI args, ensure daemon is running, dispatch.
fn run_client(args: Cli) -> anyhow::Result<()> {
    // Set up logging
    let filter = if args.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::from_default_env()
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    let rt = tokio::runtime::Runtime::new()?;
    let result = rt.block_on(async { dispatch(args).await });
    if let Err(ref e) = result {
        style::print_error(&format!("{e:#}"));
    }
    result
}

/// Start the RPC server with a SessionGraph backing session methods.
///
/// Spawns:
/// 1. The SessionGraph graph loop (single-writer task)
/// 2. The RPC Server accept loop
///
/// Both run until `cancel` is triggered.
async fn run_rpc_server(
    socket_path: PathBuf,
    cancel: tokio_util::sync::CancellationToken,
) -> anyhow::Result<()> {
    // Create SessionGraph + graph loop
    let (graph, state) = shux_core::graph::SessionGraph::new();
    let (graph_tx, graph_rx) = mpsc::channel(256);
    let graph_handle = shux_core::graph::GraphHandle::new(graph_tx, state);

    let graph_cancel = cancel.clone();
    tokio::spawn(async move {
        shux_core::graph::run_graph_loop(graph, graph_rx, graph_cancel).await;
    });

    // Build router: system builtins + session + window methods backed by GraphHandle
    let router = register_window_methods(
        register_session_methods(
            shux_rpc::server::register_builtin_methods(shux_rpc::Router::builder()),
            graph_handle.clone(),
        ),
        graph_handle,
    )
    .build();

    let config = shux_rpc::ServerConfig {
        socket_path,
        tcp_addr: String::new(),
        auth_token: None,
    };

    let server = shux_rpc::Server::new(config, router, cancel);

    tokio::spawn(async move {
        if let Err(e) = server.run().await {
            tracing::error!(error = %e, "RPC server error");
        }
    });

    Ok(())
}

/// Map GraphError to appropriate RPC error codes.
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
        GraphError::VersionConflict { expected, actual } => {
            shux_rpc::RpcError::version_conflict("resource", "?", expected, actual)
        }
        GraphError::Shutdown => shux_rpc::RpcError::internal(&e.to_string()),
    }
}

/// Build session info JSON from a Session, including window/pane IDs.
fn session_to_json(
    s: &shux_core::model::Session,
    snap: &shux_core::graph::SessionGraphSnapshot,
) -> serde_json::Value {
    let window_count = s.windows.len();
    let active_window_id = s.active_window.to_string();

    // Find window_id and pane_id for the first window
    let first_window_id = s.windows.first().map(|w| w.to_string());
    let first_pane_id = s
        .windows
        .first()
        .and_then(|wid| snap.windows.get(wid).map(|w| w.active_pane.to_string()));

    serde_json::json!({
        "id": s.id.to_string(),
        "name": s.name,
        "windows": s.windows.iter().map(|w| w.to_string()).collect::<Vec<_>>(),
        "window_count": window_count,
        "active_window_id": active_window_id,
        "window_id": first_window_id,
        "pane_id": first_pane_id,
        "created_at": s.created_at.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
    })
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

/// Register window CRUD methods on the router builder.
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
                // Include pane_id at top level for convenience
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

                // Check if window with this name already exists
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

                // Create new window
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));
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

/// Register session CRUD methods on the router builder.
///
/// These methods use a `GraphHandle` to interact with the SessionGraph.
/// They are registered here (in the binary crate) because shux-rpc
/// intentionally does not depend on shux-core.
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
                sessions.sort_by(|a, b| a.created_at.cmp(&b.created_at));
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

                    // Auto-generate name if not provided (None).
                    // Explicit empty string (Some("")) flows through to validation.
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

                    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));

                    match gh.create_session(name, cwd).await {
                        Ok(session_id) => {
                            let snap = gh.snapshot();
                            if let Some(s) = snap.sessions.get(&session_id) {
                                Ok(session_to_json(s, &snap))
                            } else {
                                Ok(serde_json::json!({
                                    "id": session_id.to_string(),
                                }))
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

                // Accept name or id — try UUID parse first, fall back to name lookup
                let session_id = if let Some(id_str) = params.get("id").and_then(|v| v.as_str()) {
                    let parsed: shux_core::model::SessionId = id_str.parse().map_err(|_| {
                        shux_rpc::RpcError::invalid_params("invalid session ID format")
                    })?;
                    // Verify it exists
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

                    // Check if session already exists
                    let snap = gh.snapshot();
                    if let Some(s) = snap.find_session_by_name(&name) {
                        let mut json = session_to_json(s, &snap);
                        json["created"] = serde_json::Value::Bool(false);
                        return Ok(json);
                    }

                    // Create new session
                    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));

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

                    // Resolve session by name or id
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

/// Dispatch CLI subcommands.
async fn dispatch(args: Cli) -> anyhow::Result<()> {
    let socket_path = args.socket_path();

    match args.command {
        // No subcommand: attach to last session or create "default"
        None => {
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;

            // For M0, create a default session and attach (stub).
            // Full "last session" logic comes in M1.
            let _result = cli::handle_new(
                &mut stream,
                Some("default".to_string()),
                None,
                false,
                OutputFormat::Text,
            )
            .await;

            // Attach via TUI client (wired in task 012)
            println!(
                "{}",
                style::muted("[TUI attach not yet wired — see task 012]")
            );
            Ok(())
        }

        Some(Command::New {
            session,
            ensure,
            detached,
            cmd,
        }) => {
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            let _result = cli::handle_new(&mut stream, session, cmd, ensure, args.format).await?;

            if !detached {
                // Attach via TUI client (wired in task 012)
                println!(
                    "{}",
                    style::muted("[TUI attach not yet wired — see task 012]")
                );
            }

            Ok(())
        }

        Some(Command::Attach { session }) => {
            let _stream = client::ensure_daemon_running_at(&socket_path).await?;
            let session_name = session.unwrap_or_else(|| "default".to_string());

            // Attach via TUI client (wired in task 012)
            println!(
                "{}",
                style::muted(format!(
                    "[TUI attach to '{session_name}' not yet wired — see task 012]"
                ))
            );
            Ok(())
        }

        Some(Command::Ls) => {
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            cli::handle_ls(&mut stream, args.format).await
        }

        Some(Command::Kill { session }) => {
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            cli::handle_kill(&mut stream, &session, args.format).await
        }

        Some(Command::Rename { session, name }) => {
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            cli::handle_rename(&mut stream, &session, &name, args.format).await
        }

        Some(Command::Window { command }) => {
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            match command {
                WindowCommand::List { session } => {
                    cli::handle_window_list(&mut stream, &session, args.format).await
                }
                WindowCommand::New {
                    session,
                    name,
                    ensure,
                } => cli::handle_window_new(&mut stream, &session, name, ensure, args.format).await,
                WindowCommand::Kill { session, window } => {
                    cli::handle_window_kill(&mut stream, &session, &window, args.format).await
                }
                WindowCommand::Rename {
                    session,
                    window,
                    name,
                } => {
                    cli::handle_window_rename(&mut stream, &session, &window, &name, args.format)
                        .await
                }
                WindowCommand::Focus { session, window } => {
                    cli::handle_window_focus(&mut stream, &session, &window, args.format).await
                }
                WindowCommand::Reorder {
                    session,
                    window,
                    index,
                } => {
                    cli::handle_window_reorder(&mut stream, &session, &window, index, args.format)
                        .await
                }
            }
        }

        Some(Command::Api { method, params }) => {
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            cli::handle_api(&mut stream, &method, &params, args.format).await
        }

        Some(Command::Version) => {
            // Quick probe — don't auto-start daemon just for version
            match client::try_connect(&socket_path).await {
                Ok(mut stream) => cli::handle_version(&mut stream, args.format).await,
                Err(_) => {
                    match args.format {
                        OutputFormat::Json => {
                            println!(
                                "{{\"version\": \"{}\", \"git_sha\": \"{}\"}}",
                                env!("CARGO_PKG_VERSION"),
                                env!("SHUX_GIT_SHA"),
                            );
                        }
                        OutputFormat::Text => {
                            style::print_version(
                                env!("CARGO_PKG_VERSION"),
                                Some(env!("SHUX_GIT_SHA")),
                                Some("daemon not running"),
                            );
                        }
                    }
                    Ok(())
                }
            }
        }

        Some(Command::__daemon) => unreachable!("handled above"),
    }
}
