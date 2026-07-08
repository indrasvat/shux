use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use clap::{CommandFactory, FromArgMatches};
use shux_rpc::{Policy, Sensitivity};
use tokio::sync::{Mutex, Notify, mpsc, oneshot, watch};
use tracing_subscriber::EnvFilter;

mod attach;
mod cli;
mod client;
mod config_validate;
mod daemon;
mod features;
mod onboarding;
mod session_meta;
mod session_persist;
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

const PANE_RECORD_CHANNEL_CAPACITY: usize = 128;
const PANE_RECORD_COMPLETED_TTL: Duration = Duration::from_secs(60);

#[derive(Debug)]
enum PaneRecordChunk {
    Bytes(Vec<u8>),
    Finish { status: PaneRecordStatus },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PaneRecordStatus {
    Recording,
    Complete,
    Error,
    Aborted,
}

impl PaneRecordStatus {
    fn as_str(self) -> &'static str {
        match self {
            PaneRecordStatus::Recording => "recording",
            PaneRecordStatus::Complete => "complete",
            PaneRecordStatus::Error => "error",
            PaneRecordStatus::Aborted => "aborted",
        }
    }
}

#[derive(Clone, Debug)]
struct PaneRecordResult {
    status: PaneRecordStatus,
    bytes_written: u64,
    error: Option<String>,
}

struct PaneRecorder {
    id: uuid::Uuid,
    path: PathBuf,
    sender: mpsc::Sender<PaneRecordChunk>,
    outcome: Arc<StdMutex<PaneRecordResult>>,
    task: tokio::task::JoinHandle<()>,
}

/// Lens ContentRevision publication payload (PRD §4, LENS-R-003). Published on
/// a per-pane `tokio::sync::watch` channel once per Class-A batch so late
/// subscribers (`pane.wait_settled`, P3) always read the current value — no
/// lost-edge races (a `watch`, deliberately NOT a `Notify`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneRevision {
    pub content_revision: u64,
    pub last_mutation_ns: u64,
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
    /// Per-pane cancellation tokens. These are child tokens of the daemon
    /// shutdown token, so daemon shutdown still cancels every pane, while
    /// explicit pane/window/session kills can target only the affected panes.
    pub shutdowns: HashMap<shux_core::model::PaneId, tokio_util::sync::CancellationToken>,
    /// Per-pane completion receivers used by daemon shutdown to wait until
    /// PTY tasks have actually signalled and reaped their children.
    pub pty_done: HashMap<shux_core::model::PaneId, oneshot::Receiver<()>>,
    /// Completion waiters for PTY tasks that were already explicitly torn
    /// down by pane/window/session kill. Daemon shutdown drains these too so
    /// it cannot exit while an earlier teardown is still in its reap/escalate
    /// path.
    pub teardown_waiters: Vec<tokio::task::JoinHandle<()>>,
    /// Per-pane VirtualTerminal instances for capturing output.
    pub vts: HashMap<shux_core::model::PaneId, shux_vt::VirtualTerminal>,
    /// Per-pane lens ContentRevision publishers (PRD §4, LENS-R-003). The
    /// single-writer PTY task publishes `(content_revision, last_mutation_ns)`
    /// here once per Class-A batch; `pane.wait_settled` (P3) subscribes. Same
    /// lifetime as `vts` (created with the pane, removed only on destroy).
    pub revisions: HashMap<shux_core::model::PaneId, watch::Sender<PaneRevision>>,
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
    /// Lossless pane-output recorders, keyed by pane. The PTY read task
    /// awaits these sends before sampled publishing, so this path is
    /// byte-exact and intentionally applies backpressure.
    recorders: HashMap<shux_core::model::PaneId, Vec<PaneRecorder>>,
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
            shutdowns: HashMap::new(),
            pty_done: HashMap::new(),
            teardown_waiters: Vec::new(),
            vts: HashMap::new(),
            revisions: HashMap::new(),
            cmd_engine: shux_pty::CommandEngine::new(),
            render_pulse: Arc::new(tokio::sync::Notify::new()),
            event_bus: None,
            recorders: HashMap::new(),
        }
    }

    pub fn with_event_bus(mut self, bus: shux_core::bus::EventBus) -> Self {
        self.event_bus = Some(bus);
        self
    }

    pub fn teardown_panes(
        &mut self,
        pane_ids: &[shux_core::model::PaneId],
        remove_vts: bool,
    ) -> Arc<Notify> {
        let (pulse, done) = self.teardown_panes_collecting(pane_ids, remove_vts);
        self.track_teardown_waiters(done);
        pulse
    }

    fn track_teardown_waiters(&mut self, done: Vec<oneshot::Receiver<()>>) {
        self.teardown_waiters.retain(|waiter| !waiter.is_finished());
        self.teardown_waiters.extend(done.into_iter().map(|rx| {
            tokio::spawn(async move {
                let _ = rx.await;
            })
        }));
    }

    pub fn teardown_panes_collecting(
        &mut self,
        pane_ids: &[shux_core::model::PaneId],
        remove_vts: bool,
    ) -> (Arc<Notify>, Vec<oneshot::Receiver<()>>) {
        let mut done = Vec::new();
        for pane_id in pane_ids {
            if let Some(token) = self.shutdowns.remove(pane_id) {
                token.cancel();
            }
            self.writers.remove(pane_id);
            self.resizers.remove(pane_id);
            if let Some(rx) = self.pty_done.remove(pane_id) {
                done.push(rx);
            }
            if remove_vts {
                self.vts.remove(pane_id);
                // The revision publisher has the same lifetime as the VT;
                // dropping the sender closes the watch for any settle waiter.
                self.revisions.remove(pane_id);
            }
        }
        (self.render_pulse.clone(), done)
    }

    /// Publish a pane's lens ContentRevision on its watch channel, but only when
    /// `content_revision` advanced (LENS-R-003: once per Class-A batch; Class-B
    /// no-op batches leave the value — and settle waiters — untouched). No-op
    /// when the pane has no publisher (e.g. test-only VT inserts).
    fn publish_revision(&self, pane_id: shux_core::model::PaneId, rev: PaneRevision) {
        if let Some(tx) = self.revisions.get(&pane_id) {
            tx.send_if_modified(|cur| {
                if cur.content_revision != rev.content_revision {
                    *cur = rev;
                    true
                } else {
                    false
                }
            });
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PtyTaskExit {
    Natural,
    RequestedTeardown,
}

async fn spawn_pane_recorder(
    path: PathBuf,
    overwrite: bool,
) -> Result<
    (
        mpsc::Sender<PaneRecordChunk>,
        Arc<StdMutex<PaneRecordResult>>,
        tokio::task::JoinHandle<()>,
    ),
    String,
> {
    use tokio::fs::OpenOptions;
    use tokio::io::AsyncWriteExt;

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            format!(
                "failed to create parent directory {}: {e}",
                parent.display()
            )
        })?;
    }

    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        options.custom_flags(nix::libc::O_NOFOLLOW);
    }
    if overwrite {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    let file = options
        .open(&path)
        .await
        .map_err(|e| format!("failed to open record file {}: {e}", path.display()))?;

    let (tx, mut rx) = mpsc::channel::<PaneRecordChunk>(PANE_RECORD_CHANNEL_CAPACITY);
    let outcome = Arc::new(StdMutex::new(PaneRecordResult {
        status: PaneRecordStatus::Recording,
        bytes_written: 0,
        error: None,
    }));
    let writer_outcome = outcome.clone();
    let task = tokio::spawn(async move {
        let mut file = file;
        let mut bytes_written = 0u64;
        let mut final_status = PaneRecordStatus::Complete;
        while let Some(chunk) = rx.recv().await {
            match chunk {
                PaneRecordChunk::Bytes(bytes) => {
                    if let Err(e) = file.write_all(&bytes).await {
                        let mut outcome = writer_outcome.lock().expect("record outcome poisoned");
                        outcome.status = PaneRecordStatus::Error;
                        outcome.bytes_written = bytes_written;
                        outcome.error = Some(format!("failed to write record chunk: {e}"));
                        return;
                    }
                    bytes_written += bytes.len() as u64;
                }
                PaneRecordChunk::Finish { status } => {
                    final_status = status;
                    break;
                }
            }
        }
        let flush_error = file.flush().await.err();
        let mut outcome = writer_outcome.lock().expect("record outcome poisoned");
        outcome.bytes_written = bytes_written;
        if let Some(e) = flush_error {
            outcome.status = PaneRecordStatus::Error;
            outcome.error = Some(format!("failed to flush record file: {e}"));
        } else if outcome.status == PaneRecordStatus::Recording {
            outcome.status = final_status;
        }
    });

    Ok((tx, outcome, task))
}

async fn tee_pane_recorders(
    io_state: &Arc<Mutex<PaneIoState>>,
    pane_id: shux_core::model::PaneId,
    data: &[u8],
    shutdown: &tokio_util::sync::CancellationToken,
) {
    let sinks: Vec<(
        mpsc::Sender<PaneRecordChunk>,
        Arc<StdMutex<PaneRecordResult>>,
    )> = {
        let state = io_state.lock().await;
        state
            .recorders
            .get(&pane_id)
            .map(|recorders| {
                recorders
                    .iter()
                    .filter(|r| {
                        r.outcome.lock().expect("record outcome poisoned").status
                            == PaneRecordStatus::Recording
                    })
                    .map(|r| (r.sender.clone(), r.outcome.clone()))
                    .collect()
            })
            .unwrap_or_default()
    };

    for (sender, outcome) in sinks {
        tokio::select! {
            result = sender.send(PaneRecordChunk::Bytes(data.to_vec())) => {
                if result.is_err() {
                    let mut outcome = outcome.lock().expect("record outcome poisoned");
                    if outcome.status == PaneRecordStatus::Recording {
                        outcome.status = PaneRecordStatus::Error;
                        outcome.error = Some("pane recorder writer closed before accepting bytes".to_string());
                    }
                }
            }
            _ = shutdown.cancelled() => {
                let mut outcome = outcome.lock().expect("record outcome poisoned");
                if outcome.status == PaneRecordStatus::Recording {
                    outcome.status = PaneRecordStatus::Aborted;
                    outcome.error = Some("pane shutdown interrupted recorder backpressure".to_string());
                }
                return;
            }
        }
    }
}

async fn finish_pane_recorders(
    io_state: &Arc<Mutex<PaneIoState>>,
    pane_id: shux_core::model::PaneId,
) {
    let senders: Vec<mpsc::Sender<PaneRecordChunk>> = {
        let state = io_state.lock().await;
        state
            .recorders
            .get(&pane_id)
            .map(|recorders| recorders.iter().map(|r| r.sender.clone()).collect())
            .unwrap_or_default()
    };

    for sender in senders {
        let _ = sender
            .send(PaneRecordChunk::Finish {
                status: PaneRecordStatus::Complete,
            })
            .await;
    }
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
                            let (vt_title, terminal_responses, rev_state) =
                                if let Some(vt) = state.vts.get_mut(&pane_id) {
                                    let responses = vt.process_with_responses(data);
                                    let rev = PaneRevision {
                                        content_revision: vt.content_revision(),
                                        last_mutation_ns: vt.last_mutation_ns(),
                                    };
                                    (vt.title().map(|s| s.to_string()), responses, Some(rev))
                                } else {
                                    (None, Vec::new(), None)
                                };
                            // LENS-R-003: publish in the same critical section as
                            // the grid mutation, once per Class-A batch.
                            if let Some(rev) = rev_state {
                                state.publish_revision(pane_id, rev);
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
                        if !terminal_responses.is_empty() {
                            if let Err(e) = handle.flush().await {
                                tracing::error!(%pane_id, error = %e, "PTY terminal response flush error");
                                break;
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
                let pulse = {
                    let mut state = io_state.lock().await;
                    let rev_state = if let Some(vt) = state.vts.get_mut(&pane_id) {
                        vt.resize(req.size.rows as usize, req.size.cols as usize);
                        Some(PaneRevision {
                            content_revision: vt.content_revision(),
                            last_mutation_ns: vt.last_mutation_ns(),
                        })
                    } else {
                        None
                    };
                    // LENS-R-003: resize is Class-A — publish the bumped revision.
                    if let Some(rev) = rev_state {
                        state.publish_revision(pane_id, rev);
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
    state.shutdowns.remove(&pane_id);
    state.pty_done.remove(&pane_id);
    let pulse = state.render_pulse.clone();
    drop(state);
    pulse.notify_one();
    let _ = done_tx.send(());
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
    let (done_tx, done_rx) = oneshot::channel::<()>();
    let pane_shutdown = shutdown.child_token();
    let vt = shux_vt::VirtualTerminal::new(24, 80);
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
        let io_state = run_rpc_server(sock_path, cancel.clone()).await?;

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
                    let expected_name = params
                        .get("expected_name")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    let expected_version = params
                        .get("expected_version")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);

                    let source = shux_plugin::PluginSource {
                        path: PathBuf::from(path),
                        args,
                        cwd,
                        watch,
                        state_root,
                        expected_name,
                        expected_version,
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
                    let pulse = state.teardown_panes(&[pane_id], true);
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

/// Extract optional initial pane title from session.create/session.ensure params.
fn parse_initial_pane_title(
    params: &serde_json::Value,
) -> Result<Option<String>, shux_rpc::RpcError> {
    match params.get("pane_title") {
        None => Ok(None),
        Some(v) if v.is_null() => Ok(None),
        Some(v) => {
            let title = v.as_str().ok_or_else(|| {
                shux_rpc::RpcError::invalid_params("'pane_title' must be a string")
            })?;
            if title.trim().is_empty() {
                return Err(shux_rpc::RpcError::invalid_params(
                    "'pane_title' must not be empty",
                ));
            }
            Ok(Some(title.to_string()))
        }
    }
}

fn initial_pane_id_for_session(
    snap: &shux_core::graph::SessionGraphSnapshot,
    session_id: shux_core::model::SessionId,
) -> Result<shux_core::model::PaneId, shux_rpc::RpcError> {
    let session = snap
        .sessions
        .get(&session_id)
        .ok_or_else(|| shux_rpc::RpcError::internal("session vanished after create"))?;
    let window_id = session
        .windows
        .first()
        .ok_or_else(|| shux_rpc::RpcError::internal("created session has no windows"))?;
    let window = snap
        .windows
        .get(window_id)
        .ok_or_else(|| shux_rpc::RpcError::internal("initial window vanished after create"))?;

    window
        .layout
        .tree
        .pane_ids()
        .into_iter()
        .next()
        .ok_or_else(|| shux_rpc::RpcError::internal("initial window has no panes"))
}

async fn set_initial_pane_title(
    gh: &shux_core::graph::GraphHandle,
    session_id: shux_core::model::SessionId,
    title: Option<String>,
) -> Result<(), shux_rpc::RpcError> {
    let Some(title) = title else {
        return Ok(());
    };

    let pane_id = {
        let snap = gh.snapshot();
        initial_pane_id_for_session(&snap, session_id)?
    };

    gh.set_pane_title(pane_id, Some(title), None)
        .await
        .map_err(graph_error_to_rpc)
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
    // Bridge the cold-start race the attach path doesn't suffer from:
    // when a snapshot fires right after daemon start or a config reload,
    // the runner tasks may not have completed their first tick yet, so
    // `populate_bar` would read an empty cache and silently emit no
    // segments. Wait up to 1.2s for every configured segment index to
    // have a cache entry; on timeout we proceed anyway so a slow / hung
    // command can't wedge the RPC. The 1.2s budget slightly exceeds the
    // runner's per-command 1s timeout so the runner's fallback-bytes
    // write has room to land before we give up (codex round-4 nit).
    // Codex-bot P2, PR #45.
    let segment_count = live_cfg.statusbar.segment.len();
    if segment_count > 0 {
        let _ = segments
            .wait_for_first_outputs(segment_count, std::time::Duration::from_millis(1200))
            .await;
    }
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

    // Snapshot just the (Grid, Cursor, dynamic colors) per pane under the io
    // lock — VT itself isn't Clone and we want to release the lock before
    // rasterizing.
    let pane_data: Vec<(
        shux_core::model::PaneId,
        shux_vt::Grid,
        shux_vt::Cursor,
        shux_vt::TerminalDefaultColors,
    )> = {
        let state = io.lock().await;
        window
            .layout
            .tree
            .pane_ids()
            .into_iter()
            .filter_map(|pid| {
                state.vts.get(&pid).map(|vt| {
                    let mut grid = vt.grid().clone();
                    let default_colors = vt.default_colors();
                    resolve_grid_default_colors(&mut grid, default_colors);
                    (pid, grid, vt.cursor().clone(), default_colors)
                })
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
        > = pane_data.iter().map(|(p, g, c, _)| (*p, (g, c))).collect();
        let focused_defaults = pane_data
            .iter()
            .find(|(pid, _, _, _)| *pid == focused)
            .map(|(_, _, _, defaults)| *defaults)
            .unwrap_or_default();
        let focused_cursor_shape = pane_data
            .iter()
            .find(|(pid, _, _, _)| *pid == focused)
            .map(|(_, _, cursor, _)| cursor.shape)
            .unwrap_or_default();
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
            cursor_shape: focused_cursor_shape,
            cursor_color: focused_defaults.cursor,
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

fn resolve_grid_default_colors(grid: &mut shux_vt::Grid, defaults: shux_vt::TerminalDefaultColors) {
    if defaults.fg.is_none() && defaults.bg.is_none() {
        return;
    }
    for row_idx in 0..grid.rows() {
        let mut row = grid.visible_row_mut(row_idx);
        for col_idx in 0..row.len() {
            let cell = &mut row[col_idx];
            if cell.style.fg == shux_vt::Color::Default
                && let Some([r, g, b]) = defaults.fg
            {
                cell.style.fg = shux_vt::Color::Rgb(r, g, b);
            }
            if cell.style.bg == shux_vt::Color::Default
                && let Some([r, g, b]) = defaults.bg
            {
                cell.style.bg = shux_vt::Color::Rgb(r, g, b);
            }
        }
    }
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
                        let pulse = state.teardown_panes(&pane_ids, true);
                        drop(state);
                        pulse.notify_one();
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
    let g6 = graph.clone();

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
                    let pane_title = parse_initial_pane_title(&params)?;

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
                            set_initial_pane_title(&gh, session_id, pane_title).await?;

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

                    // Tear down every pane that belonged to the session.
                    // The explicit per-pane shutdown token is the hard
                    // lifecycle contract; writer/resizer removal is only
                    // bookkeeping. The PTY task prioritizes cancellation
                    // and signals the pane's process group before reaping,
                    // so rich TUIs do not survive as unreachable children.
                    {
                        let mut state = io.lock().await;
                        let pulse = state.teardown_panes(&pane_ids, true);
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
                    let pane_title = parse_initial_pane_title(&params)?;

                    // Check if session already exists
                    let snap = gh.snapshot();
                    if let Some(s) = snap.find_session_by_name(&name) {
                        let mut json = session_to_json(s, &snap);
                        json["created"] = serde_json::Value::Bool(false);
                        return Ok(json);
                    }

                    let cwd = params
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .map(PathBuf::from)
                        .unwrap_or_else(|| {
                            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"))
                        });

                    match gh
                        .create_session_with_command(name, cwd.clone(), command.clone())
                        .await
                    {
                        Ok(session_id) => {
                            set_initial_pane_title(&gh, session_id, pane_title).await?;

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
            "session.export_template",
            Policy::fixed(Sensitivity::Public),
            move |params: Option<serde_json::Value>| {
                let gh = g6.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let snap = gh.snapshot();
                    let session_id = if let Some(id) = params.get("id").and_then(|v| v.as_str()) {
                        id.parse::<shux_core::model::SessionId>()
                            .map_err(|_| shux_rpc::RpcError::invalid_params("invalid session id"))?
                    } else if let Some(name) = params.get("name").and_then(|v| v.as_str()) {
                        snap.find_session_by_name(name)
                            .ok_or_else(|| shux_rpc::RpcError::not_found("session", name))?
                            .id
                    } else {
                        return Err(shux_rpc::RpcError::invalid_params(
                            "missing 'name' or 'id' parameter",
                        ));
                    };
                    let template = session_persist::export_session_template(&snap, session_id)
                        .map_err(|e| shux_rpc::RpcError::internal(&format!("{e}")))?;
                    Ok(serde_json::json!({ "template": template }))
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

enum SnapshotFontBytes {
    Static(&'static [u8]),
    Owned(Vec<u8>),
}

impl SnapshotFontBytes {
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Static(bytes) => bytes,
            Self::Owned(bytes) => bytes,
        }
    }
}

/// Build the snapshot rasterizer from current config.
///
/// - `appearance.font` unset → bundled JBM-NF primary + default fallback chain.
/// - `appearance.font` set + file readable + font parseable → that
///   font as primary, then configured/default fallbacks.
/// - `appearance.font` set BUT unreadable or unparseable → returns
///   `Err`. The hot-reload caller's `Err` branch keeps the last-good
///   rasterizer; the startup caller logs the error and falls back to the
///   bundled chain so snapshot RPCs still return PNGs.
/// - `appearance.font_fallbacks` omitted → default builtin fallback
///   tokens. Set explicitly → exact ordered fallback chain after the
///   primary font. Empty lists are rejected. When `appearance.font` is
///   unset, the bundled JBM-NF font remains the primary metrics anchor
///   and the explicit list is used strictly as glyph fallback coverage.
///
/// Council review (PR #46): the previous behaviour silently fell back
/// to the bundled chain on bad custom-font paths, contradicting the
/// "keep last good rasterizer" comment in the hot-reload spawn and
/// making the `Err` branch of the reload loop unreachable for the
/// most common failure mode.
fn build_snapshot_rasterizer(
    cfg: &shux_core::config::Config,
) -> Result<shux_raster::Rasterizer, shux_raster::RasterError> {
    let primary = match cfg.appearance.font.as_ref() {
        None => None,
        Some(path) => Some(std::fs::read(path).map_err(|e| {
            shux_raster::RasterError::Font(format!(
                "appearance.font: read {} failed: {e}",
                path.display()
            ))
        })?),
    };
    let explicit_fallback_specs = cfg.appearance.font_fallbacks.clone();
    if explicit_fallback_specs.as_ref().is_some_and(Vec::is_empty) {
        return Err(shux_raster::RasterError::Font(
            "appearance.font_fallbacks must not be empty; omit it to use the default fallback chain"
                .into(),
        ));
    }
    let mut fallback_specs = explicit_fallback_specs.unwrap_or_else(|| {
        shux_raster::DEFAULT_FALLBACK_FONT_SPECS
            .iter()
            .map(|spec| (*spec).to_string())
            .collect()
    });
    let mut bundled_primary = None;
    if primary.is_none() {
        bundled_primary = Some(
            shux_raster::builtin_font_bytes(shux_raster::BUILTIN_NERD_FONT)
                .expect("builtin nerd font token should resolve"),
        );
        if fallback_specs
            .first()
            .is_some_and(|spec| spec == shux_raster::BUILTIN_NERD_FONT)
        {
            fallback_specs.remove(0);
        }
    }
    let fallback_fonts: Vec<SnapshotFontBytes> = fallback_specs
        .iter()
        .map(|spec| {
            if let Some(bytes) = shux_raster::builtin_font_bytes(spec) {
                Ok(SnapshotFontBytes::Static(bytes))
            } else if spec.starts_with("builtin:") {
                Err(shux_raster::RasterError::Font(format!(
                    "appearance.font_fallbacks: unknown builtin font token {spec:?}; expected one of {}",
                    shux_raster::DEFAULT_FALLBACK_FONT_SPECS.join(", ")
                )))
            } else {
                std::fs::read(spec)
                    .map(SnapshotFontBytes::Owned)
                    .map_err(|e| {
                        shux_raster::RasterError::Font(format!(
                            "appearance.font_fallbacks: read {spec} failed: {e}"
                        ))
                    })
            }
        })
        .collect::<Result<_, _>>()?;

    let fallback_refs = fallback_fonts.iter().map(SnapshotFontBytes::as_slice);
    let primary_ref = primary.as_deref().or(bundled_primary);
    shux_raster::Rasterizer::with_primary_and_fallback_fonts(14.0, primary_ref, fallback_refs)
}

/// Cheap equality-key for the subset of config that affects the
/// rasterizer chain. Returning the same value across two config
/// reloads means we can skip the rebuild — most reloads (border
/// styles, theme tweaks, statusbar segments) don't touch fonts.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SnapshotFontKey {
    primary: Option<std::path::PathBuf>,
    fallbacks: Option<Vec<String>>,
}

fn snapshot_font_key(cfg: &shux_core::config::Config) -> SnapshotFontKey {
    SnapshotFontKey {
        primary: cfg.appearance.font.clone(),
        fallbacks: cfg.appearance.font_fallbacks.clone(),
    }
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
    let g10 = graph.clone();
    let g11 = graph;

    let io1 = io_state.clone();
    let io2 = io_state.clone();
    let io3 = io_state.clone();
    let io4 = io_state.clone();
    let io5 = io_state.clone();
    let io6 = io_state.clone();
    let io7 = io_state.clone();
    let io8 = io_state.clone();
    let io9 = io_state.clone();
    let io10 = io_state.clone();
    let io11 = io_state.clone();
    let io12 = io_state;

    // Shared rasterizer for `pane.snapshot` / `window.snapshot` / `session.snapshot`.
    // Wrapped in an `ArcSwap` so the snapshot handlers can pick up
    // `appearance.font` changes via the existing config hot-reload
    // signal without a daemon restart. On reload failure (bad font
    // path, corrupt file) the last-good rasterizer is kept and the
    // error logged — snapshots never produce blank PNGs because of a
    // misconfiguration. PR #46.
    //
    // Race-window note (council review, PR #46): we capture the
    // build-time config snapshot ONCE here and pass it INTO the reload
    // task. The task starts from that exact same snapshot's font key
    // and re-checks the current config before entering its `notified`
    // loop. This closes the TOCTOU between (a) the initial build and
    // (b) the spawned task starting to await — without it, a config
    // change in that gap would be silently lost because
    // `ConfigHandle::replace` uses `notify_waiters()` which only wakes
    // tasks ALREADY parked on `notified()`.
    let build_snap = config.current();
    let initial_font_key = snapshot_font_key(&build_snap);
    let rasterizer: Arc<arc_swap::ArcSwap<shux_raster::Rasterizer>> =
        Arc::new(arc_swap::ArcSwap::from(Arc::new(
            build_snapshot_rasterizer(&build_snap).unwrap_or_else(|e| {
                tracing::warn!(
                    error = %e,
                    "appearance.font invalid at startup, falling back to bundled chain"
                );
                shux_raster::Rasterizer::new(14.0)
                    .expect("shux-raster: bundled font corrupt — should be unreachable")
            }),
        )));
    {
        let raster_handle = rasterizer.clone();
        let config_for_reload = config.clone();
        let notify = config_for_reload.change_notify();
        tokio::spawn(async move {
            let mut last_font_key = initial_font_key;
            // Catch any change that landed between the initial build
            // and this task taking its first scheduling slot. Without
            // this, the racing change is silently swallowed and the
            // user sees a stale rasterizer until they edit the config
            // again. Council review (PR #46).
            let bootstrap_snap = config_for_reload.current();
            let bootstrap_key = snapshot_font_key(&bootstrap_snap);
            if bootstrap_key != last_font_key {
                match build_snapshot_rasterizer(&bootstrap_snap) {
                    Ok(new_raster) => {
                        raster_handle.store(Arc::new(new_raster));
                        last_font_key = bootstrap_key;
                        tracing::info!(
                            "snapshot rasterizer caught a config change \
                             that raced the daemon startup"
                        );
                    }
                    Err(e) => tracing::warn!(
                        error = %e,
                        "snapshot rasterizer bootstrap reload failed; keeping initial"
                    ),
                }
            }
            loop {
                notify.notified().await;
                let cfg_snap = config_for_reload.current();
                let new_key = snapshot_font_key(&cfg_snap);
                if new_key == last_font_key {
                    continue;
                }
                match build_snapshot_rasterizer(&cfg_snap) {
                    Ok(new_raster) => {
                        raster_handle.store(Arc::new(new_raster));
                        last_font_key = new_key;
                        tracing::info!("snapshot rasterizer rebuilt after config change");
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "snapshot rasterizer rebuild failed; keeping last good"
                        );
                    }
                }
            }
        });
    }
    let rasterizer_pane = rasterizer.clone();
    let rasterizer_window = rasterizer.clone();
    let rasterizer_session = rasterizer.clone();

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
            "pane.record.start",
            Policy::fixed(Sensitivity::PluginsForbidden),
            move |params: Option<serde_json::Value>| {
                let gh = g11.clone();
                let io = io11.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;
                    let path_str =
                        params.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'path' parameter")
                        })?;
                    if path_str.trim().is_empty() {
                        return Err(shux_rpc::RpcError::invalid_params(
                            "'path' parameter must not be empty",
                        ));
                    }
                    let path = PathBuf::from(path_str);
                    let overwrite = params
                        .get("overwrite")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let duration_ms = params.get("duration_ms").and_then(|v| v.as_u64());
                    if !overwrite {
                        match tokio::fs::try_exists(&path).await {
                            Ok(true) => {
                                return Err(shux_rpc::RpcError::invalid_params(
                                    "record file already exists; pass overwrite=true to replace it",
                                ));
                            }
                            Ok(false) => {}
                            Err(e) => {
                                return Err(shux_rpc::RpcError::internal(&format!(
                                    "failed to inspect record file {}: {e}",
                                    path.display()
                                )));
                            }
                        }
                    }

                    {
                        let state = io.lock().await;
                        if !state.writers.contains_key(&pane_id) {
                            return Err(shux_rpc::RpcError::not_found(
                                "live pane PTY",
                                &pane_id.to_string(),
                            ));
                        }
                        if state.recorders.get(&pane_id).is_some_and(|recorders| {
                            recorders.iter().any(|r| {
                                r.outcome.lock().expect("record outcome poisoned").status
                                    == PaneRecordStatus::Recording
                            })
                        }) {
                            return Err(shux_rpc::RpcError::name_conflict(
                                "pane recording",
                                &pane_id.to_string(),
                            ));
                        }
                    }

                    let (sender, outcome, task) = spawn_pane_recorder(path.clone(), overwrite)
                        .await
                        .map_err(|e| shux_rpc::RpcError::internal(&e))?;
                    let recording_id = uuid::Uuid::new_v4();
                    let mut state = io.lock().await;
                    if !state.writers.contains_key(&pane_id) {
                        drop(state);
                        let _ = sender
                            .send(PaneRecordChunk::Finish {
                                status: PaneRecordStatus::Aborted,
                            })
                            .await;
                        let _ = task.await;
                        return Err(shux_rpc::RpcError::not_found(
                            "live pane PTY",
                            &pane_id.to_string(),
                        ));
                    }
                    state
                        .recorders
                        .entry(pane_id)
                        .or_default()
                        .push(PaneRecorder {
                            id: recording_id,
                            path: path.clone(),
                            sender: sender.clone(),
                            outcome: outcome.clone(),
                            task,
                        });
                    drop(state);

                    if let Some(ms) = duration_ms {
                        let io_for_deadline = io.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                            let deadline_sender = {
                                let state = io_for_deadline.lock().await;
                                state.recorders.get(&pane_id).and_then(|recorders| {
                                    recorders
                                        .iter()
                                        .find(|r| r.id == recording_id)
                                        .and_then(|r| {
                                            let status = r
                                                .outcome
                                                .lock()
                                                .expect("record outcome poisoned")
                                                .status;
                                            (status == PaneRecordStatus::Recording)
                                                .then(|| r.sender.clone())
                                        })
                                })
                            };
                            if let Some(sender) = deadline_sender {
                                let _ = sender
                                    .send(PaneRecordChunk::Finish {
                                        status: PaneRecordStatus::Complete,
                                    })
                                    .await;
                            }
                            tokio::time::sleep(PANE_RECORD_COMPLETED_TTL).await;
                            let mut state = io_for_deadline.lock().await;
                            for recorders in state.recorders.values_mut() {
                                if let Some(pos) = recorders.iter().position(|r| {
                                    r.id == recording_id
                                        && r.outcome.lock().expect("record outcome poisoned").status
                                            != PaneRecordStatus::Recording
                                }) {
                                    recorders.remove(pos);
                                    break;
                                }
                            }
                            state.recorders.retain(|_, recorders| !recorders.is_empty());
                        });
                    }

                    Ok(serde_json::json!({
                        "recording_id": recording_id.to_string(),
                        "pane_id": pane_id.to_string(),
                        "path": path.display().to_string(),
                        "duration_ms": duration_ms,
                        "lossless": true,
                        "backpressure": true,
                    }))
                }
            },
        )
        .register_with_policy(
            "pane.record.stop",
            Policy::fixed(Sensitivity::PluginsForbidden),
            move |params: Option<serde_json::Value>| {
                let io = io12.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let recording_id_str = params
                        .get("recording_id")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'recording_id' parameter")
                        })?;
                    let recording_id: uuid::Uuid = recording_id_str.parse().map_err(|_| {
                        shux_rpc::RpcError::invalid_params("invalid recording_id format")
                    })?;

                    let recorder = {
                        let mut state = io.lock().await;
                        let mut found = None;
                        for recorders in state.recorders.values_mut() {
                            if let Some(pos) = recorders.iter().position(|r| r.id == recording_id) {
                                found = Some(recorders.remove(pos));
                                break;
                            }
                        }
                        state.recorders.retain(|_, recorders| !recorders.is_empty());
                        found
                    }
                    .ok_or_else(|| {
                        shux_rpc::RpcError::not_found("pane recording", recording_id_str)
                    })?;

                    let should_finish = recorder
                        .outcome
                        .lock()
                        .expect("record outcome poisoned")
                        .status
                        == PaneRecordStatus::Recording;
                    if should_finish {
                        let _ = recorder
                            .sender
                            .send(PaneRecordChunk::Finish {
                                status: PaneRecordStatus::Complete,
                            })
                            .await;
                    }
                    drop(recorder.sender);
                    let join_result =
                        tokio::time::timeout(std::time::Duration::from_secs(5), recorder.task)
                            .await;
                    match join_result {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            let mut outcome =
                                recorder.outcome.lock().expect("record outcome poisoned");
                            outcome.status = PaneRecordStatus::Error;
                            outcome.error = Some(format!("pane recorder task failed: {e}"));
                        }
                        Err(_) => {
                            let mut outcome =
                                recorder.outcome.lock().expect("record outcome poisoned");
                            if outcome.status == PaneRecordStatus::Recording {
                                outcome.status = PaneRecordStatus::Error;
                                outcome.error =
                                    Some("timed out while finalizing pane recording".to_string());
                            }
                        }
                    }
                    let result = recorder
                        .outcome
                        .lock()
                        .expect("record outcome poisoned")
                        .clone();

                    Ok(serde_json::json!({
                        "recording_id": recording_id.to_string(),
                        "path": recorder.path.display().to_string(),
                        "bytes_written": result.bytes_written,
                        "status": result.status.as_str(),
                        "lossless": result.status == PaneRecordStatus::Complete,
                        "error": result.error,
                    }))
                }
            },
        )
        .register_with_policy(
            "pane.snapshot",
            Policy::fixed(Sensitivity::ContentRead),
            move |params: Option<serde_json::Value>| {
                let gh = g6.clone();
                let io = io6.clone();
                let r = rasterizer_pane.load_full();
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
                    let (grid_snapshot, cursor_pos, snap_cols, snap_rows, default_colors) = {
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
                        let cursor_pos = cur.visible.then_some((cur.row, cur.col, cur.shape));
                        let default_colors = vt.default_colors();
                        // Visible-only clone — does NOT copy scrollback.
                        let grid_clone = vt.grid().clone_visible();
                        (grid_clone, cursor_pos, cols, rows, default_colors)
                    };

                    // Rasterize + PNG-encode off the runtime worker. Both are
                    // pure-CPU and don't yield, so we route them to a blocking
                    // worker that won't starve other RPC handlers.
                    let (img, png_buf) = tokio::task::spawn_blocking(move || {
                        let opts = shux_raster::RasterOptions {
                            cursor: cursor_pos.map(|(row, col, _)| (row, col)),
                            cursor_shape: cursor_pos.map(|(_, _, shape)| shape).unwrap_or_default(),
                            cursor_color: default_colors.cursor,
                            fg_default: default_colors.fg.unwrap_or_else(|| {
                                shux_raster::RasterOptions::default().fg_default
                            }),
                            bg_default: default_colors.bg.unwrap_or_else(|| {
                                shux_raster::RasterOptions::default().bg_default
                            }),
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
                    let r = rasterizer_window.load_full();
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
                    let r = rasterizer_session.load_full();
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
                        // LENS-R-006 (P1): collect every pane in the session with
                        // its structural entity `version`. The `content_revision`
                        // (§4 substrate) is read from each pane's VT below. This is
                        // the ONLY public exposure of the counter until pane.glance
                        // ships in P2 — it is what lets G3/G4 go green.
                        let session_version: u64 = session.version;
                        let pane_meta: Vec<(shux_core::model::PaneId, u64)> = session
                            .windows
                            .iter()
                            .filter_map(|wid| snap.windows.get(wid))
                            .flat_map(|win| win.layout.tree.pane_ids())
                            .filter_map(|pid| snap.panes.get(&pid).map(|p| (pid, p.version)))
                            .collect();
                        let (cols, rows) = parse_snapshot_dims(&params)?;
                        let mut result = snapshot_window(
                            &gh, &io, window_id, cols, rows, r, &cfg, &meta, &onb, &segs,
                        )
                        .await?;
                        // Read content_revision for each pane under a single io
                        // lock (a plain read — never touches DirtyState or any
                        // render-consumed state; LENS-R-004).
                        let content_revs: std::collections::HashMap<
                            shux_core::model::PaneId,
                            u64,
                        > = {
                            let state = io.lock().await;
                            pane_meta
                                .iter()
                                .filter_map(|(pid, _)| {
                                    state.vts.get(pid).map(|vt| (*pid, vt.content_revision()))
                                })
                                .collect()
                        };
                        // A graph pane without a VT is unreachable by design
                        // (VTs are created before session/pane creation returns
                        // and removed only when the graph pane is destroyed).
                        // If it ever happens, OMIT the entry rather than emit
                        // content_revision: 0 — LENS-R-001 says the counter
                        // starts at 1, so 0 is a lie. Skip-with-log matches
                        // snapshot_window's established handling of VT-less
                        // panes (filter_map over `state.vts`); debug builds
                        // assert loudly (council major 4).
                        let panes_json: Vec<serde_json::Value> = pane_meta
                            .iter()
                            .filter_map(|(pid, version)| match content_revs.get(pid) {
                                Some(rev) => Some(serde_json::json!({
                                    "pane_id": pid.to_string(),
                                    "version": version,
                                    "content_revision": rev,
                                })),
                                None => {
                                    debug_assert!(
                                        false,
                                        "session.snapshot: graph pane {pid} has no VT"
                                    );
                                    tracing::warn!(
                                        %pid,
                                        "session.snapshot: graph pane has no VT; \
                                         omitting from panes[] (never emit revision 0)"
                                    );
                                    None
                                }
                            })
                            .collect();
                        if let Some(obj) = result.as_object_mut() {
                            obj.insert(
                                "session_version".to_string(),
                                serde_json::json!(session_version),
                            );
                            obj.insert("panes".to_string(), serde_json::json!(panes_json));
                        }
                        Ok(result)
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
            cli::SessionCommand::Save { session, output } => {
                let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
                cli::handle_session_save(&mut stream, &session, output).await
            }
            cli::SessionCommand::Restore {
                template,
                dry_run,
                watch,
            } => {
                let ops = template::load_and_lower(&template)?;
                if dry_run {
                    println!("{}", serde_json::to_string_pretty(&ops)?);
                    Ok(())
                } else {
                    let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
                    cli::handle_apply(&mut stream, ops, watch, &socket_path).await
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
    use shux_core::graph::{GraphHandle, SessionGraph, SessionGraphSnapshot, run_graph_loop};
    use shux_core::layout::Direction;
    use shux_core::model::{Pane, Session, Window};
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

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

    fn shux_raster_asset(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../shux-raster/assets")
            .join(name)
    }

    #[test]
    fn snapshot_rasterizer_default_chain_covers_tui_text_symbols() {
        let cfg = Config::default();
        let rasterizer = build_snapshot_rasterizer(&cfg).expect("rasterizer");
        for ch in ['\u{21bb}', '\u{2839}'] {
            assert!(
                rasterizer.has_glyph(ch),
                "default snapshot chain should resolve {ch:?}"
            );
            assert!(
                rasterizer.glyph_pixel_count(ch) >= 8,
                "default snapshot chain should render {ch:?} as non-empty pixels"
            );
        }
    }

    #[test]
    fn snapshot_rasterizer_accepts_ordered_builtin_fallbacks() {
        let mut cfg = Config::default();
        cfg.appearance.font_fallbacks = Some(vec![
            shux_raster::BUILTIN_SYMBOLS.to_string(),
            shux_raster::BUILTIN_MATH.to_string(),
            shux_raster::BUILTIN_SYMBOLS_LEGACY.to_string(),
            shux_raster::BUILTIN_EMOJI.to_string(),
        ]);

        let rasterizer = build_snapshot_rasterizer(&cfg).expect("rasterizer");
        let baseline = shux_raster::Rasterizer::new(14.0).expect("baseline rasterizer");
        assert_eq!(
            rasterizer.cell_size(),
            baseline.cell_size(),
            "custom fallbacks without appearance.font must not replace primary metrics"
        );
        assert!(rasterizer.has_glyph('\u{21bb}'));
        assert!(rasterizer.has_glyph('\u{2839}'));
        assert!(rasterizer.has_glyph('\u{1f37a}'));
    }

    #[test]
    fn snapshot_rasterizer_accepts_path_fallback_without_replacing_primary_metrics() {
        let mut cfg = Config::default();
        cfg.appearance.font = Some(shux_raster_asset("JetBrainsMonoNerdFontMono-Regular.ttf"));
        cfg.appearance.font_fallbacks = Some(vec![
            shux_raster_asset("NotoSansSymbols2-Regular.ttf")
                .display()
                .to_string(),
        ]);

        let rasterizer = build_snapshot_rasterizer(&cfg).expect("rasterizer");
        let baseline = shux_raster::Rasterizer::with_primary_font(
            14.0,
            include_bytes!("../../shux-raster/assets/JetBrainsMonoNerdFontMono-Regular.ttf"),
        )
        .expect("baseline rasterizer");
        assert_eq!(
            rasterizer.cell_size(),
            baseline.cell_size(),
            "fallback chain must not change the explicit primary font metrics"
        );
        assert!(rasterizer.has_glyph('A'));
        assert!(rasterizer.has_glyph('\u{2839}'));
    }

    #[test]
    fn snapshot_rasterizer_rejects_empty_fallbacks() {
        let mut cfg = Config::default();
        cfg.appearance.font_fallbacks = Some(vec![]);

        let err = match build_snapshot_rasterizer(&cfg) {
            Ok(_) => panic!("empty fallback list should fail"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("must not be empty"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn snapshot_rasterizer_rejects_unknown_builtin_fallback_token() {
        let mut cfg = Config::default();
        cfg.appearance.font_fallbacks = Some(vec!["builtin:symbol".to_string()]);

        let err = match build_snapshot_rasterizer(&cfg) {
            Ok(_) => panic!("unknown builtin fallback token should fail"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("unknown builtin font token"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn snapshot_rasterizer_rejects_missing_fallback_path() {
        let mut cfg = Config::default();
        cfg.appearance.font_fallbacks = Some(vec![
            "/tmp/this-shux-font-fallback-does-not-exist.ttf".to_string(),
        ]);

        let err = match build_snapshot_rasterizer(&cfg) {
            Ok(_) => panic!("missing fallback path should fail"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("appearance.font_fallbacks"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn snapshot_font_key_tracks_fallback_changes() {
        let mut before = Config::default();
        before.appearance.font = Some(std::path::PathBuf::from("/tmp/primary.ttf"));
        let mut after = before.clone();
        after.appearance.font_fallbacks = Some(vec![shux_raster::BUILTIN_SYMBOLS.to_string()]);

        assert_ne!(snapshot_font_key(&before), snapshot_font_key(&after));
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

    #[tokio::test]
    async fn set_initial_pane_title_targets_original_pane_after_focus_changes() {
        let (graph, state) = SessionGraph::new();
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        let token = CancellationToken::new();
        let token_clone = token.clone();
        let handle = tokio::spawn(async move {
            run_graph_loop(graph, cmd_rx, token_clone).await;
        });
        let gh = GraphHandle::new(cmd_tx, state);

        let session_id = gh
            .create_session_with_command(
                "title-race".to_string(),
                std::path::PathBuf::from("/tmp"),
                vec!["codex".to_string(), "--yolo".to_string()],
            )
            .await
            .unwrap();

        let original_pane = {
            let snap = gh.snapshot();
            initial_pane_id_for_session(&snap, session_id).unwrap()
        };
        let new_active = gh
            .split_pane(original_pane, Direction::Vertical, 0.5)
            .await
            .unwrap();

        set_initial_pane_title(&gh, session_id, Some("aww-shux".to_string()))
            .await
            .unwrap();

        let snap = gh.snapshot();
        assert_eq!(
            snap.panes[&original_pane].manual_title.as_deref(),
            Some("aww-shux")
        );
        assert_eq!(snap.panes[&original_pane].title, "aww-shux");
        assert_eq!(snap.panes[&new_active].manual_title, None);

        token.cancel();
        handle.await.unwrap();
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

    fn write_plugin_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[test]
    fn daemon_utility_mappers_cover_error_preview_snapshot_and_color_edges() {
        use shux_core::graph::GraphError;

        assert_eq!(
            graph_error_to_rpc(GraphError::SessionNotFound(
                shux_core::model::SessionId::new()
            ))
            .code,
            shux_rpc::ErrorCode::NotFound.code()
        );
        assert_eq!(
            graph_error_to_rpc(GraphError::WindowNotFound(shux_core::model::WindowId::new())).code,
            shux_rpc::ErrorCode::NotFound.code()
        );
        assert_eq!(
            graph_error_to_rpc(GraphError::PaneNotFound(shux_core::model::PaneId::new())).code,
            shux_rpc::ErrorCode::NotFound.code()
        );
        assert_eq!(
            graph_error_to_rpc(GraphError::WindowNameConflict("logs".to_string())).code,
            shux_rpc::ErrorCode::NameConflict.code()
        );
        assert_eq!(
            graph_error_to_rpc(GraphError::InvalidSessionName("bad/name".to_string())).code,
            shux_rpc::ErrorCode::InvalidParams.code()
        );
        assert_eq!(
            graph_error_to_rpc(GraphError::EmptyWindowName).code,
            shux_rpc::ErrorCode::InvalidParams.code()
        );
        assert_eq!(
            graph_error_to_rpc(GraphError::LastPane).code,
            shux_rpc::ErrorCode::InvalidParams.code()
        );
        assert_eq!(
            graph_error_to_rpc(GraphError::LayoutError("split failed".to_string())).code,
            shux_rpc::ErrorCode::InternalError.code()
        );
        assert_eq!(
            graph_error_to_rpc(GraphError::Shutdown).code,
            shux_rpc::ErrorCode::InternalError.code()
        );
        assert_eq!(
            graph_error_to_rpc(GraphError::VersionConflict {
                resource: "pane",
                id: "p1".to_string(),
                expected: 1,
                actual: 2,
            })
            .code,
            shux_rpc::ErrorCode::VersionConflict.code()
        );

        assert_eq!(
            parse_snapshot_dims(&serde_json::json!({"cols": 80, "rows": 24})).unwrap(),
            (80, 24)
        );
        assert_eq!(
            parse_snapshot_dims(&serde_json::json!({})).unwrap(),
            (120, 36)
        );
        assert_eq!(
            parse_snapshot_dims(&serde_json::json!({"cols": 3, "rows": 24}))
                .unwrap_err()
                .code,
            shux_rpc::ErrorCode::InvalidParams.code()
        );
        assert_eq!(
            parse_expected_version(&serde_json::json!({})).unwrap(),
            None
        );
        assert_eq!(
            parse_expected_version(&serde_json::json!({"expected_version": null})).unwrap(),
            None
        );
        assert_eq!(
            parse_expected_version(&serde_json::json!({"expected_version": 7})).unwrap(),
            Some(7)
        );
        assert_eq!(
            parse_expected_version(&serde_json::json!({"expected_version": "old"}))
                .unwrap_err()
                .code,
            shux_rpc::ErrorCode::InvalidParams.code()
        );
        assert_eq!(
            parse_initial_pane_title(&serde_json::json!({"pane_title": "editor"})).unwrap(),
            Some("editor".to_string())
        );
        assert_eq!(
            parse_initial_pane_title(&serde_json::json!({"pane_title": ""}))
                .unwrap_err()
                .code,
            shux_rpc::ErrorCode::InvalidParams.code()
        );
        assert_eq!(preview_for_log("short", 20), "short");
        assert_eq!(preview_for_log("one\ntwo\nthree", 5), "three");

        let default_io = PaneIoState::default();
        assert!(default_io.writers.is_empty());

        let mut grid = shux_vt::Grid::new(1, 2, shux_vt::GridConfig::default());
        resolve_grid_default_colors(&mut grid, shux_vt::TerminalDefaultColors::default());
        grid.visible_row_mut(0)[0].style.fg = shux_vt::Color::Default;
        grid.visible_row_mut(0)[1].style.bg = shux_vt::Color::Default;
        resolve_grid_default_colors(
            &mut grid,
            shux_vt::TerminalDefaultColors {
                fg: Some([1, 2, 3]),
                bg: Some([4, 5, 6]),
                cursor: None,
            },
        );
        assert_eq!(
            grid.visible_row(0)[0].style.fg,
            shux_vt::Color::Rgb(1, 2, 3)
        );
        assert_eq!(
            grid.visible_row(0)[1].style.bg,
            shux_vt::Color::Rgb(4, 5, 6)
        );
    }

    struct RpcHarness {
        router: shux_rpc::Router,
        graph: GraphHandle,
        io: Arc<Mutex<PaneIoState>>,
        bus: shux_core::bus::EventBus,
        cancel: CancellationToken,
        graph_task: tokio::task::JoinHandle<()>,
    }

    impl RpcHarness {
        fn new() -> Self {
            let bus = shux_core::bus::EventBus::new();
            let (graph, state) = SessionGraph::new_with_event_bus(Some(bus.clone()));
            let (cmd_tx, cmd_rx) = mpsc::channel(128);
            let cancel = CancellationToken::new();
            let graph_cancel = cancel.clone();
            let graph_task = tokio::spawn(async move {
                run_graph_loop(graph, cmd_rx, graph_cancel).await;
            });
            let graph = GraphHandle::new(cmd_tx, state);
            let io = Arc::new(Mutex::new(PaneIoState::new().with_event_bus(bus.clone())));
            let meta = session_meta::SessionMetaCache::new();
            let config_path =
                std::env::temp_dir().join(format!("shux-rpc-test-{}.toml", uuid::Uuid::new_v4()));
            let config = ConfigHandle::load_or_default(&config_path);
            let onboarding = onboarding::OnboardingHandle::from_state_for_test(Default::default());
            let segments = statusbar_runner::SegmentCache::new();

            let builder = register_session_methods(
                shux_rpc::Router::builder(),
                graph.clone(),
                io.clone(),
                cancel.clone(),
                meta.clone(),
            );
            let builder =
                register_window_methods(builder, graph.clone(), io.clone(), cancel.clone());
            let builder = register_pane_methods(builder, graph.clone(), io.clone(), cancel.clone());
            let builder =
                register_state_methods(builder, graph.clone(), io.clone(), cancel.clone());
            let builder = register_pane_io_methods(
                builder,
                graph.clone(),
                io.clone(),
                cancel.clone(),
                config,
                meta,
                onboarding,
                segments,
            );
            let router = register_events_methods(builder, bus.clone()).build();
            router.assert_every_route_has_policy();

            Self {
                router,
                graph,
                io,
                bus,
                cancel,
                graph_task,
            }
        }

        async fn stop(self) {
            self.cancel.cancel();
            self.graph_task.await.unwrap();
        }

        async fn seed_session(
            &self,
            name: &str,
        ) -> (
            shux_core::model::SessionId,
            shux_core::model::WindowId,
            shux_core::model::PaneId,
        ) {
            let session_id = self
                .graph
                .create_session_with_command(
                    name.to_string(),
                    std::path::PathBuf::from("/tmp"),
                    vec!["bash".to_string()],
                )
                .await
                .unwrap();
            let snap = self.graph.snapshot();
            let session = snap.sessions.get(&session_id).unwrap();
            let window_id = session.active_window;
            let pane_id = snap.windows.get(&window_id).unwrap().active_pane;
            (session_id, window_id, pane_id)
        }

        async fn seed_io(
            &self,
            pane_id: shux_core::model::PaneId,
            text: &[u8],
        ) -> mpsc::Receiver<Vec<u8>> {
            let (write_tx, write_rx) = mpsc::channel(16);
            let (resize_tx, mut resize_rx) = mpsc::channel::<ResizeRequest>(8);
            let io = self.io.clone();
            tokio::spawn(async move {
                while let Some(req) = resize_rx.recv().await {
                    let mut state = io.lock().await;
                    if let Some(vt) = state.vts.get_mut(&pane_id) {
                        vt.resize(req.size.rows as usize, req.size.cols as usize);
                    }
                    if let Some(ack) = req.ack {
                        let _ = ack.send(());
                    }
                }
            });

            let mut vt = shux_vt::VirtualTerminal::new(6, 40);
            if !text.is_empty() {
                vt.process(text);
            }
            let mut state = self.io.lock().await;
            state.writers.insert(pane_id, write_tx);
            state.resizers.insert(pane_id, resize_tx);
            state.shutdowns.insert(pane_id, self.cancel.child_token());
            state.vts.insert(pane_id, vt);
            write_rx
        }
    }

    async fn dispatch_ok(
        router: &shux_rpc::Router,
        method: &str,
        params: serde_json::Value,
    ) -> serde_json::Value {
        router.dispatch(method, Some(params)).await.unwrap()
    }

    async fn dispatch_err(
        router: &shux_rpc::Router,
        method: &str,
        params: serde_json::Value,
    ) -> shux_rpc::RpcError {
        router.dispatch(method, Some(params)).await.unwrap_err()
    }

    #[tokio::test]
    async fn production_state_apply_reports_validation_and_spawn_results() {
        let harness = RpcHarness::new();

        let missing_ops = dispatch_err(&harness.router, "state.apply", serde_json::json!({})).await;
        assert_eq!(missing_ops.code, shux_rpc::ErrorCode::InvalidParams.code());

        let malformed_ops = dispatch_err(
            &harness.router,
            "state.apply",
            serde_json::json!({"ops": "not-an-array"}),
        )
        .await;
        assert_eq!(
            malformed_ops.code,
            shux_rpc::ErrorCode::InvalidParams.code()
        );

        let empty_ops = dispatch_err(
            &harness.router,
            "state.apply",
            serde_json::json!({"ops": []}),
        )
        .await;
        assert_eq!(empty_ops.code, shux_rpc::ErrorCode::InvalidParams.code());

        let applied = dispatch_ok(
            &harness.router,
            "state.apply",
            serde_json::json!({
                "ops": [{
                    "op": "create_session",
                    "name": "applied",
                    "cwd": "/tmp",
                    "initial_command": ["true"],
                    "initial_window_title": "dev"
                }]
            }),
        )
        .await;
        assert_eq!(applied["outputs"].as_array().unwrap().len(), 1);
        assert_eq!(applied["spawn_results"].as_array().unwrap().len(), 1);
        assert!(
            applied["correlation_id"]
                .as_str()
                .unwrap()
                .starts_with("apply-")
        );
        let snap = harness.graph.snapshot();
        let session = snap
            .find_session_by_name("applied")
            .expect("applied session");
        let session_id = session.id;
        let window_id = session.active_window;
        let pane_id = snap.windows[&window_id].active_pane;
        drop(snap);
        assert_eq!(
            resolve_window_id_from_params(
                &harness.graph,
                &serde_json::json!({"session_id": session_id.to_string()})
            )
            .unwrap(),
            window_id
        );
        assert_eq!(
            resolve_pane_id_from_params(
                &harness.graph,
                &serde_json::json!({"pane_id": pane_id.to_string()})
            )
            .unwrap(),
            pane_id
        );
        assert_eq!(
            resolve_window_id_from_params(&harness.graph, &serde_json::json!({}))
                .unwrap_err()
                .code,
            shux_rpc::ErrorCode::InvalidParams.code()
        );

        harness.stop().await;
    }

    #[tokio::test]
    async fn production_plugin_router_covers_install_grants_audit_reload_and_kill() {
        const NOOP_PLUGIN: &str = r#"#!/usr/bin/env bash
set -u
IFS= read -r _ || exit 1
printf '%s\n' '{"jsonrpc":"2.0","id":"init","result":{"name":"noop","version":"0.1.0","subscribes":[],"provides":[],"capabilities":[]}}'
while IFS= read -r line; do
  case "$line" in
    *'"plugin.shutdown"'*) exit 0 ;;
  esac
done
"#;

        let tmp = tempfile::tempdir().unwrap();
        let script = write_plugin_script(tmp.path(), "noop.sh", NOOP_PLUGIN);
        let manager = shux_plugin::PluginManager::with_state_root(
            shux_core::bus::EventBus::new(),
            tmp.path().join("plugins"),
        );
        let router = register_plugin_methods(shux_rpc::Router::builder(), manager.clone()).build();
        router.assert_every_route_has_policy();

        let missing_path = dispatch_err(&router, "plugin.install", serde_json::json!({})).await;
        assert_eq!(missing_path.code, shux_rpc::ErrorCode::InvalidParams.code());

        let identity_mismatch = dispatch_err(
            &router,
            "plugin.install",
            serde_json::json!({
                "path": script.clone(),
                "watch": false,
                "expected_name": "not-noop",
                "expected_version": "0.1.0",
            }),
        )
        .await;
        assert_eq!(
            identity_mismatch.code,
            shux_rpc::ErrorCode::InvalidParams.code()
        );
        assert!(
            identity_mismatch
                .data
                .as_ref()
                .and_then(|data| data.get("detail"))
                .and_then(|detail| detail.as_str())
                .unwrap_or_default()
                .contains("plugin manifest name mismatch")
        );

        let installed = dispatch_ok(
            &router,
            "plugin.install",
            serde_json::json!({
                "path": script,
                "watch": false,
                "state_root": tmp.path().join("plugin-state"),
            }),
        )
        .await;
        assert_eq!(installed["name"], "noop");
        assert_eq!(installed["watching"], false);

        let listed = dispatch_ok(&router, "plugin.list", serde_json::json!({})).await;
        assert_eq!(listed["plugins"].as_array().unwrap().len(), 1);

        let granted = dispatch_ok(
            &router,
            "plugin.grant",
            serde_json::json!({"plugin": "noop", "method": "pane.capture", "target": "pane-1"}),
        )
        .await;
        assert_eq!(granted["granted"], true);
        let subscribe_grant = dispatch_ok(
            &router,
            "plugin.grant",
            serde_json::json!({"plugin": "noop", "method": "pane.output.", "subscribe": true}),
        )
        .await;
        assert_eq!(subscribe_grant["subscribe"], true);

        let grants = dispatch_ok(
            &router,
            "plugin.grants",
            serde_json::json!({"plugin": "noop"}),
        )
        .await;
        assert!(
            grants["grants"]
                .as_object()
                .unwrap()
                .contains_key("pane.capture")
        );

        let audit_path = manager.audit_path("noop").await.unwrap();
        std::fs::create_dir_all(audit_path.parent().unwrap()).unwrap();
        std::fs::write(
            &audit_path,
            r#"{"seq":1,"method":"old"}
{"seq":2,"method":"new"}
"#,
        )
        .unwrap();
        let audit = dispatch_ok(
            &router,
            "plugin.audit",
            serde_json::json!({"plugin": "noop", "tail": 1}),
        )
        .await;
        assert_eq!(audit["entries"].as_array().unwrap().len(), 1);
        assert_eq!(audit["entries"][0]["method"], "new");

        let revoked = dispatch_ok(
            &router,
            "plugin.revoke",
            serde_json::json!({"plugin": "noop", "method": "pane.capture", "target": "pane-1"}),
        )
        .await;
        assert_eq!(revoked["revoked"], true);

        let reloaded = dispatch_ok(
            &router,
            "plugin.reload",
            serde_json::json!({"name": "noop"}),
        )
        .await;
        assert_eq!(reloaded["name"], "noop");

        let killed = dispatch_ok(&router, "plugin.kill", serde_json::json!({"name": "noop"})).await;
        assert_eq!(killed["killed"], "noop");
        let missing_plugin =
            dispatch_err(&router, "plugin.kill", serde_json::json!({"name": "noop"})).await;
        assert_eq!(missing_plugin.code, shux_rpc::ErrorCode::NotFound.code());
    }

    #[tokio::test]
    async fn production_router_session_window_pane_routes_mutate_graph_and_cleanup_io() {
        let harness = RpcHarness::new();
        let (session_id, window_id, first_pane) = harness.seed_session("alpha").await;
        let second_pane = harness
            .graph
            .split_pane(first_pane, Direction::Vertical, 0.5)
            .await
            .unwrap();
        let second_window = harness
            .graph
            .create_window(
                session_id,
                "logs".to_string(),
                std::path::PathBuf::from("/tmp"),
            )
            .await
            .unwrap();
        let second_window_pane = {
            let snap = harness.graph.snapshot();
            snap.windows[&second_window].active_pane
        };
        let _first_rx = harness.seed_io(first_pane, b"alpha ready\n").await;
        let _second_rx = harness.seed_io(second_pane, b"beta ready\n").await;
        let _window_rx = harness.seed_io(second_window_pane, b"logs ready\n").await;

        let listed = dispatch_ok(&harness.router, "session.list", serde_json::json!({})).await;
        assert_eq!(listed["sessions"][0]["name"], "alpha");
        assert_eq!(listed["sessions"][0]["window_count"], 2);

        let renamed = dispatch_ok(
            &harness.router,
            "session.rename",
            serde_json::json!({"name": "alpha", "new_name": "beta"}),
        )
        .await;
        assert_eq!(renamed["name"], "beta");

        let _other = harness.seed_session("other").await;
        let conflict = dispatch_err(
            &harness.router,
            "session.rename",
            serde_json::json!({"id": session_id.to_string(), "new_name": "other"}),
        )
        .await;
        assert_eq!(conflict.code, shux_rpc::ErrorCode::NameConflict.code());

        let windows = dispatch_ok(
            &harness.router,
            "window.list",
            serde_json::json!({"session_id": session_id.to_string()}),
        )
        .await;
        assert_eq!(windows.as_array().unwrap().len(), 2);

        let renamed_window = dispatch_ok(
            &harness.router,
            "window.rename",
            serde_json::json!({"id": second_window.to_string(), "name": "ops"}),
        )
        .await;
        assert_eq!(renamed_window["title"], "ops");

        let refocused_first = dispatch_ok(
            &harness.router,
            "window.focus",
            serde_json::json!({"id": window_id.to_string()}),
        )
        .await;
        assert_eq!(
            refocused_first["previous_window_id"],
            second_window.to_string()
        );

        let focused_window = dispatch_ok(
            &harness.router,
            "window.focus",
            serde_json::json!({"id": second_window.to_string()}),
        )
        .await;
        assert_eq!(focused_window["previous_window_id"], window_id.to_string());

        let reordered = dispatch_ok(
            &harness.router,
            "window.reorder",
            serde_json::json!({"id": second_window.to_string(), "new_index": 0}),
        )
        .await;
        assert_eq!(reordered["index"], 0);

        let panes = dispatch_ok(
            &harness.router,
            "pane.list",
            serde_json::json!({"window_id": window_id.to_string()}),
        )
        .await;
        assert_eq!(panes.as_array().unwrap().len(), 2);

        let focused_pane = dispatch_ok(
            &harness.router,
            "pane.focus",
            serde_json::json!({"pane_id": second_pane.to_string()}),
        )
        .await;
        assert_eq!(focused_pane["pane_id"], second_pane.to_string());

        let titled = dispatch_ok(
            &harness.router,
            "pane.set_title",
            serde_json::json!({"pane_id": second_pane.to_string(), "title": "editor", "auto": false}),
        )
        .await;
        assert_eq!(titled["manual_title"], "editor");
        assert_eq!(titled["auto_title"], false);

        let zoomed = dispatch_ok(
            &harness.router,
            "pane.zoom",
            serde_json::json!({"pane_id": second_pane.to_string()}),
        )
        .await;
        assert_eq!(zoomed["is_zoomed"], true);
        let _ = dispatch_ok(
            &harness.router,
            "pane.zoom",
            serde_json::json!({"pane_id": second_pane.to_string()}),
        )
        .await;

        let swapped = dispatch_ok(
            &harness.router,
            "pane.swap",
            serde_json::json!({"pane_id": first_pane.to_string(), "target_pane_id": second_pane.to_string()}),
        )
        .await;
        assert_eq!(swapped["pane_a"], first_pane.to_string());

        let stale = dispatch_err(
            &harness.router,
            "pane.resize",
            serde_json::json!({"pane_id": second_pane.to_string(), "direction": "horizontal", "delta": 0.2, "expected_version": 999_999}),
        )
        .await;
        assert_eq!(stale.code, shux_rpc::ErrorCode::VersionConflict.code());
        assert!(
            harness.io.lock().await.vts.contains_key(&second_pane),
            "stale pane.resize must not tear down pane IO"
        );

        let killed_pane = dispatch_ok(
            &harness.router,
            "pane.kill",
            serde_json::json!({"pane_id": second_pane.to_string()}),
        )
        .await;
        assert_eq!(killed_pane["killed"], second_pane.to_string());
        assert!(!harness.io.lock().await.vts.contains_key(&second_pane));

        let killed_window = dispatch_ok(
            &harness.router,
            "window.kill",
            serde_json::json!({"id": second_window.to_string()}),
        )
        .await;
        assert_eq!(killed_window["killed"], second_window.to_string());
        assert!(
            !harness
                .io
                .lock()
                .await
                .vts
                .contains_key(&second_window_pane)
        );

        let killed_session = dispatch_ok(
            &harness.router,
            "session.kill",
            serde_json::json!({"id": session_id.to_string()}),
        )
        .await;
        assert_eq!(killed_session["killed"], "beta");
        assert!(!harness.io.lock().await.vts.contains_key(&first_pane));
        assert!(!harness.graph.snapshot().sessions.contains_key(&session_id));

        harness.stop().await;
    }

    #[tokio::test]
    async fn production_events_routes_filter_history_and_live_data_plane() {
        let harness = RpcHarness::new();
        let (session_id, window_id, pane_id) = harness.seed_session("events").await;

        let seq = harness
            .bus
            .publish(shux_core::event::EventData::PluginEvent {
                plugin_id: "mine".to_string(),
                event_type: "tick".to_string(),
                data: serde_json::json!({"ok": true}),
            });
        harness
            .bus
            .publish(shux_core::event::EventData::PluginEvent {
                plugin_id: "other".to_string(),
                event_type: "tick".to_string(),
                data: serde_json::json!({"ok": false}),
            });

        let history = dispatch_ok(
            &harness.router,
            "events.history",
            serde_json::json!({"filter": ["plugin.mine."], "count": 10}),
        )
        .await;
        let events = history["events"].as_array().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "plugin.mine.tick");

        let watched = dispatch_ok(
            &harness.router,
            "events.watch",
            serde_json::json!({"from_seq": seq, "filter": ["plugin.mine."], "max_events": 5, "timeout_ms": 25}),
        )
        .await;
        assert_eq!(watched["events"].as_array().unwrap().len(), 1);
        assert_eq!(watched["next_seq"], seq + 1);
        assert_eq!(watched["lagged"], false);

        let router = harness.router.clone();
        let pane_str = pane_id.to_string();
        let watch = tokio::spawn(async move {
            router
                .dispatch(
                    "pane.output.watch",
                    Some(serde_json::json!({
                        "pane_id": pane_str,
                        "timeout_ms": 500,
                        "limit": 2,
                    })),
                )
                .await
                .unwrap()
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let output_seq = harness.bus.publish_pane_output(
            pane_id,
            window_id,
            session_id,
            "aGVsbG8=".to_string(),
            false,
        );
        let output = watch.await.unwrap();
        assert_eq!(output["chunks"][0]["seq"], output_seq);
        assert_eq!(output["chunks"][0]["bytes"], "aGVsbG8=");
        assert_eq!(output["chunks"][0]["sampled"], false);

        let missing_pane = dispatch_err(
            &harness.router,
            "pane.output.watch",
            serde_json::json!({"timeout_ms": 100}),
        )
        .await;
        assert_eq!(missing_pane.code, shux_rpc::ErrorCode::InvalidParams.code());

        harness.stop().await;
    }

    #[tokio::test]
    async fn pane_record_routes_capture_source_bytes_losslessly() {
        let harness = RpcHarness::new();
        let (_session_id, _window_id, pane_id) = harness.seed_session("record").await;
        let _writer_rx = harness.seed_io(pane_id, b"ready\n").await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("raw-output.bin");

        let started = dispatch_ok(
            &harness.router,
            "pane.record.start",
            serde_json::json!({
                "pane_id": pane_id.to_string(),
                "path": path.display().to_string(),
            }),
        )
        .await;
        assert_eq!(started["lossless"], true);
        assert_eq!(started["backpressure"], true);
        let recording_id = started["recording_id"].as_str().unwrap().to_string();

        let mut payload = Vec::new();
        for i in 0..8192u32 {
            payload.extend_from_slice(b"\x1b[2Jframe:");
            payload.extend_from_slice(i.to_string().as_bytes());
            payload.push(b'\n');
        }
        assert!(
            payload.len() > 64 * 1024,
            "payload should exceed sampled pane.output pending cap"
        );
        tee_pane_recorders(&harness.io, pane_id, &payload, &harness.cancel).await;

        let stopped = dispatch_ok(
            &harness.router,
            "pane.record.stop",
            serde_json::json!({
                "recording_id": recording_id,
            }),
        )
        .await;
        assert_eq!(stopped["status"], "complete");
        assert_eq!(stopped["lossless"], true);
        assert_eq!(stopped["bytes_written"], payload.len() as u64);
        let recorded = tokio::fs::read(dir.path().join("raw-output.bin"))
            .await
            .unwrap();
        assert_eq!(recorded, payload);

        harness.stop().await;
    }

    #[tokio::test]
    async fn pane_record_start_rejects_duplicate_active_recorder_for_pane() {
        let harness = RpcHarness::new();
        let (_session_id, _window_id, pane_id) = harness.seed_session("record-duplicate").await;
        let _writer_rx = harness.seed_io(pane_id, b"ready\n").await;
        let dir = tempfile::tempdir().unwrap();
        let first_path = dir.path().join("first.bin");
        let second_path = dir.path().join("second.bin");

        let started = dispatch_ok(
            &harness.router,
            "pane.record.start",
            serde_json::json!({
                "pane_id": pane_id.to_string(),
                "path": first_path.display().to_string(),
            }),
        )
        .await;
        let err = dispatch_err(
            &harness.router,
            "pane.record.start",
            serde_json::json!({
                "pane_id": pane_id.to_string(),
                "path": second_path.display().to_string(),
            }),
        )
        .await;
        assert_eq!(err.code, shux_rpc::ErrorCode::NameConflict.code());

        let _ = dispatch_ok(
            &harness.router,
            "pane.record.stop",
            serde_json::json!({
                "recording_id": started["recording_id"].as_str().unwrap(),
            }),
        )
        .await;
        harness.stop().await;
    }

    #[tokio::test]
    async fn pane_record_duration_stops_on_daemon_side() {
        let harness = RpcHarness::new();
        let (_session_id, _window_id, pane_id) = harness.seed_session("record-duration").await;
        let _writer_rx = harness.seed_io(pane_id, b"ready\n").await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("duration.bin");

        let started = dispatch_ok(
            &harness.router,
            "pane.record.start",
            serde_json::json!({
                "pane_id": pane_id.to_string(),
                "path": path.display().to_string(),
                "duration_ms": 25,
            }),
        )
        .await;
        let recording_id = started["recording_id"].as_str().unwrap().to_string();
        tee_pane_recorders(&harness.io, pane_id, b"before-deadline", &harness.cancel).await;
        tokio::time::sleep(std::time::Duration::from_millis(75)).await;
        tee_pane_recorders(&harness.io, pane_id, b"after-deadline", &harness.cancel).await;

        let stopped = dispatch_ok(
            &harness.router,
            "pane.record.stop",
            serde_json::json!({
                "recording_id": recording_id,
            }),
        )
        .await;
        assert_eq!(stopped["status"], "complete");
        assert_eq!(stopped["bytes_written"], "before-deadline".len() as u64);
        assert_eq!(
            tokio::fs::read(dir.path().join("duration.bin"))
                .await
                .unwrap(),
            b"before-deadline"
        );

        harness.stop().await;
    }

    #[tokio::test]
    async fn pane_record_start_refuses_existing_file_without_overwrite() {
        let harness = RpcHarness::new();
        let (_session_id, _window_id, pane_id) = harness.seed_session("record-exists").await;
        let _writer_rx = harness.seed_io(pane_id, b"ready\n").await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing.bin");
        tokio::fs::write(&path, b"keep").await.unwrap();

        let err = dispatch_err(
            &harness.router,
            "pane.record.start",
            serde_json::json!({
                "pane_id": pane_id.to_string(),
                "path": path.display().to_string(),
            }),
        )
        .await;
        assert_eq!(err.code, shux_rpc::ErrorCode::InvalidParams.code());
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"keep");

        harness.stop().await;
    }

    #[tokio::test]
    async fn production_pane_io_routes_cover_writes_capture_wait_resize_and_commands() {
        let harness = RpcHarness::new();
        let (session_id, window_id, pane_id) = harness.seed_session("io").await;
        let mut writer_rx = harness
            .seed_io(pane_id, b"boot complete\nagent-ready\n")
            .await;

        let sent = dispatch_ok(
            &harness.router,
            "pane.send_keys",
            serde_json::json!({"pane_id": pane_id.to_string(), "text": "echo hi\n"}),
        )
        .await;
        assert_eq!(sent["bytes_written"], 8);
        assert_eq!(writer_rx.recv().await.unwrap(), b"echo hi\n");

        let sent_b64 = dispatch_ok(
            &harness.router,
            "pane.send_keys",
            serde_json::json!({"pane_id": pane_id.to_string(), "data": "A03/"}),
        )
        .await;
        assert_eq!(sent_b64["bytes_written"], 3);
        assert_eq!(writer_rx.recv().await.unwrap(), vec![3, 77, 255]);

        let capture = dispatch_ok(
            &harness.router,
            "pane.capture",
            serde_json::json!({"session_id": session_id.to_string(), "lines": 2}),
        )
        .await;
        assert!(capture["text"].as_str().unwrap().contains("agent-ready"));
        assert_eq!(capture["requested_lines"], 2);

        let wait_text = dispatch_ok(
            &harness.router,
            "pane.wait_for",
            serde_json::json!({"window_id": window_id.to_string(), "text": "agent-ready", "timeout_ms": 40, "poll_ms": 20}),
        )
        .await;
        assert_eq!(wait_text["matched"], true);
        assert_eq!(wait_text["absent"], false);

        let wait_absent = dispatch_ok(
            &harness.router,
            "pane.wait_for",
            serde_json::json!({"pane_id": pane_id.to_string(), "regex": "panic|error", "absent": true, "timeout_ms": 40}),
        )
        .await;
        assert_eq!(wait_absent["matched"], true);
        assert_eq!(wait_absent["absent"], true);

        let timeout = dispatch_err(
            &harness.router,
            "pane.wait_for",
            serde_json::json!({"pane_id": pane_id.to_string(), "text": "never-happens", "timeout_ms": 20, "poll_ms": 20}),
        )
        .await;
        assert_eq!(timeout.code, shux_rpc::ErrorCode::NotFound.code());
        assert!(
            timeout.data.unwrap()["last_capture_preview"]
                .as_str()
                .unwrap()
                .contains("agent-ready")
        );

        let resized = dispatch_ok(
            &harness.router,
            "pane.set_size",
            serde_json::json!({"pane_id": pane_id.to_string(), "cols": 24, "rows": 4}),
        )
        .await;
        assert_eq!(resized["cols"], 24);
        assert_eq!(resized["rows"], 4);
        {
            let state = harness.io.lock().await;
            let vt = state.vts.get(&pane_id).unwrap();
            assert_eq!(vt.grid().cols(), 24);
            assert_eq!(vt.grid().rows(), 4);
        }

        let bad_size = dispatch_err(
            &harness.router,
            "pane.set_size",
            serde_json::json!({"pane_id": pane_id.to_string(), "cols": 1001, "rows": 4}),
        )
        .await;
        assert_eq!(bad_size.code, shux_rpc::ErrorCode::InvalidParams.code());

        let running = dispatch_ok(
            &harness.router,
            "pane.run_command",
            serde_json::json!({
                "pane_id": pane_id.to_string(),
                "command": "printf",
                "args": ["hello world"],
                "async": true,
                "timeout": 5,
            }),
        )
        .await;
        assert_eq!(running["state"], "running");
        let command_id = running["command_id"].as_str().unwrap().to_string();
        let pty_command = String::from_utf8(writer_rx.recv().await.unwrap()).unwrap();
        assert!(pty_command.contains("printf"));
        assert!(pty_command.contains("hello"));
        assert!(pty_command.contains("SHUX_MAR"));

        let status = dispatch_ok(
            &harness.router,
            "pane.command_status",
            serde_json::json!({"command_id": command_id}),
        )
        .await;
        assert_eq!(status["state"], "running");

        let cancelled = dispatch_ok(
            &harness.router,
            "pane.command_cancel",
            serde_json::json!({"command_id": command_id}),
        )
        .await;
        assert_eq!(cancelled["state"], "cancelled");
        assert_eq!(writer_rx.recv().await.unwrap(), vec![0x03]);

        let status = dispatch_ok(
            &harness.router,
            "pane.command_status",
            serde_json::json!({"command_id": command_id}),
        )
        .await;
        assert_eq!(status["state"], "cancelled");

        let bad_command = dispatch_err(
            &harness.router,
            "pane.run_command",
            serde_json::json!({"pane_id": pane_id.to_string()}),
        )
        .await;
        assert_eq!(bad_command.code, shux_rpc::ErrorCode::InvalidParams.code());

        harness.stop().await;
    }
}
