# 040 — Plugin WIT Host Function Implementations

**Status:** Pending
**Depends On:** 039
**Parallelizable With:** 041

---

## Problem

Task 038 created the wasmtime Engine, Store, and WIT definitions. Task 039 added permission enforcement. But the host functions themselves are still stubs -- when a plugin calls `get-active-pane()` or `create-session()`, nothing happens. This task implements every host function in the WIT `interface host`, bridging each function to the daemon's internal APIs (SessionGraph, PTY manager, event bus, config, theme engine, clipboard).

This is the largest and most critical plugin task. It proves that the plugin API surface is sufficient for building real plugins (PRD section 7.1, goal 1: "If a first-party plugin can't do something, the API is incomplete"). After this task, plugins can observe, decorate, and control the multiplexer through the complete graduated control plane (Tier 1-4).

## PRD Reference

- **section 7.5** — Complete WIT host interface: 45+ host functions across queries, pane lifecycle, pane interaction, pane manipulation, window/session lifecycle, layout, display, overlays, clipboard, API extension, utilities
- **section 7.5** (graduated control plane table) — Tier 1 (Read), Tier 2 (Display), Tier 3 (Control), Tier 4 (Extend)
- **section 7.2** — Extension points: commands, command overrides, status bar segments, pane overlays, themes, event reactors, API extensions
- **section 7.2a** — Interception chain semantics: sequential chain, fail-closed on interceptor failure
- **section 7.2b** — Overlay z-ordering: stacking, input fall-through, replace-on-update

---

## Files to Create

- `crates/shux-plugin/src/host_fns/mod.rs` — Module organization and shared utilities
- `crates/shux-plugin/src/host_fns/query.rs` — Tier 1 query functions (get-*, list-*, get-config)
- `crates/shux-plugin/src/host_fns/display.rs` — Tier 2 display functions (set-status-segment, set-badge, emit-event, overlays)
- `crates/shux-plugin/src/host_fns/pane.rs` — Tier 3 pane lifecycle and manipulation functions
- `crates/shux-plugin/src/host_fns/session.rs` — Tier 3 window/session lifecycle functions
- `crates/shux-plugin/src/host_fns/layout.rs` — Tier 3 layout functions
- `crates/shux-plugin/src/host_fns/extend.rs` — Tier 4 extension functions (register-api-method, register-command-override)
- `crates/shux-plugin/src/host_fns/utility.rs` — Utility functions (log, read-file, write-file, run-subprocess)
- `crates/shux-plugin/src/host_fns/clipboard.rs` — Clipboard functions (get-clipboard, set-clipboard)

## Files to Modify

- `crates/shux-plugin/src/lib.rs` — Export host_fns module
- `crates/shux-plugin/src/host.rs` — Wire all host function implementations into the wasmtime Linker
- `crates/shux-plugin/src/wasm.rs` — Add subsystem references to PluginState for host functions to use

---

## Execution Steps

### Step 1: Extend PluginState with subsystem references

Host functions need access to the daemon's subsystems. Add references to `PluginState` that persist across all host function calls within a plugin.

```rust
// crates/shux-plugin/src/wasm.rs (modifications)

use std::sync::Arc;

/// Per-plugin state stored in the wasmtime Store.
///
/// Host functions access subsystems through these references.
/// All references are Arc-cloned (cheap) and point to shared state.
pub struct PluginState {
    /// Plugin manifest (permissions, metadata).
    pub manifest: crate::manifest::PluginManifest,
    /// Plugin identifier for logging and error reporting.
    pub id: String,
    /// Handle to the SessionGraph for queries and mutations.
    pub graph: Arc<dyn GraphHandle + Send + Sync>,
    /// Event bus for publishing and subscribing to events.
    pub event_bus: Arc<dyn EventBusHandle + Send + Sync>,
    /// Config manager for get-config queries.
    pub config: Arc<dyn ConfigHandle + Send + Sync>,
    /// Theme engine for theme-related queries.
    pub theme: Arc<dyn ThemeHandle + Send + Sync>,
    /// Status bar registry for set-status-segment.
    pub status_registry: Arc<dyn StatusRegistryHandle + Send + Sync>,
    /// Overlay manager for show-overlay / hide-overlay.
    pub overlay_manager: Arc<dyn OverlayManagerHandle + Send + Sync>,
    /// API method registry for register-api-method.
    pub api_registry: Arc<dyn ApiRegistryHandle + Send + Sync>,
    /// Clipboard interface.
    pub clipboard: Arc<dyn ClipboardHandle + Send + Sync>,
}

/// Trait for SessionGraph access from host functions.
/// Implemented by the actual SessionGraph wrapper in shux-core.
pub trait GraphHandle {
    fn get_active_pane(&self) -> Result<PaneInfo, HostFnError>;
    fn get_pane(&self, id: &str) -> Result<PaneInfo, HostFnError>;
    fn list_panes(&self) -> Result<Vec<PaneInfo>, HostFnError>;
    fn get_active_window(&self) -> Result<WindowInfo, HostFnError>;
    fn get_window(&self, id: &str) -> Result<WindowInfo, HostFnError>;
    fn list_windows(&self) -> Result<Vec<WindowInfo>, HostFnError>;
    fn get_active_session(&self) -> Result<SessionInfo, HostFnError>;
    fn get_session(&self, id: &str) -> Result<SessionInfo, HostFnError>;
    fn list_sessions(&self) -> Result<Vec<SessionInfo>, HostFnError>;
    // Mutations go through the mpsc channel to the state owner task
    fn create_pane(&self, options: CreatePaneOpts) -> Result<String, HostFnError>;
    fn split_pane(&self, options: SplitPaneOpts) -> Result<String, HostFnError>;
    fn close_pane(&self, pane_id: &str) -> Result<(), HostFnError>;
    fn focus_pane(&self, pane_id: &str) -> Result<(), HostFnError>;
    fn resize_pane(&self, pane_id: &str, width: u16, height: u16) -> Result<(), HostFnError>;
    fn rename_pane(&self, pane_id: &str, name: &str) -> Result<(), HostFnError>;
    fn set_pane_tag(&self, pane_id: &str, key: &str, value: &str) -> Result<(), HostFnError>;
    fn clear_pane_tag(&self, pane_id: &str, key: &str) -> Result<(), HostFnError>;
    fn create_session(&self, name: &str) -> Result<String, HostFnError>;
    fn create_window(&self, session_id: &str, name: &str) -> Result<String, HostFnError>;
    fn close_window(&self, window_id: &str) -> Result<(), HostFnError>;
    fn kill_session(&self, session_id: &str) -> Result<(), HostFnError>;
    fn rename_session(&self, session_id: &str, name: &str) -> Result<(), HostFnError>;
    fn rename_window(&self, window_id: &str, name: &str) -> Result<(), HostFnError>;
    fn focus_window(&self, window_id: &str) -> Result<(), HostFnError>;
    fn send_keys(&self, pane_id: &str, data: &[u8]) -> Result<(), HostFnError>;
    fn send_text(&self, pane_id: &str, text: &str) -> Result<(), HostFnError>;
    fn read_pane_output(&self, pane_id: &str, lines: u32) -> Result<String, HostFnError>;
    fn read_pane_scrollback(&self, pane_id: &str, offset: u32, lines: u32) -> Result<String, HostFnError>;
    fn subscribe(&self, event_type: &str, pane_id: Option<&str>, exhaustive: bool) -> Result<u64, HostFnError>;
}

#[derive(Debug, thiserror::Error)]
pub enum HostFnError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("internal error: {0}")]
    Internal(String),
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
}
```

### Step 2: Implement Tier 1 query functions

These functions have no permission requirements and read from the SessionGraph snapshot.

```rust
// crates/shux-plugin/src/host_fns/query.rs

use crate::wasm::PluginState;

/// Implementation of all Tier 1 (Read) host functions.
///
/// These functions read from the SessionGraph via ArcSwap snapshot.
/// No permissions required. Sub-microsecond latency expected.

pub fn get_active_pane(state: &PluginState) -> Result<PaneInfo, HostError> {
    state.graph.get_active_pane()
        .map_err(|e| HostError { code: -1, message: e.to_string() })
}

pub fn get_pane(state: &PluginState, id: &str) -> Result<PaneInfo, HostError> {
    state.graph.get_pane(id)
        .map_err(|e| HostError { code: -1, message: e.to_string() })
}

pub fn list_panes(state: &PluginState) -> Result<Vec<PaneInfo>, HostError> {
    state.graph.list_panes()
        .map_err(|e| HostError { code: -1, message: e.to_string() })
}

pub fn get_active_window(state: &PluginState) -> Result<WindowInfo, HostError> {
    state.graph.get_active_window()
        .map_err(|e| HostError { code: -1, message: e.to_string() })
}

pub fn get_window(state: &PluginState, id: &str) -> Result<WindowInfo, HostError> {
    state.graph.get_window(id)
        .map_err(|e| HostError { code: -1, message: e.to_string() })
}

pub fn list_windows(state: &PluginState) -> Result<Vec<WindowInfo>, HostError> {
    state.graph.list_windows()
        .map_err(|e| HostError { code: -1, message: e.to_string() })
}

pub fn get_active_session(state: &PluginState) -> Result<SessionInfo, HostError> {
    state.graph.get_active_session()
        .map_err(|e| HostError { code: -1, message: e.to_string() })
}

pub fn get_session(state: &PluginState, id: &str) -> Result<SessionInfo, HostError> {
    state.graph.get_session(id)
        .map_err(|e| HostError { code: -1, message: e.to_string() })
}

pub fn list_sessions(state: &PluginState) -> Result<Vec<SessionInfo>, HostError> {
    state.graph.list_sessions()
        .map_err(|e| HostError { code: -1, message: e.to_string() })
}

pub fn get_config(state: &PluginState, key: &str) -> Result<Option<String>, HostError> {
    state.config.get(key)
        .map_err(|e| HostError { code: -1, message: e.to_string() })
}
```

### Step 3: Implement Tier 2 display functions

```rust
// crates/shux-plugin/src/host_fns/display.rs

use crate::wasm::PluginState;

/// Set a status bar segment's content.
/// No permission required. The plugin must have declared the segment
/// ID in its [extensions].status_segments list.
pub fn set_status_segment(
    state: &PluginState,
    id: &str,
    text: &str,
) -> Result<(), HostError> {
    // Verify the segment was declared by this plugin
    if !state.manifest.extensions.status_segments.contains(&id.to_string()) {
        return Err(HostError {
            code: -1,
            message: format!(
                "Segment '{}' not declared in [extensions].status_segments",
                id
            ),
        });
    }
    state.status_registry.set_segment(&state.id, id, text)
        .map_err(|e| HostError { code: -1, message: e.to_string() })
}

/// Set a badge on a pane (e.g., notification indicator).
pub fn set_badge(
    state: &PluginState,
    pane_id: &str,
    badge: &str,
) -> Result<(), HostError> {
    state.overlay_manager.set_badge(pane_id, &state.id, badge)
        .map_err(|e| HostError { code: -1, message: e.to_string() })
}

/// Clear a badge from a pane.
pub fn clear_badge(state: &PluginState, pane_id: &str) -> Result<(), HostError> {
    state.overlay_manager.clear_badge(pane_id, &state.id)
        .map_err(|e| HostError { code: -1, message: e.to_string() })
}

/// Emit a namespaced event on the event bus.
/// Event type is automatically prefixed with "plugin.event." to prevent
/// namespace collisions with core events.
pub fn emit_event(
    state: &PluginState,
    event_type: &str,
    data_json: &str,
) -> Result<(), HostError> {
    let prefixed_type = format!("plugin.event.{}", event_type);
    let data: serde_json::Value = serde_json::from_str(data_json)
        .unwrap_or(serde_json::Value::String(data_json.to_string()));

    state.event_bus.publish(&prefixed_type, serde_json::json!({
        "plugin_id": state.id,
        "event_type": event_type,
        "data": data
    }));

    Ok(())
}

/// Show a plugin-managed overlay on a pane.
/// While visible, the plugin receives on-overlay-input callbacks.
/// PRD section 7.2b: overlay z-ordering.
pub fn show_overlay(state: &PluginState, pane_id: &str) -> Result<(), HostError> {
    state.overlay_manager.show_overlay(pane_id, &state.id)
        .map_err(|e| HostError { code: -1, message: e.to_string() })
}

/// Hide a plugin-managed overlay.
pub fn hide_overlay(state: &PluginState, pane_id: &str) -> Result<(), HostError> {
    state.overlay_manager.hide_overlay(pane_id, &state.id)
        .map_err(|e| HostError { code: -1, message: e.to_string() })
}
```

### Step 4: Implement Tier 3 pane and session control functions

These functions require `manage_panes`, `send_keys`, `read_pane_output`, or `manage_sessions` permissions. Permission checks are performed by the calling wrapper (the `permission_checked!` macro from task 039).

```rust
// crates/shux-plugin/src/host_fns/pane.rs

use crate::wasm::PluginState;

/// Create a new pane. Requires manage_panes permission.
pub fn create_pane(state: &PluginState, options: PaneCreateOptions) -> Result<String, HostError> {
    state.graph.create_pane(CreatePaneOpts::from(options))
        .map_err(|e| HostError { code: -1, message: e.to_string() })
}

/// Split an existing pane. Requires manage_panes permission.
pub fn split_pane(state: &PluginState, options: SplitOptions) -> Result<String, HostError> {
    state.graph.split_pane(SplitPaneOpts::from(options))
        .map_err(|e| HostError { code: -1, message: e.to_string() })
}

/// Close a pane. Requires manage_panes permission.
pub fn close_pane(state: &PluginState, pane_id: &str) -> Result<(), HostError> {
    state.graph.close_pane(pane_id)
        .map_err(|e| HostError { code: -1, message: e.to_string() })
}

/// Focus a pane. Requires manage_panes permission.
pub fn focus_pane(state: &PluginState, pane_id: &str) -> Result<(), HostError> {
    state.graph.focus_pane(pane_id)
        .map_err(|e| HostError { code: -1, message: e.to_string() })
}

/// Send raw bytes to a pane's PTY. Requires send_keys permission.
pub fn send_keys(state: &PluginState, pane_id: &str, data: &[u8]) -> Result<(), HostError> {
    state.graph.send_keys(pane_id, data)
        .map_err(|e| HostError { code: -1, message: e.to_string() })
}

/// Send UTF-8 text to a pane's PTY. Requires send_keys permission.
pub fn send_text(state: &PluginState, pane_id: &str, text: &str) -> Result<(), HostError> {
    state.graph.send_text(pane_id, text)
        .map_err(|e| HostError { code: -1, message: e.to_string() })
}

/// Read recent visible output from a pane. Requires read_pane_output permission.
pub fn read_pane_output(state: &PluginState, pane_id: &str, lines: u32) -> Result<String, HostError> {
    state.graph.read_pane_output(pane_id, lines)
        .map_err(|e| HostError { code: -1, message: e.to_string() })
}

/// Read from pane scrollback. Requires read_pane_output permission.
pub fn read_pane_scrollback(state: &PluginState, pane_id: &str, offset: u32, lines: u32) -> Result<String, HostError> {
    state.graph.read_pane_scrollback(pane_id, offset, lines)
        .map_err(|e| HostError { code: -1, message: e.to_string() })
}

/// Subscribe to events. Returns a stream ID.
/// - `exhaustive=true`: future high-volume stream with backpressure.
///   Current v0 pane output is sampled; byte-exact transcripts use
///   `pane.record.start` / `pane.record.stop` outside the process-plugin grant model.
/// - `pane_id`: filter events to a specific pane
pub fn subscribe(state: &PluginState, event_type: &str, pane_id: Option<&str>, exhaustive: bool) -> Result<u64, HostError> {
    // Requires 'events' permission containing the event type
    if !state.manifest.permissions.events.contains(&event_type.to_string()) {
         return Err(HostError { 
             code: -1, 
             message: format!("Permission denied: event type '{}' not in [permissions].events", event_type) 
         });
    }

    state.graph.subscribe(event_type, pane_id, exhaustive)
        .map_err(|e| HostError { code: -1, message: e.to_string() })
}

// Additional pane manipulation functions: resize, rename, set_tag, clear_tag
// follow the same pattern...
```

### Step 5: Implement Tier 4 extension functions

```rust
// crates/shux-plugin/src/host_fns/extend.rs

use crate::wasm::PluginState;

/// Register a new JSON-RPC API method. Requires api_extensions permission.
///
/// Method names MUST be prefixed with the plugin's short ID to prevent
/// namespace collisions. The host rejects names that collide with built-in
/// methods or already-registered methods from other plugins.
pub fn register_api_method(
    state: &PluginState,
    method_name: &str,
    description: &str,
) -> Result<(), HostError> {
    // Validate method name prefix
    let plugin_prefix = state.id.split('.').last().unwrap_or(&state.id);
    if !method_name.starts_with(&format!("{}.", plugin_prefix)) {
        return Err(HostError {
            code: -1,
            message: format!(
                "API method name must be prefixed with '{}.': got '{}'",
                plugin_prefix, method_name
            ),
        });
    }

    state.api_registry.register_method(&state.id, method_name, description)
        .map_err(|e| HostError { code: -1, message: e.to_string() })
}

/// Register a command override. Requires override_commands permission
/// and the command must be listed in override_commands in plugin.toml.
pub fn register_command_override(
    state: &PluginState,
    command_name: &str,
) -> Result<(), HostError> {
    // Verify the command is listed in override_commands permission
    if !state.manifest.permissions.override_commands.contains(&command_name.to_string()) {
        return Err(HostError {
            code: -1,
            message: format!(
                "Command '{}' not listed in [permissions].override_commands in plugin.toml",
                command_name
            ),
        });
    }

    state.api_registry.register_override(&state.id, command_name)
        .map_err(|e| HostError { code: -1, message: e.to_string() })
}
```

### Step 6: Implement utility functions

```rust
// crates/shux-plugin/src/host_fns/utility.rs

use crate::permissions::{check_permission, Permission, PermissionCheck};
use crate::wasm::PluginState;

/// Log a message. No permission required.
/// Messages are tagged with the plugin ID for routing.
pub fn log(state: &PluginState, level: LogLevel, msg: &str) {
    match level {
        LogLevel::Trace => tracing::trace!(plugin = %state.id, "{}", msg),
        LogLevel::Debug => tracing::debug!(plugin = %state.id, "{}", msg),
        LogLevel::Info => tracing::info!(plugin = %state.id, "{}", msg),
        LogLevel::Warn => tracing::warn!(plugin = %state.id, "{}", msg),
        LogLevel::Error => tracing::error!(plugin = %state.id, "{}", msg),
    }
}

/// Read a file. Requires fs_read permission for the path.
pub fn read_file(state: &PluginState, path: &str) -> Result<Vec<u8>, HostError> {
    // Check fs_read permission with the specific path
    let perm = Permission::FsRead(path.to_string());
    match check_permission(&state.manifest.permissions, "read-file", &perm) {
        PermissionCheck::Allowed => {}
        PermissionCheck::Denied(denied) => {
            return Err(HostError {
                code: -1,
                message: denied.to_string(),
            });
        }
    }

    std::fs::read(path)
        .map_err(|e| HostError { code: -1, message: format!("Failed to read '{}': {}", path, e) })
}

/// Write a file. Requires fs_write permission for the path.
pub fn write_file(state: &PluginState, path: &str, data: &[u8]) -> Result<(), HostError> {
    let perm = Permission::FsWrite(path.to_string());
    match check_permission(&state.manifest.permissions, "write-file", &perm) {
        PermissionCheck::Allowed => {}
        PermissionCheck::Denied(denied) => {
            return Err(HostError {
                code: -1,
                message: denied.to_string(),
            });
        }
    }

    std::fs::write(path, data)
        .map_err(|e| HostError { code: -1, message: format!("Failed to write '{}': {}", path, e) })
}

/// Run a subprocess. Requires run_subprocess permission.
/// Uses direct invocation (no shell) with scrubbed environment.
pub async fn run_subprocess(
    state: &PluginState,
    command: &str,
    args: &[String],
) -> Result<String, HostError> {
    use crate::subprocess::run_sandboxed;

    let result = run_sandboxed(command, args, None, &[], None)
        .await
        .map_err(|e| HostError { code: -1, message: e.to_string() })?;

    if result.exit_code != 0 {
        return Err(HostError {
            code: result.exit_code,
            message: format!(
                "Command '{}' exited with code {}: {}",
                command, result.exit_code, result.stderr
            ),
        });
    }

    Ok(result.stdout)
}
```

### Step 7: Wire host functions into the wasmtime Linker

```rust
// crates/shux-plugin/src/host.rs

use wasmtime::component::Linker;
use crate::wasm::PluginState;
use crate::host_fns;

/// Create and configure the shared Linker with all host function implementations.
///
/// The Linker is created once at daemon startup and shared across all plugins.
/// Each host function checks the calling plugin's permissions before executing.
pub fn create_linker(engine: &wasmtime::Engine) -> wasmtime::Result<Linker<PluginState>> {
    let mut linker = Linker::new(engine);

    // Add WASI Preview 2 support
    wasmtime_wasi::add_to_linker_async(&mut linker)?;

    // Bind every `shux:plugin/host` export using generated WIT glue.
    // The generated adaptor should call into `host_fns::*` modules:
    // - query/display/pane/session/layout/clipboard/extend/utility
    // Example shape:
    // bindings::shux::plugin::host::add_to_linker(&mut linker, |state| state)?;
    //
    // Keep this mapping explicit in one place so WIT additions fail fast
    // at compile time until their Rust handlers are implemented.

    Ok(linker)
}
```

### Step 8: Organize the host_fns module

```rust
// crates/shux-plugin/src/host_fns/mod.rs

//! Host function implementations for the WIT `interface host`.
//!
//! Organized by permission tier:
//! - query: Tier 1 (Read) - no permissions required
//! - display: Tier 2 (Display) - no permissions required
//! - pane: Tier 3 (Control) - manage_panes, send_keys, read_pane_output
//! - session: Tier 3 (Control) - manage_sessions
//! - layout: Tier 3 (Control) - manage_panes
//! - clipboard: Clipboard permission required
//! - extend: Tier 4 (Extend) - api_extensions, override_commands
//! - utility: Mixed (log = none, fs = fs_read/fs_write, subprocess = run_subprocess)

pub mod query;
pub mod display;
pub mod pane;
pub mod session;
pub mod layout;
pub mod clipboard;
pub mod extend;
pub mod utility;
```

### Step 9: Write tests

```rust
#[cfg(test)]
mod host_fn_tests {
    // These tests use mock implementations of the trait handles
    // to verify host function behavior without a real daemon.

    use std::sync::Arc;
    use crate::manifest::{PluginManifest, PluginMeta, PluginKind, PluginPermissions, PluginExtensions};

    fn test_manifest(perms: PluginPermissions) -> PluginManifest {
        PluginManifest {
            plugin: PluginMeta {
                id: "com.test.plugin".to_string(),
                name: "Test Plugin".to_string(),
                version: "0.1.0".to_string(),
                api: "shux:plugin@1.0.0".to_string(),
                kind: PluginKind::Wasm,
                description: String::new(),
                homepage: None,
                license: None,
                min_shux: None,
                gc: None,
                metadata: None,
            },
            permissions: perms,
            extensions: PluginExtensions {
                status_segments: vec!["test_segment".to_string()],
                commands: vec![],
                themes: vec![],
                layouts: vec![],
            },
            dependencies: Default::default(),
            conflicts: Default::default(),
        }
    }

    #[test]
    fn test_register_api_method_requires_prefix() {
        // A plugin "com.test.plugin" should only register methods
        // prefixed with "plugin." (the last segment of the ID)
        // This test verifies the prefix enforcement

        // Valid: "plugin.my-method"
        // Invalid: "other.my-method"
        // Invalid: "my-method"
    }

    #[test]
    fn test_register_command_override_requires_declaration() {
        // The command must be listed in [permissions].override_commands
    }

    #[test]
    fn test_emit_event_prefixes_with_plugin_event() {
        // emit_event("branch_changed", ...) should produce
        // event type "plugin.event.branch_changed"
    }

    #[test]
    fn test_set_status_segment_requires_declaration() {
        // Segment must be declared in [extensions].status_segments
    }

    #[test]
    fn test_log_function_tags_with_plugin_id() {
        // log(Info, "hello") should produce a tracing event
        // with plugin="com.test.plugin"
    }

    #[test]
    fn test_read_file_checks_fs_read_permission() {
        // read_file("/etc/passwd") should be denied if fs_read
        // does not include a matching glob
    }
}
```

---

## Verification

### Functional

```bash
# Build the plugin crate
cargo build -p shux-plugin

# Verify clippy
cargo clippy -p shux-plugin -- -D warnings

# Verify all host functions are covered by compiling against the WIT
# (integration test with a real Wasm plugin that calls each function)
```

### Tests

```bash
# Run host function tests
cargo nextest run -p shux-plugin -- host_fn
cargo nextest run -p shux-plugin -- host_fn::test_register_layout_requires_permission
cargo nextest run -p shux-plugin -- host_fn::test_clipboard_requires_permission

# Run all plugin tests
cargo nextest run -p shux-plugin

# Full workspace
cargo nextest run --workspace
```

---

## Completion Criteria

- [ ] All 45+ WIT host functions have Rust implementations
- [ ] Tier 1 (Read): get-active-pane, get-pane, list-panes, get-active-window, get-window, list-windows, get-active-session, get-session, list-sessions, get-config
- [ ] Tier 2 (Display): set-status-segment (validates declared segments), set-badge, clear-badge, emit-event (auto-prefixes), show-overlay, hide-overlay
- [ ] Tier 3 (Control): create-pane, split-pane, create-floating-pane, close-pane, toggle-floating-pane, send-keys, send-text, read-pane-output, read-pane-scrollback, focus-pane, resize-pane, rename-pane, set-pane-tag, clear-pane-tag, create-session, create-window, close-window, kill-session, rename-session, rename-window, focus-window, register-layout, apply-layout
- [ ] Tier 4 (Extend): register-api-method (validates plugin prefix), register-command-override (validates declaration), get-clipboard, set-clipboard
- [ ] Utilities: log (tagged with plugin ID), read-file (checks fs_read), write-file (checks fs_write), run-subprocess (sandboxed execution)
- [ ] `PluginState` contains subsystem references (traits) for all host function dependencies
- [ ] Linker configured with all host functions and WASI Preview 2 support
- [ ] Host functions bridge to daemon internal APIs (SessionGraph, EventBus, etc.)
- [ ] All unit tests pass with mock implementations
- [ ] `cargo clippy -p shux-plugin -- -D warnings` passes

---

## Commit Message
```
feat(plugin): implement all WIT host functions with subsystem bridging

- 45+ host function implementations across Tiers 1-4
- Tier 1 (Read): pane/window/session queries, config access
- Tier 2 (Display): status segments, badges, events, overlays
- Tier 3 (Control): pane/session lifecycle, input injection, output capture
- Tier 4 (Extend): API method registration, command overrides, clipboard
- Utilities: tagged logging, sandboxed filesystem access, subprocess execution
- PluginState with trait-based subsystem references for testability
- Linker wiring with WASI Preview 2 support
```

---

## Session Protocol

1. **Before starting:** Read `CLAUDE.md`. Read tasks 038 (wasmtime integration) and 039 (permissions/sandbox). Read PRD section 7.5 (complete WIT) and the graduated control plane table. Verify tasks 038 and 039 are complete. Understand which daemon subsystems exist (SessionGraph, EventBus, etc.) and their APIs.
2. **During:** Implement tier by tier: Tier 1 (Step 2) -> Tier 2 (Step 3) -> Tier 3 (Steps 4-5) -> Tier 4 (Step 5) -> Utilities (Step 6) -> Linker wiring (Step 7) -> Tests (Step 9). Run `cargo check -p shux-plugin` after each step. Some subsystem traits may need to be defined as stubs if the subsystem crate does not yet expose the needed API.
3. **Testing:** Use mock trait implementations for testing. Each mock should track calls and return deterministic results. This allows testing host function logic without a running daemon.
4. **After:** Run `make check`. Update `docs/PROGRESS.md`. Update `CLAUDE.md` Learnings with discoveries about wasmtime Linker API, WIT binding generation, or trait-based subsystem abstraction patterns.
5. **Watch out for:**
   - The actual WIT binding generation (via `wit-bindgen` or `wasmtime::component::bindgen!`) may not be straightforward. If `bindgen!` does not work with the WIT file, use manual component-model function registration.
   - Host functions that need async (e.g., `run-subprocess`, `send-keys` to PTY) must work with wasmtime's async support (enabled in task 038).
   - The `emit-event` function should prefix event types with `"plugin.event."` to prevent namespace collisions with core events.
   - `register-api-method` must enforce the plugin prefix rule (PRD section 13: API extension squatting prevention).
   - Some subsystem handles may not exist yet. Define the traits here and implement them as subsystems are built. Use `unimplemented!()` or return errors for unimplemented subsystems.
