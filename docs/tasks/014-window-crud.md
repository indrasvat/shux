# 014 — Window CRUD (API + CLI)

**Status:** Pending
**Depends On:** 013
**Parallelizable With:** 022

---

## Problem

With session CRUD in place (task 013), the next layer of the hierarchy needs implementation: windows. Each session contains one or more windows, and each window contains a LayoutTree with at least one pane. Users and agents need to create, list, rename, focus, reorder, and kill windows to organize their work.

Windows are the primary unit of tab-like navigation in shux. Users switch between windows using `Alt+n`/`Alt+p` (next/previous) or `Alt+1..9` (by index). Agents use `window.create` and `window.focus` to set up workspaces programmatically. The `window.ensure` operation provides the same idempotent guarantee as `session.ensure` for agent workflows.

Window reordering is important for muscle memory: a user who knows "window 1 is editor, window 2 is server" needs to keep that mapping stable even as windows are created and destroyed.

## PRD Reference

- **PRD section 6.1 (Windows)**: Create, list, rename, kill, reorder, switch by index, switch by fuzzy search, MRU navigation.
- **PRD section 8.2 (window.* methods)**: window.list, window.create, window.ensure, window.rename, window.focus, window.reorder, window.kill
- **PRD section 5.1 (Window entity)**: WindowId (UUID), session (SessionId), title (String), layout (LayoutNode tree), active_pane (PaneId), cwd, theme, tags, version
- **PRD section 5.2 (Layout tree)**: Each window has a LayoutTree; a new window starts with `LayoutNode::Leaf { pane: PaneId }` (single pane)
- **PRD section 9.1 (Tier 1 keybindings)**: Alt+n = next window, Alt+p = previous window, Alt+1..9 = switch by index

---

## Files to Create

- `crates/shux-rpc/src/methods/window.rs` — JSON-RPC handlers for all window.* methods
- `crates/shux/src/commands/window.rs` — CLI window subcommands
- `crates/shux-core/src/window.rs` — Window mutation operations on SessionGraph
- `crates/shux-rpc/tests/window_api.rs` — L3 API contract tests
- `.claude/automations/test_014_window_crud.py` — L4 iterm2-driver visual tests (25 tests, 21 screenshots)

## Files to Modify

- `crates/shux-core/src/graph.rs` — Add window mutation methods to SessionGraph
- `crates/shux-core/src/events.rs` — Add window event types
- `crates/shux-rpc/src/methods/mod.rs` — Register window module
- `crates/shux-rpc/src/router.rs` — Register window.* method handlers
- `crates/shux/src/main.rs` — Wire window subcommands into clap CLI
- `crates/shux/src/commands/mod.rs` — Register window module

---

## Execution Steps

### Step 1: Define window mutation types in shux-core

Create the mutation command enum and result types for window operations. Like session commands, these flow through the single-writer mpsc channel.

In `crates/shux-core/src/window.rs`:

```rust
use uuid::Uuid;

/// Commands that mutate window state.
#[derive(Debug, Clone)]
pub enum WindowCommand {
    Create {
        session_id: Uuid,
        name: Option<String>,
        cwd: Option<std::path::PathBuf>,
    },
    Ensure {
        session_id: Uuid,
        name: String,
    },
    Rename {
        window_id: Uuid,
        new_name: String,
    },
    Focus {
        window_id: Uuid,
    },
    Reorder {
        window_id: Uuid,
        new_index: usize,
    },
    Kill {
        window_id: Uuid,
    },
    List {
        session_id: Uuid,
    },
}

#[derive(Debug, Clone)]
pub enum WindowResult {
    Created {
        window_id: Uuid,
        pane_id: Uuid,
    },
    Ensured {
        window_id: Uuid,
        created: bool,
    },
    Renamed {
        window_id: Uuid,
    },
    Focused {
        window_id: Uuid,
        previous_window_id: Option<Uuid>,
    },
    Reordered {
        window_id: Uuid,
    },
    Killed {
        window_id: Uuid,
        killed_panes: Vec<Uuid>,
    },
    Error(WindowError),
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum WindowError {
    #[error("window not found: {0}")]
    NotFound(Uuid),

    #[error("session not found: {0}")]
    SessionNotFound(Uuid),

    #[error("window name already exists in session: {0}")]
    NameConflict(String),

    #[error("cannot kill the last window in a session (kill the session instead)")]
    LastWindow,

    #[error("reorder index {0} out of range (session has {1} windows)")]
    IndexOutOfRange(usize, usize),

    #[error("window name is empty")]
    EmptyName,

    #[error("internal error: {0}")]
    Internal(String),
}
```

### Step 2: Implement window mutations on SessionGraph

Add methods to the `SessionGraph` that execute window mutations. Window creation automatically generates a default pane and a single-leaf LayoutTree.

In `crates/shux-core/src/graph.rs`, add:

```rust
impl SessionGraph {
    /// Create a new window in a session with a default pane.
    pub fn create_window(
        &mut self,
        session_id: Uuid,
        name: Option<String>,
        cwd: Option<std::path::PathBuf>,
    ) -> Result<(Uuid, Uuid), WindowError> {
        let session = self.sessions.get_mut(&session_id)
            .ok_or(WindowError::SessionNotFound(session_id))?;

        // Generate window name: use provided name or next index
        let window_name = name.unwrap_or_else(|| {
            format!("{}", session.windows.len())
        });

        let window_id = Uuid::new_v4();
        let pane_id = Uuid::new_v4();

        let pane = Pane {
            id: pane_id,
            window: window_id,
            title: String::new(),
            auto_title: true,
            cwd: cwd.clone().unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_default()
            }),
            command: vec![],
            exit_status: None,
            restart: RestartPolicy::Never,
            theme: None,
            tags: HashMap::new(),
            version: 1,
        };

        let window = Window {
            id: window_id,
            session: session_id,
            title: window_name,
            layout: LayoutNode::Leaf { pane: pane_id },
            active_pane: pane_id,
            cwd,
            theme: None,
            tags: HashMap::new(),
            version: 1,
        };

        self.panes.insert(pane_id, pane);
        self.windows.insert(window_id, window);

        // Add to session's window list and make it active
        session.windows.push(window_id);
        session.active_window = window_id;
        session.version += 1;

        Ok((window_id, pane_id))
    }

    /// Create a window if one with this name doesn't already exist in the session.
    pub fn ensure_window(
        &mut self,
        session_id: Uuid,
        name: String,
    ) -> Result<(Uuid, bool), WindowError> {
        let session = self.sessions.get(&session_id)
            .ok_or(WindowError::SessionNotFound(session_id))?;

        // Check if window with this name already exists in session
        for window_id in &session.windows {
            if let Some(window) = self.windows.get(window_id) {
                if window.title == name {
                    return Ok((*window_id, false));
                }
            }
        }

        let (window_id, _pane_id) = self.create_window(
            session_id,
            Some(name),
            None,
        )?;
        Ok((window_id, true))
    }

    /// Rename a window.
    pub fn rename_window(
        &mut self,
        window_id: Uuid,
        new_name: String,
    ) -> Result<(), WindowError> {
        if new_name.is_empty() {
            return Err(WindowError::EmptyName);
        }

        let window = self.windows.get(&window_id)
            .ok_or(WindowError::NotFound(window_id))?;
        let session_id = window.session;

        // Check for name conflict within the same session
        let session = self.sessions.get(&session_id)
            .ok_or(WindowError::SessionNotFound(session_id))?;

        for wid in &session.windows {
            if *wid != window_id {
                if let Some(w) = self.windows.get(wid) {
                    if w.title == new_name {
                        return Err(WindowError::NameConflict(new_name));
                    }
                }
            }
        }

        let window = self.windows.get_mut(&window_id)
            .ok_or(WindowError::NotFound(window_id))?;
        window.title = new_name;
        window.version += 1;
        Ok(())
    }

    /// Focus (activate) a window.
    /// Returns the previously active window ID.
    pub fn focus_window(&mut self, window_id: Uuid) -> Result<Option<Uuid>, WindowError> {
        let window = self.windows.get(&window_id)
            .ok_or(WindowError::NotFound(window_id))?;
        let session_id = window.session;

        let session = self.sessions.get_mut(&session_id)
            .ok_or(WindowError::SessionNotFound(session_id))?;

        // Verify window belongs to this session
        if !session.windows.contains(&window_id) {
            return Err(WindowError::NotFound(window_id));
        }

        let previous = if session.active_window != window_id {
            Some(session.active_window)
        } else {
            None
        };

        session.active_window = window_id;
        session.version += 1;
        Ok(previous)
    }

    /// Reorder a window to a new index position within its session.
    pub fn reorder_window(
        &mut self,
        window_id: Uuid,
        new_index: usize,
    ) -> Result<(), WindowError> {
        let window = self.windows.get(&window_id)
            .ok_or(WindowError::NotFound(window_id))?;
        let session_id = window.session;

        let session = self.sessions.get_mut(&session_id)
            .ok_or(WindowError::SessionNotFound(session_id))?;

        let window_count = session.windows.len();
        if new_index >= window_count {
            return Err(WindowError::IndexOutOfRange(new_index, window_count));
        }

        // Find current position and move
        if let Some(current_index) = session.windows.iter().position(|id| *id == window_id) {
            session.windows.remove(current_index);
            session.windows.insert(new_index, window_id);
            session.version += 1;
        }

        Ok(())
    }

    /// Kill a window and all its panes.
    /// Cannot kill the last window in a session (kill the session instead).
    pub fn kill_window(&mut self, window_id: Uuid) -> Result<Vec<Uuid>, WindowError> {
        let window = self.windows.get(&window_id)
            .ok_or(WindowError::NotFound(window_id))?;
        let session_id = window.session;

        let session = self.sessions.get(&session_id)
            .ok_or(WindowError::SessionNotFound(session_id))?;

        if session.windows.len() <= 1 {
            return Err(WindowError::LastWindow);
        }

        // Collect pane IDs before removing
        let pane_ids = self.windows.get(&window_id)
            .map(|w| w.layout.pane_ids())
            .unwrap_or_default();

        // Remove panes
        for pane_id in &pane_ids {
            self.panes.remove(pane_id);
        }

        // Remove window
        self.windows.remove(&window_id);

        // Remove from session and fix active window
        let session = self.sessions.get_mut(&session_id)
            .ok_or(WindowError::SessionNotFound(session_id))?;
        session.windows.retain(|id| *id != window_id);

        if session.active_window == window_id {
            // Focus the last window in the list (or first if this was the last)
            session.active_window = *session.windows.last()
                .expect("at least one window remains after LastWindow check");
        }
        session.version += 1;

        Ok(pane_ids)
    }

    /// List all windows in a session, in order.
    pub fn list_windows(&self, session_id: Uuid) -> Result<Vec<&Window>, WindowError> {
        let session = self.sessions.get(&session_id)
            .ok_or(WindowError::SessionNotFound(session_id))?;

        let windows: Vec<&Window> = session.windows.iter()
            .filter_map(|id| self.windows.get(id))
            .collect();
        Ok(windows)
    }
}
```

### Step 3: Add window event types

In `crates/shux-core/src/events.rs`:

```rust
/// Window-related events emitted on the event bus.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum WindowEvent {
    #[serde(rename = "window.created")]
    Created {
        window_id: Uuid,
        session_id: Uuid,
        name: String,
        pane_id: Uuid,
    },

    #[serde(rename = "window.activated")]
    Activated {
        window_id: Uuid,
        session_id: Uuid,
        previous_window_id: Option<Uuid>,
    },

    #[serde(rename = "window.renamed")]
    Renamed {
        window_id: Uuid,
        old_name: String,
        new_name: String,
    },

    #[serde(rename = "window.reordered")]
    Reordered {
        window_id: Uuid,
        session_id: Uuid,
        new_index: usize,
    },

    #[serde(rename = "window.killed")]
    Killed {
        window_id: Uuid,
        session_id: Uuid,
        killed_panes: Vec<Uuid>,
    },
}
```

### Step 4: Implement JSON-RPC method handlers

In `crates/shux-rpc/src/methods/window.rs`:

```rust
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Request params ────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct WindowCreateParams {
    pub session_id: Uuid,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WindowEnsureParams {
    pub session_id: Uuid,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct WindowListParams {
    pub session_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct WindowRenameParams {
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct WindowFocusParams {
    pub id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct WindowReorderParams {
    pub id: Uuid,
    pub new_index: usize,
}

#[derive(Debug, Deserialize)]
pub struct WindowKillParams {
    pub id: Uuid,
}

// ── Response types ────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct WindowInfo {
    pub id: Uuid,
    pub session_id: Uuid,
    pub title: String,
    pub pane_count: usize,
    pub active_pane_id: Uuid,
    pub index: usize,
    pub version: u64,
}

#[derive(Debug, Serialize)]
pub struct WindowCreateResult {
    pub window: WindowInfo,
    pub pane_id: Uuid,
}

#[derive(Debug, Serialize)]
pub struct WindowEnsureResult {
    pub window: WindowInfo,
    pub created: bool,
}

#[derive(Debug, Serialize)]
pub struct WindowFocusResult {
    pub window: WindowInfo,
    pub previous_window_id: Option<Uuid>,
}

// ── Handler functions ─────────────────────────────────

pub async fn handle_window_create(
    state: &AppState,
    params: WindowCreateParams,
) -> Result<WindowCreateResult, RpcError> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    state.mutation_tx.send(Mutation::Window(
        WindowCommand::Create {
            session_id: params.session_id,
            name: params.name,
            cwd: params.cwd.map(std::path::PathBuf::from),
        },
        tx,
    )).await.map_err(|_| RpcError::internal("state owner task is gone"))?;

    match rx.await.map_err(|_| RpcError::internal("dropped"))? {
        WindowResult::Created { window_id, pane_id } => {
            let snapshot = state.graph.load();
            let window = snapshot.windows.get(&window_id)
                .ok_or_else(|| RpcError::internal("window not in snapshot"))?;
            let session = snapshot.sessions.get(&window.session)
                .ok_or_else(|| RpcError::internal("session not in snapshot"))?;
            let index = session.windows.iter().position(|id| *id == window_id).unwrap_or(0);

            Ok(WindowCreateResult {
                window: window_to_info(window, index),
                pane_id,
            })
        }
        WindowResult::Error(e) => Err(window_error_to_rpc(e)),
        _ => Err(RpcError::internal("unexpected result")),
    }
}

pub async fn handle_window_list(
    state: &AppState,
    params: WindowListParams,
) -> Result<Vec<WindowInfo>, RpcError> {
    let snapshot = state.graph.load();
    let session = snapshot.sessions.get(&params.session_id)
        .ok_or_else(|| RpcError::new(-32002, "session not found"))?;

    let windows: Vec<WindowInfo> = session.windows.iter().enumerate()
        .filter_map(|(index, id)| {
            snapshot.windows.get(id).map(|w| window_to_info(w, index))
        })
        .collect();

    Ok(windows)
}

pub async fn handle_window_ensure(
    state: &AppState,
    params: WindowEnsureParams,
) -> Result<WindowEnsureResult, RpcError> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    state.mutation_tx.send(Mutation::Window(
        WindowCommand::Ensure {
            session_id: params.session_id,
            name: params.name,
        },
        tx,
    )).await.map_err(|_| RpcError::internal("state owner task is gone"))?;

    match rx.await.map_err(|_| RpcError::internal("dropped"))? {
        WindowResult::Ensured { window_id, created } => {
            let snapshot = state.graph.load();
            let window = snapshot.windows.get(&window_id)
                .ok_or_else(|| RpcError::internal("window not in snapshot"))?;
            let session = snapshot.sessions.get(&window.session)
                .ok_or_else(|| RpcError::internal("session not in snapshot"))?;
            let index = session.windows.iter().position(|id| *id == window_id).unwrap_or(0);

            Ok(WindowEnsureResult {
                window: window_to_info(window, index),
                created,
            })
        }
        WindowResult::Error(e) => Err(window_error_to_rpc(e)),
        _ => Err(RpcError::internal("unexpected result")),
    }
}

pub async fn handle_window_rename(
    state: &AppState,
    params: WindowRenameParams,
) -> Result<WindowInfo, RpcError> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    state.mutation_tx.send(Mutation::Window(
        WindowCommand::Rename {
            window_id: params.id,
            new_name: params.name,
        },
        tx,
    )).await.map_err(|_| RpcError::internal("state owner task is gone"))?;

    match rx.await.map_err(|_| RpcError::internal("dropped"))? {
        WindowResult::Renamed { window_id } => {
            let snapshot = state.graph.load();
            let window = snapshot.windows.get(&window_id)
                .ok_or_else(|| RpcError::internal("window not in snapshot"))?;
            let session = snapshot.sessions.get(&window.session)
                .ok_or_else(|| RpcError::internal("session not in snapshot"))?;
            let index = session.windows.iter().position(|id| *id == window_id).unwrap_or(0);
            Ok(window_to_info(window, index))
        }
        WindowResult::Error(e) => Err(window_error_to_rpc(e)),
        _ => Err(RpcError::internal("unexpected result")),
    }
}

pub async fn handle_window_focus(
    state: &AppState,
    params: WindowFocusParams,
) -> Result<WindowFocusResult, RpcError> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    state.mutation_tx.send(Mutation::Window(
        WindowCommand::Focus { window_id: params.id },
        tx,
    )).await.map_err(|_| RpcError::internal("state owner task is gone"))?;

    match rx.await.map_err(|_| RpcError::internal("dropped"))? {
        WindowResult::Focused { window_id, previous_window_id } => {
            let snapshot = state.graph.load();
            let window = snapshot.windows.get(&window_id)
                .ok_or_else(|| RpcError::internal("window not in snapshot"))?;
            let session = snapshot.sessions.get(&window.session)
                .ok_or_else(|| RpcError::internal("session not in snapshot"))?;
            let index = session.windows.iter().position(|id| *id == window_id).unwrap_or(0);

            Ok(WindowFocusResult {
                window: window_to_info(window, index),
                previous_window_id,
            })
        }
        WindowResult::Error(e) => Err(window_error_to_rpc(e)),
        _ => Err(RpcError::internal("unexpected result")),
    }
}

pub async fn handle_window_reorder(
    state: &AppState,
    params: WindowReorderParams,
) -> Result<WindowInfo, RpcError> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    state.mutation_tx.send(Mutation::Window(
        WindowCommand::Reorder {
            window_id: params.id,
            new_index: params.new_index,
        },
        tx,
    )).await.map_err(|_| RpcError::internal("state owner task is gone"))?;

    match rx.await.map_err(|_| RpcError::internal("dropped"))? {
        WindowResult::Reordered { window_id } => {
            let snapshot = state.graph.load();
            let window = snapshot.windows.get(&window_id)
                .ok_or_else(|| RpcError::internal("window not in snapshot"))?;
            let session = snapshot.sessions.get(&window.session)
                .ok_or_else(|| RpcError::internal("session not in snapshot"))?;
            let index = session.windows.iter().position(|id| *id == window_id).unwrap_or(0);
            Ok(window_to_info(window, index))
        }
        WindowResult::Error(e) => Err(window_error_to_rpc(e)),
        _ => Err(RpcError::internal("unexpected result")),
    }
}

pub async fn handle_window_kill(
    state: &AppState,
    params: WindowKillParams,
) -> Result<serde_json::Value, RpcError> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    state.mutation_tx.send(Mutation::Window(
        WindowCommand::Kill { window_id: params.id },
        tx,
    )).await.map_err(|_| RpcError::internal("state owner task is gone"))?;

    match rx.await.map_err(|_| RpcError::internal("dropped"))? {
        WindowResult::Killed { window_id, killed_panes } => {
            Ok(serde_json::json!({
                "killed": window_id,
                "killed_panes": killed_panes,
            }))
        }
        WindowResult::Error(e) => Err(window_error_to_rpc(e)),
        _ => Err(RpcError::internal("unexpected result")),
    }
}

// ── Helpers ───────────────────────────────────────────

fn window_to_info(window: &Window, index: usize) -> WindowInfo {
    WindowInfo {
        id: window.id,
        session_id: window.session,
        title: window.title.clone(),
        pane_count: window.layout.pane_ids().len(),
        active_pane_id: window.active_pane,
        index,
        version: window.version,
    }
}

fn window_error_to_rpc(e: WindowError) -> RpcError {
    match e {
        WindowError::NotFound(_) => RpcError::new(-32002, e.to_string()),
        WindowError::SessionNotFound(_) => RpcError::new(-32002, e.to_string()),
        WindowError::NameConflict(_) => RpcError::new(-32003, e.to_string()),
        WindowError::LastWindow => RpcError::new(-32006, e.to_string()),
        WindowError::IndexOutOfRange(_, _) => RpcError::invalid_params(e.to_string()),
        WindowError::EmptyName => RpcError::invalid_params(e.to_string()),
        WindowError::Internal(msg) => RpcError::internal(msg),
    }
}
```

### Step 5: Register window methods in the RPC router

In `crates/shux-rpc/src/router.rs`, add to the dispatch match alongside session methods:

```rust
"window.create" => {
    let p: WindowCreateParams = serde_json::from_value(params)
        .map_err(|e| RpcError::invalid_params(e.to_string()))?;
    let result = handle_window_create(state, p).await?;
    Ok(serde_json::to_value(result).unwrap())
}
"window.list" => {
    let p: WindowListParams = serde_json::from_value(params)
        .map_err(|e| RpcError::invalid_params(e.to_string()))?;
    let result = handle_window_list(state, p).await?;
    Ok(serde_json::to_value(result).unwrap())
}
"window.ensure" => {
    let p: WindowEnsureParams = serde_json::from_value(params)
        .map_err(|e| RpcError::invalid_params(e.to_string()))?;
    let result = handle_window_ensure(state, p).await?;
    Ok(serde_json::to_value(result).unwrap())
}
"window.rename" => {
    let p: WindowRenameParams = serde_json::from_value(params)
        .map_err(|e| RpcError::invalid_params(e.to_string()))?;
    let result = handle_window_rename(state, p).await?;
    Ok(serde_json::to_value(result).unwrap())
}
"window.focus" => {
    let p: WindowFocusParams = serde_json::from_value(params)
        .map_err(|e| RpcError::invalid_params(e.to_string()))?;
    let result = handle_window_focus(state, p).await?;
    Ok(serde_json::to_value(result).unwrap())
}
"window.reorder" => {
    let p: WindowReorderParams = serde_json::from_value(params)
        .map_err(|e| RpcError::invalid_params(e.to_string()))?;
    let result = handle_window_reorder(state, p).await?;
    Ok(serde_json::to_value(result).unwrap())
}
"window.kill" => {
    let p: WindowKillParams = serde_json::from_value(params)
        .map_err(|e| RpcError::invalid_params(e.to_string()))?;
    let result = handle_window_kill(state, p).await?;
    Ok(result)
}
```

### Step 6: Implement CLI window subcommands

In `crates/shux/src/commands/window.rs`:

```rust
use clap::Subcommand;
use uuid::Uuid;

/// Window management commands.
#[derive(Debug, Subcommand)]
pub enum WindowCommands {
    /// Create a new window in the current session
    Create {
        /// Window name
        #[arg(short = 'n', long)]
        name: Option<String>,

        /// Session name or ID (defaults to current session)
        #[arg(short = 's', long)]
        session: Option<String>,

        /// Working directory for the new window
        #[arg(short = 'c', long)]
        cwd: Option<String>,
    },

    /// List windows in a session
    List {
        /// Session name or ID (defaults to current session)
        #[arg(short = 's', long)]
        session: Option<String>,

        /// Output format
        #[arg(long, default_value = "text")]
        format: OutputFormat,
    },

    /// Rename a window
    Rename {
        /// Window index or ID
        #[arg(short = 'w', long)]
        target: String,

        /// New name
        #[arg(short = 'n', long)]
        name: String,
    },

    /// Focus (switch to) a window
    Focus {
        /// Window index, name, or ID
        #[arg(short = 'w', long)]
        target: String,
    },

    /// Reorder a window to a new position
    Reorder {
        /// Window index or ID
        #[arg(short = 'w', long)]
        target: String,

        /// New position index (0-based)
        #[arg(short = 'i', long)]
        index: usize,
    },

    /// Kill a window and all its panes
    Kill {
        /// Window index, name, or ID
        #[arg(short = 'w', long)]
        target: String,
    },
}

pub async fn execute(cmd: WindowCommands, client: &mut RpcClient) -> anyhow::Result<()> {
    match cmd {
        WindowCommands::Create { name, session, cwd } => {
            let session_id = resolve_current_session(client, session.as_deref()).await?;
            let params = serde_json::json!({
                "session_id": session_id,
                "name": name,
                "cwd": cwd,
            });
            let result = client.call("window.create", params).await?;
            println!(
                "Window: {} (index {})",
                result["window"]["title"].as_str().unwrap_or("?"),
                result["window"]["index"].as_u64().unwrap_or(0),
            );
            Ok(())
        }

        WindowCommands::List { session, format } => {
            let session_id = resolve_current_session(client, session.as_deref()).await?;
            let result = client.call("window.list", serde_json::json!({
                "session_id": session_id,
            })).await?;
            let windows: Vec<serde_json::Value> = serde_json::from_value(result)?;
            let active_window_id = windows
                .iter()
                .find(|w| w["is_active"].as_bool().unwrap_or(false))
                .and_then(|w| w["id"].as_str())
                .map(str::to_string);

            match format {
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&windows)?);
                }
                OutputFormat::Text => {
                    for w in &windows {
                        let active = if Some(w["id"].as_str().unwrap_or("")) ==
                            active_window_id.as_deref() {
                            "*"
                        } else {
                            " "
                        };
                        println!(
                            "{}{}: {} ({} panes)",
                            w["index"].as_u64().unwrap_or(0),
                            active,
                            w["title"].as_str().unwrap_or("?"),
                            w["pane_count"].as_u64().unwrap_or(0),
                        );
                    }
                }
            }
            Ok(())
        }

        WindowCommands::Kill { target } => {
            let window_id = resolve_window(client, &target).await?;
            client.call("window.kill", serde_json::json!({ "id": window_id })).await?;
            println!("Killed window: {}", target);
            Ok(())
        }

        WindowCommands::Focus { target } => {
            let window_id = resolve_window(client, &target).await?;
            client.call("window.focus", serde_json::json!({ "id": window_id })).await?;
            Ok(())
        }

        WindowCommands::Rename { target, name } => {
            let window_id = resolve_window(client, &target).await?;
            client.call("window.rename", serde_json::json!({ "id": window_id, "name": name })).await?;
            println!("Renamed window {} -> {}", target, name);
            Ok(())
        }

        WindowCommands::Reorder { target, index } => {
            let window_id = resolve_window(client, &target).await?;
            client.call("window.reorder", serde_json::json!({
                "id": window_id,
                "new_index": index,
            })).await?;
            println!("Moved window {} to index {}", target, index);
            Ok(())
        }
    }
}
```

### Step 7: Wire the state-owner task to handle window commands

Follow the same pattern as session commands: dispatch through the single-writer task, emit events after successful mutations.

```rust
Mutation::Window(cmd, reply_tx) => {
    let result = match cmd {
        WindowCommand::Create { session_id, name, cwd } => {
            match graph.create_window(session_id, name.clone(), cwd) {
                Ok((window_id, pane_id)) => {
                    if let Err(e) = pty_manager.spawn_for_pane(pane_id).await {
                        tracing::error!("Failed to spawn PTY: {}", e);
                    }
                    let window_name = graph.windows.get(&window_id)
                        .map(|w| w.title.clone())
                        .unwrap_or_default();
                    let _ = event_tx.send(Event::window(WindowEvent::Created {
                        window_id, session_id, name: window_name, pane_id,
                    }));
                    WindowResult::Created { window_id, pane_id }
                }
                Err(e) => WindowResult::Error(e),
            }
        }
        WindowCommand::Focus { window_id } => {
            match graph.focus_window(window_id) {
                Ok(previous) => {
                    let session_id = graph.windows.get(&window_id)
                        .map(|w| w.session)
                        .unwrap_or(Uuid::nil());
                    let _ = event_tx.send(Event::window(WindowEvent::Activated {
                        window_id, session_id, previous_window_id: previous,
                    }));
                    WindowResult::Focused { window_id, previous_window_id: previous }
                }
                Err(e) => WindowResult::Error(e),
            }
        }
        WindowCommand::Kill { window_id } => {
            let session_id = graph.windows.get(&window_id)
                .map(|w| w.session)
                .unwrap_or(Uuid::nil());
            match graph.kill_window(window_id) {
                Ok(killed_panes) => {
                    for pane_id in &killed_panes {
                        pty_manager.kill(*pane_id).await;
                    }
                    let _ = event_tx.send(Event::window(WindowEvent::Killed {
                        window_id, session_id, killed_panes: killed_panes.clone(),
                    }));
                    WindowResult::Killed { window_id, killed_panes }
                }
                Err(e) => WindowResult::Error(e),
            }
        }
        // ... Ensure, Rename, Reorder follow the same pattern
    };
    let _ = reply_tx.send(result);
}
```

### Step 8: Write L3 API contract tests

In `crates/shux-rpc/tests/window_api.rs`:

```rust
//! L3 API contract tests for window.* methods.

use shux_test_harness::TestDaemon;

#[tokio::test]
async fn test_window_create() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    // First create a session (prerequisite)
    let session = client.call("session.create", serde_json::json!({ "name": "test" })).await.unwrap();
    let session_id = session["session"]["id"].as_str().unwrap();

    // Create a window
    let result = client.call("window.create", serde_json::json!({
        "session_id": session_id,
        "name": "editor",
    })).await.unwrap();

    assert_eq!(result["window"]["title"], "editor");
    assert!(result["pane_id"].is_string());
}

#[tokio::test]
async fn test_window_list() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let session = client.call("session.create", serde_json::json!({ "name": "test" })).await.unwrap();
    let session_id = session["session"]["id"].as_str().unwrap();

    // Session comes with one default window
    client.call("window.create", serde_json::json!({
        "session_id": session_id, "name": "second"
    })).await.unwrap();

    let windows: Vec<serde_json::Value> = serde_json::from_value(
        client.call("window.list", serde_json::json!({ "session_id": session_id })).await.unwrap()
    ).unwrap();

    assert_eq!(windows.len(), 2);
}

#[tokio::test]
async fn test_window_ensure_creates_new() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let session = client.call("session.create", serde_json::json!({ "name": "test" })).await.unwrap();
    let session_id = session["session"]["id"].as_str().unwrap();

    let result = client.call("window.ensure", serde_json::json!({
        "session_id": session_id, "name": "logs"
    })).await.unwrap();

    assert_eq!(result["window"]["title"], "logs");
    assert_eq!(result["created"], true);
}

#[tokio::test]
async fn test_window_ensure_returns_existing() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let session = client.call("session.create", serde_json::json!({ "name": "test" })).await.unwrap();
    let session_id = session["session"]["id"].as_str().unwrap();

    let first = client.call("window.ensure", serde_json::json!({
        "session_id": session_id, "name": "logs"
    })).await.unwrap();

    let second = client.call("window.ensure", serde_json::json!({
        "session_id": session_id, "name": "logs"
    })).await.unwrap();

    assert_eq!(first["window"]["id"], second["window"]["id"]);
    assert_eq!(second["created"], false);
}

#[tokio::test]
async fn test_window_focus_switches_active() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let session = client.call("session.create", serde_json::json!({ "name": "test" })).await.unwrap();
    let session_id = session["session"]["id"].as_str().unwrap();
    let default_window_id = session["window_id"].as_str().unwrap();

    let second = client.call("window.create", serde_json::json!({
        "session_id": session_id, "name": "second"
    })).await.unwrap();
    let second_id = second["window"]["id"].as_str().unwrap();

    // Focus back to default window
    let focus_result = client.call("window.focus", serde_json::json!({
        "id": default_window_id,
    })).await.unwrap();

    assert_eq!(focus_result["window"]["id"].as_str().unwrap(), default_window_id);
    assert_eq!(focus_result["previous_window_id"].as_str().unwrap(), second_id);
}

#[tokio::test]
async fn test_window_reorder() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let session = client.call("session.create", serde_json::json!({ "name": "test" })).await.unwrap();
    let session_id = session["session"]["id"].as_str().unwrap();

    client.call("window.create", serde_json::json!({
        "session_id": session_id, "name": "second"
    })).await.unwrap();
    let third = client.call("window.create", serde_json::json!({
        "session_id": session_id, "name": "third"
    })).await.unwrap();
    let third_id = third["window"]["id"].as_str().unwrap();

    // Move third window to index 0
    let result = client.call("window.reorder", serde_json::json!({
        "id": third_id, "new_index": 0
    })).await.unwrap();

    assert_eq!(result["index"], 0);
}

#[tokio::test]
async fn test_window_kill() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let session = client.call("session.create", serde_json::json!({ "name": "test" })).await.unwrap();
    let session_id = session["session"]["id"].as_str().unwrap();

    let second = client.call("window.create", serde_json::json!({
        "session_id": session_id, "name": "doomed"
    })).await.unwrap();
    let second_id = second["window"]["id"].as_str().unwrap();

    client.call("window.kill", serde_json::json!({ "id": second_id })).await.unwrap();

    let windows: Vec<serde_json::Value> = serde_json::from_value(
        client.call("window.list", serde_json::json!({ "session_id": session_id })).await.unwrap()
    ).unwrap();

    assert_eq!(windows.len(), 1);
}

#[tokio::test]
async fn test_window_kill_last_window_fails() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let session = client.call("session.create", serde_json::json!({ "name": "test" })).await.unwrap();
    let window_id = session["window_id"].as_str().unwrap();

    let err = client.call("window.kill", serde_json::json!({ "id": window_id })).await.unwrap_err();
    assert_eq!(err.code, -32006); // LastWindow
}

#[tokio::test]
async fn test_window_rename() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let session = client.call("session.create", serde_json::json!({ "name": "test" })).await.unwrap();
    let window_id = session["window_id"].as_str().unwrap();

    let result = client.call("window.rename", serde_json::json!({
        "id": window_id, "name": "renamed"
    })).await.unwrap();

    assert_eq!(result["title"], "renamed");
}

#[tokio::test]
async fn test_window_reorder_out_of_range_fails() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let session = client.call("session.create", serde_json::json!({ "name": "test" })).await.unwrap();
    let window_id = session["window_id"].as_str().unwrap();

    let err = client.call("window.reorder", serde_json::json!({
        "id": window_id, "new_index": 99
    })).await.unwrap_err();

    assert_eq!(err.code, -32602); // InvalidParams
}
```

---

## Verification

### Functional

```bash
# Create a session and verify default window
cargo run -p shux -- new -s workspace --no-attach
cargo run -p shux -- window list -s workspace
# Expected: one window at index 0

# Create additional windows
cargo run -p shux -- window create -s workspace -n editor
cargo run -p shux -- window create -s workspace -n server
cargo run -p shux -- window list -s workspace
# Expected: three windows at indices 0, 1, 2

# Rename a window
cargo run -p shux -- window rename -w editor -n code
cargo run -p shux -- window list -s workspace
# Expected: window renamed from "editor" to "code"

# Reorder a window
cargo run -p shux -- window reorder -w server -i 0
cargo run -p shux -- window list -s workspace
# Expected: "server" now at index 0

# Kill a window
cargo run -p shux -- window kill -w server
cargo run -p shux -- window list -s workspace
# Expected: two windows remain

# JSON output
cargo run -p shux -- window list -s workspace --format json
```

### Tests

```bash
# Run unit tests for window logic
cargo nextest run -p shux-core --lib -- window

# Run API contract tests
cargo nextest run -p shux-rpc --test window_api

# Run all workspace tests
cargo nextest run --workspace

# Lint
cargo clippy --workspace --all-targets -- -D warnings
```

### Visual Tests (L4) — iterm2-driver

**Script:** `.claude/automations/test_014_window_crud.py`
**Run:** `uv run .claude/automations/test_014_window_crud.py`

This is a **mandatory** visual verification gate. Every test runs in a real iTerm2
session, sends real CLI commands, reads back actual screen contents, and captures
screenshots at every transition. No smoke tests — every assertion reads the terminal.

#### Test Matrix (25 tests)

##### Part A — Setup & Default Window Verification (Tests 1–4)

| # | Test | Command | Screen Assertions | Screenshot |
|---|------|---------|-------------------|------------|
| 1 | Build | `make build` (subprocess) | returncode == 0 | — |
| 2 | Create Session | `shux new -s ws-test -d` | Screen contains "ws-test" and ("created" or "Created"). | `014_session_created.png` |
| 3 | Default Window Exists | `shux window list -s ws-test` | Screen shows exactly 1 window entry. Entry shows index 0. Entry shows "1 pane" or "panes". Verify the default window has a name (e.g., "0"). | `014_default_window.png` |
| 4 | Default Window Has Active Marker | (from test 3 screen) | The single window entry has an active marker (`*` or equivalent). Since it's the only window, it must be active. | — |

##### Part B — Window Creation (Tests 5–9)

| # | Test | Command | Screen Assertions | Screenshot |
|---|------|---------|-------------------|------------|
| 5 | Create Named Window | `shux window create -s ws-test -n editor` | Screen contains "editor" and ("created" or "Window:" or index number). | `014_create_editor.png` |
| 6 | Create Second Named Window | `shux window create -s ws-test -n server` | Screen contains "server". | `014_create_server.png` |
| 7 | Create Third Named Window | `shux window create -s ws-test -n logs` | Screen contains "logs". | `014_create_logs.png` |
| 8 | List Shows All Windows | `shux window list -s ws-test` | Screen contains ALL of: "editor", "server", "logs", plus the default window name. Count exactly 4 window entries (4 lines with index numbers). Verify indices are 0, 1, 2, 3. | `014_list_all_windows.png` |
| 9 | Newest Window Is Active | (from test 8 screen) | The "logs" window (last created) has the active marker (`*`). Other windows do NOT have the active marker. Parse each line and verify exactly one `*`. | — |

##### Part C — Window Auto-Naming (Tests 10–11)

| # | Test | Command | Screen Assertions | Screenshot |
|---|------|---------|-------------------|------------|
| 10 | Create Unnamed Window | `shux window create -s ws-test` (no `-n`) | Window is created without error. Screen shows a window name that is index-based (e.g., "4") or auto-generated. | `014_create_unnamed.png` |
| 11 | Verify Auto-Name in List | `shux window list -s ws-test` | Now 5 windows total. The unnamed one has a numeric or auto-generated name. | `014_list_with_unnamed.png` |

##### Part D — Window Focus/Switching (Tests 12–14)

| # | Test | Command | Screen Assertions | Screenshot |
|---|------|---------|-------------------|------------|
| 12 | Focus By Name | `shux window focus -w editor -s ws-test` | Screen indicates focus changed. No error. | `014_focus_editor.png` |
| 13 | Verify Focus Changed | `shux window list -s ws-test` | The "editor" window now has the active marker (`*`). "logs" no longer has it. Parse all lines and verify exactly one active marker, on the "editor" line. | `014_list_after_focus.png` |
| 14 | Focus Returns Previous | (from test 12 output) | Output contains the previous window ID or name (the window that was active before focus switched). | — |

##### Part E — Window Rename (Tests 15–17)

| # | Test | Command | Screen Assertions | Screenshot |
|---|------|---------|-------------------|------------|
| 15 | Rename Window | `shux window rename -w server -n backend -s ws-test` | Screen contains "renamed" or "Renamed" or "backend". No error. | `014_rename_backend.png` |
| 16 | Verify Rename in List | `shux window list -s ws-test` | Screen contains "backend" and does NOT contain standalone "server" (only "backend" where "server" was). All other windows unchanged. | `014_list_after_rename.png` |
| 17 | Rename Conflict | `shux window rename -w logs -n editor -s ws-test` | Screen contains "error" or "Error" or "conflict" or "exists". Clean error message, not a crash. | `014_rename_conflict.png` |

##### Part F — Window Reorder (Tests 18–20)

| # | Test | Command | Screen Assertions | Screenshot |
|---|------|---------|-------------------|------------|
| 18 | Reorder Window to Front | `shux window reorder -w logs -i 0 -s ws-test` | Screen indicates reorder succeeded. No error. | `014_reorder.png` |
| 19 | Verify Reorder in List | `shux window list -s ws-test` | "logs" now appears at index 0 (first line of window list). The former index-0 window moved down. Parse line numbers and verify "logs" is on the first window line. | `014_list_after_reorder.png` |
| 20 | Reorder Out of Range | `shux window reorder -w editor -i 99 -s ws-test` | Screen contains "error" or "Error" or "out of range" or "invalid". | `014_reorder_out_of_range.png` |

##### Part G — Window Kill (Tests 21–24)

| # | Test | Command | Screen Assertions | Screenshot |
|---|------|---------|-------------------|------------|
| 21 | Kill a Window | `shux window kill -w logs -s ws-test` | Screen contains "killed" or "Killed". No error. | `014_kill_logs.png` |
| 22 | Verify Kill in List | `shux window list -s ws-test` | Screen does NOT contain "logs". Window count decreased by 1 (now 4 windows). | `014_list_after_kill.png` |
| 23 | Kill Active → Focus Moves | Kill the currently active window (first check which is active via `window list`, then kill it). Run `shux window list -s ws-test` afterward. | A different window now has the active marker. The killed window is gone. Exactly one window has `*`. | `014_kill_active.png` |
| 24 | Kill Last Window Fails | Kill all but one window (run multiple `window kill`), then try to kill the last one. | Screen contains "error" or "Error" or "last window" or "cannot kill". Verify the last window is NOT killed (run `window list` to confirm 1 window remains). | `014_kill_last_fails.png` |

##### Part H — JSON Output & Cross-Verification (Test 25)

| # | Test | Command | Screen Assertions | Screenshot |
|---|------|---------|-------------------|------------|
| 25 | Window List JSON | `shux window list -s ws-test --format json` | Screen contains valid JSON: `[`, `"title"`, `"pane_count"`, `"index"`. Count JSON objects matches the expected remaining window count from previous tests. | `014_list_json.png` |

#### Implementation Notes for the Test Script

```python
# Key patterns the script MUST follow:

# 1. Session setup in a setup phase — create one session, reuse it for all window tests
# This avoids the session-creation overhead for each test and tests windows in isolation.
await send_and_wait(session, f"{SHUX_BIN} new -s ws-test -d", 3.0)

# 2. Active marker parsing — extract which window has the * marker
def parse_active_window(content):
    """Parse window list output and return the name of the active window."""
    for line in content.split("\n"):
        if "*" in line:
            # Format: "1*: editor (1 panes)" or "1* editor ..."
            return line.strip()
    return None

# 3. Window count verification — count actual window entries, not just string matches
def count_windows(content):
    """Count window entries in `window list` output."""
    count = 0
    for line in content.split("\n"):
        line = line.strip()
        # Window entries start with an index number
        if line and line[0].isdigit() and ("pane" in line.lower() or ":" in line):
            count += 1
    return count

# 4. Index order verification — parse indices and verify monotonic
def parse_window_indices(content):
    """Extract (index, name) tuples from window list output."""
    windows = []
    for line in content.split("\n"):
        line = line.strip()
        if line and line[0].isdigit():
            # Parse "0 : default (1 pane)" or "0*: editor (1 pane)"
            idx = int(line[0])
            windows.append((idx, line))
    return windows

# 5. Multi-step test with state tracking
# After reorder, the script must track the new ordering and verify subsequent
# operations against the reordered state, not the original creation order.

# 6. Progressive kill — for test 24 (kill last window), the script must:
#    a) Count current windows
#    b) Kill all but 1 (loop)
#    c) Attempt to kill the last one
#    d) Verify error
#    e) Verify 1 window still exists via `window list`
remaining = count_windows(await read_screen(session))
while remaining > 1:
    # Find a non-active window to kill (or kill any)
    await send_and_wait(session, f"{SHUX_BIN} window kill -w <target> -s ws-test", 1.5)
    remaining -= 1
# Now try killing the last one — should fail
await send_and_wait(session, f"{SHUX_BIN} window kill -w <last> -s ws-test", 1.5)
content = await read_screen(session)
assert "error" in content.lower() or "last window" in content.lower()

# 7. Cleanup kills the entire session, which cascades to all windows
# in the finally: block
subprocess.run([SHUX_BIN, "kill", "-s", "ws-test"], capture_output=True, timeout=5)
```

#### Screenshots Produced (18 total)

```
.claude/screenshots/
├── 014_session_created.png
├── 014_default_window.png
├── 014_create_editor.png
├── 014_create_server.png
├── 014_create_logs.png
├── 014_list_all_windows.png
├── 014_create_unnamed.png
├── 014_list_with_unnamed.png
├── 014_focus_editor.png
├── 014_list_after_focus.png
├── 014_rename_backend.png
├── 014_list_after_rename.png
├── 014_rename_conflict.png
├── 014_reorder.png
├── 014_list_after_reorder.png
├── 014_reorder_out_of_range.png
├── 014_kill_logs.png
├── 014_list_after_kill.png
├── 014_kill_active.png
├── 014_kill_last_fails.png
└── 014_list_json.png
```

---

## Completion Criteria

- [ ] `window.create` creates a window with a default pane and LayoutTree::Leaf, spawns PTY for the pane
- [ ] `window.list` returns all windows in a session, ordered by position index
- [ ] `window.ensure` creates if not exists (by name within session), idempotent
- [ ] `window.rename` renames a window, rejects conflicts within same session
- [ ] `window.focus` switches the session's active window, returns previous window ID
- [ ] `window.reorder` moves a window to a new index position, rejects out-of-range
- [ ] `window.kill` destroys a window and its panes, rejects killing the last window
- [ ] All mutations flow through the single-writer mpsc channel
- [ ] Events emitted: window.created, window.activated, window.renamed, window.reordered, window.killed
- [ ] CLI subcommands: `shux window create`, `shux window list`, `shux window kill`, `shux window focus`, `shux window rename`, `shux window reorder`
- [ ] Window names default to index-based names ("0", "1", ...) when not specified
- [ ] New windows automatically become the active window
- [ ] Killing the active window focuses the last remaining window
- [ ] L3 API contract tests pass: happy path + error cases (last window, not found, out of range)
- [ ] **L4 visual tests pass: `uv run .claude/automations/test_014_window_crud.py` — 25/25 tests pass with screenshots**
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] `cargo nextest run --workspace` passes

---

## Commit Message

```
feat: implement window CRUD via JSON-RPC API and CLI

- Add window.create, window.list, window.ensure, window.rename,
  window.focus, window.reorder, window.kill methods
- CLI subcommands: shux window create/list/rename/focus/reorder/kill
- Windows auto-generate default pane with PTY on creation
- Cannot kill last window in session (error -32006)
- window.ensure provides idempotent by-name lookup within session
- Emit window.created/activated/renamed/reordered/killed events
- L3 API contract tests covering all operations and edge cases
```

---

## Session Protocol

1. **Before starting:** Verify task 013 is complete. Session CRUD must work; the test daemon must support session.create. Read `CLAUDE.md` for code conventions.
2. **During:** Follow step order. Each window operation depends on sessions existing, so verify that `session.create` works in tests before testing window operations. Run `cargo check` after each step.
3. **Key patterns:**
   - Window names are unique within a session but not globally. Two sessions can have windows named "editor".
   - A new window always starts with `LayoutNode::Leaf { pane }` (single pane). Splits come in task 015.
   - When killing a window, collect all pane IDs from the LayoutTree before removing (panes may be in nested splits later).
   - The "last window" guard prevents orphan sessions. To remove everything, kill the session instead.
4. **After L3 tests pass:** Write and run the L4 visual test script: `.claude/automations/test_014_window_crud.py`. Follow the test matrix above exactly. All 25 tests must pass. Fix any failures found by visual testing (these are real bugs that L3 tests miss).
5. **After all tests pass:** Run full verification. Update `docs/PROGRESS.md`. Verify task 015 (pane operations) can build on this.
