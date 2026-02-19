# 042 — Event Interception Chain

**Status:** Pending
**Depends On:** 041, 036
**Parallelizable With:** 043

---

## Problem

Plugins need the ability to intercept, modify, or block events before they propagate to the core engine. This is the foundation for safety-critical features like Danger Zone (blocking destructive commands), Pane Sync (replicating input), and Scratchpad (keybinding interception). Without a well-defined interception chain, plugins cannot gate operations -- they can only react after the fact.

The interception chain must be sequential (plugins execute in config order), support event modification (each plugin receives the cumulative result of previous modifications), support blocking (returning None terminates the chain), enforce per-interceptor timeouts (100ms via epoch interruption for Wasm, process timeout for process plugins), and fail-closed (a crashing or timing-out interceptor blocks the event rather than letting it through). The chain operates on the critical input path, so correctness and bounded latency are paramount.

Input interception specifically fires on line submission (Enter), not per-keystroke. The host buffers characters as they are typed, and on newline detection, pauses PTY delivery, runs the interception chain, and either forwards or discards the buffered line. If blocked, a line-kill sequence is sent to the PTY to clear partial echo.

## PRD Reference

- **section 7.2a** — Interception chain semantics: sequential chain, config ordering, block/modify/pass, fail-closed behavior
- **section 7.5** — `intercept-event` WIT function: `func(event-json: string) -> result<option<string>, plugin-error>`
- **section 7.2** — Extension points: Event interceptors can "block, modify, or gate events before propagation"
- **section 7.6** — Process plugin protocol: `{"type": "intercept", "id": "...", "event": {...}}` message type
- **section 14.1** — Plugin call overhead: p99 <= 5ms per call, kill at 100ms
- **section 21** — Event taxonomy: `pane.input {pane_id, data}` fires on line-submit (Enter), not per-keystroke

---

## Files to Create

- `crates/shux-plugin/src/interception.rs` — Interception chain executor: chain ordering, sequential dispatch, timeout enforcement, fail-closed logic, degraded plugin tracking
- `crates/shux-core/src/input_gate.rs` — Input gate: line buffering, newline detection, PTY pause/resume, line-kill on block, integration with the interception chain

## Files to Modify

- `crates/shux-plugin/src/lib.rs` — Add `pub mod interception;`
- `crates/shux-plugin/Cargo.toml` — Add dependencies if needed (tokio time, tracing)
- `crates/shux-core/src/lib.rs` — Add `pub mod input_gate;`
- `crates/shux-core/src/bus.rs` — Hook interception chain into the event bus dispatch path for interceptable event types
- `crates/shux-core/Cargo.toml` — Add dependencies if needed

---

## Execution Steps

### Step 1: Define interception types in `crates/shux-plugin/src/interception.rs`

Define the core types for the interception chain: interceptor registration, chain configuration, and result types.

```rust
use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{info, warn, error, instrument};

/// Maximum time an interceptor is allowed to process an event.
/// If exceeded, the event is BLOCKED (fail-closed) and the plugin is marked degraded.
const DEFAULT_INTERCEPTOR_TIMEOUT: Duration = Duration::from_millis(100);

/// Result of a single interceptor's processing of an event.
#[derive(Debug, Clone)]
pub enum InterceptResult {
    /// Pass the event through, possibly modified.
    /// The next interceptor in the chain receives this version.
    Pass(InterceptedEvent),

    /// Block the event. Chain terminates immediately.
    /// No subsequent interceptors or the core receive the event.
    Block,
}

/// An event flowing through the interception chain.
/// Wraps the serialized event JSON so interceptors can modify it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterceptedEvent {
    /// The event type (e.g., "pane.input")
    pub event_type: String,

    /// The full event payload as JSON.
    /// Each interceptor may return a modified version.
    pub event_json: String,
}

/// Outcome of running the full interception chain.
#[derive(Debug)]
pub enum ChainOutcome {
    /// All interceptors passed. The event (possibly modified) should be
    /// delivered to the core for normal processing.
    Passed(InterceptedEvent),

    /// An interceptor explicitly blocked the event by returning None.
    Blocked {
        /// The plugin that blocked the event.
        blocked_by: PluginId,
        /// Position in the chain (0-indexed).
        chain_position: usize,
    },

    /// An interceptor failed (crash, timeout, error). Event is BLOCKED
    /// per fail-closed policy.
    FailedClosed {
        /// The plugin that failed.
        failed_plugin: PluginId,
        /// The error that occurred.
        error: InterceptionError,
        /// Position in the chain (0-indexed).
        chain_position: usize,
    },
}

/// Identifies a plugin in the interception chain.
pub type PluginId = String;

/// Errors that can occur during interception.
#[derive(Debug, Error)]
pub enum InterceptionError {
    #[error("interceptor timed out after {0:?}")]
    Timeout(Duration),

    #[error("interceptor crashed: {0}")]
    Crashed(String),

    #[error("interceptor returned invalid result: {0}")]
    InvalidResult(String),

    #[error("interceptor returned error: code={code}, message={message}")]
    PluginError { code: i32, message: String },
}

/// Tracks the health state of an interceptor plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterceptorHealth {
    /// Plugin is healthy and will receive intercepted events.
    Healthy,

    /// Plugin has failed during interception. Subsequent events bypass
    /// this plugin with a warning until the plugin is reloaded.
    Degraded {
        reason: String,
        degraded_since: std::time::Instant,
    },
}
```

### Step 2: Implement the interceptor registry

Track which plugins have registered to intercept which event types, ordered by their position in the `[plugins]` config section.

```rust
/// Registry of interceptor plugins, ordered by config position.
///
/// The order matters: plugins execute in the order they appear in
/// `shux.toml`'s `[plugins]` section. Users control priority by
/// reordering plugins in config (e.g., Danger Zone should be first).
pub struct InterceptorRegistry {
    /// Map from event type to ordered list of interceptor plugin IDs.
    /// Order matches the `[plugins]` section in config.
    chains: HashMap<String, Vec<PluginId>>,

    /// Stable plugin ordering index from config (`[plugins]` list).
    positions: HashMap<PluginId, usize>,

    /// Health status for each interceptor plugin.
    health: HashMap<PluginId, InterceptorHealth>,

    /// Per-interceptor timeout override. Falls back to DEFAULT_INTERCEPTOR_TIMEOUT.
    timeouts: HashMap<PluginId, Duration>,
}

impl InterceptorRegistry {
    pub fn new() -> Self {
        Self {
            chains: HashMap::new(),
            positions: HashMap::new(),
            health: HashMap::new(),
            timeouts: HashMap::new(),
        }
    }

    /// Register a plugin as an interceptor for the given event types.
    /// `config_position` determines the plugin's order in chains.
    /// Plugins with lower config_position execute first.
    pub fn register(
        &mut self,
        plugin_id: PluginId,
        event_types: Vec<String>,
        config_position: usize,
    ) {
        self.positions.insert(plugin_id.clone(), config_position);
        self.health.insert(plugin_id.clone(), InterceptorHealth::Healthy);

        for event_type in event_types {
            let chain = self.chains.entry(event_type).or_default();

            // Remove stale entry first, then insert by config position.
            chain.retain(|id| id != &plugin_id);
            let insert_idx = chain
                .iter()
                .position(|id| {
                    self.positions
                        .get(id)
                        .copied()
                        .unwrap_or(usize::MAX)
                        > config_position
                })
                .unwrap_or(chain.len());
            chain.insert(insert_idx, plugin_id.clone());
        }
    }

    /// Unregister a plugin from all interception chains.
    /// Called when a plugin is disabled or unloaded.
    pub fn unregister(&mut self, plugin_id: &str) {
        for chain in self.chains.values_mut() {
            chain.retain(|id| id != plugin_id);
        }
        self.positions.remove(plugin_id);
        self.health.remove(plugin_id);
        self.timeouts.remove(plugin_id);
    }

    /// Get the ordered list of interceptor plugin IDs for an event type.
    /// Returns only healthy (non-degraded) plugins.
    pub fn get_chain(&self, event_type: &str) -> Vec<&PluginId> {
        self.chains
            .get(event_type)
            .map(|chain| {
                chain
                    .iter()
                    .filter(|id| {
                        matches!(
                            self.health.get(*id),
                            Some(InterceptorHealth::Healthy)
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Check if any interceptors are registered for this event type.
    pub fn has_interceptors(&self, event_type: &str) -> bool {
        !self.get_chain(event_type).is_empty()
    }

    /// Mark a plugin as degraded. Subsequent events bypass it with a warning.
    pub fn mark_degraded(&mut self, plugin_id: &str, reason: String) {
        if let Some(health) = self.health.get_mut(plugin_id) {
            warn!(
                plugin_id = plugin_id,
                reason = %reason,
                "Marking interceptor plugin as degraded — events will bypass until reload"
            );
            *health = InterceptorHealth::Degraded {
                reason,
                degraded_since: std::time::Instant::now(),
            };
        }
    }

    /// Restore a plugin to healthy state (e.g., after hot reload).
    pub fn mark_healthy(&mut self, plugin_id: &str) {
        if let Some(health) = self.health.get_mut(plugin_id) {
            info!(plugin_id = plugin_id, "Interceptor plugin restored to healthy");
            *health = InterceptorHealth::Healthy;
        }
    }

    /// Get the timeout for a specific interceptor.
    pub fn timeout_for(&self, plugin_id: &str) -> Duration {
        self.timeouts
            .get(plugin_id)
            .copied()
            .unwrap_or(DEFAULT_INTERCEPTOR_TIMEOUT)
    }

    /// Get health status for a plugin.
    pub fn health(&self, plugin_id: &str) -> Option<&InterceptorHealth> {
        self.health.get(plugin_id)
    }

    /// Get all degraded interceptor plugins.
    pub fn degraded_plugins(&self) -> Vec<(&str, &InterceptorHealth)> {
        self.health
            .iter()
            .filter(|(_, h)| matches!(h, InterceptorHealth::Degraded { .. }))
            .map(|(id, h)| (id.as_str(), h))
            .collect()
    }
}
```

### Step 3: Implement the chain executor

The chain executor runs interceptors sequentially, enforces timeouts, and implements fail-closed semantics.

```rust
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::timeout;

/// Trait for invoking interceptor plugins. The plugin host implements this.
/// Abstracted as a trait so the chain executor can be tested independently.
#[async_trait::async_trait]
pub trait InterceptorInvoker: Send + Sync {
    /// Call a plugin's `intercept-event` function with the given event JSON.
    ///
    /// Returns:
    /// - `Ok(Some(modified_json))` — plugin passes the event (possibly modified)
    /// - `Ok(None)` — plugin blocks the event
    /// - `Err(...)` — plugin crashed or returned an error
    async fn invoke_intercept(
        &self,
        plugin_id: &str,
        event_json: &str,
    ) -> Result<Option<String>, InterceptionError>;
}

/// The interception chain executor.
///
/// Given an event and a registry of interceptors, executes each interceptor
/// in config order with timeout enforcement and fail-closed semantics.
pub struct InterceptionChain {
    registry: Arc<RwLock<InterceptorRegistry>>,
    invoker: Arc<dyn InterceptorInvoker>,
}

impl InterceptionChain {
    pub fn new(
        registry: Arc<RwLock<InterceptorRegistry>>,
        invoker: Arc<dyn InterceptorInvoker>,
    ) -> Self {
        Self { registry, invoker }
    }

    /// Run the interception chain for an event.
    ///
    /// The event flows through each interceptor in config order:
    /// 1. If an interceptor returns `Some(modified)`, the next interceptor
    ///    receives the modified version.
    /// 2. If an interceptor returns `None`, the chain terminates and the
    ///    event is blocked.
    /// 3. If an interceptor times out or errors, the event is blocked
    ///    (fail-closed) and the plugin is marked degraded.
    /// 4. If all interceptors pass, the (cumulatively modified) event is
    ///    returned for core processing.
    #[instrument(skip(self), fields(event_type = %event.event_type))]
    pub async fn execute(&self, event: InterceptedEvent) -> ChainOutcome {
        let registry = self.registry.read().await;
        let chain = registry.get_chain(&event.event_type);

        if chain.is_empty() {
            return ChainOutcome::Passed(event);
        }

        let chain: Vec<String> = chain.into_iter().cloned().collect();
        let timeouts: Vec<Duration> = chain
            .iter()
            .map(|id| registry.timeout_for(id))
            .collect();

        // Drop read lock before executing interceptors (they may need write access
        // to mark plugins degraded).
        drop(registry);

        let mut current_event = event;

        for (position, (plugin_id, plugin_timeout)) in
            chain.iter().zip(timeouts.iter()).enumerate()
        {
            let result = timeout(
                *plugin_timeout,
                self.invoker.invoke_intercept(plugin_id, &current_event.event_json),
            )
            .await;

            match result {
                // Timeout expired
                Err(_elapsed) => {
                    let err = InterceptionError::Timeout(*plugin_timeout);
                    error!(
                        plugin_id = %plugin_id,
                        timeout_ms = plugin_timeout.as_millis(),
                        "Interceptor timed out — event BLOCKED (fail-closed)"
                    );

                    // Mark plugin as degraded
                    let mut registry = self.registry.write().await;
                    registry.mark_degraded(
                        plugin_id,
                        format!("Timed out during intercept-event after {:?}", plugin_timeout),
                    );

                    return ChainOutcome::FailedClosed {
                        failed_plugin: plugin_id.clone(),
                        error: err,
                        chain_position: position,
                    };
                }

                // Interceptor returned a result
                Ok(invoke_result) => match invoke_result {
                    // Plugin explicitly blocked the event
                    Ok(None) => {
                        info!(
                            plugin_id = %plugin_id,
                            event_type = %current_event.event_type,
                            "Interceptor blocked event"
                        );
                        return ChainOutcome::Blocked {
                            blocked_by: plugin_id.clone(),
                            chain_position: position,
                        };
                    }

                    // Plugin passed the event (possibly modified)
                    Ok(Some(modified_json)) => {
                        if modified_json != current_event.event_json {
                            info!(
                                plugin_id = %plugin_id,
                                event_type = %current_event.event_type,
                                "Interceptor modified event"
                            );
                        }
                        current_event.event_json = modified_json;
                    }

                    // Plugin errored
                    Err(err) => {
                        error!(
                            plugin_id = %plugin_id,
                            error = %err,
                            "Interceptor failed — event BLOCKED (fail-closed)"
                        );

                        let mut registry = self.registry.write().await;
                        registry.mark_degraded(
                            plugin_id,
                            format!("Error during intercept-event: {}", err),
                        );

                        return ChainOutcome::FailedClosed {
                            failed_plugin: plugin_id.clone(),
                            error: err,
                            chain_position: position,
                        };
                    }
                },
            }
        }

        // All interceptors passed
        ChainOutcome::Passed(current_event)
    }
}
```

### Step 4: Implement the input gate in `crates/shux-core/src/input_gate.rs`

The input gate buffers keystrokes, detects line submission, pauses PTY delivery, runs the interception chain, and either forwards or discards the buffered line.

```rust
use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::{debug, info, warn, error};

/// The line-kill sequence sent to the PTY to clear partial echo when
/// an input line is blocked. This sends Ctrl+U (kill line) which works
/// in most shells (bash, zsh, fish).
const LINE_KILL_SEQUENCE: &[u8] = b"\x15";

/// Alternate line-kill: Ctrl+C (interrupt) followed by newline.
/// Used as fallback if the shell doesn't support Ctrl+U.
const LINE_KILL_FALLBACK: &[u8] = b"\x03\n";

/// Tracks input buffering state for a single pane.
#[derive(Debug)]
struct PaneInputBuffer {
    /// Accumulated bytes since the last newline.
    buffer: Vec<u8>,

    /// Whether this pane has interceptors registered for pane.input.
    /// If false, all input is forwarded immediately (fast path).
    has_interceptors: bool,
}

impl PaneInputBuffer {
    fn new(has_interceptors: bool) -> Self {
        Self {
            buffer: Vec::with_capacity(256),
            has_interceptors,
        }
    }

    /// Append bytes to the buffer. Returns lines that are ready for
    /// interception (terminated by newline).
    fn append(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        let mut ready_lines = Vec::new();

        for &byte in data {
            self.buffer.push(byte);

            // Detect line submission: Enter key produces \r or \n or \r\n.
            // We fire interception on \r (carriage return) since that's what
            // terminals typically send for Enter.
            if byte == b'\r' || byte == b'\n' {
                // If the previous byte was \r and this is \n, it's a \r\n pair.
                // Don't produce two lines for one Enter press.
                if byte == b'\n' && self.buffer.len() >= 2
                    && self.buffer[self.buffer.len() - 2] == b'\r'
                {
                    // Already handled on \r, just append the \n to the last line
                    if let Some(last) = ready_lines.last_mut() {
                        last.push(byte);
                    }
                    self.buffer.clear();
                    continue;
                }

                ready_lines.push(std::mem::take(&mut self.buffer));
            }
        }

        ready_lines
    }

    /// Drain the buffer without producing a line (for cleanup).
    fn drain(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buffer)
    }
}

/// Action to take after the interception chain processes a line.
#[derive(Debug)]
pub enum InputAction {
    /// Forward the (possibly modified) line to the PTY.
    Forward(Vec<u8>),

    /// Block the line. Send line-kill to PTY to clear partial echo.
    Block {
        /// The plugin that blocked the input.
        blocked_by: String,
        /// Whether this was a fail-closed (plugin error) vs explicit block.
        is_fail_closed: bool,
    },
}

/// Message sent from the input gate to the PTY writer task.
#[derive(Debug)]
pub enum PtyWriteCommand {
    /// Write these bytes to the PTY master fd.
    Write(Vec<u8>),

    /// Send line-kill sequence to clear partial echo.
    LineKill,
}

/// The input gate sits between the user's keyboard input and the PTY.
///
/// For panes with registered interceptors on `pane.input`:
/// 1. Characters are buffered as they arrive.
/// 2. Characters are also forwarded to the PTY immediately (for echo).
/// 3. On newline detection, PTY delivery is paused.
/// 4. The interception chain is run on the complete line.
/// 5. If passed: the newline is delivered to the PTY (line was already echoed).
/// 6. If blocked: a line-kill sequence is sent to the PTY to clear the echo.
///
/// For panes WITHOUT interceptors, all input is forwarded immediately
/// with zero overhead (fast path).
pub struct InputGate {
    /// Per-pane input buffers.
    buffers: HashMap<String, PaneInputBuffer>,

    /// Channel to send write commands to the PTY writer.
    /// Keyed by pane ID.
    pty_writers: HashMap<String, mpsc::Sender<PtyWriteCommand>>,
}

impl InputGate {
    pub fn new() -> Self {
        Self {
            buffers: HashMap::new(),
            pty_writers: HashMap::new(),
        }
    }

    /// Register a pane with the input gate.
    pub fn register_pane(
        &mut self,
        pane_id: String,
        has_interceptors: bool,
        pty_writer: mpsc::Sender<PtyWriteCommand>,
    ) {
        self.buffers
            .insert(pane_id.clone(), PaneInputBuffer::new(has_interceptors));
        self.pty_writers.insert(pane_id, pty_writer);
    }

    /// Unregister a pane (e.g., when it is closed).
    pub fn unregister_pane(&mut self, pane_id: &str) {
        self.buffers.remove(pane_id);
        self.pty_writers.remove(pane_id);
    }

    /// Update whether a pane has interceptors (called when plugins register/unregister).
    pub fn set_has_interceptors(&mut self, pane_id: &str, has_interceptors: bool) {
        if let Some(buf) = self.buffers.get_mut(pane_id) {
            buf.has_interceptors = has_interceptors;
        }
    }

    /// Process input bytes for a pane.
    ///
    /// Returns a list of lines ready for interception, or None if
    /// there are no interceptors (fast path: input is forwarded directly).
    pub fn process_input(
        &mut self,
        pane_id: &str,
        data: &[u8],
    ) -> InputProcessResult {
        let buf = match self.buffers.get_mut(pane_id) {
            Some(buf) => buf,
            None => {
                warn!(pane_id = pane_id, "Input for unknown pane — forwarding directly");
                return InputProcessResult::ForwardImmediately(data.to_vec());
            }
        };

        // Fast path: no interceptors, forward everything immediately.
        if !buf.has_interceptors {
            return InputProcessResult::ForwardImmediately(data.to_vec());
        }

        // Slow path: buffer and detect line boundaries.
        // Forward non-newline characters immediately for echo,
        // but hold back the newline for interception.
        let ready_lines = buf.append(data);

        if ready_lines.is_empty() {
            // No complete lines yet. Forward the partial input for echo.
            // The PTY needs to see these characters for shell line editing.
            InputProcessResult::ForwardPartial(data.to_vec())
        } else {
            // One or more complete lines are ready for interception.
            InputProcessResult::LinesReady(ready_lines)
        }
    }

    /// Handle the result of interception for a line.
    pub async fn handle_interception_result(
        &self,
        pane_id: &str,
        action: InputAction,
    ) -> Result<(), InputGateError> {
        let writer = self
            .pty_writers
            .get(pane_id)
            .ok_or_else(|| InputGateError::PaneNotFound(pane_id.to_string()))?;

        match action {
            InputAction::Forward(line) => {
                // The characters were already forwarded for echo.
                // We just need to let the PTY process the complete line
                // (the newline was held back during interception).
                // Actually, in the buffering model above, we need to
                // forward the newline character that was held.
                debug!(pane_id = pane_id, "Interception passed — forwarding newline to PTY");
                writer
                    .send(PtyWriteCommand::Write(line))
                    .await
                    .map_err(|_| InputGateError::PtyWriterClosed(pane_id.to_string()))?;
            }
            InputAction::Block { blocked_by, is_fail_closed } => {
                info!(
                    pane_id = pane_id,
                    blocked_by = %blocked_by,
                    is_fail_closed = is_fail_closed,
                    "Input line blocked — sending line-kill to PTY"
                );
                writer
                    .send(PtyWriteCommand::LineKill)
                    .await
                    .map_err(|_| InputGateError::PtyWriterClosed(pane_id.to_string()))?;
            }
        }

        Ok(())
    }
}

/// Result of processing input bytes.
#[derive(Debug)]
pub enum InputProcessResult {
    /// No interceptors registered. Forward all bytes to PTY immediately.
    ForwardImmediately(Vec<u8>),

    /// Partial input (no newline yet). Forward to PTY for echo, but
    /// keep buffering for interception.
    ForwardPartial(Vec<u8>),

    /// One or more complete lines ready for interception. PTY delivery
    /// of the newline is paused until the interception chain completes.
    LinesReady(Vec<Vec<u8>>),
}

/// Errors from the input gate.
#[derive(Debug, Error)]
pub enum InputGateError {
    #[error("pane not found: {0}")]
    PaneNotFound(String),

    #[error("PTY writer channel closed for pane: {0}")]
    PtyWriterClosed(String),
}
```

### Step 5: Implement the event bus integration

Hook the interception chain into the event bus so that interceptable event types are routed through the chain before being broadcast.

```rust
// In crates/shux-core/src/bus.rs (modifications)
//
// The event bus dispatch path needs to check whether an event type has
// interceptors registered. If so, route through the interception chain
// before broadcasting.

/// Event types that support interception.
/// Only events listed here can be intercepted by plugins.
/// This is a safety boundary: plugins cannot intercept arbitrary events.
const INTERCEPTABLE_EVENT_TYPES: &[&str] = &[
    "pane.input",
    // Future: additional interceptable types as needed.
    // API-level operation interception (e.g., pane.create) is v2.
];

/// Check if an event type supports interception.
pub fn is_interceptable(event_type: &str) -> bool {
    INTERCEPTABLE_EVENT_TYPES.contains(&event_type)
}

/// Modified event dispatch: if the event type is interceptable and has
/// registered interceptors, run the chain first.
///
/// Pseudocode for the dispatch path:
///
/// ```ignore
/// async fn dispatch_event(&self, event: Event) {
///     if is_interceptable(&event.event_type)
///         && self.interception_chain.has_interceptors(&event.event_type)
///     {
///         let intercepted = InterceptedEvent {
///             event_type: event.event_type.clone(),
///             event_json: serde_json::to_string(&event).unwrap(),
///         };
///
///         match self.interception_chain.execute(intercepted).await {
///             ChainOutcome::Passed(modified_event) => {
///                 let event: Event = serde_json::from_str(&modified_event.event_json).unwrap();
///                 self.broadcast(event);
///             }
///             ChainOutcome::Blocked { blocked_by, .. } => {
///                 // Event is consumed. Emit a notification event.
///                 self.broadcast(Event::new("plugin.event_blocked", json!({
///                     "original_event_type": event.event_type,
///                     "blocked_by": blocked_by,
///                 })));
///             }
///             ChainOutcome::FailedClosed { failed_plugin, error, .. } => {
///                 // Event is consumed. Emit a plugin.error event.
///                 self.broadcast(Event::new("plugin.error", json!({
///                     "plugin_id": failed_plugin,
///                     "error": error.to_string(),
///                     "context": "intercept-event",
///                 })));
///             }
///         }
///     } else {
///         // No interception needed. Broadcast directly.
///         self.broadcast(event);
///     }
/// }
/// ```
```

### Step 6: Implement the host-rendered overlay for interception failures

When an interceptor fails (crash/timeout), the user must see a clear explanation. The host renders this overlay directly (not via the failed plugin).

```rust
// In crates/shux-ui/src/overlay.rs or a new host_overlay module

/// Render a host-managed overlay for interception failure.
///
/// This overlay is rendered by the host, not by any plugin, because
/// the plugin that should handle UI has just failed.
///
/// Example output:
/// ```text
/// ┌──────────── Plugin Error ─────────────┐
/// │                                        │
/// │  Plugin "danger-zone" failed during    │
/// │  interception — command blocked.       │
/// │                                        │
/// │  Error: Timed out after 100ms          │
/// │                                        │
/// │  Press Enter to dismiss.               │
/// │                                        │
/// └────────────────────────────────────────┘
/// ```
pub fn render_interception_failure_overlay(
    plugin_name: &str,
    error_message: &str,
    width: u16,
    height: u16,
) -> String {
    let title = " Plugin Error ";
    let msg_line1 = format!("Plugin \"{}\" failed during", plugin_name);
    let msg_line2 = "interception \u{2014} command blocked.";
    let err_line = format!("Error: {}", error_message);
    let dismiss_line = "Press Enter to dismiss.";

    // Calculate box dimensions
    let content_width = width.min(44) as usize;
    let border_h = "\u{2500}".repeat(content_width - 2);

    let mut lines = Vec::new();
    lines.push(format!("\u{250c}{}\u{2510}", center_in(&border_h, title, content_width - 2)));
    lines.push(format!("\u{2502}{}\u{2502}", " ".repeat(content_width - 2)));
    lines.push(format!("\u{2502}{}\u{2502}", pad_center(&msg_line1, content_width - 2)));
    lines.push(format!("\u{2502}{}\u{2502}", pad_center(msg_line2, content_width - 2)));
    lines.push(format!("\u{2502}{}\u{2502}", " ".repeat(content_width - 2)));
    lines.push(format!("\u{2502}{}\u{2502}", pad_center(&err_line, content_width - 2)));
    lines.push(format!("\u{2502}{}\u{2502}", " ".repeat(content_width - 2)));
    lines.push(format!("\u{2502}{}\u{2502}", pad_center(dismiss_line, content_width - 2)));
    lines.push(format!("\u{2502}{}\u{2502}", " ".repeat(content_width - 2)));
    lines.push(format!("\u{2514}{}\u{2518}", "\u{2500}".repeat(content_width - 2)));

    lines.join("\n")
}

fn pad_center(text: &str, width: usize) -> String {
    if text.len() >= width {
        return text[..width].to_string();
    }
    let padding = width - text.len();
    let left = padding / 2;
    let right = padding - left;
    format!("{}{}{}", " ".repeat(left), text, " ".repeat(right))
}

fn center_in(border: &str, title: &str, width: usize) -> String {
    if title.len() + 2 >= width {
        return border.to_string();
    }
    let left = (width - title.len()) / 2;
    let right = width - left - title.len();
    format!(
        "{}{}{}",
        &"\u{2500}".repeat(left),
        title,
        &"\u{2500}".repeat(right)
    )
}
```

### Step 7: Implement process plugin interception support

Extend the process plugin protocol handler to support the `intercept` message type.

```rust
// In crates/shux-plugin/src/process.rs (modifications)
//
// The process plugin handler needs to implement the InterceptorInvoker
// trait for process plugins. This sends the "intercept" JSON message
// and waits for the "result" response.

/// Process plugin intercept message (Host -> Plugin):
/// ```json
/// {
///     "type": "intercept",
///     "id": "req-42",
///     "event": {
///         "type": "pane.input",
///         "pane_id": "p-1",
///         "data": "rm -rf /"
///     }
/// }
/// ```
///
/// Expected response (Plugin -> Host):
/// ```json
/// // Pass (possibly modified):
/// {"type": "result", "id": "req-42", "data": {"event": {"type": "pane.input", ...}}}
///
/// // Block:
/// {"type": "result", "id": "req-42", "data": null}
///
/// // Error:
/// {"type": "result", "id": "req-42", "error": {"code": -1, "message": "..."}}
/// ```
```

### Step 8: Wire up the Wasm interceptor invoker

Implement the `InterceptorInvoker` trait for Wasm plugins using wasmtime.

```rust
// In crates/shux-plugin/src/interception.rs

/// Wasm plugin interceptor invoker.
///
/// Calls the `intercept-event` function in the plugin's Wasm module
/// via wasmtime. Uses epoch interruption for timeout enforcement.
///
/// The WIT signature:
/// ```wit
/// intercept-event: func(event-json: string) -> result<option<string>, plugin-error>;
/// ```
///
/// Return value semantics:
/// - `Ok(Some(json))` -> pass (forward modified event)
/// - `Ok(None)` -> block (terminate chain)
/// - `Err(plugin-error)` -> error (fail-closed: block + mark degraded)
///
/// Epoch interruption is set up before each call:
/// ```ignore
/// store.set_epoch_deadline(1);
/// engine.increment_epoch(); // start the clock
///
/// // A background task increments the epoch after 100ms:
/// tokio::spawn(async move {
///     tokio::time::sleep(Duration::from_millis(100)).await;
///     engine.increment_epoch();
/// });
/// ```
///
/// When the epoch deadline is exceeded, wasmtime traps with
/// `Trap::Interrupt`, which we catch and convert to
/// `InterceptionError::Timeout`.
```

### Step 9: Write tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Mock interceptor invoker for testing.
    struct MockInvoker {
        responses: Vec<Result<Option<String>, InterceptionError>>,
        call_count: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl InterceptorInvoker for MockInvoker {
        async fn invoke_intercept(
            &self,
            _plugin_id: &str,
            event_json: &str,
        ) -> Result<Option<String>, InterceptionError> {
            let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
            self.responses[idx].clone()
        }
    }

    #[tokio::test]
    async fn test_empty_chain_passes_through() {
        let registry = Arc::new(RwLock::new(InterceptorRegistry::new()));
        let invoker = Arc::new(MockInvoker {
            responses: vec![],
            call_count: AtomicUsize::new(0),
        });
        let chain = InterceptionChain::new(registry, invoker);

        let event = InterceptedEvent {
            event_type: "pane.input".to_string(),
            event_json: r#"{"type":"pane.input","data":"ls"}"#.to_string(),
        };

        match chain.execute(event).await {
            ChainOutcome::Passed(_) => {} // expected
            other => panic!("Expected Passed, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_single_interceptor_pass() {
        let mut reg = InterceptorRegistry::new();
        reg.register("danger-zone".to_string(), vec!["pane.input".to_string()], 0);
        let registry = Arc::new(RwLock::new(reg));

        let json = r#"{"type":"pane.input","data":"ls"}"#.to_string();
        let invoker = Arc::new(MockInvoker {
            responses: vec![Ok(Some(json.clone()))],
            call_count: AtomicUsize::new(0),
        });
        let chain = InterceptionChain::new(registry, invoker);

        let event = InterceptedEvent {
            event_type: "pane.input".to_string(),
            event_json: json,
        };

        match chain.execute(event).await {
            ChainOutcome::Passed(e) => {
                assert_eq!(e.event_type, "pane.input");
            }
            other => panic!("Expected Passed, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_interceptor_blocks_event() {
        let mut reg = InterceptorRegistry::new();
        reg.register("danger-zone".to_string(), vec!["pane.input".to_string()], 0);
        let registry = Arc::new(RwLock::new(reg));

        let invoker = Arc::new(MockInvoker {
            responses: vec![Ok(None)], // block
            call_count: AtomicUsize::new(0),
        });
        let chain = InterceptionChain::new(registry, invoker);

        let event = InterceptedEvent {
            event_type: "pane.input".to_string(),
            event_json: r#"{"type":"pane.input","data":"rm -rf /"}"#.to_string(),
        };

        match chain.execute(event).await {
            ChainOutcome::Blocked { blocked_by, chain_position } => {
                assert_eq!(blocked_by, "danger-zone");
                assert_eq!(chain_position, 0);
            }
            other => panic!("Expected Blocked, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_interceptor_error_fails_closed() {
        let mut reg = InterceptorRegistry::new();
        reg.register("buggy-plugin".to_string(), vec!["pane.input".to_string()], 0);
        let registry = Arc::new(RwLock::new(reg));

        let invoker = Arc::new(MockInvoker {
            responses: vec![Err(InterceptionError::Crashed("segfault".to_string()))],
            call_count: AtomicUsize::new(0),
        });
        let chain = InterceptionChain::new(registry, invoker.clone());

        let event = InterceptedEvent {
            event_type: "pane.input".to_string(),
            event_json: r#"{"type":"pane.input","data":"echo hello"}"#.to_string(),
        };

        match chain.execute(event).await {
            ChainOutcome::FailedClosed { failed_plugin, .. } => {
                assert_eq!(failed_plugin, "buggy-plugin");
            }
            other => panic!("Expected FailedClosed, got {:?}", other),
        }

        // Verify plugin is marked degraded
        // (In a real test, we'd check the registry)
    }

    #[tokio::test]
    async fn test_chain_modification_propagates() {
        let mut reg = InterceptorRegistry::new();
        reg.register("plugin-a".to_string(), vec!["pane.input".to_string()], 0);
        reg.register("plugin-b".to_string(), vec!["pane.input".to_string()], 1);
        let registry = Arc::new(RwLock::new(reg));

        // Plugin A modifies the event, Plugin B receives the modified version
        let modified = r#"{"type":"pane.input","data":"safe-ls"}"#.to_string();
        let final_json = modified.clone();
        let invoker = Arc::new(MockInvoker {
            responses: vec![
                Ok(Some(modified)),      // Plugin A modifies
                Ok(Some(final_json)),    // Plugin B passes through
            ],
            call_count: AtomicUsize::new(0),
        });
        let chain = InterceptionChain::new(registry, invoker);

        let event = InterceptedEvent {
            event_type: "pane.input".to_string(),
            event_json: r#"{"type":"pane.input","data":"ls"}"#.to_string(),
        };

        match chain.execute(event).await {
            ChainOutcome::Passed(e) => {
                assert!(e.event_json.contains("safe-ls"));
            }
            other => panic!("Expected Passed, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_degraded_plugin_bypassed() {
        let mut reg = InterceptorRegistry::new();
        reg.register("degraded-plugin".to_string(), vec!["pane.input".to_string()], 0);
        reg.register("healthy-plugin".to_string(), vec!["pane.input".to_string()], 1);
        reg.mark_degraded("degraded-plugin", "previous crash".to_string());
        let registry = Arc::new(RwLock::new(reg));

        let json = r#"{"type":"pane.input","data":"echo hello"}"#.to_string();
        let invoker = Arc::new(MockInvoker {
            // Only 1 response because degraded-plugin is skipped
            responses: vec![Ok(Some(json.clone()))],
            call_count: AtomicUsize::new(0),
        });
        let chain = InterceptionChain::new(registry, invoker);

        let event = InterceptedEvent {
            event_type: "pane.input".to_string(),
            event_json: json,
        };

        match chain.execute(event).await {
            ChainOutcome::Passed(_) => {} // expected
            other => panic!("Expected Passed, got {:?}", other),
        }
    }

    // --- Input gate tests ---

    #[test]
    fn test_input_buffer_line_detection() {
        let mut buf = PaneInputBuffer::new(true);

        // Partial input, no newline
        let lines = buf.append(b"echo hel");
        assert!(lines.is_empty());

        // Complete the line with Enter (\r)
        let lines = buf.append(b"lo\r");
        assert_eq!(lines.len(), 1);
        assert_eq!(&lines[0], b"echo hello\r");
    }

    #[test]
    fn test_input_buffer_multiple_lines() {
        let mut buf = PaneInputBuffer::new(true);

        let lines = buf.append(b"cmd1\rcmd2\r");
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn test_fast_path_no_interceptors() {
        let mut gate = InputGate::new();
        let (tx, _rx) = mpsc::channel(16);
        gate.register_pane("p-1".to_string(), false, tx);

        match gate.process_input("p-1", b"echo hello\r") {
            InputProcessResult::ForwardImmediately(data) => {
                assert_eq!(&data, b"echo hello\r");
            }
            other => panic!("Expected ForwardImmediately, got {:?}", other),
        }
    }
}
```

---

## Verification

### Functional

```bash
# Build the interception module
cargo build -p shux-plugin 2>&1 | tail -5

# Build the input gate module
cargo build -p shux-core 2>&1 | tail -5

# Verify the interception chain types compile
cargo check -p shux-plugin

# Verify no clippy warnings
cargo clippy -p shux-plugin -p shux-core -- -D warnings
```

### Tests

```bash
# Run interception chain tests
cargo nextest run -p shux-plugin interception

# Run input gate tests
cargo nextest run -p shux-core input_gate

# Run all tests
cargo nextest run --workspace

# Specifically test chain semantics
cargo nextest run -p shux-plugin test_empty_chain_passes_through
cargo nextest run -p shux-plugin test_interceptor_blocks_event
cargo nextest run -p shux-plugin test_interceptor_error_fails_closed
cargo nextest run -p shux-plugin test_chain_modification_propagates
cargo nextest run -p shux-plugin test_degraded_plugin_bypassed
```

---

## Completion Criteria

- [ ] `InterceptorRegistry` tracks interceptor plugins ordered by config position
- [ ] `InterceptionChain::execute` runs interceptors sequentially in config order
- [ ] Returning `None` from an interceptor blocks the event and terminates the chain
- [ ] Returning a modified event passes the modified version to the next interceptor
- [ ] All interceptors passing results in `ChainOutcome::Passed` with the cumulative modifications
- [ ] Timeout (100ms default) triggers `ChainOutcome::FailedClosed` and marks the plugin degraded
- [ ] Plugin errors trigger `ChainOutcome::FailedClosed` and mark the plugin degraded
- [ ] Degraded plugins are bypassed in subsequent chain executions with a warning
- [ ] `mark_healthy` restores a degraded plugin after reload
- [ ] `InputGate` buffers input and detects line boundaries (newline detection)
- [ ] Fast path: panes without interceptors forward input with zero overhead
- [ ] Blocked lines trigger a line-kill sequence to the PTY
- [ ] Event bus routes interceptable event types through the chain before broadcasting
- [ ] Host-rendered overlay displays interception failure messages to the user
- [ ] Process plugin protocol supports the `intercept` message type
- [ ] All tests pass: chain pass-through, block, error/fail-closed, modification propagation, degraded bypass
- [ ] No clippy warnings

---

## Commit Message

```
feat(plugin): implement event interception chain with fail-closed semantics

- Sequential interception chain executing plugins in config order
- Interceptors can pass, modify, or block events flowing through the chain
- Per-interceptor 100ms timeout via epoch interruption (Wasm) / process timeout
- Fail-closed: crashed/timed-out interceptors block the event and are marked degraded
- InputGate buffers keystrokes and fires interception on line submission (Enter)
- Line-kill sequence sent to PTY when input is blocked to clear partial echo
- Host-rendered overlay for interception failure notification
- Degraded plugins bypassed with warning until hot-reloaded
```

---

## Session Protocol

1. **Before starting:** Read tasks 041 (plugin lifecycle) and 036 (event bus) to understand the integration points. Read PRD section 7.2a carefully for the exact fail-closed semantics and input interception timing. Read the Danger Zone and Pane Sync use cases for concrete examples of interception.
2. **During:** Implement in order: types (Step 1) -> registry (Step 2) -> chain executor (Step 3) -> input gate (Step 4) -> bus integration (Step 5) -> overlay (Step 6) -> process plugin support (Step 7) -> Wasm support (Step 8) -> tests (Step 9). Run `cargo check` after each step. Run tests after Steps 3, 4, and 9.
3. **After:** Run the full verification suite. Verify that the interception chain correctly handles all five scenarios: empty chain, pass-through, block, error/fail-closed, and modification propagation. Verify the input gate correctly buffers and detects line boundaries. Update `docs/PROGRESS.md` (mark 042 done). Update `CLAUDE.md` Learnings with any insights about async timeout handling with wasmtime epoch interruption.
