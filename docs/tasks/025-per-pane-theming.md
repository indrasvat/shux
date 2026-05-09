# 025 — Per-Pane Theming

**Status:** Pending
**Depends On:** 024, 017
**Parallelizable With:** 027

---

## Problem

Per-pane theming is shux's key differentiator (PRD SS 2.4 point 4). SREs color-code production panes red and staging panes yellow to prevent catastrophic mistakes. Developers want distinct visual contexts for different projects. Agent orchestration tools need to visually distinguish agent panes from human panes. No existing multiplexer supports per-pane theming -- tmux, Zellij, and TUIOS all have a single global theme.

This task takes the ThemeEngine (task 024) and integrates it into the rendering pipeline so that each pane can have its own theme or token overrides. The compositor uses the pane's resolved theme when rendering its content, borders, and contribution to the status bar. Two API methods (`pane.set_theme` and `pane.set_theme_override`) provide programmatic control.

## PRD Reference

- **SS 6.1** (Per-pane theming -- KEY DIFFERENTIATOR)
- **SS 2.4** point 4 (Color-code prod vs dev, highlight active agent panes)
- **SS 5.3** (Theme cascade includes pane-level)
- **SS 8.2** (pane.set_theme, pane.set_theme_override API methods)
- **SS 9.2** (`Prefix + t` -- set pane theme interactive picker)

---

## Files to Create

- `crates/shux-rpc/src/methods/theme.rs` -- pane.set_theme, pane.set_theme_override RPC handlers
- `crates/shux-ui/src/theme_picker.rs` -- Interactive theme picker UI (Prefix + t)
- `crates/shux-core/tests/per_pane_theme_test.rs` -- Per-pane theming tests

## Files to Modify

- `crates/shux-ui/src/compositor.rs` -- Use pane-resolved themes for all rendering
- `crates/shux-core/src/theme_engine.rs` -- Ensure pane-level cascade is wired correctly
- `crates/shux-core/src/lib.rs` -- Re-export theme types for other crates
- `crates/shux-rpc/src/lib.rs` -- Register theme RPC method handlers
- `crates/shux-ui/src/prefix_actions.rs` -- Wire Prefix+t to theme picker

---

## Execution Steps

### Step 1: Wire per-pane theme into the data model

The `Pane` entity (from task 002) already has a `theme: Option<ThemeRef>` field (PRD SS 5.1). Ensure this field is properly connected to the ThemeEngine's pane theme storage.

```rust
// In the session graph mutation handler:

/// Set a pane's theme reference.
pub fn set_pane_theme(
    &mut self,
    pane_id: PaneId,
    theme_ref: ThemeRef,
    theme_engine: &mut ThemeEngine,
) -> Result<(), GraphError> {
    let pane = self.panes.get_mut(&pane_id)
        .ok_or(GraphError::PaneNotFound(pane_id))?;

    pane.theme = Some(theme_ref.clone());
    pane.version += 1;

    // Update the theme engine's pane-level cache.
    theme_engine.set_pane_theme(pane_id.0, theme_ref);

    // Emit event.
    self.emit_event(Event::PaneThemeChanged {
        pane_id,
        theme_name: pane.theme.as_ref()
            .and_then(|t| t.theme_name.clone()),
    });

    Ok(())
}

/// Clear a pane's theme (revert to inherited theme from window/session/global).
pub fn clear_pane_theme(
    &mut self,
    pane_id: PaneId,
    theme_engine: &mut ThemeEngine,
) -> Result<(), GraphError> {
    let pane = self.panes.get_mut(&pane_id)
        .ok_or(GraphError::PaneNotFound(pane_id))?;

    pane.theme = None;
    pane.version += 1;

    theme_engine.clear_pane_theme(&pane_id.0);

    self.emit_event(Event::PaneThemeChanged {
        pane_id,
        theme_name: None,
    });

    Ok(())
}
```

### Step 2: Implement pane.set_theme RPC method

The `pane.set_theme` method sets a named theme on a pane. The `pane.set_theme_override` method sets individual token overrides.

```rust
// crates/shux-rpc/src/methods/theme.rs

use serde::{Deserialize, Serialize};

/// pane.set_theme -- Set a named theme on a specific pane.
#[derive(Deserialize)]
pub struct PaneSetThemeParams {
    /// The pane to theme.
    pub pane_id: String,
    /// The theme name to apply. Use null/empty to clear.
    pub theme_name: String,
}

#[derive(Serialize)]
pub struct PaneSetThemeResult {
    pub pane_id: String,
    pub theme_name: String,
    pub version: u64,
}

pub async fn handle_pane_set_theme(
    params: PaneSetThemeParams,
    ctx: &mut DaemonContext,
) -> Result<PaneSetThemeResult, RpcError> {
    let pane_id = parse_uuid(&params.pane_id)?;

    if params.theme_name.is_empty() {
        // Clear the pane theme.
        ctx.graph.clear_pane_theme(
            PaneId(pane_id),
            &mut ctx.theme_engine,
        )?;
        let pane = ctx.graph.get_pane(&PaneId(pane_id))?;
        return Ok(PaneSetThemeResult {
            pane_id: params.pane_id,
            theme_name: String::new(),
            version: pane.version,
        });
    }

    // Verify the theme exists.
    if ctx.theme_engine.get_theme(&params.theme_name).is_none() {
        return Err(RpcError::InvalidParams(format!(
            "theme '{}' not found. Available: {:?}",
            params.theme_name,
            ctx.theme_engine.list_themes().iter().map(|t| &t.name).collect::<Vec<_>>()
        )));
    }

    let theme_ref = ThemeRef {
        theme_name: Some(params.theme_name.clone()),
        overrides: ThemeOverride::default(),
    };

    ctx.graph.set_pane_theme(
        PaneId(pane_id),
        theme_ref,
        &mut ctx.theme_engine,
    )?;

    let pane = ctx.graph.get_pane(&PaneId(pane_id))?;

    Ok(PaneSetThemeResult {
        pane_id: params.pane_id,
        theme_name: params.theme_name,
        version: pane.version,
    })
}

/// pane.set_theme_override -- Override individual theme tokens on a pane.
#[derive(Deserialize)]
pub struct PaneSetThemeOverrideParams {
    pub pane_id: String,
    /// Individual token overrides as key-value pairs.
    /// Keys are token names (e.g., "accent_primary", "border_focused").
    /// Values are color strings (e.g., "#ff0000", "red").
    pub overrides: std::collections::HashMap<String, String>,
}

#[derive(Serialize)]
pub struct PaneSetThemeOverrideResult {
    pub pane_id: String,
    pub overrides_applied: usize,
    pub version: u64,
}

pub async fn handle_pane_set_theme_override(
    params: PaneSetThemeOverrideParams,
    ctx: &mut DaemonContext,
) -> Result<PaneSetThemeOverrideResult, RpcError> {
    let pane_id = parse_uuid(&params.pane_id)?;

    // Build ThemeOverride from the provided key-value pairs.
    let mut overrides = ThemeOverride::default();
    let mut count = 0;

    for (key, value) in &params.overrides {
        let color = Color::parse(value).map_err(|e| RpcError::invalid_params(
            format!("invalid color for '{}': {}", key, e)
        ))?;
        match key.as_str() {
            "bg_deep" => { overrides.bg_deep = Some(color); count += 1; }
            "bg_surface" => { overrides.bg_surface = Some(color); count += 1; }
            "bg_subtle" => { overrides.bg_subtle = Some(color); count += 1; }
            "fg_primary" => { overrides.fg_primary = Some(color); count += 1; }
            "fg_secondary" => { overrides.fg_secondary = Some(color); count += 1; }
            "fg_muted" => { overrides.fg_muted = Some(color); count += 1; }
            "accent_primary" => { overrides.accent_primary = Some(color); count += 1; }
            "accent_secondary" => { overrides.accent_secondary = Some(color); count += 1; }
            "border_focused" => { overrides.border_focused = Some(color); count += 1; }
            "border_unfocused" => { overrides.border_unfocused = Some(color); count += 1; }
            "status_bar_bg" => { overrides.status_bar_bg = Some(color); count += 1; }
            "status_bar_fg" => { overrides.status_bar_fg = Some(color); count += 1; }
            "status_bar_accent" => { overrides.status_bar_accent = Some(color); count += 1; }
            "selection_bg" => { overrides.selection_bg = Some(color); count += 1; }
            "selection_fg" => { overrides.selection_fg = Some(color); count += 1; }
            "error" => { overrides.error = Some(color); count += 1; }
            "warn" => { overrides.warn = Some(color); count += 1; }
            "info" => { overrides.info = Some(color); count += 1; }
            "success" => { overrides.success = Some(color); count += 1; }
            unknown => {
                return Err(RpcError::InvalidParams(format!(
                    "unknown theme token: '{}'. Valid tokens: bg_deep, bg_surface, bg_subtle, \
                     fg_primary, fg_secondary, fg_muted, accent_primary, accent_secondary, \
                     border_focused, border_unfocused, status_bar_bg, status_bar_fg, \
                     status_bar_accent, selection_bg, selection_fg, error, warn, info, success",
                    unknown
                )));
            }
        }
    }

    // Preserve existing theme_name if set.
    let existing_pane = ctx.graph.get_pane(&PaneId(pane_id))?;
    let existing_name = existing_pane.theme
        .as_ref()
        .and_then(|t| t.theme_name.clone());

    let theme_ref = ThemeRef {
        theme_name: existing_name,
        overrides,
    };

    ctx.graph.set_pane_theme(
        PaneId(pane_id),
        theme_ref,
        &mut ctx.theme_engine,
    )?;

    let pane = ctx.graph.get_pane(&PaneId(pane_id))?;

    Ok(PaneSetThemeOverrideResult {
        pane_id: params.pane_id,
        overrides_applied: count,
        version: pane.version,
    })
}
```

### Step 3: Modify compositor to use per-pane themes

The compositor (from task 017) must resolve the theme for each pane individually and use it when rendering borders, content backgrounds, and selection highlights.

```rust
// In crates/shux-ui/src/compositor.rs (modifications)

impl Compositor {
    /// Render a single pane with its resolved theme.
    fn render_pane(
        &self,
        pane: &PaneState,
        rect: &PaneRect,
        focused: bool,
        theme_engine: &ThemeEngine,
        buf: &mut RenderBuffer,
    ) {
        // Resolve the theme for this specific pane.
        let resolved = theme_engine.resolve_for_pane(
            pane.id.0,
            pane.window_id.0,
            pane.session_id.0,
        );

        // Render border with pane-specific theme.
        self.render_pane_border(
            rect,
            focused,
            &resolved,
            pane.title.as_deref(),
            buf,
        );

        // Render pane content with pane-specific background.
        self.render_pane_content(
            pane,
            rect,
            &resolved,
            buf,
        );
    }

    /// Render a pane's border using its resolved theme.
    fn render_pane_border(
        &self,
        rect: &PaneRect,
        focused: bool,
        theme: &ResolvedTheme,
        title: Option<&str>,
        buf: &mut RenderBuffer,
    ) {
        let border_color = theme.border_color(focused);
        let border_style = self.config.ui.pane_border_style.as_str();

        let (tl, tr, bl, br, h, v) = match border_style {
            "rounded" => ('\u{256d}', '\u{256e}', '\u{2570}', '\u{256f}', '\u{2500}', '\u{2502}'),
            "thin" => ('\u{250c}', '\u{2510}', '\u{2514}', '\u{2518}', '\u{2500}', '\u{2502}'),
            "thick" => ('\u{250f}', '\u{2513}', '\u{2517}', '\u{251b}', '\u{2501}', '\u{2503}'),
            "double" => ('\u{2554}', '\u{2557}', '\u{255a}', '\u{255d}', '\u{2550}', '\u{2551}'),
            "none" => return,
            _ => ('\u{256d}', '\u{256e}', '\u{2570}', '\u{256f}', '\u{2500}', '\u{2502}'),
        };

        let color = border_color.to_crossterm();

        // Draw top border with optional title.
        buf.set_cell(rect.x, rect.y, tl, color, None);
        let title_space = rect.width.saturating_sub(2) as usize;
        if let Some(title) = title {
            if self.config.ui.show_pane_titles && !title.is_empty() {
                let truncated: String = title.chars().take(title_space.saturating_sub(2)).collect();
                let title_str = format!(" {} ", truncated);
                let title_len = title_str.len();
                // Draw title.
                for (i, ch) in title_str.chars().enumerate() {
                    buf.set_cell(rect.x + 1 + i as u16, rect.y, ch, color, None);
                }
                // Fill remaining with horizontal line.
                for i in (1 + title_len as u16)..rect.width.saturating_sub(1) {
                    buf.set_cell(rect.x + i, rect.y, h, color, None);
                }
            } else {
                for i in 1..rect.width.saturating_sub(1) {
                    buf.set_cell(rect.x + i, rect.y, h, color, None);
                }
            }
        } else {
            for i in 1..rect.width.saturating_sub(1) {
                buf.set_cell(rect.x + i, rect.y, h, color, None);
            }
        }
        buf.set_cell(rect.x + rect.width - 1, rect.y, tr, color, None);

        // Draw side borders.
        for row in 1..rect.height.saturating_sub(1) {
            buf.set_cell(rect.x, rect.y + row, v, color, None);
            buf.set_cell(rect.x + rect.width - 1, rect.y + row, v, color, None);
        }

        // Draw bottom border.
        buf.set_cell(rect.x, rect.y + rect.height - 1, bl, color, None);
        for i in 1..rect.width.saturating_sub(1) {
            buf.set_cell(rect.x + i, rect.y + rect.height - 1, h, color, None);
        }
        buf.set_cell(rect.x + rect.width - 1, rect.y + rect.height - 1, br, color, None);
    }

    /// Render the status bar, reflecting the active pane's theme.
    fn render_status_bar(
        &self,
        active_pane: &PaneState,
        theme_engine: &ThemeEngine,
        buf: &mut RenderBuffer,
    ) {
        let resolved = theme_engine.resolve_for_pane(
            active_pane.id.0,
            active_pane.window_id.0,
            active_pane.session_id.0,
        );

        let bg = resolved.tokens.status_bar_bg.to_crossterm();
        let fg = resolved.tokens.status_bar_fg.to_crossterm();
        let accent = resolved.tokens.status_bar_accent.to_crossterm();

        // Fill status bar background.
        let row = self.status_bar_row();
        for col in 0..self.terminal_width {
            buf.set_cell(col, row, ' ', fg, Some(bg));
        }

        // Render status segments with theme colors.
        // ... (session name, window list, clock, etc.)
    }
}
```

### Step 4: Implement interactive theme picker (Prefix + t)

When the user presses `Prefix + t`, show an interactive theme picker overlay listing all available themes. Selecting a theme applies it to the current pane.

```rust
// crates/shux-ui/src/theme_picker.rs

use crossterm::event::{KeyCode, KeyEvent};

pub struct ThemePicker {
    /// Available themes.
    themes: Vec<ThemePickerEntry>,
    /// Currently selected index.
    selected: usize,
    /// Filter/search text.
    filter: String,
    /// Filtered indices.
    filtered: Vec<usize>,
    /// The pane to apply the theme to.
    target_pane_id: Uuid,
}

#[derive(Debug, Clone)]
pub struct ThemePickerEntry {
    pub name: String,
    pub display_name: String,
    pub variant: String,
    pub source: String,
}

impl ThemePicker {
    pub fn new(themes: Vec<ThemePickerEntry>, target_pane_id: Uuid) -> Self {
        let filtered: Vec<usize> = (0..themes.len()).collect();
        Self {
            themes,
            selected: 0,
            filter: String::new(),
            filtered,
            target_pane_id,
        }
    }

    pub fn process_key(&mut self, key: KeyEvent) -> ThemePickerResult {
        match key.code {
            KeyCode::Esc => ThemePickerResult::Cancel,
            KeyCode::Enter => {
                if let Some(&idx) = self.filtered.get(self.selected) {
                    ThemePickerResult::Select(self.themes[idx].name.clone())
                } else {
                    ThemePickerResult::Cancel
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
                ThemePickerResult::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected < self.filtered.len().saturating_sub(1) {
                    self.selected += 1;
                }
                ThemePickerResult::Continue
            }
            KeyCode::Char(c) => {
                self.filter.push(c);
                self.update_filter();
                ThemePickerResult::Continue
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.update_filter();
                ThemePickerResult::Continue
            }
            _ => ThemePickerResult::Continue,
        }
    }

    fn update_filter(&mut self) {
        let query = self.filter.to_lowercase();
        self.filtered = self.themes.iter().enumerate()
            .filter(|(_, t)| {
                t.name.to_lowercase().contains(&query)
                    || t.display_name.to_lowercase().contains(&query)
            })
            .map(|(i, _)| i)
            .collect();
        self.selected = 0;
    }

    /// Render the theme picker as a centered overlay.
    pub fn render(&self, area_width: u16, area_height: u16) -> ThemePickerFrame {
        let max_visible = 10.min(self.filtered.len());
        let box_width = 40.min(area_width.saturating_sub(4) as usize);
        let box_height = max_visible + 4; // header + filter + items + footer
        let x = (area_width as usize - box_width) / 2;
        let y = (area_height as usize - box_height) / 2;

        let items: Vec<String> = self.filtered.iter()
            .take(max_visible)
            .enumerate()
            .map(|(i, &idx)| {
                let theme = &self.themes[idx];
                let marker = if i == self.selected { ">" } else { " " };
                let variant = match theme.variant.as_str() {
                    "dark" => "D",
                    "light" => "L",
                    _ => "?",
                };
                format!("{} [{}] {}", marker, variant, theme.display_name)
            })
            .collect();

        ThemePickerFrame {
            x: x as u16,
            y: y as u16,
            width: box_width as u16,
            height: box_height as u16,
            title: "Set Pane Theme".to_string(),
            filter: self.filter.clone(),
            items,
            total: self.filtered.len(),
        }
    }
}

pub enum ThemePickerResult {
    /// Continue showing the picker.
    Continue,
    /// User selected a theme.
    Select(String),
    /// User cancelled.
    Cancel,
}

pub struct ThemePickerFrame {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub title: String,
    pub filter: String,
    pub items: Vec<String>,
    pub total: usize,
}
```

### Step 5: Wire Prefix + t to the theme picker

In `prefix_actions.rs`, the `SetPaneTheme` action opens the theme picker.

```rust
// In crates/shux-ui/src/prefix_actions.rs

PrefixAction::SetPaneTheme => {
    let themes: Vec<ThemePickerEntry> = ctx.theme_engine.list_themes()
        .into_iter()
        .map(|meta| ThemePickerEntry {
            name: meta.name.clone(),
            display_name: meta.display_name.clone()
                .unwrap_or_else(|| meta.name.clone()),
            variant: format!("{:?}", meta.variant).to_lowercase(),
            source: "registered".to_string(),
        })
        .collect();

    let picker = ThemePicker::new(themes, ctx.active_pane_id());
    ctx.enter_overlay(Overlay::ThemePicker(picker));
}
```

### Step 6: Implement the demo scenario

Create a verification scenario that demonstrates the key differentiator: two panes in the same window with different themes.

```rust
// Demo setup (can be scripted via API or manually):
//
// 1. Create a session with two panes (vertical split).
// 2. Set the left pane to a theme with red accent (production).
// 3. Set the right pane to a theme with green accent (development).
// 4. Observe: borders, status bar, and background colors differ per-pane.
//
// API calls:
// pane.set_theme_override {pane_id: LEFT, overrides: {accent_primary: "#ff6b6b", border_focused: "#ff6b6b"}}
// pane.set_theme_override {pane_id: RIGHT, overrides: {accent_primary: "#51cf66", border_focused: "#51cf66"}}
```

### Step 7: Handle pane lifecycle and theme cleanup

When a pane is closed, its theme entry must be cleaned up in the ThemeEngine.

```rust
// In the pane close handler:

pub fn close_pane(
    &mut self,
    pane_id: PaneId,
    theme_engine: &mut ThemeEngine,
) -> Result<(), GraphError> {
    // Clean up theme before removing the pane.
    theme_engine.clear_pane_theme(&pane_id.0);

    // ... existing pane close logic ...
}
```

### Step 8: Propagate theme changes to render

When a pane's theme changes (via API or picker), the compositor must be notified to re-render. This happens through the event bus.

```rust
// The event loop listens for PaneThemeChanged events:

Event::PaneThemeChanged { pane_id, .. } => {
    // Trigger a full redraw -- the compositor will re-resolve themes.
    ctx.request_redraw();
}
```

---

## Verification

### Functional

```bash
# Build
cargo build --workspace

# Start a session with multiple panes
cargo run -p shux -- new -s test
# Split: Ctrl+Space then |

# Set different themes on each pane via API:
# Left pane:
shux api pane.set_theme_override '{"pane_id": "<LEFT_ID>", "overrides": {"accent_primary": "#ff6b6b", "border_focused": "#ff6b6b", "bg_subtle": "#2d1f1f"}}'

# Right pane:
shux api pane.set_theme_override '{"pane_id": "<RIGHT_ID>", "overrides": {"accent_primary": "#51cf66", "border_focused": "#51cf66", "bg_subtle": "#1f2d1f"}}'

# Verify:
# 1. Left pane has red border, right pane has green border
# 2. When focusing left pane, status bar shows red accent
# 3. When focusing right pane, status bar shows green accent
# 4. Each pane's content background has its own subtle tint

# Set a named theme on a pane:
shux api pane.set_theme '{"pane_id": "<LEFT_ID>", "theme_name": "default-light"}'
# Left pane should switch to light theme while right stays dark

# Clear pane theme:
shux api pane.set_theme '{"pane_id": "<LEFT_ID>", "theme_name": ""}'
# Left pane should revert to global theme

# Use the theme picker:
# Press Ctrl+Space then t
# Overlay appears with theme list
# Navigate with j/k, type to filter, Enter to apply, Esc to cancel
```

### Tests

```bash
# Unit tests
cargo nextest run -p shux-core --test per_pane_theme_test
cargo nextest run -p shux-ui --lib theme_picker

# Integration tests
cargo nextest run -p shux-rpc --lib theme

# Test scenarios:
# - pane.set_theme applies named theme to pane
# - pane.set_theme with empty name clears pane theme
# - pane.set_theme_override applies individual token overrides
# - Invalid theme name returns error with available themes listed
# - Invalid token name returns error with valid tokens listed
# - Theme picker lists all themes, filters by name
# - Theme picker applies selected theme to active pane
# - Border color reflects per-pane theme (focused and unfocused)
# - Status bar reflects active pane's theme
# - Theme cleanup on pane close
# - Cascade resolution: pane override > window > session > global
# - Theme change triggers re-render
```

### L4 Visual Regression — iterm2-driver (PRD §16.2)

Create `.claude/automations/test_theming_visual.py` to verify per-pane theming renders correctly:

```python
# /// script
# requires-python = ">=3.14"
# dependencies = ["iterm2", "pyobjc", "pyobjc-framework-Quartz"]
# ///
"""
shux Per-Pane Theming Visual Test (iterm2-driver)

Tests:
1. Launch shux, split into 2 panes
2. Set left pane theme to "prod" (red accent) via CLI/API
3. Set right pane theme to "default-light" via CLI/API
4. Verify screen content changed after each theme set
5. Verify left pane border uses red accent color
6. Verify right pane has different visual appearance
7. Take screenshot: theming_prod_vs_light.png
8. Open theme picker (Ctrl+Space t) on left pane
9. Verify picker overlay with theme list
10. Take screenshot: theming_picker.png
11. Press Escape to close picker
12. Clear left pane theme, verify revert to default

Verification Strategy:
- Read screen content before/after theme changes
- Assert visual difference between panes (content changes)
- Verify theme picker overlay appears/disappears

Usage:
    uv run .claude/automations/test_theming_visual.py
"""
```

Run: `uv run .claude/automations/test_theming_visual.py`

This is the key visual differentiator for shux (PRD §2.4 point 4). The screenshot
`theming_prod_vs_light.png` should be usable in documentation/README.

---

## Completion Criteria

- [ ] `pane.set_theme` API method sets a named theme on a specific pane
- [ ] `pane.set_theme_override` API method applies individual token overrides to a pane
- [ ] Border colors reflect per-pane theme (focused border from pane's resolved theme)
- [ ] Status bar reflects the active pane's resolved theme
- [ ] Compositor uses pane-resolved themes when rendering each pane's content area
- [ ] Theme picker (Prefix + t) shows interactive overlay with search/filter
- [ ] Theme picker applies selected theme to the current pane
- [ ] Pane theme cleared on pane close (no leaked theme entries)
- [ ] Demo works: two panes in same window with visually distinct themes
- [ ] SRE use case: prod pane with red accent, dev pane with green accent
- [ ] ThemeEngine cascade includes pane-level resolution (built-in -> global -> session -> window -> pane)
- [ ] Invalid theme name returns actionable error with available theme list
- [ ] Invalid token name returns actionable error with valid token list
- [ ] Theme change event emitted and triggers re-render
- [ ] Unit and integration tests cover all API methods and visual rendering paths
- [ ] Per-pane theming works correctly with multiple simultaneous themes across panes

---

## Commit Message

```
feat(ui): implement per-pane theming — key differentiator

- pane.set_theme API: apply named theme to individual panes (PRD §6.1)
- pane.set_theme_override API: override individual tokens per-pane
- Compositor renders each pane with its own resolved theme
- Border colors reflect per-pane theme (focused/unfocused)
- Status bar reflects active pane's resolved theme
- Interactive theme picker via Prefix+t with search/filter
- Theme cleanup on pane close
- Demo: prod (red) vs dev (green) panes in same window
```

---

## Session Protocol

1. **Before starting:** Read task 024 (theme engine) to understand ThemeEngine, ResolvedTheme, and cascade resolution. Read task 017 (multi-pane rendering) to understand how the compositor renders pane borders and content. Read PRD SS 2.4 point 4 for the vision.
2. **During:** Implement in order: data model wiring (Step 1), API methods (Step 2), compositor changes (Step 3), theme picker (Step 4), prefix wiring (Step 5), demo scenario (Step 6), lifecycle cleanup (Step 7), event propagation (Step 8). Run `cargo check` after each step. Visually verify per-pane theming after Steps 3 and 6.
3. **After:** Run full verification suite. Take screenshots of the prod/dev demo for documentation. Update `docs/PROGRESS.md` (mark 025 done). Update `CLAUDE.md` Learnings with any rendering/compositing insights.
