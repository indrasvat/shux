# 041 — Plugin Lifecycle and Hot Reload

**Status:** Pending
**Depends On:** 040
**Parallelizable With:** ---

---

## Problem

Plugins need a well-defined lifecycle to be predictable and debuggable. Without lifecycle management, plugin loading order is undefined, handlers leak on disable, hot reload loses state, and idle process plugins consume resources indefinitely. This task implements the complete plugin lifecycle state machine, the registration-activation separation pattern (inspired by PI), hot reload via Store drop-and-recreate, plugin GC for idle process plugins, daemon leases for long-lived service plugins, and the `plugin.*` API methods.

PRD section 7.1 (goal 3) states: "Hot reloadable: Enable/disable/reload at runtime. Wasm: drop Store, re-instantiate from new .wasm file." PRD section 7.4 defines the lifecycle: "Discover -> Validate -> Enable -> Start -> Handle events -> Stop -> Disable." The registration-activation separation (PRD section 6.1, plugin system) prevents initialization order bugs by restricting what plugins can do during `init()`.

## PRD Reference

- **section 7.1** goal 3 — Hot reloadable: drop Store, re-instantiate
- **section 7.4** — Plugin lifecycle: Discover -> Validate -> Enable -> Start -> Handle -> Stop -> Disable
- **section 6.1** (Plugin system table) — Registration-activation separation: during loading, plugins can only register handlers/commands. Action methods available only after binding.
- **section 7.7** — Bundled plugins: shux-status-bar, shux-theme-pack, shux-diagnostics
- **section 7.8** — Plugin DX: scaffold, dev mode, inspect
- **section 7.6** — Process plugin GC: idle 30s -> shutdown + stop
- **section 4.5** — Daemon leases: plugins with gc=false hold lease, prevent auto-exit

---

## Files to Create

- `crates/shux-plugin/src/lifecycle.rs` — Lifecycle state machine: state transitions, validation, error handling
- `crates/shux-plugin/src/registry.rs` — Plugin registry: tracks all plugins, their states, registered extensions, and performance stats
- `crates/shux-plugin/src/discovery.rs` — Plugin discovery: scan configured paths for plugin.toml manifests
- `crates/shux-plugin/src/reload.rs` — Hot reload logic: Store drop, recompile, reinstantiate, re-register

## Files to Modify

- `crates/shux-plugin/src/lib.rs` — Export new modules, flesh out PluginHost API
- `crates/shux-plugin/src/types.rs` — Add PluginRegistration type for registration-activation pattern
- `crates/shux-rpc/src/methods/plugin.rs` — Wire plugin.list/enable/disable/reload/inspect to PluginHost

---

## Execution Steps

### Step 1: Define the lifecycle state machine

The plugin lifecycle has well-defined states and transitions. Invalid transitions are rejected with clear errors.

```rust
// crates/shux-plugin/src/lifecycle.rs

use crate::types::PluginLifecycleState;

/// Lifecycle state transition validator.
///
/// Valid transitions:
///   Discovered -> Validated (manifest checked)
///   Validated -> Enabled (compiled, ready)
///   Enabled -> Started (Store created, init() called)
///   Started -> Stopped (shutdown() called, Store dropped)
///   Stopped -> Enabled (ready to restart)
///   Stopped -> Disabled (unloaded)
///   Enabled -> Disabled (never started, just unload)
///   Any -> Error (on failure)
///   Error -> Disabled (cleanup after error)
///   Disabled -> Validated (re-validation for reload)
pub fn validate_transition(
    from: PluginLifecycleState,
    to: PluginLifecycleState,
) -> Result<(), LifecycleError> {
    use PluginLifecycleState::*;

    let valid = matches!(
        (from, to),
        (Discovered, Validated)
        | (Validated, Enabled)
        | (Enabled, Started)
        | (Started, Stopped)
        | (Stopped, Enabled)
        | (Stopped, Disabled)
        | (Enabled, Disabled)
        | (_, Error)
        | (Error, Disabled)
        | (Disabled, Validated)
    );

    if valid {
        Ok(())
    } else {
        Err(LifecycleError::InvalidTransition { from, to })
    }
}

/// A lifecycle event, logged for debugging and emitted on the event bus.
#[derive(Debug, Clone)]
pub struct LifecycleEvent {
    pub plugin_id: String,
    pub from: PluginLifecycleState,
    pub to: PluginLifecycleState,
    pub reason: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum LifecycleError {
    #[error("invalid lifecycle transition from {from:?} to {to:?}")]
    InvalidTransition {
        from: PluginLifecycleState,
        to: PluginLifecycleState,
    },
    #[error("plugin init() failed: {0}")]
    InitFailed(String),
    #[error("plugin shutdown() failed: {0}")]
    ShutdownFailed(String),
    #[error("plugin compilation failed: {0}")]
    CompilationFailed(String),
    #[error("plugin not found: {0}")]
    NotFound(String),
    #[error("plugin already in state {0:?}")]
    AlreadyInState(PluginLifecycleState),
}

/// The complete lifecycle sequence for starting a plugin from discovery.
///
/// Discover -> Validate -> Enable -> Start
///
/// Each step can fail independently with a clear error.
pub struct PluginLifecycle;

impl PluginLifecycle {
    /// Execute the full startup sequence.
    ///
    /// 1. Discover: Read plugin.toml manifest
    /// 2. Validate: Check API version, permissions, ID format
    /// 3. Enable: Compile .wasm (or load .cwasm), configure sandbox
    /// 4. Start: Create Store, call init(config_json)
    pub async fn start_plugin(
        plugin_id: &str,
        // engine: &Arc<Engine>,
        // linker: &Linker<PluginState>,
        // manifest: &PluginManifest,
        // config_json: &str,
    ) -> Result<(), LifecycleError> {
        tracing::info!(plugin = %plugin_id, "Starting plugin lifecycle");

        // Step 1: Discover (already done by registry)
        // Step 2: Validate (already done by manifest parser)

        // Step 3: Enable
        // - Load/compile .wasm
        // - Configure ResourceLimiter
        // - Set epoch deadline
        tracing::debug!(plugin = %plugin_id, "Plugin enabled (compiled)");

        // Step 4: Start
        // - Create Store with PluginState
        // - Instantiate component
        // - Call init(config_json)
        // - During init, only registration methods are available
        //   (registration-activation separation)
        tracing::info!(plugin = %plugin_id, "Plugin started");

        Ok(())
    }

    /// Execute the shutdown sequence.
    ///
    /// 1. Stop: Call shutdown(), drop Store
    /// 2. Disable: Unregister all handlers, commands, segments
    pub async fn stop_plugin(
        plugin_id: &str,
    ) -> Result<(), LifecycleError> {
        tracing::info!(plugin = %plugin_id, "Stopping plugin");

        // Step 1: Stop
        // - Call shutdown() on the plugin
        // - Drop the Store (frees all Wasm instances, memories, tables)
        tracing::debug!(plugin = %plugin_id, "Plugin stopped (Store dropped)");

        // Step 2: Disable
        // - Unregister all handlers, commands, segments, layouts
        // - Remove from active plugin list
        tracing::info!(plugin = %plugin_id, "Plugin disabled");

        Ok(())
    }
}
```

### Step 2: Implement the plugin registry

The registry tracks all known plugins, their states, registered extensions, and performance statistics.

```rust
// crates/shux-plugin/src/registry.rs

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use crate::manifest::PluginManifest;
use crate::types::{PluginId, PluginInfo, PluginLifecycleState, PluginStats, PluginExtensionInfo};

/// Central registry of all plugins.
///
/// Thread-safe via RwLock. The registry is read frequently (for event
/// routing, status bar rendering) and written infrequently (plugin
/// load/unload/reload).
pub struct PluginRegistry {
    inner: RwLock<RegistryInner>,
}

struct RegistryInner {
    /// All known plugins by ID.
    plugins: HashMap<PluginId, PluginEntry>,
    /// Registered status bar segments: segment_id -> plugin_id.
    status_segments: HashMap<String, PluginId>,
    /// Registered commands: command_name -> plugin_id.
    commands: HashMap<String, PluginId>,
    /// Registered API methods: method_name -> plugin_id.
    api_methods: HashMap<String, PluginId>,
    /// Registered command overrides: command_name -> plugin_id.
    command_overrides: HashMap<String, PluginId>,
    /// Registered layouts: layout_name -> plugin_id.
    layouts: HashMap<String, PluginId>,
}

/// A single plugin entry in the registry.
struct PluginEntry {
    id: PluginId,
    manifest: PluginManifest,
    state: PluginLifecycleState,
    /// Registrations made during init() (registration-activation pattern).
    registrations: PluginRegistrations,
    /// Performance statistics.
    stats: PluginStats,
    /// When the plugin was last active (for GC).
    last_activity: Instant,
    /// Whether this plugin holds a daemon lease (gc = false).
    holds_lease: bool,
}

/// Registrations made by a plugin during its init() phase.
///
/// These are collected during init() (registration phase) and activated
/// after init() completes (activation phase). This prevents initialization
/// order bugs where a plugin tries to use another plugin's registration
/// that hasn't happened yet.
#[derive(Debug, Clone, Default)]
pub struct PluginRegistrations {
    pub commands: Vec<String>,
    pub status_segments: Vec<String>,
    pub api_methods: Vec<(String, String)>, // (method_name, description)
    pub command_overrides: Vec<String>,
    pub layouts: Vec<(String, String)>, // (layout_name, layout_json)
    pub event_handlers: Vec<String>, // event types this plugin handles
    pub intercept_handlers: Vec<String>, // event types this plugin intercepts
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(RegistryInner {
                plugins: HashMap::new(),
                status_segments: HashMap::new(),
                commands: HashMap::new(),
                api_methods: HashMap::new(),
                command_overrides: HashMap::new(),
                layouts: HashMap::new(),
            }),
        }
    }

    /// Register a discovered plugin.
    pub fn register(&self, id: PluginId, manifest: PluginManifest) {
        let mut inner = self.inner.write().expect("registry write lock poisoned");
        inner.plugins.insert(
            id.clone(),
            PluginEntry {
                id,
                manifest,
                state: PluginLifecycleState::Discovered,
                registrations: PluginRegistrations::default(),
                stats: PluginStats {
                    call_count: 0,
                    avg_call_us: 0,
                    p99_call_us: 0,
                    error_count: 0,
                    last_error: None,
                },
                last_activity: Instant::now(),
                holds_lease: false,
            },
        );
    }

    /// Update a plugin's lifecycle state.
    pub fn set_state(
        &self,
        id: &PluginId,
        new_state: PluginLifecycleState,
    ) -> Result<(), crate::lifecycle::LifecycleError> {
        let mut inner = self.inner.write().expect("registry write lock poisoned");
        let entry = inner.plugins.get_mut(id)
            .ok_or_else(|| crate::lifecycle::LifecycleError::NotFound(id.0.clone()))?;

        crate::lifecycle::validate_transition(entry.state, new_state)?;
        entry.state = new_state;
        entry.last_activity = Instant::now();
        Ok(())
    }

    /// Activate registrations after init() completes.
    ///
    /// This is the second phase of the registration-activation pattern.
    /// Registrations collected during init() are now committed to the
    /// global registry.
    pub fn activate_registrations(
        &self,
        id: &PluginId,
        registrations: PluginRegistrations,
    ) {
        let mut inner = self.inner.write().expect("registry write lock poisoned");

        // Register commands
        for cmd in &registrations.commands {
            inner.commands.insert(cmd.clone(), id.clone());
        }

        // Register status segments
        for seg in &registrations.status_segments {
            inner.status_segments.insert(seg.clone(), id.clone());
        }

        // Register API methods
        for (method, _desc) in &registrations.api_methods {
            inner.api_methods.insert(method.clone(), id.clone());
        }

        // Register command overrides (last-loaded wins)
        for cmd in &registrations.command_overrides {
            if let Some(existing) = inner.command_overrides.get(cmd) {
                tracing::warn!(
                    command = %cmd,
                    existing_plugin = %existing,
                    new_plugin = %id,
                    "Command override conflict; new plugin takes precedence"
                );
            }
            inner.command_overrides.insert(cmd.clone(), id.clone());
        }

        // Register layouts
        for (name, _json) in &registrations.layouts {
            inner.layouts.insert(name.clone(), id.clone());
        }

        // Store registrations in the entry
        if let Some(entry) = inner.plugins.get_mut(id) {
            entry.registrations = registrations;
        }
    }

    /// Deactivate all registrations for a plugin (during disable).
    pub fn deactivate_registrations(&self, id: &PluginId) {
        let mut inner = self.inner.write().expect("registry write lock poisoned");

        if let Some(entry) = inner.plugins.get(id) {
            for cmd in &entry.registrations.commands {
                inner.commands.remove(cmd);
            }
            for seg in &entry.registrations.status_segments {
                inner.status_segments.remove(seg);
            }
            for (method, _) in &entry.registrations.api_methods {
                inner.api_methods.remove(method);
            }
            for cmd in &entry.registrations.command_overrides {
                inner.command_overrides.remove(cmd);
            }
            for (name, _) in &entry.registrations.layouts {
                inner.layouts.remove(name);
            }
        }

        if let Some(entry) = inner.plugins.get_mut(id) {
            entry.registrations = PluginRegistrations::default();
        }
    }

    /// List all plugins with their current state.
    pub fn list(&self) -> Vec<PluginInfo> {
        let inner = self.inner.read().expect("registry read lock poisoned");
        inner
            .plugins
            .values()
            .map(|entry| PluginInfo {
                id: entry.id.0.clone(),
                name: entry.manifest.plugin.name.clone(),
                version: entry.manifest.plugin.version.clone(),
                kind: format!("{:?}", entry.manifest.plugin.kind).to_lowercase(),
                state: entry.state,
                description: entry.manifest.plugin.description.clone(),
                permissions: collect_permissions(&entry.manifest),
                extensions: PluginExtensionInfo {
                    commands: entry.registrations.commands.clone(),
                    status_segments: entry.registrations.status_segments.clone(),
                    themes: entry.manifest.extensions.themes.clone(),
                    api_methods: entry.registrations.api_methods.iter().map(|(m, _)| m.clone()).collect(),
                    command_overrides: entry.registrations.command_overrides.clone(),
                },
                stats: Some(entry.stats.clone()),
            })
            .collect()
    }

    /// Get detailed info for a specific plugin.
    pub fn inspect(&self, id: &PluginId) -> Option<PluginInfo> {
        let inner = self.inner.read().expect("registry read lock poisoned");
        let entry = inner.plugins.get(id)?;
        Some(PluginInfo {
            id: entry.id.0.clone(),
            name: entry.manifest.plugin.name.clone(),
            version: entry.manifest.plugin.version.clone(),
            kind: format!("{:?}", entry.manifest.plugin.kind).to_lowercase(),
            state: entry.state,
            description: entry.manifest.plugin.description.clone(),
            permissions: collect_permissions(&entry.manifest),
            extensions: PluginExtensionInfo {
                commands: entry.registrations.commands.clone(),
                status_segments: entry.registrations.status_segments.clone(),
                themes: entry.manifest.extensions.themes.clone(),
                api_methods: entry.registrations.api_methods.iter().map(|(m, _)| m.clone()).collect(),
                command_overrides: entry.registrations.command_overrides.clone(),
            },
            stats: Some(entry.stats.clone()),
        })
    }

    /// Find plugins idle for longer than the GC timeout.
    /// Returns plugin IDs that should be garbage collected.
    pub fn find_gc_candidates(&self, gc_timeout: std::time::Duration) -> Vec<PluginId> {
        let inner = self.inner.read().expect("registry read lock poisoned");
        inner
            .plugins
            .values()
            .filter(|e| {
                e.state == PluginLifecycleState::Started
                    && !e.holds_lease
                    && e.last_activity.elapsed() > gc_timeout
            })
            .map(|e| e.id.clone())
            .collect()
    }

    /// Record a plugin call for statistics tracking.
    pub fn record_call(&self, id: &PluginId, duration_us: u64, error: Option<&str>) {
        let mut inner = self.inner.write().expect("registry write lock poisoned");
        if let Some(entry) = inner.plugins.get_mut(id) {
            entry.stats.call_count += 1;
            entry.last_activity = Instant::now();

            // Update running average
            let prev_avg = entry.stats.avg_call_us;
            let n = entry.stats.call_count;
            entry.stats.avg_call_us = prev_avg + (duration_us.saturating_sub(prev_avg)) / n;

            // Update p99 (simplified: just track max for now)
            if duration_us > entry.stats.p99_call_us {
                entry.stats.p99_call_us = duration_us;
            }

            if let Some(err_msg) = error {
                entry.stats.error_count += 1;
                entry.stats.last_error = Some(err_msg.to_string());
            }
        }
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Collect the list of granted permissions for display.
fn collect_permissions(manifest: &crate::manifest::PluginManifest) -> Vec<String> {
    let mut perms = Vec::new();
    let p = &manifest.permissions;
    if p.read_pane_output { perms.push("read_pane_output".to_string()); }
    if p.send_keys { perms.push("send_keys".to_string()); }
    if p.manage_panes { perms.push("manage_panes".to_string()); }
    if p.manage_sessions { perms.push("manage_sessions".to_string()); }
    if p.api_extensions { perms.push("api_extensions".to_string()); }
    if p.run_subprocess { perms.push("run_subprocess".to_string()); }
    if p.network { perms.push("network".to_string()); }
    if p.clipboard { perms.push("clipboard".to_string()); }
    if !p.fs_read.is_empty() { perms.push(format!("fs_read({})", p.fs_read.len())); }
    if !p.fs_write.is_empty() { perms.push(format!("fs_write({})", p.fs_write.len())); }
    perms
}
```

### Step 3: Implement plugin discovery

```rust
// crates/shux-plugin/src/discovery.rs

use std::path::{Path, PathBuf};

use crate::manifest::{PluginManifest, ManifestError};
use crate::types::PluginId;

/// Discovered plugin: manifest + path information.
#[derive(Debug)]
pub struct DiscoveredPlugin {
    pub id: PluginId,
    pub manifest: PluginManifest,
    pub plugin_dir: PathBuf,
    pub wasm_path: Option<PathBuf>,
}

/// Scan configured plugin paths for plugin.toml manifests.
///
/// For each directory in `search_paths`, looks for subdirectories
/// containing `plugin.toml`. Each valid manifest is returned as a
/// DiscoveredPlugin.
pub fn discover_plugins(search_paths: &[PathBuf]) -> Vec<Result<DiscoveredPlugin, DiscoveryError>> {
    let mut results = Vec::new();

    for search_path in search_paths {
        if !search_path.is_dir() {
            tracing::debug!(path = %search_path.display(), "Plugin search path does not exist, skipping");
            continue;
        }

        match std::fs::read_dir(search_path) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let plugin_dir = entry.path();
                    if !plugin_dir.is_dir() {
                        continue;
                    }

                    let manifest_path = plugin_dir.join("plugin.toml");
                    if !manifest_path.exists() {
                        continue;
                    }

                    match discover_single(&plugin_dir, &manifest_path) {
                        Ok(plugin) => results.push(Ok(plugin)),
                        Err(e) => results.push(Err(e)),
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    path = %search_path.display(),
                    error = %e,
                    "Failed to read plugin search path"
                );
            }
        }
    }

    results
}

fn discover_single(
    plugin_dir: &Path,
    manifest_path: &Path,
) -> Result<DiscoveredPlugin, DiscoveryError> {
    let manifest = PluginManifest::load(manifest_path)
        .map_err(|e| DiscoveryError::ManifestError {
            path: manifest_path.to_path_buf(),
            error: e,
        })?;

    // Look for the .wasm file
    let wasm_path = plugin_dir.join("plugin.wasm");
    let wasm_path = if wasm_path.exists() {
        Some(wasm_path)
    } else {
        None
    };

    Ok(DiscoveredPlugin {
        id: PluginId(manifest.plugin.id.clone()),
        manifest,
        plugin_dir: plugin_dir.to_path_buf(),
        wasm_path,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("manifest error in {path}: {error}")]
    ManifestError {
        path: PathBuf,
        error: ManifestError,
    },
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
```

### Step 4: Implement hot reload

```rust
// crates/shux-plugin/src/reload.rs

use crate::types::PluginId;
use crate::lifecycle::LifecycleError;
use crate::registry::PluginRegistry;

/// Hot reload a plugin.
///
/// The hot reload sequence:
/// 1. Stop: Call shutdown() on the plugin
/// 2. Drop the old Store (frees Wasm instances, memories, tables)
/// 3. Engine and Linker persist (shared, not dropped)
/// 4. Enable: Re-compile from (potentially new) .wasm file
/// 5. Start: Create new Store, call init()
/// 6. Re-register: Registrations from new init() replace old ones
///
/// PRD section 7.5 (Hot reload): "Drop the old Store, then create
/// a new Store and re-instantiate from the (potentially new) .wasm file."
pub async fn hot_reload(
    plugin_id: &PluginId,
    registry: &PluginRegistry,
    // engine: &Arc<Engine>,
    // linker: &Linker<PluginState>,
) -> Result<ReloadResult, LifecycleError> {
    tracing::info!(plugin = %plugin_id, "Hot reloading plugin");

    // Step 1: Stop the running plugin
    // - Call shutdown() on the plugin instance
    // - This gives the plugin a chance to clean up
    tracing::debug!(plugin = %plugin_id, "Calling shutdown()");

    // Step 2: Drop the Store
    // - The old Store is dropped, freeing all Wasm state
    // - Engine and Linker persist (shared across all plugins)
    tracing::debug!(plugin = %plugin_id, "Dropping old Store");

    // Step 3: Deactivate old registrations
    registry.deactivate_registrations(plugin_id);

    // Step 4: Re-compile from the (potentially updated) .wasm file
    // - If the .wasm file has changed, the new version is compiled
    // - The .cwasm cache is invalidated and regenerated
    tracing::debug!(plugin = %plugin_id, "Re-compiling from .wasm");

    // Step 5: Create new Store and call init()
    // - New Store with fresh PluginState
    // - Call init(config_json) on the new instance
    // - During init(), registrations are collected (not yet activated)
    tracing::debug!(plugin = %plugin_id, "Creating new Store and calling init()");

    // Step 6: Activate new registrations
    // - Commands, segments, API methods, overrides, layouts are registered
    // registry.activate_registrations(plugin_id, new_registrations);

    tracing::info!(plugin = %plugin_id, "Hot reload complete");

    Ok(ReloadResult {
        plugin_id: plugin_id.clone(),
        new_version: read_manifest_version(plugin_id)?,
    })
}

#[derive(Debug)]
pub struct ReloadResult {
    pub plugin_id: PluginId,
    pub new_version: String,
}
```

### Step 5: Implement GC for idle process plugins

```rust
// In crates/shux-plugin/src/lifecycle.rs (additions)

use std::time::Duration;

/// Default GC timeout for idle process plugins.
const DEFAULT_GC_TIMEOUT: Duration = Duration::from_secs(30);

/// Spawn the plugin GC task.
///
/// Periodically checks for idle plugins (not called in GC_TIMEOUT)
/// and shuts them down. Plugins with gc=false are exempt.
///
/// PRD section 7.6: "Process plugins idle for more than 30 seconds
/// (configurable) receive shutdown and are stopped."
pub fn spawn_gc_task(
    registry: Arc<PluginRegistry>,
    gc_timeout: Duration,
    shutdown: tokio_util::sync::CancellationToken,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(gc_timeout);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let candidates = registry.find_gc_candidates(gc_timeout);
                    for id in candidates {
                        tracing::info!(plugin = %id, "GC: stopping idle plugin");
                        if let Err(err) = lifecycle::stop_plugin(&id, &registry).await {
                            tracing::warn!(plugin = %id, ?err, "GC stop failed");
                        }
                    }
                }
                _ = shutdown.cancelled() => {
                    break;
                }
            }
        }
    });
}
```

### Step 6: Wire plugin.* API methods

```rust
// crates/shux-rpc/src/methods/plugin.rs

use std::sync::Arc;
use serde::Deserialize;
use serde_json::Value;

use super::{MethodContext, MethodResult, MethodError};

/// `plugin.list` — List all known plugins with their state.
pub async fn handle_list(ctx: Arc<MethodContext>, _params: Value) -> MethodResult {
    let plugins = ctx.plugin_registry.list();
    Ok(serde_json::json!({ "plugins": plugins }))
}

/// `plugin.enable` — Enable a plugin (compile, but do not start).
///
/// Params:
///   id: String — plugin identifier
pub async fn handle_enable(ctx: Arc<MethodContext>, params: Value) -> MethodResult {
    #[derive(Deserialize)]
    struct Params { id: String }
    let params: Params = serde_json::from_value(params)
        .map_err(|e| MethodError::invalid_params(e.to_string()))?;

    ctx.plugin_host.enable(&params.id)?;
    Ok(serde_json::json!({"id": params.id, "state": "enabled"}))
}

/// `plugin.disable` — Disable a plugin (stop if running, unload).
pub async fn handle_disable(ctx: Arc<MethodContext>, params: Value) -> MethodResult {
    #[derive(Deserialize)]
    struct Params { id: String }
    let params: Params = serde_json::from_value(params)
        .map_err(|e| MethodError::invalid_params(e.to_string()))?;

    ctx.plugin_host.disable(&params.id)?;
    Ok(serde_json::json!({"id": params.id, "state": "disabled"}))
}

/// `plugin.reload` — Hot-reload a plugin.
pub async fn handle_reload(ctx: Arc<MethodContext>, params: Value) -> MethodResult {
    #[derive(Deserialize)]
    struct Params { id: String }
    let params: Params = serde_json::from_value(params)
        .map_err(|e| MethodError::invalid_params(e.to_string()))?;

    let result = ctx.plugin_host.reload(&params.id)?;
    Ok(serde_json::json!({"id": params.id, "reloaded": true, "version": result.new_version}))
}

/// `plugin.inspect` — Get detailed information about a plugin.
pub async fn handle_inspect(ctx: Arc<MethodContext>, params: Value) -> MethodResult {
    #[derive(Deserialize)]
    struct Params { id: String }
    let params: Params = serde_json::from_value(params)
        .map_err(|e| MethodError::invalid_params(e.to_string()))?;

    let info = ctx.plugin_registry.inspect(&PluginId(params.id))?;
    Ok(serde_json::to_value(info)?)
}
```

### Step 7: Add CLI commands for plugin management

```rust
// CLI subcommands (in the shux binary crate):
// shux plugin ls              -> plugin.list
// shux plugin enable <id>     -> plugin.enable
// shux plugin disable <id>    -> plugin.disable
// shux plugin reload <id>     -> plugin.reload
// shux plugin inspect <id>    -> plugin.inspect
```

### Step 8: Cover session persistence lifecycle contract

Add a lifecycle validation case for the session persistence plugin (PRD reliability requirement):

- Persist snapshots on interval (`~1s` default) while plugin is active.
- On daemon restart/crash recovery, surface a restore decision prompt and only apply restore on explicit confirmation.
- Detect snapshot/schema mismatches (e.g., pane command drift) and fail safe with a warning + skip restore.
- Ensure disable/unload clears scheduled persistence timers and in-memory restore state.

### Step 9: Write tests

```rust
#[cfg(test)]
mod lifecycle_tests {
    use crate::lifecycle::*;
    use crate::types::PluginLifecycleState::*;

    #[test]
    fn test_valid_transitions() {
        assert!(validate_transition(Discovered, Validated).is_ok());
        assert!(validate_transition(Validated, Enabled).is_ok());
        assert!(validate_transition(Enabled, Started).is_ok());
        assert!(validate_transition(Started, Stopped).is_ok());
        assert!(validate_transition(Stopped, Enabled).is_ok());
        assert!(validate_transition(Stopped, Disabled).is_ok());
        assert!(validate_transition(Enabled, Disabled).is_ok());
        assert!(validate_transition(Error, Disabled).is_ok());
        assert!(validate_transition(Disabled, Validated).is_ok());
    }

    #[test]
    fn test_invalid_transitions() {
        assert!(validate_transition(Discovered, Started).is_err());
        assert!(validate_transition(Disabled, Started).is_err());
        assert!(validate_transition(Started, Discovered).is_err());
        assert!(validate_transition(Stopped, Started).is_err());
    }

    #[test]
    fn test_any_to_error_is_valid() {
        assert!(validate_transition(Discovered, Error).is_ok());
        assert!(validate_transition(Validated, Error).is_ok());
        assert!(validate_transition(Enabled, Error).is_ok());
        assert!(validate_transition(Started, Error).is_ok());
        assert!(validate_transition(Stopped, Error).is_ok());
        assert!(validate_transition(Disabled, Error).is_ok());
    }
}

#[cfg(test)]
mod registry_tests {
    use crate::registry::*;
    use crate::types::{PluginId, PluginLifecycleState};
    use crate::manifest::{PluginManifest, PluginMeta, PluginKind};

    fn test_manifest(id: &str) -> PluginManifest {
        PluginManifest {
            plugin: PluginMeta {
                id: id.to_string(),
                name: "Test".to_string(),
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
            permissions: Default::default(),
            extensions: Default::default(),
            dependencies: Default::default(),
            conflicts: Default::default(),
        }
    }

    #[test]
    fn test_register_and_list() {
        let registry = PluginRegistry::new();
        let id = PluginId("com.test.plugin".to_string());
        registry.register(id.clone(), test_manifest("com.test.plugin"));

        let plugins = registry.list();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].id, "com.test.plugin");
        assert_eq!(plugins[0].state, PluginLifecycleState::Discovered);
    }

    #[test]
    fn test_state_transitions() {
        let registry = PluginRegistry::new();
        let id = PluginId("com.test.plugin".to_string());
        registry.register(id.clone(), test_manifest("com.test.plugin"));

        assert!(registry.set_state(&id, PluginLifecycleState::Validated).is_ok());
        assert!(registry.set_state(&id, PluginLifecycleState::Enabled).is_ok());
        assert!(registry.set_state(&id, PluginLifecycleState::Started).is_ok());

        // Invalid transition
        assert!(registry.set_state(&id, PluginLifecycleState::Discovered).is_err());
    }

    #[test]
    fn test_activate_and_deactivate_registrations() {
        let registry = PluginRegistry::new();
        let id = PluginId("com.test.plugin".to_string());
        registry.register(id.clone(), test_manifest("com.test.plugin"));

        let regs = PluginRegistrations {
            commands: vec!["test.cmd".to_string()],
            status_segments: vec!["test_seg".to_string()],
            ..Default::default()
        };

        registry.activate_registrations(&id, regs);
        let info = registry.inspect(&id).unwrap();
        assert_eq!(info.extensions.commands, vec!["test.cmd"]);

        registry.deactivate_registrations(&id);
        let info = registry.inspect(&id).unwrap();
        assert!(info.extensions.commands.is_empty());
    }

    #[test]
    fn test_gc_candidates() {
        let registry = PluginRegistry::new();
        let id = PluginId("com.test.plugin".to_string());
        registry.register(id.clone(), test_manifest("com.test.plugin"));

        // Plugin is Discovered, not Started, so not a GC candidate
        let candidates = registry.find_gc_candidates(std::time::Duration::from_millis(1));
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_record_call_stats() {
        let registry = PluginRegistry::new();
        let id = PluginId("com.test.plugin".to_string());
        registry.register(id.clone(), test_manifest("com.test.plugin"));

        registry.record_call(&id, 100, None);
        registry.record_call(&id, 200, None);
        registry.record_call(&id, 500, Some("test error"));

        let info = registry.inspect(&id).unwrap();
        let stats = info.stats.unwrap();
        assert_eq!(stats.call_count, 3);
        assert_eq!(stats.error_count, 1);
        assert!(stats.last_error.is_some());
    }
}

#[cfg(test)]
mod discovery_tests {
    use super::discovery::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_discover_valid_plugin() {
        let dir = TempDir::new().unwrap();
        let plugin_dir = dir.path().join("my-plugin");
        std::fs::create_dir(&plugin_dir).unwrap();

        let manifest_content = r#"
            [plugin]
            id = "com.example.my-plugin"
            name = "My Plugin"
            version = "0.1.0"
            api = "shux:plugin@1.0.0"
            kind = "wasm"
        "#;
        std::fs::write(plugin_dir.join("plugin.toml"), manifest_content).unwrap();

        let results = discover_plugins(&[dir.path().to_path_buf()]);
        assert_eq!(results.len(), 1);
        assert!(results[0].is_ok());

        let plugin = results[0].as_ref().unwrap();
        assert_eq!(plugin.id.0, "com.example.my-plugin");
    }

    #[test]
    fn test_discover_skips_invalid_manifest() {
        let dir = TempDir::new().unwrap();
        let plugin_dir = dir.path().join("bad-plugin");
        std::fs::create_dir(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("plugin.toml"), "invalid toml {{{").unwrap();

        let results = discover_plugins(&[dir.path().to_path_buf()]);
        assert_eq!(results.len(), 1);
        assert!(results[0].is_err());
    }

    #[test]
    fn test_discover_skips_nonexistent_path() {
        let results = discover_plugins(&[std::path::PathBuf::from("/nonexistent/path")]);
        assert!(results.is_empty());
    }
}
```

---

## Verification

### Functional

```bash
# Build all affected crates
cargo build -p shux-plugin -p shux-rpc

# Verify clippy
cargo clippy -p shux-plugin -p shux-rpc -- -D warnings

# Integration test: plugin lifecycle
# 1. Place a test plugin in the configured plugin path
# 2. shux plugin ls → shows discovered plugin
# 3. shux plugin enable <id> → state changes to enabled
# 4. shux plugin reload <id> → hot reload succeeds
# 5. shux plugin disable <id> → state changes to disabled
```

### Tests

```bash
# Lifecycle tests
cargo nextest run -p shux-plugin -- lifecycle

# Registry tests
cargo nextest run -p shux-plugin -- registry

# Discovery tests
cargo nextest run -p shux-plugin -- discovery

# All plugin tests
cargo nextest run -p shux-plugin

# Full workspace
cargo nextest run --workspace
```

---

## Completion Criteria

- [ ] Lifecycle state machine: Discovered -> Validated -> Enabled -> Started -> Stopped -> Disabled (with Error from any state)
- [ ] All state transitions validated; invalid transitions rejected with clear errors
- [ ] Registration-activation separation: plugins can only register during init(); registrations activated after init() completes
- [ ] `PluginRegistry` tracks all plugins, states, registrations, and performance stats
- [ ] Registration activation: commands, status segments, API methods, command overrides, layouts registered globally
- [ ] Registration deactivation: all registrations removed on plugin disable
- [ ] Plugin discovery: scans configured paths for plugin.toml manifests
- [ ] Hot reload: Stop -> Drop Store -> Recompile -> New Store -> init() -> Re-register
- [ ] `plugin.reload` reports the manifest version of the newly loaded artifact
- [ ] Engine and Linker persist across hot reload (only Store is dropped)
- [ ] Plugin GC: idle process plugins stopped after 30s (configurable)
- [ ] Session persistence lifecycle is covered (periodic snapshot, explicit restore confirmation, mismatch fail-safe)
- [ ] Plugins with gc=false hold daemon lease (prevent auto-exit)
- [ ] Call statistics tracked: call_count, avg_call_us, p99_call_us, error_count
- [ ] `plugin.list`, `plugin.enable`, `plugin.disable`, `plugin.reload`, `plugin.inspect` API methods
- [ ] CLI: `shux plugin ls`, `shux plugin enable <id>`, `shux plugin reload <id>`
- [ ] All unit tests pass (lifecycle transitions, registry, discovery, GC candidates)
- [ ] `cargo clippy --workspace -- -D warnings` passes

---

## Commit Message
```
feat(plugin): add lifecycle state machine, registry, discovery, and hot reload

- Plugin lifecycle: Discover -> Validate -> Enable -> Start -> Handle -> Stop -> Disable
- Registration-activation separation pattern (PI-inspired)
- PluginRegistry with state tracking, registration management, call stats
- Plugin discovery: scan paths for plugin.toml manifests
- Hot reload: drop Store + recompile + new Store + re-register
- Plugin GC for idle process plugins (30s default)
- plugin.list/enable/disable/reload/inspect API methods
- CLI: shux plugin ls/enable/disable/reload
```

---

## Session Protocol

1. **Before starting:** Read `CLAUDE.md`. Read tasks 038-040 to understand the wasmtime integration, permission system, and host function implementations. Read PRD sections 7.1 (hot reloadable), 7.4 (lifecycle), 7.7 (bundled plugins), 7.8 (plugin DX). Verify task 040 is complete.
2. **During:** Implement in order: lifecycle state machine (Step 1) -> registry (Step 2) -> discovery (Step 3) -> hot reload (Step 4) -> GC (Step 5) -> API methods (Step 6) -> CLI (Step 7) -> session persistence contract (Step 8) -> tests (Step 9). Run `cargo check` after each step. The lifecycle and registry are independent and can be tested in isolation.
3. **Testing:** Focus on lifecycle transition validation (every valid and invalid transition), registry registration/deactivation round-trip, and discovery with valid/invalid/missing manifests. The GC test should verify that idle plugins are identified but should NOT actually stop them (that requires integration).
4. **After:** Run `make check`. Update `docs/PROGRESS.md` (mark 041 as done, completing M2 plugin tasks). Update `CLAUDE.md` Learnings with discoveries about registration-activation patterns, hot reload timing, or RwLock contention in the registry.
5. **Watch out for:**
   - The registration-activation separation is subtle: during init(), plugins call register-api-method and set-status-segment, but these must be collected without activating them. Only after init() returns successfully are they committed to the global registry. If init() fails, nothing is registered.
   - Hot reload must handle the case where the new .wasm file fails to compile. In that case, the old registrations should be preserved (fail-safe: don't deactivate until the new version is confirmed working).
   - The GC task should be configurable via the `[plugins].process_gc_timeout_secs` config key (PRD section 10.2).
   - Command overrides use last-loaded-wins semantics. The registry should warn when a conflict is detected but allow it.
   - Discovery should handle race conditions: a plugin directory being deleted while scanning, or a plugin.toml being written while reading.

---

## Audit Note

- PRD reliability expectations include session-persistence lifecycle behavior; this task now explicitly requires snapshot cadence, restore confirmation, mismatch fail-safe handling, and cleanup on disable/unload.
