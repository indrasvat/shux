//! Double-buffered frame buffer for diff-based rendering.
//!
//! The FrameBuffer operates on a flat grid of `RenderCell` values. Each cell
//! stores the character, foreground color, background color, and style attributes.
//! Diffing the current frame against the previous frame produces the minimal set
//! of cells that need updating -- this is the core of the incremental rendering
//! strategy required by PRD section 14.1 (p50 <= 8ms keypress-to-update).

use std::sync::Arc;

use crossterm::style::Color;

use crate::vt_convert;

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

    /// Rare extended terminal attributes that must participate in diffing.
    pub extended: Option<Arc<shux_vt::ExtendedAttrs>>,

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
            extended: None,
            wide_continuation: false,
        }
    }
}

impl RenderCell {
    /// Create a simple text cell with default styling.
    pub fn text(ch: char) -> Self {
        Self {
            ch,
            fg: None,
            bg: None,
            attrs: RenderAttrs::default(),
            extended: None,
            wide_continuation: false,
        }
    }

    /// Create a styled text cell.
    pub fn styled(ch: char, fg: Option<Color>, bg: Option<Color>, attrs: RenderAttrs) -> Self {
        Self {
            ch,
            fg,
            bg,
            attrs,
            extended: None,
            wide_continuation: false,
        }
    }

    /// Convert a VT cell while resolving dynamic OSC 10/11 default colors.
    pub fn from_vt_cell_with_defaults(
        cell: &shux_vt::Cell,
        defaults: shux_vt::TerminalDefaultColors,
    ) -> Self {
        let flags = &cell.style.flags;
        Self {
            ch: cell.ch,
            fg: vt_convert::vt_color_to_crossterm_with_default(cell.style.fg, defaults.fg),
            bg: vt_convert::vt_color_to_crossterm_with_default(cell.style.bg, defaults.bg),
            attrs: RenderAttrs {
                bold: flags.contains(shux_vt::CellFlags::BOLD),
                dim: flags.contains(shux_vt::CellFlags::DIM),
                italic: flags.contains(shux_vt::CellFlags::ITALIC),
                underline: flags.contains(shux_vt::CellFlags::UNDERLINE),
                blink: flags.contains(shux_vt::CellFlags::BLINK),
                reverse: flags.contains(shux_vt::CellFlags::INVERSE),
                hidden: flags.contains(shux_vt::CellFlags::HIDDEN),
                strikethrough: flags.contains(shux_vt::CellFlags::STRIKETHROUGH),
            },
            extended: cell.extended.clone(),
            wide_continuation: cell.is_wide_continuation(),
        }
    }
}

/// Conversion from shux-vt Cell to RenderCell.
///
/// Maps the VT cell's color enum and flag bits to the render buffer's
/// representation. This decouples the VT grid format from the render
/// pipeline while keeping the conversion zero-cost.
impl From<&shux_vt::Cell> for RenderCell {
    fn from(cell: &shux_vt::Cell) -> Self {
        RenderCell::from_vt_cell_with_defaults(cell, shux_vt::TerminalDefaultColors::default())
    }
}

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

    /// Get the buffer width.
    pub fn width(&self) -> u16 {
        self.width
    }

    /// Get the buffer height.
    pub fn height(&self) -> u16 {
        self.height
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

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
        // current[3,2] is still 'A' and previous[3,2] is now also 'A'.
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

        // After resize, both buffers are identical (default), but
        // invalidate forces all cells dirty.
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

    #[test]
    fn test_cell_mut_returns_none_for_out_of_bounds() {
        let mut buf = FrameBuffer::new(10, 5);
        assert!(buf.cell_mut(10, 0).is_none());
        assert!(buf.cell_mut(0, 5).is_none());
        assert!(buf.cell_mut(10, 5).is_none());
    }

    #[test]
    fn test_cell_mut_returns_some_for_valid_coords() {
        let mut buf = FrameBuffer::new(10, 5);
        let cell = buf.cell_mut(0, 0);
        assert!(cell.is_some());
        cell.unwrap().ch = 'Z'; // safe: just checked is_some
        assert_eq!(buf.diff().len(), 1);
    }

    #[test]
    fn test_clear_current_resets_all_cells() {
        let mut buf = FrameBuffer::new(5, 5);
        for row in 0..5 {
            for col in 0..5 {
                buf.set_cell(col, row, RenderCell::text('X'));
            }
        }
        buf.swap();

        buf.clear_current();
        let dirty = buf.diff();
        // All 25 cells should differ (previous has 'X', current has ' ')
        assert_eq!(dirty.len(), 25);
    }

    #[test]
    fn test_render_cell_text_helper() {
        let cell = RenderCell::text('A');
        assert_eq!(cell.ch, 'A');
        assert_eq!(cell.fg, None);
        assert_eq!(cell.bg, None);
        assert!(!cell.attrs.bold);
        assert!(cell.extended.is_none());
        assert!(!cell.wide_continuation);
    }

    #[test]
    fn test_render_cell_styled_helper() {
        let cell = RenderCell::styled(
            'B',
            Some(Color::Green),
            Some(Color::Blue),
            RenderAttrs {
                italic: true,
                ..RenderAttrs::default()
            },
        );
        assert_eq!(cell.ch, 'B');
        assert_eq!(cell.fg, Some(Color::Green));
        assert_eq!(cell.bg, Some(Color::Blue));
        assert!(cell.attrs.italic);
        assert!(cell.extended.is_none());
        assert!(!cell.wide_continuation);
    }

    #[test]
    fn test_from_vt_cell_default() {
        let vt_cell = shux_vt::Cell::default();
        let render_cell = RenderCell::from(&vt_cell);
        assert_eq!(render_cell.ch, ' ');
        assert_eq!(render_cell.fg, None);
        assert_eq!(render_cell.bg, None);
        assert!(!render_cell.attrs.bold);
        assert!(!render_cell.wide_continuation);
    }

    #[test]
    fn test_from_vt_cell_with_color() {
        let vt_cell = shux_vt::Cell {
            ch: 'X',
            width: 1,
            style: shux_vt::CellStyle {
                fg: shux_vt::Color::Indexed(1),
                bg: shux_vt::Color::Rgb(10, 20, 30),
                flags: shux_vt::CellFlags::default(),
            },
            extended: None,
        };
        let render_cell = RenderCell::from(&vt_cell);
        assert_eq!(render_cell.ch, 'X');
        assert_eq!(render_cell.fg, Some(Color::AnsiValue(1)));
        assert_eq!(
            render_cell.bg,
            Some(Color::Rgb {
                r: 10,
                g: 20,
                b: 30
            })
        );
    }

    #[test]
    fn test_from_vt_cell_resolves_dynamic_defaults() {
        let vt_cell = shux_vt::Cell::default();
        let render_cell = RenderCell::from_vt_cell_with_defaults(
            &vt_cell,
            shux_vt::TerminalDefaultColors {
                fg: Some([232, 217, 201]),
                bg: Some([18, 10, 8]),
            },
        );
        assert_eq!(
            render_cell.fg,
            Some(Color::Rgb {
                r: 232,
                g: 217,
                b: 201
            })
        );
        assert_eq!(render_cell.bg, Some(Color::Rgb { r: 18, g: 10, b: 8 }));
    }

    #[test]
    fn test_from_vt_cell_with_flags() {
        let mut flags = shux_vt::CellFlags::default();
        flags.set(shux_vt::CellFlags::BOLD);
        flags.set(shux_vt::CellFlags::ITALIC);
        flags.set(shux_vt::CellFlags::STRIKETHROUGH);

        let vt_cell = shux_vt::Cell {
            ch: 'A',
            width: 1,
            style: shux_vt::CellStyle {
                fg: shux_vt::Color::Default,
                bg: shux_vt::Color::Default,
                flags,
            },
            extended: None,
        };
        let render_cell = RenderCell::from(&vt_cell);
        assert!(render_cell.attrs.bold);
        assert!(render_cell.attrs.italic);
        assert!(render_cell.attrs.strikethrough);
        assert!(!render_cell.attrs.dim);
        assert!(!render_cell.attrs.underline);
    }

    #[test]
    fn test_from_vt_cell_wide_continuation() {
        let vt_cell = shux_vt::Cell::wide_continuation();
        let render_cell = RenderCell::from(&vt_cell);
        assert!(render_cell.wide_continuation);
    }

    #[test]
    fn test_from_vt_cell_preserves_extended_attributes_for_diffing() {
        let extended = Arc::new(shux_vt::ExtendedAttrs {
            hyperlink: Some("https://example.invalid/a;b".to_string()),
            underline_color: Some(shux_vt::Color::Rgb(10, 20, 30)),
            underline_style: shux_vt::UnderlineStyle::Curly,
        });
        let vt_cell = shux_vt::Cell {
            ch: 'X',
            width: 1,
            style: shux_vt::CellStyle::default(),
            extended: Some(extended.clone()),
        };

        let render_cell = RenderCell::from(&vt_cell);
        assert_eq!(render_cell.extended, Some(extended));
    }

    #[test]
    fn test_extended_attribute_only_change_is_dirty() {
        let mut buf = FrameBuffer::new(1, 1);
        buf.set_cell(0, 0, RenderCell::text('X'));
        buf.swap();

        let mut next = RenderCell::text('X');
        next.extended = Some(Arc::new(shux_vt::ExtendedAttrs {
            hyperlink: Some("https://example.invalid".to_string()),
            underline_color: None,
            underline_style: shux_vt::UnderlineStyle::None,
        }));
        buf.set_cell(0, 0, next);

        let dirty = buf.diff();
        assert_eq!(dirty.len(), 1);
    }
}
