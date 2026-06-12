//! Isolated spike harness for evaluating `libghostty-vt` as a possible
//! optional shux snapshot/VT backend.
//!
//! This crate is deliberately outside the main workspace. It should answer
//! fit questions without making normal shux builds depend on libghostty.

use libghostty_vt::render::{CellIterator, CursorVisualStyle, Dirty, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::style::{RgbColor, Style};
use libghostty_vt::{RenderState, Terminal, TerminalOptions};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeCell {
    pub graphemes: String,
    pub grapheme_len: usize,
    pub wide: CellWide,
    pub has_text: bool,
    pub style: Style,
    pub fg: Option<RgbColor>,
    pub bg: Option<RgbColor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeSnapshot {
    pub cols: u16,
    pub rows: u16,
    pub dirty: Dirty,
    pub cursor_style: CursorVisualStyle,
    pub cursor_color: Option<RgbColor>,
    pub cells: Vec<Vec<ProbeCell>>,
}

pub fn capture(input: &[u8], cols: u16, rows: u16) -> libghostty_vt::error::Result<ProbeSnapshot> {
    let mut terminal = Terminal::new(TerminalOptions {
        cols,
        rows,
        max_scrollback: 1_000,
    })?;
    terminal.vt_write(input);
    capture_terminal(&terminal)
}

pub fn capture_after_resize(
    input: &[u8],
    initial_cols: u16,
    initial_rows: u16,
    resized_cols: u16,
    resized_rows: u16,
) -> libghostty_vt::error::Result<ProbeSnapshot> {
    let mut terminal = Terminal::new(TerminalOptions {
        cols: initial_cols,
        rows: initial_rows,
        max_scrollback: 1_000,
    })?;
    terminal.vt_write(input);
    terminal.resize(resized_cols, resized_rows, 8, 16)?;
    capture_terminal(&terminal)
}

fn capture_terminal(terminal: &Terminal<'_, '_>) -> libghostty_vt::error::Result<ProbeSnapshot> {
    let mut render_state = RenderState::new()?;
    let snapshot = render_state.update(terminal)?;
    let cols = snapshot.cols()?;
    let row_count = snapshot.rows()?;
    let dirty = snapshot.dirty()?;
    let cursor_style = snapshot.cursor_visual_style()?;
    let cursor_color = snapshot.cursor_color()?;

    let mut row_iter = RowIterator::new()?;
    let mut cell_iter = CellIterator::new()?;
    let mut rows_out = Vec::new();
    let mut row_iteration = row_iter.update(&snapshot)?;

    while let Some(row) = row_iteration.next() {
        let mut cells_out = Vec::new();
        let mut cells = cell_iter.update(row)?;
        while let Some(cell) = cells.next() {
            let raw = cell.raw_cell()?;
            let graphemes: String = cell.graphemes()?.into_iter().collect();
            cells_out.push(ProbeCell {
                grapheme_len: cell.graphemes_len()?,
                graphemes,
                wide: raw.wide()?,
                has_text: raw.has_text()?,
                style: cell.style()?,
                fg: cell.fg_color()?,
                bg: cell.bg_color()?,
            });
        }
        rows_out.push(cells_out);
    }

    Ok(ProbeSnapshot {
        cols,
        rows: row_count,
        dirty,
        cursor_style,
        cursor_color,
        cells: rows_out,
    })
}

pub fn row_text(snapshot: &ProbeSnapshot, row: usize) -> String {
    snapshot.cells[row]
        .iter()
        .filter(|cell| cell.has_text && cell.wide != CellWide::SpacerTail)
        .map(|cell| cell.graphemes.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use libghostty_vt::style::{StyleColor, Underline};

    fn rgb(r: u8, g: u8, b: u8) -> RgbColor {
        RgbColor { r, g, b }
    }

    #[test]
    fn libghostty_extracts_basic_render_state() {
        let snap = capture(b"hello", 12, 3).expect("capture");

        assert_eq!(snap.cols, 12);
        assert_eq!(snap.rows, 3);
        assert_eq!(row_text(&snap, 0), "hello");
        assert_eq!(snap.cells[0][0].graphemes, "h");
        assert_eq!(snap.cells[0][0].wide, CellWide::Narrow);
        assert!(matches!(snap.dirty, Dirty::Full | Dirty::Partial));
    }

    #[test]
    fn libghostty_preserves_combining_marks_but_splits_extended_emoji_sequences() {
        let sample = "e\u{301} 🏳️\u{200d}🌈 👍🏽";
        let snap = capture(sample.as_bytes(), 20, 3).expect("capture");
        let row = &snap.cells[0];

        assert!(
            row.iter()
                .any(|cell| cell.graphemes == "e\u{301}" && cell.grapheme_len == 2),
            "combining acute accent should remain attached to base character: {row:?}"
        );
        assert!(
            row.iter()
                .any(|cell| cell.graphemes == "🏳\u{fe0f}\u{200d}" && cell.grapheme_len == 3),
            "current libghostty-vt exposes the first rainbow-flag segment separately: {row:?}"
        );
        assert!(
            row.iter()
                .any(|cell| cell.graphemes == "🌈" && cell.wide == CellWide::Wide),
            "current libghostty-vt exposes the rainbow glyph separately: {row:?}"
        );
        assert!(
            row.iter()
                .any(|cell| cell.graphemes == "👍" && cell.wide == CellWide::Wide),
            "current libghostty-vt exposes the thumbs-up base emoji separately: {row:?}"
        );
        assert!(
            row.iter()
                .any(|cell| cell.graphemes == "🏽" && cell.wide == CellWide::Wide),
            "current libghostty-vt exposes the skin-tone modifier separately: {row:?}"
        );
    }

    #[test]
    fn current_shux_vt_cell_model_loses_combining_mark_context() {
        let mut vt = shux_vt::VirtualTerminal::new(20, 3);
        vt.process("e\u{301} X".as_bytes());
        let row = vt.grid().visible_row(0);
        let chars: Vec<char> = (0..row.len())
            .map(|col| &row[col])
            .filter(|cell| !cell.is_wide_continuation())
            .map(|cell| cell.ch)
            .collect();

        assert_eq!(chars[0], 'e');
        assert!(!chars.contains(&'\u{301}'));
    }

    #[test]
    fn libghostty_reports_wide_and_spacer_cells() {
        let snap = capture("A你B".as_bytes(), 8, 2).expect("capture");
        let row = &snap.cells[0];

        assert_eq!(row[0].graphemes, "A");
        assert_eq!(row[0].wide, CellWide::Narrow);
        assert_eq!(row[1].graphemes, "你");
        assert_eq!(row[1].wide, CellWide::Wide);
        assert_eq!(row[2].wide, CellWide::SpacerTail);
        assert_eq!(row[3].graphemes, "B");
    }

    #[test]
    fn libghostty_extracts_sgr_truecolor_and_text_styles() {
        let snap =
            capture(b"\x1b[1;3;4;38;2;12;34;56;48;2;90;80;70mX\x1b[0m", 4, 2).expect("capture");
        let cell = &snap.cells[0][0];

        assert_eq!(cell.graphemes, "X");
        assert!(cell.style.bold);
        assert!(cell.style.italic);
        assert_eq!(cell.style.underline, Underline::Single);
        assert_eq!(cell.style.fg_color, StyleColor::Rgb(rgb(12, 34, 56)));
        assert_eq!(cell.style.bg_color, StyleColor::Rgb(rgb(90, 80, 70)));
        assert_eq!(cell.fg, Some(rgb(12, 34, 56)));
        assert_eq!(cell.bg, Some(rgb(90, 80, 70)));
    }

    #[test]
    fn libghostty_extracts_cursor_shape_and_color() {
        let snap = capture(b"\x1b[5 q\x1b]12;#00ff80\x07X", 4, 2).expect("capture");

        assert_eq!(snap.cursor_style, CursorVisualStyle::Bar);
        assert_eq!(snap.cursor_color, Some(rgb(0, 255, 128)));
    }

    #[test]
    fn libghostty_resize_reflows_main_screen_content() {
        let snap = capture_after_resize(b"abcdef ghijkl", 12, 4, 6, 4).expect("capture");

        assert_eq!(snap.cols, 6);
        assert_eq!(snap.rows, 4);
        let text = (0..snap.cells.len())
            .map(|row| row_text(&snap, row))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(text.replace('\n', ""), "abcdef ghijkl");
    }
}
