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
use shux_core::graph::{Durability, SessionGraph, SessionGraphSnapshot};
use shux_core::model::PaneId;
use shux_core::persist::{self, PaneExtras, ProcessId, RestoreError, Restored};
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
    /// Which attempt at restoring the saved workspace this boot is (0: none).
    pub restore_attempt: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quarantined: Option<String>,
}

enum Msg {
    /// Make the graph as a close leaves it durable, before the close commits.
    Close {
        after: Arc<SessionGraphSnapshot>,
        done: oneshot::Sender<Result<(), String>>,
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
    extras: Mutex<HashMap<PaneId, PaneExtras>>,
    /// Panes whose shell has started, or that were restored.
    admitted: Mutex<std::collections::HashSet<PaneId>>,
    extras_changed: tokio::sync::Notify,
}

/// A handle on a daemon's workspace persistence. Cheap to clone; a disabled
/// one turns every call into a no-op.
#[derive(Clone)]
pub struct Persist(Arc<Shared>);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// This boot of the machine: a process recorded under another boot is not
/// the process now holding its pid, whatever its start time says.
fn boot_id() -> Option<String> {
    static BOOT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BOOT.get_or_init(read_boot_id).clone()
}

#[cfg(target_os = "linux")]
fn read_boot_id() -> Option<String> {
    let id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    Some(id.trim().to_string()).filter(|s| !s.is_empty())
}

#[cfg(target_os = "macos")]
fn read_boot_id() -> Option<String> {
    let mut tv: libc::timeval = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::timeval>();
    // SAFETY: kern.boottime fills at most `len` bytes of `tv`, a timeval we own.
    let ret = unsafe {
        libc::sysctlbyname(
            c"kern.boottime".as_ptr(),
            (&raw mut tv).cast::<libc::c_void>(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (ret == 0 && len == std::mem::size_of::<libc::timeval>())
        .then(|| format!("{}.{}", tv.tv_sec, tv.tv_usec))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn read_boot_id() -> Option<String> {
    None
}

/// End the group of a shell a killed daemon left running. Only one proven to
/// be that shell: this boot, its leader alive with the same start. A leader
/// that has gone proves nothing about whoever holds its group id now.
pub(crate) async fn reap_orphan(process: &ProcessId) {
    if still_running(process)
        && let Ok(pgid) = u32::try_from(process.pid)
    {
        let _ = crate::lens_scratch::kill_pgid_lens_sequence(pgid, process.start_token).await;
    }
}

/// `process` is still the shell it was recorded as: this boot, same start.
fn still_running(process: &ProcessId) -> bool {
    let Ok(pid) = u32::try_from(process.pid) else {
        return false;
    };
    boot_id().is_some_and(|b| b == process.boot)
        && crate::lens_scratch::process_start_token(pid) == Some(process.start_token)
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
            extras: Mutex::default(),
            admitted: Mutex::default(),
            extras_changed: tokio::sync::Notify::new(),
        }))
    }

    pub fn status(&self) -> PersistStatus {
        lock(&self.0.status).clone()
    }

    pub fn is_enabled(&self) -> bool {
        self.0.tx.is_some()
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
        let process = crate::lens_scratch::process_start_token(pid)
            .zip(boot_id())
            .map(|(start_token, boot)| ProcessId {
                pid: pid as i32,
                start_token,
                boot,
            });
        self.update(pane, |e| e.process = process);
    }

    /// The shell `sampled` in `pane` is in `cwd`; dropped if `pane` has a
    /// newer shell by now.
    pub fn note_cwd(&self, pane: PaneId, sampled: &ProcessId, cwd: PathBuf) {
        self.update(pane, |e| {
            if e.process.as_ref() == Some(sampled) {
                e.last_cwd = Some(cwd);
            }
        });
    }

    /// Forget panes the graph no longer has and whose shell has gone. One
    /// that is running is kept: a pane created after `snapshot` was taken
    /// may already have one.
    pub fn retain_panes(&self, snapshot: &SessionGraphSnapshot) {
        lock(&self.0.extras).retain(|p, e| {
            snapshot.panes.contains_key(p) || e.process.as_ref().is_some_and(still_running)
        });
    }

    /// Every pane's shell, to sample.
    pub fn processes(&self) -> Vec<(PaneId, ProcessId)> {
        lock(&self.0.extras)
            .iter()
            .filter_map(|(p, e)| e.process.clone().map(|pr| (*p, pr)))
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

/// Every close is saved, as the graph will be after it, before it commits.
/// One that cannot be saved deletes the saved workspace, or failing that
/// marks it poisoned; only when neither works is the close refused.
impl Durability for Persist {
    fn close(
        &self,
        after: Arc<SessionGraphSnapshot>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + '_>> {
        Box::pin(async move {
            let Some(tx) = &self.0.tx else { return Ok(()) };
            let (done, saved) = oneshot::channel();
            if tx.send(Msg::Close { after, done }).is_err() {
                // A writer that has gone (sealed) saves nothing more anyway.
                return Ok(());
            }
            saved.await.unwrap_or(Ok(()))
        })
    }
}

struct Writer {
    /// The debounce and the most a change waits (`DEBOUNCE`, `MAX_LATENCY`).
    timing: (Duration, Duration),
    store: Arc<Store>,
    socket: String,
    shared: Arc<Shared>,
    state: Arc<arc_swap::ArcSwap<SessionGraphSnapshot>>,
    last: Option<Vec<u8>>,
    /// The version a close made durable: no older graph is saved over it.
    floor: u64,
    /// Nothing more is saved until the saved workspace can be deleted.
    poisoned: bool,
    /// The restore marker is to go; retried with every save until it does.
    clear_marker: bool,
    /// Largest workspace the next boot will read.
    max_bytes: usize,
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
    /// The workspace `snapshot` makes. A pane is saved only once its shell
    /// has started (or it was restored): a create or split rolled back for a
    /// failed spawn is never on disk to come back. Shells stay recorded even
    /// in the final save: a daemon killed before it ends them leaves them to
    /// be reaped.
    fn bytes_for(&self, snapshot: &SessionGraphSnapshot) -> Vec<u8> {
        let shared = &self.shared;
        let extras = |p: PaneId| lock(&shared.extras).get(&p).cloned().unwrap_or_default();
        let provisional: Vec<PaneId> = {
            let mut admitted = lock(&shared.admitted);
            // Read under the lock: a pane is in the graph before it is
            // admitted, so none admitted by now is missing from `live`.
            let live = self.state.load();
            admitted.retain(|p| live.panes.contains_key(p) || snapshot.panes.contains_key(p));
            snapshot
                .panes
                .keys()
                .filter(|p| !admitted.contains(p))
                .copied()
                .collect()
        };
        let ws = if provisional.is_empty() {
            persist::capture(snapshot, &self.socket, &extras)
        } else {
            let (g, _) = SessionGraph::from_snapshot(snapshot.clone(), None);
            for p in provisional {
                let _ = g.destroy_pane_and_empty_parents(p);
            }
            persist::capture(&g.current(), &self.socket, &extras)
        };
        persist::encode(&ws)
    }

    /// Save `snapshot`; `Err` leaves the previous file in place. A finished
    /// restore is recorded whether or not the save works.
    async fn save(&mut self, snapshot: &SessionGraphSnapshot) -> std::io::Result<()> {
        let saved = self.write(snapshot).await;
        if self.clear_marker && blocking(&self.store, Store::clear_restoring).await.is_ok() {
            self.clear_marker = false;
        }
        saved
    }

    async fn write(&mut self, snapshot: &SessionGraphSnapshot) -> std::io::Result<()> {
        if snapshot.version < self.floor {
            // A close is durable that this graph has not committed yet.
            return Ok(());
        }
        if self.poisoned {
            blocking(&self.store, |s| {
                s.remove_workspace()?;
                s.clear_poisoned()
            })
            .await?;
            self.poisoned = false;
        }
        let bytes = self.bytes_for(snapshot);
        if bytes.len() > self.max_bytes {
            return Err(std::io::Error::other(format!(
                "the workspace is {} bytes, over the {} a restore reads",
                bytes.len(),
                self.max_bytes
            )));
        }
        if self.last.as_deref() != Some(&bytes[..]) {
            let written = bytes.clone();
            blocking(&self.store, move |s| s.write_workspace(&written)).await?;
            self.last = Some(bytes);
            lock(&self.shared.status).writes += 1;
        }
        let mut st = lock(&self.shared.status);
        st.durable_version = snapshot.version;
        if st.state == "failing" {
            st.state = "ok";
            st.reason = None;
        }
        Ok(())
    }

    fn fail(&self, e: &std::io::Error) {
        tracing::warn!("workspace: could not save: {e}");
        let mut st = lock(&self.shared.status);
        st.state = "failing";
        st.reason = Some(e.to_string());
    }

    /// A close that could not be saved: delete the saved workspace, so the
    /// next start restores nothing rather than the closed thing. If even that
    /// fails, mark it poisoned on disk, so the next start sets it aside.
    /// `Err` when neither worked: the close must not happen.
    async fn forget(&mut self, e: &std::io::Error) -> Result<(), String> {
        self.fail(e);
        self.last = None;
        let Err(e2) = blocking(&self.store, Store::remove_workspace).await else {
            return Ok(());
        };
        self.poisoned = true;
        let marked = blocking(&self.store, Store::mark_poisoned).await;
        tracing::error!("workspace: could not delete it after a failed save either: {e2}");
        let (reason, out) = match marked {
            Ok(()) => (
                format!(
                    "cannot save ({e}) or delete ({e2}) the workspace; the next start sets it aside"
                ),
                Ok(()),
            ),
            Err(e3) => {
                let why = format!(
                    "cannot save ({e}), delete ({e2}) or mark ({e3}) the workspace in {}",
                    self.store.dir().display()
                );
                (why.clone(), Err(why))
            }
        };
        lock(&self.shared.status).reason = Some(reason);
        out
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
                    Some(Msg::Close { after, done }) => {
                        let out = match self.save(&after).await {
                            Ok(()) => Ok(()),
                            Err(e) => self.forget(&e).await,
                        };
                        if out.is_ok() {
                            self.floor = self.floor.max(after.version);
                        }
                        let _ = done.send(out);
                    }
                    Some(Msg::Seal { snapshot, done }) => {
                        // An orderly stop, even one mid-restore, is not a crash.
                        self.clear_marker = true;
                        if let Err(e) = self.save(&snapshot).await {
                            self.fail(&e);
                        }
                        let _ = done.send(());
                        return;
                    }
                    Some(Msg::RestoreFinished) => {
                        self.clear_marker = true;
                        first_dirty.get_or_insert_with(Instant::now);
                    }
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
                    let snap = self.state.load_full();
                    // A marker that would not clear is tried again with the
                    // next save, never by holding saves back.
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
    let off = |reason: &str| Ok(Boot::disabled(socket_key, reason.to_string()));
    if !enabled {
        return off("turned off ([restore] workspace = false)");
    }
    let Some(root) = crate::workspace_store::state_root() else {
        return off("no state directory: neither XDG_STATE_HOME nor HOME is set");
    };
    boot_at(&root, socket_key).await
}

async fn boot_at(root: &Path, socket_key: &Path) -> Result<Boot, BootError> {
    let socket = socket_key.to_string_lossy().into_owned();
    let disabled = |reason: String| Boot::disabled(socket_key, reason);
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
    // A directory it cannot write is one where a later close could not be
    // saved: never restore from it.
    if let Err(e) = nix::unistd::access(store.dir(), nix::unistd::AccessFlags::W_OK) {
        return Ok(disabled(format!(
            "cannot write {}: {e}",
            store.dir().display()
        )));
    }
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
    let restored = restored.map(|(r, attempt)| {
        status.restore_attempt = attempt;
        r
    });
    status.restored_panes = restored.as_ref().map_or(0, |r| r.snapshot.panes.len());
    Ok(Boot {
        restored,
        owned: Ok((store, status)),
        socket,
    })
}

/// Restores of one workspace that may die part way before it is set aside:
/// a kill or a power cut is not the workspace's fault, a second in a row is
/// a crash loop.
const RESTORE_ATTEMPTS: u32 = 2;

enum Unreadable {
    Quarantined { path: PathBuf, reason: String },
    Stuck(String),
}

/// The saved workspace and which attempt at restoring it this is.
fn read(store: &Store, socket: &str) -> Result<Option<(Restored, u32)>, Unreadable> {
    let stuck = |what: &str, e: &dyn std::fmt::Display| Unreadable::Stuck(format!("{what}: {e}"));
    let bytes = match store.read_workspace(persist::MAX_FILE_BYTES) {
        Ok(None) => {
            // Markers outlive the file they were about only if a removal
            // failed after the unlink; left, they would refuse the next one.
            store
                .clear_restoring()
                .map_err(|e| stuck("cannot clear the restore marker", &e))?;
            store
                .clear_poisoned()
                .map_err(|e| stuck("cannot clear the poison marker", &e))?;
            return Ok(None);
        }
        Ok(Some(b)) => b,
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            return Err(quarantine(store, "too-large", &e.to_string()));
        }
        Err(e) => return Err(stuck("cannot read the workspace", &e)),
    };
    let poisoned = store
        .is_poisoned()
        .map_err(|e| stuck("cannot read the poison marker", &e))?;
    if poisoned {
        let refused = quarantine(
            store,
            "poisoned",
            "a close could neither be saved nor have the workspace deleted",
        );
        if matches!(refused, Unreadable::Quarantined { .. }) {
            store
                .clear_poisoned()
                .map_err(|e| stuck("cannot clear the poison marker", &e))?;
        }
        return Err(refused);
    }
    // The marker exists only while a restore runs: one left behind is a
    // restore that died part way, whatever was saved since.
    let attempts = store
        .restoring()
        .map_err(|e| stuck("cannot read the restore marker", &e))?
        .unwrap_or(0);
    if attempts >= RESTORE_ATTEMPTS {
        return Err(quarantine(
            store,
            "crashed-during-restore",
            &format!("{attempts} restores of it died part way"),
        ));
    }
    // Marked before decoding, so a decoder that crashes is caught too.
    store
        .mark_restoring(attempts + 1)
        .map_err(|e| stuck("cannot write the restore marker", &e))?;
    match persist::decode(&bytes, socket) {
        Ok(ws) => Ok(Some((persist::rebuild(&ws), attempts + 1))),
        Err(e) => {
            let reason = match &e {
                RestoreError::FutureSchema { .. } => "future-schema",
                RestoreError::WrongSocket { .. } => "wrong-socket",
                RestoreError::TooLarge(_) => "too-large",
                RestoreError::Decode(_) | RestoreError::Invalid(_) => "invalid",
            };
            Err(quarantine(store, reason, &e.to_string()))
        }
    }
}

/// Set the workspace aside, as `reason`; `detail` says what was wrong.
fn quarantine(store: &Store, reason: &str, detail: &str) -> Unreadable {
    let path = match store.quarantine(reason) {
        Ok(p) => p,
        Err(e) => {
            return Unreadable::Stuck(format!(
                "cannot set aside a refused workspace ({reason}: {detail}): {e}"
            ));
        }
    };
    if let Err(e) = store.clear_restoring() {
        return Unreadable::Stuck(format!("cannot clear the restore marker: {e}"));
    }
    tracing::warn!(
        "workspace: {reason} ({detail}); set aside as {}",
        path.display()
    );
    Unreadable::Quarantined {
        path,
        reason: format!("{reason} ({detail})"),
    }
}

impl Boot {
    fn disabled(socket_key: &Path, reason: String) -> Self {
        Self {
            restored: None,
            owned: Err(reason),
            socket: socket_key.to_string_lossy().into_owned(),
        }
    }

    /// Start saving. `graph` must be the graph the daemon serves, before its
    /// loop starts, so no commit is missed.
    /// `extras` are the restored panes' own (`Restored::extras`).
    pub fn start(
        self,
        graph: &SessionGraph,
        state: Arc<arc_swap::ArcSwap<SessionGraphSnapshot>>,
        extras: HashMap<PaneId, PaneExtras>,
    ) -> Persist {
        let limits = ((DEBOUNCE, MAX_LATENCY), persist::MAX_FILE_BYTES as usize);
        self.start_with(graph, state, extras, limits)
    }

    /// `limits`: the debounce, the most a change waits, the largest workspace.
    fn start_with(
        self,
        graph: &SessionGraph,
        state: Arc<arc_swap::ArcSwap<SessionGraphSnapshot>>,
        extras: HashMap<PaneId, PaneExtras>,
        ((debounce, latency), max_bytes): ((Duration, Duration), usize),
    ) -> Persist {
        let timing = (debounce, latency);
        let (store, status) = match self.owned {
            Ok(owned) => owned,
            Err(reason) => return Persist::disabled(reason),
        };
        let (tx, rx) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            tx: Some(tx),
            status: Mutex::new(status),
            admitted: Mutex::new(extras.keys().copied().collect()),
            extras: Mutex::new(extras),
            extras_changed: tokio::sync::Notify::new(),
        });
        let writer = Writer {
            store: Arc::new(store),
            socket: self.socket,
            shared: Arc::clone(&shared),
            state,
            last: None,
            floor: 0,
            poisoned: false,
            clear_marker: false,
            max_bytes,
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
    persist.retain_panes(&graph.snapshot());
    let procs = persist.processes();
    let found = tokio::task::spawn_blocking(move || {
        procs
            .into_iter()
            .filter_map(|(pane, pr)| {
                let pid = u32::try_from(pr.pid).ok()?;
                let cwd = crate::pane_cwd::sample(pid, pr.start_token)?;
                Some((pane, pr, cwd))
            })
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    for (pane, sampled, cwd) in found {
        persist.note_cwd(pane, &sampled, cwd);
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
/// Start a restored pane's shell in the first of `candidates` that works. A
/// pane closed meanwhile is left alone; one no directory works for becomes
/// an exited pane that says why, never a pane with no program at all.
async fn restore_one(
    pane: PaneId,
    candidates: Vec<PathBuf>,
    io: &Arc<tokio::sync::Mutex<crate::pane_io::PaneIoState>>,
    graph: &shux_core::graph::GraphHandle,
    cancel: &tokio_util::sync::CancellationToken,
) {
    let gone = || !graph.snapshot().panes.contains_key(&pane);
    let mut why = "no directory it can start in exists".to_string();
    for cwd in candidates.into_iter().filter(|c| c.is_dir()) {
        if cancel.is_cancelled() || gone() {
            return;
        }
        io.lock()
            .await
            .pending_notices
            .insert(pane, NOTICE.to_string());
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
            Err(e) => {
                tracing::warn!(%pane, cwd = %cwd.display(), "restore: shell did not start: {e}");
                why = crate::pane_spawn::spawn_failure_message(&e);
            }
        }
    }
    if gone() {
        return;
    }
    let mut vt = shux_vt::VirtualTerminal::new(24, 80);
    vt.process(
        format!("\x1b[31mshux: could not restart this pane's shell: {why}\x1b[0m\r\n").as_bytes(),
    );
    io.lock().await.vts.insert(pane, vt);
    let _ = graph.set_pane_exit_status(pane, 127).await;
}

pub(crate) async fn restore_panes(
    panes: Vec<(PaneId, PathBuf)>,
    io_state: Arc<tokio::sync::Mutex<crate::pane_io::PaneIoState>>,
    graph: shux_core::graph::GraphHandle,
    cancel: tokio_util::sync::CancellationToken,
    // Raised once every pane's shell has started or failed: see
    // `Router::wait_until_ready`.
    ready: tokio::sync::watch::Sender<bool>,
) {
    let persist = io_state.lock().await.persist.clone();
    let orphans: Vec<_> = panes
        .iter()
        .filter_map(|(p, _)| persist.extras(*p).process.map(|pr| (*p, pr)))
        .collect();
    futures::future::join_all(orphans.iter().map(|(_, pr)| reap_orphan(pr))).await;
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
            if let Ok(_slot) = slots.acquire_owned().await {
                restore_one(pane, candidates, &io, &graph, &cancel).await;
            }
            let mut state = io.lock().await;
            state.pending_notices.remove(&pane);
            state.awaiting_restore.remove(&pane);
        });
    }
    while spawns.join_next().await.is_some() {}
    persist.restore_finished();
    ready.send_replace(true);
}

/// The last save, at shutdown: after the graph stops taking changes and while
/// every pane is still alive to say where it is. Only then are panes ended.
pub(crate) async fn save_before_teardown(
    io_state: &Arc<tokio::sync::Mutex<crate::pane_io::PaneIoState>>,
    graph: &shux_core::graph::GraphHandle,
) {
    let persist = io_state.lock().await.persist.clone();
    if persist.is_enabled() {
        // A close stuck on a disk holds the graph: waiting for the seal
        // must not keep the daemon from stopping. The close was never
        // acknowledged, so its session is still on disk and still here.
        match tokio::time::timeout(Duration::from_secs(3), graph.seal()).await {
            Ok(sealed) => {
                let sealed = sealed.unwrap_or_else(|| graph.snapshot());
                if tokio::time::timeout(Duration::from_secs(1), sample_cwds(io_state, graph))
                    .await
                    .is_err()
                {
                    tracing::warn!("workspace: the last cwd sample did not finish in time");
                }
                persist.seal(sealed, Duration::from_secs(3)).await;
            }
            Err(_) => tracing::warn!("workspace: a close did not finish; no final save"),
        }
    }
    io_state.lock().await.end_panes();
}

#[cfg(test)]
mod tests {
    use super::*;
    use shux_core::graph::GraphHandle;
    use shux_core::model::SessionId;
    use std::sync::atomic::AtomicBool;

    /// Saves happen only when a test asks for one (a close, a seal).
    const NEVER: (Duration, Duration) = (Duration::from_secs(3600), Duration::from_secs(3600));
    const FAST: (Duration, Duration) = (Duration::ZERO, Duration::ZERO);
    const ANY_SIZE: usize = persist::MAX_FILE_BYTES as usize;

    struct Rig {
        _root: tempfile::TempDir,
        state: PathBuf,
        file: PathBuf,
        graph: GraphHandle,
        persist: Persist,
        fail_writes: Arc<std::sync::atomic::AtomicBool>,
        fail_removes: Arc<std::sync::atomic::AtomicBool>,
        fail_marks: Arc<std::sync::atomic::AtomicBool>,
    }

    async fn rig(sessions: &[&str], timing: (Duration, Duration), max_bytes: usize) -> Rig {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("shux");
        let boot = boot_at(&state, Path::new("/sock")).await.unwrap();
        let store = &boot.owned.as_ref().unwrap().0;
        let (fail_writes, fail_removes, fail_marks) = (
            store.fail_writes.clone(),
            store.fail_removes.clone(),
            store.fail_marks.clone(),
        );
        let (g, snap) = SessionGraph::new();
        let persist = boot.start_with(&g, snap.clone(), HashMap::new(), (timing, max_bytes));
        let (tx, rx) = mpsc::channel(16);
        let graph = GraphHandle::new(tx, snap);
        let durability: Arc<dyn Durability> = Arc::new(persist.clone());
        tokio::spawn(shux_core::graph::run_graph_loop_durable(
            g,
            rx,
            tokio_util::sync::CancellationToken::new(),
            Some(durability),
        ));
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
            state,
            file,
            graph,
            persist,
            fail_writes,
            fail_removes,
            fail_marks,
        }
    }

    fn fail(flag: &AtomicBool) {
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn saved(r: &Rig) -> String {
        std::fs::read_to_string(&r.file).unwrap_or_default()
    }

    fn session(r: &Rig, name: &str) -> SessionId {
        r.graph.snapshot().find_session_by_name(name).unwrap().id
    }

    /// A close the graph refuses (a stale version) is never saved: the
    /// session it named is still in the saved workspace, as in the graph.
    #[tokio::test]
    async fn a_refused_close_is_never_saved() {
        let r = rig(&["a", "b", "c"], NEVER, ANY_SIZE).await;
        r.graph
            .destroy_session(session(&r, "c"), None)
            .await
            .unwrap();
        let a = session(&r, "a");
        let stale = r.graph.snapshot().sessions[&a].version + 1;
        assert!(r.graph.destroy_session(a, Some(stale)).await.is_err());
        assert!(
            saved(&r).contains("\"a\""),
            "a refused close was saved: {}",
            saved(&r)
        );
    }

    /// The last save happens even though the graph is already gone: a daemon
    /// stops its graph before it asks for that save.
    #[tokio::test]
    async fn the_last_save_outlives_the_graph() {
        let r = rig(&["s"], NEVER, ANY_SIZE).await;
        r.graph
            .rename_session(session(&r, "s"), "last-word".into(), None)
            .await
            .unwrap();
        let sealed = r.graph.seal().await.expect("sealed");
        r.persist.seal(sealed, Duration::from_secs(5)).await;
        assert!(saved(&r).contains("last-word"), "{}", saved(&r));
    }

    /// A close whose save fails deletes the saved workspace: the next start
    /// restores nothing rather than the closed session. The close still works.
    #[tokio::test]
    async fn a_close_that_cannot_be_saved_forgets_the_workspace() {
        let r = rig(&["doomed", "kept"], NEVER, ANY_SIZE).await;
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

    /// A workspace too large for the next boot to read is never written: a
    /// close then forgets the old one, as for any failed save.
    #[tokio::test]
    async fn a_workspace_too_large_to_restore_is_never_written() {
        let r = rig(&["a", "b", "c"], NEVER, ANY_SIZE).await;
        r.graph
            .destroy_session(session(&r, "c"), None)
            .await
            .unwrap();
        assert!(r.file.exists());
        let small = rig(&["a", "b", "c"], NEVER, 64).await;
        small
            .graph
            .destroy_session(session(&small, "c"), None)
            .await
            .unwrap();
        assert!(!small.file.exists(), "wrote a workspace its reader refuses");
        let reason = small.persist.status().reason.unwrap_or_default();
        assert!(reason.contains("bytes"), "{reason}");
    }

    /// A close that can neither be saved nor have the old workspace deleted
    /// marks it poisoned on disk; the next start sets it aside instead of
    /// restoring the closed session.
    #[tokio::test]
    async fn a_workspace_that_cannot_be_forgotten_is_set_aside_next_boot() {
        let r = rig(&["doomed", "kept"], NEVER, ANY_SIZE).await;
        r.graph
            .destroy_session(session(&r, "kept"), None)
            .await
            .unwrap();
        r.fail_writes
            .store(true, std::sync::atomic::Ordering::SeqCst);
        r.fail_removes
            .store(true, std::sync::atomic::Ordering::SeqCst);
        r.graph
            .destroy_session(session(&r, "doomed"), None)
            .await
            .unwrap();
        assert!(r.file.exists() && saved(&r).contains("doomed"));
        let sealed = r.graph.seal().await.expect("sealed");
        r.persist.seal(sealed, Duration::from_secs(5)).await;

        let next = boot_at(&r.state, Path::new("/sock")).await.unwrap();
        assert!(next.restored.is_none(), "restored a closed session");
        let q = next.owned.as_ref().unwrap().1.quarantined.clone();
        assert!(
            q.as_deref().is_some_and(|q| q.starts_with("poisoned")),
            "{q:?}"
        );
    }

    /// A restore marker that could not be cleared is cleared by a later save:
    /// left in place, it would count a finished restore as a crashed one.
    #[tokio::test]
    async fn a_marker_that_failed_to_clear_is_cleared_later() {
        let r = rig(&["s"], FAST, ANY_SIZE).await;
        let marker = r.file.with_file_name("restoring");
        std::fs::write(&marker, "1").unwrap();
        r.fail_removes
            .store(true, std::sync::atomic::Ordering::SeqCst);
        r.persist.restore_finished();
        let writes = r.persist.status().writes;
        r.graph
            .rename_session(session(&r, "s"), "t".into(), None)
            .await
            .unwrap();
        shux_testkit::wait_until_async("a save while removals fail", || {
            let persist = r.persist.clone();
            async move { persist.status().writes > writes }
        })
        .await;
        assert!(marker.exists());
        r.fail_removes
            .store(false, std::sync::atomic::Ordering::SeqCst);
        r.graph
            .rename_session(session(&r, "t"), "u".into(), None)
            .await
            .unwrap();
        shux_testkit::wait_until_async("the marker to be cleared", || {
            let gone = !marker.exists();
            async move { gone }
        })
        .await;
    }

    /// A pane the sampler's graph snapshot does not have yet, but whose shell
    /// is running, is kept: it was created after the snapshot was taken.
    #[tokio::test]
    async fn a_running_pane_newer_than_the_snapshot_is_kept() {
        let r = rig(&[], NEVER, ANY_SIZE).await;
        let pane = PaneId::new();
        r.persist.note_process(pane, std::process::id());
        r.persist.retain_panes(&SessionGraphSnapshot::default());
        assert!(
            r.persist.extras(pane).process.is_some(),
            "dropped a live pane"
        );
        assert!(lock(&r.persist.0.admitted).contains(&pane));
    }

    /// A shell recorded under another boot of the machine is not the process
    /// now holding its pid, whatever its start time.
    #[test]
    fn a_shell_of_another_boot_is_not_running() {
        let pid = std::process::id();
        let start_token = crate::lens_scratch::process_start_token(pid).unwrap();
        let this = ProcessId {
            pid: pid as i32,
            start_token,
            boot: boot_id().unwrap(),
        };
        assert!(still_running(&this));
        let other = ProcessId {
            boot: "another boot".into(),
            ..this
        };
        assert!(!still_running(&other));
    }

    /// A pane closed before the restore reached it is left alone: no shell,
    /// and no "could not restart" screen for a pane that no longer exists.
    #[tokio::test]
    async fn a_pane_closed_before_its_restore_is_left_alone() {
        let h = crate::rpc::test_harness::RpcHarness::new();
        let gone = PaneId::new();
        let dir = tempfile::tempdir().unwrap();
        restore_one(
            gone,
            vec![dir.path().to_path_buf()],
            &h.io,
            &h.graph,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await;
        assert!(
            !h.io.lock().await.vts.contains_key(&gone),
            "a screen for a closed pane"
        );
        h.stop().await;
    }

    /// A daemon that dies part way through a restore, even after the restore
    /// has saved, is caught on the next boot: the workspace is set aside
    /// instead of restored again into the same crash.
    #[tokio::test]
    async fn a_crash_after_a_restore_saved_is_still_caught() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("shux");
        let fast = (Duration::ZERO, Duration::ZERO);
        let boot_graph = move |boot: Boot, snapshot: SessionGraphSnapshot| {
            let (g, snap) = SessionGraph::from_snapshot(snapshot, None);
            let persist = boot.start_with(&g, snap.clone(), HashMap::new(), (fast, ANY_SIZE));
            let (tx, rx) = mpsc::channel(16);
            tokio::spawn(shux_core::graph::run_graph_loop(
                g,
                rx,
                tokio_util::sync::CancellationToken::new(),
            ));
            (GraphHandle::new(tx, snap), persist)
        };

        // A first daemon saves a session and stops in order.
        let (graph, persist) = boot_graph(
            boot_at(&state, Path::new("/sock")).await.unwrap(),
            Default::default(),
        );
        graph.create_session("s".into(), "/".into()).await.unwrap();
        let pane = *graph.snapshot().panes.keys().next().unwrap();
        persist.note_process(pane, std::process::id());
        persist.seal(graph.snapshot(), Duration::from_secs(5)).await;

        // A daemon restores it and saves (its shells started), then dies: its
        // whole runtime goes, as a process's does, with no seal. The first
        // such death is retried; the workspace is still there to restore.
        let crash_mid_restore = || {
            let state = state.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async {
                    let mut boot = boot_at(&state, Path::new("/sock")).await.unwrap();
                    let restored = boot.restored.take().expect("not restored after one crash");
                    let (_graph, persist) = boot_graph(boot, restored.snapshot);
                    persist.note_process(pane, std::process::id());
                    shux_testkit::wait_until_async("the restoring daemon to save", || {
                        let persist = persist.clone();
                        async move { persist.status().writes > 0 }
                    })
                    .await;
                });
            })
            .join()
            .unwrap();
        };
        crash_mid_restore();
        crash_mid_restore();

        let third = boot_at(&state, Path::new("/sock")).await.unwrap();
        assert!(third.restored.is_none(), "restored into the same crash");
        let quarantined = third.owned.as_ref().unwrap().1.quarantined.clone();
        assert!(
            quarantined
                .as_deref()
                .is_some_and(|q| q.starts_with("crashed-during-restore")),
            "{quarantined:?}"
        );
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

    /// A close nothing durable can be recorded for (the workspace can be
    /// neither saved, deleted nor marked) is refused: done, a restart would
    /// bring it back.
    #[tokio::test]
    async fn a_close_nothing_can_record_is_refused() {
        let r = rig(&["doomed", "kept", "x"], NEVER, ANY_SIZE).await;
        r.graph
            .destroy_session(session(&r, "x"), None)
            .await
            .unwrap();
        fail(&r.fail_writes);
        fail(&r.fail_removes);
        fail(&r.fail_marks);
        let refused = r.graph.destroy_session(session(&r, "doomed"), None).await;
        assert!(
            matches!(refused, Err(shux_core::graph::GraphError::NotDurable(_))),
            "{refused:?}"
        );
        assert!(r.graph.snapshot().find_session_by_name("doomed").is_some());
        assert!(saved(&r).contains("doomed"));
    }

    /// A close stuck on a disk does not keep the daemon from stopping.
    #[tokio::test(start_paused = true)]
    async fn a_close_stuck_on_disk_does_not_hold_up_shutdown() {
        struct Stuck(Arc<tokio::sync::Notify>);
        impl Durability for Stuck {
            fn close(
                &self,
                _: Arc<SessionGraphSnapshot>,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + '_>>
            {
                self.0.notify_one();
                Box::pin(std::future::pending())
            }
        }
        let r = rig(&[], NEVER, ANY_SIZE).await;
        let (g, snap) = SessionGraph::new();
        let id = g.create_session("s".into(), "/".into()).unwrap();
        let (tx, rx) = mpsc::channel(16);
        let called = Arc::new(tokio::sync::Notify::new());
        tokio::spawn(shux_core::graph::run_graph_loop_durable(
            g,
            rx,
            tokio_util::sync::CancellationToken::new(),
            Some(Arc::new(Stuck(called.clone()))),
        ));
        let graph = GraphHandle::new(tx, snap);
        tokio::spawn({
            let graph = graph.clone();
            async move { graph.destroy_session(id, None).await }
        });
        called.notified().await;

        let io = Arc::new(tokio::sync::Mutex::new(
            crate::pane_io::PaneIoState::new().with_persist(r.persist.clone()),
        ));
        tokio::time::timeout(Duration::from_secs(600), save_before_teardown(&io, &graph))
            .await
            .expect("shutdown waited for a close that never finishes");
    }

    /// The final save keeps every shell's identity: a daemon killed after it
    /// and before it ends them leaves them for the next one to reap.
    #[tokio::test]
    async fn the_final_save_keeps_its_shells() {
        let r = rig(&["s"], NEVER, ANY_SIZE).await;
        let sealed = r.graph.seal().await.expect("sealed");
        r.persist.seal(sealed, Duration::from_secs(5)).await;
        let ws = persist::decode(saved(&r).as_bytes(), "/sock").expect("saved");
        let pane = &ws.sessions[0].windows[0].panes[0];
        assert_eq!(
            pane.process.as_ref().map(|p| p.pid),
            Some(std::process::id() as i32)
        );
    }

    /// A poison marker whose workspace has gone refuses nothing: the next
    /// workspace saved is restored, not set aside.
    #[tokio::test]
    async fn a_poison_marker_without_a_workspace_refuses_nothing() {
        let r = rig(&[], NEVER, ANY_SIZE).await;
        let sealed = r.graph.seal().await.expect("sealed");
        r.persist.seal(sealed, Duration::from_secs(5)).await;
        std::fs::remove_file(&r.file).unwrap();
        std::fs::write(r.file.with_file_name("poisoned"), "1").unwrap();

        // A daemon starts with no workspace and that marker, and saves one.
        let boot = boot_at(&r.state, Path::new("/sock")).await.unwrap();
        let (g, snap) = SessionGraph::new();
        let persist = boot.start_with(&g, snap, HashMap::new(), (NEVER, ANY_SIZE));
        g.create_session("s".into(), "/".into()).unwrap();
        let pane = *g.current().panes.keys().next().unwrap();
        persist.note_process(pane, std::process::id());
        persist.seal(g.current(), Duration::from_secs(5)).await;

        let next = boot_at(&r.state, Path::new("/sock")).await.unwrap();
        let q = next.owned.as_ref().unwrap().1.quarantined.clone();
        assert!(next.restored.is_some(), "set aside as {q:?}");
    }

    /// A restore that finished is recorded even when the save after it
    /// fails: a stop on a full disk is not a crash during restore.
    #[tokio::test]
    async fn a_finished_restore_is_recorded_when_the_save_fails() {
        let r = rig(&["s"], NEVER, ANY_SIZE).await;
        let marker = r.file.with_file_name("restoring");
        std::fs::write(&marker, "1").unwrap();
        r.persist.restore_finished();
        fail(&r.fail_writes);
        r.graph
            .rename_session(session(&r, "s"), "t".into(), None)
            .await
            .unwrap();
        let sealed = r.graph.seal().await.expect("sealed");
        r.persist.seal(sealed, Duration::from_secs(5)).await;
        assert!(
            !marker.exists(),
            "a finished restore still counts as crashed"
        );
    }

    /// A pane whose shell starts while the cwd sampler works from an older
    /// graph is still saved.
    #[tokio::test]
    async fn a_pane_started_while_the_sampler_ran_is_still_saved() {
        let r = rig(&[], NEVER, ANY_SIZE).await;
        let before = r.graph.snapshot();
        r.graph
            .create_session("late".into(), "/".into())
            .await
            .unwrap();
        let pane = *r.graph.snapshot().panes.keys().next().unwrap();
        // A shell that has already exited: no identity to keep it by.
        let mut gone = std::process::Command::new("true").spawn().unwrap();
        let pid = gone.id();
        gone.wait().unwrap();
        r.persist.note_process(pane, pid);
        r.persist.retain_panes(&before);
        let sealed = r.graph.seal().await.expect("sealed");
        r.persist.seal(sealed, Duration::from_secs(5)).await;
        assert!(saved(&r).contains("late"), "{}", saved(&r));
    }

    /// A saved shell whose leader has gone is not reaped: its group id may
    /// name someone else's group by now.
    #[tokio::test]
    async fn a_group_whose_leader_has_gone_is_not_signalled() {
        use std::os::unix::process::CommandExt;
        let mut leader = std::process::Command::new("sh")
            .args(["-c", "sleep 600 & exit 0"])
            .process_group(0)
            .spawn()
            .unwrap();
        let pgid = leader.id() as i32;
        leader.wait().unwrap();
        let group = nix::unistd::Pid::from_raw(pgid);
        reap_orphan(&ProcessId {
            pid: pgid,
            start_token: 1,
            boot: boot_id().unwrap(),
        })
        .await;
        let alive = nix::sys::signal::killpg(group, None).is_ok();
        let _ = nix::sys::signal::killpg(group, nix::sys::signal::Signal::SIGKILL);
        assert!(
            alive,
            "signalled a group its leader no longer proves is ours"
        );
    }
}
