# 022 — TOML Config System

**Status:** Done (2026-05-08 spike + PR #4). `crates/shux-core/src/config.rs` provides `Config` / `ConfigHandle` with lock-free `ArcSwap` snapshots. Loaded from `$XDG_CONFIG_HOME/shux/config.toml` or `$HOME/.config/shux/config.toml`. Sections: `[appearance]`, `[keys]`, `[shell]`, `[statusbar]`, `[theme]`. PR #4 added `shux config validate` with line:col diagnostics. `shux config init` / `show` / `path` ship the starter starship-integrated config.
**Depends On:** 012
**Parallelizable With:** 013, 014, 018, 019

---

## Problem

shux needs a configuration system that balances "it just works" (beautiful defaults, zero config needed) with deep customizability for power users. The PRD specifies a layered TOML configuration with five precedence levels, per-key deep merge semantics, schema validation with actionable errors, and API methods for querying and modifying configuration at runtime. Without this task, no configuration is possible -- keybindings, themes, plugins, daemon behavior, and shell settings all depend on it.

This task implements the config infrastructure only. Live reload (file watching, debounce, atomic detection) is task 023. Theme loading is task 024. Keybinding customization is task 031.

## PRD Reference

- **SS 10** (Configuration -- TOML, layered, validated)
- **SS 10.1** (Config discovery -- layered precedence, 5 levels)
- **SS 10.2** (Full config reference -- all sections and keys)
- **SS 8.2** (config.get, config.set, config.validate, config.explain API methods)
- **SS 1** (P1: It just works -- zero config for 90% of use cases)

---

## Files to Create

- `crates/shux-core/src/config.rs` -- Config types, defaults, parsing, layered merge, validation
- `crates/shux-core/src/config_schema.rs` -- Schema definition and explain output
- `crates/shux-core/src/config_merge.rs` -- Per-key deep merge algorithm
- `crates/shux-core/src/config_discovery.rs` -- Config file discovery across all layers
- `crates/shux-rpc/src/methods/config.rs` -- config.get, config.set, config.validate, config.explain handlers
- `crates/shux-core/tests/config_test.rs` -- Comprehensive config tests

## Files to Modify

- `crates/shux-core/src/lib.rs` -- Register config modules, export `ShuxConfig`
- `crates/shux-core/Cargo.toml` -- Add `toml`, `dirs`, `serde` dependencies
- `crates/shux-rpc/src/lib.rs` -- Register config method handlers
- `crates/shux/src/main.rs` -- Load config during daemon startup

---

## Execution Steps

### Step 1: Define the complete config struct

The config struct mirrors PRD SS 10.2 exactly. Every field has a documented default. The struct derives `serde::Deserialize` and `serde::Serialize` for TOML parsing and API output.

```rust
// crates/shux-core/src/config.rs

use std::collections::HashMap;
use std::path::PathBuf;
use serde::{Deserialize, Serialize};

/// Complete shux configuration.
/// All fields have defaults (via `Default` impl).
/// Serde: unknown keys produce warnings (not errors) for forward compatibility.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ShuxConfig {
    pub daemon: DaemonConfig,
    pub ui: UiConfig,
    pub theme: ThemeConfig,
    pub copy: CopyConfig,
    pub plugins: PluginsConfig,
    pub shell: ShellConfig,
    pub keybindings: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DaemonConfig {
    /// Unix domain socket path. Supports $XDG_RUNTIME_DIR expansion.
    pub socket_path: String,
    /// TCP listen address (empty = disabled).
    pub tcp_listen: String,
    /// Path to the auth token file.
    pub auth_token_path: String,
    /// Auto-start daemon on first CLI use.
    pub auto_start: bool,
    /// Auto-exit daemon when last session is destroyed.
    pub auto_exit: bool,
    /// Grace period before auto-exit (seconds).
    pub auto_exit_grace_secs: u64,
    /// Log level: trace, debug, info, warn, error.
    pub log_level: String,
    /// Log format: "pretty" or "json".
    pub log_format: String,
    /// Log file path (empty = stderr only).
    pub log_file: String,
    /// Enable gRPC API.
    pub grpc_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct UiConfig {
    /// Prefix key for Tier 2 bindings (crossterm notation).
    pub prefix: String,
    /// Enable mouse support.
    pub mouse: bool,
    /// Show status bar.
    pub status_bar: bool,
    /// Status bar position: "top" or "bottom".
    pub status_bar_position: String,
    /// Scrollback buffer size (lines per pane).
    pub scrollback_lines: usize,
    /// Pane border style: "thin", "thick", "double", "rounded", "none".
    pub pane_border_style: String,
    /// Show pane titles in borders.
    pub show_pane_titles: bool,
    /// Auto-derive pane titles from running command.
    pub auto_title: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ThemeConfig {
    /// Active theme name.
    pub name: String,
    /// Additional theme search paths.
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CopyConfig {
    /// OSC 52 clipboard policy: "auto", "allow", "deny".
    pub osc52: String,
    /// Whether mouse selection automatically copies.
    pub mouse_select_copies: bool,
    /// Use vi-style keys in copy mode.
    pub vi_keys: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PluginsConfig {
    /// Plugin search paths.
    pub paths: Vec<String>,
    /// Allow process plugins (disabled by default for security).
    pub allow_process_plugins: bool,
    /// Idle timeout for process plugin GC (seconds).
    pub process_gc_timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ShellConfig {
    /// Default shell command (empty = $SHELL).
    pub default_command: String,
    /// Use login shell.
    pub login_shell: bool,
}
```

### Step 2: Implement defaults matching PRD SS 10.2

```rust
impl Default for ShuxConfig {
    fn default() -> Self {
        Self {
            daemon: DaemonConfig::default(),
            ui: UiConfig::default(),
            theme: ThemeConfig::default(),
            copy: CopyConfig::default(),
            plugins: PluginsConfig::default(),
            shell: ShellConfig::default(),
            keybindings: HashMap::new(),
        }
    }
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: "$XDG_RUNTIME_DIR/shux/shux.sock".to_string(),
            tcp_listen: String::new(),
            auth_token_path: "~/.config/shux/token".to_string(),
            auto_start: true,
            auto_exit: true,
            auto_exit_grace_secs: 5,
            log_level: "info".to_string(),
            log_format: "pretty".to_string(),
            log_file: String::new(),
            grpc_enabled: false,
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            prefix: "ctrl+space".to_string(),
            mouse: true,
            status_bar: true,
            status_bar_position: "bottom".to_string(),
            scrollback_lines: 5000,
            pane_border_style: "rounded".to_string(),
            show_pane_titles: true,
            auto_title: true,
        }
    }
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            name: "default-dark".to_string(),
            paths: vec!["~/.config/shux/themes".to_string()],
        }
    }
}

impl Default for CopyConfig {
    fn default() -> Self {
        Self {
            osc52: "auto".to_string(),
            mouse_select_copies: false,
            vi_keys: true,
        }
    }
}

impl Default for PluginsConfig {
    fn default() -> Self {
        Self {
            paths: vec!["~/.config/shux/plugins".to_string()],
            allow_process_plugins: false,
            process_gc_timeout_secs: 30,
        }
    }
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            default_command: String::new(),
            login_shell: true,
        }
    }
}
```

### Step 3: Implement config file discovery

Discover config files across all five layers. Walk up from CWD for project config, stopping at VCS root.

```rust
// crates/shux-core/src/config_discovery.rs

use std::path::{Path, PathBuf};

/// All possible config file locations, in precedence order (lowest first).
#[derive(Debug, Clone)]
pub struct ConfigSources {
    /// Layer 1: built-in defaults (no file).
    pub builtin: bool,
    /// Layer 2: system config.
    pub system: Option<PathBuf>,
    /// Layer 3: user config.
    pub user: Option<PathBuf>,
    /// Layer 4: project config (found by walking up).
    pub project: Option<PathBuf>,
    // Layer 5 (runtime overrides) is in-memory only.
}

impl ConfigSources {
    /// Return all file-backed config layers in load order.
    /// Used by the live-reload watcher (task 023).
    pub fn all_file_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let Some(path) = &self.system {
            paths.push(path.clone());
        }
        if let Some(path) = &self.user {
            paths.push(path.clone());
        }
        if let Some(path) = &self.project {
            paths.push(path.clone());
        }
        paths
    }
}

pub fn discover_config_sources(cwd: &Path) -> ConfigSources {
    ConfigSources {
        builtin: true,
        system: discover_system_config(),
        user: discover_user_config(),
        project: discover_project_config(cwd),
    }
}

fn discover_system_config() -> Option<PathBuf> {
    let path = PathBuf::from("/etc/shux/config.toml");
    if path.exists() { Some(path) } else { None }
}

fn discover_user_config() -> Option<PathBuf> {
    let xdg_config = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("~"))
                .join(".config")
        });

    let path = xdg_config.join("shux").join("config.toml");
    if path.exists() { Some(path) } else { None }
}

fn discover_project_config(cwd: &Path) -> Option<PathBuf> {
    let mut dir = cwd.to_path_buf();
    loop {
        let candidate = dir.join(".shux").join("config.toml");
        if candidate.exists() {
            return Some(candidate);
        }

        // Stop at VCS root.
        if dir.join(".git").exists() || dir.join(".hg").exists() {
            return None;
        }

        // Stop at filesystem root.
        if !dir.pop() {
            return None;
        }
    }
}

/// Expand shell-style paths: ~ and $XDG_RUNTIME_DIR.
pub fn expand_path(path: &str) -> PathBuf {
    let expanded = if path.starts_with('~') {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
        home.join(&path[2..]) // Skip "~/"
    } else if path.contains("$XDG_RUNTIME_DIR") {
        let runtime = std::env::var("XDG_RUNTIME_DIR")
            .unwrap_or_else(|_| format!("/run/user/{}", nix::unistd::getuid()));
        PathBuf::from(path.replace("$XDG_RUNTIME_DIR", &runtime))
    } else {
        PathBuf::from(path)
    };
    expanded
}
```

### Step 4: Implement per-key deep merge

The merge algorithm follows PRD SS 10.2: scalars and arrays are replaced entirely; tables are merged recursively.

```rust
// crates/shux-core/src/config_merge.rs

use toml::Value;

/// Deep-merge `overlay` into `base`. Modifies `base` in place.
///
/// Rules (PRD §10.2):
/// - Scalar values: overlay replaces base.
/// - Arrays: overlay replaces base entirely (not appended).
/// - Tables: merged recursively (per-key).
/// - To "delete" a key from a parent layer, set it to false or empty.
pub fn deep_merge(base: &mut Value, overlay: &Value) {
    match (base, overlay) {
        (Value::Table(base_table), Value::Table(overlay_table)) => {
            for (key, overlay_val) in overlay_table {
                if let Some(base_val) = base_table.get_mut(key) {
                    deep_merge(base_val, overlay_val);
                } else {
                    base_table.insert(key.clone(), overlay_val.clone());
                }
            }
        }
        (base, overlay) => {
            // Scalar or array: replace entirely.
            *base = overlay.clone();
        }
    }
}

/// Merge multiple TOML values in precedence order (first = lowest priority).
pub fn merge_layers(layers: &[Value]) -> Value {
    let mut result = Value::Table(toml::map::Map::new());
    for layer in layers {
        deep_merge(&mut result, layer);
    }
    result
}
```

### Step 5: Implement config loading pipeline

Load config from all discovered sources, merge them, and deserialize into `ShuxConfig`.

```rust
// In crates/shux-core/src/config.rs

use crate::config_discovery::{discover_config_sources, ConfigSources};
use crate::config_merge::{merge_layers, deep_merge};

/// Errors that can occur during config loading.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config file {path}: {source}")]
    ReadError { path: PathBuf, source: std::io::Error },

    #[error("TOML parse error in {path}: {message}")]
    ParseError { path: PathBuf, message: String },

    #[error("config validation error: {0}")]
    ValidationError(String),

    #[error("config validation errors:\n{}", .0.join("\n"))]
    ValidationErrors(Vec<String>),
}

/// Loaded config with metadata about sources.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    /// The merged config.
    pub config: ShuxConfig,
    /// Which sources were loaded.
    pub sources: ConfigSources,
    /// Runtime overrides (applied on top of file-based config).
    pub runtime_overrides: toml::Value,
    /// Any warnings generated during loading.
    pub warnings: Vec<String>,
}

impl LoadedConfig {
    /// Load config from all sources. Never fails -- returns defaults on error
    /// with warnings.
    pub fn load(cwd: &Path) -> Self {
        let sources = discover_config_sources(cwd);
        let mut warnings = Vec::new();
        let mut layers = Vec::new();

        // Layer 1: built-in defaults (serialize the Default to TOML Value).
        let defaults = toml::Value::try_from(&ShuxConfig::default())
            .expect("default config must be serializable");
        layers.push(defaults);

        // Layer 2: system config.
        if let Some(ref path) = sources.system {
            match load_toml_file(path) {
                Ok(val) => layers.push(val),
                Err(e) => warnings.push(format!("system config: {e}")),
            }
        }

        // Layer 3: user config.
        if let Some(ref path) = sources.user {
            match load_toml_file(path) {
                Ok(val) => layers.push(val),
                Err(e) => warnings.push(format!("user config: {e}")),
            }
        }

        // Layer 4: project config.
        if let Some(ref path) = sources.project {
            match load_toml_file(path) {
                Ok(val) => layers.push(val),
                Err(e) => warnings.push(format!("project config: {e}")),
            }
        }

        // Merge all layers.
        let merged = merge_layers(&layers);

        // Deserialize into ShuxConfig.
        let config: ShuxConfig = match merged.try_into() {
            Ok(c) => c,
            Err(e) => {
                warnings.push(format!("config deserialization failed, using defaults: {e}"));
                ShuxConfig::default()
            }
        };

        // Validate the merged config.
        if let Err(errors) = validate_config(&config) {
            for e in &errors {
                warnings.push(format!("config validation: {e}"));
            }
        }

        Self {
            config,
            sources,
            runtime_overrides: toml::Value::Table(toml::map::Map::new()),
            warnings,
        }
    }

    /// Apply a runtime override (Layer 5).
    pub fn set_runtime(&mut self, key: &str, value: toml::Value) -> Result<(), ConfigError> {
        set_nested_value(&mut self.runtime_overrides, key, value);
        self.recompute()?;
        Ok(())
    }

    /// Get a config value by dotted key path (e.g., "ui.prefix").
    pub fn get(&self, key: &str) -> Option<toml::Value> {
        let config_value = toml::Value::try_from(&self.config).ok()?;
        get_nested_value(&config_value, key)
    }

    /// Re-merge all layers and recompute the config.
    fn recompute(&mut self) -> Result<(), ConfigError> {
        // Re-run the same pipeline as `load` atomically:
        // 1) rediscover/read sources, 2) merge defaults + files,
        // 3) merge runtime overrides, 4) deserialize + validate,
        // 5) swap `self.config`, `self.sources`, and `self.warnings`.
        // Called by both `set_runtime` and file-watch reload paths.
        Ok(())
    }
}

fn load_toml_file(path: &Path) -> Result<toml::Value, ConfigError> {
    let content = std::fs::read_to_string(path).map_err(|e| ConfigError::ReadError {
        path: path.to_path_buf(),
        source: e,
    })?;
    content
        .parse::<toml::Value>()
        .map_err(|e| ConfigError::ParseError {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
}

/// Navigate a dotted key path (e.g., "ui.prefix") in a TOML Value tree.
fn get_nested_value(value: &toml::Value, key: &str) -> Option<toml::Value> {
    let parts: Vec<&str> = key.split('.').collect();
    let mut current = value;
    for part in &parts {
        current = current.get(part)?;
    }
    Some(current.clone())
}

/// Set a value at a dotted key path, creating intermediate tables as needed.
fn set_nested_value(root: &mut toml::Value, key: &str, value: toml::Value) {
    let parts: Vec<&str> = key.split('.').collect();
    let mut current = root;
    for (i, part) in parts.iter().enumerate() {
        if i == parts.len() - 1 {
            // Last part: set the value.
            if let toml::Value::Table(table) = current {
                table.insert(part.to_string(), value);
                return;
            }
        } else {
            // Intermediate part: ensure table exists.
            if let toml::Value::Table(table) = current {
                current = table
                    .entry(part.to_string())
                    .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
            }
        }
    }
}
```

### Step 6: Implement schema validation

Validate the config with actionable errors including field location and suggestions.

```rust
// crates/shux-core/src/config_schema.rs

use crate::config::*;

/// Validate a ShuxConfig and return all errors found.
pub fn validate_config(config: &ShuxConfig) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();

    // Validate daemon config.
    if !["trace", "debug", "info", "warn", "error"]
        .contains(&config.daemon.log_level.as_str())
    {
        errors.push(format!(
            "[daemon] log_level = {:?}: must be one of: trace, debug, info, warn, error",
            config.daemon.log_level
        ));
    }

    if !["pretty", "json"].contains(&config.daemon.log_format.as_str()) {
        errors.push(format!(
            "[daemon] log_format = {:?}: must be \"pretty\" or \"json\"",
            config.daemon.log_format
        ));
    }

    // Validate UI config.
    if config.ui.scrollback_lines == 0 {
        errors.push("[ui] scrollback_lines = 0: must be at least 1".to_string());
    }
    if config.ui.scrollback_lines > 1_000_000 {
        errors.push(format!(
            "[ui] scrollback_lines = {}: maximum is 1,000,000 (memory budget)",
            config.ui.scrollback_lines
        ));
    }

    if !["thin", "thick", "double", "rounded", "none"]
        .contains(&config.ui.pane_border_style.as_str())
    {
        errors.push(format!(
            "[ui] pane_border_style = {:?}: must be one of: thin, thick, double, rounded, none",
            config.ui.pane_border_style
        ));
    }

    if !["top", "bottom"].contains(&config.ui.status_bar_position.as_str()) {
        errors.push(format!(
            "[ui] status_bar_position = {:?}: must be \"top\" or \"bottom\"",
            config.ui.status_bar_position
        ));
    }

    // Validate prefix key is parseable.
    if crate::prefix::parse_prefix_key(&config.ui.prefix).is_err() {
        errors.push(format!(
            "[ui] prefix = {:?}: not a valid key notation (examples: \"ctrl+space\", \"ctrl+b\")",
            config.ui.prefix
        ));
    }

    // Validate copy config.
    if !["auto", "allow", "deny"].contains(&config.copy.osc52.as_str()) {
        errors.push(format!(
            "[copy] osc52 = {:?}: must be \"auto\", \"allow\", or \"deny\"",
            config.copy.osc52
        ));
    }

    // Validate theme name is non-empty.
    if config.theme.name.is_empty() {
        errors.push("[theme] name: must not be empty".to_string());
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Generate a human-readable explanation of the full config schema with
/// defaults and descriptions. Used by `shux config explain`.
pub fn explain_schema() -> String {
    let mut out = String::new();
    out.push_str("# shux configuration schema\n");
    out.push_str("# All values shown are defaults.\n\n");

    out.push_str("[daemon]\n");
    explain_field(&mut out, "socket_path", "\"$XDG_RUNTIME_DIR/shux/shux.sock\"",
        "Unix domain socket path for API server");
    explain_field(&mut out, "tcp_listen", "\"\"",
        "TCP listen address (empty = disabled). Example: \"127.0.0.1:9876\"");
    explain_field(&mut out, "auth_token_path", "\"~/.config/shux/token\"",
        "Path to auth token file (required for TCP connections)");
    explain_field(&mut out, "auto_start", "true",
        "Auto-start daemon on first CLI use");
    explain_field(&mut out, "auto_exit", "true",
        "Auto-exit daemon when last session is destroyed");
    explain_field(&mut out, "auto_exit_grace_secs", "5",
        "Seconds to wait before auto-exit (prevents flapping)");
    explain_field(&mut out, "log_level", "\"info\"",
        "Log level: trace, debug, info, warn, error");
    explain_field(&mut out, "log_format", "\"pretty\"",
        "Log format: \"pretty\" (human-readable) or \"json\"");
    explain_field(&mut out, "log_file", "\"\"",
        "Log file path (empty = stderr only)");
    explain_field(&mut out, "grpc_enabled", "false",
        "Enable gRPC API (optional transport for typed streaming)");

    out.push_str("\n[ui]\n");
    explain_field(&mut out, "prefix", "\"ctrl+space\"",
        "Prefix key for Tier 2 bindings. Notation: ctrl+space, ctrl+b, etc.");
    explain_field(&mut out, "mouse", "true",
        "Enable mouse support (click-to-focus, drag-to-resize, scroll)");
    explain_field(&mut out, "status_bar", "true",
        "Show the status bar");
    explain_field(&mut out, "status_bar_position", "\"bottom\"",
        "Status bar position: \"top\" or \"bottom\"");
    explain_field(&mut out, "scrollback_lines", "5000",
        "Scrollback buffer size per pane (lines). Max: 1,000,000");
    explain_field(&mut out, "pane_border_style", "\"rounded\"",
        "Border style: thin, thick, double, rounded, none");
    explain_field(&mut out, "show_pane_titles", "true",
        "Show pane titles in borders");
    explain_field(&mut out, "auto_title", "true",
        "Auto-derive pane titles from running command");

    out.push_str("\n[theme]\n");
    explain_field(&mut out, "name", "\"default-dark\"",
        "Active theme name");
    explain_field(&mut out, "paths", "[\"~/.config/shux/themes\"]",
        "Additional theme search paths");

    out.push_str("\n[copy]\n");
    explain_field(&mut out, "osc52", "\"auto\"",
        "OSC 52 clipboard: \"auto\" (detect), \"allow\", \"deny\"");
    explain_field(&mut out, "mouse_select_copies", "false",
        "Mouse selection automatically copies to clipboard");
    explain_field(&mut out, "vi_keys", "true",
        "Use vi-style keys in copy mode");

    out.push_str("\n[plugins]\n");
    explain_field(&mut out, "paths", "[\"~/.config/shux/plugins\"]",
        "Plugin search paths");
    explain_field(&mut out, "allow_process_plugins", "false",
        "Allow process plugins (security: disabled by default)");
    explain_field(&mut out, "process_gc_timeout_secs", "30",
        "Idle timeout before killing unused process plugins (seconds)");

    out.push_str("\n[shell]\n");
    explain_field(&mut out, "default_command", "\"\"",
        "Default shell command (empty = $SHELL)");
    explain_field(&mut out, "login_shell", "true",
        "Use login shell (-l flag)");

    out.push_str("\n[keybindings]\n");
    out.push_str("# Override keybindings. Keys use crossterm notation.\n");
    out.push_str("# Examples:\n");
    out.push_str("# \"alt-h\" = \"focus-left\"\n");
    out.push_str("# \"ctrl-space c\" = \"window.create\"\n");

    out
}

fn explain_field(out: &mut String, name: &str, default: &str, description: &str) {
    out.push_str(&format!("# {}\n", description));
    out.push_str(&format!("{} = {}\n", name, default));
}
```

### Step 7: Implement config.* RPC methods

```rust
// crates/shux-rpc/src/methods/config.rs

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

/// config.get -- retrieve a config value by dotted key path.
#[derive(Deserialize)]
pub struct ConfigGetParams {
    pub key: String,
}

#[derive(Serialize)]
pub struct ConfigGetResult {
    pub key: String,
    pub value: JsonValue,
    pub source: String, // "builtin", "system", "user", "project", "runtime"
}

/// config.set -- set a runtime override.
#[derive(Deserialize)]
pub struct ConfigSetParams {
    pub key: String,
    pub value: JsonValue,
}

/// config.validate -- validate current config (or a provided TOML string).
#[derive(Deserialize)]
pub struct ConfigValidateParams {
    /// If provided, validate this TOML content. Otherwise validate active config.
    pub content: Option<String>,
}

#[derive(Serialize)]
pub struct ConfigValidateResult {
    pub valid: bool,
    pub errors: Vec<ConfigValidationError>,
    pub warnings: Vec<String>,
}

#[derive(Serialize)]
pub struct ConfigValidationError {
    pub key: String,
    pub message: String,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub suggestion: Option<String>,
}

/// config.explain -- return full schema with defaults and descriptions.
#[derive(Serialize)]
pub struct ConfigExplainResult {
    pub schema: String,
}
```

### Step 8: Implement CLI subcommands

Wire `shux config validate` and `shux config explain` as CLI subcommands that call the corresponding API methods.

```rust
// In crates/shux/src/main.rs (or a dedicated CLI module)

#[derive(clap::Subcommand)]
pub enum ConfigCommand {
    /// Validate the current configuration.
    Validate {
        /// Path to a config file to validate (default: active config).
        #[arg(short, long)]
        file: Option<PathBuf>,
    },
    /// Show full config schema with defaults and descriptions.
    Explain,
    /// Get a config value.
    Get {
        /// Dotted key path (e.g., "ui.prefix").
        key: String,
    },
    /// Set a runtime config override.
    Set {
        /// Dotted key path.
        key: String,
        /// Value to set.
        value: String,
    },
}
```

### Step 9: Diff computation for reload

Implement a config diff utility that compares old and new configs to produce a list of changes. This is used by live config reload (task 023) to emit events.

```rust
/// A single config change.
#[derive(Debug, Clone, Serialize)]
pub struct ConfigChange {
    pub key: String,
    pub old_value: Option<toml::Value>,
    pub new_value: Option<toml::Value>,
}

/// Compute the diff between two configs.
pub fn diff_configs(old: &ShuxConfig, new: &ShuxConfig) -> Vec<ConfigChange> {
    let old_val = toml::Value::try_from(old).unwrap_or(toml::Value::Table(Default::default()));
    let new_val = toml::Value::try_from(new).unwrap_or(toml::Value::Table(Default::default()));
    let mut changes = Vec::new();
    diff_values("", &old_val, &new_val, &mut changes);
    changes
}

fn diff_values(
    prefix: &str,
    old: &toml::Value,
    new: &toml::Value,
    changes: &mut Vec<ConfigChange>,
) {
    match (old, new) {
        (toml::Value::Table(old_t), toml::Value::Table(new_t)) => {
            // Check all keys in old.
            for (key, old_val) in old_t {
                let full_key = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{}.{}", prefix, key)
                };
                if let Some(new_val) = new_t.get(key) {
                    diff_values(&full_key, old_val, new_val, changes);
                } else {
                    changes.push(ConfigChange {
                        key: full_key,
                        old_value: Some(old_val.clone()),
                        new_value: None,
                    });
                }
            }
            // Check for new keys.
            for (key, new_val) in new_t {
                if !old_t.contains_key(key) {
                    let full_key = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{}.{}", prefix, key)
                    };
                    changes.push(ConfigChange {
                        key: full_key,
                        old_value: None,
                        new_value: Some(new_val.clone()),
                    });
                }
            }
        }
        _ => {
            if old != new {
                changes.push(ConfigChange {
                    key: prefix.to_string(),
                    old_value: Some(old.clone()),
                    new_value: Some(new.clone()),
                });
            }
        }
    }
}
```

### Step 10: Wire config loading into daemon startup

During daemon startup, load the config before initializing any subsystems.

```rust
// In daemon initialization:

let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
let loaded_config = LoadedConfig::load(&cwd);

// Log any warnings.
for warning in &loaded_config.warnings {
    tracing::warn!("config: {}", warning);
}

// Use the config to initialize subsystems.
let config = loaded_config.config.clone();
```

---

## Verification

### Functional

```bash
# Build
cargo build --workspace

# Verify defaults (no config file needed)
cargo run -p shux -- config explain
# Should print full schema with all defaults

# Create a test config
mkdir -p /tmp/shux-test/.config/shux
cat > /tmp/shux-test/.config/shux/config.toml << 'EOF'
[ui]
prefix = "ctrl+b"
mouse = false
scrollback_lines = 10000
EOF

# Validate the config
cargo run -p shux -- config validate --file /tmp/shux-test/.config/shux/config.toml
# Should report: valid

# Test invalid config
cat > /tmp/shux-test-bad.toml << 'EOF'
[ui]
scrollback_lines = 0
pane_border_style = "wavy"
EOF

cargo run -p shux -- config validate --file /tmp/shux-test-bad.toml
# Should report errors with suggestions

# Test config.get
cargo run -p shux -- config get ui.prefix
# Should print "ctrl+space" (default)

# Test layered merge
# Create project config
mkdir -p /tmp/shux-project/.shux
cat > /tmp/shux-project/.shux/config.toml << 'EOF'
[ui]
scrollback_lines = 20000
EOF
# When running from /tmp/shux-project, scrollback_lines should be 20000
```

### Tests

```bash
# Unit tests for config
cargo nextest run -p shux-core --lib config
cargo nextest run -p shux-core --lib config_merge
cargo nextest run -p shux-core --lib config_discovery
cargo nextest run -p shux-core --lib config_schema

# Integration tests
cargo nextest run -p shux-core --test config_test

# Test scenarios:
# - Default config matches PRD §10.2 exactly
# - Deep merge: scalars replaced, tables merged recursively
# - Deep merge: arrays replaced entirely (not appended)
# - Config discovery: system → user → project layering
# - Project config walk-up stops at .git root
# - XDG_CONFIG_HOME is respected
# - Path expansion: ~ and $XDG_RUNTIME_DIR
# - Validation catches all invalid values with actionable messages
# - Runtime override via config.set works
# - config.get returns correct value from correct layer
# - Diff correctly identifies changed, added, and removed keys
# - Invalid TOML file doesn't crash -- defaults used with warnings
# - Unknown keys produce warnings (not errors) for forward compat
```

---

## Completion Criteria

- [ ] `ShuxConfig` struct covers all sections from PRD SS 10.2
- [ ] All defaults match PRD SS 10.2 exactly
- [ ] Five-layer discovery: built-in, system, user, project, runtime
- [ ] Project config walk-up stops at `.git` or `.hg` root
- [ ] XDG_CONFIG_HOME respected for user config
- [ ] Per-key deep merge: scalars/arrays replaced, tables merged recursively
- [ ] Schema validation with actionable error messages (field, value, suggestion)
- [ ] `shux config validate` CLI command works
- [ ] `shux config explain` prints full schema with defaults and descriptions
- [ ] `config.get` API returns value by dotted key path
- [ ] `config.set` API applies runtime override
- [ ] `config.validate` API validates active or provided config
- [ ] `config.explain` API returns schema text
- [ ] Config diff utility computes changed keys between two configs
- [ ] Invalid config files are gracefully handled (defaults used, warnings logged)
- [ ] Unknown keys produce warnings (not errors)
- [ ] Path expansion works for `~` and `$XDG_RUNTIME_DIR`
- [ ] Comprehensive unit tests for all config operations
- [ ] Config loads successfully during daemon startup

---

## Commit Message

```
feat(core): implement layered TOML config system with validation

- Five-layer config discovery: built-in, system, user, project, runtime (PRD §10.1)
- Full ShuxConfig struct matching PRD §10.2 reference
- Per-key deep merge with table recursion, scalar/array replacement
- Schema validation with actionable errors (field location + suggestions)
- config.get, config.set, config.validate, config.explain API methods
- CLI: shux config validate, shux config explain
- Config diff utility for live reload (task 023)
- Project config walk-up stops at VCS root (.git/.hg)
```

---

## Session Protocol

1. **Before starting:** Read PRD SS 10 thoroughly. Review the `toml` crate's `Value` API and `serde` derive capabilities. Read task 012 (M0 integration gate) to understand what exists after M0.
2. **During:** Implement in order: struct + defaults (Steps 1-2), discovery (Step 3), merge (Step 4), loading pipeline (Step 5), validation (Step 6), RPC methods (Step 7), CLI (Step 8), diff (Step 9), integration (Step 10). Run `cargo check` after each step. Write tests alongside each step.
3. **After:** Run full verification suite. Verify all defaults match PRD. Update `docs/PROGRESS.md` (mark 022 done). Update `CLAUDE.md` Learnings with TOML crate nuances.
