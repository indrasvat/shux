use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::{CommandFactory, FromArgMatches};
use shux_rpc::{Policy, Sensitivity};
use tokio::sync::{Mutex, Notify, mpsc};
use tracing_subscriber::EnvFilter;

mod attach;
mod cli;
mod client;
mod config_validate;
mod daemon;
mod onboarding;
mod session_meta;
mod statusbar_build;
mod statusbar_runner;
mod style;
mod template;

use cli::{Cli, Command, OutputFormat, PaneCommand, WindowCommand};

/// A pane-resize message sent through `PaneIoState::resizers`.
///
/// Carries the requested `PtySize` and an optional one-shot ack that the
/// per-pane PTY task fires after applying TIOCSWINSZ + `vt.resize()`.
/// Synchronous callers (`pane.set_size` RPC) pass `Some(tx)` and await it
/// so the RPC only returns once `vt.grid().cols/rows` actually reflect the
/// new size; fire-and-forget producers (attach-client layout fan-out)
/// pass `None`.
pub struct ResizeRequest {
    pub size: shux_pty::handle::PtySize,
    pub ack: Option<tokio::sync::oneshot::Sender<()>>,
}

/// Shared state for pane I/O operations (PTY writes, VT state, command tracking).
///
/// This is the bridge between the RPC handlers and the per-pane PTY read loops.
/// Each pane gets a write channel; the read loop is a separate tokio task.
pub struct PaneIoState {
    /// Per-pane write channels: send bytes to the pane's PTY read/write task.
    pub writers: HashMap<shux_core::model::PaneId, mpsc::Sender<Vec<u8>>>,
    /// Per-pane resize channels: send `ResizeRequest` to trigger
    /// TIOCSWINSZ + VT resize. Use `ResizeRequest { ack: None }` for
    /// fire-and-forget; `ack: Some(tx)` for synchronous RPCs that must
    /// see the new dimensions on return.
    pub resizers: HashMap<shux_core::model::PaneId, mpsc::Sender<ResizeRequest>>,
    /// Per-pane VirtualTerminal instances for capturing output.
    pub vts: HashMap<shux_core::model::PaneId, shux_vt::VirtualTerminal>,
    /// Command execution engine for marker-based completion detection.
    pub cmd_engine: shux_pty::CommandEngine,
    /// Notify any attach-render loops that a pane's VT has new bytes to
    /// flush. Bumped after every PTY read so the renderer can wake up
    /// promptly (instead of polling a fixed interval).
    pub render_pulse: Arc<tokio::sync::Notify>,
    /// PR 2c — data-plane publisher. The per-pane PTY task forwards
    /// sampled PTY chunks here via `publish_pane_output`. `None` in
    /// test harnesses that don't wire an event bus. Cheap to clone
    /// (Arc internally).
    pub event_bus: Option<shux_core::bus::EventBus>,
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
            event_bus: None,
        }
    }

    pub fn with_event_bus(mut self, bus: shux_core::bus::EventBus) -> Self {
        self.event_bus = Some(bus);
        self
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
    mut resize_rx: mpsc::Receiver<ResizeRequest>,
    shutdown: tokio_util::sync::CancellationToken,
    graph: shux_core::graph::GraphHandle,
) {
    use base64::Engine;
    let mut buf = vec![0u8; 8192];
    // Track the last OSC title we forwarded to the graph so we only
    // call set_pane_osc_title when it actually changes. bash's
    // PROMPT_COMMAND re-emits OSC 2 on every prompt; without this
    // local diff we'd flood the graph + event bus.
    let mut last_osc_title: Option<String> = None;

    // PR 2c — sampled pane.output data-plane publishing.
    //
    // Coalesce PTY chunks into a single broadcast per
    // `output_sample_interval`. Without this rate limit a noisy pane
    // (npm install, cargo build, tail -F) would saturate the data-plane
    // channel and lag every subscriber. Trade-off: subscribers see at
    // most ~10 chunks/sec/pane and `sampled=true` whenever bytes were
    // dropped between intervals.
    let output_sample_interval = std::time::Duration::from_millis(100);
    let mut output_pending: Vec<u8> = Vec::new();
    let mut output_last_published_at = std::time::Instant::now()
        .checked_sub(output_sample_interval)
        .unwrap_or_else(std::time::Instant::now);
    let mut output_dropped_any = false;

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
                        let (pulse, vt_title, bus_opt) = {
                            let mut state = io_state.lock().await;
                            let vt_title = if let Some(vt) = state.vts.get_mut(&pane_id) {
                                vt.process(data);
                                vt.title().map(|s| s.to_string())
                            } else {
                                None
                            };
                            let output = String::from_utf8_lossy(data);
                            let _completed = state.cmd_engine.process_output(pane_id.0, &output);
                            (
                                state.render_pulse.clone(),
                                vt_title,
                                state.event_bus.clone(),
                            )
                        };

                        // Stage these bytes for the next sampled publish.
                        // We cap the buffered chunk at 64KB to avoid
                        // unbounded growth if the sampling interval
                        // races with a huge burst of output — anything
                        // older gets dropped (sampled=true signals that).
                        const MAX_PENDING: usize = 64 * 1024;
                        if output_pending.len() + data.len() > MAX_PENDING {
                            let overflow =
                                output_pending.len() + data.len() - MAX_PENDING;
                            let drop = overflow.min(output_pending.len());
                            output_pending.drain(..drop);
                            output_dropped_any = true;
                        }
                        output_pending.extend_from_slice(data);
                        // Publish if the sample interval has elapsed AND
                        // there's a bus + at least one buffered byte.
                        if let Some(bus) = bus_opt {
                            let now = std::time::Instant::now();
                            if !output_pending.is_empty()
                                && now.duration_since(output_last_published_at)
                                    >= output_sample_interval
                            {
                                // Resolve (window_id, session_id) outside
                                // the io_state lock to avoid holding it
                                // across the broadcast send.
                                let snap = graph.snapshot();
                                let pane = snap.panes.get(&pane_id);
                                if let Some(p) = pane {
                                    let wid = p.window_id;
                                    let sid = snap
                                        .windows
                                        .get(&wid)
                                        .map(|w| w.session_id);
                                    if let Some(sid) = sid {
                                        let chunk = std::mem::take(&mut output_pending);
                                        let b64 =
                                            base64::engine::general_purpose::STANDARD
                                                .encode(&chunk);
                                        bus.publish_pane_output(
                                            pane_id,
                                            wid,
                                            sid,
                                            b64,
                                            output_dropped_any,
                                        );
                                        output_last_published_at = now;
                                        output_dropped_any = false;
                                    }
                                }
                            }
                        }
                        // Wake any attach-render loops outside the lock.
                        // notify_one queues a permit that survives even if
                        // the renderer happens to be mid-render and not
                        // yet awaiting; notify_waiters would silently drop
                        // the wakeup in that window.
                        pulse.notify_one();
                        // Forward OSC 0/2 title changes to the graph
                        // (outside the io_state lock). Don't hold the
                        // mutex across the mpsc send — that's the
                        // deadlock pattern from PR #7. Skip empty
                        // titles entirely; some apps clear with OSC 2
                        // and we don't want a blank border title.
                        if vt_title != last_osc_title {
                            if let Some(t) = vt_title.clone() {
                                if !t.is_empty() {
                                    if let Err(e) =
                                        graph.set_pane_osc_title(pane_id, t).await
                                    {
                                        tracing::warn!(
                                            %pane_id,
                                            error = %e,
                                            "set_pane_osc_title failed",
                                        );
                                    }
                                }
                            }
                            last_osc_title = vt_title;
                        }
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
                let req = match res {
                    Some(r) => r,
                    None => {
                        tracing::debug!(%pane_id, "resizer channel closed");
                        break;
                    }
                };
                if let Err(e) = handle.resize(req.size) {
                    tracing::warn!(%pane_id, error = %e, "PTY resize failed");
                }
                let pulse = {
                    let mut state = io_state.lock().await;
                    if let Some(vt) = state.vts.get_mut(&pane_id) {
                        vt.resize(req.size.rows as usize, req.size.cols as usize);
                    }
                    state.render_pulse.clone()
                };
                pulse.notify_one();
                // Fire the ack AFTER vt + render_pulse so a synchronous
                // caller (pane.set_size RPC) is guaranteed that the next
                // pane.snapshot it issues sees the new dimensions.
                if let Some(ack) = req.ack {
                    let _ = ack.send(());
                }
            }
            _ = shutdown.cancelled() => {
                tracing::debug!(%pane_id, "PTY task cancelled");
                break;
            }
        }
    }

    // Reap the child cleanly so plugins and `events.history` see the
    // real exit code on `pane.exited`. The loop exits for several
    // reasons (EOF, read error, channel close, shutdown cancel); only
    // the EOF / read-error paths leave a still-alive child needing a
    // proper wait, while the channel-close and shutdown paths require
    // an explicit kill before waiting will return. Bound both stages
    // with timeouts so a wedged child can't stall pane teardown.
    let exit_code =
        match tokio::time::timeout(std::time::Duration::from_secs(2), handle.wait()).await {
            Ok(Ok(status)) => status.code(),
            Ok(Err(e)) => {
                tracing::warn!(%pane_id, error = %e, "PTY child wait failed");
                None
            }
            Err(_) => {
                // Still alive after 2s — SIGTERM and try once more.
                let _ = handle.kill();
                match tokio::time::timeout(std::time::Duration::from_secs(1), handle.wait()).await {
                    Ok(Ok(status)) => status.code(),
                    _ => None,
                }
            }
        };

    // Propagate the captured exit code so the daemon's PaneExited
    // event carries it. set_pane_exit_status both updates the pane
    // and fires the lifecycle event with the populated field — the
    // alternative path (graph.destroy_pane via API) fires PaneExited
    // with None, which is the right thing for "killed by user", and
    // the cascade paths in destroy_session/destroy_window do the same.
    if let Some(code) = exit_code {
        if let Err(e) = graph.set_pane_exit_status(pane_id, code).await {
            tracing::debug!(%pane_id, error = %e, "set_pane_exit_status failed (pane may already be gone)");
        }
    }

    // Drop only the PTY-bound handles. The VT (grid + scrollback) stays
    // until the pane is explicitly destroyed via pane.kill / window.kill
    // / session.kill — agents and humans alike need pane.capture and
    // pane.snapshot to keep working against the frozen output of a
    // short-lived command. The Pane's exit_status is the "dead" flag;
    // tmux does the same with its `remain-on-exit` model.
    let mut state = io_state.lock().await;
    state.writers.remove(&pane_id);
    state.resizers.remove(&pane_id);
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
    graph: shux_core::graph::GraphHandle,
) -> Result<(), shux_rpc::RpcError> {
    let config = if command.is_empty() {
        shux_pty::handle::PtyConfig::default_shell(cwd)
    } else {
        shux_pty::handle::PtyConfig::with_command(command, cwd)
    };
    let handle = shux_pty::handle::PtyHandle::spawn(&config)
        .map_err(|e| shux_rpc::RpcError::internal(&format!("PTY spawn failed: {e}")))?;

    let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>(256);
    let (resize_tx, resize_rx) = mpsc::channel::<ResizeRequest>(16);
    let vt = shux_vt::VirtualTerminal::new(24, 80);

    {
        let mut state = io_state.lock().await;
        state.writers.insert(pane_id, write_tx);
        state.resizers.insert(pane_id, resize_tx);
        state.vts.insert(pane_id, vt);
    }

    tokio::spawn(run_pane_pty_task(
        pane_id, handle, io_state, write_rx, resize_rx, shutdown, graph,
    ));

    Ok(())
}

fn main() -> anyhow::Result<()> {
    // Inject the colorised agent reference at runtime so it honours
    // NO_COLOR + the IsTerminal piped-stdout check. clap's derive macro
    // only accepts a `&'static str` literal there, so we set it here.
    let cmd = Cli::command()
        .before_help(style::banner())
        .long_about(cli::long_about())
        .after_long_help(cli::agent_help());
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
    // EventBus: typed pub/sub for lifecycle events. Wired into SessionGraph
    // so every successful mutation publishes a typed event to subscribers.
    // events.watch / events.history RPC methods read from here.
    let event_bus = shux_core::bus::EventBus::new();

    // Create SessionGraph + graph loop. Pass the bus so mutations fire events.
    let (graph, state) =
        shux_core::graph::SessionGraph::new_with_event_bus(Some(event_bus.clone()));
    let (graph_tx, graph_rx) = mpsc::channel(256);
    let graph_handle = shux_core::graph::GraphHandle::new(graph_tx, state);

    let graph_cancel = cancel.clone();
    tokio::spawn(async move {
        shux_core::graph::run_graph_loop(graph, graph_rx, graph_cancel).await;
    });

    // Create shared pane I/O state (PTY writers, VTs, command engine,
    // data-plane publisher). The event bus is the SAME bus the
    // control-plane events.watch RPC reads — but per-pane output
    // chunks land on its data plane, separate from
    // `events.history`. See `docs/PR2c-DESIGN.md`.
    let io_state = Arc::new(Mutex::new(
        PaneIoState::new().with_event_bus(event_bus.clone()),
    ));

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

    // Status-bar segment cache + runners. One runner task per
    // `[[statusbar.segment]]` in config; restarts when config reloads.
    let segment_cache = statusbar_runner::SegmentCache::new();
    statusbar_runner::spawn_segment_runners(
        config_handle.clone(),
        segment_cache.clone(),
        cancel.clone(),
    );

    // Per-session decorations (git branch, SSH context). Non-persisted,
    // populated on session.create / .ensure, cleared on session.kill.
    // The OOTB status bar reads this on every render — must stay cheap.
    let session_meta_cache = session_meta::SessionMetaCache::new();

    // First-run onboarding state (prefix-discovered, welcome-toast-seen).
    // Single state file under XDG_STATE_HOME loaded once at daemon start.
    let onboarding = onboarding::OnboardingHandle::load();

    // Daemon start instant — drives the "up Nh Nm" segment in the right
    // zone post-hint-dismissal.
    let daemon_start = std::time::Instant::now();

    // Spawn the attach UDS listener (separate socket, dedicated streaming
    // protocol). The JSON-RPC socket below stays request-response.
    let attach_path = daemon::attach_socket_path()?;
    let attach_graph = graph_handle.clone();
    let attach_io = io_state.clone();
    let attach_cancel = cancel.clone();
    let attach_config = config_handle.clone();
    let attach_segments = segment_cache.clone();
    let attach_meta = session_meta_cache.clone();
    let attach_onboarding = onboarding.clone();
    tokio::spawn(async move {
        if let Err(e) = attach::run_attach_server(
            attach_path,
            attach_graph,
            attach_io,
            attach_config,
            attach_segments,
            attach_meta,
            attach_onboarding,
            daemon_start,
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

    // Plugin host (task 044a phase 0). One PluginManager shared by
    // the plugin RPC handlers and every spawned plugin's I/O task.
    // We set the router on it AFTER `.build()` below, breaking the
    // circular dependency (manager holds Arc<OnceCell<Router>>).
    let plugins = shux_plugin::PluginManager::new(event_bus.clone());

    // Build router: system builtins + session + window + pane + pane I/O + events + state + plugin methods
    let router = register_plugin_methods(
        register_state_methods(
            register_events_methods(
                register_pane_io_methods(
                    register_pane_methods(
                        register_window_methods(
                            register_session_methods(
                                shux_rpc::server::register_builtin_methods(
                                    shux_rpc::Router::builder(),
                                ),
                                graph_handle.clone(),
                                io_state.clone(),
                                cancel.clone(),
                                session_meta_cache.clone(),
                            ),
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
                    config_handle.clone(),
                    session_meta_cache.clone(),
                    onboarding.clone(),
                    segment_cache.clone(),
                ),
                event_bus,
            ),
            graph_handle.clone(),
            io_state,
            cancel.clone(),
        ),
        plugins.clone(),
    )
    .build();

    // Startup assertion: every registered RPC method must declare a
    // sensitivity policy. Catches "added a new method, forgot to
    // classify it" at boot. See
    // `docs/designs/permissions/README.md` §9.6.
    router.assert_every_route_has_policy();

    // Plugin → daemon RPC calls dispatch through this router clone.
    // Setting it now (post-build) is what lets plugins call any
    // method registered above. Also wire in the graph handle so the
    // permission enforcer can look up entity ownership.
    plugins.set_router(router.clone());
    plugins.set_graph(graph_handle.clone()).await;

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
        GraphError::VersionConflict {
            resource,
            ref id,
            expected,
            actual,
        } => shux_rpc::RpcError::version_conflict(resource, id, expected, actual),
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
        "manual_title": p.manual_title,
        "osc_title": p.osc_title,
        "auto_title": p.auto_title,
        "cwd": p.cwd.to_string_lossy(),
        "command": p.command,
        "exit_status": p.exit_status,
        "is_focused": is_focused,
        "is_zoomed": is_zoomed,
        "version": p.version,
    })
}

/// Serialize an `Event` for JSON-RPC transport. Includes the typed payload
/// AND meta (seq, timestamp, type) at the top level so consumers can route
/// without recursing into a nested envelope.
fn event_to_json(event: &shux_core::event::Event) -> serde_json::Value {
    event.to_wire_json()
}

/// Register `events.watch` and `events.history` RPC methods.
///
/// `events.watch` is the agent-facing subscription: long-poll style, since
/// the JSON-RPC Handler trait is single-response. The handler:
///   1. Subscribes to the bus FIRST (so concurrent publishes can't slip
///      between history snapshot and subscription start — the race that
///      Codex and Gemini both flagged as the load-bearing correctness
///      requirement).
///   2. Snapshots history from `from_seq` SECOND.
///   3. Drains the subscription with `timeout_ms` until either a matching
///      event arrives or the deadline lapses.
///   4. Returns history + tail, deduped by `seq` (the overlap between the
///      two streams is real — an event published in step 2 might appear in
///      both the history snapshot and the subscription receiver buffer).
///
/// Register `state.apply` RPC method.
///
/// Takes a generic `Op` delta (NOT a TOML template — codex P0 #2: keeping
/// the daemon API agnostic to template grammar means future SDKs / MCP
/// servers / agents can target the same primitive).
///
/// Atomicity: graph-level all-or-nothing. PTY spawns happen AFTER the
/// graph commits and per-pane spawn outcomes are reported in
/// `BatchResult::spawn_results`. Spawn failure does NOT roll back the
/// graph (codex P0 #1: rolling back PTY-spawned commands would mean
/// killing already-launched subprocesses, which has its own side effects;
/// honest reporting beats dishonest atomicity).
fn register_state_methods(
    builder: shux_rpc::RouterBuilder,
    graph: shux_core::graph::GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    cancel: tokio_util::sync::CancellationToken,
) -> shux_rpc::RouterBuilder {
    builder.register_with_policy(
        "state.apply",
        Policy::fixed(Sensitivity::Grantable),
        move |params: Option<serde_json::Value>| {
            let gh = graph.clone();
            let io = io_state.clone();
            let ct = cancel.clone();
            async move {
                // Parse `{ ops: [...] }`.
                let params = params.unwrap_or_default();
                let ops_value = params
                    .get("ops")
                    .cloned()
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'ops' array"))?;
                let ops: Vec<shux_core::apply::Op> =
                    serde_json::from_value(ops_value).map_err(|e| {
                        shux_rpc::RpcError::invalid_params(&format!("ops parse error: {e}"))
                    })?;

                // Run the staged transaction through the single-writer task.
                let mut result = gh.apply_batch(ops).await.map_err(batch_error_to_rpc)?;

                // Graph commit succeeded. Now spawn PTYs for each new pane.
                // Per codex P0 #1: spawn outcomes are reported per-pane in
                // `spawn_results` and do NOT roll back the graph.
                let snap = gh.snapshot();
                let mut spawn_results = Vec::new();
                for output in &result.outputs {
                    if let Some(pane_id) = output.pane_id {
                        if let Some(pane) = snap.panes.get(&pane_id) {
                            let cwd = pane.cwd.clone();
                            let command = pane.command.clone();
                            let spawn_io = io.clone();
                            let spawn_ct = ct.clone();
                            match spawn_pane_pty(
                                pane_id,
                                cwd,
                                command,
                                spawn_io,
                                spawn_ct,
                                gh.clone(),
                            )
                            .await
                            {
                                Ok(()) => spawn_results.push(shux_core::apply::SpawnResult {
                                    op_index: output.op_index,
                                    pane_id,
                                    spawned: true,
                                    error: None,
                                }),
                                Err(e) => spawn_results.push(shux_core::apply::SpawnResult {
                                    op_index: output.op_index,
                                    pane_id,
                                    spawned: false,
                                    error: Some(e.to_string()),
                                }),
                            }
                        }
                    }
                }
                result.spawn_results = spawn_results;

                serde_json::to_value(&result).map_err(|e| {
                    shux_rpc::RpcError::internal(&format!("apply result serialize error: {e}"))
                })
            }
        },
    )
}

/// Map BatchError to an appropriate RPC error.
fn batch_error_to_rpc(e: shux_core::apply::BatchError) -> shux_rpc::RpcError {
    use shux_core::apply::BatchError;
    match e {
        BatchError::Empty => shux_rpc::RpcError::invalid_params("ops array is empty"),
        BatchError::BackRefOutOfRange { .. } | BatchError::BackRefWrongType { .. } => {
            shux_rpc::RpcError::invalid_params(&e.to_string())
        }
        BatchError::OpFailed { source, .. } => graph_error_to_rpc(source),
    }
}

/// Map PluginError → RpcError so plugin RPC handlers report
/// human-readable failures (NotFound + NameConflict reuse the
/// canonical PRD §8.3 error envelopes, everything else is
/// internal).
fn plugin_error_to_rpc(e: shux_plugin::PluginError) -> shux_rpc::RpcError {
    use shux_plugin::PluginError;
    match e {
        PluginError::NotFound(ref name) => shux_rpc::RpcError::not_found("plugin", name),
        PluginError::NameConflict(ref name) => shux_rpc::RpcError::name_conflict("plugin", name),
        PluginError::HandshakeFailed(_) | PluginError::Proto(_) => {
            shux_rpc::RpcError::invalid_params(&e.to_string())
        }
        PluginError::Io(_) => shux_rpc::RpcError::internal(&e.to_string()),
    }
}

/// Plugin RPC surface (task 044a, phase 0).
///
/// - `plugin.install` — spawn a plugin from a `path` (+ optional
///   `args`, `cwd`). Performs the handshake synchronously and
///   returns the resolved `PluginInfo`.
/// - `plugin.list` — snapshot of every running plugin.
/// - `plugin.kill` — graceful shutdown + child cleanup.
fn register_plugin_methods(
    builder: shux_rpc::RouterBuilder,
    plugins: shux_plugin::PluginManager,
) -> shux_rpc::RouterBuilder {
    let p1 = plugins.clone();
    let p2 = plugins.clone();
    let p3 = plugins.clone();
    let p4 = plugins.clone();
    let p5 = plugins.clone();
    let p6 = plugins.clone();
    let p7 = plugins.clone();
    let p8 = plugins;

    builder
        .register_with_policy(
            "plugin.install",
            Policy::fixed(Sensitivity::PluginsForbidden),
            move |params: Option<serde_json::Value>| {
                let mgr = p1.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let path = params
                        .get("path")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'path'"))?;
                    let args: Vec<String> = params
                        .get("args")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();
                    let cwd = params
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .map(PathBuf::from);
                    // Watch defaults to ON — the dogfood loop showed
                    // every iteration without hot reload felt long.
                    // Callers opt out with `"watch": false`.
                    let watch = params
                        .get("watch")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    // Per-install state root (codex P2 review on PR #32):
                    // a daemon shared across project checkouts must
                    // pin each plugin's state to the calling client's
                    // project, not to the daemon's own cwd. The CLI
                    // passes the resolved `.shux/plugins` path here.
                    let state_root = params
                        .get("state_root")
                        .and_then(|v| v.as_str())
                        .map(PathBuf::from);

                    let source = shux_plugin::PluginSource {
                        path: PathBuf::from(path),
                        args,
                        cwd,
                        watch,
                        state_root,
                    };
                    let info = mgr.install(source).await.map_err(plugin_error_to_rpc)?;
                    serde_json::to_value(&info).map_err(|e| {
                        shux_rpc::RpcError::internal(&format!("plugin info serialize: {e}"))
                    })
                }
            },
        )
        .register_with_policy("plugin.list", Policy::fixed(Sensitivity::Public), move |_params: Option<serde_json::Value>| {
            let mgr = p2.clone();
            async move {
                let infos = mgr.list().await;
                Ok(serde_json::json!({ "plugins": infos }))
            }
        })
        .register_with_policy("plugin.kill", Policy::fixed(Sensitivity::PluginsForbidden), move |params: Option<serde_json::Value>| {
            let mgr = p3.clone();
            async move {
                let params = params.unwrap_or_default();
                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'name'"))?
                    .to_string();
                mgr.kill(&name).await.map_err(plugin_error_to_rpc)?;
                Ok(serde_json::json!({ "killed": name }))
            }
        })
        .register_with_policy("plugin.reload", Policy::fixed(Sensitivity::PluginsForbidden), move |params: Option<serde_json::Value>| {
            let mgr = p4.clone();
            async move {
                let params = params.unwrap_or_default();
                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'name'"))?
                    .to_string();
                let info = mgr.reload(&name).await.map_err(plugin_error_to_rpc)?;
                serde_json::to_value(&info).map_err(|e| {
                    shux_rpc::RpcError::internal(&format!("plugin info serialize: {e}"))
                })
            }
        })
        .register_with_policy("plugin.grant", Policy::fixed(Sensitivity::PluginsForbidden), move |params: Option<serde_json::Value>| {
            let mgr = p5.clone();
            async move {
                let p = params.unwrap_or_default();
                let plugin = p.get("plugin").and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'plugin'"))?.to_string();
                let method = p.get("method").and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'method'"))?.to_string();
                let target = p.get("target").and_then(|v| v.as_str()).map(String::from);
                let subscribe = p.get("subscribe").and_then(|v| v.as_bool()).unwrap_or(false);
                mgr.grant(&plugin, &method, target.as_deref(), subscribe).await.map_err(plugin_error_to_rpc)?;
                Ok(serde_json::json!({"granted": true, "plugin": plugin, "method": method, "target": target, "subscribe": subscribe}))
            }
        })
        .register_with_policy("plugin.revoke", Policy::fixed(Sensitivity::PluginsForbidden), move |params: Option<serde_json::Value>| {
            let mgr = p6.clone();
            async move {
                let p = params.unwrap_or_default();
                let plugin = p.get("plugin").and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'plugin'"))?.to_string();
                let method = p.get("method").and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'method'"))?.to_string();
                let target = p.get("target").and_then(|v| v.as_str()).map(String::from);
                let subscribe = p.get("subscribe").and_then(|v| v.as_bool()).unwrap_or(false);
                mgr.revoke(&plugin, &method, target.as_deref(), subscribe).await.map_err(plugin_error_to_rpc)?;
                Ok(serde_json::json!({"revoked": true, "plugin": plugin, "method": method, "target": target, "subscribe": subscribe}))
            }
        })
        .register_with_policy("plugin.grants", Policy::fixed(Sensitivity::PluginsForbidden), move |params: Option<serde_json::Value>| {
            let mgr = p7.clone();
            async move {
                let p = params.unwrap_or_default();
                let plugin = p.get("plugin").and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'plugin'"))?.to_string();
                let grants = mgr.grants_for(&plugin).await.map_err(plugin_error_to_rpc)?;
                serde_json::to_value(&grants).map_err(|e| shux_rpc::RpcError::internal(&format!("grants serialize: {e}")))
            }
        })
        .register_with_policy("plugin.audit", Policy::fixed(Sensitivity::PluginsForbidden), move |params: Option<serde_json::Value>| {
            let mgr = p8.clone();
            async move {
                let p = params.unwrap_or_default();
                let plugin = p.get("plugin").and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'plugin'"))?.to_string();
                let tail = p.get("tail").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
                let path = mgr.audit_path(&plugin).await.map_err(plugin_error_to_rpc)?;
                let body = match std::fs::read_to_string(&path) {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                    Err(e) => return Err(shux_rpc::RpcError::internal(&format!("read audit log {}: {e}", path.display()))),
                };
                let mut entries: Vec<serde_json::Value> = body
                    .lines()
                    .filter(|l| !l.is_empty())
                    .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                    .collect();
                if tail > 0 && entries.len() > tail {
                    entries = entries.split_off(entries.len() - tail);
                }
                Ok(serde_json::json!({
                    "plugin": plugin,
                    "path": path.display().to_string(),
                    "entries": entries,
                }))
            }
        })
}

/// `events.history` is a simple bus.history_filtered() wrapper.
fn register_events_methods(
    builder: shux_rpc::RouterBuilder,
    bus: shux_core::bus::EventBus,
) -> shux_rpc::RouterBuilder {
    let bus_watch = bus.clone();
    let bus_hist = bus.clone();
    let bus_pane_output = bus;

    builder
        .register_with_policy(
            "events.watch",
            Policy::param_aware(|params, plugin_id| {
                // Self-namespaced filters are Public — a plugin can
                // always watch its own published events. Anything broader
                // (firehose or other plugins' namespaces) is ContentRead
                // and needs an explicit grant.
                let prefix = format!("plugin.{plugin_id}.");
                let filters = params
                    .and_then(|p| p.get("filters"))
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|f| f.as_str())
                            .map(|s| s.to_string())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if !filters.is_empty() && filters.iter().all(|f| f.starts_with(&prefix)) {
                    Sensitivity::Public
                } else {
                    Sensitivity::ContentRead
                }
            }),
            move |params: Option<serde_json::Value>| {
                let bus = bus_watch.clone();
                async move {
                    let params = params.unwrap_or_default();

                    let from_seq = params.get("from_seq").and_then(|v| v.as_u64());
                    let max_events = params
                        .get("max_events")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as usize)
                        .unwrap_or(100)
                        .min(1000);
                    let timeout_ms = params
                        .get("timeout_ms")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(5_000)
                        .min(30_000);
                    let filters: Vec<String> = params
                        .get("filter")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();

                    // 1. Subscribe FIRST so any publish during step 2 lands in the
                    //    receiver buffer, not the void.
                    let mut sub = bus.subscribe_filtered(filters.clone());

                    // 2. Snapshot history from from_seq.
                    let (history, gap) = match from_seq {
                        Some(s) => {
                            let (events, gap) = bus.events_from_seq(s);
                            let filtered: Vec<_> = if filters.is_empty() {
                                events
                            } else {
                                events
                                    .into_iter()
                                    .filter(|e| filters.iter().any(|f| e.matches_filter(f)))
                                    .collect()
                            };
                            (filtered, gap)
                        }
                        None => (Vec::new(), 0),
                    };

                    let mut collected: Vec<shux_core::event::Event> = history;
                    let mut lagged = false;

                    // 3. Tail: drain up to (max_events - history_len) events from
                    //    the subscription with timeout. If from_seq was None and
                    //    we have no history, block until at least one event or
                    //    timeout. If we already have history, just opportunistically
                    //    grab anything queued without blocking past the deadline.
                    let deadline =
                        tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
                    while collected.len() < max_events {
                        let now = tokio::time::Instant::now();
                        if now >= deadline {
                            break;
                        }
                        let remaining = deadline - now;
                        match tokio::time::timeout(remaining, sub.recv()).await {
                            Ok(Some(shux_core::bus::SubscriptionEvent::Event(e))) => {
                                collected.push(e)
                            }
                            Ok(Some(shux_core::bus::SubscriptionEvent::Lagged(_))) => {
                                // Subscriber fell behind broadcast capacity. Surface
                                // to the client so it knows the stream is degraded.
                                lagged = true;
                                break;
                            }
                            Ok(None) => break, // bus shut down
                            Err(_) => break,   // deadline reached
                        }
                    }

                    // 4. Dedup by seq. History + subscription tail can legitimately
                    //    overlap; the subscription started before history was
                    //    snapshotted, so any event published in between can land in
                    //    both streams.
                    collected.sort_by_key(|e| e.meta.seq);
                    collected.dedup_by_key(|e| e.meta.seq);
                    if collected.len() > max_events {
                        collected.truncate(max_events);
                    }

                    let next_seq = collected
                        .last()
                        .map(|e| e.meta.seq + 1)
                        .or(from_seq)
                        .unwrap_or_else(|| bus.current_seq());

                    let events: Vec<serde_json::Value> =
                        collected.iter().map(event_to_json).collect();

                    Ok(serde_json::json!({
                        "events": events,
                        "next_seq": next_seq,
                        "gap": gap,
                        "lagged": lagged,
                    }))
                }
            },
        )
        .register_with_policy(
            "events.history",
            Policy::fixed(Sensitivity::ContentRead),
            move |params: Option<serde_json::Value>| {
                let bus = bus_hist.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let count = params
                        .get("count")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as usize)
                        .unwrap_or(50)
                        .min(1000);
                    let filters: Vec<String> = params
                        .get("filter")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();

                    let events = bus.history_filtered(count, &filters);
                    let json: Vec<serde_json::Value> = events.iter().map(event_to_json).collect();

                    Ok(serde_json::json!({
                        "events": json,
                        "current_seq": bus.current_seq(),
                    }))
                }
            },
        )
        .register_with_policy(
            "pane.output.watch",
            Policy::fixed(Sensitivity::ContentRead),
            // PR 2c — sampled pane.output data-plane watch.
            //
            // Long-polls the data-plane broadcast channel for chunks
            // matching the given `pane_id`. Unlike `events.watch`,
            // there is no history snapshot — the data plane is
            // intentionally lossy to prevent secret leak via stored
            // PTY bytes and to give control-plane subscribers
            // priority. See `docs/PR2c-DESIGN.md`.
            move |params: Option<serde_json::Value>| {
                let bus = bus_pane_output.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id_str =
                        params
                            .get("pane_id")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| {
                                shux_rpc::RpcError::invalid_params("missing 'pane_id' parameter")
                            })?;
                    let pane_id: shux_core::model::PaneId = pane_id_str.parse().map_err(|_| {
                        shux_rpc::RpcError::invalid_params("invalid pane_id format")
                    })?;
                    let from_seq = params.get("from_seq").and_then(|v| v.as_u64());
                    let timeout_ms = params
                        .get("timeout_ms")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(5_000)
                        .clamp(100, 30_000);
                    let limit = params
                        .get("limit")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as usize)
                        .unwrap_or(50)
                        .min(500);

                    // Subscribe BEFORE returning any chunks so a chunk
                    // published while we're parsing params doesn't get
                    // missed. The data plane has no history, so the
                    // subscribe-first invariant from events.watch
                    // applies even more strictly here.
                    let mut sub = bus.subscribe_pane_output();

                    let mut collected: Vec<shux_core::bus::PaneOutputEvent> = Vec::new();
                    let mut lagged = false;
                    let deadline =
                        tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);

                    while collected.len() < limit {
                        let now = tokio::time::Instant::now();
                        if now >= deadline {
                            break;
                        }
                        let remaining = deadline - now;
                        match tokio::time::timeout(remaining, sub.recv()).await {
                            Ok(Some(shux_core::bus::PaneOutputSubscriptionEvent::Chunk(c))) => {
                                if c.pane_id != pane_id {
                                    continue; // not for this subscriber
                                }
                                if let Some(s) = from_seq {
                                    if c.seq < s {
                                        continue;
                                    }
                                }
                                collected.push(c);
                            }
                            Ok(Some(shux_core::bus::PaneOutputSubscriptionEvent::Lagged(_))) => {
                                lagged = true;
                                break;
                            }
                            Ok(None) => break,
                            Err(_) => break,
                        }
                    }

                    let next_seq = collected
                        .last()
                        .map(|c| c.seq + 1)
                        .or(from_seq)
                        .unwrap_or_else(|| bus.current_data_seq());

                    let chunks: Vec<serde_json::Value> = collected
                        .into_iter()
                        .map(|c| {
                            serde_json::json!({
                                "seq": c.seq,
                                "pane_id": c.pane_id.to_string(),
                                "window_id": c.window_id.to_string(),
                                "session_id": c.session_id.to_string(),
                                "timestamp": c
                                    .timestamp
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_millis() as u64)
                                    .unwrap_or(0),
                                "bytes": c.bytes,
                                "sampled": c.sampled,
                            })
                        })
                        .collect();

                    Ok(serde_json::json!({
                        "chunks": chunks,
                        "next_seq": next_seq,
                        "lagged": lagged,
                    }))
                }
            },
        )
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

                let ratio = params
                    .get("ratio")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.5) as f32;

                let new_pane_id = gh
                    .split_pane(pane_id, direction, ratio)
                    .await
                    .map_err(graph_error_to_rpc)?;

                let command: Vec<String> = params
                    .get("command")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|s| s.as_str().map(|x| x.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let cwd = params
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"))
                    });
                let _ = spawn_pane_pty(new_pane_id, cwd, command, io, ct, gh.clone()).await;

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
                let pane_id_str = params
                    .get("pane_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'pane_id' parameter"))?;

                let pane_id: shux_core::model::PaneId = pane_id_str
                    .parse()
                    .map_err(|_| shux_rpc::RpcError::invalid_params("invalid pane_id format"))?;

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
                    state.writers.remove(&pane_id);
                    state.resizers.remove(&pane_id);
                    state.vts.remove(&pane_id);
                    let pulse = state.render_pulse.clone();
                    drop(state);
                    pulse.notify_one();
                }

                Ok(serde_json::json!({ "killed": pane_id_str }))
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

/// Extract optional `expected_version` from RPC params (PR 3b — optimistic
/// concurrency). Returns `Ok(None)` when the field is absent or null,
/// `Ok(Some(v))` when it's a valid non-negative integer, and an
/// `invalid_params` RpcError if it's the wrong type or out of range. The
/// daemon then plumbs the Option through to SessionGraph mutations, which
/// reject the request with `version_conflict` (-32002) if the entity has
/// moved since the client last read it.
fn parse_expected_version(params: &serde_json::Value) -> Result<Option<u64>, shux_rpc::RpcError> {
    match params.get("expected_version") {
        None => Ok(None),
        Some(v) if v.is_null() => Ok(None),
        Some(v) => v.as_u64().map(Some).ok_or_else(|| {
            shux_rpc::RpcError::invalid_params("'expected_version' must be a non-negative integer")
        }),
    }
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

/// Build the OOTB status bar for a snapshot frame, using the same
/// `statusbar_build::build` renderer the live attach path uses so the
/// PNG matches what a fresh attached client would see.
///
/// The snapshot path doesn't have a live attach context, so we
/// synthesize a `StatusBarCtx` with defaults that mirror "fresh OOTB
/// experience": onboarding state read from the daemon-loaded handle,
/// session_meta read from the cache, no live `last_action`, no
/// copy-mode flag, no daemon-uptime (snapshots are stateless).
///
/// `segments` carries the latest script-driven `[[statusbar.segment]]`
/// outputs; `populate_bar` appends them into the same StatusBar the
/// attach loop assembles, so PNG snapshots match what an attached
/// client renders.
#[allow(clippy::too_many_arguments)]
async fn build_snapshot_status_bar(
    snap: &shux_core::graph::SessionGraphSnapshot,
    session_id: &shux_core::model::SessionId,
    window_id: shux_core::model::WindowId,
    cols: u16,
    config: &shux_core::config::ConfigHandle,
    meta_cache: &session_meta::SessionMetaCache,
    onboarding: &onboarding::OnboardingHandle,
    segments: &statusbar_runner::SegmentCache,
) -> shux_ui::StatusBar {
    let theme = {
        let cfg = config.current();
        shux_core::theme::Theme::resolve(&cfg.theme)
    };
    let live_cfg = config.current();
    let nerd_fonts = live_cfg.appearance.nerd_fonts;
    let prefix_label = statusbar_build::prefix_display(&live_cfg.keys.prefix);
    let session_meta = meta_cache.get(*session_id).await;
    let onboarding_state = onboarding.current().await;

    // The active pane id is what the live attach path would show as
    // the focus. For the snapshot we read from the graph.
    let active_pane_id = snap
        .windows
        .get(&window_id)
        .map(|w| w.active_pane)
        .unwrap_or_default();

    let session_name = snap
        .sessions
        .get(session_id)
        .map(|s| s.name.clone())
        .unwrap_or_default();

    let ctx = statusbar_build::StatusBarCtx {
        session_id: *session_id,
        session_name: &session_name,
        active_window_id: window_id,
        active_pane_id,
        session_meta: &session_meta,
        onboarding: &onboarding_state,
        daemon_uptime: std::time::Duration::from_secs(0),
        nerd_fonts,
        prefix_label: &prefix_label,
        client_cols: cols,
        copy_mode_active: false,
        last_action: None,
    };
    let mut bar = statusbar_build::build(snap, &theme, &ctx);
    // Append script-driven `[[statusbar.segment]]` outputs the same
    // way the attach render loop does. Without this, PNG snapshots
    // would only show the built-in OOTB segments and silently drop
    // every user-configured segment.
    statusbar_runner::populate_bar(&mut bar, config, segments).await;
    bar
}

/// Tail-clip captured text for inclusion in wait_for response previews.
/// Keeps the LAST `n` chars (matches are usually near the bottom of the
/// captured viewport) and trims leading whitespace.
fn preview_for_log(s: &str, n: usize) -> String {
    let bytes = s.as_bytes();
    if bytes.len() <= n {
        return s.trim_start().to_string();
    }
    let start = bytes.len() - n;
    let mut s = std::str::from_utf8(&bytes[start..])
        .unwrap_or("")
        .to_string();
    if let Some(idx) = s.find('\n') {
        s = s.split_off(idx + 1);
    }
    s.trim_start().to_string()
}

/// Parse optional `cols` / `rows` from snapshot params. Defaults: 120x36.
/// Same range guard as `pane.set_size`.
fn parse_snapshot_dims(params: &serde_json::Value) -> Result<(u16, u16), shux_rpc::RpcError> {
    let cols = params.get("cols").and_then(|v| v.as_u64()).unwrap_or(120);
    let rows = params.get("rows").and_then(|v| v.as_u64()).unwrap_or(36);
    if !(4..=1000).contains(&cols) || !(2..=1000).contains(&rows) {
        return Err(shux_rpc::RpcError::invalid_params(&format!(
            "rows/cols out of range (got rows={rows} cols={cols}; \
             valid: 4..=1000 cols, 2..=1000 rows)"
        )));
    }
    Ok((cols as u16, rows as u16))
}

/// Compose every pane in `window_id` into a single ComposedFrame at
/// `cols × rows`, rasterize it, and return the JSON `pane.snapshot`-shaped
/// response (with `window_id` in place of `pane_id`).
#[allow(clippy::too_many_arguments)]
async fn snapshot_window(
    gh: &shux_core::graph::GraphHandle,
    io: &Arc<Mutex<PaneIoState>>,
    window_id: shux_core::model::WindowId,
    cols: u16,
    rows: u16,
    rasterizer: Arc<shux_raster::Rasterizer>,
    config: &shux_core::config::ConfigHandle,
    meta_cache: &session_meta::SessionMetaCache,
    onboarding: &onboarding::OnboardingHandle,
    segments: &statusbar_runner::SegmentCache,
) -> Result<serde_json::Value, shux_rpc::RpcError> {
    let (cw, ch) = rasterizer.cell_size();
    let pixel_count = (cols as u64)
        .saturating_mul(cw as u64)
        .saturating_mul(rows as u64)
        .saturating_mul(ch as u64);
    const MAX_PIXELS: u64 = 16_000_000;
    if pixel_count > MAX_PIXELS {
        return Err(shux_rpc::RpcError::invalid_params(&format!(
            "snapshot would be {pixel_count} pixels — exceeds cap of {MAX_PIXELS}"
        )));
    }

    let snap = gh.snapshot();
    let window = snap
        .windows
        .get(&window_id)
        .ok_or_else(|| shux_rpc::RpcError::not_found("window", &window_id.to_string()))?;

    // Build per-pane title map from the graph (priority-resolved values).
    let mut titles: std::collections::HashMap<shux_core::model::PaneId, String> =
        std::collections::HashMap::new();
    for pid in window.layout.tree.pane_ids() {
        if let Some(p) = snap.panes.get(&pid) {
            if !p.title.is_empty() {
                titles.insert(pid, p.title.clone());
            }
        }
    }

    // Snapshot just the (Grid, Cursor) per pane under the io lock — VT itself
    // isn't Clone and we want to release the lock before rasterizing.
    let pane_data: Vec<(shux_core::model::PaneId, shux_vt::Grid, shux_vt::Cursor)> = {
        let state = io.lock().await;
        window
            .layout
            .tree
            .pane_ids()
            .into_iter()
            .filter_map(|pid| {
                state
                    .vts
                    .get(&pid)
                    .map(|vt| (pid, vt.grid().clone(), vt.cursor().clone()))
            })
            .collect()
    };

    let focused = window.active_pane;
    let layout_tree = window.layout.tree.clone();
    let zoom_state = window.layout.zoom.clone();

    // Build the same status bar `shux attach` would render so the snapshot
    // matches what a user sees attached. We don't have the live attached
    // state here, so we synthesize the StatusBarCtx with snapshot-time
    // defaults — every signal that does have a daemon-side source
    // (git branch, onboarding hint, theme, nerd-fonts toggle) IS still
    // populated, so PNGs honestly reflect the OOTB experience.
    let status_bar = build_snapshot_status_bar(
        &snap,
        &window.session_id,
        window_id,
        cols,
        config,
        meta_cache,
        onboarding,
        segments,
    )
    .await;
    const STATUS_BAR_ROWS: u16 = 1;

    let (img, png_buf) = tokio::task::spawn_blocking(move || {
        let panes: std::collections::HashMap<
            shux_core::model::PaneId,
            (&shux_vt::Grid, &shux_vt::Cursor),
        > = pane_data.iter().map(|(p, g, c)| (*p, (g, c))).collect();
        let inputs = shux_ui::ComposeInputs {
            layout: &layout_tree,
            zoom: zoom_state.as_ref(),
            focused,
            panes: &panes,
            titles: Some(&titles),
            status_bar: Some(&status_bar),
        };
        let composed = shux_ui::compose(
            &inputs,
            cols,
            rows,
            shux_ui::BorderStyle::Rounded,
            shux_ui::BorderColors::default(),
            STATUS_BAR_ROWS,
        );
        let opts = shux_raster::RasterOptions {
            cursor: composed.cursor,
            ..Default::default()
        };
        let img = rasterizer.render(&composed.grid, &opts);
        let mut buf: Vec<u8> = Vec::with_capacity(128 * 1024);
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
        "window_id": window_id.to_string(),
        "png_base64": b64,
        "width": img.width(),
        "height": img.height(),
        "cell_width": cw,
        "cell_height": ch,
        "cols": cols,
        "rows": rows,
        "format": "png",
    }))
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
        .register_with_policy(
            "window.create",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
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

                    let session_id: shux_core::model::SessionId =
                        session_id_str.parse().map_err(|_| {
                            shux_rpc::RpcError::invalid_params("invalid session_id format")
                        })?;

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

                    let command: Vec<String> = params
                        .get("command")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|s| s.as_str().map(|x| x.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();

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
                    let _ =
                        spawn_pane_pty(window.active_pane, cwd, command, io, ct, gh.clone()).await;

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
                    let session_id_str = params
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'session_id' parameter")
                        })?;

                    let session_id: shux_core::model::SessionId =
                        session_id_str.parse().map_err(|_| {
                            shux_rpc::RpcError::invalid_params("invalid session_id format")
                        })?;

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
                    let session_id_str = params
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'session_id' parameter")
                        })?;

                    let session_id: shux_core::model::SessionId =
                        session_id_str.parse().map_err(|_| {
                            shux_rpc::RpcError::invalid_params("invalid session_id format")
                        })?;

                    let name = params
                        .get("name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'name' parameter")
                        })?
                        .to_string();

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
                    let command: Vec<String> = params
                        .get("command")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|s| s.as_str().map(|x| x.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();
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
                    let _ =
                        spawn_pane_pty(window.active_pane, cwd, command, io, ct, gh.clone()).await;

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
                    let window_id_str =
                        params.get("id").and_then(|v| v.as_str()).ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'id' parameter")
                        })?;

                    let window_id: shux_core::model::WindowId =
                        window_id_str.parse().map_err(|_| {
                            shux_rpc::RpcError::invalid_params("invalid window id format")
                        })?;

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
                    let window_id_str =
                        params.get("id").and_then(|v| v.as_str()).ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'id' parameter")
                        })?;

                    let window_id: shux_core::model::WindowId =
                        window_id_str.parse().map_err(|_| {
                            shux_rpc::RpcError::invalid_params("invalid window id format")
                        })?;

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
                    let window_id_str =
                        params.get("id").and_then(|v| v.as_str()).ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'id' parameter")
                        })?;

                    let window_id: shux_core::model::WindowId =
                        window_id_str.parse().map_err(|_| {
                            shux_rpc::RpcError::invalid_params("invalid window id format")
                        })?;

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
                        for pid in pane_ids {
                            state.writers.remove(&pid);
                            state.resizers.remove(&pid);
                            state.vts.remove(&pid);
                        }
                    }

                    Ok(serde_json::json!({ "killed": window_id_str }))
                }
            },
        )
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
    meta_cache: session_meta::SessionMetaCache,
) -> shux_rpc::RouterBuilder {
    let g1 = graph.clone();
    let g2 = graph.clone();
    let g3 = graph.clone();
    let g4 = graph.clone();
    let g5 = graph.clone();

    let io_create = io_state.clone();
    let io_kill = io_state.clone();
    let io_ensure = io_state;
    let cancel_create = cancel.clone();
    let cancel_ensure = cancel;

    let meta_create = meta_cache.clone();
    let meta_kill = meta_cache.clone();
    let meta_ensure = meta_cache;

    builder
        .register_with_policy(
            "session.list",
            Policy::fixed(Sensitivity::Public),
            move |_params: Option<serde_json::Value>| {
                let gh = g1.clone();
                async move {
                    let snap = gh.snapshot();
                    let mut sessions: Vec<_> = snap.sessions.values().collect();
                    sessions.sort_by_key(|s| s.created_at);
                    let sessions: Vec<serde_json::Value> =
                        sessions.iter().map(|s| session_to_json(s, &snap)).collect();
                    Ok(serde_json::json!({ "sessions": sessions }))
                }
            },
        )
        .register_with_policy(
            "session.create",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g2.clone();
                let io = io_create.clone();
                let ct = cancel_create.clone();
                let meta = meta_create.clone();
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

                    let cwd = params
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .map(PathBuf::from)
                        .unwrap_or_else(|| {
                            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"))
                        });

                    // PR followup (codex P2 #10): persist `command` onto
                    // the initial pane so subscribers + auto-title see
                    // the truth. Pre-followup this RPC stored an empty
                    // `Pane.command` and only the PTY layer knew about
                    // the user's --cmd arg.
                    match gh
                        .create_session_with_command(name, cwd.clone(), command.clone())
                        .await
                    {
                        Ok(session_id) => {
                            // Populate session-meta cache: git branch from
                            // the spawn cwd, SSH context from the daemon
                            // env. spawn_blocking because detect_git_branch
                            // shells out to `git`; using async tokio for
                            // this would force the runtime to wait for git.
                            let meta_cache_clone = meta.clone();
                            let cwd_for_meta = cwd.clone();
                            tokio::task::spawn_blocking(move || {
                                let branch = session_meta::detect_git_branch(&cwd_for_meta);
                                let over_ssh = session_meta::detect_over_ssh();
                                let snapshot = session_meta::SessionMeta {
                                    git_branch: branch,
                                    over_ssh,
                                };
                                // Tiny tokio block to write the cache —
                                // SessionMetaCache.set is async because the
                                // inner RwLock is tokio::sync.
                                tokio::runtime::Handle::current().block_on(async move {
                                    meta_cache_clone.set(session_id, snapshot).await;
                                });
                            });

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
                                            gh.clone(),
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
        .register_with_policy(
            "session.kill",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g3.clone();
                let io = io_kill.clone();
                let meta = meta_kill.clone();
                async move {
                    let params = params.unwrap_or_default();

                    // Accept name or id — try UUID parse first, fall back to name lookup
                    let session_id = if let Some(id_str) = params.get("id").and_then(|v| v.as_str())
                    {
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

                    // Snapshot the session BEFORE destroying it so we know
                    // which panes belong to it. After destroy_session the
                    // graph entries are gone; without this snapshot we'd
                    // have no way to find the orphaned PTY tasks to clean up.
                    let pre_snap = gh.snapshot();
                    let name = pre_snap
                        .sessions
                        .get(&session_id)
                        .map(|s| s.name.clone())
                        .unwrap_or_default();
                    let pane_ids: Vec<shux_core::model::PaneId> = pre_snap
                        .sessions
                        .get(&session_id)
                        .map(|s| {
                            s.windows
                                .iter()
                                .flat_map(|wid| {
                                    pre_snap
                                        .windows
                                        .get(wid)
                                        .map(|w| w.layout.tree.pane_ids())
                                        .unwrap_or_default()
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    drop(pre_snap);

                    let expected_version = parse_expected_version(&params)?;

                    // Mutate the graph FIRST. If destroy_session errors, we
                    // leave PTY/VT state untouched (otherwise we'd kill PTYs
                    // for a session that's still in the graph). Same applies
                    // to a stale `expected_version` — the check inside
                    // destroy_session rejects the request before IO teardown.
                    gh.destroy_session(session_id, expected_version)
                        .await
                        .map_err(graph_error_to_rpc)?;

                    // Tear down PTY/VT/writer/resizer entries for every pane
                    // that belonged to the session. Dropping the writer
                    // Sender closes the mpsc on the recv side, which makes
                    // the per-pane PTY task observe `None` from `write_rx
                    // .recv()`, break out of its select loop, and call
                    // `handle.kill()` on its way out — reaping the child
                    // shell. Without this codex-flagged fix (P1), shells
                    // for killed sessions would stay alive but unreachable.
                    {
                        let mut state = io.lock().await;
                        for pid in &pane_ids {
                            state.writers.remove(pid);
                            state.resizers.remove(pid);
                            state.vts.remove(pid);
                        }
                        let pulse = state.render_pulse.clone();
                        drop(state);
                        pulse.notify_one();
                    }

                    meta.remove(session_id).await;

                    Ok(serde_json::json!({ "killed": name }))
                }
            },
        )
        .register_with_policy(
            "session.ensure",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g4.clone();
                let io = io_ensure.clone();
                let ct = cancel_ensure.clone();
                let meta = meta_ensure.clone();
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

                    // Create new session — `command` persisted onto the
                    // initial pane (codex P2 #10 followup, same fix as
                    // session.create above).
                    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));

                    match gh
                        .create_session_with_command(name, cwd.clone(), command.clone())
                        .await
                    {
                        Ok(session_id) => {
                            // Populate session-meta cache (git branch, SSH).
                            // Same pattern as session.create above.
                            let meta_cache_clone = meta.clone();
                            let cwd_for_meta = cwd.clone();
                            tokio::task::spawn_blocking(move || {
                                let branch = session_meta::detect_git_branch(&cwd_for_meta);
                                let over_ssh = session_meta::detect_over_ssh();
                                let snapshot = session_meta::SessionMeta {
                                    git_branch: branch,
                                    over_ssh,
                                };
                                tokio::runtime::Handle::current().block_on(async move {
                                    meta_cache_clone.set(session_id, snapshot).await;
                                });
                            });

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
                                            gh.clone(),
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
        .register_with_policy(
            "session.rename",
            Policy::fixed(Sensitivity::OwnedMutation),
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

                    let expected_version = parse_expected_version(&params)?;

                    gh.rename_session(session_id, new_name, expected_version)
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
#[allow(clippy::too_many_arguments)]
fn register_pane_io_methods(
    builder: shux_rpc::RouterBuilder,
    graph: shux_core::graph::GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    _cancel: tokio_util::sync::CancellationToken,
    config: shux_core::config::ConfigHandle,
    meta_cache: session_meta::SessionMetaCache,
    onboarding: onboarding::OnboardingHandle,
    segments: statusbar_runner::SegmentCache,
) -> shux_rpc::RouterBuilder {
    let g1 = graph.clone();
    let g2 = graph.clone();
    let g5 = graph.clone();
    let g6 = graph.clone();
    let g7 = graph.clone();
    let g8 = graph.clone();
    let g9 = graph.clone();
    let g10 = graph;

    let io1 = io_state.clone();
    let io2 = io_state.clone();
    let io3 = io_state.clone();
    let io4 = io_state.clone();
    let io5 = io_state.clone();
    let io6 = io_state.clone();
    let io7 = io_state.clone();
    let io8 = io_state.clone();
    let io9 = io_state.clone();
    let io10 = io_state;

    // Shared rasterizer for `pane.snapshot` / `window.snapshot` / `session.snapshot`.
    // Built once at startup so each snapshot call doesn't re-parse the
    // bundled fonts. When `appearance.font` is set, the user's font
    // becomes the primary text font and the bundled NF symbols subset
    // stays as the icon fallback. Font-config changes need a daemon
    // restart to take effect (hot reload only re-renders, doesn't
    // rebuild the rasterizer); documented in the config TOML.
    let rasterizer_pane: Arc<shux_raster::Rasterizer> =
        {
            let cfg_snap = config.current();
            let custom_font: Option<Vec<u8>> = cfg_snap.appearance.font.as_ref().and_then(|p| {
                match std::fs::read(p) {
                    Ok(bytes) => Some(bytes),
                    Err(e) => {
                        tracing::warn!(
                            path = %p.display(),
                            error = %e,
                            "appearance.font: read failed, falling back to bundled JetBrains Mono"
                        );
                        None
                    }
                }
            });
            let raster = match custom_font {
                Some(bytes) => shux_raster::Rasterizer::with_primary_font(14.0, &bytes)
                    .or_else(|_| shux_raster::Rasterizer::new(14.0)),
                None => shux_raster::Rasterizer::new(14.0),
            };
            Arc::new(
                raster
                    .expect("shux-raster: failed to construct rasterizer (bundled font corrupt?)"),
            )
        };
    let rasterizer_window = rasterizer_pane.clone();
    let rasterizer_session = rasterizer_pane.clone();

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
            "pane.snapshot",
            Policy::fixed(Sensitivity::ContentRead),
            move |params: Option<serde_json::Value>| {
                let gh = g6.clone();
                let io = io6.clone();
                let r = rasterizer_pane.clone();
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
                    let (grid_snapshot, cursor_pos, snap_cols, snap_rows) = {
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
                        let cursor_pos = cur.visible.then_some((cur.row, cur.col));
                        // Visible-only clone — does NOT copy scrollback.
                        let grid_clone = vt.grid().clone_visible();
                        (grid_clone, cursor_pos, cols, rows)
                    };

                    // Rasterize + PNG-encode off the runtime worker. Both are
                    // pure-CPU and don't yield, so we route them to a blocking
                    // worker that won't starve other RPC handlers.
                    let (img, png_buf) = tokio::task::spawn_blocking(move || {
                        let opts = shux_raster::RasterOptions {
                            cursor: cursor_pos,
                            ..Default::default()
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
                    let r = rasterizer_window.clone();
                    let cfg = cfg.clone();
                    let meta = meta.clone();
                    let onb = onb.clone();
                    let segs = segs.clone();
                    async move {
                        let params = params.unwrap_or_default();
                        let window_id = resolve_window_id_from_params(&gh, &params)?;
                        let (cols, rows) = parse_snapshot_dims(&params)?;
                        snapshot_window(
                            &gh, &io, window_id, cols, rows, r, &cfg, &meta, &onb, &segs,
                        )
                        .await
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
                    let r = rasterizer_session.clone();
                    let cfg = cfg.clone();
                    let meta = meta.clone();
                    let onb = onb.clone();
                    let segs = segs.clone();
                    async move {
                        let params = params.unwrap_or_default();
                        let session_id_str = params
                            .get("session_id")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| {
                                shux_rpc::RpcError::invalid_params("missing 'session_id' parameter")
                            })?;
                        let session_id: shux_core::model::SessionId =
                            session_id_str.parse().map_err(|_| {
                                shux_rpc::RpcError::invalid_params("invalid session_id format")
                            })?;
                        let snap = gh.snapshot();
                        let session = snap.sessions.get(&session_id).ok_or_else(|| {
                            shux_rpc::RpcError::not_found("session", session_id_str)
                        })?;
                        let window_id = session.active_window;
                        let (cols, rows) = parse_snapshot_dims(&params)?;
                        snapshot_window(
                            &gh, &io, window_id, cols, rows, r, &cfg, &meta, &onb, &segs,
                        )
                        .await
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
        // No subcommand: attach to last session (TTY only). On a
        // non-TTY stdin OR stdout (piped, CI, redirected), don't
        // block — print structured help so scripts get a deterministic
        // response. Attach drives crossterm raw-mode keyboard input,
        // so `shux </dev/null` (stdout-tty + stdin-piped) would
        // hang on the input thread. Guard on BOTH. (Codex council
        // May 2026 + codex bot review of PR #24.)
        None => {
            use std::io::IsTerminal;
            if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
                let help = serde_json::json!({
                    "shux": env!("CARGO_PKG_VERSION"),
                    "help": "Run `shux --help` to see commands. \
                             `shux` with no args attaches to the last session — \
                             but only when BOTH stdin and stdout are a TTY. Try \
                             `shux session list` or `shux rpc call session.list`.",
                    "common_commands": [
                        "shux session create <NAME>",
                        "shux session list",
                        "shux session attach <NAME>",
                        "shux session kill <NAME>",
                        "shux window create -s <SESSION>",
                        "shux pane send-keys -s <SESSION> --text '...'",
                        "shux pane snapshot",
                        "shux plugin install <PATH>",
                        "shux state apply <template.toml>",
                        "shux rpc call <method> --params @file"
                    ]
                });
                println!("{}", serde_json::to_string_pretty(&help)?);
                return Ok(());
            }
            // Recursion guard. Every pane shux spawns gets `SHUX=1`
            // injected (mirrors tmux's `TMUX` env var, see
            // crates/shux-pty defaults). Without a guard here, bare
            // `shux` inside a pane attaches the current TTY to its own
            // daemon — instant render-loop hall-of-mirrors. Mirrors
            // tmux's terse refusal: one line, suggest the env unset.
            if std::env::var_os("SHUX").is_some() {
                eprintln!("sessions should be nested with care, unset $SHUX to force");
                std::process::exit(1);
            }
            let _ = client::ensure_daemon_running_at(&socket_path).await?;
            let session_name = pick_attach_target(&socket_path).await;
            run_attach(&socket_path, session_name).await
        }

        // `shux session <verb>` — canonical session lifecycle.
        // Mirrors `session.*` RPC namespace (`session.create` ↔
        // `shux session create`, etc.). Codex council May 2026
        // established this as the agent-first invariant: RPC dots
        // become CLI spaces, no top-level shortcut verbs.
        Some(Command::Session { command: sc }) => match sc {
            cli::SessionCommand::Create {
                name,
                session,
                ensure,
                detached,
                cmd,
                argv,
            } => {
                let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
                let resolved = name.or(session);
                let session_name = resolved.clone().unwrap_or_else(default_session_name);
                let _ =
                    cli::handle_new(&mut stream, resolved, cmd, argv, ensure, args.format).await?;
                drop(stream);
                if !detached {
                    run_attach(&socket_path, session_name).await
                } else {
                    Ok(())
                }
            }
            cli::SessionCommand::List => {
                let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
                cli::handle_ls(&mut stream, args.format).await
            }
            cli::SessionCommand::Kill {
                name_pos,
                session,
                expected_version,
            } => {
                let resolved = name_pos.or(session).ok_or_else(|| {
                    anyhow::anyhow!(
                        "missing session name: pass it as a positional or via -s/--session"
                    )
                })?;
                let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
                cli::handle_kill(&mut stream, &resolved, expected_version, args.format).await
            }
            cli::SessionCommand::Rename {
                session,
                name,
                expected_version,
            } => {
                let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
                cli::handle_rename(&mut stream, &session, &name, expected_version, args.format)
                    .await
            }
            cli::SessionCommand::Attach { name_pos, session } => {
                let _ = client::ensure_daemon_running_at(&socket_path).await?;
                let session_name = name_pos
                    .or(session)
                    .unwrap_or_else(|| "default".to_string());
                run_attach(&socket_path, session_name).await
            }
            cli::SessionCommand::Snapshot {
                session,
                output,
                cols,
                rows,
            } => {
                let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
                // session.snapshot dispatch: (Some(session), None) → handle_snapshot
                // routes to session.snapshot RPC.
                cli::handle_snapshot(
                    &mut stream,
                    Some(&session),
                    None,
                    output,
                    cols,
                    rows,
                    args.format,
                )
                .await
            }
        },

        Some(Command::Window { command }) => {
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            match command {
                WindowCommand::List { session } => {
                    cli::handle_window_list(&mut stream, &session, args.format).await
                }
                WindowCommand::Create {
                    session,
                    name,
                    cwd,
                    cmd,
                    ensure,
                    argv,
                } => {
                    cli::handle_window_new(
                        &mut stream,
                        &session,
                        name,
                        cwd,
                        cmd,
                        argv,
                        ensure,
                        args.format,
                    )
                    .await
                }
                WindowCommand::Kill {
                    session,
                    window,
                    expected_version,
                } => {
                    cli::handle_window_kill(
                        &mut stream,
                        &session,
                        &window,
                        expected_version,
                        args.format,
                    )
                    .await
                }
                WindowCommand::Rename {
                    session,
                    window,
                    name,
                    expected_version,
                } => {
                    cli::handle_window_rename(
                        &mut stream,
                        &session,
                        &window,
                        &name,
                        expected_version,
                        args.format,
                    )
                    .await
                }
                WindowCommand::Focus {
                    session,
                    window,
                    expected_version,
                } => {
                    cli::handle_window_focus(
                        &mut stream,
                        &session,
                        &window,
                        expected_version,
                        args.format,
                    )
                    .await
                }
                WindowCommand::Reorder {
                    session,
                    window,
                    index,
                    expected_version,
                } => {
                    cli::handle_window_reorder(
                        &mut stream,
                        &session,
                        &window,
                        index,
                        expected_version,
                        args.format,
                    )
                    .await
                }
                WindowCommand::Snapshot {
                    session,
                    window,
                    output,
                    cols,
                    rows,
                } => {
                    cli::handle_snapshot(
                        &mut stream,
                        session.as_deref(),
                        window.as_deref(),
                        output,
                        cols,
                        rows,
                        args.format,
                    )
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
                    expected_version,
                } => {
                    cli::handle_pane_resize(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        &direction,
                        delta,
                        expected_version,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Zoom {
                    session,
                    window,
                    pane,
                    expected_version,
                } => {
                    cli::handle_pane_zoom(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        expected_version,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Swap {
                    session,
                    window,
                    pane,
                    target,
                    expected_version,
                } => {
                    cli::handle_pane_swap(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        &pane,
                        &target,
                        expected_version,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Kill {
                    session,
                    window,
                    pane,
                    expected_version,
                } => {
                    cli::handle_pane_kill(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        &pane,
                        expected_version,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Title {
                    session,
                    window,
                    pane,
                    title,
                    clear,
                    auto,
                    no_auto,
                } => {
                    cli::handle_pane_title(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        title.as_deref(),
                        clear,
                        auto,
                        no_auto,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Watch {
                    session,
                    pane,
                    timeout_ms,
                    limit,
                } => {
                    cli::handle_pane_watch(
                        &mut stream,
                        &session,
                        &pane,
                        timeout_ms,
                        limit,
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
                PaneCommand::WaitFor {
                    session,
                    window,
                    pane,
                    text,
                    regex,
                    absent,
                    lines,
                    timeout_ms,
                    poll_ms,
                } => {
                    cli::handle_wait_for(
                        &mut stream,
                        session.as_deref(),
                        window.as_deref(),
                        pane.as_deref(),
                        text.as_deref(),
                        regex.as_deref(),
                        absent,
                        lines,
                        timeout_ms,
                        poll_ms,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Snapshot {
                    session,
                    window,
                    pane,
                    output,
                } => {
                    cli::handle_pane_snapshot(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        output,
                        args.format,
                    )
                    .await
                }
                PaneCommand::SetSize {
                    session,
                    window,
                    pane,
                    cols,
                    rows,
                } => {
                    cli::handle_pane_set_size(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        cols,
                        rows,
                        args.format,
                    )
                    .await
                }
            }
        }

        Some(Command::Rpc {
            command: cli::RpcCommand::Call { method, params },
        }) => {
            // Resolve `--params` source: inline JSON, `@<file>`, or `-` (stdin).
            // Codex council May 2026: eliminate shell-escaping bait for JSON.
            let resolved = if params == "-" {
                let mut buf = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
                buf
            } else if let Some(path) = params.strip_prefix('@') {
                std::fs::read_to_string(path)?
            } else {
                params
            };
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            cli::handle_api(&mut stream, &method, &resolved, args.format).await
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

        Some(Command::Config { command: cfg_cmd }) => match cfg_cmd {
            cli::ConfigCommand::Init { force } => cli::handle_config_init(force),
            cli::ConfigCommand::ResetHints => cli::handle_config_reset_hints(),
            cli::ConfigCommand::Path => cli::handle_config_path(),
            cli::ConfigCommand::Show => cli::handle_config_show(),
            cli::ConfigCommand::Validate { path, config } => {
                // Either positional or `--config` (mutually exclusive at the
                // clap layer); fold to a single Option for the handler.
                let code = cli::handle_config_validate(path.or(config))?;
                std::process::exit(code);
            }
        },

        Some(Command::Plugin { command: pl_cmd }) => {
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            match pl_cmd {
                cli::PluginCommand::Install {
                    path,
                    args: pargs,
                    cwd,
                    no_watch,
                } => {
                    cli::handle_plugin_install(
                        &mut stream,
                        &path,
                        &pargs,
                        cwd.as_deref(),
                        !no_watch,
                        args.format,
                    )
                    .await
                }
                cli::PluginCommand::List => cli::handle_plugin_list(&mut stream, args.format).await,
                cli::PluginCommand::Kill { name } => {
                    cli::handle_plugin_kill(&mut stream, &name, args.format).await
                }
                cli::PluginCommand::Reload { name } => {
                    cli::handle_plugin_reload(&mut stream, &name, args.format).await
                }
                cli::PluginCommand::Grant {
                    plugin,
                    method,
                    target,
                    subscribe,
                } => {
                    cli::handle_plugin_grant(
                        &mut stream,
                        &plugin,
                        &method,
                        target.as_deref(),
                        subscribe,
                        args.format,
                    )
                    .await
                }
                cli::PluginCommand::Revoke {
                    plugin,
                    method,
                    target,
                    subscribe,
                } => {
                    cli::handle_plugin_revoke(
                        &mut stream,
                        &plugin,
                        &method,
                        target.as_deref(),
                        subscribe,
                        args.format,
                    )
                    .await
                }
                cli::PluginCommand::Grants { plugin } => {
                    cli::handle_plugin_grants(&mut stream, &plugin, args.format).await
                }
                cli::PluginCommand::Audit { plugin, tail } => {
                    cli::handle_plugin_audit(&mut stream, &plugin, tail, args.format).await
                }
            }
        }

        Some(Command::Events { command: ev_cmd }) => {
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            match ev_cmd {
                cli::EventsCommand::Watch {
                    filter,
                    from_seq,
                    timeout_ms,
                    limit,
                } => {
                    cli::handle_events_watch(&mut stream, filter, from_seq, timeout_ms, limit).await
                }
                cli::EventsCommand::History { filter, count } => {
                    cli::handle_events_history(&mut stream, filter, count).await
                }
            }
        }

        Some(Command::State {
            command:
                cli::StateCommand::Apply {
                    template,
                    dry_run,
                    watch,
                },
        }) => {
            // Lower the TOML template to apply ops first (no daemon needed
            // for parse / validate). If --dry-run, print the lowered ops as
            // pretty JSON and exit.
            let ops = match template::load_and_lower(&template) {
                Ok(ops) => ops,
                Err(e) => {
                    eprintln!("{} {e}", style::error("✗ template error:"));
                    std::process::exit(1);
                }
            };

            if dry_run {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({"ops": ops}))?
                );
                return Ok(());
            }

            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            cli::handle_apply(&mut stream, ops, watch, &socket_path).await
        }

        Some(Command::Init { dir }) => {
            let root = dir.unwrap_or_else(|| std::path::PathBuf::from("."));
            cli::handle_init(&root, args.format)
        }

        Some(Command::__daemon) => unreachable!("handled above"),
    }
}

#[cfg(test)]
mod tests {
    //! Snapshot-path regression tests.
    //!
    //! These exercise the seam between `snapshot_window` and the
    //! script-driven `[[statusbar.segment]]` runner: PR #43 shipped the
    //! attach path with `populate_bar` but the snapshot path silently
    //! dropped every user segment. The test below pre-populates a
    //! `SegmentCache` and drives `build_snapshot_status_bar` directly,
    //! asserting the segment text survives into the rendered StatusBar.
    //! If anyone removes the `populate_bar` call from
    //! `build_snapshot_status_bar` it breaks here.
    use super::*;
    use shux_core::config::{Config, ConfigHandle, SegmentDef, StatusBarConfig};
    use shux_core::graph::SessionGraphSnapshot;
    use shux_core::model::{Pane, Session, Window};

    fn config_with_segment(zone: &str) -> ConfigHandle {
        let cfg = Config {
            statusbar: StatusBarConfig {
                left: None,
                center: None,
                right: None,
                segment: vec![SegmentDef {
                    zone: zone.to_string(),
                    command: vec!["echo".to_string()],
                    env: Default::default(),
                    starship_config: None,
                    interval_ms: 1_000,
                    fallback: None,
                }],
            },
            ..Default::default()
        };
        // The cache is pre-populated by the test so the command never
        // runs; we only need `handle.current()` to return our cfg. Use
        // `replace()` to seed it directly — avoids round-tripping a
        // tempfile through TOML serialize/parse just to exercise an
        // in-memory accessor. Pass a never-existing path so
        // `load_or_default` takes the NotFound branch on every platform.
        let nonexistent = std::env::temp_dir().join("__shux_test_no_such_config__.toml");
        let handle = ConfigHandle::load_or_default(&nonexistent);
        handle.replace(cfg);
        handle
    }

    fn snap_with_one_session() -> (
        SessionGraphSnapshot,
        shux_core::model::SessionId,
        shux_core::model::WindowId,
    ) {
        let pane = Pane::new(shux_core::model::WindowId::new(), "/");
        let mut window = Window::new(shux_core::model::SessionId::new(), "0", pane.id);
        // Fix up cross-refs: Window::new and Pane::new each minted their
        // own ids; pane.window_id must match window.id, window.session_id
        // must match session.id.
        let session_id = shux_core::model::SessionId::new();
        window.session_id = session_id;
        let mut pane = pane;
        pane.window_id = window.id;
        let session = Session::new("test", window.id);
        let session = Session {
            id: session_id,
            ..session
        };

        let mut snap = SessionGraphSnapshot::default();
        snap.sessions.insert(session.id, session);
        snap.windows.insert(window.id, window.clone());
        snap.panes.insert(pane.id, pane);
        (snap, session_id, window.id)
    }

    #[tokio::test]
    async fn snapshot_statusbar_includes_script_segments() {
        // Use the test-only OnboardingHandle constructor — no env
        // mutation, no filesystem. Process env is shared mutable state
        // across `cargo test` threads, so any env-mutating test risks
        // racing every other env-mutating test in the same binary
        // (codex round-2 P1: this would race
        // `onboarding::tests::round_trip_dismissal`).
        let onb = onboarding::OnboardingHandle::from_state_for_test(
            onboarding::OnboardingState::default(),
        );
        let config = config_with_segment("right");
        let meta = session_meta::SessionMetaCache::new();
        let segments = statusbar_runner::SegmentCache::new();
        segments
            .set_for_test(0, b"shux-test-sentinel".to_vec())
            .await;

        let (snap, session_id, window_id) = snap_with_one_session();

        let bar = build_snapshot_status_bar(
            &snap,
            &session_id,
            window_id,
            120,
            &config,
            &meta,
            &onb,
            &segments,
        )
        .await;

        let right_text: String = bar.right.iter().map(|s| s.text.clone()).collect();
        assert!(
            right_text.contains("shux-test-sentinel"),
            "expected snapshot status bar's right zone to contain the \
             segment sentinel, got: {right_text:?}"
        );
    }
}
