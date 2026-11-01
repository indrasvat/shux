//! Pane output recording: the registry behind `pane.record.*` and the
//! `lens.run` cast recorder.
//!
//! A recording is a channel, a writer task and a supervisor. The PTY read loop
//! *admits* a chunk by taking a sender under the registry lock while the
//! recording is accepting; ending a recording drops the registry's sender, so
//! the writer stops only once every admitted chunk has reached it. The
//! supervisor publishes the final [`Outcome`] exactly once, then retires the
//! entry after [`PANE_RECORD_COMPLETED_TTL`].
//!
//! A start reserves its pane and path before the file is opened, and the
//! reservation is held until the open has returned, however the caller fares.
//! Every task a recording runs is on the registry's tracker, which daemon
//! shutdown drains.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use futures::FutureExt;
use futures::future::BoxFuture;
use shux_core::model::PaneId;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex, mpsc, watch};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::pane_io::PaneIoState;

pub(crate) const PANE_RECORD_CHANNEL_CAPACITY: usize = 128;
pub(crate) const PANE_RECORD_COMPLETED_TTL: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub(crate) enum PaneRecordChunk {
    /// A cast's header geometry, sent once when the recording starts accepting.
    Begin { cols: u16, rows: u16, at: Instant },
    /// Raw PTY output, stamped where it was read so backpressure never skews
    /// cast timing.
    Bytes { data: Vec<u8>, at: Instant },
    /// A pane resize: an asciinema `"r"` event in a cast; ignored in raw.
    Resize { cols: u16, rows: u16, at: Instant },
}

/// What the file holds. `Complete`: every admitted byte written and flushed.
/// `Aborted`: admitted bytes were dropped. `Error`: a write or flush failed.
/// Declared in rising severity: a worse status replaces a lesser one.
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub(crate) enum PaneRecordStatus {
    Recording,
    Complete,
    Aborted,
    Error,
}

impl PaneRecordStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            PaneRecordStatus::Recording => "recording",
            PaneRecordStatus::Complete => "complete",
            PaneRecordStatus::Error => "error",
            PaneRecordStatus::Aborted => "aborted",
        }
    }
}

/// Why a recording stopped accepting output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EndedBy {
    Stop,
    Duration,
    PaneExit,
    OwnerDisconnected,
    DaemonShutdown,
}

impl EndedBy {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            EndedBy::Stop => "stop",
            EndedBy::Duration => "duration",
            EndedBy::PaneExit => "pane_exit",
            EndedBy::OwnerDisconnected => "owner_disconnected",
            EndedBy::DaemonShutdown => "daemon_shutdown",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub(crate) status: PaneRecordStatus,
    pub(crate) bytes_written: u64,
    /// The first diagnostic recorded.
    pub(crate) error: Option<String>,
    /// `None` while accepting, and for a recording that failed or was rolled
    /// back with nothing having ended it.
    pub(crate) ended_by: Option<EndedBy>,
    /// Set once, by the supervisor; nothing changes after it.
    pub(crate) finalized: bool,
}

impl Outcome {
    pub(crate) fn to_json(&self, id: uuid::Uuid, path: &Path) -> serde_json::Value {
        serde_json::json!({
            "recording_id": id.to_string(),
            "path": path.display().to_string(),
            "bytes_written": self.bytes_written,
            "status": self.status.as_str(),
            "lossless": self.finalized && self.status == PaneRecordStatus::Complete,
            "error": self.error,
            "ended_by": self.ended_by.map(EndedBy::as_str),
            "finalized": self.finalized,
        })
    }
}

struct Control {
    outcome: watch::Sender<Outcome>,
    /// Bytes the writer has written, kept outside the watch so a supervisor
    /// reporting a crashed writer still has the count.
    progress: AtomicU64,
}

impl Control {
    fn new() -> Self {
        let (outcome, _) = watch::channel(Outcome {
            status: PaneRecordStatus::Recording,
            bytes_written: 0,
            error: None,
            ended_by: None,
            finalized: false,
        });
        Self {
            outcome,
            progress: AtomicU64::new(0),
        }
    }

    /// Record a flaw; a worse one replaces a lesser, the first diagnostic stays.
    fn degrade(&self, status: PaneRecordStatus, error: &str) {
        self.outcome.send_modify(|o| {
            if o.finalized {
                return;
            }
            if status > o.status {
                o.status = status;
            }
            o.error.get_or_insert_with(|| error.to_string());
        });
    }

    fn finalize(&self, writer: Result<(), String>) {
        if let Err(e) = &writer {
            self.degrade(PaneRecordStatus::Error, e);
        }
        let written = self.progress.load(Ordering::SeqCst);
        self.outcome.send_modify(|o| {
            o.status = o.status.max(PaneRecordStatus::Complete);
            o.bytes_written = written;
            o.finalized = true;
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Pane and path held while the file opens; nothing admitted yet.
    Reserved {
        cancelled: bool,
    },
    Accepting,
    /// The registry's sender is gone; the writer is finishing.
    Stopping,
    Finalized,
}

struct Entry {
    pane: Option<PaneId>,
    path: PathBuf,
    phase: Phase,
    sink: Option<mpsc::Sender<PaneRecordChunk>>,
    control: Arc<Control>,
}

impl Entry {
    fn live(&self) -> bool {
        self.phase != Phase::Finalized
    }
}

struct Registry {
    admitting: bool,
    entries: HashMap<uuid::Uuid, Entry>,
}

/// Where a recording's bytes go. Production opens a file; tests inject sinks.
pub(crate) type RecordSink = Box<dyn AsyncWrite + Send + Unpin>;

/// An opened sink, and how many bytes actually reached it where that can be
/// measured: after a failed write, a file holds some prefix of what it was
/// handed, and only the file can say how much.
pub(crate) struct Opened {
    pub(crate) sink: RecordSink,
    pub(crate) landed: Option<Box<dyn Fn() -> Option<u64> + Send>>,
}

/// Opens `path` for recording `id` (the id is for tests; files ignore it).
pub(crate) type Opener = Arc<
    dyn Fn(uuid::Uuid, PathBuf, bool) -> BoxFuture<'static, std::io::Result<Opened>> + Send + Sync,
>;

fn file_opener() -> Opener {
    Arc::new(|_id: uuid::Uuid, path: PathBuf, overwrite: bool| {
        async move {
            use nix::fcntl::{FcntlArg, OFlag, fcntl};
            let mut options = tokio::fs::OpenOptions::new();
            // Non-blocking only for the open: a FIFO nobody reads fails at
            // once (ENXIO) instead of parking the open, and the start, forever.
            options
                .write(true)
                .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK);
            if overwrite {
                options.create(true).truncate(true);
            } else {
                options.create_new(true);
            }
            let file = options.open(&path).await.map_err(|e| {
                if e.raw_os_error() == Some(nix::libc::ENXIO) {
                    std::io::Error::other("no process is reading this FIFO")
                } else {
                    e
                }
            })?;
            let flags = OFlag::from_bits_retain(fcntl(&file, FcntlArg::F_GETFL)?);
            fcntl(&file, FcntlArg::F_SETFL(flags - OFlag::O_NONBLOCK))?;
            // Its own descriptor, so the size is the open file's, whatever
            // later happens to the path.
            let probe = file.try_clone().await?.into_std().await;
            Ok(Opened {
                sink: Box::new(file),
                landed: Some(Box::new(move || {
                    probe
                        .metadata()
                        .ok()
                        .filter(|m| m.is_file())
                        .map(|m| m.len())
                })),
            })
        }
        .boxed()
    })
}

struct Shared {
    state: StdMutex<Registry>,
    tracker: TaskTracker,
    /// Cancelled once daemon shutdown has begun.
    root: CancellationToken,
    /// Cancelled by [`Recordings::drain`]: stop waiting out retention.
    retire: CancellationToken,
    opener: Opener,
    /// Chunks in flight per recording before its tee waits.
    channel: usize,
}

#[derive(Debug)]
pub(crate) enum RecordStartError {
    ShuttingDown,
    /// The pane ended while the start was opening its file.
    PaneGone,
    PaneBusy(uuid::Uuid),
    PathBusy(PathBuf),
    BadPath(String),
    Open(String),
}

impl RecordStartError {
    pub(crate) fn to_rpc(&self, pane: PaneId) -> shux_rpc::RpcError {
        let conflict = |resource: &str, name: String, extra: serde_json::Value| {
            let mut data = serde_json::json!({
                "resource": resource,
                "name": name,
                "detail": self.to_string(),
            });
            if let (Some(d), Some(e)) = (data.as_object_mut(), extra.as_object()) {
                d.extend(e.clone());
            }
            shux_rpc::RpcError::with_data(shux_rpc::ErrorCode::NameConflict, data)
        };
        match self {
            RecordStartError::PaneGone => {
                shux_rpc::RpcError::not_found("live pane PTY", &pane.to_string())
            }
            RecordStartError::PaneBusy(running) => conflict(
                "pane recording",
                pane.to_string(),
                serde_json::json!({ "recording_id": running.to_string() }),
            ),
            RecordStartError::PathBusy(path) => conflict(
                "record file",
                path.display().to_string(),
                serde_json::Value::Null,
            ),
            RecordStartError::BadPath(e) => shux_rpc::RpcError::invalid_params(e),
            RecordStartError::ShuttingDown | RecordStartError::Open(_) => {
                shux_rpc::RpcError::internal(&self.to_string())
            }
        }
    }
}

impl std::fmt::Display for RecordStartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecordStartError::ShuttingDown => f.write_str("daemon is shutting down"),
            RecordStartError::PaneGone => f.write_str("the pane ended"),
            RecordStartError::PaneBusy(id) => {
                write!(f, "a recording ({id}) is already running on this pane")
            }
            RecordStartError::PathBusy(p) => {
                write!(f, "another recording is writing {}", p.display())
            }
            RecordStartError::BadPath(e) | RecordStartError::Open(e) => f.write_str(e),
        }
    }
}

/// Every recording in the daemon. Cheap to clone; clones share the registry.
#[derive(Clone)]
pub(crate) struct Recordings {
    shared: Arc<Shared>,
}

impl Recordings {
    /// `root` is the daemon's shutdown token.
    pub(crate) fn new(root: CancellationToken) -> Self {
        Self::with_opener(root, file_opener(), PANE_RECORD_CHANNEL_CAPACITY)
    }

    pub(crate) fn with_opener(root: CancellationToken, opener: Opener, channel: usize) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: StdMutex::new(Registry {
                    admitting: true,
                    entries: HashMap::new(),
                }),
                tracker: TaskTracker::new(),
                root,
                retire: CancellationToken::new(),
                opener,
                channel,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Registry> {
        self.shared
            .state
            .lock()
            .expect("recording registry poisoned")
    }

    /// Reserve `pane` (when known) and `path`, open the file, and start its
    /// writer. The reservation is kept until the open returns even if this
    /// future is dropped; the result then rolls itself back.
    pub(crate) async fn prepare(
        &self,
        path: &Path,
        overwrite: bool,
        pane: Option<PaneId>,
    ) -> Result<Prepared, RecordStartError> {
        let key = path_key(path).await?;
        let id = uuid::Uuid::new_v4();
        let control = Arc::new(Control::new());
        let task = {
            let mut reg = self.lock();
            if !reg.admitting {
                return Err(RecordStartError::ShuttingDown);
            }
            if let Some(pane) = pane
                && let Some((running, _)) = reg
                    .entries
                    .iter()
                    .find(|(_, e)| e.live() && e.pane == Some(pane))
            {
                return Err(RecordStartError::PaneBusy(*running));
            }
            if reg.entries.values().any(|e| e.live() && e.path == key) {
                return Err(RecordStartError::PathBusy(key));
            }
            reg.entries.insert(
                id,
                Entry {
                    pane,
                    path: key.clone(),
                    phase: Phase::Reserved { cancelled: false },
                    sink: None,
                    control: control.clone(),
                },
            );
            // Tracked in the same critical section that passed admission, so
            // shutdown cannot drain an empty tracker while this open runs.
            let this = self.clone();
            self.shared
                .tracker
                .spawn(async move { this.open(id, key, overwrite, control).await })
        };
        task.await
            .unwrap_or_else(|e| Err(RecordStartError::Open(format!("record open failed: {e}"))))
    }

    async fn open(
        &self,
        id: uuid::Uuid,
        path: PathBuf,
        overwrite: bool,
        control: Arc<Control>,
    ) -> Result<Prepared, RecordStartError> {
        let opened =
            std::panic::AssertUnwindSafe((self.shared.opener)(id, path.clone(), overwrite))
                .catch_unwind()
                .await;
        let opened = match opened {
            Ok(Ok(opened)) => opened,
            failed => {
                self.lock().entries.remove(&id);
                let why = match failed {
                    Ok(Err(e)) => e.to_string(),
                    _ => "the opener panicked".to_string(),
                };
                return Err(RecordStartError::Open(format!(
                    "failed to open record file {}: {why}",
                    path.display()
                )));
            }
        };
        let (tx, rx) = mpsc::channel(self.shared.channel);
        let writer = self
            .shared
            .tracker
            .spawn(write_recording(rx, opened, control.clone()));
        let this = self.clone();
        self.shared
            .tracker
            .spawn(async move { this.supervise(id, writer, control).await });
        Ok(Prepared {
            recordings: self.clone(),
            id,
            path,
            sink: Some(tx),
        })
    }

    async fn supervise(
        &self,
        id: uuid::Uuid,
        writer: tokio::task::JoinHandle<Result<(), String>>,
        control: Arc<Control>,
    ) {
        let result = match writer.await {
            Ok(result) => result,
            Err(e) => Err(format!("pane recorder task failed: {e}")),
        };
        control.finalize(result);
        if let Some(entry) = self.lock().entries.get_mut(&id) {
            entry.phase = Phase::Finalized;
            entry.sink = None;
        }
        tokio::select! {
            () = retention() => {}
            () = self.shared.retire.cancelled() => {}
        }
        self.lock().entries.remove(&id);
    }

    /// Senders for every recording of `pane` that is accepting output.
    pub(crate) fn admit(&self, pane: PaneId) -> Vec<Admission> {
        self.lock()
            .entries
            .iter()
            .filter(|(_, e)| e.pane == Some(pane) && e.phase == Phase::Accepting)
            .filter_map(|(id, e)| {
                Some(Admission {
                    id: *id,
                    sink: e.sink.clone()?,
                    control: e.control.clone(),
                    settled: false,
                })
            })
            .collect()
    }

    /// Stop accepting output.
    pub(crate) fn stop(&self, id: uuid::Uuid, cause: EndedBy) {
        if let Some(entry) = self.lock().entries.get_mut(&id) {
            self.end(entry, cause);
        }
    }

    /// End every recording of `pane`.
    pub(crate) fn stop_pane(&self, pane: PaneId, cause: EndedBy) {
        let mut reg = self.lock();
        for entry in reg.entries.values_mut().filter(|e| e.pane == Some(pane)) {
            self.end(entry, cause);
        }
    }

    /// The one place a recording stops accepting. Shutdown, once begun, is
    /// the cause of every recording that ends after it.
    fn end(&self, entry: &mut Entry, cause: EndedBy) {
        match &mut entry.phase {
            Phase::Reserved { cancelled } => *cancelled = true,
            Phase::Accepting => {
                let cause = if self.shared.root.is_cancelled() {
                    EndedBy::DaemonShutdown
                } else {
                    cause
                };
                entry.control.outcome.send_modify(|o| {
                    o.ended_by.get_or_insert(cause);
                });
                entry.sink = None;
                entry.phase = Phase::Stopping;
            }
            Phase::Stopping | Phase::Finalized => {}
        }
    }

    pub(crate) fn watch(&self, id: uuid::Uuid) -> Option<(watch::Receiver<Outcome>, PathBuf)> {
        let reg = self.lock();
        let entry = reg.entries.get(&id)?;
        Some((entry.control.outcome.subscribe(), entry.path.clone()))
    }

    /// End the recording with `cause` once `trigger` completes, unless it
    /// has ended first: the owner's connection closing, or its duration.
    pub(crate) fn end_when(
        &self,
        id: uuid::Uuid,
        trigger: impl std::future::Future<Output = ()> + Send + 'static,
        cause: EndedBy,
    ) {
        let Some((mut outcome, _)) = self.watch(id) else {
            return;
        };
        let this = self.clone();
        self.shared.tracker.spawn(async move {
            tokio::select! {
                _ = outcome.wait_for(|o| o.finalized) => {}
                () = trigger => this.stop(id, cause),
                () = this.shared.retire.cancelled() => {}
            }
        });
    }

    /// Refuse new recordings from now on.
    pub(crate) fn close_admission(&self) {
        self.lock().admitting = false;
    }

    /// End every recording, reserved ones included.
    pub(crate) fn stop_all(&self, cause: EndedBy) {
        let mut reg = self.lock();
        for entry in reg.entries.values_mut() {
            self.end(entry, cause);
        }
    }

    /// Wait for every recording's writer to finish, up to `budget`. Returns
    /// the paths still unfinished when it ran out.
    pub(crate) async fn drain(&self, budget: Duration) -> Vec<PathBuf> {
        self.shared.retire.cancel();
        self.shared.tracker.close();
        if tokio::time::timeout(budget, self.shared.tracker.wait())
            .await
            .is_ok()
        {
            return Vec::new();
        }
        self.lock()
            .entries
            .values()
            .filter(|e| e.live())
            .map(|e| e.path.clone())
            .collect()
    }

    /// Id, pane and path of every live recording.
    #[cfg(any(test, fuzzing))]
    pub(crate) fn live(&self) -> Vec<(uuid::Uuid, Option<PaneId>, PathBuf)> {
        self.lock()
            .entries
            .iter()
            .filter(|(_, e)| e.live())
            .map(|(id, e)| (*id, e.pane, e.path.clone()))
            .collect()
    }

    #[cfg(any(test, fuzzing))]
    pub(crate) fn entry_count(&self) -> usize {
        self.lock().entries.len()
    }

    #[cfg(any(test, fuzzing))]
    pub(crate) fn task_count(&self) -> usize {
        self.shared.tracker.len()
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "a finished recording stays queryable for its TTL"
)]
async fn retention() {
    tokio::time::sleep(PANE_RECORD_COMPLETED_TTL).await;
}

#[expect(
    clippy::disallowed_methods,
    reason = "duration_ms is the recording's requested length"
)]
pub(crate) async fn requested_length(after: Duration) {
    tokio::time::sleep(after).await;
}

/// `path` with its parent resolved, which is how two spellings of one file
/// are recognised. Creates the parent, as the open would.
async fn path_key(path: &Path) -> Result<PathBuf, RecordStartError> {
    let name = path
        .file_name()
        .ok_or_else(|| RecordStartError::BadPath(format!("{} names no file", path.display())))?;
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    tokio::fs::create_dir_all(&parent).await.map_err(|e| {
        RecordStartError::Open(format!(
            "failed to create parent directory {}: {e}",
            parent.display()
        ))
    })?;
    let parent = tokio::fs::canonicalize(&parent).await.map_err(|e| {
        RecordStartError::Open(format!("failed to resolve {}: {e}", parent.display()))
    })?;
    Ok(parent.join(name))
}

/// An opened recording that admits nothing yet. Dropping it uncommitted
/// rolls it back: its writer finishes an empty file.
pub(crate) struct Prepared {
    recordings: Recordings,
    id: uuid::Uuid,
    /// The resolved path the file was opened at.
    path: PathBuf,
    sink: Option<mpsc::Sender<PaneRecordChunk>>,
}

impl Prepared {
    #[cfg(test)]
    pub(crate) fn id(&self) -> uuid::Uuid {
        self.id
    }

    /// Start admitting `pane`'s output. A cast is given its header geometry
    /// here, so no resize between reservation and now is lost.
    pub(crate) fn commit(
        mut self,
        pane: PaneId,
        cast_size: Option<(u16, u16)>,
    ) -> Result<(uuid::Uuid, PathBuf), RecordStartError> {
        let recordings = self.recordings.clone();
        let mut reg = recordings.lock();
        if !reg.admitting {
            return Err(RecordStartError::ShuttingDown);
        }
        if let Some((running, _)) = reg
            .entries
            .iter()
            .find(|(id, e)| **id != self.id && e.live() && e.pane == Some(pane))
        {
            return Err(RecordStartError::PaneBusy(*running));
        }
        let Some(entry) = reg.entries.get_mut(&self.id) else {
            return Err(RecordStartError::Open(
                "recording vanished before it started".into(),
            ));
        };
        // Only the pane's end cancels a reservation while admission is open.
        if entry.phase != (Phase::Reserved { cancelled: false }) {
            return Err(RecordStartError::PaneGone);
        }
        let sink = self
            .sink
            .take()
            .expect("an uncommitted recording holds its sender");
        if let Some((cols, rows)) = cast_size {
            sink.try_send(PaneRecordChunk::Begin {
                cols,
                rows,
                at: Instant::now(),
            })
            .expect("a fresh channel has room for its header");
        }
        entry.pane = Some(pane);
        entry.sink = Some(sink);
        entry.phase = Phase::Accepting;
        Ok((self.id, self.path.clone()))
    }
}

impl Drop for Prepared {
    fn drop(&mut self) {
        let Some(sink) = self.sink.take() else {
            return;
        };
        if let Some(entry) = self.recordings.lock().entries.get_mut(&self.id) {
            entry.phase = Phase::Stopping;
        }
        drop(sink);
    }
}

/// One chunk's place in one recording. Dropped before its send completed,
/// it marks the recording `aborted` before releasing its sender, so the
/// loss is on record before the writer can finish.
pub(crate) struct Admission {
    id: uuid::Uuid,
    sink: mpsc::Sender<PaneRecordChunk>,
    control: Arc<Control>,
    settled: bool,
}

impl Admission {
    #[cfg(any(test, fuzzing))]
    pub(crate) fn id(&self) -> uuid::Uuid {
        self.id
    }

    /// True if the chunk reached the writer's channel.
    pub(crate) async fn send_bytes(
        mut self,
        data: Vec<u8>,
        at: Instant,
        shutdown: &CancellationToken,
    ) -> bool {
        tokio::select! {
            // A failed send means the writer already failed; it reports why.
            sent = self.sink.send(PaneRecordChunk::Bytes { data, at }) => {
                self.settled = true;
                sent.is_ok()
            }
            () = shutdown.cancelled() => false,
        }
    }

    /// Best effort: a missing resize event is not lost output.
    pub(crate) fn try_send_resize(mut self, cols: u16, rows: u16, at: Instant) {
        if self
            .sink
            .try_send(PaneRecordChunk::Resize { cols, rows, at })
            .is_err()
        {
            tracing::debug!(recording = %self.id, "cast resize event dropped (recorder busy)");
        }
        self.settled = true;
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        if !self.settled {
            self.control.degrade(
                PaneRecordStatus::Aborted,
                "pane shutdown interrupted recorder backpressure",
            );
        }
    }
}

async fn write_recording(
    rx: mpsc::Receiver<PaneRecordChunk>,
    opened: Opened,
    control: Arc<Control>,
) -> Result<(), String> {
    let Opened { sink, landed } = opened;
    let written = std::panic::AssertUnwindSafe(write_chunks(rx, sink, &control))
        .catch_unwind()
        .await
        .unwrap_or_else(|_| Err("pane recorder writer panicked".to_string()));
    if written.is_err()
        && let Some(n) = landed.as_ref().and_then(|landed| landed())
    {
        control.progress.store(n, Ordering::SeqCst);
    }
    written
}

async fn write_chunks(
    mut rx: mpsc::Receiver<PaneRecordChunk>,
    mut sink: RecordSink,
    control: &Control,
) -> Result<(), String> {
    let mut cast: Option<shux_vt::CastWriter> = None;
    while let Some(chunk) = rx.recv().await {
        let line = match chunk {
            // Only a cast is sent a header; it makes the recording a cast.
            PaneRecordChunk::Begin { cols, rows, at } => {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                cast = Some(shux_vt::CastWriter::new(at));
                let header = serde_json::json!({
                    "version": 2, "width": cols, "height": rows, "timestamp": ts
                });
                write(
                    &mut sink,
                    control,
                    format!("{header}\n").as_bytes(),
                    "cast header",
                )
                .await?;
                continue;
            }
            PaneRecordChunk::Bytes { data, at } => match cast.as_mut() {
                Some(c) => match c.output_line(&data, at) {
                    Some(line) => format!("{line}\n").into_bytes(),
                    None => continue,
                },
                None => data,
            },
            PaneRecordChunk::Resize { cols, rows, at } => match cast.as_mut() {
                Some(c) => format!("{}\n", c.resize_line(cols, rows, at)).into_bytes(),
                None => continue,
            },
        };
        let what = if cast.is_some() {
            "cast chunk"
        } else {
            "record chunk"
        };
        write(&mut sink, control, &line, what).await?;
    }
    if let Some(line) = cast.as_mut().and_then(shux_vt::CastWriter::flush_line) {
        write(
            &mut sink,
            control,
            format!("{line}\n").as_bytes(),
            "cast tail",
        )
        .await?;
    }
    sink.flush()
        .await
        .map_err(|e| format!("failed to flush record file: {e}"))
}

async fn write(
    sink: &mut RecordSink,
    control: &Control,
    bytes: &[u8],
    what: &str,
) -> Result<(), String> {
    sink.write_all(bytes)
        .await
        .map_err(|e| format!("failed to write {what}: {e}"))?;
    // A file reports a write done before it lands; the flush waits for it,
    // so a chunk is counted once it reached the file. A chunk that lands only
    // in part is measured from the file instead (see `Opened::landed`).
    sink.flush()
        .await
        .map_err(|e| format!("failed to write {what}: {e}"))?;
    control
        .progress
        .fetch_add(bytes.len() as u64, Ordering::SeqCst);
    Ok(())
}

/// Tee `data` into every recording of `pane` that is accepting output, on
/// behalf of PTY incarnation `owner` (`None`: unconditionally). A task that no
/// longer owns the pane admits nothing.
pub(crate) async fn tee_pane_recorders(
    io_state: &Arc<Mutex<PaneIoState>>,
    pane_id: PaneId,
    owner: Option<u64>,
    data: &[u8],
    shutdown: &CancellationToken,
) {
    let admitted = {
        let state = io_state.lock().await;
        if owner.is_some_and(|inc| !state.is_current(pane_id, inc)) {
            return;
        }
        state.recordings.admit(pane_id)
    };
    // Stamped once for all sinks: when the bytes left the fd.
    let at = Instant::now();
    for admission in admitted {
        if shutdown.is_cancelled() {
            // Dropping the rest records their loss.
            return;
        }
        admission.send_bytes(data.to_vec(), at, shutdown).await;
    }
}

/// End `pane`'s recordings when PTY incarnation `owner` ends (`None`:
/// unconditionally). Only a successor's recordings are off limits: a pane torn
/// down has no incarnation left, and its recordings must still end.
pub(crate) async fn finish_pane_recorders(
    io_state: &Arc<Mutex<PaneIoState>>,
    pane_id: PaneId,
    owner: Option<u64>,
) {
    let state = io_state.lock().await;
    let successor = owner.is_some_and(|inc| {
        state
            .incarnations
            .get(&pane_id)
            .is_some_and(|&current| current != inc)
    });
    if !successor {
        state.recordings.stop_pane(pane_id, EndedBy::PaneExit);
    }
}

/// Tee a pane resize into every accepting recording; a cast writes an `"r"`
/// event, raw ignores it. Same channel as the output, from the same PTY task,
/// so it lands in order with the output around it.
pub(crate) async fn tee_pane_resize_recorders(
    io_state: &Arc<Mutex<PaneIoState>>,
    pane_id: PaneId,
    owner: Option<u64>,
    cols: u16,
    rows: u16,
) {
    let admitted = {
        let state = io_state.lock().await;
        if owner.is_some_and(|inc| !state.is_current(pane_id, inc)) {
            return;
        }
        state.recordings.admit(pane_id)
    };
    let at = Instant::now();
    for admission in admitted {
        admission.try_send_resize(cols, rows, at);
    }
}

/// Sinks and openers a test controls, shared by the unit tests and the
/// lifecycle fuzzer.
#[cfg(any(test, fuzzing))]
pub(crate) mod testing {
    use super::*;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::AsyncReadExt;

    /// What a sink has been given, and whether its reader is draining it.
    #[derive(Clone)]
    pub(crate) struct SinkState {
        pub(crate) written: Arc<StdMutex<Vec<u8>>>,
        draining: watch::Sender<bool>,
    }

    impl SinkState {
        /// Let writes through, or hold them (a full pipe then blocks the writer).
        pub(crate) fn set_draining(&self, on: bool) {
            self.draining.send_replace(on);
        }

        pub(crate) fn bytes(&self) -> Vec<u8> {
            self.written.lock().expect("sink log").clone()
        }
    }

    /// How a sink misbehaves.
    #[derive(Clone, Copy, Debug, Default)]
    pub(crate) struct Fault {
        /// Fail every write once this many bytes have been accepted.
        pub(crate) fail_after: Option<usize>,
        /// Panic in the writer once this many bytes have been accepted.
        pub(crate) panic_after: Option<usize>,
        /// Like a buffered file on a full disk: past this many bytes, writes
        /// are taken and lost, and the loss surfaces at the next flush.
        pub(crate) lose_after: Option<usize>,
    }

    struct FaultySink {
        inner: tokio::io::DuplexStream,
        fault: Fault,
        accepted: usize,
        lost: bool,
        /// What reached the pipe, as a file's size would say.
        landed: Arc<AtomicU64>,
    }

    impl AsyncWrite for FaultySink {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            if self.fault.panic_after.is_some_and(|n| self.accepted >= n) {
                panic!("injected writer panic");
            }
            if self.fault.fail_after.is_some_and(|n| self.accepted >= n) {
                return Poll::Ready(Err(std::io::Error::other("injected write failure")));
            }
            if self.fault.lose_after.is_some_and(|n| self.accepted >= n) {
                self.lost = true;
                return Poll::Ready(Ok(buf.len()));
            }
            let room = [
                self.fault.fail_after,
                self.fault.panic_after,
                self.fault.lose_after,
            ]
            .into_iter()
            .flatten()
            .map(|n| n - self.accepted)
            .min()
            .unwrap_or(buf.len())
            .min(buf.len())
            .max(1);
            let poll = Pin::new(&mut self.inner).poll_write(cx, &buf[..room]);
            if let Poll::Ready(Ok(n)) = poll {
                self.accepted += n;
                self.landed.fetch_add(n as u64, Ordering::SeqCst);
            }
            poll
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            if self.lost {
                return Poll::Ready(Err(std::io::Error::other(
                    "injected deferred write failure",
                )));
            }
            Pin::new(&mut self.inner).poll_flush(cx)
        }

        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }

    /// A sink of `capacity` bytes in flight, its state, and the task reading
    /// it (which runs until the writer drops the sink).
    pub(crate) fn sink(capacity: usize, fault: Fault) -> (Opened, SinkState) {
        let (writer, mut reader) = tokio::io::duplex(capacity.max(1));
        let (draining, mut on) = watch::channel(true);
        let state = SinkState {
            written: Arc::new(StdMutex::new(Vec::new())),
            draining,
        };
        let log = state.written.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            loop {
                if on.wait_for(|on| *on).await.is_err() {
                    return;
                }
                match reader.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => log.lock().expect("sink log").extend_from_slice(&buf[..n]),
                }
            }
        });
        let landed = Arc::new(AtomicU64::new(0));
        let sink = FaultySink {
            inner: writer,
            fault,
            accepted: 0,
            lost: false,
            landed: landed.clone(),
        };
        let opened = Opened {
            sink: Box::new(sink),
            landed: Some(Box::new(move || Some(landed.load(Ordering::SeqCst)))),
        };
        (opened, state)
    }

    /// An opener handing out [`sink`]s, one per open, each held at its open
    /// until the test lets it through. Records every open of every path.
    #[derive(Clone)]
    pub(crate) struct TestOpener {
        pub(crate) opens: Arc<StdMutex<Vec<(uuid::Uuid, PathBuf, SinkState)>>>,
        gate: watch::Sender<bool>,
        pub(crate) fault: Arc<StdMutex<Fault>>,
        pub(crate) fail_next_open: Arc<std::sync::atomic::AtomicBool>,
    }

    impl TestOpener {
        pub(crate) fn new() -> Self {
            Self {
                opens: Arc::new(StdMutex::new(Vec::new())),
                gate: watch::channel(true).0,
                fault: Arc::new(StdMutex::new(Fault::default())),
                fail_next_open: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            }
        }

        /// Hold opens (`false`) or let them complete (`true`).
        pub(crate) fn set_open(&self, on: bool) {
            self.gate.send_replace(on);
        }

        pub(crate) fn opener(&self, capacity: usize) -> Opener {
            let this = self.clone();
            Arc::new(move |id: uuid::Uuid, path: PathBuf, _overwrite: bool| {
                let this = this.clone();
                async move {
                    let mut gate = this.gate.subscribe();
                    let _ = gate.wait_for(|on| *on).await;
                    if this.fail_next_open.swap(false, Ordering::SeqCst) {
                        return Err(std::io::Error::other("injected open failure"));
                    }
                    let fault = *this.fault.lock().expect("fault");
                    let (sink, state) = sink(capacity, fault);
                    this.opens.lock().expect("opens").push((id, path, state));
                    Ok(sink)
                }
                .boxed()
            })
        }

        /// The sink recording `id` was opened with.
        pub(crate) fn of(&self, id: uuid::Uuid) -> Option<SinkState> {
            self.opens
                .lock()
                .expect("opens")
                .iter()
                .find(|(i, _, _)| *i == id)
                .map(|(_, _, s)| s.clone())
        }

        /// The sink of the most recent open of `path`.
        #[cfg(test)]
        pub(crate) fn last(&self, path: &Path) -> Option<SinkState> {
            self.opens
                .lock()
                .expect("opens")
                .iter()
                .rev()
                .find(|(_, p, _)| p == path)
                .map(|(_, _, s)| s.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{Fault, TestOpener};
    use super::*;

    struct Rig {
        dir: tempfile::TempDir,
        root: CancellationToken,
        opener: TestOpener,
        rec: Recordings,
    }

    fn rig(capacity: usize) -> Rig {
        let root = CancellationToken::new();
        let opener = TestOpener::new();
        let rec = Recordings::with_opener(
            root.clone(),
            opener.opener(capacity),
            PANE_RECORD_CHANNEL_CAPACITY,
        );
        Rig {
            dir: tempfile::tempdir().unwrap(),
            root,
            opener,
            rec,
        }
    }

    impl Rig {
        fn path(&self, name: &str) -> PathBuf {
            std::fs::canonicalize(self.dir.path()).unwrap().join(name)
        }

        async fn start(&self, name: &str, pane: PaneId) -> uuid::Uuid {
            self.rec
                .prepare(&self.path(name), true, Some(pane))
                .await
                .unwrap()
                .commit(pane, None)
                .unwrap()
                .0
        }

        async fn send(&self, pane: PaneId, data: &[u8]) {
            for a in self.rec.admit(pane) {
                a.send_bytes(data.to_vec(), Instant::now(), &CancellationToken::new())
                    .await;
            }
        }

        async fn ended(&self, id: uuid::Uuid) -> Outcome {
            let (mut rx, _) = self.rec.watch(id).expect("a known recording");
            tokio::time::timeout(Duration::from_secs(5), rx.wait_for(|o| o.finalized))
                .await
                .expect("the recording never finished")
                .unwrap()
                .clone()
        }

        /// Wait out retention; true once the registry holds nothing.
        async fn retired(&self) -> bool {
            tokio::time::advance(Duration::from_secs(61)).await;
            for _ in 0..64 {
                if self.rec.entry_count() == 0 {
                    return true;
                }
                tokio::task::yield_now().await;
            }
            false
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_recording_is_retired_after_its_ttl_however_it_ends() {
        for how in [
            "stop",
            "duration",
            "pane_exit",
            "owner",
            "rollback",
            "write_failure",
        ] {
            let r = rig(1 << 16);
            let pane = PaneId::new();
            if how == "write_failure" {
                r.opener.fault.lock().unwrap().fail_after = Some(4);
            }
            if how == "rollback" {
                let prepared = r.rec.prepare(&r.path("x"), true, Some(pane)).await.unwrap();
                let id = prepared.id();
                drop(prepared);
                assert_eq!(r.ended(id).await.ended_by, None, "a rollback has no cause");
            } else {
                let id = r.start("x", pane).await;
                match how {
                    "stop" => {
                        r.rec.stop(id, EndedBy::Stop);
                    }
                    "duration" => r.rec.end_when(
                        id,
                        requested_length(Duration::from_millis(5)),
                        EndedBy::Duration,
                    ),
                    "pane_exit" => r.rec.stop_pane(pane, EndedBy::PaneExit),
                    "owner" => {
                        let owner = CancellationToken::new();
                        r.rec.end_when(
                            id,
                            owner.clone().cancelled_owned(),
                            EndedBy::OwnerDisconnected,
                        );
                        owner.cancel();
                    }
                    "write_failure" => {
                        // The second tee finds the writer gone; the outcome
                        // must still name the writer's own failure.
                        r.send(pane, b"more than four").await;
                        r.send(pane, b"after").await;
                    }
                    _ => unreachable!(),
                }
                let out = r.ended(id).await;
                if how == "write_failure" {
                    assert_eq!(out.status, PaneRecordStatus::Error, "{out:?}");
                    assert_eq!(out.ended_by, None);
                    assert!(
                        out.error
                            .as_deref()
                            .is_some_and(|e| e.contains("injected write failure")),
                        "the writer's own error was masked: {out:?}"
                    );
                }
            }
            assert!(
                r.retired().await,
                "a recording ended by {how} was never retired"
            );
            assert_eq!(r.rec.task_count(), 0, "{how} left a task running");
        }
    }

    /// Shutdown has begun: whatever ends a recording now, the cause is the
    /// shutdown, so a command waiting on it cannot report a normal end.
    #[tokio::test(start_paused = true)]
    async fn after_shutdown_begins_every_recording_ends_by_it() {
        let r = rig(1 << 16);
        let (a, b, c) = (PaneId::new(), PaneId::new(), PaneId::new());
        let (ia, ib, ic) = (
            r.start("a", a).await,
            r.start("b", b).await,
            r.start("c", c).await,
        );
        r.rec.end_when(
            ia,
            requested_length(Duration::from_millis(5)),
            EndedBy::Duration,
        );
        r.root.cancel();
        r.rec.stop_pane(b, EndedBy::PaneExit);
        r.rec.stop(ic, EndedBy::Stop);
        for id in [ia, ib, ic] {
            assert_eq!(r.ended(id).await.ended_by, Some(EndedBy::DaemonShutdown));
        }
    }

    /// A cause settled before shutdown began is kept.
    #[tokio::test(start_paused = true)]
    async fn an_end_before_shutdown_keeps_its_cause() {
        let r = rig(1 << 16);
        let pane = PaneId::new();
        let id = r.start("a", pane).await;
        r.rec.stop_pane(pane, EndedBy::PaneExit);
        r.root.cancel();
        r.rec.stop_all(EndedBy::DaemonShutdown);
        assert_eq!(r.ended(id).await.ended_by, Some(EndedBy::PaneExit));
    }

    /// Drain returns once every admitted byte is written, not before.
    #[tokio::test]
    async fn drain_waits_for_admitted_bytes_to_reach_the_sink() {
        let r = rig(8);
        let pane = PaneId::new();
        let id = r.start("a", pane).await;
        let sink = r.opener.last(&r.path("a")).unwrap();
        sink.set_draining(false);
        let payload = vec![b'x'; 4096];
        let rec = r.rec.clone();
        let tee = tokio::spawn(async move {
            for a in rec.admit(pane) {
                a.send_bytes(payload.clone(), Instant::now(), &CancellationToken::new())
                    .await;
            }
        });
        tokio::task::yield_now().await;
        r.rec.close_admission();
        r.rec.stop_all(EndedBy::DaemonShutdown);
        let release = tokio::spawn({
            let sink = sink.clone();
            async move {
                tokio::task::yield_now().await;
                sink.set_draining(true);
            }
        });
        // Drain retires what it finishes, so subscribe first.
        let (outcome, _) = r.rec.watch(id).unwrap();
        let unfinished = r.rec.drain(Duration::from_secs(10)).await;
        tee.await.unwrap();
        release.await.unwrap();
        assert!(unfinished.is_empty(), "{unfinished:?}");
        let out = outcome.borrow().clone();
        assert!(out.finalized, "drain returned before the writer finished");
        assert_eq!(
            sink.bytes().len(),
            4096,
            "drain returned with admitted bytes unwritten"
        );
    }

    /// A sink that never drains: drain gives up at its budget and names it.
    #[tokio::test(start_paused = true)]
    async fn drain_names_what_it_could_not_finish() {
        let r = rig(8);
        let pane = PaneId::new();
        r.start("stuck", pane).await;
        r.opener.last(&r.path("stuck")).unwrap().set_draining(false);
        let rec = r.rec.clone();
        tokio::spawn(async move {
            for a in rec.admit(pane) {
                a.send_bytes(vec![0; 64], Instant::now(), &CancellationToken::new())
                    .await;
            }
        });
        tokio::task::yield_now().await;
        r.rec.stop_all(EndedBy::DaemonShutdown);
        assert_eq!(
            r.rec.drain(Duration::from_secs(3)).await,
            vec![r.path("stuck")]
        );
    }

    /// An admission dropped before its send went through is a loss, on record
    /// before the writer can call the file complete.
    #[tokio::test]
    async fn an_admitted_chunk_that_never_went_through_aborts_the_recording() {
        let r = rig(1 << 16);
        let pane = PaneId::new();
        let id = r.start("a", pane).await;
        let admitted = r.rec.admit(pane);
        r.rec.stop(id, EndedBy::Stop);
        drop(admitted);
        assert_eq!(r.ended(id).await.status, PaneRecordStatus::Aborted);
    }

    /// The reservation holds the pane and path until the open returns, even
    /// when the start that made it is gone.
    #[tokio::test]
    async fn a_dropped_start_keeps_its_path_until_its_open_returns() {
        let r = rig(1 << 16);
        let (a, b) = (PaneId::new(), PaneId::new());
        r.opener.set_open(false);
        let first = {
            let rec = r.rec.clone();
            let path = r.path("p");
            tokio::spawn(async move { rec.prepare(&path, true, Some(a)).await })
        };
        let rec = r.rec.clone();
        shux_testkit::wait_until_async("the first start to reserve", || {
            let rec = rec.clone();
            async move { rec.live().iter().filter(|(_, p, _)| *p == Some(a)).count() == 1 }
        })
        .await;
        first.abort();
        let _ = first.await;
        let second = r.rec.prepare(&r.path("p"), true, Some(b)).await;
        assert!(
            matches!(second, Err(RecordStartError::PathBusy(_))),
            "a second start took the path while the first open was in flight: {:?}",
            second.map(|p| p.id())
        );
        r.opener.set_open(true);
        shux_testkit::wait_until_async("the abandoned start to roll back", || {
            let rec = rec.clone();
            async move { rec.live().iter().filter(|(_, p, _)| *p == Some(a)).count() == 0 }
        })
        .await;
        assert!(r.rec.prepare(&r.path("p"), true, Some(b)).await.is_ok());
        assert_eq!(
            r.opener.opens.lock().unwrap().len(),
            2,
            "one open per start that got the path"
        );
    }

    /// A start already past admission when shutdown closes it is tracked:
    /// drain waits for it, and it never begins accepting.
    #[tokio::test]
    async fn a_start_in_flight_at_shutdown_is_drained_and_refused() {
        let r = rig(1 << 16);
        let pane = PaneId::new();
        r.opener.set_open(false);
        let start = {
            let rec = r.rec.clone();
            let path = r.path("late");
            tokio::spawn(async move {
                let prepared = rec.prepare(&path, true, Some(pane)).await?;
                prepared.commit(pane, None)
            })
        };
        let rec = r.rec.clone();
        shux_testkit::wait_until_async("the start to reserve", || {
            let rec = rec.clone();
            async move {
                rec.live()
                    .iter()
                    .filter(|(_, p, _)| *p == Some(pane))
                    .count()
                    == 1
            }
        })
        .await;
        r.rec.close_admission();
        r.rec.stop_all(EndedBy::DaemonShutdown);
        let drain = {
            let rec = r.rec.clone();
            tokio::spawn(async move { rec.drain(Duration::from_secs(10)).await })
        };
        tokio::task::yield_now().await;
        assert!(
            !drain.is_finished(),
            "drain did not wait for the open in flight"
        );
        r.opener.set_open(true);
        assert!(drain.await.unwrap().is_empty());
        assert!(matches!(
            start.await.unwrap(),
            Err(RecordStartError::ShuttingDown)
        ));
        assert_eq!(
            r.rec
                .live()
                .iter()
                .filter(|(_, p, _)| *p == Some(pane))
                .count(),
            0
        );
    }

    /// A sink that takes a write and loses it, as a buffered file on a full
    /// disk does: `bytes_written` counts only what landed.
    #[tokio::test]
    async fn bytes_written_counts_only_what_landed() {
        let r = rig(1 << 16);
        let pane = PaneId::new();
        *r.opener.fault.lock().unwrap() = Fault {
            lose_after: Some(7),
            ..Fault::default()
        };
        let id = r.start("a", pane).await;
        r.send(pane, b"12345").await;
        // Lands two bytes, then fails.
        r.send(pane, b"67890").await;
        r.rec.stop(id, EndedBy::Stop);
        let out = r.ended(id).await;
        let landed = r.opener.of(id).unwrap();
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        assert_eq!(out.status, PaneRecordStatus::Error, "{out:?}");
        assert_eq!(
            out.bytes_written,
            landed.bytes().len() as u64,
            "bytes_written counts bytes that never landed: {out:?}"
        );
    }

    /// A writer that panics still finalizes, with the bytes it wrote.
    #[tokio::test]
    async fn a_writer_panic_is_reported_with_its_progress() {
        let r = rig(1 << 16);
        let pane = PaneId::new();
        *r.opener.fault.lock().unwrap() = Fault {
            panic_after: Some(5),
            ..Fault::default()
        };
        let id = r.start("a", pane).await;
        r.send(pane, b"12345").await;
        r.send(pane, b"678").await;
        let out = r.ended(id).await;
        assert_eq!(out.status, PaneRecordStatus::Error, "{out:?}");
        assert_eq!(out.bytes_written, 5);
    }
}
