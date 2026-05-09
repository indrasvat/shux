# 038 — Plugin Host: Wasmtime Integration

**Status:** Pending
**Depends On:** 034
**Parallelizable With:** 035, 037

---

## Problem

The plugin system is shux's crown jewel (PRD section 7). Without the Wasm plugin host, there are no bundled plugins (status-bar, theme-pack, diagnostics), no third-party extensions, and no proof that the plugin API surface is sufficient. This task establishes the foundational wasmtime integration: Engine creation, Linker setup with all host function stubs, per-plugin Store management, WIT binding generation, plugin loading from `.wasm` files, and pre-compilation caching for fast startup.

The architecture follows wasmtime 41+ best practices:
- **Engine**: Created once at daemon startup, shared across all plugins. Configures epoch interruption and resource limits.
- **Linker**: Created once, shared. Contains all host function implementations that plugins can call.
- **Store**: Created per plugin instance. Contains the plugin's memory, tables, and instance state. Dropped and recreated on hot reload.

This separation is critical for hot reload (task 041): dropping only the Store preserves the Engine and Linker, allowing a new plugin version to be instantiated without recompiling the host function bindings.

## PRD Reference

- **section 7** — Plugin system: crown jewel, design goals, extension points
- **section 7.1** — Design goals: first-class, safe by default, hot reloadable, language-agnostic, debuggable, composable, overridable, interception-capable
- **section 7.5** — WIT interface: `package shux:plugin@1.0.0`, `interface host`, `interface plugin`, `world runtime`
- **section 15.2** — wasmtime 41+: Component Model, WASI Preview 2, epoch interruption, ResourceLimiter
- **section 14.1** — Performance: Wasm instantiation p50 <= 50us (pre-compiled), function call p99 <= 100us

---

## Files to Create

- `crates/shux-plugin/src/engine.rs` — Shared wasmtime `Engine` configuration and creation
- `crates/shux-plugin/src/host.rs` — Shared `Linker` with host function stubs (full implementation in task 040)
- `crates/shux-plugin/src/wasm.rs` — Per-plugin `Store`, `.wasm` loading, instantiation, pre-compilation cache
- `crates/shux-plugin/src/manifest.rs` — `plugin.toml` parser and validator
- `crates/shux-plugin/src/types.rs` — Shared types: PluginId, PluginInfo, PluginState
- `wit/shux-plugin.wit` — WIT interface definition from PRD section 7.5

## Files to Modify

- `crates/shux-plugin/src/lib.rs` — Export modules, define PluginHost top-level API
- `crates/shux-plugin/Cargo.toml` — Add dependencies: `wasmtime`, `wasmtime-wasi`, `toml`, `serde`

---

## Execution Steps

### Step 1: Create the WIT interface definition

Copy the WIT definition from PRD section 7.5 into the project. This is the contract between the host (shux daemon) and plugins.

```wit
// wit/shux-plugin.wit

package shux:plugin@1.0.0;

interface host {
  // Types
  record pane-info {
    id: string,
    title: string,
    cwd: string,
    command: string,
    is-focused: bool,
    width: u16,
    height: u16,
    exit-code: option<s32>,
    tags: list<key-value>,
  }

  record window-info {
    id: string,
    name: string,
    pane-ids: list<string>,
    active-pane-id: string,
  }

  record session-info {
    id: string,
    name: string,
    window-ids: list<string>,
    active-window-id: string,
    created-at: u64,
  }

  record key-value {
    key: string,
    value: string,
  }

  enum log-level { trace, debug, info, warn, error }
  enum split-direction { horizontal, vertical }

  record host-error { code: s32, message: string }

  record pane-create-options {
    window-id: string,
    command: option<string>,
    cwd: option<string>,
    env: list<key-value>,
    name: option<string>,
  }

  record split-options {
    target-pane-id: string,
    direction: split-direction,
    size-percent: option<u8>,
    command: option<string>,
    cwd: option<string>,
  }

  record floating-pane-options {
    width: option<u16>,
    height: option<u16>,
    x: option<u16>,
    y: option<u16>,
    command: option<string>,
    cwd: option<string>,
    name: option<string>,
  }

  // Queries (no permissions required)
  get-active-pane: func() -> result<pane-info, host-error>;
  get-pane: func(id: string) -> result<pane-info, host-error>;
  list-panes: func() -> result<list<pane-info>, host-error>;
  get-active-window: func() -> result<window-info, host-error>;
  get-window: func(id: string) -> result<window-info, host-error>;
  list-windows: func() -> result<list<window-info>, host-error>;
  get-active-session: func() -> result<session-info, host-error>;
  get-session: func(id: string) -> result<session-info, host-error>;
  list-sessions: func() -> result<list<session-info>, host-error>;
  get-config: func(key: string) -> result<option<string>, host-error>;

  // Pane lifecycle (requires: manage_panes)
  create-pane: func(options: pane-create-options) -> result<string, host-error>;
  split-pane: func(options: split-options) -> result<string, host-error>;
  create-floating-pane: func(options: floating-pane-options) -> result<string, host-error>;
  close-pane: func(pane-id: string) -> result<_, host-error>;
  toggle-floating-pane: func(pane-id: string) -> result<_, host-error>;

  // Pane interaction (requires: send_keys / read_pane_output)
  send-keys: func(pane-id: string, data: list<u8>) -> result<_, host-error>;
  send-text: func(pane-id: string, text: string) -> result<_, host-error>;
  read-pane-output: func(pane-id: string, lines: u32) -> result<string, host-error>;
  read-pane-scrollback: func(pane-id: string, offset: u32, lines: u32) -> result<string, host-error>;

  // Pane manipulation (requires: manage_panes)
  focus-pane: func(pane-id: string) -> result<_, host-error>;
  resize-pane: func(pane-id: string, width: u16, height: u16) -> result<_, host-error>;
  rename-pane: func(pane-id: string, name: string) -> result<_, host-error>;
  set-pane-tag: func(pane-id: string, key: string, value: string) -> result<_, host-error>;
  clear-pane-tag: func(pane-id: string, key: string) -> result<_, host-error>;

  // Window/session lifecycle (requires: manage_sessions)
  create-session: func(name: string) -> result<string, host-error>;
  create-window: func(session-id: string, name: string) -> result<string, host-error>;
  close-window: func(window-id: string) -> result<_, host-error>;
  kill-session: func(session-id: string) -> result<_, host-error>;
  rename-session: func(session-id: string, name: string) -> result<_, host-error>;
  rename-window: func(window-id: string, name: string) -> result<_, host-error>;
  focus-window: func(window-id: string) -> result<_, host-error>;

  // Layout (requires: manage_panes)
  register-layout: func(name: string, layout-json: string) -> result<_, host-error>;
  apply-layout: func(name: string) -> result<_, host-error>;

  // Display & status (no extra permissions)
  set-status-segment: func(id: string, text: string) -> result<_, host-error>;
  set-badge: func(pane-id: string, badge: string) -> result<_, host-error>;
  clear-badge: func(pane-id: string) -> result<_, host-error>;
  emit-event: func(event-type: string, data-json: string) -> result<_, host-error>;

  // Overlays
  show-overlay: func(pane-id: string) -> result<_, host-error>;
  hide-overlay: func(pane-id: string) -> result<_, host-error>;

  // Clipboard (requires: clipboard)
  get-clipboard: func() -> result<string, host-error>;
  set-clipboard: func(content: string) -> result<_, host-error>;

  // API extension (requires: api_extensions)
  register-api-method: func(method-name: string, description: string) -> result<_, host-error>;
  register-command-override: func(command-name: string) -> result<_, host-error>;

  // Utilities
  log: func(level: log-level, msg: string);
  read-file: func(path: string) -> result<list<u8>, host-error>;
  write-file: func(path: string, data: list<u8>) -> result<_, host-error>;

  /// Run a command and return its stdout. Requires the exec permission.
  /// The command runs in a subprocess (not a pane). Max 30s timeout.
  /// Uses direct process invocation without a shell to prevent injection.
  run-subprocess: func(command: string, args: list<string>) -> result<string, host-error>;
}

interface plugin {
  record plugin-error { code: s32, message: string }

  init: func(config-json: string) -> result<_, plugin-error>;
  shutdown: func();
  on-event: func(event-json: string) -> result<_, plugin-error>;
  intercept-event: func(event-json: string) -> result<option<string>, plugin-error>;
  on-command: func(name: string, args: list<string>) -> result<string, plugin-error>;
  render-segment: func(id: string, width: u16) -> result<string, plugin-error>;
  render-overlay: func(pane-id: string, width: u16, height: u16) -> result<option<string>, plugin-error>;
  on-overlay-input: func(pane-id: string, key-event-json: string) -> result<bool, plugin-error>;
}

world runtime {
  import host;
  export plugin;
}
```

### Step 2: Configure the wasmtime Engine

The Engine is created once at daemon startup and shared across all plugins.

```rust
// crates/shux-plugin/src/engine.rs

use std::sync::Arc;
use std::time::Duration;

use wasmtime::{Config, Engine, Result as WasmResult};

/// Default memory limit per plugin (16 MB).
const DEFAULT_MEMORY_LIMIT: usize = 16 * 1024 * 1024;

/// Epoch interruption interval. The daemon increments the epoch every
/// 10ms; plugins get a budget of 10 epochs (100ms) before being killed.
const EPOCH_TICK_INTERVAL: Duration = Duration::from_millis(10);

/// Number of epoch ticks before a plugin is killed.
const EPOCH_DEADLINE_TICKS: u64 = 10; // 10 * 10ms = 100ms

/// Create and configure the shared wasmtime Engine.
///
/// This Engine is shared across all plugin instances. Configuration
/// includes:
/// - Component Model support (WASI Preview 2)
/// - Epoch-based interruption for CPU timeout enforcement
/// - Pre-compilation cache support
/// - Cranelift backend for JIT compilation
pub fn create_engine() -> WasmResult<Engine> {
    let mut config = Config::new();

    // Enable Component Model (required for WIT-based plugins)
    config.wasm_component_model(true);

    // Enable epoch-based interruption for CPU timeouts.
    // The daemon spawns a background task that increments the epoch
    // every EPOCH_TICK_INTERVAL. Each plugin gets a deadline of
    // EPOCH_DEADLINE_TICKS epochs. If exceeded, the Wasm execution
    // traps with an interrupt error.
    config.epoch_interruption(true);

    // Cache compiled modules for fast startup.
    // Pre-compiled .cwasm files are stored alongside .wasm files.
    config.cranelift_opt_level(wasmtime::OptLevel::Speed);

    // Enable async support for host functions that need to await
    config.async_support(true);

    Engine::new(&config)
}

/// Start the epoch ticker background task.
///
/// This task increments the engine's epoch counter every EPOCH_TICK_INTERVAL.
/// Plugins that exceed their epoch deadline are interrupted (their Wasm
/// execution traps).
pub fn start_epoch_ticker(
    engine: Arc<Engine>,
    shutdown: tokio_util::sync::CancellationToken,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(EPOCH_TICK_INTERVAL);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    engine.increment_epoch();
                }
                _ = shutdown.cancelled() => {
                    break;
                }
            }
        }
    });
}

/// Get the epoch deadline ticks for plugin configuration.
pub fn epoch_deadline_ticks() -> u64 {
    EPOCH_DEADLINE_TICKS
}
```

### Step 3: Parse plugin.toml manifests

```rust
// crates/shux-plugin/src/manifest.rs

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Parsed plugin.toml manifest.
#[derive(Debug, Clone, Deserialize)]
pub struct PluginManifest {
    pub plugin: PluginMeta,
    #[serde(default)]
    pub permissions: PluginPermissions,
    #[serde(default)]
    pub extensions: PluginExtensions,
    #[serde(default)]
    pub dependencies: HashMap<String, String>,
    #[serde(default)]
    pub conflicts: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PluginMeta {
    /// Unique plugin identifier (reverse-domain, e.g., "com.example.git-status").
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Semantic version.
    pub version: String,
    /// API version this plugin targets (e.g., "shux:plugin@1.0.0").
    pub api: String,
    /// Plugin type: "wasm" or "process".
    pub kind: PluginKind,
    /// Description.
    #[serde(default)]
    pub description: String,
    /// Homepage URL.
    #[serde(default)]
    pub homepage: Option<String>,
    /// License (SPDX identifier).
    #[serde(default)]
    pub license: Option<String>,
    /// Minimum shux version required.
    #[serde(default)]
    pub min_shux: Option<String>,
    /// Whether to disable GC for this plugin (keep running indefinitely).
    #[serde(default)]
    pub gc: Option<bool>,
    /// Plugin metadata for registry/marketplace.
    #[serde(default)]
    pub metadata: Option<PluginMetadata>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PluginMetadata {
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    Wasm,
    Process,
}

/// Plugin permissions declared in plugin.toml [permissions].
///
/// Each permission controls access to specific host functions.
/// The host enforces these at every call boundary.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PluginPermissions {
    /// Event types this plugin can receive.
    #[serde(default)]
    pub events: Vec<String>,
    /// Can call read-pane-output / read-pane-scrollback.
    #[serde(default)]
    pub read_pane_output: bool,
    /// Can call send-keys (input injection -- security-sensitive).
    #[serde(default)]
    pub send_keys: bool,
    /// Can create, close, resize, focus, split panes and layouts.
    #[serde(default)]
    pub manage_panes: bool,
    /// Can create, kill, rename sessions and windows.
    #[serde(default)]
    pub manage_sessions: bool,
    /// Can register new JSON-RPC API methods.
    #[serde(default)]
    pub api_extensions: bool,
    /// Can run arbitrary subprocess commands. Uses direct process
    /// invocation (no shell) with scrubbed environment variables.
    #[serde(default)]
    pub run_subprocess: bool,
    /// Filesystem read paths (glob patterns).
    #[serde(default)]
    pub fs_read: Vec<String>,
    /// Filesystem write paths (glob patterns).
    #[serde(default)]
    pub fs_write: Vec<String>,
    /// Can make network requests.
    #[serde(default)]
    pub network: bool,
    /// Can access system clipboard.
    #[serde(default)]
    pub clipboard: bool,
    /// Events this plugin can intercept (block/modify).
    #[serde(default)]
    pub intercept_events: Vec<String>,
    /// Built-in commands this plugin can override.
    #[serde(default)]
    pub override_commands: Vec<String>,
}

/// Plugin extension declarations.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PluginExtensions {
    /// Status bar segment IDs this plugin provides.
    #[serde(default)]
    pub status_segments: Vec<String>,
    /// Command names this plugin provides.
    #[serde(default)]
    pub commands: Vec<String>,
    /// Theme names this plugin provides.
    #[serde(default)]
    pub themes: Vec<String>,
    /// Layout names this plugin provides.
    #[serde(default)]
    pub layouts: Vec<String>,
}

impl PluginManifest {
    /// Load and parse a plugin.toml from the given path.
    pub fn load(path: &Path) -> Result<Self, ManifestError> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| ManifestError::Io(path.to_path_buf(), e))?;
        let manifest: Self = toml::from_str(&content)
            .map_err(|e| ManifestError::Parse(path.to_path_buf(), e))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Validate the manifest for correctness.
    pub fn validate(&self) -> Result<(), ManifestError> {
        // Check API version compatibility
        if !self.plugin.api.starts_with("shux:plugin@") {
            return Err(ManifestError::InvalidApi(self.plugin.api.clone()));
        }

        // Parse the API version and check major version compatibility
        let api_version = self
            .plugin
            .api
            .strip_prefix("shux:plugin@")
            .unwrap_or("");
        let parts: Vec<&str> = api_version.split('.').collect();
        if parts.len() < 2 {
            return Err(ManifestError::InvalidApi(self.plugin.api.clone()));
        }
        let major: u32 = parts[0]
            .parse()
            .map_err(|_| ManifestError::InvalidApi(self.plugin.api.clone()))?;
        if major != 1 {
            return Err(ManifestError::IncompatibleApi {
                plugin: self.plugin.api.clone(),
                host: "shux:plugin@1.0.0".to_string(),
            });
        }

        // Validate plugin ID format (reverse-domain)
        if self.plugin.id.is_empty() || !self.plugin.id.contains('.') {
            return Err(ManifestError::InvalidId(self.plugin.id.clone()));
        }

        Ok(())
    }

    /// Check if this plugin has a specific permission.
    pub fn has_permission(&self, perm: &str) -> bool {
        match perm {
            "read_pane_output" => self.permissions.read_pane_output,
            "send_keys" => self.permissions.send_keys,
            "manage_panes" => self.permissions.manage_panes,
            "manage_sessions" => self.permissions.manage_sessions,
            "api_extensions" => self.permissions.api_extensions,
            "run_subprocess" => self.permissions.run_subprocess,
            "network" => self.permissions.network,
            "clipboard" => self.permissions.clipboard,
            _ => false,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("failed to read {0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("failed to parse {0}: {1}")]
    Parse(PathBuf, toml::de::Error),
    #[error("invalid API version: {0}")]
    InvalidApi(String),
    #[error("incompatible API version: plugin requires {plugin}, host provides {host}")]
    IncompatibleApi { plugin: String, host: String },
    #[error("invalid plugin ID (must be reverse-domain format): {0}")]
    InvalidId(String),
}
```

### Step 4: Implement per-plugin Store and Wasm loading

```rust
// crates/shux-plugin/src/wasm.rs

use std::path::{Path, PathBuf};
use std::sync::Arc;

use wasmtime::{
    component::Component,
    Engine, Store,
};

use crate::engine::epoch_deadline_ticks;
use crate::manifest::PluginManifest;

/// Per-plugin state stored in the wasmtime Store.
///
/// This is dropped and recreated on hot reload (task 041).
/// The Engine and Linker persist across reloads.
pub struct PluginState {
    /// Plugin manifest (permissions, metadata).
    pub manifest: PluginManifest,
    /// Plugin identifier for logging and error reporting.
    pub id: String,
    // Future: references to daemon subsystems for host function dispatch
    // pub graph_handle: Arc<SessionGraphHandle>,
    // pub event_bus: Arc<EventBus>,
}

/// A loaded Wasm plugin instance.
pub struct WasmPlugin {
    /// The compiled Wasm component.
    component: Component,
    /// The per-plugin store (dropped on hot reload).
    store: Store<PluginState>,
    /// Path to the .wasm file (for reload).
    wasm_path: PathBuf,
    /// Path to the pre-compiled .cwasm cache file.
    cwasm_path: PathBuf,
}

impl WasmPlugin {
    /// Load a Wasm plugin from a .wasm file.
    ///
    /// Attempts to load a pre-compiled .cwasm cache first. If the cache
    /// is missing or stale, compiles the .wasm and caches the result.
    ///
    /// PRD section 14.1 target: p50 <= 50us for pre-compiled instantiation.
    pub fn load(
        engine: &Engine,
        wasm_path: &Path,
        manifest: PluginManifest,
    ) -> Result<Self, PluginLoadError> {
        let cwasm_path = wasm_path.with_extension("cwasm");

        // Try to load pre-compiled component
        let component = if cwasm_path.exists() {
            match Self::load_precompiled(engine, &cwasm_path) {
                Ok(component) => {
                    tracing::debug!(
                        plugin = %manifest.plugin.id,
                        "Loaded pre-compiled component"
                    );
                    component
                }
                Err(e) => {
                    tracing::warn!(
                        plugin = %manifest.plugin.id,
                        error = %e,
                        "Pre-compiled cache invalid, recompiling"
                    );
                    Self::compile_and_cache(engine, wasm_path, &cwasm_path)?
                }
            }
        } else {
            Self::compile_and_cache(engine, wasm_path, &cwasm_path)?
        };

        // Create the per-plugin Store
        let state = PluginState {
            id: manifest.plugin.id.clone(),
            manifest: manifest.clone(),
        };
        let mut store = Store::new(engine, state);

        // Configure epoch deadline for CPU timeout
        store.set_epoch_deadline(epoch_deadline_ticks());

        Ok(Self {
            component,
            store,
            wasm_path: wasm_path.to_path_buf(),
            cwasm_path,
        })
    }

    /// Load a pre-compiled .cwasm component.
    ///
    /// SAFETY: The .cwasm file is assumed to be trusted (generated by
    /// the same Engine configuration). Pre-compiled modules from
    /// untrusted sources could contain malicious code.
    fn load_precompiled(
        engine: &Engine,
        cwasm_path: &Path,
    ) -> Result<Component, PluginLoadError> {
        let bytes = std::fs::read(cwasm_path)
            .map_err(|e| PluginLoadError::Io(cwasm_path.to_path_buf(), e))?;
        // SAFETY: We trust the cwasm file because we compiled it ourselves.
        // The Engine verifies the compilation fingerprint matches.
        unsafe {
            Component::deserialize(engine, &bytes)
                .map_err(|e| PluginLoadError::Wasmtime(e.to_string()))
        }
    }

    /// Compile a .wasm file and cache the result as .cwasm.
    fn compile_and_cache(
        engine: &Engine,
        wasm_path: &Path,
        cwasm_path: &Path,
    ) -> Result<Component, PluginLoadError> {
        let wasm_bytes = std::fs::read(wasm_path)
            .map_err(|e| PluginLoadError::Io(wasm_path.to_path_buf(), e))?;

        let component = Component::new(engine, &wasm_bytes)
            .map_err(|e| PluginLoadError::Wasmtime(e.to_string()))?;

        // Cache the pre-compiled component
        match component.serialize() {
            Ok(bytes) => {
                if let Err(e) = std::fs::write(cwasm_path, &bytes) {
                    tracing::warn!(
                        path = %cwasm_path.display(),
                        error = %e,
                        "Failed to cache pre-compiled component"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to serialize component for caching");
            }
        }

        Ok(component)
    }

    /// Get a reference to the plugin state.
    pub fn state(&self) -> &PluginState {
        self.store.data()
    }

    /// Get a mutable reference to the Store (for calling plugin functions).
    pub fn store_mut(&mut self) -> &mut Store<PluginState> {
        &mut self.store
    }

    /// Path to the .wasm file.
    pub fn wasm_path(&self) -> &Path {
        &self.wasm_path
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PluginLoadError {
    #[error("I/O error reading {0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("wasmtime error: {0}")]
    Wasmtime(String),
    #[error("manifest error: {0}")]
    Manifest(#[from] crate::manifest::ManifestError),
}
```

### Step 5: Define shared types

```rust
// crates/shux-plugin/src/types.rs

use serde::{Deserialize, Serialize};

/// Unique identifier for a plugin.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct PluginId(pub String);

impl std::fmt::Display for PluginId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Current state of a plugin in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginLifecycleState {
    /// Plugin has been discovered but not loaded.
    Discovered,
    /// Plugin manifest has been validated.
    Validated,
    /// Plugin is enabled (compiled and ready to instantiate).
    Enabled,
    /// Plugin is running (Store created, init() called).
    Started,
    /// Plugin is stopped (shutdown() called, Store dropped).
    Stopped,
    /// Plugin is disabled (unloaded, handlers deregistered).
    Disabled,
    /// Plugin encountered an error.
    Error,
}

/// Summary information about a plugin, returned by plugin.list and
/// plugin.inspect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub kind: String,
    pub state: PluginLifecycleState,
    pub description: String,
    /// Permissions declared by this plugin.
    pub permissions: Vec<String>,
    /// Extension points registered by this plugin.
    pub extensions: PluginExtensionInfo,
    /// Performance statistics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<PluginStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginExtensionInfo {
    pub commands: Vec<String>,
    pub status_segments: Vec<String>,
    pub themes: Vec<String>,
    pub api_methods: Vec<String>,
    pub command_overrides: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginStats {
    /// Number of times the plugin has been called.
    pub call_count: u64,
    /// Average call duration in microseconds.
    pub avg_call_us: u64,
    /// P99 call duration in microseconds.
    pub p99_call_us: u64,
    /// Number of errors encountered.
    pub error_count: u64,
    /// Last error message (if any).
    pub last_error: Option<String>,
}
```

### Step 6: Wire up the PluginHost top-level API

```rust
// crates/shux-plugin/src/lib.rs

//! shux-plugin -- Plugin host for Wasm (wasmtime) and process plugins.
//!
//! The plugin system is shux's crown jewel. This crate provides:
//! - Wasm plugin loading, compilation, and caching
//! - Plugin manifest (plugin.toml) parsing and validation
//! - WIT-based host function interface
//! - Permission enforcement
//! - Plugin lifecycle management
//! - Hot reload support

pub mod engine;
pub mod host;
pub mod manifest;
pub mod types;
pub mod wasm;

use std::sync::Arc;

use engine::create_engine;
use wasmtime::Engine;

/// The top-level plugin host.
///
/// Created once at daemon startup. Manages all plugin instances.
pub struct PluginHost {
    /// Shared wasmtime Engine (created once).
    engine: Arc<Engine>,
}

impl PluginHost {
    /// Create a new plugin host.
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let engine = Arc::new(create_engine()?);

        Ok(Self {
            engine,
        })
    }

    /// Get a reference to the shared Engine.
    pub fn engine(&self) -> &Arc<Engine> {
        &self.engine
    }
}
```

### Step 7: Update Cargo.toml

```toml
# crates/shux-plugin/Cargo.toml
[package]
name = "shux-plugin"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
shux-core = { path = "../shux-core" }
wasmtime = { workspace = true }
wasmtime-wasi = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
toml = { workspace = true }
tracing = { workspace = true }
thiserror = { workspace = true }
tokio = { workspace = true }
tokio-util = { workspace = true }
uuid = { workspace = true }
```

### Step 8: Write unit tests

```rust
#[cfg(test)]
mod manifest_tests {
    use super::manifest::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_manifest(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    #[test]
    fn test_parse_valid_manifest() {
        let f = write_manifest(r#"
            [plugin]
            id = "com.example.git-status"
            name = "Git Status"
            version = "0.1.0"
            api = "shux:plugin@1.0.0"
            kind = "wasm"
            description = "Git status bar segments"

            [permissions]
            events = ["pane.focused", "pane.cwd_changed"]
            read_pane_output = false

            [extensions]
            status_segments = ["git_branch"]
            commands = ["git-status.refresh"]
        "#);

        let manifest = PluginManifest::load(f.path()).unwrap();
        assert_eq!(manifest.plugin.id, "com.example.git-status");
        assert_eq!(manifest.plugin.kind, PluginKind::Wasm);
        assert_eq!(manifest.permissions.events.len(), 2);
        assert!(!manifest.permissions.run_subprocess);
        assert_eq!(manifest.extensions.status_segments, vec!["git_branch"]);
    }

    #[test]
    fn test_reject_incompatible_api_version() {
        let f = write_manifest(r#"
            [plugin]
            id = "com.example.test"
            name = "Test"
            version = "0.1.0"
            api = "shux:plugin@2.0.0"
            kind = "wasm"
        "#);

        let result = PluginManifest::load(f.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("incompatible"));
    }

    #[test]
    fn test_reject_invalid_plugin_id() {
        let f = write_manifest(r#"
            [plugin]
            id = "no-dots"
            name = "Test"
            version = "0.1.0"
            api = "shux:plugin@1.0.0"
            kind = "wasm"
        "#);

        let result = PluginManifest::load(f.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_has_permission() {
        let f = write_manifest(r#"
            [plugin]
            id = "com.example.test"
            name = "Test"
            version = "0.1.0"
            api = "shux:plugin@1.0.0"
            kind = "wasm"

            [permissions]
            send_keys = true
            manage_panes = false
        "#);

        let manifest = PluginManifest::load(f.path()).unwrap();
        assert!(manifest.has_permission("send_keys"));
        assert!(!manifest.has_permission("manage_panes"));
        assert!(!manifest.has_permission("run_subprocess"));
    }

    #[test]
    fn test_default_permissions_are_deny() {
        let f = write_manifest(r#"
            [plugin]
            id = "com.example.test"
            name = "Test"
            version = "0.1.0"
            api = "shux:plugin@1.0.0"
            kind = "wasm"
        "#);

        let manifest = PluginManifest::load(f.path()).unwrap();
        assert!(!manifest.has_permission("send_keys"));
        assert!(!manifest.has_permission("run_subprocess"));
        assert!(!manifest.has_permission("network"));
        assert!(!manifest.has_permission("clipboard"));
    }
}

#[cfg(test)]
mod engine_tests {
    use super::engine::create_engine;

    #[test]
    fn test_engine_creation() {
        let engine = create_engine();
        assert!(engine.is_ok());
    }
}
```

---

## Verification

### Functional

```bash
# Build the plugin crate
cargo build -p shux-plugin

# Verify the WIT file is syntactically valid (if wasm-tools is installed)
# wasm-tools component wit wit/shux-plugin.wit

# Verify clippy
cargo clippy -p shux-plugin -- -D warnings
```

### Tests

```bash
# Run all plugin tests
cargo nextest run -p shux-plugin

# Run manifest tests specifically
cargo nextest run -p shux-plugin -- manifest

# Full workspace
cargo nextest run --workspace
```

---

## Completion Criteria

- [ ] `wit/shux-plugin.wit` contains the complete WIT interface from PRD section 7.5
- [ ] `Engine` created with Component Model, epoch interruption, async support, Cranelift
- [ ] Epoch ticker background task increments engine epoch every 10ms
- [ ] `PluginManifest` parses `plugin.toml` with all fields: plugin metadata, permissions, extensions
- [ ] Manifest validation: API version compatibility (major version must match), plugin ID format
- [ ] `PluginPermissions` covers all permissions: events, read_pane_output, send_keys, manage_panes, manage_sessions, api_extensions, run_subprocess, fs_read, fs_write, network, clipboard, intercept_events, override_commands
- [ ] `WasmPlugin::load()` compiles `.wasm` and caches as `.cwasm`
- [ ] Pre-compiled `.cwasm` loading for fast startup (target: p50 <= 50us)
- [ ] Per-plugin `Store` with `PluginState` (manifest, id, subsystem references)
- [ ] Epoch deadline configured on Store for CPU timeout enforcement
- [ ] `PluginHost` created at daemon startup with shared Engine
- [ ] `PluginId`, `PluginLifecycleState`, `PluginInfo`, `PluginStats` types defined
- [ ] All unit tests pass (manifest parsing, validation, engine creation)
- [ ] `cargo clippy -p shux-plugin -- -D warnings` passes

---

## Commit Message
```
feat(plugin): add wasmtime integration with WIT bindings and manifest parser

- WIT interface definition (shux:plugin@1.0.0) with host and plugin interfaces
- wasmtime Engine with Component Model, epoch interruption, async support
- Epoch ticker for CPU timeout enforcement (100ms kill threshold)
- plugin.toml manifest parser with permission and extension declarations
- WasmPlugin loader with pre-compilation caching (.cwasm)
- Per-plugin Store with epoch deadline and plugin state
- PluginHost top-level API with shared Engine
```

---

## Session Protocol

1. **Before starting:** Read `CLAUDE.md`. Read PRD sections 7 (plugin system), 7.1 (design goals), 7.4 (plugin.toml), 7.5 (WIT), and 15.2 (wasmtime 41+). Read the wasmtime Component Model documentation. Verify task 034 is complete.
2. **During:** Implement in order: WIT file (Step 1) -> Engine (Step 2) -> manifest parser (Step 3) -> Wasm loader (Step 4) -> types (Step 5) -> PluginHost (Step 6) -> Cargo.toml (Step 7) -> tests (Step 8). Run `cargo check -p shux-plugin` after each step. The WIT file and manifest parser are independent and can be tested separately.
3. **Testing:** The manifest parser should be tested extensively with valid and invalid inputs. The engine creation test verifies wasmtime links correctly. Wasm loading tests require a sample `.wasm` file -- create a minimal "hello world" component for testing (or use wasmtime's built-in test components).
4. **After:** Run `make check`. Update `docs/PROGRESS.md`. Update `CLAUDE.md` Learnings with any discoveries about wasmtime 41 API changes (especially Component Model API), WIT syntax edge cases, or `toml` crate parsing behavior.
5. **Watch out for:**
   - wasmtime 41 may have API differences from v40. Check `Component::new` vs `Component::from_binary`, `Store::new` signatures, and `Config` method names.
   - The WIT file must be syntactically valid. Test with `wasm-tools component wit` if available.
   - `unsafe { Component::deserialize() }` is required for loading pre-compiled `.cwasm` files. The safety justification is that we only load files we compiled ourselves.
   - The `wasm32-wasip2` target must be installed for compiling Wasm plugins (set up in task 000 via `rust-toolchain.toml`).
   - Epoch interruption has ~10% overhead. This is acceptable per PRD section 14.1.
