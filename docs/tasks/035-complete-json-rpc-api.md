# 035 — Complete JSON-RPC API Surface

**Status:** Pending
**Depends On:** 034
**Parallelizable With:** 038

---

## Problem

The JSON-RPC API is the primary integration surface for shux. Every CLI command is a thin wrapper over a JSON-RPC call, every agent interacts through JSON-RPC, and every plugin's external interface routes through it. Task 034 established the JSON-RPC server skeleton and a handful of foundational methods (`system.version`, `system.health`, `session.list`, `session.create`, etc.). This task completes the remaining ~40 methods specified in PRD section 8.2 to achieve full API coverage. Without this, agents cannot perform batch operations, copy mode is inaccessible from the API, theme/config/keybinding management lacks programmatic access, plugin lifecycle cannot be driven remotely, and observability tooling has no API backing.

The most complex piece is `state.apply` -- the atomic batch operation that enables agents to create entire workspaces in a single transactional call. Its `$N.field` back-reference syntax, all-or-nothing rollback semantics, and `client_request_id` deduplication are all critical for the agent-safe patterns described in PRD section 8.5.

## PRD Reference

- **section 8.2** — Complete JSON-RPC method list (`state.*`, `copy.*`, `theme.*`, `config.*`, `keybinding.*`, `plugin.*`, `events.history`, `log.*`, `metrics.get`, `diagnose.run`, `admin.*`)
- **section 8.3** — Request/response format, error codes (`-32001` version conflict, `-32002` stale version, standard JSON-RPC errors)
- **section 5.4** — `state.apply` delta schema: sequential operations, `$N.field` back-references, all-or-nothing rollback, `client_request_id` deduplication
- **section 8.5** — Agent-safe patterns: prefer `run_command`, use `ensure`, use `state.apply`, subscribe to events, check versions, use idempotency keys
- **section 8.6** — CLI-to-API mapping (`--format json|text` on all commands)

---

## Files to Create

- `crates/shux-rpc/src/methods/state.rs` — `state.snapshot`, `state.apply`
- `crates/shux-rpc/src/methods/copy.rs` — `copy.enter`, `copy.search`, `copy.select`, `copy.to_clipboard`
- `crates/shux-rpc/src/methods/theme.rs` — `theme.list`, `theme.get`, `theme.set`
- `crates/shux-rpc/src/methods/config.rs` — `config.get`, `config.set`, `config.validate`, `config.explain`
- `crates/shux-rpc/src/methods/keybinding.rs` — `keybinding.list`, `keybinding.set`, `keybinding.reset`
- `crates/shux-rpc/src/methods/plugin.rs` — `plugin.list`, `plugin.enable`, `plugin.disable`, `plugin.reload`, `plugin.inspect`
- `crates/shux-rpc/src/methods/events.rs` — `events.history`
- `crates/shux-rpc/src/methods/log.rs` — `log.set_level`, `log.tail`
- `crates/shux-rpc/src/methods/metrics.rs` — `metrics.get`
- `crates/shux-rpc/src/methods/diagnose.rs` — `diagnose.run`
- `crates/shux-rpc/src/methods/admin.rs` — `admin.shutdown`, `admin.gc`
- `crates/shux-rpc/src/methods/mod.rs` — Method dispatch registry
- `crates/shux-rpc/src/apply.rs` — `state.apply` transaction engine (back-references, rollback, dedup)
- `crates/shux-rpc/src/dedup.rs` — `client_request_id` deduplication cache (LRU, 10000 entries)
- `crates/shux-rpc/src/error.rs` — Structured error codes per PRD section 8.3

## Files to Modify

- `crates/shux-rpc/src/lib.rs` — Wire new method modules into dispatcher
- `crates/shux-rpc/Cargo.toml` — Add dependencies: `lru`, `chrono` (or `time`), `base64`
- `crates/shux-core/src/graph.rs` — Add snapshot pagination, version accessors needed by API methods

---

## Execution Steps

### Step 1: Define the error code system

All API methods must return structured errors per PRD section 8.3. Define a canonical error enum that maps to JSON-RPC error codes.

```rust
// crates/shux-rpc/src/error.rs

use serde::{Deserialize, Serialize};

/// JSON-RPC error codes used by shux.
///
/// Standard JSON-RPC codes (-32700 to -32600) are handled by the framing layer.
/// Application-specific codes use the -32000 to -32099 range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShuxErrorCode {
    /// Version conflict: mutation attempted with stale version stamp.
    VersionConflict = -32001,
    /// Frame too large (exceeds 16 MB max payload).
    FrameTooLarge = -32002,
    /// Resource not found (session, window, pane, plugin, etc.).
    NotFound = -32003,
    /// Permission denied (plugin lacks required capability).
    PermissionDenied = -32004,
    /// Invalid parameters (beyond JSON-RPC parse errors).
    InvalidParams = -32005,
    /// Duplicate request (client_request_id already processed).
    DuplicateRequest = -32006,
    /// Transaction failed (state.apply rollback).
    TransactionFailed = -32007,
    /// Plugin error (plugin returned an error or timed out).
    PluginError = -32008,
    /// Internal error (unexpected daemon failure).
    InternalError = -32009,
    /// Resource already exists (e.g., session name collision).
    AlreadyExists = -32010,
}

/// Structured error data included in JSON-RPC error responses.
/// Provides actionable hints per PRD section 8.3.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShuxErrorData {
    /// The type of resource involved (e.g., "pane", "session").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    /// The ID of the resource involved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The version the client expected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<u64>,
    /// The actual current version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual_version: Option<u64>,
    /// Actionable hint for the client.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Index of the failed operation within a state.apply batch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_index: Option<usize>,
}

impl ShuxErrorData {
    pub fn version_conflict(
        resource: &str,
        id: &str,
        expected: u64,
        actual: u64,
    ) -> Self {
        Self {
            resource: Some(resource.to_string()),
            id: Some(id.to_string()),
            expected_version: Some(expected),
            actual_version: Some(actual),
            hint: Some(format!(
                "Re-read the {} state and retry with current version {}",
                resource, actual
            )),
            operation_index: None,
        }
    }

    pub fn not_found(resource: &str, id: &str) -> Self {
        Self {
            resource: Some(resource.to_string()),
            id: Some(id.to_string()),
            expected_version: None,
            actual_version: None,
            hint: Some(format!("No {} with id '{}' exists", resource, id)),
            operation_index: None,
        }
    }
}
```

### Step 2: Implement the `client_request_id` deduplication cache

Agents retry operations with idempotency keys. The daemon must detect duplicates and return the cached result rather than re-executing.

```rust
// crates/shux-rpc/src/dedup.rs

use std::sync::Mutex;
use lru::LruCache;
use std::num::NonZeroUsize;
use serde_json::Value;

/// Maximum number of recent request IDs to cache.
const DEDUP_CACHE_SIZE: usize = 10_000;

/// Deduplication cache for `client_request_id` values.
///
/// When a `state.apply` batch includes a `client_request_id`, the daemon
/// stores the result in this cache. If the same ID is seen again (agent
/// retry), the cached result is returned without re-executing.
///
/// Uses an LRU eviction policy: oldest entries are dropped when the cache
/// reaches capacity.
pub struct DedupCache {
    inner: Mutex<LruCache<String, CachedResult>>,
}

#[derive(Debug, Clone)]
pub struct CachedResult {
    /// The JSON-RPC result value that was returned for this request.
    pub result: Value,
    /// When this result was cached (monotonic instant for TTL if needed).
    pub cached_at: std::time::Instant,
}

impl DedupCache {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(LruCache::new(
                NonZeroUsize::new(DEDUP_CACHE_SIZE).expect("cache size > 0"),
            )),
        }
    }

    /// Check if a request ID has been seen before. Returns the cached
    /// result if so.
    pub fn get(&self, request_id: &str) -> Option<CachedResult> {
        let mut cache = self.inner.lock().expect("dedup cache lock poisoned");
        cache.get(request_id).cloned()
    }

    /// Store a result for a request ID.
    pub fn insert(&self, request_id: String, result: Value) {
        let mut cache = self.inner.lock().expect("dedup cache lock poisoned");
        cache.put(
            request_id,
            CachedResult {
                result,
                cached_at: std::time::Instant::now(),
            },
        );
    }

    /// Number of entries currently cached.
    pub fn len(&self) -> usize {
        let cache = self.inner.lock().expect("dedup cache lock poisoned");
        cache.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for DedupCache {
    fn default() -> Self {
        Self::new()
    }
}
```

### Step 3: Implement the `state.apply` transaction engine

This is the most complex piece. It processes a batch of operations sequentially, resolves `$N.field` back-references, and rolls back all changes if any operation fails.

```rust
// crates/shux-rpc/src/apply.rs

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A single operation within a `state.apply` batch.
#[derive(Debug, Clone, Deserialize)]
pub struct ApplyOperation {
    /// The operation name (e.g., "session.create", "pane.split").
    pub op: String,
    /// The parameters for this operation. May contain `$N.field`
    /// back-references to results of earlier operations.
    pub params: Value,
}

/// The full `state.apply` request parameters.
#[derive(Debug, Clone, Deserialize)]
pub struct ApplyRequest {
    /// Optional idempotency key for deduplication.
    #[serde(default)]
    pub client_request_id: Option<String>,
    /// Ordered list of operations to execute atomically.
    pub operations: Vec<ApplyOperation>,
}

/// Result of a single operation within a batch.
#[derive(Debug, Clone, Serialize)]
pub struct OperationResult {
    /// Index of this operation in the batch.
    pub index: usize,
    /// The operation that was executed.
    pub op: String,
    /// The result data from executing this operation.
    pub result: Value,
}

/// Result of the entire `state.apply` batch.
#[derive(Debug, Clone, Serialize)]
pub struct ApplyResult {
    /// Results from each operation in order.
    pub results: Vec<OperationResult>,
    /// Whether this was a cached replay (duplicate client_request_id).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub replayed: bool,
}

/// Resolve `$N.field` back-references in a JSON value.
///
/// Back-references have the form `$N.field` where N is a zero-based
/// index into the results of previous operations in the batch, and
/// field is a dot-separated path into that result's JSON value.
///
/// Examples:
/// - `"$0.id"` → the `id` field from operation 0's result
/// - `"$1.active_pane_id"` → the `active_pane_id` field from operation 1's result
///
/// Back-references can appear as string values anywhere in the params
/// object (including nested arrays and objects).
pub fn resolve_back_references(
    params: &Value,
    previous_results: &[Value],
) -> Result<Value, BackRefError> {
    match params {
        Value::String(s) if s.starts_with('$') => {
            resolve_single_back_ref(s, previous_results)
        }
        Value::Object(map) => {
            let mut resolved = serde_json::Map::new();
            for (key, val) in map {
                resolved.insert(key.clone(), resolve_back_references(val, previous_results)?);
            }
            Ok(Value::Object(resolved))
        }
        Value::Array(arr) => {
            let resolved: Result<Vec<Value>, BackRefError> = arr
                .iter()
                .map(|v| resolve_back_references(v, previous_results))
                .collect();
            Ok(Value::Array(resolved?))
        }
        other => Ok(other.clone()),
    }
}

/// Parse and resolve a single `$N.field` reference.
fn resolve_single_back_ref(
    reference: &str,
    previous_results: &[Value],
) -> Result<Value, BackRefError> {
    // Parse "$N.field.path"
    let rest = reference
        .strip_prefix('$')
        .ok_or_else(|| BackRefError::InvalidSyntax(reference.to_string()))?;

    let dot_pos = rest
        .find('.')
        .ok_or_else(|| BackRefError::InvalidSyntax(reference.to_string()))?;

    let index_str = &rest[..dot_pos];
    let field_path = &rest[dot_pos + 1..];

    let index: usize = index_str
        .parse()
        .map_err(|_| BackRefError::InvalidIndex(reference.to_string()))?;

    if index >= previous_results.len() {
        return Err(BackRefError::IndexOutOfRange {
            reference: reference.to_string(),
            index,
            available: previous_results.len(),
        });
    }

    // Navigate the dot-separated field path
    let mut current = &previous_results[index];
    for segment in field_path.split('.') {
        match current {
            Value::Object(map) => {
                current = map.get(segment).ok_or_else(|| BackRefError::FieldNotFound {
                    reference: reference.to_string(),
                    field: segment.to_string(),
                })?;
            }
            _ => {
                return Err(BackRefError::FieldNotFound {
                    reference: reference.to_string(),
                    field: segment.to_string(),
                });
            }
        }
    }

    Ok(current.clone())
}

#[derive(Debug, thiserror::Error)]
pub enum BackRefError {
    #[error("invalid back-reference syntax: {0}")]
    InvalidSyntax(String),
    #[error("invalid index in back-reference: {0}")]
    InvalidIndex(String),
    #[error("back-reference {reference} index {index} out of range (only {available} results available)")]
    IndexOutOfRange {
        reference: String,
        index: usize,
        available: usize,
    },
    #[error("back-reference {reference} field '{field}' not found in result")]
    FieldNotFound { reference: String, field: String },
}
```

### Step 4: Create the method dispatch registry

Build a central dispatcher that routes JSON-RPC method names to handler functions.

```rust
// crates/shux-rpc/src/methods/mod.rs

pub mod admin;
pub mod config;
pub mod copy;
pub mod diagnose;
pub mod events;
pub mod keybinding;
pub mod log;
pub mod metrics;
pub mod plugin;
pub mod state;
pub mod theme;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;

use crate::error::{ShuxErrorCode, ShuxErrorData};

/// Shared context passed to every method handler.
/// Contains references to the daemon's subsystems.
pub struct MethodContext {
    // These fields will be populated as subsystems are built:
    // pub graph: Arc<SessionGraphHandle>,
    // pub event_bus: Arc<EventBus>,
    // pub plugin_host: Arc<PluginHost>,
    // pub config: Arc<ConfigManager>,
    // pub theme_engine: Arc<ThemeEngine>,
    // pub dedup_cache: Arc<DedupCache>,
    // pub daemon_cmd_tx: mpsc::Sender<DaemonCommand>,
    // pub metrics: Arc<MetricsCollector>,
    // pub shutdown_tokens: ShutdownTokens,
}

/// A JSON-RPC method handler function.
pub type MethodHandler = Box<
    dyn Fn(Arc<MethodContext>, Value) -> Pin<Box<dyn Future<Output = MethodResult> + Send>>
        + Send
        + Sync,
>;

/// Result type for method handlers.
pub type MethodResult = Result<Value, MethodError>;

/// An error returned from a method handler.
#[derive(Debug)]
pub struct MethodError {
    pub code: ShuxErrorCode,
    pub message: String,
    pub data: Option<ShuxErrorData>,
}

impl MethodError {
    pub fn not_found(resource: &str, id: &str) -> Self {
        Self {
            code: ShuxErrorCode::NotFound,
            message: format!("{} not found: {}", resource, id),
            data: Some(ShuxErrorData::not_found(resource, id)),
        }
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: ShuxErrorCode::InvalidParams,
            message: message.into(),
            data: None,
        }
    }

    pub fn version_conflict(
        resource: &str,
        id: &str,
        expected: u64,
        actual: u64,
    ) -> Self {
        Self {
            code: ShuxErrorCode::VersionConflict,
            message: "version_conflict".to_string(),
            data: Some(ShuxErrorData::version_conflict(resource, id, expected, actual)),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: ShuxErrorCode::InternalError,
            message: message.into(),
            data: None,
        }
    }
}

/// Registry of all JSON-RPC methods.
///
/// Method names follow the `<resource>.<action>` convention from PRD section 8.2.
pub struct MethodRegistry {
    handlers: HashMap<String, MethodHandler>,
}

impl MethodRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            handlers: HashMap::new(),
        };
        registry.register_all();
        registry
    }

    /// Look up a handler by method name.
    pub fn get(&self, method: &str) -> Option<&MethodHandler> {
        self.handlers.get(method)
    }

    /// List all registered method names (sorted).
    pub fn method_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.handlers.keys().map(|s| s.as_str()).collect();
        names.sort();
        names
    }

    /// Register all method handlers.
    fn register_all(&mut self) {
        // State methods
        self.register("state.snapshot", state::handle_snapshot);
        self.register("state.apply", state::handle_apply);

        // Copy mode methods
        self.register("copy.enter", copy::handle_enter);
        self.register("copy.search", copy::handle_search);
        self.register("copy.select", copy::handle_select);
        self.register("copy.to_clipboard", copy::handle_to_clipboard);

        // Theme methods
        self.register("theme.list", theme::handle_list);
        self.register("theme.get", theme::handle_get);
        self.register("theme.set", theme::handle_set);

        // Config methods
        self.register("config.get", config::handle_get);
        self.register("config.set", config::handle_set);
        self.register("config.validate", config::handle_validate);
        self.register("config.explain", config::handle_explain);

        // Keybinding methods
        self.register("keybinding.list", keybinding::handle_list);
        self.register("keybinding.set", keybinding::handle_set);
        self.register("keybinding.reset", keybinding::handle_reset);

        // Plugin methods
        self.register("plugin.list", plugin::handle_list);
        self.register("plugin.enable", plugin::handle_enable);
        self.register("plugin.disable", plugin::handle_disable);
        self.register("plugin.reload", plugin::handle_reload);
        self.register("plugin.inspect", plugin::handle_inspect);

        // Event methods
        self.register("events.history", events::handle_history);

        // Log methods
        self.register("log.set_level", log::handle_set_level);
        self.register("log.tail", log::handle_tail);

        // Metrics
        self.register("metrics.get", metrics::handle_get);

        // Diagnostics
        self.register("diagnose.run", diagnose::handle_run);

        // Admin
        self.register("admin.shutdown", admin::handle_shutdown);
        self.register("admin.gc", admin::handle_gc);
    }

    fn register<F, Fut>(&mut self, name: &str, handler: F)
    where
        F: Fn(Arc<MethodContext>, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = MethodResult> + Send + 'static,
    {
        self.handlers.insert(
            name.to_string(),
            Box::new(move |ctx, params| Box::pin(handler(ctx, params))),
        );
    }
}

impl Default for MethodRegistry {
    fn default() -> Self {
        Self::new()
    }
}
```

### Step 5: Implement state methods (`state.snapshot`, `state.apply`)

```rust
// crates/shux-rpc/src/methods/state.rs

use std::sync::Arc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{MethodContext, MethodResult, MethodError};

/// `state.snapshot` — Return the complete SessionGraph.
///
/// Supports pagination via cursor token for large sessions.
/// Each page is a consistent snapshot at the sequence number
/// returned in the response.
///
/// Params:
///   cursor: Option<String> — pagination cursor from previous response
///   page_size: Option<u32> — items per page (default 100)
pub async fn handle_snapshot(ctx: Arc<MethodContext>, params: Value) -> MethodResult {
    #[derive(Deserialize)]
    struct Params {
        #[serde(default)]
        cursor: Option<String>,
        #[serde(default = "default_page_size")]
        page_size: u32,
    }

    fn default_page_size() -> u32 {
        100
    }

    let params: Params = serde_json::from_value(params)
        .map_err(|e| MethodError::invalid_params(e.to_string()))?;

    // TODO: Read snapshot from SessionGraph via ArcSwap
    // let snapshot = ctx.graph.snapshot();
    // let page = snapshot.paginate(params.cursor, params.page_size);

    // Placeholder response structure
    let result = serde_json::json!({
        "seq": 0,
        "sessions": [],
        "next_cursor": null
    });

    Ok(result)
}

/// `state.apply` — Atomic delta application.
///
/// Executes operations sequentially within a single transaction.
/// $N.field back-references allow operations to use results from
/// earlier operations in the same batch. If any operation fails,
/// all preceding operations are rolled back (all-or-nothing).
///
/// Params:
///   client_request_id: Option<String> — idempotency key
///   operations: Vec<{op: String, params: Value}>
pub async fn handle_apply(ctx: Arc<MethodContext>, params: Value) -> MethodResult {
    use crate::apply::{resolve_back_references, ApplyRequest, ApplyResult, OperationResult};

    let request: ApplyRequest = serde_json::from_value(params)
        .map_err(|e| MethodError::invalid_params(e.to_string()))?;

    // Check deduplication cache
    // if let Some(req_id) = &request.client_request_id {
    //     if let Some(cached) = ctx.dedup_cache.get(req_id) {
    //         return Ok(cached.result);
    //     }
    // }

    let mut results: Vec<Value> = Vec::new();
    let mut operation_results: Vec<OperationResult> = Vec::new();

    // Execute each operation sequentially
    for (index, op) in request.operations.iter().enumerate() {
        // Resolve back-references in params
        let resolved_params = resolve_back_references(&op.params, &results)
            .map_err(|e| MethodError {
                code: crate::error::ShuxErrorCode::InvalidParams,
                message: e.to_string(),
                data: Some(crate::error::ShuxErrorData {
                    resource: None,
                    id: None,
                    expected_version: None,
                    actual_version: None,
                    hint: Some("Check $N.field back-reference syntax".to_string()),
                    operation_index: Some(index),
                }),
            })?;

        // Dispatch the operation to the appropriate handler
        // TODO: Route op.op to the correct method handler
        // let result = dispatch_operation(&ctx, &op.op, resolved_params).await?;
        let result = serde_json::json!({"status": "ok"});

        results.push(result.clone());
        operation_results.push(OperationResult {
            index,
            op: op.op.clone(),
            result,
        });
    }

    let apply_result = ApplyResult {
        results: operation_results,
        replayed: false,
    };

    let result_value = serde_json::to_value(&apply_result)
        .map_err(|e| MethodError::internal(e.to_string()))?;

    // Cache result for deduplication
    // if let Some(req_id) = request.client_request_id {
    //     ctx.dedup_cache.insert(req_id, result_value.clone());
    // }

    Ok(result_value)
}
```

### Step 6: Implement copy mode methods

```rust
// crates/shux-rpc/src/methods/copy.rs

use std::sync::Arc;
use serde::Deserialize;
use serde_json::Value;

use super::{MethodContext, MethodResult, MethodError};

/// `copy.enter` — Enter copy mode for a pane.
///
/// Params:
///   pane_id: String — target pane UUID
pub async fn handle_enter(ctx: Arc<MethodContext>, params: Value) -> MethodResult {
    #[derive(Deserialize)]
    struct Params {
        pane_id: String,
    }

    let params: Params = serde_json::from_value(params)
        .map_err(|e| MethodError::invalid_params(e.to_string()))?;

    // TODO: Enter copy mode for the specified pane
    Ok(serde_json::json!({"pane_id": params.pane_id, "mode": "copy"}))
}

/// `copy.search` — Search within pane scrollback.
///
/// Params:
///   pane_id: String — target pane UUID
///   query: String — search string (regex or literal)
///   direction: Option<String> — "forward" (default) or "backward"
pub async fn handle_search(ctx: Arc<MethodContext>, params: Value) -> MethodResult {
    #[derive(Deserialize)]
    struct Params {
        pane_id: String,
        query: String,
        #[serde(default = "default_direction")]
        direction: String,
    }

    fn default_direction() -> String {
        "forward".to_string()
    }

    let params: Params = serde_json::from_value(params)
        .map_err(|e| MethodError::invalid_params(e.to_string()))?;

    // TODO: Search scrollback and return matches
    Ok(serde_json::json!({
        "pane_id": params.pane_id,
        "matches": 0,
        "current_match": null
    }))
}

/// `copy.select` — Set selection range in copy mode.
///
/// Params:
///   pane_id: String
///   start_line: u32
///   start_col: u32
///   end_line: u32
///   end_col: u32
pub async fn handle_select(ctx: Arc<MethodContext>, params: Value) -> MethodResult {
    #[derive(Deserialize)]
    struct Params {
        pane_id: String,
        start_line: u32,
        start_col: u32,
        end_line: u32,
        end_col: u32,
    }

    let params: Params = serde_json::from_value(params)
        .map_err(|e| MethodError::invalid_params(e.to_string()))?;

    // TODO: Set selection in copy mode
    Ok(serde_json::json!({"selected": true}))
}

/// `copy.to_clipboard` — Copy the current selection to the system clipboard.
///
/// Params:
///   pane_id: String
pub async fn handle_to_clipboard(ctx: Arc<MethodContext>, params: Value) -> MethodResult {
    #[derive(Deserialize)]
    struct Params {
        pane_id: String,
    }

    let params: Params = serde_json::from_value(params)
        .map_err(|e| MethodError::invalid_params(e.to_string()))?;

    // TODO: Read selection text and copy via OSC 52
    Ok(serde_json::json!({"copied": true}))
}
```

### Step 7: Implement theme, config, keybinding, plugin, events, log, metrics, diagnose, and admin methods

Each module follows the same pattern: parse params, dispatch to the appropriate subsystem, return structured JSON. The method signatures are defined here; the actual subsystem calls will be wired in as the subsystems are built.

Key implementation notes:

- **`theme.set`** accepts a `scope` parameter (`"session"`, `"window"`, `"pane"`) and `scope_id` to support per-pane theming (PRD section 5.3).
- **`config.set`** creates a runtime override (layer 5 in PRD section 10.1), stored in daemon memory.
- **`config.explain`** returns the full schema with defaults and descriptions (PRD section 10.2).
- **`events.history`** queries the bounded ring buffer (PRD section 8.4) and returns recent events matching optional filters.
- **`admin.gc`** triggers plugin garbage collection and memory reclamation.
- **`admin.shutdown`** sends `DaemonCommand::Shutdown` through the mpsc channel.
- **`diagnose.run`** collects config, caps, plugin status, recent errors, terminal info (PRD section 11.2).

### Step 8: Add `--format json|text` support to CLI dispatching

All CLI commands map to API calls and support `--format json|text`. Define a formatter trait:

```rust
// crates/shux-rpc/src/format.rs (or in shux binary crate)

use serde_json::Value;

/// Output format for CLI commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Json,
    Text,
}

impl OutputFormat {
    pub fn from_str_or_detect(s: Option<&str>) -> Self {
        match s {
            Some("json") => Self::Json,
            Some("text") => Self::Text,
            None => {
                // Auto-detect: JSON if stdout is not a TTY (piped), text if TTY
                if atty::is(atty::Stream::Stdout) {
                    Self::Text
                } else {
                    Self::Json
                }
            }
            _ => Self::Text,
        }
    }

    /// Format a JSON-RPC result for output.
    pub fn format(&self, method: &str, result: &Value) -> String {
        match self {
            Self::Json => serde_json::to_string_pretty(result)
                .unwrap_or_else(|_| "{}".to_string()),
            Self::Text => format_as_text(method, result),
        }
    }
}

/// Format a method result as human-readable text.
/// Each method family gets custom formatting.
fn format_as_text(method: &str, result: &Value) -> String {
    match method.split('.').next() {
        Some("session") => format_session_result(method, result),
        Some("plugin") => format_plugin_result(method, result),
        Some("theme") => format_theme_result(method, result),
        _ => serde_json::to_string_pretty(result)
            .unwrap_or_else(|_| "{}".to_string()),
    }
}

fn format_session_result(_method: &str, result: &Value) -> String {
    // TODO: Human-readable session formatting
    format!("{}", serde_json::to_string_pretty(result).unwrap_or_default())
}

fn format_plugin_result(_method: &str, result: &Value) -> String {
    // TODO: Table-formatted plugin list
    format!("{}", serde_json::to_string_pretty(result).unwrap_or_default())
}

fn format_theme_result(_method: &str, result: &Value) -> String {
    // TODO: Human-readable theme formatting
    format!("{}", serde_json::to_string_pretty(result).unwrap_or_default())
}
```

### Step 9: Write unit tests

```rust
// Tests for back-reference resolution
#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply::resolve_back_references;
    use serde_json::json;

    #[test]
    fn test_resolve_simple_back_ref() {
        let previous = vec![json!({"id": "session-123", "name": "work"})];
        let params = json!({"session_id": "$0.id"});
        let resolved = resolve_back_references(&params, &previous).unwrap();
        assert_eq!(resolved["session_id"], "session-123");
    }

    #[test]
    fn test_resolve_nested_back_ref() {
        let previous = vec![
            json!({"id": "s-1"}),
            json!({"id": "w-1", "active_pane_id": "p-1"}),
        ];
        let params = json!({"pane_id": "$1.active_pane_id", "direction": "vertical"});
        let resolved = resolve_back_references(&params, &previous).unwrap();
        assert_eq!(resolved["pane_id"], "p-1");
        assert_eq!(resolved["direction"], "vertical");
    }

    #[test]
    fn test_back_ref_index_out_of_range() {
        let previous = vec![json!({"id": "s-1"})];
        let params = json!({"session_id": "$5.id"});
        let result = resolve_back_references(&params, &previous);
        assert!(result.is_err());
    }

    #[test]
    fn test_back_ref_field_not_found() {
        let previous = vec![json!({"id": "s-1"})];
        let params = json!({"session_id": "$0.nonexistent"});
        let result = resolve_back_references(&params, &previous);
        assert!(result.is_err());
    }

    #[test]
    fn test_no_back_refs_passes_through() {
        let previous: Vec<serde_json::Value> = vec![];
        let params = json!({"name": "work", "count": 42});
        let resolved = resolve_back_references(&params, &previous).unwrap();
        assert_eq!(resolved, params);
    }

    #[test]
    fn test_back_ref_in_array() {
        let previous = vec![json!({"id": "s-1"})];
        let params = json!({"ids": ["$0.id", "static"]});
        let resolved = resolve_back_references(&params, &previous).unwrap();
        assert_eq!(resolved["ids"][0], "s-1");
        assert_eq!(resolved["ids"][1], "static");
    }
}

#[cfg(test)]
mod dedup_tests {
    use crate::dedup::DedupCache;
    use serde_json::json;

    #[test]
    fn test_dedup_cache_insert_and_get() {
        let cache = DedupCache::new();
        cache.insert("req-1".to_string(), json!({"id": "s-1"}));
        let result = cache.get("req-1");
        assert!(result.is_some());
        assert_eq!(result.unwrap().result, json!({"id": "s-1"}));
    }

    #[test]
    fn test_dedup_cache_miss() {
        let cache = DedupCache::new();
        assert!(cache.get("nonexistent").is_none());
    }

    #[test]
    fn test_dedup_cache_eviction() {
        let cache = DedupCache::new();
        // Fill cache beyond capacity (10000 entries)
        for i in 0..10_001 {
            cache.insert(format!("req-{}", i), json!({"i": i}));
        }
        // First entry should have been evicted
        assert!(cache.get("req-0").is_none());
        // Last entry should still exist
        assert!(cache.get("req-10000").is_some());
    }
}
```

### Step 10: Update Cargo.toml and wire modules

```toml
# Add to crates/shux-rpc/Cargo.toml [dependencies]
lru = "0.12"
base64 = "0.22"
```

---

## Verification

### Functional

```bash
# Build the shux-rpc crate
cargo build -p shux-rpc

# Verify the method registry contains all expected methods
cargo nextest run -p shux-rpc -- method_registry

# Verify clippy passes
cargo clippy -p shux-rpc -- -D warnings
```

### Tests

```bash
# Run all shux-rpc tests
cargo nextest run -p shux-rpc

# Run specifically the apply/dedup/error tests
cargo nextest run -p shux-rpc -- apply
cargo nextest run -p shux-rpc -- dedup
cargo nextest run -p shux-rpc -- back_ref

# Full workspace check
cargo nextest run --workspace
```

---

## Completion Criteria

- [ ] All JSON-RPC methods from PRD section 8.2 are registered in `MethodRegistry`
- [ ] `state.snapshot` supports pagination via cursor token
- [ ] `state.apply` supports sequential operations, `$N.field` back-references, and all-or-nothing rollback
- [ ] `client_request_id` deduplication works via LRU cache (10000 entries)
- [ ] `copy.*` methods: `enter`, `search`, `select`, `to_clipboard`
- [ ] `theme.*` methods: `list`, `get`, `set` (with session/window/pane scope)
- [ ] `config.*` methods: `get`, `set` (runtime override), `validate`, `explain`
- [ ] `keybinding.*` methods: `list`, `set`, `reset`
- [ ] `plugin.*` methods: `list`, `enable`, `disable`, `reload`, `inspect`
- [ ] `events.history` queries bounded ring buffer
- [ ] `log.set_level` and `log.tail` methods implemented
- [ ] `metrics.get` returns collected metrics
- [ ] `diagnose.run` collects diagnostic bundle
- [ ] `admin.shutdown` triggers graceful daemon shutdown
- [ ] `admin.gc` triggers plugin GC and memory reclamation
- [ ] Structured error codes per PRD section 8.3 (`-32001` through `-32010`)
- [ ] Error responses include `resource`, `id`, `expected_version`, `actual_version`, `hint`, and failed `operation_index` for `state.apply`
- [ ] `--format json|text` supported on all CLI commands
- [ ] All unit tests pass for back-reference resolution, dedup cache, and error codes
- [ ] `cargo clippy -p shux-rpc -- -D warnings` passes

---

## Commit Message
```
feat(rpc): complete JSON-RPC API surface with state.apply transaction engine

- Register all ~40 JSON-RPC methods from PRD section 8.2
- state.apply with sequential operations, $N.field back-references, rollback
- client_request_id deduplication via LRU cache (10000 entries)
- copy.*, theme.*, config.*, keybinding.*, plugin.* method families
- events.history, log.*, metrics.get, diagnose.run, admin.* methods
- Structured error codes (-32001 through -32010) with actionable hints
- --format json|text output formatting for CLI integration
```

---

## Session Protocol

1. **Before starting:** Read `CLAUDE.md`. Read task 034 to understand the existing JSON-RPC server skeleton, framing layer, and method dispatch pattern. Read PRD sections 5.4 (state.apply schema), 8.2 (method list), 8.3 (error format), 8.5 (agent-safe patterns), and 8.6 (CLI mapping). Verify task 034 is complete.
2. **During:** Implement in order: error codes (Step 1) -> dedup cache (Step 2) -> apply engine (Step 3) -> method registry (Step 4) -> state methods (Step 5) -> remaining method families (Steps 6-7) -> CLI format (Step 8) -> tests (Step 9). Run `cargo check -p shux-rpc` after each step. The `state.apply` engine is the most complex piece -- test the back-reference resolver thoroughly before moving on.
3. **Testing:** Run `cargo nextest run -p shux-rpc` after each method family. Pay special attention to edge cases in back-reference resolution: nested objects, arrays, invalid indices, missing fields.
4. **After:** Run `make check`. Update `docs/PROGRESS.md` (mark 035 as done). Update `CLAUDE.md` Learnings with any discoveries about JSON-RPC error code conventions, LRU crate API, or `serde_json::Value` manipulation patterns.
5. **Watch out for:**
   - The `$N.field` back-reference parser must handle dollar signs in non-reference contexts (e.g., environment variables). Only strings starting with exactly `$` followed by a digit should be treated as references.
   - The dedup cache uses a `Mutex` -- this is fine since dedup lookups are fast (O(1) LRU) and infrequent relative to the event loop.
   - Method handlers receive `Arc<MethodContext>` -- subsystem references will be `None` until the respective tasks wire them in. Use early returns or stubs for now.
   - The rollback mechanism for `state.apply` requires the SessionGraph to support undo. If the graph does not yet support this, implement a snapshot-and-restore approach (take an ArcSwap snapshot before the batch, restore it on failure).
