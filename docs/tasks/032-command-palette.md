# 032 — Command Palette

**Status:** Pending
**Depends On:** 019, 031
**Parallelizable With:** —

---

## Problem

shux has graded keybindings (Tier 1 bare keys, Tier 2 prefix keys), but no user can memorize all of them, and plugin commands compound the problem. The command palette is Tier 3: a searchable overlay where every command is discoverable. Press `Prefix + :` and a centered overlay appears with a filterable list of all commands. Type to filter, arrow keys to navigate, Enter to execute.

This is a critical discoverability feature. New users should be able to find any command without consulting documentation. The palette sources commands from the keybinding registry (built-in + user + plugin) and displays the command name, description, and keybinding hint. In M2, plugin-registered commands will appear here automatically.

The command palette is rendered as an overlay on the compositor's overlay system, meaning it floats above all pane content and captures all input while open.

## PRD Reference

- **section 6.1** P0 feature matrix, UX & keybindings: command palette + keybinding hints + plugin commands
- **section 9.3** Tier 3: everything reachable via `Prefix + :`, including plugin commands
- **section 9.2** Tier 2: `Prefix + :` shortcut

---

## Files to Create

- `crates/shux-ui/src/command_palette.rs` — Command palette overlay: rendering, search, selection, execution

## Files to Modify

- `crates/shux-ui/src/lib.rs` — Add `pub mod command_palette;`
- `crates/shux-ui/src/compositor.rs` — Render command palette as overlay layer
- `crates/shux-ui/src/input.rs` — Route input to command palette when open
- `crates/shux-core/src/keybinding.rs` — Add method to list all commands for palette consumption

---

## Execution Steps

### Step 1: Define Command Palette Data Model

Create `crates/shux-ui/src/command_palette.rs`:

```rust
//! Command palette — Tier 3 keybinding discoverability.
//!
//! Activated by Prefix + : (default Ctrl+Space :).
//! Shows a centered overlay with a searchable list of all available commands.
//! Type to filter, arrow keys to navigate, Enter to execute, Escape to dismiss.
//!
//! In M2, plugin-registered commands appear here automatically.

use shux_core::keybinding::{Binding, BindingCategory, KeySequence, KeybindingRegistry};
use crossterm::style::Color;

/// Represents a command entry in the palette.
#[derive(Debug, Clone)]
pub struct PaletteEntry {
    /// The action identifier (e.g., "pane.focus-left")
    pub action: String,
    /// Human-readable command name for display
    pub display_name: String,
    /// Description of what the command does
    pub description: String,
    /// Keybinding hint (e.g., "Alt+H") — empty if unbound
    pub keybinding_hint: String,
    /// Category for grouping
    pub category: BindingCategory,
    /// Match score for fuzzy search (higher = better match)
    pub score: i32,
}

/// The state of the command palette overlay.
pub struct CommandPalette {
    /// Whether the palette is currently visible
    visible: bool,
    /// The current search query (what the user has typed)
    query: String,
    /// Cursor position within the query string
    cursor_pos: usize,
    /// All available commands (refreshed when palette opens)
    all_entries: Vec<PaletteEntry>,
    /// Filtered entries matching the current query
    filtered_entries: Vec<PaletteEntry>,
    /// Index of the currently selected entry in filtered_entries
    selected_index: usize,
    /// Scroll offset for the visible window of entries
    scroll_offset: usize,
    /// Maximum number of visible entries (determined by terminal height)
    max_visible: usize,
}

/// Actions that the command palette can produce.
#[derive(Debug, Clone)]
pub enum PaletteAction {
    /// Execute the selected command
    Execute(String),
    /// Dismiss the palette without executing
    Dismiss,
    /// No action (palette consumed the input)
    Consumed,
}

impl CommandPalette {
    pub fn new() -> Self {
        Self {
            visible: false,
            query: String::new(),
            cursor_pos: 0,
            all_entries: Vec::new(),
            filtered_entries: Vec::new(),
            selected_index: 0,
            scroll_offset: 0,
            max_visible: 15,
        }
    }

    /// Open the command palette.
    /// Refreshes the command list from the keybinding registry.
    pub fn open(&mut self, registry: &KeybindingRegistry) {
        self.visible = true;
        self.query.clear();
        self.cursor_pos = 0;
        self.selected_index = 0;
        self.scroll_offset = 0;

        // Populate entries from keybinding registry
        self.all_entries = registry
            .list_all()
            .iter()
            .map(|binding| PaletteEntry {
                action: binding.action.clone(),
                display_name: action_to_display_name(&binding.action),
                description: binding.description.clone(),
                keybinding_hint: binding.key.to_string(),
                category: binding.category.clone(),
                score: 0,
            })
            .collect();

        // Deduplicate by action (a command may have multiple bindings)
        self.all_entries.sort_by(|a, b| a.action.cmp(&b.action));
        self.all_entries.dedup_by(|a, b| a.action == b.action);

        // Initially show all entries
        self.filtered_entries = self.all_entries.clone();
    }

    /// Close the command palette.
    pub fn close(&mut self) {
        self.visible = false;
        self.query.clear();
    }

    /// Whether the palette is currently visible.
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Handle a key event while the palette is open.
    /// Returns the action to take.
    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> PaletteAction {
        use crossterm::event::{KeyCode, KeyModifiers};

        match key.code {
            KeyCode::Esc => {
                self.close();
                PaletteAction::Dismiss
            }

            KeyCode::Enter => {
                if let Some(entry) = self.selected_entry() {
                    let action = entry.action.clone();
                    self.close();
                    PaletteAction::Execute(action)
                } else {
                    PaletteAction::Consumed
                }
            }

            KeyCode::Up => {
                if self.selected_index > 0 {
                    self.selected_index -= 1;
                    self.ensure_visible();
                }
                PaletteAction::Consumed
            }

            KeyCode::Down => {
                if self.selected_index + 1 < self.filtered_entries.len() {
                    self.selected_index += 1;
                    self.ensure_visible();
                }
                PaletteAction::Consumed
            }

            KeyCode::PageUp => {
                self.selected_index = self.selected_index.saturating_sub(self.max_visible);
                self.ensure_visible();
                PaletteAction::Consumed
            }

            KeyCode::PageDown => {
                self.selected_index = (self.selected_index + self.max_visible)
                    .min(self.filtered_entries.len().saturating_sub(1));
                self.ensure_visible();
                PaletteAction::Consumed
            }

            KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.selected_index = 0;
                self.ensure_visible();
                PaletteAction::Consumed
            }

            KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.selected_index = self.filtered_entries.len().saturating_sub(1);
                self.ensure_visible();
                PaletteAction::Consumed
            }

            KeyCode::Char(ch) => {
                self.query.insert(self.cursor_pos, ch);
                self.cursor_pos += 1;
                self.update_filter();
                PaletteAction::Consumed
            }

            KeyCode::Backspace => {
                if self.cursor_pos > 0 {
                    self.cursor_pos -= 1;
                    self.query.remove(self.cursor_pos);
                    self.update_filter();
                }
                PaletteAction::Consumed
            }

            KeyCode::Delete => {
                if self.cursor_pos < self.query.len() {
                    self.query.remove(self.cursor_pos);
                    self.update_filter();
                }
                PaletteAction::Consumed
            }

            KeyCode::Left => {
                if self.cursor_pos > 0 {
                    self.cursor_pos -= 1;
                }
                PaletteAction::Consumed
            }

            KeyCode::Right => {
                if self.cursor_pos < self.query.len() {
                    self.cursor_pos += 1;
                }
                PaletteAction::Consumed
            }

            _ => PaletteAction::Consumed, // Consume all input while open
        }
    }

    /// Get the currently selected entry.
    pub fn selected_entry(&self) -> Option<&PaletteEntry> {
        self.filtered_entries.get(self.selected_index)
    }

    /// Update the filtered entries based on the current query.
    fn update_filter(&mut self) {
        if self.query.is_empty() {
            self.filtered_entries = self.all_entries.clone();
        } else {
            let query_lower = self.query.to_lowercase();

            self.filtered_entries = self
                .all_entries
                .iter()
                .filter_map(|entry| {
                    let score = substring_match_score(entry, &query_lower);
                    if score > 0 {
                        let mut scored = entry.clone();
                        scored.score = score;
                        Some(scored)
                    } else {
                        None
                    }
                })
                .collect();

            // Sort by score (descending), then by name
            self.filtered_entries.sort_by(|a, b| {
                b.score
                    .cmp(&a.score)
                    .then_with(|| a.display_name.cmp(&b.display_name))
            });
        }

        // Reset selection to top
        self.selected_index = 0;
        self.scroll_offset = 0;
    }

    /// Ensure the selected entry is within the visible scroll window.
    fn ensure_visible(&mut self) {
        if self.selected_index < self.scroll_offset {
            self.scroll_offset = self.selected_index;
        } else if self.selected_index >= self.scroll_offset + self.max_visible {
            self.scroll_offset = self.selected_index - self.max_visible + 1;
        }
    }
}
```

### Step 2: Implement Substring Search Scoring

```rust
/// Simple substring matching with scoring.
///
/// Scoring:
/// - Exact match on action name: 100 points
/// - Prefix match on display name: 80 points
/// - Substring match on display name: 60 points
/// - Substring match on description: 40 points
/// - Substring match on keybinding hint: 20 points
/// - No match: 0 points
///
/// This is intentionally simple. Upgrade to fuzzy matching (e.g., skim/nucleo) in a later task.
fn substring_match_score(entry: &PaletteEntry, query: &str) -> i32 {
    let display_lower = entry.display_name.to_lowercase();
    let action_lower = entry.action.to_lowercase();
    let desc_lower = entry.description.to_lowercase();
    let hint_lower = entry.keybinding_hint.to_lowercase();

    if action_lower == query {
        return 100;
    }
    if display_lower.starts_with(query) {
        return 80;
    }
    if display_lower.contains(query) {
        return 60;
    }
    if action_lower.contains(query) {
        return 50;
    }
    if desc_lower.contains(query) {
        return 40;
    }
    if hint_lower.contains(query) {
        return 20;
    }
    0
}

/// Convert an action identifier to a human-readable display name.
///
/// "pane.focus-left" -> "Pane: Focus Left"
/// "window.create" -> "Window: Create"
/// "command-palette.open" -> "Command Palette: Open"
fn action_to_display_name(action: &str) -> String {
    let parts: Vec<&str> = action.splitn(2, '.').collect();
    if parts.len() == 2 {
        let category = parts[0]
            .split('-')
            .map(|w| {
                let mut chars = w.chars();
                match chars.next() {
                    None => String::new(),
                    Some(c) => c.to_uppercase().to_string() + chars.as_str(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ");

        let action = parts[1]
            .split('-')
            .map(|w| {
                let mut chars = w.chars();
                match chars.next() {
                    None => String::new(),
                    Some(c) => c.to_uppercase().to_string() + chars.as_str(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ");

        format!("{}: {}", category, action)
    } else {
        action
            .split('-')
            .map(|w| {
                let mut chars = w.chars();
                match chars.next() {
                    None => String::new(),
                    Some(c) => c.to_uppercase().to_string() + chars.as_str(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}
```

### Step 3: Implement Palette Rendering

```rust
impl CommandPalette {
    /// Render the command palette overlay.
    ///
    /// Returns the cells to draw on the compositor's overlay layer.
    pub fn render(
        &self,
        terminal_width: u16,
        terminal_height: u16,
        theme: &PaletteTheme,
    ) -> PaletteRenderOutput {
        if !self.visible {
            return PaletteRenderOutput::empty();
        }

        // Palette dimensions: 60% width, up to 60% height
        let palette_width = (terminal_width as f64 * 0.6).min(80.0).max(40.0) as u16;
        let palette_height = (terminal_height as f64 * 0.6).min(25.0).max(8.0) as u16;

        // Center the palette
        let x = (terminal_width.saturating_sub(palette_width)) / 2;
        let y = (terminal_height.saturating_sub(palette_height)) / 2;

        self.max_visible = (palette_height as usize).saturating_sub(4); // border + prompt + status

        let mut output = PaletteRenderOutput::new(x, y, palette_width, palette_height);

        // Draw border
        output.draw_border(theme.border_color);

        // Draw title: " Command Palette "
        output.draw_title(" Command Palette ", theme.title_fg, theme.title_bg);

        // Draw search prompt: "> query|"
        let prompt_row = y + 1;
        let prompt_text = format!("> {}", self.query);
        output.draw_text(
            x + 1,
            prompt_row,
            &prompt_text,
            theme.prompt_fg,
            theme.bg,
            palette_width - 2,
        );

        // Draw cursor
        let cursor_col = x + 1 + 2 + self.cursor_pos as u16; // "> " prefix
        if cursor_col < x + palette_width - 1 {
            output.set_cursor(cursor_col, prompt_row);
        }

        // Draw separator
        let sep_row = y + 2;
        output.draw_horizontal_line(x + 1, sep_row, palette_width - 2, theme.border_color);

        // Draw filtered entries
        let entries_start_row = y + 3;
        let visible_entries = &self.filtered_entries
            [self.scroll_offset..self.scroll_offset + self.max_visible.min(self.filtered_entries.len() - self.scroll_offset)];

        for (i, entry) in visible_entries.iter().enumerate() {
            let row = entries_start_row + i as u16;
            let is_selected = self.scroll_offset + i == self.selected_index;

            let (fg, bg) = if is_selected {
                (theme.selected_fg, theme.selected_bg)
            } else {
                (theme.entry_fg, theme.bg)
            };

            // Format: "  Display Name          Ctrl+H  "
            let hint_width = entry.keybinding_hint.len().min(15);
            let name_width = (palette_width as usize)
                .saturating_sub(4) // margins
                .saturating_sub(hint_width)
                .saturating_sub(2); // separator

            let name_display = if entry.display_name.len() > name_width {
                format!("{}...", &entry.display_name[..name_width.saturating_sub(3)])
            } else {
                entry.display_name.clone()
            };

            // Draw entry background
            output.fill_row(x + 1, row, palette_width - 2, bg);

            // Draw command name
            output.draw_text(x + 2, row, &name_display, fg, bg, name_width as u16);

            // Draw keybinding hint (right-aligned, dimmer)
            if !entry.keybinding_hint.is_empty() {
                let hint_col = x + palette_width - 2 - hint_width as u16;
                output.draw_text(
                    hint_col,
                    row,
                    &entry.keybinding_hint,
                    theme.hint_fg,
                    bg,
                    hint_width as u16,
                );
            }
        }

        // Draw status line: "N/M commands"
        let status_row = y + palette_height - 1;
        let status_text = format!(
            " {}/{} commands ",
            self.filtered_entries.len(),
            self.all_entries.len()
        );
        output.draw_text(
            x + 1,
            status_row,
            &status_text,
            theme.status_fg,
            theme.bg,
            palette_width - 2,
        );

        output
    }
}

/// Theme tokens for the command palette.
pub struct PaletteTheme {
    pub bg: Color,
    pub border_color: Color,
    pub title_fg: Color,
    pub title_bg: Color,
    pub prompt_fg: Color,
    pub entry_fg: Color,
    pub selected_fg: Color,
    pub selected_bg: Color,
    pub hint_fg: Color,
    pub status_fg: Color,
}

/// Render output from the command palette.
pub struct PaletteRenderOutput {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub cells: Vec<(u16, u16, char, Color, Color)>, // (col, row, ch, fg, bg)
    pub cursor: Option<(u16, u16)>,
}

impl PaletteRenderOutput {
    pub fn empty() -> Self {
        Self {
            x: 0, y: 0, width: 0, height: 0,
            cells: Vec::new(),
            cursor: None,
        }
    }

    pub fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x, y, width, height,
            cells: Vec::with_capacity((width as usize) * (height as usize)),
            cursor: None,
        }
    }

    pub fn set_cursor(&mut self, col: u16, row: u16) {
        self.cursor = Some((col, row));
    }

    // ... drawing helper methods (draw_border, draw_text, fill_row, etc.)
}
```

### Step 4: Integrate with Compositor

Modify `crates/shux-ui/src/compositor.rs` to render the command palette as an overlay:

```rust
impl Compositor {
    pub fn render(&mut self, ctx: &RenderContext) -> Result<()> {
        let _guard = SyncGuard::new(&mut self.stdout, self.synchronized_output)?;

        // Render panes
        self.render_inner(ctx)?;

        // Render overlays (command palette, help overlay, etc.)
        if self.command_palette.is_visible() {
            let theme = PaletteTheme::from_resolved_theme(&ctx.theme);
            let output = self.command_palette.render(
                ctx.terminal_size.0,
                ctx.terminal_size.1,
                &theme,
            );
            self.apply_overlay(&output)?;

            // Position cursor in the palette's search field
            if let Some((col, row)) = output.cursor {
                crossterm::execute!(
                    self.stdout,
                    crossterm::cursor::MoveTo(col, row),
                    crossterm::cursor::Show
                )?;
            }
        }

        Ok(())
    }
}
```

### Step 5: Route Input to Palette

Modify `crates/shux-ui/src/input.rs` to intercept input when the palette is open:

```rust
/// Process an input event.
pub fn handle_input(
    &mut self,
    event: crossterm::event::Event,
) -> Option<Action> {
    // If command palette is open, it captures all input
    if self.command_palette.is_visible() {
        if let crossterm::event::Event::Key(key) = event {
            return match self.command_palette.handle_key(key) {
                PaletteAction::Execute(action) => {
                    Some(Action::ExecuteCommand(action))
                }
                PaletteAction::Dismiss => {
                    Some(Action::Redraw) // Redraw to remove overlay
                }
                PaletteAction::Consumed => {
                    Some(Action::Redraw) // Redraw to update palette
                }
            };
        }
        return None; // Consume non-key events
    }

    // Normal input processing...
    // (existing keybinding resolution logic)
}
```

### Step 6: Add Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use shux_core::keybinding::KeybindingRegistry;

    fn setup_palette() -> CommandPalette {
        let mut palette = CommandPalette::new();
        let registry = KeybindingRegistry::new();
        palette.open(&registry);
        palette
    }

    #[test]
    fn test_palette_opens_with_all_commands() {
        let palette = setup_palette();
        assert!(palette.is_visible());
        assert!(!palette.all_entries.is_empty());
        assert_eq!(palette.filtered_entries.len(), palette.all_entries.len());
    }

    #[test]
    fn test_palette_closes_on_escape() {
        let mut palette = setup_palette();
        let result = palette.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert!(matches!(result, PaletteAction::Dismiss));
        assert!(!palette.is_visible());
    }

    #[test]
    fn test_palette_filters_on_typing() {
        let mut palette = setup_palette();
        let initial_count = palette.filtered_entries.len();

        // Type "focus"
        for ch in "focus".chars() {
            palette.handle_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(ch),
                crossterm::event::KeyModifiers::empty(),
            ));
        }

        assert!(palette.filtered_entries.len() < initial_count);
        assert!(palette.filtered_entries.iter().all(|e| {
            e.display_name.to_lowercase().contains("focus")
                || e.action.to_lowercase().contains("focus")
                || e.description.to_lowercase().contains("focus")
        }));
    }

    #[test]
    fn test_palette_arrow_navigation() {
        let mut palette = setup_palette();
        assert_eq!(palette.selected_index, 0);

        palette.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Down,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(palette.selected_index, 1);

        palette.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Up,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(palette.selected_index, 0);
    }

    #[test]
    fn test_palette_enter_executes() {
        let mut palette = setup_palette();
        let first_action = palette.filtered_entries[0].action.clone();

        let result = palette.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ));

        assert!(matches!(result, PaletteAction::Execute(a) if a == first_action));
        assert!(!palette.is_visible());
    }

    #[test]
    fn test_palette_backspace_removes_char() {
        let mut palette = setup_palette();

        // Type "abc"
        for ch in "abc".chars() {
            palette.handle_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(ch),
                crossterm::event::KeyModifiers::empty(),
            ));
        }
        assert_eq!(palette.query, "abc");

        // Backspace
        palette.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Backspace,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(palette.query, "ab");
    }

    #[test]
    fn test_action_to_display_name() {
        assert_eq!(action_to_display_name("pane.focus-left"), "Pane: Focus Left");
        assert_eq!(action_to_display_name("window.create"), "Window: Create");
        assert_eq!(action_to_display_name("command-palette.open"), "Command Palette: Open");
    }

    #[test]
    fn test_substring_match_scoring() {
        let entry = PaletteEntry {
            action: "pane.focus-left".into(),
            display_name: "Pane: Focus Left".into(),
            description: "Focus the pane to the left".into(),
            keybinding_hint: "Alt+H".into(),
            category: BindingCategory::Navigation,
            score: 0,
        };

        assert_eq!(substring_match_score(&entry, "pane.focus-left"), 100); // exact
        assert_eq!(substring_match_score(&entry, "pane"), 80); // prefix of display
        assert_eq!(substring_match_score(&entry, "focus"), 60); // substring of display
        assert_eq!(substring_match_score(&entry, "left pane"), 0); // no match (not fuzzy)
    }

    #[test]
    fn test_palette_empty_query_shows_all() {
        let mut palette = setup_palette();

        // Type and clear
        palette.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('x'),
            crossterm::event::KeyModifiers::empty(),
        ));
        palette.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Backspace,
            crossterm::event::KeyModifiers::empty(),
        ));

        assert_eq!(palette.filtered_entries.len(), palette.all_entries.len());
    }

    #[test]
    fn test_palette_no_matches() {
        let mut palette = setup_palette();

        for ch in "xyzxyzxyz".chars() {
            palette.handle_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(ch),
                crossterm::event::KeyModifiers::empty(),
            ));
        }

        assert!(palette.filtered_entries.is_empty());
    }
}
```

---

## Verification

### Functional

```bash
# Build the workspace
cargo build --workspace

# Verify command palette compiles
cargo check -p shux-ui

# Manual test:
# 1. Start shux
# 2. Press Ctrl+Space :
# Expected: centered overlay with "Command Palette" title and search prompt
# 3. Type "split"
# Expected: filtered list showing split-related commands
# 4. Arrow down to select
# 5. Press Enter
# Expected: selected command executes
# 6. Re-open palette, press Escape
# Expected: palette dismissed, pane content visible again
# 7. Register/enable a plugin command (e.g., `plugin.ping`), open palette, type `plugin`
# Expected: plugin-provided command appears with source/provenance and is executable
# 8. Type a query with no matches (e.g., `xyzxyz`)
# Expected: explicit "no matches" state, no panic/crash
```

### Tests

```bash
# Run command palette tests
cargo nextest run -p shux-ui -- command_palette

# Expected passing tests:
# - test_palette_opens_with_all_commands
# - test_palette_closes_on_escape
# - test_palette_filters_on_typing
# - test_palette_arrow_navigation
# - test_palette_enter_executes
# - test_palette_backspace_removes_char
# - test_action_to_display_name
# - test_substring_match_scoring
# - test_palette_empty_query_shows_all
# - test_palette_no_matches
```

### L4 Visual Regression — iterm2-driver (PRD §16.2)

Create `.claude/automations/test_palette_visual.py` to verify command palette rendering:

```python
# /// script
# requires-python = ">=3.14"
# dependencies = ["iterm2", "pyobjc", "pyobjc-framework-Quartz"]
# ///
"""
shux Command Palette Visual Test (iterm2-driver)

Tests:
1. Launch shux, create session with content
2. Press Ctrl+Space then : to open palette
3. Verify centered overlay appears (screen content changes)
4. Verify overlay has border and title
5. Verify command list is visible
6. Type "split" to filter
7. Verify filtered results shown (fewer entries)
8. Take screenshot: palette_filtered.png
9. Press Down arrow, verify selection indicator moves
10. Press Escape, verify palette dismissed
11. Verify pane content visible again (overlay removed)
12. Check box-drawing connectivity of palette border

Verification Strategy:
- Read screen, check for overlay content (command names)
- Assert content changes on filter typing
- Verify overlay disappears on Escape
- Layout verification: palette border corners connected

Usage:
    uv run .claude/automations/test_palette_visual.py
"""
```

Run: `uv run .claude/automations/test_palette_visual.py`

---

## Completion Criteria

- [ ] `Prefix + :` opens command palette overlay
- [ ] Centered overlay with border and "Command Palette" title
- [ ] Search prompt with cursor, real-time filtering as user types
- [ ] All built-in commands listed with display name, description, keybinding hint
- [ ] Substring search scoring: exact > prefix > substring (action > display > description > hint)
- [ ] Up/Down arrow keys navigate the filtered list
- [ ] PageUp/PageDown for fast scrolling
- [ ] Enter executes the selected command and closes palette
- [ ] Escape dismisses the palette
- [ ] Backspace/Delete edit the search query
- [ ] Left/Right arrow keys move cursor within query
- [ ] Status line shows "N/M commands" (filtered/total)
- [ ] Palette captures all input while open (no key passthrough to panes)
- [ ] Overlay rendered above pane content via compositor overlay system
- [ ] Cursor visible in search field
- [ ] Long command names truncated with "..."
- [ ] Empty search shows all commands
- [ ] No-match state handled gracefully (empty list, no crash)
- [ ] Unit tests pass for opening, filtering, navigation, execution, and scoring

---

## Commit Message

```
feat(ui): add command palette with searchable command list

- Prefix + : opens centered overlay with all available commands
- Real-time substring search with scoring (exact > prefix > substring)
- Arrow key navigation, Enter to execute, Escape to dismiss
- Shows command name, description, and keybinding hint per entry
- Overlay captures all input while open (no passthrough)
- Status line with filtered/total command count
- Deduplicates commands with multiple keybindings
```

---

## Session Protocol

1. **Before starting:** Read task 031 (keybinding configuration) for the `KeybindingRegistry` API. Read task 019 (prefix key system) for how `Prefix + :` is dispatched. Read task 009 (render compositor) for the overlay rendering approach.
2. **During:** Implement in order: data model (Step 1), search scoring (Step 2), rendering (Step 3), compositor integration (Step 4), input routing (Step 5), tests (Step 6). Run `cargo check` after each step.
3. **Edge cases to watch for:**
   - Very small terminal (palette must fit or degrade gracefully)
   - Terminal with only 1 command matching (selected_index stays 0)
   - Unicode in command names or descriptions
   - Rapid typing (filter must not lag)
   - Opening palette with no keybindings registered (should show empty list gracefully)
   - Command execution failure (palette is already closed; error handled by executor)
4. **After:** Run full test suite. Manually test in a real terminal with various queries. Update `docs/PROGRESS.md`. Update `CLAUDE.md` Learnings.
