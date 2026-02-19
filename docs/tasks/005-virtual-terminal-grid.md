# 005 — Virtual Terminal Grid

**Status:** Pending
**Depends On:** 000
**Parallelizable With:** 001, 002, 006

---

## Problem

Each pane in shux needs a virtual terminal emulator that interprets the raw byte stream coming from the PTY process. This task builds the VT grid — the in-memory representation of terminal state (cells, scrollback, cursor) and the ANSI/VT parser that drives it. Without this, there is no way to render pane content to the screen.

The grid must be memory-efficient (shux targets 80 MB idle RSS for 10 panes with 5K scrollback each — PRD §14.1), support wide characters and full Unicode, and handle the full spectrum of ANSI/VT escape sequences that real-world programs emit (vim, htop, cargo output, colored prompts, etc.).

We follow Alacritty's VecDeque-based grid pattern (proven at scale) but build our own implementation rather than using `alacritty_terminal` (too tightly coupled to Alacritty's rendering pipeline — PRD §15.2).

## PRD Reference

- §5.5 — Virtual terminal grid (VecDeque, compact cells, lazy scrollback, vte 0.15)
- §15.2 — Technology choices: `vte` 0.15 with `ansi` feature; custom VecDeque grid (NOT `alacritty_terminal`)
- §4.4 — `VirtualTerminal` abstraction (per-pane grid + scrollback + vte parser)
- §14.1 — Memory budget: 80 MB idle for 10 panes with 5K scrollback
- §16.1 — L1 headless unit tests, L2 PTY integration tests

---

## Files to Create

- `crates/shux-vt/src/cell.rs` — Cell representation (compact ASCII + extended storage)
- `crates/shux-vt/src/grid.rs` — VecDeque-based grid with scrollback
- `crates/shux-vt/src/cursor.rs` — Cursor state tracking
- `crates/shux-vt/src/parser.rs` — vte::Perform implementation, escape sequence handler
- `crates/shux-vt/src/lib.rs` — VirtualTerminal public API (replaces stub)

## Files to Modify

- `crates/shux-vt/Cargo.toml` — Add dependencies (vte, unicode-width, etc.)

---

## Execution Steps

### Step 1: Add dependencies to shux-vt

Update `crates/shux-vt/Cargo.toml`:

```toml
[package]
name = "shux-vt"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
vte = { workspace = true }
unicode-width = "0.2"
tracing = { workspace = true }
thiserror = { workspace = true }

[dev-dependencies]
proptest = "1"
```

The `vte` workspace dependency already specifies `features = ["ansi"]` in the root `Cargo.toml`.

### Step 2: Implement cell representation (`cell.rs`)

The cell representation uses a two-tier strategy to minimize memory:
- **Inline cells** (common case — plain ASCII with basic styling): packed into a compact struct
- **Extended cells** (rare — wide characters, complex attributes, hyperlinks): stored in a separate side table

```rust
use std::sync::Arc;

/// Compact cell flags packed into a single byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellFlags(u8);

impl CellFlags {
    pub const BOLD: u8       = 0b0000_0001;
    pub const DIM: u8        = 0b0000_0010;
    pub const ITALIC: u8     = 0b0000_0100;
    pub const UNDERLINE: u8  = 0b0000_1000;
    pub const BLINK: u8      = 0b0001_0000;
    pub const INVERSE: u8    = 0b0010_0000;
    pub const HIDDEN: u8     = 0b0100_0000;
    pub const STRIKETHROUGH: u8 = 0b1000_0000;

    pub fn contains(self, flag: u8) -> bool { self.0 & flag != 0 }
    pub fn set(&mut self, flag: u8) { self.0 |= flag; }
    pub fn unset(&mut self, flag: u8) { self.0 &= !flag; }
    pub fn reset(&mut self) { self.0 = 0; }
}

/// Terminal color representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    /// Default terminal color (foreground or background).
    Default,
    /// Named ANSI color (0-7 normal, 8-15 bright).
    Indexed(u8),
    /// 24-bit RGB color.
    Rgb(u8, u8, u8),
}

impl Default for Color {
    fn default() -> Self { Color::Default }
}

/// Cell style attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellStyle {
    pub fg: Color,
    pub bg: Color,
    pub flags: CellFlags,
}

/// A single terminal cell.
///
/// Optimized for memory usage (PRD §5.5 targets 4 bytes for simple ASCII).
///
/// Strategy:
/// - Most cells are plain ASCII (1 byte) with default colors/style.
/// - We use a bit-packed `u64` (8 bytes) or a tagged union to store common cases inline.
/// - Rare cases (wide chars, RGB colors, hyperlinks) use an index into a side table (arena).
///
/// For the initial implementation, we use a slightly larger struct (approx 16 bytes)
/// to prioritize correctness, but the API hides this detail so internal representation
/// can be optimized later without breaking consumers.
///
/// Future optimization target:
/// ```rust
/// enum CompactCell {
///     Ascii(u8, StyleIndex), // 1 byte char + 3 byte style index
///     Unicode(char, StyleIndex), // 4 byte char + 3 byte style index
///     Extended(Box<ExtendedCell>), // Heap alloc for very rare cases
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// The character.
    pub ch: char,
    /// Display width.
    pub width: u8,
    /// Style attributes.
    pub style: CellStyle,
    /// Extended attributes (hyperlink, underline color, etc.).
    pub extended: Option<Arc<ExtendedAttrs>>,
}

/// Extended attributes that are rare enough to be heap-allocated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtendedAttrs {
    /// OSC 8 hyperlink target.
    pub hyperlink: Option<String>,
    /// Underline color (separate from fg).
    pub underline_color: Option<Color>,
    /// Underline style (single, double, curly, dotted, dashed).
    pub underline_style: UnderlineStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnderlineStyle {
    #[default]
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

impl Cell {
    /// An empty cell (space with default style).
    pub const EMPTY: Cell = Cell {
        ch: ' ',
        width: 1,
        style: CellStyle {
            fg: Color::Default,
            bg: Color::Default,
            flags: CellFlags(0),
        },
        extended: None,
    };

    /// A wide-character continuation cell (placeholder for the second column).
    pub fn wide_continuation() -> Cell {
        Cell {
            ch: ' ',
            width: 0,
            style: CellStyle::default(),
            extended: None,
        }
    }

    /// Whether this cell is a wide-character continuation placeholder.
    pub fn is_wide_continuation(&self) -> bool { self.width == 0 }

    /// Whether this cell is a wide character (width 2).
    pub fn is_wide(&self) -> bool { self.width == 2 }

    /// Reset this cell to empty with the given background color.
    pub fn reset(&mut self, bg: Color) {
        self.ch = ' ';
        self.width = 1;
        self.style = CellStyle {
            fg: Color::Default,
            bg,
            flags: CellFlags(0),
        };
        self.extended = None;
    }
}

impl Default for Cell {
    fn default() -> Self { Cell::EMPTY }
}
```

**Memory analysis**: `Cell` is approximately 16 bytes (char=4 + width=1 + style=5 + Option<Arc>=8, with padding). For 5000 lines x 200 cols = 1M cells x 16 bytes = 16 MB per pane. 10 panes = 160 MB — this exceeds the budget. We address this in Step 3 with lazy scrollback allocation and compact empty-line storage.

### Step 3: Implement the VecDeque grid (`grid.rs`)

The grid uses a `VecDeque<Row>` where each row is a `Vec<Cell>`. The VecDeque enables O(1) push/pop at both ends for scrolling. Scrollback lines are pushed to the front as new lines scroll off the visible area.

```rust
use std::collections::VecDeque;
use std::ops::{Index, IndexMut};
use crate::cell::{Cell, Color};

/// A single row of terminal cells.
#[derive(Debug, Clone)]
pub struct Row {
    cells: Vec<Cell>,
    /// Whether this row was wrapped from the previous line (soft wrap).
    pub wrapped: bool,
}

impl Row {
    pub fn new(cols: usize) -> Self {
        Row {
            cells: vec![Cell::default(); cols],
            wrapped: false,
        }
    }

    pub fn len(&self) -> usize { self.cells.len() }
    pub fn is_empty(&self) -> bool { self.cells.is_empty() }

    /// Resize the row, filling new cells with the given template.
    pub fn resize(&mut self, cols: usize, template: Cell) {
        self.cells.resize(cols, template);
    }

    /// Reset all cells in the row to the given background color.
    pub fn reset(&mut self, bg: Color) {
        for cell in &mut self.cells {
            cell.reset(bg);
        }
        self.wrapped = false;
    }

    /// Check if the row is entirely empty (all default spaces).
    pub fn is_blank(&self) -> bool {
        self.cells.iter().all(|c| c.ch == ' ' && c.style == Default::default())
    }
}

impl Index<usize> for Row {
    type Output = Cell;
    fn index(&self, col: usize) -> &Cell { &self.cells[col] }
}

impl IndexMut<usize> for Row {
    fn index_mut(&mut self, col: usize) -> &mut Cell { &mut self.cells[col] }
}

/// Configuration for the grid.
#[derive(Debug, Clone)]
pub struct GridConfig {
    /// Maximum number of scrollback lines. Default: 5000 (PRD §5.5).
    pub max_scrollback: usize,
}

impl Default for GridConfig {
    fn default() -> Self {
        GridConfig {
            max_scrollback: 5000,
        }
    }
}

/// VecDeque-based terminal grid with scrollback.
///
/// The grid is organized as:
///   - scrollback lines (index 0..scrollback_len): lines that have scrolled off the top
///   - visible lines (index scrollback_len..scrollback_len+rows): the current viewport
///
/// The VecDeque allows O(1) push_front (for scrollback) and O(1) push_back (for new lines).
#[derive(Debug)]
pub struct Grid {
    /// All lines: scrollback + visible area.
    raw: VecDeque<Row>,
    /// Number of visible rows (terminal height).
    rows: usize,
    /// Number of columns (terminal width).
    cols: usize,
    /// Configuration (max scrollback, etc.).
    config: GridConfig,
}

impl Grid {
    /// Create a new grid with the given dimensions.
    pub fn new(rows: usize, cols: usize, config: GridConfig) -> Self {
        let mut raw = VecDeque::with_capacity(rows);
        for _ in 0..rows {
            raw.push_back(Row::new(cols));
        }
        Grid { raw, rows, cols, config }
    }

    /// Number of visible rows.
    pub fn rows(&self) -> usize { self.rows }

    /// Number of columns.
    pub fn cols(&self) -> usize { self.cols }

    /// Number of scrollback lines above the visible area.
    pub fn scrollback_len(&self) -> usize {
        self.raw.len().saturating_sub(self.rows)
    }

    /// Total number of lines (scrollback + visible).
    pub fn total_lines(&self) -> usize { self.raw.len() }

    /// Access a visible row (0 = top of visible area).
    pub fn visible_row(&self, row: usize) -> &Row {
        let idx = self.scrollback_len() + row;
        &self.raw[idx]
    }

    /// Mutably access a visible row (0 = top of visible area).
    pub fn visible_row_mut(&mut self, row: usize) -> &mut Row {
        let idx = self.scrollback_len() + row;
        &mut self.raw[idx]
    }

    /// Access a scrollback row (0 = oldest scrollback line).
    pub fn scrollback_row(&self, row: usize) -> Option<&Row> {
        if row < self.scrollback_len() {
            Some(&self.raw[row])
        } else {
            None
        }
    }

    /// Scroll the visible area up by one line within a scroll region.
    /// The top line of the region moves into scrollback (if region starts at line 0).
    /// A new empty line appears at the bottom of the region.
    pub fn scroll_up(&mut self, region_top: usize, region_bottom: usize) {
        if region_top == 0 && region_bottom == self.rows - 1 {
            // Full-screen scroll: top line goes to scrollback.
            // We already have it in the VecDeque — just add a new line at the bottom.
            self.raw.push_back(Row::new(self.cols));
            // Trim scrollback if over limit.
            let max_total = self.rows + self.config.max_scrollback;
            while self.raw.len() > max_total {
                self.raw.pop_front();
            }
        } else {
            // Scroll region: remove top of region, insert blank at bottom of region.
            let sb = self.scrollback_len();
            let abs_top = sb + region_top;
            let abs_bottom = sb + region_bottom;
            self.raw.remove(abs_top);
            self.raw.insert(abs_bottom, Row::new(self.cols));
        }
    }

    /// Scroll the visible area down by one line within a scroll region.
    /// A new empty line appears at the top of the region.
    /// The bottom line of the region is discarded.
    pub fn scroll_down(&mut self, region_top: usize, region_bottom: usize) {
        let sb = self.scrollback_len();
        let abs_top = sb + region_top;
        let abs_bottom = sb + region_bottom;
        self.raw.remove(abs_bottom);
        self.raw.insert(abs_top, Row::new(self.cols));
    }

    /// Clear all visible rows (reset to empty with given background).
    pub fn clear_visible(&mut self, bg: Color) {
        let sb = self.scrollback_len();
        for i in sb..self.raw.len() {
            self.raw[i].reset(bg);
        }
    }

    /// Clear rows from `start_row` to the end of the visible area.
    pub fn clear_below(&mut self, start_row: usize, bg: Color) {
        let sb = self.scrollback_len();
        for i in (sb + start_row)..self.raw.len() {
            self.raw[i].reset(bg);
        }
    }

    /// Clear rows from the top of the visible area to `end_row` (inclusive).
    pub fn clear_above(&mut self, end_row: usize, bg: Color) {
        let sb = self.scrollback_len();
        for i in sb..=(sb + end_row) {
            self.raw[i].reset(bg);
        }
    }

    /// Resize the grid. Handles both growing and shrinking.
    ///
    /// On shrink (fewer rows): excess visible lines move to scrollback.
    /// On grow (more rows): lines are pulled back from scrollback if available.
    /// Column resize: each row is resized (truncated or extended).
    pub fn resize(&mut self, new_rows: usize, new_cols: usize) {
        // Handle column resize first.
        if new_cols != self.cols {
            for row in self.raw.iter_mut() {
                row.resize(new_cols, Cell::default());
            }
            self.cols = new_cols;
        }

        // Handle row resize.
        if new_rows < self.rows {
            // Shrinking: excess visible lines at the bottom become scrollback?
            // Actually, the cursor position matters. For simplicity, we keep
            // the top of the visible area stable and trim from the bottom.
            // The caller (VirtualTerminal) should adjust cursor position.
        } else if new_rows > self.rows {
            // Growing: pull lines from scrollback or add empty lines.
            let lines_needed = new_rows - self.rows;
            let from_scrollback = lines_needed.min(self.scrollback_len());
            // Lines from scrollback are already in the VecDeque — we just
            // expand the "visible" window by adjusting self.rows.
            let new_lines = lines_needed - from_scrollback;
            for _ in 0..new_lines {
                self.raw.push_back(Row::new(self.cols));
            }
        }

        self.rows = new_rows;
    }

    /// Clear the scrollback buffer entirely.
    pub fn clear_scrollback(&mut self) {
        let sb = self.scrollback_len();
        for _ in 0..sb {
            self.raw.pop_front();
        }
    }

    /// Erase `count` characters starting at `(row, col)` in the visible area.
    pub fn erase_chars(&mut self, row: usize, col: usize, count: usize, bg: Color) {
        let r = self.visible_row_mut(row);
        let end = (col + count).min(r.len());
        for c in col..end {
            r[c].reset(bg);
        }
    }

    /// Insert `count` blank cells at `(row, col)`, shifting existing cells right.
    /// Cells that shift past the right edge are lost.
    pub fn insert_chars(&mut self, row: usize, col: usize, count: usize) {
        let r = self.visible_row_mut(row);
        let len = r.len();
        // Shift right from the end.
        for i in (col..len).rev() {
            let target = i + count;
            if target < len {
                r.cells[target] = r.cells[i].clone();
            }
        }
        // Fill inserted positions with blanks.
        for i in col..(col + count).min(len) {
            r.cells[i] = Cell::default();
        }
    }

    /// Delete `count` cells at `(row, col)`, shifting remaining cells left.
    /// New cells at the right edge are blank.
    pub fn delete_chars(&mut self, row: usize, col: usize, count: usize) {
        let r = self.visible_row_mut(row);
        let len = r.len();
        let actual = count.min(len.saturating_sub(col));
        // Shift left.
        for i in col..(len - actual) {
            r.cells[i] = r.cells[i + actual].clone();
        }
        // Fill right edge with blanks.
        for i in (len - actual)..len {
            r.cells[i] = Cell::default();
        }
    }
}
```

**Key design note**: The `Row` uses `Vec<Cell>` rather than a fixed-size array. This allows rows to be different widths during resize transitions and avoids wasted allocations for rows that never receive content. The `raw` field needs to expose `cells` for `insert_chars` and `delete_chars` — either make it `pub(crate)` or provide accessor methods. Use `pub(crate)` on `Row::cells`.

### Step 4: Implement cursor state (`cursor.rs`)

```rust
use crate::cell::CellStyle;

/// Cursor visibility state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Underline,
    Bar,
}

impl Default for CursorShape {
    fn default() -> Self { CursorShape::Block }
}

/// Saved cursor state (for DECSC/DECRC — ESC 7 / ESC 8).
#[derive(Debug, Clone)]
pub struct SavedCursor {
    pub row: usize,
    pub col: usize,
    pub style: CellStyle,
    pub auto_wrap_pending: bool,
    pub origin_mode: bool,
}

/// Terminal cursor state.
#[derive(Debug, Clone)]
pub struct Cursor {
    /// Current row (0-indexed, relative to visible area top).
    pub row: usize,
    /// Current column (0-indexed).
    pub col: usize,
    /// Current style that will be applied to newly written cells.
    pub style: CellStyle,
    /// Cursor shape.
    pub shape: CursorShape,
    /// Whether the cursor is visible.
    pub visible: bool,
    /// Whether a wrap is pending (cursor is past the right margin but hasn't wrapped yet).
    /// This is the "auto-wrap pending" state described in VT102 behavior.
    pub auto_wrap_pending: bool,
    /// Saved cursor state (DECSC/DECRC).
    pub saved: Option<SavedCursor>,
}

impl Cursor {
    pub fn new() -> Self {
        Cursor {
            row: 0,
            col: 0,
            style: CellStyle::default(),
            shape: CursorShape::Block,
            visible: true,
            auto_wrap_pending: false,
            saved: None,
        }
    }

    /// Save cursor state (DECSC — ESC 7).
    pub fn save(&mut self, origin_mode: bool) {
        self.saved = Some(SavedCursor {
            row: self.row,
            col: self.col,
            style: self.style,
            auto_wrap_pending: self.auto_wrap_pending,
            origin_mode,
        });
    }

    /// Restore cursor state (DECRC — ESC 8). Returns the saved origin_mode.
    pub fn restore(&mut self) -> Option<bool> {
        if let Some(saved) = self.saved.take() {
            self.row = saved.row;
            self.col = saved.col;
            self.style = saved.style;
            self.auto_wrap_pending = saved.auto_wrap_pending;
            self.saved = Some(saved.clone());
            Some(saved.origin_mode)
        } else {
            None
        }
    }

    /// Clamp cursor position to the grid bounds.
    pub fn clamp(&mut self, rows: usize, cols: usize) {
        self.row = self.row.min(rows.saturating_sub(1));
        self.col = self.col.min(cols.saturating_sub(1));
        self.auto_wrap_pending = false;
    }
}

impl Default for Cursor {
    fn default() -> Self { Cursor::new() }
}
```

### Step 5: Implement the vte::Perform handler (`parser.rs`)

This is the core of the VT parser integration. We implement `vte::ansi::Handler` (via the `ansi` feature) which gives us typed callbacks instead of raw byte-level `Perform` trait methods. The handler translates escape sequences into grid mutations.

```rust
use vte::{Params, Parser};
use tracing::{trace, warn};
use crate::cell::{Cell, CellFlags, CellStyle, Color};
use crate::cursor::CursorShape;
use crate::grid::Grid;
use crate::cursor::Cursor;

/// Terminal mode flags (DECSET/DECRST).
#[derive(Debug, Clone)]
pub struct TerminalModes {
    /// DECAWM — auto-wrap mode (default: true).
    pub auto_wrap: bool,
    /// DECCKM — cursor keys mode (application vs normal).
    pub application_cursor_keys: bool,
    /// DECOM — origin mode (cursor relative to scroll region).
    pub origin_mode: bool,
    /// DECTCEM — text cursor enable mode (cursor visibility via mode).
    pub cursor_visible: bool,
    /// Bracketed paste mode (Mode 2004).
    pub bracketed_paste: bool,
    /// Mouse tracking modes.
    pub mouse_tracking: MouseMode,
    /// Alternate screen buffer active.
    pub alternate_screen: bool,
    /// Insert mode (IRM).
    pub insert_mode: bool,
    /// Newline mode (LNM): LF also does CR.
    pub newline_mode: bool,
}

impl Default for TerminalModes {
    fn default() -> Self {
        TerminalModes {
            auto_wrap: true,
            application_cursor_keys: false,
            origin_mode: false,
            cursor_visible: true,
            bracketed_paste: false,
            mouse_tracking: MouseMode::None,
            alternate_screen: false,
            insert_mode: false,
            newline_mode: false,
        }
    }
}

/// Mouse tracking mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseMode {
    #[default]
    None,
    /// Mode 1000 — normal tracking (button press/release).
    Normal,
    /// Mode 1002 — button event tracking (press/release/motion with button).
    ButtonEvent,
    /// Mode 1003 — any event tracking (all motion).
    AnyEvent,
}

/// Scroll region (top and bottom margins, 0-indexed inclusive).
#[derive(Debug, Clone, Copy)]
pub struct ScrollRegion {
    pub top: usize,
    pub bottom: usize,
}

/// The VT handler that translates escape sequences into grid operations.
///
/// This struct is NOT the public API — VirtualTerminal (in lib.rs) owns this
/// and delegates parsed bytes to it. The handler modifies the grid and cursor
/// directly.
pub struct VtHandler<'a> {
    pub grid: &'a mut Grid,
    pub cursor: &'a mut Cursor,
    pub modes: &'a mut TerminalModes,
    pub scroll_region: &'a mut ScrollRegion,
    pub title: &'a mut Option<String>,
    pub alt_grid: &'a mut Option<Grid>,
    pub alt_cursor: &'a mut Option<Cursor>,
}

impl<'a> VtHandler<'a> {
    /// Write a character at the current cursor position, advancing the cursor.
    fn write_char(&mut self, ch: char) {
        let width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
        let cols = self.grid.cols();
        let rows = self.grid.rows();

        // Handle auto-wrap pending state.
        if self.cursor.auto_wrap_pending {
            if self.modes.auto_wrap {
                self.cursor.col = 0;
                self.cursor.auto_wrap_pending = false;
                // Mark the current row as wrapped.
                self.grid.visible_row_mut(self.cursor.row).wrapped = true;
                if self.cursor.row == self.scroll_region.bottom {
                    self.grid.scroll_up(self.scroll_region.top, self.scroll_region.bottom);
                } else {
                    self.cursor.row += 1;
                }
            } else {
                // No auto-wrap: overwrite last column.
                self.cursor.col = cols.saturating_sub(1);
                self.cursor.auto_wrap_pending = false;
            }
        }

        // Ensure cursor is in bounds.
        if self.cursor.col >= cols {
            self.cursor.col = cols.saturating_sub(1);
        }
        if self.cursor.row >= rows {
            self.cursor.row = rows.saturating_sub(1);
        }

        // Insert mode: shift characters right.
        if self.modes.insert_mode {
            self.grid.insert_chars(self.cursor.row, self.cursor.col, width);
        }

        // Write the cell.
        let row = self.grid.visible_row_mut(self.cursor.row);
        row[self.cursor.col] = Cell {
            ch,
            width: width as u8,
            style: self.cursor.style,
            extended: None,
        };

        // For wide characters, write a continuation cell.
        if width == 2 && self.cursor.col + 1 < cols {
            row[self.cursor.col + 1] = Cell::wide_continuation();
        }

        // Advance cursor.
        self.cursor.col += width;
        if self.cursor.col >= cols {
            self.cursor.col = cols.saturating_sub(1);
            self.cursor.auto_wrap_pending = true;
        }
    }

    /// Carriage return: move cursor to column 0.
    fn carriage_return(&mut self) {
        self.cursor.col = 0;
        self.cursor.auto_wrap_pending = false;
    }

    /// Line feed: move cursor down, scrolling if at bottom of scroll region.
    fn linefeed(&mut self) {
        if self.cursor.row == self.scroll_region.bottom {
            self.grid.scroll_up(self.scroll_region.top, self.scroll_region.bottom);
        } else if self.cursor.row < self.grid.rows() - 1 {
            self.cursor.row += 1;
        }
        if self.modes.newline_mode {
            self.cursor.col = 0;
        }
        self.cursor.auto_wrap_pending = false;
    }

    /// Reverse index (ESC M): move cursor up, scrolling down if at top of scroll region.
    fn reverse_index(&mut self) {
        if self.cursor.row == self.scroll_region.top {
            self.grid.scroll_down(self.scroll_region.top, self.scroll_region.bottom);
        } else if self.cursor.row > 0 {
            self.cursor.row -= 1;
        }
        self.cursor.auto_wrap_pending = false;
    }

    /// Apply an SGR (Select Graphic Rendition) parameter to the cursor style.
    fn apply_sgr(&mut self, param: u16) {
        match param {
            0 => self.cursor.style = CellStyle::default(),
            1 => self.cursor.style.flags.set(CellFlags::BOLD),
            2 => self.cursor.style.flags.set(CellFlags::DIM),
            3 => self.cursor.style.flags.set(CellFlags::ITALIC),
            4 => self.cursor.style.flags.set(CellFlags::UNDERLINE),
            5 | 6 => self.cursor.style.flags.set(CellFlags::BLINK),
            7 => self.cursor.style.flags.set(CellFlags::INVERSE),
            8 => self.cursor.style.flags.set(CellFlags::HIDDEN),
            9 => self.cursor.style.flags.set(CellFlags::STRIKETHROUGH),
            21 => self.cursor.style.flags.unset(CellFlags::BOLD), // doubly underline or bold-off
            22 => {
                self.cursor.style.flags.unset(CellFlags::BOLD);
                self.cursor.style.flags.unset(CellFlags::DIM);
            }
            23 => self.cursor.style.flags.unset(CellFlags::ITALIC),
            24 => self.cursor.style.flags.unset(CellFlags::UNDERLINE),
            25 => self.cursor.style.flags.unset(CellFlags::BLINK),
            27 => self.cursor.style.flags.unset(CellFlags::INVERSE),
            28 => self.cursor.style.flags.unset(CellFlags::HIDDEN),
            29 => self.cursor.style.flags.unset(CellFlags::STRIKETHROUGH),
            // Standard foreground colors (30-37).
            30..=37 => self.cursor.style.fg = Color::Indexed((param - 30) as u8),
            38 => {} // Extended foreground (handled via sub-params).
            39 => self.cursor.style.fg = Color::Default,
            // Standard background colors (40-47).
            40..=47 => self.cursor.style.bg = Color::Indexed((param - 40) as u8),
            48 => {} // Extended background (handled via sub-params).
            49 => self.cursor.style.bg = Color::Default,
            // Bright foreground colors (90-97).
            90..=97 => self.cursor.style.fg = Color::Indexed((param - 90 + 8) as u8),
            // Bright background colors (100-107).
            100..=107 => self.cursor.style.bg = Color::Indexed((param - 100 + 8) as u8),
            _ => trace!(sgr = param, "unhandled SGR parameter"),
        }
    }
}
```

**Implementation note on vte 0.15 with `ansi` feature**: The `ansi` feature provides the `vte::ansi::Processor` and `vte::ansi::Handler` trait. The `Handler` trait has typed methods like `input(&mut self, c: char)`, `goto(&mut self, row: i32, col: i32)`, `set_scrolling_region(&mut self, top: usize, bottom: Option<usize>)`, etc. If the `ansi` feature's `Handler` trait API does not match what is available in vte 0.15 at implementation time, fall back to the raw `vte::Perform` trait and implement the dispatch manually. The raw `Perform` trait provides:

- `print(char)` — printable character
- `execute(byte)` — C0/C1 control character
- `hook/put/unhook` — DCS sequences
- `osc_dispatch(&[&[u8]], bool)` — OSC sequences
- `csi_dispatch(params, intermediates, ignore, action)` — CSI sequences
- `esc_dispatch(intermediates, ignore, byte)` — ESC sequences

The implementing agent should check the actual vte 0.15 API and use whichever trait is available. The logic in `VtHandler` above applies either way.

### Step 6: Wire it all together in `lib.rs`

```rust
//! shux-vt — Virtual terminal grid and VT parser.
//!
//! Provides per-pane terminal emulation: a VecDeque-based grid that tracks
//! cell content, styles, cursor position, and scrollback. Driven by the
//! vte crate parsing raw PTY output bytes.

mod cell;
mod cursor;
mod grid;
mod parser;

pub use cell::{Cell, CellFlags, CellStyle, Color, ExtendedAttrs, UnderlineStyle};
pub use cursor::{Cursor, CursorShape, SavedCursor};
pub use grid::{Grid, GridConfig, Row};
pub use parser::{MouseMode, ScrollRegion, TerminalModes, VtHandler};

use vte::Parser;

/// Per-pane virtual terminal.
///
/// Owns the grid, cursor, terminal modes, and the vte parser state machine.
/// Feed PTY output bytes via `process()` and read the resulting grid state
/// for rendering.
pub struct VirtualTerminal {
    /// Primary screen grid.
    grid: Grid,
    /// Alternate screen grid (for fullscreen apps like vim).
    alt_grid: Option<Grid>,
    /// Current cursor state.
    cursor: Cursor,
    /// Saved cursor for alternate screen.
    alt_cursor: Option<Cursor>,
    /// Terminal mode flags.
    modes: TerminalModes,
    /// Scroll region (top/bottom margins).
    scroll_region: ScrollRegion,
    /// Window title (set via OSC 0/2).
    title: Option<String>,
    /// vte parser state machine.
    parser: Parser,
    /// Number of visible rows.
    rows: usize,
    /// Number of columns.
    cols: usize,
}

impl VirtualTerminal {
    /// Create a new virtual terminal with the given dimensions.
    pub fn new(rows: usize, cols: usize) -> Self {
        Self::with_config(rows, cols, GridConfig::default())
    }

    /// Create a new virtual terminal with custom grid configuration.
    pub fn with_config(rows: usize, cols: usize, config: GridConfig) -> Self {
        VirtualTerminal {
            grid: Grid::new(rows, cols, config),
            alt_grid: None,
            cursor: Cursor::new(),
            alt_cursor: None,
            modes: TerminalModes::default(),
            scroll_region: ScrollRegion {
                top: 0,
                bottom: rows.saturating_sub(1),
            },
            title: None,
            parser: Parser::new(),
            rows,
            cols,
        }
    }

    /// Process raw PTY output bytes through the VT parser.
    ///
    /// This is the main entry point for feeding terminal data.
    /// Each byte is parsed by vte, which calls back into our handler
    /// to mutate the grid and cursor.
    pub fn process(&mut self, bytes: &[u8]) {
        // We need to create a VtHandler that borrows our fields mutably.
        // Because vte::Parser::advance requires &mut self and the Perform
        // callback needs &mut to our state, we process byte-by-byte with
        // a temporary handler.
        for &byte in bytes {
            let mut handler = VtHandler {
                grid: &mut self.grid,
                cursor: &mut self.cursor,
                modes: &mut self.modes,
                scroll_region: &mut self.scroll_region,
                title: &mut self.title,
                alt_grid: &mut self.alt_grid,
                alt_cursor: &mut self.alt_cursor,
            };
            self.parser.advance(&mut handler, byte);
        }
    }

    /// Access the current (active) grid.
    pub fn grid(&self) -> &Grid { &self.grid }

    /// Access the cursor state.
    pub fn cursor(&self) -> &Cursor { &self.cursor }

    /// Access terminal modes.
    pub fn modes(&self) -> &TerminalModes { &self.modes }

    /// Get the window title (set by OSC 0/2).
    pub fn title(&self) -> Option<&str> { self.title.as_deref() }

    /// Whether alternate screen is active.
    pub fn is_alternate_screen(&self) -> bool { self.modes.alternate_screen }

    /// Get the current scroll region.
    pub fn scroll_region(&self) -> &ScrollRegion { &self.scroll_region }

    /// Resize the virtual terminal.
    ///
    /// This resizes both primary and alternate grids, adjusts the scroll
    /// region, and clamps the cursor position.
    pub fn resize(&mut self, rows: usize, cols: usize) {
        self.grid.resize(rows, cols);
        if let Some(ref mut alt) = self.alt_grid {
            alt.resize(rows, cols);
        }
        self.rows = rows;
        self.cols = cols;
        self.scroll_region = ScrollRegion {
            top: 0,
            bottom: rows.saturating_sub(1),
        };
        self.cursor.clamp(rows, cols);
    }

    /// Switch to alternate screen buffer (DECSET 1049).
    pub fn enter_alternate_screen(&mut self) {
        if !self.modes.alternate_screen {
            let config = GridConfig { max_scrollback: 0 }; // No scrollback on alt screen.
            let alt_grid = Grid::new(self.rows, self.cols, config);
            let alt_cursor = Cursor::new();
            self.alt_grid = Some(std::mem::replace(&mut self.grid, alt_grid));
            self.alt_cursor = Some(std::mem::replace(&mut self.cursor, alt_cursor));
            self.modes.alternate_screen = true;
        }
    }

    /// Switch back to primary screen buffer (DECRST 1049).
    pub fn leave_alternate_screen(&mut self) {
        if self.modes.alternate_screen {
            if let Some(primary_grid) = self.alt_grid.take() {
                self.grid = primary_grid;
            }
            if let Some(primary_cursor) = self.alt_cursor.take() {
                self.cursor = primary_cursor;
            }
            self.modes.alternate_screen = false;
        }
    }

    /// Clear the scrollback buffer.
    pub fn clear_scrollback(&mut self) {
        self.grid.clear_scrollback();
    }

    /// Get the number of scrollback lines.
    pub fn scrollback_len(&self) -> usize {
        self.grid.scrollback_len()
    }
}
```

### Step 7: Implement the vte::Perform trait

The `VtHandler` must implement `vte::Perform` so it can be passed to `Parser::advance`. This is the bridge between the raw byte parser and our typed handler methods.

```rust
// In parser.rs, add:

impl<'a> vte::Perform for VtHandler<'a> {
    fn print(&mut self, ch: char) {
        self.write_char(ch);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            // BEL — bell.
            0x07 => { /* emit bell event in the future */ }
            // BS — backspace.
            0x08 => {
                if self.cursor.col > 0 {
                    self.cursor.col -= 1;
                    self.cursor.auto_wrap_pending = false;
                }
            }
            // HT — horizontal tab.
            0x09 => {
                let next_tab = (self.cursor.col / 8 + 1) * 8;
                self.cursor.col = next_tab.min(self.grid.cols() - 1);
                self.cursor.auto_wrap_pending = false;
            }
            // LF, VT, FF — linefeed variants.
            0x0A | 0x0B | 0x0C => self.linefeed(),
            // CR — carriage return.
            0x0D => self.carriage_return(),
            // SO (0x0E), SI (0x0F) — character set shift (ignored for now).
            _ => trace!(byte, "unhandled C0 control"),
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
        let params_vec: Vec<u16> = params.iter()
            .flat_map(|subparam| subparam.iter().copied())
            .collect();
        let p = |idx: usize, default: u16| -> u16 {
            params_vec.get(idx).copied().filter(|&v| v != 0).unwrap_or(default)
        };
        let rows = self.grid.rows();
        let cols = self.grid.cols();

        match (action, intermediates) {
            // CUU — Cursor Up.
            ('A', []) => {
                let n = p(0, 1) as usize;
                self.cursor.row = self.cursor.row.saturating_sub(n);
                self.cursor.auto_wrap_pending = false;
            }
            // CUD — Cursor Down.
            ('B', []) => {
                let n = p(0, 1) as usize;
                self.cursor.row = (self.cursor.row + n).min(rows - 1);
                self.cursor.auto_wrap_pending = false;
            }
            // CUF — Cursor Forward.
            ('C', []) => {
                let n = p(0, 1) as usize;
                self.cursor.col = (self.cursor.col + n).min(cols - 1);
                self.cursor.auto_wrap_pending = false;
            }
            // CUB — Cursor Backward.
            ('D', []) => {
                let n = p(0, 1) as usize;
                self.cursor.col = self.cursor.col.saturating_sub(n);
                self.cursor.auto_wrap_pending = false;
            }
            // CUP / HVP — Cursor Position.
            ('H', []) | ('f', []) => {
                let row = (p(0, 1) as usize).saturating_sub(1).min(rows - 1);
                let col = (p(1, 1) as usize).saturating_sub(1).min(cols - 1);
                self.cursor.row = row;
                self.cursor.col = col;
                self.cursor.auto_wrap_pending = false;
            }
            // ED — Erase in Display.
            ('J', []) => {
                let bg = self.cursor.style.bg;
                match p(0, 0) {
                    0 => {
                        // Clear from cursor to end.
                        self.grid.erase_chars(
                            self.cursor.row, self.cursor.col,
                            cols - self.cursor.col, bg,
                        );
                        self.grid.clear_below(self.cursor.row + 1, bg);
                    }
                    1 => {
                        // Clear from beginning to cursor.
                        self.grid.clear_above(self.cursor.row.saturating_sub(1), bg);
                        self.grid.erase_chars(self.cursor.row, 0, self.cursor.col + 1, bg);
                    }
                    2 => {
                        // Clear entire screen.
                        self.grid.clear_visible(bg);
                    }
                    3 => {
                        // Clear screen + scrollback (xterm extension).
                        self.grid.clear_visible(bg);
                        self.grid.clear_scrollback();
                    }
                    _ => {}
                }
            }
            // EL — Erase in Line.
            ('K', []) => {
                let bg = self.cursor.style.bg;
                match p(0, 0) {
                    0 => self.grid.erase_chars(self.cursor.row, self.cursor.col, cols - self.cursor.col, bg),
                    1 => self.grid.erase_chars(self.cursor.row, 0, self.cursor.col + 1, bg),
                    2 => self.grid.erase_chars(self.cursor.row, 0, cols, bg),
                    _ => {}
                }
            }
            // IL — Insert Lines.
            ('L', []) => {
                let n = p(0, 1) as usize;
                for _ in 0..n {
                    self.grid.scroll_down(self.cursor.row, self.scroll_region.bottom);
                }
            }
            // DL — Delete Lines.
            ('M', []) => {
                let n = p(0, 1) as usize;
                for _ in 0..n {
                    self.grid.scroll_up(self.cursor.row, self.scroll_region.bottom);
                }
            }
            // ICH — Insert Characters.
            ('@', []) => {
                let n = p(0, 1) as usize;
                self.grid.insert_chars(self.cursor.row, self.cursor.col, n);
            }
            // DCH — Delete Characters.
            ('P', []) => {
                let n = p(0, 1) as usize;
                self.grid.delete_chars(self.cursor.row, self.cursor.col, n);
            }
            // ECH — Erase Characters.
            ('X', []) => {
                let n = p(0, 1) as usize;
                self.grid.erase_chars(self.cursor.row, self.cursor.col, n, self.cursor.style.bg);
            }
            // SGR — Select Graphic Rendition.
            ('m', []) => {
                if params_vec.is_empty() {
                    self.apply_sgr(0);
                    return;
                }
                let mut i = 0;
                while i < params_vec.len() {
                    match params_vec[i] {
                        38 if i + 4 < params_vec.len() && params_vec[i + 1] == 2 => {
                            // 38;2;R;G;B — 24-bit foreground.
                            self.cursor.style.fg = Color::Rgb(
                                params_vec[i + 2] as u8,
                                params_vec[i + 3] as u8,
                                params_vec[i + 4] as u8,
                            );
                            i += 5;
                        }
                        38 if i + 2 < params_vec.len() && params_vec[i + 1] == 5 => {
                            // 38;5;N — 256-color foreground.
                            self.cursor.style.fg = Color::Indexed(params_vec[i + 2] as u8);
                            i += 3;
                        }
                        48 if i + 4 < params_vec.len() && params_vec[i + 1] == 2 => {
                            // 48;2;R;G;B — 24-bit background.
                            self.cursor.style.bg = Color::Rgb(
                                params_vec[i + 2] as u8,
                                params_vec[i + 3] as u8,
                                params_vec[i + 4] as u8,
                            );
                            i += 5;
                        }
                        48 if i + 2 < params_vec.len() && params_vec[i + 1] == 5 => {
                            // 48;5;N — 256-color background.
                            self.cursor.style.bg = Color::Indexed(params_vec[i + 2] as u8);
                            i += 3;
                        }
                        other => {
                            self.apply_sgr(other);
                            i += 1;
                        }
                    }
                }
            }
            // DECSTBM — Set Scrolling Region.
            ('r', []) => {
                let top = (p(0, 1) as usize).saturating_sub(1);
                let bottom = (p(1, rows as u16) as usize).saturating_sub(1).min(rows - 1);
                if top < bottom {
                    self.scroll_region.top = top;
                    self.scroll_region.bottom = bottom;
                }
                self.cursor.row = 0;
                self.cursor.col = 0;
                self.cursor.auto_wrap_pending = false;
            }
            // DECSET — set private mode.
            ('h', [b'?']) => {
                for &param in &params_vec {
                    self.set_private_mode(param, true);
                }
            }
            // DECRST — reset private mode.
            ('l', [b'?']) => {
                for &param in &params_vec {
                    self.set_private_mode(param, false);
                }
            }
            // DECSCUSR — Set Cursor Style (CSI Ps SP q).
            ('q', [b' ']) => {
                self.cursor.shape = match p(0, 1) {
                    0 | 1 => CursorShape::Block,
                    2 => CursorShape::Block, // steady block
                    3 | 4 => CursorShape::Underline,
                    5 | 6 => CursorShape::Bar,
                    _ => CursorShape::Block,
                };
            }
            _ => {
                trace!(
                    action = %action,
                    intermediates = ?intermediates,
                    params = ?params_vec,
                    "unhandled CSI sequence"
                );
            }
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        match (byte, intermediates) {
            // DECSC — Save Cursor (ESC 7).
            (b'7', []) => self.cursor.save(self.modes.origin_mode),
            // DECRC — Restore Cursor (ESC 8).
            (b'8', []) => {
                if let Some(origin) = self.cursor.restore() {
                    self.modes.origin_mode = origin;
                }
            }
            // RI — Reverse Index (ESC M).
            (b'M', []) => self.reverse_index(),
            // IND — Index (ESC D) — move cursor down, scroll if needed.
            (b'D', []) => self.linefeed(),
            // NEL — Next Line (ESC E).
            (b'E', []) => {
                self.carriage_return();
                self.linefeed();
            }
            // RIS — Full Reset (ESC c).
            (b'c', []) => {
                self.grid.clear_visible(Color::Default);
                self.grid.clear_scrollback();
                *self.cursor = Cursor::new();
                *self.modes = TerminalModes::default();
                self.scroll_region.top = 0;
                self.scroll_region.bottom = self.grid.rows().saturating_sub(1);
            }
            _ => {
                trace!(byte, intermediates = ?intermediates, "unhandled ESC sequence");
            }
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if params.is_empty() {
            return;
        }
        match params[0] {
            // OSC 0 — Set Icon Name and Window Title.
            b"0" | b"2" => {
                if let Some(title_bytes) = params.get(1) {
                    if let Ok(title) = std::str::from_utf8(title_bytes) {
                        *self.title = Some(title.to_string());
                    }
                }
            }
            _ => {
                trace!(osc = ?params[0], "unhandled OSC sequence");
            }
        }
    }

    fn hook(&mut self, _params: &vte::Params, _intermediates: &[u8], _ignore: bool, _action: char) {
        // DCS sequences — not needed for M0.
    }

    fn put(&mut self, _byte: u8) {
        // DCS payload — not needed for M0.
    }

    fn unhook(&mut self) {
        // DCS termination — not needed for M0.
    }
}

impl<'a> VtHandler<'a> {
    /// Handle DECSET/DECRST private mode toggles.
    fn set_private_mode(&mut self, mode: u16, enable: bool) {
        match mode {
            // DECCKM — Cursor keys mode.
            1 => self.modes.application_cursor_keys = enable,
            // DECOM — Origin mode.
            6 => self.modes.origin_mode = enable,
            // DECAWM — Auto-wrap mode.
            7 => self.modes.auto_wrap = enable,
            // DECTCEM — Text cursor enable.
            25 => {
                self.modes.cursor_visible = enable;
                self.cursor.visible = enable;
            }
            // Mouse tracking: normal (1000).
            1000 => {
                self.modes.mouse_tracking = if enable { MouseMode::Normal } else { MouseMode::None };
            }
            // Mouse tracking: button event (1002).
            1002 => {
                self.modes.mouse_tracking = if enable { MouseMode::ButtonEvent } else { MouseMode::None };
            }
            // Mouse tracking: any event (1003).
            1003 => {
                self.modes.mouse_tracking = if enable { MouseMode::AnyEvent } else { MouseMode::None };
            }
            // Alternate screen buffer (1047, 1049).
            1047 | 1049 => {
                if enable {
                    if mode == 1049 {
                        self.cursor.save(self.modes.origin_mode);
                    }
                    // Enter alternate screen: swap grids.
                    let rows = self.grid.rows();
                    let cols = self.grid.cols();
                    let config = GridConfig { max_scrollback: 0 };
                    let alt_grid = Grid::new(rows, cols, config);
                    let alt_cursor = Cursor::new();
                    *self.alt_grid = Some(std::mem::replace(self.grid, alt_grid));
                    *self.alt_cursor = Some(std::mem::replace(self.cursor, alt_cursor));
                    self.modes.alternate_screen = true;
                } else {
                    // Leave alternate screen: restore grids.
                    if let Some(primary_grid) = self.alt_grid.take() {
                        *self.grid = primary_grid;
                    }
                    if let Some(primary_cursor) = self.alt_cursor.take() {
                        *self.cursor = primary_cursor;
                    }
                    self.modes.alternate_screen = false;
                    if mode == 1049 {
                        let origin = self.cursor.restore();
                        if let Some(o) = origin {
                            self.modes.origin_mode = o;
                        }
                    }
                }
            }
            // Bracketed paste mode (2004).
            2004 => self.modes.bracketed_paste = enable,
            _ => trace!(mode, enable, "unhandled private mode"),
        }
    }
}
```

### Step 8: Write unit tests

Create comprehensive tests in each module. Key test scenarios:

1. **Cell tests**: creation, reset, wide character markers, style application
2. **Grid tests**: write and read, scrolling, scroll regions, resize, clear operations, scrollback limits
3. **Cursor tests**: movement, clamping, save/restore
4. **Parser integration tests**: feed real ANSI sequences, verify grid state

```rust
// In grid.rs, add at the bottom:
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_grid_dimensions() {
        let grid = Grid::new(24, 80, GridConfig::default());
        assert_eq!(grid.rows(), 24);
        assert_eq!(grid.cols(), 80);
        assert_eq!(grid.scrollback_len(), 0);
    }

    #[test]
    fn test_scroll_up_adds_scrollback() {
        let mut grid = Grid::new(3, 10, GridConfig::default());
        grid.visible_row_mut(0)[0].ch = 'A';
        grid.scroll_up(0, 2);
        // Row with 'A' should now be in scrollback.
        assert_eq!(grid.scrollback_len(), 1);
        assert_eq!(grid.scrollback_row(0).unwrap()[0].ch, 'A');
        // New bottom row should be empty.
        assert_eq!(grid.visible_row(2)[0].ch, ' ');
    }

    #[test]
    fn test_scrollback_limit() {
        let config = GridConfig { max_scrollback: 2 };
        let mut grid = Grid::new(3, 10, config);
        for i in 0..5 {
            grid.visible_row_mut(0)[0].ch = char::from(b'A' + i as u8);
            grid.scroll_up(0, 2);
        }
        // Only 2 lines should be in scrollback.
        assert_eq!(grid.scrollback_len(), 2);
    }

    #[test]
    fn test_scroll_region() {
        let mut grid = Grid::new(5, 10, GridConfig::default());
        grid.visible_row_mut(1)[0].ch = 'X';
        grid.visible_row_mut(3)[0].ch = 'Y';
        // Scroll region 1..3 up: row 1 disappears, row 3 shifts to row 2.
        grid.scroll_up(1, 3);
        assert_eq!(grid.visible_row(2)[0].ch, 'Y');
        assert_eq!(grid.visible_row(3)[0].ch, ' '); // New empty row.
        assert_eq!(grid.scrollback_len(), 0); // No scrollback for region scroll.
    }

    #[test]
    fn test_resize_grow() {
        let mut grid = Grid::new(3, 10, GridConfig::default());
        grid.resize(5, 15);
        assert_eq!(grid.rows(), 5);
        assert_eq!(grid.cols(), 15);
    }

    #[test]
    fn test_resize_shrink_columns() {
        let mut grid = Grid::new(3, 10, GridConfig::default());
        grid.visible_row_mut(0)[9].ch = 'Z';
        grid.resize(3, 5);
        assert_eq!(grid.cols(), 5);
        // Column 9 is gone.
        assert_eq!(grid.visible_row(0).len(), 5);
    }

    #[test]
    fn test_clear_visible() {
        let mut grid = Grid::new(3, 10, GridConfig::default());
        grid.visible_row_mut(0)[0].ch = 'A';
        grid.visible_row_mut(1)[0].ch = 'B';
        grid.clear_visible(Color::Default);
        assert_eq!(grid.visible_row(0)[0].ch, ' ');
        assert_eq!(grid.visible_row(1)[0].ch, ' ');
    }

    #[test]
    fn test_insert_delete_chars() {
        let mut grid = Grid::new(1, 5, GridConfig::default());
        grid.visible_row_mut(0)[0].ch = 'A';
        grid.visible_row_mut(0)[1].ch = 'B';
        grid.visible_row_mut(0)[2].ch = 'C';
        grid.insert_chars(0, 1, 1);
        assert_eq!(grid.visible_row(0)[0].ch, 'A');
        assert_eq!(grid.visible_row(0)[1].ch, ' '); // inserted
        assert_eq!(grid.visible_row(0)[2].ch, 'B'); // shifted
        assert_eq!(grid.visible_row(0)[3].ch, 'C'); // shifted

        grid.delete_chars(0, 1, 1);
        assert_eq!(grid.visible_row(0)[1].ch, 'B'); // shifted back
    }
}

// In lib.rs, add at the bottom:
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_plain_text() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"Hello, world!");
        assert_eq!(vt.grid().visible_row(0)[0].ch, 'H');
        assert_eq!(vt.grid().visible_row(0)[4].ch, 'o');
        assert_eq!(vt.cursor().col, 13);
    }

    #[test]
    fn test_process_newline() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"line1\r\nline2");
        assert_eq!(vt.grid().visible_row(0)[0].ch, 'l');
        assert_eq!(vt.grid().visible_row(1)[0].ch, 'l');
        assert_eq!(vt.cursor().row, 1);
    }

    #[test]
    fn test_cursor_movement() {
        let mut vt = VirtualTerminal::new(24, 80);
        // CSI 5;10H — move cursor to row 5, col 10.
        vt.process(b"\x1b[5;10H");
        assert_eq!(vt.cursor().row, 4); // 0-indexed
        assert_eq!(vt.cursor().col, 9); // 0-indexed
    }

    #[test]
    fn test_sgr_colors() {
        let mut vt = VirtualTerminal::new(24, 80);
        // Set red foreground (SGR 31), then write a character.
        vt.process(b"\x1b[31mX");
        let cell = &vt.grid().visible_row(0)[0];
        assert_eq!(cell.ch, 'X');
        assert_eq!(cell.style.fg, Color::Indexed(1)); // red
    }

    #[test]
    fn test_sgr_24bit_color() {
        let mut vt = VirtualTerminal::new(24, 80);
        // Set 24-bit foreground: SGR 38;2;255;128;0.
        vt.process(b"\x1b[38;2;255;128;0mX");
        let cell = &vt.grid().visible_row(0)[0];
        assert_eq!(cell.style.fg, Color::Rgb(255, 128, 0));
    }

    #[test]
    fn test_erase_in_display() {
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"AAAAAAAAAA\r\nBBBBBBBBBB\r\nCCCCCCCCCC");
        // Move to row 2, col 0 and clear above.
        vt.process(b"\x1b[2;1H\x1b[1J");
        // Row 0 should be cleared.
        assert_eq!(vt.grid().visible_row(0)[0].ch, ' ');
    }

    #[test]
    fn test_scroll_region() {
        let mut vt = VirtualTerminal::new(5, 10);
        // Set scroll region to lines 2-4 (1-indexed: CSI 2;4r).
        vt.process(b"\x1b[2;4r");
        assert_eq!(vt.scroll_region().top, 1);
        assert_eq!(vt.scroll_region().bottom, 3);
    }

    #[test]
    fn test_auto_wrap() {
        let mut vt = VirtualTerminal::new(3, 5);
        vt.process(b"ABCDE"); // Fills the row exactly.
        // Cursor should be at col 4 with wrap pending.
        assert_eq!(vt.cursor().col, 4);
        // Next character should wrap.
        vt.process(b"F");
        assert_eq!(vt.cursor().row, 1);
        assert_eq!(vt.grid().visible_row(1)[0].ch, 'F');
    }

    #[test]
    fn test_alternate_screen() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"primary content");
        assert!(!vt.is_alternate_screen());
        // DECSET 1049 — enter alternate screen.
        vt.process(b"\x1b[?1049h");
        assert!(vt.is_alternate_screen());
        vt.process(b"alt content");
        // DECRST 1049 — leave alternate screen.
        vt.process(b"\x1b[?1049l");
        assert!(!vt.is_alternate_screen());
        assert_eq!(vt.grid().visible_row(0)[0].ch, 'p'); // primary content restored
    }

    #[test]
    fn test_osc_title() {
        let mut vt = VirtualTerminal::new(24, 80);
        // OSC 2 — set window title.
        vt.process(b"\x1b]2;my window title\x07");
        assert_eq!(vt.title(), Some("my window title"));
    }

    #[test]
    fn test_wide_character() {
        let mut vt = VirtualTerminal::new(24, 80);
        // Write a wide character (CJK character, width 2).
        vt.process("你".as_bytes());
        assert_eq!(vt.grid().visible_row(0)[0].ch, '你');
        assert_eq!(vt.grid().visible_row(0)[0].width, 2);
        assert!(vt.grid().visible_row(0)[1].is_wide_continuation());
        assert_eq!(vt.cursor().col, 2);
    }

    #[test]
    fn test_resize() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"test");
        vt.resize(10, 40);
        assert_eq!(vt.grid().rows(), 10);
        assert_eq!(vt.grid().cols(), 40);
        // Content should be preserved.
        assert_eq!(vt.grid().visible_row(0)[0].ch, 't');
    }
}
```

---

## Verification

### Functional

```bash
# Build the shux-vt crate
cargo build -p shux-vt

# Check for clippy warnings
cargo clippy -p shux-vt -- -D warnings

# Format check
cargo fmt -p shux-vt -- --check
```

### Tests

```bash
# Run all shux-vt tests
cargo nextest run -p shux-vt

# L2 PTY integration smoke test
cargo nextest run -p shux-vt -- vt::tests::pty_integration

# Run with output for debugging
cargo nextest run -p shux-vt --no-capture

# Run specific test module
cargo nextest run -p shux-vt -- grid::tests
cargo nextest run -p shux-vt -- tests::test_process_plain_text
```

---

## Completion Criteria

- [ ] `crates/shux-vt/src/cell.rs` — Cell, CellStyle, CellFlags, Color types with compact representation
- [ ] `crates/shux-vt/src/grid.rs` — VecDeque-based Grid with Row type, scrollback, scroll regions, clear, resize, insert/delete chars
- [ ] `crates/shux-vt/src/cursor.rs` — Cursor with position, style, shape, visibility, save/restore
- [ ] `crates/shux-vt/src/parser.rs` — VtHandler implementing vte::Perform with CSI, ESC, OSC, SGR handling
- [ ] `crates/shux-vt/src/lib.rs` — VirtualTerminal public API with process(), grid access, resize, alternate screen
- [ ] `crates/shux-vt/Cargo.toml` — Dependencies: vte (with ansi feature), unicode-width, tracing, thiserror
- [ ] Grid supports configurable scrollback (default 5000 lines per PRD §5.5)
- [ ] Scrollback is lazily allocated (no pre-allocation for empty panes)
- [ ] Compact cell representation (inline for ASCII, extended for rare attributes)
- [ ] Wide character support (CJK, emoji) with continuation cells
- [ ] SGR handling: basic attributes, 256-color, 24-bit RGB (foreground and background)
- [ ] Cursor movement: CUU, CUD, CUF, CUB, CUP, HVP
- [ ] Screen operations: ED, EL, IL, DL, ICH, DCH, ECH
- [ ] Scroll region support (DECSTBM)
- [ ] Private mode handling: DECAWM, DECTCEM, alternate screen (1049), bracketed paste (2004), mouse tracking
- [ ] OSC title parsing (OSC 0/2)
- [ ] Alternate screen buffer (enter/leave, grid swap, cursor save/restore)
- [ ] Auto-wrap behavior (pending state, configurable via DECAWM)
- [ ] Resize preserves content and adjusts cursor
- [ ] Unit tests for grid operations pass
- [ ] Unit tests for parser integration pass (plain text, escape sequences, colors, cursor movement)
- [ ] L2 integration test feeds real PTY output into `VirtualTerminal` and validates final grid/scrollback state
- [ ] `cargo clippy -p shux-vt -- -D warnings` passes
- [ ] `cargo fmt -p shux-vt -- --check` passes

---

## Commit Message

```
feat(vt): implement virtual terminal grid with VecDeque and vte parser

- VecDeque-based grid with configurable scrollback (default 5000 lines)
- Compact cell representation with extended storage for styled/wide chars
- vte 0.15 parser integration for ANSI escape sequence handling
- Cursor state tracking with save/restore (DECSC/DECRC)
- Alternate screen buffer support (DECSET/DECRST 1049)
- SGR handling: bold, dim, italic, underline, 256-color, 24-bit RGB
- Scroll region support (DECSTBM), wide character handling
- Unit tests for grid operations and parser integration
```

---

## Session Protocol

1. **Before starting:** Read `CLAUDE.md`, `docs/PRD.md` §5.5 (Virtual terminal grid), §15.2 (vte crate), and §14.1 (performance budgets). Verify task 000 is complete (workspace compiles).
2. **During implementation:**
   - Start with `cell.rs` — get the data types right first.
   - Then `grid.rs` — the VecDeque mechanics are the most complex part. Write grid tests immediately.
   - Then `cursor.rs` — straightforward state tracking.
   - Then `parser.rs` — implement vte::Perform. Check the actual vte 0.15 API (the `ansi` feature may provide `Handler` trait instead of raw `Perform`). Adapt the implementation accordingly.
   - Finally `lib.rs` — wire everything into `VirtualTerminal` and write integration tests.
   - Run `cargo clippy -p shux-vt -- -D warnings` after each file.
3. **Key gotchas:**
   - The `Row::cells` field must be accessible from `Grid` methods (`insert_chars`, `delete_chars`). Use `pub(crate)`.
   - vte 0.15 `Params` iteration: `params.iter()` yields `&[u16]` sub-parameter slices. Handle the nesting correctly.
   - The alternate screen swap in the `vte::Perform` handler borrows `self.grid` and `self.alt_grid` — both through `VtHandler`. This works because `VtHandler` holds `&mut Grid` and `&mut Option<Grid>` separately.
   - Memory: verify with a quick calculation that 10 panes x 5000 scrollback x 200 cols x `size_of::<Cell>()` fits within 80 MB. Adjust cell size if needed.
4. **After:** Run full test suite (`cargo nextest run -p shux-vt`). Update `docs/PROGRESS.md` (mark 005 done). Update `CLAUDE.md` Learnings with any discoveries about vte API, cell sizing, or grid behavior.
