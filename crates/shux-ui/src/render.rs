//! RenderBackend: crossterm output abstraction.
//!
//! Translates `DirtyCell` values into crossterm commands. Batches all commands
//! into a single write using crossterm's command queue for performance.
//! Uses synchronized output (Mode 2026) via `BeginSynchronizedUpdate` /
//! `EndSynchronizedUpdate` to prevent tearing (PRD section 6.1).

use std::io::{self, Write};

use crossterm::{
    QueueableCommand,
    cursor::MoveTo,
    style::{
        Attribute, Color as CtColor, Print, ResetColor, SetAttribute, SetBackgroundColor,
        SetForegroundColor,
    },
    terminal::{self, BeginSynchronizedUpdate, EndSynchronizedUpdate},
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

    /// Borrow the underlying writer (used for inspecting captured bytes
    /// during tests; production code should not need this).
    pub fn inner(&self) -> &W {
        &self.out
    }

    /// Mutably borrow the underlying writer. Useful when the writer is a
    /// drainable buffer (e.g., `Vec<u8>`) and the daemon's attach loop
    /// wants to take and ship the bytes after a render cycle.
    pub fn inner_mut(&mut self) -> &mut W {
        &mut self.out
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
    pub fn render_full(&mut self, width: u16, height: u16, cells: &[RenderCell]) -> io::Result<()> {
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
        // Attributes first -- Attribute::Reset clears fg/bg too, so we
        // must handle attributes before colors.
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

            // After Attribute::Reset, fg/bg state is also reset.
            // Force re-emit of colors below.
            self.last_fg = None;
            self.last_bg = None;
        }

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

        Ok(())
    }

    /// Clear the entire screen.
    pub fn clear_screen(&mut self) -> io::Result<()> {
        self.out.queue(terminal::Clear(terminal::ClearType::All))?;
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

    #[test]
    fn test_hide_cursor() {
        let mut output = Vec::new();
        let mut backend = RenderBackend::new(&mut output);
        backend.hide_cursor().unwrap();
        assert!(!output.is_empty());
    }

    #[test]
    fn test_show_cursor() {
        let mut output = Vec::new();
        let mut backend = RenderBackend::new(&mut output);
        backend.show_cursor().unwrap();
        assert!(!output.is_empty());
    }

    #[test]
    fn test_set_cursor_position() {
        let mut output = Vec::new();
        let mut backend = RenderBackend::new(&mut output);

        backend.set_cursor(10, 20).unwrap();
        assert!(!output.is_empty());
    }

    #[test]
    fn test_render_diff_with_styled_cells() {
        let mut output = Vec::new();
        let mut backend = RenderBackend::new(&mut output);

        let dirty = vec![
            DirtyCell {
                col: 0,
                row: 0,
                cell: RenderCell::styled(
                    'A',
                    Some(crossterm::style::Color::Red),
                    Some(crossterm::style::Color::Blue),
                    RenderAttrs {
                        bold: true,
                        ..RenderAttrs::default()
                    },
                ),
            },
            DirtyCell {
                col: 1,
                row: 0,
                cell: RenderCell::styled(
                    'B',
                    Some(crossterm::style::Color::Green),
                    None,
                    RenderAttrs::default(),
                ),
            },
        ];

        backend.render_diff(&dirty).unwrap();

        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains('A'));
        assert!(output_str.contains('B'));
    }

    #[test]
    fn test_render_full() {
        let mut output = Vec::new();
        let mut backend = RenderBackend::new(&mut output);

        let cells = vec![
            RenderCell::text('A'),
            RenderCell::text('B'),
            RenderCell::text('C'),
            RenderCell::text('D'),
        ];

        backend.render_full(2, 2, &cells).unwrap();

        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains('A'));
        assert!(output_str.contains('B'));
        assert!(output_str.contains('C'));
        assert!(output_str.contains('D'));
    }

    #[test]
    fn test_render_full_skips_wide_continuation() {
        let mut output = Vec::new();
        let mut backend = RenderBackend::new(&mut output);

        let cells = vec![
            RenderCell::text('\u{4E16}'), // Wide CJK char
            RenderCell {
                ch: ' ',
                wide_continuation: true,
                ..RenderCell::default()
            },
        ];

        backend.render_full(2, 1, &cells).unwrap();

        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains('\u{4E16}'));
    }

    #[test]
    fn test_style_tracking_avoids_redundant_changes() {
        // Two cells with the same style should not produce redundant
        // style sequences between them (beyond the initial one).
        let mut output1 = Vec::new();
        let mut backend1 = RenderBackend::new(&mut output1);

        let attrs = RenderAttrs {
            bold: true,
            ..RenderAttrs::default()
        };
        let dirty_same_style = vec![
            DirtyCell {
                col: 0,
                row: 0,
                cell: RenderCell::styled('A', Some(crossterm::style::Color::Red), None, attrs),
            },
            DirtyCell {
                col: 1,
                row: 0,
                cell: RenderCell::styled('B', Some(crossterm::style::Color::Red), None, attrs),
            },
        ];
        backend1.render_diff(&dirty_same_style).unwrap();
        let same_style_len = output1.len();

        // Now render two cells with DIFFERENT styles
        let mut output2 = Vec::new();
        let mut backend2 = RenderBackend::new(&mut output2);

        let dirty_diff_style = vec![
            DirtyCell {
                col: 0,
                row: 0,
                cell: RenderCell::styled(
                    'A',
                    Some(crossterm::style::Color::Red),
                    None,
                    RenderAttrs {
                        bold: true,
                        ..RenderAttrs::default()
                    },
                ),
            },
            DirtyCell {
                col: 1,
                row: 0,
                cell: RenderCell::styled(
                    'B',
                    Some(crossterm::style::Color::Green),
                    None,
                    RenderAttrs {
                        italic: true,
                        ..RenderAttrs::default()
                    },
                ),
            },
        ];
        backend2.render_diff(&dirty_diff_style).unwrap();
        let diff_style_len = output2.len();

        // Different styles should produce more output than same styles
        assert!(diff_style_len > same_style_len);
    }
}
