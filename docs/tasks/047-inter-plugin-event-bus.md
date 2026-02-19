# 047 — Inter-Plugin Event Bus

**Status:** Pending
**Depends On:** 041, 036
**Parallelizable With:** 046

---

## Problem

Plugins need to communicate with each other without tight coupling. Smart Context detects the project language and emits `context.detected` -- Danger Zone subscribes to this to apply language-specific blocklists. Workspace Profiles emits `workspace.opened` -- SSH Tunnels auto-starts tunnels, Smart Context pre-populates tags, and the theme plugin switches themes. Git Worktree Manager emits `worktree.opened` -- Smart Context picks up the branch info.

Without an inter-plugin communication mechanism, plugins would need to know about each other's internal APIs, creating brittle dependencies. The inter-plugin event bus provides a namespaced pub/sub system where plugins emit custom events via `emit-event` and other plugins subscribe to them via the standard event subscription mechanism. Events flow through the existing core event bus as `plugin.event` typed events, maintaining the single-source-of-truth event model.

The bus must enforce schema versioning conventions (emitters include a "version" field, consumers check version and ignore unknown fields), log warnings when events are emitted with no consumers (catches typos), and support the specific inter-plugin contracts defined in the use cases document.

## PRD Reference

- **section 7.2** — Inter-plugin bus extension point: "Namespaced pub/sub between plugins"
- **section 7.5** — `emit-event` WIT function: `func(event-type: string, data-json: string) -> result<_, host-error>`
- **section 21** — Event taxonomy: `plugin.event {plugin_id, event_type, data}`
- **Use cases document** — Inter-Plugin Event Contracts table: context.detected, workspace.opened, worktree.opened, palette.register

---

## Files to Create

- `crates/shux-plugin/src/inter_plugin.rs` — Inter-plugin event system: event namespacing, schema version validation, consumer tracking, no-consumer warnings, contract documentation

## Files to Modify

- `crates/shux-plugin/src/lib.rs` — Add `pub mod inter_plugin;`
- `crates/shux-core/src/bus.rs` — Add inter-plugin event routing: wrap plugin-emitted events as `plugin.event`, track consumers, emit no-consumer warnings
- `crates/shux-plugin/Cargo.toml` — Add dependencies if needed

---

## Execution Steps

### Step 1: Define inter-plugin event types in `crates/shux-plugin/src/inter_plugin.rs`

Define the types for inter-plugin communication, including the well-known event contracts.

```rust
use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Identifies a plugin.
pub type PluginId = String;

/// A plugin-emitted event that flows on the inter-plugin bus.
///
/// When a plugin calls `emit-event(event_type, data_json)`, the host
/// wraps it as a `plugin.event` on the core event bus:
///
/// ```json
/// {
///     "type": "plugin.event",
///     "plugin_id": "com.example.smart-context",
///     "event_type": "context.detected",
///     "data": {
///         "version": 1,
///         "pane_id": "p-1",
///         "lang": "rust",
///         "project": "shux",
///         "branch": "feat/plugins",
///         "framework": null
///     }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterPluginEvent {
    /// The plugin that emitted this event.
    pub plugin_id: PluginId,

    /// The event type (e.g., "context.detected", "workspace.opened").
    /// This is the custom event type, NOT "plugin.event" (which is the
    /// wrapper event type on the core bus).
    pub event_type: String,

    /// The event payload as arbitrary JSON.
    /// Must include a "version" field per the schema versioning convention.
    pub data: serde_json::Value,
}

/// Well-known inter-plugin event contracts.
///
/// These are the documented event types that plugins emit and consume.
/// The host tracks these for consumer matching and no-consumer warnings.
pub mod contracts {
    /// Emitted by Smart Context when project context is detected for a pane.
    ///
    /// Schema v1:
    /// ```json
    /// {
    ///     "version": 1,
    ///     "pane_id": "p-1",
    ///     "lang": "rust",
    ///     "project": "shux",
    ///     "branch": "feat/plugins",
    ///     "framework": null
    /// }
    /// ```
    ///
    /// Consumers: Danger Zone, status bar, theme plugins
    pub const CONTEXT_DETECTED: &str = "context.detected";

    /// Emitted by Workspace Profiles when a workspace is opened.
    ///
    /// Schema v1:
    /// ```json
    /// {
    ///     "version": 1,
    ///     "name": "shux-dev",
    ///     "cwd": "~/code/shux",
    ///     "session_id": "s-1",
    ///     "pane_ids": ["p-1", "p-2", "p-3"]
    /// }
    /// ```
    ///
    /// Consumers: Smart Context, SSH Tunnels, theme plugins
    pub const WORKSPACE_OPENED: &str = "workspace.opened";

    /// Emitted by Git Worktree Manager when a worktree is opened.
    ///
    /// Schema v1:
    /// ```json
    /// {
    ///     "version": 1,
    ///     "branch": "feat/plugins",
    ///     "path": "/home/user/code/shux-feat-plugins",
    ///     "pane_id": "p-5"
    /// }
    /// ```
    ///
    /// Consumers: Smart Context
    pub const WORKTREE_OPENED: &str = "worktree.opened";

    /// Emitted by any plugin to register an entry in the Command Palette.
    ///
    /// Schema v1:
    /// ```json
    /// {
    ///     "version": 1,
    ///     "entries": [
    ///         {
    ///             "label": "Git: Switch Branch",
    ///             "command": "git-worktree.switch",
    ///             "category": "Git"
    ///         }
    ///     ]
    /// }
    /// ```
    ///
    /// Consumers: Command Palette
    pub const PALETTE_REGISTER: &str = "palette.register";
}

/// Schema version validation result.
#[derive(Debug)]
pub enum SchemaValidation {
    /// Schema version is present and valid.
    Valid { version: u64 },

    /// Schema version field is missing from the event data.
    MissingVersion,

    /// Schema version field is not a number.
    InvalidVersionFormat,
}

/// Validate the schema version in an event's data payload.
pub fn validate_schema_version(data: &serde_json::Value) -> SchemaValidation {
    match data.get("version") {
        Some(serde_json::Value::Number(n)) => {
            if let Some(v) = n.as_u64() {
                SchemaValidation::Valid { version: v }
            } else {
                SchemaValidation::InvalidVersionFormat
            }
        }
        Some(_) => SchemaValidation::InvalidVersionFormat,
        None => SchemaValidation::MissingVersion,
    }
}
```

### Step 2: Implement the inter-plugin event tracker

Track which plugins emit and consume which event types. This enables no-consumer warnings and helps plugin developers catch typos in event type names.

```rust
/// Tracks inter-plugin event producers and consumers.
///
/// Used to:
/// 1. Warn when an event is emitted with no consumers (catches typos).
/// 2. Provide observability into the inter-plugin communication graph.
/// 3. Validate schema version conventions.
pub struct InterPluginTracker {
    /// Map from event type to the set of plugins that have subscribed to it.
    /// A plugin subscribes to a custom event type by subscribing to
    /// "plugin.event" and filtering on the event_type field.
    consumers: HashMap<String, HashSet<PluginId>>,

    /// Map from event type to the set of plugins that have emitted it.
    /// Tracked for observability.
    producers: HashMap<String, HashSet<PluginId>>,

    /// Count of events emitted per event type (for metrics).
    emit_counts: HashMap<String, u64>,

    /// Event types that have been warned about having no consumers.
    /// Prevents repeated warnings for the same event type.
    warned_no_consumers: HashSet<String>,
}

impl InterPluginTracker {
    pub fn new() -> Self {
        Self {
            consumers: HashMap::new(),
            producers: HashMap::new(),
            emit_counts: HashMap::new(),
            warned_no_consumers: HashSet::new(),
        }
    }

    /// Register a plugin as a consumer of a custom event type.
    ///
    /// Called when a plugin subscribes to events and the subscription
    /// filter includes "plugin.event" with a specific event_type.
    pub fn register_consumer(&mut self, event_type: &str, plugin_id: &str) {
        self.consumers
            .entry(event_type.to_string())
            .or_default()
            .insert(plugin_id.to_string());

        // Clear the no-consumer warning if it was previously set.
        self.warned_no_consumers.remove(event_type);

        debug!(
            event_type = event_type,
            plugin_id = plugin_id,
            "Inter-plugin event consumer registered"
        );
    }

    /// Unregister a plugin as a consumer of all event types.
    /// Called when a plugin is disabled or unloaded.
    pub fn unregister_consumer(&mut self, plugin_id: &str) {
        for consumers in self.consumers.values_mut() {
            consumers.remove(plugin_id);
        }
        // Clean up empty sets.
        self.consumers.retain(|_, consumers| !consumers.is_empty());
    }

    /// Record an event emission and check for consumers.
    ///
    /// Returns `true` if there are consumers, `false` if there are none
    /// (in which case the caller should log a warning).
    pub fn record_emission(&mut self, event_type: &str, plugin_id: &str) -> bool {
        // Track producer.
        self.producers
            .entry(event_type.to_string())
            .or_default()
            .insert(plugin_id.to_string());

        // Increment counter.
        *self.emit_counts.entry(event_type.to_string()).or_default() += 1;

        // Check for consumers.
        let has_consumers = self
            .consumers
            .get(event_type)
            .map(|c| !c.is_empty())
            .unwrap_or(false);

        if !has_consumers && !self.warned_no_consumers.contains(event_type) {
            warn!(
                event_type = event_type,
                emitting_plugin = plugin_id,
                "Inter-plugin event emitted with no consumers — \
                 this may be a typo in the event type name. \
                 Expected consumers should subscribe to 'plugin.event' \
                 with event_type filter '{}'",
                event_type
            );
            self.warned_no_consumers.insert(event_type.to_string());
        }

        has_consumers
    }

    /// Get the set of consumers for an event type.
    pub fn consumers_for(&self, event_type: &str) -> Vec<&str> {
        self.consumers
            .get(event_type)
            .map(|c| c.iter().map(|s| s.as_str()).collect())
            .unwrap_or_default()
    }

    /// Get the set of producers for an event type.
    pub fn producers_for(&self, event_type: &str) -> Vec<&str> {
        self.producers
            .get(event_type)
            .map(|p| p.iter().map(|s| s.as_str()).collect())
            .unwrap_or_default()
    }

    /// Get the emission count for an event type.
    pub fn emit_count(&self, event_type: &str) -> u64 {
        self.emit_counts.get(event_type).copied().unwrap_or(0)
    }

    /// Get the inter-plugin communication graph for diagnostics.
    pub fn communication_graph(&self) -> Vec<InterPluginLink> {
        let mut links = Vec::new();

        for (event_type, producers) in &self.producers {
            if let Some(consumers) = self.consumers.get(event_type) {
                for producer in producers {
                    for consumer in consumers {
                        links.push(InterPluginLink {
                            event_type: event_type.clone(),
                            producer: producer.clone(),
                            consumer: consumer.clone(),
                            emit_count: self.emit_count(event_type),
                        });
                    }
                }
            }
        }

        links
    }

    /// Check for known contract violations.
    ///
    /// Validates that well-known event types have expected consumers.
    /// Returns a list of warnings.
    pub fn check_contracts(&self) -> Vec<String> {
        let mut warnings = Vec::new();

        // Check well-known contracts.
        let expected_contracts = [
            (
                contracts::CONTEXT_DETECTED,
                "Smart Context",
                &["Danger Zone", "status bar"][..],
            ),
            (
                contracts::WORKSPACE_OPENED,
                "Workspace Profiles",
                &["Smart Context", "SSH Tunnels"][..],
            ),
        ];

        for (event_type, _producer, expected_consumers) in &expected_contracts {
            if self.producers.contains_key(*event_type)
                && !self.consumers.contains_key(*event_type)
            {
                warnings.push(format!(
                    "Event '{}' is emitted but has no consumers. Expected consumers: {:?}",
                    event_type, expected_consumers
                ));
            }
        }

        warnings
    }
}

/// A link in the inter-plugin communication graph.
#[derive(Debug, Clone, Serialize)]
pub struct InterPluginLink {
    pub event_type: String,
    pub producer: PluginId,
    pub consumer: PluginId,
    pub emit_count: u64,
}
```

### Step 3: Implement the emit-event host function handler

Handle the `emit-event` WIT function call from plugins.

```rust
use std::sync::Arc;
use tokio::sync::RwLock;

/// Handler for the emit-event host function.
///
/// When a plugin calls emit-event(event_type, data_json):
/// 1. Validate the schema version convention.
/// 2. Wrap the event as a plugin.event on the core bus.
/// 3. Record the emission for consumer tracking.
/// 4. Broadcast via the core event bus.
pub struct EmitEventHandler {
    tracker: Arc<RwLock<InterPluginTracker>>,
    // bus: reference to the core event bus
}

impl EmitEventHandler {
    pub fn new(tracker: Arc<RwLock<InterPluginTracker>>) -> Self {
        Self { tracker }
    }

    /// Handle an emit-event call from a plugin.
    ///
    /// # Arguments
    /// * `plugin_id` — The plugin emitting the event.
    /// * `event_type` — The custom event type (e.g., "context.detected").
    /// * `data_json` — The event payload as JSON.
    ///
    /// # Returns
    /// * `Ok(())` on success.
    /// * `Err(...)` if the data_json is invalid or the schema version is missing.
    pub async fn handle_emit(
        &self,
        plugin_id: &str,
        event_type: &str,
        data_json: &str,
    ) -> Result<(), EmitEventError> {
        // Parse the data JSON.
        let data: serde_json::Value = serde_json::from_str(data_json)
            .map_err(|e| EmitEventError::InvalidJson(e.to_string()))?;

        // Validate schema version convention.
        match validate_schema_version(&data) {
            SchemaValidation::Valid { version } => {
                debug!(
                    plugin_id = plugin_id,
                    event_type = event_type,
                    schema_version = version,
                    "Inter-plugin event schema version validated"
                );
            }
            SchemaValidation::MissingVersion => {
                warn!(
                    plugin_id = plugin_id,
                    event_type = event_type,
                    "Inter-plugin event missing 'version' field in data. \
                     Emitters should include a version field for schema compatibility."
                );
                // Don't fail -- just warn. The event still flows.
            }
            SchemaValidation::InvalidVersionFormat => {
                warn!(
                    plugin_id = plugin_id,
                    event_type = event_type,
                    "Inter-plugin event 'version' field is not a number."
                );
            }
        }

        // Record emission and check for consumers.
        {
            let mut tracker = self.tracker.write().await;
            tracker.record_emission(event_type, plugin_id);
        }

        // Construct the plugin.event wrapper.
        let plugin_event = InterPluginEvent {
            plugin_id: plugin_id.to_string(),
            event_type: event_type.to_string(),
            data,
        };

        self.bus.broadcast(Event::new(
            "plugin.event",
            serde_json::to_value(&plugin_event)?,
        ));

        info!(
            plugin_id = plugin_id,
            event_type = event_type,
            "Inter-plugin event emitted"
        );

        Ok(())
    }
}

/// Errors from the emit-event handler.
#[derive(Debug, thiserror::Error)]
pub enum EmitEventError {
    #[error("invalid JSON data: {0}")]
    InvalidJson(String),

    #[error("event bus error: {0}")]
    BusError(String),
}
```

### Step 4: Integrate with the core event bus

Modify the event bus to handle plugin.event events and route them to subscribing plugins.

```rust
// In crates/shux-core/src/bus.rs (modifications)
//
// The core event bus already supports typed events with filtering.
// Inter-plugin events are a special case:
//
// 1. When a plugin calls emit-event, the host creates a core event:
//    Event {
//        event_type: "plugin.event",
//        data: {
//            "plugin_id": "com.example.smart-context",
//            "event_type": "context.detected",
//            "data": { ... actual payload ... }
//        },
//        sequence: <next sequence number>,
//        timestamp: <now>,
//    }
//
// 2. This event is broadcast on the core bus like any other event.
//
// 3. Plugins that subscribed to "plugin.event" receive it.
//    They filter by the inner event_type field.
//
// 4. The inter-plugin tracker records producers and consumers
//    for diagnostics and no-consumer warnings.
//
// No special routing is needed: the existing event bus subscription
// and filtering mechanism handles inter-plugin events. The tracker
// provides the additional intelligence (warnings, diagnostics).
//
// Example subscription from a consumer plugin during init:
//
// Wasm plugin: subscribes to events with type "plugin.event" in its
// declared events list in plugin.toml:
//   events = ["plugin.event"]
//
// Then in on-event, it filters:
//   fn on_event(event_json: &str) {
//       let event: serde_json::Value = serde_json::from_str(event_json)?;
//       if event["event_type"] == "context.detected" {
//           let data = &event["data"];
//           let version = data["version"].as_u64().unwrap_or(0);
//           if version >= 1 {
//               // Process the event
//               let lang = data["lang"].as_str();
//               // ...
//           }
//       }
//       Ok(())
//   }
//
// Process plugin: subscribes via subscribe message:
//   {"type": "subscribe", "event_type": "plugin.event"}
```

### Step 5: Write tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_schema_version_valid() {
        let data = serde_json::json!({
            "version": 1,
            "lang": "rust",
            "project": "shux"
        });
        match validate_schema_version(&data) {
            SchemaValidation::Valid { version } => assert_eq!(version, 1),
            other => panic!("Expected Valid, got {:?}", other),
        }
    }

    #[test]
    fn test_validate_schema_version_missing() {
        let data = serde_json::json!({
            "lang": "rust",
            "project": "shux"
        });
        assert!(matches!(
            validate_schema_version(&data),
            SchemaValidation::MissingVersion
        ));
    }

    #[test]
    fn test_validate_schema_version_invalid_format() {
        let data = serde_json::json!({
            "version": "one",
            "lang": "rust"
        });
        assert!(matches!(
            validate_schema_version(&data),
            SchemaValidation::InvalidVersionFormat
        ));
    }

    #[test]
    fn test_register_consumer() {
        let mut tracker = InterPluginTracker::new();
        tracker.register_consumer("context.detected", "com.example.danger-zone");

        let consumers = tracker.consumers_for("context.detected");
        assert_eq!(consumers.len(), 1);
        assert_eq!(consumers[0], "com.example.danger-zone");
    }

    #[test]
    fn test_emit_with_consumers() {
        let mut tracker = InterPluginTracker::new();
        tracker.register_consumer("context.detected", "com.example.danger-zone");

        let has_consumers =
            tracker.record_emission("context.detected", "com.example.smart-context");
        assert!(has_consumers);
        assert_eq!(tracker.emit_count("context.detected"), 1);
    }

    #[test]
    fn test_emit_without_consumers_warns() {
        let mut tracker = InterPluginTracker::new();

        // First emission with no consumers should warn.
        let has_consumers =
            tracker.record_emission("typo.event", "com.example.buggy");
        assert!(!has_consumers);

        // Second emission should not repeat the warning.
        let has_consumers =
            tracker.record_emission("typo.event", "com.example.buggy");
        assert!(!has_consumers);
        assert_eq!(tracker.emit_count("typo.event"), 2);
    }

    #[test]
    fn test_unregister_consumer() {
        let mut tracker = InterPluginTracker::new();
        tracker.register_consumer("context.detected", "com.example.danger-zone");
        tracker.register_consumer("context.detected", "com.example.status-bar");

        tracker.unregister_consumer("com.example.danger-zone");

        let consumers = tracker.consumers_for("context.detected");
        assert_eq!(consumers.len(), 1);
        assert_eq!(consumers[0], "com.example.status-bar");
    }

    #[test]
    fn test_communication_graph() {
        let mut tracker = InterPluginTracker::new();
        tracker.register_consumer("context.detected", "com.example.danger-zone");
        tracker.record_emission("context.detected", "com.example.smart-context");

        let graph = tracker.communication_graph();
        assert_eq!(graph.len(), 1);
        assert_eq!(graph[0].event_type, "context.detected");
        assert_eq!(graph[0].producer, "com.example.smart-context");
        assert_eq!(graph[0].consumer, "com.example.danger-zone");
    }

    #[test]
    fn test_multiple_consumers() {
        let mut tracker = InterPluginTracker::new();
        tracker.register_consumer("workspace.opened", "com.example.smart-context");
        tracker.register_consumer("workspace.opened", "com.example.ssh-tunnels");
        tracker.register_consumer("workspace.opened", "com.example.theme-switcher");

        let consumers = tracker.consumers_for("workspace.opened");
        assert_eq!(consumers.len(), 3);

        let has_consumers =
            tracker.record_emission("workspace.opened", "com.example.workspace-profiles");
        assert!(has_consumers);
    }

    #[test]
    fn test_producers_tracked() {
        let mut tracker = InterPluginTracker::new();
        tracker.register_consumer("context.detected", "consumer-1");

        tracker.record_emission("context.detected", "producer-1");
        tracker.record_emission("context.detected", "producer-2");

        let producers = tracker.producers_for("context.detected");
        assert_eq!(producers.len(), 2);
    }

    #[test]
    fn test_no_consumer_warning_clears_on_register() {
        let mut tracker = InterPluginTracker::new();

        // Emit with no consumers (triggers warning).
        tracker.record_emission("my.event", "producer");
        assert!(tracker.warned_no_consumers.contains("my.event"));

        // Register a consumer.
        tracker.register_consumer("my.event", "consumer");
        assert!(!tracker.warned_no_consumers.contains("my.event"));
    }

    #[tokio::test]
    async fn test_emit_event_handler_valid_json() {
        let tracker = Arc::new(RwLock::new(InterPluginTracker::new()));
        let handler = EmitEventHandler::new(tracker.clone());

        let result = handler
            .handle_emit(
                "com.example.smart-context",
                "context.detected",
                r#"{"version": 1, "lang": "rust", "project": "shux"}"#,
            )
            .await;
        assert!(result.is_ok());

        let tracker = tracker.read().await;
        assert_eq!(tracker.emit_count("context.detected"), 1);
    }

    #[tokio::test]
    async fn test_emit_event_handler_invalid_json() {
        let tracker = Arc::new(RwLock::new(InterPluginTracker::new()));
        let handler = EmitEventHandler::new(tracker);

        let result = handler
            .handle_emit(
                "com.example.test",
                "test.event",
                "not valid json",
            )
            .await;
        assert!(matches!(result, Err(EmitEventError::InvalidJson(_))));
    }

    #[tokio::test]
    async fn test_emit_event_handler_missing_version_warns() {
        let tracker = Arc::new(RwLock::new(InterPluginTracker::new()));
        let handler = EmitEventHandler::new(tracker);

        // Should succeed but warn about missing version.
        let result = handler
            .handle_emit(
                "com.example.test",
                "test.event",
                r#"{"lang": "rust"}"#, // no version field
            )
            .await;
        assert!(result.is_ok()); // Still succeeds, just warns.
    }

    #[test]
    fn test_well_known_contracts() {
        // Verify contract constants are defined correctly.
        assert_eq!(contracts::CONTEXT_DETECTED, "context.detected");
        assert_eq!(contracts::WORKSPACE_OPENED, "workspace.opened");
        assert_eq!(contracts::WORKTREE_OPENED, "worktree.opened");
        assert_eq!(contracts::PALETTE_REGISTER, "palette.register");
    }
}
```

---

## Verification

### Functional

```bash
# Build the inter-plugin module
cargo build -p shux-plugin 2>&1 | tail -5

# Build the bus modifications
cargo build -p shux-core 2>&1 | tail -5

# Verify types compile
cargo check -p shux-plugin -p shux-core

# Verify no clippy warnings
cargo clippy -p shux-plugin -p shux-core -- -D warnings
```

### Tests

```bash
# Run inter-plugin tests
cargo nextest run -p shux-plugin inter_plugin

# Run specific tests
cargo nextest run -p shux-plugin test_emit_with_consumers
cargo nextest run -p shux-plugin test_emit_without_consumers_warns
cargo nextest run -p shux-plugin test_communication_graph
cargo nextest run -p shux-plugin test_emit_event_handler_valid_json

# Run all tests
cargo nextest run --workspace
```

---

## Completion Criteria

- [ ] `InterPluginEvent` type wraps plugin-emitted events as `plugin.event` on the core bus
- [ ] Schema version validation: warns when `version` field is missing from event data
- [ ] Schema version validation: warns when `version` field is not a number
- [ ] `InterPluginTracker` tracks producers and consumers per event type
- [ ] No-consumer warning: logged once when an event is emitted with no subscribers
- [ ] No-consumer warning clears when a consumer registers
- [ ] `communication_graph()` provides observability into inter-plugin communication
- [ ] `EmitEventHandler` validates JSON, checks schema version, records emission, broadcasts
- [ ] Well-known contracts documented: context.detected, workspace.opened, worktree.opened, palette.register
- [ ] Consumer unregistration cleans up when a plugin is disabled/unloaded
- [ ] Core event bus broadcasts `plugin.event` events to subscribers
- [ ] Process plugin protocol supports emit_event and subscribe messages for inter-plugin events
- [ ] All tests pass
- [ ] No clippy warnings

---

## Commit Message

```
feat(plugin): implement inter-plugin event bus with schema versioning

- Plugins emit custom events via emit-event, broadcast as plugin.event on core bus
- Schema versioning convention: warns when version field is missing
- Inter-plugin tracker: tracks producers, consumers, and emission counts
- No-consumer warning: catches typos in event type names
- Communication graph for diagnostics and observability
- Well-known contracts: context.detected, workspace.opened, worktree.opened, palette.register
- Integrates with core event bus subscription and filtering
```

---

## Session Protocol

1. **Before starting:** Read task 041 (plugin lifecycle) for how plugins register event subscriptions. Read task 036 (event bus) for the broadcast and subscription mechanism. Read the use cases document's "Inter-Plugin Event Contracts" table for the exact event schemas. Pay special attention to the forward-compatibility rules (check version, ignore unknown fields).
2. **During:** Implement in order: types + contracts (Step 1) -> tracker (Step 2) -> emit handler (Step 3) -> bus integration (Step 4) -> tests (Step 5). Run `cargo check` after each step. Run tests after Steps 2, 3, and 5.
3. **After:** Run the full verification suite. Verify no-consumer warnings fire correctly. Verify schema version validation catches missing fields. Verify the communication graph accurately represents producer-consumer relationships. Update `docs/PROGRESS.md` (mark 047 done). Update `CLAUDE.md` Learnings with insights about event-driven plugin coupling patterns.
