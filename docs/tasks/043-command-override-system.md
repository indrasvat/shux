# 043 — Command Override System

**Status:** Pending
**Depends On:** 041
**Parallelizable With:** 042

---

## Problem

Plugins need the ability to replace built-in commands with custom implementations. This is inspired by VS Code's tool override pattern where extensions can provide alternative implementations of core functionality. For example, an auditing plugin might override the `pane.send_keys` command to log all input, or a workspace plugin might override `session.create` to apply project-specific defaults.

Without command overrides, plugins can only add new commands -- they cannot modify existing behavior. This limits the plugin system's power and forces users to choose between built-in behavior and plugin behavior rather than seamlessly composing them.

The override system must handle: registration during plugin init, permission enforcement (only commands listed in `override_commands` permission), last-loaded-wins conflict resolution, user notification of overrides, a `builtin.<name>` escape hatch for accessing the original implementation, and per-plugin config to disable overrides.

## PRD Reference

- **section 7.2** — Command overrides extension point: "Plugins can override built-in commands by registering the same name"
- **section 7.5** — `register-command-override` WIT function: `func(command-name: string) -> result<_, host-error>`
- **section 7.4** — Plugin manifest: `override_commands = []` permission listing specific command names
- **section 7.6** — Process plugin protocol: `{"type": "register_command_override", "command_name": "pane.create"}` message type
- **section 7.1** — Plugin design goals: "Overridable: Plugins can override built-in commands by registering the same name (PI's tool override pattern)"

---

## Files to Create

- `crates/shux-plugin/src/overrides.rs` — Command override registry: registration, conflict detection, resolution, escape hatch routing, permission validation

## Files to Modify

- `crates/shux-plugin/src/lib.rs` — Add `pub mod overrides;`
- `crates/shux-rpc/src/router.rs` — Modify the command router to check for overrides before dispatching to built-in handlers
- `crates/shux-plugin/Cargo.toml` — Add dependencies if needed

---

## Execution Steps

### Step 1: Define override types in `crates/shux-plugin/src/overrides.rs`

Define the core types for the command override system: registration records, conflict tracking, and routing decisions.

```rust
use std::collections::HashMap;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{info, warn, error};

/// Identifies a plugin.
pub type PluginId = String;

/// A record of a single command override.
#[derive(Debug, Clone)]
pub struct OverrideRecord {
    /// The built-in command name being overridden (e.g., "session.create").
    pub command_name: String,

    /// The plugin providing the override.
    pub plugin_id: PluginId,

    /// When the override was registered.
    pub registered_at: Instant,

    /// Whether this override is currently active.
    /// Can be disabled per-plugin in config.
    pub active: bool,
}

/// Information about an override conflict (multiple plugins overriding the same command).
#[derive(Debug, Clone)]
pub struct OverrideConflict {
    /// The command name that is overridden by multiple plugins.
    pub command_name: String,

    /// All plugins that attempted to override this command, in load order.
    /// The last entry is the active override (last-loaded wins).
    pub plugins: Vec<PluginId>,
}

/// Decision made by the router when a command is invoked.
#[derive(Debug)]
pub enum RouteDecision {
    /// No override registered. Route to the built-in handler.
    Builtin,

    /// An active override exists. Route to the plugin's on-command handler.
    Override {
        plugin_id: PluginId,
        command_name: String,
    },

    /// The command was called via the "builtin.<name>" escape hatch.
    /// Always route to the built-in handler, ignoring any overrides.
    EscapeHatch {
        original_command_name: String,
    },
}

/// Errors from the override system.
#[derive(Debug, Error)]
pub enum OverrideError {
    #[error("command '{0}' is not a valid built-in command")]
    NotBuiltinCommand(String),

    #[error("plugin '{plugin_id}' does not have override_commands permission for '{command_name}'")]
    PermissionDenied {
        plugin_id: String,
        command_name: String,
    },

    #[error("plugin '{plugin_id}' is disabled for overrides by user config")]
    DisabledByConfig {
        plugin_id: String,
    },

    #[error("command '{0}' cannot be overridden (protected)")]
    ProtectedCommand(String),
}
```

### Step 2: Implement the override registry

The registry tracks all registered overrides, resolves conflicts, and provides routing decisions.

```rust
/// The prefix for the escape hatch. When a command is called as
/// "builtin.session.create", it always routes to the built-in handler.
const BUILTIN_PREFIX: &str = "builtin.";

/// Commands that cannot be overridden for safety reasons.
/// These are critical system commands where overriding could
/// break fundamental daemon operation.
const PROTECTED_COMMANDS: &[&str] = &[
    "admin.shutdown",
    "admin.gc",
    "system.version",
    "system.health",
    "plugin.list",
    "plugin.enable",
    "plugin.disable",
    "plugin.reload",
    "plugin.inspect",
    "events.watch",
    "events.history",
    "config.get",
    "config.validate",
    "log.set_level",
    "log.tail",
    "diagnose.run",
];

/// Registry of command overrides.
///
/// Tracks which plugins have overridden which built-in commands.
/// When multiple plugins override the same command, last-loaded wins
/// (as determined by load order from the `[plugins]` config section).
pub struct OverrideRegistry {
    /// Map from command name to the stack of override records.
    /// The last entry in the Vec is the active override (last-loaded wins).
    /// Earlier entries are shadowed but retained for conflict reporting.
    overrides: HashMap<String, Vec<OverrideRecord>>,

    /// Set of valid built-in command names, used for validation.
    builtin_commands: Vec<String>,

    /// Per-plugin override disable flags from user config.
    /// If a plugin ID is in this set, its overrides are inactive.
    disabled_plugins: Vec<PluginId>,
}

impl OverrideRegistry {
    /// Create a new registry with the set of valid built-in command names.
    pub fn new(builtin_commands: Vec<String>) -> Self {
        Self {
            overrides: HashMap::new(),
            builtin_commands,
            disabled_plugins: Vec::new(),
        }
    }

    /// Register a command override.
    ///
    /// # Arguments
    /// * `plugin_id` — The plugin registering the override.
    /// * `command_name` — The built-in command to override.
    /// * `permitted_commands` — The list of commands this plugin is permitted
    ///   to override (from `plugin.toml` `override_commands`).
    ///
    /// # Returns
    /// * `Ok(Option<OverrideConflict>)` — Success. Returns conflict info if
    ///   another plugin already overrides this command.
    /// * `Err(OverrideError)` — Registration failed.
    pub fn register(
        &mut self,
        plugin_id: PluginId,
        command_name: String,
        permitted_commands: &[String],
    ) -> Result<Option<OverrideConflict>, OverrideError> {
        // Validate: command must be a real built-in command.
        if !self.builtin_commands.contains(&command_name) {
            return Err(OverrideError::NotBuiltinCommand(command_name));
        }

        // Validate: command must not be protected.
        if PROTECTED_COMMANDS.contains(&command_name.as_str()) {
            return Err(OverrideError::ProtectedCommand(command_name));
        }

        // Validate: plugin must have permission to override this specific command.
        if !permitted_commands.contains(&command_name) {
            return Err(OverrideError::PermissionDenied {
                plugin_id,
                command_name,
            });
        }

        // Check if the plugin is disabled by user config.
        let active = !self.disabled_plugins.contains(&plugin_id);

        let record = OverrideRecord {
            command_name: command_name.clone(),
            plugin_id: plugin_id.clone(),
            registered_at: Instant::now(),
            active,
        };

        let stack = self.overrides.entry(command_name.clone()).or_default();

        // Check for existing override by the same plugin (replace).
        stack.retain(|r| r.plugin_id != plugin_id);

        // Detect conflict before adding.
        let conflict = if !stack.is_empty() {
            let mut plugins: Vec<PluginId> =
                stack.iter().map(|r| r.plugin_id.clone()).collect();
            plugins.push(plugin_id.clone());

            warn!(
                command = %command_name,
                overriding_plugin = %plugin_id,
                existing_plugins = ?&plugins[..plugins.len()-1],
                "Command override conflict — last-loaded wins"
            );

            Some(OverrideConflict {
                command_name: command_name.clone(),
                plugins,
            })
        } else {
            info!(
                command = %command_name,
                plugin = %plugin_id,
                "Command override registered"
            );
            None
        };

        stack.push(record);

        Ok(conflict)
    }

    /// Unregister all overrides from a plugin.
    /// Called when a plugin is disabled, unloaded, or hot-reloaded.
    pub fn unregister_plugin(&mut self, plugin_id: &str) {
        for stack in self.overrides.values_mut() {
            stack.retain(|r| r.plugin_id != plugin_id);
        }
        // Remove empty stacks.
        self.overrides.retain(|_, stack| !stack.is_empty());

        info!(plugin = plugin_id, "All command overrides unregistered");
    }

    /// Determine how to route a command invocation.
    ///
    /// # Routing logic
    /// 1. If the command starts with "builtin.", strip the prefix and
    ///    route to the built-in handler (escape hatch).
    /// 2. If an active override exists, route to the overriding plugin.
    /// 3. Otherwise, route to the built-in handler.
    pub fn route(&self, command_name: &str) -> RouteDecision {
        // Check for escape hatch: "builtin.session.create" -> "session.create"
        if let Some(original) = command_name.strip_prefix(BUILTIN_PREFIX) {
            return RouteDecision::EscapeHatch {
                original_command_name: original.to_string(),
            };
        }

        // Check for active override.
        if let Some(stack) = self.overrides.get(command_name) {
            // Find the last active override (last-loaded wins).
            if let Some(record) = stack.iter().rev().find(|r| r.active) {
                return RouteDecision::Override {
                    plugin_id: record.plugin_id.clone(),
                    command_name: command_name.to_string(),
                };
            }
        }

        RouteDecision::Builtin
    }

    /// Disable all overrides from a specific plugin (user config).
    pub fn disable_plugin_overrides(&mut self, plugin_id: &str) {
        if !self.disabled_plugins.contains(&plugin_id.to_string()) {
            self.disabled_plugins.push(plugin_id.to_string());
        }
        // Mark existing overrides as inactive.
        for stack in self.overrides.values_mut() {
            for record in stack.iter_mut() {
                if record.plugin_id == plugin_id {
                    record.active = false;
                    info!(
                        command = %record.command_name,
                        plugin = plugin_id,
                        "Override disabled by user config"
                    );
                }
            }
        }
    }

    /// Re-enable overrides from a specific plugin.
    pub fn enable_plugin_overrides(&mut self, plugin_id: &str) {
        self.disabled_plugins.retain(|id| id != plugin_id);
        for stack in self.overrides.values_mut() {
            for record in stack.iter_mut() {
                if record.plugin_id == plugin_id {
                    record.active = true;
                }
            }
        }
    }

    /// List all active overrides.
    pub fn list_active_overrides(&self) -> Vec<&OverrideRecord> {
        self.overrides
            .values()
            .filter_map(|stack| stack.iter().rev().find(|r| r.active))
            .collect()
    }

    /// List all conflicts (commands overridden by multiple plugins).
    pub fn list_conflicts(&self) -> Vec<OverrideConflict> {
        self.overrides
            .iter()
            .filter(|(_, stack)| stack.len() > 1)
            .map(|(cmd, stack)| OverrideConflict {
                command_name: cmd.clone(),
                plugins: stack.iter().map(|r| r.plugin_id.clone()).collect(),
            })
            .collect()
    }

    /// Get the override record for a specific command, if any.
    pub fn get_override(&self, command_name: &str) -> Option<&OverrideRecord> {
        self.overrides
            .get(command_name)
            .and_then(|stack| stack.iter().rev().find(|r| r.active))
    }
}
```

### Step 3: Integrate with the RPC router

Modify the command router to check for overrides before dispatching to built-in handlers.

```rust
// In crates/shux-rpc/src/router.rs (modifications)
//
// The router is the central dispatch point for all JSON-RPC method calls.
// Before this task, it directly dispatches to built-in handlers.
// After this task, it first consults the OverrideRegistry.

use crate::override_registry; // imported from plugin crate or passed as dependency

/// Dispatch a JSON-RPC method call.
///
/// Routing order:
/// 1. Check for "builtin." prefix (escape hatch -> always built-in).
/// 2. Check OverrideRegistry for an active override (-> plugin on-command).
/// 3. Check for plugin-registered API methods (task 045).
/// 4. Fall back to built-in handler.
/// 5. If no handler found, return JSON-RPC error -32601 (Method not found).
///
/// ```ignore
/// pub async fn dispatch(
///     &self,
///     method: &str,
///     params: serde_json::Value,
/// ) -> Result<serde_json::Value, RpcError> {
///     match self.override_registry.route(method) {
///         RouteDecision::EscapeHatch { original_command_name } => {
///             // Always route to built-in, even if an override exists.
///             self.dispatch_builtin(&original_command_name, params).await
///         }
///         RouteDecision::Override { plugin_id, command_name } => {
///             // Route to the plugin's on-command handler.
///             let args = self.params_to_args(params);
///             self.plugin_host
///                 .invoke_command(&plugin_id, &command_name, args)
///                 .await
///                 .map_err(|e| RpcError::plugin_error(e))
///         }
///         RouteDecision::Builtin => {
///             self.dispatch_builtin(method, params).await
///         }
///     }
/// }
/// ```
///
/// The plugin's `on-command` callback receives:
/// - `name`: the original command name (e.g., "session.create")
/// - `args`: the JSON-RPC params serialized as a list of strings
///
/// The plugin returns a JSON string that the router deserializes
/// and returns as the JSON-RPC result.
```

### Step 4: Handle override events and notifications

When an override is registered, emit events to notify clients and provide observability.

```rust
// In crates/shux-plugin/src/overrides.rs (additional functions)

/// Events emitted by the override system.
///
/// These are emitted on the event bus so clients and other plugins
/// can observe override registrations and conflicts.

/// Emitted when a command override is registered.
/// Event type: "plugin.override_registered"
/// ```json
/// {
///     "command_name": "session.create",
///     "plugin_id": "com.example.workspace",
///     "replaced_plugin": null
/// }
/// ```
#[derive(Debug, Serialize)]
pub struct OverrideRegisteredEvent {
    pub command_name: String,
    pub plugin_id: String,
    /// If this override replaces another plugin's override, that plugin's ID.
    pub replaced_plugin: Option<String>,
}

/// Emitted when an override conflict is detected.
/// Event type: "plugin.override_conflict"
/// ```json
/// {
///     "command_name": "session.create",
///     "plugins": ["plugin-a", "plugin-b"],
///     "active_plugin": "plugin-b"
/// }
/// ```
#[derive(Debug, Serialize)]
pub struct OverrideConflictEvent {
    pub command_name: String,
    pub plugins: Vec<String>,
    pub active_plugin: String,
}

impl OverrideRegistry {
    /// Generate events for a newly registered override.
    pub fn generate_registration_events(
        &self,
        command_name: &str,
        plugin_id: &str,
        conflict: Option<&OverrideConflict>,
    ) -> Vec<OverrideRegisteredEvent> {
        let mut events = Vec::new();

        let replaced = conflict.and_then(|c| {
            c.plugins.iter().rev().nth(1).cloned()
        });

        events.push(OverrideRegisteredEvent {
            command_name: command_name.to_string(),
            plugin_id: plugin_id.to_string(),
            replaced_plugin: replaced,
        });

        events
    }
}
```

### Step 5: Handle override re-registration on hot reload

When a plugin is hot-reloaded, its overrides must be re-registered during the new `init` phase.

```rust
// In crates/shux-plugin/src/overrides.rs

impl OverrideRegistry {
    /// Prepare for a plugin hot reload.
    ///
    /// Called before the plugin is reloaded. Removes all existing overrides
    /// from the plugin. The plugin will re-register its overrides during
    /// the new `init` phase.
    pub fn prepare_reload(&mut self, plugin_id: &str) {
        self.unregister_plugin(plugin_id);
        info!(
            plugin = plugin_id,
            "Overrides cleared for hot reload — will be re-registered during init"
        );
    }

    /// Validate override permissions for a plugin against its manifest.
    ///
    /// Called during plugin loading to verify that the plugin's
    /// `override_commands` permission lists valid, non-protected commands.
    pub fn validate_permissions(
        &self,
        plugin_id: &str,
        override_commands: &[String],
    ) -> Vec<OverrideError> {
        let mut errors = Vec::new();

        for cmd in override_commands {
            if !self.builtin_commands.contains(cmd) {
                errors.push(OverrideError::NotBuiltinCommand(cmd.clone()));
            }
            if PROTECTED_COMMANDS.contains(&cmd.as_str()) {
                errors.push(OverrideError::ProtectedCommand(cmd.clone()));
            }
        }

        if !errors.is_empty() {
            warn!(
                plugin = plugin_id,
                errors = ?errors,
                "Plugin declares invalid override_commands permissions"
            );
        }

        errors
    }
}
```

### Step 6: Add process plugin protocol support

Ensure the process plugin protocol handler supports the `register_command_override` message.

```rust
// Process plugin sends:
// {"type": "register_command_override", "command_name": "session.create"}
//
// Host validates:
// 1. Plugin has override_commands permission for "session.create"
// 2. "session.create" is a valid, non-protected built-in command
//
// Host responds (via result to a previously sent register message, or
// as part of the registration phase after hello):
// Success: implicit (no error)
// Failure: {"type": "error", "code": -32600, "message": "Permission denied: ..."}
//
// The process plugin's register message can include command_overrides:
// {
//     "type": "register",
//     "commands": [...],
//     "segments": [...],
//     "command_overrides": ["session.create", "pane.split"],
//     ...
// }
```

### Step 7: Write tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn make_registry() -> OverrideRegistry {
        OverrideRegistry::new(vec![
            "session.create".to_string(),
            "session.kill".to_string(),
            "pane.split".to_string(),
            "pane.focus".to_string(),
            "pane.send_keys".to_string(),
            "admin.shutdown".to_string(), // protected
        ])
    }

    #[test]
    fn test_register_override_success() {
        let mut reg = make_registry();
        let result = reg.register(
            "my-plugin".to_string(),
            "session.create".to_string(),
            &["session.create".to_string()],
        );
        assert!(result.is_ok());
        assert!(result.unwrap().is_none()); // no conflict
    }

    #[test]
    fn test_register_override_not_builtin() {
        let mut reg = make_registry();
        let result = reg.register(
            "my-plugin".to_string(),
            "nonexistent.command".to_string(),
            &["nonexistent.command".to_string()],
        );
        assert!(matches!(result, Err(OverrideError::NotBuiltinCommand(_))));
    }

    #[test]
    fn test_register_override_protected() {
        let mut reg = make_registry();
        let result = reg.register(
            "my-plugin".to_string(),
            "admin.shutdown".to_string(),
            &["admin.shutdown".to_string()],
        );
        assert!(matches!(result, Err(OverrideError::ProtectedCommand(_))));
    }

    #[test]
    fn test_register_override_no_permission() {
        let mut reg = make_registry();
        let result = reg.register(
            "my-plugin".to_string(),
            "session.create".to_string(),
            &["pane.split".to_string()], // wrong permission
        );
        assert!(matches!(result, Err(OverrideError::PermissionDenied { .. })));
    }

    #[test]
    fn test_conflict_detection_last_loaded_wins() {
        let mut reg = make_registry();

        // Plugin A overrides session.create
        let result = reg.register(
            "plugin-a".to_string(),
            "session.create".to_string(),
            &["session.create".to_string()],
        );
        assert!(result.unwrap().is_none());

        // Plugin B also overrides session.create -> conflict
        let result = reg.register(
            "plugin-b".to_string(),
            "session.create".to_string(),
            &["session.create".to_string()],
        );
        let conflict = result.unwrap().expect("should have conflict");
        assert_eq!(conflict.command_name, "session.create");
        assert_eq!(conflict.plugins, vec!["plugin-a", "plugin-b"]);

        // Last-loaded (plugin-b) wins routing
        match reg.route("session.create") {
            RouteDecision::Override { plugin_id, .. } => {
                assert_eq!(plugin_id, "plugin-b");
            }
            other => panic!("Expected Override, got {:?}", other),
        }
    }

    #[test]
    fn test_escape_hatch_always_builtin() {
        let mut reg = make_registry();
        reg.register(
            "my-plugin".to_string(),
            "session.create".to_string(),
            &["session.create".to_string()],
        )
        .unwrap();

        // Normal call goes to override
        assert!(matches!(
            reg.route("session.create"),
            RouteDecision::Override { .. }
        ));

        // Escape hatch goes to builtin
        match reg.route("builtin.session.create") {
            RouteDecision::EscapeHatch { original_command_name } => {
                assert_eq!(original_command_name, "session.create");
            }
            other => panic!("Expected EscapeHatch, got {:?}", other),
        }
    }

    #[test]
    fn test_no_override_routes_builtin() {
        let reg = make_registry();
        assert!(matches!(reg.route("session.create"), RouteDecision::Builtin));
    }

    #[test]
    fn test_unregister_plugin_removes_overrides() {
        let mut reg = make_registry();
        reg.register(
            "my-plugin".to_string(),
            "session.create".to_string(),
            &["session.create".to_string()],
        )
        .unwrap();

        assert!(matches!(
            reg.route("session.create"),
            RouteDecision::Override { .. }
        ));

        reg.unregister_plugin("my-plugin");

        assert!(matches!(
            reg.route("session.create"),
            RouteDecision::Builtin
        ));
    }

    #[test]
    fn test_disable_plugin_overrides() {
        let mut reg = make_registry();
        reg.register(
            "my-plugin".to_string(),
            "session.create".to_string(),
            &["session.create".to_string()],
        )
        .unwrap();

        // Override is active
        assert!(matches!(
            reg.route("session.create"),
            RouteDecision::Override { .. }
        ));

        // User disables overrides for this plugin
        reg.disable_plugin_overrides("my-plugin");

        // Now routes to builtin
        assert!(matches!(
            reg.route("session.create"),
            RouteDecision::Builtin
        ));

        // Re-enable
        reg.enable_plugin_overrides("my-plugin");
        assert!(matches!(
            reg.route("session.create"),
            RouteDecision::Override { .. }
        ));
    }

    #[test]
    fn test_hot_reload_clears_overrides() {
        let mut reg = make_registry();
        reg.register(
            "my-plugin".to_string(),
            "session.create".to_string(),
            &["session.create".to_string()],
        )
        .unwrap();

        reg.prepare_reload("my-plugin");

        // Override is gone
        assert!(matches!(
            reg.route("session.create"),
            RouteDecision::Builtin
        ));

        // Re-register after reload
        reg.register(
            "my-plugin".to_string(),
            "session.create".to_string(),
            &["session.create".to_string()],
        )
        .unwrap();

        assert!(matches!(
            reg.route("session.create"),
            RouteDecision::Override { .. }
        ));
    }

    #[test]
    fn test_conflict_resolution_after_unload() {
        let mut reg = make_registry();

        // Two plugins override the same command
        reg.register(
            "plugin-a".to_string(),
            "session.create".to_string(),
            &["session.create".to_string()],
        )
        .unwrap();
        reg.register(
            "plugin-b".to_string(),
            "session.create".to_string(),
            &["session.create".to_string()],
        )
        .unwrap();

        // Plugin B wins (last-loaded)
        match reg.route("session.create") {
            RouteDecision::Override { plugin_id, .. } => {
                assert_eq!(plugin_id, "plugin-b");
            }
            _ => panic!("Expected Override"),
        }

        // Unload plugin B -> plugin A becomes active
        reg.unregister_plugin("plugin-b");
        match reg.route("session.create") {
            RouteDecision::Override { plugin_id, .. } => {
                assert_eq!(plugin_id, "plugin-a");
            }
            _ => panic!("Expected Override for plugin-a"),
        }
    }

    #[test]
    fn test_list_active_overrides() {
        let mut reg = make_registry();
        reg.register(
            "plugin-a".to_string(),
            "session.create".to_string(),
            &["session.create".to_string()],
        )
        .unwrap();
        reg.register(
            "plugin-b".to_string(),
            "pane.split".to_string(),
            &["pane.split".to_string()],
        )
        .unwrap();

        let active = reg.list_active_overrides();
        assert_eq!(active.len(), 2);
    }

    #[test]
    fn test_list_conflicts() {
        let mut reg = make_registry();
        reg.register(
            "plugin-a".to_string(),
            "session.create".to_string(),
            &["session.create".to_string()],
        )
        .unwrap();
        reg.register(
            "plugin-b".to_string(),
            "session.create".to_string(),
            &["session.create".to_string()],
        )
        .unwrap();

        let conflicts = reg.list_conflicts();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].command_name, "session.create");
    }

    #[test]
    fn test_validate_permissions() {
        let reg = make_registry();
        let errors = reg.validate_permissions(
            "my-plugin",
            &[
                "session.create".to_string(),
                "nonexistent.command".to_string(),
                "admin.shutdown".to_string(),
            ],
        );
        assert_eq!(errors.len(), 2); // one NotBuiltin, one Protected
    }
}
```

---

## Verification

### Functional

```bash
# Build the overrides module
cargo build -p shux-plugin 2>&1 | tail -5

# Verify the override types compile
cargo check -p shux-plugin

# Verify the router integration compiles
cargo check -p shux-rpc

# Verify no clippy warnings
cargo clippy -p shux-plugin -p shux-rpc -- -D warnings
```

### Tests

```bash
# Run override registry tests
cargo nextest run -p shux-plugin overrides

# Run specific tests
cargo nextest run -p shux-plugin test_register_override_success
cargo nextest run -p shux-plugin test_conflict_detection_last_loaded_wins
cargo nextest run -p shux-plugin test_escape_hatch_always_builtin
cargo nextest run -p shux-plugin test_hot_reload_clears_overrides

# Run all tests
cargo nextest run --workspace
```

---

## Completion Criteria

- [ ] `OverrideRegistry` tracks which plugins override which built-in commands
- [ ] Registration validates: command exists as built-in, command is not protected, plugin has `override_commands` permission
- [ ] Last-loaded plugin wins when multiple plugins override the same command
- [ ] Conflict detection: warns user (log + event) when an override replaces an existing one
- [ ] `builtin.<name>` escape hatch always routes to the built-in handler
- [ ] User can disable overrides per-plugin in config (`disable_plugin_overrides`)
- [ ] `prepare_reload` clears overrides before hot reload; plugin re-registers during init
- [ ] Unregistering a plugin falls back to the next override in the stack (or builtin)
- [ ] Protected commands (`admin.shutdown`, etc.) cannot be overridden
- [ ] `list_active_overrides` and `list_conflicts` provide observability
- [ ] RPC router checks `OverrideRegistry` before dispatching to built-in handlers
- [ ] Process plugin protocol supports `register_command_override` message
- [ ] All tests pass
- [ ] No clippy warnings

---

## Commit Message

```
feat(plugin): implement command override system with conflict detection

- Plugins can override built-in commands via register-command-override
- Permission enforcement: override_commands must list specific command names
- Last-loaded plugin wins when multiple override the same command
- Conflict detection: warns user via log and event when overrides conflict
- builtin.<name> escape hatch always routes to original implementation
- Per-plugin config to disable overrides
- Protected commands (admin.shutdown, etc.) cannot be overridden
- Hot reload clears and re-registers overrides during init
- RPC router integration: checks overrides before dispatching to built-in
```

---

## Session Protocol

1. **Before starting:** Read task 041 (plugin lifecycle) to understand the init/shutdown flow where overrides are registered/unregistered. Read PRD section 7.2 (Command overrides extension point) and section 7.5 (register-command-override WIT). Review the Danger Zone use case which demonstrates command overrides.
2. **During:** Implement in order: types (Step 1) -> registry (Step 2) -> router integration (Step 3) -> events (Step 4) -> hot reload (Step 5) -> process protocol (Step 6) -> tests (Step 7). Run `cargo check` after each step. Run tests after Steps 2, 3, and 7.
3. **After:** Run the full verification suite. Verify conflict detection works with multiple plugins. Verify the escape hatch always routes to builtin. Verify hot reload correctly clears and re-registers overrides. Update `docs/PROGRESS.md` (mark 043 done). Update `CLAUDE.md` Learnings with any insights about the routing architecture.
