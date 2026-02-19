# 009 — Render Compositor (Single Pane)

**Status:** Pending
**Depends On:** 005, 006
**Parallelizable With:** 007, 008

---

## Problem

The daemon and TUI client need a way to compose the VirtualTerminal grid (built in task 005) into actual terminal output. Without a render compositor, there is no visual output -- the terminal grid data exists in memory but never reaches the user's screen. This task builds the rendering pipeline: read cells from the VirtualTerminal, diff against the previous frame, and emit only the changed cells as crossterm commands. This is the critical path for perceived performance -- PRD section 14.1 sets a p50 target of 8ms or less for keypress-to-visible-update latency, and the compositor is the largest contributor to that budget.

Single-pane rendering is scoped here. Multi-pane rendering with borders, layout-aware composition, and status bar integration come in task 017.

## PRD Reference

- **section 4.4** — `RenderCompositor`: "Composes VirtualTerminal grids + chrome (borders, status bar, overlays) into per-client output. Diff-based incremental rendering."
- **section 5.5** — Virtual terminal grid: VecDeque-based cell representation
- **section 14.1** — Performance budgets: keypress to visible update p50 <= 8ms, p99 <= 25ms
- **section 15.2** — crossterm 0.29 for client terminal I/O; ratatui 0.30 optionally for chrome
- **section 6.1 (Terminal compatibility)** — Synchronized output (Mode 2026), truecolor when available

---

## Files to Create

- `crates/shux-ui/src/compositor.rs` — RenderCompositor: orchestrates frame composition and diffing
- `crates/shux-ui/src/buffer.rs` — FrameBuffer: double-buffered cell grid for diff-based rendering
- `crates/shux-ui/src/render.rs` — RenderBackend: crossterm output abstraction (MoveTo, Print, SetColors, etc.)

## Files to Modify

- `crates/shux-ui/Cargo.toml` — Add dependencies: crossterm, shux-vt
- `crates/shux-ui/src/lib.rs` — Re-export compositor, buffer, render modules

---

## Execution Steps

### Step 1: Define the FrameBuffer cell type

The FrameBuffer operates on a flat grid of `RenderCell` values. Each cell stores the character, foreground color, background color, and style attributes. This must match or translate from the VirtualTerminal's cell representation (task 005).

```rust
// crates/shux-ui/src/buffer.rs

use crossterm::style::{Attribute, Color};

/// A single cell in the render buffer. Compact representation optimized for
/// diffing -- we compare the entire struct with PartialEq to decide whether
/// a cell needs to be redrawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderCell {
    /// The character to display. Space for empty cells. Wide characters
    /// occupy the primary cell; the continuation cell is marked with
    /// `wide_continuation = true`.
    pub ch: char,

    /// Foreground color. None means "terminal default".
    pub fg: Option<Color>,

    /// Background color. None means "terminal default".
    pub bg: Option<Color>,

    /// Style attributes (bold, italic, underline, etc.).
    pub attrs: RenderAttrs,

    /// True if this cell is the trailing half of a wide (CJK) character.
    /// The compositor skips these cells during output -- the primary cell's
    /// character already occupies both columns.
    pub wide_continuation: bool,
}

/// Bitflag-style attributes for rendering. Using an explicit struct rather
/// than crossterm's Attributes because we need PartialEq/Eq for diffing
/// and want to control the representation precisely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RenderAttrs {
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub blink: bool,
    pub reverse: bool,
    pub hidden: bool,
    pub strikethrough: bool,
}

impl Default for RenderCell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: None,
            bg: None,
            attrs: RenderAttrs::default(),
            wide_continuation: false,
        }
    }
}
```

### Step 2: Build the FrameBuffer with double-buffering

The FrameBuffer maintains two grids: the current frame (being written to) and the previous frame (what was last rendered to the terminal). Diffing these two grids produces the minimal set of cells that need updating.

```rust
// crates/shux-ui/src/buffer.rs (continued)

/// Double-buffered frame buffer for diff-based rendering.
///
/// The compositor writes the new frame into `current`, then calls `diff()`
/// to get the list of changed cells, then swaps current into previous.
pub struct FrameBuffer {
    width: u16,
    height: u16,
    current: Vec<RenderCell>,
    previous: Vec<RenderCell>,
}

/// A cell that has changed between frames and needs to be redrawn.
#[derive(Debug)]
pub struct DirtyCell {
    pub col: u16,
    pub row: u16,
    pub cell: RenderCell,
}

impl FrameBuffer {
    /// Create a new FrameBuffer with the given dimensions. Both buffers
    /// are initialized to blank (space, default colors).
    pub fn new(width: u16, height: u16) -> Self {
        let size = (width as usize) * (height as usize);
        Self {
            width,
            height,
            current: vec![RenderCell::default(); size],
            previous: vec![RenderCell::default(); size],
        }
    }

    /// Resize the buffer. Both buffers are cleared to blank. This forces
    /// a full redraw on the next frame, which is correct behavior after
    /// a terminal resize.
    pub fn resize(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
        let size = (width as usize) * (height as usize);
        self.current = vec![RenderCell::default(); size];
        self.previous = vec![RenderCell::default(); size];
    }

    /// Get a mutable reference to a cell in the current buffer.
    /// Returns None if coordinates are out of bounds.
    pub fn cell_mut(&mut self, col: u16, row: u16) -> Option<&mut RenderCell> {
        if col < self.width && row < self.height {
            let idx = (row as usize) * (self.width as usize) + (col as usize);
            Some(&mut self.current[idx])
        } else {
            None
        }
    }

    /// Write a cell directly into the current buffer at (col, row).
    pub fn set_cell(&mut self, col: u16, row: u16, cell: RenderCell) {
        if col < self.width && row < self.height {
            let idx = (row as usize) * (self.width as usize) + (col as usize);
            self.current[idx] = cell;
        }
    }

    /// Clear the current buffer to blank cells.
    pub fn clear_current(&mut self) {
        self.current.fill(RenderCell::default());
    }

    /// Compute the diff between current and previous frames. Returns a
    /// list of cells that have changed. After calling this, the caller
    /// should call `swap()` to promote current to previous.
    pub fn diff(&self) -> Vec<DirtyCell> {
        let mut dirty = Vec::new();
        for row in 0..self.height {
            for col in 0..self.width {
                let idx = (row as usize) * (self.width as usize) + (col as usize);
                if self.current[idx] != self.previous[idx] {
                    // Skip wide-char continuation cells; the primary cell
                    // handles rendering both columns.
                    if !self.current[idx].wide_continuation {
                        dirty.push(DirtyCell {
                            col,
                            row,
                            cell: self.current[idx].clone(),
                        });
                    }
                }
            }
        }
        dirty
    }

    /// Swap: copy current into previous. Call this after rendering the
    /// diff to the terminal.
    pub fn swap(&mut self) {
        self.previous.clone_from(&self.current);
    }

    /// Force a full redraw on the next frame by clearing the previous
    /// buffer. Useful after terminal resize or when the terminal state
    /// may be corrupted.
    pub fn invalidate(&mut self) {
        self.previous.fill(RenderCell::default());
        // Set a sentinel to force all cells dirty -- make previous differ
        // from any possible current frame.
        for cell in &mut self.previous {
            cell.ch = '\x00';
        }
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }
}
```

### Step 3: Build the RenderBackend (crossterm output)

The RenderBackend translates `DirtyCell` values into crossterm commands. It batches all commands into a single write using crossterm's command queue for performance.

```rust
// crates/shux-ui/src/render.rs

use std::io::{self, Write};

use crossterm::{
    cursor::MoveTo,
    style::{
        Attribute, Color as CtColor, Print, ResetColor,
        SetAttribute, SetBackgroundColor, SetForegroundColor,
    },
    terminal::{self, BeginSynchronizedUpdate, EndSynchronizedUpdate},
    QueueableCommand,
};

use crate::buffer::{DirtyCell, RenderAttrs, RenderCell};

/// Abstraction over crossterm terminal output. Queues commands and
/// flushes them in a single synchronized batch.
pub struct RenderBackend<W: Write> {
    out: W,
    /// Track the last style we emitted to avoid redundant style changes.
    last_fg: Option<Option<CtColor>>,
    last_bg: Option<Option<CtColor>>,
    last_attrs: Option<RenderAttrs>,
}

impl<W: Write> RenderBackend<W> {
    pub fn new(out: W) -> Self {
        Self {
            out,
            last_fg: None,
            last_bg: None,
            last_attrs: None,
        }
    }

    /// Render a list of dirty cells to the terminal. Uses synchronized
    /// output (Mode 2026) to prevent tearing.
    ///
    /// The cells should be sorted by (row, col) for optimal cursor
    /// movement, but this method works correctly regardless of order.
    pub fn render_diff(&mut self, dirty: &[DirtyCell]) -> io::Result<()> {
        if dirty.is_empty() {
            return Ok(());
        }

        // Begin synchronized update to prevent flicker
        self.out.queue(BeginSynchronizedUpdate)?;

        for cell in dirty {
            // Move cursor to the cell position
            self.out.queue(MoveTo(cell.col, cell.row))?;

            // Apply style changes (only emit commands when style differs
            // from the last emitted style to reduce output volume)
            self.apply_style(&cell.cell)?;

            // Print the character
            self.out.queue(Print(cell.cell.ch))?;
        }

        // Reset colors at the end so the terminal is in a clean state
        self.out.queue(ResetColor)?;

        // End synchronized update
        self.out.queue(EndSynchronizedUpdate)?;

        // Flush everything in one write
        self.out.flush()?;

        // Reset tracked style state (we just reset colors)
        self.last_fg = None;
        self.last_bg = None;
        self.last_attrs = None;

        Ok(())
    }

    /// Render a full frame (not diff-based). Used for initial render
    /// or after terminal resize.
    pub fn render_full(
        &mut self,
        width: u16,
        height: u16,
        cells: &[RenderCell],
    ) -> io::Result<()> {
        self.out.queue(BeginSynchronizedUpdate)?;

        for row in 0..height {
            self.out.queue(MoveTo(0, row))?;
            for col in 0..width {
                let idx = (row as usize) * (width as usize) + (col as usize);
                let cell = &cells[idx];

                if cell.wide_continuation {
                    continue;
                }

                self.apply_style(cell)?;
                self.out.queue(Print(cell.ch))?;
            }
        }

        self.out.queue(ResetColor)?;
        self.out.queue(EndSynchronizedUpdate)?;
        self.out.flush()?;

        self.last_fg = None;
        self.last_bg = None;
        self.last_attrs = None;

        Ok(())
    }

    /// Apply foreground, background, and attribute style to the output
    /// stream. Only emits crossterm commands when the style actually
    /// changes from the last emitted style.
    fn apply_style(&mut self, cell: &RenderCell) -> io::Result<()> {
        // Foreground
        if self.last_fg != Some(cell.fg) {
            match cell.fg {
                Some(color) => {
                    self.out.queue(SetForegroundColor(color))?;
                }
                None => {
                    self.out.queue(SetForegroundColor(CtColor::Reset))?;
                }
            }
            self.last_fg = Some(cell.fg);
        }

        // Background
        if self.last_bg != Some(cell.bg) {
            match cell.bg {
                Some(color) => {
                    self.out.queue(SetBackgroundColor(color))?;
                }
                None => {
                    self.out.queue(SetBackgroundColor(CtColor::Reset))?;
                }
            }
            self.last_bg = Some(cell.bg);
        }

        // Attributes
        if self.last_attrs != Some(cell.attrs) {
            // Reset all attributes first, then set the ones we need.
            // This is simpler than tracking individual attribute deltas
            // and crossterm's Reset is cheap.
            self.out.queue(SetAttribute(Attribute::Reset))?;

            if cell.attrs.bold {
                self.out.queue(SetAttribute(Attribute::Bold))?;
            }
            if cell.attrs.dim {
                self.out.queue(SetAttribute(Attribute::Dim))?;
            }
            if cell.attrs.italic {
                self.out.queue(SetAttribute(Attribute::Italic))?;
            }
            if cell.attrs.underline {
                self.out.queue(SetAttribute(Attribute::Underlined))?;
            }
            if cell.attrs.blink {
                self.out.queue(SetAttribute(Attribute::SlowBlink))?;
            }
            if cell.attrs.reverse {
                self.out.queue(SetAttribute(Attribute::Reverse))?;
            }
            if cell.attrs.hidden {
                self.out.queue(SetAttribute(Attribute::Hidden))?;
            }
            if cell.attrs.strikethrough {
                self.out.queue(SetAttribute(Attribute::CrossedOut))?;
            }

            self.last_attrs = Some(cell.attrs);
            // After Attribute::Reset, fg/bg state is also reset
            self.last_fg = None;
            self.last_bg = None;
            // Re-apply fg/bg after attribute reset
            match cell.fg {
                Some(color) => {
                    self.out.queue(SetForegroundColor(color))?;
                }
                None => {
                    self.out.queue(SetForegroundColor(CtColor::Reset))?;
                }
            }
            self.last_fg = Some(cell.fg);

            match cell.bg {
                Some(color) => {
                    self.out.queue(SetBackgroundColor(color))?;
                }
                None => {
                    self.out.queue(SetBackgroundColor(CtColor::Reset))?;
                }
            }
            self.last_bg = Some(cell.bg);
        }

        Ok(())
    }

    /// Clear the entire screen.
    pub fn clear_screen(&mut self) -> io::Result<()> {
        self.out
            .queue(crossterm::terminal::Clear(
                crossterm::terminal::ClearType::All,
            ))?;
        self.out.queue(MoveTo(0, 0))?;
        self.out.flush()
    }

    /// Hide the cursor during rendering for cleaner output.
    pub fn hide_cursor(&mut self) -> io::Result<()> {
        self.out.queue(crossterm::cursor::Hide)?;
        self.out.flush()
    }

    /// Show the cursor (call after rendering to restore cursor visibility).
    pub fn show_cursor(&mut self) -> io::Result<()> {
        self.out.queue(crossterm::cursor::Show)?;
        self.out.flush()
    }

    /// Move the cursor to a specific position (for placing the active
    /// pane's cursor after rendering).
    pub fn set_cursor(&mut self, col: u16, row: u16) -> io::Result<()> {
        self.out.queue(MoveTo(col, row))?;
        self.out.flush()
    }
}
```

### Step 4: Build the RenderCompositor

The compositor ties everything together. It takes a VirtualTerminal reference, maps its grid cells into the FrameBuffer, diffs, and renders.

```rust
// crates/shux-ui/src/compositor.rs

use std::io::{self, Write};
use std::time::Instant;

use crate::buffer::{FrameBuffer, RenderAttrs, RenderCell};
use crate::render::RenderBackend;

/// Statistics from the last render pass, used for performance monitoring
/// against the PRD section 14.1 budget (p50 <= 8ms).
#[derive(Debug, Clone)]
pub struct RenderStats {
    /// How many cells were dirty (differed from previous frame).
    pub dirty_cells: usize,
    /// Total number of cells in the frame.
    pub total_cells: usize,
    /// Time spent composing the VirtualTerminal grid into the FrameBuffer.
    pub compose_time_us: u64,
    /// Time spent diffing current vs previous frame.
    pub diff_time_us: u64,
    /// Time spent rendering dirty cells to the terminal.
    pub render_time_us: u64,
    /// Total frame time (compose + diff + render).
    pub total_time_us: u64,
}

/// Configuration for the RenderCompositor.
#[derive(Debug, Clone)]
pub struct CompositorConfig {
    /// Whether to show a simple border around the single pane.
    /// In single-pane mode this is typically false (the pane fills the
    /// entire terminal). Set to true for testing or when a status bar
    /// reserves space.
    pub show_border: bool,

    /// Number of rows reserved at the bottom for a status bar.
    /// In M0 this is 0 (no status bar). Task 026 will set this to 1.
    pub status_bar_height: u16,
}

impl Default for CompositorConfig {
    fn default() -> Self {
        Self {
            show_border: false,
            status_bar_height: 0,
        }
    }
}

/// The RenderCompositor is responsible for:
/// 1. Reading cells from a VirtualTerminal grid
/// 2. Writing them into a FrameBuffer
/// 3. Diffing against the previous frame
/// 4. Rendering only changed cells to the terminal via RenderBackend
///
/// In this task (009) we support single-pane rendering only. Task 017
/// extends this to multi-pane with borders and layout-aware composition.
pub struct RenderCompositor<W: Write> {
    buffer: FrameBuffer,
    backend: RenderBackend<W>,
    config: CompositorConfig,
    last_stats: Option<RenderStats>,
    /// Set to true after resize or other events that require a full redraw.
    force_full_redraw: bool,
}

impl<W: Write> RenderCompositor<W> {
    pub fn new(width: u16, height: u16, out: W, config: CompositorConfig) -> Self {
        Self {
            buffer: FrameBuffer::new(width, height),
            backend: RenderBackend::new(out),
            config,
            last_stats: None,
            force_full_redraw: true, // First render is always full
        }
    }

    /// Handle a terminal resize event. Resizes the internal buffer and
    /// forces a full redraw on the next render pass.
    pub fn resize(&mut self, width: u16, height: u16) {
        self.buffer.resize(width, height);
        self.force_full_redraw = true;
    }

    /// Get the usable area for the pane content (excluding borders and
    /// status bar).
    pub fn content_area(&self) -> (u16, u16, u16, u16) {
        let border_offset = if self.config.show_border { 1 } else { 0 };
        let x = border_offset;
        let y = border_offset;
        let w = self.buffer.width().saturating_sub(border_offset * 2);
        let h = self
            .buffer
            .height()
            .saturating_sub(border_offset * 2)
            .saturating_sub(self.config.status_bar_height);
        (x, y, w, h)
    }

    /// Compose a single pane's grid into the frame buffer and render.
    ///
    /// `grid_cells` is a callback that returns the RenderCell at a given
    /// (col, row) position in the virtual terminal grid. This decouples
    /// the compositor from the VirtualTerminal type (which lives in
    /// shux-vt).
    ///
    /// `cursor_pos` is the cursor position within the pane (col, row),
    /// or None if the cursor should be hidden.
    pub fn render_frame<F>(
        &mut self,
        grid_cell_fn: F,
        grid_width: u16,
        grid_height: u16,
        cursor_pos: Option<(u16, u16)>,
    ) -> io::Result<RenderStats>
    where
        F: Fn(u16, u16) -> RenderCell,
    {
        let frame_start = Instant::now();

        // --- Phase 1: Compose ---
        let compose_start = Instant::now();

        self.buffer.clear_current();

        // Draw border if configured
        if self.config.show_border {
            self.compose_border();
        }

        // Map VT grid cells into the frame buffer
        let (content_x, content_y, content_w, content_h) = self.content_area();

        for row in 0..content_h.min(grid_height) {
            for col in 0..content_w.min(grid_width) {
                let cell = grid_cell_fn(col, row);
                self.buffer
                    .set_cell(content_x + col, content_y + row, cell);
            }
        }

        let compose_time = compose_start.elapsed();

        // --- Phase 2: Diff ---
        let diff_start = Instant::now();

        let dirty = if self.force_full_redraw {
            self.buffer.invalidate();
            self.force_full_redraw = false;
            self.buffer.diff()
        } else {
            self.buffer.diff()
        };

        let dirty_count = dirty.len();
        let diff_time = diff_start.elapsed();

        // --- Phase 3: Render ---
        let render_start = Instant::now();

        self.backend.hide_cursor()?;
        self.backend.render_diff(&dirty)?;

        // Position cursor
        if let Some((cx, cy)) = cursor_pos {
            let screen_x = content_x + cx;
            let screen_y = content_y + cy;
            if screen_x < self.buffer.width() && screen_y < self.buffer.height() {
                self.backend.set_cursor(screen_x, screen_y)?;
                self.backend.show_cursor()?;
            }
        }

        let render_time = render_start.elapsed();

        // --- Swap buffers ---
        self.buffer.swap();

        let total_time = frame_start.elapsed();

        let stats = RenderStats {
            dirty_cells: dirty_count,
            total_cells: (self.buffer.width() as usize) * (self.buffer.height() as usize),
            compose_time_us: compose_time.as_micros() as u64,
            diff_time_us: diff_time.as_micros() as u64,
            render_time_us: render_time.as_micros() as u64,
            total_time_us: total_time.as_micros() as u64,
        };

        self.last_stats = Some(stats.clone());
        Ok(stats)
    }

    /// Draw a simple Unicode box border around the content area.
    /// Uses single-line box-drawing characters: top-left, top-right,
    /// bottom-left, bottom-right, horizontal, vertical.
    fn compose_border(&mut self) {
        let w = self.buffer.width();
        let h = self.buffer.height().saturating_sub(self.config.status_bar_height);

        if w < 2 || h < 2 {
            return;
        }

        let border_cell = |ch: char| RenderCell {
            ch,
            fg: None,   // Use default terminal color; task 024 adds theme tokens
            bg: None,
            attrs: RenderAttrs::default(),
            wide_continuation: false,
        };

        // Top-left corner
        self.buffer.set_cell(0, 0, border_cell('\u{250C}')); // box light down and right
        // Top-right corner
        self.buffer.set_cell(w - 1, 0, border_cell('\u{2510}')); // box light down and left
        // Bottom-left corner
        self.buffer.set_cell(0, h - 1, border_cell('\u{2514}')); // box light up and right
        // Bottom-right corner
        self.buffer.set_cell(w - 1, h - 1, border_cell('\u{2518}')); // box light up and left

        // Top and bottom borders
        for col in 1..(w - 1) {
            self.buffer.set_cell(col, 0, border_cell('\u{2500}')); // horizontal
            self.buffer.set_cell(col, h - 1, border_cell('\u{2500}'));
        }

        // Left and right borders
        for row in 1..(h - 1) {
            self.buffer.set_cell(0, row, border_cell('\u{2502}')); // vertical
            self.buffer.set_cell(w - 1, row, border_cell('\u{2502}'));
        }
    }

    /// Force a full redraw on the next render pass. Call this after
    /// events that may have corrupted the terminal state (e.g., a
    /// child process writing directly to the terminal).
    pub fn force_redraw(&mut self) {
        self.force_full_redraw = true;
    }

    /// Get statistics from the last render pass.
    pub fn last_stats(&self) -> Option<&RenderStats> {
        self.last_stats.as_ref()
    }

    /// Clear the screen and reset internal state. Call during
    /// initialization or when restoring the terminal.
    pub fn clear(&mut self) -> io::Result<()> {
        self.backend.clear_screen()?;
        self.force_full_redraw = true;
        Ok(())
    }
}
```

### Step 5: Update crate Cargo.toml and lib.rs

```toml
# crates/shux-ui/Cargo.toml
[package]
name = "shux-ui"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
shux-vt = { path = "../shux-vt" }
crossterm.workspace = true
tracing.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

```rust
// crates/shux-ui/src/lib.rs
//! shux-ui — TUI client: render compositor, terminal management, client connection

pub mod buffer;
pub mod compositor;
pub mod render;
```

### Step 6: Implement conversion from VT grid cells to RenderCells

The VirtualTerminal (task 005) stores cells in its own format. We need a conversion trait or function that maps VT cells into `RenderCell`. Since the VT crate is a separate workspace crate, define a `From` impl or a standalone function.

```rust
// crates/shux-ui/src/buffer.rs (additional impl)

// This conversion will be filled in when the VT grid cell type is finalized
// in task 005. For now, provide a helper that creates RenderCells from
// basic parameters so the compositor can be tested independently.

impl RenderCell {
    /// Create a simple text cell with default styling.
    pub fn text(ch: char) -> Self {
        Self {
            ch,
            fg: None,
            bg: None,
            attrs: RenderAttrs::default(),
            wide_continuation: false,
        }
    }

    /// Create a styled text cell.
    pub fn styled(
        ch: char,
        fg: Option<Color>,
        bg: Option<Color>,
        attrs: RenderAttrs,
    ) -> Self {
        Self {
            ch,
            fg,
            bg,
            attrs,
            wide_continuation: false,
        }
    }
}
```

### Step 7: Handle terminal resize

Terminal resize is signaled via SIGWINCH. The compositor itself does not listen for signals (that is the client's job in task 010), but it exposes the `resize()` method. The flow is:

1. Client (task 010) receives SIGWINCH or crossterm resize event
2. Client calls `compositor.resize(new_width, new_height)`
3. FrameBuffer is reallocated, previous buffer cleared
4. `force_full_redraw` is set to true
5. Next `render_frame()` call redraws everything

This is already implemented in the `resize()` method above.

### Step 8: Write unit tests

```rust
// crates/shux-ui/src/buffer.rs — tests module

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_buffer_dimensions() {
        let buf = FrameBuffer::new(80, 24);
        assert_eq!(buf.width(), 80);
        assert_eq!(buf.height(), 24);
    }

    #[test]
    fn test_empty_diff_on_new_buffer() {
        let buf = FrameBuffer::new(80, 24);
        // Both buffers are identical (all default cells), so diff should
        // produce no dirty cells.
        let dirty = buf.diff();
        assert!(dirty.is_empty());
    }

    #[test]
    fn test_diff_detects_changed_cell() {
        let mut buf = FrameBuffer::new(10, 5);

        // Write a character to the current buffer
        buf.set_cell(3, 2, RenderCell::text('A'));

        let dirty = buf.diff();
        assert_eq!(dirty.len(), 1);
        assert_eq!(dirty[0].col, 3);
        assert_eq!(dirty[0].row, 2);
        assert_eq!(dirty[0].cell.ch, 'A');
    }

    #[test]
    fn test_swap_makes_diff_empty() {
        let mut buf = FrameBuffer::new(10, 5);
        buf.set_cell(3, 2, RenderCell::text('A'));

        let dirty = buf.diff();
        assert_eq!(dirty.len(), 1);

        // Swap: current becomes previous
        buf.swap();

        // Without changing current, diff should now be empty
        // ... but current was not cleared, so current[3,2] is still 'A'
        // and previous[3,2] is now also 'A'. They match.
        let dirty = buf.diff();
        assert!(dirty.is_empty());
    }

    #[test]
    fn test_resize_forces_full_redraw() {
        let mut buf = FrameBuffer::new(10, 5);
        buf.set_cell(3, 2, RenderCell::text('A'));
        buf.swap();

        // Resize clears both buffers
        buf.resize(20, 10);

        // After resize, both buffers are identical (default), so diff
        // should be empty -- but the compositor sets force_full_redraw
        // which calls invalidate() before diffing.
        // Here we test invalidate directly:
        buf.invalidate();
        let dirty = buf.diff();

        // All cells should be dirty because invalidate sets previous to
        // sentinel values.
        assert_eq!(dirty.len(), 20 * 10);
    }

    #[test]
    fn test_out_of_bounds_set_cell_is_noop() {
        let mut buf = FrameBuffer::new(10, 5);
        buf.set_cell(100, 100, RenderCell::text('X'));
        // Should not panic or corrupt state
        let dirty = buf.diff();
        assert!(dirty.is_empty());
    }

    #[test]
    fn test_wide_char_continuation_skipped_in_diff() {
        let mut buf = FrameBuffer::new(10, 5);

        // Simulate a wide character at col 0, with continuation at col 1
        buf.set_cell(0, 0, RenderCell::text('\u{4E16}')); // CJK character
        buf.set_cell(
            1,
            0,
            RenderCell {
                ch: ' ',
                wide_continuation: true,
                ..RenderCell::default()
            },
        );

        let dirty = buf.diff();
        // Only the primary cell should appear in the diff, not the continuation
        assert_eq!(dirty.len(), 1);
        assert_eq!(dirty[0].col, 0);
        assert_eq!(dirty[0].row, 0);
    }

    #[test]
    fn test_style_change_detected() {
        use crossterm::style::Color;

        let mut buf = FrameBuffer::new(10, 5);
        buf.set_cell(
            0,
            0,
            RenderCell::styled(
                'A',
                Some(Color::Red),
                None,
                RenderAttrs {
                    bold: true,
                    ..RenderAttrs::default()
                },
            ),
        );

        let dirty = buf.diff();
        assert_eq!(dirty.len(), 1);
        assert_eq!(dirty[0].cell.fg, Some(Color::Red));
        assert!(dirty[0].cell.attrs.bold);
    }
}
```

```rust
// crates/shux-ui/src/render.rs — tests module

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::{RenderAttrs, RenderCell};

    #[test]
    fn test_render_diff_to_buffer() {
        // Use a Vec<u8> as the output sink to capture the crossterm commands
        let mut output = Vec::new();
        let mut backend = RenderBackend::new(&mut output);

        let dirty = vec![DirtyCell {
            col: 5,
            row: 3,
            cell: RenderCell::text('H'),
        }];

        backend.render_diff(&dirty).unwrap();

        // The output should contain crossterm escape sequences.
        // We verify it is non-empty (detailed sequence validation is
        // fragile across crossterm versions, so we test behavior
        // rather than exact bytes).
        assert!(!output.is_empty());

        // Verify the output contains the character 'H'
        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains('H'));
    }

    #[test]
    fn test_empty_diff_produces_no_output() {
        let mut output = Vec::new();
        let mut backend = RenderBackend::new(&mut output);

        backend.render_diff(&[]).unwrap();

        // No dirty cells means no output at all
        assert!(output.is_empty());
    }

    #[test]
    fn test_clear_screen() {
        let mut output = Vec::new();
        let mut backend = RenderBackend::new(&mut output);

        backend.clear_screen().unwrap();

        assert!(!output.is_empty());
    }
}
```

```rust
// crates/shux-ui/src/compositor.rs — tests module

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::RenderCell;

    #[test]
    fn test_compositor_single_pane_render() {
        let mut output = Vec::new();
        let config = CompositorConfig::default();
        let mut compositor = RenderCompositor::new(80, 24, &mut output, config);

        // Render a simple grid with "Hello" in the first row
        let stats = compositor
            .render_frame(
                |col, _row| {
                    let chars = ['H', 'e', 'l', 'l', 'o'];
                    if (col as usize) < chars.len() {
                        RenderCell::text(chars[col as usize])
                    } else {
                        RenderCell::default()
                    }
                },
                80,
                24,
                Some((5, 0)),
            )
            .unwrap();

        // First render should touch at least some cells
        assert!(stats.dirty_cells > 0);
        assert!(stats.total_time_us > 0);

        // Output should contain the characters
        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains('H'));
        assert!(output_str.contains('e'));
        assert!(output_str.contains('o'));
    }

    #[test]
    fn test_compositor_incremental_render() {
        let mut output = Vec::new();
        let config = CompositorConfig::default();
        let mut compositor = RenderCompositor::new(10, 5, &mut output, config);

        // First render: "Hello"
        compositor
            .render_frame(
                |col, _row| {
                    let chars = ['H', 'e', 'l', 'l', 'o'];
                    if (col as usize) < chars.len() {
                        RenderCell::text(chars[col as usize])
                    } else {
                        RenderCell::default()
                    }
                },
                10,
                5,
                None,
            )
            .unwrap();

        // Clear output to measure incremental render
        output.clear();

        // Second render: "Hxllo" (only col 1 changed)
        let stats = compositor
            .render_frame(
                |col, _row| {
                    let chars = ['H', 'x', 'l', 'l', 'o'];
                    if (col as usize) < chars.len() {
                        RenderCell::text(chars[col as usize])
                    } else {
                        RenderCell::default()
                    }
                },
                10,
                5,
                None,
            )
            .unwrap();

        // Only 1 cell should be dirty (the 'x' replacing 'e')
        assert_eq!(stats.dirty_cells, 1);
    }

    #[test]
    fn test_compositor_resize() {
        let mut output = Vec::new();
        let config = CompositorConfig::default();
        let mut compositor = RenderCompositor::new(10, 5, &mut output, config);

        // Initial render
        compositor
            .render_frame(|_, _| RenderCell::text('A'), 10, 5, None)
            .unwrap();

        // Resize
        compositor.resize(20, 10);

        // Clear output
        output.clear();

        // Render after resize should be a full redraw
        let stats = compositor
            .render_frame(|_, _| RenderCell::text('B'), 20, 10, None)
            .unwrap();

        // All cells should be dirty (full redraw after resize)
        assert_eq!(stats.dirty_cells, 20 * 10);
    }

    #[test]
    fn test_compositor_with_border() {
        let mut output = Vec::new();
        let config = CompositorConfig {
            show_border: true,
            status_bar_height: 0,
        };
        let mut compositor = RenderCompositor::new(10, 5, &mut output, config);

        let (x, y, w, h) = compositor.content_area();
        // With border: content starts at (1,1), width and height reduced by 2
        assert_eq!(x, 1);
        assert_eq!(y, 1);
        assert_eq!(w, 8);
        assert_eq!(h, 3);
    }

    #[test]
    fn test_compositor_with_status_bar() {
        let mut output = Vec::new();
        let config = CompositorConfig {
            show_border: false,
            status_bar_height: 1,
        };
        let mut compositor = RenderCompositor::new(80, 24, &mut output, config);

        let (x, y, w, h) = compositor.content_area();
        assert_eq!(x, 0);
        assert_eq!(y, 0);
        assert_eq!(w, 80);
        assert_eq!(h, 23); // 24 - 1 for status bar
    }

    #[test]
    fn test_render_stats_reported() {
        let mut output = Vec::new();
        let config = CompositorConfig::default();
        let mut compositor = RenderCompositor::new(10, 5, &mut output, config);

        assert!(compositor.last_stats().is_none());

        compositor
            .render_frame(|_, _| RenderCell::default(), 10, 5, None)
            .unwrap();

        let stats = compositor.last_stats().unwrap();
        assert_eq!(stats.total_cells, 50);
    }
}
```

---

## Verification

### Functional

```bash
# Build the shux-ui crate
cargo build -p shux-ui

# Verify no clippy warnings
cargo clippy -p shux-ui -- -D warnings

# Verify formatting
cargo fmt -p shux-ui -- --check
```

### Tests

```bash
# Run all shux-ui tests
cargo nextest run -p shux-ui

# Run with output visible for debugging
cargo nextest run -p shux-ui --no-capture
cargo nextest run -p shux-ui -- render::tests::synchronized_update_fallback

# Expected: all tests pass:
#   buffer::tests::test_new_buffer_dimensions
#   buffer::tests::test_empty_diff_on_new_buffer
#   buffer::tests::test_diff_detects_changed_cell
#   buffer::tests::test_swap_makes_diff_empty
#   buffer::tests::test_resize_forces_full_redraw
#   buffer::tests::test_out_of_bounds_set_cell_is_noop
#   buffer::tests::test_wide_char_continuation_skipped_in_diff
#   buffer::tests::test_style_change_detected
#   render::tests::test_render_diff_to_buffer
#   render::tests::test_empty_diff_produces_no_output
#   render::tests::test_clear_screen
#   compositor::tests::test_compositor_single_pane_render
#   compositor::tests::test_compositor_incremental_render
#   compositor::tests::test_compositor_resize
#   compositor::tests::test_compositor_with_border
#   compositor::tests::test_compositor_with_status_bar
#   compositor::tests::test_render_stats_reported
```

---

## Completion Criteria

- [ ] `crates/shux-ui/src/buffer.rs` implements `RenderCell`, `RenderAttrs`, `FrameBuffer`, `DirtyCell`
- [ ] `FrameBuffer` supports double-buffering with `diff()` and `swap()`
- [ ] `FrameBuffer` supports `resize()` that clears both buffers and forces full redraw
- [ ] `FrameBuffer` supports `invalidate()` to force all cells dirty
- [ ] `crates/shux-ui/src/render.rs` implements `RenderBackend` with crossterm output
- [ ] `RenderBackend` uses `BeginSynchronizedUpdate`/`EndSynchronizedUpdate` (Mode 2026)
- [ ] `RenderBackend` tracks last emitted style to minimize redundant escape sequences
- [ ] `RenderBackend` supports `render_diff()`, `render_full()`, `clear_screen()`, `hide_cursor()`, `show_cursor()`, `set_cursor()`
- [ ] `crates/shux-ui/src/compositor.rs` implements `RenderCompositor` orchestrating buffer + backend
- [ ] `RenderCompositor` accepts a cell callback (decoupled from VirtualTerminal type)
- [ ] `RenderCompositor` computes `RenderStats` with compose/diff/render timings
- [ ] `RenderCompositor` supports border rendering via `CompositorConfig`
- [ ] `RenderCompositor` handles resize and cursor positioning
- [ ] Capability fallback test verifies rendering remains correct when Mode 2026 and/or truecolor are unavailable
- [ ] All unit tests pass (buffer diff, incremental rendering, resize, border, stats)
- [ ] `cargo clippy -p shux-ui -- -D warnings` passes
- [ ] Render stats show composition below 8ms for an 80x24 terminal (p50 target from PRD section 14.1)

---

## Commit Message
```
feat(ui): add render compositor with diff-based incremental rendering

- FrameBuffer with double-buffering and cell-level diffing
- RenderBackend wrapping crossterm with synchronized output (Mode 2026)
- RenderCompositor orchestrating compose → diff → render pipeline
- Border rendering support for single pane
- Render statistics tracking (compose/diff/render time in microseconds)
- Unit tests for buffer diffing, incremental rendering, resize, borders
```

---

## Session Protocol

1. **Before starting:** Read task 005 (VirtualTerminal grid) to understand the cell type you will need to convert from. Read `crates/shux-vt/src/lib.rs` (or `grid.rs`) for the actual cell representation. Read task 006 (input decoder) to understand the input side of the pipeline.
2. **During:** Implement in order: `buffer.rs` (Step 1-2) -> `render.rs` (Step 3) -> `compositor.rs` (Step 4) -> Cargo.toml updates (Step 5) -> conversion helpers (Step 6) -> tests (Step 8). Run `cargo check -p shux-ui` after each file to catch compilation issues early. Run tests after each test module.
3. **Performance validation:** After all tests pass, add a quick timing test that renders a full 80x24 grid and asserts the total render time is under 8ms. If it exceeds that, profile and optimize (likely the diff loop or the crossterm command queue).
4. **After:** Run `make check`. Update `docs/PROGRESS.md` (mark 009 in-progress or done). Update `CLAUDE.md` Learnings if crossterm 0.29 has any API surprises (e.g., `QueueableCommand` trait import requirements, synchronized update behavior).
