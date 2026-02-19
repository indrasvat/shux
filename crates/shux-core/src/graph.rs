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

    #[error("version conflict: expected {expected}, found {actual}")]
    VersionConflict { expected: Version, actual: Version },

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
        reply: tokio::sync::oneshot::Sender<Result<(), GraphError>>,
    },
    FocusWindow {
        id: WindowId,
        reply: tokio::sync::oneshot::Sender<Result<Option<WindowId>, GraphError>>,
    },
    ReorderWindow {
        id: WindowId,
        new_index: usize,
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
        reply: tokio::sync::oneshot::Sender<Result<(), GraphError>>,
    },
    ZoomPane {
        id: PaneId,
        reply: tokio::sync::oneshot::Sender<Result<bool, GraphError>>,
    },
    SwapPanes {
        a: PaneId,
        b: PaneId,
        reply: tokio::sync::oneshot::Sender<Result<(), GraphError>>,
    },
    Snapshot {
        reply: tokio::sync::oneshot::Sender<Arc<SessionGraphSnapshot>>,
    },
}

/// The authoritative session graph (single-writer owner of state).
pub struct SessionGraph {
    state: Arc<ArcSwap<SessionGraphSnapshot>>,
}

impl SessionGraph {
    pub fn new() -> (Self, Arc<ArcSwap<SessionGraphSnapshot>>) {
        let snapshot = Arc::new(SessionGraphSnapshot::default());
        let state = Arc::new(ArcSwap::from(snapshot));
        let graph = Self {
            state: Arc::clone(&state),
        };
        (graph, state)
    }

    fn current(&self) -> Arc<SessionGraphSnapshot> {
        self.state.load_full()
    }

    fn publish(&self, snapshot: SessionGraphSnapshot) {
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

    pub fn create_session(
        &self,
        name: String,
        cwd: std::path::PathBuf,
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

        let mut pane = Pane::new(window_id, cwd);
        pane.id = pane_id;

        let mut window = Window::new(SessionId::new(), "1", pane_id);
        window.id = window_id;

        let session = Session::new(name, window_id);
        let session_id = session.id;

        window.session_id = session_id;

        snapshot.panes.insert(pane_id, pane);
        snapshot.windows.insert(window_id, window);
        snapshot.sessions.insert(session_id, session);

        self.publish(snapshot);
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
                    expected: ev,
                    actual: session.version,
                });
            }
        }

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        let window_ids: Vec<WindowId> = session.windows.clone();

        let pane_ids_to_remove: Vec<PaneId> = snapshot
            .panes
            .values()
            .filter(|p| window_ids.contains(&p.window_id))
            .map(|p| p.id)
            .collect();

        for pid in &pane_ids_to_remove {
            snapshot.panes.remove(pid);
        }
        for wid in &window_ids {
            snapshot.windows.remove(wid);
        }
        snapshot.sessions.remove(&id);

        self.publish(snapshot);
        info!(%id, panes_removed = pane_ids_to_remove.len(), "Session destroyed");
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
                    expected: ev,
                    actual: session.version,
                });
            }
        }

        if current.session_name_exists(&new_name) {
            return Err(GraphError::SessionNameExists(new_name));
        }

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        if let Some(s) = snapshot.sessions.get_mut(&id) {
            s.name = new_name;
            s.version += 1;
        }

        self.publish(snapshot);
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

        let mut window = Window::new(session_id, title, pane_id);
        window.id = window_id;

        snapshot.panes.insert(pane_id, pane);
        snapshot.windows.insert(window_id, window);

        if let Some(s) = snapshot.sessions.get_mut(&session_id) {
            s.windows.push(window_id);
            s.active_window = window_id;
            s.version += 1;
        }

        self.publish(snapshot);
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

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        let pane_ids: Vec<PaneId> = snapshot
            .panes
            .values()
            .filter(|p| p.window_id == id)
            .map(|p| p.id)
            .collect();

        for pid in &pane_ids {
            snapshot.panes.remove(pid);
        }

        if let Some(s) = snapshot.sessions.get_mut(&window.session_id) {
            s.windows.retain(|wid| *wid != id);
            if s.active_window == id {
                // Safe: we verified len > 1 above, so at least one window remains
                s.active_window = s.windows[0];
            }
            s.version += 1;
        }

        snapshot.windows.remove(&id);

        self.publish(snapshot);
        Ok(())
    }

    pub fn rename_window(&self, id: WindowId, new_title: String) -> Result<(), GraphError> {
        if new_title.is_empty() {
            return Err(GraphError::EmptyWindowName);
        }

        let current = self.current();

        let window = current
            .windows
            .get(&id)
            .ok_or(GraphError::WindowNotFound(id))?;
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

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        if let Some(w) = snapshot.windows.get_mut(&id) {
            w.title = new_title;
            w.version += 1;
        }

        self.publish(snapshot);
        Ok(())
    }

    /// Focus (activate) a window. Returns the previously active window ID.
    pub fn focus_window(&self, id: WindowId) -> Result<Option<WindowId>, GraphError> {
        let current = self.current();

        let window = current
            .windows
            .get(&id)
            .ok_or(GraphError::WindowNotFound(id))?;
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

        self.publish(snapshot);
        info!(%id, %session_id, "Window focused");
        Ok(previous)
    }

    /// Reorder a window to a new index position within its session.
    pub fn reorder_window(&self, id: WindowId, new_index: usize) -> Result<(), GraphError> {
        let current = self.current();

        let window = current
            .windows
            .get(&id)
            .ok_or(GraphError::WindowNotFound(id))?;
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

        self.publish(snapshot);
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

        let pane = if command.is_empty() {
            Pane::new(window_id, cwd)
        } else {
            Pane::with_command(window_id, cwd, command)
        };
        let pane_id = pane.id;

        snapshot.panes.insert(pane_id, pane);

        self.publish(snapshot);
        info!(%pane_id, %window_id, "Pane created");
        Ok(pane_id)
    }

    pub fn destroy_pane(&self, id: PaneId) -> Result<(), GraphError> {
        let current = self.current();

        let pane = current.panes.get(&id).ok_or(GraphError::PaneNotFound(id))?;

        let window_pane_count = current
            .panes
            .values()
            .filter(|p| p.window_id == pane.window_id)
            .count();

        if window_pane_count <= 1 {
            return Err(GraphError::LastPane);
        }

        let window_id = pane.window_id;

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

        self.publish(snapshot);
        Ok(())
    }

    pub fn set_pane_exit_status(&self, id: PaneId, exit_status: i32) -> Result<(), GraphError> {
        let current = self.current();

        if !current.panes.contains_key(&id) {
            return Err(GraphError::PaneNotFound(id));
        }

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        if let Some(p) = snapshot.panes.get_mut(&id) {
            p.exit_status = Some(exit_status);
            p.version += 1;
        }

        self.publish(snapshot);
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

        self.publish(snapshot);
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

        self.publish(snapshot);
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

        self.publish(snapshot);
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

        self.publish(snapshot);
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

        self.publish(snapshot);
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

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        if let Some(w) = snapshot.windows.get_mut(&window_id) {
            w.active_pane = id;
            w.version += 1;
        }

        self.publish(snapshot);
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

                self.publish(snapshot);
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
    ) -> Result<(), GraphError> {
        let (current, window_id) = self.find_pane_window(id)?;

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
            self.publish(snapshot);
            Ok(())
        } else {
            // No matching split direction — not an error, just a no-op
            Ok(())
        }
    }

    /// Toggle zoom on a pane. Returns whether the pane is now zoomed.
    pub fn zoom_pane(&self, id: PaneId) -> Result<bool, GraphError> {
        let (current, window_id) = self.find_pane_window(id)?;

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        let window = snapshot
            .windows
            .get_mut(&window_id)
            .ok_or(GraphError::WindowNotFound(window_id))?;

        window.layout.toggle_zoom(id);
        let is_zoomed = window.layout.is_zoomed();
        window.version += 1;

        self.publish(snapshot);
        Ok(is_zoomed)
    }

    /// Swap two panes in the layout tree. Both must be in the same window.
    pub fn swap_panes(&self, a: PaneId, b: PaneId) -> Result<(), GraphError> {
        if a == b {
            return Err(GraphError::PaneSwapSelf);
        }

        let current = self.current();

        let pane_a = current.panes.get(&a).ok_or(GraphError::PaneNotFound(a))?;
        let pane_b = current.panes.get(&b).ok_or(GraphError::PaneNotFound(b))?;

        if pane_a.window_id != pane_b.window_id {
            return Err(GraphError::PaneCrossWindow);
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
            self.publish(snapshot);
            Ok(())
        } else {
            Err(GraphError::LayoutError("swap_panes failed".into()))
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
                    Some(GraphCommand::CreateSession { name, cwd, reply }) => {
                        let _ = reply.send(graph.create_session(name, cwd));
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
                    Some(GraphCommand::RenameWindow { id, new_title, reply }) => {
                        let _ = reply.send(graph.rename_window(id, new_title));
                    }
                    Some(GraphCommand::FocusWindow { id, reply }) => {
                        let _ = reply.send(graph.focus_window(id));
                    }
                    Some(GraphCommand::ReorderWindow { id, new_index, reply }) => {
                        let _ = reply.send(graph.reorder_window(id, new_index));
                    }
                    Some(GraphCommand::CreatePane { window_id, cwd, command, reply }) => {
                        let _ = reply.send(graph.create_pane(window_id, cwd, command));
                    }
                    Some(GraphCommand::DestroyPane { id, reply }) => {
                        let _ = reply.send(graph.destroy_pane(id));
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
                    Some(GraphCommand::SplitPane { target_pane, direction, ratio, reply }) => {
                        let _ = reply.send(graph.split_pane(target_pane, direction, ratio));
                    }
                    Some(GraphCommand::FocusPane { id, reply }) => {
                        let _ = reply.send(graph.focus_pane(id));
                    }
                    Some(GraphCommand::FocusPaneDirection { window_id, direction, viewport, reply }) => {
                        let _ = reply.send(graph.focus_pane_direction(window_id, direction, viewport));
                    }
                    Some(GraphCommand::ResizePane { id, direction, delta, reply }) => {
                        let _ = reply.send(graph.resize_pane(id, direction, delta));
                    }
                    Some(GraphCommand::ZoomPane { id, reply }) => {
                        let _ = reply.send(graph.zoom_pane(id));
                    }
                    Some(GraphCommand::SwapPanes { a, b, reply }) => {
                        let _ = reply.send(graph.swap_panes(a, b));
                    }
                    Some(GraphCommand::Snapshot { reply }) => {
                        let _ = reply.send(graph.current());
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

    pub async fn create_session(
        &self,
        name: String,
        cwd: std::path::PathBuf,
    ) -> Result<SessionId, GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::CreateSession {
                name,
                cwd,
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

    pub async fn rename_window(&self, id: WindowId, new_title: String) -> Result<(), GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::RenameWindow {
                id,
                new_title,
                reply: tx,
            })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
    }

    pub async fn focus_window(&self, id: WindowId) -> Result<Option<WindowId>, GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::FocusWindow { id, reply: tx })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
    }

    pub async fn reorder_window(&self, id: WindowId, new_index: usize) -> Result<(), GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::ReorderWindow {
                id,
                new_index,
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

    pub async fn destroy_pane(&self, id: PaneId) -> Result<(), GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::DestroyPane { id, reply: tx })
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
    ) -> Result<(), GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::ResizePane {
                id,
                direction,
                delta,
                reply: tx,
            })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
    }

    pub async fn zoom_pane(&self, id: PaneId) -> Result<bool, GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::ZoomPane { id, reply: tx })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
    }

    pub async fn swap_panes(&self, a: PaneId, b: PaneId) -> Result<(), GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::SwapPanes { a, b, reply: tx })
            .await
            .map_err(|_| GraphError::Shutdown)?;
        rx.await.map_err(|_| GraphError::Shutdown)?
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

        let err = graph.destroy_pane(pid).unwrap_err();
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

        graph.rename_window(wid, "new-title".into()).unwrap();

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
        let err = graph.rename_window(wid2, default_title).unwrap_err();
        assert!(matches!(err, GraphError::WindowNameConflict(_)));
    }

    #[test]
    fn test_rename_window_empty_name() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();

        let snap = state.load();
        let wid = snap.sessions[&sid].windows[0];

        let err = graph.rename_window(wid, "".into()).unwrap_err();
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
        let prev = graph.focus_window(w1).unwrap();
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
        let prev = graph.focus_window(w1).unwrap();
        assert_eq!(prev, None);
    }

    #[test]
    fn test_reorder_window() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();
        let w2 = graph.create_window(sid, "second".into(), home()).unwrap();
        let w3 = graph.create_window(sid, "third".into(), home()).unwrap();

        // Move w3 to index 0
        graph.reorder_window(w3, 0).unwrap();

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

        let err = graph.reorder_window(wid, 99).unwrap_err();
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
}
