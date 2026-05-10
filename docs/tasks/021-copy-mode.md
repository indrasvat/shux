# 021 — Copy Mode

**Status:** Done (PR #7, 2026-05-09). `Prefix + [` enters copy mode; selection via cursor + space-mark; OSC 52 yank to host clipboard. Implementation in `crates/shux-ui/src/copy_mode.rs`. Verified via `.claude/automations/test_021_copy_mode.py`.
**Depends On:** 019
**Parallelizable With:** 024

---

## Problem

Terminal multiplexers need a way for users to scroll through output, search for text, select regions, and copy them to the system clipboard -- all without the underlying PTY application knowing about it. This is "copy mode" (tmux calls it the same thing). shux's copy mode is entered via `Prefix + [` (from task 019) or via the `copy.enter` API method. It must support vi-style navigation (the dominant convention), incremental search, visual selection (character/line/block), and reliable clipboard integration.

Without copy mode, users cannot select and copy text from a pane's scrollback. This is a fundamental daily-driver requirement (PRD SS 6.1 P0). Copy mode also serves agents via the `copy.*` API methods for programmatic text extraction.

## PRD Reference

- **SS 6.1** (Copy mode: enter, scroll, search, select, copy to system clipboard)
- **SS 8.2** (copy.enter, copy.search, copy.select, copy.to_clipboard API methods)
- **SS 10.2** (`[copy] osc52, mouse_select_copies, vi_keys` configuration)
- **SS 9.2** (`Prefix + [` enters copy mode)

---

## Files to Create

- `crates/shux-ui/src/copy_mode.rs` -- Copy mode state machine, navigation, selection
- `crates/shux-ui/src/clipboard.rs` -- Clipboard integration (OSC 52 + platform fallbacks)
- `crates/shux-ui/src/search.rs` -- Incremental search within scrollback
- `crates/shux-ui/tests/copy_mode_test.rs` -- Unit and integration tests

## Files to Modify

- `crates/shux-ui/src/lib.rs` -- Register new modules
- `crates/shux-ui/src/compositor.rs` -- Render selection highlights and search matches
- `crates/shux-ui/src/event_loop.rs` -- Route input to copy mode when active
- `crates/shux-ui/src/prefix_actions.rs` -- Wire EnterCopyMode action
- `crates/shux-rpc/src/methods/copy.rs` -- Implement copy.* API methods (new file)
- `crates/shux-rpc/src/lib.rs` -- Register copy methods

---

## Execution Steps

### Step 1: Define copy mode state machine

Copy mode has several sub-states: normal navigation, visual selection, and search. The state machine tracks cursor position, scroll offset, selection range, and search state.

```rust
// crates/shux-ui/src/copy_mode.rs

use uuid::Uuid;

/// Copy mode state for a single pane.
pub struct CopyMode {
    /// The pane we are browsing.
    pane_id: Uuid,
    /// Cursor position within the scrollback (0 = bottom-left of visible area).
    cursor: CopyModeCursor,
    /// Current scroll offset (0 = bottom of scrollback, positive = scrolled up).
    scroll_offset: usize,
    /// Active visual selection, if any.
    selection: Option<Selection>,
    /// Active search state, if any.
    search: Option<SearchState>,
    /// The total number of lines in scrollback + visible area.
    total_lines: usize,
    /// Width of the pane in columns.
    pane_width: u16,
    /// Height of the pane in rows.
    pane_height: u16,
}

#[derive(Debug, Clone, Copy)]
pub struct CopyModeCursor {
    /// Column position (0-based).
    pub col: usize,
    /// Line position (0-based, 0 = first line of scrollback).
    pub line: usize,
}

#[derive(Debug, Clone)]
pub struct Selection {
    /// Where selection started (anchor point).
    pub anchor: CopyModeCursor,
    /// Current end of selection (moves with cursor).
    pub cursor: CopyModeCursor,
    /// Selection mode.
    pub mode: SelectionMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionMode {
    /// Character-wise selection (vi `v`).
    Character,
    /// Line-wise selection (vi `V`).
    Line,
    /// Block/rectangular selection (vi `Ctrl+v`).
    Block,
}

/// Result of processing a key in copy mode.
#[derive(Debug)]
pub enum CopyModeResult {
    /// Key consumed, copy mode continues.
    Continue,
    /// Exit copy mode, return to normal.
    Exit,
    /// Text was yanked to clipboard.
    Yanked(String),
}
```

### Step 2: Implement vi-style navigation

All navigation follows vi conventions. The PRD specifies: h/j/k/l, w/b/e, 0/$, gg/G, Ctrl+u/d.

```rust
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

impl CopyMode {
    pub fn process_key(&mut self, key: KeyEvent) -> CopyModeResult {
        // If search mode is active, route to search handler.
        if let Some(ref mut search) = self.search {
            return self.process_search_key(key);
        }

        match (key.code, key.modifiers) {
            // === Exit ===
            (KeyCode::Char('q'), KeyModifiers::NONE) |
            (KeyCode::Esc, _) => CopyModeResult::Exit,

            // === Basic movement ===
            (KeyCode::Char('h'), KeyModifiers::NONE) | (KeyCode::Left, _) => {
                self.move_left();
                CopyModeResult::Continue
            }
            (KeyCode::Char('j'), KeyModifiers::NONE) | (KeyCode::Down, _) => {
                self.move_down(1);
                CopyModeResult::Continue
            }
            (KeyCode::Char('k'), KeyModifiers::NONE) | (KeyCode::Up, _) => {
                self.move_up(1);
                CopyModeResult::Continue
            }
            (KeyCode::Char('l'), KeyModifiers::NONE) | (KeyCode::Right, _) => {
                self.move_right();
                CopyModeResult::Continue
            }

            // === Word movement ===
            (KeyCode::Char('w'), KeyModifiers::NONE) => {
                self.move_word_forward();
                CopyModeResult::Continue
            }
            (KeyCode::Char('b'), KeyModifiers::NONE) => {
                self.move_word_backward();
                CopyModeResult::Continue
            }
            (KeyCode::Char('e'), KeyModifiers::NONE) => {
                self.move_word_end();
                CopyModeResult::Continue
            }

            // === Line movement ===
            (KeyCode::Char('0'), KeyModifiers::NONE) => {
                self.cursor.col = 0;
                CopyModeResult::Continue
            }
            (KeyCode::Char('$'), KeyModifiers::NONE) => {
                self.move_end_of_line();
                CopyModeResult::Continue
            }
            (KeyCode::Char('^'), KeyModifiers::NONE) => {
                self.move_first_non_blank();
                CopyModeResult::Continue
            }

            // === Page movement ===
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                // Half-page up.
                let half = self.pane_height as usize / 2;
                self.move_up(half);
                CopyModeResult::Continue
            }
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                // Half-page down.
                let half = self.pane_height as usize / 2;
                self.move_down(half);
                CopyModeResult::Continue
            }
            (KeyCode::Char('f'), KeyModifiers::CONTROL) => {
                // Full page down.
                self.move_down(self.pane_height as usize);
                CopyModeResult::Continue
            }
            (KeyCode::Char('b'), KeyModifiers::CONTROL) => {
                // Full page up.
                self.move_up(self.pane_height as usize);
                CopyModeResult::Continue
            }

            // === Document movement ===
            (KeyCode::Char('g'), KeyModifiers::NONE) => {
                // 'gg' = go to top. Track pending 'g' for double-press.
                if self.pending_g {
                    self.cursor.line = 0;
                    self.cursor.col = 0;
                    self.scroll_to_cursor();
                    self.pending_g = false;
                } else {
                    self.pending_g = true;
                }
                CopyModeResult::Continue
            }
            (KeyCode::Char('G'), KeyModifiers::SHIFT) => {
                // Go to bottom.
                self.cursor.line = self.total_lines.saturating_sub(1);
                self.cursor.col = 0;
                self.scroll_to_cursor();
                CopyModeResult::Continue
            }

            // === Selection ===
            (KeyCode::Char('v'), KeyModifiers::NONE) => {
                self.toggle_selection(SelectionMode::Character);
                CopyModeResult::Continue
            }
            (KeyCode::Char('V'), KeyModifiers::SHIFT) => {
                self.toggle_selection(SelectionMode::Line);
                CopyModeResult::Continue
            }
            (KeyCode::Char('v'), KeyModifiers::CONTROL) => {
                self.toggle_selection(SelectionMode::Block);
                CopyModeResult::Continue
            }

            // === Yank (copy) ===
            (KeyCode::Char('y'), KeyModifiers::NONE) => {
                if let Some(text) = self.yank_selection() {
                    CopyModeResult::Yanked(text)
                } else {
                    CopyModeResult::Continue
                }
            }

            // === Search ===
            (KeyCode::Char('/'), KeyModifiers::NONE) => {
                self.start_search(SearchDirection::Forward);
                CopyModeResult::Continue
            }
            (KeyCode::Char('?'), KeyModifiers::NONE) => {
                self.start_search(SearchDirection::Backward);
                CopyModeResult::Continue
            }
            (KeyCode::Char('n'), KeyModifiers::NONE) => {
                self.search_next();
                CopyModeResult::Continue
            }
            (KeyCode::Char('N'), KeyModifiers::SHIFT) => {
                self.search_prev();
                CopyModeResult::Continue
            }

            // All other keys: ignore (don't exit copy mode).
            _ => {
                self.pending_g = false;
                CopyModeResult::Continue
            }
        }
    }
}
```

### Step 3: Implement cursor movement helpers

```rust
impl CopyMode {
    fn move_left(&mut self) {
        if self.cursor.col > 0 {
            self.cursor.col -= 1;
        }
        self.update_selection_cursor();
    }

    fn move_right(&mut self) {
        let line_len = self.line_length(self.cursor.line);
        if self.cursor.col < line_len.saturating_sub(1) {
            self.cursor.col += 1;
        }
        self.update_selection_cursor();
    }

    fn move_up(&mut self, count: usize) {
        self.cursor.line = self.cursor.line.saturating_sub(count);
        self.clamp_cursor_col();
        self.scroll_to_cursor();
        self.update_selection_cursor();
    }

    fn move_down(&mut self, count: usize) {
        self.cursor.line = (self.cursor.line + count).min(self.total_lines.saturating_sub(1));
        self.clamp_cursor_col();
        self.scroll_to_cursor();
        self.update_selection_cursor();
    }

    fn move_end_of_line(&mut self) {
        let line_len = self.line_length(self.cursor.line);
        self.cursor.col = line_len.saturating_sub(1);
        self.update_selection_cursor();
    }

    fn move_first_non_blank(&mut self) {
        let line = self.get_line_text(self.cursor.line);
        self.cursor.col = line
            .chars()
            .position(|c| !c.is_whitespace())
            .unwrap_or(0);
        self.update_selection_cursor();
    }

    fn move_word_forward(&mut self) {
        let line = self.get_line_text(self.cursor.line);
        let chars: Vec<char> = line.chars().collect();
        let mut pos = self.cursor.col;

        // Skip current word characters.
        while pos < chars.len() && !chars[pos].is_whitespace() {
            pos += 1;
        }
        // Skip whitespace.
        while pos < chars.len() && chars[pos].is_whitespace() {
            pos += 1;
        }

        if pos >= chars.len() && self.cursor.line < self.total_lines - 1 {
            // Wrap to next line.
            self.cursor.line += 1;
            self.cursor.col = 0;
            self.move_first_non_blank();
        } else {
            self.cursor.col = pos.min(chars.len().saturating_sub(1));
        }
        self.scroll_to_cursor();
        self.update_selection_cursor();
    }

    fn move_word_backward(&mut self) {
        let line = self.get_line_text(self.cursor.line);
        let chars: Vec<char> = line.chars().collect();
        let mut pos = self.cursor.col;

        if pos == 0 && self.cursor.line > 0 {
            // Wrap to end of previous line.
            self.cursor.line -= 1;
            self.move_end_of_line();
            return;
        }

        // Skip whitespace backward.
        while pos > 0 && chars[pos.saturating_sub(1)].is_whitespace() {
            pos -= 1;
        }
        // Skip word characters backward.
        while pos > 0 && !chars[pos.saturating_sub(1)].is_whitespace() {
            pos -= 1;
        }

        self.cursor.col = pos;
        self.scroll_to_cursor();
        self.update_selection_cursor();
    }

    fn move_word_end(&mut self) {
        let line = self.get_line_text(self.cursor.line);
        let chars: Vec<char> = line.chars().collect();
        let mut pos = self.cursor.col + 1;

        // Skip whitespace.
        while pos < chars.len() && chars[pos].is_whitespace() {
            pos += 1;
        }
        // Move to end of word.
        while pos < chars.len() && !chars[pos].is_whitespace() {
            pos += 1;
        }

        self.cursor.col = pos.saturating_sub(1).min(chars.len().saturating_sub(1));
        self.scroll_to_cursor();
        self.update_selection_cursor();
    }

    /// Ensure the cursor is visible by adjusting the scroll offset.
    fn scroll_to_cursor(&mut self) {
        let visible_top = self.total_lines.saturating_sub(self.scroll_offset + self.pane_height as usize);
        let visible_bottom = visible_top + self.pane_height as usize;

        if self.cursor.line < visible_top {
            self.scroll_offset = self.total_lines.saturating_sub(self.cursor.line + self.pane_height as usize);
        } else if self.cursor.line >= visible_bottom {
            self.scroll_offset = self.total_lines.saturating_sub(self.cursor.line + 1);
        }
    }

    fn clamp_cursor_col(&mut self) {
        let line_len = self.line_length(self.cursor.line);
        if self.cursor.col >= line_len {
            self.cursor.col = line_len.saturating_sub(1);
        }
    }
}
```

### Step 4: Implement visual selection

Selection is toggled with `v` (character), `V` (line), or `Ctrl+v` (block). When active, the cursor movement updates the selection's end point.

```rust
impl CopyMode {
    fn toggle_selection(&mut self, mode: SelectionMode) {
        if let Some(ref sel) = self.selection {
            if sel.mode == mode {
                // Same mode pressed again: cancel selection.
                self.selection = None;
                return;
            }
        }
        // Start new selection or change mode.
        self.selection = Some(Selection {
            anchor: self.cursor,
            cursor: self.cursor,
            mode,
        });
    }

    fn update_selection_cursor(&mut self) {
        if let Some(ref mut sel) = self.selection {
            sel.cursor = self.cursor;
        }
    }

    /// Extract the selected text based on the current selection mode.
    fn yank_selection(&self) -> Option<String> {
        let selection = self.selection.as_ref()?;
        let (start, end) = self.normalize_selection(selection);

        match selection.mode {
            SelectionMode::Character => {
                self.extract_character_selection(start, end)
            }
            SelectionMode::Line => {
                self.extract_line_selection(start.line, end.line)
            }
            SelectionMode::Block => {
                self.extract_block_selection(start, end)
            }
        }
    }

    /// Normalize selection so start <= end.
    fn normalize_selection(&self, sel: &Selection) -> (CopyModeCursor, CopyModeCursor) {
        if sel.anchor.line < sel.cursor.line
            || (sel.anchor.line == sel.cursor.line && sel.anchor.col <= sel.cursor.col)
        {
            (sel.anchor, sel.cursor)
        } else {
            (sel.cursor, sel.anchor)
        }
    }

    fn extract_character_selection(
        &self,
        start: CopyModeCursor,
        end: CopyModeCursor,
    ) -> Option<String> {
        let mut result = String::new();
        for line_idx in start.line..=end.line {
            let line = self.get_line_text(line_idx);
            let start_col = if line_idx == start.line { start.col } else { 0 };
            let end_col = if line_idx == end.line {
                (end.col + 1).min(line.len())
            } else {
                line.len()
            };
            if start_col < line.len() {
                let chars: Vec<char> = line.chars().collect();
                let slice: String = chars[start_col..end_col.min(chars.len())]
                    .iter()
                    .collect();
                result.push_str(&slice);
            }
            if line_idx < end.line {
                result.push('\n');
            }
        }
        if result.is_empty() { None } else { Some(result) }
    }

    fn extract_line_selection(
        &self,
        start_line: usize,
        end_line: usize,
    ) -> Option<String> {
        let mut lines = Vec::new();
        for line_idx in start_line..=end_line {
            lines.push(self.get_line_text(line_idx));
        }
        let result = lines.join("\n");
        if result.is_empty() { None } else { Some(result) }
    }

    fn extract_block_selection(
        &self,
        start: CopyModeCursor,
        end: CopyModeCursor,
    ) -> Option<String> {
        let left = start.col.min(end.col);
        let right = start.col.max(end.col);
        let mut lines = Vec::new();
        for line_idx in start.line..=end.line {
            let line = self.get_line_text(line_idx);
            let chars: Vec<char> = line.chars().collect();
            let slice: String = chars
                .get(left..=(right.min(chars.len().saturating_sub(1))))
                .unwrap_or(&[])
                .iter()
                .collect();
            lines.push(slice);
        }
        let result = lines.join("\n");
        if result.is_empty() { None } else { Some(result) }
    }
}
```

### Step 5: Implement incremental search

Search is entered via `/` (forward) or `?` (backward). As the user types, the view jumps to the first match. `n`/`N` cycle through matches.

```rust
// crates/shux-ui/src/search.rs

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDirection {
    Forward,
    Backward,
}

#[derive(Debug, Clone)]
pub struct SearchState {
    /// The search query being built.
    pub query: String,
    /// Search direction.
    pub direction: SearchDirection,
    /// All match positions (line, start_col, end_col).
    pub matches: Vec<SearchMatch>,
    /// Index into `matches` for the current match.
    pub current_match: usize,
    /// Whether the search input is still being edited.
    pub editing: bool,
}

#[derive(Debug, Clone)]
pub struct SearchMatch {
    pub line: usize,
    pub start_col: usize,
    pub end_col: usize,
}

impl CopyMode {
    fn start_search(&mut self, direction: SearchDirection) {
        self.search = Some(SearchState {
            query: String::new(),
            direction,
            matches: Vec::new(),
            current_match: 0,
            editing: true,
        });
    }

    fn process_search_key(&mut self, key: KeyEvent) -> CopyModeResult {
        let search = match self.search.as_mut() {
            Some(s) => s,
            None => return CopyModeResult::Continue,
        };

        if !search.editing {
            // Search is finalized, handle n/N/Escape.
            match key.code {
                KeyCode::Char('n') => { self.search_next(); }
                KeyCode::Char('N') => { self.search_prev(); }
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.search = None;
                }
                _ => {
                    // Any other key exits search mode but stays in copy mode.
                    self.search = None;
                    return self.process_key(key);
                }
            }
            return CopyModeResult::Continue;
        }

        // Editing the search query.
        match key.code {
            KeyCode::Enter => {
                search.editing = false;
                // Jump to first match.
                if !search.matches.is_empty() {
                    let m = &search.matches[search.current_match];
                    self.cursor.line = m.line;
                    self.cursor.col = m.start_col;
                    self.scroll_to_cursor();
                }
            }
            KeyCode::Esc => {
                self.search = None;
            }
            KeyCode::Backspace => {
                search.query.pop();
                self.update_search_matches();
            }
            KeyCode::Char(c) => {
                search.query.push(c);
                self.update_search_matches();
                // Incremental: jump to nearest match as user types.
                self.jump_to_nearest_match();
            }
            _ => {}
        }

        CopyModeResult::Continue
    }

    fn update_search_matches(&mut self) {
        let search = match self.search.as_mut() {
            Some(s) => s,
            None => return,
        };

        search.matches.clear();
        if search.query.is_empty() {
            return;
        }

        let query_lower = search.query.to_lowercase();
        for line_idx in 0..self.total_lines {
            let line = self.get_line_text(line_idx).to_lowercase();
            let mut start = 0;
            while let Some(pos) = line[start..].find(&query_lower) {
                let abs_pos = start + pos;
                search.matches.push(SearchMatch {
                    line: line_idx,
                    start_col: abs_pos,
                    end_col: abs_pos + query_lower.len(),
                });
                start = abs_pos + 1;
            }
        }

        search.current_match = 0;
    }

    fn jump_to_nearest_match(&mut self) {
        let search = match self.search.as_ref() {
            Some(s) => s,
            None => return,
        };

        if search.matches.is_empty() {
            return;
        }

        // Find the match nearest to the current cursor position.
        let cursor_line = self.cursor.line;
        let nearest = search.matches.iter().enumerate().min_by_key(|(_, m)| {
            (m.line as isize - cursor_line as isize).unsigned_abs()
        });

        if let Some((idx, m)) = nearest {
            self.cursor.line = m.line;
            self.cursor.col = m.start_col;
            self.scroll_to_cursor();
            if let Some(ref mut s) = self.search {
                s.current_match = idx;
            }
        }
    }

    fn search_next(&mut self) {
        let search = match self.search.as_mut() {
            Some(s) => s,
            None => return,
        };
        if search.matches.is_empty() {
            return;
        }
        search.current_match = (search.current_match + 1) % search.matches.len();
        let m = &search.matches[search.current_match];
        self.cursor.line = m.line;
        self.cursor.col = m.start_col;
        self.scroll_to_cursor();
    }

    fn search_prev(&mut self) {
        let search = match self.search.as_mut() {
            Some(s) => s,
            None => return,
        };
        if search.matches.is_empty() {
            return;
        }
        search.current_match = if search.current_match == 0 {
            search.matches.len() - 1
        } else {
            search.current_match - 1
        };
        let m = &search.matches[search.current_match];
        self.cursor.line = m.line;
        self.cursor.col = m.start_col;
        self.scroll_to_cursor();
    }
}
```

### Step 6: Implement clipboard integration

Copy to clipboard uses OSC 52 when available (the preferred path -- works over SSH). Falls back to platform-specific commands.

```rust
// crates/shux-ui/src/clipboard.rs

use std::io::Write;
use std::process::Command;
use base64::Engine as _;

/// Clipboard backend configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Osc52Policy {
    /// Detect from terminal capabilities (default).
    Auto,
    /// Always use OSC 52.
    Allow,
    /// Never use OSC 52 (use fallback only).
    Deny,
}

pub struct ClipboardManager {
    osc52_policy: Osc52Policy,
    osc52_available: bool,
}

impl ClipboardManager {
    pub fn new(policy: Osc52Policy, terminal_supports_osc52: bool) -> Self {
        Self {
            osc52_policy: policy,
            osc52_available: terminal_supports_osc52,
        }
    }

    /// Copy text to the system clipboard.
    pub fn copy(&self, text: &str) -> Result<(), ClipboardError> {
        if self.should_use_osc52() {
            self.copy_osc52(text)?;
        }
        // Always try platform fallback too (belt and suspenders).
        // OSC 52 might fail silently if the terminal strips it.
        let _ = self.copy_platform(text);
        Ok(())
    }

    fn should_use_osc52(&self) -> bool {
        match self.osc52_policy {
            Osc52Policy::Allow => true,
            Osc52Policy::Deny => false,
            Osc52Policy::Auto => self.osc52_available,
        }
    }

    /// Copy via OSC 52 escape sequence.
    /// Format: ESC ] 52 ; c ; <base64-encoded text> ESC \
    fn copy_osc52(&self, text: &str) -> Result<(), ClipboardError> {
        let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
        let sequence = format!("\x1b]52;c;{}\x1b\\", encoded);
        let mut stdout = std::io::stdout().lock();
        stdout
            .write_all(sequence.as_bytes())
            .map_err(|e| ClipboardError::Osc52Failed(e.to_string()))?;
        stdout
            .flush()
            .map_err(|e| ClipboardError::Osc52Failed(e.to_string()))?;
        Ok(())
    }

    /// Copy via platform-specific command.
    fn copy_platform(&self, text: &str) -> Result<(), ClipboardError> {
        let cmd = if cfg!(target_os = "macos") {
            "pbcopy"
        } else if cfg!(target_os = "linux") {
            // Try xclip first, then xsel, then wl-copy (Wayland).
            return self.copy_linux(text);
        } else {
            return Err(ClipboardError::UnsupportedPlatform);
        };

        let mut child = Command::new(cmd)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| ClipboardError::CommandFailed(cmd.to_string(), e.to_string()))?;

        if let Some(stdin) = child.stdin.as_mut() {
            stdin
                .write_all(text.as_bytes())
                .map_err(|e| ClipboardError::CommandFailed(cmd.to_string(), e.to_string()))?;
        }

        let status = child
            .wait()
            .map_err(|e| ClipboardError::CommandFailed(cmd.to_string(), e.to_string()))?;

        if status.success() {
            Ok(())
        } else {
            Err(ClipboardError::CommandFailed(
                cmd.to_string(),
                format!("exit code: {:?}", status.code()),
            ))
        }
    }

    fn copy_linux(&self, text: &str) -> Result<(), ClipboardError> {
        // Try commands in order of preference.
        let commands = [
            ("xclip", vec!["-selection", "clipboard"]),
            ("xsel", vec!["--clipboard", "--input"]),
            ("wl-copy", vec![]),
        ];

        for (cmd, args) in &commands {
            match Command::new(cmd)
                .args(args)
                .stdin(std::process::Stdio::piped())
                .spawn()
            {
                Ok(mut child) => {
                    if let Some(stdin) = child.stdin.as_mut() {
                        let _ = stdin.write_all(text.as_bytes());
                    }
                    let status = child.wait();
                    if status.map(|s| s.success()).unwrap_or(false) {
                        return Ok(());
                    }
                }
                Err(_) => continue,
            }
        }

        Err(ClipboardError::NoClipboardCommand)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    #[error("OSC 52 clipboard failed: {0}")]
    Osc52Failed(String),
    #[error("clipboard command `{0}` failed: {1}")]
    CommandFailed(String, String),
    #[error("no clipboard command available (install xclip, xsel, or wl-copy)")]
    NoClipboardCommand,
    #[error("clipboard not supported on this platform")]
    UnsupportedPlatform,
}
```

### Step 7: Render copy mode visuals in compositor

The compositor needs to render several visual elements when copy mode is active:
- Cursor position (highlighted cell, typically reverse-video)
- Selection highlight (all selected cells with selection theme color)
- Search matches (highlighted with a different color)
- Current search match (brighter highlight)
- Status indicator showing "COPY" mode and search query

```rust
// Additions to crates/shux-ui/src/compositor.rs

/// Render copy mode overlays for a pane.
pub fn render_copy_mode_overlay(
    &self,
    pane_rect: &PaneRect,
    copy_mode: &CopyMode,
    theme: &ResolvedTheme,
    buf: &mut RenderBuffer,
) {
    // Render selection highlight.
    if let Some(ref selection) = copy_mode.selection {
        let (start, end) = copy_mode.normalize_selection(selection);
        match selection.mode {
            SelectionMode::Character => {
                self.highlight_character_range(pane_rect, start, end, theme.selection_bg, theme.selection_fg, buf);
            }
            SelectionMode::Line => {
                self.highlight_line_range(pane_rect, start.line, end.line, theme.selection_bg, theme.selection_fg, buf);
            }
            SelectionMode::Block => {
                self.highlight_block_range(pane_rect, start, end, theme.selection_bg, theme.selection_fg, buf);
            }
        }
    }

    // Render search match highlights.
    if let Some(ref search) = copy_mode.search {
        for (i, m) in search.matches.iter().enumerate() {
            let is_current = i == search.current_match;
            let bg = if is_current {
                theme.accent_primary
            } else {
                theme.info
            };
            self.highlight_character_range(
                pane_rect,
                CopyModeCursor { line: m.line, col: m.start_col },
                CopyModeCursor { line: m.line, col: m.end_col.saturating_sub(1) },
                bg,
                theme.fg_primary,
                buf,
            );
        }
    }

    // Render cursor (reverse-video on the current cell).
    let cursor_screen = copy_mode.cursor_screen_position(pane_rect);
    if let Some((col, row)) = cursor_screen {
        buf.set_style(col, row, Style {
            fg: Some(theme.bg_deep),
            bg: Some(theme.fg_primary),
            ..Default::default()
        });
    }
}
```

### Step 8: Implement copy.* API methods

Wire up the JSON-RPC API methods for programmatic copy mode control. These allow agents to enter copy mode, search, select, and extract text.

```rust
// crates/shux-rpc/src/methods/copy.rs

use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct CopyEnterParams {
    pub pane_id: String,
}

#[derive(Deserialize)]
pub struct CopySearchParams {
    pub pane_id: String,
    pub query: String,
    pub direction: Option<String>, // "forward" | "backward"
}

#[derive(Deserialize)]
pub struct CopySelectParams {
    pub pane_id: String,
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub mode: Option<String>, // "character" | "line" | "block"
}

#[derive(Deserialize)]
pub struct CopyToClipboardParams {
    pub pane_id: String,
}

#[derive(Serialize)]
pub struct CopyToClipboardResult {
    pub text: String,
    pub lines: usize,
    pub bytes: usize,
}

// Method handlers call into the UI context's copy mode state.
```

### Step 9: Add status indicator for copy mode

When copy mode is active, the status area displays "COPY" (and optionally the search query when searching). This integrates with the prefix mode indicator from task 019.

```rust
impl CopyMode {
    /// Returns status text for the status bar.
    pub fn status_text(&self) -> String {
        if let Some(ref search) = self.search {
            if search.editing {
                format!("COPY SEARCH: /{}", search.query)
            } else {
                format!(
                    "COPY [{}/{}]",
                    search.current_match + 1,
                    search.matches.len()
                )
            }
        } else if self.selection.is_some() {
            "COPY VISUAL".to_string()
        } else {
            "COPY".to_string()
        }
    }
}
```

---

## Verification

### Functional

```bash
# Build the project
cargo build --workspace

# Start a test session and generate some output
cargo run -p shux -- new -s test
# In the pane, run: seq 1 1000 (generates 1000 lines of output)

# Test copy mode:
# 1. Press Ctrl+Space then '[' -- "COPY" indicator appears, cursor visible
# 2. Press k to move up, j to move down -- cursor moves through scrollback
# 3. Press gg -- jump to top of scrollback
# 4. Press G -- jump to bottom
# 5. Press Ctrl+u / Ctrl+d -- half-page up/down
# 6. Press / then type "500" -- incremental search highlights "500"
# 7. Press Enter to confirm search, then n/N to cycle matches
# 8. Press v to start character selection, move cursor -- text highlights
# 9. Press y -- selected text copied to clipboard
# 10. Verify clipboard content (paste elsewhere)
# 11. Press q or Escape -- exit copy mode, "COPY" indicator disappears
# 12. Test V (line selection) and Ctrl+v (block selection) modes
```

### Tests

```bash
# Unit tests
cargo nextest run -p shux-ui --lib copy_mode
cargo nextest run -p shux-ui --lib clipboard
cargo nextest run -p shux-ui --lib search

# Integration tests
cargo nextest run -p shux-ui --test copy_mode_test

# Test scenarios:
# - All vi navigation keys produce correct cursor movements
# - 'gg' double-press detection works (single 'g' does not jump)
# - Selection modes: character, line, block extraction are correct
# - Search: incremental matching, case-insensitive, n/N cycling, wrap-around
# - Clipboard: OSC 52 encoding is correct base64
# - Clipboard: platform fallback invokes correct command
# - Scroll-to-cursor keeps cursor in visible area
# - Exit from copy mode cleans up all state
```

### L4 Visual Regression — iterm2-driver (PRD §16.2)

Create `.claude/automations/test_copy_mode_visual.py` to verify copy mode rendering:

```python
# /// script
# requires-python = ">=3.14"
# dependencies = ["iterm2", "pyobjc", "pyobjc-framework-Quartz"]
# ///
"""
shux Copy Mode Visual Test (iterm2-driver)

Tests:
1. Launch shux, generate some scrollback (`seq 1 100`)
2. Enter copy mode: Ctrl+Space then [
3. Verify "COPY" indicator appears in status bar
4. Verify cursor visible at bottom of pane
5. Press gg — verify cursor moves to top (content changes)
6. Press v to start selection, press j 5 times
7. Verify selection is visually highlighted (content changes)
8. Press / to enter search, type "50"
9. Verify "50" is highlighted in pane content
10. Press Escape to exit copy mode
11. Verify "COPY" indicator disappears
12. Take screenshots at key states

Verification Strategy:
- Read screen content, check for "COPY" in status bar
- Assert content changes after each key interaction
- Verify cursor position changes on navigation

Usage:
    uv run .claude/automations/test_copy_mode_visual.py
"""
```

Run: `uv run .claude/automations/test_copy_mode_visual.py`

---

## Completion Criteria

- [ ] `Prefix + [` enters copy mode with visual cursor and "COPY" status indicator
- [ ] Vi navigation: h/j/k/l move cursor correctly
- [ ] Word navigation: w/b/e move by word boundaries
- [ ] Line navigation: 0 (start), $ (end), ^ (first non-blank)
- [ ] Page navigation: Ctrl+u/d (half-page), Ctrl+f/b (full page)
- [ ] Document navigation: gg (top), G (bottom)
- [ ] Forward search (/) with incremental matching as user types
- [ ] Backward search (?) with incremental matching
- [ ] n/N cycle through search matches with wrap-around
- [ ] Search matches highlighted in the pane content
- [ ] `v` starts character selection, movement extends it
- [ ] `V` starts line selection
- [ ] `Ctrl+v` starts block/rectangular selection
- [ ] `y` yanks selected text to clipboard
- [ ] OSC 52 clipboard when configured/available
- [ ] Platform fallback: pbcopy (macOS), xclip/xsel/wl-copy (Linux)
- [ ] `q` or `Escape` exits copy mode
- [ ] Status indicator shows mode (COPY, COPY VISUAL, COPY SEARCH: /query)
- [ ] `copy.enter`, `copy.search`, `copy.select`, `copy.to_clipboard` API methods work
- [ ] Config: `[copy] osc52`, `vi_keys` settings respected
- [ ] Unit tests for navigation, selection extraction, search, clipboard
- [ ] No PTY input forwarded during copy mode

---

## Commit Message

```
feat(ui): implement copy mode with vi navigation, search, and clipboard

- Enter copy mode via Prefix+[ or copy.enter API (PRD §6.1)
- Vi-style navigation: h/j/k/l, w/b/e, 0/$, gg/G, Ctrl+u/d
- Incremental forward (/) and backward (?) search with match highlighting
- Visual selection: character (v), line (V), block (Ctrl+v) modes
- Yank to clipboard via OSC 52 (crossterm 0.29) with platform fallbacks
- copy.enter, copy.search, copy.select, copy.to_clipboard API methods
- Status bar indicator shows COPY mode and search state
```

---

## Session Protocol

1. **Before starting:** Read task 019 (prefix system) to understand how EnterCopyMode is triggered. Read task 005 (VT grid) to understand how scrollback text is accessed. Review crossterm 0.29 docs for OSC 52 support.
2. **During:** Implement in order: state machine (Step 1), navigation (Steps 2-3), selection (Step 4), search (Step 5), clipboard (Step 6), rendering (Step 7), API (Step 8), status (Step 9). Run `cargo check` after each step. Test navigation manually after Step 3, search after Step 5, clipboard after Step 6.
3. **After:** Run full verification suite. Test clipboard over SSH (OSC 52 path). Update `docs/PROGRESS.md` (mark 021 done). Update `CLAUDE.md` Learnings with clipboard compatibility findings.
