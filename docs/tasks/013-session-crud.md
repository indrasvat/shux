# 013 — Session CRUD (API + CLI)

**Status:** Pending
**Depends On:** 012
**Parallelizable With:** 022

---

## Problem

After the M0 architecture spike establishes the daemon skeleton, SessionGraph, JSON-RPC server, and minimal TUI client, there is no way to create, list, rename, or destroy sessions through the API or CLI. The M0 session support is limited to a hardcoded default session created at daemon startup.

This task implements the full session lifecycle as the first layer of the CRUD hierarchy (sessions > windows > panes). Every operation flows through the JSON-RPC API, with CLI commands as thin wrappers. This is foundational: windows (task 014), panes (task 015), and every higher-level feature depend on sessions existing and being manageable.

The `session.ensure` operation is particularly important for AI agent workflows. Agents operate in read-plan-apply-verify loops and need idempotent operations that are safe to retry without side effects. An agent calling `session.ensure {name: "work"}` three times must get the same session back each time.

## PRD Reference

- **PRD section 6.1 (Sessions)**: Create, list, rename, kill, attach, detach. Auto-start daemon on first use.
- **PRD section 8.2 (session.* methods)**: session.list, session.create, session.ensure, session.rename, session.kill, session.attach
- **PRD section 8.6 (CLI to API mapping)**: `shux ls`, `shux new -s <name>`, `shux new -s <name> --ensure`, `shux kill -s <name>`, `shux attach -s <name>`
- **PRD section 4.3 (Architectural invariants)**: Single writer / many readers; mutations via mpsc channel; CLI == API
- **PRD section 5.1 (Session entity)**: SessionId (UUID), name (unique, user-facing), created_at, windows, active_window, env, theme, tags, version
- **PRD section 8.5 (Agent-safe patterns)**: Prefer `ensure` operations; use idempotency keys for retry loops

---

## Files to Create

- `crates/shux-rpc/src/methods/session.rs` — JSON-RPC handlers for all session.* methods
- `crates/shux-rpc/src/methods/mod.rs` — Module index for method handlers (if not already created in 008)
- `crates/shux/src/commands/session.rs` — CLI session subcommands (new, ls, kill, attach, rename)
- `crates/shux/src/commands/mod.rs` — Module index for CLI commands
- `crates/shux-core/src/session.rs` — Session mutation operations on SessionGraph
- `crates/shux-rpc/tests/session_api.rs` — L3 API contract tests

## Files to Modify

- `crates/shux-core/src/graph.rs` — Add session mutation methods to SessionGraph
- `crates/shux-core/src/events.rs` — Add session event types
- `crates/shux-rpc/src/router.rs` — Register session.* method handlers
- `crates/shux/src/main.rs` — Wire session subcommands into clap CLI
- `crates/shux/src/cli.rs` — Add session-related clap subcommands to the top-level parser

---

## Execution Steps

### Step 1: Define session mutation types in shux-core

Create the mutation command enum and result types that flow through the single-writer mpsc channel. All state changes to the SessionGraph are serialized through this channel (PRD section 4.3, invariant 6).

In `crates/shux-core/src/session.rs`:

```rust
use uuid::Uuid;
use std::collections::HashMap;

/// Commands that mutate session state.
/// Sent through the single-writer mpsc channel to the state-owner task.
#[derive(Debug, Clone)]
pub enum SessionCommand {
    Create {
        name: String,
        env: HashMap<String, String>,
    },
    Ensure {
        name: String,
        env: HashMap<String, String>,
    },
    Rename {
        session_id: Uuid,
        new_name: String,
    },
    Kill {
        session_id: Uuid,
    },
    Attach {
        session_id: Uuid,
        client_id: Uuid,
    },
    Detach {
        client_id: Uuid,
    },
}

/// The result of a session mutation, returned to the caller via oneshot.
#[derive(Debug, Clone)]
pub enum SessionResult {
    Created {
        session_id: Uuid,
        window_id: Uuid,
        pane_id: Uuid,
    },
    Ensured {
        session_id: Uuid,
        created: bool, // true if newly created, false if already existed
    },
    Renamed {
        session_id: Uuid,
    },
    Killed {
        session_id: Uuid,
    },
    Attached {
        session_id: Uuid,
    },
    Detached {
        client_id: Uuid,
    },
    Error(SessionError),
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum SessionError {
    #[error("session not found: {0}")]
    NotFound(Uuid),

    #[error("session name already exists: {0}")]
    NameConflict(String),

    #[error("session name is empty")]
    EmptyName,

    #[error("session name too long (max 128 characters)")]
    NameTooLong,

    #[error("session name contains invalid characters")]
    InvalidName,

    #[error("session has attached clients; use force to kill")]
    HasAttachedClients,

    #[error("client {0} is not attached to any session")]
    ClientNotAttached(Uuid),

    #[error("internal error: {0}")]
    Internal(String),
}
```

### Step 2: Implement session mutations on SessionGraph

Add methods to the `SessionGraph` that execute session mutations. These methods are called by the state-owner task after receiving commands from the mpsc channel.

In `crates/shux-core/src/graph.rs`, add:

```rust
impl SessionGraph {
    /// Create a new session with a default window and pane.
    /// Returns the IDs of the created session, window, and pane.
    pub fn create_session(
        &mut self,
        name: String,
        env: HashMap<String, String>,
    ) -> Result<(Uuid, Uuid, Uuid), SessionError> {
        // Validate name
        Self::validate_session_name(&name)?;

        // Check uniqueness
        if self.sessions.values().any(|s| s.name == name) {
            return Err(SessionError::NameConflict(name));
        }

        let session_id = Uuid::new_v4();
        let window_id = Uuid::new_v4();
        let pane_id = Uuid::new_v4();

        // Create the default pane (PTY will be spawned by the caller)
        let pane = Pane {
            id: pane_id,
            window: window_id,
            title: String::new(),
            auto_title: true,
            cwd: std::env::current_dir().unwrap_or_default(),
            command: vec![],
            exit_status: None,
            restart: RestartPolicy::Never,
            theme: None,
            tags: HashMap::new(),
            version: 1,
        };

        // Create the default window with a single-pane layout
        let window = Window {
            id: window_id,
            session: session_id,
            title: String::from("0"),
            layout: LayoutNode::Leaf { pane: pane_id },
            active_pane: pane_id,
            cwd: None,
            theme: None,
            tags: HashMap::new(),
            version: 1,
        };

        // Create the session
        let session = Session {
            id: session_id,
            name,
            created_at: std::time::SystemTime::now(),
            windows: vec![window_id],
            active_window: window_id,
            env,
            theme: None,
            tags: HashMap::new(),
            version: 1,
        };

        self.panes.insert(pane_id, pane);
        self.windows.insert(window_id, window);
        self.sessions.insert(session_id, session);

        Ok((session_id, window_id, pane_id))
    }

    /// Create a session if it does not already exist (idempotent).
    /// Returns the session ID and whether it was newly created.
    pub fn ensure_session(
        &mut self,
        name: String,
        env: HashMap<String, String>,
    ) -> Result<(Uuid, bool), SessionError> {
        // Check if session with this name already exists
        if let Some(existing) = self.sessions.values().find(|s| s.name == name) {
            return Ok((existing.id, false));
        }
        let (session_id, _, _) = self.create_session(name, env)?;
        Ok((session_id, true))
    }

    /// Rename a session. The new name must be unique.
    pub fn rename_session(
        &mut self,
        session_id: Uuid,
        new_name: String,
    ) -> Result<(), SessionError> {
        Self::validate_session_name(&new_name)?;

        // Check uniqueness (excluding self)
        if self.sessions.values().any(|s| s.name == new_name && s.id != session_id) {
            return Err(SessionError::NameConflict(new_name));
        }

        let session = self.sessions.get_mut(&session_id)
            .ok_or(SessionError::NotFound(session_id))?;

        session.name = new_name;
        session.version += 1;
        Ok(())
    }

    /// Kill a session and all its windows/panes.
    /// Returns the list of pane IDs that were destroyed (caller must kill PTYs).
    pub fn kill_session(&mut self, session_id: Uuid) -> Result<Vec<Uuid>, SessionError> {
        let session = self.sessions.remove(&session_id)
            .ok_or(SessionError::NotFound(session_id))?;

        let mut killed_panes = Vec::new();

        for window_id in &session.windows {
            if let Some(window) = self.windows.remove(window_id) {
                let pane_ids = window.layout.pane_ids();
                for pane_id in &pane_ids {
                    self.panes.remove(pane_id);
                }
                killed_panes.extend(pane_ids);
            }
        }

        Ok(killed_panes)
    }

    /// List all sessions with metadata.
    pub fn list_sessions(&self) -> Vec<&Session> {
        let mut sessions: Vec<_> = self.sessions.values().collect();
        sessions.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        sessions
    }

    /// Validate a session name.
    fn validate_session_name(name: &str) -> Result<(), SessionError> {
        if name.is_empty() {
            return Err(SessionError::EmptyName);
        }
        if name.len() > 128 {
            return Err(SessionError::NameTooLong);
        }
        // Allow alphanumeric, hyphens, underscores, dots
        if !name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.') {
            return Err(SessionError::InvalidName);
        }
        Ok(())
    }
}
```

### Step 3: Add session event types

Extend the event system with session lifecycle events. These events are emitted on the broadcast bus after each successful mutation.

In `crates/shux-core/src/events.rs`:

```rust
/// Session-related events emitted on the event bus.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum SessionEvent {
    #[serde(rename = "session.created")]
    Created {
        session_id: Uuid,
        name: String,
        window_id: Uuid,
        pane_id: Uuid,
    },

    #[serde(rename = "session.renamed")]
    Renamed {
        session_id: Uuid,
        old_name: String,
        new_name: String,
    },

    #[serde(rename = "session.killed")]
    Killed {
        session_id: Uuid,
        name: String,
        killed_panes: Vec<Uuid>,
    },

    #[serde(rename = "session.attached")]
    Attached {
        session_id: Uuid,
        client_id: Uuid,
    },

    #[serde(rename = "session.detached")]
    Detached {
        session_id: Uuid,
        client_id: Uuid,
    },
}
```

### Step 4: Implement JSON-RPC method handlers

Create the RPC method handlers that deserialize JSON-RPC params, send commands through the mutation channel, await results, and return JSON-RPC responses.

In `crates/shux-rpc/src/methods/session.rs`:

```rust
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use std::collections::HashMap;

// ── Request params ────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SessionCreateParams {
    pub name: String,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub client_request_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SessionEnsureParams {
    pub name: String,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct SessionRenameParams {
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct SessionKillParams {
    pub id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct SessionAttachParams {
    pub id: Uuid,
}

// ── Response types ────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct SessionInfo {
    pub id: Uuid,
    pub name: String,
    pub created_at: String, // ISO 8601
    pub window_count: usize,
    pub active_window_id: Uuid,
    pub version: u64,
}

#[derive(Debug, Serialize)]
pub struct SessionCreateResult {
    pub session: SessionInfo,
    pub window_id: Uuid,
    pub pane_id: Uuid,
}

#[derive(Debug, Serialize)]
pub struct SessionEnsureResult {
    pub session: SessionInfo,
    pub created: bool,
}

// ── Handler functions ─────────────────────────────────

/// Handle session.create: creates a new session with a default window and pane.
pub async fn handle_session_create(
    state: &AppState,
    params: SessionCreateParams,
) -> Result<SessionCreateResult, RpcError> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    state.mutation_tx.send(Mutation::Session(
        SessionCommand::Create {
            name: params.name,
            env: params.env,
        },
        tx,
    )).await.map_err(|_| RpcError::internal("state owner task is gone"))?;

    match rx.await.map_err(|_| RpcError::internal("state owner dropped response"))? {
        SessionResult::Created { session_id, window_id, pane_id } => {
            let snapshot = state.graph.load();
            let session = snapshot.sessions.get(&session_id)
                .ok_or_else(|| RpcError::internal("session created but not in snapshot"))?;
            Ok(SessionCreateResult {
                session: session_to_info(session),
                window_id,
                pane_id,
            })
        }
        SessionResult::Error(e) => Err(session_error_to_rpc(e)),
        _ => Err(RpcError::internal("unexpected result variant")),
    }
}

/// Handle session.list: returns all sessions with metadata.
pub async fn handle_session_list(
    state: &AppState,
) -> Result<Vec<SessionInfo>, RpcError> {
    let snapshot = state.graph.load();
    let sessions = snapshot.list_sessions()
        .into_iter()
        .map(session_to_info)
        .collect();
    Ok(sessions)
}

/// Handle session.ensure: create-if-not-exists (idempotent).
pub async fn handle_session_ensure(
    state: &AppState,
    params: SessionEnsureParams,
) -> Result<SessionEnsureResult, RpcError> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    state.mutation_tx.send(Mutation::Session(
        SessionCommand::Ensure {
            name: params.name,
            env: params.env,
        },
        tx,
    )).await.map_err(|_| RpcError::internal("state owner task is gone"))?;

    match rx.await.map_err(|_| RpcError::internal("state owner dropped response"))? {
        SessionResult::Ensured { session_id, created } => {
            let snapshot = state.graph.load();
            let session = snapshot.sessions.get(&session_id)
                .ok_or_else(|| RpcError::internal("session not in snapshot"))?;
            Ok(SessionEnsureResult {
                session: session_to_info(session),
                created,
            })
        }
        SessionResult::Error(e) => Err(session_error_to_rpc(e)),
        _ => Err(RpcError::internal("unexpected result variant")),
    }
}

/// Handle session.rename: rename a session.
pub async fn handle_session_rename(
    state: &AppState,
    params: SessionRenameParams,
) -> Result<SessionInfo, RpcError> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    state.mutation_tx.send(Mutation::Session(
        SessionCommand::Rename {
            session_id: params.id,
            new_name: params.name,
        },
        tx,
    )).await.map_err(|_| RpcError::internal("state owner task is gone"))?;

    match rx.await.map_err(|_| RpcError::internal("state owner dropped response"))? {
        SessionResult::Renamed { session_id } => {
            let snapshot = state.graph.load();
            let session = snapshot.sessions.get(&session_id)
                .ok_or_else(|| RpcError::internal("session not in snapshot"))?;
            Ok(session_to_info(session))
        }
        SessionResult::Error(e) => Err(session_error_to_rpc(e)),
        _ => Err(RpcError::internal("unexpected result variant")),
    }
}

/// Handle session.kill: destroy a session and all its children.
pub async fn handle_session_kill(
    state: &AppState,
    params: SessionKillParams,
) -> Result<serde_json::Value, RpcError> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    state.mutation_tx.send(Mutation::Session(
        SessionCommand::Kill {
            session_id: params.id,
        },
        tx,
    )).await.map_err(|_| RpcError::internal("state owner task is gone"))?;

    match rx.await.map_err(|_| RpcError::internal("state owner dropped response"))? {
        SessionResult::Killed { session_id } => {
            Ok(serde_json::json!({ "killed": session_id }))
        }
        SessionResult::Error(e) => Err(session_error_to_rpc(e)),
        _ => Err(RpcError::internal("unexpected result variant")),
    }
}

// ── Helpers ───────────────────────────────────────────

fn session_to_info(session: &Session) -> SessionInfo {
    SessionInfo {
        id: session.id,
        name: session.name.clone(),
        created_at: humantime::format_rfc3339(session.created_at).to_string(),
        window_count: session.windows.len(),
        active_window_id: session.active_window,
        version: session.version,
    }
}

fn session_error_to_rpc(e: SessionError) -> RpcError {
    match e {
        SessionError::NotFound(_) => RpcError::new(-32002, e.to_string()),
        SessionError::NameConflict(_) => RpcError::new(-32003, e.to_string()),
        SessionError::EmptyName
        | SessionError::NameTooLong
        | SessionError::InvalidName => RpcError::invalid_params(e.to_string()),
        SessionError::HasAttachedClients => RpcError::new(-32004, e.to_string()),
        SessionError::ClientNotAttached(_) => RpcError::new(-32005, e.to_string()),
        SessionError::Internal(msg) => RpcError::internal(msg),
    }
}
```

### Step 5: Register session methods in the RPC router

Wire the session handlers into the JSON-RPC method dispatch table.

In `crates/shux-rpc/src/router.rs`, add to the dispatch match:

```rust
pub async fn dispatch(
    state: &AppState,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    match method {
        // ... existing system.* methods from task 008 ...

        "session.create" => {
            let p: SessionCreateParams = serde_json::from_value(params)
                .map_err(|e| RpcError::invalid_params(e.to_string()))?;
            let result = handle_session_create(state, p).await?;
            Ok(serde_json::to_value(result).unwrap())
        }

        "session.list" => {
            let result = handle_session_list(state).await?;
            Ok(serde_json::to_value(result).unwrap())
        }

        "session.ensure" => {
            let p: SessionEnsureParams = serde_json::from_value(params)
                .map_err(|e| RpcError::invalid_params(e.to_string()))?;
            let result = handle_session_ensure(state, p).await?;
            Ok(serde_json::to_value(result).unwrap())
        }

        "session.rename" => {
            let p: SessionRenameParams = serde_json::from_value(params)
                .map_err(|e| RpcError::invalid_params(e.to_string()))?;
            let result = handle_session_rename(state, p).await?;
            Ok(serde_json::to_value(result).unwrap())
        }

        "session.kill" => {
            let p: SessionKillParams = serde_json::from_value(params)
                .map_err(|e| RpcError::invalid_params(e.to_string()))?;
            let result = handle_session_kill(state, p).await?;
            Ok(result)
        }

        "session.attach" => {
            let p: SessionAttachParams = serde_json::from_value(params)
                .map_err(|e| RpcError::invalid_params(e.to_string()))?;
            let result = handle_session_attach(state, p).await?;
            Ok(serde_json::to_value(result).unwrap())
        }

        _ => Err(RpcError::method_not_found(method)),
    }
}
```

### Step 6: Implement CLI session commands

Create clap subcommands that invoke the JSON-RPC API. Each command connects to the daemon socket, sends the request, and formats the response.

In `crates/shux/src/commands/session.rs`:

```rust
use clap::Subcommand;
use uuid::Uuid;

/// Session management commands.
/// These are exposed as top-level shux subcommands for ergonomics.
#[derive(Debug, Subcommand)]
pub enum SessionCommands {
    /// Create a new session (or attach to existing with --ensure)
    New {
        /// Session name
        #[arg(short = 's', long)]
        name: Option<String>,

        /// Create only if it doesn't exist (idempotent)
        #[arg(long)]
        ensure: bool,

        /// Attach to the session after creating
        #[arg(long, default_value = "true")]
        attach: bool,
    },

    /// List all sessions
    Ls {
        /// Output format
        #[arg(long, default_value = "text")]
        format: OutputFormat,
    },

    /// Kill a session and all its windows/panes
    Kill {
        /// Session name or ID
        #[arg(short = 's', long)]
        target: String,
    },

    /// Attach to an existing session
    Attach {
        /// Session name or ID
        #[arg(short = 's', long)]
        target: Option<String>,
    },

    /// Rename a session
    Rename {
        /// Current session name or ID
        #[arg(short = 's', long)]
        target: String,

        /// New name
        #[arg(short = 'n', long)]
        name: String,
    },
}

pub async fn execute(cmd: SessionCommands, client: &mut RpcClient) -> anyhow::Result<()> {
    match cmd {
        SessionCommands::New { name, ensure, attach } => {
            let session_name = name.unwrap_or_else(generate_session_name);

            let result = if ensure {
                let params = serde_json::json!({ "name": session_name });
                client.call("session.ensure", params).await?
            } else {
                let params = serde_json::json!({ "name": session_name });
                client.call("session.create", params).await?
            };

            let session_id = result["session"]["id"].as_str()
                .expect("session id in response");
            println!("Session: {} ({})", result["session"]["name"], session_id);

            if attach {
                // Hand off to TUI client attachment
                crate::tui::attach(client, session_id).await?;
            }
            Ok(())
        }

        SessionCommands::Ls { format } => {
            let result = client.call("session.list", serde_json::Value::Null).await?;
            let sessions: Vec<serde_json::Value> = serde_json::from_value(result)?;

            match format {
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&sessions)?);
                }
                OutputFormat::Text => {
                    if sessions.is_empty() {
                        println!("No sessions.");
                    } else {
                        for s in &sessions {
                            println!(
                                "{}: {} ({} windows) [created {}]",
                                s["name"].as_str().unwrap_or("?"),
                                &s["id"].as_str().unwrap_or("?")[..8],
                                s["window_count"].as_u64().unwrap_or(0),
                                s["created_at"].as_str().unwrap_or("?"),
                            );
                        }
                    }
                }
            }
            Ok(())
        }

        SessionCommands::Kill { target } => {
            let session_id = resolve_session(client, &target).await?;
            let params = serde_json::json!({ "id": session_id });
            client.call("session.kill", params).await?;
            println!("Killed session: {}", target);
            Ok(())
        }

        SessionCommands::Attach { target } => {
            let session_id = if let Some(t) = target {
                resolve_session(client, &t).await?
            } else {
                // Attach to most recent session
                let result = client.call("session.list", serde_json::Value::Null).await?;
                let sessions: Vec<serde_json::Value> = serde_json::from_value(result)?;
                sessions.last()
                    .and_then(|s| s["id"].as_str())
                    .map(String::from)
                    .ok_or_else(|| anyhow::anyhow!("No sessions to attach to"))?
            };
            crate::tui::attach(client, &session_id).await
        }

        SessionCommands::Rename { target, name } => {
            let session_id = resolve_session(client, &target).await?;
            let params = serde_json::json!({ "id": session_id, "name": name });
            client.call("session.rename", params).await?;
            println!("Renamed session {} -> {}", target, name);
            Ok(())
        }
    }
}

/// Resolve a session target (name or UUID) to a UUID string.
async fn resolve_session(client: &mut RpcClient, target: &str) -> anyhow::Result<String> {
    // Try parsing as UUID first
    if Uuid::parse_str(target).is_ok() {
        return Ok(target.to_string());
    }

    // Otherwise, look up by name
    let result = client.call("session.list", serde_json::Value::Null).await?;
    let sessions: Vec<serde_json::Value> = serde_json::from_value(result)?;

    sessions.iter()
        .find(|s| s["name"].as_str() == Some(target))
        .and_then(|s| s["id"].as_str())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("Session not found: {}", target))
}

/// Generate a default session name (e.g., "session-0", "session-1", ...).
fn generate_session_name() -> String {
    // In practice, query existing sessions and find the next available index.
    // For now, use a timestamp-based default.
    format!("session-{}", std::process::id())
}
```

### Step 7: Wire the state-owner task to handle session commands

In the daemon's state-owner task (the single writer), handle incoming session commands by dispatching to SessionGraph methods and emitting events.

```rust
// In the state-owner task (crates/shux-core/src/state_owner.rs or similar)
async fn handle_mutation(
    graph: &mut SessionGraph,
    event_tx: &broadcast::Sender<Event>,
    pty_manager: &PtyManager,
    mutation: Mutation,
) {
    match mutation {
        Mutation::Session(cmd, reply_tx) => {
            let result = match cmd {
                SessionCommand::Create { name, env } => {
                    match graph.create_session(name.clone(), env) {
                        Ok((session_id, window_id, pane_id)) => {
                            // Spawn PTY for the default pane
                            if let Err(e) = pty_manager.spawn_for_pane(pane_id).await {
                                tracing::error!("Failed to spawn PTY for new session: {}", e);
                            }

                            // Emit event
                            let _ = event_tx.send(Event::session(SessionEvent::Created {
                                session_id,
                                name,
                                window_id,
                                pane_id,
                            }));

                            SessionResult::Created { session_id, window_id, pane_id }
                        }
                        Err(e) => SessionResult::Error(e),
                    }
                }

                SessionCommand::Ensure { name, env } => {
                    match graph.ensure_session(name.clone(), env) {
                        Ok((session_id, created)) => {
                            if created {
                                let _ = event_tx.send(Event::session(SessionEvent::Created {
                                    session_id,
                                    name,
                                    window_id: graph.sessions[&session_id].active_window,
                                    pane_id: Uuid::nil(), // filled properly in real impl
                                }));
                            }
                            SessionResult::Ensured { session_id, created }
                        }
                        Err(e) => SessionResult::Error(e),
                    }
                }

                SessionCommand::Rename { session_id, new_name } => {
                    let old_name = graph.sessions.get(&session_id)
                        .map(|s| s.name.clone())
                        .unwrap_or_default();
                    match graph.rename_session(session_id, new_name.clone()) {
                        Ok(()) => {
                            let _ = event_tx.send(Event::session(SessionEvent::Renamed {
                                session_id,
                                old_name,
                                new_name,
                            }));
                            SessionResult::Renamed { session_id }
                        }
                        Err(e) => SessionResult::Error(e),
                    }
                }

                SessionCommand::Kill { session_id } => {
                    let name = graph.sessions.get(&session_id)
                        .map(|s| s.name.clone())
                        .unwrap_or_default();
                    match graph.kill_session(session_id) {
                        Ok(killed_panes) => {
                            // Kill PTYs for all removed panes
                            for pane_id in &killed_panes {
                                pty_manager.kill(*pane_id).await;
                            }

                            let _ = event_tx.send(Event::session(SessionEvent::Killed {
                                session_id,
                                name,
                                killed_panes,
                            }));
                            SessionResult::Killed { session_id }
                        }
                        Err(e) => SessionResult::Error(e),
                    }
                }

                SessionCommand::Attach { session_id, client_id } => {
                    // Verify session exists
                    if !graph.sessions.contains_key(&session_id) {
                        SessionResult::Error(SessionError::NotFound(session_id))
                    } else {
                        let _ = event_tx.send(Event::session(SessionEvent::Attached {
                            session_id,
                            client_id,
                        }));
                        SessionResult::Attached { session_id }
                    }
                }

                SessionCommand::Detach { client_id } => {
                    // The session_id needs to be looked up from client state
                    let _ = event_tx.send(Event::session(SessionEvent::Detached {
                        session_id: Uuid::nil(), // Resolved from client registry
                        client_id,
                    }));
                    SessionResult::Detached { client_id }
                }
            };

            // Publish updated snapshot
            // (ArcSwap store happens after all mutations in this batch)

            let _ = reply_tx.send(result);
        }

        // ... other mutation types ...
    }
}
```

### Step 8: Implement session.attach for TUI client

The `session.attach` method is special: it transitions the CLI process from a one-shot RPC caller into a full TUI client that renders the session. This involves entering raw mode, subscribing to events, and starting the render loop.

```rust
// In crates/shux/src/tui.rs (attach function)
pub async fn attach(client: &mut RpcClient, session_id: &str) -> anyhow::Result<()> {
    // 1. Call session.attach via JSON-RPC to register as attached
    let params = serde_json::json!({ "id": session_id });
    client.call("session.attach", params).await?;

    // 2. Subscribe to events for this session
    let watch_params = serde_json::json!({
        "filters": ["session.", "window.", "pane."],
    });
    client.call("events.watch", watch_params).await?;

    // 3. Enter raw mode and start the TUI render loop
    // (Delegates to the shux-ui crate's client implementation)
    let exit_reason = shux_ui::run_attached(client, session_id).await?;

    // 4. On detach or session kill, restore terminal and send detach
    let detach_params = serde_json::json!({});
    let _ = client.call("session.detach", detach_params).await;

    match exit_reason {
        ExitReason::Detach => println!("Detached from session."),
        ExitReason::SessionKilled => println!("Session was killed."),
        ExitReason::ServerGone => println!("Server connection lost."),
    }

    Ok(())
}
```

### Step 9: Write L3 API contract tests

Create integration tests that start a real daemon, exercise each session API endpoint, and verify correct behavior including error cases.

In `crates/shux-rpc/tests/session_api.rs`:

```rust
//! L3 API contract tests for session.* methods.
//!
//! These tests start a real daemon instance (in-process) and exercise
//! the JSON-RPC API through the actual UDS transport.

use shux_test_harness::TestDaemon;

#[tokio::test]
async fn test_session_create() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let result = client
        .call("session.create", serde_json::json!({ "name": "test-session" }))
        .await
        .unwrap();

    assert_eq!(result["session"]["name"], "test-session");
    assert!(result["session"]["id"].is_string());
    assert!(result["window_id"].is_string());
    assert!(result["pane_id"].is_string());
    assert_eq!(result["session"]["window_count"], 1);
}

#[tokio::test]
async fn test_session_create_duplicate_name_fails() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    client
        .call("session.create", serde_json::json!({ "name": "dupe" }))
        .await
        .unwrap();

    let err = client
        .call("session.create", serde_json::json!({ "name": "dupe" }))
        .await
        .unwrap_err();

    assert_eq!(err.code, -32003); // NameConflict
}

#[tokio::test]
async fn test_session_create_empty_name_fails() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let err = client
        .call("session.create", serde_json::json!({ "name": "" }))
        .await
        .unwrap_err();

    assert_eq!(err.code, -32602); // InvalidParams
}

#[tokio::test]
async fn test_session_create_invalid_name_fails() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let err = client
        .call("session.create", serde_json::json!({ "name": "bad name with spaces" }))
        .await
        .unwrap_err();

    assert_eq!(err.code, -32602); // InvalidParams
}

#[tokio::test]
async fn test_session_list_empty() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let result = client
        .call("session.list", serde_json::Value::Null)
        .await
        .unwrap();

    let sessions: Vec<serde_json::Value> = serde_json::from_value(result).unwrap();
    assert!(sessions.is_empty());
}

#[tokio::test]
async fn test_session_list_returns_created_sessions() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    client.call("session.create", serde_json::json!({ "name": "alpha" })).await.unwrap();
    client.call("session.create", serde_json::json!({ "name": "beta" })).await.unwrap();

    let result = client.call("session.list", serde_json::Value::Null).await.unwrap();
    let sessions: Vec<serde_json::Value> = serde_json::from_value(result).unwrap();

    assert_eq!(sessions.len(), 2);
    let names: Vec<&str> = sessions.iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"alpha"));
    assert!(names.contains(&"beta"));
}

#[tokio::test]
async fn test_session_ensure_creates_new() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let result = client
        .call("session.ensure", serde_json::json!({ "name": "ensured" }))
        .await
        .unwrap();

    assert_eq!(result["session"]["name"], "ensured");
    assert_eq!(result["created"], true);
}

#[tokio::test]
async fn test_session_ensure_returns_existing() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let first = client
        .call("session.ensure", serde_json::json!({ "name": "ensured" }))
        .await
        .unwrap();

    let second = client
        .call("session.ensure", serde_json::json!({ "name": "ensured" }))
        .await
        .unwrap();

    assert_eq!(first["session"]["id"], second["session"]["id"]);
    assert_eq!(second["created"], false);
}

#[tokio::test]
async fn test_session_ensure_idempotent_triple_call() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let r1 = client.call("session.ensure", serde_json::json!({ "name": "idem" })).await.unwrap();
    let r2 = client.call("session.ensure", serde_json::json!({ "name": "idem" })).await.unwrap();
    let r3 = client.call("session.ensure", serde_json::json!({ "name": "idem" })).await.unwrap();

    assert_eq!(r1["session"]["id"], r2["session"]["id"]);
    assert_eq!(r2["session"]["id"], r3["session"]["id"]);
    assert_eq!(r1["created"], true);
    assert_eq!(r2["created"], false);
    assert_eq!(r3["created"], false);
}

#[tokio::test]
async fn test_session_rename() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let created = client
        .call("session.create", serde_json::json!({ "name": "old-name" }))
        .await
        .unwrap();

    let session_id = created["session"]["id"].as_str().unwrap();

    let renamed = client
        .call("session.rename", serde_json::json!({ "id": session_id, "name": "new-name" }))
        .await
        .unwrap();

    assert_eq!(renamed["name"], "new-name");
    assert_eq!(renamed["id"], session_id);
}

#[tokio::test]
async fn test_session_rename_conflict_fails() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    client.call("session.create", serde_json::json!({ "name": "first" })).await.unwrap();
    let second = client.call("session.create", serde_json::json!({ "name": "second" })).await.unwrap();
    let second_id = second["session"]["id"].as_str().unwrap();

    let err = client
        .call("session.rename", serde_json::json!({ "id": second_id, "name": "first" }))
        .await
        .unwrap_err();

    assert_eq!(err.code, -32003); // NameConflict
}

#[tokio::test]
async fn test_session_kill() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let created = client
        .call("session.create", serde_json::json!({ "name": "doomed" }))
        .await
        .unwrap();

    let session_id = created["session"]["id"].as_str().unwrap();

    client
        .call("session.kill", serde_json::json!({ "id": session_id }))
        .await
        .unwrap();

    // Verify it's gone
    let list = client.call("session.list", serde_json::Value::Null).await.unwrap();
    let sessions: Vec<serde_json::Value> = serde_json::from_value(list).unwrap();
    assert!(sessions.iter().all(|s| s["id"].as_str().unwrap() != session_id));
}

#[tokio::test]
async fn test_session_kill_nonexistent_fails() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let err = client
        .call("session.kill", serde_json::json!({ "id": "00000000-0000-0000-0000-000000000000" }))
        .await
        .unwrap_err();

    assert_eq!(err.code, -32002); // NotFound
}

#[tokio::test]
async fn test_session_kill_removes_all_children() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let created = client
        .call("session.create", serde_json::json!({ "name": "parent" }))
        .await
        .unwrap();

    let session_id = created["session"]["id"].as_str().unwrap();
    let pane_id = created["pane_id"].as_str().unwrap();

    // The session has 1 window and 1 pane
    // Kill the session — all children should be cleaned up
    client.call("session.kill", serde_json::json!({ "id": session_id })).await.unwrap();

    // Verify panes are gone too (pane.list would return empty or not include this pane)
    let snapshot = client.call("state.snapshot", serde_json::Value::Null).await.unwrap();
    let panes = snapshot["panes"].as_object().unwrap_or(&serde_json::Map::new());
    assert!(!panes.contains_key(pane_id));
}
```

### Step 10: Implement event emission and verify event stream

After each mutation, verify that the correct events are emitted on the broadcast bus. This can be tested by subscribing to events before performing mutations.

```rust
#[tokio::test]
async fn test_session_create_emits_event() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;
    let mut event_client = daemon.connect().await;

    // Subscribe to session events
    event_client
        .call("events.watch", serde_json::json!({ "filters": ["session."] }))
        .await
        .unwrap();

    // Create a session
    client
        .call("session.create", serde_json::json!({ "name": "evented" }))
        .await
        .unwrap();

    // Read the event
    let event = event_client.next_event().await.unwrap();
    assert_eq!(event["type"], "session.created");
    assert_eq!(event["data"]["name"], "evented");
}
```

---

## Verification

### Functional

```bash
# Start daemon and create a session
cargo run -p shux -- new -s test-session --no-attach
# Expected: "Session: test-session (<uuid>)"

# List sessions
cargo run -p shux -- ls
# Expected: "test-session: <uuid> (1 windows) [created ...]"

# Rename session
cargo run -p shux -- rename -s test-session -n renamed-session
# Expected: "Renamed session test-session -> renamed-session"

# Ensure session (already exists)
cargo run -p shux -- new -s renamed-session --ensure --no-attach
# Expected: shows existing session, not a new one

# Ensure session (new)
cargo run -p shux -- new -s brand-new --ensure --no-attach
# Expected: creates new session

# Kill session
cargo run -p shux -- kill -s renamed-session
# Expected: "Killed session: renamed-session"

# Verify killed
cargo run -p shux -- ls
# Expected: only "brand-new" remains

# JSON output
cargo run -p shux -- ls --format json
# Expected: JSON array with session objects
```

### Tests

```bash
# Run unit tests for session logic
cargo nextest run -p shux-core --lib -- session

# Run API contract tests
cargo nextest run -p shux-rpc --test session_api

# Run all workspace tests
cargo nextest run --workspace

# Verify no clippy warnings
cargo clippy --workspace --all-targets -- -D warnings
```

---

## Completion Criteria

- [ ] `session.create` creates a session with a default window and pane, returns session/window/pane IDs
- [ ] `session.list` returns all sessions sorted by creation time with metadata (id, name, window_count, active_window_id, version)
- [ ] `session.rename` renames a session, rejects duplicate names with error code -32003
- [ ] `session.kill` destroys a session and all child windows/panes, kills associated PTYs
- [ ] `session.ensure` creates if not exists, returns existing if it does, idempotent across multiple calls
- [ ] `session.attach` registers a client as attached to a session
- [ ] All mutations flow through the single-writer mpsc channel (no direct graph mutation)
- [ ] Events emitted: session.created, session.renamed, session.killed, session.attached, session.detached
- [ ] Session name validation: non-empty, max 128 chars, alphanumeric + hyphens + underscores + dots
- [ ] CLI commands work: `shux new -s <name>`, `shux ls`, `shux kill -s <name>`, `shux new -s <name> --ensure`
- [ ] CLI supports `--format json` on list commands
- [ ] CLI resolves sessions by name or UUID
- [ ] L3 API contract tests pass: happy path for all 6 methods + error cases (duplicate name, not found, empty name, invalid name)
- [ ] Event emission tests pass: verify events are received by subscribers
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] `cargo nextest run --workspace` passes

---

## Commit Message

```
feat: implement session CRUD via JSON-RPC API and CLI

- Add session.create, session.list, session.ensure, session.rename,
  session.kill, session.attach methods to JSON-RPC server
- Implement CLI commands: shux new, shux ls, shux kill, shux attach,
  shux rename as thin API wrappers
- Session mutations flow through single-writer mpsc channel
- Emit session.created/renamed/killed/attached/detached events
- session.ensure provides idempotent create-if-not-exists for agents
- L3 API contract tests covering happy paths and error cases
```

---

## Session Protocol

1. **Before starting:** Read task 012 completion state to ensure M0 integration gate passed. Verify daemon starts, JSON-RPC server accepts connections, SessionGraph exists with ArcSwap snapshots, event bus is operational. Read `CLAUDE.md` for code conventions.
2. **During:** Implement in step order. After each step, run `cargo check --workspace` to catch compilation errors early. After step 4 (RPC handlers), run `cargo clippy`. After step 9 (tests), run `cargo nextest run --workspace`.
3. **Key patterns to follow:**
   - All mutations go through `mpsc::Sender<Mutation>` to the state-owner task. Never mutate SessionGraph directly from an RPC handler.
   - Return `RpcError` with specific error codes: -32002 (not found), -32003 (name conflict), -32602 (invalid params).
   - Emit events via `broadcast::Sender<Event>` after successful mutations, before sending the reply.
   - Use `ArcSwap::load()` for reads (lock-free snapshots).
4. **After:** Run full verification suite. Update `docs/PROGRESS.md` (mark 013 done, add session log entry). Update `CLAUDE.md` Learnings if anything was discovered. Verify that task 014 (window CRUD) can build on this foundation.
