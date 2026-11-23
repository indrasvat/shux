//! RenderBackend: crossterm output abstraction.
//!
//! Translates `DirtyCell` values into crossterm commands. Batches all commands
//! into a single write using crossterm's command queue for performance.
//! Uses synchronized output (Mode 2026) via `BeginSynchronizedUpdate` /
//! `EndSynchronizedUpdate` to prevent tearing (PRD section 6.1).

use std::fmt;
use std::io::{self, Write};

use crossterm::{
    Command, QueueableCommand,
    cursor::{MoveTo, SetCursorStyle},
    style::{Attribute, Color as CtColor, Print, ResetColor, SetAttribute},
    terminal::{self, BeginSynchronizedUpdate, EndSynchronizedUpdate},
};

use crate::buffer::{DirtyCell, RenderAttrs, RenderCell};
use crate::vt_convert;

/// Abstraction over crossterm terminal output. Queues commands and
/// flushes them in a single synchronized batch.
pub struct RenderBackend<W: Write> {
    out: W,
    /// Track the last style we emitted to avoid redundant style changes.
    last_fg: Option<Option<CtColor>>,
    last_bg: Option<Option<CtColor>>,
    last_attrs: Option<RenderAttrs>,
    last_underline_style: Option<shux_vt::UnderlineStyle>,
    last_underline_color: Option<Option<CtColor>>,
    last_hyperlink: Option<Option<String>>,
    plain: bool,
}

/// A plain client's frame without the zero-width characters that reorder or
/// hide text without drawing anything. Applied to the whole frame, so copy
/// mode, selection and overlays are covered as well as cells.
pub fn strip_hidden_controls(bytes: Vec<u8>) -> Vec<u8> {
    let hidden = |chunk: std::str::Utf8Chunk<'_>| chunk.valid().chars().any(is_hidden_control);
    if !bytes.utf8_chunks().any(hidden) {
        return bytes;
    }
    let mut out = Vec::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        for c in chunk.valid().chars().filter(|&c| !is_hidden_control(c)) {
            let mut b = [0; 4];
            out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
        }
        out.extend_from_slice(chunk.invalid());
    }
    out
}

/// Bidi overrides and isolates, marks, and invisible spaces. ZWJ and ZWNJ
/// are not here; emoji and conjuncts need them.
fn is_hidden_control(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{200B}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SetAnsiForegroundColor(CtColor);

impl Command for SetAnsiForegroundColor {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write_color_sgr(f, ColorTarget::Foreground, self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SetAnsiBackgroundColor(CtColor);

impl Command for SetAnsiBackgroundColor {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write_color_sgr(f, ColorTarget::Background, self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SetAnsiUnderlineColor(CtColor);

impl Command for SetAnsiUnderlineColor {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write_color_sgr(f, ColorTarget::Underline, self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColorTarget {
    Foreground,
    Background,
    Underline,
}

fn write_color_sgr(f: &mut impl fmt::Write, target: ColorTarget, color: CtColor) -> fmt::Result {
    match color {
        CtColor::Reset => write!(f, "\x1b[{}m", reset_code(target)),
        CtColor::AnsiValue(index) => write!(f, "\x1b[{};5;{}m", extended_code(target), index),
        CtColor::Rgb { r, g, b } => {
            write!(f, "\x1b[{};2;{};{};{}m", extended_code(target), r, g, b)
        }
        named => {
            let Some(index) = named_color_index(named) else {
                return write!(f, "\x1b[{}m", reset_code(target));
            };
            match target {
                ColorTarget::Foreground | ColorTarget::Background => {
                    let base = if target == ColorTarget::Foreground {
                        if index < 8 { 30 } else { 90 }
                    } else if index < 8 {
                        40
                    } else {
                        100
                    };
                    write!(f, "\x1b[{}m", base + u16::from(index % 8))
                }
                ColorTarget::Underline => {
                    write!(f, "\x1b[{};5;{}m", extended_code(target), index)
                }
            }
        }
    }
}

fn reset_code(target: ColorTarget) -> u16 {
    match target {
        ColorTarget::Foreground => 39,
        ColorTarget::Background => 49,
        ColorTarget::Underline => 59,
    }
}

fn extended_code(target: ColorTarget) -> u16 {
    match target {
        ColorTarget::Foreground => 38,
        ColorTarget::Background => 48,
        ColorTarget::Underline => 58,
    }
}

fn named_color_index(color: CtColor) -> Option<u8> {
    match color {
        CtColor::Black => Some(0),
        CtColor::DarkRed => Some(1),
        CtColor::DarkGreen => Some(2),
        CtColor::DarkYellow => Some(3),
        CtColor::DarkBlue => Some(4),
        CtColor::DarkMagenta => Some(5),
        CtColor::DarkCyan => Some(6),
        CtColor::Grey => Some(7),
        CtColor::DarkGrey => Some(8),
        CtColor::Red => Some(9),
        CtColor::Green => Some(10),
        CtColor::Yellow => Some(11),
        CtColor::Blue => Some(12),
        CtColor::Magenta => Some(13),
        CtColor::Cyan => Some(14),
        CtColor::White => Some(15),
        _ => None,
    }
}

impl<W: Write> RenderBackend<W> {
    pub fn new(out: W) -> Self {
        Self {
            out,
            last_fg: None,
            last_bg: None,
            last_attrs: None,
            last_underline_style: None,
            last_underline_color: None,
            last_hyperlink: None,
            plain: false,
        }
    }

    pub fn set_plain(&mut self, plain: bool) {
        self.plain = plain;
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
        // Autowrap off while drawing: a frame composed for a wider host than
        // this one clips at its edge instead of wrapping onto the next row.
        self.out.write_all(b"\x1b[?7l")?;

        // Where the host cursor is known to sit. Only a single-column ASCII
        // print advances it predictably on every terminal; after anything
        // else the next cell is positioned explicitly, so a width the host
        // disagrees with heals at once.
        let mut cursor: Option<(u16, u16)> = None;
        for cell in dirty {
            if cursor != Some((cell.col, cell.row)) {
                self.out.queue(MoveTo(cell.col, cell.row))?;
            }

            // Apply style changes (only emit commands when style differs
            // from the last emitted style to reduce output volume)
            self.apply_style(&cell.cell)?;

            // Print the full cell display text when the VT preserved a
            // multi-codepoint grapheme payload.
            self.out.queue(Print(cell.cell.display_text()))?;

            let plain_ascii = (cell.cell.ch.is_ascii_graphic() || cell.cell.ch == ' ')
                && cell
                    .cell
                    .extended
                    .as_ref()
                    .is_none_or(|attrs| attrs.grapheme.is_none());
            cursor = plain_ascii.then(|| (cell.col.saturating_add(1), cell.row));
        }

        // Reset colors at the end so the terminal is in a clean state
        self.clear_hyperlink()?;
        self.out.queue(ResetColor)?;
        self.out.write_all(b"\x1b[?7h")?;

        // End synchronized update
        self.out.queue(EndSynchronizedUpdate)?;

        // Flush everything in one write
        self.out.flush()?;

        // Reset tracked style state (we just reset colors)
        self.last_fg = None;
        self.last_bg = None;
        self.last_attrs = None;
        self.last_underline_style = None;
        self.last_underline_color = None;

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
                self.out.queue(Print(cell.display_text()))?;
            }
        }

        self.clear_hyperlink()?;
        self.out.queue(ResetColor)?;
        self.out.queue(EndSynchronizedUpdate)?;
        self.out.flush()?;

        self.last_fg = None;
        self.last_bg = None;
        self.last_attrs = None;
        self.last_underline_style = None;
        self.last_underline_color = None;

        Ok(())
    }

    /// Apply foreground, background, and attribute style to the output
    /// stream. Only emits crossterm commands when the style actually
    /// changes from the last emitted style.
    fn apply_style(&mut self, cell: &RenderCell) -> io::Result<()> {
        let underline_style = cell
            .extended
            .as_deref()
            .map(|ext| ext.underline_style)
            .unwrap_or_default();
        let underline_color = cell
            .extended
            .as_deref()
            .and_then(|ext| ext.underline_color)
            .and_then(vt_convert::vt_color_to_crossterm);
        let hyperlink = cell
            .extended
            .as_deref()
            .and_then(|ext| ext.hyperlink.as_deref());

        self.apply_hyperlink(hyperlink)?;

        // Attributes first -- Attribute::Reset clears fg/bg too, so we
        // must handle attributes before colors.
        if self.last_attrs != Some(cell.attrs)
            || self.last_underline_style != Some(underline_style)
            || self.last_underline_color != Some(underline_color)
        {
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
            match underline_style {
                shux_vt::UnderlineStyle::None if cell.attrs.underline => {
                    self.out.queue(SetAttribute(Attribute::Underlined))?;
                }
                shux_vt::UnderlineStyle::None => {}
                shux_vt::UnderlineStyle::Single => {
                    self.out.queue(SetAttribute(Attribute::Underlined))?;
                }
                shux_vt::UnderlineStyle::Double => {
                    self.out.queue(SetAttribute(Attribute::DoubleUnderlined))?;
                }
                shux_vt::UnderlineStyle::Curly => {
                    self.out.queue(SetAttribute(Attribute::Undercurled))?;
                }
                shux_vt::UnderlineStyle::Dotted => {
                    self.out.queue(SetAttribute(Attribute::Underdotted))?;
                }
                shux_vt::UnderlineStyle::Dashed => {
                    self.out.queue(SetAttribute(Attribute::Underdashed))?;
                }
            }
            if let Some(color) = underline_color {
                self.out.queue(SetAnsiUnderlineColor(color))?;
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
            self.last_underline_style = Some(underline_style);
            self.last_underline_color = Some(underline_color);

            // After Attribute::Reset, fg/bg state is also reset.
            // Force re-emit of colors below.
            self.last_fg = None;
            self.last_bg = None;
        }

        // Foreground
        if self.last_fg != Some(cell.fg) {
            match cell.fg {
                Some(color) => {
                    self.out.queue(SetAnsiForegroundColor(color))?;
                }
                None => {
                    self.out.queue(SetAnsiForegroundColor(CtColor::Reset))?;
                }
            }
            self.last_fg = Some(cell.fg);
        }

        // Background
        if self.last_bg != Some(cell.bg) {
            match cell.bg {
                Some(color) => {
                    self.out.queue(SetAnsiBackgroundColor(color))?;
                }
                None => {
                    self.out.queue(SetAnsiBackgroundColor(CtColor::Reset))?;
                }
            }
            self.last_bg = Some(cell.bg);
        }

        Ok(())
    }

    fn apply_hyperlink(&mut self, hyperlink: Option<&str>) -> io::Result<()> {
        if self.plain {
            return Ok(());
        }
        let next = hyperlink.map(str::to_string);
        if self.last_hyperlink.is_none() && next.is_none() {
            return Ok(());
        }
        if self.last_hyperlink.as_ref() == Some(&next) {
            return Ok(());
        }

        match hyperlink {
            Some(uri) => write!(self.out, "\x1b]8;;{uri}\x1b\\")?,
            None => self.out.write_all(b"\x1b]8;;\x1b\\")?,
        }
        self.last_hyperlink = Some(next);
        Ok(())
    }

    fn clear_hyperlink(&mut self) -> io::Result<()> {
        if self
            .last_hyperlink
            .as_ref()
            .is_some_and(|link| link.is_some())
        {
            self.out.write_all(b"\x1b]8;;\x1b\\")?;
        }
        self.last_hyperlink = None;
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

    /// Set the host terminal cursor shape to match the focused pane's VT cursor.
    pub fn set_cursor_shape(&mut self, shape: shux_vt::CursorShape) -> io::Result<()> {
        if self.plain {
            return Ok(());
        }
        let style = match shape {
            shux_vt::CursorShape::Block => SetCursorStyle::SteadyBlock,
            shux_vt::CursorShape::Underline => SetCursorStyle::SteadyUnderScore,
            shux_vt::CursorShape::Bar => SetCursorStyle::SteadyBar,
        };
        self.out.queue(style)?;
        self.out.flush()
    }

    /// Set or reset the host terminal cursor color.
    ///
    /// Crossterm does not expose OSC 12, so write the xterm-compatible
    /// sequence directly. `None` resets to the user's terminal default.
    pub fn set_cursor_color(&mut self, color: Option<shux_vt::Rgb>) -> io::Result<()> {
        if self.plain {
            return Ok(());
        }
        match color {
            Some([r, g, b]) => write!(self.out, "\x1b]12;#{r:02x}{g:02x}{b:02x}\x1b\\")?,
            None => self.out.write_all(b"\x1b]112\x1b\\")?,
        }
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
    use std::sync::Arc;

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
    fn grapheme_render_diff_emits_full_payload() {
        let mut output = Vec::new();
        let mut backend = RenderBackend::new(&mut output);
        let mut cell = RenderCell::text('e');
        cell.extended = Some(Arc::new(shux_vt::ExtendedAttrs {
            grapheme: Some("e\u{0301}".to_string()),
            hyperlink: None,
            underline_color: None,
            underline_style: shux_vt::UnderlineStyle::None,
        }));

        backend
            .render_diff(&[DirtyCell {
                col: 0,
                row: 0,
                cell,
            }])
            .unwrap();

        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains("e\u{0301}"));
    }

    /// Skipping the cursor move between adjacent cells must leave the screen
    /// exactly as positioning every cell does, for any mix of cells.
    #[test]
    fn render_diff_skipping_moves_draws_the_same_screen() {
        const W: u16 = 14;
        const H: u16 = 3;
        let mut seed: u64 = 0x5eed_cafe;
        let mut next = move |n: u64| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        let ext = |grapheme: Option<&str>, link: Option<&str>| {
            Some(Arc::new(shux_vt::ExtendedAttrs {
                grapheme: grapheme.map(str::to_string),
                hyperlink: link.map(str::to_string),
                underline_color: None,
                underline_style: shux_vt::UnderlineStyle::None,
            }))
        };
        for _ in 0..2000 {
            let mut dirty = Vec::new();
            for row in 0..H {
                let mut col = 0;
                while col < W {
                    let skip = next(4) == 0;
                    let mut cell = RenderCell::text(char::from(b'a' + next(26) as u8));
                    let mut width = 1;
                    match next(9) {
                        0 => cell.ch = ' ',
                        1 if col + 1 < W => {
                            cell.ch = '界';
                            width = 2;
                        }
                        2 if col + 1 < W => {
                            cell.ch = '👍';
                            width = 2;
                        }
                        3 => cell.extended = ext(Some("e\u{0301}"), None),
                        4 => cell.extended = ext(None, Some("https://example.invalid/x")),
                        5 => {
                            cell.fg = Some(crossterm::style::Color::Rgb {
                                r: 200,
                                g: 10,
                                b: next(255) as u8,
                            })
                        }
                        6 => cell.attrs.bold = true,
                        _ => {}
                    }
                    if !skip {
                        dirty.push(DirtyCell { col, row, cell });
                    }
                    col += width;
                }
            }
            let mut old = Vec::new();
            {
                let mut b = RenderBackend::new(&mut old);
                for d in &dirty {
                    b.render_diff(std::slice::from_ref(d)).unwrap();
                }
            }
            let mut new = Vec::new();
            RenderBackend::new(&mut new).render_diff(&dirty).unwrap();
            let (mut a, mut b) = (
                shux_vt::VirtualTerminal::new(H.into(), W.into()),
                shux_vt::VirtualTerminal::new(H.into(), W.into()),
            );
            a.process(&old);
            b.process(&new);
            for r in 0..usize::from(H) {
                for c in 0..usize::from(W) {
                    assert_eq!(
                        a.grid().visible_row(r).get(c),
                        b.grid().visible_row(r).get(c),
                        "cell ({r},{c}) differs for {dirty:?}"
                    );
                }
            }
        }
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
    fn a_plain_backend_draws_text_and_colour_but_no_terminal_effects() {
        let mut output = Vec::new();
        let mut backend = RenderBackend::new(&mut output);
        backend.set_plain(true);
        let mut cell = RenderCell::text('L');
        cell.fg = Some(CtColor::Rgb { r: 1, g: 2, b: 3 });
        cell.extended = Some(Arc::new(shux_vt::ExtendedAttrs {
            grapheme: None,
            hyperlink: Some("https://example.invalid/x".to_string()),
            underline_color: None,
            underline_style: shux_vt::UnderlineStyle::None,
        }));
        backend
            .render_diff(&[DirtyCell {
                col: 0,
                row: 0,
                cell,
            }])
            .unwrap();
        backend.set_cursor_shape(shux_vt::CursorShape::Bar).unwrap();
        backend.set_cursor_color(Some([9, 9, 9])).unwrap();
        backend.set_cursor_color(None).unwrap();

        let out = String::from_utf8(output).unwrap();
        assert!(out.contains('L') && out.contains("38;2;1;2;3"), "{out:?}");
        for effect in ["\x1b]8;", "\x1b]12;", "\x1b]112", " q"] {
            assert!(
                !out.contains(effect),
                "plain output carries {effect:?}: {out:?}"
            );
        }
    }

    /// A cell in the last column leaves the host cursor in its pending-wrap
    /// state, so the next row's first cell is always positioned explicitly.
    #[test]
    fn a_cell_after_the_last_column_is_positioned_explicitly() {
        let mut output = Vec::new();
        let dirty = [
            DirtyCell {
                col: 9,
                row: 0,
                cell: RenderCell::text('z'),
            },
            DirtyCell {
                col: 0,
                row: 1,
                cell: RenderCell::text('a'),
            },
        ];
        RenderBackend::new(&mut output).render_diff(&dirty).unwrap();
        let out = String::from_utf8_lossy(&output).into_owned();
        assert!(
            out.contains("\x1b[2;1H"),
            "no move to the next row: {out:?}"
        );
        let mut vt = shux_vt::VirtualTerminal::new(3, 10);
        vt.process(&output);
        assert_eq!(vt.grid().visible_row(0)[9].ch, 'z');
        assert_eq!(vt.grid().visible_row(1)[0].ch, 'a');
    }

    /// The host may draw a wide or composed glyph at another width than shux
    /// does, so the cell after one is always positioned explicitly.
    #[test]
    fn a_cell_after_a_wide_glyph_is_positioned_explicitly() {
        let mut output = Vec::new();
        let dirty = [
            DirtyCell {
                col: 0,
                row: 0,
                cell: RenderCell::text('界'),
            },
            DirtyCell {
                col: 2,
                row: 0,
                cell: RenderCell::text('a'),
            },
        ];
        RenderBackend::new(&mut output).render_diff(&dirty).unwrap();
        let out = String::from_utf8_lossy(&output).into_owned();
        assert!(
            out.contains("\x1b[1;3H"),
            "no move after the wide glyph: {out:?}"
        );
    }

    /// A frame composed for a wider terminal can land after the host shrank.
    /// It must clip at the host's edge, as positioning every cell did, not
    /// wrap onto the next row (or scroll the screen from the bottom one).
    #[test]
    fn a_frame_wider_than_the_host_clips_instead_of_wrapping() {
        let dirty: Vec<DirtyCell> = "abcdefg"
            .chars()
            .enumerate()
            .map(|(col, ch)| DirtyCell {
                col: col as u16,
                row: 0,
                cell: RenderCell::text(ch),
            })
            .collect();
        let mut output = Vec::new();
        RenderBackend::new(&mut output).render_diff(&dirty).unwrap();
        let mut host = shux_vt::VirtualTerminal::new(2, 4);
        host.process(&output);
        let row = |r: usize| -> String {
            let cells = host.grid().visible_row(r);
            (0..cells.len()).map(|c| cells[c].ch).collect()
        };
        assert_eq!(
            row(1).trim(),
            "",
            "the frame wrapped: {:?} / {:?}",
            row(0),
            row(1)
        );
        assert!(row(0).starts_with("abc"), "{:?}", row(0));
    }

    /// Invisible direction and spacing controls a pane printed can reorder
    /// or hide text in a guest's terminal; a plain viewer gets the visible
    /// text, and joiners that build emoji and conjuncts stay.
    #[test]
    fn stripping_drops_invisible_direction_controls_and_keeps_joiners() {
        let frame = "\x1b[1;1HA\u{202E}\u{200B}\u{2066}\u{FEFF}\x1b[1;3H\u{1F468}\u{200D}\u{1F469}\u{0915}\u{094D}\u{200C}";
        let mut bytes = frame.as_bytes().to_vec();
        bytes.push(0xFF);
        let out = strip_hidden_controls(bytes);
        assert_eq!(out.last(), Some(&0xFF), "bytes that are not text were lost");
        let out = String::from_utf8_lossy(&out).into_owned();
        for hidden in ['\u{202E}', '\u{200B}', '\u{2066}', '\u{FEFF}'] {
            assert!(!out.contains(hidden), "plain output carries {hidden:?}");
        }
        assert!(
            out.contains("\u{1F468}\u{200D}\u{1F469}"),
            "a ZWJ emoji was broken"
        );
        assert!(out.contains("\u{094D}\u{200C}"), "a ZWNJ was dropped");
    }

    #[test]
    fn test_set_cursor_shape_and_color() {
        let mut output = Vec::new();
        let mut backend = RenderBackend::new(&mut output);

        backend
            .set_cursor_shape(shux_vt::CursorShape::Underline)
            .unwrap();
        backend.set_cursor_color(Some([0, 255, 128])).unwrap();
        backend.set_cursor_color(None).unwrap();

        let rendered = String::from_utf8(output).unwrap();
        assert!(rendered.contains("\x1b[4 q"));
        assert!(rendered.contains("\x1b]12;#00ff80\x1b\\"));
        assert!(rendered.contains("\x1b]112\x1b\\"));
    }

    #[test]
    fn test_render_diff_with_styled_cells() {
        crossterm::style::Colored::set_ansi_color_disabled(false);
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
        assert!(
            output_str.contains("\x1b[91m"),
            "missing red fg SGR: {output_str:?}"
        );
        assert!(
            output_str.contains("\x1b[104m"),
            "missing blue bg SGR: {output_str:?}"
        );
        assert!(
            output_str.contains("\x1b[92m"),
            "missing green fg SGR: {output_str:?}"
        );
    }

    #[test]
    fn test_render_backend_colors_ignore_no_color_without_mutating_global_state() {
        crossterm::style::Colored::set_ansi_color_disabled(true);
        let mut output = Vec::new();
        let mut backend = RenderBackend::new(&mut output);
        assert!(crossterm::style::Colored::ansi_color_disabled_memoized());

        backend
            .render_diff(&[
                DirtyCell {
                    col: 0,
                    row: 0,
                    cell: RenderCell::styled(
                        'R',
                        Some(crossterm::style::Color::Rgb { r: 1, g: 2, b: 3 }),
                        Some(crossterm::style::Color::Rgb { r: 4, g: 5, b: 6 }),
                        RenderAttrs::default(),
                    ),
                },
                DirtyCell {
                    col: 1,
                    row: 0,
                    cell: RenderCell::styled(
                        'I',
                        Some(crossterm::style::Color::AnsiValue(196)),
                        None,
                        RenderAttrs::default(),
                    ),
                },
            ])
            .unwrap();

        assert!(crossterm::style::Colored::ansi_color_disabled_memoized());
        crossterm::style::Colored::set_ansi_color_disabled(false);

        let output_str = String::from_utf8_lossy(&output);
        assert!(
            output_str.contains("\x1b[38;2;1;2;3m"),
            "truecolor fg was stripped: {output_str:?}"
        );
        assert!(
            output_str.contains("\x1b[48;2;4;5;6m"),
            "truecolor bg was stripped: {output_str:?}"
        );
        assert!(
            output_str.contains("\x1b[38;5;196m"),
            "256-color fg was stripped: {output_str:?}"
        );
    }

    #[test]
    fn test_render_diff_emits_and_clears_hyperlinks() {
        let mut output = Vec::new();
        let mut backend = RenderBackend::new(&mut output);

        let mut cell = RenderCell::text('L');
        cell.extended = Some(Arc::new(shux_vt::ExtendedAttrs {
            grapheme: None,
            hyperlink: Some("https://example.invalid/a;b".to_string()),
            underline_color: None,
            underline_style: shux_vt::UnderlineStyle::None,
        }));

        backend
            .render_diff(&[DirtyCell {
                col: 0,
                row: 0,
                cell,
            }])
            .unwrap();

        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains("\x1b]8;;https://example.invalid/a;b\x1b\\"));
        assert!(output_str.contains("L\x1b]8;;\x1b\\"));
    }

    #[test]
    fn test_render_diff_emits_advanced_underline_style_and_color() {
        crossterm::style::Colored::set_ansi_color_disabled(false);
        let mut output = Vec::new();
        let mut backend = RenderBackend::new(&mut output);

        let mut cell = RenderCell::text('U');
        cell.attrs.underline = true;
        cell.extended = Some(Arc::new(shux_vt::ExtendedAttrs {
            grapheme: None,
            hyperlink: None,
            underline_color: Some(shux_vt::Color::Rgb(10, 20, 30)),
            underline_style: shux_vt::UnderlineStyle::Curly,
        }));

        backend
            .render_diff(&[DirtyCell {
                col: 0,
                row: 0,
                cell,
            }])
            .unwrap();

        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains("\x1b[4:3m"));
        assert!(output_str.contains("\x1b[58;2;10;20;30m"));
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
    fn grapheme_render_full_emits_full_payload() {
        let mut output = Vec::new();
        let mut backend = RenderBackend::new(&mut output);
        let mut cell = RenderCell::text('👨');
        cell.extended = Some(Arc::new(shux_vt::ExtendedAttrs {
            grapheme: Some("👨\u{200d}💻".to_string()),
            hyperlink: None,
            underline_color: None,
            underline_style: shux_vt::UnderlineStyle::None,
        }));

        backend.render_full(1, 1, &[cell]).unwrap();

        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains("👨\u{200d}💻"));
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
