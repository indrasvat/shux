//! Daemon bootstrap and the `daemon` subcommand.
//!
//! `run_daemon` forks before tokio starts (PRD §4.5 — forking a
//! multi-threaded process is UB), then `run_rpc_server` wires the graph loop,
//! the pane I/O state, the attach listener and the router together and hands
//! back the handle daemon shutdown drains.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, Notify, mpsc};

use crate::pane_io::{PaneIoState, PaneRevision};
use crate::pane_spawn::publish_daemon_config;
use crate::{
    attach, cli, daemon, lens_scratch, onboarding, rpc, session_meta, statusbar_runner, style,
};

/// Daemon entry point.
///
/// 1. Daemonize (double-fork) — BEFORE tokio runtime
/// 2. Create tokio runtime
/// 3. Set up CancellationToken tree
/// 4. Start signal handlers
/// 5. Bind UDS
/// 6. Run daemon state loop
pub fn run_daemon(socket_override: Option<PathBuf>) -> anyhow::Result<()> {
    // Resolve the socket BEFORE forking: the pidfile is keyed to it, and
    // `daemonize` is what writes the pidfile.
    let sock_path = match socket_override {
        Some(p) => p,
        None => daemon::socket_path()?,
    };

    // Step 1: Daemonize BEFORE tokio
    if !daemon::daemonize(&sock_path)? {
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
        let runtime_dir = daemon::ensure_runtime_dir()?;
        daemon::remove_socket_file_for(&sock_path)?;

        // Daemon-level lens audit log (LENS-R-052): ONE instance for the
        // whole daemon — the chain head is cached in memory, so a second
        // opener would fork the chain. Built before the startup reap so
        // reap(reason=registry) entries chain onto the same log.
        let lens_audit = lens_scratch::LensAuditLog::open_default();

        // Lens scratch registry startup reap (LENS-R-044, DEC-7): scratch
        // sessions never survive a restart. Kill any process groups a prior
        // daemon incarnation left registered BEFORE the RPC server starts
        // accepting `lens.run` calls that would populate a fresh registry.
        let (reaped, unresolved_scratch) =
            lens_scratch::ScratchRegistry::startup_reap(&runtime_dir, &lens_audit).await;
        if reaped > 0 {
            tracing::info!(
                reaped,
                "startup: reaped orphaned scratch sessions from a prior daemon"
            );
        }

        // Set up SessionGraph + graph loop.
        //
        // The path the CLIENT resolved, not a second independent guess. The
        // client honours `--socket` / `SHUX_SOCKET` and the daemon did not, so
        // pointing `SHUX_SOCKET` at an unreachable path made the client probe
        // one socket while the daemon it had just auto-started served another.
        // They never met: the client retried, gave up, and left a fully
        // working daemon behind with nothing referencing it — and every
        // subsequent invocation did it again, each new daemon overwriting the
        // pidfile so `daemon stop` could only ever reap the last one.
        let cancel = tokens.root.clone();
        let io_state = run_rpc_server(
            sock_path.clone(),
            cancel.clone(),
            lens_audit,
            unresolved_scratch,
        )
        .await?;

        // Run the daemon state loop (blocks until shutdown)
        shux_core::daemon::run_daemon_state_loop(cmd_rx, tokens.clone(), config_reload_notify)
            .await;

        // Root cancellation is idempotent. Do it here as a final guard,
        // then wait for pane PTY tasks to signal and reap their process
        // groups before the runtime starts dropping tasks.
        tokens.root.cancel();
        shutdown_all_pane_io(io_state).await;

        // Cleanup
        daemon::remove_pid_file_for(&sock_path)?;
        daemon::remove_socket_file_for(&sock_path)?;
        tracing::info!("Daemon shut down cleanly");

        Ok::<(), anyhow::Error>(())
    })?;

    Ok(())
}

/// The exact argument vector of `pid`, or `None` if it cannot be read.
///
/// `/proc` is preferred wherever it exists: it is NUL-separated, so an argument
/// containing a space survives intact, and it cannot be truncated. `ps` is the
/// fallback for macOS, which CI and the release workflow both build for. `-ww`
/// is not optional there -- without it BSD `ps` truncates argv, and a truncated
/// argv silently fails the socket check below, which would resurrect exactly the
/// "no daemon running" leak this function exists to prevent.
fn process_argv(pid: u32) -> Option<Vec<String>> {
    #[cfg(target_os = "linux")]
    if let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) {
        let argv: Vec<String> = raw
            .split(|b| *b == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect();
        if !argv.is_empty() {
            return Some(argv);
        }
    }
    let out = std::process::Command::new("ps")
        .args(["-ww", "-p", &pid.to_string(), "-o", "args="])
        .output()
        .ok()?;
    let joined = String::from_utf8_lossy(&out.stdout);
    let joined = joined.trim_end();
    if joined.is_empty() {
        return None;
    }
    // Space-joined and therefore lossy for arguments containing spaces. Only
    // reachable off Linux.
    Some(joined.split_whitespace().map(str::to_owned).collect())
}

/// The socket a daemon process is serving, read from its own argv.
///
/// `start_daemon_process` always passes `--socket`, but a daemon started by hand
/// as `shux __daemon` has none and serves the default path.
fn served_socket(argv: &[String]) -> Option<PathBuf> {
    let mut it = argv.iter();
    while let Some(arg) = it.next() {
        if arg == "--socket" {
            return it.next().map(PathBuf::from);
        }
        if let Some(inline) = arg.strip_prefix("--socket=") {
            return Some(PathBuf::from(inline));
        }
    }
    daemon::socket_path().ok()
}

/// Whether two socket paths name the same endpoint.
fn same_socket(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// True when `pid` is alive AND is a shux daemon serving `socket`.
///
/// The pidfile is untrusted input: it survives SIGKILL and reboots, and pids get reused, so
/// a bare `kill(pid)` on its contents can hit a bystander. Identity is therefore read from
/// the process's own argv, and it is deliberately NOT an identity check on the executable.
///
/// Two things went wrong with an executable check, both reproduced against real daemons:
///
/// 1. Requiring the basename to be literally `shux` disowned every daemon started by a
///    differently-named build -- an A/B pair, a versioned or distro-renamed install. `daemon
///    stop` reported "no daemon running", exited 0, deleted the pidfile and left the daemon
///    running. Widening it to "or the path equals `current_exe()`" fixed the case where the
///    SAME renamed binary stops its own daemon and left the case where one build stops a
///    daemon started by another still leaking.
/// 2. Neither form looked at WHICH daemon it had found, so any shux daemon at that pid
///    qualified -- including another checkout's, on a recycled pid. `daemon stop` could
///    signal a daemon it had nothing to do with.
///
/// Matching `__daemon` plus the served socket answers both: the socket path is what makes a
/// daemon *ours* rather than merely *a shux daemon*, and it does not care what the file on
/// disk is called. It is also strictly tighter than the executable check it replaces -- a
/// bystander must now reproduce our exact socket path, not merely be named `shux`.
pub(crate) fn is_live_shux_daemon(pid: u32, socket: &Path) -> bool {
    if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_err() {
        return false;
    }
    let Some(argv) = process_argv(pid) else {
        // Without a way to confirm identity, refuse to claim it is ours.
        return false;
    };
    argv_is_daemon_for(&argv, socket)
}

/// Whether `argv` is a shux daemon serving `socket`.
///
/// Split from [`is_live_shux_daemon`] so it can be tested against hand-built argv.
/// Spawning a bystander with a chosen argv is not a workable fixture here: every
/// blocking binary available (`sleep`, `sh -c`) parses its own arguments, so
/// `exec -a` cannot place `__daemon` at argv[1] without the process exiting. The
/// end-to-end proof that this reads a REAL daemon's argv correctly lives in
/// `daemon_lifecycle_integration.rs`, against a real daemon.
fn argv_is_daemon_for(argv: &[String], socket: &Path) -> bool {
    // Exact argv position, not a substring match (085 QA P3): a bystander whose command line
    // merely CONTAINS both words -- say `watch-for shux __daemon` -- must not be accepted.
    if argv.get(1).map(String::as_str) != Some("__daemon") {
        return false;
    }
    served_socket(argv).is_some_and(|served| same_socket(&served, socket))
}

/// The pid of OUR live daemon for `socket`, or `None`.
///
/// **The only sanctioned way to turn a pidfile into a pid you may signal.** Both
/// signalling paths go through here so the identity check cannot be forgotten by
/// a future caller: `daemon stop`, and the version-mismatch restart in
/// `client::kill_stale_daemon`. The latter had no check at all and would SIGTERM
/// whatever number the pidfile held, on an ordinary command.
///
/// pid 0 and 1 are rejected outright: `kill(0, ..)` signals the whole process
/// group, and pid 1 is init.
pub(crate) fn our_live_daemon(socket: &Path) -> Option<u32> {
    let pid = daemon::read_pid_file_for(socket).ok().flatten()?;
    verify_our_daemon(pid, socket).then_some(pid)
}

/// Whether `pid`, already read from a pidfile, is our live daemon for `socket`.
///
/// Callers that need BOTH the recorded pid and the verdict must read the pidfile
/// once and pass the result here, never call [`our_live_daemon`] alongside their
/// own read: two reads can straddle a concurrent restart, and then the pid that
/// was validated is not the pid that gets signalled. That is the arbitrary-
/// process signalling hole this whole change exists to close, reopened as a race.
pub(crate) fn verify_our_daemon(pid: u32, socket: &Path) -> bool {
    pid > 1 && is_live_shux_daemon(pid, socket)
}

/// `shux daemon stop|status` — the missing half of the daemon lifecycle (085 F5).
///
/// The daemon auto-starts on first use and outlives the command, so every scripted or CI
/// invocation leaked one. There was no verb to stop it, and the documented workaround was
/// `pkill -f "shux __daemon"` — which also kills other checkouts' and other agents'
/// daemons. This reaps exactly the pid in this runtime dir's pidfile.
///
/// Deliberately never auto-starts a daemon: starting one in order to stop it would be
/// absurd, and `status` must be able to report "not running".
pub(crate) fn handle_daemon_command(
    command: cli::DaemonCommand,
    format: cli::OutputFormat,
    socket: &Path,
) -> anyhow::Result<()> {
    // ONE read. `ours` is derived from this exact value, so the pid signalled
    // below is the pid that was validated -- see `verify_our_daemon`.
    let pid = daemon::read_pid_file_for(socket).ok().flatten();
    let ours = pid.filter(|p| verify_our_daemon(*p, socket));
    // A pidfile can outlive its process (SIGKILL, a reboot) and the OS reuses pids, so the
    // number in it may name a COMPLETELY UNRELATED process by the time we read it. Probe
    // with signal 0, then confirm the process really is a shux daemon before believing the
    // file — otherwise `daemon stop` becomes "SIGTERM an arbitrary pid", which is the exact
    // failure this verb exists to avoid. pid 0 and 1 are rejected outright: `kill(0, …)`
    // signals the whole process group and pid 1 is init.
    let alive = ours.is_some();

    match command {
        cli::DaemonCommand::Status => {
            match (pid, alive) {
                (Some(p), true) => match format {
                    cli::OutputFormat::Json => println!("{{\"running\": true, \"pid\": {p}}}"),
                    _ => println!(
                        "{} {}",
                        style::success("daemon running"),
                        style::muted(format!("pid {p}"))
                    ),
                },
                _ => match format {
                    cli::OutputFormat::Json => println!("{{\"running\": false, \"pid\": null}}"),
                    _ => println!("{}", style::warning("daemon not running")),
                },
            }
            Ok(())
        }
        cli::DaemonCommand::Stop => {
            let Some(p) = ours else {
                // Alive but not ours means an ordinarily stale pidfile: our daemon
                // died and the OS reused its number. Exit 0 keeps the documented
                // idempotence contract (skills/shux/references/gate.md,
                // skills/shux/examples/headless-tui-test.md). The warning goes to
                // stderr so a trap piping stdout is unaffected -- worth saying out
                // loud, because the bug this branch fixed had exactly this symptom.
                if let Some(p) = pid.filter(|p| *p > 1)
                    && nix::sys::signal::kill(nix::unistd::Pid::from_raw(p as i32), None).is_ok()
                {
                    eprintln!(
                        "{}",
                        style::warning(format!(
                            "stale pidfile named pid {p}, which is alive but is not one of \
                             our daemons; treating the pidfile as stale"
                        ))
                    );
                }
                // Idempotent: safe to call from a cleanup trap that may run twice,
                // and the stale pidfile goes.
                if let cli::OutputFormat::Json = format {
                    println!("{{\"stopped\": false, \"reason\": \"not_running\"}}");
                } else {
                    println!("{}", style::muted("no daemon running"));
                }
                let _ = daemon::remove_pid_file_for(socket);
                return Ok(());
            };
            // SIGTERM → the daemon's signal handler runs a graceful shutdown.
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(p as i32),
                nix::sys::signal::Signal::SIGTERM,
            );
            let mut gone = false;
            for _ in 0..40 {
                if nix::sys::signal::kill(nix::unistd::Pid::from_raw(p as i32), None).is_err() {
                    gone = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            if gone {
                let _ = daemon::remove_pid_file_for(socket);
                let _ = daemon::remove_socket_file_for(socket);
            }
            match (format, gone) {
                (cli::OutputFormat::Json, _) => {
                    println!("{{\"stopped\": {gone}, \"pid\": {p}}}")
                }
                (_, true) => println!(
                    "{} {}",
                    style::success("daemon stopped"),
                    style::muted(format!("pid {p}"))
                ),
                (_, false) => println!(
                    "{}",
                    style::warning(format!(
                        "daemon (pid {p}) did not exit within 2s; it may be draining a session"
                    ))
                ),
            }
            Ok(())
        }
    }
}

/// Start the RPC server with a SessionGraph backing session methods.
///
/// Spawns:
/// 1. The SessionGraph graph loop (single-writer task)
/// 2. The RPC Server accept loop
///
/// Both run until `cancel` is triggered.
pub(crate) async fn run_rpc_server(
    socket_path: PathBuf,
    cancel: tokio_util::sync::CancellationToken,
    lens_audit: Arc<lens_scratch::LensAuditLog>,
    unresolved_scratch: Vec<lens_scratch::RegistryRow>,
) -> anyhow::Result<Arc<Mutex<PaneIoState>>> {
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
    let shutdown_io_state = io_state.clone();

    // Lens scratch registry (§8 SPEC-E, LENS-R-040..046). One per daemon
    // incarnation — the startup reap in `run_daemon` already cleared any
    // resolvable rows from a prior incarnation before this runs.
    let scratch_registry =
        lens_scratch::ScratchRegistry::new(&daemon::runtime_dir()?, lens_audit.clone());

    // Seed rows the startup reap could NOT confirm dead (P5 round-4 codex
    // — the daemon-lifecycle half of N3): seeded rows survive normal
    // persists, count toward the quota, and get a short-deadline standard
    // reaper so the RUNNING daemon retries the kill. Seeding happens
    // before the RPC server accepts connections, so the very first
    // lens.run sees them in the quota.
    scratch_registry
        .seed_unresolved(
            unresolved_scratch,
            &graph_handle,
            &io_state,
            &event_bus,
            Duration::from_secs(1),
        )
        .await;

    // Load user config (~/.config/shux/config.toml). Missing file is
    // valid — defaults match current hardcoded behavior. Spawn a watcher
    // task so edits to the file are picked up live.
    let config_path = shux_core::config::default_config_path();
    let config_handle = shux_core::config::ConfigHandle::load_or_default(&config_path);
    // Publish before the RPC server accepts connections, so the very first
    // pane spawn already sees `[shell]` (issue #132).
    publish_daemon_config(config_handle.clone());
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
    let timeout_graph = graph_handle.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let mut state = timeout_io.lock().await;
                    let _timed_out = state.cmd_engine.check_timeouts();
                    // Issue #115: a pane that opened a synchronized-output
                    // window (`CSI ?2026h`) and then went silent — an
                    // application killed mid-redraw — shows the frame it froze
                    // and nothing else, for ever. The VT enforces the deadline
                    // itself on every batch of pane output, which covers every
                    // pane that is still saying anything; this covers the one
                    // that is not, because no output means nothing calls
                    // `process`. Same critical section as the PTY write path,
                    // so a revealed frame publishes its revision the same way.
                    // Whatever the release reveals has to travel the SAME
                    // routes the PTY read loop would have sent it down, or a
                    // pane that went silent inside a window keeps a stale
                    // title and stale checkpoints for ever. The alt flag and
                    // the title are read on both sides of the release for
                    // exactly that reason.
                    let expired: Vec<_> = state
                        .vts
                        .iter_mut()
                        .filter_map(|(pane_id, vt)| {
                            let alt_before = vt.is_alternate_screen();
                            if !vt.release_expired_sync() {
                                return None;
                            }
                            Some((
                                *pane_id,
                                PaneRevision {
                                    content_revision: vt.content_revision(),
                                    last_mutation_ns: vt.last_mutation_ns(),
                                },
                                alt_before != vt.is_alternate_screen(),
                                vt.title().map(str::to_string),
                            ))
                        })
                        .collect();
                    // Only when a frame actually moved. Pulsing every tick
                    // would wake the renderer once a second on an idle daemon
                    // for nothing.
                    if !expired.is_empty() {
                        let mut revealed_titles = Vec::new();
                        for (pane_id, rev, alt_switched, title) in expired {
                            tracing::debug!(
                                %pane_id,
                                alt_switched,
                                "released a stale synchronized-output window"
                            );
                            state.publish_revision(pane_id, rev);
                            // LENS-R-032/DEC-4: the switch was hidden by the
                            // frozen frame, so the PTY path never saw it. A
                            // checkpoint taken on the other screen buffer must
                            // not be diffable against this one.
                            if alt_switched {
                                state.invalidate_checkpoints(pane_id, rev.content_revision);
                            }
                            if let Some(title) = title.filter(|t| !t.is_empty()) {
                                revealed_titles.push((pane_id, title));
                            }
                        }
                        state.render_pulse.notify_waiters();
                        // Outside the io_state lock: holding it across the
                        // graph's mpsc send is the deadlock pattern from PR #7.
                        drop(state);
                        for (pane_id, title) in revealed_titles {
                            if let Err(e) =
                                timeout_graph.set_pane_osc_title(pane_id, title).await
                            {
                                tracing::warn!(
                                    %pane_id,
                                    error = %e,
                                    "set_pane_osc_title failed after a synchronized-output \
                                     window timed out",
                                );
                            }
                        }
                    }
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

    // Build router: system builtins + session + window + pane + pane I/O + lens.run + events + state + plugin methods
    let router = rpc::build_router(
        graph_handle.clone(),
        io_state,
        cancel.clone(),
        session_meta_cache.clone(),
        scratch_registry.clone(),
        config_handle.clone(),
        onboarding.clone(),
        segment_cache.clone(),
        lens_audit.clone(),
        event_bus,
        plugins.clone(),
    );

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

    // LENS-R-052 denial entries: mirror plugin permission DENIALS of the
    // lens methods into the daemon-level lens audit log (the per-plugin
    // audit log records every denial regardless; this adds the lens view).
    // The caller field here is the one place the identity IS known.
    {
        let audit = lens_audit.clone();
        plugins.set_denial_hook(std::sync::Arc::new(move |_name, uuid, method| {
            const LENS_METHODS: [&str; 5] = [
                "pane.glance",
                "pane.wait_settled",
                "pane.checkpoint",
                "pane.diff_since",
                "lens.run",
            ];
            if LENS_METHODS.contains(&method) {
                audit.append(serde_json::json!({
                    "ts": lens_scratch::iso_now(),
                    "caller": format!("plugin:{uuid}"),
                    "method": method,
                    "decision": "deny",
                }));
            }
        }));
    }

    let config = shux_rpc::ServerConfig {
        socket_path,
        tcp_addr: String::new(),
        auth_token: None,
    };

    let server = shux_rpc::Server::new(config, router, cancel.clone());

    tokio::spawn(async move {
        if let Err(e) = server.run().await {
            tracing::error!(error = %e, "RPC server error");
            // And then SHUT DOWN. A daemon whose RPC server failed to bind
            // serves nobody, but it used to keep running anyway: the failure
            // was logged inside a detached task, onto a subscriber whose
            // output is /dev/null, while the state loop carried on. The client
            // meanwhile retried its ten times, gave up, and left the thing
            // running — once per invocation, each new daemon overwriting the
            // pidfile so `daemon stop` could only ever reap the last of them.
            // Cancelling the root token unwinds the state loop, which is how
            // every other fatal daemon condition already exits.
            cancel.cancel();
        }
    });

    Ok(shutdown_io_state)
}

pub(crate) async fn shutdown_all_pane_io(io_state: Arc<Mutex<PaneIoState>>) {
    let (done, teardown_waiters) = {
        let mut state = io_state.lock().await;
        let pane_ids: Vec<_> = state
            .shutdowns
            .keys()
            .chain(state.writers.keys())
            .chain(state.resizers.keys())
            .chain(state.pty_done.keys())
            .copied()
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        state
            .teardown_waiters
            .retain(|waiter| !waiter.is_finished());
        let teardown_waiters = std::mem::take(&mut state.teardown_waiters);
        let (pulse, done) = state.teardown_panes_collecting(&pane_ids, true);
        drop(state);
        pulse.notify_waiters();
        (done, teardown_waiters)
    };

    let wait_all = async move {
        for rx in done {
            let _ = rx.await;
        }
        for waiter in teardown_waiters {
            let _ = waiter.await;
        }
    };
    if tokio::time::timeout(Duration::from_secs(3), wait_all)
        .await
        .is_err()
    {
        tracing::warn!("timed out waiting for pane PTY tasks during daemon shutdown");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;
    use tokio_util::sync::CancellationToken;

    /// 085 adversarial: `daemon stop` must not SIGTERM whatever number the pidfile holds.
    /// A pidfile survives SIGKILL and reboots, and the OS reuses pids — so its contents can
    /// name a completely unrelated live process. Reproduced before the fix: `daemon stop`
    /// killed an innocent `sleep`. The identity check is what makes the verb safe.
    #[test]
    fn a_live_non_daemon_pid_is_not_mistaken_for_the_daemon() {
        let sock = Path::new("/nonexistent/shux.sock");
        // This test process is alive and is emphatically not a shux daemon.
        assert!(
            !super::is_live_shux_daemon(std::process::id(), sock),
            "a live process that is not `shux __daemon` must never be treated as the daemon"
        );
        // pid 1 is init; signalling it would be catastrophic and it is never our daemon.
        assert!(!super::is_live_shux_daemon(1, sock));

        // 085 QA P3: a bystander whose command line merely CONTAINS both words must be
        // rejected. A substring check accepted this and killed it.
        //
        // argv0 is spoofed with `CommandExt::arg0`, NOT `sh -c 'exec -a ...'`.
        // That shell form is a bashism: `/bin/sh` is dash on Debian and Ubuntu,
        // whose `exec` has no `-a`, so the shell errored out and the "bystander"
        // was already dead by the time the assertion ran. The check then passed
        // because there was no process, not because its argv was rejected --
        // green on every tree, proving nothing. `arg0` is a direct execve and
        // needs no shell at all.
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("/bin/sleep")
            .arg0("watch-for shux __daemon")
            .arg("5")
            .spawn()
            .expect("spawn crafted-argv bystander");

        // Wait for the exec, not for a duration: before it lands argv is still
        // this test's own, and the negative below would again pass for the
        // wrong reason.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let argv = super::process_argv(child.id()).unwrap_or_default();
            if argv.first().is_some_and(|a| a.contains("watch-for")) {
                assert_eq!(
                    argv.get(1).map(String::as_str),
                    Some("5"),
                    "the bystander must be a real live process with a spoofed argv0"
                );
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "bystander never exec'd; argv still {argv:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let verdict = super::is_live_shux_daemon(child.id(), sock);
        let _ = child.kill();
        let _ = child.wait();
        assert!(
            !verdict,
            "a process whose argv merely contains `shux` and `__daemon` is not the daemon"
        );
    }

    /// A daemon is claimed only for the socket it actually serves.
    ///
    /// Without this, any shux daemon at the pidfile's pid qualified -- so on a
    /// recycled pid, `daemon stop` could signal another checkout's daemon.
    #[test]
    fn a_daemon_serving_another_socket_is_not_ours() {
        let argv: Vec<String> = [
            "/opt/shux/bin/shux",
            "__daemon",
            "--socket",
            "/run/other/x.sock",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert!(
            !super::argv_is_daemon_for(&argv, Path::new("/run/mine/x.sock")),
            "a daemon serving another socket must never be ours"
        );
        assert!(
            super::argv_is_daemon_for(&argv, Path::new("/run/other/x.sock")),
            "the same argv IS the daemon for the socket it serves -- without this the \
             test would pass on a predicate that rejects everything"
        );
    }

    /// Identity does not depend on what the executable is called.
    ///
    /// The bug this replaced required the basename to be `shux`, which disowned
    /// every daemon from a renamed build and leaked it.
    #[test]
    fn the_executable_name_does_not_decide_identity() {
        for exe in ["/tmp/shux-AAA", "/opt/shux-0.46.21", "/x/y/shux"] {
            let argv: Vec<String> = [exe, "__daemon", "--socket", "/run/mine/x.sock"]
                .iter()
                .map(|s| s.to_string())
                .collect();
            assert!(
                super::argv_is_daemon_for(&argv, Path::new("/run/mine/x.sock")),
                "{exe} serving our socket is our daemon whatever it is called"
            );
        }
    }

    /// `__daemon` must sit at argv[1], not merely appear somewhere.
    #[test]
    fn daemon_must_be_the_first_argument() {
        let argv: Vec<String> = [
            "watch-for",
            "shux",
            "__daemon",
            "--socket",
            "/run/mine/x.sock",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert!(!super::argv_is_daemon_for(
            &argv,
            Path::new("/run/mine/x.sock")
        ));
    }

    /// A daemon with no `--socket` in its argv serves the default path.
    #[test]
    fn an_argv_without_socket_falls_back_to_the_default_path() {
        let argv = vec!["/usr/bin/shux".to_string(), "__daemon".to_string()];
        assert_eq!(
            super::served_socket(&argv),
            super::daemon::socket_path().ok()
        );

        let explicit = vec![
            "/usr/bin/shux".to_string(),
            "__daemon".to_string(),
            "--socket".to_string(),
            "/run/x/shux.sock".to_string(),
        ];
        assert_eq!(
            super::served_socket(&explicit),
            Some(PathBuf::from("/run/x/shux.sock"))
        );

        let inline = vec![
            "/usr/bin/shux".to_string(),
            "__daemon".to_string(),
            "--socket=/run/y/shux.sock".to_string(),
        ];
        assert_eq!(
            super::served_socket(&inline),
            Some(PathBuf::from("/run/y/shux.sock"))
        );
    }

    #[tokio::test]
    async fn shutdown_awaits_explicit_teardown_waiters() {
        let pane_id = shux_core::model::PaneId::new();
        let pane_shutdown = CancellationToken::new();
        let (done_tx, done_rx) = oneshot::channel();
        let io_state = Arc::new(Mutex::new(PaneIoState::new()));

        {
            let mut state = io_state.lock().await;
            state.shutdowns.insert(pane_id, pane_shutdown.clone());
            state.pty_done.insert(pane_id, done_rx);
            let pulse = state.teardown_panes(&[pane_id], true);
            pulse.notify_one();

            assert!(pane_shutdown.is_cancelled());
            assert!(state.pty_done.is_empty());
            assert_eq!(state.teardown_waiters.len(), 1);
        }

        let mut shutdown = tokio::spawn(shutdown_all_pane_io(io_state.clone()));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut shutdown)
                .await
                .is_err(),
            "daemon shutdown must wait for explicit teardown PTY completion"
        );

        done_tx.send(()).unwrap();
        shutdown.await.unwrap();
        assert!(io_state.lock().await.teardown_waiters.is_empty());
    }
}
