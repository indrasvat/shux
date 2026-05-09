# 046 — Overlay System

**Status:** Pending
**Depends On:** 041
**Parallelizable With:** 045

---

## Problem

Plugins need to render UI on top of pane content -- confirmation dialogs, search interfaces, inline help, notification badges, replay controls, and more. The overlay system provides a per-pane overlay stack where multiple plugins can display content simultaneously with well-defined z-ordering, input routing, and compositing rules.

Without overlays, plugins are limited to status bar segments and pane tags. The overlay system is what makes interactive plugins like Danger Zone (confirmation dialogs), Command Palette (fuzzy search), and Session Replay (playback controls) possible. It is the foundation for rich, interactive plugin UX.

The overlay system must handle: per-pane overlay stacks with z-ordering, show/hide/replace semantics, input routing (topmost first, with fall-through), rendering (all visible overlays composited, topmost last), and integration with the render compositor to layer overlays on top of pane content.

## PRD Reference

- **section 7.2b** — Overlay z-ordering: stack ordering, render-overlay for all visible overlays, input routing with fall-through
- **section 7.5** — WIT functions: `show-overlay(pane-id)`, `hide-overlay(pane-id)`, `render-overlay(pane-id, width, height)`, `on-overlay-input(pane-id, key-event-json)`
- **section 7.2** — Extension points: "Pane overlays: Render text/UI above pane content"
- **section 4.4** — `LayoutTree`: "Plugins can register overlay layers that render above the base layout"

---

## Files to Create

- `crates/shux-ui/src/overlay.rs` — Overlay stack manager: per-pane overlay stacks, z-ordering, show/hide/replace logic, render compositing, input routing
- `crates/shux-plugin/src/overlay_bridge.rs` — Bridge between the plugin host and the overlay system: translates plugin overlay requests into overlay stack operations, invokes plugin render/input callbacks

## Files to Modify

- `crates/shux-ui/src/lib.rs` — Add `pub mod overlay;`
- `crates/shux-plugin/src/lib.rs` — Add `pub mod overlay_bridge;`
- `crates/shux-ui/Cargo.toml` — Add dependencies if needed
- `crates/shux-plugin/Cargo.toml` — Add dependencies if needed

---

## Execution Steps

### Step 1: Define overlay types in `crates/shux-ui/src/overlay.rs`

Define the core types for the overlay system.

```rust
use std::collections::HashMap;

use tracing::{debug, info, warn};

/// Identifies a plugin in the overlay system.
pub type PluginId = String;

/// Identifies a pane.
pub type PaneId = String;

/// A single overlay entry in the stack.
#[derive(Debug, Clone)]
pub struct OverlayEntry {
    /// The plugin that owns this overlay.
    pub plugin_id: PluginId,

    /// Whether this overlay is currently visible.
    /// Hidden overlays remain in the stack but are not rendered.
    pub visible: bool,

    /// The last rendered content (ANSI-styled text).
    /// Updated each time render-overlay is called.
    pub last_render: Option<String>,

    /// The width used in the last render call.
    pub last_width: u16,

    /// The height used in the last render call.
    pub last_height: u16,
}

/// The overlay stack for a single pane.
///
/// Maintains an ordered list of overlays. The order determines z-ordering:
/// later entries are on top (rendered last, receive input first).
///
/// Z-ordering rules (from PRD section 7.2b):
/// 1. Overlays are stacked in the order show-overlay was called (most recent on top).
/// 2. The topmost overlay receives on-overlay-input first.
/// 3. render-overlay is called for ALL visible overlays; host composites them.
/// 4. hide-overlay removes only that plugin's overlay.
/// 5. If a plugin calls show-overlay while already having one, it replaces in-place.
#[derive(Debug, Default)]
pub struct OverlayStack {
    /// Ordered list of overlays. Last entry is topmost (highest z-order).
    entries: Vec<OverlayEntry>,
}

impl OverlayStack {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Show an overlay for a plugin.
    ///
    /// If the plugin already has an overlay on this pane, replace it
    /// at the same stack position (rule 5). Otherwise, push it on top.
    pub fn show(&mut self, plugin_id: &str) {
        // Check if the plugin already has an overlay.
        if let Some(entry) = self.entries.iter_mut().find(|e| e.plugin_id == plugin_id) {
            // Replace existing overlay at the same position.
            entry.visible = true;
            entry.last_render = None; // Clear stale render
            debug!(
                plugin_id = plugin_id,
                "Overlay replaced at existing stack position"
            );
        } else {
            // New overlay: push on top.
            self.entries.push(OverlayEntry {
                plugin_id: plugin_id.to_string(),
                visible: true,
                last_render: None,
                last_width: 0,
                last_height: 0,
            });
            info!(
                plugin_id = plugin_id,
                stack_size = self.entries.len(),
                "Overlay pushed onto stack"
            );
        }
    }

    /// Hide an overlay for a plugin.
    /// Removes the overlay from the stack entirely.
    pub fn hide(&mut self, plugin_id: &str) -> bool {
        let initial_len = self.entries.len();
        self.entries.retain(|e| e.plugin_id != plugin_id);
        let removed = self.entries.len() < initial_len;

        if removed {
            info!(
                plugin_id = plugin_id,
                stack_size = self.entries.len(),
                "Overlay removed from stack"
            );
        } else {
            debug!(
                plugin_id = plugin_id,
                "No overlay found to hide"
            );
        }

        removed
    }

    /// Check if a plugin has a visible overlay on this pane.
    pub fn has_overlay(&self, plugin_id: &str) -> bool {
        self.entries.iter().any(|e| e.plugin_id == plugin_id && e.visible)
    }

    /// Check if the stack has any visible overlays.
    pub fn has_visible_overlays(&self) -> bool {
        self.entries.iter().any(|e| e.visible)
    }

    /// Get the topmost visible overlay's plugin ID.
    /// Used for input routing: this plugin gets first crack at input.
    pub fn topmost_plugin(&self) -> Option<&str> {
        self.entries
            .iter()
            .rev()
            .find(|e| e.visible)
            .map(|e| e.plugin_id.as_str())
    }

    /// Get all visible overlays in render order (bottom to top).
    /// The compositor renders them in this order, so the last one
    /// is visually on top.
    pub fn visible_overlays(&self) -> Vec<&OverlayEntry> {
        self.entries.iter().filter(|e| e.visible).collect()
    }

    /// Get all visible overlay plugin IDs in input routing order
    /// (top to bottom). The first plugin gets input first.
    pub fn input_routing_order(&self) -> Vec<&str> {
        self.entries
            .iter()
            .rev()
            .filter(|e| e.visible)
            .map(|e| e.plugin_id.as_str())
            .collect()
    }

    /// Update the rendered content for an overlay.
    pub fn update_render(
        &mut self,
        plugin_id: &str,
        content: Option<String>,
        width: u16,
        height: u16,
    ) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.plugin_id == plugin_id) {
            entry.last_render = content;
            entry.last_width = width;
            entry.last_height = height;
        }
    }

    /// Remove all overlays from a plugin (e.g., plugin unloaded).
    pub fn remove_plugin(&mut self, plugin_id: &str) {
        self.entries.retain(|e| e.plugin_id != plugin_id);
    }

    /// Get the number of visible overlays.
    pub fn visible_count(&self) -> usize {
        self.entries.iter().filter(|e| e.visible).count()
    }

    /// Get the total number of overlays (visible + hidden).
    pub fn total_count(&self) -> usize {
        self.entries.len()
    }
}
```

### Step 2: Implement the overlay manager

The overlay manager maintains per-pane overlay stacks and provides the API used by the compositor and plugin host.

```rust
/// Global overlay manager. Maintains per-pane overlay stacks and
/// coordinates rendering and input routing across all panes.
pub struct OverlayManager {
    /// Per-pane overlay stacks.
    stacks: HashMap<PaneId, OverlayStack>,
}

impl OverlayManager {
    pub fn new() -> Self {
        Self {
            stacks: HashMap::new(),
        }
    }

    /// Show a plugin overlay on a pane.
    pub fn show_overlay(&mut self, pane_id: &str, plugin_id: &str) {
        let stack = self
            .stacks
            .entry(pane_id.to_string())
            .or_insert_with(OverlayStack::new);
        stack.show(plugin_id);
    }

    /// Hide a plugin overlay on a pane.
    pub fn hide_overlay(&mut self, pane_id: &str, plugin_id: &str) -> bool {
        if let Some(stack) = self.stacks.get_mut(pane_id) {
            let removed = stack.hide(plugin_id);
            // Clean up empty stacks.
            if stack.total_count() == 0 {
                self.stacks.remove(pane_id);
            }
            removed
        } else {
            false
        }
    }

    /// Check if a pane has any visible overlays.
    pub fn pane_has_overlays(&self, pane_id: &str) -> bool {
        self.stacks
            .get(pane_id)
            .map(|s| s.has_visible_overlays())
            .unwrap_or(false)
    }

    /// Get the overlay stack for a pane (for rendering).
    pub fn get_stack(&self, pane_id: &str) -> Option<&OverlayStack> {
        self.stacks.get(pane_id)
    }

    /// Get a mutable overlay stack for a pane (for updating renders).
    pub fn get_stack_mut(&mut self, pane_id: &str) -> Option<&mut OverlayStack> {
        self.stacks.get_mut(pane_id)
    }

    /// Get the input routing order for a pane.
    /// Returns plugin IDs from topmost to bottommost.
    pub fn input_routing_order(&self, pane_id: &str) -> Vec<&str> {
        self.stacks
            .get(pane_id)
            .map(|s| s.input_routing_order())
            .unwrap_or_default()
    }

    /// Remove all overlays from a plugin across all panes.
    /// Called when a plugin is disabled or unloaded.
    pub fn remove_plugin(&mut self, plugin_id: &str) {
        for stack in self.stacks.values_mut() {
            stack.remove_plugin(plugin_id);
        }
        // Clean up empty stacks.
        self.stacks.retain(|_, stack| stack.total_count() > 0);

        info!(
            plugin_id = plugin_id,
            "All overlays removed for plugin"
        );
    }

    /// Remove all overlays for a pane.
    /// Called when a pane is closed.
    pub fn remove_pane(&mut self, pane_id: &str) {
        self.stacks.remove(pane_id);
    }

    /// Get all panes that have visible overlays from a specific plugin.
    pub fn panes_with_plugin_overlay(&self, plugin_id: &str) -> Vec<&str> {
        self.stacks
            .iter()
            .filter(|(_, stack)| stack.has_overlay(plugin_id))
            .map(|(pane_id, _)| pane_id.as_str())
            .collect()
    }
}
```

### Step 3: Implement the render compositing integration

Define how overlays are composited on top of pane content during rendering.

```rust
/// Overlay compositing for the render pipeline.
///
/// After the compositor renders the pane's terminal content, it
/// calls this function to overlay plugin content on top.
///
/// The compositing process:
/// 1. Get the overlay stack for the pane.
/// 2. For each visible overlay (bottom to top):
///    a. Call the plugin's render-overlay(pane_id, width, height).
///    b. The plugin returns an Option<String> of ANSI-styled text.
///    c. If Some, overlay the text onto the pane's rendered output.
///    d. If None, the overlay is temporarily invisible (e.g., nothing to show).
/// 3. Later overlays (higher z-order) paint over earlier ones.
///
/// The overlay text is ANSI-styled and positioned relative to the pane.
/// Plugins are responsible for rendering within the given width/height.
/// The host does not clip or scroll overlay content -- it trusts the
/// plugin to respect the dimensions.
pub struct OverlayCompositor;

impl OverlayCompositor {
    /// Composite overlays onto a pane's rendered output.
    ///
    /// `base_content` is the pane's rendered terminal content as a grid
    /// of styled cells. The overlays are painted on top.
    ///
    /// Returns the composited output ready for the terminal.
    pub fn composite(
        pane_id: &str,
        base_lines: &[String],
        overlay_manager: &OverlayManager,
        width: u16,
        height: u16,
    ) -> Vec<String> {
        let stack = match overlay_manager.get_stack(pane_id) {
            Some(s) if s.has_visible_overlays() => s,
            _ => return base_lines.to_vec(), // No overlays, return base content.
        };

        let mut output = base_lines.to_vec();

        // Ensure output has enough lines.
        while output.len() < height as usize {
            output.push(" ".repeat(width as usize));
        }

        // Render each visible overlay bottom-to-top.
        for entry in stack.visible_overlays() {
            if let Some(ref rendered) = entry.last_render {
                // Parse the overlay content into lines and paint
                // them onto the output buffer.
                let overlay_lines: Vec<&str> = rendered.lines().collect();

                // Center the overlay vertically and horizontally.
                let overlay_height = overlay_lines.len().min(height as usize);
                let overlay_width = overlay_lines
                    .iter()
                    .map(|l| strip_ansi_len(l))
                    .max()
                    .unwrap_or(0)
                    .min(width as usize);

                let y_offset = (height as usize).saturating_sub(overlay_height) / 2;
                let x_offset = (width as usize).saturating_sub(overlay_width) / 2;

                for (i, overlay_line) in overlay_lines.iter().enumerate() {
                    let target_y = y_offset + i;
                    if target_y < output.len() {
                        // Replace the portion of the output line with the overlay line.
                        // In a full implementation, this would handle ANSI escapes properly.
                        output[target_y] = compose_line(
                            &output[target_y],
                            overlay_line,
                            x_offset,
                            width as usize,
                        );
                    }
                }
            }
        }

        output
    }
}

/// Compose an overlay line onto a base line at a given x offset.
///
/// In the full implementation, this handles ANSI escape sequences
/// properly. For now, it does a simple character-level replacement.
fn compose_line(base: &str, overlay: &str, x_offset: usize, _max_width: usize) -> String {
    // Simple implementation: replace characters starting at x_offset.
    // The real implementation would parse ANSI sequences in both
    // the base and overlay to properly handle colors and styles.
    let mut result = String::new();
    let base_chars: Vec<char> = base.chars().collect();
    let overlay_chars: Vec<char> = overlay.chars().collect();

    for i in 0..base_chars.len().max(x_offset + overlay_chars.len()) {
        if i >= x_offset && i < x_offset + overlay_chars.len() {
            result.push(overlay_chars[i - x_offset]);
        } else if i < base_chars.len() {
            result.push(base_chars[i]);
        } else {
            result.push(' ');
        }
    }

    result
}

/// Get the visible length of a string, ignoring ANSI escape sequences.
fn strip_ansi_len(s: &str) -> usize {
    // Simple heuristic: count non-escape characters.
    // The real implementation would use a proper ANSI parser.
    let mut len = 0;
    let mut in_escape = false;

    for ch in s.chars() {
        if ch == '\x1b' {
            in_escape = true;
        } else if in_escape {
            if ch.is_ascii_alphabetic() {
                in_escape = false;
            }
        } else {
            len += 1;
        }
    }

    len
}
```

### Step 4: Implement input routing for overlays

Define the input routing logic that sends keystrokes to overlays before the pane.

```rust
/// Result of routing input through the overlay stack.
#[derive(Debug)]
pub enum InputRouteResult {
    /// A plugin consumed the input. Do not pass to the pane.
    Consumed {
        plugin_id: String,
    },

    /// No plugin consumed the input. Pass to the pane.
    PassThrough,

    /// An error occurred while routing input.
    Error {
        plugin_id: String,
        error: String,
    },
}

/// Input routing for overlays.
///
/// When a keypress occurs on a pane that has visible overlays:
/// 1. The topmost overlay gets on-overlay-input first.
/// 2. If it returns true (consumed), stop -- the key is not passed further.
/// 3. If it returns false, pass to the next overlay.
/// 4. If no overlay consumes the key, pass it to the pane.
///
/// This is implemented in the overlay bridge (plugin side), which
/// calls each plugin's on-overlay-input callback in order.
///
/// ```ignore
/// async fn route_input(
///     &self,
///     pane_id: &str,
///     key_event_json: &str,
/// ) -> InputRouteResult {
///     let routing_order = self.overlay_manager.input_routing_order(pane_id);
///
///     for plugin_id in routing_order {
///         match self.invoke_overlay_input(plugin_id, pane_id, key_event_json).await {
///             Ok(true) => {
///                 // Plugin consumed the input.
///                 return InputRouteResult::Consumed {
///                     plugin_id: plugin_id.to_string(),
///                 };
///             }
///             Ok(false) => {
///                 // Plugin did not consume. Pass to next.
///                 continue;
///             }
///             Err(e) => {
///                 // Plugin error. Log and continue to next overlay.
///                 warn!(
///                     plugin_id = plugin_id,
///                     error = %e,
///                     "Error in on-overlay-input, skipping"
///                 );
///                 continue;
///             }
///         }
///     }
///
///     InputRouteResult::PassThrough
/// }
/// ```
```

### Step 5: Implement the overlay bridge in `crates/shux-plugin/src/overlay_bridge.rs`

The bridge connects the plugin host to the overlay system, translating plugin calls into overlay operations and invoking render/input callbacks.

```rust
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn, error};

use crate::overlay::{OverlayManager, InputRouteResult};

/// Trait for invoking plugin overlay callbacks.
/// The plugin host (Wasm or process) implements this.
#[async_trait::async_trait]
pub trait OverlayPluginInvoker: Send + Sync {
    /// Call a plugin's render-overlay function.
    ///
    /// WIT: `render-overlay: func(pane-id: string, width: u16, height: u16) -> result<option<string>, plugin-error>`
    ///
    /// Returns the ANSI-styled overlay content, or None to hide temporarily.
    async fn invoke_render_overlay(
        &self,
        plugin_id: &str,
        pane_id: &str,
        width: u16,
        height: u16,
    ) -> Result<Option<String>, OverlayBridgeError>;

    /// Call a plugin's on-overlay-input function.
    ///
    /// WIT: `on-overlay-input: func(pane-id: string, key-event-json: string) -> result<bool, plugin-error>`
    ///
    /// Returns true if the plugin consumed the input, false to pass through.
    async fn invoke_overlay_input(
        &self,
        plugin_id: &str,
        pane_id: &str,
        key_event_json: &str,
    ) -> Result<bool, OverlayBridgeError>;
}

/// Bridge between plugin host and overlay system.
pub struct OverlayBridge {
    overlay_manager: Arc<RwLock<OverlayManager>>,
    invoker: Arc<dyn OverlayPluginInvoker>,
}

impl OverlayBridge {
    pub fn new(
        overlay_manager: Arc<RwLock<OverlayManager>>,
        invoker: Arc<dyn OverlayPluginInvoker>,
    ) -> Self {
        Self {
            overlay_manager,
            invoker,
        }
    }

    /// Handle a plugin's show-overlay request.
    pub async fn handle_show_overlay(
        &self,
        plugin_id: &str,
        pane_id: &str,
    ) -> Result<(), OverlayBridgeError> {
        let mut manager = self.overlay_manager.write().await;
        manager.show_overlay(pane_id, plugin_id);
        Ok(())
    }

    /// Handle a plugin's hide-overlay request.
    pub async fn handle_hide_overlay(
        &self,
        plugin_id: &str,
        pane_id: &str,
    ) -> Result<(), OverlayBridgeError> {
        let mut manager = self.overlay_manager.write().await;
        if !manager.hide_overlay(pane_id, plugin_id) {
            warn!(
                plugin_id = plugin_id,
                pane_id = pane_id,
                "Plugin tried to hide overlay that doesn't exist"
            );
        }
        Ok(())
    }

    /// Render all visible overlays for a pane.
    ///
    /// Called by the compositor during the render cycle.
    /// Invokes render-overlay on each plugin with a visible overlay.
    pub async fn render_overlays(
        &self,
        pane_id: &str,
        width: u16,
        height: u16,
    ) -> Result<(), OverlayBridgeError> {
        let plugins: Vec<String> = {
            let manager = self.overlay_manager.read().await;
            match manager.get_stack(pane_id) {
                Some(stack) => stack
                    .visible_overlays()
                    .iter()
                    .map(|e| e.plugin_id.clone())
                    .collect(),
                None => return Ok(()),
            }
        };

        for plugin_id in &plugins {
            match self
                .invoker
                .invoke_render_overlay(plugin_id, pane_id, width, height)
                .await
            {
                Ok(content) => {
                    let mut manager = self.overlay_manager.write().await;
                    if let Some(stack) = manager.get_stack_mut(pane_id) {
                        stack.update_render(plugin_id, content, width, height);
                    }
                }
                Err(e) => {
                    warn!(
                        plugin_id = %plugin_id,
                        pane_id = pane_id,
                        error = %e,
                        "Failed to render overlay, skipping"
                    );
                }
            }
        }

        Ok(())
    }

    /// Route a key event through the overlay stack for a pane.
    ///
    /// Returns the routing result indicating whether the input was
    /// consumed by an overlay or should be passed to the pane.
    pub async fn route_input(
        &self,
        pane_id: &str,
        key_event_json: &str,
    ) -> InputRouteResult {
        let routing_order: Vec<String> = {
            let manager = self.overlay_manager.read().await;
            manager
                .input_routing_order(pane_id)
                .iter()
                .map(|s| s.to_string())
                .collect()
        };

        if routing_order.is_empty() {
            return InputRouteResult::PassThrough;
        }

        for plugin_id in &routing_order {
            match self
                .invoker
                .invoke_overlay_input(plugin_id, pane_id, key_event_json)
                .await
            {
                Ok(true) => {
                    debug!(
                        plugin_id = %plugin_id,
                        pane_id = pane_id,
                        "Overlay consumed input"
                    );
                    return InputRouteResult::Consumed {
                        plugin_id: plugin_id.clone(),
                    };
                }
                Ok(false) => {
                    continue;
                }
                Err(e) => {
                    warn!(
                        plugin_id = %plugin_id,
                        pane_id = pane_id,
                        error = %e,
                        "Error in on-overlay-input, skipping to next overlay"
                    );
                    continue;
                }
            }
        }

        InputRouteResult::PassThrough
    }

    /// Handle plugin unload: remove all overlays for this plugin.
    pub async fn handle_plugin_unload(&self, plugin_id: &str) {
        let mut manager = self.overlay_manager.write().await;
        manager.remove_plugin(plugin_id);
    }

    /// Handle pane close: remove all overlays for this pane.
    pub async fn handle_pane_close(&self, pane_id: &str) {
        let mut manager = self.overlay_manager.write().await;
        manager.remove_pane(pane_id);
    }
}

/// Errors from the overlay bridge.
#[derive(Debug, thiserror::Error)]
pub enum OverlayBridgeError {
    #[error("plugin error: {0}")]
    PluginError(String),

    #[error("pane not found: {0}")]
    PaneNotFound(String),

    #[error("plugin not loaded: {0}")]
    PluginNotLoaded(String),

    #[error("render timeout")]
    RenderTimeout,
}
```

### Step 6: Write tests

```rust
#[cfg(test)]
mod overlay_stack_tests {
    use super::*;

    #[test]
    fn test_show_new_overlay() {
        let mut stack = OverlayStack::new();
        stack.show("plugin-a");

        assert_eq!(stack.total_count(), 1);
        assert_eq!(stack.visible_count(), 1);
        assert_eq!(stack.topmost_plugin(), Some("plugin-a"));
    }

    #[test]
    fn test_z_ordering_most_recent_on_top() {
        let mut stack = OverlayStack::new();
        stack.show("plugin-a");
        stack.show("plugin-b");
        stack.show("plugin-c");

        // Most recent (plugin-c) is topmost
        assert_eq!(stack.topmost_plugin(), Some("plugin-c"));

        // Input routing order is top to bottom
        let order = stack.input_routing_order();
        assert_eq!(order, vec!["plugin-c", "plugin-b", "plugin-a"]);

        // Render order is bottom to top
        let render = stack.visible_overlays();
        assert_eq!(render[0].plugin_id, "plugin-a");
        assert_eq!(render[1].plugin_id, "plugin-b");
        assert_eq!(render[2].plugin_id, "plugin-c");
    }

    #[test]
    fn test_show_existing_replaces_in_place() {
        let mut stack = OverlayStack::new();
        stack.show("plugin-a");
        stack.show("plugin-b");
        stack.show("plugin-a"); // re-show: replace in place, NOT move to top

        // plugin-a stays at its original position
        let order = stack.input_routing_order();
        assert_eq!(order, vec!["plugin-b", "plugin-a"]);

        // Still only 2 entries, not 3
        assert_eq!(stack.total_count(), 2);
    }

    #[test]
    fn test_hide_removes_from_stack() {
        let mut stack = OverlayStack::new();
        stack.show("plugin-a");
        stack.show("plugin-b");

        assert!(stack.hide("plugin-a"));
        assert_eq!(stack.total_count(), 1);
        assert_eq!(stack.topmost_plugin(), Some("plugin-b"));
    }

    #[test]
    fn test_hide_nonexistent_returns_false() {
        let mut stack = OverlayStack::new();
        assert!(!stack.hide("plugin-a"));
    }

    #[test]
    fn test_remove_plugin_clears_all() {
        let mut stack = OverlayStack::new();
        stack.show("plugin-a");
        stack.show("plugin-b");

        stack.remove_plugin("plugin-a");
        assert_eq!(stack.total_count(), 1);
        assert!(!stack.has_overlay("plugin-a"));
        assert!(stack.has_overlay("plugin-b"));
    }

    #[test]
    fn test_empty_stack_no_overlays() {
        let stack = OverlayStack::new();
        assert!(!stack.has_visible_overlays());
        assert!(stack.topmost_plugin().is_none());
        assert!(stack.input_routing_order().is_empty());
        assert!(stack.visible_overlays().is_empty());
    }

    #[test]
    fn test_update_render() {
        let mut stack = OverlayStack::new();
        stack.show("plugin-a");
        stack.update_render("plugin-a", Some("Hello, overlay!".to_string()), 80, 24);

        let overlays = stack.visible_overlays();
        assert_eq!(overlays[0].last_render.as_deref(), Some("Hello, overlay!"));
        assert_eq!(overlays[0].last_width, 80);
        assert_eq!(overlays[0].last_height, 24);
    }
}

#[cfg(test)]
mod overlay_manager_tests {
    use super::*;

    #[test]
    fn test_multi_pane_overlays() {
        let mut mgr = OverlayManager::new();

        mgr.show_overlay("pane-1", "plugin-a");
        mgr.show_overlay("pane-2", "plugin-a");
        mgr.show_overlay("pane-2", "plugin-b");

        assert!(mgr.pane_has_overlays("pane-1"));
        assert!(mgr.pane_has_overlays("pane-2"));
        assert!(!mgr.pane_has_overlays("pane-3"));

        let panes = mgr.panes_with_plugin_overlay("plugin-a");
        assert_eq!(panes.len(), 2);
    }

    #[test]
    fn test_remove_plugin_from_all_panes() {
        let mut mgr = OverlayManager::new();

        mgr.show_overlay("pane-1", "plugin-a");
        mgr.show_overlay("pane-2", "plugin-a");

        mgr.remove_plugin("plugin-a");

        assert!(!mgr.pane_has_overlays("pane-1"));
        assert!(!mgr.pane_has_overlays("pane-2"));
    }

    #[test]
    fn test_remove_pane() {
        let mut mgr = OverlayManager::new();

        mgr.show_overlay("pane-1", "plugin-a");
        mgr.show_overlay("pane-1", "plugin-b");

        mgr.remove_pane("pane-1");
        assert!(!mgr.pane_has_overlays("pane-1"));
    }

    #[test]
    fn test_input_routing_order() {
        let mut mgr = OverlayManager::new();

        mgr.show_overlay("pane-1", "plugin-a");
        mgr.show_overlay("pane-1", "plugin-b");
        mgr.show_overlay("pane-1", "plugin-c");

        let order = mgr.input_routing_order("pane-1");
        assert_eq!(order, vec!["plugin-c", "plugin-b", "plugin-a"]);
    }
}

#[cfg(test)]
mod compositor_tests {
    use super::*;

    #[test]
    fn test_strip_ansi_len() {
        assert_eq!(strip_ansi_len("hello"), 5);
        assert_eq!(strip_ansi_len("\x1b[31mred\x1b[0m"), 3);
        assert_eq!(strip_ansi_len(""), 0);
    }

    #[test]
    fn test_composite_no_overlays() {
        let mgr = OverlayManager::new();
        let base = vec!["line 1".to_string(), "line 2".to_string()];

        let result = OverlayCompositor::composite("pane-1", &base, &mgr, 80, 24);
        assert_eq!(result, base);
    }
}
```

---

## Verification

### Functional

```bash
# Build the overlay modules
cargo build -p shux-ui 2>&1 | tail -5
cargo build -p shux-plugin 2>&1 | tail -5

# Verify types compile
cargo check -p shux-ui -p shux-plugin

# Verify no clippy warnings
cargo clippy -p shux-ui -p shux-plugin -- -D warnings
```

### Tests

```bash
# Run overlay stack tests
cargo nextest run -p shux-ui overlay_stack_tests

# Run overlay manager tests
cargo nextest run -p shux-ui overlay_manager_tests

# Run compositor tests
cargo nextest run -p shux-ui compositor_tests

# Run all tests
cargo nextest run --workspace
```

---

## Completion Criteria

- [ ] `OverlayStack` maintains an ordered list of overlays per pane
- [ ] `show` pushes a new overlay on top (or replaces in-place if same plugin)
- [ ] `hide` removes the plugin's overlay from the stack
- [ ] Z-ordering: most recent on top, rendered last
- [ ] Input routing: topmost overlay gets `on-overlay-input` first
- [ ] If `on-overlay-input` returns true, input is consumed (stops propagation)
- [ ] If `on-overlay-input` returns false, input passes to next overlay, then pane
- [ ] `render-overlay` called for ALL visible overlays during render cycle
- [ ] `OverlayManager` manages per-pane stacks across the entire session
- [ ] Plugin unload removes all overlays from that plugin
- [ ] Pane close removes all overlays for that pane
- [ ] `OverlayBridge` translates plugin requests into overlay operations
- [ ] `OverlayCompositor` composites overlays on top of pane content
- [ ] Process plugin protocol supports show_overlay, hide_overlay, render_overlay, overlay_input
- [ ] All tests pass
- [ ] No clippy warnings

---

## Commit Message

```
feat(ui): implement per-pane overlay system with z-ordering and input routing

- Per-pane overlay stack with z-ordering (most recent on top)
- show-overlay pushes new or replaces existing overlay in-place
- hide-overlay removes plugin's overlay from the stack
- Input routing: topmost overlay gets input first, falls through on false
- render-overlay called for all visible overlays, composited bottom-to-top
- OverlayManager coordinates stacks across all panes
- OverlayBridge connects plugin host to overlay system
- Overlay compositor integrates with render pipeline
- Plugin unload and pane close cleanup
```

---

## Session Protocol

1. **Before starting:** Read task 041 (plugin lifecycle) for how overlays are triggered by plugins. Read PRD section 7.2b for the exact z-ordering and input routing rules. Review the Danger Zone, Command Palette, and Session Replay use cases for concrete overlay usage patterns.
2. **During:** Implement in order: types (Step 1) -> overlay stack (Step 1) -> overlay manager (Step 2) -> compositor (Step 3) -> input routing (Step 4) -> bridge (Step 5) -> tests (Step 6). Run `cargo check` after each step. Run tests after Steps 2, 3, and 6.
3. **After:** Run the full verification suite. Verify z-ordering with 3+ overlays. Verify show-replace semantics (same plugin, same position). Verify input routing order. Update `docs/PROGRESS.md` (mark 046 done). Update `CLAUDE.md` Learnings with insights about ANSI escape handling in overlay compositing.
