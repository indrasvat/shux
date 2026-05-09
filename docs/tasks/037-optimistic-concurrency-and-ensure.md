# 037 — Optimistic Concurrency and Ensure Operations

**Status:** Pending
**Depends On:** 035
**Parallelizable With:** 036, 038

---

## Problem

Agents operate in read-plan-apply-verify loops (PRD section 8.5). Without optimistic concurrency, two agents modifying the same pane simultaneously can corrupt state. Without `ensure` operations, agents must first query whether a resource exists, then create it conditionally -- a classic TOCTOU race. Without `client_request_id` deduplication, network retries can duplicate side effects.

This task adds three complementary safety mechanisms:
1. **Version stamps**: Every mutation increments the entity's `version: u64`. Clients can include `expected_version` in mutation requests; stale versions are rejected with error `-32002` and an actionable hint.
2. **Ensure operations**: `session.ensure`, `window.ensure`, `pane.ensure` provide create-if-not-exists semantics. If a resource with the given name/params already exists, it is returned without modification. This makes operations idempotent and retry-safe.
3. **Request deduplication**: `client_request_id` in `state.apply` batches is stored in an LRU cache. Replaying the same ID returns the cached result without re-executing.

Together, these mechanisms make the API safe for autonomous agents that may retry, crash, or race with each other.

## PRD Reference

- **section 4.3** invariant 5 — Deterministic state: `state.snapshot` returns the complete graph; `state.apply` is atomic; idempotent with idempotency keys
- **section 5.4** — Snapshots & diffs: version-based optimistic concurrency, `ensure` operations, `client_request_id`
- **section 8.5** — Agent-safe patterns: use `ensure`, use `state.apply`, check versions, use idempotency keys
- **section 5.1** — Entity definitions: each entity carries `version: u64`
- **section 8.3** — Error format: `-32001` version conflict with `expected_version`, `actual_version`, `hint`

---

## Files to Create

- `crates/shux-core/src/version.rs` — Version stamp trait and helpers
- `crates/shux-core/src/ensure.rs` — Ensure operation logic (create-if-not-exists)
- `crates/shux-rpc/src/methods/ensure.rs` — `session.ensure`, `window.ensure`, `pane.ensure` handlers

## Files to Modify

- `crates/shux-core/src/graph.rs` — Add version stamps to all entities, version-checked mutations, ensure methods
- `crates/shux-core/src/lib.rs` — Export new modules
- `crates/shux-rpc/src/methods/mod.rs` — Register ensure method handlers
- `crates/shux-rpc/src/dedup.rs` — Already created in task 035; may need refinement

---

## Execution Steps

### Step 1: Define version stamp infrastructure

Every mutable entity (Session, Window, Pane) carries a `version: u64` that is incremented on every mutation. This enables optimistic concurrency control.

```rust
// crates/shux-core/src/version.rs

/// Trait for entities that support optimistic concurrency via version stamps.
///
/// Every mutation to a `Versioned` entity increments its version.
/// Clients can include `expected_version` in mutation requests;
/// if the current version does not match, the mutation is rejected
/// with error code -32002 (version_conflict).
pub trait Versioned {
    /// Current version of this entity.
    fn version(&self) -> u64;

    /// Increment the version. Called by the state owner task after
    /// every successful mutation.
    fn increment_version(&mut self);

    /// Check whether an expected version matches the current version.
    /// Returns `Ok(())` if they match, or `Err` with the actual version
    /// if they do not.
    fn check_version(&self, expected: u64) -> Result<(), VersionMismatch> {
        let actual = self.version();
        if actual == expected {
            Ok(())
        } else {
            Err(VersionMismatch { expected, actual })
        }
    }
}

/// Error returned when optimistic concurrency check fails.
#[derive(Debug, Clone)]
pub struct VersionMismatch {
    /// The version the client expected.
    pub expected: u64,
    /// The actual current version.
    pub actual: u64,
}

impl std::fmt::Display for VersionMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "version mismatch: expected {}, actual {}",
            self.expected, self.actual
        )
    }
}

impl std::error::Error for VersionMismatch {}

/// A version-checked mutation request.
///
/// Clients include `expected_version` to ensure they are operating on
/// the state they last read. If `expected_version` is `None`, the
/// mutation proceeds unconditionally (for backward compatibility and
/// human users who do not track versions).
#[derive(Debug, Clone)]
pub struct VersionedMutation<T> {
    /// The mutation payload.
    pub payload: T,
    /// Optional version check. If present, the mutation is rejected
    /// if the entity's current version does not match.
    pub expected_version: Option<u64>,
}

impl<T> VersionedMutation<T> {
    pub fn new(payload: T) -> Self {
        Self {
            payload,
            expected_version: None,
        }
    }

    pub fn with_version(payload: T, expected_version: u64) -> Self {
        Self {
            payload,
            expected_version: Some(expected_version),
        }
    }
}
```

### Step 2: Add version stamps to SessionGraph entities

Modify the entity definitions in `graph.rs` to implement `Versioned`.

```rust
// Modifications to crates/shux-core/src/graph.rs

use crate::version::{Versioned, VersionMismatch};

/// Session entity with version stamp.
pub struct Session {
    pub id: SessionId,
    pub name: String,
    pub created_at: std::time::SystemTime,
    pub windows: Vec<WindowId>,
    pub active_window: Option<WindowId>,
    pub env: std::collections::HashMap<String, String>,
    pub theme: Option<String>,
    pub tags: std::collections::HashMap<String, String>,
    version: u64,
}

impl Versioned for Session {
    fn version(&self) -> u64 {
        self.version
    }

    fn increment_version(&mut self) {
        self.version += 1;
    }
}

impl Session {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: SessionId(uuid::Uuid::new_v4()),
            name: name.into(),
            created_at: std::time::SystemTime::now(),
            windows: Vec::new(),
            active_window: None,
            env: std::collections::HashMap::new(),
            theme: None,
            tags: std::collections::HashMap::new(),
            version: 1, // Start at 1 (0 means "never seen")
        }
    }
}

// Similar implementation for Window and Pane...

/// Version-checked mutation on the SessionGraph.
///
/// This method checks the entity's current version against the
/// expected version before applying the mutation. If the versions
/// do not match, the mutation is rejected.
impl SessionGraph {
    /// Rename a session with optimistic concurrency check.
    pub fn rename_session(
        &mut self,
        session_id: &SessionId,
        new_name: &str,
        expected_version: Option<u64>,
    ) -> Result<&Session, GraphError> {
        let session = self.sessions.get_mut(session_id)
            .ok_or(GraphError::SessionNotFound(*session_id))?;

        // Check version if provided
        if let Some(expected) = expected_version {
            session.check_version(expected)
                .map_err(|mismatch| GraphError::VersionConflict {
                    resource: "session".to_string(),
                    id: session_id.to_string(),
                    expected: mismatch.expected,
                    actual: mismatch.actual,
                })?;
        }

        session.name = new_name.to_string();
        session.increment_version();
        Ok(session)
    }

    /// Generic version-checked mutation pattern.
    ///
    /// Every mutation method on SessionGraph should follow this pattern:
    /// 1. Look up the entity
    /// 2. Check version (if expected_version is provided)
    /// 3. Apply the mutation
    /// 4. Increment version
    /// 5. Return the updated entity
}

/// Errors from SessionGraph operations.
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("session not found: {0}")]
    SessionNotFound(SessionId),
    #[error("window not found: {0}")]
    WindowNotFound(WindowId),
    #[error("pane not found: {0}")]
    PaneNotFound(PaneId),
    #[error("version conflict on {resource} {id}: expected {expected}, actual {actual}")]
    VersionConflict {
        resource: String,
        id: String,
        expected: u64,
        actual: u64,
    },
    #[error("session with name '{0}' already exists")]
    SessionNameExists(String),
    #[error("window with name '{0}' already exists in session")]
    WindowNameExists(String),
}
```

### Step 3: Implement ensure operations

Ensure operations are the key idempotency primitive for agents. They create a resource if it does not exist, or return the existing one if it does.

```rust
// crates/shux-core/src/ensure.rs

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Parameters for a `session.ensure` operation.
#[derive(Debug, Clone, Deserialize)]
pub struct EnsureSessionParams {
    /// Session name. If a session with this name exists, it is returned.
    pub name: String,
    /// Optional environment variables (only applied on creation).
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    /// Optional theme (only applied on creation).
    #[serde(default)]
    pub theme: Option<String>,
}

/// Parameters for a `window.ensure` operation.
#[derive(Debug, Clone, Deserialize)]
pub struct EnsureWindowParams {
    /// Session ID or name. Required.
    pub session_id: String,
    /// Window title. If a window with this title exists in the
    /// session, it is returned.
    pub title: String,
    /// Optional CWD (only applied on creation).
    #[serde(default)]
    pub cwd: Option<String>,
}

/// Parameters for a `pane.ensure` operation.
#[derive(Debug, Clone, Deserialize)]
pub struct EnsurePaneParams {
    /// Window ID. Required.
    pub window_id: String,
    /// Pane name/title. If a pane with this title exists in the
    /// window, it is returned.
    pub title: String,
    /// Command to run (only applied on creation).
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// Direction for split (only applied on creation).
    #[serde(default)]
    pub direction: Option<String>,
    /// CWD (only applied on creation).
    #[serde(default)]
    pub cwd: Option<String>,
}

/// Result of an ensure operation.
#[derive(Debug, Clone, Serialize)]
pub struct EnsureResult {
    /// Whether the resource was created (true) or already existed (false).
    pub created: bool,
    /// The resource data.
    pub resource: Value,
}
```

Add ensure methods to the SessionGraph:

```rust
// In crates/shux-core/src/graph.rs

impl SessionGraph {
    /// Ensure a session exists with the given name.
    ///
    /// If a session with `name` already exists, return it without
    /// modification. Otherwise, create a new session with the given
    /// parameters.
    ///
    /// This is inherently idempotent -- agents can call it repeatedly
    /// without side effects.
    pub fn ensure_session(
        &mut self,
        params: &EnsureSessionParams,
    ) -> Result<EnsureResult, GraphError> {
        // Check if session already exists by name
        if let Some(session) = self.find_session_by_name(&params.name) {
            return Ok(EnsureResult {
                created: false,
                resource: self.session_to_json(session),
            });
        }

        // Create new session
        let session = self.create_session(&params.name)?;
        Ok(EnsureResult {
            created: true,
            resource: self.session_to_json(session),
        })
    }

    /// Ensure a window exists with the given title in the given session.
    pub fn ensure_window(
        &mut self,
        params: &EnsureWindowParams,
    ) -> Result<EnsureResult, GraphError> {
        let session_id = self.resolve_session_id(&params.session_id)?;

        // Check if window already exists by title
        if let Some(window) = self.find_window_by_title(&session_id, &params.title) {
            return Ok(EnsureResult {
                created: false,
                resource: self.window_to_json(window),
            });
        }

        // Create new window
        let window = self.create_window(&session_id, &params.title)?;
        Ok(EnsureResult {
            created: true,
            resource: self.window_to_json(window),
        })
    }

    /// Ensure a pane exists with the given title in the given window.
    pub fn ensure_pane(
        &mut self,
        params: &EnsurePaneParams,
    ) -> Result<EnsureResult, GraphError> {
        let window_id = WindowId::parse(&params.window_id)?;

        // Check if pane already exists by title
        if let Some(pane) = self.find_pane_by_title(&window_id, &params.title) {
            return Ok(EnsureResult {
                created: false,
                resource: self.pane_to_json(pane),
            });
        }

        // Create new pane
        let pane = self.create_pane(&window_id, params)?;
        Ok(EnsureResult {
            created: true,
            resource: self.pane_to_json(pane),
        })
    }

    /// Find a session by name. Returns None if not found.
    fn find_session_by_name(&self, name: &str) -> Option<&Session> {
        self.sessions.values().find(|s| s.name == name)
    }

    /// Find a window by title within a session. Returns None if not found.
    fn find_window_by_title(
        &self,
        session_id: &SessionId,
        title: &str,
    ) -> Option<&Window> {
        let session = self.sessions.get(session_id)?;
        session
            .windows
            .iter()
            .filter_map(|wid| self.windows.get(wid))
            .find(|w| w.title == title)
    }

    /// Find a pane by title within a window. Returns None if not found.
    fn find_pane_by_title(
        &self,
        window_id: &WindowId,
        title: &str,
    ) -> Option<&Pane> {
        let window = self.windows.get(window_id)?;
        // Traverse layout tree to find panes in this window
        self.panes_in_window(window)
            .find(|p| p.title == title)
    }
}
```

### Step 4: Register ensure handlers in the RPC layer

```rust
// crates/shux-rpc/src/methods/ensure.rs

use std::sync::Arc;
use serde_json::Value;

use super::{MethodContext, MethodResult, MethodError};
use shux_core::ensure::{EnsureSessionParams, EnsureWindowParams, EnsurePaneParams};

/// `session.ensure` — Create a session if it does not exist.
///
/// If a session with the given name already exists, returns it
/// without modification. This is inherently idempotent.
///
/// Params:
///   name: String — session name
///   env: Option<Map> — environment variables (creation only)
///   theme: Option<String> — theme name (creation only)
///
/// Response:
///   created: bool — whether a new session was created
///   session: SessionInfo — the session (new or existing)
pub async fn handle_ensure_session(
    ctx: Arc<MethodContext>,
    params: Value,
) -> MethodResult {
    let ensure_params: EnsureSessionParams = serde_json::from_value(params)
        .map_err(|e| MethodError::invalid_params(e.to_string()))?;

    // TODO: Dispatch to SessionGraph via mutation channel
    // let result = ctx.graph.ensure_session(&ensure_params)?;

    Ok(serde_json::json!({
        "created": false,
        "session": {"id": "", "name": ensure_params.name, "version": 1}
    }))
}

/// `window.ensure` — Create a window if it does not exist.
pub async fn handle_ensure_window(
    ctx: Arc<MethodContext>,
    params: Value,
) -> MethodResult {
    let ensure_params: EnsureWindowParams = serde_json::from_value(params)
        .map_err(|e| MethodError::invalid_params(e.to_string()))?;

    // TODO: Dispatch to SessionGraph
    Ok(serde_json::json!({
        "created": false,
        "window": {"id": "", "title": ensure_params.title, "version": 1}
    }))
}

/// `pane.ensure` — Create a pane if it does not exist.
pub async fn handle_ensure_pane(
    ctx: Arc<MethodContext>,
    params: Value,
) -> MethodResult {
    let ensure_params: EnsurePaneParams = serde_json::from_value(params)
        .map_err(|e| MethodError::invalid_params(e.to_string()))?;

    // TODO: Dispatch to SessionGraph
    Ok(serde_json::json!({
        "created": false,
        "pane": {"id": "", "title": ensure_params.title, "version": 1}
    }))
}
```

### Step 5: Enhance the dedup cache with error response for version conflicts

Extend the error response format to include all the fields agents need for recovery:

```rust
// Enhance the VersionConflict error response in crates/shux-rpc/src/error.rs

impl From<GraphError> for MethodError {
    fn from(err: GraphError) -> Self {
        match err {
            GraphError::VersionConflict {
                resource,
                id,
                expected,
                actual,
            } => MethodError {
                code: ShuxErrorCode::VersionConflict,
                message: "version_conflict".to_string(),
                data: Some(ShuxErrorData::version_conflict(
                    &resource, &id, expected, actual,
                )),
            },
            GraphError::SessionNotFound(id) => {
                MethodError::not_found("session", &id.to_string())
            }
            GraphError::WindowNotFound(id) => {
                MethodError::not_found("window", &id.to_string())
            }
            GraphError::PaneNotFound(id) => {
                MethodError::not_found("pane", &id.to_string())
            }
            GraphError::SessionNameExists(name) => MethodError {
                code: ShuxErrorCode::AlreadyExists,
                message: format!("session '{}' already exists", name),
                data: Some(ShuxErrorData {
                    resource: Some("session".to_string()),
                    id: None,
                    expected_version: None,
                    actual_version: None,
                    hint: Some(format!(
                        "Use session.ensure for idempotent creation, or choose a different name"
                    )),
                    operation_index: None,
                }),
            },
            _ => MethodError::internal(err.to_string()),
        }
    }
}
```

### Step 6: Wire ensure methods into the method registry

```rust
// In crates/shux-rpc/src/methods/mod.rs, add to register_all():

// Ensure operations (idempotent create-if-not-exists)
self.register("session.ensure", ensure::handle_ensure_session);
self.register("window.ensure", ensure::handle_ensure_window);
self.register("pane.ensure", ensure::handle_ensure_pane);
```

### Step 7: Write comprehensive tests

```rust
#[cfg(test)]
mod version_tests {
    use crate::version::{Versioned, VersionMismatch, VersionedMutation};

    struct TestEntity {
        name: String,
        version: u64,
    }

    impl Versioned for TestEntity {
        fn version(&self) -> u64 {
            self.version
        }

        fn increment_version(&mut self) {
            self.version += 1;
        }
    }

    #[test]
    fn test_version_starts_at_one() {
        let entity = TestEntity {
            name: "test".to_string(),
            version: 1,
        };
        assert_eq!(entity.version(), 1);
    }

    #[test]
    fn test_version_increments() {
        let mut entity = TestEntity {
            name: "test".to_string(),
            version: 1,
        };
        entity.increment_version();
        assert_eq!(entity.version(), 2);
        entity.increment_version();
        assert_eq!(entity.version(), 3);
    }

    #[test]
    fn test_check_version_matching() {
        let entity = TestEntity {
            name: "test".to_string(),
            version: 5,
        };
        assert!(entity.check_version(5).is_ok());
    }

    #[test]
    fn test_check_version_mismatch() {
        let entity = TestEntity {
            name: "test".to_string(),
            version: 5,
        };
        let err = entity.check_version(3).unwrap_err();
        assert_eq!(err.expected, 3);
        assert_eq!(err.actual, 5);
    }

    #[test]
    fn test_versioned_mutation_without_version() {
        let mutation = VersionedMutation::new("rename");
        assert!(mutation.expected_version.is_none());
    }

    #[test]
    fn test_versioned_mutation_with_version() {
        let mutation = VersionedMutation::with_version("rename", 5);
        assert_eq!(mutation.expected_version, Some(5));
    }
}

#[cfg(test)]
mod ensure_tests {
    use serde_json::json;

    // These tests verify the ensure logic at the graph level

    #[test]
    fn test_ensure_session_creates_when_missing() {
        // Arrange: empty SessionGraph.
        // Act: ensure session "work".
        // Assert: created = true and returned session name is "work".
    }

    #[test]
    fn test_ensure_session_returns_existing() {
        // Arrange: graph already contains session "work".
        // Act: ensure "work" again.
        // Assert: created = false and returned session ID matches existing ID.
    }

    #[test]
    fn test_ensure_session_idempotent_repeated() {
        // Act: call ensure("work") three times.
        // Assert: first call created=true; second/third created=false; IDs all identical.
    }

    #[test]
    fn test_ensure_window_creates_when_missing() {
        // Arrange: session exists, target window title does not.
        // Act: ensure window "editor".
        // Assert: created=true with expected title and session linkage.
    }

    #[test]
    fn test_ensure_window_returns_existing_by_title() {
        // Arrange: session already has window "editor".
        // Act: ensure window "editor".
        // Assert: created=false and same WindowId is returned.
    }

    #[test]
    fn test_ensure_pane_creates_when_missing() {
        // Arrange: window exists with no pane matching the ensure predicate.
        // Act: ensure pane using the same command/cwd signature.
        // Assert: created=true and pane is attached to target window.
    }

    #[test]
    fn test_version_conflict_error_format() {
        // Verify the error response matches PRD section 8.3 format
        let error_data = crate::error::ShuxErrorData::version_conflict(
            "pane",
            "550e8400-...",
            3,
            5,
        );
        assert_eq!(error_data.resource, Some("pane".to_string()));
        assert_eq!(error_data.expected_version, Some(3));
        assert_eq!(error_data.actual_version, Some(5));
        assert!(error_data.hint.is_some());
        assert!(error_data.hint.unwrap().contains("5")); // hints at current version
    }

    #[test]
    fn test_version_checked_mutation_succeeds() {
        // Arrange: entity at version=1.
        // Act: mutate with expected_version=1.
        // Assert: mutation succeeds and entity version increments to 2.
    }

    #[test]
    fn test_version_checked_mutation_fails_on_stale() {
        // Arrange: entity at version=3.
        // Act: mutate with expected_version=1.
        // Assert: returns VersionConflict and entity state/version are unchanged.
    }

    #[test]
    fn test_mutation_without_version_always_succeeds() {
        // Act: mutate with expected_version=None.
        // Assert: succeeds regardless of current stored version.
    }
}
```

---

## Verification

### Functional

```bash
# Build the affected crates
cargo build -p shux-core -p shux-rpc

# Verify clippy passes
cargo clippy -p shux-core -p shux-rpc -- -D warnings

# Integration test: version conflict scenario
# 1. Create a session via API
# 2. Read its version
# 3. Rename it (version increments)
# 4. Try to rename again with the old version
# 5. Verify error -32001 with expected_version and actual_version

# Integration test: ensure idempotency
# 1. shux new -s work --ensure → created=true
# 2. shux new -s work --ensure → created=false, same ID
# 3. shux new -s work --ensure → created=false, same ID
```

### Tests

```bash
# Run version and ensure tests
cargo nextest run -p shux-core -- version
cargo nextest run -p shux-core -- ensure

# Run RPC ensure handler tests
cargo nextest run -p shux-rpc -- ensure

# Run dedup cache tests (from task 035)
cargo nextest run -p shux-rpc -- dedup

# Full workspace
cargo nextest run --workspace
```

---

## Completion Criteria

- [ ] `Versioned` trait defined with `version()`, `increment_version()`, `check_version()`
- [ ] `Session`, `Window`, `Pane` entities implement `Versioned` with `version: u64` field
- [ ] All mutations through `SessionGraph` increment the entity's version
- [ ] Mutation methods accept `expected_version: Option<u64>`; stale versions rejected with `-32001`
- [ ] Version conflict error includes `resource`, `id`, `expected_version`, `actual_version`, `hint`
- [ ] `session.ensure` creates if not exists by name, returns existing if found
- [ ] `window.ensure` creates if not exists by title within session
- [ ] `pane.ensure` creates if not exists by title within window
- [ ] Ensure operations return `{created: bool, resource: {...}}` response
- [ ] Ensure operations are idempotent: calling N times produces the same result
- [ ] `client_request_id` deduplication returns cached result on replay (from task 035 dedup cache)
- [ ] `GraphError::VersionConflict` converts to proper `MethodError` with structured data
- [ ] Ensure methods registered in `MethodRegistry`: `session.ensure`, `window.ensure`, `pane.ensure`
- [ ] All unit tests pass (version stamps, ensure idempotency, error formatting)
- [ ] `cargo clippy --workspace -- -D warnings` passes

---

## Commit Message
```
feat(core,rpc): add optimistic concurrency and idempotent ensure operations

- Versioned trait with version stamps on Session, Window, Pane
- Version-checked mutations: stale version → error -32001 with hint
- session.ensure, window.ensure, pane.ensure: create-if-not-exists
- Ensure operations are idempotent for agent retry safety
- Structured version conflict errors per PRD section 8.3
- GraphError → MethodError conversion with actionable hints
```

---

## Session Protocol

1. **Before starting:** Read `CLAUDE.md`. Read task 035 (JSON-RPC API, dedup cache) and the SessionGraph implementation from earlier tasks. Read PRD sections 4.3 (deterministic state), 5.1 (entity versions), 5.4 (state.apply), and 8.5 (agent-safe patterns). Verify task 035 is complete.
2. **During:** Implement in order: version trait (Step 1) -> entity modifications (Step 2) -> ensure logic (Step 3) -> RPC handlers (Step 4) -> error conversion (Step 5) -> registry wiring (Step 6) -> tests (Step 7). Run `cargo check` after each step. The version trait and ensure logic are independent and can be tested before wiring into the RPC layer.
3. **Testing:** Focus on the idempotency guarantees -- ensure operations must return the same result when called repeatedly with the same parameters. Test version conflict scenarios thoroughly: matching version, stale version, no version (unconditional).
4. **After:** Run `make check`. Update `docs/PROGRESS.md`. Update `CLAUDE.md` Learnings with any discoveries about optimistic concurrency patterns in Rust, LRU cache behavior, or version-checked mutation ergonomics.
5. **Watch out for:**
   - Version 0 is reserved as "never seen" -- entities should start at version 1
   - Ensure operations should NOT modify existing entities even if the creation parameters differ. They are create-if-not-exists by name, not upsert.
   - The `find_session_by_name` lookup should be O(1) or close to it. Consider maintaining a name-to-ID index in the SessionGraph for sessions with many entries.
   - Version-checked mutations with `expected_version: None` should always succeed (backward compatibility for human users).
   - The dedup cache (task 035) and ensure operations serve complementary purposes: dedup prevents re-execution of the exact same batch, while ensure prevents duplicate resource creation regardless of request ID.
