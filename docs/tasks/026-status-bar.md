# 026 — Status Bar (Hardcoded, Pre-Plugin)

**Status:** Pending
**Depends On:** 025
**Parallelizable With:** 027

---

## Problem

shux needs a status bar to provide at-a-glance context: which session, which window is active, what the current pane is running, and the time. The PRD specifies a bundled `shux-status-bar` plugin (task 048), but that requires the full plugin system (M2). For M1 daily-driver use, we need a hardcoded status bar rendered directly by the compositor. This version will be replaced by the plugin in M2, but must be fully functional so that M1 dogfooding can begin with a polished experience.

The status bar must be theme-aware (using the resolved theme's status bar tokens from task 024/025), configurable in position (top or bottom), and display the four key segments: session name, window list with active indicator, active pane info, and clock. Where mouse support exists (task 020), window tabs in the status bar should be clickable.

## PRD Reference

- **SS 6.1** P0 Feature Matrix, UX & keybindings: "Status bar: Rendered by bundled status-bar plugin. Shows session name, window list, active pane info, clock. Clickable segments where terminal supports."
- **SS 7.7** Bundled plugins: shux-status-bar — "Default status line: session, windows, pane info, clock" (this task implements the pre-plugin hardcoded version)
- **SS 10.2** Config reference: `status_bar = true`, `status_bar_position = "bottom"`
- **SS 14.1** Performance budgets: Keypress-to-visible-update p99 <= 25ms (status bar must not regress this)

---

## Files to Create

- `crates/shux-ui/src/status_bar.rs` — Status bar widget: layout, rendering, data sourcing
- `crates/shux-ui/src/status_bar/segments.rs` — Individual segment renderers (session, windows, pane info, clock)

## Files to Modify

- `crates/shux-ui/src/lib.rs` — Add `pub mod status_bar;`
- `crates/shux-ui/src/compositor.rs` — Integrate status bar into render cycle, reserve row for status bar
- `crates/shux-core/src/config.rs` — Ensure `status_bar` and `status_bar_position` fields are present
- `crates/shux-core/src/theme.rs` — Ensure status bar theme tokens are defined (status_bar_bg, status_bar_fg, status_bar_active_fg, etc.)
- `crates/shux-ui/Cargo.toml` — Add `chrono` dependency for clock formatting (or use `std::time`)

---

## Execution Steps

### Step 1: Define Status Bar Theme Tokens

Ensure the theme token set from task 024 includes status bar tokens. In `crates/shux-core/src/theme.rs`:

```rust
/// Theme tokens specific to the status bar.
/// These are resolved from the active session-level theme (not per-pane).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusBarTokens {
    /// Background color for the entire status bar
    pub bg: Color,
    /// Default foreground color for text
    pub fg: Color,
    /// Foreground color for the active window tab
    pub active_fg: Color,
    /// Background color for the active window tab
    pub active_bg: Color,
    /// Foreground color for inactive window tabs
    pub inactive_fg: Color,
    /// Separator character style
    pub separator_fg: Color,
}
```

These tokens should already be part of the `ResolvedTheme` struct from task 024. Verify they exist and add defaults if missing.

### Step 2: Define Status Bar Configuration

In `crates/shux-core/src/config.rs`, verify the UI config section includes:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    // ... existing fields ...

    /// Whether to show the status bar
    #[serde(default = "default_true")]
    pub status_bar: bool,

    /// Position of the status bar: "top" or "bottom"
    #[serde(default = "default_bottom")]
    pub status_bar_position: StatusBarPosition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StatusBarPosition {
    Top,
    Bottom,
}

impl Default for StatusBarPosition {
    fn default() -> Self {
        StatusBarPosition::Bottom
    }
}
```

### Step 3: Create Status Bar Data Model

Create `crates/shux-ui/src/status_bar.rs` with the core status bar struct:

```rust
//! Hardcoded status bar for M1 (pre-plugin).
//!
//! This module renders a single-line status bar showing:
//! - Session name (left)
//! - Window list with active indicator (center-left)
//! - Active pane info (center-right)
//! - Clock in HH:MM format (right)
//!
//! This will be replaced by the shux-status-bar plugin in task 048.

mod segments;

use crate::compositor::RenderContext;
use shux_core::config::StatusBarPosition;
use shux_core::theme::StatusBarTokens;

/// Data needed to render the status bar, extracted from session state.
#[derive(Debug, Clone)]
pub struct StatusBarData {
    /// Name of the active session
    pub session_name: String,
    /// List of windows in the active session
    pub windows: Vec<WindowInfo>,
    /// Index of the currently active window (0-based)
    pub active_window_index: usize,
    /// Info about the currently active pane
    pub active_pane: PaneInfo,
    /// Whether the status bar is enabled
    pub enabled: bool,
    /// Position: top or bottom
    pub position: StatusBarPosition,
}

#[derive(Debug, Clone)]
pub struct WindowInfo {
    /// Window index (1-based, for display)
    pub index: usize,
    /// Window name/title
    pub name: String,
    /// Whether this window is the active one
    pub active: bool,
    /// Number of panes in this window
    pub pane_count: usize,
}

#[derive(Debug, Clone)]
pub struct PaneInfo {
    /// Pane title (manual or auto-derived)
    pub title: String,
    /// Currently running command name
    pub command: String,
    /// Current working directory (abbreviated)
    pub cwd: String,
}

/// The status bar renderer.
pub struct StatusBar {
    /// Cached clock string, updated every minute
    clock_cache: String,
    /// Last minute value used to determine when to update clock
    last_minute: u32,
    /// Clickable regions for mouse support (column ranges mapped to actions)
    click_regions: Vec<ClickRegion>,
}

/// A clickable region in the status bar.
#[derive(Debug, Clone)]
pub struct ClickRegion {
    /// Start column (inclusive)
    pub start_col: u16,
    /// End column (exclusive)
    pub end_col: u16,
    /// Action to perform when clicked
    pub action: ClickAction,
}

#[derive(Debug, Clone)]
pub enum ClickAction {
    /// Switch to window by index (0-based)
    SwitchWindow(usize),
}

impl StatusBar {
    pub fn new() -> Self {
        Self {
            clock_cache: String::new(),
            last_minute: u32::MAX,
            click_regions: Vec::new(),
        }
    }

    /// Extract status bar data from the current session state snapshot.
    pub fn gather_data(&self, ctx: &RenderContext) -> StatusBarData {
        // Read from the ArcSwap state snapshot
        // Session name from active session
        // Window list from active session's windows
        // Active pane info from focused pane
        // Config for enabled/position
        todo!("Extract from RenderContext — depends on task 025 compositor")
    }

    /// Render the status bar into a buffer row.
    ///
    /// Returns the list of clickable regions for mouse handling.
    pub fn render(
        &mut self,
        data: &StatusBarData,
        tokens: &StatusBarTokens,
        width: u16,
        buf: &mut Vec<StyledCell>,
    ) -> &[ClickRegion] {
        buf.clear();
        self.click_regions.clear();

        if !data.enabled {
            return &self.click_regions;
        }

        // Layout: [session_name] [window_list ...] <spacer> [pane_info] [HH:MM]
        let session_segment = segments::render_session(&data.session_name, tokens);
        let window_segment = segments::render_windows(
            &data.windows,
            data.active_window_index,
            tokens,
        );
        let pane_segment = segments::render_pane_info(&data.active_pane, tokens);
        let clock_segment = self.render_clock(tokens);

        // Calculate widths
        let left_width = session_segment.len() + 1 + window_segment.len();
        let right_width = pane_segment.len() + 1 + clock_segment.len();
        let total_width = width as usize;

        // Fill left side
        buf.extend_from_slice(&session_segment);
        buf.push(StyledCell::new(' ', tokens.bg, tokens.separator_fg));

        // Track click regions for window tabs
        let window_start = buf.len() as u16;
        buf.extend_from_slice(&window_segment);
        self.register_window_click_regions(
            &data.windows,
            window_start,
            tokens,
        );

        // Fill spacer
        let spacer_len = total_width.saturating_sub(left_width + right_width);
        for _ in 0..spacer_len {
            buf.push(StyledCell::new(' ', tokens.bg, tokens.fg));
        }

        // Fill right side
        buf.extend_from_slice(&pane_segment);
        buf.push(StyledCell::new(' ', tokens.bg, tokens.separator_fg));
        buf.extend_from_slice(&clock_segment);

        // Truncate or pad to exact width
        buf.truncate(total_width);
        while buf.len() < total_width {
            buf.push(StyledCell::new(' ', tokens.bg, tokens.fg));
        }

        &self.click_regions
    }

    /// Update and render the clock segment.
    fn render_clock(&mut self, tokens: &StatusBarTokens) -> Vec<StyledCell> {
        use std::time::SystemTime;

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        let secs = now.as_secs();
        let minutes = (secs / 60) % 60;
        let hours = (secs / 3600) % 24;

        // Only reformat when the minute changes
        let current_minute = (secs / 60) as u32;
        if current_minute != self.last_minute {
            self.clock_cache = format!("{:02}:{:02}", hours, minutes);
            self.last_minute = current_minute;
        }

        segments::styled_string(&self.clock_cache, tokens.bg, tokens.fg)
    }

    /// Register clickable regions for each window tab.
    fn register_window_click_regions(
        &mut self,
        windows: &[WindowInfo],
        start_col: u16,
        _tokens: &StatusBarTokens,
    ) {
        let mut col = start_col;
        for (i, win) in windows.iter().enumerate() {
            // Format: " N:name " — calculate width
            let label = format!(" {}:{} ", win.index, win.name);
            let end_col = col + label.len() as u16;
            self.click_regions.push(ClickRegion {
                start_col: col,
                end_col,
                action: ClickAction::SwitchWindow(i),
            });
            col = end_col;
        }
    }

    /// Handle a mouse click at the given column.
    /// Returns the action to perform, if any.
    pub fn handle_click(&self, col: u16) -> Option<&ClickAction> {
        self.click_regions
            .iter()
            .find(|r| col >= r.start_col && col < r.end_col)
            .map(|r| &r.action)
    }
}
```

### Step 4: Create Segment Renderers

Create `crates/shux-ui/src/status_bar/segments.rs`:

```rust
//! Individual segment renderers for the status bar.
//!
//! Each segment function takes data and theme tokens, returning a Vec<StyledCell>.

use shux_core::theme::StatusBarTokens;
use super::{PaneInfo, WindowInfo, StyledCell};

/// Render the session name segment: " session_name "
pub fn render_session(
    session_name: &str,
    tokens: &StatusBarTokens,
) -> Vec<StyledCell> {
    let label = format!(" {} ", session_name);
    // Session name uses accent colors to stand out
    styled_string(&label, tokens.active_bg, tokens.active_fg)
}

/// Render the window list segment: " 1:editor  2:servers* 3:logs "
/// Active window is highlighted with active_bg/active_fg.
/// Inactive windows use default fg.
pub fn render_windows(
    windows: &[WindowInfo],
    active_index: usize,
    tokens: &StatusBarTokens,
) -> Vec<StyledCell> {
    let mut cells = Vec::new();

    for (i, win) in windows.iter().enumerate() {
        let active_marker = if i == active_index { "*" } else { "" };
        let label = format!(" {}:{}{} ", win.index, win.name, active_marker);

        if i == active_index {
            cells.extend(styled_string(&label, tokens.active_bg, tokens.active_fg));
        } else {
            cells.extend(styled_string(&label, tokens.bg, tokens.inactive_fg));
        }
    }

    cells
}

/// Render the active pane info segment: " title | command | ~/path "
pub fn render_pane_info(
    pane: &PaneInfo,
    tokens: &StatusBarTokens,
) -> Vec<StyledCell> {
    // Abbreviate CWD: replace $HOME with ~
    let cwd_display = abbreviate_path(&pane.cwd);

    let label = if pane.title.is_empty() {
        format!(" {} | {} ", pane.command, cwd_display)
    } else {
        format!(" {} | {} | {} ", pane.title, pane.command, cwd_display)
    };

    styled_string(&label, tokens.bg, tokens.fg)
}

/// Convert a string into a vector of StyledCells with uniform colors.
pub fn styled_string(
    text: &str,
    bg: crossterm::style::Color,
    fg: crossterm::style::Color,
) -> Vec<StyledCell> {
    text.chars()
        .map(|ch| StyledCell::new(ch, bg, fg))
        .collect()
}

/// Abbreviate a filesystem path for display.
/// Replaces the user's home directory with ~.
/// Truncates to last N components if too long.
fn abbreviate_path(path: &str) -> String {
    // Replace home directory with ~
    let home = std::env::var("HOME").unwrap_or_default();
    let abbreviated = if !home.is_empty() && path.starts_with(&home) {
        format!("~{}", &path[home.len()..])
    } else {
        path.to_string()
    };

    // If still too long (>30 chars), show .../last_two_components
    if abbreviated.len() > 30 {
        let parts: Vec<&str> = abbreviated.rsplit('/').take(2).collect();
        if parts.len() >= 2 {
            format!(".../{}/{}", parts[1], parts[0])
        } else {
            abbreviated
        }
    } else {
        abbreviated
    }
}
```

### Step 5: Integrate Status Bar into Compositor

Modify `crates/shux-ui/src/compositor.rs` to reserve space for and render the status bar:

```rust
use crate::status_bar::{StatusBar, StatusBarData, ClickAction};

pub struct Compositor {
    // ... existing fields ...

    /// The status bar renderer
    status_bar: StatusBar,
}

impl Compositor {
    /// Calculate the available area for panes, accounting for the status bar.
    fn pane_area(&self, terminal_size: (u16, u16), config: &UiConfig) -> Rect {
        let (cols, rows) = terminal_size;

        if !config.status_bar {
            return Rect { x: 0, y: 0, width: cols, height: rows };
        }

        match config.status_bar_position {
            StatusBarPosition::Top => Rect {
                x: 0,
                y: 1, // status bar takes row 0
                width: cols,
                height: rows.saturating_sub(1),
            },
            StatusBarPosition::Bottom => Rect {
                x: 0,
                y: 0,
                width: cols,
                height: rows.saturating_sub(1), // status bar takes last row
            },
        }
    }

    /// Determine the row where the status bar should be rendered.
    fn status_bar_row(&self, terminal_rows: u16, config: &UiConfig) -> u16 {
        match config.status_bar_position {
            StatusBarPosition::Top => 0,
            StatusBarPosition::Bottom => terminal_rows.saturating_sub(1),
        }
    }

    /// Main render function — called each frame.
    pub fn render(&mut self, ctx: &RenderContext) -> Result<()> {
        // ... existing pane rendering ...

        // Render status bar if enabled
        if ctx.config.ui.status_bar {
            let data = self.status_bar.gather_data(ctx);
            let tokens = ctx.theme.status_bar_tokens();
            let row = self.status_bar_row(ctx.terminal_size.1, &ctx.config.ui);
            let mut cells = Vec::with_capacity(ctx.terminal_size.0 as usize);

            self.status_bar.render(&data, &tokens, ctx.terminal_size.0, &mut cells);

            // Write status bar row to the terminal buffer
            self.write_row(row, &cells)?;
        }

        Ok(())
    }

    /// Handle a mouse click event. Check if it hit the status bar.
    pub fn handle_mouse_click(&self, col: u16, row: u16, config: &UiConfig) -> Option<ClickAction> {
        if !config.status_bar {
            return None;
        }

        let sb_row = self.status_bar_row(
            /* terminal rows */ 0, // TODO: get from context
            config,
        );

        if row == sb_row {
            self.status_bar.handle_click(col).cloned()
        } else {
            None
        }
    }
}
```

### Step 6: Wire Up Clock Refresh

The clock displays HH:MM and updates every minute. Rather than a dedicated timer, the clock is re-evaluated on every render cycle. Since the compositor already renders at a regular interval (driven by PTY output or input events), and the clock only changes every 60 seconds, the cached minute comparison in `StatusBar::render_clock` ensures minimal overhead.

For scenarios where no events occur for extended periods (idle terminal), add a periodic tick:

```rust
// In the main event loop (crates/shux-ui/src/lib.rs or event loop module):
use tokio::time::{interval, Duration};

let mut clock_tick = interval(Duration::from_secs(60));

loop {
    tokio::select! {
        // ... other event sources ...

        _ = clock_tick.tick() => {
            // Trigger a compositor redraw to update the clock
            compositor.request_redraw();
        }
    }
}
```

### Step 7: Handle Status Bar in Resize

When the terminal resizes, the status bar must re-render to fill the new width. The pane area must be recalculated to account for the status bar row:

```rust
// In resize handler:
fn handle_resize(&mut self, new_cols: u16, new_rows: u16) {
    let pane_area = self.pane_area((new_cols, new_rows), &self.config.ui);
    self.layout_engine.resize(pane_area);
    // Status bar re-renders automatically on next render() call
}
```

### Step 8: Add Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use shux_core::theme::StatusBarTokens;
    use crossterm::style::Color;

    fn test_tokens() -> StatusBarTokens {
        StatusBarTokens {
            bg: Color::Rgb { r: 30, g: 30, b: 46 },
            fg: Color::Rgb { r: 205, g: 214, b: 244 },
            active_fg: Color::Rgb { r: 30, g: 30, b: 46 },
            active_bg: Color::Rgb { r: 137, g: 180, b: 250 },
            inactive_fg: Color::Rgb { r: 108, g: 112, b: 134 },
            separator_fg: Color::Rgb { r: 69, g: 71, b: 90 },
        }
    }

    fn sample_data() -> StatusBarData {
        StatusBarData {
            session_name: "work".into(),
            windows: vec![
                WindowInfo { index: 1, name: "editor".into(), active: true, pane_count: 1 },
                WindowInfo { index: 2, name: "servers".into(), active: false, pane_count: 2 },
                WindowInfo { index: 3, name: "logs".into(), active: false, pane_count: 1 },
            ],
            active_window_index: 0,
            active_pane: PaneInfo {
                title: "nvim".into(),
                command: "nvim".into(),
                cwd: "/home/user/project/src".into(),
            },
            enabled: true,
            position: StatusBarPosition::Bottom,
        }
    }

    #[test]
    fn test_status_bar_render_fills_exact_width() {
        let mut sb = StatusBar::new();
        let data = sample_data();
        let tokens = test_tokens();
        let mut buf = Vec::new();

        sb.render(&data, &tokens, 120, &mut buf);
        assert_eq!(buf.len(), 120, "Status bar must fill exact terminal width");
    }

    #[test]
    fn test_status_bar_disabled_returns_empty() {
        let mut sb = StatusBar::new();
        let mut data = sample_data();
        data.enabled = false;
        let tokens = test_tokens();
        let mut buf = Vec::new();

        let regions = sb.render(&data, &tokens, 120, &mut buf);
        assert!(buf.is_empty());
        assert!(regions.is_empty());
    }

    #[test]
    fn test_window_click_regions_registered() {
        let mut sb = StatusBar::new();
        let data = sample_data();
        let tokens = test_tokens();
        let mut buf = Vec::new();

        let regions = sb.render(&data, &tokens, 120, &mut buf);
        assert_eq!(regions.len(), 3, "One click region per window");
    }

    #[test]
    fn test_click_on_window_tab() {
        let mut sb = StatusBar::new();
        let data = sample_data();
        let tokens = test_tokens();
        let mut buf = Vec::new();

        sb.render(&data, &tokens, 120, &mut buf);

        // Click on the second window tab region
        if let Some(region) = sb.click_regions.get(1) {
            let mid = (region.start_col + region.end_col) / 2;
            let action = sb.handle_click(mid);
            assert!(matches!(action, Some(ClickAction::SwitchWindow(1))));
        }
    }

    #[test]
    fn test_narrow_terminal_truncates_gracefully() {
        let mut sb = StatusBar::new();
        let data = sample_data();
        let tokens = test_tokens();
        let mut buf = Vec::new();

        // Very narrow terminal — must not panic
        sb.render(&data, &tokens, 20, &mut buf);
        assert_eq!(buf.len(), 20);
    }

    #[test]
    fn test_abbreviate_path_replaces_home() {
        std::env::set_var("HOME", "/home/user");
        let result = segments::abbreviate_path("/home/user/projects/shux");
        assert_eq!(result, "~/projects/shux");
    }

    #[test]
    fn test_abbreviate_path_truncates_long() {
        std::env::set_var("HOME", "/home/user");
        let long_path = "/home/user/very/deeply/nested/directory/structure/here";
        let result = segments::abbreviate_path(long_path);
        assert!(result.len() <= 35, "Long paths should be abbreviated");
    }

    #[test]
    fn test_clock_cache_updates_on_minute_change() {
        let mut sb = StatusBar::new();
        let tokens = test_tokens();

        // First call populates cache
        let cells1 = sb.render_clock(&tokens);
        assert_eq!(cells1.len(), 5); // "HH:MM" = 5 chars

        // Second call within same minute should use cache
        let cells2 = sb.render_clock(&tokens);
        assert_eq!(cells2.len(), 5);
    }
}
```

---

## Verification

### Functional

```bash
# Build the workspace
cargo build --workspace

# Verify status bar module compiles
cargo check -p shux-ui

# Run with a session and verify status bar appears
# (manual verification during M1 integration)
cargo run -p shux -- new -s test
# Expected: status bar visible at bottom with "test" session name, window list, clock
```

### Tests

```bash
# Run status bar unit tests
cargo nextest run -p shux-ui status_bar

# Expected: all status bar tests pass
# - render_fills_exact_width
# - disabled_returns_empty
# - window_click_regions_registered
# - click_on_window_tab
# - narrow_terminal_truncates_gracefully
# - abbreviate_path_replaces_home
# - abbreviate_path_truncates_long
# - clock_cache_updates_on_minute_change
```

### L4 Visual Regression — iterm2-driver (PRD §16.2)

The status bar is verified as part of `test_m1_visual.py` (task 034, Scenario 1).
Additionally, create `.claude/automations/test_status_bar_visual.py` for focused status bar checks:

```python
# /// script
# requires-python = ">=3.14"
# dependencies = ["iterm2", "pyobjc", "pyobjc-framework-Quartz"]
# ///
"""
shux Status Bar Visual Test (iterm2-driver)

Tests:
1. Launch shux with session name "status-test"
2. Read bottom row of screen — verify status bar present
3. Verify session name "status-test" appears in status bar
4. Verify window number "1:" appears
5. Verify clock format "HH:MM" appears (right side)
6. Create second window — verify window list updates ("1: 2:")
7. Switch windows — verify active window indicator changes
8. Take screenshot: status_bar.png

Verification Strategy:
- Read specific screen rows (bottom 1-2 rows)
- Assert expected text patterns in status bar content

Usage:
    uv run .claude/automations/test_status_bar_visual.py
"""
```

Run: `uv run .claude/automations/test_status_bar_visual.py`

---

## Completion Criteria

- [ ] Status bar renders as a single row at configurable position (top or bottom)
- [ ] Session name segment displayed on the left
- [ ] Window list displayed with numbered windows and active indicator (*)
- [ ] Active window highlighted with distinct colors (active_bg/active_fg tokens)
- [ ] Active pane info displayed: title, command, abbreviated CWD
- [ ] Clock displayed in HH:MM format on the right, updating every minute
- [ ] Status bar uses theme tokens from `ResolvedTheme.status_bar_tokens`
- [ ] Status bar position configurable via `config.ui.status_bar_position` (top/bottom)
- [ ] Status bar can be disabled via `config.ui.status_bar = false`
- [ ] Compositor reserves one row for status bar and adjusts pane area accordingly
- [ ] Window tabs are clickable (ClickRegion registered for each window)
- [ ] Mouse click on window tab triggers window switch
- [ ] Narrow terminals handled gracefully (truncation, no panic)
- [ ] 60-second clock tick triggers redraw for idle terminals
- [ ] Terminal resize correctly recalculates pane area with status bar
- [ ] Unit tests pass for all segments, click regions, and edge cases
- [ ] No performance regression: render cycle stays within p99 <= 25ms budget

---

## Commit Message

```
feat(ui): add hardcoded status bar with session, windows, pane info, clock

- Render status bar as compositor row (top or bottom, configurable)
- Display session name, numbered window list with active highlight,
  active pane info (title/command/CWD), and HH:MM clock
- Theme-aware: uses StatusBarTokens from resolved theme
- Clickable window tabs with ClickRegion tracking for mouse support
- 60-second periodic tick for clock refresh on idle terminals
- Graceful truncation for narrow terminals
- Pre-plugin implementation for M1 (replaced by shux-status-bar in M2)
```

---

## Session Protocol

1. **Before starting:** Read task 025 (per-pane theming) completion output to understand the `ResolvedTheme` structure and compositor's `RenderContext`. Read task 020 (mouse support) to understand how mouse clicks are dispatched. Read task 024 (theme engine) for `StatusBarTokens`.
2. **During:** Implement in order: theme tokens verification (Step 1), config (Step 2), data model (Step 3), segments (Step 4), compositor integration (Step 5), clock tick (Step 6), resize handling (Step 7), tests (Step 8). Run `cargo check -p shux-ui` after each step.
3. **Edge cases to watch for:**
   - Terminal width smaller than minimum content (session name alone exceeds width)
   - Sessions with many windows (window list overflows available space)
   - Empty pane title and command (freshly spawned shell)
   - Unicode characters in session/window/pane names
   - Clock timezone: use local time, not UTC
4. **After:** Run full test suite (`cargo nextest run --workspace`). Verify status bar renders correctly in a real terminal. Update `docs/PROGRESS.md`. Update `CLAUDE.md` Learnings.
