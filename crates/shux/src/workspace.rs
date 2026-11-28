//! Keeping a daemon's workspace across restarts (issue #329): the writer that
//! saves it, the close protocol that keeps closed things closed, and the boot
//! step that brings it back.
//!
//! Three rules hold everywhere here:
//! - a closed session, window or pane never comes back: when saving is unsure,
//!   less is restored, never something the user closed;
//! - no state problem stops a daemon from starting;
//! - only the daemon holding a workspace's lock restores or writes it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use shux_core::graph::{CloseGuard, Removal, SessionGraph, SessionGraphSnapshot};
use shux_core::model::PaneId;
use shux_core::persist::{self, PaneExtras, RestoreError, Restored};
use tokio::sync::{mpsc, oneshot, watch};

use crate::workspace_store::{OpenError, Store};

/// Changes are saved this long after the last one...
const DEBOUNCE: Duration = Duration::from_millis(250);
/// ...but never later than this after the first unsaved one.
const MAX_LATENCY: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(30);

/// How saving stands, for `daemon status`.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct PersistStatus {
    /// `ok`, `failing` or `disabled`.
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
    /// The graph version the saved file holds.
    pub durable_version: u64,
    pub writes: u64,
    pub restored_panes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quarantined: Option<String>,
}

enum Msg {
    Tombstone {
        id: u64,
        removal: Removal,
        done: oneshot::Sender<()>,
    },
    Settle {
        id: u64,
    },
    Seal {
        snapshot: Arc<SessionGraphSnapshot>,
        done: oneshot::Sender<()>,
    },
    RestoreFinished,
}

struct Shared {
    tx: Option<mpsc::UnboundedSender<Msg>>,
    status: Mutex<PersistStatus>,
    durable: watch::Sender<u64>,
    extras: Mutex<HashMap<PaneId, PaneExtras>>,
    /// Panes whose shell has started, or that were restored.
    admitted: Mutex<std::collections::HashSet<PaneId>>,
    extras_changed: tokio::sync::Notify,
    next_tombstone: std::sync::atomic::AtomicU64,
}

/// A handle on a daemon's workspace persistence. Cheap to clone; a disabled
/// one turns every call into a no-op.
#[derive(Clone)]
pub struct Persist(Arc<Shared>);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Persist {
    pub fn disabled(reason: impl Into<String>) -> Self {
        Self(Arc::new(Shared {
            tx: None,
            status: Mutex::new(PersistStatus {
                state: "disabled",
                reason: Some(reason.into()),
                ..Default::default()
            }),
            durable: watch::Sender::new(0),
            extras: Mutex::default(),
            admitted: Mutex::default(),
            extras_changed: tokio::sync::Notify::new(),
            next_tombstone: Default::default(),
        }))
    }

    pub fn status(&self) -> PersistStatus {
        lock(&self.0.status).clone()
    }

    pub fn is_enabled(&self) -> bool {
        self.0.tx.is_some()
    }

    /// The graph version the saved workspace holds.
    pub fn durable(&self) -> watch::Receiver<u64> {
        self.0.durable.subscribe()
    }

    fn update(&self, pane: PaneId, f: impl FnOnce(&mut PaneExtras)) {
        if !self.is_enabled() {
            return;
        }
        let changed = {
            let mut map = lock(&self.0.extras);
            let entry = map.entry(pane).or_default();
            let before = entry.clone();
            f(entry);
            *entry != before
        };
        if changed {
            self.0.extras_changed.notify_one();
        }
    }

    /// A shell started in `pane`: the next daemon reaps it if a `kill -9`
    /// leaves it running.
    pub fn note_process(&self, pane: PaneId, pid: u32) {
        if !self.is_enabled() {
            return;
        }
        lock(&self.0.admitted).insert(pane);
        let start_token = crate::lens_scratch::process_start_token(pid);
        self.update(pane, |e| {
            e.process = start_token.map(|start_token| persist::ProcessId {
                pid: pid as i32,
                start_token,
            });
        });
    }

    /// The pane's shell is now in `cwd`.
    pub fn note_cwd(&self, pane: PaneId, cwd: PathBuf) {
        self.update(pane, |e| e.last_cwd = Some(cwd));
    }

    /// Forget panes no longer in the graph.
    pub fn retain_panes(&self, keep: impl Fn(&PaneId) -> bool) {
        lock(&self.0.extras).retain(|p, _| keep(p));
        lock(&self.0.admitted).retain(|p| keep(p));
    }

    /// Every pane's shell, to sample.
    pub fn processes(&self) -> Vec<(PaneId, persist::ProcessId)> {
        lock(&self.0.extras)
            .iter()
            .filter_map(|(p, e)| e.process.map(|pr| (*p, pr)))
            .collect()
    }

    pub fn extras(&self, pane: PaneId) -> PaneExtras {
        lock(&self.0.extras).get(&pane).cloned().unwrap_or_default()
    }

    /// Every restored pane's shell has started or failed: a crash from here on
    /// is no longer a crash during restore.
    pub fn restore_finished(&self) {
        if let Some(tx) = &self.0.tx {
            let _ = tx.send(Msg::RestoreFinished);
        }
    }

    /// The final save: `snapshot` is the sealed graph. Bounded, because a
    /// stuck disk must not keep a daemon from stopping.
    pub async fn seal(&self, snapshot: Arc<SessionGraphSnapshot>, budget: Duration) {
        let Some(tx) = &self.0.tx else { return };
        let (done, saved) = oneshot::channel();
        if tx.send(Msg::Seal { snapshot, done }).is_ok()
            && tokio::time::timeout(budget, saved).await.is_err()
        {
            tracing::warn!("workspace: the final save did not finish within {budget:?}");
        }
    }
}

/// Every close is saved before it is made. Until it has committed, every save
/// leaves the removed things out, so no save racing it can put them back. A
/// close is never refused for a storage error: if the removal cannot be
/// saved, the saved workspace is deleted instead.
impl CloseGuard for Persist {
    fn before_close(
        &self,
        removal: Removal,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = u64> + Send + '_>> {
        Box::pin(async move {
            let id = self
                .0
                .next_tombstone
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let Some(tx) = &self.0.tx else { return id };
            let (done, saved) = oneshot::channel();
            if tx.send(Msg::Tombstone { id, removal, done }).is_ok() {
                // A writer that has gone (sealed) saves nothing more anyway.
                let _ = saved.await;
            }
            id
        })
    }

    fn after_close(&self, ticket: u64) {
        if let Some(tx) = &self.0.tx {
            let _ = tx.send(Msg::Settle { id: ticket });
        }
    }
}

struct Writer {
    /// The debounce and the most a change waits (`DEBOUNCE`, `MAX_LATENCY`).
    timing: (Duration, Duration),
    store: Arc<Store>,
    socket: String,
    shared: Arc<Shared>,
    state: Arc<arc_swap::ArcSwap<SessionGraphSnapshot>>,
    tombstones: Vec<(u64, Removal)>,
    last: Option<Vec<u8>>,
    /// Set when a save could not happen: nothing more is written.
    poisoned: bool,
}

/// Run a store operation off the runtime: every one of them syncs to disk.
async fn blocking(
    store: &Arc<Store>,
    op: impl FnOnce(&Store) -> std::io::Result<()> + Send + 'static,
) -> std::io::Result<()> {
    let store = Arc::clone(store);
    tokio::task::spawn_blocking(move || op(&store))
        .await
        .map_err(std::io::Error::other)?
}

impl Writer {
    fn current(&self) -> Arc<SessionGraphSnapshot> {
        self.state.load_full()
    }

    /// The workspace `snapshot` makes once every pending removal is applied.
    /// A pane is saved only once its shell has started (or it was restored):
    /// a create or split that is rolled back for a failed spawn is never on
    /// disk to come back.
    fn bytes_for(&self, snapshot: &SessionGraphSnapshot) -> Vec<u8> {
        let shared = &self.shared;
        let extras = |p: PaneId| lock(&shared.extras).get(&p).cloned().unwrap_or_default();
        let provisional: Vec<PaneId> = {
            let admitted = lock(&shared.admitted);
            snapshot
                .panes
                .keys()
                .filter(|p| !admitted.contains(p))
                .copied()
                .collect()
        };
        let ws = if self.tombstones.is_empty() && provisional.is_empty() {
            persist::capture(snapshot, &self.socket, &extras)
        } else {
            let (g, _) = SessionGraph::from_snapshot(snapshot.clone(), None);
            for (_, r) in &self.tombstones {
                r.apply(&g);
            }
            for p in provisional {
                Removal::PaneCascade(p).apply(&g);
            }
            persist::capture(&g.current(), &self.socket, &extras)
        };
        persist::encode(&ws)
    }

    /// Save `snapshot`; `Err` leaves the previous file in place.
    async fn save(&mut self, snapshot: &SessionGraphSnapshot) -> std::io::Result<()> {
        if self.poisoned {
            return Ok(());
        }
        let bytes = self.bytes_for(snapshot);
        if self.last.as_deref() != Some(&bytes[..]) {
            let written = bytes.clone();
            blocking(&self.store, move |s| s.write_workspace(&written)).await?;
            self.last = Some(bytes);
            lock(&self.shared.status).writes += 1;
        }
        self.shared.durable.send_replace(snapshot.version);
        let mut st = lock(&self.shared.status);
        st.durable_version = snapshot.version;
        if st.state == "failing" {
            st.state = "ok";
            st.reason = None;
        }
        Ok(())
    }

    async fn clear_marker(&self) {
        if let Err(e) = blocking(&self.store, Store::clear_restoring).await {
            tracing::warn!("workspace: could not clear the restore marker: {e}");
        }
    }

    fn fail(&self, e: &std::io::Error) {
        tracing::warn!("workspace: could not save: {e}");
        let mut st = lock(&self.shared.status);
        st.state = "failing";
        st.reason = Some(e.to_string());
    }

    /// A removal that could not be saved: delete the saved workspace, so the
    /// next start restores nothing rather than the closed thing.
    async fn forget(&mut self, e: &std::io::Error) {
        self.fail(e);
        self.last = None;
        if let Err(e2) = blocking(&self.store, Store::remove_workspace).await {
            tracing::error!("workspace: could not delete it after a failed save either: {e2}");
            self.poisoned = true;
            let mut st = lock(&self.shared.status);
            st.state = "disabled";
            st.reason = Some(format!("cannot save ({e}) or delete ({e2}) the workspace"));
        }
    }

    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Msg>, commits: watch::Receiver<u64>) {
        let mut first_dirty: Option<Instant> = None;
        let mut last_change = Instant::now();
        let mut retry = Duration::ZERO;
        // The graph goes away at shutdown before the final save is asked
        // for: the writer outlives it, and stops only at that save.
        let mut commits = Some(commits);
        loop {
            let deadline = first_dirty.map(|first| {
                (last_change + self.timing.0)
                    .min(first + self.timing.1)
                    .max(Instant::now() + retry)
            });
            tokio::select! {
                msg = rx.recv() => match msg {
                    None => return,
                    Some(Msg::Tombstone { id, removal, done }) => {
                        self.tombstones.push((id, removal));
                        let snap = self.current();
                        if let Err(e) = self.save(&snap).await {
                            self.forget(&e).await;
                        }
                        let _ = done.send(());
                    }
                    Some(Msg::Settle { id }) => {
                        self.tombstones.retain(|(t, _)| *t != id);
                        first_dirty.get_or_insert_with(Instant::now);
                        last_change = Instant::now();
                    }
                    Some(Msg::Seal { snapshot, done }) => {
                        if let Err(e) = self.save(&snapshot).await {
                            self.fail(&e);
                        }
                        // An orderly stop, even one mid-restore, is not a crash.
                        self.clear_marker().await;
                        let _ = done.send(());
                        return;
                    }
                    Some(Msg::RestoreFinished) => self.clear_marker().await,
                },
                changed = changed(&mut commits) => {
                    if changed {
                        first_dirty.get_or_insert_with(Instant::now);
                        last_change = Instant::now();
                    } else {
                        commits = None;
                    }
                }
                () = self.shared.extras_changed.notified() => {
                    first_dirty.get_or_insert_with(Instant::now);
                    last_change = Instant::now();
                }
                () = sleep_until(deadline) => {
                    let snap = self.current();
                    match self.save(&snap).await {
                        Ok(()) => {
                            first_dirty = None;
                            retry = Duration::ZERO;
                        }
                        Err(e) => {
                            self.fail(&e);
                            retry = (retry * 2).clamp(Duration::from_secs(1), RETRY_MAX);
                        }
                    }
                }
            }
        }
    }
}

/// The next commit, or `false` once the graph is gone (and never again).
async fn changed(commits: &mut Option<watch::Receiver<u64>>) -> bool {
    match commits {
        Some(rx) => rx.changed().await.is_ok(),
        None => std::future::pending().await,
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        #[expect(
            clippy::disallowed_methods,
            reason = "the writer's debounce is a deadline by design: it saves a burst of changes once"
        )]
        Some(d) => tokio::time::sleep_until(d.into()).await,
        None => std::future::pending().await,
    }
}

/// What a booting daemon gets from its state directory.
pub struct Boot {
    pub restored: Option<Restored>,
    owned: Result<(Store, PersistStatus), String>,
    socket: String,
}

/// Why a daemon must not start.
#[derive(Debug, thiserror::Error)]
pub enum BootError {
    #[error(
        "another shux daemon (pid {holder}) still owns the workspace for this socket; \
         stop it first (`shux daemon stop`)"
    )]
    Held { holder: String },
}

/// How long a successor waits for a predecessor that lost its runtime dir to
/// save its workspace and exit.
const HANDOFF: Duration = Duration::from_secs(10);

/// Take this daemon's state directory and read its workspace. Never fails for
/// a storage problem: those boot empty, with persistence disabled and the
/// reason in `daemon status`. Fails only when a live daemon owns the state.
pub async fn boot(socket_key: &Path, enabled: bool) -> Result<Boot, BootError> {
    let socket = socket_key.to_string_lossy().into_owned();
    let disabled = |reason: String| Boot {
        restored: None,
        owned: Err(reason),
        socket: socket.clone(),
    };
    if !enabled {
        return Ok(disabled("turned off ([restore] workspace = false)".into()));
    }
    let Some(root) = crate::workspace_store::state_root() else {
        return Ok(disabled(
            "no state directory: neither XDG_STATE_HOME nor HOME is set".into(),
        ));
    };
    boot_at(&root, socket_key).await
}

async fn boot_at(root: &Path, socket_key: &Path) -> Result<Boot, BootError> {
    let socket = socket_key.to_string_lossy().into_owned();
    let disabled = |reason: String| Boot {
        restored: None,
        owned: Err(reason),
        socket: socket.clone(),
    };
    let key = crate::workspace_store::key_for(socket_key);
    let started = tokio::time::Instant::now();
    let store = loop {
        match Store::open(root, &key) {
            Ok(store) => break store,
            Err(OpenError::Unusable(e)) => {
                return Ok(disabled(format!("cannot use {}: {e}", root.display())));
            }
            Err(OpenError::Held { holder }) => {
                if started.elapsed() >= HANDOFF {
                    return Err(BootError::Held {
                        holder: holder.map_or("unknown".into(), |p| p.to_string()),
                    });
                }
                #[expect(
                    clippy::disallowed_methods,
                    reason = "polling a lock another process holds: there is no event to wait on"
                )]
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    };
    let mut status = PersistStatus {
        state: "ok",
        dir: Some(store.dir().to_path_buf()),
        ..Default::default()
    };
    let restored = match read(&store, &socket) {
        Ok(r) => r,
        Err(Unreadable::Quarantined { path, reason }) => {
            status.quarantined = Some(format!("{reason}: {}", path.display()));
            None
        }
        Err(Unreadable::Stuck(reason)) => {
            // The file could not be read or set aside: leave it untouched as
            // evidence, and never write over it.
            return Ok(disabled(reason));
        }
    };
    status.restored_panes = restored.as_ref().map_or(0, |r| r.snapshot.panes.len());
    Ok(Boot {
        restored,
        owned: Ok((store, status)),
        socket,
    })
}

enum Unreadable {
    Quarantined { path: PathBuf, reason: String },
    Stuck(String),
}

fn read(store: &Store, socket: &str) -> Result<Option<Restored>, Unreadable> {
    let stuck = |what: &str, e: &dyn std::fmt::Display| Unreadable::Stuck(format!("{what}: {e}"));
    let bytes = match store.read_workspace(persist::MAX_FILE_BYTES) {
        Ok(None) => {
            store
                .clear_restoring()
                .map_err(|e| stuck("cannot clear the restore marker", &e))?;
            return Ok(None);
        }
        Ok(Some(b)) => b,
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            return quarantine(store, &e.to_string());
        }
        Err(e) => return Err(stuck("cannot read the workspace", &e)),
    };
    let digest = crate::workspace_store::digest(&bytes);
    let marker = store
        .restoring_digest()
        .map_err(|e| stuck("cannot read the restore marker", &e))?;
    if marker.as_deref() == Some(digest.as_str()) {
        return quarantine(store, "crashed-during-restore");
    }
    // Marked before decoding, so a decoder that crashes is caught next time.
    store
        .mark_restoring(&digest)
        .map_err(|e| stuck("cannot write the restore marker", &e))?;
    match persist::decode(&bytes, socket) {
        Ok(ws) => Ok(Some(persist::rebuild(&ws))),
        Err(e) => {
            let reason = match &e {
                RestoreError::FutureSchema { .. } => "future-schema",
                RestoreError::WrongSocket { .. } => "wrong-socket",
                RestoreError::TooLarge(_) => "too-large",
                RestoreError::Decode(_) | RestoreError::Invalid(_) => "invalid",
            };
            tracing::warn!("workspace: refusing the saved workspace: {e}");
            quarantine(store, reason)
        }
    }
}

fn quarantine(store: &Store, reason: &str) -> Result<Option<Restored>, Unreadable> {
    let path = store.quarantine(reason).map_err(|e| {
        Unreadable::Stuck(format!(
            "cannot set aside a refused workspace ({reason}): {e}"
        ))
    })?;
    store
        .clear_restoring()
        .map_err(|e| Unreadable::Stuck(format!("cannot clear the restore marker: {e}")))?;
    tracing::warn!("workspace: set aside as {}", path.display());
    Err(Unreadable::Quarantined {
        path,
        reason: reason.to_string(),
    })
}

impl Boot {
    /// Start saving. `graph` must be the graph the daemon serves, before its
    /// loop starts, so no commit is missed.
    /// `extras` are the restored panes' own (`Restored::extras`).
    pub fn start(
        self,
        graph: &SessionGraph,
        state: Arc<arc_swap::ArcSwap<SessionGraphSnapshot>>,
        extras: HashMap<PaneId, PaneExtras>,
    ) -> Persist {
        self.start_timed(graph, state, extras, (DEBOUNCE, MAX_LATENCY))
    }

    fn start_timed(
        self,
        graph: &SessionGraph,
        state: Arc<arc_swap::ArcSwap<SessionGraphSnapshot>>,
        extras: HashMap<PaneId, PaneExtras>,
        timing: (Duration, Duration),
    ) -> Persist {
        let (store, status) = match self.owned {
            Ok(owned) => owned,
            Err(reason) => return Persist::disabled(reason),
        };
        let (tx, rx) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            tx: Some(tx),
            status: Mutex::new(status),
            durable: watch::Sender::new(0),
            admitted: Mutex::new(extras.keys().copied().collect()),
            extras: Mutex::new(extras),
            extras_changed: tokio::sync::Notify::new(),
            next_tombstone: Default::default(),
        });
        let writer = Writer {
            store: Arc::new(store),
            socket: self.socket,
            shared: Arc::clone(&shared),
            state,
            tombstones: Vec::new(),
            last: None,
            poisoned: false,
            timing,
        };
        tokio::spawn(writer.run(rx, graph.subscribe_commits()));
        Persist(shared)
    }
}

/// How often every live pane's shell is asked where it is.
const SAMPLE_EVERY: Duration = Duration::from_secs(2);

/// Ask every pane's shell for its cwd and record what changed.
pub(crate) async fn sample_cwds(
    io_state: &Arc<tokio::sync::Mutex<crate::pane_io::PaneIoState>>,
    graph: &shux_core::graph::GraphHandle,
) {
    let persist = io_state.lock().await.persist.clone();
    if !persist.is_enabled() {
        return;
    }
    let snap = graph.snapshot();
    persist.retain_panes(|p| snap.panes.contains_key(p));
    let procs = persist.processes();
    let found = tokio::task::spawn_blocking(move || {
        procs
            .into_iter()
            .filter_map(|(pane, pr)| {
                let pid = u32::try_from(pr.pid).ok()?;
                crate::pane_cwd::sample(pid, pr.start_token).map(|cwd| (pane, cwd))
            })
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    for (pane, cwd) in found {
        persist.note_cwd(pane, cwd);
    }
}

/// Sample on a fixed cadence, and as soon as a shell reports a command
/// boundary: that is when a `cd` lands.
pub(crate) async fn run_cwd_sampler(
    io_state: Arc<tokio::sync::Mutex<crate::pane_io::PaneIoState>>,
    graph: shux_core::graph::GraphHandle,
    cancel: tokio_util::sync::CancellationToken,
) {
    let mut boundaries = io_state.lock().await.boundaries_moved.subscribe();
    let mut tick = tokio::time::interval(SAMPLE_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            _ = tick.tick() => {}
            changed = boundaries.changed() => {
                if changed.is_err() {
                    return;
                }
            }
        }
        sample_cwds(&io_state, &graph).await;
    }
}

const NOTICE: &str =
    "\x1b[2mshux: restored after a daemon restart (scrollback was not kept)\x1b[0m\r\n";
const RESTORE_CONCURRENCY: usize = 8;

/// Start a shell in every restored pane: first reap whatever shells a killed
/// daemon left running, then start each in its last directory, its spawn
/// directory or `$HOME`, whichever still exists and starts.
pub(crate) async fn restore_panes(
    panes: Vec<(PaneId, PathBuf)>,
    io_state: Arc<tokio::sync::Mutex<crate::pane_io::PaneIoState>>,
    graph: shux_core::graph::GraphHandle,
    cancel: tokio_util::sync::CancellationToken,
) {
    let persist = io_state.lock().await.persist.clone();
    let orphans: Vec<_> = panes
        .iter()
        .filter_map(|(p, _)| persist.extras(*p).process.map(|pr| (*p, pr)))
        .collect();
    futures::future::join_all(orphans.iter().map(|(_, pr)| async move {
        if let Ok(pgid) = u32::try_from(pr.pid) {
            let _ = crate::lens_scratch::kill_pgid_lens_sequence(pgid, pr.start_token).await;
        }
    }))
    .await;
    for (p, _) in &orphans {
        persist.update(*p, |e| e.process = None);
    }

    let slots = Arc::new(tokio::sync::Semaphore::new(RESTORE_CONCURRENCY));
    let mut spawns = tokio::task::JoinSet::new();
    for (pane, spawn_cwd) in panes {
        let candidates: Vec<PathBuf> = persist
            .extras(pane)
            .last_cwd
            .into_iter()
            .chain([spawn_cwd])
            .chain(std::env::var_os("HOME").map(PathBuf::from))
            .collect();
        let (slots, io, graph, cancel) = (
            slots.clone(),
            io_state.clone(),
            graph.clone(),
            cancel.clone(),
        );
        spawns.spawn(async move {
            let Ok(_slot) = slots.acquire_owned().await else { return };
            for cwd in candidates.into_iter().filter(|c| c.is_dir()) {
                if cancel.is_cancelled() {
                    return;
                }
                io.lock().await.pending_notices.insert(pane, NOTICE.to_string());
                let spawned = crate::pane_spawn::spawn_pane_pty(
                    pane,
                    cwd.clone(),
                    Vec::new(),
                    shux_pty::handle::PtySize::default(),
                    Vec::new(),
                    false,
                    io.clone(),
                    cancel.clone(),
                    graph.clone(),
                )
                .await;
                match spawned {
                    Ok(()) => return,
                    Err(e) => tracing::warn!(%pane, cwd = %cwd.display(), "restore: shell did not start: {e}"),
                }
            }
            io.lock().await.pending_notices.remove(&pane);
            tracing::warn!(%pane, "restore: no directory to start the shell in");
        });
    }
    while spawns.join_next().await.is_some() {}
    persist.restore_finished();
}

/// The last save, at shutdown: after the graph stops taking changes and while
/// every pane is still alive to say where it is. Only then are panes ended.
pub(crate) async fn save_before_teardown(
    io_state: &Arc<tokio::sync::Mutex<crate::pane_io::PaneIoState>>,
    graph: &shux_core::graph::GraphHandle,
) {
    let persist = io_state.lock().await.persist.clone();
    if persist.is_enabled() {
        let sealed = graph.seal().await.unwrap_or_else(|| graph.snapshot());
        if tokio::time::timeout(Duration::from_secs(1), sample_cwds(io_state, graph))
            .await
            .is_err()
        {
            tracing::warn!("workspace: the last cwd sample did not finish in time");
        }
        persist.seal(sealed, Duration::from_secs(3)).await;
    }
    io_state.lock().await.end_panes();
}

#[cfg(test)]
mod tests {
    use super::*;
    use shux_core::graph::GraphHandle;
    use shux_core::model::SessionId;

    /// Saves happen only when a test asks for one (a tombstone, a seal).
    const NEVER: (Duration, Duration) = (Duration::from_secs(3600), Duration::from_secs(3600));

    struct Rig {
        _root: tempfile::TempDir,
        file: PathBuf,
        graph: GraphHandle,
        persist: Persist,
        stop_graph: tokio_util::sync::CancellationToken,
        fail_writes: Arc<std::sync::atomic::AtomicBool>,
    }

    async fn rig(sessions: &[&str], timing: (Duration, Duration)) -> Rig {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("shux");
        let boot = boot_at(&state, Path::new("/sock")).await.unwrap();
        let fail_writes = boot.owned.as_ref().unwrap().0.fail_writes.clone();
        let (g, snap) = SessionGraph::new();
        let persist = boot.start_timed(&g, snap.clone(), HashMap::new(), timing);
        let (tx, rx) = mpsc::channel(16);
        let graph = GraphHandle::new(tx, snap).with_close_guard(Arc::new(persist.clone()));
        let stop_graph = tokio_util::sync::CancellationToken::new();
        tokio::spawn(shux_core::graph::run_graph_loop(g, rx, stop_graph.clone()));
        for name in sessions {
            graph
                .create_session((*name).into(), "/".into())
                .await
                .unwrap();
        }
        // Their shells "started": saved from now on.
        for p in graph.snapshot().panes.keys() {
            persist.note_process(*p, std::process::id());
        }
        let file = state
            .join("servers")
            .join(crate::workspace_store::key_for(Path::new("/sock")))
            .join("workspace.json");
        Rig {
            _root: root,
            file,
            graph,
            persist,
            stop_graph,
            fail_writes,
        }
    }

    fn saved(r: &Rig) -> String {
        std::fs::read_to_string(&r.file).unwrap_or_default()
    }

    fn session(r: &Rig, name: &str) -> SessionId {
        r.graph.snapshot().find_session_by_name(name).unwrap().id
    }

    /// A save that lands while a close is pending leaves the closed session
    /// out, so no save racing a close can write it back.
    #[tokio::test]
    async fn a_save_racing_a_close_leaves_the_closed_thing_out() {
        let r = rig(&["doomed", "kept"], (Duration::ZERO, Duration::ZERO)).await;
        let (doomed, kept) = (session(&r, "doomed"), session(&r, "kept"));
        // The close's two halves, with a change landing between them.
        let ticket = r.persist.before_close(Removal::Session(doomed)).await;
        assert!(!saved(&r).contains("doomed"), "not saved before the close");
        r.graph
            .rename_session(kept, "renamed".into(), None)
            .await
            .unwrap();
        let want = r.graph.snapshot().version;
        r.persist.durable().wait_for(|v| *v >= want).await.unwrap();
        let racing = saved(&r);
        assert!(
            racing.contains("renamed") && !racing.contains("doomed"),
            "{racing}"
        );
        r.persist.after_close(ticket);
    }

    /// The last save happens even though the graph is already gone: a daemon
    /// stops its graph before it asks for that save.
    #[tokio::test]
    async fn the_last_save_outlives_the_graph() {
        let r = rig(&["s"], NEVER).await;
        r.graph
            .rename_session(session(&r, "s"), "last-word".into(), None)
            .await
            .unwrap();
        let sealed = r.graph.snapshot();
        r.stop_graph.cancel();
        shux_testkit::wait_until_async("the graph to stop", || {
            let g = r.graph.clone();
            async move { g.seal().await.is_none() }
        })
        .await;
        r.persist.seal(sealed, Duration::from_secs(5)).await;
        assert!(saved(&r).contains("last-word"), "{}", saved(&r));
    }

    /// A close whose save fails deletes the saved workspace: the next start
    /// restores nothing rather than the closed session. The close still works.
    #[tokio::test]
    async fn a_close_that_cannot_be_saved_forgets_the_workspace() {
        let r = rig(&["doomed", "kept"], NEVER).await;
        r.graph
            .destroy_session(session(&r, "doomed"), None)
            .await
            .unwrap();
        assert!(saved(&r).contains("kept"), "a saved close keeps the rest");
        r.fail_writes
            .store(true, std::sync::atomic::Ordering::SeqCst);
        r.graph
            .destroy_session(session(&r, "kept"), None)
            .await
            .unwrap();
        assert!(
            !r.file.exists(),
            "a failed close left the old workspace in place"
        );
        assert_eq!(r.persist.status().state, "failing");
    }

    /// A successor whose predecessor still holds the state waits for it to
    /// let go, and refuses to start if it never does.
    #[tokio::test(start_paused = true)]
    async fn a_successor_waits_for_its_predecessor_then_refuses() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("shux");
        let key = crate::workspace_store::key_for(Path::new("/sock"));
        let held = Store::open(&state, &key).unwrap();
        let (state2, release) = (state.clone(), tokio_util::sync::CancellationToken::new());
        let waiter = tokio::spawn({
            let release = release.clone();
            async move {
                release.cancelled().await;
                drop(held);
            }
        });
        let successor = tokio::spawn(async move { boot_at(&state2, Path::new("/sock")).await });
        tokio::time::advance(Duration::from_secs(2)).await;
        release.cancel();
        waiter.await.unwrap();
        assert!(
            successor.await.unwrap().is_ok(),
            "did not take over once released"
        );

        let _held = Store::open(&state, &key).unwrap();
        match boot_at(&state, Path::new("/sock")).await {
            Err(BootError::Held { holder }) => assert_eq!(holder, std::process::id().to_string()),
            Ok(_) => panic!("started while another daemon owns the workspace"),
        }
    }
}
