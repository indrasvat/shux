# 045 — Plugin API Extensions

**Status:** Pending
**Depends On:** 041, 035
**Parallelizable With:** 046

---

## Problem

Plugins need to expose their own JSON-RPC methods to external clients. A session replay plugin needs `replay.seek`, `replay.search`, and `replay.export`. An agent conductor needs `agent.status`, `agent.list`, and `agent.abort`. An MCP bridge needs `mcp.status`. Without API extensions, plugins can only be controlled via the event bus or internal host functions -- they cannot be first-class API citizens.

The API extension system must enforce namespace validation (method names must be prefixed with the plugin's short ID), prevent collisions with built-in methods or other plugins' methods, route incoming calls to the correct plugin's `on-command` callback, make extended methods discoverable via `shux help` and the command palette, and respect the same auth mechanisms (UDS file permissions, TCP token) as built-in methods. Plugins must declare `api_extensions = true` in their permissions.

## PRD Reference

- **section 7.2** — API extensions extension point: "Register new API endpoints"
- **section 7.5** — `register-api-method` WIT function: `func(method-name: string, description: string) -> result<_, host-error>`
- **section 13.1** — Plugin API namespace rule: "Registered API methods must use `<plugin-id>.` prefix... Built-in method names cannot be overridden via `register-api-method`"
- **section 7.6** — Process plugin protocol: `{"type": "register_api_method", "method_name": "replay.seek", "description": "..."}`
- **section 8.2** — JSON-RPC method naming: `<resource>.<action>` convention

---

## Files to Create

- `crates/shux-plugin/src/api_ext.rs` — API extension registry: method registration, namespace validation, collision detection, method metadata for discoverability

## Files to Modify

- `crates/shux-plugin/src/lib.rs` — Add `pub mod api_ext;`
- `crates/shux-rpc/src/router.rs` — Modify the router to check for plugin-registered API methods when no built-in handler is found
- `crates/shux-plugin/Cargo.toml` — Add dependencies if needed

---

## Execution Steps

### Step 1: Define API extension types in `crates/shux-plugin/src/api_ext.rs`

Define the core types for the API extension system.

```rust
use std::collections::HashMap;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{info, warn, error};

/// Identifies a plugin.
pub type PluginId = String;

/// A registered API method from a plugin.
#[derive(Debug, Clone)]
pub struct ApiMethod {
    /// The full method name, including namespace (e.g., "replay.seek").
    pub method_name: String,

    /// Human-readable description for help text and command palette.
    pub description: String,

    /// The plugin that registered this method.
    pub plugin_id: PluginId,

    /// Short ID extracted from the plugin ID for namespace validation.
    /// e.g., "com.example.replay" -> "replay"
    pub plugin_short_id: String,

    /// When the method was registered.
    pub registered_at: Instant,

    /// Whether the method is currently active (plugin is healthy).
    pub active: bool,
}

/// Metadata for discoverability (used by `shux help` and command palette).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiMethodInfo {
    /// The full method name.
    pub method_name: String,

    /// Human-readable description.
    pub description: String,

    /// The plugin that provides this method.
    pub plugin_id: String,

    /// Whether this is a plugin-provided method (vs built-in).
    pub is_plugin_method: bool,
}

/// Errors from the API extension system.
#[derive(Debug, Error)]
pub enum ApiExtError {
    #[error("method name '{method_name}' does not start with plugin short ID '{expected_prefix}.'")]
    NamespaceViolation {
        method_name: String,
        expected_prefix: String,
    },

    #[error("method name '{0}' collides with built-in method")]
    CollidesWithBuiltin(String),

    #[error("method name '{method_name}' already registered by plugin '{existing_plugin}'")]
    AlreadyRegistered {
        method_name: String,
        existing_plugin: String,
    },

    #[error("plugin '{0}' does not have api_extensions permission")]
    PermissionDenied(String),

    #[error("method name '{0}' is empty or invalid")]
    InvalidMethodName(String),

    #[error("plugin '{0}' is not loaded or has been unloaded")]
    PluginNotLoaded(String),
}
```

### Step 2: Implement the API extension registry

The registry manages method registration, validates namespaces, detects collisions, and provides routing information.

```rust
/// Registry of plugin-provided API methods.
///
/// Each plugin can register methods that become available via the
/// JSON-RPC API alongside built-in methods. Methods are namespaced
/// with the plugin's short ID to prevent collisions.
pub struct ApiExtensionRegistry {
    /// Map from method name to the registered method.
    methods: HashMap<String, ApiMethod>,

    /// Set of built-in method names (for collision detection).
    builtin_methods: Vec<String>,
}

impl ApiExtensionRegistry {
    /// Create a new registry with the set of built-in method names.
    pub fn new(builtin_methods: Vec<String>) -> Self {
        Self {
            methods: HashMap::new(),
            builtin_methods,
        }
    }

    /// Register a new API method from a plugin.
    ///
    /// # Validation
    /// 1. Method name must start with the plugin's short ID followed by a dot.
    /// 2. Method name must not collide with any built-in method.
    /// 3. Method name must not already be registered by another plugin.
    ///
    /// # Arguments
    /// * `plugin_id` — Full plugin ID (e.g., "com.example.replay")
    /// * `method_name` — The method name to register (e.g., "replay.seek")
    /// * `description` — Human-readable description
    /// * `has_permission` — Whether the plugin has `api_extensions` permission
    pub fn register(
        &mut self,
        plugin_id: PluginId,
        method_name: String,
        description: String,
        has_permission: bool,
    ) -> Result<(), ApiExtError> {
        // Check permission.
        if !has_permission {
            return Err(ApiExtError::PermissionDenied(plugin_id));
        }

        // Validate method name format.
        if method_name.is_empty() || !method_name.contains('.') {
            return Err(ApiExtError::InvalidMethodName(method_name));
        }

        // Extract plugin short ID from the full plugin ID.
        // "com.example.replay" -> "replay"
        // "my-plugin" -> "my-plugin"
        let short_id = extract_short_id(&plugin_id);

        // Validate namespace: method name must start with "<short_id>."
        if !method_name.starts_with(&format!("{}.", short_id)) {
            return Err(ApiExtError::NamespaceViolation {
                method_name,
                expected_prefix: short_id,
            });
        }

        // Check collision with built-in methods.
        if self.builtin_methods.contains(&method_name) {
            return Err(ApiExtError::CollidesWithBuiltin(method_name));
        }

        // Check collision with existing plugin methods.
        if let Some(existing) = self.methods.get(&method_name) {
            if existing.plugin_id != plugin_id {
                return Err(ApiExtError::AlreadyRegistered {
                    method_name,
                    existing_plugin: existing.plugin_id.clone(),
                });
            }
            // Same plugin re-registering (e.g., during hot reload) — replace.
        }

        info!(
            method = %method_name,
            plugin = %plugin_id,
            description = %description,
            "API method registered"
        );

        self.methods.insert(
            method_name.clone(),
            ApiMethod {
                method_name,
                description,
                plugin_id,
                plugin_short_id: short_id,
                registered_at: Instant::now(),
                active: true,
            },
        );

        Ok(())
    }

    /// Unregister all API methods from a plugin.
    /// Called when a plugin is disabled, unloaded, or hot-reloaded.
    pub fn unregister_plugin(&mut self, plugin_id: &str) {
        let removed: Vec<String> = self
            .methods
            .iter()
            .filter(|(_, m)| m.plugin_id == plugin_id)
            .map(|(name, _)| name.clone())
            .collect();

        for name in &removed {
            self.methods.remove(name);
        }

        if !removed.is_empty() {
            info!(
                plugin = plugin_id,
                methods = ?removed,
                "API methods unregistered"
            );
        }
    }

    /// Look up an API method for routing.
    /// Returns the plugin ID that should handle this method, or None
    /// if it's not a plugin-registered method.
    pub fn lookup(&self, method_name: &str) -> Option<&ApiMethod> {
        self.methods.get(method_name).filter(|m| m.active)
    }

    /// Mark all methods from a plugin as inactive (e.g., plugin stopped).
    pub fn deactivate_plugin(&mut self, plugin_id: &str) {
        for method in self.methods.values_mut() {
            if method.plugin_id == plugin_id {
                method.active = false;
            }
        }
    }

    /// Reactivate all methods from a plugin (e.g., plugin restarted).
    pub fn activate_plugin(&mut self, plugin_id: &str) {
        for method in self.methods.values_mut() {
            if method.plugin_id == plugin_id {
                method.active = true;
            }
        }
    }

    /// Get all registered method names (for help/palette).
    pub fn list_methods(&self) -> Vec<ApiMethodInfo> {
        self.methods
            .values()
            .filter(|m| m.active)
            .map(|m| ApiMethodInfo {
                method_name: m.method_name.clone(),
                description: m.description.clone(),
                plugin_id: m.plugin_id.clone(),
                is_plugin_method: true,
            })
            .collect()
    }

    /// Get all methods from a specific plugin.
    pub fn methods_for_plugin(&self, plugin_id: &str) -> Vec<&ApiMethod> {
        self.methods
            .values()
            .filter(|m| m.plugin_id == plugin_id)
            .collect()
    }

    /// Get all active method names (for autocomplete).
    pub fn method_names(&self) -> Vec<&str> {
        self.methods
            .values()
            .filter(|m| m.active)
            .map(|m| m.method_name.as_str())
            .collect()
    }

    /// Prepare for a plugin hot reload.
    /// Removes all existing methods from the plugin; they will be
    /// re-registered during the new `init` phase.
    pub fn prepare_reload(&mut self, plugin_id: &str) {
        self.unregister_plugin(plugin_id);
        info!(
            plugin = plugin_id,
            "API methods cleared for hot reload — will be re-registered during init"
        );
    }
}

/// Extract the short ID from a full plugin ID.
///
/// Examples:
/// - "com.example.replay" -> "replay"
/// - "com.example.my-plugin" -> "my-plugin"
/// - "replay" -> "replay"
fn extract_short_id(plugin_id: &str) -> String {
    plugin_id
        .rsplit('.')
        .next()
        .unwrap_or(plugin_id)
        .to_string()
}
```

### Step 3: Integrate with the RPC router

Modify the command router to check for plugin-registered API methods when no built-in handler or override is found.

```rust
// In crates/shux-rpc/src/router.rs (modifications)
//
// The router dispatch chain becomes:
//
// 1. Check for "builtin." prefix (escape hatch -> always built-in).
// 2. Check OverrideRegistry for an active override (task 043).
// 3. Check built-in handler map.
// 4. Check ApiExtensionRegistry for a plugin-registered method.
// 5. Return JSON-RPC error -32601 (Method not found).
//
// ```ignore
// pub async fn dispatch(
//     &self,
//     method: &str,
//     params: serde_json::Value,
// ) -> Result<serde_json::Value, RpcError> {
//     // Step 1: Escape hatch
//     match self.override_registry.route(method) {
//         RouteDecision::EscapeHatch { original_command_name } => {
//             return self.dispatch_builtin(&original_command_name, params).await;
//         }
//         RouteDecision::Override { plugin_id, command_name } => {
//             return self.invoke_plugin_command(&plugin_id, &command_name, params).await;
//         }
//         RouteDecision::Builtin => {
//             // Fall through to built-in check
//         }
//     }
//
//     // Step 3: Built-in handler
//     if let Some(handler) = self.builtin_handlers.get(method) {
//         return handler.call(params).await;
//     }
//
//     // Step 4: Plugin API extension
//     if let Some(api_method) = self.api_ext_registry.lookup(method) {
//         let args = self.params_to_args(params);
//         return self.plugin_host
//             .invoke_command(&api_method.plugin_id, method, args)
//             .await
//             .map_err(|e| RpcError::plugin_error(e));
//     }
//
//     // Step 5: Method not found
//     Err(RpcError::method_not_found(method))
// }
// ```
//
// The plugin's `on-command` callback receives:
// - `name`: the registered method name (e.g., "replay.seek")
// - `args`: the JSON-RPC params serialized as a list of strings
//
// Auth enforcement note:
// Plugin-registered API methods go through the SAME transport layer
// as built-in methods. UDS file permissions and TCP token auth are
// enforced at the transport level BEFORE method dispatch. No additional
// auth is needed per-method.
```

### Step 4: Implement method listing for help and command palette

Provide a unified method listing that combines built-in methods and plugin-registered methods.

```rust
// In crates/shux-rpc/src/router.rs or a new module

/// Combined method listing for `shux help` and command palette.
///
/// Returns all available methods: built-in + plugin-registered.
/// Each entry includes the method name, description, and whether
/// it's a plugin method (and if so, which plugin provides it).
pub fn list_all_methods(
    builtin_methods: &[(String, String)], // (name, description)
    api_ext_registry: &ApiExtensionRegistry,
) -> Vec<ApiMethodInfo> {
    let mut methods: Vec<ApiMethodInfo> = Vec::new();

    // Built-in methods
    for (name, desc) in builtin_methods {
        methods.push(ApiMethodInfo {
            method_name: name.clone(),
            description: desc.clone(),
            plugin_id: String::new(),
            is_plugin_method: false,
        });
    }

    // Plugin-registered methods
    methods.extend(api_ext_registry.list_methods());

    // Sort alphabetically for consistent display.
    methods.sort_by(|a, b| a.method_name.cmp(&b.method_name));

    methods
}
```

### Step 5: Implement WIT host function handler

Implement the `register-api-method` host function for Wasm plugins.

```rust
// In the WIT host function implementations (crates/shux-plugin/src/wasm_host.rs or similar)
//
// The register-api-method WIT function:
//
// ```wit
// register-api-method: func(method-name: string, description: string) -> result<_, host-error>;
// ```
//
// Implementation:
//
// ```ignore
// fn register_api_method(
//     &mut self,
//     method_name: String,
//     description: String,
// ) -> Result<(), HostError> {
//     let plugin_id = self.plugin_id.clone();
//     let has_permission = self.permissions.api_extensions;
//
//     self.api_ext_registry
//         .write()
//         .unwrap()
//         .register(plugin_id, method_name, description, has_permission)
//         .map_err(|e| HostError {
//             code: match &e {
//                 ApiExtError::PermissionDenied(_) => -32003,
//                 ApiExtError::NamespaceViolation { .. } => -32004,
//                 ApiExtError::CollidesWithBuiltin(_) => -32005,
//                 ApiExtError::AlreadyRegistered { .. } => -32006,
//                 _ => -32000,
//             },
//             message: e.to_string(),
//         })
// }
// ```
//
// This is called during the plugin's `init` function.
// After init completes, the registered methods become routable.
```

### Step 6: Handle process plugin API method registration

Support the `register_api_method` message in the process plugin protocol.

```rust
// In crates/shux-plugin/src/process.rs (message handling)
//
// When the host receives a register_api_method message from a process plugin:
//
// ```json
// {"type": "register_api_method", "method_name": "replay.seek", "description": "Seek to a timestamp in replay"}
// ```
//
// The handler:
// 1. Checks that the plugin has api_extensions permission.
// 2. Calls api_ext_registry.register() with the plugin ID, method name, and description.
// 3. If registration fails, sends an error response.
//
// This can also happen as part of the "register" bulk message after hello:
// ```json
// {
//     "type": "register",
//     "api_methods": [
//         {"method_name": "replay.seek", "description": "Seek to a timestamp"},
//         {"method_name": "replay.search", "description": "Search replay history"}
//     ]
// }
// ```
```

### Step 7: Write tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn make_registry() -> ApiExtensionRegistry {
        ApiExtensionRegistry::new(vec![
            "session.create".to_string(),
            "session.list".to_string(),
            "pane.split".to_string(),
            "pane.focus".to_string(),
            "state.snapshot".to_string(),
        ])
    }

    #[test]
    fn test_extract_short_id() {
        assert_eq!(extract_short_id("com.example.replay"), "replay");
        assert_eq!(extract_short_id("com.example.my-plugin"), "my-plugin");
        assert_eq!(extract_short_id("replay"), "replay");
        assert_eq!(extract_short_id("a.b.c.d"), "d");
    }

    #[test]
    fn test_register_success() {
        let mut reg = make_registry();
        let result = reg.register(
            "com.example.replay".to_string(),
            "replay.seek".to_string(),
            "Seek to a timestamp in replay".to_string(),
            true,
        );
        assert!(result.is_ok());

        // Method should be discoverable.
        let methods = reg.list_methods();
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].method_name, "replay.seek");
    }

    #[test]
    fn test_namespace_validation() {
        let mut reg = make_registry();

        // Method name doesn't match plugin short ID
        let result = reg.register(
            "com.example.replay".to_string(),
            "agent.status".to_string(), // wrong namespace
            "desc".to_string(),
            true,
        );
        assert!(matches!(result, Err(ApiExtError::NamespaceViolation { .. })));
    }

    #[test]
    fn test_collision_with_builtin() {
        let mut reg = make_registry();

        // Try to register a method that collides with a built-in
        let result = reg.register(
            "com.example.session".to_string(),
            "session.create".to_string(),
            "desc".to_string(),
            true,
        );
        assert!(matches!(result, Err(ApiExtError::CollidesWithBuiltin(_))));
    }

    #[test]
    fn test_collision_with_other_plugin() {
        let mut reg = make_registry();

        // Plugin A registers replay.seek
        reg.register(
            "com.example.replay".to_string(),
            "replay.seek".to_string(),
            "desc".to_string(),
            true,
        )
        .unwrap();

        // Plugin B tries to register replay.seek (different plugin ID, same short ID)
        // This would actually be a namespace violation since plugin B has different short ID.
        // Let's test with same short ID from different full IDs:
        let result = reg.register(
            "org.other.replay".to_string(),
            "replay.seek".to_string(),
            "different desc".to_string(),
            true,
        );
        assert!(matches!(result, Err(ApiExtError::AlreadyRegistered { .. })));
    }

    #[test]
    fn test_same_plugin_reregister() {
        let mut reg = make_registry();

        // Register
        reg.register(
            "com.example.replay".to_string(),
            "replay.seek".to_string(),
            "desc v1".to_string(),
            true,
        )
        .unwrap();

        // Re-register from same plugin (e.g., hot reload)
        let result = reg.register(
            "com.example.replay".to_string(),
            "replay.seek".to_string(),
            "desc v2".to_string(),
            true,
        );
        assert!(result.is_ok());

        // Should still be 1 method, with updated description
        let methods = reg.list_methods();
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].description, "desc v2");
    }

    #[test]
    fn test_permission_denied() {
        let mut reg = make_registry();
        let result = reg.register(
            "com.example.replay".to_string(),
            "replay.seek".to_string(),
            "desc".to_string(),
            false, // no permission
        );
        assert!(matches!(result, Err(ApiExtError::PermissionDenied(_))));
    }

    #[test]
    fn test_invalid_method_name() {
        let mut reg = make_registry();

        // Empty name
        let result = reg.register(
            "com.example.replay".to_string(),
            "".to_string(),
            "desc".to_string(),
            true,
        );
        assert!(matches!(result, Err(ApiExtError::InvalidMethodName(_))));

        // No dot separator
        let result = reg.register(
            "com.example.replay".to_string(),
            "replayseek".to_string(),
            "desc".to_string(),
            true,
        );
        assert!(matches!(result, Err(ApiExtError::InvalidMethodName(_))));
    }

    #[test]
    fn test_unregister_plugin() {
        let mut reg = make_registry();

        reg.register(
            "com.example.replay".to_string(),
            "replay.seek".to_string(),
            "desc".to_string(),
            true,
        )
        .unwrap();
        reg.register(
            "com.example.replay".to_string(),
            "replay.search".to_string(),
            "desc".to_string(),
            true,
        )
        .unwrap();

        assert_eq!(reg.list_methods().len(), 2);

        reg.unregister_plugin("com.example.replay");

        assert_eq!(reg.list_methods().len(), 0);
    }

    #[test]
    fn test_deactivate_activate_plugin() {
        let mut reg = make_registry();

        reg.register(
            "com.example.replay".to_string(),
            "replay.seek".to_string(),
            "desc".to_string(),
            true,
        )
        .unwrap();

        // Method is active and routable
        assert!(reg.lookup("replay.seek").is_some());

        // Deactivate
        reg.deactivate_plugin("com.example.replay");
        assert!(reg.lookup("replay.seek").is_none());

        // Methods still listed but inactive
        assert_eq!(reg.list_methods().len(), 0);

        // Reactivate
        reg.activate_plugin("com.example.replay");
        assert!(reg.lookup("replay.seek").is_some());
    }

    #[test]
    fn test_hot_reload_clears_methods() {
        let mut reg = make_registry();

        reg.register(
            "com.example.replay".to_string(),
            "replay.seek".to_string(),
            "desc".to_string(),
            true,
        )
        .unwrap();

        reg.prepare_reload("com.example.replay");
        assert_eq!(reg.list_methods().len(), 0);

        // Re-register after reload
        reg.register(
            "com.example.replay".to_string(),
            "replay.seek".to_string(),
            "desc v2".to_string(),
            true,
        )
        .unwrap();
        assert_eq!(reg.list_methods().len(), 1);
    }

    #[test]
    fn test_lookup_routing() {
        let mut reg = make_registry();

        reg.register(
            "com.example.replay".to_string(),
            "replay.seek".to_string(),
            "desc".to_string(),
            true,
        )
        .unwrap();

        // Plugin method found
        let method = reg.lookup("replay.seek").unwrap();
        assert_eq!(method.plugin_id, "com.example.replay");

        // Built-in method not found in extension registry
        assert!(reg.lookup("session.create").is_none());

        // Unknown method not found
        assert!(reg.lookup("unknown.method").is_none());
    }

    #[test]
    fn test_methods_for_plugin() {
        let mut reg = make_registry();

        reg.register(
            "com.example.replay".to_string(),
            "replay.seek".to_string(),
            "Seek".to_string(),
            true,
        )
        .unwrap();
        reg.register(
            "com.example.replay".to_string(),
            "replay.search".to_string(),
            "Search".to_string(),
            true,
        )
        .unwrap();
        reg.register(
            "com.example.agent".to_string(),
            "agent.status".to_string(),
            "Status".to_string(),
            true,
        )
        .unwrap();

        let replay_methods = reg.methods_for_plugin("com.example.replay");
        assert_eq!(replay_methods.len(), 2);

        let agent_methods = reg.methods_for_plugin("com.example.agent");
        assert_eq!(agent_methods.len(), 1);
    }
}
```

---

## Verification

### Functional

```bash
# Build the API extension module
cargo build -p shux-plugin 2>&1 | tail -5

# Verify types compile
cargo check -p shux-plugin

# Verify router integration compiles
cargo check -p shux-rpc

# Verify extension methods are listed for discovery surfaces
# (help/command-palette consume this metadata)
cargo nextest run -p shux-plugin test_list_methods_includes_metadata

# Verify no clippy warnings
cargo clippy -p shux-plugin -p shux-rpc -- -D warnings
```

### Tests

```bash
# Run API extension tests
cargo nextest run -p shux-plugin api_ext

# Run specific tests
cargo nextest run -p shux-plugin test_register_success
cargo nextest run -p shux-plugin test_namespace_validation
cargo nextest run -p shux-plugin test_collision_with_builtin
cargo nextest run -p shux-plugin test_collision_with_other_plugin

# Run all tests
cargo nextest run --workspace
```

---

## Completion Criteria

- [ ] `ApiExtensionRegistry` tracks plugin-registered JSON-RPC methods
- [ ] Namespace validation: method name must start with plugin's short ID + dot
- [ ] Short ID extraction: "com.example.replay" -> "replay"
- [ ] Collision detection: rejects methods that collide with built-in methods
- [ ] Collision detection: rejects methods already registered by another plugin
- [ ] Collision detection also handles short-ID namespace clashes (`org.a.agent` vs `com.b.agent`)
- [ ] Same-plugin re-registration allowed (for hot reload)
- [ ] Permission enforcement: requires `api_extensions = true` in plugin.toml
- [ ] `lookup` returns the plugin ID for routing incoming API calls
- [ ] `deactivate_plugin` / `activate_plugin` for plugin stop/start
- [ ] `prepare_reload` clears methods before hot reload
- [ ] `list_methods` provides metadata for `shux help` and command palette
- [ ] `list_methods` metadata includes source plugin ID and human-readable description for help output
- [ ] RPC router checks `ApiExtensionRegistry` after built-in handlers
- [ ] Plugin `on-command` callback receives the method name and args
- [ ] Process plugin protocol supports `register_api_method` message
- [ ] No additional auth needed: plugin methods use same transport-level auth
- [ ] All tests pass
- [ ] No clippy warnings

---

## Commit Message

```
feat(plugin): implement plugin API extension system with namespace validation

- Plugins register JSON-RPC methods via register-api-method
- Namespace validation: method name must be prefixed with plugin short ID
- Collision detection against built-in methods and other plugins
- Methods appear in shux help and command palette
- Same auth enforcement as built-in methods (UDS perms, TCP token)
- Requires api_extensions permission in plugin.toml
- Hot reload: clear and re-register during init
- RPC router integration: checks extensions after built-in handlers
```

---

## Session Protocol

1. **Before starting:** Read task 041 (plugin lifecycle) for the init phase where methods are registered. Read task 035 (RPC server) for the router architecture. Read PRD section 13.1 for the namespace rule and section 7.5 for the WIT function signature. Review the Session Replay, Agent Conductor, and MCP Bridge use cases for concrete API extension examples.
2. **During:** Implement in order: types (Step 1) -> registry (Step 2) -> router integration (Step 3) -> help listing (Step 4) -> WIT handler (Step 5) -> process protocol (Step 6) -> tests (Step 7). Run `cargo check` after each step. Run tests after Steps 2, 4, and 7.
3. **After:** Run the full verification suite. Verify namespace validation catches bad prefixes. Verify collision detection works for both built-in and cross-plugin collisions. Verify hot reload correctly clears and re-registers methods. Update `docs/PROGRESS.md` (mark 045 done). Update `CLAUDE.md` Learnings with insights about the namespace design.
