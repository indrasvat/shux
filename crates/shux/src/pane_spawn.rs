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
    /// See [`PaneIoState::incarnations`].
    incarnation: u64,
    write_rx: mpsc::Receiver<Vec<u8>>,
    resize_rx: mpsc::Receiver<ResizeRequest>,
    shutdown: tokio_util::sync::CancellationToken,
    done_tx: oneshot::Sender<()>,
    input: shux_pty::PtyInputWriter,
}

/// Everything bound for the program's input, in order: terminal replies
/// (DA1, DSR, OSC 7501 probes) and user input. Replies go between input
/// chunks, never inside one. A reply that does not fit is dropped whole; input
/// is never dropped — the loop stops taking more until the program reads.
#[derive(Default)]
struct PtyInputQueue {
    replies: std::collections::VecDeque<u8>,
    input: std::collections::VecDeque<Vec<u8>>,
    /// Bytes of `input.front()` already written.
    written: usize,
    input_len: usize,
}

impl PtyInputQueue {
    /// Replies stop queueing early: a program that ignores them gets none.
    const CAP: usize = 4096;
    /// Input queued past this waits in the pane's channel instead.
    const INPUT_CAP: usize = 1 << 20;

    fn push(&mut self, reply: &[u8]) -> bool {
        if self.replies.len() + reply.len() > Self::CAP {
            return false;
        }
        self.replies.extend(reply);
        true
    }

    fn push_input(&mut self, data: Vec<u8>) {
        if !data.is_empty() {
            self.input_len += data.len();
            self.input.push_back(data);
        }
    }

    fn takes_input(&self) -> bool {
        self.input_len < Self::INPUT_CAP
    }

    fn is_empty(&self) -> bool {
        self.replies.is_empty() && self.input.is_empty()
    }

    fn replying(&self) -> bool {
        self.written == 0 && !self.replies.is_empty()
    }

    /// The bytes to write next.
    fn next(&mut self) -> &[u8] {
        if self.replying() {
            self.replies.make_contiguous()
        } else {
            self.input.front().map_or(&[], |c| &c[self.written..])
        }
    }

    /// `n` bytes of [`Self::next`] were written.
    fn consume(&mut self, n: usize) {
        if self.replying() {
            self.replies.drain(..n);
            return;
        }
        self.written += n;
        self.input_len -= n;
        if self.input.front().is_some_and(|c| c.len() == self.written) {
            self.input.pop_front();
            self.written = 0;
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
    control: PtyTaskControl,
    graph: shux_core::graph::GraphHandle,
) {
    use base64::Engine;
    let PtyTaskControl {
        incarnation,
        mut write_rx,
        mut resize_rx,
        shutdown,
        done_tx,
        input,
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
    let mut pending = PtyInputQueue::default();

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
                        tee_pane_recorders(&io_state, pane_id, Some(incarnation), data, &shutdown).await;
                        let (pulse, vt_title, bus_opt, terminal_responses) = {
                            let mut state = io_state.lock().await;
                            if !state.is_current(pane_id, incarnation) {
                                // Replaced while this read was in flight: these
                                // bytes belong to a process the pane no longer has.
                                task_exit = PtyTaskExit::RequestedTeardown;
                                break;
                            }
                            let (vt_title, terminal_responses, rev_state, alt_switched, status) =
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
                                        Some(vt.program_status_publication()),
                                    )
                                } else {
                                    (None, Vec::new(), None, false, None)
                                };
                            if let (Some(status), Some(channel)) = (status, state.status.get(&pane_id)) {
                                channel.publish(status);
                            }
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

                        // An unbounded write to a program that never reads
                        // would stop this loop reading output or seeing a cancel.
                        for response in &terminal_responses {
                            if !pending.push(response) {
                                tracing::warn!(%pane_id, "program is not reading its terminal replies; dropping one");
                            }
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
                                // Re-checked: a replacement during this read
                                // must not leave the old program's title.
                                && io_state.lock().await.is_current(pane_id, incarnation)
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
            // Bounded: past the cap, senders wait instead of input being lost.
            res = write_rx.recv(), if pending.takes_input() => {
                let Some(data) = res else {
                    // Sender dropped -- the pane was destroyed.
                    // Exit so we can kill() the child shell.
                    tracing::debug!(%pane_id, "writer channel closed");
                    task_exit = PtyTaskExit::RequestedTeardown;
                    break;
                };
                pending.push_input(data);
            }
            // Waits for room, never blocks the loop: a program that stops
            // reading cannot stop this loop reading output or seeing a cancel.
            res = input.write_some(pending.next()), if !pending.is_empty() => {
                match res {
                    Ok(0) => {
                        tracing::error!(%pane_id, "PTY write took nothing");
                        break;
                    }
                    Ok(n) => pending.consume(n),
                    Err(e) => {
                        tracing::error!(%pane_id, error = %e, "PTY write error");
                        break;
                    }
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
                    let current = state.is_current(pane_id, incarnation);
                    let rev_state = if !current {
                        None
                    } else if let Some(vt) = state.vts.get_mut(&pane_id) {
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
                        Some(incarnation),
                        req.size.cols,
                        req.size.rows,
                    )
                    .await;
                }
            }
        }
    }

    finish_pane_recorders(&io_state, pane_id, Some(incarnation)).await;

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
    // The graph applies this only if `incarnation` still owns the pane.
    let exit_status = exit_code.unwrap_or(-1);
    // The process is gone: end its live records and send the final status
    // with the exit, so status_changed precedes exited. One lock hold decides
    // ownership and reads the terminal; a successor's records are not ours.
    let final_status = {
        let mut guard = io_state.lock().await;
        let state = &mut *guard;
        match state.vts.get_mut(&pane_id) {
            Some(vt) if state.incarnations.get(&pane_id) == Some(&incarnation) => {
                // Sealed in this lock hold: the config walker leaves the pane
                // alone, so nothing can overtake the final status. The writer
                // stays until the exit commits, or attach would read the pane
                // as never spawned and run its command again.
                state.status_sealed.insert(pane_id);
                vt.note_process_exit();
                let publication = vt.program_status_publication();
                let update = crate::program_status::update(&publication);
                if let Some(channel) = state.status.get(&pane_id) {
                    channel.publish(publication);
                }
                Some(update)
            }
            _ => None,
        }
    };
    if let Err(e) = graph
        .set_pane_exit_status_owned(pane_id, exit_status, incarnation, final_status)
        .await
    {
        tracing::debug!(%pane_id, error = %e, "set_pane_exit_status failed (pane may already be gone)");
    }

    // Drop only the PTY-bound handles. The VT (grid + scrollback) stays
    // until the pane is explicitly destroyed via pane.kill / window.kill
    // / session.kill — agents and humans alike need pane.capture and
    // pane.snapshot to keep working against the frozen output of a
    // short-lived command. The Pane's exit_status is the "dead" flag;
    // tmux does the same with its `remain-on-exit` model.
    let mut state = io_state.lock().await;
    if state.is_current(pane_id, incarnation) {
        state.writers.remove(&pane_id);
        state.resizers.remove(&pane_id);
        state.shutdowns.remove(&pane_id);
        state.pty_done.remove(&pane_id);
    }
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
    // One spawn per pane, decided before anything forks. A second spawn
    // waits for one in flight and reports its outcome, or finds the pane
    // already ran and leaves it.
    let ticket = {
        let mut in_flight = spawning();
        match in_flight.get(&pane_id) {
            Some(first) => Err(first.clone()),
            None => {
                let (tx, rx) = tokio::sync::watch::channel(None);
                in_flight.insert(pane_id, rx);
                Ok(SpawnTicket {
                    pane_id,
                    tx,
                    ok: false,
                })
            }
        }
    };
    let mut ticket = match ticket {
        Ok(ticket) => ticket,
        Err(mut first) => {
            let ok = matches!(
                first.wait_for(Option::is_some).await.as_deref(),
                Ok(Some(true))
            );
            return if ok {
                Ok(())
            } else {
                Err(pane_gone(
                    pane_id,
                    &"a concurrent spawn of this pane failed",
                ))
            };
        }
    };
    // Under the ticket, so every earlier spawn has installed. An incarnation
    // outlives the program's exit and goes only with the pane.
    if io_state.lock().await.incarnations.contains_key(&pane_id) {
        ticket.ok = true;
        return Ok(());
    }
    let spawned = spawn_pane_pty_unshared(
        pane_id,
        cwd,
        command,
        size,
        extra_env,
        env_clear,
        io_state.clone(),
        shutdown,
        graph,
        cast_recorder,
    )
    .await;
    ticket.ok = spawned.is_ok();
    spawned
}

type SpawnOutcome = tokio::sync::watch::Receiver<Option<bool>>;

/// Spawns in flight, by pane. Its own lock, never held across an await:
/// spawns must not queue behind the io lock to learn they are redundant.
fn spawning()
-> std::sync::MutexGuard<'static, std::collections::HashMap<shux_core::model::PaneId, SpawnOutcome>>
{
    static SPAWNING: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<shux_core::model::PaneId, SpawnOutcome>>,
    > = std::sync::LazyLock::new(Default::default);
    SPAWNING.lock().unwrap_or_else(|e| e.into_inner())
}

/// The in-flight mark for one spawn. Released however the spawn ends —
/// returned, cancelled or panicked — and publishes its outcome to waiters.
struct SpawnTicket {
    pane_id: shux_core::model::PaneId,
    tx: tokio::sync::watch::Sender<Option<bool>>,
    ok: bool,
}

impl Drop for SpawnTicket {
    fn drop(&mut self) {
        spawning().remove(&self.pane_id);
        let _ = self.tx.send(Some(self.ok));
    }
}

#[allow(clippy::too_many_arguments)]
async fn spawn_pane_pty_unshared(
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
    let input = handle.input_writer()?;
    let pid = handle.pid();

    let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>(256);
    let (resize_tx, resize_rx) = mpsc::channel::<ResizeRequest>(16);
    let (done_tx, done_rx) = oneshot::channel::<()>();
    let pane_shutdown = shutdown.child_token();
    let mut vt = shux_vt::VirtualTerminal::new(size.rows as usize, size.cols as usize);
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

    static NEXT_INCARNATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let incarnation = NEXT_INCARNATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut status_flush = None;
    // Before the claim: a claim clears an exited pane's exit and announces a
    // respawn. The check under the io lock below still covers a shutdown that
    // lands in between.
    if shutdown.is_cancelled() {
        discard_child(handle);
        return Err(pane_gone(pane_id, &"the daemon is shutting down"));
    }
    match graph.claim_pane_pty(pane_id, incarnation).await {
        Ok(true) => {}
        // A newer spawn owns the pane; it is the one that runs there.
        Ok(false) => {
            discard_child(handle);
            return Ok(());
        }
        Err(e) => {
            discard_child(handle);
            return Err(pane_gone(pane_id, &e));
        }
    }
    {
        let mut state = io_state.lock().await;
        // Daemon shutdown cancels this token before it collects the panes to
        // wait for, so an install after that would go unwaited.
        if shutdown.is_cancelled() {
            drop(state);
            discard_child(handle);
            return Err(pane_gone(pane_id, &"the daemon is shutting down"));
        }
        if state
            .incarnations
            .get(&pane_id)
            .is_some_and(|&installed| installed > incarnation)
        {
            drop(state);
            discard_child(handle);
            return Ok(());
        }
        if let Some(old) = state.shutdowns.insert(pane_id, pane_shutdown.clone()) {
            old.cancel();
        }
        vt.set_program_status_enabled(state.program_status_enabled);
        let status = vt.program_status_publication();
        let status_generation = status.generation;
        state.incarnations.insert(pane_id, incarnation);
        state.writers.insert(pane_id, write_tx);
        state.resizers.insert(pane_id, resize_tx);
        state.pty_done.insert(pane_id, done_rx);
        let replaced = state.vts.insert(pane_id, vt).is_some();
        state.status_sealed.remove(&pane_id);
        state.revisions.insert(pane_id, rev_tx);
        state.pty_pids.insert(pane_id, pid);
        // The slot outlives a respawn: the new terminal's newer generation
        // clears what the old program reported.
        match state.status.get(&pane_id) {
            Some(channel) => channel.publish(status),
            None => {
                let channel =
                    crate::program_status::StatusChannel::start(pane_id, status, graph.clone());
                state.status.insert(pane_id, channel);
            }
        }
        if replaced {
            status_flush = state
                .status
                .get(&pane_id)
                .map(|c| (c.flusher(), status_generation));
        }
        // Arm the cast recorder in the SAME lock, before the read task spawns, so no output can
        // race ahead of it (task 083).
        if let Some(rec) = cast_recorder {
            state.recorders.entry(pane_id).or_default().push(rec);
        }
    }

    // Best-effort: consumers see the old program's status cleared before the
    // new one can report.
    if let Some((flusher, generation)) = status_flush {
        flusher.flush(generation).await;
    }
    tokio::spawn(run_pane_pty_task(
        pane_id,
        handle,
        io_state.clone(),
        PtyTaskControl {
            incarnation,
            write_rx,
            resize_rx,
            shutdown: pane_shutdown,
            done_tx,
            input,
        },
        graph.clone(),
    ));

    // Destruction commits to the graph before it tears down IO: either the
    // pane is still here, or that teardown ran before the install above.
    if !graph.snapshot().panes.contains_key(&pane_id) {
        let mut state = io_state.lock().await;
        if state.is_current(pane_id, incarnation) {
            let (pulse, _done) = state.teardown_panes_collecting(&[pane_id], true);
            drop(state);
            pulse.notify_waiters();
        }
        return Err(pane_gone(
            pane_id,
            &"it was destroyed while its PTY started",
        ));
    }
    Ok(())
}

/// Kill and reap the child of a spawn that will not be installed.
fn discard_child(mut handle: shux_pty::handle::PtyHandle) {
    let _ = handle.kill();
    tokio::spawn(async move {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle.wait()).await;
    });
}

fn pane_gone(pane_id: shux_core::model::PaneId, why: &dyn std::fmt::Display) -> shux_pty::PtyError {
    shux_pty::PtyError::Spawn(std::io::Error::other(format!(
        "pane {pane_id} cannot take a PTY: {why}"
    )))
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

    /// A pane whose PTY is installed again while its first task still runs
    /// must end up owned by the second task. The first, cancelled, must
    /// neither mark the pane exited nor tear down its successor's handles.
    /// (`spawn_pane_pty` leaves a started pane alone; this drives the install.)
    #[tokio::test(flavor = "multi_thread")]
    async fn a_replaced_pty_task_leaves_its_successor_alone() {
        let h = crate::rpc::test_harness::RpcHarness::new();
        let (_s, _w, pane) = h.seed_session("respawn").await;
        let spawn = || {
            spawn_pane_pty_unshared(
                pane,
                "/tmp".into(),
                vec!["sleep".into(), "30".into()],
                shux_pty::handle::PtySize::default(),
                Vec::new(),
                false,
                h.io.clone(),
                h.cancel.clone(),
                h.graph.clone(),
                None,
            )
        };
        spawn().await.unwrap();
        let first_done = h.io.lock().await.pty_done.remove(&pane).unwrap();
        spawn().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), first_done)
            .await
            .expect("the replaced task finishes")
            .ok();

        assert_eq!(
            h.graph.snapshot().panes[&pane].exit_status,
            None,
            "the successor is still running"
        );
        let state = h.io.lock().await;
        assert!(
            state.writers.contains_key(&pane),
            "successor writer removed"
        );
        assert!(
            state.shutdowns.contains_key(&pane),
            "successor shutdown removed"
        );
        drop(state);
        h.stop().await;
    }

    /// A recorder the successor armed at spawn belongs to the successor; the
    /// replaced task's exit must not finish it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_replaced_pty_task_does_not_finish_its_successors_recorder() {
        let h = crate::rpc::test_harness::RpcHarness::new();
        let (_s, _w, pane) = h.seed_session("respawn-rec").await;
        let dir = tempfile::tempdir().unwrap();
        let spawn = |rec| {
            spawn_pane_pty_unshared(
                pane,
                "/tmp".into(),
                vec!["sleep".into(), "30".into()],
                shux_pty::handle::PtySize::default(),
                Vec::new(),
                false,
                h.io.clone(),
                h.cancel.clone(),
                h.graph.clone(),
                rec,
            )
        };
        spawn(None).await.unwrap();
        let first_done = h.io.lock().await.pty_done.remove(&pane).unwrap();
        let rec = crate::pane_record::open_cast_recorder(dir.path().join("s.cast"), 80, 24)
            .await
            .unwrap();
        let outcome = rec.outcome.clone();
        spawn(Some(rec)).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), first_done)
            .await
            .expect("the replaced task finishes")
            .ok();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(
            outcome.lock().unwrap().status,
            crate::pane_record::PaneRecordStatus::Recording,
            "the successor's recording was finished by its predecessor"
        );
        h.stop().await;
    }

    /// A pane killed while recording has no successor: its recording must
    /// finish, or a recorder without a duration waits forever.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_replaced_pty_task_torn_down_with_its_pane_finishes_its_recorder() {
        let h = crate::rpc::test_harness::RpcHarness::new();
        let (_s, _w, pane) = h.seed_session("kill-rec").await;
        let dir = tempfile::tempdir().unwrap();
        let rec = crate::pane_record::open_cast_recorder(dir.path().join("k.cast"), 80, 24)
            .await
            .unwrap();
        let outcome = rec.outcome.clone();
        spawn_pane_pty_with_recorder(
            pane,
            "/tmp".into(),
            vec!["sleep".into(), "30".into()],
            shux_pty::handle::PtySize::default(),
            Vec::new(),
            false,
            h.io.clone(),
            h.cancel.clone(),
            h.graph.clone(),
            Some(rec),
        )
        .await
        .unwrap();
        // What pane.kill does: tear the pane down, VT and all.
        let (_pulse, done) = h.io.lock().await.teardown_panes_collecting(&[pane], true);
        for rx in done {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), rx).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(
            outcome.lock().unwrap().status,
            crate::pane_record::PaneRecordStatus::Complete,
            "a recording on a killed pane was never finished"
        );
        h.stop().await;
    }

    /// What a program taking at most `limit` bytes per write receives.
    fn write(q: &mut PtyInputQueue, limit: usize) -> Vec<u8> {
        let mut sent = Vec::new();
        while !q.is_empty() && sent.len() < limit {
            let next = q.next();
            let n = next.len().min(limit - sent.len());
            sent.extend_from_slice(&next[..n]);
            q.consume(n);
        }
        sent
    }

    #[test]
    fn a_reply_the_program_only_partly_took_finishes_before_anything_else() {
        let mut q = PtyInputQueue::default();
        assert!(q.push(b"\x1b[?1;2c"));
        assert!(q.push(b"\x1b[0n"));
        let mut sent = write(&mut q, 4);
        assert_eq!(sent, b"\x1b[?1");
        sent.extend(write(&mut q, usize::MAX));
        assert_eq!(
            sent, b"\x1b[?1;2c\x1b[0n",
            "the tail went first, nothing was lost"
        );
        assert!(q.is_empty());
    }

    #[test]
    fn a_reply_waits_for_the_input_chunk_being_written() {
        let mut q = PtyInputQueue::default();
        q.push_input(b"hello".to_vec());
        q.push_input(b"world".to_vec());
        let mut sent = write(&mut q, 2);
        assert!(q.push(b"\x1b[0n"));
        sent.extend(write(&mut q, usize::MAX));
        assert_eq!(sent, b"hello\x1b[0nworld", "a reply landed inside a chunk");
    }

    #[test]
    fn input_is_never_dropped_only_held_back_past_the_cap() {
        let mut q = PtyInputQueue::default();
        q.push_input(vec![b'p'; PtyInputQueue::INPUT_CAP + 10]);
        assert!(!q.takes_input(), "input piled up past the cap");
        let sent = write(&mut q, 20);
        assert!(q.takes_input(), "room freed by the program reading");
        let rest = write(&mut q, usize::MAX);
        assert_eq!(sent.len() + rest.len(), PtyInputQueue::INPUT_CAP + 10);
    }

    #[test]
    fn a_reply_that_does_not_fit_is_dropped_whole() {
        let mut q = PtyInputQueue::default();
        let reply = vec![b'x'; 1000];
        let mut accepted = 0;
        while q.push(&reply) {
            accepted += 1;
        }
        assert_eq!(accepted, PtyInputQueue::CAP / reply.len());
        assert_eq!(
            q.replies.len(),
            accepted * reply.len(),
            "no partial reply queued"
        );
    }

    /// A pane id plus the pid file its shell writes, so a test can prove the
    /// child of a refused spawn did not survive.
    fn pid_probe(dir: &std::path::Path) -> (std::path::PathBuf, Vec<String>) {
        let pid_file = dir.join("pid");
        let cmd = format!("echo $$ > '{}'; exec sleep 30", pid_file.display());
        (pid_file, vec!["sh".into(), "-c".into(), cmd])
    }

    /// The child either never got far enough to write its pid, or is reaped.
    async fn assert_child_gone(pid_file: &std::path::Path) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let alive = std::fs::read_to_string(pid_file)
                .ok()
                .and_then(|s| s.trim().parse::<i32>().ok())
                .is_some_and(|pid| {
                    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
                });
            if !alive && (pid_file.exists() || tokio::time::Instant::now() >= deadline) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the refused spawn's child is still running"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    async fn spawn_probe(
        h: &crate::rpc::test_harness::RpcHarness,
        pane: shux_core::model::PaneId,
        argv: Vec<String>,
    ) -> Result<(), shux_pty::PtyError> {
        spawn_pane_pty(
            pane,
            "/tmp".into(),
            argv,
            shux_pty::handle::PtySize::default(),
            Vec::new(),
            false,
            h.io.clone(),
            h.cancel.clone(),
            h.graph.clone(),
        )
        .await
    }

    /// A newer spawn already claimed the pane: the older one must not take
    /// it back, install its handles, or leave its child running.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_replaced_pty_task_that_lost_the_claim_installs_nothing() {
        let h = crate::rpc::test_harness::RpcHarness::new();
        let (_s, _w, pane) = h.seed_session("lost-claim").await;
        let dir = tempfile::tempdir().unwrap();
        let (pid_file, argv) = pid_probe(dir.path());
        let _ = h.graph.claim_pane_pty(pane, u64::MAX).await;
        spawn_probe(&h, pane, argv).await.unwrap();
        assert!(
            !h.io.lock().await.writers.contains_key(&pane),
            "a spawn older than the pane's owner installed its PTY"
        );
        assert_child_gone(&pid_file).await;
        h.stop().await;
    }

    /// Claims and installs are separate steps, so a newer spawn can install
    /// first. The older one must then leave the pane to it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_replaced_pty_task_older_than_the_installed_one_installs_nothing() {
        let h = crate::rpc::test_harness::RpcHarness::new();
        let (_s, _w, pane) = h.seed_session("lost-install").await;
        let dir = tempfile::tempdir().unwrap();
        let (pid_file, argv) = pid_probe(dir.path());
        h.io.lock().await.incarnations.insert(pane, u64::MAX);
        spawn_probe(&h, pane, argv).await.unwrap();
        let state = h.io.lock().await;
        assert_eq!(state.incarnations.get(&pane), Some(&u64::MAX));
        assert!(
            !state.writers.contains_key(&pane),
            "an older spawn replaced the newer installed PTY"
        );
        drop(state);
        assert_child_gone(&pid_file).await;
        h.stop().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_replaced_pty_task_for_a_missing_pane_fails() {
        let h = crate::rpc::test_harness::RpcHarness::new();
        let dir = tempfile::tempdir().unwrap();
        let (pid_file, argv) = pid_probe(dir.path());
        let ghost = shux_core::model::PaneId::new();
        assert!(
            spawn_probe(&h, ghost, argv).await.is_err(),
            "a spawn for a pane that does not exist reported success"
        );
        assert!(!h.io.lock().await.writers.contains_key(&ghost));
        assert_child_gone(&pid_file).await;
        h.stop().await;
    }

    /// A spawn refused because the daemon is stopping must not claim the pane
    /// first: a claim clears an exited pane's exit and announces a respawn
    /// that never happens.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_replaced_pty_task_refused_at_shutdown_leaves_the_exit_alone() {
        let h = crate::rpc::test_harness::RpcHarness::new();
        let (_s, _w, pane) = h.seed_session("refused-at-shutdown").await;
        let dir = tempfile::tempdir().unwrap();
        let (pid_file, argv) = pid_probe(dir.path());
        h.graph.set_pane_exit_status(pane, 0).await.unwrap();
        let stopping = tokio_util::sync::CancellationToken::new();
        stopping.cancel();
        let spawned = spawn_pane_pty(
            pane,
            "/tmp".into(),
            argv,
            shux_pty::handle::PtySize::default(),
            Vec::new(),
            false,
            h.io.clone(),
            stopping,
            h.graph.clone(),
        )
        .await;
        assert!(spawned.is_err(), "a spawn during shutdown reported success");
        assert_eq!(h.graph.snapshot().panes[&pane].exit_status, Some(0));
        let respawned = h.bus.history(50).into_iter().any(|e| {
            matches!(e.data, shux_core::event::EventData::PaneRespawned { pane_id, .. } if pane_id == pane)
        });
        assert!(!respawned, "announced a respawn that never happened");
        assert_child_gone(&pid_file).await;
        h.stop().await;
    }

    /// The pane is destroyed after the claim but before the install, and its
    /// IO teardown has already run: the spawn must clean up after itself.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_replaced_pty_task_whose_pane_died_mid_spawn_cleans_up() {
        let h = crate::rpc::test_harness::RpcHarness::new();
        let (session, _w, pane) = h.seed_session("died-mid-spawn").await;
        let dir = tempfile::tempdir().unwrap();
        let (pid_file, argv) = pid_probe(dir.path());
        // An exited pane makes the claim observable: it clears the exit.
        h.graph.set_pane_exit_status(pane, 0).await.unwrap();
        let guard = h.io.lock().await;
        let task = {
            let io = h.io.clone();
            let cancel = h.cancel.clone();
            let graph = h.graph.clone();
            tokio::spawn(async move {
                spawn_pane_pty_unshared(
                    pane,
                    "/tmp".into(),
                    argv,
                    shux_pty::handle::PtySize::default(),
                    Vec::new(),
                    false,
                    io,
                    cancel,
                    graph,
                    None,
                )
                .await
            })
        };
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while h.graph.snapshot().panes[&pane].exit_status.is_some() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "spawn never claimed"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        h.graph.destroy_session(session, None).await.unwrap();
        drop(guard);
        let result = task.await.unwrap();
        assert!(
            result.is_err(),
            "spawn for a destroyed pane reported success"
        );
        let state = h.io.lock().await;
        assert!(
            !state.writers.contains_key(&pane)
                && !state.vts.contains_key(&pane)
                && !state.incarnations.contains_key(&pane),
            "a destroyed pane kept its PTY handles"
        );
        drop(state);
        assert_child_gone(&pid_file).await;
        h.stop().await;
    }
}
