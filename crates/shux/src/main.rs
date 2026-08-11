use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::{CommandFactory, FromArgMatches};
use tokio::sync::{Mutex, Notify, mpsc};
use tracing_subscriber::EnvFilter;

mod attach;
mod cli;
mod client;
mod config_validate;
mod daemon;
mod features;
mod gate;
mod lens_render;
mod lens_scratch;
mod onboarding;
mod pane_command;
mod pane_io;
mod pane_record;
mod pane_spawn;
mod rpc;
mod session_meta;
mod session_persist;
mod settle;
mod snapshot;
mod statusbar_build;
mod statusbar_runner;
mod style;
mod template;

use cli::{Cli, Command, OutputFormat, PaneCommand, WindowCommand};
use pane_io::{PaneIoState, PaneRevision};
use pane_spawn::publish_daemon_config;

fn main() {
    // Inject the colorised agent reference at runtime so it honours
    // NO_COLOR + the IsTerminal piped-stdout check. clap's derive macro
    // only accepts a `&'static str` literal there, so we set it here.
    let cmd = Cli::command()
        .before_help(style::banner())
        .long_about(cli::long_about())
        .after_long_help(cli::agent_help());
    let matches = cmd.get_matches();
    let args = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());

    let result = if matches!(args.command, Some(Command::__daemon)) {
        // Internal daemon subcommand — called by auto-start. `--socket` is a
        // global arg, so the re-exec'd daemon sees whatever the client saw.
        run_daemon(args.socket.clone())
    } else {
        // Normal CLI client mode
        run_client(args)
    };

    if let Err(e) = result {
        report_fatal(&e);
    }
}

/// Render a fatal error once, then exit non-zero.
///
/// `main` used to return `anyhow::Result<()>`, so every error `run_client` had
/// already rendered through `style::print_error` got rendered a SECOND time by
/// std's `Termination` impl — `Error: …` followed by thirty frames of
/// backtrace naming our own dependency paths, for conditions as ordinary as a
/// typo'd session name. That is issue #133, and it made every "not found" read
/// like a crash.
///
/// Exiting here means the `Termination` path is never reached, so the message
/// an operator sees is the one we chose to write. The backtrace stays
/// reachable behind the standard opt-in, for whoever is actually debugging.
fn report_fatal(e: &anyhow::Error) -> ! {
    use std::io::Write as _;

    // `{:#}` walks the anyhow chain onto one line; `print_error` owns the
    // marker, the colour and the NO_COLOR check.
    style::print_error(&format!("{e:#}"));

    if std::env::var_os("RUST_BACKTRACE").is_some_and(|v| !v.is_empty() && v != "0") {
        // anyhow's Debug is exactly what Termination used to print — chain
        // plus captured backtrace. Opted into, it is useful; unconditional,
        // it was the defect.
        //
        // Sanitized, and NOT optionally. The chain carries error text built
        // from untrusted input — a TOML parse diagnostic quotes the offending
        // source line verbatim, so a template containing a raw ESC replays it
        // straight at the operator's terminal (issue #104's whole class).
        // `safe_diagnostic` keeps `\n`/`\t`, which are this block's structure,
        // and escapes everything else. Asking for a backtrace is not consent
        // to be attacked by one.
        eprintln!("\n{}", style::safe_diagnostic(&format!("{e:?}")));
    }

    // `exit` runs no destructors, so anything buffered on the data channel
    // has to be flushed by hand or a partial `--format json` payload is lost.
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    std::process::exit(1);
}

/// Daemon entry point.
///
/// 1. Daemonize (double-fork) — BEFORE tokio runtime
/// 2. Create tokio runtime
/// 3. Set up CancellationToken tree
/// 4. Start signal handlers
/// 5. Bind UDS
/// 6. Run daemon state loop
fn run_daemon(socket_override: Option<PathBuf>) -> anyhow::Result<()> {
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

/// The human-readable half of an `RpcError`, for CLI-side reporting.
///
/// `RpcError`'s `Display` is the JSON-RPC code name (`invalid_params`); the
/// sentence a person needs is in `data.detail`.
fn rpc_error_detail(e: &shux_rpc::RpcError) -> String {
    serde_json::to_value(e)
        .ok()
        .and_then(|v| {
            v.get("data")
                .and_then(|d| d.get("detail"))
                .and_then(|d| d.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| e.to_string())
}

/// True when `pid` is alive AND is actually a shux daemon.
///
/// The pidfile is untrusted input: it survives SIGKILL and reboots, and pids get reused, so
/// a bare `kill(pid)` on its contents can hit a bystander. Verified by reading the process's
/// own argv — a shux daemon runs as `<path>/shux __daemon`.
fn is_live_shux_daemon(pid: u32) -> bool {
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
fn handle_daemon_command(
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
                // Idempotent: safe to call from a cleanup trap that may run twice.
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

/// Client entry point — parse CLI args, ensure daemon is running, dispatch.
fn run_client(args: Cli) -> anyhow::Result<()> {
    // Set up logging
    let filter = if args.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::from_default_env()
    };
    // 085 F22: logs go to STDERR, never stdout. stdout is the DATA channel — `--format
    // json` and `lens gate --report -` promise it carries only the payload — and the old
    // default writer broke that contract exactly when someone reached for `-v` to debug,
    // emitting ANSI-coloured DEBUG lines ahead of the JSON.
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    let rt = tokio::runtime::Runtime::new()?;
    // Rendering belongs to `report_fatal`, which is the single place an error
    // reaches the operator. Printing here as well as returning the `Err` is
    // what produced the double-print in issue #133.
    rt.block_on(async { dispatch(args).await })
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

async fn shutdown_all_pane_io(io_state: Arc<Mutex<PaneIoState>>) {
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

/// Decide which session `shux` (no args) should attach to.
///
/// Strategy: query the daemon for sessions; if any exist, pick the most
/// recently created. If none, fall through to "default" (which the
/// daemon-side attach handler will create on demand).
/// Choose the session a bare `shux` / `shux attach` lands on from a
/// `session.list` result. NEVER a scratch session (lens PRD LENS-R-041 —
/// P5 round-1 minor: scratch panes are agent working surfaces; a human
/// attaching blind must not land inside one). Defense in depth: the
/// default `session.list` already omits scratch, and this filter also
/// rejects `scratch: true` flags and the reserved `__scratch-` name prefix
/// in case a future caller feeds an `--include-scratch` listing through.
fn choose_attach_session(sessions: &[serde_json::Value]) -> Option<String> {
    sessions
        .iter()
        .filter(|s| s.get("scratch").and_then(|v| v.as_bool()) != Some(true))
        .filter_map(|s| s.get("name")?.as_str())
        .find(|name| !name.starts_with("__scratch-"))
        .map(str::to_string)
}

async fn pick_attach_target(socket_path: &std::path::Path) -> String {
    if let Ok(mut stream) = client::try_connect(socket_path).await
        && let Ok(value) = cli::rpc_call(&mut stream, "session.list", serde_json::json!({})).await
        && let Some(arr) = value.get("sessions").and_then(|v| v.as_array())
        && let Some(target) = choose_attach_session(arr)
    {
        return target;
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
    let cfg_snapshot =
        shux_core::config::ConfigHandle::load_or_default(&shux_core::config::default_config_path())
            .current();
    let cfg = shux_ui::ClientConfig {
        socket_path: attach_path.to_string_lossy().to_string(),
        session_name: session_name.clone(),
        prefix: cfg_snapshot.keys.prefix.clone(),
        prefix_key: crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(' '),
            crossterm::event::KeyModifiers::CONTROL,
        ),
        keybindings: cfg_snapshot.keybindings.clone(),
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
                println!(
                    "{}",
                    style::json_safe(&serde_json::to_string_pretty(&help)?)
                );
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
                cwd,
                title,
                cmd,
                argv,
            } => {
                let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
                let resolved = name.or(session);
                let session_name = resolved.clone().unwrap_or_else(default_session_name);
                let _ = cli::handle_new(
                    &mut stream,
                    cli::SessionCreateOptions {
                        session_name: resolved,
                        cwd,
                        title,
                        cmd,
                        argv,
                        ensure,
                    },
                    args.format,
                )
                .await?;
                drop(stream);
                if !detached {
                    run_attach(&socket_path, session_name).await
                } else {
                    Ok(())
                }
            }
            cli::SessionCommand::List { include_scratch } => {
                let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
                cli::handle_ls(&mut stream, include_scratch, args.format).await
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
                // Refuse before touching the terminal. `run_attach` drives
                // crossterm raw mode, and crossterm opens `/dev/tty` DIRECTLY —
                // it resolves through the process's controlling terminal and
                // ignores stdin/stdout. So piping a child's stdio does not stop
                // it: with a controlling terminal present it grabbed the tty and
                // sat in the interactive loop forever, and without one it died
                // on `enter raw mode: No such device or address` after already
                // emitting escape sequences and a 35-frame backtrace.
                //
                // The bare-`shux` path at the `None` arm above has had this
                // guard since a council + bot review of PR #24, for exactly this
                // hang. The subcommand was missed. Same test, same reason.
                use std::io::IsTerminal;
                if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
                    anyhow::bail!(
                        "`session attach` needs a terminal: it takes over the screen \
                         and reads keys, so it cannot run with stdin or stdout \
                         redirected. Run it from a terminal, or use \
                         `shux pane capture`/`shux pane snapshot` to read a session \
                         non-interactively."
                    );
                }
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
            cli::SessionCommand::Save { session, output } => {
                let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
                cli::handle_session_save(&mut stream, &session, output).await
            }
            cli::SessionCommand::Restore {
                template,
                dry_run,
                watch,
            } => {
                // Identical wording and identical funnel to `state apply` —
                // see the comment there (issue #137).
                let ops = template::load_and_lower(&template)
                    .map_err(|e| anyhow::Error::from(e).context("template error"))?;
                pane_command::validate_ops(&ops)
                    .map_err(|e| anyhow::anyhow!("template error: {}", rpc_error_detail(&e)))?;
                if dry_run {
                    // Same payload, same shape as `state apply --dry-run`:
                    // both lower through `template::load_and_lower` and both
                    // preview what goes on the wire to `state.apply`, so a
                    // consumer should not have to know which verb produced it
                    // (issue #137).
                    println!(
                        "{}",
                        style::json_safe(&serde_json::to_string_pretty(
                            &serde_json::json!({"ops": ops})
                        )?)
                    );
                    Ok(())
                } else {
                    let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
                    cli::handle_apply(&mut stream, ops, watch, &socket_path, args.format).await
                }
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
                    cmd,
                    argv,
                } => {
                    cli::handle_pane_split(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        direction.as_deref(),
                        ratio,
                        cmd,
                        argv,
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
                PaneCommand::Record {
                    session,
                    pane,
                    to,
                    force,
                    duration_ms,
                } => {
                    cli::handle_pane_record(
                        &mut stream,
                        &session,
                        &pane,
                        &to,
                        force,
                        duration_ms,
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
                PaneCommand::Glance {
                    pane,
                    png,
                    text_only,
                    no_cursor,
                    checkpoint,
                    cells,
                    cells_out,
                    masks,
                } => {
                    cli::handle_pane_glance(
                        &mut stream,
                        &pane,
                        png,
                        text_only,
                        no_cursor,
                        checkpoint,
                        cells || cells_out.is_some(),
                        cells_out,
                        masks,
                        args.format,
                    )
                    .await
                }
                PaneCommand::WaitSettled {
                    pane,
                    quiet,
                    timeout,
                    hold_ms,
                    stable_frames,
                } => {
                    cli::handle_pane_wait_settled(
                        &mut stream,
                        &pane,
                        quiet,
                        timeout,
                        hold_ms,
                        stable_frames,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Checkpoint { pane } => {
                    cli::handle_pane_checkpoint(&mut stream, &pane, args.format).await
                }
                PaneCommand::Diff {
                    pane,
                    since,
                    heat,
                    no_row_text,
                } => {
                    cli::handle_pane_diff(&mut stream, &pane, since, heat, no_row_text, args.format)
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

        Some(Command::Lens {
            command:
                cli::LensCommand::Run {
                    size,
                    ttl,
                    max_runtime,
                    env,
                    cwd,
                    wait,
                    argv,
                },
        }) => {
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            cli::handle_lens_run(
                &mut stream,
                &argv,
                size,
                ttl,
                max_runtime,
                &env,
                cwd.as_deref(),
                wait,
                args.format,
            )
            .await
        }

        Some(Command::Lens {
            command:
                cli::LensCommand::Gate {
                    scenario,
                    golden_dir,
                    report,
                    on_missing,
                    update,
                    reason,
                    tol,
                    out,
                    retries,
                    cast,
                    trace,
                    sub,
                    argv,
                },
        }) => {
            let code = match sub {
                Some(cli::GateSubcommand::Review {
                    scenario,
                    golden_dir,
                    out,
                }) => gate::review::run_review(&socket_path, scenario, golden_dir, out).await?,
                Some(cli::GateSubcommand::Init { name, dir }) => {
                    gate::init::run_init(&socket_path, name, dir).await?
                }
                None => {
                    let scenario_path = scenario.ok_or_else(|| {
                        anyhow::anyhow!(
                            "lens gate: a SCENARIO is required (or use `review`/`init`)"
                        )
                    })?;
                    let opts = gate::driver::GateRunOptions {
                        scenario_path,
                        golden_dir,
                        report,
                        on_missing,
                        update,
                        reason,
                        tol,
                        out,
                        retries,
                        cast,
                        trace,
                        argv,
                        format: args.format,
                    };
                    gate::driver::run_gate(&socket_path, opts).await?
                }
            };
            std::process::exit(code);
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

        Some(Command::Daemon { command }) => handle_daemon_command(command, args.format),

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
            features::plugin::dispatch(pl_cmd, &socket_path, args.format).await
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
            // Both template verbs report a bad template the same way, through
            // the same funnel: `report_fatal` renders the anyhow chain once
            // and `style::print_error` sanitizes it, which matters because the
            // TOML diagnostic quotes the offending source line verbatim and
            // this runs before the daemon exists, so ingress sanitizing cannot
            // reach it (issue #104). Previously `state apply` prefixed
            // `template error:` here while `session restore` propagated the
            // bare error, so the same broken template read differently
            // depending on which verb you typed (issue #137).
            let ops = template::load_and_lower(&template)
                .map_err(|e| anyhow::Error::from(e).context("template error"))?;

            // Before printing OR sending: `--dry-run` exists to answer "will
            // this apply succeed?", and the argv rule used to live only in the
            // daemon, so dry-run said yes to templates the real run rejects.
            pane_command::validate_ops(&ops)
                .map_err(|e| anyhow::anyhow!("template error: {}", rpc_error_detail(&e)))?;

            if dry_run {
                // `--dry-run` prints the ops BEFORE the graph sanitizes
                // them — it is the one place a hostile title is meant to
                // be shown verbatim, so it must be shown inertly.
                println!(
                    "{}",
                    style::json_safe(&serde_json::to_string_pretty(
                        &serde_json::json!({"ops": ops})
                    )?)
                );
                return Ok(());
            }

            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            cli::handle_apply(&mut stream, ops, watch, &socket_path, args.format).await
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

    use tokio::sync::oneshot;

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

    use tokio_util::sync::CancellationToken;

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

    /// Bare `shux` / `shux attach` target choice never lands on a scratch
    /// session (P5 round-1 minor — attach guard), whether flagged
    /// `scratch: true` or recognizable by the reserved name prefix.
    #[test]
    fn choose_attach_session_never_picks_scratch() {
        // Scratch-only listing → None (fall through to "default").
        let scratch_only = vec![
            serde_json::json!({"name": "__scratch-abc", "scratch": true}),
            serde_json::json!({"name": "__scratch-def"}),
        ];
        assert_eq!(choose_attach_session(&scratch_only), None);

        // Mixed listing → first NON-scratch name, regardless of order.
        let mixed = vec![
            serde_json::json!({"name": "__scratch-abc", "scratch": true}),
            serde_json::json!({"name": "work"}),
            serde_json::json!({"name": "other"}),
        ];
        assert_eq!(choose_attach_session(&mixed), Some("work".to_string()));

        // Flag wins even when the name looks ordinary.
        let flagged = vec![
            serde_json::json!({"name": "sneaky", "scratch": true}),
            serde_json::json!({"name": "real"}),
        ];
        assert_eq!(choose_attach_session(&flagged), Some("real".to_string()));

        assert_eq!(choose_attach_session(&[]), None);
    }
}
