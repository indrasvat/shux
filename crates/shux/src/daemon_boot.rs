//! Daemon bootstrap and the `daemon` subcommand.
//!
//! `run_daemon` forks before tokio starts (PRD §4.5 — forking a
//! multi-threaded process is UB), then `run_rpc_server` wires the graph loop,
//! the pane I/O state, the attach listener and the router together and hands
//! back the handle daemon shutdown drains.

use std::path::PathBuf;
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
        let runtime_dir = daemon::ensure_runtime_dir()?;
        daemon::remove_socket_file()?;

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
        let sock_path = match socket_override {
            Some(p) => p,
            None => daemon::socket_path()?,
        };
        let cancel = tokens.root.clone();
        let io_state =
            run_rpc_server(sock_path, cancel.clone(), lens_audit, unresolved_scratch).await?;

        // Run the daemon state loop (blocks until shutdown)
        shux_core::daemon::run_daemon_state_loop(cmd_rx, tokens.clone(), config_reload_notify)
            .await;

        // Root cancellation is idempotent. Do it here as a final guard,
        // then wait for pane PTY tasks to signal and reap their process
        // groups before the runtime starts dropping tasks.
        tokens.root.cancel();
        shutdown_all_pane_io(io_state).await;

        // Cleanup
        daemon::remove_pid_file()?;
        daemon::remove_socket_file()?;
        tracing::info!("Daemon shut down cleanly");

        Ok::<(), anyhow::Error>(())
    })?;

    Ok(())
}

/// True when `pid` is alive AND is actually a shux daemon.
///
/// The pidfile is untrusted input: it survives SIGKILL and reboots, and pids get reused, so
/// a bare `kill(pid)` on its contents can hit a bystander. Verified by reading the process's
/// own argv — a shux daemon runs as `<path>/shux __daemon`.
pub(crate) fn is_live_shux_daemon(pid: u32) -> bool {
    if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_err() {
        return false;
    }
    let Ok(out) = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "args="])
        .output()
    else {
        // Without a way to confirm identity, refuse to claim it is ours.
        return false;
    };
    // Exact argv positions, not a substring match (085 QA P3): a bystander whose command
    // line merely CONTAINS both words — say `watch-for shux __daemon` — would otherwise be
    // accepted as the daemon and signalled. A shux daemon is `<path>/shux __daemon`.
    let args = String::from_utf8_lossy(&out.stdout);
    let args = args.trim_end();

    // A daemon THIS executable started is ours whatever the file is called.
    // Requiring the basename to be literally `shux` disowned every daemon from a
    // differently-named build (an A/B pair, a renamed install): `daemon stop`
    // reported "no daemon running" and leaked it. Works because the daemon is
    // spawned with `current_exe()` (`start_daemon_process` in client.rs).
    // Matching the whole argv prefix, not a whitespace-split field, also
    // survives an executable path containing spaces.
    //
    // Caveat: BSD/macOS `ps` can truncate argv without `-ww`. The basename
    // branch below has the same exposure, so this is not a new risk.
    if let Ok(self_exe) = std::env::current_exe()
        && let Some(self_exe) = self_exe.to_str()
        && args
            .strip_prefix(self_exe)
            .and_then(|rest| rest.strip_prefix(" __daemon"))
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
    {
        return true;
    }

    // An installed `shux` is ours too, even from a different build than this one.
    let mut argv = args.split_whitespace();
    let exe_is_shux = argv
        .next()
        .and_then(|p| p.rsplit('/').next())
        .is_some_and(|base| base == "shux");
    let first_arg_is_daemon = argv.next() == Some("__daemon");
    exe_is_shux && first_arg_is_daemon
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
) -> anyhow::Result<()> {
    let pid = daemon::read_pid_file().ok().flatten();
    // A pidfile can outlive its process (SIGKILL, a reboot) and the OS reuses pids, so the
    // number in it may name a COMPLETELY UNRELATED process by the time we read it. Probe
    // with signal 0, then confirm the process really is a shux daemon before believing the
    // file — otherwise `daemon stop` becomes "SIGTERM an arbitrary pid", which is the exact
    // failure this verb exists to avoid. pid 0 and 1 are rejected outright: `kill(0, …)`
    // signals the whole process group and pid 1 is init.
    let alive = pid.is_some_and(|p| p > 1 && is_live_shux_daemon(p));

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
            let Some(p) = pid.filter(|_| alive) else {
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
                let _ = daemon::remove_pid_file();
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
                let _ = daemon::remove_pid_file();
                let _ = daemon::remove_socket_file();
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
        // This test process is alive and is emphatically not a shux daemon.
        assert!(
            !super::is_live_shux_daemon(std::process::id()),
            "a live process that is not `shux __daemon` must never be treated as the daemon"
        );
        // pid 1 is init; signalling it would be catastrophic and it is never our daemon.
        assert!(!super::is_live_shux_daemon(1));

        // 085 QA P3: a bystander whose command line merely CONTAINS both words must be
        // rejected. A substring check accepted this and killed it.
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", r#"exec -a "watch-for shux __daemon" /bin/sleep 5"#])
            .spawn()
            .expect("spawn crafted-argv bystander");
        std::thread::sleep(std::time::Duration::from_millis(400));
        let verdict = super::is_live_shux_daemon(child.id());
        let _ = child.kill();
        let _ = child.wait();
        assert!(
            !verdict,
            "a process whose argv merely contains `shux` and `__daemon` is not the daemon"
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
