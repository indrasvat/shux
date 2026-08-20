use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::trace;

use crate::cell::{
    Cell, CellFlags, CellStyle, Color, ExtendedAttrs, TerminalDefaultColors, UnderlineStyle,
};
use crate::charset::{CharsetSlot, TerminalCharset, TerminalCharsets};
use crate::cursor::{Cursor, CursorShape};
use crate::grid::Grid;
use crate::screen::ScreenSwap;
use crate::sync::Presented;
use crate::tabstops::TabStops;

/// Terminal mode flags (DECSET/DECRST).
#[derive(Debug, Clone)]
pub struct TerminalModes {
    /// DECAWM -- auto-wrap mode (default: true).
    pub auto_wrap: bool,
    /// DECCKM -- cursor keys mode (application vs normal).
    pub application_cursor_keys: bool,
    /// DECNKM -- keypad mode (application vs numeric).
    pub application_keypad: bool,
    /// DECOM -- origin mode (cursor relative to scroll region).
    pub origin_mode: bool,
    /// DECTCEM -- text cursor enable mode (cursor visibility via mode).
    pub cursor_visible: bool,
    /// Bracketed paste mode (Mode 2004).
    pub bracketed_paste: bool,
    /// Send focus events mode (Mode 1004).
    pub focus_events: bool,
    /// Mouse tracking modes.
    pub mouse_tracking: MouseMode,
    /// SGR mouse coordinate encoding (Mode 1006).
    pub sgr_mouse: bool,
    /// Alternate-scroll mode (Mode 1007): while on the alternate screen and the
    /// app has NOT requested mouse tracking, the terminal translates wheel
    /// events into arrow-key presses so pagers/editors scroll. Defaults on
    /// (matches Debian-xterm / iTerm2 / kitty).
    pub alternate_scroll: bool,
    /// Synchronized output mode (Mode 2026).
    pub synchronized_output: bool,
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
            application_keypad: false,
            origin_mode: false,
            cursor_visible: true,
            bracketed_paste: false,
            focus_events: false,
            mouse_tracking: MouseMode::None,
            sgr_mouse: false,
            alternate_scroll: true,
            synchronized_output: false,
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
    /// Mode 1000 -- normal tracking (button press/release).
    Normal,
    /// Mode 1002 -- button event tracking (press/release/motion with button).
    ButtonEvent,
    /// Mode 1003 -- any event tracking (all motion).
    AnyEvent,
}

/// Scroll region (top and bottom margins, 0-indexed inclusive).
#[derive(Debug, Clone, Copy)]
pub struct ScrollRegion {
    pub top: usize,
    pub bottom: usize,
}

/// Maximum bytes retained for one in-progress DCS payload (issue #102).
///
/// XTGETTCAP and DECRQSS payloads are capability names — tens of bytes. This is
/// generous headroom; anything past it is a pane streaming an unterminated
/// control string to grow parser state without bound.
pub const MAX_DCS_PAYLOAD_BYTES: usize = 4096;

/// Maximum terminal replies produced from a single `process` batch (issue
/// #102). One PTY read can carry thousands of `ESC[6n`-style queries, each of
/// which would otherwise push a reply.
///
/// Sized against the largest LEGITIMATE burst, not the smallest safe number: an
/// application probing the full 256-colour palette emits exactly 256 replies in
/// one batch, which sat precisely on an earlier 256 budget — adding any other
/// startup query (DA, DA2, DSR, XTVERSION, DECRQM) would have clipped a valid
/// probe. 512 leaves headroom for a full palette probe plus normal startup
/// chatter while still cutting a 5,000-query flood by an order of magnitude.
pub const MAX_RESPONSES_PER_BATCH: usize = 512;

/// Size of vte's OSC buffer, and therefore the largest OSC payload we accept.
///
/// This MUST stay equal to the const generic passed to `vte::Parser` — the
/// truncation check compares against it, and a mismatch would either miss real
/// truncation or reject valid sequences. 4 KiB leaves room for genuine OSC 8
/// deep links and signed URLs while keeping parser state bounded.
pub const MAX_OSC_PAYLOAD_BYTES: usize = 4096;

/// Maximum characters retained for a window title inside the VT.
pub const MAX_TITLE_CHARS: usize = 256;

/// vte's private `MAX_OSC_PARAMS`. Mirrored here because vte does not export it
/// and gives no overflow signal; reaching it means the parameter list was cut
/// short. Must track vte's value — see the version pinned in the workspace
/// `Cargo.toml`.
const VTE_MAX_OSC_PARAMS: usize = 16;

/// The VT parser type, with its OSC buffer sized to [`MAX_OSC_PAYLOAD_BYTES`].
///
/// vte only enforces this cap when built without its `std` feature — see the
/// workspace `Cargo.toml`. Under `std` the buffer is an unbounded `Vec` and the
/// cap silently does not exist.
pub type VtParser = vte::Parser<MAX_OSC_PAYLOAD_BYTES>;

#[derive(Debug, Clone)]
pub struct DcsState {
    intermediates: Vec<u8>,
    action: char,
    payload: Vec<u8>,
    /// Set when the payload exceeded [`MAX_DCS_PAYLOAD_BYTES`]. A poisoned
    /// sequence is discarded at `unhook` rather than answered, because
    /// replying to a truncated capability query is worse than not replying.
    overflowed: bool,
}

/// The preceding character in the data stream — what REP (`CSI Pn b`) repeats.
///
/// ECMA-48 §8.3.103 defines REP against the DATA STREAM: it repeats "the
/// preceding character in the data stream", which survives anything that
/// happens between the character and the `CSI b` — a cursor move, an erase, a
/// line feed, a change of pen. shux re-read the cell to the LEFT OF THE CURSOR
/// instead, which agrees only while nothing has moved the cursor since. At
/// column 0 there is no cell to the left at all, so the repeat was dropped
/// silently (issue #122); anywhere else, a cursor move made REP repeat whatever
/// was parked next to the new position — a blank, half a wide character, or an
/// unrelated glyph from an earlier frame.
///
/// Only the CHARACTER is remembered. Colours, attributes and the hyperlink come
/// from the pen that is current at the `CSI b`, because the pen belongs to the
/// terminal rather than to the character — which is also what a copy of the
/// character arriving in the stream would pick up.
///
/// What is stored is the exact scalar sequence that was printed, so a repeat can
/// be replayed through the ordinary printing path and is therefore, by
/// construction, indistinguishable from the character arriving again. A grapheme
/// cluster keeps all of its scalars (`e` + U+0301, a flag pair, a ZWJ sequence);
/// they are stored as PRINTED, after charset translation, so `ESC ( 0 q`
/// followed by REP draws more horizontal line rather than more `q`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LastGraphic {
    /// Base scalar — the character that occupied the columns.
    ch: char,
    /// The zero-width scalars that joined it, in arrival order. Empty for a
    /// plain single-scalar character, which is the overwhelmingly common case.
    ///
    /// This buffer is REUSED rather than reallocated per character. Every
    /// printed character updates the record, so this sits on the hot path for
    /// all terminal output, not just for REP: building a fresh `String` per
    /// cluster-growing scalar cost 10-24% of parsing throughput on
    /// combining-mark and emoji text (adversarial review, issue #122).
    rest: String,
}

impl LastGraphic {
    /// The scalars to replay, in the order they were printed.
    fn scalars(&self) -> impl Iterator<Item = char> + '_ {
        std::iter::once(self.ch).chain(self.rest.chars())
    }

    fn scalar_count(&self) -> usize {
        1 + self.rest.chars().count()
    }
}

/// The VT handler that translates escape sequences into grid operations.
///
/// This struct is NOT the public API -- VirtualTerminal (in lib.rs) owns this
/// and delegates parsed bytes to it. The handler modifies the grid and cursor
/// directly.
pub struct VtHandler<'a> {
    /// The live grid, wrapped so that a write to it takes the
    /// synchronized-output snapshot first (issue #115, see [`crate::sync`]).
    pub(crate) grid: Presented<'a, Grid>,
    /// The live cursor, wrapped for the same reason: cursor position and
    /// visibility are part of the presented frame.
    pub(crate) cursor: Presented<'a, Cursor>,
    pub(crate) modes: &'a mut TerminalModes,
    pub(crate) scroll_region: &'a mut ScrollRegion,
    pub(crate) title: Presented<'a, Option<String>>,
    pub(crate) default_colors: Presented<'a, TerminalDefaultColors>,
    pub(crate) alt_grid: &'a mut Option<Grid>,
    pub(crate) alt_cursor: &'a mut Option<Cursor>,
    pub(crate) dcs_state: &'a mut Option<DcsState>,
    /// Whether synchronized output is currently holding the presentation
    /// open. Shared with every [`Presented`] above, which is how a `?2026h`
    /// arriving mid-batch arms snapshots taken later in the same batch.
    pub(crate) sync_armed: &'a AtomicBool,
    /// Frozen alternate-screen flag. Not a [`Presented`] because the live flag
    /// lives inside `TerminalModes` among fields that are not presented state;
    /// written only via [`VtHandler::set_alternate_screen`].
    pub(crate) frozen_alt: &'a mut Option<bool>,
    /// Take the whole snapshot at `?2026h` instead of at the first write.
    /// Always `false` in production — see
    /// [`crate::VirtualTerminal::set_eager_sync_freeze`].
    pub(crate) eager_sync_freeze: bool,
    pub(crate) active_grapheme_cell: &'a mut Option<(usize, usize)>,
    /// The preceding character in the data stream, for REP. Deliberately NOT
    /// cleared alongside `active_grapheme_cell` — that one tracks a position on
    /// the screen and every control sequence invalidates it, while this one
    /// tracks the stream and only RIS ends the stream (issue #122).
    pub(crate) last_graphic: &'a mut Option<LastGraphic>,
    pub(crate) charsets: &'a mut TerminalCharsets,
    pub(crate) tab_stops: &'a mut TabStops,
    pub(crate) responses: &'a mut Vec<Vec<u8>>,
    /// Set when a sequence handled inside vte resets the terminal, so the
    /// caller can drop graphics state it owns outside the parser (the APC
    /// scanner's in-flight sequence, stored images, placements). RIS runs
    /// downstream of the scanner, so it cannot reach that state directly.
    pub(crate) graphics_reset: &'a mut bool,
    /// Sticky flag set when a valid OSC 4 palette override is applied. shux-vt
    /// discards the override (Class-B limitation), so an indexed-colour capture
    /// taken afterwards is non-portable — the lens gate reads this to emit the
    /// `palette_unportable` diagnostic (task 078, R1).
    pub(crate) palette_overridden: &'a mut bool,
    /// The retired alternate-screen buffer, reused by the next entry
    /// (issue #106). At most one, ever.
    pub(crate) alt_spare: &'a mut Option<Grid>,
    /// Whether retired buffers may be recycled. Always `true` in production;
    /// the differential tests drive a second terminal with it off.
    pub(crate) reuse_retired_grids: bool,
}

impl<'a> VtHandler<'a> {
    /// Borrow everything the alternate-screen swap needs. One definition, used
    /// by `DECSET 1047/1049` and by `RIS`.
    fn screen_swap(&mut self) -> ScreenSwap<'_> {
        ScreenSwap {
            grid: &mut self.grid,
            cursor: &mut self.cursor,
            stashed_grid: self.alt_grid,
            stashed_cursor: self.alt_cursor,
            spare: self.alt_spare,
            reuse: self.reuse_retired_grids,
        }
    }

    /// Freeze the alternate-screen flag, if synchronized output is armed and
    /// it has not been frozen yet. Counterpart of [`Presented::freeze`] for
    /// the one presented component that is not wrapped.
    #[inline]
    fn freeze_alt_flag(&mut self) {
        if self.sync_armed.load(Ordering::Relaxed) && self.frozen_alt.is_none() {
            *self.frozen_alt = Some(self.modes.alternate_screen);
        }
    }

    /// The only writer of the live alternate-screen flag.
    ///
    /// Presented readers must never see a future alt flag against the frozen
    /// pixels of a past frame, so the flag is snapshotted on the way in — the
    /// same rule [`Presented`] enforces for the grid, cursor, title and
    /// default colours.
    fn set_alternate_screen(&mut self, on: bool) {
        self.freeze_alt_flag();
        self.modes.alternate_screen = on;
    }

    /// Snapshot every component of the presented frame at once.
    ///
    /// Used by the eager mode the differential oracle runs as its reference
    /// arm, and by the `?2026h` path when that mode is on.
    fn freeze_whole_presentation(&mut self) {
        self.grid.freeze();
        self.cursor.freeze();
        self.title.freeze();
        self.default_colors.freeze();
        self.freeze_alt_flag();
    }

    /// Release synchronized output: disarm first, then drop every snapshot, so
    /// that nothing done afterwards on the way out can re-take one.
    fn release_sync_presentation(&mut self) {
        self.sync_armed.store(false, Ordering::Relaxed);
        self.grid.discard();
        self.cursor.discard();
        self.title.discard();
        self.default_colors.discard();
        *self.frozen_alt = None;
    }

    fn clear_active_grapheme_cell(&mut self) {
        *self.active_grapheme_cell = None;
    }

    fn set_active_grapheme_cell(&mut self, row: usize, col: usize) {
        *self.active_grapheme_cell = Some((row, col));
    }

    /// Start a new remembered character. See [`LastGraphic`].
    ///
    /// Reuses the record's scalar buffer rather than allocating a new one: this
    /// runs for every printed character.
    fn remember_graphic_scalar(&mut self, ch: char) {
        match self.last_graphic.as_mut() {
            Some(record) => {
                record.ch = ch;
                record.rest.clear();
            }
            None => {
                *self.last_graphic = Some(LastGraphic {
                    ch,
                    rest: String::new(),
                })
            }
        }
    }

    /// Extend the remembered character with a zero-width scalar that joined it.
    ///
    /// `joined` is the cell the scalar actually landed in, and the record is only
    /// extended when that is the ACTIVE grapheme cell — the cell the last printed
    /// character went into. A mark that lands anywhere else has attached itself to
    /// a cell on the SCREEN that the data stream moved on from: shux gives a
    /// stray mark to whatever is left of the cursor, so after a cursor move it can
    /// be a character several positions back. Letting that redefine the preceding
    /// character is the screen-derived reasoning issue #122 exists to remove, and
    /// it made `ABCZ` + a cursor move + a mark + `CSI 3 b` repeat the `A` and
    /// overwrite `B`, `C` and `Z` (adversarial review).
    fn extend_remembered_graphic(&mut self, joined: (usize, usize), ch: char) {
        if *self.active_grapheme_cell != Some(joined) {
            return;
        }
        if let Some(record) = self.last_graphic.as_mut() {
            record.rest.push(ch);
        }
    }

    fn cursor_blank_cell(&self) -> Cell {
        Cell {
            ch: ' ',
            width: 1,
            style: self.cursor.style,
            extended: None,
        }
    }

    fn cursor_cell_extended(&self) -> Option<Arc<ExtendedAttrs>> {
        self.cursor.extended.clone()
    }

    fn save_cursor_state(&mut self) {
        self.cursor.save(self.modes.origin_mode, *self.charsets);
    }

    fn restore_cursor_state(&mut self) {
        if let Some((origin, charsets)) = self.cursor.restore() {
            self.modes.origin_mode = origin;
            *self.charsets = charsets;
            self.clamp_cursor_to_grid();
        }
    }

    fn grid_last_row(&self) -> usize {
        self.grid.rows().saturating_sub(1)
    }

    fn grid_last_col(&self) -> usize {
        self.grid.cols().saturating_sub(1)
    }

    fn origin_top(&self) -> usize {
        if self.modes.origin_mode {
            self.scroll_region.top.min(self.grid_last_row())
        } else {
            0
        }
    }

    fn origin_bottom(&self) -> usize {
        if self.modes.origin_mode {
            self.scroll_region.bottom.min(self.grid_last_row())
        } else {
            self.grid_last_row()
        }
    }

    fn addressed_row(&self, param: u16, default: u16) -> usize {
        let row_offset = usize::from(if param == 0 { default } else { param }).saturating_sub(1);
        let top = self.origin_top();
        let bottom = self.origin_bottom().max(top);
        top.saturating_add(row_offset)
            .min(bottom)
            .min(self.grid_last_row())
    }

    fn upward_vertical_top(&self) -> usize {
        let top = self.scroll_region.top.min(self.grid_last_row());
        let bottom = self.scroll_region.bottom.min(self.grid_last_row());
        if top <= bottom && self.cursor.row >= top {
            top
        } else {
            0
        }
    }

    fn downward_vertical_bottom(&self) -> usize {
        let top = self.scroll_region.top.min(self.grid_last_row());
        let bottom = self.scroll_region.bottom.min(self.grid_last_row());
        if top <= bottom && self.cursor.row <= bottom {
            bottom
        } else {
            self.grid_last_row()
        }
    }

    /// Rows in the active scroll region. Scrolling further than this only
    /// shuffles blank rows, so it is the work bound for SU/SD (issue #102).
    fn scroll_region_height(&self) -> usize {
        self.scroll_region
            .bottom
            .saturating_sub(self.scroll_region.top)
            .saturating_add(1)
    }

    /// Rows from the cursor to the bottom of the scroll region, or `None` when
    /// the cursor sits outside the region — where IL/DL must do nothing.
    /// Returning `None` rather than 0 keeps "outside the region" distinct from
    /// "at the last row", and stops the subtraction underflowing.
    fn lines_from_cursor_to_region_bottom(&self) -> Option<usize> {
        if self.cursor.row < self.scroll_region.top || self.cursor.row > self.scroll_region.bottom {
            return None;
        }
        Some(
            self.scroll_region
                .bottom
                .saturating_sub(self.cursor.row)
                .saturating_add(1),
        )
    }

    fn move_cursor_up(&mut self, n: usize) {
        let top = self.upward_vertical_top();
        self.cursor.row = self.cursor.row.saturating_sub(n).max(top);
        self.cursor.auto_wrap_pending = false;
    }

    fn move_cursor_down(&mut self, n: usize) {
        let bottom = self.downward_vertical_bottom();
        self.cursor.row = self.cursor.row.saturating_add(n).min(bottom);
        self.cursor.auto_wrap_pending = false;
    }

    fn home_cursor_to_origin(&mut self) {
        self.cursor.row = self.origin_top();
        self.cursor.col = 0;
        self.cursor.auto_wrap_pending = false;
    }

    /// DECALN -- Screen Alignment Pattern (`ESC # 8`, issue #117).
    ///
    /// The DEC screen-alignment test: fill the page with `E` so the margins can
    /// be seen. It is the first sequence a conformance suite emits — `vttest`
    /// opens with it — so a terminal that ignores it reports a blank screen
    /// where every other terminal reports a full one.
    ///
    /// VT510 §DECALN spells out three things beyond the fill, and each is a
    /// separate way to get it wrong:
    ///
    /// * the pattern covers the COMPLETE page — the scroll region does not
    ///   clip it (that is the whole point: the operator is looking at where the
    ///   margins fall);
    /// * it "sets the margins to the extremes of the page"; and
    /// * it "moves the cursor to the home position".
    ///
    /// The margins are reset BEFORE the cursor is homed so that home means the
    /// top-left of the page under origin mode too, rather than the top-left of
    /// whatever region the application had set.
    ///
    /// The fill carries default attributes, not the current SGR pen: DECALN
    /// draws a fixed test pattern, not text. The pen itself is untouched — the
    /// next printable character still uses it.
    fn screen_alignment_pattern(&mut self) {
        self.grid.fill_alignment_pattern();
        self.scroll_region.top = 0;
        self.scroll_region.bottom = self.grid.rows().saturating_sub(1);
        self.cursor.row = 0;
        self.cursor.col = 0;
        self.cursor.auto_wrap_pending = false;
    }

    fn clamp_cursor_to_grid(&mut self) {
        let row = self.cursor.row.min(self.grid_last_row());
        let col = self.cursor.col.min(self.grid_last_col());
        if row != self.cursor.row || col != self.cursor.col {
            self.cursor.auto_wrap_pending = false;
        }
        self.cursor.row = row;
        self.cursor.col = col;
    }

    fn reset_charsets(&mut self) {
        *self.charsets = TerminalCharsets::default();
    }

    fn active_charset(&self) -> TerminalCharset {
        match self.charsets.active {
            CharsetSlot::G0 => self.charsets.g0,
            CharsetSlot::G1 => self.charsets.g1,
        }
    }

    fn translate_printable(&self, ch: char) -> char {
        if self.active_charset() != TerminalCharset::DecSpecialGraphics {
            return ch;
        }
        dec_special_graphics(ch).unwrap_or(ch)
    }

    fn designate_charset(&mut self, slot: CharsetSlot, final_byte: u8) {
        let charset = match final_byte {
            b'0' => TerminalCharset::DecSpecialGraphics,
            b'B' => TerminalCharset::Ascii,
            _ => TerminalCharset::Ascii,
        };
        match slot {
            CharsetSlot::G0 => self.charsets.g0 = charset,
            CharsetSlot::G1 => self.charsets.g1 = charset,
        }
    }

    fn wrap_to_next_line(&mut self) {
        self.clear_active_grapheme_cell();
        self.cursor.col = 0;
        self.cursor.auto_wrap_pending = false;
        self.grid.visible_row_mut_marked(self.cursor.row).wrapped = true;
        if self.cursor.row == self.scroll_region.bottom {
            self.grid
                .scroll_up(self.scroll_region.top, self.scroll_region.bottom);
        } else if self.cursor.row + 1 < self.grid.rows() {
            // Same guard `linefeed` has, and for the same reason: when the
            // cursor sits BELOW the scroll region — a bottom status line
            // outside the region is the common case — it is not equal to
            // `region.bottom`, so without this it walked off the grid and the
            // next cell write panicked. `write_char` clamps the cursor before
            // the wide-character wrap branch, not after, so only wide glyphs
            // at the right edge reached it (issue #107 adversarial review).
            self.cursor.row += 1;
        }
    }

    /// Write a character at the current cursor position, advancing the cursor.
    fn write_char(&mut self, ch: char) {
        let width = unicode_width::UnicodeWidthChar::width(ch)
            .unwrap_or(1)
            .min(2);
        if width == 0 {
            self.append_zero_width_scalar(ch);
            return;
        }
        // Both join paths EXTEND the remembered character rather than replacing
        // it, because a scalar that joins a cluster is part of the same
        // character. Everything past them starts a new one -- including the
        // degenerate paths below, where a wide character has nowhere to go on a
        // one-column terminal: the stream still carried it, so a repeat of it
        // does the same harmless nothing another copy would have (issue #122).
        if self.try_append_to_active_grapheme(ch, width) {
            return;
        }
        if self.try_append_regional_indicator_pair(ch) {
            return;
        }
        self.remember_graphic_scalar(ch);
        let cols = self.grid.cols();
        let rows = self.grid.rows();

        // Handle auto-wrap pending state.
        if self.cursor.auto_wrap_pending {
            if self.modes.auto_wrap {
                self.wrap_to_next_line();
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

        if width == 2 && cols < 2 {
            let col = self.cursor.col;
            let blank = self.cursor_blank_cell();
            {
                let row = self.grid.visible_row_mut_marked(self.cursor.row);
                row.clear_wide_pair_around(col, self.cursor.style.bg);
                row[col] = blank;
            }
            self.cursor.auto_wrap_pending = false;
            self.clear_active_grapheme_cell();
            return;
        }

        if width == 2 && self.cursor.col + 1 >= cols {
            let col = self.cursor.col;
            let blank = self.cursor_blank_cell();
            {
                let row = self.grid.visible_row_mut_marked(self.cursor.row);
                row.clear_wide_pair_around(col, self.cursor.style.bg);
                row[col] = blank;
            }
            if self.modes.auto_wrap {
                self.wrap_to_next_line();
            } else {
                self.cursor.auto_wrap_pending = false;
                self.clear_active_grapheme_cell();
                return;
            }
        }

        // Insert mode: shift characters right.
        if self.modes.insert_mode {
            self.grid
                .insert_chars(self.cursor.row, self.cursor.col, width);
        }

        // Write the cell.
        let col = self.cursor.col;
        let cursor_row = self.cursor.row;
        let bg = self.cursor.style.bg;
        let extended = self.cursor_cell_extended();
        {
            let row = self.grid.visible_row_mut_marked(cursor_row);
            row.clear_wide_pair_around(col, bg);
            if width == 2 {
                row.clear_wide_pair_around(col + 1, bg);
            }
            row[col] = Cell {
                ch,
                width: width as u8,
                style: self.cursor.style,
                extended,
            };

            // For wide characters, write a continuation cell.
            if width == 2 && col + 1 < cols {
                row[col + 1] = Cell::wide_continuation();
            }
        }
        self.set_active_grapheme_cell(cursor_row, col);

        // Advance cursor.
        self.cursor.col += width;
        if self.cursor.col >= cols {
            self.cursor.col = cols.saturating_sub(1);
            self.cursor.auto_wrap_pending = true;
        }
    }

    fn append_zero_width_scalar(&mut self, ch: char) {
        let Some((row, col)) = self
            .active_grapheme_position()
            .or_else(|| self.preceding_cell_position())
        else {
            return;
        };
        if row >= self.grid.rows() || col >= self.grid.cols() {
            return;
        }
        // Peek immutably FIRST: taking the mutable row bumps the content
        // tally (lens ContentRevision), and a combining mark landing on a
        // blank or wide-continuation cell commits no write — access is not
        // a write (lens council P1 major 3).
        let will_write = self
            .grid
            .visible_row(row)
            .get(col)
            .is_some_and(|cell| !cell.is_wide_continuation() && cell.ch != ' ');
        if !will_write {
            return;
        }
        let appended = {
            let row_ref = self.grid.visible_row_mut_marked(row);
            row_ref
                .cells_mut()
                .get_mut(col)
                .is_some_and(|cell| cell.append_grapheme_scalar(ch))
        };
        // BEFORE `set_active_grapheme_cell` moves it: the record follows the
        // character the stream is building, and this scalar only belongs to it
        // if it landed where that character already is.
        if appended {
            self.extend_remembered_graphic((row, col), ch);
        }
        self.set_active_grapheme_cell(row, col);
    }

    fn try_append_to_active_grapheme(&mut self, ch: char, width: usize) -> bool {
        let Some((row, col)) = self.active_grapheme_position() else {
            return false;
        };
        let should_join = self
            .grid
            .visible_row(row)
            .get(col)
            .is_some_and(|cell| cell.grapheme().is_some_and(str_ends_with_zwj));
        if !should_join {
            return false;
        }
        let cols = self.grid.cols();
        let target_width = self
            .grid
            .visible_row(row)
            .get(col)
            .map(|cell| usize::from(cell.width).max(width))
            .unwrap_or(width)
            .min(2);
        if target_width == 2 && col + 1 >= cols {
            return false;
        }
        {
            let bg = self.cursor.style.bg;
            let row_ref = self.grid.visible_row_mut_marked(row);
            // A full payload rejects the scalar. Reporting it as consumed would
            // swallow this character and every one after it, because the stored
            // grapheme still ends in ZWJ and keeps requesting the join (#109).
            if !row_ref[col].append_grapheme_scalar(ch) {
                return false;
            }
            row_ref[col].width = target_width as u8;
            if target_width == 2 {
                if !row_ref[col + 1].is_wide_continuation() {
                    row_ref.clear_wide_pair_around(col + 1, bg);
                }
                row_ref[col + 1] = Cell::wide_continuation();
            }
        }
        self.extend_remembered_graphic((row, col), ch);
        let next_col = col + target_width;
        if next_col >= cols {
            self.cursor.col = cols.saturating_sub(1);
            self.cursor.auto_wrap_pending = true;
        } else {
            self.cursor.col = self.cursor.col.max(next_col);
        }
        true
    }

    fn try_append_regional_indicator_pair(&mut self, ch: char) -> bool {
        if !is_regional_indicator(ch) {
            return false;
        }
        // The pair has to form out of two indicators that ARRIVED together, not
        // out of two that merely ended up adjacent. `preceding_cell_position` is
        // derived from the cursor, so a cursor move landing exactly one past an
        // older lone indicator fused the two across a gap the data stream never
        // had -- and left the remembered character pointing at whatever was
        // printed before the move, so REP drew that instead of the indicator
        // (Codex review on PR #129). The ZWJ join has always been gated this
        // way; this one was not.
        let Some((row, col)) = self.active_grapheme_position() else {
            return false;
        };
        let previous_is_single_ri = self
            .grid
            .visible_row(row)
            .get(col)
            .is_some_and(cell_contains_single_regional_indicator);
        if !previous_is_single_ri {
            return false;
        }

        let cols = self.grid.cols();
        let target_width = 2;
        if col + 1 >= cols {
            return false;
        }
        {
            let bg = self.cursor.style.bg;
            let row_ref = self.grid.visible_row_mut_marked(row);
            // A full payload rejects the scalar. Reporting it as consumed would
            // swallow this character and every one after it, because the stored
            // grapheme still ends in ZWJ and keeps requesting the join (#109).
            if !row_ref[col].append_grapheme_scalar(ch) {
                return false;
            }
            row_ref[col].width = target_width as u8;
            if col + 1 < cols {
                if !row_ref[col + 1].is_wide_continuation() {
                    row_ref.clear_wide_pair_around(col + 1, bg);
                }
                row_ref[col + 1] = Cell::wide_continuation();
            }
        }
        self.extend_remembered_graphic((row, col), ch);
        self.set_active_grapheme_cell(row, col);
        self.cursor.col = (col + target_width).min(cols.saturating_sub(1));
        self.cursor.auto_wrap_pending = col + target_width >= cols;
        true
    }

    fn active_grapheme_position(&self) -> Option<(usize, usize)> {
        let (row, col) = (*self.active_grapheme_cell)?;
        let cell = self.grid.visible_row(row).get(col)?;
        (!cell.is_wide_continuation() && cell.ch != ' ').then_some((row, col))
    }

    fn preceding_cell_position(&self) -> Option<(usize, usize)> {
        let source_col = if self.cursor.auto_wrap_pending {
            self.cursor.col
        } else {
            self.cursor.col.checked_sub(1)?
        };
        let row = self.grid.visible_row(self.cursor.row);
        let source_col = if row
            .get(source_col)
            .is_some_and(|cell| cell.is_wide_continuation())
        {
            source_col.checked_sub(1)?
        } else {
            source_col
        };
        row.get(source_col)?;
        Some((self.cursor.row, source_col))
    }

    fn cursor_extended_mut(&mut self) -> &mut ExtendedAttrs {
        let extended = self
            .cursor
            .extended
            .get_or_insert_with(|| Arc::new(ExtendedAttrs::default()));
        Arc::make_mut(extended)
    }

    fn prune_cursor_extended(&mut self) {
        if self.cursor.extended.as_deref() == Some(&ExtendedAttrs::default()) {
            self.cursor.extended = None;
        }
    }

    fn set_cursor_hyperlink(&mut self, hyperlink: Option<String>) {
        self.cursor_extended_mut().hyperlink = hyperlink;
        self.prune_cursor_extended();
    }

    fn set_underline_style(&mut self, underline_style: UnderlineStyle) {
        self.cursor_extended_mut().underline_style = underline_style;
        self.prune_cursor_extended();
    }

    fn set_underline_color(&mut self, underline_color: Option<Color>) {
        self.cursor_extended_mut().underline_color = underline_color;
        self.prune_cursor_extended();
    }

    /// REP -- repeat the preceding character in the data stream.
    ///
    /// The source is [`LastGraphic`], recorded when the character was printed,
    /// and it is replayed SCALAR BY SCALAR through the ordinary printing path.
    /// That is not an implementation detail, it is the specification: REP means
    /// "n more copies of that character arrived", so anything the printing path
    /// does to an arriving character — wrapping at the right margin, scrolling
    /// at the bottom of the region, inserting under IRM, growing a grapheme
    /// cluster — must happen to a repeat identically. Re-deriving the placement
    /// here instead is how the old implementation ended up writing a
    /// two-column grapheme into a one-column cell.
    fn repeat_preceding_char(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        // Nothing has been printed yet, or a RIS ended the stream. There is no
        // preceding character, so REP repeats nothing -- as opposed to
        // repeating whatever the cursor happens to be parked next to.
        let Some(source) = self.last_graphic.clone() else {
            return;
        };
        for _ in 0..self.repeat_iterations(count, &source) {
            for ch in source.scalars() {
                self.write_char(ch);
            }
        }
    }

    /// How many copies REP is allowed to write (issue #102).
    ///
    /// Ten bytes of pane output must not buy unbounded work. One screenful of
    /// cells is the largest repeat that can still leave a visible mark -- REP
    /// legitimately wraps onto following lines, so clamping to the current row
    /// would break it -- and no real application exceeds that. A multi-scalar
    /// cluster costs more per copy, so the total number of scalars written is
    /// bounded a second time. The two together cap the work at
    /// `max(2 * rows * cols, MAX_GRAPHEME_SCALARS)` scalars -- the floor matters
    /// only on a grid so small that two screenfuls is fewer scalars than one
    /// cluster holds, where it forces a single whole copy through rather than
    /// writing nothing at all.
    fn repeat_iterations(&self, count: usize, source: &LastGraphic) -> usize {
        let cells = self.grid.rows().saturating_mul(self.grid.cols()).max(1);
        let scalar_budget = cells.saturating_mul(2) / source.scalar_count();
        count.min(cells).min(scalar_budget.max(1))
    }

    fn next_tab_col(&self, count: usize) -> usize {
        self.tab_stops.next_from(self.cursor.col, count)
    }

    fn prev_tab_col(&self, count: usize) -> usize {
        self.tab_stops.prev_from(self.cursor.col, count)
    }

    /// Carriage return: move cursor to column 0.
    fn carriage_return(&mut self) {
        self.cursor.col = 0;
        self.cursor.auto_wrap_pending = false;
    }

    /// Line feed: move cursor down, scrolling if at bottom of scroll region.
    fn linefeed(&mut self) {
        if self.cursor.row == self.scroll_region.bottom {
            self.grid
                .scroll_up(self.scroll_region.top, self.scroll_region.bottom);
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
            self.grid
                .scroll_down(self.scroll_region.top, self.scroll_region.bottom);
        } else if self.cursor.row > 0 {
            self.cursor.row -= 1;
        }
        self.cursor.auto_wrap_pending = false;
    }

    /// Apply an SGR (Select Graphic Rendition) parameter to the cursor style.
    fn apply_sgr(&mut self, param: u16) {
        match param {
            0 => {
                self.cursor.style = CellStyle::default();
                self.cursor.extended = None;
            }
            1 => self.cursor.style.flags.set(CellFlags::BOLD),
            2 => self.cursor.style.flags.set(CellFlags::DIM),
            3 => self.cursor.style.flags.set(CellFlags::ITALIC),
            4 => {
                self.cursor.style.flags.set(CellFlags::UNDERLINE);
                self.set_underline_style(UnderlineStyle::Single);
            }
            5 | 6 => self.cursor.style.flags.set(CellFlags::BLINK),
            7 => self.cursor.style.flags.set(CellFlags::INVERSE),
            8 => self.cursor.style.flags.set(CellFlags::HIDDEN),
            9 => self.cursor.style.flags.set(CellFlags::STRIKETHROUGH),
            21 => self.cursor.style.flags.unset(CellFlags::BOLD),
            22 => {
                self.cursor.style.flags.unset(CellFlags::BOLD);
                self.cursor.style.flags.unset(CellFlags::DIM);
            }
            23 => self.cursor.style.flags.unset(CellFlags::ITALIC),
            24 => {
                self.cursor.style.flags.unset(CellFlags::UNDERLINE);
                self.set_underline_style(UnderlineStyle::None);
            }
            25 => self.cursor.style.flags.unset(CellFlags::BLINK),
            27 => self.cursor.style.flags.unset(CellFlags::INVERSE),
            28 => self.cursor.style.flags.unset(CellFlags::HIDDEN),
            29 => self.cursor.style.flags.unset(CellFlags::STRIKETHROUGH),
            // Standard foreground colors (30-37).
            30..=37 => self.cursor.style.fg = Color::Indexed((param - 30) as u8),
            38 => {} // Extended foreground (handled via sub-params in csi_dispatch).
            39 => self.cursor.style.fg = Color::Default,
            // Standard background colors (40-47).
            40..=47 => self.cursor.style.bg = Color::Indexed((param - 40) as u8),
            48 => {} // Extended background (handled via sub-params in csi_dispatch).
            49 => self.cursor.style.bg = Color::Default,
            59 => self.set_underline_color(None),
            // Bright foreground colors (90-97).
            90..=97 => self.cursor.style.fg = Color::Indexed((param - 90 + 8) as u8),
            // Bright background colors (100-107).
            100..=107 => self.cursor.style.bg = Color::Indexed((param - 100 + 8) as u8),
            _ => trace!(sgr = param, "unhandled SGR parameter"),
        }
    }

    /// Handle DECSET/DECRST private mode toggles.
    fn set_private_mode(&mut self, mode: u16, enable: bool) {
        match mode {
            // DECCKM -- Cursor keys mode.
            1 => self.modes.application_cursor_keys = enable,
            // DECOM -- Origin mode.
            6 => {
                self.modes.origin_mode = enable;
                self.home_cursor_to_origin();
            }
            // DECAWM -- Auto-wrap mode.
            7 => self.modes.auto_wrap = enable,
            // DECTCEM -- Text cursor enable.
            25 => {
                self.modes.cursor_visible = enable;
                self.cursor.visible = enable;
            }
            // Mouse tracking: normal (1000).
            1000 => {
                self.modes.mouse_tracking = if enable {
                    MouseMode::Normal
                } else {
                    MouseMode::None
                };
            }
            // Mouse tracking: button event (1002).
            1002 => {
                self.modes.mouse_tracking = if enable {
                    MouseMode::ButtonEvent
                } else {
                    MouseMode::None
                };
            }
            // Mouse tracking: any event (1003).
            1003 => {
                self.modes.mouse_tracking = if enable {
                    MouseMode::AnyEvent
                } else {
                    MouseMode::None
                };
            }
            // Focus in/out events.
            1004 => self.modes.focus_events = enable,
            // SGR mouse coordinate encoding.
            1006 => self.modes.sgr_mouse = enable,
            // Alternate-scroll: wheel -> arrow keys on the alternate screen.
            1007 => self.modes.alternate_scroll = enable,
            // Save cursor (1048).
            1048 => {
                if enable {
                    self.save_cursor_state();
                } else {
                    self.restore_cursor_state();
                }
            }
            // Alternate screen buffer (47, 1047, 1049).
            //
            // `47` is the original xterm mode and is still emitted by anything
            // built against pre-1049 terminfo — it is the old termcap `ti`/`te`
            // pair. It was previously unhandled, so a program that asked for
            // the alternate screen the old way drew on the PRIMARY one and its
            // `?47l` restored nothing. Harmless-looking until something wrote
            // the whole page: a screen-alignment test under `?47` destroyed the
            // user's screen outright (issue #117 adversarial review).
            //
            // It behaves as `1047` does here: the cursor is carried across
            // rather than parked, because only `1049` saves and restores one.
            47 | 1047 | 1049 => {
                if enable {
                    if mode == 1049 {
                        self.save_cursor_state();
                    }
                    if self.modes.alternate_screen {
                        if mode == 1049 {
                            let saved = self.cursor.saved.clone();
                            self.grid.clear_visible(self.cursor.style.bg);
                            *self.cursor = Cursor::new();
                            self.cursor.saved = saved;
                        }
                        return;
                    }
                    // Enter alternate screen: swap grids. 1049 parks the
                    // primary cursor and homes the alternate one; 1047 carries
                    // the cursor across and parks nothing.
                    self.screen_swap().enter(mode == 1049);
                    self.set_alternate_screen(true);
                } else {
                    // Leave alternate screen: restore grids.
                    if self.modes.alternate_screen {
                        self.screen_swap().leave(mode == 1049);
                        self.set_alternate_screen(false);
                    }
                    if mode == 1049 {
                        self.restore_cursor_state();
                    }
                }
            }
            // Bracketed paste mode (2004).
            2004 => self.modes.bracketed_paste = enable,
            // Synchronized output mode (2026).
            2026 => {
                if enable {
                    if !self.sync_armed.load(Ordering::Relaxed) {
                        // The presented buffer the renderer is about to be
                        // shown is a different buffer from the one it has been
                        // tracking, so it repaints in full. O(1): a flag on the
                        // dirty state, not a walk of the grid. Marked BEFORE
                        // arming, so it is not itself a reason to take a copy.
                        self.grid.live_mut_unfrozen().mark_all_dirty();
                        self.sync_armed.store(true, Ordering::Relaxed);
                        if self.eager_sync_freeze {
                            self.freeze_whole_presentation();
                        }
                    }
                    self.modes.synchronized_output = true;
                } else {
                    self.modes.synchronized_output = false;
                    self.release_sync_presentation();
                    // Presentation jumps from the frozen frame to the live one:
                    // every cell on screen may differ, so the renderer repaints
                    // in full. Disarmed above, so this cannot take a snapshot.
                    self.grid.live_mut_unfrozen().mark_all_dirty();
                }
            }
            _ => trace!(mode, enable, "unhandled private mode"),
        }
    }

    /// Queue a terminal reply, bounded per batch (issue #102).
    ///
    /// A pane controls how many query sequences one read carries, so an
    /// unbounded reply queue is a write/CPU amplifier. Past the budget replies
    /// are dropped: a pane spamming thousands of DSR queries in one batch is
    /// not doing anything that needs answering.
    fn push_response(&mut self, response: impl Into<Vec<u8>>) {
        if self.responses.len() >= MAX_RESPONSES_PER_BATCH {
            trace!("terminal response budget exhausted for this batch; reply dropped");
            return;
        }
        self.responses.push(response.into());
    }

    fn report_cursor_position(&mut self, private: bool) {
        let row = if self.modes.origin_mode {
            self.cursor
                .row
                .saturating_sub(self.scroll_region.top.min(self.grid_last_row()))
                + 1
        } else {
            self.cursor.row + 1
        };
        let col = self.cursor.col + 1;
        if private {
            self.push_response(format!("\x1b[?{row};{col}R"));
        } else {
            self.push_response(format!("\x1b[{row};{col}R"));
        }
    }

    fn report_mode(&mut self, mode: u16, private: bool) {
        let value = if private {
            self.private_mode_report_value(mode)
        } else {
            self.standard_mode_report_value(mode)
        };
        if private {
            self.push_response(format!("\x1b[?{mode};{value}$y"));
        } else {
            self.push_response(format!("\x1b[{mode};{value}$y"));
        }
    }

    fn standard_mode_report_value(&self, mode: u16) -> u8 {
        match mode {
            4 => mode_report(self.modes.insert_mode),
            20 => mode_report(self.modes.newline_mode),
            _ => 0,
        }
    }

    fn private_mode_report_value(&self, mode: u16) -> u8 {
        match mode {
            1 => mode_report(self.modes.application_cursor_keys),
            6 => mode_report(self.modes.origin_mode),
            7 => mode_report(self.modes.auto_wrap),
            25 => mode_report(self.modes.cursor_visible),
            66 => mode_report(self.modes.application_keypad),
            1000 => mode_report(self.modes.mouse_tracking == MouseMode::Normal),
            1002 => mode_report(self.modes.mouse_tracking == MouseMode::ButtonEvent),
            1003 => mode_report(self.modes.mouse_tracking == MouseMode::AnyEvent),
            1004 => mode_report(self.modes.focus_events),
            1006 => mode_report(self.modes.sgr_mouse),
            1007 => mode_report(self.modes.alternate_scroll),
            47 | 1047 | 1049 => mode_report(self.modes.alternate_screen),
            2004 => mode_report(self.modes.bracketed_paste),
            2026 => mode_report(self.modes.synchronized_output),
            _ => 0,
        }
    }
}

impl<'a> vte::Perform for VtHandler<'a> {
    fn print(&mut self, ch: char) {
        self.write_char(self.translate_printable(ch));
    }

    fn execute(&mut self, byte: u8) {
        self.clear_active_grapheme_cell();
        match byte {
            // BEL -- bell.
            0x07 => { /* emit bell event in the future */ }
            // BS -- backspace.
            0x08 => {
                if self.cursor.col > 0 {
                    self.cursor.col -= 1;
                    self.cursor.auto_wrap_pending = false;
                }
            }
            // HT -- horizontal tab.
            0x09 => {
                self.cursor.col = self.next_tab_col(1);
                self.cursor.auto_wrap_pending = false;
            }
            // LF, VT, FF -- linefeed variants.
            0x0A..=0x0C => self.linefeed(),
            // CR -- carriage return.
            0x0D => self.carriage_return(),
            // SO -- Shift Out / LS1: select G1 into GL.
            0x0E => self.charsets.active = CharsetSlot::G1,
            // SI -- Shift In / LS0: select G0 into GL.
            0x0F => self.charsets.active = CharsetSlot::G0,
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
        // A control sequence ends the grapheme cluster under construction: a
        // combining mark arriving after a cursor move belongs to whatever is at
        // the new position, not to the character before the move.
        //
        // SGR and REP are the exceptions. SGR changes the pen without moving
        // anything. REP is not a break either -- it is MORE OF THE SAME
        // CHARACTER, so it has to join a cluster exactly as the character
        // arriving again would; breaking here made `a ZWJ CSI 3 b` land in a
        // different cell from `a ZWJ a ZWJ a ZWJ a ZWJ` (issue #122).
        let continues_the_cluster = intermediates.is_empty() && (action == 'm' || action == 'b');
        if !continues_the_cluster {
            self.clear_active_grapheme_cell();
        }
        let params_groups: Vec<Vec<u16>> =
            params.iter().map(|subparam| subparam.to_vec()).collect();
        // Flatten params: each subparam slice is collected into a flat Vec<u16>.
        let params_vec: Vec<u16> = params
            .iter()
            .flat_map(|subparam| subparam.iter().copied())
            .collect();
        let p = |idx: usize, default: u16| -> u16 {
            params_vec
                .get(idx)
                .copied()
                .filter(|&v| v != 0)
                .unwrap_or(default)
        };
        let rows = self.grid.rows();
        let cols = self.grid.cols();

        match (action, intermediates) {
            // CUU -- Cursor Up.
            ('A', []) => {
                let n = p(0, 1) as usize;
                self.move_cursor_up(n);
            }
            // CUD -- Cursor Down.
            ('B', []) => {
                let n = p(0, 1) as usize;
                self.move_cursor_down(n);
            }
            // CUF -- Cursor Forward.
            ('C', []) => {
                let n = p(0, 1) as usize;
                self.cursor.col = (self.cursor.col + n).min(cols - 1);
                self.cursor.auto_wrap_pending = false;
            }
            // HPR -- Horizontal Position Relative.
            ('a', []) => {
                let n = p(0, 1) as usize;
                self.cursor.col = (self.cursor.col + n).min(cols - 1);
                self.cursor.auto_wrap_pending = false;
            }
            // CUB -- Cursor Backward.
            ('D', []) => {
                let n = p(0, 1) as usize;
                self.cursor.col = self.cursor.col.saturating_sub(n);
                self.cursor.auto_wrap_pending = false;
            }
            // CNL -- Cursor Next Line.
            ('E', []) => {
                let n = p(0, 1) as usize;
                self.move_cursor_down(n);
                self.cursor.col = 0;
            }
            // VPR -- Vertical Position Relative.
            ('e', []) => {
                let n = p(0, 1) as usize;
                self.move_cursor_down(n);
            }
            // CPL -- Cursor Previous Line.
            ('F', []) => {
                let n = p(0, 1) as usize;
                self.move_cursor_up(n);
                self.cursor.col = 0;
            }
            // CHA -- Cursor Character Absolute (column).
            ('G', []) => {
                let col = (p(0, 1) as usize).saturating_sub(1).min(cols - 1);
                self.cursor.col = col;
                self.cursor.auto_wrap_pending = false;
            }
            // HPA -- Horizontal Position Absolute.
            ('`', []) => {
                let col = (p(0, 1) as usize).saturating_sub(1).min(cols - 1);
                self.cursor.col = col;
                self.cursor.auto_wrap_pending = false;
            }
            // CHT -- Cursor Forward Tabulation.
            ('I', []) => {
                self.cursor.col = self.next_tab_col(p(0, 1) as usize);
                self.cursor.auto_wrap_pending = false;
            }
            // CBT -- Cursor Backward Tabulation.
            ('Z', []) => {
                self.cursor.col = self.prev_tab_col(p(0, 1) as usize);
                self.cursor.auto_wrap_pending = false;
            }
            // CUP / HVP -- Cursor Position.
            ('H', []) | ('f', []) => {
                let row = self.addressed_row(p(0, 1), 1);
                let col = (p(1, 1) as usize).saturating_sub(1).min(cols - 1);
                self.cursor.row = row;
                self.cursor.col = col;
                self.cursor.auto_wrap_pending = false;
            }
            // ED -- Erase in Display.
            ('J', []) => {
                let bg = self.cursor.style.bg;
                match p(0, 0) {
                    0 => {
                        // Clear from cursor to end.
                        self.grid.erase_chars(
                            self.cursor.row,
                            self.cursor.col,
                            cols - self.cursor.col,
                            bg,
                        );
                        if self.cursor.row + 1 < rows {
                            self.grid.clear_below(self.cursor.row + 1, bg);
                        }
                    }
                    1 => {
                        // Clear from beginning to cursor.
                        if self.cursor.row > 0 {
                            self.grid.clear_above(self.cursor.row - 1, bg);
                        }
                        self.grid
                            .erase_chars(self.cursor.row, 0, self.cursor.col + 1, bg);
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
            // EL -- Erase in Line.
            ('K', []) => {
                let bg = self.cursor.style.bg;
                match p(0, 0) {
                    0 => self.grid.erase_chars(
                        self.cursor.row,
                        self.cursor.col,
                        cols - self.cursor.col,
                        bg,
                    ),
                    1 => self
                        .grid
                        .erase_chars(self.cursor.row, 0, self.cursor.col + 1, bg),
                    2 => self.grid.erase_chars(self.cursor.row, 0, cols, bg),
                    _ => {}
                }
            }
            // IL -- Insert Lines. Ignored when the cursor sits outside the
            // scroll region (matches xterm, and keeps the clamp below from
            // underflowing).
            ('L', []) => {
                if let Some(limit) = self.lines_from_cursor_to_region_bottom() {
                    let n = (p(0, 1) as usize).min(limit);
                    self.grid
                        .scroll_down_n(self.cursor.row, self.scroll_region.bottom, n);
                }
            }
            // DL -- Delete Lines.
            ('M', []) => {
                if let Some(limit) = self.lines_from_cursor_to_region_bottom() {
                    let n = (p(0, 1) as usize).min(limit);
                    self.grid
                        .scroll_up_n(self.cursor.row, self.scroll_region.bottom, n);
                }
            }
            // SU -- Scroll Up.
            ('S', []) => {
                let n = (p(0, 1) as usize).min(self.scroll_region_height());
                self.grid
                    .scroll_up_n(self.scroll_region.top, self.scroll_region.bottom, n);
            }
            // SD -- Scroll Down.
            ('T', []) => {
                let n = (p(0, 1) as usize).min(self.scroll_region_height());
                self.grid
                    .scroll_down_n(self.scroll_region.top, self.scroll_region.bottom, n);
            }
            // ICH -- Insert Characters.
            ('@', []) => {
                let n = p(0, 1) as usize;
                self.grid.insert_chars(self.cursor.row, self.cursor.col, n);
            }
            // DCH -- Delete Characters.
            ('P', []) => {
                let n = p(0, 1) as usize;
                self.grid.delete_chars(self.cursor.row, self.cursor.col, n);
            }
            // ECH -- Erase Characters.
            ('X', []) => {
                let n = p(0, 1) as usize;
                self.grid
                    .erase_chars(self.cursor.row, self.cursor.col, n, self.cursor.style.bg);
            }
            // REP -- Repeat Preceding Character.
            ('b', []) => {
                self.repeat_preceding_char(p(0, 1) as usize);
            }
            // VPA -- Vertical Line Position Absolute.
            ('d', []) => {
                let row = self.addressed_row(p(0, 1), 1);
                self.cursor.row = row;
                self.cursor.auto_wrap_pending = false;
            }
            // TBC -- Tab Clear.
            ('g', []) => match p(0, 0) {
                0 => self.tab_stops.clear_current(self.cursor.col),
                3 => self.tab_stops.clear_all(),
                param => trace!(param, "unhandled TBC parameter"),
            },
            // SCOSC -- Save Cursor (SCO/private form, common in modern TUI diff renderers).
            ('s', []) if params_vec.iter().all(|&param| param == 0) => {
                self.save_cursor_state();
            }
            // SCORC -- Restore Cursor (SCO/private form).
            ('u', []) if params_vec.iter().all(|&param| param == 0) => {
                self.restore_cursor_state();
            }
            // SGR -- Select Graphic Rendition.
            ('m', []) => {
                if params_groups.is_empty() {
                    self.apply_sgr(0);
                    return;
                }
                let mut i = 0;
                while i < params_groups.len() {
                    let group = &params_groups[i];
                    if group.is_empty() {
                        self.apply_sgr(0);
                        i += 1;
                        continue;
                    }
                    match group[0] {
                        4 if group.len() > 1 => {
                            match group[1] {
                                0 => {
                                    self.cursor.style.flags.unset(CellFlags::UNDERLINE);
                                    self.set_underline_style(UnderlineStyle::None);
                                }
                                1 => {
                                    self.cursor.style.flags.set(CellFlags::UNDERLINE);
                                    self.set_underline_style(UnderlineStyle::Single);
                                }
                                2 => {
                                    self.cursor.style.flags.set(CellFlags::UNDERLINE);
                                    self.set_underline_style(UnderlineStyle::Double);
                                }
                                3 => {
                                    self.cursor.style.flags.set(CellFlags::UNDERLINE);
                                    self.set_underline_style(UnderlineStyle::Curly);
                                }
                                4 => {
                                    self.cursor.style.flags.set(CellFlags::UNDERLINE);
                                    self.set_underline_style(UnderlineStyle::Dotted);
                                }
                                5 => {
                                    self.cursor.style.flags.set(CellFlags::UNDERLINE);
                                    self.set_underline_style(UnderlineStyle::Dashed);
                                }
                                _ => trace!(sgr = ?group, "unhandled underline style SGR"),
                            }
                            i += 1;
                        }
                        38 => {
                            if let Some((color, consumed)) = parse_sgr_color(&params_groups, i) {
                                self.cursor.style.fg = color;
                                i += consumed;
                            } else {
                                trace!(sgr = ?group, "unhandled foreground color SGR");
                                i += 1;
                            }
                        }
                        48 => {
                            if let Some((color, consumed)) = parse_sgr_color(&params_groups, i) {
                                self.cursor.style.bg = color;
                                i += consumed;
                            } else {
                                trace!(sgr = ?group, "unhandled background color SGR");
                                i += 1;
                            }
                        }
                        58 => {
                            if let Some((color, consumed)) = parse_sgr_color(&params_groups, i) {
                                self.set_underline_color(Some(color));
                                i += consumed;
                            } else {
                                trace!(sgr = ?group, "unhandled underline color SGR");
                                i += 1;
                            }
                        }
                        59 => {
                            self.set_underline_color(None);
                            i += 1;
                        }
                        other if group.len() == 1 => {
                            self.apply_sgr(other);
                            i += 1;
                        }
                        _ => {
                            for &param in group {
                                self.apply_sgr(param);
                            }
                            i += 1;
                        }
                    }
                }
            }
            // DA -- Primary Device Attributes.
            ('c', []) => {
                if params_vec.is_empty() || params_vec == [0] {
                    self.push_response(b"\x1b[?62;1;2;6;9;15;22c".to_vec());
                }
            }
            // DA2 -- Secondary Device Attributes.
            ('c', [b'>']) => {
                if params_vec.is_empty() || params_vec == [0] {
                    self.push_response(b"\x1b[>0;95;0c".to_vec());
                }
            }
            // DSR -- Device Status Report.
            ('n', []) => {
                for &param in &params_vec {
                    match param {
                        5 => self.push_response(b"\x1b[0n".to_vec()),
                        6 => self.report_cursor_position(false),
                        _ => trace!(param, "unhandled DSR request"),
                    }
                }
            }
            // DEC-specific DSR.
            ('n', [b'?']) => {
                for &param in &params_vec {
                    match param {
                        6 => self.report_cursor_position(true),
                        15 => self.push_response(b"\x1b[?10n".to_vec()),
                        25 => self.push_response(b"\x1b[?20n".to_vec()),
                        26 => self.push_response(b"\x1b[?27;1;0;0n".to_vec()),
                        53 => self.push_response(b"\x1b[?50n".to_vec()),
                        _ => trace!(param, "unhandled private DSR request"),
                    }
                }
            }
            // DECSTBM -- Set Scrolling Region.
            ('r', []) => {
                // `rows - 1` underflowed on a 0-row grid, wrapping the clamp to
                // usize::MAX and letting DECSTBM name a region of ~65535 rows
                // the grid does not have (issue #107). A grid with no rows has
                // no region to set.
                if let Some(last_row) = rows.checked_sub(1) {
                    let top = (p(0, 1) as usize).saturating_sub(1);
                    let bottom = (p(1, rows as u16) as usize).saturating_sub(1).min(last_row);
                    if top < bottom {
                        self.scroll_region.top = top;
                        self.scroll_region.bottom = bottom;
                        self.home_cursor_to_origin();
                    }
                }
            }
            // SM -- Set Mode (standard modes).
            ('h', []) => {
                for &param in &params_vec {
                    match param {
                        // IRM -- Insert/Replace mode.
                        4 => self.modes.insert_mode = true,
                        // LNM -- Newline mode.
                        20 => self.modes.newline_mode = true,
                        _ => trace!(param, "unhandled SM mode"),
                    }
                }
            }
            // RM -- Reset Mode (standard modes).
            ('l', []) => {
                for &param in &params_vec {
                    match param {
                        // IRM -- Insert/Replace mode.
                        4 => self.modes.insert_mode = false,
                        // LNM -- Newline mode.
                        20 => self.modes.newline_mode = false,
                        _ => trace!(param, "unhandled RM mode"),
                    }
                }
            }
            // DECSET -- set private mode.
            ('h', [b'?']) => {
                for &param in &params_vec {
                    self.set_private_mode(param, true);
                }
            }
            // DECRST -- reset private mode.
            ('l', [b'?']) => {
                for &param in &params_vec {
                    self.set_private_mode(param, false);
                }
            }
            // DECSCUSR -- Set Cursor Style (CSI Ps SP q).
            ('q', [b' ']) => {
                self.cursor.shape = match p(0, 1) {
                    0 | 1 => CursorShape::Block,
                    2 => CursorShape::Block, // steady block
                    3 | 4 => CursorShape::Underline,
                    5 | 6 => CursorShape::Bar,
                    _ => CursorShape::Block,
                };
            }
            // XTVERSION -- Report xterm name and version (CSI > Ps q).
            ('q', [b'>']) => {
                if params_vec.is_empty() || params_vec == [0] {
                    self.push_response(format!("\x1bP>|shux {}\x1b\\", env!("CARGO_PKG_VERSION")));
                }
            }
            // DECRQM -- Request ANSI mode.
            ('p', [b'$']) => {
                for &mode in &params_vec {
                    self.report_mode(mode, false);
                }
            }
            // DECRQM -- Request DEC private mode.
            ('p', [b'?', b'$']) => {
                for &mode in &params_vec {
                    self.report_mode(mode, true);
                }
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
        self.clear_active_grapheme_cell();
        match (byte, intermediates) {
            // DECSC -- Save Cursor (ESC 7).
            (b'7', []) => self.save_cursor_state(),
            // DECRC -- Restore Cursor (ESC 8).
            (b'8', []) => self.restore_cursor_state(),
            // DECALN -- Screen Alignment Pattern (ESC # 8).
            //
            // Must stay BELOW the bare `ESC 8` arm above and be matched on the
            // `#` intermediate: the two sequences differ only by that byte, and
            // this one used to fall through to the "unhandled" arm entirely
            // (issue #117).
            (b'8', [b'#']) => self.screen_alignment_pattern(),
            // Designate G0/G1 character sets.
            (byte, [b'(']) => self.designate_charset(CharsetSlot::G0, byte),
            (byte, [b')']) => self.designate_charset(CharsetSlot::G1, byte),
            // DECPAM -- Application keypad mode (ESC =).
            (b'=', []) => self.modes.application_keypad = true,
            // DECPNM -- Normal keypad mode (ESC >).
            (b'>', []) => self.modes.application_keypad = false,
            // HTS -- Horizontal Tab Set.
            (b'H', []) => self.tab_stops.set(self.cursor.col),
            // RI -- Reverse Index (ESC M).
            (b'M', []) => self.reverse_index(),
            // IND -- Index (ESC D) -- move cursor down, scroll if needed.
            (b'D', []) => self.linefeed(),
            // NEL -- Next Line (ESC E).
            (b'E', []) => {
                self.carriage_return();
                self.linefeed();
            }
            // RIS -- Full Reset (ESC c).
            (b'c', []) => {
                // The initial state RIS restores is the PRIMARY screen, so the
                // swap has to happen before anything is cleared. Resetting
                // `modes` alone only lowered the alt-screen flag: the
                // alternate buffer stayed live and the primary one stayed
                // parked and unreachable. Because the alternate buffer is
                // built with no scrollback budget, it then became the pane's
                // primary buffer — and the pane lost scrollback permanently,
                // which `reset(1)` and a crashed full-screen app both trigger.
                // The cursor is homed a few lines down, so there is nothing
                // worth restoring from the parked one.
                // Release synchronized output FIRST. RIS discards the frozen
                // presentation outright, so freezing on the way through it
                // would be a full grid copy taken only to be dropped — and a
                // pane can emit `ESC[?2026h ESC c` as readily as any other ten
                // bytes (issue #115).
                self.release_sync_presentation();
                if self.modes.alternate_screen {
                    self.screen_swap().leave(false);
                }
                self.grid.clear_visible(Color::Default);
                self.grid.clear_scrollback();
                self.grid.mark_all_dirty();
                *self.cursor = Cursor::new();
                *self.modes = TerminalModes::default();
                *self.default_colors = TerminalDefaultColors::default();
                // RIS ends the data stream as far as REP is concerned: a
                // terminal that has just been switched on has no preceding
                // character, so a REP straight after one repeats nothing
                // (issue #122).
                *self.last_graphic = None;
                // RIS clears images too: the graphics protocol requires that
                // "when resetting the terminal, all images that are visible on
                // the screen must be cleared".
                *self.graphics_reset = true;
                self.reset_charsets();
                self.tab_stops.reset(self.grid.cols());
                self.scroll_region.top = 0;
                self.scroll_region.bottom = self.grid.rows().saturating_sub(1);
            }
            _ => {
                trace!(
                    byte,
                    intermediates = ?intermediates,
                    "unhandled ESC sequence"
                );
            }
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        if params.is_empty() {
            return;
        }
        let terminator = osc_terminator(bell_terminated);
        match params[0] {
            // OSC 0 -- Set Icon Name and Window Title.
            // OSC 2 -- Set Window Title.
            b"0" | b"2" => {
                if let Some(title_bytes) = params.get(1)
                    && let Ok(title) = std::str::from_utf8(title_bytes)
                {
                    // Clamp at parse time: the 64-char clamp in shux-core is a
                    // display concern and is not a bound on VT memory.
                    *self.title = Some(clamp_title(title));
                }
            }
            // OSC 10/11/12 -- Set dynamic default foreground/background/cursor.
            b"10" | b"11" | b"12" => {
                if let Some(color_bytes) = params.get(1) {
                    if *color_bytes == b"?" {
                        let color = match params[0] {
                            b"10" => self.default_colors.fg.unwrap_or([238, 238, 238]),
                            b"11" => self.default_colors.bg.unwrap_or([0, 0, 0]),
                            b"12" => self
                                .default_colors
                                .cursor
                                .or(self.default_colors.fg)
                                .unwrap_or([238, 238, 238]),
                            _ => [238, 238, 238],
                        };
                        let selector = std::str::from_utf8(params[0]).unwrap_or("10");
                        self.push_response(format!(
                            "\x1b]{selector};{}{}",
                            format_osc_rgb(color),
                            terminator
                        ));
                    } else if let Ok(color) = parse_osc_color(color_bytes) {
                        match params[0] {
                            b"10" if self.default_colors.fg != Some(color) => {
                                self.default_colors.fg = Some(color);
                                self.grid.mark_all_dirty();
                            }
                            b"11" if self.default_colors.bg != Some(color) => {
                                self.default_colors.bg = Some(color);
                                self.grid.mark_all_dirty();
                            }
                            b"12" if self.default_colors.cursor != Some(color) => {
                                self.default_colors.cursor = Some(color);
                                self.grid.mark_all_dirty();
                            }
                            _ => {}
                        }
                    }
                }
            }
            // OSC 110/111/112 -- Reset dynamic default foreground/background/cursor.
            b"110" => {
                if self.default_colors.fg.take().is_some() {
                    self.grid.mark_all_dirty();
                }
            }
            b"111" => {
                if self.default_colors.bg.take().is_some() {
                    self.grid.mark_all_dirty();
                }
            }
            b"112" => {
                if self.default_colors.cursor.take().is_some() {
                    self.grid.mark_all_dirty();
                }
            }
            b"4" => {
                let (pairs, _odd_trailing) = params[1..].as_chunks::<2>();
                for pair in pairs {
                    let Ok(index) = std::str::from_utf8(pair[0]).unwrap_or("").parse::<u8>() else {
                        continue;
                    };
                    if pair[1] == b"?" {
                        let color = xterm_256_palette(index);
                        self.push_response(format!(
                            "\x1b]4;{index};{}{}",
                            format_osc_rgb(color),
                            terminator
                        ));
                    } else if parse_osc_color(pair[1]).is_ok() {
                        // The override colour is discarded (Class-B limitation),
                        // but record that one happened so an indexed-colour
                        // capture afterwards is flagged non-portable (078 R1).
                        // This must NOT bump content_revision — the adjudicated
                        // `osc_4_palette_no_bump` invariant still holds.
                        *self.palette_overridden = true;
                        self.grid.mark_all_dirty();
                    }
                }
            }
            // OSC 8 -- Set/clear hyperlink for subsequent cells.
            b"8" => {
                // vte truncates silently and still dispatches (issue #102). A
                // cut-short URI is a valid-looking link to somewhere the sender
                // never specified, so drop it rather than store it. Scoped to
                // OSC 8: for other selectors truncation only loses trailing
                // content, and dropping outright is the worse outcome.
                if osc8_payload_was_truncated(params) {
                    trace!("truncated OSC 8 hyperlink discarded without dispatch");
                    return;
                }
                if params.len() >= 3 {
                    let uri_bytes = join_osc_parts(&params[2..]);
                    if uri_bytes.is_empty() {
                        self.set_cursor_hyperlink(None);
                    } else if let Ok(uri) = String::from_utf8(uri_bytes) {
                        self.set_cursor_hyperlink(Some(uri));
                    }
                }
            }
            _ => {
                trace!(osc = ?params[0], "unhandled OSC sequence");
            }
        }
    }

    fn hook(&mut self, _params: &vte::Params, intermediates: &[u8], _ignore: bool, action: char) {
        *self.dcs_state = Some(DcsState {
            intermediates: intermediates.to_vec(),
            action,
            payload: Vec::new(),
            overflowed: false,
        });
    }

    fn put(&mut self, byte: u8) {
        if let Some(dcs) = self.dcs_state.as_mut() {
            // Bound the in-progress payload (issue #102). Once poisoned the
            // sequence stays poisoned until `unhook` drops it, rather than
            // answering a truncated query. The two DCS types we support
            // (XTGETTCAP `+q`, DECRQSS `$q`) carry capability names of tens of
            // bytes, so the cap is unreachable in practice.
            if dcs.overflowed {
                return;
            }
            if dcs.payload.len() >= MAX_DCS_PAYLOAD_BYTES {
                dcs.overflowed = true;
                // Release the buffer now; `unhook` will not read it.
                dcs.payload = Vec::new();
                return;
            }
            dcs.payload.push(byte);
        }
    }

    fn unhook(&mut self) {
        let Some(dcs) = self.dcs_state.take() else {
            return;
        };
        if dcs.overflowed {
            trace!("oversized DCS payload discarded without dispatch");
            return;
        }
        match (dcs.intermediates.as_slice(), dcs.action) {
            ([b'+'], 'q') => {
                if let Some(response) = xtgettcap_response(&dcs.payload) {
                    self.push_response(response);
                } else {
                    self.push_response(b"\x1bP0+r\x1b\\".to_vec());
                }
            }
            ([b'$'], 'q') => {
                if let Some(response) = decrqss_response(&dcs.payload, self) {
                    self.push_response(response);
                } else {
                    self.push_response(b"\x1bP0$r\x1b\\".to_vec());
                }
            }
            _ => trace!(
                intermediates = ?dcs.intermediates,
                action = %dcs.action,
                "unhandled DCS sequence"
            ),
        }
    }
}

/// Whether an OSC 8 dispatch carries content vte truncated.
///
/// **Scoped to OSC 8 deliberately.** Truncation is only *dangerous* for
/// hyperlinks: a cut-short URI is a valid-looking link to somewhere the sender
/// never specified, so it must be dropped rather than stored. Everywhere else
/// truncation merely loses trailing content, and dropping the whole sequence
/// would be the bigger regression — a blanket guard silently voided OSC 4
/// palette batches of 8+ pairs, which in turn voided `palette_overridden` and
/// the `has_indexed_colors` portability signal that `shux lens gate` depends
/// on.
///
/// vte offers no overflow signal and truncates in two independent ways:
///
/// 1. **Byte buffer full.** Its buffer holds every parameter concatenated, so a
///    dispatch whose parameters sum to the buffer size is one that filled it.
/// 2. **Parameter list full.** vte tracks at most `MAX_OSC_PARAMS` (16)
///    parameters regardless of buffer space, so a semicolon flood truncates the
///    list without ever filling the buffer — check 1 cannot see that, and it
///    left a cell holding `";;;;;;;;;;;;;"` as its hyperlink.
///
/// **The parameter-count case is deliberately fail-closed, and the false
/// positive is unavoidable.** A URI whose path legitimately contains 13
/// semicolons produces exactly 16 parameters with nothing lost, and is dropped
/// here. That is not a missed refinement: vte discards everything past the cap
/// with no signal, so a *complete* 14-segment URI and a *truncated* 30-segment
/// one arrive as byte-identical dispatches — same parameter count, same total
/// bytes, same parameter values. Verified directly against vte 0.15:
///
/// ```text
/// A "s0;s1;...;s13"            -> 16 params, 33 bytes, [.., "s12", "s13"]
/// B "s0;s1;...;s13;...;s29"    -> 16 params, 33 bytes, [.., "s12", "s13"]
/// ```
///
/// Nothing at this boundary can tell them apart, so the choice is only which
/// error to make: drop a rare valid link, or store a link to a destination the
/// sender never specified. Dropping degrades to plain text; storing is a wrong
/// destination a user may click. Semicolons in URI paths are legal but
/// uncommon, and 13 of them is rarer still.
///
/// Note the DoS bound does NOT depend on this: memory is bounded by vte's
/// buffer cap (see the workspace `Cargo.toml`), which applies to every OSC
/// regardless. This function is purely about hyperlink safety.
fn osc8_payload_was_truncated(params: &[&[u8]]) -> bool {
    params.len() >= VTE_MAX_OSC_PARAMS
        || params.iter().map(|p| p.len()).sum::<usize>() >= MAX_OSC_PAYLOAD_BYTES
}

/// Bound a window title at VT storage time.
fn clamp_title(title: &str) -> String {
    if title.chars().count() <= MAX_TITLE_CHARS {
        return title.to_string();
    }
    title.chars().take(MAX_TITLE_CHARS).collect()
}

fn format_osc_rgb(rgb: [u8; 3]) -> String {
    format!(
        "rgb:{:04x}/{:04x}/{:04x}",
        u16::from(rgb[0]) * 257,
        u16::from(rgb[1]) * 257,
        u16::from(rgb[2]) * 257
    )
}

fn osc_terminator(bell_terminated: bool) -> &'static str {
    if bell_terminated { "\x07" } else { "\x1b\\" }
}

fn mode_report(enabled: bool) -> u8 {
    if enabled { 1 } else { 2 }
}

fn str_ends_with_zwj(text: &str) -> bool {
    text.ends_with('\u{200d}')
}

fn is_regional_indicator(ch: char) -> bool {
    ('\u{1f1e6}'..='\u{1f1ff}').contains(&ch)
}

fn cell_contains_single_regional_indicator(cell: &Cell) -> bool {
    let text = cell.display_text();
    let mut chars = text.chars();
    chars.next().is_some_and(is_regional_indicator) && chars.next().is_none()
}

fn parse_sgr_color(groups: &[Vec<u16>], start: usize) -> Option<(Color, usize)> {
    let group = groups.get(start)?;
    if group.len() > 1 {
        return parse_sgr_color_tail(&group[1..]).map(|color| (color, 1));
    }

    match groups
        .get(start + 1)
        .and_then(|group| group.first())
        .copied()
    {
        Some(5) => groups
            .get(start + 2)
            .and_then(|group| group.first())
            .copied()
            .map(|index| (Color::Indexed(sgr_u8(index)), 3)),
        Some(2) => {
            let r = groups.get(start + 2)?.first().copied()?;
            let g = groups.get(start + 3)?.first().copied()?;
            let b = groups.get(start + 4)?.first().copied()?;
            Some((Color::Rgb(sgr_u8(r), sgr_u8(g), sgr_u8(b)), 5))
        }
        _ => None,
    }
}

fn parse_sgr_color_tail(tail: &[u16]) -> Option<Color> {
    match tail.first().copied() {
        Some(5) if tail.len() >= 2 => Some(Color::Indexed(sgr_u8(tail[1]))),
        Some(2) if tail.len() >= 4 => {
            let rgb = &tail[tail.len() - 3..];
            Some(Color::Rgb(sgr_u8(rgb[0]), sgr_u8(rgb[1]), sgr_u8(rgb[2])))
        }
        _ => None,
    }
}

fn sgr_u8(value: u16) -> u8 {
    value.min(u8::MAX as u16) as u8
}

fn xterm_256_palette(index: u8) -> [u8; 3] {
    const BASE16: [[u8; 3]; 16] = [
        [0, 0, 0],
        [205, 0, 0],
        [0, 205, 0],
        [205, 205, 0],
        [0, 0, 238],
        [205, 0, 205],
        [0, 205, 205],
        [229, 229, 229],
        [127, 127, 127],
        [255, 0, 0],
        [0, 255, 0],
        [255, 255, 0],
        [92, 92, 255],
        [255, 0, 255],
        [0, 255, 255],
        [255, 255, 255],
    ];

    match index {
        0..=15 => BASE16[index as usize],
        16..=231 => {
            let n = index - 16;
            let component = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            [component(n / 36), component((n / 6) % 6), component(n % 6)]
        }
        232..=255 => {
            let level = 8 + (index - 232) * 10;
            [level, level, level]
        }
    }
}

fn xtgettcap_response(payload: &[u8]) -> Option<Vec<u8>> {
    let names = std::str::from_utf8(payload).ok()?;
    let mut pairs = Vec::new();
    for encoded_name in names.split(';').filter(|name| !name.is_empty()) {
        let name = decode_hex_ascii(encoded_name)?;
        let value = match name.as_str() {
            "Co" | "colors" => "256",
            "TN" | "name" => "xterm-256color",
            "RGB" | "Tc" => "",
            "AX" => "",
            "Ms" => "\x1b]52;%p1%s;%p2%s\x07",
            "Ss" => "\x1b[%p1%d q",
            "Se" => "\x1b[ q",
            "smcup" | "ti" => "\x1b[?1049h",
            "rmcup" | "te" => "\x1b[?1049l",
            "smkx" | "ks" => "\x1b[?1h\x1b=",
            "rmkx" | "ke" => "\x1b[?1l\x1b>",
            "setaf" | "AF" => "\x1b[%?%p1%{8}%<%t3%p1%d%e%p1%{16}%<%t9%p1%{8}%-%d%e38;5;%p1%d%;m",
            "setab" | "AB" => "\x1b[%?%p1%{8}%<%t4%p1%d%e%p1%{16}%<%t10%p1%{8}%-%d%e48;5;%p1%d%;m",
            "setrgbf" => "\x1b[38;2;%p1%d;%p2%d;%p3%dm",
            "setrgbb" => "\x1b[48;2;%p1%d;%p2%d;%p3%dm",
            _ => return None,
        };
        pairs.push(encode_hex_ascii(&format!("{name}={value}")));
    }
    if pairs.is_empty() {
        return None;
    }
    Some(format!("\x1bP1+r{}\x1b\\", pairs.join(";")).into_bytes())
}

fn decrqss_response(payload: &[u8], handler: &VtHandler<'_>) -> Option<Vec<u8>> {
    let response = match payload {
        b"m" => format!(
            "{}m",
            sgr_response(&handler.cursor.style, handler.cursor.extended.as_deref())
        ),
        b"r" => format!(
            "{};{}r",
            handler.scroll_region.top + 1,
            handler.scroll_region.bottom + 1
        ),
        b" q" => {
            let shape = match handler.cursor.shape {
                CursorShape::Block => 1,
                CursorShape::Underline => 3,
                CursorShape::Bar => 5,
            };
            format!("{shape} q")
        }
        b"\"q" => "0\"q".to_string(),
        b"\"p" => "61;1\"p".to_string(),
        _ => return None,
    };
    Some(format!("\x1bP1$r{response}\x1b\\").into_bytes())
}

fn sgr_response(style: &CellStyle, extended: Option<&ExtendedAttrs>) -> String {
    let mut params = Vec::new();
    if style.flags.contains(CellFlags::BOLD) {
        params.push("1".to_string());
    }
    if style.flags.contains(CellFlags::DIM) {
        params.push("2".to_string());
    }
    if style.flags.contains(CellFlags::ITALIC) {
        params.push("3".to_string());
    }
    if style.flags.contains(CellFlags::UNDERLINE) {
        match extended
            .map(|ext| ext.underline_style)
            .unwrap_or(UnderlineStyle::Single)
        {
            UnderlineStyle::None | UnderlineStyle::Single => params.push("4".to_string()),
            UnderlineStyle::Double => params.push("4:2".to_string()),
            UnderlineStyle::Curly => params.push("4:3".to_string()),
            UnderlineStyle::Dotted => params.push("4:4".to_string()),
            UnderlineStyle::Dashed => params.push("4:5".to_string()),
        }
    }
    if style.flags.contains(CellFlags::BLINK) {
        params.push("5".to_string());
    }
    if style.flags.contains(CellFlags::INVERSE) {
        params.push("7".to_string());
    }
    if style.flags.contains(CellFlags::HIDDEN) {
        params.push("8".to_string());
    }
    if style.flags.contains(CellFlags::STRIKETHROUGH) {
        params.push("9".to_string());
    }

    append_color_sgr(&mut params, style.fg, false);
    append_color_sgr(&mut params, style.bg, true);
    if let Some(color) = extended.and_then(|ext| ext.underline_color) {
        append_underline_color_sgr(&mut params, color);
    }

    if params.is_empty() {
        "0".to_string()
    } else {
        params.join(";")
    }
}

fn join_osc_parts(parts: &[&[u8]]) -> Vec<u8> {
    let mut joined = Vec::new();
    for (idx, part) in parts.iter().enumerate() {
        if idx > 0 {
            joined.push(b';');
        }
        joined.extend_from_slice(part);
    }
    joined
}

fn dec_special_graphics(ch: char) -> Option<char> {
    match ch {
        '_' => Some(' '),
        '`' => Some('◆'),
        'a' => Some('▒'),
        'b' => Some('␉'),
        'c' => Some('␌'),
        'd' => Some('␍'),
        'e' => Some('␊'),
        'f' => Some('°'),
        'g' => Some('±'),
        'h' => Some('␤'),
        'i' => Some('␋'),
        'j' => Some('┘'),
        'k' => Some('┐'),
        'l' => Some('┌'),
        'm' => Some('└'),
        'n' => Some('┼'),
        'o' => Some('⎺'),
        'p' => Some('⎻'),
        'q' => Some('─'),
        'r' => Some('⎼'),
        's' => Some('⎽'),
        't' => Some('├'),
        'u' => Some('┤'),
        'v' => Some('┴'),
        'w' => Some('┬'),
        'x' => Some('│'),
        'y' => Some('≤'),
        'z' => Some('≥'),
        '{' => Some('π'),
        '|' => Some('≠'),
        '}' => Some('£'),
        '~' => Some('·'),
        _ => None,
    }
}

fn append_color_sgr(params: &mut Vec<String>, color: Color, background: bool) {
    match color {
        Color::Default => {}
        Color::Indexed(index @ 0..=7) => {
            params.push((index as u16 + if background { 40 } else { 30 }).to_string());
        }
        Color::Indexed(index @ 8..=15) => {
            params.push((index as u16 - 8 + if background { 100 } else { 90 }).to_string());
        }
        Color::Indexed(index) => {
            params.push(if background { "48" } else { "38" }.to_string());
            params.push("5".to_string());
            params.push(index.to_string());
        }
        Color::Rgb(r, g, b) => {
            params.push(if background { "48" } else { "38" }.to_string());
            params.push("2".to_string());
            params.push(r.to_string());
            params.push(g.to_string());
            params.push(b.to_string());
        }
    }
}

fn append_underline_color_sgr(params: &mut Vec<String>, color: Color) {
    match color {
        Color::Default => params.push("59".to_string()),
        Color::Indexed(index) => {
            params.push("58".to_string());
            params.push("5".to_string());
            params.push(index.to_string());
        }
        Color::Rgb(r, g, b) => {
            params.push("58".to_string());
            params.push("2".to_string());
            params.push(r.to_string());
            params.push(g.to_string());
            params.push(b.to_string());
        }
    }
}

fn decode_hex_ascii(encoded: &str) -> Option<String> {
    if !encoded.len().is_multiple_of(2) {
        return None;
    }
    // Remainder is provably empty — the length guard above rejects odd input.
    let bytes = encoded
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let hex = std::str::from_utf8(pair).ok()?;
            u8::from_str_radix(hex, 16).ok()
        })
        .collect::<Option<Vec<_>>>()?;
    String::from_utf8(bytes).ok()
}

fn encode_hex_ascii(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn parse_osc_color(bytes: &[u8]) -> Result<[u8; 3], ()> {
    let s = std::str::from_utf8(bytes).map_err(|_| ())?;
    if let Some(hex) = s.strip_prefix('#') {
        return parse_hex_color(hex);
    }
    if let Some(rgb) = s.strip_prefix("rgb:") {
        return parse_rgb_color(rgb);
    }
    Err(())
}

fn parse_hex_color(hex: &str) -> Result<[u8; 3], ()> {
    if hex.len() != 6 {
        return Err(());
    }
    let r = u8::from_str_radix(&hex[0..2], 16).map_err(|_| ())?;
    let g = u8::from_str_radix(&hex[2..4], 16).map_err(|_| ())?;
    let b = u8::from_str_radix(&hex[4..6], 16).map_err(|_| ())?;
    Ok([r, g, b])
}

fn parse_rgb_color(rgb: &str) -> Result<[u8; 3], ()> {
    let mut parts = rgb.split('/');
    let r = parse_rgb_component(parts.next().ok_or(())?)?;
    let g = parse_rgb_component(parts.next().ok_or(())?)?;
    let b = parse_rgb_component(parts.next().ok_or(())?)?;
    if parts.next().is_some() {
        return Err(());
    }
    Ok([r, g, b])
}

fn parse_rgb_component(component: &str) -> Result<u8, ()> {
    if component.is_empty() || component.len() > 4 {
        return Err(());
    }
    let value = u16::from_str_radix(component, 16).map_err(|_| ())?;
    let max = (1u32 << (component.len() * 4)) - 1;
    Ok(((value as u32 * 255 + max / 2) / max) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::GridConfig;

    /// Helper to create a VtHandler and all backing state for testing.
    struct TestTerminal {
        grid: Grid,
        cursor: Cursor,
        modes: TerminalModes,
        scroll_region: ScrollRegion,
        title: Option<String>,
        default_colors: TerminalDefaultColors,
        alt_grid: Option<Grid>,
        alt_spare: Option<Grid>,
        alt_cursor: Option<Cursor>,
        dcs_state: Option<DcsState>,
        sync_armed: AtomicBool,
        frozen_grid: Option<crate::sync::FrozenScreen>,
        frozen_cursor: Option<Cursor>,
        frozen_colors: Option<TerminalDefaultColors>,
        frozen_title: Option<Option<String>>,
        frozen_alt: Option<bool>,
        active_grapheme_cell: Option<(usize, usize)>,
        last_graphic: Option<LastGraphic>,
        charsets: TerminalCharsets,
        tab_stops: TabStops,
        responses: Vec<Vec<u8>>,
        parser: VtParser,
    }

    impl TestTerminal {
        fn new(rows: usize, cols: usize) -> Self {
            TestTerminal {
                grid: Grid::new(rows, cols, GridConfig::default()),
                cursor: Cursor::new(),
                modes: TerminalModes::default(),
                scroll_region: ScrollRegion {
                    top: 0,
                    bottom: rows.saturating_sub(1),
                },
                title: None,
                default_colors: TerminalDefaultColors::default(),
                alt_grid: None,
                alt_spare: None,
                alt_cursor: None,
                dcs_state: None,
                sync_armed: AtomicBool::new(false),
                frozen_grid: None,
                frozen_cursor: None,
                frozen_colors: None,
                frozen_title: None,
                frozen_alt: None,
                active_grapheme_cell: None,
                last_graphic: None,
                charsets: TerminalCharsets::default(),
                tab_stops: TabStops::new(cols),
                responses: Vec::new(),
                parser: VtParser::new_with_size(),
            }
        }

        fn process(&mut self, bytes: &[u8]) {
            let mut palette_overridden = false;
            let mut graphics_reset = false;
            let mut handler = VtHandler {
                grid: Presented::new(&mut self.grid, &mut self.frozen_grid, &self.sync_armed),
                cursor: Presented::new(&mut self.cursor, &mut self.frozen_cursor, &self.sync_armed),
                modes: &mut self.modes,
                scroll_region: &mut self.scroll_region,
                title: Presented::new(&mut self.title, &mut self.frozen_title, &self.sync_armed),
                default_colors: Presented::new(
                    &mut self.default_colors,
                    &mut self.frozen_colors,
                    &self.sync_armed,
                ),
                alt_grid: &mut self.alt_grid,
                alt_cursor: &mut self.alt_cursor,
                dcs_state: &mut self.dcs_state,
                sync_armed: &self.sync_armed,
                frozen_alt: &mut self.frozen_alt,
                eager_sync_freeze: false,
                active_grapheme_cell: &mut self.active_grapheme_cell,
                last_graphic: &mut self.last_graphic,
                charsets: &mut self.charsets,
                tab_stops: &mut self.tab_stops,
                responses: &mut self.responses,
                graphics_reset: &mut graphics_reset,
                palette_overridden: &mut palette_overridden,
                alt_spare: &mut self.alt_spare,
                reuse_retired_grids: true,
            };
            self.parser.advance(&mut handler, bytes);
        }
    }

    #[test]
    fn test_write_char() {
        let mut t = TestTerminal::new(24, 80);
        t.process(b"A");
        assert_eq!(t.grid.visible_row(0)[0].ch, 'A');
        assert_eq!(t.cursor.col, 1);
    }

    #[test]
    fn test_linefeed() {
        let mut t = TestTerminal::new(3, 10);
        t.process(b"A\r\nB");
        assert_eq!(t.grid.visible_row(0)[0].ch, 'A');
        assert_eq!(t.grid.visible_row(1)[0].ch, 'B');
        assert_eq!(t.cursor.row, 1);
    }

    #[test]
    fn test_linefeed_without_cr() {
        let mut t = TestTerminal::new(3, 10);
        t.process(b"A\nB");
        assert_eq!(t.grid.visible_row(0)[0].ch, 'A');
        // LF only moves down, not to column 0. Cursor was at col 1 after 'A'.
        assert_eq!(t.grid.visible_row(1)[1].ch, 'B');
        assert_eq!(t.cursor.row, 1);
        assert_eq!(t.cursor.col, 2);
    }

    #[test]
    fn test_carriage_return() {
        let mut t = TestTerminal::new(3, 10);
        t.process(b"ABC\rD");
        assert_eq!(t.grid.visible_row(0)[0].ch, 'D');
        assert_eq!(t.cursor.col, 1);
    }

    #[test]
    fn test_backspace() {
        let mut t = TestTerminal::new(3, 10);
        t.process(b"AB\x08C");
        assert_eq!(t.grid.visible_row(0)[0].ch, 'A');
        assert_eq!(t.grid.visible_row(0)[1].ch, 'C');
    }

    #[test]
    fn test_tab() {
        let mut t = TestTerminal::new(3, 80);
        t.process(b"A\tB");
        assert_eq!(t.grid.visible_row(0)[0].ch, 'A');
        assert_eq!(t.cursor.col, 9); // 'B' at col 8, cursor at 9
        assert_eq!(t.grid.visible_row(0)[8].ch, 'B');
    }

    #[test]
    fn tab_honor_hts_without_losing_default_stops() {
        let mut t = TestTerminal::new(3, 40);
        t.process(b"\x1b[13G\x1bH\r\tA\tB\tC");
        let row = t.grid.visible_row(0);
        assert_eq!(row[8].ch, 'A');
        assert_eq!(row[12].ch, 'B');
        assert_eq!(row[16].ch, 'C');
    }

    #[test]
    fn tab_clear_current_preserves_other_default_stops() {
        let mut t = TestTerminal::new(3, 40);
        t.process(b"\x1b[9G\x1b[g\r\tX");
        let row = t.grid.visible_row(0);
        assert_eq!(row[8].ch, ' ');
        assert_eq!(row[16].ch, 'X');
    }

    #[test]
    fn tab_clear_all_clamps_forward_and_backward() {
        let mut t = TestTerminal::new(3, 20);
        t.process(b"\x1b[3g\r\tX\x1b[2ZB");
        let row = t.grid.visible_row(0);
        assert_eq!(row[19].ch, 'X');
        assert_eq!(row[0].ch, 'B');
    }

    #[test]
    fn tab_forward_and_backward_counts_use_custom_stops() {
        let mut t = TestTerminal::new(3, 40);
        t.process(b"\x1b[13G\x1bH\r\x1b[3IY\x1b[21G\x1b[2ZX");
        let row = t.grid.visible_row(0);
        assert_eq!(row[16].ch, 'Y');
        assert_eq!(row[12].ch, 'X');
    }

    #[test]
    fn decstr_does_not_reset_tab_stops() {
        let mut t = TestTerminal::new(3, 40);
        t.process(b"\x1b[9G\x1b[g\x1b[!p\r\tX");
        let row = t.grid.visible_row(0);
        assert_eq!(row[8].ch, ' ');
        assert_eq!(row[16].ch, 'X');
    }

    #[test]
    fn test_sgr_bold() {
        let mut t = TestTerminal::new(24, 80);
        t.process(b"\x1b[1mX");
        let cell = &t.grid.visible_row(0)[0];
        assert!(cell.style.flags.contains(CellFlags::BOLD));
    }

    #[test]
    fn test_sgr_reset() {
        let mut t = TestTerminal::new(24, 80);
        t.process(b"\x1b[1;31m\x1b[0mX");
        let cell = &t.grid.visible_row(0)[0];
        assert!(!cell.style.flags.contains(CellFlags::BOLD));
        assert_eq!(cell.style.fg, Color::Default);
    }

    #[test]
    fn test_cursor_position() {
        let mut t = TestTerminal::new(24, 80);
        t.process(b"\x1b[5;10H");
        assert_eq!(t.cursor.row, 4); // 0-indexed
        assert_eq!(t.cursor.col, 9); // 0-indexed
    }

    #[test]
    fn test_scroll_region_set() {
        let mut t = TestTerminal::new(24, 80);
        t.process(b"\x1b[5;20r");
        assert_eq!(t.scroll_region.top, 4);
        assert_eq!(t.scroll_region.bottom, 19);
        // Cursor should be homed.
        assert_eq!(t.cursor.row, 0);
        assert_eq!(t.cursor.col, 0);
    }

    #[test]
    fn test_reverse_index() {
        let mut t = TestTerminal::new(5, 10);
        t.process(b"\x1b[2;4r"); // Set scroll region lines 2-4.
        t.process(b"\x1b[2;1H"); // Move to top of region.
        t.process(b"\x1bM"); // Reverse index -- should scroll down.
        // Row 1 (top of region) should be blank (new row inserted).
        assert_eq!(t.grid.visible_row(1)[0].ch, ' ');
    }

    #[test]
    fn test_decset_cursor_visibility() {
        let mut t = TestTerminal::new(24, 80);
        assert!(t.cursor.visible);
        t.process(b"\x1b[?25l"); // Hide cursor.
        assert!(!t.cursor.visible);
        t.process(b"\x1b[?25h"); // Show cursor.
        assert!(t.cursor.visible);
    }

    #[test]
    fn test_ris_full_reset() {
        let mut t = TestTerminal::new(24, 80);
        t.process(b"Hello\x1b[31m"); // Write text and set color.
        t.process(b"\x1bc"); // Full reset.
        assert_eq!(t.grid.visible_row(0)[0].ch, ' ');
        assert_eq!(t.cursor.row, 0);
        assert_eq!(t.cursor.col, 0);
        assert_eq!(t.cursor.style.fg, Color::Default);
    }

    #[test]
    fn test_ris_full_reset_clears_dynamic_default_colors() {
        let mut t = TestTerminal::new(24, 80);
        t.process(b"\x1b]10;#ff8000\x07\x1b]11;#120a08\x07\x1b]12;#00ff00\x07");
        assert_eq!(t.default_colors.fg, Some([0xff, 0x80, 0x00]));
        assert_eq!(t.default_colors.bg, Some([0x12, 0x0a, 0x08]));
        assert_eq!(t.default_colors.cursor, Some([0x00, 0xff, 0x00]));

        t.process(b"\x1bc");

        assert_eq!(t.default_colors, TerminalDefaultColors::default());
    }

    #[test]
    fn test_osc_title() {
        let mut t = TestTerminal::new(24, 80);
        t.process(b"\x1b]2;test title\x07");
        assert_eq!(t.title.as_deref(), Some("test title"));
    }

    #[test]
    fn test_osc_dynamic_default_background_hex() {
        let mut t = TestTerminal::new(24, 80);
        t.process(b"\x1b]11;#120A08\x07");
        assert_eq!(t.default_colors.bg, Some([0x12, 0x0a, 0x08]));
    }

    #[test]
    fn test_osc_dynamic_default_foreground_rgb_and_reset() {
        let mut t = TestTerminal::new(24, 80);
        t.process(b"\x1b]10;rgb:ffff/8000/0000\x07");
        assert_eq!(t.default_colors.fg, Some([255, 128, 0]));
        t.process(b"\x1b]110\x07");
        assert_eq!(t.default_colors.fg, None);
    }

    #[test]
    fn test_osc_dynamic_cursor_color_rgb_and_reset() {
        let mut t = TestTerminal::new(24, 80);
        t.process(b"\x1b]12;rgb:0000/ffff/8000\x07");
        assert_eq!(t.default_colors.cursor, Some([0, 255, 128]));
        t.process(b"\x1b]112\x07");
        assert_eq!(t.default_colors.cursor, None);
    }

    #[test]
    fn test_insert_mode() {
        let mut t = TestTerminal::new(1, 10);
        t.process(b"ABC");
        t.process(b"\x1b[4h"); // Enable insert mode.
        t.process(b"\x1b[1;2H"); // Move to col 1.
        t.process(b"X");
        assert_eq!(t.grid.visible_row(0)[0].ch, 'A');
        assert_eq!(t.grid.visible_row(0)[1].ch, 'X');
        assert_eq!(t.grid.visible_row(0)[2].ch, 'B');
        assert_eq!(t.grid.visible_row(0)[3].ch, 'C');
    }
}
