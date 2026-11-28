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
use shux_core::graph::{GraphError, SessionGraph, SessionGraphSnapshot};
use shux_core::model::{PaneId, SessionId, WindowId};
use shux_core::persist::{self, PaneExtras, RestoreError, Restored};
use tokio::sync::{mpsc, oneshot, watch};

use crate::workspace_store::{OpenError, Store};

/// Changes are saved this long after the last one...
const DEBOUNCE: Duration = Duration::from_millis(250);
/// ...but never later than this after the first unsaved one.
const MAX_LATENCY: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(30);

/// What a close removes, applied to a copy of the graph exactly as the real
/// close applies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removal {
    Session(SessionId),
    Window(WindowId),
    Pane(PaneId),
    /// A pane and every window and session it leaves empty.
    PaneCascade(PaneId),
}

impl Removal {
    fn apply(self, g: &SessionGraph) {
        // A removal that no longer applies (already gone) changes nothing.
        let _ = match self {
            Self::Session(id) => g.destroy_session(id, None),
            Self::Window(id) => g.destroy_window(id, None),
            Self::Pane(id) => g.destroy_pane(id, None),
            Self::PaneCascade(id) => g.destroy_pane_and_empty_parents(id),
        };
    }
}

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

    /// Record what the graph does not hold about a pane (its sampled cwd, its
    /// shell). Saved with the next write.
    pub fn set_extras(&self, pane: PaneId, extras: PaneExtras) {
        let changed = {
            let mut map = lock(&self.0.extras);
            map.insert(pane, extras.clone()) != Some(extras)
        };
        if changed {
            self.0.extras_changed.notify_one();
        }
    }

    pub fn extras(&self, pane: PaneId) -> PaneExtras {
        lock(&self.0.extras).get(&pane).cloned().unwrap_or_default()
    }

    /// Make `removal` durable before the close that performs it, then run the
    /// close. Until the close has committed, every save leaves the removed
    /// things out, so no save racing it can put them back. A close is never
    /// refused for a storage error: if the removal cannot be saved, the saved
    /// workspace is deleted instead.
    pub async fn closing<T>(
        &self,
        removal: Removal,
        close: impl std::future::Future<Output = Result<T, GraphError>>,
    ) -> Result<T, GraphError> {
        let Some(tx) = &self.0.tx else {
            return close.await;
        };
        let id = self
            .0
            .next_tombstone
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (done, saved) = oneshot::channel();
        if tx.send(Msg::Tombstone { id, removal, done }).is_ok() {
            // A writer that has gone away (sealed) saves nothing more anyway.
            let _ = saved.await;
        }
        let out = close.await;
        let _ = tx.send(Msg::Settle { id });
        out
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

struct Writer {
    store: Store,
    socket: String,
    shared: Arc<Shared>,
    state: Arc<arc_swap::ArcSwap<SessionGraphSnapshot>>,
    tombstones: Vec<(u64, Removal)>,
    last: Option<Vec<u8>>,
    /// Set when a save could not happen: nothing more is written.
    poisoned: bool,
}

impl Writer {
    fn current(&self) -> Arc<SessionGraphSnapshot> {
        self.state.load_full()
    }

    /// The workspace `snapshot` makes once every pending removal is applied.
    fn bytes_for(&self, snapshot: &SessionGraphSnapshot) -> Vec<u8> {
        let shared = &self.shared;
        let extras = |p: PaneId| lock(&shared.extras).get(&p).cloned().unwrap_or_default();
        let ws = if self.tombstones.is_empty() {
            persist::capture(snapshot, &self.socket, &extras)
        } else {
            let (g, _) = SessionGraph::from_snapshot(snapshot.clone(), None);
            for (_, r) in &self.tombstones {
                r.apply(&g);
            }
            persist::capture(&g.current(), &self.socket, &extras)
        };
        persist::encode(&ws)
    }

    /// Save `snapshot`; `Err` leaves the previous file in place.
    fn save(&mut self, snapshot: &SessionGraphSnapshot) -> std::io::Result<()> {
        if self.poisoned {
            return Ok(());
        }
        let bytes = self.bytes_for(snapshot);
        if self.last.as_deref() != Some(&bytes[..]) {
            self.store.write_workspace(&bytes)?;
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

    fn clear_marker(&self) {
        if let Err(e) = self.store.clear_restoring() {
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
    fn forget(&mut self, e: &std::io::Error) {
        self.fail(e);
        self.last = None;
        if let Err(e2) = self.store.remove_workspace() {
            tracing::error!("workspace: could not delete it after a failed save either: {e2}");
            self.poisoned = true;
            let mut st = lock(&self.shared.status);
            st.state = "disabled";
            st.reason = Some(format!("cannot save ({e}) or delete ({e2}) the workspace"));
        }
    }

    async fn run(
        mut self,
        mut rx: mpsc::UnboundedReceiver<Msg>,
        mut commits: watch::Receiver<u64>,
    ) {
        let mut first_dirty: Option<Instant> = None;
        let mut last_change = Instant::now();
        let mut retry = Duration::ZERO;
        loop {
            let deadline = first_dirty.map(|first| {
                (last_change + DEBOUNCE)
                    .min(first + MAX_LATENCY)
                    .max(Instant::now() + retry)
            });
            tokio::select! {
                msg = rx.recv() => match msg {
                    None => return,
                    Some(Msg::Tombstone { id, removal, done }) => {
                        self.tombstones.push((id, removal));
                        let snap = self.current();
                        if let Err(e) = self.save(&snap) {
                            self.forget(&e);
                        }
                        let _ = done.send(());
                    }
                    Some(Msg::Settle { id }) => {
                        self.tombstones.retain(|(t, _)| *t != id);
                        first_dirty.get_or_insert_with(Instant::now);
                        last_change = Instant::now();
                    }
                    Some(Msg::Seal { snapshot, done }) => {
                        if let Err(e) = self.save(&snapshot) {
                            self.fail(&e);
                        }
                        // An orderly stop, even one mid-restore, is not a crash.
                        self.clear_marker();
                        let _ = done.send(());
                        return;
                    }
                    Some(Msg::RestoreFinished) => self.clear_marker(),
                },
                changed = commits.changed() => {
                    if changed.is_err() {
                        // The graph is gone without a seal: keep what is saved.
                        return;
                    }
                    first_dirty.get_or_insert_with(Instant::now);
                    last_change = Instant::now();
                }
                () = self.shared.extras_changed.notified() => {
                    first_dirty.get_or_insert_with(Instant::now);
                    last_change = Instant::now();
                }
                () = sleep_until(deadline) => {
                    let snap = self.current();
                    match self.save(&snap) {
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
    let key = crate::workspace_store::key_for(socket_key);
    let started = Instant::now();
    let store = loop {
        match Store::open(&root, &key) {
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
    pub fn start(
        self,
        graph: &SessionGraph,
        state: Arc<arc_swap::ArcSwap<SessionGraphSnapshot>>,
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
            extras: Mutex::new(
                self.restored
                    .as_ref()
                    .map(|r| r.extras.clone())
                    .unwrap_or_default(),
            ),
            extras_changed: tokio::sync::Notify::new(),
            next_tombstone: Default::default(),
        });
        let writer = Writer {
            store,
            socket: self.socket,
            shared: Arc::clone(&shared),
            state,
            tombstones: Vec::new(),
            last: None,
            poisoned: false,
        };
        tokio::spawn(writer.run(rx, graph.subscribe_commits()));
        Persist(shared)
    }
}
