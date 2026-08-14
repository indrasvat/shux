//! Spawning a pane's PTY, and the per-pane task that owns it.
//!
//! Every pane in the daemon is created through
//! [`spawn_pane_pty_with_recorder`] — one funnel, so `[shell]` config, the
//! recorder arm-at-spawn path and the PTY task's shutdown discipline cannot
//! drift apart between call sites. The task itself is the single writer for a
//! pane's VT: reads feed the VT and the command engine, writes and resizes
//! arrive on their own channels, and teardown reaps the child.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{Mutex, mpsc, oneshot, watch};

use crate::pane_io::{PaneIoState, PaneRevision, ResizeRequest};
use crate::pane_record::{
    PaneRecorder, finish_pane_recorders, tee_pane_recorders, tee_pane_resize_recorders,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PtyTaskExit {
    Natural,
    RequestedTeardown,
}

struct PtyTaskControl {
    write_rx: mpsc::Receiver<Vec<u8>>,
    resize_rx: mpsc::Receiver<ResizeRequest>,
    shutdown: tokio_util::sync::CancellationToken,
    done_tx: oneshot::Sender<()>,
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
    control: PtyTaskControl,
    graph: shux_core::graph::GraphHandle,
) {
    use base64::Engine;
    let PtyTaskControl {
        mut write_rx,
        mut resize_rx,
        shutdown,
        done_tx,
    } = control;
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
    let mut task_exit = PtyTaskExit::Natural;

    loop {
        tokio::select! {
            biased;

            _ = shutdown.cancelled() => {
                tracing::debug!(%pane_id, "PTY task cancelled");
                task_exit = PtyTaskExit::RequestedTeardown;
                break;
            }
            result = handle.read(&mut buf) => {
                match result {
                    Ok(0) => {
                        tracing::debug!(%pane_id, "PTY read EOF");
                        break;
                    }
                    Ok(n) => {
                        let data = &buf[..n];
                        tee_pane_recorders(&io_state, pane_id, data, &shutdown).await;
                        let (pulse, vt_title, bus_opt, terminal_responses) = {
                            let mut state = io_state.lock().await;
                            let (vt_title, terminal_responses, rev_state, alt_switched) =
                                if let Some(vt) = state.vts.get_mut(&pane_id) {
                                    // LENS-R-032/DEC-4: detect an alt-screen
                                    // switch by comparing the PRESENTED alt flag
                                    // across the batch (same presented source as
                                    // grid()/glance — frozen under DEC 2026 sync,
                                    // so a switch is seen at the presented frame,
                                    // never mid-sync). A net-zero enter+leave in
                                    // one batch leaves the flag equal → no switch,
                                    // matching §4.2's "nets to no bump".
                                    let alt_before = vt.is_alternate_screen();
                                    let responses = vt.process_with_responses(data);
                                    let alt_after = vt.is_alternate_screen();
                                    let rev = PaneRevision {
                                        content_revision: vt.content_revision(),
                                        last_mutation_ns: vt.last_mutation_ns(),
                                    };
                                    (
                                        vt.title().map(|s| s.to_string()),
                                        responses,
                                        Some(rev),
                                        alt_before != alt_after,
                                    )
                                } else {
                                    (None, Vec::new(), None, false)
                                };
                            // LENS-R-003: publish in the same critical section as
                            // the grid mutation, once per Class-A batch.
                            if let Some(rev) = rev_state {
                                state.publish_revision(pane_id, rev);
                                // LENS-R-032/DEC-4: an alt-screen switch
                                // invalidates every checkpoint of this pane
                                // (marker at the POST-switch revision,
                                // LENS-R-033).
                                if alt_switched {
                                    state.invalidate_checkpoints(pane_id, rev.content_revision);
                                }
                            }
                            let output = String::from_utf8_lossy(data);
                            let _completed = state.cmd_engine.process_output(pane_id.0, &output);
                            (
                                state.render_pulse.clone(),
                                vt_title,
                                state.event_bus.clone(),
                                terminal_responses,
                            )
                        };

                        let mut response_write_failed = false;
                        for response in &terminal_responses {
                            if let Err(e) = handle.write(response).await {
                                tracing::error!(%pane_id, error = %e, "PTY terminal response write error");
                                response_write_failed = true;
                                break;
                            }
                        }
                        if response_write_failed {
                            break;
                        }
                        if !terminal_responses.is_empty()
                            && let Err(e) = handle.flush().await {
                                tracing::error!(%pane_id, error = %e, "PTY terminal response flush error");
                                break;
                            }

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
                            if let Some(t) = vt_title.clone()
                                && !t.is_empty()
                                    && let Err(e) =
                                        graph.set_pane_osc_title(pane_id, t).await
                                    {
                                        tracing::warn!(
                                            %pane_id,
                                            error = %e,
                                            "set_pane_osc_title failed",
                                        );
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
                        task_exit = PtyTaskExit::RequestedTeardown;
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
                        task_exit = PtyTaskExit::RequestedTeardown;
                        break;
                    }
                };
                if let Err(e) = handle.resize(req.size) {
                    tracing::warn!(%pane_id, error = %e, "PTY resize failed");
                }
                let (pulse, dims_changed) = {
                    let mut state = io_state.lock().await;
                    let rev_state = if let Some(vt) = state.vts.get_mut(&pane_id) {
                        // P4 convergence round 1 (claude blocker): gate the
                        // invalidation on an ACTUAL dimension change, exactly
                        // like the process branch gates on alt_switched. The
                        // attach render loop re-fans EVERY pane of the active
                        // window at its computed size on attach, client
                        // resize, window switch, and zoom toggle — at an
                        // unchanged client size those are no-op resizes, and
                        // ungated invalidation made merely attaching (or a
                        // same-size pane.set_size) destroy every checkpoint.
                        // §4.2: only a dims change is the Class-A "pane
                        // resize"; only that invalidates (LENS-R-032).
                        let dims_before = (vt.grid().rows(), vt.grid().cols());
                        vt.resize(req.size.rows as usize, req.size.cols as usize);
                        let dims_changed = (vt.grid().rows(), vt.grid().cols()) != dims_before;
                        Some((
                            PaneRevision {
                                content_revision: vt.content_revision(),
                                last_mutation_ns: vt.last_mutation_ns(),
                            },
                            dims_changed,
                        ))
                    } else {
                        None
                    };
                    // Whether the resize actually changed the pane geometry (used below to gate
                    // both checkpoint invalidation and the cast resize event).
                    let dims_changed = rev_state.as_ref().map(|(_, dc)| *dc).unwrap_or(false);
                    // LENS-R-003: resize is Class-A — publish the bumped revision.
                    if let Some((rev, dc)) = rev_state {
                        state.publish_revision(pane_id, rev);
                        // LENS-R-032/DEC-4: a REAL resize invalidates every
                        // checkpoint of this pane. Record the marker at the
                        // POST-resize revision (LENS-R-033) BEFORE the ack, so
                        // a synchronous `pane.set_size` caller that immediately
                        // diffs an older checkpoint gets RESIZE_INVALIDATED.
                        // Same-size requests never reach here (dims_changed
                        // false): the frame did not change, checkpoints stay
                        // valid.
                        if dc {
                            state.invalidate_checkpoints(pane_id, rev.content_revision);
                        }
                    }
                    (state.render_pulse.clone(), dims_changed)
                };
                pulse.notify_one();
                // Fire the ack AFTER vt + render_pulse so a synchronous
                // caller (pane.set_size RPC) is guaranteed that the next
                // pane.snapshot it issues sees the new dimensions.
                if let Some(ack) = req.ack {
                    let _ = ack.send(());
                }
                // Task 083 cast: emit an honest resize event (only on a REAL geometry change,
                // matching the checkpoint-invalidation gate). Best-effort, non-blocking.
                if dims_changed {
                    tee_pane_resize_recorders(
                        &io_state,
                        pane_id,
                        req.size.cols,
                        req.size.rows,
                    )
                    .await;
                }
            }
        }
    }

    finish_pane_recorders(&io_state, pane_id).await;

    // Reap the child cleanly so plugins and `events.history` see the
    // real exit code on `pane.exited`. The loop exits for several
    // reasons (EOF, read error, channel close, shutdown cancel); only
    // the EOF / read-error paths leave a still-alive child needing a
    // proper wait, while the channel-close and shutdown paths require
    // an explicit kill before waiting will return. Bound both stages
    // with timeouts so a wedged child can't stall pane teardown.
    let exit_code = if task_exit == PtyTaskExit::RequestedTeardown {
        let _ = handle.terminate();
        match tokio::time::timeout(std::time::Duration::from_millis(500), handle.wait()).await {
            Ok(Ok(status)) => status.code(),
            Ok(Err(e)) => {
                tracing::warn!(%pane_id, error = %e, "PTY child wait after teardown failed");
                None
            }
            Err(_) => {
                let _ = handle.kill();
                match tokio::time::timeout(std::time::Duration::from_secs(1), handle.wait()).await {
                    Ok(Ok(status)) => status.code(),
                    _ => None,
                }
            }
        }
    } else {
        match tokio::time::timeout(std::time::Duration::from_secs(2), handle.wait()).await {
            Ok(Ok(status)) => status.code(),
            Ok(Err(e)) => {
                tracing::warn!(%pane_id, error = %e, "PTY child wait failed");
                None
            }
            Err(_) => {
                // Still alive after 2s — send a PTY-style hangup to the
                // process group, then escalate if it refuses to exit.
                let _ = handle.terminate();
                match tokio::time::timeout(std::time::Duration::from_millis(500), handle.wait())
                    .await
                {
                    Ok(Ok(status)) => status.code(),
                    _ => {
                        let _ = handle.kill();
                        match tokio::time::timeout(std::time::Duration::from_secs(1), handle.wait())
                            .await
                        {
                            Ok(Ok(status)) => status.code(),
                            _ => None,
                        }
                    }
                }
            }
        }
    };

    // Propagate the exit so the daemon's PaneExited event fires — set_pane_exit_status
    // both updates the pane and fires the lifecycle event. A SIGNAL death (or a wait
    // failure) has no POSIX code (`status.code()` is None); we still fire, with the
    // lens sentinel `-1` (the same value `lens.run --wait` already reports for a
    // signalled child). This is load-bearing for the lens-gate runner (task 081, adv
    // BLOCKER): its `ExitMonitor` watches `pane.exited`, and a crash that fires no exit
    // event would let the runner compare the crash frame instead of short-circuiting.
    // The reaper and `--wait` also key on this event, so a signalled child is now reaped
    // promptly rather than lingering to `max-runtime`.
    let exit_status = exit_code.unwrap_or(-1);
    if let Err(e) = graph.set_pane_exit_status(pane_id, exit_status).await {
        tracing::debug!(%pane_id, error = %e, "set_pane_exit_status failed (pane may already be gone)");
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
    state.shutdowns.remove(&pane_id);
    state.pty_done.remove(&pane_id);
    let pulse = state.render_pulse.clone();
    drop(state);
    pulse.notify_one();
    let _ = done_tx.send(());
}

/// The daemon's live user config, published for the pane-spawn path.
///
/// [`spawn_pane_pty_with_recorder`] is the single funnel every pane spawn goes
/// through, but none of its ~10 RPC/attach call sites carries a `ConfigHandle`
/// (of the five method registrars, only `register_pane_io_methods` is handed
/// one). Publishing the daemon's handle once at startup wires `[shell]` into
/// that funnel without threading an eleventh argument through all of them.
/// `ConfigHandle::current()` reads the live `ArcSwap`, so edits to `[shell]`
/// are picked up by the next pane spawn — same hot-reload story as the rest of
/// the file, no daemon restart.
///
/// Unset in any process that is not the daemon (the CLI client, unit and
/// integration tests that call `spawn_pane_pty` directly). Those read
/// `ShellConfig::default()`, which is byte-for-byte the pre-issue-#132
/// behaviour.
static DAEMON_CONFIG: std::sync::OnceLock<shux_core::config::ConfigHandle> =
    std::sync::OnceLock::new();

/// Publish the daemon's config handle. Called once, from daemon startup.
pub(crate) fn publish_daemon_config(handle: shux_core::config::ConfigHandle) {
    let _ = DAEMON_CONFIG.set(handle);
}

/// The live `[shell]` section, or defaults outside the daemon.
pub(crate) fn daemon_shell_config() -> shux_core::config::ShellConfig {
    DAEMON_CONFIG
        .get()
        .map(|h| h.current().shell.clone())
        .unwrap_or_default()
}

/// The configured shell argv, or `None` when `[shell].command` does not name a
/// program.
///
/// One place decides what "configured" means, so the pane shell and the shell
/// that interprets a `--cmd` string cannot disagree about it.
pub(crate) fn configured_shell_argv(shell: &shux_core::config::ShellConfig) -> Option<Vec<String>> {
    let program = shell.command.first()?;
    if program.trim().is_empty() {
        return None;
    }
    Some(shell.command.clone())
}

/// Fold the user's `[shell]` config into one pane's spawn plan (issue #132).
///
/// Returns the argv to exec — empty keeps `PtyConfig::default_shell`'s
/// `$SHELL -l -i` — and the env to inject.
///
/// - `shell.command` is the *default* shell override, so it applies only when
///   the caller asked for no command. An explicit `shux new -- vim a.rs` still
///   runs `vim`, never the configured shell.
/// - A **blank** `command[0]` means "not configured". `command = [""]` and
///   `command = ["   "]` parse and validate — the schema is `Vec<String>` and
///   nothing there knows what a program name is — and treating them as an
///   override execs a blank program, so the default pane dies while `--cmd`
///   still works because `interpreting_shell` filters the same blank and falls
///   back to `$SHELL`. That is exactly the drift this change exists to remove.
///   Blank-means-unset is already the house rule: `default_shell_argv` treats a
///   blank `$SHELL` as unset, and `parse_pane_command` treats a blank `command`
///   string as "no command given".
/// - `shell.env` is layered *under* `extra_env`: `PtyHandle::spawn` applies
///   `config.env` in order and last write wins, so a caller that names a
///   variable explicitly (`lens.run`'s deterministic plan) beats the config.
/// - `env_clear` callers get no `shell.env` at all. That flag exists to give
///   the scratch gate runner a hermetic environment containing *only* its own
///   plan; letting user config leak in would defeat the point.
pub(crate) fn resolve_pane_spawn(
    command: Vec<String>,
    shell: &shux_core::config::ShellConfig,
    extra_env: Vec<(String, String)>,
    env_clear: bool,
) -> (Vec<String>, Vec<(String, String)>) {
    let argv = if command.is_empty() {
        configured_shell_argv(shell).unwrap_or_default()
    } else {
        command
    };

    let mut env = Vec::with_capacity(shell.env.len() + extra_env.len());
    if !env_clear {
        env.extend(shell.env.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    env.extend(extra_env);

    (argv, env)
}

/// Spawn a PTY process and VT instance for a pane.
///
/// When `command` is empty, spawns the user's default login+interactive
/// shell — `[shell].command` from the config file when set, otherwise
/// `PtyConfig::default_shell`'s `$SHELL -l -i`. When non-empty, runs that
/// argv directly — this is what `shux new -s X -- vim foo.rs` lands on,
/// so the pane runs `vim foo.rs` instead of a shell. The pane lifetime
/// becomes the lifetime of that command (when it exits, the pane EOFs).
///
/// `size`/`extra_env` let `lens.run` (P5, LENS-R-040) request a non-default
/// PTY size and environment additions for a scratch pane; every other
/// caller passes `PtySize::default()` / an empty env, which reproduces the
/// exact pre-P5 behavior byte-for-byte (`config.size` defaults to 80×24,
/// same as the VT construction this replaces). Returns the raw
/// `shux_pty::PtyError` (rather than an `RpcError`) so callers can map it
/// to their own error code — `lens.run` needs `SPAWN_FAILED (-32014)`,
/// every other caller keeps mapping to `internal()`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn spawn_pane_pty(
    pane_id: shux_core::model::PaneId,
    cwd: PathBuf,
    command: Vec<String>,
    size: shux_pty::handle::PtySize,
    extra_env: Vec<(String, String)>,
    env_clear: bool,
    io_state: Arc<Mutex<PaneIoState>>,
    shutdown: tokio_util::sync::CancellationToken,
    graph: shux_core::graph::GraphHandle,
) -> Result<(), shux_pty::PtyError> {
    spawn_pane_pty_with_recorder(
        pane_id, cwd, command, size, extra_env, env_clear, io_state, shutdown, graph, None,
    )
    .await
}

/// Like [`spawn_pane_pty`] but arms `cast_recorder` (a pre-opened recorder) BEFORE the read loop
/// starts, so the recording captures the child's very first bytes — alt-screen setup, initial
/// geometry, early output — with no post-spawn race (task 083 cast; council: arm at spawn).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn spawn_pane_pty_with_recorder(
    pane_id: shux_core::model::PaneId,
    cwd: PathBuf,
    command: Vec<String>,
    size: shux_pty::handle::PtySize,
    extra_env: Vec<(String, String)>,
    env_clear: bool,
    io_state: Arc<Mutex<PaneIoState>>,
    shutdown: tokio_util::sync::CancellationToken,
    graph: shux_core::graph::GraphHandle,
    cast_recorder: Option<PaneRecorder>,
) -> Result<(), shux_pty::PtyError> {
    let (argv, env) = resolve_pane_spawn(command, &daemon_shell_config(), extra_env, env_clear);
    let mut config = if argv.is_empty() {
        shux_pty::handle::PtyConfig::default_shell(cwd)
    } else {
        shux_pty::handle::PtyConfig::with_command(argv, cwd)
    };
    config.size = size;
    config.env = env;
    config.env_clear = env_clear;
    let handle = shux_pty::handle::PtyHandle::spawn(&config)?;
    let pid = handle.pid();

    let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>(256);
    let (resize_tx, resize_rx) = mpsc::channel::<ResizeRequest>(16);
    let (done_tx, done_rx) = oneshot::channel::<()>();
    let pane_shutdown = shutdown.child_token();
    let vt = shux_vt::VirtualTerminal::new(size.rows as usize, size.cols as usize);
    // LENS-R-003: seed the per-pane revision watch with the VT's initial
    // (content_revision=1, last_mutation_ns=creation time). The initial
    // receiver is dropped; P3 subscribers call `subscribe()` on the stored
    // sender (send_if_modified still updates the retained value with no
    // receivers attached).
    let initial_rev = PaneRevision {
        content_revision: vt.content_revision(),
        last_mutation_ns: vt.last_mutation_ns(),
    };
    let (rev_tx, _rev_rx) = watch::channel(initial_rev);

    {
        let mut state = io_state.lock().await;
        if let Some(old) = state.shutdowns.insert(pane_id, pane_shutdown.clone()) {
            old.cancel();
        }
        state.writers.insert(pane_id, write_tx);
        state.resizers.insert(pane_id, resize_tx);
        state.pty_done.insert(pane_id, done_rx);
        state.vts.insert(pane_id, vt);
        state.revisions.insert(pane_id, rev_tx);
        state.pty_pids.insert(pane_id, pid);
        // Arm the cast recorder in the SAME lock, before the read task spawns, so no output can
        // race ahead of it (task 083).
        if let Some(rec) = cast_recorder {
            state.recorders.entry(pane_id).or_default().push(rec);
        }
    }

    tokio::spawn(run_pane_pty_task(
        pane_id,
        handle,
        io_state,
        PtyTaskControl {
            write_rx,
            resize_rx,
            shutdown: pane_shutdown,
            done_tx,
        },
        graph,
    ));

    Ok(())
}

/// Turn a PTY spawn failure into an error whose hint matches the actual cause.
///
/// `spawn_failed`'s default hint — check `argv[0]` and the cwd — is right for
/// "No such file or directory" and wrong for everything else. `E2BIG` in
/// particular fires when `argv[0]` resolves perfectly and the cwd exists; the
/// command and the environment together are simply larger than `ARG_MAX`, and
/// no ceiling this process could impose would know that number (issue #125
/// follow-up).
pub(crate) fn spawn_failure(e: &shux_pty::PtyError) -> shux_rpc::RpcError {
    shux_rpc::RpcError::spawn_failed_with_hint(&e.to_string(), &spawn_failure_hint(e))
}

/// The same diagnosis as one flat string, for `state.apply`'s per-pane results —
/// `SpawnResult` has room for a message and not for a structured hint.
pub(crate) fn spawn_failure_message(e: &shux_pty::PtyError) -> String {
    format!("{e} — {}", spawn_failure_hint(e))
}

fn spawn_failure_hint(e: &shux_pty::PtyError) -> String {
    spawn_failure_hint_for(&e.to_string(), &daemon_shell_config())
}

/// [`spawn_failure_hint`] with the config injected, so the diagnosis can be
/// tested without a process-global `OnceLock` that no test can reset.
fn spawn_failure_hint_for(detail: &str, shell: &shux_core::config::ShellConfig) -> String {
    if detail.contains("Argument list too long") {
        "the command's arguments and environment together exceed the kernel's \
         ARG_MAX; shorten the command or the environment"
            .to_string()
    } else if detail.contains("Is a directory") || detail.contains("Permission denied") {
        // A directory as argv[0] reports EACCES on Linux, and no amount of
        // chmod fixes that — so the two share a hint that covers both.
        "argv[0] is not an executable file this user can run (a directory, or \
         a file without the execute bit)"
            .to_string()
    } else {
        // Since issue #132 argv[0] can come from a file the user edited days
        // ago instead of the command line they just typed, and "check argv[0]"
        // does not say where to look. Naming the configured program is a fact,
        // not a diagnosis — it does not claim THIS pane used it, only that a
        // pane with no command of its own would.
        match configured_shell_argv(shell).map(|argv| argv[0].clone()) {
            Some(program) => format!(
                "check argv[0] resolves via PATH and cwd exists — note \
                 [shell].command in your shux config runs `{program}` for any \
                 pane with no command of its own"
            ),
            None => "check argv[0] resolves via PATH and cwd exists".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // ── `[shell]` → pane spawn plan (issue #132) ──────────────────────
    //
    // The end-to-end proof lives in `tests/shell_config_e2e.rs`, which reads
    // real pane screens. These pin the branch table itself, including the two
    // precedence rules that are invisible on a screen.

    fn shell_cfg(command: &[&str], env: &[(&str, &str)]) -> shux_core::config::ShellConfig {
        shux_core::config::ShellConfig {
            command: command.iter().map(|s| s.to_string()).collect(),
            env: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn owned(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn configured_shell_command_becomes_a_default_panes_argv() {
        let (argv, _) = resolve_pane_spawn(
            Vec::new(),
            &shell_cfg(&["/bin/dash", "-l"], &[]),
            Vec::new(),
            false,
        );
        assert_eq!(argv, owned(&["/bin/dash", "-l"]));
    }

    #[test]
    fn an_empty_configured_command_keeps_the_dollar_shell_default() {
        // Empty argv is the signal `PtyConfig::default_shell` reads as
        // "$SHELL -l -i". Anything else here would hardcode a shell.
        let (argv, _) = resolve_pane_spawn(Vec::new(), &shell_cfg(&[], &[]), Vec::new(), false);
        assert!(argv.is_empty(), "{argv:?}");
    }

    #[test]
    fn a_blank_configured_program_is_not_a_configured_shell() {
        // `[""]` and `["   "]` parse and validate — the schema is `Vec<String>`
        // and nothing there knows what a program name is. Treating them as an
        // override execs a blank program: the default pane dies while `--cmd`
        // keeps working off `$SHELL`, which is the drift this change removes.
        for blank in [vec![""], vec!["   "], vec!["\t"], vec!["", "-l"]] {
            let cfg = shell_cfg(&blank, &[]);
            assert!(configured_shell_argv(&cfg).is_none(), "{blank:?}");
            let (argv, _) = resolve_pane_spawn(Vec::new(), &cfg, Vec::new(), false);
            assert!(argv.is_empty(), "{blank:?} should fall back to $SHELL");
        }
    }

    #[test]
    fn a_blank_argument_after_a_real_program_is_kept() {
        // Only argv[0] names the program. An empty ARGUMENT is legitimate and
        // must survive — the blank rule is about the program, not the argv.
        let cfg = shell_cfg(&["/bin/sh", "", "-l"], &[]);
        assert_eq!(
            configured_shell_argv(&cfg),
            Some(owned(&["/bin/sh", "", "-l"]))
        );
    }

    #[test]
    fn a_blank_configured_program_gets_no_config_note_in_the_hint() {
        // It is not the configured shell, so naming it would be a lie.
        let hint = spawn_failure_hint_for(
            "failed to spawn child process: No such file or directory (os error 2)",
            &shell_cfg(&["   "], &[]),
        );
        assert_eq!(hint, "check argv[0] resolves via PATH and cwd exists");
    }

    #[test]
    fn an_explicit_command_is_never_replaced_by_the_configured_shell() {
        let (argv, _) = resolve_pane_spawn(
            owned(&["nvim", "a.rs"]),
            &shell_cfg(&["/bin/dash", "-l"], &[]),
            Vec::new(),
            false,
        );
        assert_eq!(argv, owned(&["nvim", "a.rs"]));
    }

    #[test]
    fn configured_env_is_injected_into_every_pane() {
        let (_, env) = resolve_pane_spawn(
            Vec::new(),
            &shell_cfg(&[], &[("LC_ALL", "en_US.UTF-8")]),
            Vec::new(),
            false,
        );
        assert_eq!(env, vec![("LC_ALL".to_string(), "en_US.UTF-8".to_string())]);
    }

    #[test]
    fn a_callers_explicit_env_wins_over_the_configured_env() {
        // `PtyHandle::spawn` applies `config.env` in order and the last write
        // wins, so the caller's entry must come SECOND, not merely be present.
        let (_, env) = resolve_pane_spawn(
            Vec::new(),
            &shell_cfg(&[], &[("LC_ALL", "from-config")]),
            vec![("LC_ALL".to_string(), "from-caller".to_string())],
            false,
        );
        assert_eq!(
            env,
            vec![
                ("LC_ALL".to_string(), "from-config".to_string()),
                ("LC_ALL".to_string(), "from-caller".to_string()),
            ]
        );
    }

    #[test]
    fn env_clear_callers_get_no_configured_env_at_all() {
        // `env_clear` exists to give the scratch gate runner an environment
        // containing ONLY its own plan. User config leaking in would defeat it.
        let (_, env) = resolve_pane_spawn(
            Vec::new(),
            &shell_cfg(&[], &[("LC_ALL", "from-config")]),
            vec![("PATH".to_string(), "/usr/bin".to_string())],
            true,
        );
        assert_eq!(env, vec![("PATH".to_string(), "/usr/bin".to_string())]);
    }

    #[test]
    fn a_failed_spawn_names_the_configured_shell_as_a_place_to_look() {
        // argv[0] can now come from a file the user edited days ago. "check
        // argv[0]" alone does not say where to look.
        let hint = spawn_failure_hint_for(
            "failed to spawn child process: No such file or directory (os error 2)",
            &shell_cfg(&["/usr/bin/zsh-typo", "-l"], &[]),
        );
        assert!(hint.contains("[shell].command"), "{hint}");
        assert!(hint.contains("/usr/bin/zsh-typo"), "{hint}");
    }

    #[test]
    fn a_failed_spawn_says_nothing_about_config_when_there_is_none() {
        let hint = spawn_failure_hint_for(
            "failed to spawn child process: No such file or directory (os error 2)",
            &shell_cfg(&[], &[]),
        );
        assert_eq!(hint, "check argv[0] resolves via PATH and cwd exists");
    }

    #[test]
    fn the_argv_too_long_diagnosis_does_not_get_a_config_note() {
        // That failure is about total size, not about which program was named.
        let hint = spawn_failure_hint_for(
            "failed to spawn child process: Argument list too long (os error 7)",
            &shell_cfg(&["/bin/zsh"], &[]),
        );
        assert!(hint.contains("ARG_MAX"), "{hint}");
        assert!(!hint.contains("[shell].command"), "{hint}");
    }

    #[test]
    fn a_process_without_a_published_config_sees_plain_defaults() {
        // Every non-daemon process (the CLI client, these tests) reads this.
        // It must be the pre-issue-#132 behaviour, byte for byte.
        let shell = daemon_shell_config();
        assert!(shell.command.is_empty());
        assert!(shell.env.is_empty());
    }
}
