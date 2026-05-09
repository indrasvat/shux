use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::{CommandFactory, FromArgMatches};
use tokio::sync::{Mutex, Notify, mpsc};
use tracing_subscriber::EnvFilter;

mod attach;
mod cli;
mod client;
mod daemon;
mod style;

use cli::{Cli, Command, OutputFormat, PaneCommand, WindowCommand};

/// Shared state for pane I/O operations (PTY writes, VT state, command tracking).
///
/// This is the bridge between the RPC handlers and the per-pane PTY read loops.
/// Each pane gets a write channel; the read loop is a separate tokio task.
pub struct PaneIoState {
    /// Per-pane write channels: send bytes to the pane's PTY read/write task.
    pub writers: HashMap<shux_core::model::PaneId, mpsc::Sender<Vec<u8>>>,
    /// Per-pane resize channels: send PtySize to trigger TIOCSWINSZ + VT resize.
    pub resizers: HashMap<shux_core::model::PaneId, mpsc::Sender<shux_pty::handle::PtySize>>,
    /// Per-pane VirtualTerminal instances for capturing output.
    pub vts: HashMap<shux_core::model::PaneId, shux_vt::VirtualTerminal>,
    /// Command execution engine for marker-based completion detection.
    pub cmd_engine: shux_pty::CommandEngine,
    /// Notify any attach-render loops that a pane's VT has new bytes to
    /// flush. Bumped after every PTY read so the renderer can wake up
    /// promptly (instead of polling a fixed interval).
    pub render_pulse: Arc<tokio::sync::Notify>,
}

impl Default for PaneIoState {
    fn default() -> Self {
        Self::new()
    }
}

impl PaneIoState {
    pub fn new() -> Self {
        Self {
            writers: HashMap::new(),
            resizers: HashMap::new(),
            vts: HashMap::new(),
            cmd_engine: shux_pty::CommandEngine::new(),
            render_pulse: Arc::new(tokio::sync::Notify::new()),
        }
    }
}

/// Per-pane async task that owns the PtyHandle and handles both reads and writes.
///
/// Reads from the PTY and feeds VT + CommandEngine. Receives writes from the
/// channel. Receives resize requests on a separate channel and applies them
/// via TIOCSWINSZ + VT resize.
async fn run_pane_pty_task(
    pane_id: shux_core::model::PaneId,
    mut handle: shux_pty::handle::PtyHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    mut write_rx: mpsc::Receiver<Vec<u8>>,
    mut resize_rx: mpsc::Receiver<shux_pty::handle::PtySize>,
    shutdown: tokio_util::sync::CancellationToken,
) {
    let mut buf = vec![0u8; 8192];

    loop {
        tokio::select! {
            result = handle.read(&mut buf) => {
                match result {
                    Ok(0) => {
                        tracing::debug!(%pane_id, "PTY read EOF");
                        break;
                    }
                    Ok(n) => {
                        let data = &buf[..n];
                        let pulse = {
                            let mut state = io_state.lock().await;
                            if let Some(vt) = state.vts.get_mut(&pane_id) {
                                vt.process(data);
                            }
                            let output = String::from_utf8_lossy(data);
                            let _completed = state.cmd_engine.process_output(pane_id.0, &output);
                            state.render_pulse.clone()
                        };
                        // Wake any attach-render loops outside the lock.
                        // notify_one queues a permit that survives even if
                        // the renderer happens to be mid-render and not
                        // yet awaiting; notify_waiters would silently drop
                        // the wakeup in that window.
                        pulse.notify_one();
                    }
                    Err(e) => {
                        tracing::error!(%pane_id, error = %e, "PTY read error");
                        break;
                    }
                }
            }
            res = write_rx.recv() => {
                let data = match res {
                    Some(d) => d,
                    None => {
                        // Sender dropped -- the pane was destroyed.
                        // Exit so we can kill() the child shell.
                        tracing::debug!(%pane_id, "writer channel closed");
                        break;
                    }
                };
                if let Err(e) = handle.write(&data).await {
                    tracing::error!(%pane_id, error = %e, "PTY write error");
                    break;
                }
                if let Err(e) = handle.flush().await {
                    tracing::error!(%pane_id, error = %e, "PTY flush error");
                    break;
                }
            }
            res = resize_rx.recv() => {
                let size = match res {
                    Some(s) => s,
                    None => {
                        tracing::debug!(%pane_id, "resizer channel closed");
                        break;
                    }
                };
                if let Err(e) = handle.resize(size) {
                    tracing::warn!(%pane_id, error = %e, "PTY resize failed");
                }
                let pulse = {
                    let mut state = io_state.lock().await;
                    if let Some(vt) = state.vts.get_mut(&pane_id) {
                        vt.resize(size.rows as usize, size.cols as usize);
                    }
                    state.render_pulse.clone()
                };
                pulse.notify_one();
            }
            _ = shutdown.cancelled() => {
                tracing::debug!(%pane_id, "PTY task cancelled");
                break;
            }
        }
    }

    // Best-effort: ensure the child shell is sent SIGTERM. Most exit
    // paths (EOF, write error, cancel) leave the PtyHandle alive only
    // until this scope ends — tokio::process::Child does NOT reap on
    // Drop, so an explicit kill prevents zombie shells.
    let _ = handle.kill();
    let mut state = io_state.lock().await;
    state.writers.remove(&pane_id);
    state.resizers.remove(&pane_id);
    state.vts.remove(&pane_id);
    let pulse = state.render_pulse.clone();
    drop(state);
    pulse.notify_one();
}

/// Spawn a PTY process and VT instance for a pane.
///
/// When `command` is empty, spawns the user's default login+interactive
/// shell (via `PtyConfig::default_shell`). When non-empty, runs that
/// argv directly — this is what `shux new -s X -- vim foo.rs` lands on,
/// so the pane runs `vim foo.rs` instead of a shell. The pane lifetime
/// becomes the lifetime of that command (when it exits, the pane EOFs).
pub(crate) async fn spawn_pane_pty(
    pane_id: shux_core::model::PaneId,
    cwd: PathBuf,
    command: Vec<String>,
    io_state: Arc<Mutex<PaneIoState>>,
    shutdown: tokio_util::sync::CancellationToken,
) -> Result<(), shux_rpc::RpcError> {
    let config = if command.is_empty() {
        shux_pty::handle::PtyConfig::default_shell(cwd)
    } else {
        shux_pty::handle::PtyConfig::with_command(command, cwd)
    };
    let handle = shux_pty::handle::PtyHandle::spawn(&config)
        .map_err(|e| shux_rpc::RpcError::internal(&format!("PTY spawn failed: {e}")))?;

    let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>(256);
    let (resize_tx, resize_rx) = mpsc::channel::<shux_pty::handle::PtySize>(16);
    let vt = shux_vt::VirtualTerminal::new(24, 80);

    {
        let mut state = io_state.lock().await;
        state.writers.insert(pane_id, write_tx);
        state.resizers.insert(pane_id, resize_tx);
        state.vts.insert(pane_id, vt);
    }

    tokio::spawn(run_pane_pty_task(
        pane_id, handle, io_state, write_rx, resize_rx, shutdown,
    ));

    Ok(())
}

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
        // Format error: strip "RPC error" prefix if present, avoid "error: Error:" duplication
        let msg = format!("{e:#}");
        style::print_error(&msg);
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

    // Create shared pane I/O state (PTY writers, VTs, command engine)
    let io_state = Arc::new(Mutex::new(PaneIoState::new()));

    // Load user config (~/.config/shux/config.toml). Missing file is
    // valid — defaults match current hardcoded behavior. Spawn a watcher
    // task so edits to the file are picked up live.
    let config_path = shux_core::config::default_config_path();
    let config_handle = shux_core::config::ConfigHandle::load_or_default(&config_path);
    let cfg_watcher_handle = config_handle.clone();
    let cfg_watcher_path = config_path.clone();
    let cfg_watcher_cancel = cancel.clone();
    tokio::spawn(async move {
        shux_core::config::run_hot_reload(cfg_watcher_path, cfg_watcher_handle, cfg_watcher_cancel)
            .await;
    });

    // Spawn the attach UDS listener (separate socket, dedicated streaming
    // protocol). The JSON-RPC socket below stays request-response.
    let attach_path = daemon::attach_socket_path()?;
    let attach_graph = graph_handle.clone();
    let attach_io = io_state.clone();
    let attach_cancel = cancel.clone();
    let attach_config = config_handle.clone();
    tokio::spawn(async move {
        if let Err(e) = attach::run_attach_server(
            attach_path,
            attach_graph,
            attach_io,
            attach_config,
            attach_cancel,
        )
        .await
        {
            tracing::error!(error = %e, "attach server error");
        }
    });

    // Spawn timeout checker (1s interval)
    let timeout_io = io_state.clone();
    let timeout_cancel = cancel.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let mut state = timeout_io.lock().await;
                    let _timed_out = state.cmd_engine.check_timeouts();
                }
                _ = timeout_cancel.cancelled() => break,
            }
        }
    });

    // Build router: system builtins + session + window + pane + pane I/O methods
    let router = register_pane_io_methods(
        register_pane_methods(
            register_window_methods(
                register_session_methods(
                    shux_rpc::server::register_builtin_methods(shux_rpc::Router::builder()),
                    graph_handle.clone(),
                    io_state.clone(),
                    cancel.clone(),
                ),
                graph_handle.clone(),
                io_state.clone(),
                cancel.clone(),
            ),
            graph_handle.clone(),
            io_state.clone(),
            cancel.clone(),
        ),
        graph_handle,
        io_state,
        cancel.clone(),
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

    let io_split = io_state.clone();
    let io_kill = io_state;
    let cancel_split = cancel;

    builder
        .register("pane.list", move |params: Option<serde_json::Value>| {
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
        .register("pane.split", move |params: Option<serde_json::Value>| {
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

                let ratio = params
                    .get("ratio")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.5) as f32;

                let new_pane_id = gh
                    .split_pane(pane_id, direction, ratio)
                    .await
                    .map_err(graph_error_to_rpc)?;

                // Spawn PTY for the new pane
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));
                let _ = spawn_pane_pty(new_pane_id, cwd, Vec::new(), io, ct).await;

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
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'pane_id' parameter"))?;

                let pane_id: shux_core::model::PaneId = pane_id_str
                    .parse()
                    .map_err(|_| shux_rpc::RpcError::invalid_params("invalid pane_id format"))?;

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
        .register("pane.resize", move |params: Option<serde_json::Value>| {
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

                let is_zoomed = gh
                    .zoom_pane(pane_id)
                    .await
                    .map_err(graph_error_to_rpc)?;

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
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'pane_id' parameter"))?;
                let target_str = params
                    .get("target_pane_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        shux_rpc::RpcError::invalid_params("missing 'target_pane_id' parameter")
                    })?;

                let pane_a: shux_core::model::PaneId = pane_id_str
                    .parse()
                    .map_err(|_| shux_rpc::RpcError::invalid_params("invalid pane_id format"))?;
                let pane_b: shux_core::model::PaneId = target_str
                    .parse()
                    .map_err(|_| shux_rpc::RpcError::invalid_params("invalid target_pane_id format"))?;

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
            let io = io_kill.clone();
            async move {
                let params = params.unwrap_or_default();
                let pane_id_str = params
                    .get("pane_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'pane_id' parameter"))?;

                let pane_id: shux_core::model::PaneId = pane_id_str
                    .parse()
                    .map_err(|_| shux_rpc::RpcError::invalid_params("invalid pane_id format"))?;

                // Clean up PTY/VT
                {
                    let mut state = io.lock().await;
                    state.writers.remove(&pane_id);
                    state.vts.remove(&pane_id);
                }

                gh.destroy_pane(pane_id)
                    .await
                    .map_err(graph_error_to_rpc)?;

                Ok(serde_json::json!({ "killed": pane_id_str }))
            }
        })
}

/// Resolve a pane_id from params: either explicit `pane_id` or active pane of resolved window.
fn resolve_pane_id_from_params(
    gh: &shux_core::graph::GraphHandle,
    params: &serde_json::Value,
) -> Result<shux_core::model::PaneId, shux_rpc::RpcError> {
    if let Some(pane_id_str) = params.get("pane_id").and_then(|v| v.as_str()) {
        return pane_id_str
            .parse()
            .map_err(|_| shux_rpc::RpcError::invalid_params("invalid pane_id format"));
    }

    // Fall back to active pane of the resolved window
    let window_id = resolve_window_id_from_params(gh, params)?;
    let snap = gh.snapshot();
    let window = snap
        .windows
        .get(&window_id)
        .ok_or_else(|| shux_rpc::RpcError::not_found("window", &window_id.to_string()))?;
    Ok(window.active_pane)
}

/// Resolve a window_id from params: either explicit `window_id` or active window of session.
fn resolve_window_id_from_params(
    gh: &shux_core::graph::GraphHandle,
    params: &serde_json::Value,
) -> Result<shux_core::model::WindowId, shux_rpc::RpcError> {
    if let Some(wid_str) = params.get("window_id").and_then(|v| v.as_str()) {
        return wid_str
            .parse()
            .map_err(|_| shux_rpc::RpcError::invalid_params("invalid window_id format"));
    }

    // Resolve from session
    let session_id_str = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            shux_rpc::RpcError::invalid_params(
                "missing 'pane_id' or 'window_id' or 'session_id' parameter",
            )
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

/// Register window CRUD methods on the router builder.
fn register_window_methods(
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
        .register("window.create", move |params: Option<serde_json::Value>| {
            let gh = g1.clone();
            let io = io_create.clone();
            let ct = cancel_create.clone();
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
                    .create_window(session_id, title, cwd.clone())
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

                // Spawn PTY for the new pane
                let _ = spawn_pane_pty(window.active_pane, cwd, Vec::new(), io, ct).await;

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
            let io = io_ensure.clone();
            let ct = cancel_ensure.clone();
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
                    .create_window(session_id, name, cwd.clone())
                    .await
                    .map_err(graph_error_to_rpc)?;

                let snap = gh.snapshot();
                let window = snap
                    .windows
                    .get(&window_id)
                    .ok_or_else(|| shux_rpc::RpcError::internal("window not in snapshot"))?;

                // Spawn PTY for the new pane
                let _ = spawn_pane_pty(window.active_pane, cwd, Vec::new(), io, ct).await;

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
            let io = io_kill.clone();
            async move {
                let params = params.unwrap_or_default();
                let window_id_str = params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'id' parameter"))?;

                let window_id: shux_core::model::WindowId = window_id_str
                    .parse()
                    .map_err(|_| shux_rpc::RpcError::invalid_params("invalid window id format"))?;

                // Clean up PTY/VT for all panes in this window before destroying
                {
                    let snap = gh.snapshot();
                    let pane_ids: Vec<_> = snap
                        .panes
                        .values()
                        .filter(|p| p.window_id == window_id)
                        .map(|p| p.id)
                        .collect();
                    let mut state = io.lock().await;
                    for pid in pane_ids {
                        state.writers.remove(&pid);
                        state.vts.remove(&pid);
                    }
                }

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
    io_state: Arc<Mutex<PaneIoState>>,
    cancel: tokio_util::sync::CancellationToken,
) -> shux_rpc::RouterBuilder {
    let g1 = graph.clone();
    let g2 = graph.clone();
    let g3 = graph.clone();
    let g4 = graph.clone();
    let g5 = graph.clone();

    let io_create = io_state.clone();
    let io_ensure = io_state;
    let cancel_create = cancel.clone();
    let cancel_ensure = cancel;

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
                let io = io_create.clone();
                let ct = cancel_create.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let name = params
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    // Optional pane command. Accepts:
                    //   {"command": ["vim", "foo.rs"]}     — preferred (passthrough)
                    //   {"command": "top"}                 — convenience: split on whitespace
                    //   omitted / null                     — spawn the user's default shell
                    let command: Vec<String> = match params.get("command") {
                        Some(serde_json::Value::Array(arr)) => arr
                            .iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect(),
                        Some(serde_json::Value::String(s)) if !s.trim().is_empty() => {
                            s.split_whitespace().map(|s| s.to_string()).collect()
                        }
                        _ => Vec::new(),
                    };

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

                    match gh.create_session(name, cwd.clone()).await {
                        Ok(session_id) => {
                            let snap = gh.snapshot();
                            // Spawn PTY for the initial pane
                            if let Some(s) = snap.sessions.get(&session_id) {
                                if let Some(wid) = s.windows.first() {
                                    if let Some(w) = snap.windows.get(wid) {
                                        let _ = spawn_pane_pty(
                                            w.active_pane,
                                            cwd,
                                            command.clone(),
                                            io,
                                            ct,
                                        )
                                        .await;
                                    }
                                }
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
                let io = io_ensure.clone();
                let ct = cancel_ensure.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let name = params
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("default")
                        .to_string();

                    // Optional pane command (same shape as session.create).
                    let command: Vec<String> = match params.get("command") {
                        Some(serde_json::Value::Array(arr)) => arr
                            .iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect(),
                        Some(serde_json::Value::String(s)) if !s.trim().is_empty() => {
                            s.split_whitespace().map(|s| s.to_string()).collect()
                        }
                        _ => Vec::new(),
                    };

                    // Check if session already exists
                    let snap = gh.snapshot();
                    if let Some(s) = snap.find_session_by_name(&name) {
                        let mut json = session_to_json(s, &snap);
                        json["created"] = serde_json::Value::Bool(false);
                        return Ok(json);
                    }

                    // Create new session
                    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));

                    match gh.create_session(name, cwd.clone()).await {
                        Ok(session_id) => {
                            let snap = gh.snapshot();
                            // Spawn PTY for the initial pane
                            if let Some(s) = snap.sessions.get(&session_id) {
                                if let Some(wid) = s.windows.first() {
                                    if let Some(w) = snap.windows.get(wid) {
                                        let _ = spawn_pane_pty(
                                            w.active_pane,
                                            cwd,
                                            command.clone(),
                                            io,
                                            ct,
                                        )
                                        .await;
                                    }
                                }
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

/// Register pane I/O methods (send_keys, run_command, command_status, command_cancel, capture).
fn register_pane_io_methods(
    builder: shux_rpc::RouterBuilder,
    graph: shux_core::graph::GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    _cancel: tokio_util::sync::CancellationToken,
) -> shux_rpc::RouterBuilder {
    let g1 = graph.clone();
    let g2 = graph.clone();
    let g5 = graph;

    let io1 = io_state.clone();
    let io2 = io_state.clone();
    let io3 = io_state.clone();
    let io4 = io_state.clone();
    let io5 = io_state;

    builder
        .register(
            "pane.send_keys",
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
        .register(
            "pane.run_command",
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

                    let args: Vec<String> = params
                        .get("args")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();

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
                                let text = vt.capture_text(50);
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
        .register(
            "pane.command_status",
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
        .register(
            "pane.command_cancel",
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
        .register("pane.capture", move |params: Option<serde_json::Value>| {
            let gh = g5.clone();
            let io = io5.clone();
            async move {
                let params = params.unwrap_or_default();
                let pane_id = resolve_pane_id_from_params(&gh, &params)?;

                let lines = params.get("lines").and_then(|v| v.as_u64()).unwrap_or(50) as usize;

                let state = io.lock().await;
                let vt = state.vts.get(&pane_id).ok_or_else(|| {
                    shux_rpc::RpcError::not_found("pane VT", &pane_id.to_string())
                })?;

                let text = vt.capture_text(lines);
                let clean = shux_pty::strip_ansi(&text);

                Ok(serde_json::json!({
                    "pane_id": pane_id.to_string(),
                    "text": clean,
                    "lines": lines,
                }))
            }
        })
}

/// Decide which session `shux` (no args) should attach to.
///
/// Strategy: query the daemon for sessions; if any exist, pick the most
/// recently created. If none, fall through to "default" (which the
/// daemon-side attach handler will create on demand).
async fn pick_attach_target(socket_path: &std::path::Path) -> String {
    if let Ok(mut stream) = client::try_connect(socket_path).await {
        if let Ok(value) = cli::rpc_call(&mut stream, "session.list", serde_json::json!({})).await {
            if let Some(arr) = value.get("sessions").and_then(|v| v.as_array()) {
                if let Some(latest) = arr.iter().filter_map(|s| s.get("name")?.as_str()).next() {
                    return latest.to_string();
                }
            }
        }
    }
    "default".to_string()
}

fn default_session_name() -> String {
    "default".to_string()
}

/// Run the attach TUI client. Translates the daemon's `attach.sock` into
/// real keystrokes / ANSI bytes on the user's terminal. Restores the
/// terminal on every exit path via `TerminalGuard`'s Drop.
async fn run_attach(_jsonrpc_socket: &std::path::Path, session_name: String) -> anyhow::Result<()> {
    let attach_path = daemon::attach_socket_path()?;
    let cfg = shux_ui::ClientConfig {
        socket_path: attach_path.to_string_lossy().to_string(),
        session_name: session_name.clone(),
        prefix_key: crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(' '),
            crossterm::event::KeyModifiers::CONTROL,
        ),
    };
    match shux_ui::attach::run_attach(&attach_path, cfg).await {
        Ok(reason) => {
            match reason {
                shux_ui::ExitReason::Detached => {
                    println!("[detached from session '{session_name}']");
                }
                shux_ui::ExitReason::SessionEnded => {
                    println!("[session '{session_name}' ended]");
                }
                shux_ui::ExitReason::ConnectionLost => {
                    eprintln!("[connection to daemon lost]");
                }
                shux_ui::ExitReason::Error(msg) => {
                    eprintln!("[attach error: {msg}]");
                }
            }
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Dispatch CLI subcommands.
async fn dispatch(args: Cli) -> anyhow::Result<()> {
    let socket_path = args.socket_path();

    match args.command {
        // No subcommand: attach to last session, or create "default" if none.
        None => {
            let _ = client::ensure_daemon_running_at(&socket_path).await?;
            let session_name = pick_attach_target(&socket_path).await;
            run_attach(&socket_path, session_name).await
        }

        Some(Command::New {
            session,
            ensure,
            detached,
            cmd,
            argv,
        }) => {
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            let session_name = session.clone().unwrap_or_else(default_session_name);
            let _result =
                cli::handle_new(&mut stream, session, cmd, argv, ensure, args.format).await?;
            drop(stream);

            if !detached {
                run_attach(&socket_path, session_name).await
            } else {
                Ok(())
            }
        }

        Some(Command::Attach { session }) => {
            let _ = client::ensure_daemon_running_at(&socket_path).await?;
            let session_name = session.unwrap_or_else(|| "default".to_string());
            run_attach(&socket_path, session_name).await
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

        Some(Command::Pane { command }) => {
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            match command {
                PaneCommand::List { session, window } => {
                    cli::handle_pane_list(&mut stream, &session, window.as_deref(), args.format)
                        .await
                }
                PaneCommand::Split {
                    session,
                    window,
                    pane,
                    direction,
                    ratio,
                } => {
                    cli::handle_pane_split(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        direction.as_deref(),
                        ratio,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Focus {
                    session,
                    window,
                    pane,
                } => {
                    cli::handle_pane_focus(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        &pane,
                        args.format,
                    )
                    .await
                }
                PaneCommand::FocusDir {
                    session,
                    window,
                    direction,
                } => {
                    cli::handle_pane_focus_dir(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        &direction,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Resize {
                    session,
                    window,
                    pane,
                    direction,
                    delta,
                } => {
                    cli::handle_pane_resize(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        &direction,
                        delta,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Zoom {
                    session,
                    window,
                    pane,
                } => {
                    cli::handle_pane_zoom(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        args.format,
                    )
                    .await
                }
                PaneCommand::Swap {
                    session,
                    window,
                    pane,
                    target,
                } => {
                    cli::handle_pane_swap(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        &pane,
                        &target,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Kill {
                    session,
                    window,
                    pane,
                } => {
                    cli::handle_pane_kill(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        &pane,
                        args.format,
                    )
                    .await
                }
                PaneCommand::SendKeys {
                    session,
                    window,
                    pane,
                    text,
                    data,
                } => {
                    cli::handle_pane_send_keys(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        text.as_deref(),
                        data.as_deref(),
                        args.format,
                    )
                    .await
                }
                PaneCommand::Run {
                    session,
                    window,
                    pane,
                    command,
                    timeout,
                    is_async,
                } => {
                    cli::handle_pane_run(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        &command,
                        timeout,
                        is_async,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Capture {
                    session,
                    window,
                    pane,
                    lines,
                } => {
                    cli::handle_pane_capture(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        lines,
                        args.format,
                    )
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
                        OutputFormat::Text | OutputFormat::Plain => {
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
