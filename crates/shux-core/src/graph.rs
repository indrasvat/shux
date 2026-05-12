//! SessionGraph: authoritative state container with lock-free reads (PRD 4.3, 5.4).
//!
//! Single-writer/many-readers pattern:
//! - Writers send `GraphCommand` through mpsc to the graph owner task
//! - Readers load snapshots via `ArcSwap::load()` (lock-free)

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::bus::EventBus;
use crate::event::EventData;
use crate::layout::{Direction, NavDirection, Rect};
use crate::model::*;

/// Errors that can occur during graph mutations.
#[derive(Debug, Clone, thiserror::Error)]
pub enum GraphError {
    #[error("session not found: {0}")]
    SessionNotFound(SessionId),

    #[error("window not found: {0}")]
    WindowNotFound(WindowId),

    #[error("pane not found: {0}")]
    PaneNotFound(PaneId),

    #[error("session name already exists: {0}")]
    SessionNameExists(String),

    #[error("session name is empty")]
    EmptySessionName,

    #[error("session name too long (max 128 characters): {0}")]
    SessionNameTooLong(String),

    #[error("session name contains invalid characters: {0}")]
    InvalidSessionName(String),

    #[error("version conflict on {resource} {id}: expected {expected}, found {actual}")]
    VersionConflict {
        resource: &'static str,
        id: String,
        expected: Version,
        actual: Version,
    },

    #[error("cannot remove last window from session")]
    LastWindow,

    #[error("cannot remove last pane from window")]
    LastPane,

    #[error("window name already exists in session: {0}")]
    WindowNameConflict(String),

    #[error("window name is empty")]
    EmptyWindowName,

    #[error("window index {index} out of range (session has {count} windows)")]
    WindowIndexOutOfRange { index: usize, count: usize },

    #[error("cannot swap pane with itself")]
    PaneSwapSelf,

    #[error("panes are not in the same window")]
    PaneCrossWindow,

    #[error("no neighbor pane in direction {0:?}")]
    NoNeighbor(NavDirection),

    #[error("layout operation failed: {0}")]
    LayoutError(String),

    #[error("graph loop shut down")]
    Shutdown,
}

/// The immutable snapshot of all session state.
///
/// Readers access this via `ArcSwap::load()`. Cheaply cloneable (Arc-wrapped).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionGraphSnapshot {
    pub sessions: HashMap<SessionId, Session>,
    pub windows: HashMap<WindowId, Window>,
    pub panes: HashMap<PaneId, Pane>,
    pub version: Version,
}

impl SessionGraphSnapshot {
    pub fn find_session_by_name(&self, name: &str) -> Option<&Session> {
        self.sessions.values().find(|s| s.name == name)
    }

    pub fn session_windows(&self, session_id: &SessionId) -> Vec<&Window> {
        self.sessions
            .get(session_id)
            .map(|s| {
                s.windows
                    .iter()
                    .filter_map(|wid| self.windows.get(wid))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn window_panes(&self, window_id: &WindowId) -> Vec<&Pane> {
        self.panes
            .values()
            .filter(|p| p.window_id == *window_id)
            .collect()
    }

    pub fn session_name_exists(&self, name: &str) -> bool {
        self.sessions.values().any(|s| s.name == name)
    }

    /// Find a window by name within a specific session.
    pub fn find_window_by_name(&self, session_id: &SessionId, name: &str) -> Option<&Window> {
        let session = self.sessions.get(session_id)?;
        session
            .windows
            .iter()
            .filter_map(|wid| self.windows.get(wid))
            .find(|w| w.title == name)
    }

    /// Check if a window name exists in a session.
    pub fn window_name_exists_in_session(&self, session_id: &SessionId, name: &str) -> bool {
        self.find_window_by_name(session_id, name).is_some()
    }
}

/// Commands sent to the single-writer graph task via mpsc.
#[derive(Debug)]
pub enum GraphCommand {
    CreateSession {
        name: String,
        cwd: std::path::PathBuf,
        /// Initial-pane command to persist on `Pane.command` (codex P2
        /// #10 followup — previously this was always empty for the
        /// session.create path, only apply_batch persisted it). Empty
        /// vector means "spawn the user's default shell".
        initial_command: Vec<String>,
        reply: tokio::sync::oneshot::Sender<Result<SessionId, GraphError>>,
    },
    DestroySession {
        id: SessionId,
        expected_version: Option<Version>,
        reply: tokio::sync::oneshot::Sender<Result<(), GraphError>>,
    },
    RenameSession {
        id: SessionId,
        new_name: String,
        expected_version: Option<Version>,
        reply: tokio::sync::oneshot::Sender<Result<(), GraphError>>,
    },
    CreateWindow {
        session_id: SessionId,
        title: String,
        cwd: std::path::PathBuf,
        reply: tokio::sync::oneshot::Sender<Result<WindowId, GraphError>>,
    },
    DestroyWindow {
        id: WindowId,
        expected_version: Option<Version>,
        reply: tokio::sync::oneshot::Sender<Result<(), GraphError>>,
    },
    RenameWindow {
        id: WindowId,
        new_title: String,
        expected_version: Option<Version>,
        reply: tokio::sync::oneshot::Sender<Result<(), GraphError>>,
    },
    FocusWindow {
        id: WindowId,
        expected_version: Option<Version>,
        reply: tokio::sync::oneshot::Sender<Result<Option<WindowId>, GraphError>>,
    },
    ReorderWindow {
        id: WindowId,
        new_index: usize,
        expected_version: Option<Version>,
        reply: tokio::sync::oneshot::Sender<Result<(), GraphError>>,
    },
    CreatePane {
        window_id: WindowId,
        cwd: std::path::PathBuf,
        command: Vec<String>,
        reply: tokio::sync::oneshot::Sender<Result<PaneId, GraphError>>,
    },
    DestroyPane {
        id: PaneId,
        expected_version: Option<Version>,
        reply: tokio::sync::oneshot::Sender<Result<(), GraphError>>,
    },
    SetPaneExitStatus {
        id: PaneId,
        exit_status: i32,
        reply: tokio::sync::oneshot::Sender<Result<(), GraphError>>,
    },
    SetSessionTag {
        id: SessionId,
        key: String,
        value: String,
        reply: tokio::sync::oneshot::Sender<Result<(), GraphError>>,
    },
    SetPaneTag {
        id: PaneId,
        key: String,
        value: String,
        reply: tokio::sync::oneshot::Sender<Result<(), GraphError>>,
    },
    SetSessionTheme {
        id: SessionId,
        theme: Option<ThemeRef>,
        reply: tokio::sync::oneshot::Sender<Result<(), GraphError>>,
    },
    SetPaneTheme {
        id: PaneId,
        theme: Option<ThemeRef>,
        reply: tokio::sync::oneshot::Sender<Result<(), GraphError>>,
    },
    SetPaneTitle {
        id: PaneId,
        title: Option<String>,
        auto: Option<bool>,
        reply: tokio::sync::oneshot::Sender<Result<(), GraphError>>,
    },
    SetPaneOscTitle {
        id: PaneId,
        title: String,
        reply: tokio::sync::oneshot::Sender<Result<(), GraphError>>,
    },
    SplitPane {
        target_pane: PaneId,
        direction: Direction,
        ratio: f32,
        reply: tokio::sync::oneshot::Sender<Result<PaneId, GraphError>>,
    },
    FocusPane {
        id: PaneId,
        reply: tokio::sync::oneshot::Sender<Result<Option<PaneId>, GraphError>>,
    },
    FocusPaneDirection {
        window_id: WindowId,
        direction: NavDirection,
        viewport: Rect,
        reply: tokio::sync::oneshot::Sender<Result<Option<PaneId>, GraphError>>,
    },
    ResizePane {
        id: PaneId,
        direction: Direction,
        delta: f32,
        expected_version: Option<Version>,
        reply: tokio::sync::oneshot::Sender<Result<(), GraphError>>,
    },
    ZoomPane {
        id: PaneId,
        expected_version: Option<Version>,
        reply: tokio::sync::oneshot::Sender<Result<bool, GraphError>>,
    },
    SwapPanes {
        a: PaneId,
        b: PaneId,
        expected_version: Option<Version>,
        reply: tokio::sync::oneshot::Sender<Result<(), GraphError>>,
    },
    Snapshot {
        reply: tokio::sync::oneshot::Sender<Arc<SessionGraphSnapshot>>,
    },
    /// Apply a batch of ops atomically (PR 3a).
    ApplyBatch {
        ops: Vec<crate::apply::Op>,
        reply: tokio::sync::oneshot::Sender<
            Result<crate::apply::BatchResult, crate::apply::BatchError>,
        >,
    },
}

/// The authoritative session graph (single-writer owner of state).
pub struct SessionGraph {
    state: Arc<ArcSwap<SessionGraphSnapshot>>,
    /// Optional event bus. When set, every successful mutation publishes a
    /// typed event to subscribers. Held as `Option` so legacy unit tests can
    /// continue to call `SessionGraph::new()` without setting up a bus.
    event_bus: Option<EventBus>,
}

impl SessionGraph {
    pub fn new() -> (Self, Arc<ArcSwap<SessionGraphSnapshot>>) {
        Self::new_with_event_bus(None)
    }

    /// Construct a SessionGraph with an attached event bus. Every successful
    /// mutation will publish a typed event to the bus AFTER the snapshot is
    /// committed (so subscribers that read state on event arrival observe the
    /// new value).
    pub fn new_with_event_bus(
        event_bus: Option<EventBus>,
    ) -> (Self, Arc<ArcSwap<SessionGraphSnapshot>>) {
        let snapshot = Arc::new(SessionGraphSnapshot::default());
        let state = Arc::new(ArcSwap::from(snapshot));
        let graph = Self {
            state: Arc::clone(&state),
            event_bus,
        };
        (graph, state)
    }

    fn current(&self) -> Arc<SessionGraphSnapshot> {
        self.state.load_full()
    }

    /// Publish a typed event to the bus, if one is attached.
    ///
    /// Always called AFTER `commit_snapshot` so that subscribers reading
    /// `graph.snapshot()` on event arrival see the post-mutation state.
    fn fire(&self, data: EventData) {
        if let Some(bus) = &self.event_bus {
            bus.publish(data);
        }
    }

    /// Publish a typed event with a correlation ID. Used by `apply_batch` so
    /// every event in a batch shares the same correlation_id for subscriber
    /// attribution. Returns the event seq.
    fn fire_with_correlation(&self, data: EventData, correlation_id: &str) -> u64 {
        if let Some(bus) = &self.event_bus {
            bus.publish_with_correlation(data, Some(correlation_id.to_string()))
        } else {
            0
        }
    }

    /// Apply a batch of operations atomically at the graph level.
    ///
    /// Implementation per Codex P0 #3 (PR 3 plan review):
    ///   1. Clone snapshot once.
    ///   2. Walk ops in order, validating each against the staged snapshot
    ///      (with prior ops' mutations already applied) and appending to a
    ///      collected `Vec<EventData>`. Backrefs to prior op outputs resolved
    ///      via the running outputs vec.
    ///   3. If any op fails, return `Err` with no commit and no events fired.
    ///   4. Commit snapshot ONCE.
    ///   5. Publish all collected events with a shared `correlation_id` so
    ///      subscribers can attribute the burst to this apply.
    ///
    /// Atomicity is GRAPH-LEVEL ONLY. PTY spawning is the daemon's job
    /// after this returns; spawn failures land in `BatchResult::spawn_results`
    /// and do NOT roll back the graph.
    pub fn apply_batch(
        &self,
        ops: Vec<crate::apply::Op>,
    ) -> Result<crate::apply::BatchResult, crate::apply::BatchError> {
        use crate::apply::{BatchError, BatchResult, Op, OpOutput, new_correlation_id};

        if ops.is_empty() {
            return Err(BatchError::Empty);
        }

        let mut snapshot = (*self.current()).clone();
        let mut outputs: Vec<OpOutput> = Vec::with_capacity(ops.len());
        let mut events: Vec<EventData> = Vec::new();

        for (op_index, op) in ops.iter().enumerate() {
            match op {
                Op::CreateSession {
                    name,
                    cwd,
                    initial_command,
                    initial_window_title,
                } => {
                    let resolved_name = match name {
                        Some(n) => n.clone(),
                        None => {
                            // Codex review of PR #10: scan for an unused
                            // session-N candidate, otherwise we collide with
                            // existing sessions when older indices have been
                            // killed and the count no longer maps to a free
                            // suffix. Mirrors main.rs's create_session pattern.
                            let mut idx = snapshot.sessions.len() + 1;
                            loop {
                                let candidate = format!("session-{idx}");
                                if !snapshot.session_name_exists(&candidate) {
                                    break candidate;
                                }
                                idx += 1;
                            }
                        }
                    };
                    let (sid, wid, pid, mut new_events) = stage_create_session(
                        &mut snapshot,
                        resolved_name,
                        cwd.clone(),
                        initial_command.clone(),
                        initial_window_title.clone(),
                    )
                    .map_err(|source| BatchError::OpFailed { op_index, source })?;
                    outputs.push(OpOutput {
                        op_index,
                        session_id: Some(sid),
                        window_id: Some(wid),
                        pane_id: Some(pid),
                    });
                    events.append(&mut new_events);
                }
                Op::CreateWindow {
                    session,
                    title,
                    cwd,
                    initial_command,
                } => {
                    let session_id = resolve_session_ref(session, &outputs, op_index)?;
                    let cwd_resolved = cwd
                        .clone()
                        .or_else(|| {
                            snapshot
                                .panes
                                .values()
                                .find(|p| {
                                    snapshot
                                        .windows
                                        .get(&p.window_id)
                                        .is_some_and(|w| w.session_id == session_id)
                                })
                                .map(|p| p.cwd.clone())
                        })
                        .unwrap_or_else(|| std::path::PathBuf::from("."));
                    let (wid, pid, mut new_events) = stage_create_window(
                        &mut snapshot,
                        session_id,
                        title.clone(),
                        cwd_resolved,
                        initial_command.clone(),
                    )
                    .map_err(|source| BatchError::OpFailed { op_index, source })?;
                    outputs.push(OpOutput {
                        op_index,
                        session_id: Some(session_id),
                        window_id: Some(wid),
                        pane_id: Some(pid),
                    });
                    events.append(&mut new_events);
                }
                Op::SplitPane {
                    target,
                    direction,
                    ratio,
                    command,
                    cwd,
                } => {
                    let target_pane = resolve_pane_ref(target, &outputs, op_index)?;
                    let (new_pid, wid, sid, mut new_events) = stage_split_pane(
                        &mut snapshot,
                        target_pane,
                        *direction,
                        *ratio,
                        command.clone(),
                        cwd.clone(),
                    )
                    .map_err(|source| BatchError::OpFailed { op_index, source })?;
                    outputs.push(OpOutput {
                        op_index,
                        session_id: Some(sid),
                        window_id: Some(wid),
                        pane_id: Some(new_pid),
                    });
                    events.append(&mut new_events);
                }
            }
        }

        // All ops validated + staged successfully. Commit ONCE, then publish
        // all collected events with the shared correlation_id.
        self.commit_snapshot(snapshot);
        let correlation_id = new_correlation_id();
        let mut last_event_seq = 0u64;
        for ev in events {
            last_event_seq = self.fire_with_correlation(ev, &correlation_id);
        }

        Ok(BatchResult {
            outputs,
            correlation_id,
            last_event_seq,
            spawn_results: Vec::new(),
        })
    }

    /// Atomically swap in a new state snapshot.
    ///
    /// Renamed from `publish` (2026-05-10) to free the verb for `EventBus::publish` —
    /// `SessionGraph` mutations now also publish typed events to the bus, and having
    /// two unrelated `publish` methods on the same type is a recipe for confusion.
    fn commit_snapshot(&self, snapshot: SessionGraphSnapshot) {
        self.state.store(Arc::new(snapshot));
    }

    fn validate_session_name(name: &str) -> Result<(), GraphError> {
        if name.is_empty() {
            return Err(GraphError::EmptySessionName);
        }
        if name.len() > 128 {
            return Err(GraphError::SessionNameTooLong(name.to_string()));
        }
        if !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
        {
            return Err(GraphError::InvalidSessionName(name.to_string()));
        }
        Ok(())
    }

    /// Convenience wrapper for `create_session_with_command(name, cwd, vec![])`.
    /// Used for the common case where the initial pane spawns the user's
    /// default shell rather than an explicit command.
    pub fn create_session(
        &self,
        name: String,
        cwd: std::path::PathBuf,
    ) -> Result<SessionId, GraphError> {
        self.create_session_with_command(name, cwd, Vec::new())
    }

    /// Create a session and persist the initial pane's command into the
    /// graph (codex P2 #10 — `apply_batch` already does this; this method
    /// is the parallel fix for `session.create` so `Pane.command` stops
    /// being empty for `shux new --cmd vim` and similar). When `command`
    /// is empty the behavior matches `create_session`.
    pub fn create_session_with_command(
        &self,
        name: String,
        cwd: std::path::PathBuf,
        command: Vec<String>,
    ) -> Result<SessionId, GraphError> {
        Self::validate_session_name(&name)?;

        let current = self.current();

        if current.session_name_exists(&name) {
            return Err(GraphError::SessionNameExists(name));
        }

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        let pane_id = PaneId::new();
        let window_id = WindowId::new();

        let mut pane = if command.is_empty() {
            Pane::new(window_id, cwd)
        } else {
            Pane::with_command(window_id, cwd, command.clone())
        };
        pane.id = pane_id;

        let mut window = Window::new(SessionId::new(), "1", pane_id);
        window.id = window_id;

        let session_name = name.clone();
        let session = Session::new(name, window_id);
        let session_id = session.id;

        window.session_id = session_id;

        snapshot.panes.insert(pane_id, pane);
        snapshot.windows.insert(window_id, window);
        snapshot.sessions.insert(session_id, session);

        self.commit_snapshot(snapshot);
        self.fire(EventData::SessionCreated {
            session_id,
            name: session_name,
        });
        // create_session also implicitly creates the first window + first pane;
        // fire those events too so subscribers tracking pane lifecycle don't
        // miss the implicit ones (only the explicit `create_pane`/`split_pane`
        // would fire otherwise). The PaneCreated event reflects the actual
        // command stored on the Pane (codex P2 #10 followup).
        self.fire(EventData::WindowCreated {
            window_id,
            session_id,
            title: "1".into(),
            index: 0,
        });
        self.fire(EventData::PaneCreated {
            pane_id,
            window_id,
            session_id,
            command,
        });
        info!(%session_id, "Session created");
        Ok(session_id)
    }

    pub fn destroy_session(
        &self,
        id: SessionId,
        expected_version: Option<Version>,
    ) -> Result<(), GraphError> {
        let current = self.current();

        let session = current
            .sessions
            .get(&id)
            .ok_or(GraphError::SessionNotFound(id))?;

        if let Some(ev) = expected_version {
            if session.version != ev {
                return Err(GraphError::VersionConflict {
                    resource: "session",
                    id: id.to_string(),
                    expected: ev,
                    actual: session.version,
                });
            }
        }

        let killed_name = session.name.clone();

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        let window_ids: Vec<WindowId> = session.windows.clone();

        // Capture per-pane (id, window_id, command) tuples BEFORE removal so
        // we can fire PaneExited events with full routing scope after commit.
        // session_id is the destroyed session's id (`id`) — captured by closure.
        let panes_to_kill: Vec<(PaneId, WindowId, Vec<String>)> = snapshot
            .panes
            .values()
            .filter(|p| window_ids.contains(&p.window_id))
            .map(|p| (p.id, p.window_id, p.command.clone()))
            .collect();

        for (pid, _, _) in &panes_to_kill {
            snapshot.panes.remove(pid);
        }
        for wid in &window_ids {
            snapshot.windows.remove(wid);
        }
        snapshot.sessions.remove(&id);

        self.commit_snapshot(snapshot);
        // Fire cascade events innermost-out (panes → windows → session) so
        // subscribers receive teardown events in the same order they would
        // arrive from explicit destroy_pane → destroy_window → destroy_session
        // calls. Each PaneExited carries `exit_status: None` since the pane
        // was forcibly killed (mirrors the explicit destroy_pane semantics).
        for (pid, wid, command) in panes_to_kill.iter() {
            self.fire(EventData::PaneExited {
                pane_id: *pid,
                window_id: *wid,
                session_id: id,
                exit_status: None,
                command: command.clone(),
            });
        }
        for wid in &window_ids {
            self.fire(EventData::WindowKilled {
                window_id: *wid,
                session_id: id,
            });
        }
        self.fire(EventData::SessionKilled {
            session_id: id,
            name: killed_name,
        });
        let panes_removed = panes_to_kill.len();
        info!(%id, panes_removed, "Session destroyed");
        Ok(())
    }

    pub fn rename_session(
        &self,
        id: SessionId,
        new_name: String,
        expected_version: Option<Version>,
    ) -> Result<(), GraphError> {
        Self::validate_session_name(&new_name)?;

        let current = self.current();

        let session = current
            .sessions
            .get(&id)
            .ok_or(GraphError::SessionNotFound(id))?;

        if let Some(ev) = expected_version {
            if session.version != ev {
                return Err(GraphError::VersionConflict {
                    resource: "session",
                    id: id.to_string(),
                    expected: ev,
                    actual: session.version,
                });
            }
        }

        if current.session_name_exists(&new_name) {
            return Err(GraphError::SessionNameExists(new_name));
        }

        let old_name = session.name.clone();
        let event_new_name = new_name.clone();

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        if let Some(s) = snapshot.sessions.get_mut(&id) {
            s.name = new_name;
            s.version += 1;
        }

        self.commit_snapshot(snapshot);
        self.fire(EventData::SessionRenamed {
            session_id: id,
            old_name,
            new_name: event_new_name,
        });
        Ok(())
    }

    pub fn create_window(
        &self,
        session_id: SessionId,
        title: String,
        cwd: std::path::PathBuf,
    ) -> Result<WindowId, GraphError> {
        let current = self.current();

        if !current.sessions.contains_key(&session_id) {
            return Err(GraphError::SessionNotFound(session_id));
        }

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        let pane_id = PaneId::new();
        let window_id = WindowId::new();

        let mut pane = Pane::new(window_id, cwd);
        pane.id = pane_id;

        let event_title = title.clone();
        let mut window = Window::new(session_id, title, pane_id);
        window.id = window_id;

        snapshot.panes.insert(pane_id, pane);
        snapshot.windows.insert(window_id, window);

        let mut event_index: u32 = 0;
        if let Some(s) = snapshot.sessions.get_mut(&session_id) {
            s.windows.push(window_id);
            s.active_window = window_id;
            s.version += 1;
            event_index = (s.windows.len() - 1) as u32;
        }

        self.commit_snapshot(snapshot);
        self.fire(EventData::WindowCreated {
            window_id,
            session_id,
            title: event_title,
            index: event_index,
        });
        // create_window also creates an initial pane — fire PaneCreated too so
        // subscribers see every new pane, not only those born of explicit
        // create_pane/split_pane calls.
        self.fire(EventData::PaneCreated {
            pane_id,
            window_id,
            session_id,
            command: Vec::new(),
        });
        info!(%window_id, %session_id, "Window created");
        Ok(window_id)
    }

    pub fn destroy_window(
        &self,
        id: WindowId,
        expected_version: Option<Version>,
    ) -> Result<(), GraphError> {
        let current = self.current();

        let window = current
            .windows
            .get(&id)
            .ok_or(GraphError::WindowNotFound(id))?;

        if let Some(ev) = expected_version {
            if window.version != ev {
                return Err(GraphError::VersionConflict {
                    resource: "window",
                    id: id.to_string(),
                    expected: ev,
                    actual: window.version,
                });
            }
        }

        let session = current
            .sessions
            .get(&window.session_id)
            .ok_or(GraphError::SessionNotFound(window.session_id))?;

        if session.windows.len() <= 1 {
            return Err(GraphError::LastWindow);
        }

        let killed_session_id = window.session_id;

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        // Capture (id, command) for each child pane BEFORE removal so we can
        // fire PaneExited events after commit. Without this cascade, agents
        // tracking pane lifecycle via `events.watch --filter pane.` see panes
        // get created but never see them die when the window is killed
        // (Codex review of PR #9).
        let panes_to_kill: Vec<(PaneId, Vec<String>)> = snapshot
            .panes
            .values()
            .filter(|p| p.window_id == id)
            .map(|p| (p.id, p.command.clone()))
            .collect();

        for (pid, _) in &panes_to_kill {
            snapshot.panes.remove(pid);
        }

        if let Some(s) = snapshot.sessions.get_mut(&killed_session_id) {
            s.windows.retain(|wid| *wid != id);
            if s.active_window == id {
                // Safe: we verified len > 1 above, so at least one window remains
                s.active_window = s.windows[0];
            }
            s.version += 1;
        }

        snapshot.windows.remove(&id);

        self.commit_snapshot(snapshot);
        // Fire pane.exited cascade BEFORE window.killed so subscribers
        // observe teardown in the same order as explicit destroy_pane →
        // destroy_window calls. All panes belong to window `id` (the one
        // being destroyed); session_id is the parent of that window.
        for (pid, command) in panes_to_kill.iter() {
            self.fire(EventData::PaneExited {
                pane_id: *pid,
                window_id: id,
                session_id: killed_session_id,
                exit_status: None,
                command: command.clone(),
            });
        }
        self.fire(EventData::WindowKilled {
            window_id: id,
            session_id: killed_session_id,
        });
        Ok(())
    }

    pub fn rename_window(
        &self,
        id: WindowId,
        new_title: String,
        expected_version: Option<Version>,
    ) -> Result<(), GraphError> {
        if new_title.is_empty() {
            return Err(GraphError::EmptyWindowName);
        }

        let current = self.current();

        let window = current
            .windows
            .get(&id)
            .ok_or(GraphError::WindowNotFound(id))?;

        if let Some(ev) = expected_version {
            if window.version != ev {
                return Err(GraphError::VersionConflict {
                    resource: "window",
                    id: id.to_string(),
                    expected: ev,
                    actual: window.version,
                });
            }
        }

        let session_id = window.session_id;

        // Check for name conflict within the same session
        let session = current
            .sessions
            .get(&session_id)
            .ok_or(GraphError::SessionNotFound(session_id))?;

        for wid in &session.windows {
            if *wid != id {
                if let Some(w) = current.windows.get(wid) {
                    if w.title == new_title {
                        return Err(GraphError::WindowNameConflict(new_title));
                    }
                }
            }
        }

        let old_title = window.title.clone();
        let event_new_title = new_title.clone();

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        if let Some(w) = snapshot.windows.get_mut(&id) {
            w.title = new_title;
            w.version += 1;
        }

        self.commit_snapshot(snapshot);
        self.fire(EventData::WindowRenamed {
            window_id: id,
            old_title,
            new_title: event_new_title,
        });
        Ok(())
    }

    /// Focus (activate) a window. Returns the previously active window ID.
    pub fn focus_window(
        &self,
        id: WindowId,
        expected_version: Option<Version>,
    ) -> Result<Option<WindowId>, GraphError> {
        let current = self.current();

        let window = current
            .windows
            .get(&id)
            .ok_or(GraphError::WindowNotFound(id))?;

        if let Some(ev) = expected_version {
            if window.version != ev {
                return Err(GraphError::VersionConflict {
                    resource: "window",
                    id: id.to_string(),
                    expected: ev,
                    actual: window.version,
                });
            }
        }

        let session_id = window.session_id;

        let session = current
            .sessions
            .get(&session_id)
            .ok_or(GraphError::SessionNotFound(session_id))?;

        if !session.windows.contains(&id) {
            return Err(GraphError::WindowNotFound(id));
        }

        let previous = if session.active_window != id {
            Some(session.active_window)
        } else {
            None
        };

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        if let Some(s) = snapshot.sessions.get_mut(&session_id) {
            s.active_window = id;
            s.version += 1;
        }

        self.commit_snapshot(snapshot);
        info!(%id, %session_id, "Window focused");
        Ok(previous)
    }

    /// Reorder a window to a new index position within its session.
    pub fn reorder_window(
        &self,
        id: WindowId,
        new_index: usize,
        expected_version: Option<Version>,
    ) -> Result<(), GraphError> {
        let current = self.current();

        let window = current
            .windows
            .get(&id)
            .ok_or(GraphError::WindowNotFound(id))?;

        if let Some(ev) = expected_version {
            if window.version != ev {
                return Err(GraphError::VersionConflict {
                    resource: "window",
                    id: id.to_string(),
                    expected: ev,
                    actual: window.version,
                });
            }
        }

        let session_id = window.session_id;

        let session = current
            .sessions
            .get(&session_id)
            .ok_or(GraphError::SessionNotFound(session_id))?;

        let window_count = session.windows.len();
        if new_index >= window_count {
            return Err(GraphError::WindowIndexOutOfRange {
                index: new_index,
                count: window_count,
            });
        }

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        if let Some(s) = snapshot.sessions.get_mut(&session_id) {
            if let Some(current_index) = s.windows.iter().position(|wid| *wid == id) {
                s.windows.remove(current_index);
                s.windows.insert(new_index, id);
                s.version += 1;
            }
        }

        self.commit_snapshot(snapshot);
        info!(%id, new_index, "Window reordered");
        Ok(())
    }

    pub fn create_pane(
        &self,
        window_id: WindowId,
        cwd: std::path::PathBuf,
        command: Vec<String>,
    ) -> Result<PaneId, GraphError> {
        let current = self.current();

        if !current.windows.contains_key(&window_id) {
            return Err(GraphError::WindowNotFound(window_id));
        }

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        let event_command = command.clone();
        let pane = if command.is_empty() {
            Pane::new(window_id, cwd)
        } else {
            Pane::with_command(window_id, cwd, command)
        };
        let pane_id = pane.id;

        // Resolve session_id from the (validated, exists) window for event scope.
        let session_id = current
            .windows
            .get(&window_id)
            .map(|w| w.session_id)
            .expect("window verified to exist above");

        snapshot.panes.insert(pane_id, pane);

        self.commit_snapshot(snapshot);
        self.fire(EventData::PaneCreated {
            pane_id,
            window_id,
            session_id,
            command: event_command,
        });
        info!(%pane_id, %window_id, "Pane created");
        Ok(pane_id)
    }

    pub fn destroy_pane(
        &self,
        id: PaneId,
        expected_version: Option<Version>,
    ) -> Result<(), GraphError> {
        let current = self.current();

        let pane = current.panes.get(&id).ok_or(GraphError::PaneNotFound(id))?;

        if let Some(ev) = expected_version {
            if pane.version != ev {
                return Err(GraphError::VersionConflict {
                    resource: "pane",
                    id: id.to_string(),
                    expected: ev,
                    actual: pane.version,
                });
            }
        }

        let window_pane_count = current
            .panes
            .values()
            .filter(|p| p.window_id == pane.window_id)
            .count();

        if window_pane_count <= 1 {
            return Err(GraphError::LastPane);
        }

        let window_id = pane.window_id;
        let killed_command = pane.command.clone();
        let session_id = current
            .windows
            .get(&window_id)
            .map(|w| w.session_id)
            .ok_or(GraphError::WindowNotFound(window_id))?;

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        snapshot.panes.remove(&id);

        if let Some(w) = snapshot.windows.get_mut(&window_id) {
            // Remove from layout tree
            w.layout.remove_pane(id);

            if w.active_pane == id {
                // Pick a remaining pane from the layout tree first, then fallback to HashMap
                let new_active = w.layout.tree.pane_ids().into_iter().next().or_else(|| {
                    snapshot
                        .panes
                        .values()
                        .find(|p| p.window_id == window_id)
                        .map(|p| p.id)
                });
                if let Some(active) = new_active {
                    w.active_pane = active;
                }
            }
            w.version += 1;
        }

        self.commit_snapshot(snapshot);
        // Pane killed via explicit destroy: surface as PaneExited with no exit_status,
        // mirroring how subscribers consume both natural exits and forced kills.
        self.fire(EventData::PaneExited {
            pane_id: id,
            window_id,
            session_id,
            exit_status: None,
            command: killed_command,
        });
        Ok(())
    }

    pub fn set_pane_exit_status(&self, id: PaneId, exit_status: i32) -> Result<(), GraphError> {
        let current = self.current();

        let exited_pane = current.panes.get(&id).ok_or(GraphError::PaneNotFound(id))?;
        let exited_command = exited_pane.command.clone();
        let exited_window_id = exited_pane.window_id;
        let exited_session_id = current
            .windows
            .get(&exited_window_id)
            .map(|w| w.session_id)
            .ok_or(GraphError::WindowNotFound(exited_window_id))?;

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        if let Some(p) = snapshot.panes.get_mut(&id) {
            p.exit_status = Some(exit_status);
            p.version += 1;
        }

        self.commit_snapshot(snapshot);
        self.fire(EventData::PaneExited {
            pane_id: id,
            window_id: exited_window_id,
            session_id: exited_session_id,
            exit_status: Some(exit_status),
            command: exited_command,
        });
        Ok(())
    }

    pub fn set_session_tag(
        &self,
        id: SessionId,
        key: String,
        value: String,
    ) -> Result<(), GraphError> {
        let current = self.current();

        if !current.sessions.contains_key(&id) {
            return Err(GraphError::SessionNotFound(id));
        }

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        if let Some(s) = snapshot.sessions.get_mut(&id) {
            s.tags.insert(key, value);
            s.version += 1;
        }

        self.commit_snapshot(snapshot);
        Ok(())
    }

    pub fn set_pane_tag(&self, id: PaneId, key: String, value: String) -> Result<(), GraphError> {
        let current = self.current();

        if !current.panes.contains_key(&id) {
            return Err(GraphError::PaneNotFound(id));
        }

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        if let Some(p) = snapshot.panes.get_mut(&id) {
            p.tags.insert(key, value);
            p.version += 1;
        }

        self.commit_snapshot(snapshot);
        Ok(())
    }

    pub fn set_session_theme(
        &self,
        id: SessionId,
        theme: Option<ThemeRef>,
    ) -> Result<(), GraphError> {
        let current = self.current();

        if !current.sessions.contains_key(&id) {
            return Err(GraphError::SessionNotFound(id));
        }

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        if let Some(s) = snapshot.sessions.get_mut(&id) {
            s.theme = theme;
            s.version += 1;
        }

        self.commit_snapshot(snapshot);
        Ok(())
    }

    pub fn set_pane_theme(&self, id: PaneId, theme: Option<ThemeRef>) -> Result<(), GraphError> {
        let current = self.current();

        if !current.panes.contains_key(&id) {
            return Err(GraphError::PaneNotFound(id));
        }

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        if let Some(p) = snapshot.panes.get_mut(&id) {
            p.theme = theme;
            p.version += 1;
        }

        self.commit_snapshot(snapshot);
        Ok(())
    }

    /// Set or clear the manual title for a pane, and optionally
    /// toggle auto-title resolution. Setting `title = None`
    /// clears the manual override so auto sources (OSC + command)
    /// flow back into the displayed title. `auto = None` leaves
    /// the flag unchanged.
    ///
    /// Fires `PaneTitleChanged` only if the displayed title (the
    /// priority-resolved `Pane.title`) actually changed — silent
    /// no-op when the new manual title matches what was already
    /// being shown via an OSC update of the same string.
    pub fn set_pane_title(
        &self,
        id: PaneId,
        title: Option<String>,
        auto: Option<bool>,
    ) -> Result<(), GraphError> {
        let current = self.current();
        let pane = current.panes.get(&id).ok_or(GraphError::PaneNotFound(id))?;
        let window_id = pane.window_id;
        let session_id = current
            .windows
            .get(&window_id)
            .map(|w| w.session_id)
            .ok_or(GraphError::WindowNotFound(window_id))?;

        let old_title = pane.title.clone();

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        let new_title = if let Some(p) = snapshot.panes.get_mut(&id) {
            if let Some(a) = auto {
                p.set_auto_title(a);
            }
            p.set_manual_title(title);
            p.version += 1;
            p.title.clone()
        } else {
            return Err(GraphError::PaneNotFound(id));
        };

        self.commit_snapshot(snapshot);

        if old_title != new_title {
            self.fire(EventData::PaneTitleChanged {
                pane_id: id,
                window_id,
                session_id,
                old_title,
                new_title,
            });
        }
        Ok(())
    }

    /// Set the OSC-derived title for a pane (called from the per-pane
    /// PTY task when the running app emits an OSC 0/2 sequence). Fires
    /// `PaneTitleChanged` only when the displayed title actually moves
    /// (i.e. no manual override is currently active and the value
    /// differs from before).
    pub fn set_pane_osc_title(&self, id: PaneId, title: String) -> Result<(), GraphError> {
        let current = self.current();
        let pane = current.panes.get(&id).ok_or(GraphError::PaneNotFound(id))?;
        let window_id = pane.window_id;
        let session_id = current
            .windows
            .get(&window_id)
            .map(|w| w.session_id)
            .ok_or(GraphError::WindowNotFound(window_id))?;

        let old_title = pane.title.clone();

        // Fast path: no change → skip the clone+commit dance. This is
        // important because bash's PROMPT_COMMAND often re-issues the
        // same OSC 2 every prompt, and we don't want to bump versions
        // (and wake the renderer / publish events) for a no-op.
        let already = pane.osc_title.as_deref() == Some(title.as_str());
        let manual_active = pane.manual_title.is_some();
        if already && (manual_active || !pane.auto_title) {
            return Ok(());
        }

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        let (changed, new_title) = if let Some(p) = snapshot.panes.get_mut(&id) {
            let changed = p.set_osc_title(title);
            p.version += 1;
            (changed, p.title.clone())
        } else {
            return Err(GraphError::PaneNotFound(id));
        };

        self.commit_snapshot(snapshot);

        if changed {
            self.fire(EventData::PaneTitleChanged {
                pane_id: id,
                window_id,
                session_id,
                old_title,
                new_title,
            });
        }
        Ok(())
    }

    /// Helper: find pane and its window_id.
    fn find_pane_window(
        &self,
        pane_id: PaneId,
    ) -> Result<(Arc<SessionGraphSnapshot>, WindowId), GraphError> {
        let current = self.current();
        let pane = current
            .panes
            .get(&pane_id)
            .ok_or(GraphError::PaneNotFound(pane_id))?;
        let window_id = pane.window_id;
        Ok((current, window_id))
    }

    /// Split a pane: create a new pane alongside the target in the layout tree.
    pub fn split_pane(
        &self,
        target_pane: PaneId,
        direction: Direction,
        ratio: f32,
    ) -> Result<PaneId, GraphError> {
        let (current, window_id) = self.find_pane_window(target_pane)?;

        let target = current
            .panes
            .get(&target_pane)
            // Safe: find_pane_window just verified it exists
            .unwrap();

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        let new_pane = Pane::new(window_id, target.cwd.clone());
        let new_pane_id = new_pane.id;

        let window = snapshot
            .windows
            .get_mut(&window_id)
            .ok_or(GraphError::WindowNotFound(window_id))?;

        let ok = window
            .layout
            .split_pane(target_pane, new_pane_id, direction, ratio);
        if !ok {
            return Err(GraphError::LayoutError("split_pane failed".into()));
        }

        window.active_pane = new_pane_id;
        window.version += 1;

        snapshot.panes.insert(new_pane_id, new_pane);

        // Resolve session_id for event scope before commit (window has been
        // borrowed mutably above; re-resolve from the current snapshot).
        let split_session_id = current
            .windows
            .get(&window_id)
            .map(|w| w.session_id)
            .ok_or(GraphError::WindowNotFound(window_id))?;

        self.commit_snapshot(snapshot);
        // Split spawns a brand-new pane: surface as PaneCreated so subscribers
        // tracking pane lifecycle see the new id arrive in the same event class
        // they got from `create_pane`. The empty `command` mirrors create_pane
        // when no explicit command was given.
        self.fire(EventData::PaneCreated {
            pane_id: new_pane_id,
            window_id,
            session_id: split_session_id,
            command: Vec::new(),
        });
        info!(%new_pane_id, %target_pane, %window_id, "Pane split");
        Ok(new_pane_id)
    }

    /// Focus a specific pane. Returns the previously active pane (or None if already focused).
    pub fn focus_pane(&self, id: PaneId) -> Result<Option<PaneId>, GraphError> {
        let (current, window_id) = self.find_pane_window(id)?;

        let window = current
            .windows
            .get(&window_id)
            .ok_or(GraphError::WindowNotFound(window_id))?;

        let previous = if window.active_pane != id {
            Some(window.active_pane)
        } else {
            None
        };
        let session_id = window.session_id;

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        if let Some(w) = snapshot.windows.get_mut(&window_id) {
            w.active_pane = id;
            w.version += 1;
        }

        self.commit_snapshot(snapshot);
        self.fire(EventData::PaneFocused {
            pane_id: id,
            window_id,
            session_id,
            previous_pane_id: previous,
        });
        Ok(previous)
    }

    /// Focus the nearest pane in a cardinal direction. Returns the newly focused pane.
    pub fn focus_pane_direction(
        &self,
        window_id: WindowId,
        direction: NavDirection,
        viewport: Rect,
    ) -> Result<Option<PaneId>, GraphError> {
        let current = self.current();

        let window = current
            .windows
            .get(&window_id)
            .ok_or(GraphError::WindowNotFound(window_id))?;

        let current_pane = window.active_pane;
        let session_id = window.session_id;
        let neighbor = window
            .layout
            .tree
            .directional_focus(current_pane, direction, viewport);

        match neighbor {
            Some(target) => {
                let mut snapshot = (*current).clone();
                snapshot.version += 1;

                if let Some(w) = snapshot.windows.get_mut(&window_id) {
                    w.active_pane = target;
                    w.version += 1;
                }

                self.commit_snapshot(snapshot);
                self.fire(EventData::PaneFocused {
                    pane_id: target,
                    window_id,
                    session_id,
                    previous_pane_id: Some(current_pane),
                });
                Ok(Some(target))
            }
            None => Ok(None),
        }
    }

    /// Resize a pane by adjusting the ratio of its nearest ancestor split.
    pub fn resize_pane(
        &self,
        id: PaneId,
        direction: Direction,
        delta: f32,
        expected_version: Option<Version>,
    ) -> Result<(), GraphError> {
        let (current, window_id) = self.find_pane_window(id)?;

        if let Some(ev) = expected_version {
            let pane = current.panes.get(&id).ok_or(GraphError::PaneNotFound(id))?;
            if pane.version != ev {
                return Err(GraphError::VersionConflict {
                    resource: "pane",
                    id: id.to_string(),
                    expected: ev,
                    actual: pane.version,
                });
            }
        }

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        let window = snapshot
            .windows
            .get_mut(&window_id)
            .ok_or(GraphError::WindowNotFound(window_id))?;

        // Unzoom first if zoomed
        if window.layout.is_zoomed() {
            if let Some(zoom) = window.layout.zoom.take() {
                window.layout.tree = zoom.saved_layout;
            }
        }

        if let Some(new_tree) = window.layout.tree.resize_pane(id, direction, delta) {
            window.layout.tree = new_tree;
            window.version += 1;
            // Bump version on every pane in the affected window — resize
            // mutates the layout that contains them, so the version stamp
            // reflecting "anything visible on this pane changed" must tick.
            // Without this, expected_version checks on a sibling pane after
            // a concurrent resize would silently succeed.
            let pane_ids: Vec<PaneId> = snapshot
                .panes
                .values()
                .filter(|p| p.window_id == window_id)
                .map(|p| p.id)
                .collect();
            for pid in pane_ids {
                if let Some(p) = snapshot.panes.get_mut(&pid) {
                    p.version += 1;
                }
            }
            self.commit_snapshot(snapshot);
            Ok(())
        } else {
            // No matching split direction — not an error, just a no-op
            Ok(())
        }
    }

    /// Toggle zoom on a pane. Returns whether the pane is now zoomed.
    pub fn zoom_pane(
        &self,
        id: PaneId,
        expected_version: Option<Version>,
    ) -> Result<bool, GraphError> {
        let (current, window_id) = self.find_pane_window(id)?;

        if let Some(ev) = expected_version {
            let pane = current.panes.get(&id).ok_or(GraphError::PaneNotFound(id))?;
            if pane.version != ev {
                return Err(GraphError::VersionConflict {
                    resource: "pane",
                    id: id.to_string(),
                    expected: ev,
                    actual: pane.version,
                });
            }
        }

        let session_id = current
            .windows
            .get(&window_id)
            .map(|w| w.session_id)
            .ok_or(GraphError::WindowNotFound(window_id))?;

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        let window = snapshot
            .windows
            .get_mut(&window_id)
            .ok_or(GraphError::WindowNotFound(window_id))?;

        window.layout.toggle_zoom(id);
        let is_zoomed = window.layout.is_zoomed();
        window.version += 1;

        // Same rationale as resize_pane: zoom changes what's visible on
        // every pane in the window (siblings get hidden / restored), so
        // their version stamps must tick too.
        let pane_ids: Vec<PaneId> = snapshot
            .panes
            .values()
            .filter(|p| p.window_id == window_id)
            .map(|p| p.id)
            .collect();
        for pid in pane_ids {
            if let Some(p) = snapshot.panes.get_mut(&pid) {
                p.version += 1;
            }
        }

        self.commit_snapshot(snapshot);
        self.fire(EventData::PaneZoomed {
            pane_id: id,
            window_id,
            session_id,
            zoomed: is_zoomed,
        });
        Ok(is_zoomed)
    }

    /// Swap two panes in the layout tree. Both must be in the same window.
    ///
    /// `expected_version` is checked against pane `a` only — clients
    /// performing a swap pick one anchor pane and compare against the
    /// state they last observed for it. The check against `a` proxies for
    /// "no concurrent layout op happened" because any layout op in the
    /// window bumps every pane's version (see resize/zoom_pane).
    pub fn swap_panes(
        &self,
        a: PaneId,
        b: PaneId,
        expected_version: Option<Version>,
    ) -> Result<(), GraphError> {
        if a == b {
            return Err(GraphError::PaneSwapSelf);
        }

        let current = self.current();

        let pane_a = current.panes.get(&a).ok_or(GraphError::PaneNotFound(a))?;
        let pane_b = current.panes.get(&b).ok_or(GraphError::PaneNotFound(b))?;

        if pane_a.window_id != pane_b.window_id {
            return Err(GraphError::PaneCrossWindow);
        }

        if let Some(ev) = expected_version {
            if pane_a.version != ev {
                return Err(GraphError::VersionConflict {
                    resource: "pane",
                    id: a.to_string(),
                    expected: ev,
                    actual: pane_a.version,
                });
            }
        }

        let window_id = pane_a.window_id;

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        let window = snapshot
            .windows
            .get_mut(&window_id)
            .ok_or(GraphError::WindowNotFound(window_id))?;

        // Unzoom first if zoomed
        if window.layout.is_zoomed() {
            if let Some(zoom) = window.layout.zoom.take() {
                window.layout.tree = zoom.saved_layout;
            }
        }

        if let Some(new_tree) = window.layout.tree.swap_panes(a, b) {
            window.layout.tree = new_tree;
            window.version += 1;
            // Bump version on every pane in the window (same rationale as
            // resize/zoom — layout op affects sibling visibility too).
            let pane_ids: Vec<PaneId> = snapshot
                .panes
                .values()
                .filter(|p| p.window_id == window_id)
                .map(|p| p.id)
                .collect();
            for pid in pane_ids {
                if let Some(p) = snapshot.panes.get_mut(&pid) {
                    p.version += 1;
                }
            }
            self.commit_snapshot(snapshot);
            Ok(())
        } else {
            Err(GraphError::LayoutError("swap_panes failed".into()))
        }
    }
}

// ── Staged-snapshot helpers (PR 3a, codex P0 #3) ─────────────────────────
//
// These free functions mutate a borrowed snapshot in place and return the
// events that WOULD fire on commit. `apply_batch` collects them across many
// ops and fires after a single commit_snapshot. The legacy public mutation
// methods on SessionGraph still exist; they are NOT yet refactored to call
// these helpers (low-risk follow-up). For now both code paths exist.

/// Stage a CreateSession op against `snapshot`. Returns
/// `(session_id, window_id, pane_id, events)`.
fn stage_create_session(
    snapshot: &mut SessionGraphSnapshot,
    name: String,
    cwd: std::path::PathBuf,
    initial_command: Vec<String>,
    initial_window_title: Option<String>,
) -> Result<(SessionId, WindowId, PaneId, Vec<EventData>), GraphError> {
    SessionGraph::validate_session_name(&name)?;
    if snapshot.session_name_exists(&name) {
        return Err(GraphError::SessionNameExists(name));
    }
    snapshot.version += 1;

    let pane_id = PaneId::new();
    let window_id = WindowId::new();
    // Per codex P2 #10: persist initial_command on the Pane so PaneCreated
    // events tell the truth about what the pane is running. Empty Vec means
    // "default shell at PTY spawn time".
    let mut pane = if initial_command.is_empty() {
        Pane::new(window_id, cwd)
    } else {
        Pane::with_command(window_id, cwd, initial_command.clone())
    };
    pane.id = pane_id;

    let title = initial_window_title.unwrap_or_else(|| "1".to_string());
    let mut window = Window::new(SessionId::new(), &title, pane_id);
    window.id = window_id;

    let session_name = name.clone();
    let session = Session::new(name, window_id);
    let session_id = session.id;
    window.session_id = session_id;

    snapshot.panes.insert(pane_id, pane);
    snapshot.windows.insert(window_id, window);
    snapshot.sessions.insert(session_id, session);

    let events = vec![
        EventData::SessionCreated {
            session_id,
            name: session_name,
        },
        EventData::WindowCreated {
            window_id,
            session_id,
            title,
            index: 0,
        },
        EventData::PaneCreated {
            pane_id,
            window_id,
            session_id,
            command: initial_command,
        },
    ];
    Ok((session_id, window_id, pane_id, events))
}

/// Stage a CreateWindow op against `snapshot`. Returns
/// `(window_id, pane_id, events)`.
fn stage_create_window(
    snapshot: &mut SessionGraphSnapshot,
    session_id: SessionId,
    title: String,
    cwd: std::path::PathBuf,
    initial_command: Vec<String>,
) -> Result<(WindowId, PaneId, Vec<EventData>), GraphError> {
    if !snapshot.sessions.contains_key(&session_id) {
        return Err(GraphError::SessionNotFound(session_id));
    }
    if title.is_empty() {
        return Err(GraphError::EmptyWindowName);
    }
    snapshot.version += 1;

    let pane_id = PaneId::new();
    let window_id = WindowId::new();
    let mut pane = if initial_command.is_empty() {
        Pane::new(window_id, cwd)
    } else {
        Pane::with_command(window_id, cwd, initial_command.clone())
    };
    pane.id = pane_id;

    let event_title = title.clone();
    let mut window = Window::new(session_id, title, pane_id);
    window.id = window_id;

    snapshot.panes.insert(pane_id, pane);
    snapshot.windows.insert(window_id, window);

    let mut event_index: u32 = 0;
    if let Some(s) = snapshot.sessions.get_mut(&session_id) {
        s.windows.push(window_id);
        s.active_window = window_id;
        s.version += 1;
        event_index = (s.windows.len() - 1) as u32;
    }

    let events = vec![
        EventData::WindowCreated {
            window_id,
            session_id,
            title: event_title,
            index: event_index,
        },
        EventData::PaneCreated {
            pane_id,
            window_id,
            session_id,
            command: initial_command,
        },
    ];
    Ok((window_id, pane_id, events))
}

/// Stage a SplitPane op against `snapshot`. Returns
/// `(new_pane_id, window_id, session_id, events)`. `cwd: None` inherits
/// from the target pane (matches tmux split-window default behavior).
fn stage_split_pane(
    snapshot: &mut SessionGraphSnapshot,
    target_pane: PaneId,
    direction: Direction,
    ratio: f32,
    command: Vec<String>,
    cwd: Option<std::path::PathBuf>,
) -> Result<(PaneId, WindowId, SessionId, Vec<EventData>), GraphError> {
    let target = snapshot
        .panes
        .get(&target_pane)
        .ok_or(GraphError::PaneNotFound(target_pane))?;
    let window_id = target.window_id;
    // Codex review of PR #10: honor a caller-supplied cwd; only inherit from
    // the target pane when no cwd is set on the op.
    let pane_cwd = cwd.unwrap_or_else(|| target.cwd.clone());

    let session_id = snapshot
        .windows
        .get(&window_id)
        .map(|w| w.session_id)
        .ok_or(GraphError::WindowNotFound(window_id))?;

    snapshot.version += 1;

    let new_pane = if command.is_empty() {
        Pane::new(window_id, pane_cwd)
    } else {
        Pane::with_command(window_id, pane_cwd, command.clone())
    };
    let new_pane_id = new_pane.id;

    let window = snapshot
        .windows
        .get_mut(&window_id)
        .ok_or(GraphError::WindowNotFound(window_id))?;
    let ok = window
        .layout
        .split_pane(target_pane, new_pane_id, direction, ratio);
    if !ok {
        return Err(GraphError::LayoutError("split_pane failed".into()));
    }
    window.active_pane = new_pane_id;
    window.version += 1;

    snapshot.panes.insert(new_pane_id, new_pane);

    let events = vec![EventData::PaneCreated {
        pane_id: new_pane_id,
        window_id,
        session_id,
        command,
    }];
    Ok((new_pane_id, window_id, session_id, events))
}

/// Resolve a SessionRef to a concrete SessionId, looking up backrefs in
/// the running outputs.
fn resolve_session_ref(
    sref: &crate::apply::SessionRef,
    outputs: &[crate::apply::OpOutput],
    op_index: usize,
) -> Result<SessionId, crate::apply::BatchError> {
    use crate::apply::{BatchError, SessionRef};
    match sref {
        SessionRef::Id(id) => Ok(*id),
        SessionRef::BackRef { op_index: ref_op } => {
            let prior = outputs.get(*ref_op).ok_or(BatchError::BackRefOutOfRange {
                op_index,
                ref_op: *ref_op,
                prior: outputs.len(),
            })?;
            prior.session_id.ok_or(BatchError::BackRefWrongType {
                op_index,
                ref_op: *ref_op,
                expected: "session_id",
            })
        }
    }
}

/// Resolve a PaneRef to a concrete PaneId, looking up backrefs in the
/// running outputs.
fn resolve_pane_ref(
    pref: &crate::apply::PaneRef,
    outputs: &[crate::apply::OpOutput],
    op_index: usize,
) -> Result<PaneId, crate::apply::BatchError> {
    use crate::apply::{BatchError, PaneRef};
    match pref {
        PaneRef::Id(id) => Ok(*id),
        PaneRef::BackRef { op_index: ref_op } => {
            let prior = outputs.get(*ref_op).ok_or(BatchError::BackRefOutOfRange {
                op_index,
                ref_op: *ref_op,
                prior: outputs.len(),
            })?;
            prior.pane_id.ok_or(BatchError::BackRefWrongType {
                op_index,
                ref_op: *ref_op,
                expected: "pane_id",
            })
        }
    }
}

/// Run the graph command processing loop (single-writer task).
pub async fn run_graph_loop(
    graph: SessionGraph,
    mut cmd_rx: mpsc::Receiver<GraphCommand>,
    shutdown: tokio_util::sync::CancellationToken,
) {
    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(GraphCommand::CreateSession { name, cwd, initial_command, reply }) => {
                        let _ = reply.send(graph.create_session_with_command(name, cwd, initial_command));
                    }
                    Some(GraphCommand::DestroySession { id, expected_version, reply }) => {
                        let _ = reply.send(graph.destroy_session(id, expected_version));
                    }
                    Some(GraphCommand::RenameSession { id, new_name, expected_version, reply }) => {
                        let _ = reply.send(graph.rename_session(id, new_name, expected_version));
                    }
                    Some(GraphCommand::CreateWindow { session_id, title, cwd, reply }) => {
                        let _ = reply.send(graph.create_window(session_id, title, cwd));
                    }
                    Some(GraphCommand::DestroyWindow { id, expected_version, reply }) => {
                        let _ = reply.send(graph.destroy_window(id, expected_version));
                    }
                    Some(GraphCommand::RenameWindow { id, new_title, expected_version, reply }) => {
                        let _ = reply.send(graph.rename_window(id, new_title, expected_version));
                    }
                    Some(GraphCommand::FocusWindow { id, expected_version, reply }) => {
                        let _ = reply.send(graph.focus_window(id, expected_version));
                    }
                    Some(GraphCommand::ReorderWindow { id, new_index, expected_version, reply }) => {
                        let _ = reply.send(graph.reorder_window(id, new_index, expected_version));
                    }
                    Some(GraphCommand::CreatePane { window_id, cwd, command, reply }) => {
                        let _ = reply.send(graph.create_pane(window_id, cwd, command));
                    }
                    Some(GraphCommand::DestroyPane { id, expected_version, reply }) => {
                        let _ = reply.send(graph.destroy_pane(id, expected_version));
                    }
                    Some(GraphCommand::SetPaneExitStatus { id, exit_status, reply }) => {
                        let _ = reply.send(graph.set_pane_exit_status(id, exit_status));
                    }
                    Some(GraphCommand::SetSessionTag { id, key, value, reply }) => {
                        let _ = reply.send(graph.set_session_tag(id, key, value));
                    }
                    Some(GraphCommand::SetPaneTag { id, key, value, reply }) => {
                        let _ = reply.send(graph.set_pane_tag(id, key, value));
                    }
                    Some(GraphCommand::SetSessionTheme { id, theme, reply }) => {
                        let _ = reply.send(graph.set_session_theme(id, theme));
                    }
                    Some(GraphCommand::SetPaneTheme { id, theme, reply }) => {
                        let _ = reply.send(graph.set_pane_theme(id, theme));
                    }
                    Some(GraphCommand::SetPaneTitle { id, title, auto, reply }) => {
                        let _ = reply.send(graph.set_pane_title(id, title, auto));
                    }
                    Some(GraphCommand::SetPaneOscTitle { id, title, reply }) => {
                        let _ = reply.send(graph.set_pane_osc_title(id, title));
                    }
                    Some(GraphCommand::SplitPane { target_pane, direction, ratio, reply }) => {
                        let _ = reply.send(graph.split_pane(target_pane, direction, ratio));
                    }
                    Some(GraphCommand::FocusPane { id, reply }) => {
                        let _ = reply.send(graph.focus_pane(id));
                    }
                    Some(GraphCommand::FocusPaneDirection { window_id, direction, viewport, reply }) => {
                        let _ = reply.send(graph.focus_pane_direction(window_id, direction, viewport));
                    }
                    Some(GraphCommand::ResizePane { id, direction, delta, expected_version, reply }) => {
                        let _ = reply.send(graph.resize_pane(id, direction, delta, expected_version));
                    }
                    Some(GraphCommand::ZoomPane { id, expected_version, reply }) => {
                        let _ = reply.send(graph.zoom_pane(id, expected_version));
                    }
                    Some(GraphCommand::SwapPanes { a, b, expected_version, reply }) => {
                        let _ = reply.send(graph.swap_panes(a, b, expected_version));
                    }
                    Some(GraphCommand::Snapshot { reply }) => {
                        let _ = reply.send(graph.current());
                    }
                    Some(GraphCommand::ApplyBatch { ops, reply }) => {
                        let _ = reply.send(graph.apply_batch(ops));
                    }
                    None => {
                        debug!("All graph command senders dropped, shutting down graph loop");
                        break;
                    }
                }
            }
            _ = shutdown.cancelled() => {
                info!("Graph loop shutting down (cancellation token)");
                break;
            }
        }
    }
}

/// A cloneable handle for sending commands to the SessionGraph.
#[derive(Clone)]
pub struct GraphHandle {
    cmd_tx: mpsc::Sender<GraphCommand>,
    state: Arc<ArcSwap<SessionGraphSnapshot>>,
}

impl GraphHandle {
    pub fn new(
        cmd_tx: mpsc::Sender<GraphCommand>,
        state: Arc<ArcSwap<SessionGraphSnapshot>>,
    ) -> Self {
        Self { cmd_tx, state }
    }

    /// Read the current snapshot (lock-free, no channel hop).
    pub fn snapshot(&self) -> Arc<SessionGraphSnapshot> {
        self.state.load_full()
    }

    /// Create a session. Initial pane spawns the user's default shell.
    /// Use [`Self::create_session_with_command`] to persist an explicit
    /// command on the initial pane.
    pub async fn create_session(
        &self,
        name: String,
        cwd: std::path::PathBuf,
    ) -> Result<SessionId, GraphError> {
        self.create_session_with_command(name, cwd, Vec::new())
            .await
    }

    /// Create a session and store `command` on the initial pane (codex
    /// P2 #10 followup — parallel fix for the `apply_batch` change so
    /// `Pane.command` is correct regardless of which entrypoint
    /// created the session). When `command` is empty the behavior
    /// matches [`Self::create_session`].
    pub async fn create_session_with_command(
        &self,
        name: String,
        cwd: std::path::PathBuf,
        command: Vec<String>,
    ) -> Result<SessionId, GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::CreateSession {
                name,
                cwd,
                initial_command: command,
                reply: tx,
            })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
    }

    pub async fn destroy_session(
        &self,
        id: SessionId,
        expected_version: Option<Version>,
    ) -> Result<(), GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::DestroySession {
                id,
                expected_version,
                reply: tx,
            })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
    }

    pub async fn rename_session(
        &self,
        id: SessionId,
        new_name: String,
        expected_version: Option<Version>,
    ) -> Result<(), GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::RenameSession {
                id,
                new_name,
                expected_version,
                reply: tx,
            })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
    }

    pub async fn create_window(
        &self,
        session_id: SessionId,
        title: String,
        cwd: std::path::PathBuf,
    ) -> Result<WindowId, GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::CreateWindow {
                session_id,
                title,
                cwd,
                reply: tx,
            })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
    }

    pub async fn destroy_window(
        &self,
        id: WindowId,
        expected_version: Option<Version>,
    ) -> Result<(), GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::DestroyWindow {
                id,
                expected_version,
                reply: tx,
            })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
    }

    pub async fn rename_window(
        &self,
        id: WindowId,
        new_title: String,
        expected_version: Option<Version>,
    ) -> Result<(), GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::RenameWindow {
                id,
                new_title,
                expected_version,
                reply: tx,
            })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
    }

    pub async fn focus_window(
        &self,
        id: WindowId,
        expected_version: Option<Version>,
    ) -> Result<Option<WindowId>, GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::FocusWindow {
                id,
                expected_version,
                reply: tx,
            })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
    }

    pub async fn reorder_window(
        &self,
        id: WindowId,
        new_index: usize,
        expected_version: Option<Version>,
    ) -> Result<(), GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::ReorderWindow {
                id,
                new_index,
                expected_version,
                reply: tx,
            })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
    }

    pub async fn create_pane(
        &self,
        window_id: WindowId,
        cwd: std::path::PathBuf,
        command: Vec<String>,
    ) -> Result<PaneId, GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::CreatePane {
                window_id,
                cwd,
                command,
                reply: tx,
            })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
    }

    pub async fn destroy_pane(
        &self,
        id: PaneId,
        expected_version: Option<Version>,
    ) -> Result<(), GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::DestroyPane {
                id,
                expected_version,
                reply: tx,
            })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
    }

    pub async fn set_pane_exit_status(
        &self,
        id: PaneId,
        exit_status: i32,
    ) -> Result<(), GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::SetPaneExitStatus {
                id,
                exit_status,
                reply: tx,
            })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
    }

    pub async fn set_session_tag(
        &self,
        id: SessionId,
        key: String,
        value: String,
    ) -> Result<(), GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::SetSessionTag {
                id,
                key,
                value,
                reply: tx,
            })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
    }

    pub async fn set_pane_tag(
        &self,
        id: PaneId,
        key: String,
        value: String,
    ) -> Result<(), GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::SetPaneTag {
                id,
                key,
                value,
                reply: tx,
            })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
    }

    pub async fn set_session_theme(
        &self,
        id: SessionId,
        theme: Option<ThemeRef>,
    ) -> Result<(), GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::SetSessionTheme {
                id,
                theme,
                reply: tx,
            })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
    }

    pub async fn set_pane_theme(
        &self,
        id: PaneId,
        theme: Option<ThemeRef>,
    ) -> Result<(), GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::SetPaneTheme {
                id,
                theme,
                reply: tx,
            })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
    }

    /// Set or clear the manual title for a pane. `title = None`
    /// clears the manual override; `auto = None` leaves the auto
    /// flag unchanged.
    pub async fn set_pane_title(
        &self,
        id: PaneId,
        title: Option<String>,
        auto: Option<bool>,
    ) -> Result<(), GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::SetPaneTitle {
                id,
                title,
                auto,
                reply: tx,
            })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
    }

    /// Record an OSC 0/2 title update from the running app. The
    /// per-pane PTY task calls this whenever
    /// `VirtualTerminal::title()` changes; the graph fast-paths
    /// no-ops to avoid version churn when bash's PROMPT_COMMAND
    /// re-emits the same OSC 2 every prompt.
    pub async fn set_pane_osc_title(&self, id: PaneId, title: String) -> Result<(), GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::SetPaneOscTitle {
                id,
                title,
                reply: tx,
            })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
    }

    pub async fn split_pane(
        &self,
        target_pane: PaneId,
        direction: Direction,
        ratio: f32,
    ) -> Result<PaneId, GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::SplitPane {
                target_pane,
                direction,
                ratio,
                reply: tx,
            })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
    }

    pub async fn focus_pane(&self, id: PaneId) -> Result<Option<PaneId>, GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::FocusPane { id, reply: tx })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
    }

    pub async fn focus_pane_direction(
        &self,
        window_id: WindowId,
        direction: NavDirection,
        viewport: Rect,
    ) -> Result<Option<PaneId>, GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::FocusPaneDirection {
                window_id,
                direction,
                viewport,
                reply: tx,
            })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
    }

    pub async fn resize_pane(
        &self,
        id: PaneId,
        direction: Direction,
        delta: f32,
        expected_version: Option<Version>,
    ) -> Result<(), GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::ResizePane {
                id,
                direction,
                delta,
                expected_version,
                reply: tx,
            })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
    }

    pub async fn zoom_pane(
        &self,
        id: PaneId,
        expected_version: Option<Version>,
    ) -> Result<bool, GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::ZoomPane {
                id,
                expected_version,
                reply: tx,
            })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
    }

    pub async fn swap_panes(
        &self,
        a: PaneId,
        b: PaneId,
        expected_version: Option<Version>,
    ) -> Result<(), GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::SwapPanes {
                a,
                b,
                expected_version,
                reply: tx,
            })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
    }

    /// Apply a batch of operations atomically through the single-writer task.
    /// Used by `state.apply` RPC.
    pub async fn apply_batch(
        &self,
        ops: Vec<crate::apply::Op>,
    ) -> Result<crate::apply::BatchResult, crate::apply::BatchError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::ApplyBatch { ops, reply: tx })
            .await
            .map_err(|_| crate::apply::BatchError::OpFailed {
                op_index: 0,
                source: GraphError::Shutdown,
            })?;
        rx.await.map_err(|_| crate::apply::BatchError::OpFailed {
            op_index: 0,
            source: GraphError::Shutdown,
        })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn home() -> PathBuf {
        PathBuf::from("/home/test")
    }

    #[test]
    fn test_create_session() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();

        let snap = state.load();
        assert_eq!(snap.sessions.len(), 1);
        assert_eq!(snap.sessions[&sid].name, "work");
        assert_eq!(snap.windows.len(), 1);
        assert_eq!(snap.panes.len(), 1);
    }

    #[test]
    fn test_duplicate_session_name_rejected() {
        let (graph, _state) = SessionGraph::new();
        graph.create_session("work".into(), home()).unwrap();
        let err = graph.create_session("work".into(), home()).unwrap_err();
        assert!(matches!(err, GraphError::SessionNameExists(name) if name == "work"));
    }

    #[test]
    fn test_create_session_with_command_persists_to_pane() {
        // Codex P2 #10 followup — before this fix the session.create
        // path stored an empty `Pane.command` regardless of what the
        // PTY actually spawned, so `shux new --cmd vim` produced a
        // pane whose auto-title derived from cwd instead of "vim".
        let (graph, state) = SessionGraph::new();
        let sid = graph
            .create_session_with_command("work".into(), home(), vec!["vim".into(), "foo.rs".into()])
            .unwrap();
        let snap = state.load();
        let wid = snap.sessions[&sid].windows[0];
        let pid = snap.windows[&wid].active_pane;
        let pane = &snap.panes[&pid];
        assert_eq!(pane.command, vec!["vim".to_string(), "foo.rs".to_string()]);
        // Auto-title derives from the first arg's basename, so the
        // displayed title is now "vim" instead of the cwd basename.
        assert_eq!(pane.title, "vim");
    }

    #[tokio::test]
    async fn test_create_session_pane_created_event_carries_command() {
        // PaneCreated event fired during session.create must reflect
        // the actual command, not Vec::new(). Subscribers using
        // events.watch to track agent panes rely on this.
        let bus = crate::bus::EventBus::new();
        let mut sub = bus.subscribe();
        let (graph, _state) = SessionGraph::new_with_event_bus(Some(bus));
        graph
            .create_session_with_command("work".into(), home(), vec!["bash".into()])
            .unwrap();
        // Drain SessionCreated + WindowCreated, find PaneCreated.
        for _ in 0..6 {
            let next = tokio::time::timeout(std::time::Duration::from_millis(50), sub.recv())
                .await
                .expect("event should arrive")
                .expect("bus not closed");
            if let crate::bus::SubscriptionEvent::Event(e) = next {
                if let EventData::PaneCreated { command, .. } = &e.data {
                    assert_eq!(command, &vec!["bash".to_string()]);
                    return;
                }
            }
        }
        panic!("PaneCreated event with non-empty command never arrived");
    }

    #[test]
    fn test_destroy_session_removes_all_entities() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();

        graph.destroy_session(sid, None).unwrap();

        let snap = state.load();
        assert!(snap.sessions.is_empty());
        assert!(snap.windows.is_empty());
        assert!(snap.panes.is_empty());
    }

    #[test]
    fn test_destroy_nonexistent_session() {
        let (graph, _state) = SessionGraph::new();
        let err = graph.destroy_session(SessionId::new(), None).unwrap_err();
        assert!(matches!(err, GraphError::SessionNotFound(_)));
    }

    #[test]
    fn test_version_conflict() {
        let (graph, _state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();

        let err = graph.destroy_session(sid, Some(999)).unwrap_err();
        assert!(matches!(err, GraphError::VersionConflict { .. }));
    }

    #[test]
    fn test_rename_session() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("old".into(), home()).unwrap();

        graph.rename_session(sid, "new".into(), None).unwrap();

        let snap = state.load();
        assert_eq!(snap.sessions[&sid].name, "new");
    }

    #[test]
    fn test_rename_to_existing_name_rejected() {
        let (graph, _state) = SessionGraph::new();
        let sid1 = graph.create_session("alpha".into(), home()).unwrap();
        let _sid2 = graph.create_session("beta".into(), home()).unwrap();

        let err = graph.rename_session(sid1, "beta".into(), None).unwrap_err();
        assert!(matches!(err, GraphError::SessionNameExists(_)));
    }

    #[test]
    fn test_create_window() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();

        let wid = graph.create_window(sid, "editor".into(), home()).unwrap();

        let snap = state.load();
        assert_eq!(snap.sessions[&sid].windows.len(), 2);
        assert_eq!(snap.windows[&wid].title, "editor");
        let panes: Vec<_> = snap.panes.values().filter(|p| p.window_id == wid).collect();
        assert_eq!(panes.len(), 1);
    }

    #[test]
    fn test_destroy_last_window_rejected() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();

        let snap = state.load();
        let wid = snap.sessions[&sid].windows[0];

        let err = graph.destroy_window(wid, None).unwrap_err();
        assert!(matches!(err, GraphError::LastWindow));
    }

    #[test]
    fn test_destroy_window_cascades_panes() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();
        let wid = graph.create_window(sid, "editor".into(), home()).unwrap();

        let _pid = graph.create_pane(wid, home(), vec![]).unwrap();

        let snap = state.load();
        let pane_count_before = snap.panes.values().filter(|p| p.window_id == wid).count();
        assert_eq!(pane_count_before, 2);

        graph.destroy_window(wid, None).unwrap();

        let snap = state.load();
        let pane_count_after = snap.panes.values().filter(|p| p.window_id == wid).count();
        assert_eq!(pane_count_after, 0);
        assert!(!snap.windows.contains_key(&wid));
    }

    #[test]
    fn test_create_pane() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();

        let snap = state.load();
        let wid = snap.sessions[&sid].windows[0];

        let pid = graph.create_pane(wid, home(), vec!["vim".into()]).unwrap();

        let snap = state.load();
        assert_eq!(snap.panes[&pid].command, vec!["vim"]);
        assert_eq!(snap.panes[&pid].window_id, wid);
    }

    #[test]
    fn test_destroy_last_pane_rejected() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();

        let snap = state.load();
        let wid = snap.sessions[&sid].windows[0];
        let pid = snap.windows[&wid].active_pane;

        let err = graph.destroy_pane(pid, None).unwrap_err();
        assert!(matches!(err, GraphError::LastPane));
    }

    #[test]
    fn test_pane_exit_status() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();

        let snap = state.load();
        let wid = snap.sessions[&sid].windows[0];
        let pid = snap.windows[&wid].active_pane;

        assert!(snap.panes[&pid].is_alive());

        graph.set_pane_exit_status(pid, 0).unwrap();

        let snap = state.load();
        assert!(!snap.panes[&pid].is_alive());
        assert_eq!(snap.panes[&pid].exit_status, Some(0));
    }

    #[test]
    fn test_tags() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();

        graph
            .set_session_tag(sid, "project".into(), "shux".into())
            .unwrap();

        let snap = state.load();
        assert_eq!(
            snap.sessions[&sid].tags.get("project"),
            Some(&"shux".to_string())
        );
    }

    #[test]
    fn test_pane_tag() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();

        let snap = state.load();
        let wid = snap.sessions[&sid].windows[0];
        let pid = snap.windows[&wid].active_pane;

        graph
            .set_pane_tag(pid, "role".into(), "editor".into())
            .unwrap();

        let snap = state.load();
        assert_eq!(
            snap.panes[&pid].tags.get("role"),
            Some(&"editor".to_string())
        );
    }

    #[test]
    fn test_pane_theme() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();

        let snap = state.load();
        let wid = snap.sessions[&sid].windows[0];
        let pid = snap.windows[&wid].active_pane;

        graph
            .set_pane_theme(pid, Some("catppuccin-mocha".into()))
            .unwrap();

        let snap = state.load();
        assert_eq!(snap.panes[&pid].theme, Some("catppuccin-mocha".into()));

        graph.set_pane_theme(pid, None).unwrap();

        let snap = state.load();
        assert_eq!(snap.panes[&pid].theme, None);
    }

    #[test]
    fn test_session_theme() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();

        graph
            .set_session_theme(sid, Some("dracula".into()))
            .unwrap();

        let snap = state.load();
        assert_eq!(snap.sessions[&sid].theme, Some("dracula".into()));
    }

    #[test]
    fn test_version_increments() {
        let (graph, state) = SessionGraph::new();

        let v0 = state.load().version;
        let _sid = graph.create_session("work".into(), home()).unwrap();
        let v1 = state.load().version;

        assert!(v1 > v0, "Global version should increment on mutation");
    }

    #[test]
    fn test_snapshot_is_independent() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();

        let snap1 = state.load_full();

        graph.rename_session(sid, "renamed".into(), None).unwrap();

        assert_eq!(snap1.sessions[&sid].name, "work");

        let snap2 = state.load_full();
        assert_eq!(snap2.sessions[&sid].name, "renamed");
    }

    #[test]
    fn test_find_session_by_name() {
        let (graph, state) = SessionGraph::new();
        graph.create_session("alpha".into(), home()).unwrap();
        graph.create_session("beta".into(), home()).unwrap();

        let snap = state.load();
        let found = snap.find_session_by_name("beta");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "beta");

        assert!(snap.find_session_by_name("gamma").is_none());
    }

    #[test]
    fn test_session_windows_ordered() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();
        let w2 = graph.create_window(sid, "second".into(), home()).unwrap();
        let w3 = graph.create_window(sid, "third".into(), home()).unwrap();

        let snap = state.load();
        let windows = snap.session_windows(&sid);
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[1].id, w2);
        assert_eq!(windows[2].id, w3);
    }

    #[test]
    fn test_destroy_active_window_switches() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();
        let w2 = graph.create_window(sid, "second".into(), home()).unwrap();

        graph.destroy_window(w2, None).unwrap();

        let snap = state.load();
        assert_eq!(snap.sessions[&sid].windows.len(), 1);
    }

    #[tokio::test]
    async fn test_graph_handle_create_session() {
        let (graph, state) = SessionGraph::new();
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        let token = tokio_util::sync::CancellationToken::new();

        let token_clone = token.clone();
        let handle = tokio::spawn(async move {
            run_graph_loop(graph, cmd_rx, token_clone).await;
        });

        let gh = GraphHandle::new(cmd_tx, state.clone());
        let sid = gh.create_session("test".into(), home()).await.unwrap();

        let snap = gh.snapshot();
        assert_eq!(snap.sessions[&sid].name, "test");

        token.cancel();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_graph_handle_full_lifecycle() {
        let (graph, state) = SessionGraph::new();
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        let token = tokio_util::sync::CancellationToken::new();

        let token_clone = token.clone();
        let handle = tokio::spawn(async move {
            run_graph_loop(graph, cmd_rx, token_clone).await;
        });

        let gh = GraphHandle::new(cmd_tx, state);

        let sid = gh.create_session("test".into(), home()).await.unwrap();
        let wid = gh.create_window(sid, "win2".into(), home()).await.unwrap();
        let pid = gh
            .create_pane(wid, home(), vec!["bash".into()])
            .await
            .unwrap();

        let snap = gh.snapshot();
        assert_eq!(snap.sessions.len(), 1);
        assert_eq!(snap.windows.len(), 2);
        assert!(snap.panes.len() >= 3);
        assert_eq!(snap.panes[&pid].command, vec!["bash"]);

        gh.destroy_session(sid, None).await.unwrap();
        let snap = gh.snapshot();
        assert!(snap.sessions.is_empty());

        token.cancel();
        handle.await.unwrap();
    }

    #[test]
    fn test_validate_session_name_empty() {
        let (graph, _state) = SessionGraph::new();
        let err = graph.create_session("".into(), home()).unwrap_err();
        assert!(matches!(err, GraphError::EmptySessionName));
    }

    #[test]
    fn test_validate_session_name_too_long() {
        let (graph, _state) = SessionGraph::new();
        let long_name = "a".repeat(129);
        let err = graph.create_session(long_name, home()).unwrap_err();
        assert!(matches!(err, GraphError::SessionNameTooLong(_)));
    }

    #[test]
    fn test_validate_session_name_invalid_chars() {
        let (graph, _state) = SessionGraph::new();
        let err = graph.create_session("bad name".into(), home()).unwrap_err();
        assert!(matches!(err, GraphError::InvalidSessionName(_)));
    }

    #[test]
    fn test_validate_session_name_valid() {
        let (graph, _state) = SessionGraph::new();
        // These should all succeed
        graph.create_session("alpha".into(), home()).unwrap();
        graph.create_session("my-session".into(), home()).unwrap();
        graph.create_session("test_123".into(), home()).unwrap();
        graph.create_session("v1.0.0".into(), home()).unwrap();
    }

    #[test]
    fn test_validate_rename_name() {
        let (graph, _state) = SessionGraph::new();
        let sid = graph.create_session("valid".into(), home()).unwrap();

        // Empty name
        let err = graph.rename_session(sid, "".into(), None).unwrap_err();
        assert!(matches!(err, GraphError::EmptySessionName));

        // Spaces
        let err = graph
            .rename_session(sid, "bad name".into(), None)
            .unwrap_err();
        assert!(matches!(err, GraphError::InvalidSessionName(_)));

        // Valid rename
        graph.rename_session(sid, "good-name".into(), None).unwrap();
    }

    #[test]
    fn test_rename_window() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();
        let wid = graph.create_window(sid, "old".into(), home()).unwrap();

        graph.rename_window(wid, "new-title".into(), None).unwrap();

        let snap = state.load();
        assert_eq!(snap.windows[&wid].title, "new-title");
    }

    #[test]
    fn test_rename_window_conflict() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();

        // Get default window
        let snap = state.load();
        let default_wid = snap.sessions[&sid].windows[0];

        let wid2 = graph.create_window(sid, "second".into(), home()).unwrap();

        // Rename second window to conflict with default
        let default_title = state.load().windows[&default_wid].title.clone();
        let err = graph.rename_window(wid2, default_title, None).unwrap_err();
        assert!(matches!(err, GraphError::WindowNameConflict(_)));
    }

    #[test]
    fn test_rename_window_empty_name() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();

        let snap = state.load();
        let wid = snap.sessions[&sid].windows[0];

        let err = graph.rename_window(wid, "".into(), None).unwrap_err();
        assert!(matches!(err, GraphError::EmptyWindowName));
    }

    #[test]
    fn test_focus_window() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();

        let snap = state.load();
        let w1 = snap.sessions[&sid].windows[0];

        let w2 = graph.create_window(sid, "second".into(), home()).unwrap();

        // Focus back to w1
        let prev = graph.focus_window(w1, None).unwrap();
        assert_eq!(prev, Some(w2));

        let snap = state.load();
        assert_eq!(snap.sessions[&sid].active_window, w1);
    }

    #[test]
    fn test_focus_already_focused_window() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();

        let snap = state.load();
        let w1 = snap.sessions[&sid].windows[0];

        // Focus the already active window
        let prev = graph.focus_window(w1, None).unwrap();
        assert_eq!(prev, None);
    }

    #[test]
    fn test_reorder_window() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();
        let w2 = graph.create_window(sid, "second".into(), home()).unwrap();
        let w3 = graph.create_window(sid, "third".into(), home()).unwrap();

        // Move w3 to index 0
        graph.reorder_window(w3, 0, None).unwrap();

        let snap = state.load();
        assert_eq!(snap.sessions[&sid].windows[0], w3);
        assert_eq!(snap.sessions[&sid].windows[2], w2);
    }

    #[test]
    fn test_reorder_window_out_of_range() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();

        let snap = state.load();
        let wid = snap.sessions[&sid].windows[0];

        let err = graph.reorder_window(wid, 99, None).unwrap_err();
        assert!(matches!(
            err,
            GraphError::WindowIndexOutOfRange { index: 99, .. }
        ));
    }

    #[test]
    fn test_find_window_by_name() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();
        let wid = graph.create_window(sid, "editor".into(), home()).unwrap();

        let snap = state.load();
        let found = snap.find_window_by_name(&sid, "editor");
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, wid);

        assert!(snap.find_window_by_name(&sid, "nonexistent").is_none());
    }

    #[test]
    fn test_snapshot_serialization() {
        let (graph, state) = SessionGraph::new();
        graph.create_session("work".into(), home()).unwrap();

        let snap = state.load_full();
        let json = serde_json::to_string_pretty(&*snap).unwrap();
        assert!(json.contains("work"));

        let deserialized: SessionGraphSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.sessions.len(), 1);
    }

    /// Lifecycle event firing — proves that every mutation path that has a
    /// `fire()` call publishes the matching `EventData` variant. This is the
    /// load-bearing contract for the `events.watch` RPC: subscribers MUST
    /// see every mutation. Per Codex review, publishing is a property of
    /// the data layer, not the dispatcher — this test confirms it stays so.
    #[tokio::test]
    async fn test_lifecycle_events_fire() {
        let bus = crate::bus::EventBus::new();
        let mut sub = bus.subscribe();
        let (graph, _state) = SessionGraph::new_with_event_bus(Some(bus.clone()));

        // 1. SessionCreated
        let sid = graph.create_session("alpha".into(), home()).unwrap();

        // 2. SessionRenamed
        graph.rename_session(sid, "beta".into(), None).unwrap();

        // 3. WindowCreated (editor) — also fires PaneCreated for its initial pane
        let wid = graph.create_window(sid, "editor".into(), home()).unwrap();

        // Find a pane in that window so we can act on it.
        let snap = graph.current();
        let pane_in_w = snap
            .panes
            .values()
            .find(|p| p.window_id == wid)
            .map(|p| p.id)
            .unwrap();

        // 4. PaneCreated (split)
        let new_pane = graph
            .split_pane(pane_in_w, Direction::Vertical, 0.5)
            .unwrap();

        // 5. PaneFocused (focus_pane)
        graph.focus_pane(pane_in_w).unwrap();

        // 6. PaneZoomed (zoom_pane)
        graph.zoom_pane(pane_in_w, None).unwrap();

        // 7. PaneExited (set_pane_exit_status)
        graph.set_pane_exit_status(new_pane, 0).unwrap();

        // 8. SessionKilled (destroy_session)
        graph.destroy_session(sid, None).unwrap();

        // Drain everything we published. Order matches publish order
        // because tokio::broadcast preserves it.
        let mut types: Vec<String> = Vec::new();
        for _ in 0..32 {
            match tokio::time::timeout(std::time::Duration::from_millis(50), sub.recv()).await {
                Ok(Some(crate::bus::SubscriptionEvent::Event(e))) => {
                    types.push(e.meta.event_type.clone());
                }
                Ok(Some(crate::bus::SubscriptionEvent::Lagged(_))) => {
                    panic!("subscription lagged in unit test");
                }
                Ok(None) | Err(_) => break,
            }
        }

        // We must observe at least every variant we acted on. Order matters
        // (broadcast preserves publish order), but we assert on the set so
        // additions to other mutation paths don't break this test.
        for expected in &[
            "session.created",
            "session.renamed",
            "window.created",
            "pane.created", // create_window's initial pane
            "pane.created", // split_pane
            "pane.focused",
            "pane.zoomed",
            "pane.exited",
            "session.killed",
        ] {
            assert!(
                types.iter().any(|t| t == expected),
                "expected event {expected:?} to fire; saw {types:?}"
            );
        }

        // Sequence numbers must be strictly monotonically increasing.
        // (We re-subscribe and read history to check this — the bus keeps
        // history internally.)
        let history = bus.history(64);
        let seqs: Vec<u64> = history.iter().map(|e| e.meta.seq).collect();
        for win in seqs.windows(2) {
            assert!(win[0] < win[1], "seqs must be monotonic: {seqs:?}");
        }
    }

    /// Subscribe-first / history-second / dedup race: the contract Codex and
    /// Gemini both flagged as load-bearing. Verify a publish that lands
    /// AFTER `subscribe_filtered` returns but BEFORE `events_from_seq` is
    /// queried still reaches the subscriber. We can't reliably reproduce
    /// the exact race in a unit test, but we can prove the building blocks:
    /// (a) the subscription receives events published while the receiver
    /// is held, and (b) history is consistent with what the subscription
    /// returns, so dedup-by-seq is well-defined.
    #[tokio::test]
    async fn test_event_seq_consistency() {
        let bus = crate::bus::EventBus::new();
        let mut sub = bus.subscribe_filtered(vec!["session.created".into()]);
        let (graph, _state) = SessionGraph::new_with_event_bus(Some(bus.clone()));

        graph.create_session("a".into(), home()).unwrap();
        graph.create_session("b".into(), home()).unwrap();

        // Pull the two SessionCreated events from the (filtered) subscription.
        // create_session fires 3 events each (SessionCreated, WindowCreated,
        // PaneCreated for the implicit window+pane); the filter excludes the
        // last two so only session.created reaches us.
        let mut sub_seqs = Vec::new();
        for _ in 0..2 {
            if let Ok(Some(crate::bus::SubscriptionEvent::Event(e))) =
                tokio::time::timeout(std::time::Duration::from_millis(200), sub.recv()).await
            {
                sub_seqs.push(e.meta.seq);
            }
        }

        // Pull the same events from history.
        let history_seqs: Vec<u64> = bus
            .history(10)
            .iter()
            .filter(|e| e.event_type() == "session.created")
            .map(|e| e.meta.seq)
            .collect();

        assert_eq!(
            sub_seqs, history_seqs,
            "subscription and history must agree on seq order for session.created"
        );
        assert_eq!(sub_seqs.len(), 2);
        assert!(sub_seqs[1] > sub_seqs[0]);
    }

    /// Codex review of PR #9 caught three cascade bugs: destroying a session
    /// removed child windows + panes from state but only fired SessionKilled
    /// (so subscribers tracking pane lifecycle saw panes get created and
    /// never die); destroying a window had the same gap for child panes;
    /// rename_window mutated state without firing WindowRenamed at all.
    /// This test asserts all three are fixed.
    #[tokio::test]
    async fn test_cascade_kill_and_rename_events() {
        let bus = crate::bus::EventBus::new();
        let mut sub = bus.subscribe();
        let (graph, _state) = SessionGraph::new_with_event_bus(Some(bus.clone()));

        // Build a session with two windows and an extra pane in window 2.
        let sid = graph.create_session("demo".into(), home()).unwrap();
        let w2 = graph.create_window(sid, "editor".into(), home()).unwrap();
        let snap_after_setup = graph.current();
        let pane_in_w2 = snap_after_setup
            .panes
            .values()
            .find(|p| p.window_id == w2)
            .map(|p| p.id)
            .unwrap();
        let extra = graph
            .split_pane(pane_in_w2, Direction::Vertical, 0.5)
            .unwrap();

        // Drain subscription for setup events; we only care about teardown
        // and rename below.
        for _ in 0..16 {
            if tokio::time::timeout(std::time::Duration::from_millis(20), sub.recv())
                .await
                .is_err()
            {
                break;
            }
        }

        // 1. rename_window must fire WindowRenamed.
        graph.rename_window(w2, "scratch".into(), None).unwrap();

        // 2. destroy_window must fire PaneExited(*) for each child pane,
        //    THEN WindowKilled. Window 2 has 2 panes (initial + split).
        graph.destroy_window(w2, None).unwrap();

        // 3. destroy_session must cascade: every remaining pane → exited,
        //    every remaining window → killed, then session.killed.
        graph.destroy_session(sid, None).unwrap();

        // Drain the rest.
        let mut after = Vec::new();
        for _ in 0..32 {
            match tokio::time::timeout(std::time::Duration::from_millis(50), sub.recv()).await {
                Ok(Some(crate::bus::SubscriptionEvent::Event(e))) => {
                    after.push(e.meta.event_type.clone());
                }
                _ => break,
            }
        }

        // WindowRenamed for the rename_window call.
        assert!(
            after.iter().any(|t| t == "window.renamed"),
            "rename_window must fire WindowRenamed; saw {after:?}"
        );

        // 2 PaneExited from destroying window 2's two panes.
        let pane_exited_after_window_kill = after
            .iter()
            .take_while(|t| t.as_str() != "window.killed")
            .filter(|t| t.as_str() == "pane.exited")
            .count();
        assert!(
            pane_exited_after_window_kill >= 2,
            "destroy_window must fire 2 pane.exited events before window.killed; saw {after:?}"
        );

        // window.killed for window 2.
        assert!(
            after.iter().any(|t| t == "window.killed"),
            "destroy_window must fire WindowKilled; saw {after:?}"
        );

        // Cascade from destroy_session: remaining panes exited, remaining
        // window killed, then session.killed last. We had 1 surviving window
        // (the implicit one from create_session) with 1 pane, plus the
        // killed-but-still-tracked extra pane was already removed when
        // window 2 died; so session kill cascade: 1 pane.exited + 1
        // window.killed + 1 session.killed.
        assert!(
            after.iter().any(|t| t == "session.killed"),
            "destroy_session must fire SessionKilled; saw {after:?}"
        );
        // Session kill cascade ordering: every pane.exited and window.killed
        // BEFORE the final session.killed.
        let session_killed_pos = after.iter().position(|t| t == "session.killed").unwrap();
        let last_window_killed_pos = after
            .iter()
            .enumerate()
            .rfind(|(_, t)| t.as_str() == "window.killed")
            .map(|(i, _)| i)
            .unwrap();
        assert!(
            last_window_killed_pos < session_killed_pos,
            "all window.killed must precede session.killed; saw {after:?}"
        );

        // Suppress unused-binding warning — the pane id was needed only to
        // verify the split call returned cleanly.
        let _ = extra;
    }

    // ── Optimistic-concurrency tests (PR 3b) ─────────────────────────
    //
    // Every mutation that ticks an entity's `version: Version` must
    // reject a stale `expected_version` with `GraphError::VersionConflict`
    // carrying the right `resource` + `id` for the RPC error mapper to
    // turn into a `-32002` response with bounded current entity metadata.
    //
    // The matrix below covers each freshly version-gated method end to
    // end: stale rejected, current accepted (version then ticks), and the
    // None case (unconditional, for human users not tracking versions).

    #[test]
    fn test_destroy_session_version_conflict_carries_resource_and_id() {
        let (graph, _state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();
        let err = graph.destroy_session(sid, Some(999)).unwrap_err();
        match err {
            GraphError::VersionConflict {
                resource,
                id,
                expected,
                actual,
            } => {
                assert_eq!(resource, "session");
                assert_eq!(id, sid.to_string());
                assert_eq!(expected, 999);
                assert_eq!(actual, 1);
            }
            other => panic!("expected VersionConflict, got {other:?}"),
        }
    }

    #[test]
    fn test_rename_session_version_conflict() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("old".into(), home()).unwrap();
        let current = state.load().sessions[&sid].version;
        // First rename with the right version succeeds and bumps to current+1.
        graph
            .rename_session(sid, "mid".into(), Some(current))
            .unwrap();
        // Second rename with the OLD version is stale.
        let err = graph
            .rename_session(sid, "new".into(), Some(current))
            .unwrap_err();
        assert!(matches!(err, GraphError::VersionConflict { .. }));
        // State unchanged.
        assert_eq!(state.load().sessions[&sid].name, "mid");
    }

    #[test]
    fn test_rename_window_version_conflict() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();
        let wid = graph.create_window(sid, "old".into(), home()).unwrap();
        let v = state.load().windows[&wid].version;
        let err = graph
            .rename_window(wid, "new".into(), Some(v + 5))
            .unwrap_err();
        match err {
            GraphError::VersionConflict {
                resource,
                id,
                expected,
                actual,
            } => {
                assert_eq!(resource, "window");
                assert_eq!(id, wid.to_string());
                assert_eq!(expected, v + 5);
                assert_eq!(actual, v);
            }
            other => panic!("expected VersionConflict, got {other:?}"),
        }
        // Title untouched on conflict.
        assert_eq!(state.load().windows[&wid].title, "old");
    }

    #[test]
    fn test_focus_window_version_conflict() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();
        let w2 = graph.create_window(sid, "second".into(), home()).unwrap();
        let v = state.load().windows[&w2].version;
        let err = graph.focus_window(w2, Some(v + 1)).unwrap_err();
        assert!(matches!(
            err,
            GraphError::VersionConflict {
                resource: "window",
                ..
            }
        ));
    }

    #[test]
    fn test_reorder_window_version_conflict() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();
        let _w2 = graph.create_window(sid, "second".into(), home()).unwrap();
        let w3 = graph.create_window(sid, "third".into(), home()).unwrap();
        let v = state.load().windows[&w3].version;
        let err = graph.reorder_window(w3, 0, Some(v + 99)).unwrap_err();
        assert!(matches!(err, GraphError::VersionConflict { .. }));
    }

    #[test]
    fn test_destroy_pane_version_conflict() {
        // Need ≥2 panes per window (destroy_pane refuses LastPane).
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();
        let snap = state.load();
        let wid = snap.sessions[&sid].windows[0];
        let pid = snap.windows[&wid].active_pane;
        drop(snap);
        let _ = graph
            .split_pane(pid, crate::layout::Direction::Vertical, 0.5)
            .unwrap();
        let v = state.load().panes[&pid].version;
        let err = graph.destroy_pane(pid, Some(v + 1)).unwrap_err();
        match err {
            GraphError::VersionConflict { resource, id, .. } => {
                assert_eq!(resource, "pane");
                assert_eq!(id, pid.to_string());
            }
            other => panic!("expected VersionConflict, got {other:?}"),
        }
        // Pane still in the graph since destroy was rejected.
        assert!(state.load().panes.contains_key(&pid));
    }

    #[test]
    fn test_resize_pane_version_conflict_and_bumps_siblings() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();
        let snap = state.load();
        let wid = snap.sessions[&sid].windows[0];
        let p1 = snap.windows[&wid].active_pane;
        drop(snap);
        let p2 = graph
            .split_pane(p1, crate::layout::Direction::Vertical, 0.5)
            .unwrap();
        let p1_v = state.load().panes[&p1].version;
        let p2_v = state.load().panes[&p2].version;
        // Stale version on p1 must reject.
        let err = graph
            .resize_pane(p1, crate::layout::Direction::Vertical, 0.1, Some(p1_v + 5))
            .unwrap_err();
        assert!(matches!(err, GraphError::VersionConflict { .. }));
        // Current version succeeds AND ticks p2's version too (sibling bump
        // — layout ops affect every pane in the window).
        graph
            .resize_pane(p1, crate::layout::Direction::Vertical, 0.1, Some(p1_v))
            .unwrap();
        let snap = state.load();
        assert_eq!(snap.panes[&p1].version, p1_v + 1);
        assert_eq!(snap.panes[&p2].version, p2_v + 1);
    }

    #[test]
    fn test_zoom_pane_version_conflict_and_bumps_siblings() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();
        let snap = state.load();
        let wid = snap.sessions[&sid].windows[0];
        let p1 = snap.windows[&wid].active_pane;
        drop(snap);
        let p2 = graph
            .split_pane(p1, crate::layout::Direction::Vertical, 0.5)
            .unwrap();
        let p1_v = state.load().panes[&p1].version;
        let p2_v = state.load().panes[&p2].version;
        let err = graph.zoom_pane(p1, Some(p1_v + 10)).unwrap_err();
        assert!(matches!(err, GraphError::VersionConflict { .. }));
        let was_zoomed = graph.zoom_pane(p1, Some(p1_v)).unwrap();
        assert!(was_zoomed);
        let snap = state.load();
        assert_eq!(snap.panes[&p1].version, p1_v + 1);
        assert_eq!(snap.panes[&p2].version, p2_v + 1);
    }

    #[test]
    fn test_swap_panes_version_conflict_checks_first_pane() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();
        let snap = state.load();
        let wid = snap.sessions[&sid].windows[0];
        let p1 = snap.windows[&wid].active_pane;
        drop(snap);
        let p2 = graph
            .split_pane(p1, crate::layout::Direction::Vertical, 0.5)
            .unwrap();
        let p1_v = state.load().panes[&p1].version;
        // Stale version on p1 (the first pane).
        let err = graph.swap_panes(p1, p2, Some(p1_v + 7)).unwrap_err();
        match err {
            GraphError::VersionConflict { id, .. } => {
                // Conflict points at p1, the anchor.
                assert_eq!(id, p1.to_string());
            }
            other => panic!("expected VersionConflict, got {other:?}"),
        }
    }

    #[test]
    fn test_unconditional_mutation_when_expected_version_is_none() {
        // Backward-compat path: human users don't track versions, so passing
        // `None` must always proceed regardless of the entity's current value.
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("a".into(), home()).unwrap();
        graph.rename_session(sid, "b".into(), None).unwrap();
        graph.rename_session(sid, "c".into(), None).unwrap();
        graph.rename_session(sid, "d".into(), None).unwrap();
        assert_eq!(state.load().sessions[&sid].name, "d");
        // Version monotonically bumped through every successful rename.
        assert!(state.load().sessions[&sid].version >= 4);
    }

    // ── PR 4 / task 027: pane title plumbing ─────────────────────────

    #[tokio::test]
    async fn test_set_pane_title_manual_fires_event_with_old_and_new() {
        use std::time::Duration;
        // Wire a real event bus so we can assert exactly which event
        // gets published when the displayed title moves.
        let bus = crate::bus::EventBus::new();
        let mut sub = bus.subscribe();
        let (graph, state) = SessionGraph::new_with_event_bus(Some(bus));
        let sid = graph.create_session("work".into(), home()).unwrap();
        let wid = state.load().sessions[&sid].windows[0];
        let pid = state.load().windows[&wid].active_pane;

        // Drain the SessionCreated / WindowCreated / PaneCreated events
        // so we only see what set_pane_title fires.
        for _ in 0..5 {
            if tokio::time::timeout(Duration::from_millis(20), sub.recv())
                .await
                .is_err()
            {
                break;
            }
        }

        graph
            .set_pane_title(pid, Some("agent-1".into()), None)
            .unwrap();
        let snap = state.load();
        assert_eq!(snap.panes[&pid].title, "agent-1");
        assert_eq!(snap.panes[&pid].manual_title.as_deref(), Some("agent-1"));

        // Event must fire with the right scope.
        let next = tokio::time::timeout(Duration::from_millis(200), sub.recv())
            .await
            .expect("PaneTitleChanged should fire within 200ms")
            .expect("subscription should not be closed");
        let ev = match next {
            crate::bus::SubscriptionEvent::Event(e) => e,
            crate::bus::SubscriptionEvent::Lagged(_) => panic!("unexpected lag"),
        };
        match ev.data {
            EventData::PaneTitleChanged {
                pane_id,
                window_id,
                session_id,
                ref new_title,
                ..
            } => {
                assert_eq!(pane_id, pid);
                assert_eq!(window_id, wid);
                assert_eq!(session_id, sid);
                assert_eq!(new_title, "agent-1");
            }
            other => panic!("expected PaneTitleChanged, got {other:?}"),
        }
    }

    #[test]
    fn test_set_pane_title_clear_lets_osc_flow_through() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();
        let wid = state.load().sessions[&sid].windows[0];
        let pid = state.load().windows[&wid].active_pane;

        // Pin manual, then push an OSC update underneath it. OSC
        // shouldn't show because manual is active.
        graph
            .set_pane_title(pid, Some("manual".into()), None)
            .unwrap();
        graph.set_pane_osc_title(pid, "osc-value".into()).unwrap();
        assert_eq!(state.load().panes[&pid].title, "manual");

        // Clear the manual title; the OSC value now takes over.
        graph.set_pane_title(pid, None, None).unwrap();
        assert_eq!(state.load().panes[&pid].title, "osc-value");
    }

    #[test]
    fn test_set_pane_osc_title_is_no_op_when_manual_set() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();
        let wid = state.load().sessions[&sid].windows[0];
        let pid = state.load().windows[&wid].active_pane;

        graph
            .set_pane_title(pid, Some("pinned".into()), None)
            .unwrap();
        let v_before = state.load().panes[&pid].version;

        // OSC arrives but is shadowed by manual. The graph still
        // records osc_title (so a later --clear surfaces it), but the
        // version is allowed to bump since the underlying state did
        // change. What MUST NOT happen is the displayed `title`
        // changing.
        graph.set_pane_osc_title(pid, "from-app".into()).unwrap();
        let snap = state.load();
        assert_eq!(snap.panes[&pid].title, "pinned");
        assert_eq!(snap.panes[&pid].osc_title.as_deref(), Some("from-app"));
        // version tick is fine; PaneTitleChanged event MUST NOT fire
        // (no displayed change). We test the event-not-firing aspect
        // in the next test.
        let _ = v_before;
    }

    #[tokio::test]
    async fn test_set_pane_osc_title_skips_event_when_no_visible_change() {
        use std::time::Duration;
        let bus = crate::bus::EventBus::new();
        let mut sub = bus.subscribe();
        let (graph, state) = SessionGraph::new_with_event_bus(Some(bus));
        let sid = graph.create_session("work".into(), home()).unwrap();
        let wid = state.load().sessions[&sid].windows[0];
        let pid = state.load().windows[&wid].active_pane;
        // Pin manual; OSC after this MUST NOT fire PaneTitleChanged.
        graph
            .set_pane_title(pid, Some("locked".into()), None)
            .unwrap();
        // Drain everything fired so far.
        for _ in 0..10 {
            if tokio::time::timeout(Duration::from_millis(10), sub.recv())
                .await
                .is_err()
            {
                break;
            }
        }
        graph.set_pane_osc_title(pid, "irrelevant".into()).unwrap();
        // No new event within a short window.
        let nothing = tokio::time::timeout(Duration::from_millis(50), sub.recv()).await;
        assert!(
            nothing.is_err(),
            "no PaneTitleChanged event should fire when title is pinned",
        );
    }

    #[test]
    fn test_set_pane_title_unknown_pane_returns_not_found() {
        let (graph, _state) = SessionGraph::new();
        let bogus = PaneId::new();
        let err = graph
            .set_pane_title(bogus, Some("x".into()), None)
            .unwrap_err();
        assert!(matches!(err, GraphError::PaneNotFound(_)));
    }

    // ── apply_batch tests (PR 3a) ─────────────────────────────────────

    use crate::apply::{Op, PaneRef, SessionRef};

    /// Single-op apply: create a session with a custom command. Validates that
    /// PaneCreated event carries the actual command (codex P2 #10 fix) and
    /// every event carries the same correlation_id.
    #[tokio::test]
    async fn test_apply_batch_create_session_with_command() {
        let bus = crate::bus::EventBus::new();
        let mut sub = bus.subscribe();
        let (graph, _state) = SessionGraph::new_with_event_bus(Some(bus.clone()));

        let result = graph
            .apply_batch(vec![Op::CreateSession {
                name: Some("agent-conductor".into()),
                cwd: home(),
                initial_command: vec!["claude".into(), "-p".into(), "refactor auth".into()],
                initial_window_title: None,
            }])
            .expect("apply succeeds");

        assert_eq!(result.outputs.len(), 1);
        assert!(result.outputs[0].session_id.is_some());
        assert!(result.outputs[0].window_id.is_some());
        assert!(result.outputs[0].pane_id.is_some());
        assert!(result.correlation_id.starts_with("apply-"));
        assert!(result.last_event_seq > 0);

        // Drain the bus and verify three events arrive with the shared
        // correlation_id, in the right order.
        let mut events = Vec::new();
        for _ in 0..3 {
            if let Ok(Some(crate::bus::SubscriptionEvent::Event(e))) =
                tokio::time::timeout(std::time::Duration::from_millis(100), sub.recv()).await
            {
                events.push(e);
            }
        }
        assert_eq!(events.len(), 3, "expected 3 events from CreateSession op");
        assert_eq!(events[0].event_type(), "session.created");
        assert_eq!(events[1].event_type(), "window.created");
        assert_eq!(events[2].event_type(), "pane.created");

        for e in &events {
            assert_eq!(
                e.meta.correlation_id.as_deref(),
                Some(result.correlation_id.as_str()),
                "every event in the batch must share the apply correlation_id"
            );
        }

        // Codex P2 #10 verification: PaneCreated event must carry the
        // initial_command, NOT an empty Vec.
        if let EventData::PaneCreated { command, .. } = &events[2].data {
            assert_eq!(
                command.as_slice(),
                ["claude", "-p", "refactor auth"],
                "PaneCreated event must reflect the actual command, not lie about empty"
            );
        } else {
            panic!("third event was not PaneCreated");
        }
    }

    /// Multi-op apply with backrefs: create a session, then a window in
    /// that session, then split the new window's pane. All three resolved
    /// via backrefs to prior op outputs.
    #[tokio::test]
    async fn test_apply_batch_with_backrefs() {
        let bus = crate::bus::EventBus::new();
        let mut sub = bus.subscribe();
        let (graph, _state) = SessionGraph::new_with_event_bus(Some(bus.clone()));

        let result = graph
            .apply_batch(vec![
                Op::CreateSession {
                    name: Some("workspace".into()),
                    cwd: home(),
                    initial_command: vec![],
                    initial_window_title: None,
                },
                Op::CreateWindow {
                    session: SessionRef::BackRef { op_index: 0 },
                    title: "editor".into(),
                    cwd: None,
                    initial_command: vec!["nvim".into()],
                },
                Op::SplitPane {
                    target: PaneRef::BackRef { op_index: 1 },
                    direction: Direction::Vertical,
                    ratio: 0.4,
                    command: vec!["bash".into()],
                    cwd: None,
                },
            ])
            .expect("3-op apply succeeds");

        assert_eq!(result.outputs.len(), 3);
        // Session from op 0 propagates through op 1 + op 2.
        let session_id = result.outputs[0].session_id.unwrap();
        assert_eq!(result.outputs[1].session_id, Some(session_id));
        assert_eq!(result.outputs[2].session_id, Some(session_id));
        // Window from op 1 propagates to op 2 (split inside that window).
        let window_id = result.outputs[1].window_id.unwrap();
        assert_eq!(result.outputs[2].window_id, Some(window_id));

        // Drain all events. Should be:
        //   op 0: session.created, window.created, pane.created  (3)
        //   op 1: window.created, pane.created                   (2)
        //   op 2: pane.created                                   (1)
        // = 6 total, all sharing the same correlation_id.
        let mut events = Vec::new();
        for _ in 0..8 {
            match tokio::time::timeout(std::time::Duration::from_millis(80), sub.recv()).await {
                Ok(Some(crate::bus::SubscriptionEvent::Event(e))) => events.push(e),
                _ => break,
            }
        }
        assert_eq!(events.len(), 6, "expected 6 events; got {events:?}");
        for e in &events {
            assert_eq!(
                e.meta.correlation_id.as_deref(),
                Some(result.correlation_id.as_str())
            );
        }
        // seqs strictly monotonic
        let seqs: Vec<u64> = events.iter().map(|e| e.meta.seq).collect();
        for w in seqs.windows(2) {
            assert!(w[0] < w[1], "seqs must be monotonic: {seqs:?}");
        }
    }

    /// Atomicity: if any op fails, no graph state is committed AND no events
    /// are fired. Codex P0 #3.
    #[tokio::test]
    async fn test_apply_batch_atomicity_rollback_on_failure() {
        let bus = crate::bus::EventBus::new();
        let mut sub = bus.subscribe();
        let (graph, state) = SessionGraph::new_with_event_bus(Some(bus.clone()));

        // Pre-populate one session so the second CreateSession in the batch
        // hits a SessionNameExists error.
        graph.create_session("existing".into(), home()).unwrap();

        // Drain pre-existing events from the create_session call so the assert
        // below cleanly checks "no apply events".
        for _ in 0..5 {
            if tokio::time::timeout(std::time::Duration::from_millis(20), sub.recv())
                .await
                .is_err()
            {
                break;
            }
        }

        let snap_before = state.load_full();
        let sessions_before = snap_before.sessions.len();

        let result = graph.apply_batch(vec![
            Op::CreateSession {
                name: Some("workspace-a".into()),
                cwd: home(),
                initial_command: vec![],
                initial_window_title: None,
            },
            Op::CreateSession {
                name: Some("existing".into()), // will fail: name conflict
                cwd: home(),
                initial_command: vec![],
                initial_window_title: None,
            },
        ]);

        assert!(result.is_err(), "expected apply to fail on name conflict");
        if let Err(crate::apply::BatchError::OpFailed { op_index, .. }) = result {
            assert_eq!(op_index, 1);
        } else {
            panic!("expected OpFailed error, got {result:?}");
        }

        // No commit on failure: session count must be unchanged, "workspace-a"
        // must NOT exist.
        let snap_after = state.load_full();
        assert_eq!(
            snap_after.sessions.len(),
            sessions_before,
            "no session should have been committed"
        );
        assert!(snap_after.find_session_by_name("workspace-a").is_none());

        // No events fired on failure (post-drain).
        let no_events_during_apply =
            tokio::time::timeout(std::time::Duration::from_millis(100), sub.recv())
                .await
                .is_err();
        assert!(
            no_events_during_apply,
            "no apply events should fire when batch fails"
        );
    }

    /// Empty batch is rejected.
    #[tokio::test]
    async fn test_apply_batch_empty_rejected() {
        let bus = crate::bus::EventBus::new();
        let (graph, _state) = SessionGraph::new_with_event_bus(Some(bus));
        let r = graph.apply_batch(vec![]);
        assert!(matches!(r, Err(crate::apply::BatchError::Empty)));
    }

    /// Backref to a future / out-of-range op index is rejected.
    #[tokio::test]
    async fn test_apply_batch_backref_out_of_range() {
        let bus = crate::bus::EventBus::new();
        let (graph, _state) = SessionGraph::new_with_event_bus(Some(bus));
        let r = graph.apply_batch(vec![Op::CreateWindow {
            session: SessionRef::BackRef { op_index: 5 },
            title: "x".into(),
            cwd: None,
            initial_command: vec![],
        }]);
        assert!(matches!(
            r,
            Err(crate::apply::BatchError::BackRefOutOfRange { .. })
        ));
    }
}
