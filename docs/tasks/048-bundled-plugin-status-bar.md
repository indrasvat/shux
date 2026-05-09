# 048 — Bundled Plugin: Status Bar

**Status:** Pending
**Depends On:** 046, 047
**Parallelizable With:** 049

---

## Problem

The status bar is the most visible piece of shux's UI -- it shows session names, window lists, pane info, and clock. In the PRD's architecture, the status bar is not a hardcoded UI component but a bundled Wasm plugin that proves the plugin system is powerful enough to implement core UI features. Task 026 implemented a hardcoded status bar as a temporary measure. This task replaces it with a proper plugin-based status bar.

The plugin-based status bar must: subscribe to session/window/pane/theme events to stay current, render left/center/right segments, support plugin-contributed segments (other plugins add segments via `set-status-segment`), be theme-aware (reads theme tokens for colors), support clickable segments where mouse input is available, and be replaceable/extendable by the user.

This task is the first bundled plugin -- it validates the entire plugin infrastructure built in tasks 038-047 by exercising event subscription, status segment rendering, theme token access, and the inter-plugin event bus.

## PRD Reference

- **section 7.7** — Bundled plugins: "shux-status-bar: Default status line: session, windows, pane info, clock. Status segments, event reactors."
- **section 6.1** — Status bar: "Rendered by bundled status-bar plugin. Shows session name, window list, active pane info, clock. Clickable segments where terminal supports."
- **section 7.2** — Extension points: Status bar segments
- **section 7.5** — WIT: `set-status-segment`, `render-segment`
- **section 7.1** — "Plugins are first-class: Bundled v1.0 plugins prove the plugin API by using it"

---

## Files to Create

- `plugins/shux-status-bar/plugin.toml` — Plugin manifest: metadata, permissions (events only), extension points
- `plugins/shux-status-bar/src/lib.rs` — Status bar plugin implementation: event handling, segment rendering, theme integration
- `plugins/shux-status-bar/Cargo.toml` — Rust crate for compiling to Wasm

## Files to Modify

- `Cargo.toml` (workspace root) — Add `plugins/shux-status-bar` to workspace members
- `crates/shux-plugin/src/lib.rs` — Register the bundled status bar plugin during daemon startup

---

## Execution Steps

### Step 1: Create the plugin manifest in `plugins/shux-status-bar/plugin.toml`

```toml
[plugin]
id = "io.shux.status-bar"
name = "shux Status Bar"
version = "0.1.0"
api = "shux:plugin@1.0.0"
kind = "wasm"
description = "Default status bar for shux — session name, window list, pane info, clock"
license = "MIT"
min_shux = "0.1.0"

[plugin.metadata]
categories = ["ui", "status-bar"]
keywords = ["status", "bar", "ui", "built-in"]
bundled = true

[permissions]
# Status bar needs to observe state changes but does not control panes/sessions.
events = [
    "session.created",
    "session.renamed",
    "session.killed",
    "session.attached",
    "window.created",
    "window.activated",
    "window.renamed",
    "window.killed",
    "pane.focused",
    "pane.title_changed",
    "theme.changed",
    "plugin.event",
]
read_pane_output = false
send_keys = false
manage_panes = false
manage_sessions = false
api_extensions = false
exec = false
fs_read = []
fs_write = []
network = false
clipboard = false
intercept_events = []
override_commands = []

[extensions]
status_segments = ["session_name", "window_list", "pane_info", "clock", "plugin_segments"]
```

### Step 2: Create the Cargo.toml in `plugins/shux-status-bar/Cargo.toml`

```toml
[package]
name = "shux-status-bar"
version = "0.1.0"
edition = "2024"
publish = false

[lib]
crate-type = ["cdylib"]

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# WIT bindings (generated or hand-written)
# wit-bindgen = "0.41"  # When WIT bindgen is set up
```

### Step 3: Implement the status bar plugin in `plugins/shux-status-bar/src/lib.rs`

```rust
//! shux-status-bar — The default status bar plugin for shux.
//!
//! This bundled plugin renders the status bar at the bottom of the screen.
//! It subscribes to session, window, pane, and theme events to keep the
//! display current.
//!
//! ## Segments
//!
//! The status bar is divided into three regions:
//!
//! ```text
//! ┌──────────────┬──────────────────────────────┬──────────────────┐
//! │ Left         │ Center                       │ Right            │
//! │ session_name │ window_list                  │ pane_info | clock│
//! └──────────────┴──────────────────────────────┴──────────────────┘
//! ```
//!
//! Plugin-contributed segments (from other plugins via set-status-segment)
//! are inserted between the left and right built-in segments.
//!
//! ## Theme integration
//!
//! The status bar reads theme tokens for its colors:
//! - `status.bg` — background color
//! - `status.fg` — foreground (text) color
//! - `status.accent` — active window highlight
//! - `status.inactive` — inactive window color

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The status bar plugin state.
///
/// Maintained across event callbacks. Updated incrementally as events arrive.
struct StatusBar {
    /// Current session info.
    session: SessionState,

    /// All windows in the current session, in order.
    windows: Vec<WindowState>,

    /// Active window ID.
    active_window_id: String,

    /// Active pane info.
    active_pane: PaneState,

    /// Theme tokens for rendering.
    theme: ThemeTokens,

    /// Plugin-contributed segments (from other plugins).
    /// Keyed by segment ID, ordered by registration time.
    plugin_segments: Vec<PluginSegment>,

    /// Cached render width.
    width: u16,
}

#[derive(Debug, Clone, Default)]
struct SessionState {
    id: String,
    name: String,
}

#[derive(Debug, Clone)]
struct WindowState {
    id: String,
    name: String,
    index: usize,
    is_active: bool,
    has_activity: bool,
}

#[derive(Debug, Clone, Default)]
struct PaneState {
    id: String,
    title: String,
    command: String,
}

#[derive(Debug, Clone)]
struct ThemeTokens {
    bg: String,
    fg: String,
    accent: String,
    inactive: String,
}

impl Default for ThemeTokens {
    fn default() -> Self {
        Self {
            bg: "#1e1e2e".to_string(),  // catppuccin mocha base
            fg: "#cdd6f4".to_string(),  // catppuccin mocha text
            accent: "#89b4fa".to_string(), // catppuccin mocha blue
            inactive: "#6c7086".to_string(), // catppuccin mocha overlay0
        }
    }
}

/// A segment contributed by another plugin.
#[derive(Debug, Clone)]
struct PluginSegment {
    id: String,
    text: String,
    fg: Option<String>,
    bg: Option<String>,
}

impl StatusBar {
    fn new() -> Self {
        Self {
            session: SessionState::default(),
            windows: Vec::new(),
            active_window_id: String::new(),
            active_pane: PaneState::default(),
            theme: ThemeTokens::default(),
            plugin_segments: Vec::new(),
            width: 80,
        }
    }

    /// Handle an incoming event and update internal state.
    fn handle_event(&mut self, event_json: &str) -> Result<(), String> {
        let event: serde_json::Value = serde_json::from_str(event_json)
            .map_err(|e| format!("Failed to parse event: {}", e))?;

        let event_type = event["type"]
            .as_str()
            .unwrap_or("");

        match event_type {
            "session.created" | "session.renamed" | "session.attached" => {
                if let Some(name) = event["name"].as_str().or(event["new_name"].as_str()) {
                    self.session.name = name.to_string();
                }
                if let Some(id) = event["session_id"].as_str() {
                    self.session.id = id.to_string();
                }
            }

            "window.created" => {
                let id = event["window_id"].as_str().unwrap_or("").to_string();
                let title = event["title"].as_str().unwrap_or("").to_string();
                let index = self.windows.len();
                self.windows.push(WindowState {
                    id: id.clone(),
                    name: title,
                    index,
                    is_active: false,
                    has_activity: false,
                });
            }

            "window.activated" => {
                let new_id = event["window_id"].as_str().unwrap_or("");
                self.active_window_id = new_id.to_string();
                for win in &mut self.windows {
                    win.is_active = win.id == new_id;
                }
            }

            "window.renamed" => {
                let id = event["window_id"].as_str().unwrap_or("");
                let new_title = event["new_title"].as_str().unwrap_or("");
                if let Some(win) = self.windows.iter_mut().find(|w| w.id == id) {
                    win.name = new_title.to_string();
                }
            }

            "window.killed" => {
                let id = event["window_id"].as_str().unwrap_or("");
                self.windows.retain(|w| w.id != id);
                // Re-index windows.
                for (i, win) in self.windows.iter_mut().enumerate() {
                    win.index = i;
                }
            }

            "pane.focused" => {
                if let Some(id) = event["pane_id"].as_str() {
                    self.active_pane.id = id.to_string();
                }
            }

            "pane.title_changed" => {
                let id = event["pane_id"].as_str().unwrap_or("");
                if id == self.active_pane.id {
                    if let Some(title) = event["new_title"].as_str() {
                        self.active_pane.title = title.to_string();
                    }
                }
            }

            "theme.changed" => {
                // Read updated theme tokens.
                // In the full implementation, this would call get-config
                // to read the current theme tokens.
                if let Some(new_theme) = event["new_theme"].as_str() {
                    // Parse theme tokens from the event or query the host.
                    self.update_theme_from_event(&event);
                }
            }

            "plugin.event" => {
                // Handle inter-plugin events (e.g., plugin-contributed segments).
                // Other plugins call set-status-segment, which the host routes
                // to us as plugin.event or through the segment update mechanism.
            }

            _ => {
                // Unknown event type — ignore.
            }
        }

        Ok(())
    }

    fn update_theme_from_event(&mut self, _event: &serde_json::Value) {
        // In the full implementation, query the host for theme tokens.
        // For now, use defaults.
    }

    /// Render the left segment: session name.
    fn render_left(&self) -> String {
        let session_name = if self.session.name.is_empty() {
            "default"
        } else {
            &self.session.name
        };

        format!(
            "\x1b[48;2;{bg}m\x1b[38;2;{accent}m [{session}] \x1b[0m",
            bg = hex_to_rgb_params(&self.theme.bg),
            accent = hex_to_rgb_params(&self.theme.accent),
            session = session_name,
        )
    }

    /// Render the center segment: window list.
    fn render_center(&self, available_width: usize) -> String {
        let mut parts = Vec::new();

        for win in &self.windows {
            let label = if win.name.is_empty() {
                format!("{}", win.index + 1)
            } else {
                format!("{}:{}", win.index + 1, win.name)
            };

            if win.is_active {
                parts.push(format!(
                    "\x1b[48;2;{bg}m\x1b[38;2;{accent}m\x1b[1m {label} \x1b[0m",
                    bg = hex_to_rgb_params(&self.theme.bg),
                    accent = hex_to_rgb_params(&self.theme.accent),
                    label = label,
                ));
            } else {
                let fg_color = if win.has_activity {
                    &self.theme.fg
                } else {
                    &self.theme.inactive
                };
                parts.push(format!(
                    "\x1b[48;2;{bg}m\x1b[38;2;{fg}m {label} \x1b[0m",
                    bg = hex_to_rgb_params(&self.theme.bg),
                    fg = hex_to_rgb_params(fg_color),
                    label = label,
                ));
            }
        }

        parts.join("")
    }

    /// Render the right segment: pane info + clock.
    fn render_right(&self) -> String {
        let pane_info = if self.active_pane.title.is_empty() {
            &self.active_pane.command
        } else {
            &self.active_pane.title
        };

        // Get current time (HH:MM format).
        // In Wasm, we'd need the host to provide the time or use WASI clocks.
        let clock = "00:00"; // Placeholder — real impl queries host or WASI clock.

        format!(
            "\x1b[48;2;{bg}m\x1b[38;2;{fg}m {pane} \x1b[38;2;{inactive}m {clock} \x1b[0m",
            bg = hex_to_rgb_params(&self.theme.bg),
            fg = hex_to_rgb_params(&self.theme.fg),
            inactive = hex_to_rgb_params(&self.theme.inactive),
            pane = truncate(pane_info, 30),
            clock = clock,
        )
    }

    /// Render plugin-contributed segments.
    fn render_plugin_segments(&self) -> String {
        if self.plugin_segments.is_empty() {
            return String::new();
        }

        let mut parts = Vec::new();
        for seg in &self.plugin_segments {
            let fg = seg
                .fg
                .as_ref()
                .map(|c| hex_to_rgb_params(c))
                .unwrap_or_else(|| hex_to_rgb_params(&self.theme.fg));
            let bg = seg
                .bg
                .as_ref()
                .map(|c| hex_to_rgb_params(c))
                .unwrap_or_else(|| hex_to_rgb_params(&self.theme.bg));

            parts.push(format!(
                "\x1b[48;2;{bg}m\x1b[38;2;{fg}m {text} \x1b[0m",
                bg = bg,
                fg = fg,
                text = seg.text,
            ));
        }

        parts.join("")
    }

    /// Render the full status bar for a given width.
    ///
    /// Layout: [left] [plugin_segments] [center (fills remaining)] [right]
    fn render_full(&self, width: u16) -> String {
        let left = self.render_left();
        let right = self.render_right();
        let plugins = self.render_plugin_segments();

        // Calculate visible widths (excluding ANSI escapes).
        let left_width = visible_width(&left);
        let right_width = visible_width(&right);
        let plugin_width = visible_width(&plugins);

        let center_width = (width as usize)
            .saturating_sub(left_width)
            .saturating_sub(right_width)
            .saturating_sub(plugin_width);

        let center = self.render_center(center_width);
        let center_visible = visible_width(&center);

        // Pad center to fill available space.
        let padding = center_width.saturating_sub(center_visible);
        let pad_str = format!(
            "\x1b[48;2;{}m{}\x1b[0m",
            hex_to_rgb_params(&self.theme.bg),
            " ".repeat(padding)
        );

        format!("{}{}{}{}{}", left, plugins, center, pad_str, right)
    }

    /// Update a plugin-contributed segment.
    fn update_plugin_segment(&mut self, id: &str, text: &str, fg: Option<&str>, bg: Option<&str>) {
        if let Some(seg) = self.plugin_segments.iter_mut().find(|s| s.id == id) {
            seg.text = text.to_string();
            seg.fg = fg.map(|s| s.to_string());
            seg.bg = bg.map(|s| s.to_string());
        } else {
            self.plugin_segments.push(PluginSegment {
                id: id.to_string(),
                text: text.to_string(),
                fg: fg.map(|s| s.to_string()),
                bg: bg.map(|s| s.to_string()),
            });
        }
    }

    /// Remove a plugin-contributed segment.
    fn remove_plugin_segment(&mut self, id: &str) {
        self.plugin_segments.retain(|s| s.id != id);
    }
}

// ═══════════════════════════════════════════════
// WIT Plugin Interface Implementation
// ═══════════════════════════════════════════════

// In the full implementation, these functions are exported via WIT bindgen.
// For now, they are documented as the implementation plan.

/// `init(config-json: string) -> result<_, plugin-error>`
///
/// Called when the plugin is loaded. Reads initial state from the host.
/// ```ignore
/// fn init(config_json: &str) -> Result<(), PluginError> {
///     let config: serde_json::Value = serde_json::from_str(config_json)?;
///
///     // Query the host for current state.
///     let session = host::get_active_session()?;
///     let windows = host::list_windows()?;
///     let pane = host::get_active_pane()?;
///
///     // Initialize status bar state.
///     STATE.lock().session = SessionState {
///         id: session.id,
///         name: session.name,
///     };
///     // ... populate windows, active pane, etc.
///
///     // Read theme tokens.
///     let theme_bg = host::get_config("theme.status.bg")?;
///     // ... read other tokens ...
///
///     Ok(())
/// }
/// ```

/// `on-event(event-json: string) -> result<_, plugin-error>`
///
/// Called for each subscribed event. Updates internal state.
/// ```ignore
/// fn on_event(event_json: &str) -> Result<(), PluginError> {
///     STATE.lock().handle_event(event_json)?;
///     Ok(())
/// }
/// ```

/// `render-segment(id: string, width: u16) -> result<string, plugin-error>`
///
/// Called by the compositor to render a status bar segment.
/// The status bar plugin renders the full bar as a single segment.
/// ```ignore
/// fn render_segment(id: &str, width: u16) -> Result<String, PluginError> {
///     let state = STATE.lock();
///     Ok(state.render_full(width))
/// }
/// ```

/// `shutdown()`
///
/// Called when the plugin is unloaded. No cleanup needed.
/// ```ignore
/// fn shutdown() {
///     // No resources to clean up.
/// }
/// ```

// ═══════════════════════════════════════════════
// Helper Functions
// ═══════════════════════════════════════════════

/// Convert a hex color string to RGB parameters for ANSI escape.
/// "#89b4fa" -> "137;180;250"
fn hex_to_rgb_params(hex: &str) -> String {
    let hex = hex.trim_start_matches('#');
    if hex.len() != 6 {
        return "255;255;255".to_string(); // fallback to white
    }

    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(255);
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(255);
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(255);

    format!("{};{};{}", r, g, b)
}

/// Get the visible width of a string, ignoring ANSI escape sequences.
fn visible_width(s: &str) -> usize {
    let mut width = 0;
    let mut in_escape = false;

    for ch in s.chars() {
        if ch == '\x1b' {
            in_escape = true;
        } else if in_escape {
            if ch.is_ascii_alphabetic() {
                in_escape = false;
            }
        } else {
            width += 1;
        }
    }

    width
}

/// Truncate a string to a maximum visible width.
fn truncate(s: &str, max_width: usize) -> String {
    if s.len() <= max_width {
        return s.to_string();
    }
    if max_width <= 3 {
        return ".".repeat(max_width);
    }
    format!("{}...", &s[..max_width - 3])
}

// ═══════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hex_to_rgb_params() {
        assert_eq!(hex_to_rgb_params("#89b4fa"), "137;180;250");
        assert_eq!(hex_to_rgb_params("#000000"), "0;0;0");
        assert_eq!(hex_to_rgb_params("#ffffff"), "255;255;255");
        assert_eq!(hex_to_rgb_params("89b4fa"), "137;180;250"); // without #
    }

    #[test]
    fn test_visible_width() {
        assert_eq!(visible_width("hello"), 5);
        assert_eq!(visible_width("\x1b[31mred\x1b[0m"), 3);
        assert_eq!(visible_width(""), 0);
        assert_eq!(visible_width("\x1b[48;2;30;30;46m test \x1b[0m"), 6);
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world this is long", 10), "hello w...");
        assert_eq!(truncate("ab", 2), "ab");
        assert_eq!(truncate("abcdef", 3), "...");
    }

    #[test]
    fn test_initial_state() {
        let bar = StatusBar::new();
        assert!(bar.session.name.is_empty());
        assert!(bar.windows.is_empty());
        assert!(bar.plugin_segments.is_empty());
    }

    #[test]
    fn test_handle_session_event() {
        let mut bar = StatusBar::new();
        bar.handle_event(r#"{"type": "session.created", "session_id": "s-1", "name": "work"}"#)
            .unwrap();
        assert_eq!(bar.session.name, "work");
        assert_eq!(bar.session.id, "s-1");
    }

    #[test]
    fn test_handle_window_events() {
        let mut bar = StatusBar::new();

        bar.handle_event(
            r#"{"type": "window.created", "window_id": "w-1", "session_id": "s-1", "title": "editor"}"#,
        )
        .unwrap();
        assert_eq!(bar.windows.len(), 1);
        assert_eq!(bar.windows[0].name, "editor");
        assert_eq!(bar.windows[0].index, 0);

        bar.handle_event(
            r#"{"type": "window.created", "window_id": "w-2", "session_id": "s-1", "title": "tests"}"#,
        )
        .unwrap();
        assert_eq!(bar.windows.len(), 2);

        bar.handle_event(
            r#"{"type": "window.activated", "window_id": "w-1", "session_id": "s-1"}"#,
        )
        .unwrap();
        assert!(bar.windows[0].is_active);
        assert!(!bar.windows[1].is_active);

        bar.handle_event(
            r#"{"type": "window.renamed", "window_id": "w-1", "old_title": "editor", "new_title": "nvim"}"#,
        )
        .unwrap();
        assert_eq!(bar.windows[0].name, "nvim");

        bar.handle_event(
            r#"{"type": "window.killed", "window_id": "w-1", "session_id": "s-1"}"#,
        )
        .unwrap();
        assert_eq!(bar.windows.len(), 1);
        assert_eq!(bar.windows[0].id, "w-2");
        assert_eq!(bar.windows[0].index, 0); // re-indexed
    }

    #[test]
    fn test_handle_pane_events() {
        let mut bar = StatusBar::new();

        bar.handle_event(
            r#"{"type": "pane.focused", "pane_id": "p-1", "window_id": "w-1"}"#,
        )
        .unwrap();
        assert_eq!(bar.active_pane.id, "p-1");

        bar.handle_event(
            r#"{"type": "pane.title_changed", "pane_id": "p-1", "old_title": "", "new_title": "nvim src/main.rs"}"#,
        )
        .unwrap();
        assert_eq!(bar.active_pane.title, "nvim src/main.rs");
    }

    #[test]
    fn test_plugin_segment_management() {
        let mut bar = StatusBar::new();

        bar.update_plugin_segment("git_branch", " main", Some("#a6e3a1"), None);
        assert_eq!(bar.plugin_segments.len(), 1);
        assert_eq!(bar.plugin_segments[0].text, " main");

        // Update existing segment
        bar.update_plugin_segment("git_branch", " feat/plugins", Some("#a6e3a1"), None);
        assert_eq!(bar.plugin_segments.len(), 1); // still 1
        assert_eq!(bar.plugin_segments[0].text, " feat/plugins");

        // Add another segment
        bar.update_plugin_segment("k8s_context", "prod", Some("#f38ba8"), None);
        assert_eq!(bar.plugin_segments.len(), 2);

        // Remove segment
        bar.remove_plugin_segment("git_branch");
        assert_eq!(bar.plugin_segments.len(), 1);
        assert_eq!(bar.plugin_segments[0].id, "k8s_context");
    }

    #[test]
    fn test_render_left_segment() {
        let mut bar = StatusBar::new();
        bar.session.name = "work".to_string();

        let rendered = bar.render_left();
        assert!(rendered.contains("work"));
    }

    #[test]
    fn test_render_center_with_windows() {
        let mut bar = StatusBar::new();
        bar.windows = vec![
            WindowState {
                id: "w-1".to_string(),
                name: "editor".to_string(),
                index: 0,
                is_active: true,
                has_activity: false,
            },
            WindowState {
                id: "w-2".to_string(),
                name: "tests".to_string(),
                index: 1,
                is_active: false,
                has_activity: false,
            },
        ];

        let rendered = bar.render_center(80);
        assert!(rendered.contains("1:editor"));
        assert!(rendered.contains("2:tests"));
    }

    #[test]
    fn test_render_full_bar() {
        let mut bar = StatusBar::new();
        bar.session.name = "shux".to_string();
        bar.active_pane.title = "nvim".to_string();
        bar.windows = vec![
            WindowState {
                id: "w-1".to_string(),
                name: "editor".to_string(),
                index: 0,
                is_active: true,
                has_activity: false,
            },
        ];

        let rendered = bar.render_full(120);
        // The rendered string should contain session name, window, and pane info.
        assert!(rendered.contains("shux"));
        assert!(rendered.contains("1:editor"));
        assert!(rendered.contains("nvim"));
    }

    #[test]
    fn test_render_default_session_name() {
        let bar = StatusBar::new();
        let rendered = bar.render_left();
        assert!(rendered.contains("default"));
    }
}
```

### Step 4: Add to workspace and configure bundled loading

Add the plugin to the Cargo workspace and configure the daemon to load it at startup.

```rust
// In Cargo.toml (workspace root), add to members:
// "plugins/shux-status-bar"

// In the daemon startup code (crates/shux-plugin/src/lib.rs or daemon bootstrap):
//
// The bundled status bar plugin is loaded differently from external plugins:
// 1. Its .wasm is compiled into the shux binary (via include_bytes! or
//    loaded from an adjacent file path).
// 2. It is always enabled by default.
// 3. It can be disabled via config: `[plugins."io.shux.status-bar"] enabled = false`
// 4. It can be replaced by a user-provided status bar plugin that
//    registers the same segments.
//
// ```ignore
// pub fn register_bundled_plugins(plugin_host: &mut PluginHost) {
//     // Status bar
//     plugin_host.register_bundled(
//         BundledPlugin {
//             id: "io.shux.status-bar",
//             wasm_bytes: include_bytes!("../../../plugins/shux-status-bar/target/wasm32-wasip2/release/shux_status_bar.wasm"),
//             manifest: include_str!("../../../plugins/shux-status-bar/plugin.toml"),
//             default_enabled: true,
//         }
//     );
// }
// ```
```

### Step 5: Implement the config flag to switch between hardcoded and plugin status bar

Provide a config option to switch back to the task-026 hardcoded status bar if the plugin has issues.

```rust
// In shux config:
//
// [status_bar]
// # "plugin" (default) uses the bundled shux-status-bar plugin.
// # "builtin" uses the hardcoded status bar from task 026.
// mode = "plugin"
//
// The render compositor checks this config:
// - If mode == "plugin": delegate to the status bar plugin's render-segment.
// - If mode == "builtin": use the hardcoded status bar renderer.
// - If the plugin fails to render: fall back to builtin automatically.
```

---

## Verification

### Functional

```bash
# Build the status bar plugin crate
cargo build -p shux-status-bar 2>&1 | tail -5

# Build the full workspace
cargo build --workspace 2>&1 | tail -5

# Verify no clippy warnings
cargo clippy -p shux-status-bar -- -D warnings

# Verify the Wasm target compiles (if wasm toolchain is set up)
# cargo build -p shux-status-bar --target wasm32-wasip2 --release
```

### Tests

```bash
# Run status bar plugin tests
cargo nextest run -p shux-status-bar

# Run specific tests
cargo nextest run -p shux-status-bar test_handle_session_event
cargo nextest run -p shux-status-bar test_handle_window_events
cargo nextest run -p shux-status-bar test_render_full_bar
cargo nextest run -p shux-status-bar test_plugin_segment_management
cargo nextest run -p shux-status-bar test_handles_plugin_event_contracts

# Run all workspace tests
cargo nextest run --workspace
```

---

## Completion Criteria

- [ ] `plugin.toml` declares correct metadata, permissions (events only), and segments
- [ ] Plugin subscribes to: session.*, window.*, pane.focused, pane.title_changed, theme.changed
- [ ] Left segment renders: session name (defaults to "default" when empty)
- [ ] Center segment renders: numbered window list with active window highlighted
- [ ] Right segment renders: active pane info (title or command) + clock
- [ ] Plugin-contributed segments: other plugins add segments via set-status-segment
- [ ] Plugin-event contracts update segments/badges (e.g., `context.detected`, tunnel/diagnostic updates)
- [ ] Segment ordering: left | plugin_segments | center (fills remaining) | right
- [ ] Theme-aware: reads theme tokens for bg, fg, accent, inactive colors
- [ ] ANSI escape sequences correctly applied for truecolor rendering
- [ ] `visible_width` correctly strips ANSI escapes for width calculation
- [ ] Config flag to switch between plugin and hardcoded status bar
- [ ] Plugin is registered as bundled in the daemon startup
- [ ] All event handlers update state correctly (session, window, pane events)
- [ ] Window re-indexing after deletion
- [ ] All tests pass
- [ ] No clippy warnings

---

## Commit Message

```
feat(plugin): implement bundled status bar plugin replacing hardcoded bar

- Wasm plugin proving the plugin system can implement core UI
- Renders session name, window list, active pane info, and clock
- Subscribes to session/window/pane/theme events for live updates
- Plugin-contributed segments: other plugins add custom segments
- Theme-aware rendering with ANSI truecolor escape sequences
- Config flag to switch between plugin and hardcoded (task 026) status bar
- Registered as bundled plugin loaded at daemon startup
```

---

## Session Protocol

1. **Before starting:** Read tasks 046 (overlay system) and 047 (inter-plugin event bus) -- the status bar consumes inter-plugin events for plugin-contributed segments. Read task 026 (hardcoded status bar) to understand what is being replaced. Read PRD section 7.7 for bundled plugin requirements. Review the status bar references in the use cases document (Smart Context, SSH Tunnels, etc. all contribute segments).
2. **During:** Implement in order: plugin.toml (Step 1) -> Cargo.toml (Step 2) -> plugin implementation (Step 3) -> bundled loading (Step 4) -> config flag (Step 5). Run `cargo check` after each step. Run tests after Step 3. The Wasm compilation (Step 4) requires the wasm32-wasip2 target -- if not available, the plugin can be tested as a native library first.
3. **After:** Run the full verification suite. Verify the status bar renders correctly with multiple windows. Verify plugin-contributed segments appear in the correct position. Verify theme token changes update colors. Update `docs/PROGRESS.md` (mark 048 done). Update `CLAUDE.md` Learnings with insights about Wasm plugin development and the bundled plugin pattern.
