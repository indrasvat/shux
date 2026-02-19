# 002 — Core Data Model and Session Graph

**Status:** Pending
**Depends On:** 000
**Parallelizable With:** 001, 005, 006

---

## Problem

Every subsystem in shux -- layout engine, PTY manager, API server, plugin host, renderer -- needs to query and mutate the session/window/pane hierarchy. Without a well-defined, thread-safe, lock-free data model, these subsystems will end up with ad-hoc state management, race conditions, and inconsistent views. This task establishes the authoritative `SessionGraph` that owns all state, using the single-writer/many-readers pattern (PRD 4.3, 4.6) with `ArcSwap` for lock-free reads and an `mpsc` channel for serialized mutations. Every entity carries a version stamp for optimistic concurrency, tags for plugin metadata, and UUIDs for stable identity.

## PRD Reference

- **5.1** Entities — Session, Window, Pane definitions with all fields
- **5.2** Layout tree — LayoutNode enum (delegated to task 003, but the Pane entity references it)
- **5.4** Snapshots & diffs — `state.snapshot`, optimistic concurrency, version stamps
- **4.3** Architectural invariants — single source of truth, CLI == API, single writer / many readers
- **4.4** Key abstractions — SessionId/WindowId/PaneId as stable UUIDs, SessionGraph with ArcSwap

---

## Files to Create

- `crates/shux-core/src/model.rs` — Entity structs: Session, Window, Pane, and their ID types. RestartPolicy, ThemeRef, and other value types.
- `crates/shux-core/src/graph.rs` — SessionGraph: the authoritative state container with ArcSwap snapshots, single-writer mutation channel, CRUD operations, and snapshot serialization.

## Files to Modify

- `crates/shux-core/src/lib.rs` — Add `pub mod model;` and `pub mod graph;`
- `crates/shux-core/Cargo.toml` — Add dependencies: `arc-swap`, `serde`, `serde_json`, `uuid`

---

## Execution Steps

### Step 1: Define ID types in `crates/shux-core/src/model.rs`

All entity IDs are UUIDs (v4) wrapped in newtypes for type safety. They must be `Copy`, `Hash`, `Eq`, `Serialize`, `Deserialize`, and `Display`.

```rust
use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A newtype wrapper around UUID for type-safe entity identification.
/// All shux entity IDs follow this pattern.
macro_rules! define_id {
    ($name:ident, $display_prefix:expr) => {
        #[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Serialize, Deserialize)]
        pub struct $name(pub Uuid);

        impl $name {
            /// Create a new random ID.
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Create from an existing UUID (for deserialization/testing).
            pub fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            /// Get the inner UUID.
            pub fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl From<Uuid> for $name {
            fn from(uuid: Uuid) -> Self {
                Self(uuid)
            }
        }
    };
}

define_id!(SessionId, "session");
define_id!(WindowId, "window");
define_id!(PaneId, "pane");
```

### Step 2: Define value types

```rust
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

/// Restart policy for a pane's child process (PRD 5.1, 6.2).
///
/// - `Never`: process exits and pane shows exit status. Default.
/// - `OnFail`: restart only if exit code != 0.
/// - `Always`: restart unconditionally (useful for long-running services).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    Never,
    OnFail,
    Always,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self::Never
    }
}

/// A reference to a named theme. Themes are resolved by the ThemeEngine
/// at render time via the cascade (PRD 5.3):
///
/// Built-in Default -> User Global -> Session -> Window -> Pane -> Runtime Override
pub type ThemeRef = String;

/// Tags are arbitrary key-value metadata visible to plugins (PRD 5.1).
/// Plugins use tags to annotate entities without modifying the core schema.
pub type Tags = HashMap<String, String>;

/// Monotonically increasing version stamp for optimistic concurrency (PRD 5.1, 5.4).
/// Every mutation to an entity increments its version. Clients include the expected
/// version in mutation requests; stale versions are rejected.
pub type Version = u64;
```

### Step 3: Define the Session entity

```rust
/// A session groups windows and represents a named workspace (PRD 5.1).
///
/// Sessions are the top-level entity. Users attach to sessions.
/// Multiple clients can attach to the same session simultaneously.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Unique, stable identifier (UUID v4).
    pub id: SessionId,

    /// Human-readable name. Must be unique across all sessions.
    /// Used in CLI: `shux attach -s work`.
    pub name: String,

    /// When this session was created.
    pub created_at: SystemTime,

    /// Ordered list of window IDs in this session.
    /// The order determines window index numbering (1-based in UI).
    pub windows: Vec<WindowId>,

    /// The currently active (focused) window in this session.
    pub active_window: WindowId,

    /// Environment variables inherited by new panes in this session.
    pub env: HashMap<String, String>,

    /// Optional theme override for this session (PRD 5.3 cascade).
    pub theme: Option<ThemeRef>,

    /// Plugin-visible metadata (PRD 5.1).
    pub tags: Tags,

    /// Optimistic concurrency version stamp.
    pub version: Version,
}

impl Session {
    /// Create a new session with the given name and an initial window.
    pub fn new(name: impl Into<String>, initial_window_id: WindowId) -> Self {
        Self {
            id: SessionId::new(),
            name: name.into(),
            created_at: SystemTime::now(),
            windows: vec![initial_window_id],
            active_window: initial_window_id,
            env: HashMap::new(),
            theme: None,
            tags: HashMap::new(),
            version: 1,
        }
    }
}
```

### Step 4: Define the Window entity

```rust
/// A window contains a layout tree of panes (PRD 5.1).
///
/// Each window has exactly one layout tree (a binary split tree defined
/// in task 003). The layout determines how panes are arranged spatially.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Window {
    /// Unique, stable identifier (UUID v4).
    pub id: WindowId,

    /// The session this window belongs to.
    pub session_id: SessionId,

    /// Human-readable title. Defaults to index or CWD.
    pub title: String,

    /// The currently focused pane in this window.
    pub active_pane: PaneId,

    /// Working directory for this window (used as default for new panes).
    pub cwd: Option<PathBuf>,

    /// Optional theme override (PRD 5.3 cascade).
    pub theme: Option<ThemeRef>,

    /// Plugin-visible metadata.
    pub tags: Tags,

    /// Optimistic concurrency version stamp.
    pub version: Version,

    // NOTE: The layout tree (LayoutNode) is NOT stored here directly.
    // It lives in the SessionGraph alongside the Window, indexed by WindowId.
    // This avoids circular dependencies between model.rs and layout.rs.
}

impl Window {
    /// Create a new window with the given title and initial pane.
    pub fn new(
        session_id: SessionId,
        title: impl Into<String>,
        initial_pane_id: PaneId,
    ) -> Self {
        Self {
            id: WindowId::new(),
            session_id,
            title: title.into(),
            active_pane: initial_pane_id,
            cwd: None,
            theme: None,
            tags: HashMap::new(),
            version: 1,
        }
    }
}
```

### Step 5: Define the Pane entity

```rust
/// A pane is a terminal viewport running a child process (PRD 5.1).
///
/// Each pane owns a PTY handle (managed by the PTY manager, task 004)
/// and a virtual terminal grid (task 005). The Pane struct in the data
/// model stores the configuration and metadata; the PTY handle and VT
/// grid are stored in their respective subsystems, indexed by PaneId.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pane {
    /// Unique, stable identifier (UUID v4).
    pub id: PaneId,

    /// The window this pane belongs to.
    pub window_id: WindowId,

    /// Human-readable title. Can be set manually or auto-derived.
    pub title: String,

    /// Whether the title is automatically derived from the running
    /// command / CWD. When true, the title updates as the foreground
    /// process changes.
    pub auto_title: bool,

    /// Current working directory of the child process.
    pub cwd: PathBuf,

    /// The command and arguments used to spawn the child process.
    /// Empty means the user's default shell.
    pub command: Vec<String>,

    /// Exit status of the child process, if it has exited.
    /// `None` means the process is still running.
    pub exit_status: Option<i32>,

    /// What to do when the child process exits (PRD 5.1, 6.2).
    pub restart: RestartPolicy,

    /// Optional theme override (PRD 5.3 cascade — per-pane theming
    /// is a key differentiator for shux).
    pub theme: Option<ThemeRef>,

    /// Plugin-visible metadata.
    pub tags: Tags,

    /// Optimistic concurrency version stamp.
    pub version: Version,
}

impl Pane {
    /// Create a new pane with default settings.
    pub fn new(window_id: WindowId, cwd: impl Into<PathBuf>) -> Self {
        Self {
            id: PaneId::new(),
            window_id,
            title: String::new(),
            auto_title: true,
            cwd: cwd.into(),
            command: Vec::new(),
            exit_status: None,
            restart: RestartPolicy::default(),
            theme: None,
            tags: HashMap::new(),
            version: 1,
        }
    }

    /// Create a pane with a specific command.
    pub fn with_command(
        window_id: WindowId,
        cwd: impl Into<PathBuf>,
        command: Vec<String>,
    ) -> Self {
        let mut pane = Self::new(window_id, cwd);
        pane.command = command;
        pane
    }

    /// Whether the child process is still running.
    pub fn is_alive(&self) -> bool {
        self.exit_status.is_none()
    }

    /// Whether this pane should be restarted based on its policy and exit status.
    pub fn should_restart(&self) -> bool {
        match (self.restart, self.exit_status) {
            (RestartPolicy::Always, Some(_)) => true,
            (RestartPolicy::OnFail, Some(code)) => code != 0,
            _ => false,
        }
    }
}
```

### Step 6: Define the SessionGraph snapshot in `crates/shux-core/src/graph.rs`

The `SessionGraphSnapshot` is the immutable, cheaply cloneable state that readers access via `ArcSwap`. It contains all sessions, windows, and panes indexed by their IDs.

```rust
use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

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

    #[error("version conflict: expected {expected}, found {actual}")]
    VersionConflict { expected: Version, actual: Version },

    #[error("cannot remove last window from session")]
    LastWindow,

    #[error("cannot remove last pane from window")]
    LastPane,
}

/// The immutable snapshot of all session state.
///
/// This is what readers see via `ArcSwap::load()`. It is cheaply cloneable
/// (Arc around the inner data) and can be held across await points without
/// blocking writers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionGraphSnapshot {
    /// All sessions, indexed by SessionId.
    pub sessions: HashMap<SessionId, Session>,

    /// All windows, indexed by WindowId.
    pub windows: HashMap<WindowId, Window>,

    /// All panes, indexed by PaneId.
    pub panes: HashMap<PaneId, Pane>,

    /// Global version counter. Incremented on every mutation.
    pub version: Version,
}

impl SessionGraphSnapshot {
    /// Find a session by name.
    pub fn find_session_by_name(&self, name: &str) -> Option<&Session> {
        self.sessions.values().find(|s| s.name == name)
    }

    /// Get all windows belonging to a session, in order.
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

    /// Get all panes belonging to a window.
    pub fn window_panes(&self, window_id: &WindowId) -> Vec<&Pane> {
        self.panes
            .values()
            .filter(|p| p.window_id == *window_id)
            .collect()
    }

    /// Check if a session name is already taken.
    pub fn session_name_exists(&self, name: &str) -> bool {
        self.sessions.values().any(|s| s.name == name)
    }
}
```

### Step 7: Define mutation commands for the graph

```rust
/// Commands sent to the single-writer graph task via mpsc.
///
/// Each command represents an atomic mutation. Results are sent back
/// via a oneshot channel embedded in the command.
#[derive(Debug)]
pub enum GraphCommand {
    /// Create a new session with a default window and pane.
    CreateSession {
        name: String,
        cwd: std::path::PathBuf,
        reply: tokio::sync::oneshot::Sender<Result<SessionId, GraphError>>,
    },

    /// Destroy a session and all its windows/panes.
    DestroySession {
        id: SessionId,
        expected_version: Option<Version>,
        reply: tokio::sync::oneshot::Sender<Result<(), GraphError>>,
    },

    /// Rename a session.
    RenameSession {
        id: SessionId,
        new_name: String,
        expected_version: Option<Version>,
        reply: tokio::sync::oneshot::Sender<Result<(), GraphError>>,
    },

    /// Create a new window in a session with a default pane.
    CreateWindow {
        session_id: SessionId,
        title: String,
        cwd: std::path::PathBuf,
        reply: tokio::sync::oneshot::Sender<Result<WindowId, GraphError>>,
    },

    /// Destroy a window and all its panes.
    DestroyWindow {
        id: WindowId,
        expected_version: Option<Version>,
        reply: tokio::sync::oneshot::Sender<Result<(), GraphError>>,
    },

    /// Create a new pane in a window.
    CreatePane {
        window_id: WindowId,
        cwd: std::path::PathBuf,
        command: Vec<String>,
        reply: tokio::sync::oneshot::Sender<Result<PaneId, GraphError>>,
    },

    /// Destroy a pane.
    DestroyPane {
        id: PaneId,
        reply: tokio::sync::oneshot::Sender<Result<(), GraphError>>,
    },

    /// Update a pane's exit status.
    SetPaneExitStatus {
        id: PaneId,
        exit_status: i32,
        reply: tokio::sync::oneshot::Sender<Result<(), GraphError>>,
    },

    /// Set a tag on a session.
    SetSessionTag {
        id: SessionId,
        key: String,
        value: String,
        reply: tokio::sync::oneshot::Sender<Result<(), GraphError>>,
    },

    /// Set a tag on a pane.
    SetPaneTag {
        id: PaneId,
        key: String,
        value: String,
        reply: tokio::sync::oneshot::Sender<Result<(), GraphError>>,
    },

    /// Set a theme on a session.
    SetSessionTheme {
        id: SessionId,
        theme: Option<ThemeRef>,
        reply: tokio::sync::oneshot::Sender<Result<(), GraphError>>,
    },

    /// Set a theme on a pane (per-pane theming — key differentiator).
    SetPaneTheme {
        id: PaneId,
        theme: Option<ThemeRef>,
        reply: tokio::sync::oneshot::Sender<Result<(), GraphError>>,
    },

    /// Get a consistent snapshot (for API responses).
    Snapshot {
        reply: tokio::sync::oneshot::Sender<Arc<SessionGraphSnapshot>>,
    },
}
```

### Step 8: Implement the SessionGraph owner

The `SessionGraph` is the single-writer owner of state. It holds the mutable snapshot and publishes updates via `ArcSwap` for lock-free reads.

```rust
/// The authoritative session graph.
///
/// This struct is owned by a single task (the graph owner). It processes
/// `GraphCommand`s sequentially, mutates the snapshot, and publishes
/// updates via `ArcSwap` for lock-free reads by any number of readers.
pub struct SessionGraph {
    /// The current state, wrapped in ArcSwap for lock-free reads.
    /// Writers clone the Arc, mutate, and store back.
    state: Arc<ArcSwap<SessionGraphSnapshot>>,
}

impl SessionGraph {
    /// Create a new empty graph with an ArcSwap for sharing.
    pub fn new() -> (Self, Arc<ArcSwap<SessionGraphSnapshot>>) {
        let snapshot = Arc::new(SessionGraphSnapshot::default());
        let state = Arc::new(ArcSwap::from(snapshot));
        let graph = Self {
            state: Arc::clone(&state),
        };
        (graph, state)
    }

    /// Get the current snapshot (for the writer task).
    fn current(&self) -> Arc<SessionGraphSnapshot> {
        self.state.load_full()
    }

    /// Publish an updated snapshot.
    fn publish(&self, snapshot: SessionGraphSnapshot) {
        self.state.store(Arc::new(snapshot));
    }

    /// Create a new session with a default window and pane.
    pub fn create_session(
        &self,
        name: String,
        cwd: std::path::PathBuf,
    ) -> Result<SessionId, GraphError> {
        let current = self.current();

        if current.session_name_exists(&name) {
            return Err(GraphError::SessionNameExists(name));
        }

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        // Create pane -> window -> session (bottom-up)
        let pane = Pane::new(WindowId::new(), cwd); // temporary window_id, fixed below
        let pane_id = pane.id;

        let mut window = Window::new(SessionId::new(), "1", pane_id); // temp session_id
        let window_id = window.id;

        // Fix up the pane's window_id
        let mut pane = pane;
        pane.window_id = window_id;

        let session = Session::new(name, window_id);
        let session_id = session.id;

        // Fix up the window's session_id
        window.session_id = session_id;

        snapshot.panes.insert(pane_id, pane);
        snapshot.windows.insert(window_id, window);
        snapshot.sessions.insert(session_id, session);

        self.publish(snapshot);
        info!(%session_id, "Session created");
        Ok(session_id)
    }

    /// Destroy a session and all its windows and panes.
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

        // Collect window IDs to remove
        let window_ids: Vec<WindowId> = session.windows.clone();

        // Remove all panes belonging to these windows
        let pane_ids_to_remove: Vec<PaneId> = snapshot
            .panes
            .values()
            .filter(|p| window_ids.contains(&p.window_id))
            .map(|p| p.id)
            .collect();

        for pid in &pane_ids_to_remove {
            snapshot.panes.remove(pid);
        }

        // Remove windows
        for wid in &window_ids {
            snapshot.windows.remove(wid);
        }

        // Remove session
        snapshot.sessions.remove(&id);

        self.publish(snapshot);
        info!(%id, panes_removed = pane_ids_to_remove.len(), "Session destroyed");
        Ok(())
    }

    /// Rename a session.
    pub fn rename_session(
        &self,
        id: SessionId,
        new_name: String,
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

    /// Create a new window in a session with a default pane.
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

        let pane = Pane::new(WindowId::new(), cwd);
        let pane_id = pane.id;

        let mut window = Window::new(session_id, title, pane_id);
        let window_id = window.id;

        // Fix pane's window_id
        let mut pane = pane;
        pane.window_id = window_id;

        snapshot.panes.insert(pane_id, pane);
        snapshot.windows.insert(window_id, window);

        if let Some(s) = snapshot.sessions.get_mut(&session_id) {
            s.windows.push(window_id);
            s.version += 1;
        }

        self.publish(snapshot);
        info!(%window_id, %session_id, "Window created");
        Ok(window_id)
    }

    /// Destroy a window and all its panes.
    /// Fails if it's the last window in the session (use destroy_session instead).
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

        // Check it's not the last window
        let session = current
            .sessions
            .get(&window.session_id)
            .ok_or(GraphError::SessionNotFound(window.session_id))?;

        if session.windows.len() <= 1 {
            return Err(GraphError::LastWindow);
        }

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        // Remove panes
        let pane_ids: Vec<PaneId> = snapshot
            .panes
            .values()
            .filter(|p| p.window_id == id)
            .map(|p| p.id)
            .collect();

        for pid in &pane_ids {
            snapshot.panes.remove(pid);
        }

        // Remove window from session's list
        if let Some(s) = snapshot.sessions.get_mut(&window.session_id) {
            s.windows.retain(|wid| *wid != id);
            // If we removed the active window, activate the first remaining one
            if s.active_window == id {
                s.active_window = s.windows[0];
            }
            s.version += 1;
        }

        snapshot.windows.remove(&id);

        self.publish(snapshot);
        Ok(())
    }

    /// Create a new pane in a window.
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

    /// Destroy a pane. Returns an error if it's the last pane in the window.
    pub fn destroy_pane(&self, id: PaneId) -> Result<(), GraphError> {
        let current = self.current();

        let pane = current
            .panes
            .get(&id)
            .ok_or(GraphError::PaneNotFound(id))?;

        let window_panes: Vec<&Pane> = current
            .panes
            .values()
            .filter(|p| p.window_id == pane.window_id)
            .collect();

        if window_panes.len() <= 1 {
            return Err(GraphError::LastPane);
        }

        let window_id = pane.window_id;

        let mut snapshot = (*current).clone();
        snapshot.version += 1;

        snapshot.panes.remove(&id);

        // If this was the active pane, pick another one
        if let Some(w) = snapshot.windows.get_mut(&window_id) {
            if w.active_pane == id {
                // Pick the first remaining pane
                if let Some(remaining) = snapshot
                    .panes
                    .values()
                    .find(|p| p.window_id == window_id)
                {
                    w.active_pane = remaining.id;
                }
            }
            w.version += 1;
        }

        self.publish(snapshot);
        Ok(())
    }

    /// Set the exit status on a pane.
    pub fn set_pane_exit_status(
        &self,
        id: PaneId,
        exit_status: i32,
    ) -> Result<(), GraphError> {
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

    /// Set a tag on a session.
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

    /// Set a tag on a pane.
    pub fn set_pane_tag(
        &self,
        id: PaneId,
        key: String,
        value: String,
    ) -> Result<(), GraphError> {
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

    /// Set a theme on a pane (per-pane theming).
    pub fn set_pane_theme(
        &self,
        id: PaneId,
        theme: Option<ThemeRef>,
    ) -> Result<(), GraphError> {
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
}

impl Default for SessionGraph {
    fn default() -> Self {
        Self::new().0
    }
}
```

### Step 9: Implement the graph command loop

This is the single-writer task that processes mutation commands from the mpsc channel and delegates to `SessionGraph` methods.

```rust
/// Run the graph command processing loop.
///
/// This is spawned as a single tokio task. All mutations are serialized
/// through the `cmd_rx` channel, ensuring the single-writer invariant.
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
                        let result = graph.create_session(name, cwd);
                        let _ = reply.send(result);
                    }
                    Some(GraphCommand::DestroySession { id, expected_version, reply }) => {
                        let result = graph.destroy_session(id, expected_version);
                        let _ = reply.send(result);
                    }
                    Some(GraphCommand::RenameSession { id, new_name, expected_version, reply }) => {
                        let result = graph.rename_session(id, new_name, expected_version);
                        let _ = reply.send(result);
                    }
                    Some(GraphCommand::CreateWindow { session_id, title, cwd, reply }) => {
                        let result = graph.create_window(session_id, title, cwd);
                        let _ = reply.send(result);
                    }
                    Some(GraphCommand::DestroyWindow { id, expected_version, reply }) => {
                        let result = graph.destroy_window(id, expected_version);
                        let _ = reply.send(result);
                    }
                    Some(GraphCommand::CreatePane { window_id, cwd, command, reply }) => {
                        let result = graph.create_pane(window_id, cwd, command);
                        let _ = reply.send(result);
                    }
                    Some(GraphCommand::DestroyPane { id, reply }) => {
                        let result = graph.destroy_pane(id);
                        let _ = reply.send(result);
                    }
                    Some(GraphCommand::SetPaneExitStatus { id, exit_status, reply }) => {
                        let result = graph.set_pane_exit_status(id, exit_status);
                        let _ = reply.send(result);
                    }
                    Some(GraphCommand::SetSessionTag { id, key, value, reply }) => {
                        let result = graph.set_session_tag(id, key, value);
                        let _ = reply.send(result);
                    }
                    Some(GraphCommand::SetPaneTag { id, key, value, reply }) => {
                        let result = graph.set_pane_tag(id, key, value);
                        let _ = reply.send(result);
                    }
                    Some(GraphCommand::SetSessionTheme { id, theme, reply }) => {
                        // Inline: similar pattern to set_pane_theme
                        let current = graph.current();
                        let result = if !current.sessions.contains_key(&id) {
                            Err(GraphError::SessionNotFound(id))
                        } else {
                            let mut snapshot = (*current).clone();
                            snapshot.version += 1;
                            if let Some(s) = snapshot.sessions.get_mut(&id) {
                                s.theme = theme;
                                s.version += 1;
                            }
                            graph.publish(snapshot);
                            Ok(())
                        };
                        let _ = reply.send(result);
                    }
                    Some(GraphCommand::SetPaneTheme { id, theme, reply }) => {
                        let result = graph.set_pane_theme(id, theme);
                        let _ = reply.send(result);
                    }
                    Some(GraphCommand::Snapshot { reply }) => {
                        let snapshot = graph.current();
                        let _ = reply.send(snapshot);
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
```

### Step 10: Implement GraphHandle for convenient async access

Provide a type-safe handle that other subsystems use to send commands to the graph.

```rust
/// A cloneable handle for sending commands to the SessionGraph.
///
/// Every subsystem that needs to mutate state gets a `GraphHandle`.
/// Reads go directly through the `ArcSwap` (no channel needed).
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

    /// Create a session (async, goes through mutation channel).
    pub async fn create_session(
        &self,
        name: String,
        cwd: std::path::PathBuf,
    ) -> Result<SessionId, GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::CreateSession { name, cwd, reply: tx })
            .await
            .map_err(|_| GraphError::SessionNotFound(SessionId::new()))?;
        rx.await.map_err(|_| GraphError::SessionNotFound(SessionId::new()))?
    }

    /// Destroy a session.
    pub async fn destroy_session(
        &self,
        id: SessionId,
        expected_version: Option<Version>,
    ) -> Result<(), GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::DestroySession { id, expected_version, reply: tx })
            .await
            .map_err(|_| GraphError::SessionNotFound(id))?;
        rx.await.map_err(|_| GraphError::SessionNotFound(id))?
    }

    /// Create a pane.
    pub async fn create_pane(
        &self,
        window_id: WindowId,
        cwd: std::path::PathBuf,
        command: Vec<String>,
    ) -> Result<PaneId, GraphError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(GraphCommand::CreatePane { window_id, cwd, command, reply: tx })
            .await
            .map_err(|_| GraphError::WindowNotFound(window_id))?;
        rx.await.map_err(|_| GraphError::WindowNotFound(window_id))?
    }

    // Additional convenience methods follow the same pattern:
    // build command with oneshot reply, send through cmd_tx, await rx.
}
```

### Step 11: Write comprehensive unit tests

Add a `#[cfg(test)]` module in `crates/shux-core/src/graph.rs`:

```rust
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
        // New window creates one pane
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

        // Add an extra pane to the new window
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
    fn test_pane_should_restart() {
        let mut pane = Pane::new(WindowId::new(), home());

        // Never restart (default)
        pane.exit_status = Some(1);
        assert!(!pane.should_restart());

        // OnFail with failure
        pane.restart = RestartPolicy::OnFail;
        pane.exit_status = Some(1);
        assert!(pane.should_restart());

        // OnFail with success
        pane.exit_status = Some(0);
        assert!(!pane.should_restart());

        // Always
        pane.restart = RestartPolicy::Always;
        pane.exit_status = Some(0);
        assert!(pane.should_restart());

        // Still running — no restart needed
        pane.exit_status = None;
        assert!(!pane.should_restart());
    }

    #[test]
    fn test_tags() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();

        graph.set_session_tag(sid, "project".into(), "shux".into()).unwrap();

        let snap = state.load();
        assert_eq!(
            snap.sessions[&sid].tags.get("project"),
            Some(&"shux".to_string())
        );
    }

    #[test]
    fn test_pane_theme() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();

        let snap = state.load();
        let wid = snap.sessions[&sid].windows[0];
        let pid = snap.windows[&wid].active_pane;

        graph.set_pane_theme(pid, Some("catppuccin-mocha".into())).unwrap();

        let snap = state.load();
        assert_eq!(
            snap.panes[&pid].theme,
            Some("catppuccin-mocha".into())
        );

        // Clear theme
        graph.set_pane_theme(pid, None).unwrap();

        let snap = state.load();
        assert_eq!(snap.panes[&pid].theme, None);
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

        // Take a snapshot
        let snap1 = state.load_full();

        // Mutate
        graph.rename_session(sid, "renamed".into(), None).unwrap();

        // Original snapshot is unchanged
        assert_eq!(snap1.sessions[&sid].name, "work");

        // New snapshot reflects the change
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
    fn test_destroy_active_window_switches_to_first() {
        let (graph, state) = SessionGraph::new();
        let sid = graph.create_session("work".into(), home()).unwrap();
        let w2 = graph.create_window(sid, "second".into(), home()).unwrap();

        // Make w2 the active window (can't yet through the current API,
        // so we just verify that destroying w2 keeps the session valid)
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

    #[test]
    fn test_snapshot_serialization() {
        let (graph, state) = SessionGraph::new();
        graph.create_session("work".into(), home()).unwrap();

        let snap = state.load_full();
        let json = serde_json::to_string_pretty(&*snap).unwrap();
        assert!(json.contains("work"));

        // Round-trip
        let deserialized: SessionGraphSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.sessions.len(), 1);
    }
}
```

Also add a `#[cfg(test)]` module in `crates/shux-core/src/model.rs` for the ID types and entity tests.

---

## Verification

### Functional

```bash
# Build
cargo build --workspace

# Verify the types are public and accessible
cargo doc --workspace --no-deps --document-private-items

# Check serialization round-trip
cargo nextest run -p shux-core graph::tests::test_snapshot_serialization
```

### Tests

```bash
# Run all graph tests
cargo nextest run -p shux-core graph::tests

# Run all model tests
cargo nextest run -p shux-core model::tests

# Run everything
cargo nextest run --workspace

# Clippy
cargo clippy --workspace --all-targets -- -D warnings
```

---

## Completion Criteria

- [ ] `crates/shux-core/src/model.rs` exists with `SessionId`, `WindowId`, `PaneId`, `Session`, `Window`, `Pane`, `RestartPolicy`, `ThemeRef`, `Tags`, `Version`
- [ ] All IDs are UUID v4 newtypes with `Copy`, `Hash`, `Eq`, `Serialize`, `Deserialize`, `Display`
- [ ] `Session` has: id, name, created_at, windows (ordered Vec), active_window, env, theme, tags, version
- [ ] `Window` has: id, session_id, title, active_pane, cwd, theme, tags, version
- [ ] `Pane` has: id, window_id, title, auto_title, cwd, command, exit_status, restart, theme, tags, version
- [ ] `RestartPolicy` enum: Never, OnFail, Always with `should_restart()` method on Pane
- [ ] `crates/shux-core/src/graph.rs` exists with `SessionGraph`, `SessionGraphSnapshot`, `GraphCommand`, `GraphHandle`
- [ ] `SessionGraphSnapshot` is wrapped in `ArcSwap` for lock-free reads
- [ ] Mutations go through `mpsc` channel to single-writer task (`run_graph_loop`)
- [ ] `GraphHandle` provides async convenience methods for common operations
- [ ] CRUD operations: create/destroy/rename session, create/destroy window, create/destroy pane
- [ ] Version stamps incremented on every mutation (both entity-level and global)
- [ ] Optimistic concurrency: mutations with expected_version fail on mismatch
- [ ] Tags: get/set on sessions and panes
- [ ] Theme: set/clear on sessions and panes
- [ ] Snapshot serializes to JSON via serde
- [ ] Cannot destroy last window in session (returns `GraphError::LastWindow`)
- [ ] Cannot destroy last pane in window (returns `GraphError::LastPane`)
- [ ] All unit tests pass (`cargo nextest run -p shux-core`)
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes

---

## Commit Message

```
feat: add core data model with SessionGraph and lock-free snapshots

- Session/Window/Pane entities with UUID v4 IDs and version stamps
- SessionGraph with ArcSwap for lock-free reads, mpsc for serialized writes
- GraphHandle for async CRUD: create/destroy/rename session, window, pane
- Optimistic concurrency via version stamps on all entities
- Tags (HashMap<String, String>) and ThemeRef on all entities
- RestartPolicy enum (Never, OnFail, Always) with should_restart()
- Snapshot serialization via serde for API state.snapshot support
- Comprehensive unit tests for CRUD, version conflicts, cascade deletes
```

---

## Session Protocol

1. **Before starting:** Read `CLAUDE.md`, `docs/PRD.md` sections 4.3, 4.4, 5.1, 5.2, 5.4. Verify task 000 is complete (workspace builds). Understand the ArcSwap pattern -- readers call `state.load()` for cheap access, writers clone-mutate-store.
2. **During:** Implement model.rs first (Steps 1-5), then graph.rs (Steps 6-10), then tests (Step 11). Run `cargo check` after each step. The graph.rs is the largest file -- build it incrementally, testing each CRUD method as you add it.
3. **Key design decisions:**
   - Layout trees are NOT stored inside Window. They will be stored alongside windows in the SessionGraph (task 003 adds `layouts: HashMap<WindowId, LayoutNode>`). This avoids circular dependencies.
   - PTY handles are NOT stored in Pane. The PTY manager (task 004) maintains its own `HashMap<PaneId, PtyHandle>`. The Pane struct is purely data/metadata.
   - The `GraphHandle` send channel errors are mapped to domain errors for ergonomics. In practice, a send failure means the graph loop crashed, which is a fatal condition.
4. **After:** Run full verification suite. Update `docs/PROGRESS.md` (mark 002 as done, add session log entry). Update `CLAUDE.md` Learnings.
5. **Watch out for:**
   - `ArcSwap::load()` returns a `Guard` that derefs to `Arc`. Use `load_full()` when you need to store the `Arc` across await points.
   - `SystemTime` serialization via serde requires the `serde` feature or a custom serializer. Consider using `u64` (Unix timestamp) instead if serde_json serialization fails.
   - The clone-mutate-store pattern on `SessionGraphSnapshot` means every mutation clones the entire state. This is fine for shux's scale (tens of sessions, hundreds of panes) but would not scale to millions of entities. Document this tradeoff.
