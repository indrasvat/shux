//! shux-vt -- Virtual terminal grid and VT parser.
//!
//! Provides per-pane terminal emulation: a VecDeque-based grid that tracks
//! cell content, styles, cursor position, and scrollback. Driven by the
//! vte crate parsing raw PTY output bytes.

mod cell;
mod charset;
mod cursor;
mod grid;
mod parser;

pub use cell::{
    Cell, CellFlags, CellStyle, Color, ExtendedAttrs, Rgb, TerminalDefaultColors, UnderlineStyle,
};
pub use charset::{CharsetSlot, TerminalCharset, TerminalCharsets};
pub use cursor::{Cursor, CursorShape, SavedCursor};
pub use grid::{Grid, GridConfig, Row};
pub use parser::{MouseMode, ScrollRegion, TerminalModes, VtHandler};

use vte::Parser;

use crate::parser::DcsState;

/// Frozen presentation state while synchronized output mode is active.
#[doc(hidden)]
#[derive(Clone)]
pub struct SyncPresentation {
    pub grid: Grid,
    pub cursor: Cursor,
    pub default_colors: TerminalDefaultColors,
    pub title: Option<String>,
}

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
    /// Dynamic default foreground/background/cursor set via OSC 10/11/12.
    default_colors: TerminalDefaultColors,
    /// vte parser state machine.
    parser: Parser,
    /// In-progress DCS payload, preserved across partial PTY chunks.
    dcs_state: Option<DcsState>,
    /// Frozen presentation while synchronized output mode is active.
    sync_present: Option<SyncPresentation>,
    /// Visible cell currently accepting zero-width/joined grapheme scalars.
    active_grapheme_cell: Option<(usize, usize)>,
    /// VT100 G0/G1 charset designations and active locking shift.
    charsets: TerminalCharsets,
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
            default_colors: TerminalDefaultColors::default(),
            parser: Parser::new(),
            dcs_state: None,
            sync_present: None,
            active_grapheme_cell: None,
            charsets: TerminalCharsets::default(),
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
        let _ = self.process_with_responses(bytes);
    }

    /// Process raw PTY output bytes and return terminal reply bytes.
    ///
    /// This is the request/response half of terminal emulation. Apps running
    /// under `TERM=xterm-256color` commonly emit DA/DSR/OSC/DCS probes and
    /// wait for the terminal to answer on stdin. Callers that own the PTY must
    /// write every returned response back to the child process.
    pub fn process_with_responses(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        // We need to create a VtHandler that borrows our fields mutably.
        // The vte Parser is taken out temporarily so we can pass both
        // the parser and the handler without conflicting borrows.
        let mut responses = Vec::new();
        let mut handler = VtHandler {
            grid: &mut self.grid,
            cursor: &mut self.cursor,
            modes: &mut self.modes,
            scroll_region: &mut self.scroll_region,
            title: &mut self.title,
            default_colors: &mut self.default_colors,
            alt_grid: &mut self.alt_grid,
            alt_cursor: &mut self.alt_cursor,
            dcs_state: &mut self.dcs_state,
            sync_present: &mut self.sync_present,
            active_grapheme_cell: &mut self.active_grapheme_cell,
            charsets: &mut self.charsets,
            responses: &mut responses,
        };
        self.parser.advance(&mut handler, bytes);
        responses
    }

    /// Access the current (active) grid.
    pub fn grid(&self) -> &Grid {
        self.sync_present
            .as_ref()
            .map(|present| &present.grid)
            .unwrap_or(&self.grid)
    }

    /// Access the cursor state.
    pub fn cursor(&self) -> &Cursor {
        self.sync_present
            .as_ref()
            .map(|present| &present.cursor)
            .unwrap_or(&self.cursor)
    }

    /// Access terminal modes.
    pub fn modes(&self) -> &TerminalModes {
        &self.modes
    }

    /// Get the window title (set by OSC 0/2).
    pub fn title(&self) -> Option<&str> {
        self.sync_present
            .as_ref()
            .and_then(|present| present.title.as_deref())
            .or(self.title.as_deref())
    }

    /// Dynamic default foreground/background/cursor set by OSC 10/11/12.
    pub fn default_colors(&self) -> TerminalDefaultColors {
        self.sync_present
            .as_ref()
            .map(|present| present.default_colors)
            .unwrap_or(self.default_colors)
    }

    /// Whether alternate screen is active.
    pub fn is_alternate_screen(&self) -> bool {
        self.modes.alternate_screen
    }

    /// Get the current scroll region.
    pub fn scroll_region(&self) -> &ScrollRegion {
        &self.scroll_region
    }

    /// Resize the virtual terminal.
    ///
    /// This resizes both primary and alternate grids, adjusts the scroll
    /// region, and clamps the cursor position.
    pub fn resize(&mut self, rows: usize, cols: usize) {
        self.active_grapheme_cell = None;
        if self.modes.alternate_screen {
            self.grid.resize_canvas(rows, cols);
            if let (Some(primary), Some(primary_cursor)) =
                (&mut self.alt_grid, &mut self.alt_cursor)
            {
                if let Some((row, col)) = primary.resize_with_cursor(
                    rows,
                    cols,
                    Some((primary_cursor.row, primary_cursor.col)),
                ) {
                    primary_cursor.row = row;
                    primary_cursor.col = col;
                }
                primary_cursor.clamp(rows, cols);
            }
        } else {
            if let Some((row, col)) =
                self.grid
                    .resize_with_cursor(rows, cols, Some((self.cursor.row, self.cursor.col)))
            {
                self.cursor.row = row;
                self.cursor.col = col;
            }
            if let Some(ref mut alt) = self.alt_grid {
                alt.resize_canvas(rows, cols);
            }
        }
        if let Some(ref mut present) = self.sync_present {
            if let Some((row, col)) = present.grid.resize_with_cursor(
                rows,
                cols,
                Some((present.cursor.row, present.cursor.col)),
            ) {
                present.cursor.row = row;
                present.cursor.col = col;
            }
        }
        self.rows = rows;
        self.cols = cols;
        self.scroll_region = ScrollRegion {
            top: 0,
            bottom: rows.saturating_sub(1),
        };
        self.cursor.clamp(rows, cols);
        if let Some(ref mut saved_cursor) = self.alt_cursor {
            saved_cursor.clamp(rows, cols);
        }
        if let Some(ref mut present) = self.sync_present {
            present.cursor.clamp(rows, cols);
        }
    }

    /// Switch to alternate screen buffer (DECSET 1049).
    pub fn enter_alternate_screen(&mut self) {
        self.active_grapheme_cell = None;
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
        self.active_grapheme_cell = None;
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

    /// Capture visible-viewport text, matching iTerm2 `get_screen_contents`.
    ///
    /// - `lines = None` → entire visible viewport, with trailing blank rows
    ///   trimmed (matches iTerm2's behaviour: get the whole screen, drop
    ///   the always-empty tail).
    /// - `lines = Some(N)` → up to N most-recent non-blank rows. Walks
    ///   back from the LAST non-blank row, picking that row and up to
    ///   N-1 preceding ones. Empty rows below the cursor are never
    ///   counted toward N.
    ///
    /// Returns `"\n"` when the viewport is entirely blank.
    pub fn capture_text(&self, lines: Option<usize>) -> String {
        let grid = self.grid();
        let total_rows = grid.rows();
        if total_rows == 0 {
            return String::new();
        }

        let last_content = (0..total_rows)
            .rev()
            .find(|&r| !grid.visible_row(r).is_blank())
            .unwrap_or(0);
        let end = last_content + 1;
        let start = match lines {
            Some(0) => return String::new(),
            Some(n) => end.saturating_sub(n),
            None => 0,
        };

        let mut output = String::new();
        for row_idx in start..end {
            let row = grid.visible_row(row_idx);
            let mut line = String::new();
            for cell in &row.cells {
                if cell.is_wide_continuation() {
                    continue;
                }
                cell.push_display_text(&mut line);
            }
            output.push_str(line.trim_end());
            output.push('\n');
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn compact_capture(vt: &VirtualTerminal) -> String {
        vt.capture_text(None).replace('\n', "")
    }

    fn assert_grid_wide_invariants(grid: &Grid) {
        for row_idx in 0..grid.total_lines() {
            let row = grid.row(row_idx).expect("row exists");
            for col in 0..row.len() {
                let cell = &row[col];
                if cell.is_wide_continuation() {
                    assert_eq!(
                        cell.ch, ' ',
                        "continuation at row {row_idx} col {col} carries glyph"
                    );
                    assert!(col > 0, "orphan continuation at row {row_idx} col 0");
                    assert!(
                        row[col - 1].is_wide(),
                        "orphan continuation at row {row_idx} col {col}"
                    );
                }
                if cell.is_wide() {
                    assert!(
                        col + 1 < row.len(),
                        "wide head at row {row_idx} final col {col}"
                    );
                    assert!(
                        row[col + 1].is_wide_continuation(),
                        "wide head at row {row_idx} col {col} missing tail"
                    );
                }
            }
        }
    }

    #[test]
    fn capture_text_skips_trailing_blank_rows_below_content() {
        // Regression: pane.capture {lines:N} used to take the literal
        // bottom-N visible rows, returning blank output when the cursor
        // sat near the top of a 24-row viewport. capture_text now walks
        // back from the LAST non-blank row.
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"first line\r\nsecond line\r\n");
        // Cursor is now on row 2; rows 3..23 are blank.
        let text = vt.capture_text(Some(1));
        assert_eq!(text.trim_end(), "second line");

        let text = vt.capture_text(Some(5));
        // Should pick up the two content lines, not blanks 19..23.
        assert!(text.contains("first line"), "first line missing: {text:?}");
        assert!(
            text.contains("second line"),
            "second line missing: {text:?}"
        );
    }

    #[test]
    fn capture_text_empty_viewport_returns_single_newline() {
        let vt = VirtualTerminal::new(24, 80);
        // Fresh VT has only blank rows.
        let text = vt.capture_text(Some(5));
        assert_eq!(
            text, "\n",
            "expected single newline for blank pane, got {text:?}"
        );
    }

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
        // CSI 5;10H -- move cursor to row 5, col 10.
        vt.process(b"\x1b[5;10H");
        assert_eq!(vt.cursor().row, 4); // 0-indexed
        assert_eq!(vt.cursor().col, 9); // 0-indexed
    }

    #[test]
    fn process_with_responses_answers_da_and_dsr_queries() {
        let mut vt = VirtualTerminal::new(24, 80);
        let responses = vt.process_with_responses(b"\x1b[5;10H\x1b[6n\x1b[5n\x1b[c\x1b[>c");

        assert_eq!(
            responses,
            vec![
                b"\x1b[5;10R".to_vec(),
                b"\x1b[0n".to_vec(),
                b"\x1b[?62;1;2;6;9;15;22c".to_vec(),
                b"\x1b[>0;95;0c".to_vec(),
            ]
        );
    }

    #[test]
    fn process_with_responses_answers_private_dsr() {
        let mut vt = VirtualTerminal::new(24, 80);
        let responses = vt.process_with_responses(b"\x1b[2;3H\x1b[?6n\x1b[?15n\x1b[?25n");

        assert_eq!(
            responses,
            vec![
                b"\x1b[?2;3R".to_vec(),
                b"\x1b[?10n".to_vec(),
                b"\x1b[?20n".to_vec(),
            ]
        );
    }

    #[test]
    fn process_with_responses_answers_osc_color_queries() {
        let mut vt = VirtualTerminal::new(24, 80);
        let responses = vt.process_with_responses(
            b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b]12;?\x1b\\\x1b]4;1;?\x1b\\",
        );

        assert_eq!(
            responses,
            vec![
                b"\x1b]10;rgb:eeee/eeee/eeee\x1b\\".to_vec(),
                b"\x1b]11;rgb:0000/0000/0000\x1b\\".to_vec(),
                b"\x1b]12;rgb:eeee/eeee/eeee\x1b\\".to_vec(),
                b"\x1b]4;1;rgb:cdcd/0000/0000\x1b\\".to_vec(),
            ]
        );
    }

    #[test]
    fn osc_12_query_falls_back_to_dynamic_foreground() {
        let mut vt = VirtualTerminal::new(24, 80);
        let responses = vt.process_with_responses(b"\x1b]10;#ff0000\x1b\\\x1b]12;?\x1b\\");

        assert_eq!(
            responses,
            vec![b"\x1b]12;rgb:ffff/0000/0000\x1b\\".to_vec()]
        );
    }

    #[test]
    fn process_with_responses_preserves_osc_bel_query_terminator() {
        let mut vt = VirtualTerminal::new(24, 80);
        let responses = vt.process_with_responses(b"\x1b]10;?\x07\x1b]4;2;?\x07");

        assert_eq!(
            responses,
            vec![
                b"\x1b]10;rgb:eeee/eeee/eeee\x07".to_vec(),
                b"\x1b]4;2;rgb:0000/cdcd/0000\x07".to_vec(),
            ]
        );
    }

    #[test]
    fn process_with_responses_uses_dynamic_default_colors_in_osc_queries() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b]10;#123456\x1b\\\x1b]11;rgb:1/2/3\x1b\\\x1b]12;#00ff80\x1b\\");
        let responses = vt.process_with_responses(b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b]12;?\x1b\\");
        vt.process(b"\x1b]110\x1b\\\x1b]111\x1b\\\x1b]112\x1b\\");
        let reset_responses =
            vt.process_with_responses(b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b]12;?\x1b\\");

        assert_eq!(
            responses,
            vec![
                b"\x1b]10;rgb:1212/3434/5656\x1b\\".to_vec(),
                b"\x1b]11;rgb:1111/2222/3333\x1b\\".to_vec(),
                b"\x1b]12;rgb:0000/ffff/8080\x1b\\".to_vec(),
            ]
        );
        assert_eq!(
            reset_responses,
            vec![
                b"\x1b]10;rgb:eeee/eeee/eeee\x1b\\".to_vec(),
                b"\x1b]11;rgb:0000/0000/0000\x1b\\".to_vec(),
                b"\x1b]12;rgb:eeee/eeee/eeee\x1b\\".to_vec(),
            ]
        );
    }

    #[test]
    fn process_with_responses_answers_extended_osc_palette_queries() {
        let mut vt = VirtualTerminal::new(24, 80);
        let responses = vt.process_with_responses(b"\x1b]4;16;?;231;?;232;?;255;?\x1b\\");

        assert_eq!(
            responses,
            vec![
                b"\x1b]4;16;rgb:0000/0000/0000\x1b\\".to_vec(),
                b"\x1b]4;231;rgb:ffff/ffff/ffff\x1b\\".to_vec(),
                b"\x1b]4;232;rgb:0808/0808/0808\x1b\\".to_vec(),
                b"\x1b]4;255;rgb:eeee/eeee/eeee\x1b\\".to_vec(),
            ]
        );
    }

    #[test]
    fn process_with_responses_answers_xtgettcap() {
        let mut vt = VirtualTerminal::new(24, 80);
        // Hex for TN;Co;RGB.
        let responses = vt.process_with_responses(b"\x1bP+q544e;436f;524742\x1b\\");

        assert_eq!(
            responses,
            vec![
                b"\x1bP1+r544e3d787465726d2d323536636f6c6f72;436f3d323536;5247423d\x1b\\".to_vec()
            ]
        );
    }

    #[test]
    fn process_with_responses_answers_extended_xtgettcap() {
        let mut vt = VirtualTerminal::new(24, 80);
        // Hex for AX;Ms;Ss;Se;smcup;rmcup;smkx;rmkx;setaf;setab;setrgbf;setrgbb.
        let responses = vt.process_with_responses(
            b"\x1bP+q4158;4d73;5373;5365;736d637570;726d637570;736d6b78;726d6b78;7365746166;7365746162;73657472676266;73657472676262\x1b\\",
        );

        let response = String::from_utf8(responses.into_iter().next().unwrap()).unwrap();
        assert!(response.starts_with("\x1bP1+r"));
        assert!(response.contains("41583d")); // AX=
        assert!(response.contains("4d733d1b5d35323b25703125733b257032257307"));
        assert!(response.contains("53733d1b5b25703125642071"));
        assert!(response.contains("53653d1b5b2071"));
        assert!(response.contains("736d6375703d1b5b3f3130343968"));
        assert!(response.contains("726d6375703d1b5b3f313034396c"));
        assert!(response.contains("736d6b783d1b5b3f31681b3d"));
        assert!(response.contains("726d6b783d1b5b3f316c1b3e"));
        assert!(response.contains("73657461663d"));
        assert!(response.contains("73657461623d"));
        assert!(response.contains("736574726762663d1b5b33383b323b"));
        assert!(response.contains("736574726762623d1b5b34383b323b"));
    }

    #[test]
    fn process_with_responses_reports_failed_dcs_queries() {
        let mut vt = VirtualTerminal::new(24, 80);
        let responses = vt.process_with_responses(
            b"\x1bP+q556e6b6e6f776e\x1b\\\x1bP$qunknown\x1b\\\x1bP!qignored\x1b\\",
        );

        assert_eq!(
            responses,
            vec![b"\x1bP0+r\x1b\\".to_vec(), b"\x1bP0$r\x1b\\".to_vec()]
        );
    }

    #[test]
    fn process_with_responses_answers_decrqm_for_modes() {
        let mut vt = VirtualTerminal::new(24, 80);
        let initial = vt.process_with_responses(
            b"\x1b[4$p\x1b[20$p\x1b[?1$p\x1b[?6$p\x1b[?7$p\x1b[?25$p\x1b[?66$p\x1b[?1000$p\x1b[?1002$p\x1b[?1003$p\x1b[?1004$p\x1b[?1006$p\x1b[?1049$p\x1b[?2004$p\x1b[?2026$p\x1b[?9999$p",
        );

        assert_eq!(
            initial,
            vec![
                b"\x1b[4;2$y".to_vec(),
                b"\x1b[20;2$y".to_vec(),
                b"\x1b[?1;2$y".to_vec(),
                b"\x1b[?6;2$y".to_vec(),
                b"\x1b[?7;1$y".to_vec(),
                b"\x1b[?25;1$y".to_vec(),
                b"\x1b[?66;2$y".to_vec(),
                b"\x1b[?1000;2$y".to_vec(),
                b"\x1b[?1002;2$y".to_vec(),
                b"\x1b[?1003;2$y".to_vec(),
                b"\x1b[?1004;2$y".to_vec(),
                b"\x1b[?1006;2$y".to_vec(),
                b"\x1b[?1049;2$y".to_vec(),
                b"\x1b[?2004;2$y".to_vec(),
                b"\x1b[?2026;2$y".to_vec(),
                b"\x1b[?9999;0$y".to_vec(),
            ]
        );

        vt.process(
            b"\x1b[4h\x1b[20h\x1b[?1h\x1b[?6h\x1b[?25l\x1b=\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1004h\x1b[?1006h\x1b[?1049h\x1b[?2004h\x1b[?2026h\x1b[?2026h",
        );
        let enabled = vt.process_with_responses(
            b"\x1b[4$p\x1b[20$p\x1b[?1$p\x1b[?6$p\x1b[?25$p\x1b[?66$p\x1b[?1000$p\x1b[?1002$p\x1b[?1003$p\x1b[?1004$p\x1b[?1006$p\x1b[?1049$p\x1b[?2004$p\x1b[?2026$p",
        );

        assert_eq!(
            enabled,
            vec![
                b"\x1b[4;1$y".to_vec(),
                b"\x1b[20;1$y".to_vec(),
                b"\x1b[?1;1$y".to_vec(),
                b"\x1b[?6;1$y".to_vec(),
                b"\x1b[?25;2$y".to_vec(),
                b"\x1b[?66;1$y".to_vec(),
                b"\x1b[?1000;2$y".to_vec(),
                b"\x1b[?1002;2$y".to_vec(),
                b"\x1b[?1003;1$y".to_vec(),
                b"\x1b[?1004;1$y".to_vec(),
                b"\x1b[?1006;1$y".to_vec(),
                b"\x1b[?1049;1$y".to_vec(),
                b"\x1b[?2004;1$y".to_vec(),
                b"\x1b[?2026;1$y".to_vec(),
            ]
        );
    }

    #[test]
    fn process_with_responses_answers_xtversion() {
        let mut vt = VirtualTerminal::new(24, 80);
        let responses = vt.process_with_responses(b"\x1b[>0q");

        assert_eq!(
            responses,
            vec![format!("\x1bP>|shux {}\x1b\\", env!("CARGO_PKG_VERSION")).into_bytes()]
        );
    }

    #[test]
    fn synchronized_output_freezes_presented_frame_until_reset() {
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"old");

        vt.process(b"\x1b[?2026h\x1b[1;1Hnew");

        assert!(vt.modes().synchronized_output);
        assert_eq!(vt.grid().visible_row(0)[0].ch, 'o');
        assert_eq!(vt.grid().visible_row(0)[1].ch, 'l');
        assert_eq!(vt.grid().visible_row(0)[2].ch, 'd');
        assert_eq!(vt.capture_text(Some(1)).trim_end(), "old");

        vt.process(b"\x1b[?2026l");

        assert!(!vt.modes().synchronized_output);
        assert_eq!(vt.grid().visible_row(0)[0].ch, 'n');
        assert_eq!(vt.grid().visible_row(0)[1].ch, 'e');
        assert_eq!(vt.grid().visible_row(0)[2].ch, 'w');
        assert_eq!(vt.capture_text(Some(1)).trim_end(), "new");
    }

    #[test]
    fn synchronized_output_freezes_presented_colors_and_title() {
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"\x1b]10;#112233\x1b\\\x1b]11;#000000\x1b\\\x1b]12;#eeeeee\x1b\\");
        vt.process(b"\x1b]2;stable\x1b\\old");

        vt.process(
            b"\x1b[?2026h\x1b]10;#ff0000\x1b\\\x1b]11;#00ff00\x1b\\\
              \x1b]12;#0000ff\x1b\\\x1b]2;pending\x1b\\\x1b[1;1Hnew",
        );

        assert!(vt.modes().synchronized_output);
        assert_eq!(
            vt.default_colors(),
            TerminalDefaultColors {
                fg: Some([0x11, 0x22, 0x33]),
                bg: Some([0x00, 0x00, 0x00]),
                cursor: Some([0xee, 0xee, 0xee]),
            }
        );
        assert_eq!(vt.title(), Some("stable"));
        assert_eq!(vt.capture_text(Some(1)).trim_end(), "old");

        vt.process(b"\x1b[?2026l");

        assert!(!vt.modes().synchronized_output);
        assert_eq!(
            vt.default_colors(),
            TerminalDefaultColors {
                fg: Some([0xff, 0x00, 0x00]),
                bg: Some([0x00, 0xff, 0x00]),
                cursor: Some([0x00, 0x00, 0xff]),
            }
        );
        assert_eq!(vt.title(), Some("pending"));
        assert_eq!(vt.capture_text(Some(1)).trim_end(), "new");
    }

    #[test]
    fn synchronized_output_preserves_presented_scrollback() {
        let mut vt = VirtualTerminal::new(2, 10);
        vt.process(b"first\r\nsecond\r\nthird");
        let presented_total = vt.grid().total_lines();
        let presented_scrollback = vt.grid().scrollback_len();

        vt.process(b"\x1b[?2026h\x1b[1;1Hpending\r\nwork  ");

        assert!(vt.modes().synchronized_output);
        assert_eq!(vt.grid().total_lines(), presented_total);
        assert_eq!(vt.grid().scrollback_len(), presented_scrollback);
        assert!(
            vt.grid()
                .scrollback_row(0)
                .expect("scrollback row should remain visible")
                .cells
                .iter()
                .map(|cell| cell.ch)
                .collect::<String>()
                .contains("first")
        );
        assert_eq!(vt.capture_text(Some(1)).trim_end(), "third");

        vt.process(b"\x1b[?2026l");

        assert!(!vt.modes().synchronized_output);
        assert_eq!(vt.capture_text(Some(1)).trim_end(), "work");
    }

    #[test]
    fn synchronized_output_resize_keeps_presented_dimensions_valid() {
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"stable\x1b[?2026h\x1b[1;1Hpending");

        vt.resize(5, 12);

        assert_eq!(vt.grid().rows(), 5);
        assert_eq!(vt.grid().cols(), 12);
        assert_eq!(vt.grid().visible_row(0)[0].ch, 's');

        vt.process(b"\x1b[?2026l");

        assert_eq!(vt.grid().rows(), 5);
        assert_eq!(vt.grid().cols(), 12);
        assert_eq!(vt.grid().visible_row(0)[0].ch, 'p');
    }

    #[test]
    fn synchronized_output_resize_reflows_presented_frame() {
        let mut vt = VirtualTerminal::new(4, 5);
        vt.process(b"ABCDEFGHIJK");
        vt.process(b"\x1b[?2026h\x1b[1;1Hpending");

        vt.resize(5, 4);

        assert!(vt.modes().synchronized_output);
        assert_eq!(compact_capture(&vt), "ABCDEFGHIJK");
        vt.process(b"\x1b[?2026l");
        assert!(compact_capture(&vt).starts_with("pending"));
    }

    #[test]
    fn process_with_responses_reports_active_sgr_in_decrqss() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[1;2;3;4;5;7;8;9;38;2;1;2;3;48;5;196m");
        let responses = vt.process_with_responses(b"\x1bP$qm\x1b\\");

        assert_eq!(
            responses,
            vec![b"\x1bP1$r1;2;3;4;5;7;8;9;38;2;1;2;3;48;5;196m\x1b\\".to_vec()]
        );
    }

    #[test]
    fn process_with_responses_reports_advanced_underline_sgr_in_decrqss() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[4:3;58:2::10:20:30m");
        let responses = vt.process_with_responses(b"\x1bP$qm\x1b\\");

        assert_eq!(
            responses,
            vec![b"\x1bP1$r4:3;58;2;10;20;30m\x1b\\".to_vec()]
        );
    }

    #[test]
    fn process_with_responses_reports_indexed_sgr_in_decrqss() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[91;44m");
        let responses = vt.process_with_responses(b"\x1bP$qm\x1b\\");

        assert_eq!(responses, vec![b"\x1bP1$r91;44m\x1b\\".to_vec()]);
    }

    #[test]
    fn process_with_responses_answers_decrqss() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[2;4r\x1b[5 q");
        let responses = vt.process_with_responses(b"\x1bP$qm\x1b\\\x1bP$qr\x1b\\\x1bP$q q\x1b\\");

        assert_eq!(
            responses,
            vec![
                b"\x1bP1$r0m\x1b\\".to_vec(),
                b"\x1bP1$r2;4r\x1b\\".to_vec(),
                b"\x1bP1$r5 q\x1b\\".to_vec(),
            ]
        );
    }

    #[test]
    fn test_cursor_up_down() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[10;1H"); // Go to row 10.
        vt.process(b"\x1b[3A"); // Move up 3.
        assert_eq!(vt.cursor().row, 6);
        vt.process(b"\x1b[2B"); // Move down 2.
        assert_eq!(vt.cursor().row, 8);
    }

    #[test]
    fn test_cursor_forward_backward() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[1;20H"); // Go to col 20.
        vt.process(b"\x1b[5C"); // Forward 5.
        assert_eq!(vt.cursor().col, 24);
        vt.process(b"\x1b[10D"); // Backward 10.
        assert_eq!(vt.cursor().col, 14);
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
    fn truecolor_mid_row_sgr_preserves_unicode_box_cells() {
        let mut vt = VirtualTerminal::new(3, 40);
        vt.process(
            "\x1b[38;2;31;122;120m│\x1b[m \
             \x1b[38;2;240;190;101m2E 7W 8I\x1b[m \
             \x1b[38;2;31;122;120m│\x1b[m"
                .as_bytes(),
        );

        let row = vt.grid().visible_row(0);
        assert_eq!(row[0].ch, '│');
        assert_eq!(row[0].style.fg, Color::Rgb(31, 122, 120));
        assert_eq!(row[11].ch, '│');
        assert_eq!(row[11].style.fg, Color::Rgb(31, 122, 120));
    }

    #[test]
    fn erase_in_line_clears_shorter_diff_redraw_content() {
        let mut vt = VirtualTerminal::new(3, 40);
        vt.process(b"\x1b[?1049h");
        vt.process(b"\x1b[2;1Hhad 1. Fix blockers");
        vt.process(b"\x1b[2;1H\x1b[0K1. Fix blockers");

        let row = vt.grid().visible_row(1);
        let rendered: String = row.cells.iter().map(|cell| cell.ch).collect();
        assert!(rendered.starts_with("1. Fix blockers"));
        assert!(
            !rendered.contains("had "),
            "stale prior-frame content remained: {rendered:?}"
        );
    }

    #[test]
    fn csi_save_restore_keeps_diff_redraw_at_saved_cursor() {
        let mut vt = VirtualTerminal::new(3, 40);
        vt.process(b"\x1b[?1049h");
        vt.process(b"\x1b[2;1H\x1b[s");
        vt.process(b"had 1. Fix blockers");
        vt.process(b"\x1b[u\x1b[0K1. Fix blockers");

        let row = vt.grid().visible_row(1);
        let rendered: String = row.cells.iter().map(|cell| cell.ch).collect();
        assert!(rendered.starts_with("1. Fix blockers"));
        assert!(
            !rendered.contains("had "),
            "CSI u did not restore before erase/redraw: {rendered:?}"
        );
    }

    #[test]
    fn bubbletea_style_sync_redraw_preserves_box_and_clears_stale_text() {
        let mut vt = VirtualTerminal::new(4, 40);
        vt.process(b"\x1b[?1049h\x1b[?2026h");
        vt.process(
            "\x1b[2;1H\x1b[s\
             \x1b[38;2;31;122;120m│\x1b[m \
             \x1b[38;2;166;173;200mhad\x1b[m \
             \x1b[38;2;240;190;101m1.\x1b[m \
             \x1b[38;2;137;180;250mFix blockers\x1b[m \
             \x1b[38;2;31;122;120m│\x1b[m"
                .as_bytes(),
        );
        vt.process(
            "\x1b[u\x1b[0K\
             \x1b[38;2;31;122;120m│\x1b[m \
             \x1b[38;2;240;190;101m1.\x1b[m \
             \x1b[38;2;137;180;250mFix blockers\x1b[m \
             \x1b[38;2;31;122;120m│\x1b[m\
             \x1b[?2026l"
                .as_bytes(),
        );

        let row = vt.grid().visible_row(1);
        let rendered: String = row.cells.iter().map(|cell| cell.ch).collect();
        assert!(rendered.starts_with("│ 1. Fix blockers │"));
        assert!(
            !rendered.contains("had "),
            "stale prior-frame content remained: {rendered:?}"
        );
        assert_eq!(row[0].ch, '│');
        assert_eq!(row[18].ch, '│');
        assert_eq!(row[0].style.fg, Color::Rgb(31, 122, 120));
        assert_eq!(row[2].style.fg, Color::Rgb(240, 190, 101));
        assert_eq!(row[5].style.fg, Color::Rgb(137, 180, 250));
        assert_eq!(row[18].style.fg, Color::Rgb(31, 122, 120));
    }

    #[test]
    fn parameterized_csi_u_is_not_cursor_restore() {
        let mut vt = VirtualTerminal::new(3, 40);
        vt.process(b"\x1b[1;1H\x1b[s");
        vt.process(b"\x1b[2;2H\x1b[27;2u");

        assert_eq!((vt.cursor().row, vt.cursor().col), (1, 1));
    }

    #[test]
    fn private_mode_1048_saves_and_restores_cursor() {
        let mut vt = VirtualTerminal::new(3, 40);
        vt.process(b"\x1b[2;5H\x1b[?1048h");
        vt.process(b"\x1b[31m");
        vt.process(b"\x1b[3;20H");
        assert_eq!((vt.cursor().row, vt.cursor().col), (2, 19));

        vt.process(b"\x1b[?1048l");

        assert_eq!((vt.cursor().row, vt.cursor().col), (1, 4));
        assert_eq!(vt.cursor().style.fg, Color::Default);
    }

    #[test]
    fn repeated_alt_screen_enter_does_not_discard_primary_grid() {
        let mut vt = VirtualTerminal::new(3, 40);
        vt.process(b"primary");
        vt.process(b"\x1b[?1049h");
        vt.process(b"alt");
        vt.process(b"\x1b[?1049h");
        assert_eq!(
            vt.grid().visible_row(0)[0].ch,
            ' ',
            "re-entering 1049 should clear the active alternate grid"
        );
        vt.process(b"\x1b[?1049l");

        let row = vt.grid().visible_row(0);
        let rendered: String = row.cells.iter().map(|cell| cell.ch).collect();
        assert!(rendered.starts_with("primary"));
        assert!(!vt.is_alternate_screen());
    }

    #[test]
    fn mode_1047_does_not_restore_primary_cursor_on_leave() {
        let mut vt = VirtualTerminal::new(3, 40);
        vt.process(b"\x1b[2;5H");
        vt.process(b"\x1b[?1047h");
        vt.process(b"\x1b[3;20H");
        vt.process(b"\x1b[?1047l");

        assert_eq!((vt.cursor().row, vt.cursor().col), (2, 19));
        assert!(!vt.is_alternate_screen());
    }

    #[test]
    fn mode_1049_leave_restores_saved_cursor_even_when_primary_active() {
        let mut vt = VirtualTerminal::new(3, 40);
        vt.process(b"\x1b[2;5H\x1b[?1048h");
        vt.process(b"\x1b[3;20H");
        vt.process(b"\x1b[?1049l");

        assert_eq!((vt.cursor().row, vt.cursor().col), (1, 4));
        assert!(!vt.is_alternate_screen());
    }

    #[test]
    fn nested_1049_enter_preserves_saved_cursor_while_already_alt_screen() {
        let mut vt = VirtualTerminal::new(3, 40);
        vt.process(b"\x1b[?1047h\x1b[2;5H");
        vt.process(b"\x1b[?1049h");
        vt.process(b"\x1b[3;20Hnested");
        vt.process(b"\x1b[?1049l");

        assert_eq!((vt.cursor().row, vt.cursor().col), (1, 4));
    }

    #[test]
    fn repeat_preceding_space_clears_stale_prefix_before_short_redraw() {
        let mut vt = VirtualTerminal::new(3, 40);
        vt.process(b"\x1b[?1049h");
        vt.process(b"\x1b[2;1Hhad 1. Fix blockers");
        vt.process(b"\x1b[2;1H \x1b[3b1. Fix blockers");

        let row = vt.grid().visible_row(1);
        let rendered: String = row.cells.iter().map(|cell| cell.ch).collect();
        assert!(rendered.starts_with("    1. Fix blockers"));
        assert!(
            !rendered.contains("had "),
            "REP failed to clear stale prefix: {rendered:?}"
        );
    }

    #[test]
    fn rep_after_last_column_repeats_pending_wrap_character() {
        let mut vt = VirtualTerminal::new(3, 2);
        vt.process(b"\x1b[2GA\x1b[2b");

        assert_eq!(vt.grid().visible_row(0)[1].ch, 'A');
        assert_eq!(vt.grid().visible_row(1)[0].ch, 'A');
        assert_eq!(vt.grid().visible_row(1)[1].ch, 'A');
    }

    #[test]
    fn hpa_before_erase_clears_stale_scan_text_before_summary_redraw() {
        let mut vt = VirtualTerminal::new(3, 80);
        vt.process(b"\x1b[?1049h");
        vt.process(b"\x1b[2;1Hhadolint x");
        vt.process(b"\x1b[2;40H\x1b[1`\x1b[K    1. Fix blockers");

        let row = vt.grid().visible_row(1);
        let rendered: String = row.cells.iter().map(|cell| cell.ch).collect();
        assert!(rendered.starts_with("    1. Fix blockers"));
        assert!(
            !rendered.contains("hadolint"),
            "HPA before EL failed to clear stale scan text: {rendered:?}"
        );
    }

    #[test]
    fn renderer_cursor_tabulation_and_relative_moves_are_applied() {
        let mut vt = VirtualTerminal::new(4, 40);
        vt.process(b"A\x1b[2IC");
        assert_eq!(vt.cursor().col, 17);
        vt.process(b"\x1b[1ZX");
        assert_eq!(vt.grid().visible_row(0)[16].ch, 'X');
        vt.process(b"\x1b[2aY");
        assert_eq!(vt.grid().visible_row(0)[19].ch, 'Y');
        vt.process(b"\x1b[1eZ");
        assert_eq!(vt.grid().visible_row(1)[20].ch, 'Z');
        vt.process(b"\x1b[5`H");
        assert_eq!(vt.grid().visible_row(1)[4].ch, 'H');
    }

    #[test]
    fn renderer_scroll_up_down_primitives_shift_scroll_region() {
        let mut vt = VirtualTerminal::new(5, 10);
        vt.process(b"\x1b[1;1HA\x1b[2;1HB\x1b[3;1HC\x1b[4;1HD\x1b[5;1HE");
        vt.process(b"\x1b[2;4r");
        vt.process(b"\x1b[1S");

        assert_eq!(vt.grid().visible_row(0)[0].ch, 'A');
        assert_eq!(vt.grid().visible_row(1)[0].ch, 'C');
        assert_eq!(vt.grid().visible_row(2)[0].ch, 'D');
        assert_eq!(vt.grid().visible_row(3)[0].ch, ' ');
        assert_eq!(vt.grid().visible_row(4)[0].ch, 'E');

        vt.process(b"\x1b[1T");
        assert_eq!(vt.grid().visible_row(0)[0].ch, 'A');
        assert_eq!(vt.grid().visible_row(1)[0].ch, ' ');
        assert_eq!(vt.grid().visible_row(2)[0].ch, 'C');
        assert_eq!(vt.grid().visible_row(3)[0].ch, 'D');
        assert_eq!(vt.grid().visible_row(4)[0].ch, 'E');
    }

    #[test]
    fn advanced_underline_sgr_sets_extended_cell_attributes() {
        let mut vt = VirtualTerminal::new(2, 40);
        vt.process(b"\x1b[4:3mC\x1b[58:2::10:20:30mU\x1b[59mN\x1b[24mP");

        let row = vt.grid().visible_row(0);
        let curly = row[0].extended.as_ref().expect("curly underline");
        assert_eq!(curly.underline_style, UnderlineStyle::Curly);
        assert!(row[0].style.flags.contains(CellFlags::UNDERLINE));

        let colored = row[1].extended.as_ref().expect("underline color");
        assert_eq!(colored.underline_style, UnderlineStyle::Curly);
        assert_eq!(colored.underline_color, Some(Color::Rgb(10, 20, 30)));

        let no_color = row[2].extended.as_ref().expect("underline style remains");
        assert_eq!(no_color.underline_style, UnderlineStyle::Curly);
        assert_eq!(no_color.underline_color, None);
        assert!(row[3].extended.is_none());
        assert!(!row[3].style.flags.contains(CellFlags::UNDERLINE));
    }

    #[test]
    fn osc8_hyperlink_applies_to_subsequent_cells_until_cleared() {
        let mut vt = VirtualTerminal::new(2, 40);
        vt.process(b"\x1b]8;;https://example.invalid/a;b\x07L\x1b]8;;\x07N");

        let linked = vt.grid().visible_row(0)[0]
            .extended
            .as_ref()
            .expect("hyperlink");
        assert_eq!(
            linked.hyperlink.as_deref(),
            Some("https://example.invalid/a;b")
        );
        assert!(vt.grid().visible_row(0)[1].extended.is_none());
    }

    #[test]
    fn grapheme_combining_mark_is_stored_and_captured() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process("e\u{0301}x".as_bytes());

        let row = vt.grid().visible_row(0);
        assert_eq!(row[0].ch, 'e');
        assert_eq!(row[0].grapheme(), Some("e\u{0301}"));
        assert_eq!(row[1].ch, 'x');
        assert!(row[1].grapheme().is_none());
        assert_eq!(vt.capture_text(Some(1)).trim_end(), "e\u{0301}x");
    }

    #[test]
    fn grapheme_variation_modifier_zwj_and_flag_payloads_are_preserved() {
        let mut vt = VirtualTerminal::new(3, 40);
        vt.process("A🛠\u{fe0f} B👍🏽 C👨\u{200d}💻 D🇺🇸".as_bytes());

        let captured = vt.capture_text(Some(1));
        assert!(
            captured.contains("🛠\u{fe0f}"),
            "VS16 payload missing: {captured:?}"
        );
        assert!(
            captured.contains("👍🏽"),
            "skin-tone payload missing: {captured:?}"
        );
        assert!(
            captured.contains("👨\u{200d}💻"),
            "ZWJ payload missing: {captured:?}"
        );
        assert!(
            captured.contains("🇺🇸"),
            "flag payload missing: {captured:?}"
        );
        assert!(
            vt.grid()
                .visible_row(0)
                .cells
                .iter()
                .any(|cell| cell.grapheme() == Some("👨\u{200d}💻"))
        );
        assert!(
            vt.grid()
                .visible_row(0)
                .cells
                .iter()
                .any(|cell| cell.grapheme() == Some("🇺🇸") && cell.width == 2)
        );
        assert_grid_wide_invariants(vt.grid());
    }

    #[test]
    fn ascii_cells_remain_on_compact_common_path() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process(b"plain ascii");

        for cell in vt.grid().visible_row(0).cells.iter().take(11) {
            assert!(cell.extended.is_none(), "ASCII cell used extended attrs");
            assert!(
                cell.grapheme().is_none(),
                "ASCII cell used grapheme payload"
            );
        }
    }

    #[test]
    fn grapheme_payload_is_cell_local_not_cursor_style() {
        let mut vt = VirtualTerminal::new(2, 40);
        vt.process(b"\x1b]8;;https://example.invalid/a;b\x07");
        vt.process("e\u{0301}x".as_bytes());

        let row = vt.grid().visible_row(0);
        let first = row[0].extended.as_ref().expect("first cell extended attrs");
        assert_eq!(first.grapheme.as_deref(), Some("e\u{0301}"));
        assert_eq!(
            first.hyperlink.as_deref(),
            Some("https://example.invalid/a;b")
        );

        let second = row[1].extended.as_ref().expect("second cell hyperlink");
        assert_eq!(
            second.hyperlink.as_deref(),
            Some("https://example.invalid/a;b")
        );
        assert_eq!(
            second.grapheme, None,
            "grapheme payload leaked through cursor extended attrs"
        );
    }

    #[test]
    fn styled_ascii_cells_share_cursor_extended_attrs() {
        let mut vt = VirtualTerminal::new(2, 40);
        vt.process(b"\x1b]8;;https://example.invalid/a;b\x07");
        vt.process(b"ab");
        vt.process("e\u{0301}".as_bytes());

        let row = vt.grid().visible_row(0);
        let first = row[0].extended.as_ref().expect("first hyperlink");
        let second = row[1].extended.as_ref().expect("second hyperlink");
        let grapheme = row[2].extended.as_ref().expect("grapheme hyperlink");

        assert!(
            Arc::ptr_eq(first, second),
            "plain styled run should share cursor extended attrs"
        );
        assert!(
            !Arc::ptr_eq(second, grapheme),
            "grapheme payload should copy-on-write cell attrs"
        );
        assert_eq!(first.grapheme, None);
        assert_eq!(second.grapheme, None);
        assert_eq!(grapheme.grapheme.as_deref(), Some("e\u{0301}"));
        assert_eq!(
            grapheme.hyperlink.as_deref(),
            Some("https://example.invalid/a;b")
        );
    }

    #[test]
    fn combining_after_cursor_motion_does_not_attach_to_stale_cell() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process("e\x1b[1;10H\u{0301}x".as_bytes());

        let row = vt.grid().visible_row(0);
        assert_eq!(row[0].ch, 'e');
        assert!(row[0].grapheme().is_none());
        assert_eq!(row[9].ch, 'x');
    }

    #[test]
    fn combining_after_esc_line_movement_does_not_attach_to_stale_cell() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process("e\x1bE\u{0301}x".as_bytes());

        let row0 = vt.grid().visible_row(0);
        let row1 = vt.grid().visible_row(1);
        assert_eq!(row0[0].ch, 'e');
        assert!(row0[0].grapheme().is_none());
        assert_eq!(row1[0].ch, 'x');
    }

    #[test]
    fn combining_after_final_column_preserves_pending_wrap() {
        let mut vt = VirtualTerminal::new(2, 3);
        vt.process("abe\u{0301}x".as_bytes());

        assert_eq!(vt.grid().visible_row(0)[2].grapheme(), Some("e\u{0301}"));
        assert_eq!(vt.grid().visible_row(1)[0].ch, 'x');
        assert_eq!(vt.capture_text(None), "abe\u{0301}\nx\n");
    }

    #[test]
    fn combining_after_cjk_attaches_to_wide_head() {
        let mut vt = VirtualTerminal::new(2, 10);
        vt.process("界\u{0301}x".as_bytes());

        let row = vt.grid().visible_row(0);
        assert_eq!(row[0].grapheme(), Some("界\u{0301}"));
        assert!(row[1].is_wide_continuation());
        assert_eq!(row[2].ch, 'x');
        assert_grid_wide_invariants(vt.grid());
    }

    #[test]
    fn zwj_width_expansion_preserves_wide_invariants() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process("a\u{200d}👨x".as_bytes());

        let row = vt.grid().visible_row(0);
        assert_eq!(row[0].grapheme(), Some("a\u{200d}👨"));
        assert!(row[0].is_wide());
        assert!(row[1].is_wide_continuation());
        assert_eq!(row[2].ch, 'x');
        assert_grid_wide_invariants(vt.grid());
    }

    #[test]
    fn zwj_and_flag_at_final_column_do_not_create_final_wide_head() {
        let mut zwj = VirtualTerminal::new(2, 5);
        zwj.process(b"\x1b[1;5H");
        zwj.process("a\u{200d}👨".as_bytes());
        assert!(
            !zwj.grid().visible_row(0)[4].is_wide(),
            "ZWJ sequence created a width-2 head in the final column"
        );
        assert!(
            zwj.capture_text(None).contains("a\u{200d}"),
            "final-column ZWJ marker was lost"
        );
        assert_grid_wide_invariants(zwj.grid());

        let mut flag = VirtualTerminal::new(2, 5);
        flag.process(b"\x1b[1;5H");
        flag.process("🇺🇸".as_bytes());
        assert!(
            !flag.grid().visible_row(0)[4].is_wide(),
            "flag pair created a width-2 head in the final column"
        );
        assert!(
            flag.capture_text(None).contains("🇺") && flag.capture_text(None).contains("🇸"),
            "final-column regional indicators were lost"
        );
        assert_grid_wide_invariants(flag.grid());
    }

    #[test]
    fn rep_repeats_full_grapheme_payload() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process("e\u{0301}\x1b[3b".as_bytes());

        assert_eq!(
            vt.capture_text(Some(1)).trim_end(),
            "e\u{0301}e\u{0301}e\u{0301}e\u{0301}"
        );
    }

    #[test]
    fn rep_preserves_width_expanded_grapheme_payload() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process("a\u{200d}👨\x1b[2bZ".as_bytes());

        let row = vt.grid().visible_row(0);
        for col in [0, 2, 4] {
            assert_eq!(row[col].grapheme(), Some("a\u{200d}👨"));
            assert!(row[col].is_wide(), "cell {col} lost width-2 head");
            assert!(
                row[col + 1].is_wide_continuation(),
                "cell {} lost continuation",
                col + 1
            );
        }
        assert_eq!(row[6].ch, 'Z');
        assert_eq!(
            vt.capture_text(Some(1)).trim_end(),
            "a\u{200d}👨a\u{200d}👨a\u{200d}👨Z"
        );
        assert_grid_wide_invariants(vt.grid());
    }

    #[test]
    fn dec_special_graphics_maps_g0_line_drawing() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process(b"\x1b(0lqkxmj\x1b(B ascii");

        assert_eq!(vt.capture_text(Some(1)).trim_end(), "┌─┐│└┘ ascii");
    }

    #[test]
    fn dec_special_graphics_maps_complete_standard_set() {
        let mut vt = VirtualTerminal::new(2, 80);
        vt.process(b"\x1b(0_`abcdefghijklmnopqrstuvwxyz{|}~");

        assert_eq!(
            vt.capture_text(Some(1)).trim_end(),
            " ◆▒␉␌␍␊°±␤␋┘┐┌└┼⎺⎻─⎼⎽├┤┴┬│≤≥π≠£·"
        );
    }

    #[test]
    fn dec_special_graphics_state_persists_across_process_chunks() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process(b"\x1b(0");
        vt.process(b"lqk");

        assert_eq!(vt.capture_text(Some(1)).trim_end(), "┌─┐");
    }

    #[test]
    fn dec_special_graphics_so_si_switches_g1_without_leaking() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process(b"\x1b)0A\x0elqk\x0fZ");

        assert_eq!(vt.capture_text(Some(1)).trim_end(), "A┌─┐Z");
    }

    #[test]
    fn dec_special_graphics_dynamic_redesignation_updates_active_slot() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process(b"\x1b)0\x0eq\x1b)Bq");

        assert_eq!(vt.capture_text(Some(1)).trim_end(), "─q");
    }

    #[test]
    fn dec_special_graphics_invalid_designation_falls_back_to_ascii() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process(b"\x1b(0q\x1b(Xq");

        assert_eq!(vt.capture_text(Some(1)).trim_end(), "─q");
    }

    #[test]
    fn dec_special_graphics_rep_repeats_translated_cell() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process(b"\x1b(0q\x1b[3b");

        assert_eq!(vt.capture_text(Some(1)).trim_end(), "────");
    }

    #[test]
    fn dec_special_graphics_ris_resets_charset_state() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process(b"\x1b(0q\x1bcq");

        assert_eq!(vt.capture_text(Some(1)).trim_end(), "q");
    }

    #[test]
    fn dec_special_graphics_decsc_decrc_restore_charset_state() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process(b"\x1b[3C\x1b(0\x1b7\x1b(Ba\x1b8q");

        let row = vt.grid().visible_row(0);
        assert_eq!(row[3].ch, '─');
        assert_eq!(vt.capture_text(Some(1)).trim_end(), "   ─");
    }

    #[test]
    fn dec_special_graphics_1049_restore_keeps_primary_saved_charset() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process(b"\x1b(0\x1b[?1049h\x1b(B\x1b7\x1b(0q\x1b8\x1b[?1049lq");

        assert_eq!(vt.capture_text(Some(1)).trim_end(), "─");
    }

    #[test]
    fn dec_special_graphics_does_not_translate_or_narrow_unicode_wide_cells() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process(" \x1b(0你q".as_bytes());

        let row = vt.grid().visible_row(0);
        assert_eq!(row[1].ch, '你');
        assert!(row[1].is_wide());
        assert!(row[2].is_wide_continuation());
        assert_eq!(row[3].ch, '─');
        assert_grid_wide_invariants(vt.grid());
    }

    #[test]
    fn test_sgr_256_color() {
        let mut vt = VirtualTerminal::new(24, 80);
        // Set 256-color foreground: SGR 38;5;196.
        vt.process(b"\x1b[38;5;196mX");
        let cell = &vt.grid().visible_row(0)[0];
        assert_eq!(cell.style.fg, Color::Indexed(196));
    }

    #[test]
    fn test_sgr_background_colors() {
        let mut vt = VirtualTerminal::new(24, 80);
        // 24-bit background.
        vt.process(b"\x1b[48;2;10;20;30mX");
        let cell = &vt.grid().visible_row(0)[0];
        assert_eq!(cell.style.bg, Color::Rgb(10, 20, 30));
    }

    #[test]
    fn test_sgr_bright_colors() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[91mX"); // Bright red fg.
        let cell = &vt.grid().visible_row(0)[0];
        assert_eq!(cell.style.fg, Color::Indexed(9)); // 8 + 1 = bright red
    }

    #[test]
    fn test_sgr_multiple_attributes() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[1;3;31mX"); // Bold + Italic + Red.
        let cell = &vt.grid().visible_row(0)[0];
        assert!(cell.style.flags.contains(CellFlags::BOLD));
        assert!(cell.style.flags.contains(CellFlags::ITALIC));
        assert_eq!(cell.style.fg, Color::Indexed(1));
    }

    #[test]
    fn test_erase_in_display() {
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"AAAAAAAAAA\r\nBBBBBBBBBB\r\nCCCCCCCCCC");
        // Move to row 2, col 1 and clear above (ED 1).
        vt.process(b"\x1b[2;1H\x1b[1J");
        // Row 0 should be cleared.
        assert_eq!(vt.grid().visible_row(0)[0].ch, ' ');
    }

    #[test]
    fn test_erase_in_line() {
        let mut vt = VirtualTerminal::new(1, 10);
        vt.process(b"ABCDEFGHIJ");
        vt.process(b"\x1b[1;6H"); // Move to col 5.
        vt.process(b"\x1b[0K"); // Erase from cursor to end.
        assert_eq!(vt.grid().visible_row(0)[4].ch, 'E');
        assert_eq!(vt.grid().visible_row(0)[5].ch, ' ');
        assert_eq!(vt.grid().visible_row(0)[9].ch, ' ');
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
    fn test_auto_wrap_scrolls_at_bottom() {
        let mut vt = VirtualTerminal::new(3, 5);
        // Fill all 3 rows.
        vt.process(b"ABCDE");
        vt.process(b"FGHIJ");
        vt.process(b"KLMNO");
        // Now one more character should cause scroll.
        vt.process(b"P");
        assert_eq!(vt.grid().scrollback_len(), 1);
        assert_eq!(vt.grid().visible_row(2)[0].ch, 'P');
    }

    #[test]
    fn test_alternate_screen() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"primary content");
        assert!(!vt.is_alternate_screen());
        // DECSET 1049 -- enter alternate screen.
        vt.process(b"\x1b[?1049h");
        assert!(vt.is_alternate_screen());
        vt.process(b"alt content");
        // DECRST 1049 -- leave alternate screen.
        vt.process(b"\x1b[?1049l");
        assert!(!vt.is_alternate_screen());
        assert_eq!(vt.grid().visible_row(0)[0].ch, 'p'); // primary content restored
    }

    #[test]
    fn test_alternate_screen_via_api() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"primary");
        vt.enter_alternate_screen();
        assert!(vt.is_alternate_screen());
        vt.process(b"alt");
        assert_eq!(vt.grid().visible_row(0)[0].ch, 'a');
        vt.leave_alternate_screen();
        assert!(!vt.is_alternate_screen());
        assert_eq!(vt.grid().visible_row(0)[0].ch, 'p');
    }

    #[test]
    fn test_osc_title() {
        let mut vt = VirtualTerminal::new(24, 80);
        // OSC 2 -- set window title.
        vt.process(b"\x1b]2;my window title\x07");
        assert_eq!(vt.title(), Some("my window title"));
    }

    #[test]
    fn test_osc_title_icon_name() {
        let mut vt = VirtualTerminal::new(24, 80);
        // OSC 0 -- set icon name and window title.
        vt.process(b"\x1b]0;icon and title\x07");
        assert_eq!(vt.title(), Some("icon and title"));
    }

    #[test]
    fn test_wide_character() {
        let mut vt = VirtualTerminal::new(24, 80);
        // Write a wide character (CJK character, width 2).
        vt.process("\u{4f60}".as_bytes()); // Unicode for a CJK char
        assert_eq!(vt.grid().visible_row(0)[0].ch, '\u{4f60}');
        assert_eq!(vt.grid().visible_row(0)[0].width, 2);
        assert!(vt.grid().visible_row(0)[1].is_wide_continuation());
        assert_eq!(vt.cursor().col, 2);
    }

    #[test]
    fn wide_overwrite_on_continuation_clears_both_old_pairs() {
        let mut vt = VirtualTerminal::new(2, 8);
        vt.process("你你".as_bytes());
        vt.process("\x1b[1;2H好".as_bytes());

        let row = vt.grid().visible_row(0);
        assert_eq!(row[0].ch, ' ');
        assert_eq!(row[1].ch, '好');
        assert!(row[2].is_wide_continuation());
        assert_eq!(row[3].ch, ' ');
        assert_grid_wide_invariants(vt.grid());
        assert_eq!(compact_capture(&vt), " 好");
    }

    #[test]
    fn final_column_wide_char_wraps_before_writing_when_autowrap_enabled() {
        let mut vt = VirtualTerminal::new(2, 4);
        vt.process("\x1b[1;4H界".as_bytes());

        assert_eq!(vt.grid().visible_row(0)[3].ch, ' ');
        assert!(vt.grid().visible_row(0).wrapped);
        assert_eq!(vt.grid().visible_row(1)[0].ch, '界');
        assert!(vt.grid().visible_row(1)[1].is_wide_continuation());
        assert_grid_wide_invariants(vt.grid());
    }

    #[test]
    fn final_column_wide_char_degrades_to_space_when_autowrap_disabled() {
        let mut vt = VirtualTerminal::new(1, 4);
        vt.process("\x1b[?7l\x1b[1;4H界".as_bytes());

        let cell = &vt.grid().visible_row(0)[3];
        assert_eq!(cell.ch, ' ');
        assert_eq!(cell.width, 1);
        assert_eq!(vt.cursor().col, 3);
        assert!(!vt.cursor().auto_wrap_pending);
        assert_grid_wide_invariants(vt.grid());
    }

    #[test]
    fn zero_width_combining_character_does_not_create_fake_continuation() {
        let mut vt = VirtualTerminal::new(1, 4);
        vt.process("A\u{0301}B".as_bytes());

        assert_eq!(vt.grid().visible_row(0)[0].ch, 'A');
        assert_eq!(vt.grid().visible_row(0)[1].ch, 'B');
        assert_grid_wide_invariants(vt.grid());
    }

    #[test]
    fn rep_of_wide_character_preserves_wide_pairs() {
        let mut vt = VirtualTerminal::new(1, 8);
        vt.process("界\x1b[2b".as_bytes());

        assert_eq!(compact_capture(&vt), "界界界");
        assert_grid_wide_invariants(vt.grid());
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

    #[test]
    fn parser_marks_source_row_as_wrapped_before_advancing() {
        let mut vt = VirtualTerminal::new(3, 5);
        vt.process(b"abcdef");

        assert!(vt.grid().visible_row(0).wrapped);
        assert!(!vt.grid().visible_row(1).wrapped);
        assert_eq!(vt.grid().visible_row(0)[4].ch, 'e');
        assert_eq!(vt.grid().visible_row(1)[0].ch, 'f');
    }

    #[test]
    fn resize_reflow_preserves_wrapped_capture_text() {
        let text = "abcdefghijklmnopqrstuvwxyz";
        let mut vt = VirtualTerminal::new(5, 10);
        vt.process(text.as_bytes());

        vt.resize(7, 7);

        assert_eq!(compact_capture(&vt), text);

        vt.resize(7, 13);

        assert_eq!(compact_capture(&vt), text);
    }

    #[test]
    fn resize_keeps_alternate_screen_canvas_separate_from_primary_reflow() {
        let mut vt = VirtualTerminal::new(4, 5);
        vt.process(b"primary-wrap");
        vt.enter_alternate_screen();
        vt.process(b"ALT-CANVAS");

        vt.resize(4, 4);

        assert!(vt.is_alternate_screen());
        assert_eq!(vt.grid().visible_row(0)[0].ch, 'A');
        assert_eq!(vt.grid().visible_row(0)[3].ch, '-');

        vt.leave_alternate_screen();

        assert!(!vt.is_alternate_screen());
        assert_eq!(compact_capture(&vt), "primary-wrap");
    }

    #[test]
    fn resize_preserves_dynamic_default_colors() {
        let mut vt = VirtualTerminal::new(4, 5);
        vt.process(b"\x1b]10;#112233\x1b\\\x1b]11;#445566\x1b\\\x1b]12;#778899\x1b\\");
        vt.process(b"abcdef");

        vt.resize(5, 4);

        assert_eq!(
            vt.default_colors(),
            TerminalDefaultColors {
                fg: Some([0x11, 0x22, 0x33]),
                bg: Some([0x44, 0x55, 0x66]),
                cursor: Some([0x77, 0x88, 0x99]),
            }
        );
        assert_eq!(compact_capture(&vt), "abcdef");
    }

    #[test]
    fn test_resize_resets_scroll_region() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[5;20r"); // Set custom scroll region.
        vt.resize(10, 40);
        assert_eq!(vt.scroll_region().top, 0);
        assert_eq!(vt.scroll_region().bottom, 9);
    }

    #[test]
    fn test_resize_clamps_cursor() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[24;80H"); // Cursor at bottom-right.
        vt.resize(10, 40);
        assert!(vt.cursor().row <= 9);
        assert!(vt.cursor().col <= 39);
    }

    #[test]
    fn test_clear_scrollback() {
        let mut vt = VirtualTerminal::new(3, 5);
        // Generate scrollback.
        for _ in 0..10 {
            vt.process(b"\n");
        }
        assert!(vt.scrollback_len() > 0);
        vt.clear_scrollback();
        assert_eq!(vt.scrollback_len(), 0);
    }

    #[test]
    fn test_cursor_save_restore_via_esc() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[5;10H"); // Move to 5,10.
        vt.process(b"\x1b7"); // Save cursor.
        vt.process(b"\x1b[1;1H"); // Move home.
        vt.process(b"\x1b8"); // Restore cursor.
        assert_eq!(vt.cursor().row, 4);
        assert_eq!(vt.cursor().col, 9);
    }

    #[test]
    fn test_insert_lines() {
        let mut vt = VirtualTerminal::new(5, 10);
        vt.process(b"\x1b[3;1HX"); // Write X at row 3 (0-indexed row 2).
        vt.process(b"\x1b[2;1H"); // Move to row 2 (0-indexed row 1).
        vt.process(b"\x1b[1L"); // Insert 1 line at row 1.
        // Row 1 should be the newly inserted blank line.
        assert_eq!(vt.grid().visible_row(1)[0].ch, ' ');
        // X was at row 2, shifted down to row 3.
        assert_eq!(vt.grid().visible_row(3)[0].ch, 'X');
    }

    #[test]
    fn test_delete_lines() {
        let mut vt = VirtualTerminal::new(5, 10);
        vt.process(b"A\r\nB\r\nC\r\nD\r\nE");
        vt.process(b"\x1b[2;1H"); // Move to row 2.
        vt.process(b"\x1b[1M"); // Delete 1 line.
        assert_eq!(vt.grid().visible_row(1)[0].ch, 'C'); // B removed, C shifted up.
    }

    #[test]
    fn test_insert_characters() {
        let mut vt = VirtualTerminal::new(1, 10);
        vt.process(b"ABCDE");
        vt.process(b"\x1b[1;3H"); // Move to col 3.
        vt.process(b"\x1b[2@"); // Insert 2 characters.
        assert_eq!(vt.grid().visible_row(0)[0].ch, 'A');
        assert_eq!(vt.grid().visible_row(0)[1].ch, 'B');
        assert_eq!(vt.grid().visible_row(0)[2].ch, ' ');
        assert_eq!(vt.grid().visible_row(0)[3].ch, ' ');
        assert_eq!(vt.grid().visible_row(0)[4].ch, 'C');
    }

    #[test]
    fn test_delete_characters() {
        let mut vt = VirtualTerminal::new(1, 10);
        vt.process(b"ABCDE");
        vt.process(b"\x1b[1;2H"); // Move to col 2.
        vt.process(b"\x1b[2P"); // Delete 2 characters.
        assert_eq!(vt.grid().visible_row(0)[0].ch, 'A');
        assert_eq!(vt.grid().visible_row(0)[1].ch, 'D');
        assert_eq!(vt.grid().visible_row(0)[2].ch, 'E');
    }

    #[test]
    fn test_erase_characters() {
        let mut vt = VirtualTerminal::new(1, 10);
        vt.process(b"ABCDE");
        vt.process(b"\x1b[1;2H"); // Move to col 2.
        vt.process(b"\x1b[3X"); // Erase 3 characters.
        assert_eq!(vt.grid().visible_row(0)[0].ch, 'A');
        assert_eq!(vt.grid().visible_row(0)[1].ch, ' ');
        assert_eq!(vt.grid().visible_row(0)[2].ch, ' ');
        assert_eq!(vt.grid().visible_row(0)[3].ch, ' ');
        assert_eq!(vt.grid().visible_row(0)[4].ch, 'E');
    }

    #[test]
    fn test_decawm_disable() {
        let mut vt = VirtualTerminal::new(3, 5);
        vt.process(b"\x1b[?7l"); // Disable auto-wrap.
        vt.process(b"ABCDEFGH"); // Should overwrite last col, not wrap.
        assert_eq!(vt.cursor().row, 0);
        assert_eq!(vt.grid().visible_row(0)[4].ch, 'H');
    }

    #[test]
    fn test_bracketed_paste_mode() {
        let mut vt = VirtualTerminal::new(24, 80);
        assert!(!vt.modes().bracketed_paste);
        vt.process(b"\x1b[?2004h");
        assert!(vt.modes().bracketed_paste);
        vt.process(b"\x1b[?2004l");
        assert!(!vt.modes().bracketed_paste);
    }

    #[test]
    fn test_application_keypad_mode() {
        let mut vt = VirtualTerminal::new(24, 80);
        assert!(!vt.modes().application_keypad);
        vt.process(b"\x1b=");
        assert!(vt.modes().application_keypad);
        vt.process(b"\x1b>");
        assert!(!vt.modes().application_keypad);
    }

    #[test]
    fn test_cursor_shape() {
        let mut vt = VirtualTerminal::new(24, 80);
        assert_eq!(vt.cursor().shape, CursorShape::Block);
        vt.process(b"\x1b[5 q"); // Bar cursor.
        assert_eq!(vt.cursor().shape, CursorShape::Bar);
        vt.process(b"\x1b[3 q"); // Underline cursor.
        assert_eq!(vt.cursor().shape, CursorShape::Underline);
        vt.process(b"\x1b[1 q"); // Block cursor.
        assert_eq!(vt.cursor().shape, CursorShape::Block);
    }

    #[test]
    fn test_scroll_up_generates_scrollback() {
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"AAA\r\nBBB\r\nCCC");
        // At bottom, linefeed should scroll. LF does NOT do CR.
        vt.process(b"\r\nDDD");
        assert_eq!(vt.scrollback_len(), 1);
        // After CR+LF at bottom: scroll up, cursor at row 2 col 0, then write DDD.
        assert_eq!(vt.grid().visible_row(2)[0].ch, 'D');
        // Scrollback should have the first line.
        assert_eq!(vt.grid().scrollback_row(0).unwrap()[0].ch, 'A');
    }

    #[test]
    fn test_ed_clear_entire_screen() {
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"AAA\r\nBBB\r\nCCC");
        vt.process(b"\x1b[2J"); // Clear entire screen.
        assert_eq!(vt.grid().visible_row(0)[0].ch, ' ');
        assert_eq!(vt.grid().visible_row(1)[0].ch, ' ');
        assert_eq!(vt.grid().visible_row(2)[0].ch, ' ');
    }

    #[test]
    fn test_ed_clear_screen_and_scrollback() {
        let mut vt = VirtualTerminal::new(3, 10);
        // Generate scrollback.
        for _ in 0..5 {
            vt.process(b"\n");
        }
        assert!(vt.scrollback_len() > 0);
        vt.process(b"\x1b[3J"); // Clear screen + scrollback.
        assert_eq!(vt.scrollback_len(), 0);
        assert_eq!(vt.grid().visible_row(0)[0].ch, ' ');
    }

    #[test]
    fn test_nel_next_line() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"ABC\x1bE"); // ESC E = Next Line.
        assert_eq!(vt.cursor().row, 1);
        assert_eq!(vt.cursor().col, 0);
    }

    #[test]
    fn test_ind_index() {
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"\x1b[3;1H"); // Move to last row.
        vt.process(b"\x1bD"); // ESC D = Index (linefeed without CR).
        // Should scroll since at bottom.
        assert_eq!(vt.scrollback_len(), 1);
    }

    #[test]
    fn test_vpa_vertical_position() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[10d"); // VPA to row 10.
        assert_eq!(vt.cursor().row, 9); // 0-indexed
    }

    #[test]
    fn test_cha_cursor_character_absolute() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"ABCDEFG");
        vt.process(b"\x1b[4G"); // CHA to col 4.
        assert_eq!(vt.cursor().col, 3); // 0-indexed
    }

    #[test]
    fn test_cnl_cursor_next_line() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[5;10H"); // Row 5, Col 10.
        vt.process(b"\x1b[2E"); // CNL 2 -- move down 2 lines, col 0.
        assert_eq!(vt.cursor().row, 6);
        assert_eq!(vt.cursor().col, 0);
    }

    #[test]
    fn test_cpl_cursor_previous_line() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[5;10H"); // Row 5, Col 10.
        vt.process(b"\x1b[2F"); // CPL 2 -- move up 2 lines, col 0.
        assert_eq!(vt.cursor().row, 2);
        assert_eq!(vt.cursor().col, 0);
    }
}
