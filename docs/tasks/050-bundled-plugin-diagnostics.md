# 050 — Bundled Plugin: shux-diagnostics

**Status:** Pending
**Depends On:** 046, 045
**Parallelizable With:** 049

---

## Problem

shux needs production-grade observability built in, not bolted on. The PRD specifies a `shux doctor` command for troubleshooting and a TUI diagnostics overlay for real-time monitoring. Both must be implemented as a plugin to prove the overlay system (task 046) and API extension system (task 045) work together. The diagnostics plugin collects config, terminal capabilities, plugin status, recent errors, PTY health, renderer stats, event bus lag, and plugin latencies — everything needed to diagnose problems without staring at log files. The doctor bundle generates a shareable JSON file with redaction for sensitive paths and environment values.

## PRD Reference

- **SS 7.7** Bundled plugins: "shux-diagnostics — `shux doctor` + TUI diagnostics overlay. Extension points used: Commands, pane overlays"
- **SS 6.1** Observability: "Diagnostics overlay: TUI overlay showing health, PTY status, renderer stats, events, plugins, capabilities" and "Doctor bundle: `shux doctor` -> collects config, caps, plugin status, recent errors, terminal info. Redaction options."
- **SS 11.1** Metrics: Full metric list (input_decode_duration, render_duration, pty_read_bytes, event_bus_lag, api_request_duration, plugin_call_duration, etc.)
- **SS 11.2** Doctor bundle: "JSON file containing: version, config, capabilities, plugin status, recent errors, metrics, terminal environment. `--redact strict` removes paths and env values."
- **SS 8.2** API methods: `diagnose.run`, `metrics.get`
- **SS 7.2** Extension points: Commands, pane overlays, API extensions
- **SS 7.5** WIT interface: `register-api-method`, `show-overlay`, `render-overlay`, `on-overlay-input`

---

## Files to Create

- `plugins/shux-diagnostics/plugin.toml` — Plugin manifest
- `plugins/shux-diagnostics/Cargo.toml` — Rust crate for Wasm compilation
- `plugins/shux-diagnostics/src/lib.rs` — Plugin entrypoint, lifecycle, command dispatch
- `plugins/shux-diagnostics/src/doctor.rs` — Doctor bundle collection and serialization
- `plugins/shux-diagnostics/src/overlay.rs` — Real-time diagnostics TUI overlay rendering
- `plugins/shux-diagnostics/src/metrics.rs` — Metrics collection and formatting
- `plugins/shux-diagnostics/src/redact.rs` — Redaction engine for sensitive data
- `plugins/shux-diagnostics/README.md` — User-facing documentation

## Files to Modify

- `Cargo.toml` — Add `plugins/shux-diagnostics` to workspace members
- `crates/shux-plugin/src/builtin.rs` — Register shux-diagnostics as a bundled plugin
- `docs/PROGRESS.md` — Mark task 050 complete

---

## Execution Steps

### Step 1: Define Plugin Manifest

Create `plugins/shux-diagnostics/plugin.toml`:

```toml
[plugin]
id = "com.shux.diagnostics"
name = "shux Diagnostics"
version = "1.0.0"
api = "shux:plugin@1.0.0"
kind = "wasm"
description = "Diagnostics overlay, shux doctor command, and metrics API"
license = "MIT"
min_shux = "1.0.0"

[plugin.metadata]
categories = ["diagnostics", "observability"]
keywords = ["doctor", "metrics", "health", "debug"]

[permissions]
events = [
    "pane.created", "pane.exited", "pane.resized",
    "plugin.error", "plugin.enabled", "plugin.disabled",
    "config.reloaded", "error"
]
read_pane_output = false
api_extensions = true

[extensions]
commands = ["diagnostics.toggle-overlay"]
```

### Step 2: Define Doctor Bundle Schema

The doctor bundle JSON output follows this schema:

```rust
/// Complete diagnostic bundle produced by `shux doctor`.
#[derive(Serialize, Deserialize)]
pub struct DoctorBundle {
    /// Bundle metadata
    pub meta: BundleMeta,
    /// shux version and build info
    pub version: VersionInfo,
    /// Resolved configuration (layered merge result)
    pub config: serde_json::Value,
    /// Terminal capabilities for each attached client
    pub client_caps: Vec<ClientCapsInfo>,
    /// Plugin statuses
    pub plugins: Vec<PluginStatus>,
    /// Recent error log entries (last 100)
    pub recent_errors: Vec<ErrorEntry>,
    /// Current metric snapshots
    pub metrics: MetricsSnapshot,
    /// Terminal environment variables
    pub terminal_env: TerminalEnv,
    /// PTY health for each pane
    pub pty_status: Vec<PtyStatus>,
    /// Event bus diagnostics
    pub event_bus: EventBusInfo,
    /// System info (OS, architecture, Rust version)
    pub system: SystemInfo,
}

#[derive(Serialize, Deserialize)]
pub struct BundleMeta {
    pub generated_at: String,  // ISO 8601
    pub redaction_level: String,  // "none", "standard", "strict"
    pub shux_version: String,
}

#[derive(Serialize, Deserialize)]
pub struct PluginStatus {
    pub id: String,
    pub name: String,
    pub version: String,
    pub state: String,  // "running", "stopped", "error", "disabled"
    pub kind: String,   // "wasm", "process"
    pub uptime_secs: u64,
    pub call_count: u64,
    pub avg_latency_us: u64,
    pub p99_latency_us: u64,
    pub last_error: Option<String>,
    pub memory_bytes: Option<u64>,
}

#[derive(Serialize, Deserialize)]
pub struct MetricsSnapshot {
    pub render_duration_p50_ms: f64,
    pub render_duration_p99_ms: f64,
    pub input_decode_p50_ms: f64,
    pub input_decode_p99_ms: f64,
    pub event_bus_lag_ms: f64,
    pub pty_read_bytes_total: u64,
    pub api_request_p50_ms: f64,
    pub api_request_p99_ms: f64,
    pub active_sessions: u32,
    pub active_panes: u32,
    pub total_scrollback_bytes: u64,
}
```

### Step 3: Implement Doctor Collection

Create `plugins/shux-diagnostics/src/doctor.rs`:

```rust
//! Doctor bundle collection and serialization.
//!
//! Collects comprehensive diagnostic information from the shux daemon
//! and formats it as a JSON bundle for troubleshooting.

use crate::redact::Redactor;
use serde::{Deserialize, Serialize};

/// Collect a complete doctor bundle by querying the host.
pub fn collect_bundle(redaction: RedactionLevel) -> Result<DoctorBundle, String> {
    let redactor = Redactor::new(redaction);

    // Query host for all diagnostic data
    let version = collect_version()?;
    let config = collect_config(&redactor)?;
    let client_caps = collect_client_caps()?;
    let plugins = collect_plugin_status()?;
    let recent_errors = collect_recent_errors(&redactor)?;
    let metrics = collect_metrics()?;
    let terminal_env = collect_terminal_env(&redactor)?;
    let pty_status = collect_pty_status()?;
    let event_bus = collect_event_bus_info()?;
    let system = collect_system_info(&redactor)?;

    Ok(DoctorBundle {
        meta: BundleMeta {
            generated_at: chrono_now_iso(),
            redaction_level: redaction.as_str().to_string(),
            shux_version: version.version.clone(),
        },
        version,
        config,
        client_caps,
        plugins,
        recent_errors,
        metrics,
        terminal_env,
        pty_status,
        event_bus,
        system,
    })
}

fn collect_version() -> Result<VersionInfo, String> {
    let result = host::get_config("system.version")
        .map_err(|e| format!("Failed to get version: {}", e.message))?;
    // Parse version info from host response
    Ok(VersionInfo {
        version: result.unwrap_or_else(|| "unknown".to_string()),
        build_date: String::new(),
        git_hash: String::new(),
        rust_version: String::new(),
    })
}

fn collect_config(redactor: &Redactor) -> Result<serde_json::Value, String> {
    let raw = host::get_config("*")
        .map_err(|e| format!("Failed to get config: {}", e.message))?;
    match raw {
        Some(json_str) => {
            let value: serde_json::Value = serde_json::from_str(&json_str)
                .map_err(|e| format!("Config parse error: {}", e))?;
            Ok(redactor.redact_config(value))
        }
        None => Ok(serde_json::Value::Null),
    }
}

fn collect_plugin_status() -> Result<Vec<PluginStatus>, String> {
    // Query each plugin's status via host API
    let sessions = host::list_sessions()
        .map_err(|e| format!("Failed to list sessions: {}", e.message))?;
    // Plugin status is queried via the metrics.get API extension
    Ok(Vec::new()) // Populated via host queries at runtime
}

fn collect_metrics() -> Result<MetricsSnapshot, String> {
    // Read current metric values from the host's metric registry
    Ok(MetricsSnapshot::default())
}

fn collect_terminal_env(redactor: &Redactor) -> Result<TerminalEnv, String> {
    let env_vars = [
        "TERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION",
        "COLORTERM", "LANG", "LC_ALL", "SHELL",
        "TMUX", "ZELLIJ", "SSH_CONNECTION",
    ];
    let mut entries = Vec::new();
    for var in &env_vars {
        let value = std::env::var(var).ok();
        entries.push((var.to_string(), redactor.redact_env_value(var, value)));
    }
    Ok(TerminalEnv { variables: entries })
}

/// Format a timestamp as ISO 8601.
fn chrono_now_iso() -> String {
    // Uses std::time since chrono is not available in Wasm
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}Z", duration.as_secs())
}

#[derive(Debug, Clone, Copy)]
pub enum RedactionLevel {
    None,
    Standard,
    Strict,
}

impl RedactionLevel {
    pub fn as_str(&self) -> &str {
        match self {
            RedactionLevel::None => "none",
            RedactionLevel::Standard => "standard",
            RedactionLevel::Strict => "strict",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "strict" => RedactionLevel::Strict,
            "standard" => RedactionLevel::Standard,
            _ => RedactionLevel::None,
        }
    }
}
```

### Step 4: Implement Redaction Engine

Create `plugins/shux-diagnostics/src/redact.rs`:

```rust
//! Redaction engine for sensitive data in doctor bundles.
//!
//! Supports three levels:
//! - none: no redaction (for personal debugging)
//! - standard: redact home paths, auth tokens
//! - strict: redact all paths, all env values, hostnames

pub struct Redactor {
    level: RedactionLevel,
}

impl Redactor {
    pub fn new(level: RedactionLevel) -> Self {
        Self { level }
    }

    /// Redact a config value tree, replacing sensitive values.
    pub fn redact_config(&self, mut value: serde_json::Value) -> serde_json::Value {
        match self.level {
            RedactionLevel::None => value,
            RedactionLevel::Standard => {
                self.redact_keys(&mut value, &["token", "auth", "secret", "password"]);
                self.redact_home_paths(&mut value);
                value
            }
            RedactionLevel::Strict => {
                self.redact_keys(&mut value, &["token", "auth", "secret", "password"]);
                self.redact_all_paths(&mut value);
                value
            }
        }
    }

    /// Redact environment variable values based on level.
    pub fn redact_env_value(&self, name: &str, value: Option<String>) -> Option<String> {
        match self.level {
            RedactionLevel::None => value,
            RedactionLevel::Standard => {
                if name.contains("TOKEN") || name.contains("SECRET") || name.contains("KEY") {
                    value.map(|_| "[REDACTED]".to_string())
                } else {
                    value
                }
            }
            RedactionLevel::Strict => {
                // In strict mode, only show presence/absence
                value.map(|v| {
                    if v.is_empty() {
                        "(empty)".to_string()
                    } else {
                        format!("[REDACTED len={}]", v.len())
                    }
                })
            }
        }
    }

    /// Redact a file path (standard: replace home dir; strict: replace entire path).
    pub fn redact_path(&self, path: &str) -> String {
        match self.level {
            RedactionLevel::None => path.to_string(),
            RedactionLevel::Standard => {
                let home = std::env::var("HOME").unwrap_or_default();
                if !home.is_empty() && path.starts_with(&home) {
                    format!("$HOME{}", &path[home.len()..])
                } else {
                    path.to_string()
                }
            }
            RedactionLevel::Strict => "[REDACTED_PATH]".to_string(),
        }
    }

    fn redact_keys(&self, value: &mut serde_json::Value, patterns: &[&str]) {
        if let serde_json::Value::Object(map) = value {
            for (key, val) in map.iter_mut() {
                let key_lower = key.to_lowercase();
                if patterns.iter().any(|p| key_lower.contains(p)) {
                    *val = serde_json::Value::String("[REDACTED]".to_string());
                } else {
                    self.redact_keys(val, patterns);
                }
            }
        }
    }

    fn redact_home_paths(&self, value: &mut serde_json::Value) {
        let home = std::env::var("HOME").unwrap_or_default();
        if home.is_empty() {
            return;
        }
        self.walk_replace_strings(value, &home, "$HOME");
    }

    fn redact_all_paths(&self, value: &mut serde_json::Value) {
        match value {
            serde_json::Value::String(s) if s.starts_with('/') => {
                *s = "[REDACTED_PATH]".to_string();
            }
            serde_json::Value::Object(map) => {
                for val in map.values_mut() {
                    self.redact_all_paths(val);
                }
            }
            serde_json::Value::Array(arr) => {
                for val in arr.iter_mut() {
                    self.redact_all_paths(val);
                }
            }
            _ => {}
        }
    }

    fn walk_replace_strings(
        &self,
        value: &mut serde_json::Value,
        find: &str,
        replace: &str,
    ) {
        match value {
            serde_json::Value::String(s) => {
                if s.contains(find) {
                    *s = s.replace(find, replace);
                }
            }
            serde_json::Value::Object(map) => {
                for val in map.values_mut() {
                    self.walk_replace_strings(val, find, replace);
                }
            }
            serde_json::Value::Array(arr) => {
                for val in arr.iter_mut() {
                    self.walk_replace_strings(val, find, replace);
                }
            }
            _ => {}
        }
    }
}
```

### Step 5: Implement Diagnostics Overlay

Create `plugins/shux-diagnostics/src/overlay.rs`:

```rust
//! Real-time diagnostics TUI overlay.
//!
//! Renders a transparent overlay on the active pane showing:
//! - Health status (green/yellow/red)
//! - PTY status (alive, bytes/s)
//! - Renderer stats (frame time, diff cells)
//! - Event bus lag
//! - Plugin latencies (table)
//! - Memory usage
//!
//! Refreshes every 500ms via event subscription.
//! Toggle with `diagnostics.toggle-overlay` command.

use serde::Serialize;

/// Current overlay state.
pub struct DiagnosticsOverlay {
    /// Whether the overlay is currently visible.
    pub visible: bool,
    /// Pane ID where the overlay is shown (None = focused pane).
    pub pane_id: Option<String>,
    /// Last collected metrics for rendering.
    pub last_metrics: Option<OverlayMetrics>,
    /// Which section is currently highlighted (for scrolling).
    pub selected_section: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct OverlayMetrics {
    pub health: HealthStatus,
    pub render_time_p50_ms: f64,
    pub render_time_p99_ms: f64,
    pub render_diff_cells: u64,
    pub event_bus_lag_ms: f64,
    pub pty_throughput_bytes_s: u64,
    pub active_panes: u32,
    pub memory_rss_mb: f64,
    pub plugins: Vec<PluginMetric>,
}

#[derive(Debug, Clone, Serialize)]
pub enum HealthStatus {
    Healthy,
    Degraded(String),  // reason
    Unhealthy(String), // reason
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginMetric {
    pub name: String,
    pub state: String,
    pub p99_latency_us: u64,
    pub call_count: u64,
}

impl DiagnosticsOverlay {
    pub fn new() -> Self {
        Self {
            visible: false,
            pane_id: None,
            last_metrics: None,
            selected_section: 0,
        }
    }

    /// Toggle overlay visibility.
    pub fn toggle(&mut self, pane_id: &str) {
        self.visible = !self.visible;
        if self.visible {
            self.pane_id = Some(pane_id.to_string());
        } else {
            self.pane_id = None;
        }
    }

    /// Render the overlay as ANSI-styled text.
    ///
    /// Returns None if invisible or no metrics available.
    pub fn render(&self, width: u16, height: u16) -> Option<String> {
        if !self.visible {
            return None;
        }

        let metrics = self.last_metrics.as_ref()?;
        let mut lines: Vec<String> = Vec::new();

        // Header
        let health_indicator = match &metrics.health {
            HealthStatus::Healthy => "\x1b[32m●\x1b[0m Healthy",
            HealthStatus::Degraded(r) => &format!("\x1b[33m●\x1b[0m Degraded: {}", r),
            HealthStatus::Unhealthy(r) => &format!("\x1b[31m●\x1b[0m Unhealthy: {}", r),
        };
        lines.push(format!(
            "\x1b[1m┌─ shux diagnostics ─{}─┐\x1b[0m",
            "─".repeat(width.saturating_sub(24) as usize)
        ));
        lines.push(format!("│ {} ", health_indicator));
        lines.push("│".to_string());

        // Renderer section
        lines.push("\x1b[1m│ Renderer\x1b[0m".to_string());
        lines.push(format!(
            "│   Frame time: p50={:.1}ms p99={:.1}ms",
            metrics.render_time_p50_ms, metrics.render_time_p99_ms
        ));
        lines.push(format!("│   Diff cells: {}", metrics.render_diff_cells));
        lines.push("│".to_string());

        // Event bus section
        lines.push("\x1b[1m│ Event Bus\x1b[0m".to_string());
        lines.push(format!("│   Lag: {:.1}ms", metrics.event_bus_lag_ms));
        lines.push("│".to_string());

        // PTY section
        lines.push("\x1b[1m│ PTY\x1b[0m".to_string());
        lines.push(format!(
            "│   Panes: {}  Throughput: {} bytes/s",
            metrics.active_panes, metrics.pty_throughput_bytes_s
        ));
        lines.push("│".to_string());

        // Memory section
        lines.push("\x1b[1m│ Memory\x1b[0m".to_string());
        lines.push(format!("│   RSS: {:.1} MB", metrics.memory_rss_mb));
        lines.push("│".to_string());

        // Plugin latencies
        lines.push("\x1b[1m│ Plugins\x1b[0m".to_string());
        lines.push("│   Name                State     p99(us)  Calls".to_string());
        lines.push(format!(
            "│   {}",
            "─".repeat(width.saturating_sub(6) as usize)
        ));
        for plugin in &metrics.plugins {
            lines.push(format!(
                "│   {:<20} {:<9} {:>7}  {}",
                truncate(&plugin.name, 20),
                plugin.state,
                plugin.p99_latency_us,
                plugin.call_count,
            ));
        }

        // Footer
        lines.push(format!(
            "\x1b[1m└{}┘\x1b[0m",
            "─".repeat(width.saturating_sub(2) as usize)
        ));
        lines.push("\x1b[2mPress 'q' to close | 'r' to refresh\x1b[0m".to_string());

        // Truncate to available height
        let max_lines = height.saturating_sub(1) as usize;
        lines.truncate(max_lines);

        Some(lines.join("\n"))
    }

    /// Handle a key event while the overlay is visible.
    /// Returns true if the key was consumed.
    pub fn handle_input(&mut self, key: &str) -> bool {
        if !self.visible {
            return false;
        }

        match key {
            "q" | "Escape" => {
                self.visible = false;
                self.pane_id = None;
                true
            }
            "r" => {
                // Request metric refresh — handled by plugin on next tick
                true
            }
            "j" | "Down" => {
                self.selected_section = self.selected_section.saturating_add(1);
                true
            }
            "k" | "Up" => {
                self.selected_section = self.selected_section.saturating_sub(1);
                true
            }
            _ => false,
        }
    }
}

/// Truncate a string to max_len, appending ".." if truncated.
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}..", &s[..max_len.saturating_sub(2)])
    }
}
```

### Step 6: Implement Metrics Collection

Create `plugins/shux-diagnostics/src/metrics.rs`:

```rust
//! Metrics collection and formatting for diagnostics.
//!
//! Queries the host for current metric values and formats
//! them for both the overlay and the doctor bundle.

use serde::{Deserialize, Serialize};

/// All metrics exposed via the `metrics.get` API method.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AllMetrics {
    pub shux_input_decode_duration_ms: HistogramSummary,
    pub shux_render_duration_ms: HistogramSummary,
    pub shux_render_diff_cells: HistogramSummary,
    pub shux_pty_read_bytes_total: u64,
    pub shux_event_bus_lag_ms: f64,
    pub shux_api_request_duration_ms: HistogramSummary,
    pub shux_plugin_call_duration_ms: Vec<PluginCallMetric>,
    pub shux_sessions_total: u32,
    pub shux_panes_total: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HistogramSummary {
    pub count: u64,
    pub p50: f64,
    pub p99: f64,
    pub max: f64,
    pub mean: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginCallMetric {
    pub plugin_id: String,
    pub plugin_name: String,
    pub call_count: u64,
    pub p50_us: u64,
    pub p99_us: u64,
}

/// Query the host for current metrics via the metrics.get API.
pub fn collect_all_metrics() -> Result<AllMetrics, String> {
    // This calls the host's metrics.get method
    // The host collects from its internal metric registry
    let result_json = host::get_config("metrics.snapshot")
        .map_err(|e| format!("Failed to get metrics: {}", e.message))?;

    match result_json {
        Some(json) => serde_json::from_str(&json)
            .map_err(|e| format!("Metrics parse error: {}", e)),
        None => Ok(AllMetrics::default()),
    }
}

/// Format metrics for human-readable display.
pub fn format_metrics_summary(metrics: &AllMetrics) -> String {
    let mut lines = Vec::new();

    lines.push("=== shux Metrics ===".to_string());
    lines.push(String::new());

    lines.push(format!(
        "Render:      p50={:.1}ms p99={:.1}ms (n={})",
        metrics.shux_render_duration_ms.p50,
        metrics.shux_render_duration_ms.p99,
        metrics.shux_render_duration_ms.count,
    ));

    lines.push(format!(
        "Input:       p50={:.1}ms p99={:.1}ms",
        metrics.shux_input_decode_duration_ms.p50,
        metrics.shux_input_decode_duration_ms.p99,
    ));

    lines.push(format!(
        "API:         p50={:.1}ms p99={:.1}ms",
        metrics.shux_api_request_duration_ms.p50,
        metrics.shux_api_request_duration_ms.p99,
    ));

    lines.push(format!(
        "Event lag:   {:.1}ms",
        metrics.shux_event_bus_lag_ms,
    ));

    lines.push(format!(
        "PTY bytes:   {} total",
        metrics.shux_pty_read_bytes_total,
    ));

    lines.push(format!(
        "Active:      {} sessions, {} panes",
        metrics.shux_sessions_total,
        metrics.shux_panes_total,
    ));

    if !metrics.shux_plugin_call_duration_ms.is_empty() {
        lines.push(String::new());
        lines.push("Plugin latencies:".to_string());
        for pm in &metrics.shux_plugin_call_duration_ms {
            lines.push(format!(
                "  {}: p50={}us p99={}us (n={})",
                pm.plugin_name, pm.p50_us, pm.p99_us, pm.call_count,
            ));
        }
    }

    lines.join("\n")
}
```

### Step 7: Implement Plugin Entrypoint

Create `plugins/shux-diagnostics/src/lib.rs`:

```rust
//! shux-diagnostics — Bundled diagnostics plugin.
//!
//! Provides:
//! 1. `shux doctor` command — comprehensive diagnostic bundle
//! 2. Diagnostics overlay — real-time metrics in TUI
//! 3. `metrics.get` API method — programmatic metric access
//! 4. `diagnose.run` API method — trigger doctor bundle collection

mod doctor;
mod metrics;
mod overlay;
mod redact;

use doctor::RedactionLevel;
use overlay::DiagnosticsOverlay;
use shux_plugin_api::prelude::*;

struct DiagnosticsPlugin {
    overlay: DiagnosticsOverlay,
}

impl Plugin for DiagnosticsPlugin {
    fn init(&mut self, _config: &str) -> Result<(), PluginError> {
        host::log(LogLevel::Info, "shux-diagnostics initializing");

        // Register API methods
        host::register_api_method(
            "diagnose.run",
            "Run diagnostics and return a doctor bundle",
        )
        .map_err(|e| PluginError {
            code: -1,
            message: format!("Failed to register diagnose.run: {}", e.message),
        })?;

        host::register_api_method(
            "metrics.get",
            "Get current metrics snapshot",
        )
        .map_err(|e| PluginError {
            code: -1,
            message: format!("Failed to register metrics.get: {}", e.message),
        })?;

        host::log(LogLevel::Info, "shux-diagnostics initialized");
        Ok(())
    }

    fn shutdown(&mut self) {
        if self.overlay.visible {
            if let Some(pane_id) = &self.overlay.pane_id {
                let _ = host::hide_overlay(pane_id);
            }
        }
        host::log(LogLevel::Debug, "shux-diagnostics shutting down");
    }

    fn on_event(&mut self, event_json: &str) -> Result<(), PluginError> {
        // Track errors for the recent errors list in doctor bundle
        if let Ok(event) = serde_json::from_str::<serde_json::Value>(event_json) {
            if let Some(event_type) = event.get("type").and_then(|t| t.as_str()) {
                if event_type == "error" || event_type == "plugin.error" {
                    host::log(
                        LogLevel::Debug,
                        &format!("Diagnostics captured error event: {}", event_type),
                    );
                }
            }
        }

        // If overlay is visible, refresh metrics on relevant events
        if self.overlay.visible {
            // Refresh overlay metrics periodically via event-driven updates
        }

        Ok(())
    }

    fn on_command(&mut self, name: &str, args: &[String]) -> Result<String, PluginError> {
        match name {
            "diagnose.run" => {
                let redaction = args
                    .iter()
                    .find(|a| a.starts_with("--redact"))
                    .and_then(|a| a.strip_prefix("--redact=").or_else(|| a.strip_prefix("--redact ")))
                    .map(RedactionLevel::from_str)
                    .unwrap_or(RedactionLevel::Standard);

                let bundle = doctor::collect_bundle(redaction).map_err(|e| PluginError {
                    code: -1,
                    message: format!("Doctor collection failed: {}", e),
                })?;

                serde_json::to_string_pretty(&bundle).map_err(|e| PluginError {
                    code: -1,
                    message: format!("Serialization failed: {}", e),
                })
            }

            "metrics.get" => {
                let all_metrics =
                    metrics::collect_all_metrics().map_err(|e| PluginError {
                        code: -1,
                        message: format!("Metrics collection failed: {}", e),
                    })?;
                serde_json::to_string(&all_metrics).map_err(|e| PluginError {
                    code: -1,
                    message: format!("Serialization failed: {}", e),
                })
            }

            "diagnostics.toggle-overlay" => {
                let pane_id = match host::get_active_pane() {
                    Ok(pane) => pane.id,
                    Err(e) => {
                        return Err(PluginError {
                            code: -1,
                            message: format!("No active pane: {}", e.message),
                        });
                    }
                };

                self.overlay.toggle(&pane_id);

                if self.overlay.visible {
                    host::show_overlay(&pane_id).map_err(|e| PluginError {
                        code: -1,
                        message: format!("Failed to show overlay: {}", e.message),
                    })?;
                } else {
                    host::hide_overlay(&pane_id).map_err(|e| PluginError {
                        code: -1,
                        message: format!("Failed to hide overlay: {}", e.message),
                    })?;
                }

                Ok(format!(
                    "{{\"overlay\": {}}}",
                    if self.overlay.visible { "true" } else { "false" }
                ))
            }

            _ => Err(PluginError {
                code: -1,
                message: format!("Unknown command: {}", name),
            }),
        }
    }

    fn render_segment(&mut self, _id: &str, _width: u16) -> Result<String, PluginError> {
        Ok(String::new())
    }

    fn render_overlay(
        &mut self,
        _pane_id: &str,
        width: u16,
        height: u16,
    ) -> Result<Option<String>, PluginError> {
        // Refresh metrics before rendering
        if self.overlay.visible {
            if let Ok(all_metrics) = metrics::collect_all_metrics() {
                self.overlay.last_metrics = Some(overlay::OverlayMetrics {
                    health: determine_health(&all_metrics),
                    render_time_p50_ms: all_metrics.shux_render_duration_ms.p50,
                    render_time_p99_ms: all_metrics.shux_render_duration_ms.p99,
                    render_diff_cells: all_metrics.shux_render_diff_cells.count,
                    event_bus_lag_ms: all_metrics.shux_event_bus_lag_ms,
                    pty_throughput_bytes_s: 0, // Calculated from delta
                    active_panes: all_metrics.shux_panes_total,
                    memory_rss_mb: 0.0, // Queried from system
                    plugins: all_metrics
                        .shux_plugin_call_duration_ms
                        .iter()
                        .map(|pm| overlay::PluginMetric {
                            name: pm.plugin_name.clone(),
                            state: "running".to_string(),
                            p99_latency_us: pm.p99_us,
                            call_count: pm.call_count,
                        })
                        .collect(),
                });
            }
        }

        Ok(self.overlay.render(width, height))
    }

    fn on_overlay_input(
        &mut self,
        _pane_id: &str,
        key_event_json: &str,
    ) -> Result<bool, PluginError> {
        let key = parse_key_from_json(key_event_json);
        Ok(self.overlay.handle_input(&key))
    }

    fn intercept_event(&mut self, event_json: &str) -> Result<Option<String>, PluginError> {
        Ok(Some(event_json.to_string()))
    }
}

fn determine_health(metrics: &metrics::AllMetrics) -> overlay::HealthStatus {
    // Check against performance budgets from PRD SS14.1
    if metrics.shux_render_duration_ms.p99 > 50.0 {
        return overlay::HealthStatus::Unhealthy(format!(
            "Render p99 {:.1}ms > 50ms hard limit",
            metrics.shux_render_duration_ms.p99
        ));
    }
    if metrics.shux_render_duration_ms.p99 > 25.0 {
        return overlay::HealthStatus::Degraded(format!(
            "Render p99 {:.1}ms > 25ms target",
            metrics.shux_render_duration_ms.p99
        ));
    }
    if metrics.shux_event_bus_lag_ms > 10.0 {
        return overlay::HealthStatus::Degraded(format!(
            "Event bus lag {:.1}ms",
            metrics.shux_event_bus_lag_ms
        ));
    }
    overlay::HealthStatus::Healthy
}

fn parse_key_from_json(json: &str) -> String {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.get("key").and_then(|k| k.as_str()).map(String::from))
        .unwrap_or_default()
}

export_plugin!(DiagnosticsPlugin);
```

### Step 8: Add Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_toggle_state() {
        let mut overlay = DiagnosticsOverlay::new();
        assert!(!overlay.visible);

        overlay.toggle("pane-1");
        assert!(overlay.visible);
        assert_eq!(overlay.pane_id.as_deref(), Some("pane-1"));

        overlay.toggle("pane-1");
        assert!(!overlay.visible);
        assert!(overlay.pane_id.is_none());
    }

    #[test]
    fn overlay_render_returns_none_when_hidden() {
        let overlay = DiagnosticsOverlay::new();
        assert!(overlay.render(80, 24).is_none());
    }

    #[test]
    fn overlay_render_returns_none_without_metrics() {
        let mut overlay = DiagnosticsOverlay::new();
        overlay.visible = true;
        assert!(overlay.render(80, 24).is_none());
    }

    #[test]
    fn overlay_handle_input_q_closes() {
        let mut overlay = DiagnosticsOverlay::new();
        overlay.toggle("pane-1");
        assert!(overlay.visible);

        let consumed = overlay.handle_input("q");
        assert!(consumed);
        assert!(!overlay.visible);
    }

    #[test]
    fn overlay_handle_input_escape_closes() {
        let mut overlay = DiagnosticsOverlay::new();
        overlay.toggle("pane-1");

        let consumed = overlay.handle_input("Escape");
        assert!(consumed);
        assert!(!overlay.visible);
    }

    #[test]
    fn overlay_ignores_input_when_hidden() {
        let mut overlay = DiagnosticsOverlay::new();
        assert!(!overlay.handle_input("q"));
    }

    #[test]
    fn redactor_none_preserves_all() {
        let redactor = Redactor::new(RedactionLevel::None);
        assert_eq!(redactor.redact_path("/home/user/secret"), "/home/user/secret");
        assert_eq!(
            redactor.redact_env_value("API_TOKEN", Some("secret123".to_string())),
            Some("secret123".to_string())
        );
    }

    #[test]
    fn redactor_standard_hides_tokens() {
        let redactor = Redactor::new(RedactionLevel::Standard);
        assert_eq!(
            redactor.redact_env_value("API_TOKEN", Some("secret123".to_string())),
            Some("[REDACTED]".to_string())
        );
    }

    #[test]
    fn redactor_strict_hides_all_paths() {
        let redactor = Redactor::new(RedactionLevel::Strict);
        assert_eq!(redactor.redact_path("/any/path"), "[REDACTED_PATH]");
    }

    #[test]
    fn redactor_strict_shows_length_only() {
        let redactor = Redactor::new(RedactionLevel::Strict);
        assert_eq!(
            redactor.redact_env_value("SHELL", Some("/bin/zsh".to_string())),
            Some("[REDACTED len=8]".to_string())
        );
    }

    #[test]
    fn health_status_healthy_within_budget() {
        let metrics = metrics::AllMetrics {
            shux_render_duration_ms: metrics::HistogramSummary {
                p99: 20.0,
                ..Default::default()
            },
            shux_event_bus_lag_ms: 2.0,
            ..Default::default()
        };
        assert!(matches!(determine_health(&metrics), overlay::HealthStatus::Healthy));
    }

    #[test]
    fn health_status_degraded_on_slow_render() {
        let metrics = metrics::AllMetrics {
            shux_render_duration_ms: metrics::HistogramSummary {
                p99: 30.0,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(matches!(determine_health(&metrics), overlay::HealthStatus::Degraded(_)));
    }

    #[test]
    fn health_status_unhealthy_on_very_slow_render() {
        let metrics = metrics::AllMetrics {
            shux_render_duration_ms: metrics::HistogramSummary {
                p99: 60.0,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(matches!(determine_health(&metrics), overlay::HealthStatus::Unhealthy(_)));
    }
}
```

---

## Verification

### Functional

```bash
# Build the diagnostics plugin
cargo build -p shux-diagnostics

# Run the doctor command
shux doctor
# Expected: JSON output with version, config, caps, plugins, errors, metrics

# Run with redaction
shux doctor --redact strict
# Expected: paths replaced with [REDACTED_PATH], env values show length only

# Toggle diagnostics overlay
shux diagnostics toggle-overlay
# Expected: overlay appears showing health, render stats, event lag, plugins

# Query metrics via API
shux api metrics.get --format json
# Expected: JSON with all metric values from SS11.1
```

### Tests

```bash
# Run diagnostics plugin tests
cargo nextest run -p shux-diagnostics

# Expected tests passing:
# - overlay_toggle_state
# - overlay_render_returns_none_when_hidden
# - overlay_render_returns_none_without_metrics
# - overlay_handle_input_q_closes
# - overlay_handle_input_escape_closes
# - overlay_ignores_input_when_hidden
# - redactor_none_preserves_all
# - redactor_standard_hides_tokens
# - redactor_strict_hides_all_paths
# - redactor_strict_shows_length_only
# - health_status_healthy_within_budget
# - health_status_degraded_on_slow_render
# - health_status_unhealthy_on_very_slow_render
```

---

## Completion Criteria

- [ ] Plugin manifest declares api_extensions permission and registers commands
- [ ] `shux doctor` produces a JSON bundle with: version, config, client_caps, plugins, recent_errors, metrics, terminal_env, pty_status, event_bus, system
- [ ] `--redact strict` removes all paths, shows only value lengths for env vars
- [ ] `--redact standard` (default) replaces home dir with $HOME, redacts tokens/secrets
- [ ] No redaction (`--redact none`) preserves all values
- [ ] `diagnose.run` API method works via JSON-RPC
- [ ] `metrics.get` API method returns all metrics from PRD SS11.1
- [ ] Diagnostics overlay toggles via `diagnostics.toggle-overlay` command
- [ ] Overlay shows: health status (green/yellow/red), render stats, event lag, PTY throughput, memory, plugin latencies
- [ ] Overlay responds to keyboard: q/Escape to close, r to refresh, j/k to scroll
- [ ] Health determination uses PRD SS14.1 performance budgets as thresholds
- [ ] Overlay rendering respects available width and height (no overflow)
- [ ] Plugin handles errors gracefully (missing metrics, host API failures)
- [ ] Unit tests pass for overlay, redaction, and health determination
- [ ] Plugin compiles to Wasm target (wasm32-wasip2)

---

## Commit Message

```
feat(plugins): add shux-diagnostics with doctor, overlay, and metrics API

- `shux doctor` collects config, caps, plugins, errors, metrics,
  terminal info into a JSON bundle with 3 redaction levels
- TUI diagnostics overlay shows real-time health, render stats,
  event bus lag, PTY throughput, memory, and plugin latencies
- Registers diagnose.run and metrics.get API methods
- Health status derived from PRD performance budgets (green/yellow/red)
- Proves overlay system (task 046) and API extension (task 045)
```

---

## Session Protocol

1. **Before starting:** Read task 046 (overlay system) to understand `show-overlay`, `render-overlay`, `on-overlay-input` contract. Read task 045 (API extensions) to understand `register-api-method`. Read PRD SS11.1 for the complete metrics list and SS14.1 for performance budget thresholds.
2. **During:** Implement in order: plugin manifest (Step 1), doctor schema (Step 2), doctor collection (Step 3), redaction (Step 4), overlay (Step 5), metrics (Step 6), entrypoint (Step 7), tests (Step 8). Run `cargo check -p shux-diagnostics` after each module.
3. **Edge cases to watch for:**
   - Doctor collection when no clients are attached (client_caps is empty)
   - Overlay on very small terminals (height < 10 lines)
   - Metrics not yet populated (daemon just started, zeroed values)
   - Strict redaction must not leak any filesystem paths
   - Plugin errors during metric collection should not crash the overlay
4. **After:** Run full test suite (`cargo nextest run --workspace`). Manually test `shux doctor` output and verify JSON is valid and complete. Update `docs/PROGRESS.md`. Update `CLAUDE.md` Learnings (create from task 000 template if missing).
