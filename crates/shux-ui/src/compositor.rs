//! RenderCompositor: orchestrates compose -> diff -> render pipeline.
//!
//! The compositor ties everything together: it takes a grid cell accessor
//! (decoupled from VirtualTerminal via a closure), maps cells into the
//! FrameBuffer, diffs against the previous frame, and renders only
//! changed cells to the terminal via RenderBackend.
//!
//! In this task (009) we support single-pane rendering only. Task 017
//! extends this to multi-pane with borders and layout-aware composition.

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
#[derive(Debug, Clone, Default)]
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

/// The RenderCompositor is responsible for:
/// 1. Reading cells from a VirtualTerminal grid (via a closure)
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
    ///
    /// Returns `(x, y, width, height)` describing the content rectangle.
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
    /// `grid_cell_fn` is a callback that returns the RenderCell at a given
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
                self.buffer.set_cell(content_x + col, content_y + row, cell);
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
        let h = self
            .buffer
            .height()
            .saturating_sub(self.config.status_bar_height);

        if w < 2 || h < 2 {
            return;
        }

        let border_cell = |ch: char| RenderCell {
            ch,
            fg: None, // Use default terminal color; task 024 adds theme tokens
            bg: None,
            attrs: RenderAttrs::default(),
            wide_continuation: false,
        };

        // Corners
        self.buffer.set_cell(0, 0, border_cell('\u{250C}')); // box light down and right
        self.buffer.set_cell(w - 1, 0, border_cell('\u{2510}')); // box light down and left
        self.buffer.set_cell(0, h - 1, border_cell('\u{2514}')); // box light up and right
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

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::buffer::RenderCell;

    /// Helper: create a compositor backed by a `Cursor<Vec<u8>>` sink.
    /// The Cursor wrapper is owned by the compositor, so there are no
    /// borrow conflicts when calling `render_frame` multiple times.
    fn make_compositor(
        width: u16,
        height: u16,
        config: CompositorConfig,
    ) -> RenderCompositor<Cursor<Vec<u8>>> {
        RenderCompositor::new(width, height, Cursor::new(Vec::new()), config)
    }

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

        // Output should contain the characters
        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains('H'));
        assert!(output_str.contains('e'));
        assert!(output_str.contains('o'));
    }

    #[test]
    fn test_compositor_incremental_render() {
        let mut compositor = make_compositor(10, 5, CompositorConfig::default());

        // First render: "Hello" on row 0 only
        compositor
            .render_frame(
                |col, row| {
                    if row == 0 {
                        let chars = ['H', 'e', 'l', 'l', 'o'];
                        if (col as usize) < chars.len() {
                            RenderCell::text(chars[col as usize])
                        } else {
                            RenderCell::default()
                        }
                    } else {
                        RenderCell::default()
                    }
                },
                10,
                5,
                None,
            )
            .unwrap();

        // Second render: "Hxllo" on row 0 only (only col 1 changed)
        let stats = compositor
            .render_frame(
                |col, row| {
                    if row == 0 {
                        let chars = ['H', 'x', 'l', 'l', 'o'];
                        if (col as usize) < chars.len() {
                            RenderCell::text(chars[col as usize])
                        } else {
                            RenderCell::default()
                        }
                    } else {
                        RenderCell::default()
                    }
                },
                10,
                5,
                None,
            )
            .unwrap();

        // Only 1 cell should be dirty (the 'x' replacing 'e' at row 0, col 1)
        assert_eq!(stats.dirty_cells, 1);
    }

    #[test]
    fn test_compositor_resize() {
        let mut compositor = make_compositor(10, 5, CompositorConfig::default());

        // Initial render
        compositor
            .render_frame(|_, _| RenderCell::text('A'), 10, 5, None)
            .unwrap();

        // Resize
        compositor.resize(20, 10);

        // Render after resize should be a full redraw
        let stats = compositor
            .render_frame(|_, _| RenderCell::text('B'), 20, 10, None)
            .unwrap();

        // All cells should be dirty (full redraw after resize)
        assert_eq!(stats.dirty_cells, 20 * 10);
    }

    #[test]
    fn test_compositor_with_border() {
        let config = CompositorConfig {
            show_border: true,
            status_bar_height: 0,
        };
        let compositor = make_compositor(10, 5, config);

        let (x, y, w, h) = compositor.content_area();
        // With border: content starts at (1,1), width and height reduced by 2
        assert_eq!(x, 1);
        assert_eq!(y, 1);
        assert_eq!(w, 8);
        assert_eq!(h, 3);
    }

    #[test]
    fn test_compositor_with_status_bar() {
        let config = CompositorConfig {
            show_border: false,
            status_bar_height: 1,
        };
        let compositor = make_compositor(80, 24, config);

        let (x, y, w, h) = compositor.content_area();
        assert_eq!(x, 0);
        assert_eq!(y, 0);
        assert_eq!(w, 80);
        assert_eq!(h, 23); // 24 - 1 for status bar
    }

    #[test]
    fn test_compositor_with_border_and_status_bar() {
        let config = CompositorConfig {
            show_border: true,
            status_bar_height: 1,
        };
        let compositor = make_compositor(80, 24, config);

        let (x, y, w, h) = compositor.content_area();
        assert_eq!(x, 1);
        assert_eq!(y, 1);
        assert_eq!(w, 78); // 80 - 2 for borders
        assert_eq!(h, 21); // 24 - 2 borders - 1 status bar
    }

    #[test]
    fn test_render_stats_reported() {
        let mut compositor = make_compositor(10, 5, CompositorConfig::default());

        assert!(compositor.last_stats().is_none());

        compositor
            .render_frame(|_, _| RenderCell::default(), 10, 5, None)
            .unwrap();

        let stats = compositor.last_stats().unwrap();
        assert_eq!(stats.total_cells, 50);
    }

    #[test]
    fn test_force_redraw() {
        let mut compositor = make_compositor(10, 5, CompositorConfig::default());

        // Initial render (full redraw forced automatically)
        compositor
            .render_frame(|_, _| RenderCell::text('A'), 10, 5, None)
            .unwrap();

        // Second render -- identical content, should have 0 dirty cells
        let stats = compositor
            .render_frame(|_, _| RenderCell::text('A'), 10, 5, None)
            .unwrap();
        assert_eq!(stats.dirty_cells, 0);

        // Force redraw
        compositor.force_redraw();

        // Third render -- same content but force redraw means all dirty
        let stats = compositor
            .render_frame(|_, _| RenderCell::text('A'), 10, 5, None)
            .unwrap();
        assert_eq!(stats.dirty_cells, 10 * 5);
    }

    #[test]
    fn test_compositor_clear() {
        let mut output = Vec::new();
        let config = CompositorConfig::default();
        let mut compositor = RenderCompositor::new(10, 5, &mut output, config);

        compositor.clear().unwrap();
        assert!(!output.is_empty());
    }

    #[test]
    fn test_compositor_border_rendering() {
        let mut output = Vec::new();
        let config = CompositorConfig {
            show_border: true,
            status_bar_height: 0,
        };
        let mut compositor = RenderCompositor::new(10, 5, &mut output, config);

        let stats = compositor
            .render_frame(|_, _| RenderCell::text('X'), 8, 3, None)
            .unwrap();

        // All cells should be dirty on first render
        assert_eq!(stats.dirty_cells, 10 * 5);

        // Output should contain box-drawing characters
        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains('\u{250C}')); // top-left corner
        assert!(output_str.contains('\u{2510}')); // top-right corner
        assert!(output_str.contains('\u{2514}')); // bottom-left corner
        assert!(output_str.contains('\u{2518}')); // bottom-right corner
        assert!(output_str.contains('\u{2500}')); // horizontal
        assert!(output_str.contains('\u{2502}')); // vertical
        // Content character should also be present
        assert!(output_str.contains('X'));
    }

    #[test]
    fn test_compositor_cursor_hidden_when_none() {
        let mut output = Vec::new();
        let config = CompositorConfig::default();
        let mut compositor = RenderCompositor::new(10, 5, &mut output, config);

        // Render with no cursor
        compositor
            .render_frame(|_, _| RenderCell::default(), 10, 5, None)
            .unwrap();

        // The output should contain Hide cursor but not Show cursor
        // (since cursor_pos is None)
        let output_str = String::from_utf8_lossy(&output);
        // crossterm Hide = CSI ?25l, Show = CSI ?25h
        assert!(output_str.contains("\x1b[?25l")); // Hide
        assert!(!output_str.contains("\x1b[?25h")); // No Show
    }

    #[test]
    fn test_compositor_cursor_shown_when_some() {
        let mut output = Vec::new();
        let config = CompositorConfig::default();
        let mut compositor = RenderCompositor::new(10, 5, &mut output, config);

        // Render with cursor at (3, 2)
        compositor
            .render_frame(|_, _| RenderCell::default(), 10, 5, Some((3, 2)))
            .unwrap();

        let output_str = String::from_utf8_lossy(&output);
        // Should contain both Hide and Show
        assert!(output_str.contains("\x1b[?25l")); // Hide (during render)
        assert!(output_str.contains("\x1b[?25h")); // Show (after render)
    }

    #[test]
    fn test_compositor_grid_smaller_than_content_area() {
        // When the grid is smaller than the content area, only the
        // grid portion should be filled; the rest stays blank.
        let mut compositor = make_compositor(20, 10, CompositorConfig::default());

        // Grid is only 5x3
        let stats = compositor
            .render_frame(|_, _| RenderCell::text('Z'), 5, 3, None)
            .unwrap();

        // First frame is always full redraw, so all 200 cells are dirty
        assert_eq!(stats.dirty_cells, 200);

        // Second render: grid content unchanged, blank area unchanged
        let stats = compositor
            .render_frame(|_, _| RenderCell::text('Z'), 5, 3, None)
            .unwrap();
        assert_eq!(stats.dirty_cells, 0);
    }

    #[test]
    fn test_performance_80x24_under_budget() {
        // Quick sanity check: a full 80x24 render should complete well
        // under the 8ms PRD budget. Even with overhead, this should be
        // < 1ms on modern hardware for an in-memory Vec<u8> sink.
        let mut compositor = make_compositor(80, 24, CompositorConfig::default());

        let stats = compositor
            .render_frame(
                |col, row| {
                    let ch = (b'A' + ((col + row) % 26) as u8) as char;
                    RenderCell::text(ch)
                },
                80,
                24,
                Some((0, 0)),
            )
            .unwrap();

        // 8ms = 8000 microseconds. Should be well under this.
        assert!(
            stats.total_time_us < 8000,
            "Full 80x24 render took {}us, exceeds 8ms budget",
            stats.total_time_us
        );
    }
}
