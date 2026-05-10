# 033 — Help Overlay (Keybinding Cheat Sheet)

**Status:** Done (PR #6, 2026-05-09). `Prefix + ?` opens a keybinding cheat-sheet overlay rendered above the active layout. Implementation in `crates/shux-ui/src/help_overlay.rs`. Verified via `.claude/automations/test_033_help_overlay.py`.
**Depends On:** 032
**Parallelizable With:** —

---

## Problem

Even with a command palette, users need a quick reference for keybindings. The help overlay provides an at-a-glance cheat sheet of all keybindings, organized by category. Unlike the command palette (which is action-oriented: "what do I want to do?"), the help overlay is key-oriented: "what does this key do?" and "what keys are available?"

`Prefix + ?` shows a full-screen overlay listing all keybindings grouped by category (Navigation, Windows, Panes, Copy, Config, Session, General). The overlay is searchable (type to filter) and contextual (can show different hints based on the current mode: normal, copy, prefix). Dismissed with Escape or `q`.

This overlay is essential for onboarding: a new user can press `Ctrl+Space ?` at any time to see everything they can do. It also serves as documentation for customized keybindings, since it reflects the actual current bindings (including user overrides).

## PRD Reference

- **section 6.1** P0 feature matrix, UX & keybindings: discoverability overlay (`Prefix + ?`)
- **section 9.2** Tier 2: keybinding help overlay shortcut
- **section 1** design philosophy: "it just works" defaults and discoverability

---

## Files to Create

- `crates/shux-ui/src/help_overlay.rs` — Help overlay: rendering, search, category organization

## Files to Modify

- `crates/shux-ui/src/lib.rs` — Add `pub mod help_overlay;`
- `crates/shux-ui/src/compositor.rs` — Render help overlay as full-screen overlay layer
- `crates/shux-ui/src/input.rs` — Route input to help overlay when open

---

## Execution Steps

### Step 1: Define Help Overlay Data Model

Create `crates/shux-ui/src/help_overlay.rs`:

```rust
//! Help overlay — keybinding cheat sheet.
//!
//! Activated by Prefix + ? (default Ctrl+Space ?).
//! Shows a full-screen overlay listing all keybindings organized by category.
//! Searchable: type to filter keybindings.
//! Dismissed with Escape or q.

use shux_core::keybinding::{
    Binding, BindingCategory, BindingSource, KeybindingRegistry,
};
use crossterm::style::Color;

/// A keybinding entry for display in the help overlay.
#[derive(Debug, Clone)]
pub struct HelpEntry {
    /// Human-readable key combo (e.g., "Alt+H", "Ctrl+Space C")
    pub key_display: String,
    /// Action description (e.g., "Focus pane to the left")
    pub description: String,
    /// Source indicator (e.g., "[built-in]", "[user]", "[plugin:name]")
    pub source_display: String,
    /// Category for grouping
    pub category: BindingCategory,
    /// Whether this binding has been customized from its default
    pub customized: bool,
}

/// A category section in the help overlay.
#[derive(Debug, Clone)]
pub struct HelpSection {
    /// Category name for the header
    pub name: String,
    /// Entries in this category
    pub entries: Vec<HelpEntry>,
}

/// The current mode context affects which bindings are shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpContext {
    /// Normal mode — show all standard keybindings
    Normal,
    /// Copy mode — show copy-specific keybindings
    CopyMode,
    /// Prefix mode — show Tier 2 keybindings (after prefix key)
    PrefixMode,
}

/// The state of the help overlay.
pub struct HelpOverlay {
    /// Whether the overlay is currently visible
    visible: bool,
    /// Current search query
    query: String,
    /// Current context (affects which bindings are highlighted)
    context: HelpContext,
    /// All sections (populated when overlay opens)
    all_sections: Vec<HelpSection>,
    /// Filtered sections (matching current query)
    filtered_sections: Vec<HelpSection>,
    /// Scroll position (line offset from top)
    scroll_offset: usize,
    /// Total number of visible lines (for scroll bounds)
    total_lines: usize,
    /// Terminal height (for page calculations)
    viewport_height: usize,
}

impl HelpOverlay {
    pub fn new() -> Self {
        Self {
            visible: false,
            query: String::new(),
            context: HelpContext::Normal,
            all_sections: Vec::new(),
            filtered_sections: Vec::new(),
            scroll_offset: 0,
            total_lines: 0,
            viewport_height: 0,
        }
    }

    /// Open the help overlay with the given context.
    pub fn open(&mut self, registry: &KeybindingRegistry, context: HelpContext) {
        self.visible = true;
        self.query.clear();
        self.scroll_offset = 0;
        self.context = context;

        // Build sections from the keybinding registry
        self.all_sections = Self::build_sections(registry, context);
        self.filtered_sections = self.all_sections.clone();
        self.recalculate_total_lines();
    }

    /// Close the help overlay.
    pub fn close(&mut self) {
        self.visible = false;
        self.query.clear();
    }

    /// Whether the overlay is currently visible.
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Build organized sections from the keybinding registry.
    fn build_sections(
        registry: &KeybindingRegistry,
        context: HelpContext,
    ) -> Vec<HelpSection> {
        let bindings = registry.list_all();

        // Define category display order
        let category_order = [
            (BindingCategory::Navigation, "Navigation"),
            (BindingCategory::Panes, "Panes"),
            (BindingCategory::Windows, "Windows"),
            (BindingCategory::Session, "Session"),
            (BindingCategory::Copy, "Copy Mode"),
            (BindingCategory::Config, "Configuration"),
            (BindingCategory::General, "General"),
            (BindingCategory::Plugin, "Plugins"),
        ];

        let mut sections = Vec::new();

        for (category, name) in &category_order {
            let entries: Vec<HelpEntry> = bindings
                .iter()
                .filter(|b| &b.category == category)
                .map(|b| HelpEntry {
                    key_display: b.key.to_string(),
                    description: b.description.clone(),
                    source_display: format_source(&b.source),
                    category: b.category.clone(),
                    customized: !matches!(b.source, BindingSource::BuiltIn),
                })
                .collect();

            if !entries.is_empty() {
                sections.push(HelpSection {
                    name: name.to_string(),
                    entries,
                });
            }
        }

        // Add contextual hint section
        match context {
            HelpContext::CopyMode => {
                sections.insert(
                    0,
                    HelpSection {
                        name: "Copy Mode (active)".to_string(),
                        entries: Self::copy_mode_entries(),
                    },
                );
            }
            HelpContext::PrefixMode => {
                // Highlight prefix bindings at the top
            }
            HelpContext::Normal => {}
        }

        sections
    }

    /// Copy mode-specific keybinding entries.
    fn copy_mode_entries() -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key_display: "h/j/k/l".into(),
                description: "Move cursor left/down/up/right".into(),
                source_display: "[built-in]".into(),
                category: BindingCategory::Copy,
                customized: false,
            },
            HelpEntry {
                key_display: "v".into(),
                description: "Begin selection".into(),
                source_display: "[built-in]".into(),
                category: BindingCategory::Copy,
                customized: false,
            },
            HelpEntry {
                key_display: "V".into(),
                description: "Select entire line".into(),
                source_display: "[built-in]".into(),
                category: BindingCategory::Copy,
                customized: false,
            },
            HelpEntry {
                key_display: "y".into(),
                description: "Copy selection to clipboard".into(),
                source_display: "[built-in]".into(),
                category: BindingCategory::Copy,
                customized: false,
            },
            HelpEntry {
                key_display: "/".into(),
                description: "Search forward".into(),
                source_display: "[built-in]".into(),
                category: BindingCategory::Copy,
                customized: false,
            },
            HelpEntry {
                key_display: "?".into(),
                description: "Search backward".into(),
                source_display: "[built-in]".into(),
                category: BindingCategory::Copy,
                customized: false,
            },
            HelpEntry {
                key_display: "n/N".into(),
                description: "Next/previous search match".into(),
                source_display: "[built-in]".into(),
                category: BindingCategory::Copy,
                customized: false,
            },
            HelpEntry {
                key_display: "Esc / q".into(),
                description: "Exit copy mode".into(),
                source_display: "[built-in]".into(),
                category: BindingCategory::Copy,
                customized: false,
            },
        ]
    }

    /// Handle a key event while the help overlay is open.
    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> HelpAction {
        use crossterm::event::{KeyCode, KeyModifiers};

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') if self.query.is_empty() => {
                self.close();
                HelpAction::Dismiss
            }

            // Scroll
            KeyCode::Down | KeyCode::Char('j') if self.query.is_empty() => {
                self.scroll_down(1);
                HelpAction::Consumed
            }
            KeyCode::Up | KeyCode::Char('k') if self.query.is_empty() => {
                self.scroll_up(1);
                HelpAction::Consumed
            }
            KeyCode::PageDown | KeyCode::Char('d')
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.scroll_down(self.viewport_height / 2);
                HelpAction::Consumed
            }
            KeyCode::PageUp | KeyCode::Char('u')
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.scroll_up(self.viewport_height / 2);
                HelpAction::Consumed
            }
            KeyCode::Home | KeyCode::Char('g') if self.query.is_empty() => {
                self.scroll_offset = 0;
                HelpAction::Consumed
            }
            KeyCode::End | KeyCode::Char('G') if self.query.is_empty() => {
                self.scroll_offset = self.total_lines.saturating_sub(self.viewport_height);
                HelpAction::Consumed
            }

            // Search
            KeyCode::Char('/') if self.query.is_empty() => {
                // Enter search mode (query becomes active)
                self.query = "/".to_string();
                HelpAction::Consumed
            }
            KeyCode::Char(ch) if !self.query.is_empty() => {
                self.query.push(ch);
                self.update_filter();
                HelpAction::Consumed
            }
            KeyCode::Backspace if !self.query.is_empty() => {
                self.query.pop();
                if self.query == "/" {
                    self.query.clear();
                    self.filtered_sections = self.all_sections.clone();
                } else {
                    self.update_filter();
                }
                HelpAction::Consumed
            }
            KeyCode::Esc if !self.query.is_empty() => {
                self.query.clear();
                self.filtered_sections = self.all_sections.clone();
                self.recalculate_total_lines();
                HelpAction::Consumed
            }

            _ => HelpAction::Consumed,
        }
    }

    fn scroll_down(&mut self, lines: usize) {
        let max = self.total_lines.saturating_sub(self.viewport_height);
        self.scroll_offset = (self.scroll_offset + lines).min(max);
    }

    fn scroll_up(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    /// Update filtered sections based on the search query.
    fn update_filter(&mut self) {
        let search_term = self.query.trim_start_matches('/').to_lowercase();
        if search_term.is_empty() {
            self.filtered_sections = self.all_sections.clone();
        } else {
            self.filtered_sections = self
                .all_sections
                .iter()
                .filter_map(|section| {
                    let filtered_entries: Vec<HelpEntry> = section
                        .entries
                        .iter()
                        .filter(|e| {
                            e.key_display.to_lowercase().contains(&search_term)
                                || e.description.to_lowercase().contains(&search_term)
                        })
                        .cloned()
                        .collect();

                    if filtered_entries.is_empty() {
                        None
                    } else {
                        Some(HelpSection {
                            name: section.name.clone(),
                            entries: filtered_entries,
                        })
                    }
                })
                .collect();
        }
        self.scroll_offset = 0;
        self.recalculate_total_lines();
    }

    fn recalculate_total_lines(&mut self) {
        self.total_lines = self
            .filtered_sections
            .iter()
            .map(|s| 2 + s.entries.len()) // header + blank line + entries
            .sum::<usize>()
            + 2; // top/bottom padding
    }
}

/// Actions the help overlay can produce.
#[derive(Debug, Clone)]
pub enum HelpAction {
    /// Dismiss the overlay
    Dismiss,
    /// Input consumed (redraw needed)
    Consumed,
}

/// Format a BindingSource for display.
fn format_source(source: &BindingSource) -> String {
    match source {
        BindingSource::BuiltIn => String::new(), // Don't clutter with "[built-in]"
        BindingSource::User => "[user]".into(),
        BindingSource::Plugin(id) => format!("[{id}]"),
        BindingSource::Runtime => "[runtime]".into(),
    }
}
```

### Step 2: Implement Help Overlay Rendering

```rust
impl HelpOverlay {
    /// Render the help overlay as a full-screen overlay.
    pub fn render(
        &mut self,
        terminal_width: u16,
        terminal_height: u16,
        theme: &HelpTheme,
    ) -> HelpRenderOutput {
        if !self.visible {
            return HelpRenderOutput::empty();
        }

        self.viewport_height = (terminal_height as usize).saturating_sub(4);

        let mut output = HelpRenderOutput::new(terminal_width, terminal_height);

        // Fill background
        output.fill_background(theme.bg);

        // Header: " shux — Keybinding Reference  (/ to search, q to close) "
        let header = if self.query.is_empty() {
            " shux - Keybinding Reference    / search  q close  j/k scroll "
        } else {
            &format!(" Search: {}_ ", &self.query[1..]) // Strip leading /
        };
        output.draw_header(header, theme.header_fg, theme.header_bg);

        // Context indicator
        let context_str = match self.context {
            HelpContext::Normal => "Mode: Normal",
            HelpContext::CopyMode => "Mode: Copy",
            HelpContext::PrefixMode => "Mode: Prefix",
        };
        output.draw_right_header(context_str, theme.context_fg, theme.header_bg);

        // Render sections
        let mut line = 2_usize; // Start after header + gap
        let visible_start = self.scroll_offset;
        let visible_end = self.scroll_offset + self.viewport_height;
        let mut current_line = 0_usize;

        for section in &self.filtered_sections {
            // Section header
            if current_line >= visible_start && current_line < visible_end {
                let row = (current_line - visible_start + 2) as u16;
                output.draw_section_header(
                    row,
                    &section.name,
                    theme.section_fg,
                    theme.bg,
                );
            }
            current_line += 1;

            // Entries
            let key_col_width = 20u16; // Fixed width for key column

            for entry in &section.entries {
                if current_line >= visible_start && current_line < visible_end {
                    let row = (current_line - visible_start + 2) as u16;

                    // Key combo (left column)
                    let key_fg = if entry.customized {
                        theme.customized_fg
                    } else {
                        theme.key_fg
                    };

                    output.draw_text(
                        2,
                        row,
                        &entry.key_display,
                        key_fg,
                        theme.bg,
                        key_col_width,
                    );

                    // Description (middle column)
                    let desc_col = 2 + key_col_width + 2;
                    let desc_width = terminal_width.saturating_sub(desc_col + 15);
                    output.draw_text(
                        desc_col,
                        row,
                        &entry.description,
                        theme.desc_fg,
                        theme.bg,
                        desc_width,
                    );

                    // Source indicator (right column, if not built-in)
                    if !entry.source_display.is_empty() {
                        let source_col = terminal_width.saturating_sub(
                            entry.source_display.len() as u16 + 2,
                        );
                        output.draw_text(
                            source_col,
                            row,
                            &entry.source_display,
                            theme.source_fg,
                            theme.bg,
                            entry.source_display.len() as u16,
                        );
                    }
                }
                current_line += 1;
            }

            // Blank line between sections
            current_line += 1;
        }

        // Footer: scroll indicator
        if self.total_lines > self.viewport_height {
            let scroll_pct = if self.total_lines > 0 {
                (self.scroll_offset * 100) / self.total_lines.saturating_sub(self.viewport_height)
            } else {
                0
            };
            let footer = format!("  {:3}%  ", scroll_pct.min(100));
            output.draw_footer(&footer, theme.footer_fg, theme.header_bg);
        }

        output
    }
}

/// Theme tokens for the help overlay.
pub struct HelpTheme {
    pub bg: Color,
    pub header_fg: Color,
    pub header_bg: Color,
    pub context_fg: Color,
    pub section_fg: Color,
    pub key_fg: Color,
    pub customized_fg: Color,
    pub desc_fg: Color,
    pub source_fg: Color,
    pub footer_fg: Color,
}

/// Render output from the help overlay.
pub struct HelpRenderOutput {
    pub width: u16,
    pub height: u16,
    pub cells: Vec<(u16, u16, char, Color, Color)>,
}

impl HelpRenderOutput {
    pub fn empty() -> Self {
        Self { width: 0, height: 0, cells: Vec::new() }
    }

    pub fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            cells: Vec::with_capacity((width as usize) * (height as usize)),
        }
    }

    pub fn fill_background(&mut self, bg: Color) {
        for row in 0..self.height {
            for col in 0..self.width {
                self.cells.push((col, row, ' ', bg, bg));
            }
        }
    }

    pub fn draw_header(&mut self, text: &str, fg: Color, bg: Color) {
        for (i, ch) in text.chars().enumerate() {
            if (i as u16) < self.width {
                self.cells.push((i as u16, 0, ch, fg, bg));
            }
        }
        // Fill rest of header line
        for col in text.len() as u16..self.width {
            self.cells.push((col, 0, ' ', fg, bg));
        }
    }

    pub fn draw_right_header(&mut self, text: &str, fg: Color, bg: Color) {
        let start_col = self.width.saturating_sub(text.len() as u16 + 2);
        for (i, ch) in text.chars().enumerate() {
            self.cells.push((start_col + i as u16, 0, ch, fg, bg));
        }
    }

    pub fn draw_section_header(&mut self, row: u16, title: &str, fg: Color, bg: Color) {
        // Format: "-- Category Name --"
        let header = format!("-- {} ", title);
        for (i, ch) in header.chars().enumerate() {
            if (i as u16) + 2 < self.width {
                self.cells.push((i as u16 + 2, row, ch, fg, bg));
            }
        }
        // Extend with dashes
        let dash_start = header.len() as u16 + 2;
        for col in dash_start..self.width.saturating_sub(2) {
            self.cells.push((col, row, '-', fg, bg));
        }
    }

    pub fn draw_text(
        &mut self,
        col: u16,
        row: u16,
        text: &str,
        fg: Color,
        bg: Color,
        max_width: u16,
    ) {
        for (i, ch) in text.chars().take(max_width as usize).enumerate() {
            if col + i as u16 < self.width {
                self.cells.push((col + i as u16, row, ch, fg, bg));
            }
        }
    }

    pub fn draw_footer(&mut self, text: &str, fg: Color, bg: Color) {
        let row = self.height.saturating_sub(1);
        for (i, ch) in text.chars().enumerate() {
            if (i as u16) < self.width {
                self.cells.push((i as u16, row, ch, fg, bg));
            }
        }
        for col in text.len() as u16..self.width {
            self.cells.push((col, row, ' ', fg, bg));
        }
    }
}
```

### Step 3: Integrate with Compositor and Input Router

Modify `crates/shux-ui/src/compositor.rs`:

```rust
impl Compositor {
    pub fn render(&mut self, ctx: &RenderContext) -> Result<()> {
        let _guard = SyncGuard::new(&mut self.stdout, self.synchronized_output)?;

        self.render_inner(ctx)?;

        // Render overlays in z-order: help overlay on top of command palette
        if self.help_overlay.is_visible() {
            let theme = HelpTheme::from_resolved_theme(&ctx.theme);
            let output = self.help_overlay.render(
                ctx.terminal_size.0,
                ctx.terminal_size.1,
                &theme,
            );
            self.apply_fullscreen_overlay(&output)?;
            // Hide cursor in help overlay (no text input)
            crossterm::execute!(self.stdout, crossterm::cursor::Hide)?;
        } else if self.command_palette.is_visible() {
            // ... command palette rendering (from task 032) ...
        }

        Ok(())
    }
}
```

Modify `crates/shux-ui/src/input.rs`:

```rust
pub fn handle_input(&mut self, event: crossterm::event::Event) -> Option<Action> {
    // Priority: help overlay > command palette > normal input
    if self.help_overlay.is_visible() {
        if let crossterm::event::Event::Key(key) = event {
            return match self.help_overlay.handle_key(key) {
                HelpAction::Dismiss => Some(Action::Redraw),
                HelpAction::Consumed => Some(Action::Redraw),
            };
        }
        return None;
    }

    if self.command_palette.is_visible() {
        // ... (from task 032)
    }

    // Normal input processing
    // ...
}
```

### Step 4: Add Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use shux_core::keybinding::KeybindingRegistry;

    fn setup_overlay() -> HelpOverlay {
        let mut overlay = HelpOverlay::new();
        let registry = KeybindingRegistry::new();
        overlay.open(&registry, HelpContext::Normal);
        overlay
    }

    #[test]
    fn test_help_opens_with_sections() {
        let overlay = setup_overlay();
        assert!(overlay.is_visible());
        assert!(!overlay.all_sections.is_empty());
    }

    #[test]
    fn test_help_closes_on_q() {
        let mut overlay = setup_overlay();
        let result = overlay.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('q'),
            crossterm::event::KeyModifiers::empty(),
        ));
        assert!(matches!(result, HelpAction::Dismiss));
        assert!(!overlay.is_visible());
    }

    #[test]
    fn test_help_closes_on_escape() {
        let mut overlay = setup_overlay();
        let result = overlay.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert!(matches!(result, HelpAction::Dismiss));
    }

    #[test]
    fn test_help_scroll_down() {
        let mut overlay = setup_overlay();
        assert_eq!(overlay.scroll_offset, 0);

        overlay.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('j'),
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(overlay.scroll_offset, 1);
    }

    #[test]
    fn test_help_scroll_up_at_top() {
        let mut overlay = setup_overlay();
        overlay.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('k'),
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(overlay.scroll_offset, 0); // Cannot go below 0
    }

    #[test]
    fn test_help_search_filters() {
        let mut overlay = setup_overlay();
        let initial_sections = overlay.filtered_sections.len();

        // Enter search mode with /
        overlay.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('/'),
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(overlay.query, "/");

        // Type "focus"
        for ch in "focus".chars() {
            overlay.handle_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(ch),
                crossterm::event::KeyModifiers::empty(),
            ));
        }
        assert_eq!(overlay.query, "/focus");

        // Filtered sections should contain only matching entries
        let matching_entries: usize = overlay
            .filtered_sections
            .iter()
            .map(|s| s.entries.len())
            .sum();
        assert!(matching_entries > 0);

        // All matching entries should contain "focus"
        for section in &overlay.filtered_sections {
            for entry in &section.entries {
                assert!(
                    entry.key_display.to_lowercase().contains("focus")
                        || entry.description.to_lowercase().contains("focus"),
                    "Entry does not match filter: {:?}",
                    entry
                );
            }
        }
    }

    #[test]
    fn test_help_search_escape_clears() {
        let mut overlay = setup_overlay();

        // Enter search and type
        overlay.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('/'),
            crossterm::event::KeyModifiers::empty(),
        ));
        overlay.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('x'),
            crossterm::event::KeyModifiers::empty(),
        ));
        assert!(!overlay.query.is_empty());

        // Escape clears search (but doesn't close overlay)
        overlay.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert!(overlay.query.is_empty());
        assert!(overlay.is_visible()); // Still open
    }

    #[test]
    fn test_help_copy_mode_context() {
        let mut overlay = HelpOverlay::new();
        let registry = KeybindingRegistry::new();
        overlay.open(&registry, HelpContext::CopyMode);

        // Should have a "Copy Mode (active)" section first
        assert_eq!(overlay.all_sections[0].name, "Copy Mode (active)");
        assert!(!overlay.all_sections[0].entries.is_empty());
    }

    #[test]
    fn test_help_sections_ordered() {
        let overlay = setup_overlay();

        // Verify sections appear in expected order
        let section_names: Vec<&str> = overlay
            .all_sections
            .iter()
            .map(|s| s.name.as_str())
            .collect();

        // Navigation should appear before Plugins
        if let (Some(nav_pos), Some(plugin_pos)) = (
            section_names.iter().position(|&n| n == "Navigation"),
            section_names.iter().position(|&n| n == "Plugins"),
        ) {
            assert!(nav_pos < plugin_pos);
        }
    }

    #[test]
    fn test_format_source() {
        assert_eq!(format_source(&BindingSource::BuiltIn), "");
        assert_eq!(format_source(&BindingSource::User), "[user]");
        assert_eq!(
            format_source(&BindingSource::Plugin("my-plugin".into())),
            "[my-plugin]"
        );
        assert_eq!(format_source(&BindingSource::Runtime), "[runtime]");
    }

    #[test]
    fn test_help_total_lines_calculated() {
        let overlay = setup_overlay();
        assert!(overlay.total_lines > 0);
    }
}
```

---

## Verification

### Functional

```bash
# Build the workspace
cargo build --workspace

# Verify help overlay compiles
cargo check -p shux-ui

# Manual test:
# 1. Start shux
# 2. Press Ctrl+Space ?
# Expected: full-screen overlay showing categorized keybindings
# 3. Press j/k to scroll
# Expected: content scrolls smoothly
# 4. Press / then type "split"
# Expected: only split-related bindings shown
# 5. Press Escape to clear search
# 6. Press q to close overlay
# Expected: pane content visible again
# 7. Open command palette, then open help overlay
# Expected: help overlay is top-most and captures input until dismissed
```

### Tests

```bash
# Run help overlay tests
cargo nextest run -p shux-ui -- help_overlay

# Expected passing tests:
# - test_help_opens_with_sections
# - test_help_closes_on_q
# - test_help_closes_on_escape
# - test_help_scroll_down
# - test_help_scroll_up_at_top
# - test_help_search_filters
# - test_help_search_escape_clears
# - test_help_copy_mode_context
# - test_help_sections_ordered
# - test_format_source
# - test_help_total_lines_calculated
```

### L4 Visual Regression — iterm2-driver (PRD §16.2)

Create `.claude/automations/test_help_visual.py` to verify help overlay rendering:

```python
# /// script
# requires-python = ">=3.14"
# dependencies = ["iterm2", "pyobjc", "pyobjc-framework-Quartz"]
# ///
"""
shux Help Overlay Visual Test (iterm2-driver)

Tests:
1. Launch shux, create session
2. Press Ctrl+Space then ? to open help
3. Verify full-screen overlay appears (content changes)
4. Verify category headers visible: Navigation, Panes, Windows, etc.
5. Take screenshot: help_full.png
6. Press j 5 times — verify scrolling (content changes)
7. Verify scroll percentage in footer
8. Press / to enter search, type "zoom"
9. Verify only zoom-related bindings shown (content changes)
10. Take screenshot: help_search.png
11. Press Escape to clear search, verify full list restored
12. Press q to close overlay
13. Verify pane content visible again

Verification Strategy:
- Read screen, check for category headers
- Assert content changes on scroll and search
- Verify overlay fully covers pane content
- Layout check: no content bleeding through overlay

Usage:
    uv run .claude/automations/test_help_visual.py
"""
```

Run: `uv run .claude/automations/test_help_visual.py`

---

## Completion Criteria

- [ ] `Prefix + ?` opens full-screen help overlay
- [ ] Keybindings organized by category: Navigation, Panes, Windows, Session, Copy Mode, Config, General, Plugins
- [ ] Categories displayed in logical order with section headers
- [ ] Each entry shows: key combo, description, source indicator (for non-built-in)
- [ ] Customized bindings (user/plugin/runtime) visually distinguished
- [ ] Search: press `/` to enter search mode, type to filter, Escape to clear
- [ ] Search matches on key display string and description
- [ ] Scroll: j/k (or Up/Down), Ctrl+d/u (half-page), g/G (top/bottom)
- [ ] Scroll percentage indicator in footer
- [ ] Dismiss with Escape (when not searching) or `q`
- [ ] Contextual: copy mode context shows copy-specific bindings at top
- [ ] Full-screen overlay covers all pane content
- [ ] All input captured while overlay is open (no passthrough)
- [ ] Help overlay renders above command palette in z-order
- [ ] Empty search restores full list
- [ ] Unit tests pass for opening, scrolling, searching, context, and dismissal

---

## Commit Message

```
feat(ui): add keybinding help overlay with categorized reference

- Prefix + ? opens full-screen keybinding cheat sheet
- Bindings organized by category (Navigation, Panes, Windows, etc.)
- Search mode (/) for real-time filtering by key or description
- Vim-style scrolling (j/k, Ctrl+d/u, g/G) with scroll indicator
- Contextual: copy mode shows copy-specific bindings first
- Custom bindings visually distinguished with source indicator
- Dismiss with q or Escape
```

---

## Session Protocol

1. **Before starting:** Read task 032 (command palette) for overlay rendering patterns and input routing. Read task 031 (keybinding configuration) for the `KeybindingRegistry` API and `Binding` structure. Read task 021 (copy mode) for copy mode keybinding context.
2. **During:** Implement in order: data model (Step 1), rendering (Step 2), compositor/input integration (Step 3), tests (Step 4). Run `cargo check` after each step.
3. **Edge cases to watch for:**
   - Very small terminal (minimal viable help display)
   - No bindings in a category (section should be omitted)
   - Many plugin bindings (should not overflow or break layout)
   - Search with no results (show "No matching keybindings" message)
   - Opening help overlay while command palette is open (help should take priority)
   - Unicode in binding descriptions
   - Very long key combo displays (e.g., "Ctrl+Shift+Alt+F12")
4. **After:** Run full test suite. Manually verify the overlay renders correctly with various terminal sizes. Update `docs/PROGRESS.md`. Update `CLAUDE.md` Learnings.
