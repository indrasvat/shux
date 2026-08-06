//! DECALN — the DEC screen-alignment pattern, `ESC # 8` (issue #117).
//!
//! `ESC # 8` fills the whole page with `E`. It is the first thing every
//! terminal conformance suite emits (`vttest` opens with it) and, in shux, it
//! was parsed and dropped: the `esc_dispatch` match had an arm for `ESC 8`
//! (DECRC) and none for `ESC # 8`, so the sequence fell through to the
//! "unhandled" trace arm and touched nothing.
//!
//! The VT510 reference is specific about what it does, and each clause is a
//! separate way to get it wrong:
//!
//!   * fills the COMPLETE page — the scroll region does not clip it;
//!   * fills with the alignment pattern, not with styled text — the current
//!     SGR state is not applied;
//!   * "sets the margins to the extremes of the page"; and
//!   * "moves the cursor to the home position".
//!
//! Beyond the sequence's own semantics, a full-screen write in shux has to
//! respect three grid invariants that a naive `cells[i].ch = 'E'` loop
//! silently breaks — and one of them is a cross-pane content leak:
//!
//!   * the copy-on-write row sharing that the synchronized-output freeze and
//!     `pane.snapshot` depend on (issue #115);
//!   * the write tally, which is what licenses recycling a retired
//!     alternate-screen buffer as a blank canvas (issue #106) — a fill that
//!     does not advance it leaves a screen full of `E` to be handed to the
//!     next application that enters the alternate screen;
//!   * wide-cell pairing, extended attributes and the soft-wrap flag, which
//!     survive into resize reflow and capture.

use shux_vt::{Cell, CellFlags, Color, Grid, GridConfig, VirtualTerminal};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const DECALN: &[u8] = b"\x1b#8";

fn row_chars(g: &Grid, row: usize) -> String {
    let row = g.visible_row(row);
    (0..row.len()).map(|c| row[c].ch).collect()
}

fn visible_rows(g: &Grid) -> Vec<String> {
    (0..g.rows()).map(|r| row_chars(g, r)).collect()
}

/// Every visible cell is exactly the alignment pattern: an `E` one column
/// wide, in default attributes, with no extended payload.
fn assert_screen_is_alignment_pattern(g: &Grid, what: &str) {
    assert!(g.rows() > 0 && g.cols() > 0, "{what}: nothing to check");
    for r in 0..g.rows() {
        let row = g.visible_row(r);
        assert_eq!(row.len(), g.cols(), "{what}: row {r} width");
        assert!(!row.wrapped, "{what}: row {r} still flagged soft-wrapped");
        for c in 0..row.len() {
            let cell = &row[c];
            assert_eq!(cell, &Cell::ALIGNMENT, "{what}: cell ({r},{c}) is {cell:?}");
        }
    }
}

fn scrollback_lines(g: &Grid) -> Vec<String> {
    (0..g.scrollback_len())
        .map(|i| {
            let row = g.scrollback_row(i).expect("scrollback row");
            (0..row.len()).map(|c| row[c].ch).collect::<String>()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 1. The fill itself
// ---------------------------------------------------------------------------

/// The reproduction from issue #117, verbatim.
#[test]
fn decaln_fills_the_screen_with_e() {
    let mut vt = VirtualTerminal::new(4, 8);
    vt.process(DECALN);
    assert_eq!(row_chars(vt.grid(), 0), "EEEEEEEE");
    assert_eq!(visible_rows(vt.grid()), vec!["EEEEEEEE"; 4]);
    assert_screen_is_alignment_pattern(vt.grid(), "plain DECALN");
}

/// The fill overwrites existing content everywhere, not just where the cursor
/// happens to be.
#[test]
fn decaln_overwrites_existing_content_on_every_row() {
    let mut vt = VirtualTerminal::new(4, 8);
    vt.process(b"row-zero\r\nrow-one\r\nrow-two\r\nrow-3");
    vt.process(DECALN);
    assert_screen_is_alignment_pattern(vt.grid(), "overwrite");
}

/// A one-cell terminal is still a terminal.
#[test]
fn decaln_fills_a_one_by_one_grid() {
    let mut vt = VirtualTerminal::new(1, 1);
    vt.process(DECALN);
    assert_eq!(row_chars(vt.grid(), 0), "E");
    assert_screen_is_alignment_pattern(vt.grid(), "1x1");
}

/// Degenerate geometries must not panic. `VirtualTerminal` clamps a pane to at
/// least 1x1, so the smallest terminal is exercised through the public API and
/// the genuinely empty grids straight through `Grid`, which is where the
/// arithmetic would underflow.
#[test]
fn decaln_on_degenerate_geometry_does_not_panic() {
    for (rows, cols) in [(0usize, 0usize), (0, 10), (10, 0), (1, 0), (0, 1)] {
        let mut vt = VirtualTerminal::new(rows, cols);
        vt.process(DECALN);
        assert_eq!(vt.grid().rows(), rows.max(1), "{rows}x{cols} rows");
        assert_eq!(vt.grid().cols(), cols.max(1), "{rows}x{cols} cols");
        assert_eq!(vt.cursor().row, 0);
        assert_eq!(vt.cursor().col, 0);
        assert_eq!(vt.scroll_region().top, 0);
        assert_eq!(vt.scroll_region().bottom, vt.grid().rows() - 1);

        let mut grid = Grid::new(rows, cols, GridConfig::default());
        grid.fill_alignment_pattern();
        assert_eq!(grid.rows(), rows, "bare grid {rows}x{cols} rows");
        assert_eq!(grid.cols(), cols, "bare grid {rows}x{cols} cols");
    }
}

/// A PTY read boundary can fall anywhere, including inside `ESC # 8`.
#[test]
fn decaln_survives_every_chunk_boundary() {
    for split in 0..=DECALN.len() {
        let mut vt = VirtualTerminal::new(3, 6);
        vt.process(&DECALN[..split]);
        vt.process(&DECALN[split..]);
        assert_screen_is_alignment_pattern(vt.grid(), &format!("split at {split}"));
    }
}

// ---------------------------------------------------------------------------
// 2. The scroll region does not clip it, and DECALN resets it
// ---------------------------------------------------------------------------

/// VT510 DECALN: the pattern covers the complete page. A scroll region set
/// beforehand must not clip the fill to its own rows.
#[test]
fn decaln_ignores_the_scroll_region() {
    let mut vt = VirtualTerminal::new(6, 5);
    vt.process(b"\x1b[3;4r"); // rows 3..4 (1-based) only
    vt.process(DECALN);
    assert_screen_is_alignment_pattern(vt.grid(), "with scroll region set");
}

/// VT510 DECALN: "sets the margins to the extremes of the page".
#[test]
fn decaln_resets_the_margins_to_the_whole_page() {
    let mut vt = VirtualTerminal::new(6, 5);
    vt.process(b"\x1b[3;4r");
    assert_eq!((vt.scroll_region().top, vt.scroll_region().bottom), (2, 3));
    vt.process(DECALN);
    assert_eq!(
        (vt.scroll_region().top, vt.scroll_region().bottom),
        (0, 5),
        "margins were not reset to the extremes of the page"
    );
}

/// The reset margins have to be the ones scrolling actually uses afterwards:
/// a newline at the bottom row must scroll the whole page, not the stale
/// region.
#[test]
fn scrolling_after_decaln_uses_the_full_page() {
    let mut vt = VirtualTerminal::new(4, 4);
    vt.process(b"\x1b[2;3r"); // a region in the middle
    vt.process(DECALN);
    // Home, walk to the last row, and force one scroll.
    vt.process(b"\x1b[4;1H\nX");
    let rows = visible_rows(vt.grid());
    assert_eq!(
        rows[0], "EEEE",
        "row 0 should have been scrolled into, not fixed by a stale margin"
    );
    assert_eq!(
        rows[3], "X   ",
        "the write landed on the bottom row: {rows:?}"
    );
    // A full-page scroll pushes one row of the pattern into history.
    assert_eq!(scrollback_lines(vt.grid()), vec!["EEEE"]);
}

// ---------------------------------------------------------------------------
// 3. The cursor
// ---------------------------------------------------------------------------

/// VT510 DECALN: "moves the cursor to the home position".
#[test]
fn decaln_homes_the_cursor() {
    let mut vt = VirtualTerminal::new(6, 10);
    vt.process(b"\x1b[4;7H");
    assert_eq!((vt.cursor().row, vt.cursor().col), (3, 6));
    vt.process(DECALN);
    assert_eq!((vt.cursor().row, vt.cursor().col), (0, 0));
}

/// Home is the top-left of the PAGE. Origin mode makes the cursor relative to
/// the scroll region — but DECALN has just reset the region to the whole page,
/// so the two agree, and the cursor must land at (0,0) either way.
#[test]
fn decaln_homes_to_the_page_origin_even_in_origin_mode() {
    let mut vt = VirtualTerminal::new(8, 6);
    vt.process(b"\x1b[?6h"); // DECOM on
    vt.process(b"\x1b[3;6r"); // region rows 3..6 (1-based)
    vt.process(b"\x1b[2;2H"); // relative to the region: absolute row 3
    assert_eq!(vt.cursor().row, 3);
    vt.process(DECALN);
    assert_eq!(
        (vt.cursor().row, vt.cursor().col),
        (0, 0),
        "origin mode must not park the cursor at a stale margin"
    );
    // Origin mode itself is not reset by DECALN — only the margins are.
    assert!(vt.modes().origin_mode);
    // And with the page-wide margins, writing at home lands on row 0.
    vt.process(b"Z");
    assert_eq!(row_chars(vt.grid(), 0), "ZEEEEE");
}

/// A pending auto-wrap (the cursor parked past the last column after filling
/// it) must not survive DECALN, or the next printable character wraps to row 1
/// instead of landing at home.
#[test]
fn decaln_clears_a_pending_auto_wrap() {
    let mut vt = VirtualTerminal::new(3, 4);
    vt.process(b"abcd"); // fills row 0, leaves wrap pending at col 3
    vt.process(DECALN);
    assert_eq!((vt.cursor().row, vt.cursor().col), (0, 0));
    vt.process(b"Z");
    assert_eq!(
        row_chars(vt.grid(), 0),
        "ZEEE",
        "the write wrapped away from home"
    );
    assert_eq!(row_chars(vt.grid(), 1), "EEEE");
}

/// DECALN moves the cursor; it does not touch the DECSC save slot.
#[test]
fn decaln_leaves_the_saved_cursor_alone() {
    let mut vt = VirtualTerminal::new(6, 10);
    vt.process(b"\x1b[3;5H\x1b7"); // DECSC at (2,4)
    vt.process(DECALN);
    assert_eq!((vt.cursor().row, vt.cursor().col), (0, 0));
    vt.process(b"\x1b8"); // DECRC
    assert_eq!((vt.cursor().row, vt.cursor().col), (2, 4));
}

/// Tab stops are terminal state, not screen content. DECALN resets the
/// MARGINS; nothing in VT510 says it resets the tab stops, and a conformance
/// suite that sets custom stops before the alignment test then tabs across the
/// pattern would silently get the default every-8 grid instead.
#[test]
fn decaln_leaves_tab_stops_alone() {
    let mut vt = VirtualTerminal::new(4, 20);
    vt.process(b"\x1b[3g"); // TBC 3 -- clear every stop
    vt.process(b"\x1b[1;12H\x1bH"); // HTS -- one stop at column 12 (0-based 11)
    vt.process(DECALN);
    // Home, then tab: the custom stop must still be the one that answers.
    vt.process(b"\x1b[1;1H\tX");
    assert_eq!(vt.cursor().row, 0);
    let row = row_chars(vt.grid(), 0);
    assert_eq!(
        row.find('X'),
        Some(11),
        "tab landed at {:?}, not the custom stop at column 12: {row:?}",
        row.find('X')
    );
}

/// The window title is presented state that belongs to the application, not to
/// the page. Filling the screen must not clear or change it.
#[test]
fn decaln_leaves_the_window_title_alone() {
    let mut vt = VirtualTerminal::new(3, 10);
    vt.process(b"\x1b]0;DECALN-TITLE-PROBE\x07");
    assert_eq!(vt.title(), Some("DECALN-TITLE-PROBE"));
    vt.process(DECALN);
    assert_eq!(
        vt.title(),
        Some("DECALN-TITLE-PROBE"),
        "the alignment fill changed the window title"
    );
    assert_screen_is_alignment_pattern(vt.grid(), "with a title set");
}

/// Cursor visibility and shape are not part of the alignment pattern.
#[test]
fn decaln_leaves_cursor_visibility_and_shape_alone() {
    let mut vt = VirtualTerminal::new(4, 4);
    vt.process(b"\x1b[?25l\x1b[4 q"); // hidden, underline shape
    let shape = vt.cursor().shape;
    vt.process(DECALN);
    assert!(!vt.cursor().visible);
    assert_eq!(vt.cursor().shape, shape);
}

// ---------------------------------------------------------------------------
// 4. Attributes: the pattern is not styled text
// ---------------------------------------------------------------------------

/// VT510 DECALN draws the alignment pattern, not styled text: the current SGR
/// state is not applied to the filled cells.
#[test]
fn decaln_ignores_the_current_sgr_state() {
    let mut vt = VirtualTerminal::new(3, 5);
    vt.process(b"\x1b[1;4;7;38;2;10;20;30;48;5;99m");
    // Draw with the pen first, so the cells the fill replaces are styled too:
    // a fill that inherited either the pen OR the cell it overwrites fails.
    vt.process(b"styled");
    assert_ne!(
        vt.grid().visible_row(0)[0].style,
        Default::default(),
        "precondition: the pen reached the cells"
    );
    vt.process(DECALN);
    assert_screen_is_alignment_pattern(vt.grid(), "under heavy SGR");
}

/// ...but DECALN does not RESET the SGR state either. Text written afterwards
/// still carries the pen the application set.
#[test]
fn decaln_does_not_reset_the_pen() {
    let mut vt = VirtualTerminal::new(3, 5);
    vt.process(b"\x1b[1;31m");
    vt.process(DECALN);
    vt.process(b"Z");
    let cell = &vt.grid().visible_row(0)[0];
    assert_eq!(cell.ch, 'Z');
    assert_eq!(cell.style.fg, Color::Indexed(1));
    assert!(cell.style.flags.contains(CellFlags::BOLD));
    // The neighbouring pattern cell stays unstyled.
    assert_eq!(vt.grid().visible_row(0)[1].style, Default::default());
}

/// The dynamic default background (OSC 11) is a terminal-level default, not a
/// cell attribute — DECALN leaves it exactly as it was, and the pattern cells
/// resolve against it like any other default-background cell.
#[test]
fn decaln_leaves_dynamic_default_colors_alone() {
    let mut vt = VirtualTerminal::new(3, 5);
    vt.process(b"\x1b]11;#112233\x1b\\");
    let before = vt.default_colors();
    vt.process(DECALN);
    assert_eq!(vt.default_colors(), before);
    assert_screen_is_alignment_pattern(vt.grid(), "with OSC 11 set");
}

/// The pattern is a fixed pattern, not a printed character: a designated
/// alternate character set must not translate it. With DEC Special Graphics
/// designated into G0, a printed `E` becomes a plus-minus-ish glyph — the
/// alignment pattern must still be `E`.
#[test]
fn decaln_is_not_translated_by_the_active_charset() {
    let mut vt = VirtualTerminal::new(3, 5);
    vt.process(b"\x1b(0"); // DEC Special Graphics into G0
    let translated = {
        let mut probe = VirtualTerminal::new(1, 1);
        probe.process(b"\x1b(0E");
        probe.grid().visible_row(0)[0].ch
    };
    vt.process(DECALN);
    assert_screen_is_alignment_pattern(vt.grid(), "with DEC graphics designated");
    // Sanity: the probe really did translate, so this test cannot pass because
    // the charset happened to be a no-op for `E`.
    if translated == 'E' {
        // DEC graphics maps only 0x5f..0x7e; `E` is outside it. Assert the
        // charset is live via a character that IS mapped, so the test still
        // proves the designation took effect.
        let mut probe = VirtualTerminal::new(1, 1);
        probe.process(b"\x1b(0q");
        assert_ne!(probe.grid().visible_row(0)[0].ch, 'q', "charset not active");
    }
}

/// Extended attributes (OSC 8 hyperlinks, underline colour) are heap payloads
/// hanging off a cell. The pattern must drop them, not inherit them.
#[test]
fn decaln_drops_extended_cell_attributes() {
    let mut vt = VirtualTerminal::new(3, 6);
    vt.process(b"\x1b]8;;https://example.invalid/very/long\x1b\\link\x1b]8;;\x1b\\");
    vt.process(b"\x1b[58;2;1;2;3;4:3m");
    assert!(
        vt.grid().visible_row(0)[0].extended.is_some(),
        "precondition: the link cell carries extended attrs"
    );
    vt.process(DECALN);
    assert_screen_is_alignment_pattern(vt.grid(), "over hyperlinked cells");
}

// ---------------------------------------------------------------------------
// 5. Grid invariants the fill must not break
// ---------------------------------------------------------------------------

/// A wide character occupies a lead cell and a continuation cell. The fill
/// writes single-width `E` over both, so no orphan continuation cell may be
/// left behind — an orphan reads back as a zero-width hole in capture and
/// rasterization.
#[test]
fn decaln_leaves_no_orphan_wide_continuation_cells() {
    let mut vt = VirtualTerminal::new(3, 7);
    vt.process("日本語x".as_bytes()); // 3 wide + 1 narrow = 7 columns
    assert!(
        (0..7).any(|c| vt.grid().visible_row(0)[c].is_wide_continuation()),
        "precondition: wide pairs on the row"
    );
    vt.process(DECALN);
    assert_screen_is_alignment_pattern(vt.grid(), "over wide characters");
}

/// A wide character straddling the last column and a soft-wrapped row are the
/// two ways a row carries state outside its cells. Both must be cleared, or a
/// later resize reflows rows that DECALN made independent back together.
#[test]
fn decaln_clears_soft_wrap_flags_so_reflow_does_not_join_rows() {
    let mut vt = VirtualTerminal::new(3, 4);
    vt.process(b"abcdefgh"); // soft-wraps across rows 0 and 1
    assert!(
        vt.grid().visible_row(0).wrapped,
        "precondition: row 0 is soft-wrapped"
    );
    vt.process(DECALN);
    assert_screen_is_alignment_pattern(vt.grid(), "over soft-wrapped rows");
    vt.resize(3, 8);
    let rows = visible_rows(vt.grid());
    assert!(
        rows.iter().all(|r| r == "EEEE    " || r == "        "),
        "reflow joined DECALN rows: {rows:?}"
    );
}

/// A combining mark arriving right after DECALN must not reach back into a
/// cell the fill has just rewritten.
#[test]
fn decaln_ends_the_active_grapheme_cell() {
    let mut vt = VirtualTerminal::new(3, 5);
    vt.process("e".as_bytes());
    vt.process(DECALN);
    vt.process("\u{0301}".as_bytes()); // combining acute
    assert_screen_is_alignment_pattern(vt.grid(), "after a stray combining mark");
}

/// History is not part of the page. DECALN fills the viewport and leaves
/// scrollback untouched.
#[test]
fn decaln_does_not_touch_scrollback() {
    let mut vt = VirtualTerminal::new(3, 8);
    for i in 0..6 {
        vt.process(format!("hist-{i}\r\n").as_bytes());
    }
    let before = scrollback_lines(vt.grid());
    assert!(!before.is_empty(), "precondition: history exists");
    vt.process(DECALN);
    assert_eq!(scrollback_lines(vt.grid()), before);
    assert_screen_is_alignment_pattern(vt.grid(), "with history behind it");
}

// ---------------------------------------------------------------------------
// 6. Change notification: the fill is a content mutation
// ---------------------------------------------------------------------------

/// A renderer that drained dirty regions before DECALN must be told the whole
/// viewport changed, or the screen shows stale content until something else
/// happens to repaint it.
#[test]
fn decaln_marks_the_whole_viewport_dirty() {
    let mut vt = VirtualTerminal::new(5, 9);
    vt.take_dirty_regions();
    assert!(!vt.is_dirty(), "precondition: drained");
    vt.process(DECALN);
    assert!(vt.is_dirty(), "DECALN did not dirty the viewport");
    let regions = vt.take_dirty_regions();
    for r in 0..5 {
        let covered = regions
            .iter()
            .any(|d| d.row == r && d.cols.start == 0 && d.cols.end >= 9);
        assert!(covered, "row {r} not fully reported dirty: {regions:?}");
    }
}

/// `ContentRevision` is what `pane.wait_settled` and every watcher key off.
/// A full-screen repaint that does not advance it is invisible to them.
#[test]
fn decaln_advances_the_content_revision() {
    let mut vt = VirtualTerminal::new(4, 6);
    let before = vt.content_revision();
    vt.process(DECALN);
    assert!(
        vt.content_revision() > before,
        "content revision stuck at {before}"
    );
}

/// The write tally is the cheap stand-in for "this buffer has been drawn on".
/// Issue #106's alternate-screen recycling reads it directly.
#[test]
fn decaln_advances_the_grid_write_tally() {
    let mut vt = VirtualTerminal::new(4, 6);
    let before = vt.grid().mutations();
    vt.process(DECALN);
    assert!(
        vt.grid().mutations() > before,
        "write tally stuck at {before}: a retired buffer full of `E` would be recycled as blank"
    );
}

// ---------------------------------------------------------------------------
// 7. Alternate screen — including the cross-pane content leak
// ---------------------------------------------------------------------------

/// DECALN fills whichever screen is live. The parked primary screen keeps its
/// content.
#[test]
fn decaln_fills_the_alternate_screen_only() {
    let mut vt = VirtualTerminal::new(4, 8);
    vt.process(b"primary!");
    vt.process(b"\x1b[?1049h");
    vt.process(DECALN);
    assert_screen_is_alignment_pattern(vt.grid(), "on the alternate screen");
    vt.process(b"\x1b[?1049l");
    assert_eq!(
        row_chars(vt.grid(), 0),
        "primary!",
        "the primary screen was overwritten through the alternate one"
    );
}

/// **The content leak.** Leaving the alternate screen parks the retired buffer
/// in a one-slot spare, and the next entry takes it back — reusing it as-is
/// when the write tally says nothing was ever drawn on it (issue #106). A
/// DECALN that fills cells without advancing that tally hands the next
/// application a screen full of `E` it never drew.
///
/// The same slot is shared across the pane's lifetime, so the next application
/// is routinely a different program than the one that drew.
#[test]
fn a_retired_alternate_screen_filled_by_decaln_is_not_recycled_as_blank() {
    let mut vt = VirtualTerminal::new(5, 10);
    vt.process(b"\x1b[?1049h");
    vt.process(DECALN);
    assert_screen_is_alignment_pattern(vt.grid(), "alt screen before retiring");
    vt.process(b"\x1b[?1049l");
    // A second application enters the alternate screen and draws one line.
    vt.process(b"\x1b[?1049h");
    let rows = visible_rows(vt.grid());
    assert!(
        rows.iter().all(|r| r.trim().is_empty()),
        "recycled alternate screen still shows the previous application's pattern: {rows:?}"
    );
}

/// The same leak with content drawn on top: entering, DECALN, drawing, leaving
/// and re-entering repeatedly must always present a blank canvas.
#[test]
fn repeated_alternate_screen_cycles_with_decaln_always_start_blank() {
    let mut vt = VirtualTerminal::new(4, 12);
    for i in 0..8 {
        vt.process(b"\x1b[?1049h");
        let rows = visible_rows(vt.grid());
        assert!(
            rows.iter().all(|r| r.trim().is_empty()),
            "cycle {i}: entered a dirty alternate screen: {rows:?}"
        );
        vt.process(DECALN);
        vt.process(format!("\x1b[2;1Hcycle-{i}").as_bytes());
        vt.process(b"\x1b[?1049l");
    }
}

// ---------------------------------------------------------------------------
// 8. Synchronized output (issue #115) — the freeze must survive the fill
// ---------------------------------------------------------------------------

/// `CSI ?2026h` promises the presented frame does not change until `?2026l`.
/// DECALN is a full-screen write; taken through the copy-on-write row sharing
/// it must copy before it writes, leaving the frozen frame untouched.
#[test]
fn decaln_inside_a_sync_window_does_not_disturb_the_frozen_frame() {
    let mut vt = VirtualTerminal::new(4, 10);
    vt.process(b"\x1b[1;1Hbefore-one\x1b[2;1Hbefore-two");
    let frozen: Vec<String> = visible_rows(vt.grid());

    vt.process(b"\x1b[?2026h");
    vt.process(DECALN);
    assert_eq!(
        visible_rows(vt.grid()),
        frozen,
        "the presented frame changed inside a synchronized-output window"
    );

    vt.process(b"\x1b[?2026l");
    assert_screen_is_alignment_pattern(vt.grid(), "after the window closed");
}

/// A grid clone held outside any sync window (what `pane.snapshot` hands the
/// rasterizer) shares rows with the live grid until something writes. DECALN
/// writing through that sharing would rewrite the snapshot in place.
#[test]
fn decaln_does_not_write_into_a_held_grid_clone() {
    let mut vt = VirtualTerminal::new(4, 12);
    vt.process(b"\x1b[1;1HHELD-CONTENT\x1b[2;1Hsecond-line");
    let held = vt.grid().clone();
    let held_before = visible_rows(&held);
    vt.process(DECALN);
    assert_eq!(
        visible_rows(&held),
        held_before,
        "DECALN wrote through a held clone"
    );
    assert_screen_is_alignment_pattern(vt.grid(), "live grid after the fill");
}

// ---------------------------------------------------------------------------
// 9. The sequence space around `ESC # 8`
// ---------------------------------------------------------------------------

/// `ESC 8` is DECRC. Adding a `#`-intermediate arm must not shadow it.
#[test]
fn plain_esc_8_is_still_decrc() {
    let mut vt = VirtualTerminal::new(5, 10);
    vt.process(b"\x1b[3;5H\x1b7\x1b[1;1H");
    vt.process(b"\x1b8");
    assert_eq!((vt.cursor().row, vt.cursor().col), (2, 4));
    assert!(
        visible_rows(vt.grid()).iter().all(|r| r.trim().is_empty()),
        "DECRC filled the screen"
    );
}

/// The other `ESC #` sequences are line-attribute controls (DECDHL/DECSWL/
/// DECDWL). shux does not implement them; none of them may fill the screen.
#[test]
fn other_hash_sequences_do_not_fill_the_screen() {
    for seq in [
        &b"\x1b#3"[..], // DECDHL top half
        b"\x1b#4",      // DECDHL bottom half
        b"\x1b#5",      // DECSWL
        b"\x1b#6",      // DECDWL
        b"\x1b#0",
        b"\x1b#9",
        b"\x1b#7",
    ] {
        let mut vt = VirtualTerminal::new(3, 5);
        vt.process(seq);
        let rows = visible_rows(vt.grid());
        assert!(
            rows.iter().all(|r| r.trim().is_empty()),
            "{seq:?} filled the screen: {rows:?}"
        );
    }
}

/// `CSI # 8` is not DECALN — the `#` has to be an ESC intermediate.
#[test]
fn csi_hash_8_is_not_decaln() {
    let mut vt = VirtualTerminal::new(3, 5);
    vt.process(b"\x1b[#8");
    let rows = visible_rows(vt.grid());
    assert!(
        rows.iter().all(|r| r.trim().is_empty()),
        "CSI #8 filled the screen: {rows:?}"
    );
}

/// A `#` intermediate followed by `8` where the `8` came from a *parameter*
/// position, and other near-misses, must not fill either.
#[test]
fn near_miss_sequences_do_not_fill_the_screen() {
    for seq in [
        &b"#8"[..],                  // no ESC at all
        b"\x1b#",                    // truncated
        b"\x1b(8",                   // charset designation
        b"\x1b)8",                   // charset designation
        b"\x1b%8",                   // other intermediate
        b"\x1b# 8",                  // extra intermediate
        b"\x1b\x1b#8"[..3].as_ref(), // ESC ESC # -- cancelled, then truncated
    ] {
        let mut vt = VirtualTerminal::new(3, 5);
        vt.process(seq);
        let rows = visible_rows(vt.grid());
        assert!(
            rows.iter().all(|r| !r.contains("EEEEE")),
            "{seq:?} filled the screen: {rows:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 10. RIS and repeated fills
// ---------------------------------------------------------------------------

/// `ESC c` after DECALN clears the pattern — the alignment test is not sticky.
#[test]
fn ris_clears_the_alignment_pattern() {
    let mut vt = VirtualTerminal::new(4, 6);
    vt.process(DECALN);
    vt.process(b"\x1bc");
    let rows = visible_rows(vt.grid());
    assert!(
        rows.iter().all(|r| r.trim().is_empty()),
        "RIS left the pattern: {rows:?}"
    );
}

/// Repeating the sequence is idempotent and does not grow anything.
#[test]
fn repeated_decaln_is_idempotent() {
    let mut vt = VirtualTerminal::new(4, 6);
    vt.process(DECALN);
    let after_one = visible_rows(vt.grid());
    let history_after_one = vt.grid().total_lines();
    for _ in 0..64 {
        vt.process(DECALN);
    }
    assert_eq!(visible_rows(vt.grid()), after_one);
    assert_eq!(vt.grid().total_lines(), history_after_one);
    assert_screen_is_alignment_pattern(vt.grid(), "after 65 fills");
}

/// A resize after DECALN keeps the pattern in the rows that survive, and the
/// grid stays self-consistent.
#[test]
fn decaln_then_resize_keeps_a_consistent_grid() {
    for (rows, cols) in [(2usize, 3usize), (10, 40), (6, 6), (1, 1)] {
        let mut vt = VirtualTerminal::new(5, 12);
        vt.process(DECALN);
        vt.resize(rows, cols);
        assert_eq!(vt.grid().rows(), rows);
        assert_eq!(vt.grid().cols(), cols);
        for r in 0..rows {
            let row = vt.grid().visible_row(r);
            assert_eq!(row.len(), cols, "{rows}x{cols}: row {r} width after resize");
            for c in 0..cols {
                let ch = row[c].ch;
                assert!(
                    ch == 'E' || ch == ' ',
                    "{rows}x{cols}: cell ({r},{c}) is {ch:?}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 11. What a consumer sees
// ---------------------------------------------------------------------------

/// `capture_text` is what `pane capture` and the agent-facing lens return.
#[test]
fn capture_text_shows_the_pattern() {
    let mut vt = VirtualTerminal::new(3, 5);
    vt.process(DECALN);
    let text = vt.capture_text(None);
    assert_eq!(
        text.lines().collect::<Vec<_>>(),
        vec!["EEEEE"; 3],
        "{text:?}"
    );
}

/// `glance_text` pads every row to the full width, so the pattern must be the
/// full width too.
#[test]
fn glance_text_shows_a_full_width_pattern() {
    let mut vt = VirtualTerminal::new(3, 5);
    vt.process(DECALN);
    let text = vt.grid().clone_visible().glance_text();
    assert_eq!(text, "EEEEE\nEEEEE\nEEEEE");
}

// ---------------------------------------------------------------------------
// 12. The property: DECALN normalises whatever came before it
// ---------------------------------------------------------------------------

/// Everything the individual tests above pin, stated once over arbitrary
/// programs: after `ESC # 8`, the page is the pattern, the margins are the
/// extremes of the page and the cursor is at home — no matter what state the
/// pane drove itself into first.
///
/// The alphabet is weighted towards what shares machinery with the fill:
/// styling, wide characters, combining marks, scroll regions, origin mode,
/// alternate-screen switches, soft wraps and synchronized-output windows.
mod properties {
    use super::*;
    use proptest::prelude::*;

    const OPS: &[&[u8]] = &[
        b"\x1b[1;4;7;38;2;9;9;9;48;5;42m",        // a loud pen
        b"\x1b[0m",                               // and back
        b"\x1b[2;5r",                             // scroll region
        b"\x1b[?6h",                              // origin mode on
        b"\x1b[?6l",                              // origin mode off
        b"\x1b[?1049h",                           // alternate screen
        b"\x1b[?1049l",                           // back
        b"\x1b[?2026h",                           // synchronized output
        b"\x1b[?2026l",                           // released
        b"\x1b[3;4H",                             // cursor position
        b"\x1b7",                                 // DECSC
        b"\x1b8",                                 // DECRC
        b"\x1b(0",                                // DEC graphics into G0
        b"\x1b(B",                                // ASCII back
        b"\x1b]8;;https://example.invalid\x1b\\", // open a hyperlink
        b"\x1b]8;;\x1b\\",                        // close it
        b"\x1b]11;#204060\x1b\\",                 // dynamic default bg
        b"\x1b[2J",                               // erase display
        b"\x1b[3S",                               // scroll up
        b"\x1b[2T",                               // scroll down
        b"\x1b[4L",                               // insert lines
        b"\x1b[2M",                               // delete lines
        b"\x1b[5@",                               // insert chars
        b"\x1b[3P",                               // delete chars
        b"\x1bM",                                 // reverse index
        b"\r\n",
        b"\t",
        b"wrap-me-past-the-margin-and-then-some", // forces soft wraps
        "\u{65}\u{0301}\u{4F60}\u{597D}".as_bytes(), // combining mark + wide
        b"\x1b#8",                                // DECALN itself, mid-program
    ];

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 256,
            failure_persistence: None,
            .. ProptestConfig::default()
        })]

        #[test]
        fn decaln_normalises_any_preceding_program(
            rows in 1usize..9,
            cols in 1usize..13,
            program in proptest::collection::vec(0usize..OPS.len(), 0..24),
            chunk in 1usize..7,
        ) {
            let mut vt = VirtualTerminal::new(rows, cols);
            for op in &program {
                // Chunked, because a PTY read boundary can fall anywhere.
                for piece in OPS[*op].chunks(chunk) {
                    vt.process(piece);
                }
            }
            // A synchronized-output window left open would keep the presented
            // frame frozen, which is correct but not what this property is
            // about; close it so `grid()` is the live frame.
            vt.process(b"\x1b[?2026l");
            vt.process(DECALN);

            let g = vt.grid();
            prop_assert_eq!(g.rows(), rows);
            prop_assert_eq!(g.cols(), cols);
            for r in 0..g.rows() {
                let row = g.visible_row(r);
                prop_assert!(!row.wrapped, "row {} still soft-wrapped", r);
                prop_assert_eq!(row.len(), cols);
                for c in 0..row.len() {
                    prop_assert_eq!(&row[c], &Cell::ALIGNMENT, "cell ({},{})", r, c);
                }
            }
            prop_assert_eq!((vt.cursor().row, vt.cursor().col), (0, 0));
            prop_assert!(!vt.cursor().auto_wrap_pending);
            prop_assert_eq!(vt.scroll_region().top, 0);
            prop_assert_eq!(vt.scroll_region().bottom, rows - 1);
        }
    }
}

// ---------------------------------------------------------------------------
// 13. Adversarial battery
// ---------------------------------------------------------------------------
//
// Written to break the fix rather than to demonstrate it, and kept because
// probing that finds nothing is only worth something if it leaves the probes
// behind. Every case here was run against the finished implementation; none of
// them found a defect, and each one is a way a plausible future change could
// introduce one.

/// The full negative space in one table: everything that looks like `ESC # 8`
/// without being it. The bar is "did not draw the alignment pattern" rather
/// than "screen is blank", because some of these legitimately print text —
/// `#8` with no ESC in front of it is two ordinary characters.
#[test]
fn the_whole_near_miss_table_leaves_the_screen_unfilled() {
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("ESC 8 (DECRC)", b"\x1b8".to_vec()),
        ("ESC # 3", b"\x1b#3".to_vec()),
        ("ESC # 4", b"\x1b#4".to_vec()),
        ("ESC # 5", b"\x1b#5".to_vec()),
        ("ESC # 6", b"\x1b#6".to_vec()),
        ("ESC # 0", b"\x1b#0".to_vec()),
        ("ESC # 7", b"\x1b#7".to_vec()),
        ("ESC # 9", b"\x1b#9".to_vec()),
        ("CSI # 8", b"\x1b[#8".to_vec()),
        ("CSI 8 #", b"\x1b[8#".to_vec()),
        ("ESC ( 8", b"\x1b(8".to_vec()),
        ("ESC ) 8", b"\x1b)8".to_vec()),
        ("ESC % 8", b"\x1b%8".to_vec()),
        ("ESC * 8", b"\x1b*8".to_vec()),
        ("ESC + 8", b"\x1b+8".to_vec()),
        ("ESC SP 8", b"\x1b 8".to_vec()),
        ("ESC # SP 8", b"\x1b# 8".to_vec()),
        ("bare #8", b"#8".to_vec()),
        ("ESC # truncated", b"\x1b#".to_vec()),
        ("ESC ESC # cancelled", b"\x1b\x1b#".to_vec()),
        ("CSI aborted by ESC", b"\x1b[1;2\x1b#".to_vec()),
        ("C1 0x9b # 8", b"\x9b#8".to_vec()),
        // Control-string payloads with NO embedded ESC are swallowed whole.
        ("DCS payload #8", b"\x1bP#8\x1b\\".to_vec()),
        ("OSC payload #8", b"\x1b]0;#8\x07".to_vec()),
        ("APC payload #8", b"\x1b_#8\x1b\\".to_vec()),
        ("PM payload #8", b"\x1b^#8\x1b\\".to_vec()),
        ("SOS payload #8", b"\x1bX#8\x1b\\".to_vec()),
    ];
    let mut bad = vec![];
    for (name, bytes) in &cases {
        let mut vt = VirtualTerminal::new(4, 8);
        vt.process(bytes);
        let rows = visible_rows(vt.grid());
        if rows.iter().any(|r| r.contains("EEEE")) {
            bad.push(format!("{name}: {rows:?}"));
        }
    }
    assert!(
        bad.is_empty(),
        "these filled the screen:\n{}",
        bad.join("\n")
    );
}

/// `ESC` inside a control string ABORTS the string — that is the VT500 state
/// machine, and it is not specific to DECALN: the same construction makes ED,
/// RIS and CUP fire too. So `ESC P ESC # 8 ESC \` legitimately fills the
/// screen, and this is pinned so that a future "harden the parser" change
/// cannot quietly make DECALN the one exception in either direction.
#[test]
fn esc_inside_a_control_string_aborts_it_and_decaln_then_fires() {
    for wrapper in [
        &b"\x1bP\x1b#8\x1b\\"[..],
        b"\x1b]0;\x1b#8\x07",
        b"\x1b_\x1b#8\x1b\\",
        b"\x1b^\x1b#8\x1b\\",
        b"\x1bX\x1b#8\x1b\\",
    ] {
        let mut vt = VirtualTerminal::new(3, 6);
        vt.process(wrapper);
        assert_screen_is_alignment_pattern(vt.grid(), &format!("{wrapper:?}"));
    }
}

/// Modes and designations are terminal state, not page content.
#[test]
fn decaln_preserves_modes_and_charset_designation() {
    let mut vt = VirtualTerminal::new(2, 4);
    vt.process(b"\x1b(0"); // DEC graphics into G0
    vt.process(DECALN);
    vt.process(b"\x1b[1;1Hq");
    assert_ne!(
        vt.grid().visible_row(0)[0].ch,
        'q',
        "charset designation lost across DECALN"
    );

    let mut vt = VirtualTerminal::new(2, 6);
    vt.process(b"\x1b[4h\x1b[?7l\x1b[?1002h\x1b[?2004h");
    vt.process(DECALN);
    assert!(vt.modes().insert_mode, "IRM reset by DECALN");
    assert!(!vt.modes().auto_wrap, "DECAWM reset by DECALN");
    assert!(
        vt.modes().bracketed_paste,
        "bracketed paste reset by DECALN"
    );
}

/// An application that asks where the cursor is right after the fill must be
/// told the home position — this is the reply an alignment test reads.
#[test]
fn a_cursor_position_report_after_decaln_says_row_one_column_one() {
    let mut vt = VirtualTerminal::new(10, 20);
    vt.process(b"\x1b[7;13H");
    let replies = vt.process_with_responses(b"\x1b#8\x1b[6n");
    let joined: Vec<String> = replies
        .iter()
        .map(|r| String::from_utf8_lossy(r).to_string())
        .collect();
    assert!(
        joined.iter().any(|r| r == "\x1b[1;1R"),
        "expected CPR 1;1, got {joined:?}"
    );
}

/// The alternate-screen recycle, across every mode number, every ordering, and
/// with a full reset thrown in: 18 permutations, two assertions each.
///
/// The FIRST assertion is the one that matters, and an earlier version of this
/// test did not have it. It checked only that the next application got a blank
/// screen — which is true even when the fill never reached an alternate screen
/// at all, because entering `?1049` always yields a fresh one. So it passed
/// while `?47` was silently destroying the primary screen. Adversarial review
/// caught what this test could not.
#[test]
fn no_alternate_screen_mode_combination_damages_the_primary_or_recycles_the_pattern() {
    let enters: [&[u8]; 3] = [b"\x1b[?1049h", b"\x1b[?1047h", b"\x1b[?47h"];
    let leaves: [&[u8]; 3] = [b"\x1b[?1049l", b"\x1b[?1047l", b"\x1b[?47l"];
    let mut bad = vec![];
    for (i, enter) in enters.iter().enumerate() {
        for (j, leave) in leaves.iter().enumerate() {
            for &ris in &[false, true] {
                let mut vt = VirtualTerminal::new(5, 10);
                vt.process(b"primary-x\r\n");
                let primary = visible_rows(vt.grid());
                vt.process(enter);
                vt.process(DECALN);
                if ris {
                    vt.process(b"\x1bc");
                }
                vt.process(leave);

                // 1. The primary screen survived. RIS is the one exception:
                //    clearing the primary screen is what RIS is for.
                if !ris && visible_rows(vt.grid()) != primary {
                    bad.push(format!(
                        "enter{i}/leave{j}: the fill reached the PRIMARY screen -> {:?}",
                        visible_rows(vt.grid())
                    ));
                }

                // 2. The next application gets a clean canvas.
                vt.process(enters[0]);
                let rows = visible_rows(vt.grid());
                if !rows.iter().all(|r| r.trim().is_empty()) {
                    bad.push(format!(
                        "enter{i}/leave{j}/ris={ris}: recycled dirty -> {rows:?}"
                    ));
                }
                vt.process(leaves[0]);
            }
        }
    }
    assert!(
        bad.is_empty(),
        "alternate-screen defects:\n{}",
        bad.join("\n")
    );
}

/// A resize landing between leaving and re-entering the alternate screen must
/// not let the retired buffer come back as a blank canvas with content on it.
#[test]
fn a_resize_between_alternate_screen_cycles_still_yields_a_clean_canvas() {
    for (rows, cols) in [(3usize, 6usize), (9, 20), (5, 10), (1, 1)] {
        let mut vt = VirtualTerminal::new(5, 10);
        vt.process(b"\x1b[?1049h");
        vt.process(DECALN);
        vt.process(b"\x1b[?1049l");
        vt.resize(rows, cols);
        vt.process(b"\x1b[?1049h");
        let seen = visible_rows(vt.grid());
        assert!(
            seen.iter().all(|r| r.trim().is_empty()),
            "resize to {rows}x{cols} then re-enter showed: {seen:?}"
        );
        vt.process(b"\x1b[?1049l");
    }
}

/// Five thousand fills in one write: bounded work, no growth, no panic.
#[test]
fn a_flood_of_decaln_grows_nothing() {
    let mut vt = VirtualTerminal::new(24, 80);
    let before_lines = vt.grid().total_lines();
    vt.process(&DECALN.repeat(5000));
    assert_eq!(
        vt.grid().total_lines(),
        before_lines,
        "the flood grew the grid"
    );
    assert_eq!(vt.grid().scrollback_len(), 0, "the flood created history");
    assert_screen_is_alignment_pattern(vt.grid(), "after 5000 fills");
}

/// Margins, origin mode and the save/restore slot in every order of three,
/// always ending in the fill: 216 programs, one invariant.
#[test]
fn decaln_normalises_every_ordering_of_margins_origin_and_save_restore() {
    let ops: [&[u8]; 6] = [
        b"\x1b[2;5r",
        b"\x1b[?6h",
        b"\x1b7",
        b"\x1b8",
        b"\x1b[?6l",
        b"\x1b#8",
    ];
    for a in 0..6 {
        for b in 0..6 {
            for c in 0..6 {
                let mut vt = VirtualTerminal::new(8, 10);
                vt.process(ops[a]);
                vt.process(ops[b]);
                vt.process(ops[c]);
                vt.process(DECALN);
                assert_screen_is_alignment_pattern(vt.grid(), &format!("{a},{b},{c}"));
                assert_eq!((vt.cursor().row, vt.cursor().col), (0, 0), "{a},{b},{c}");
                assert_eq!(vt.scroll_region().top, 0, "{a},{b},{c}");
                assert_eq!(vt.scroll_region().bottom, 7, "{a},{b},{c}");
            }
        }
    }
}

/// A synchronized window containing a fill, an alternate-screen switch and
/// another fill: the presented frame may not move until the window closes.
#[test]
fn a_sync_window_holds_through_fill_and_alternate_screen_switch() {
    let mut vt = VirtualTerminal::new(4, 10);
    vt.process(b"\x1b[1;1Hbefore-one\x1b[2;1Hbefore-two");
    let frozen = visible_rows(vt.grid());

    vt.process(b"\x1b[?2026h");
    vt.process(DECALN);
    assert_eq!(
        visible_rows(vt.grid()),
        frozen,
        "the fill moved the frozen frame"
    );
    vt.process(b"\x1b[?1049h");
    vt.process(DECALN);
    assert_eq!(
        visible_rows(vt.grid()),
        frozen,
        "alt + fill moved the frozen frame"
    );
    vt.process(b"\x1b[?1049l");
    assert_eq!(
        visible_rows(vt.grid()),
        frozen,
        "alt leave moved the frozen frame"
    );
    vt.process(b"\x1b[?2026l");
    assert_screen_is_alignment_pattern(vt.grid(), "after the window closed");
}

// ---------------------------------------------------------------------------
// 14. DECSET 47 — the older alternate-screen mode (found by adversarial review)
// ---------------------------------------------------------------------------
//
// `ESC[?47h` is the original xterm "use alternate screen buffer" mode, still
// emitted by applications built against pre-1049 terminfo (it is the old
// termcap `ti`/`te` pair). shux implemented `?1047` and `?1049` and let `?47`
// fall through unhandled, so a program that asked for the alternate screen the
// old way was silently drawing on the PRIMARY one and `?47l` restored nothing.
//
// That gap predates DECALN — plain text under `?47` corrupted the primary too
// — but DECALN is what turns it from "an application overwrote part of your
// screen" into "your whole page is gone and there is nothing to restore".

/// The reported case: a full-screen application takes the screen the old way,
/// runs the alignment test, and gives the screen back.
#[test]
fn mode_47_round_trip_leaves_the_primary_screen_intact() {
    let mut vt = VirtualTerminal::new(4, 12);
    vt.process(b"SECRET-LINE\r\nSECOND-LINE");
    let before = visible_rows(vt.grid());

    vt.process(b"\x1b[?47h");
    assert!(
        vt.is_alternate_screen(),
        "?47h did not enter the alternate screen"
    );
    vt.process(DECALN);
    assert_screen_is_alignment_pattern(vt.grid(), "the alternate screen under ?47");

    vt.process(b"\x1b[?47l");
    assert!(
        !vt.is_alternate_screen(),
        "?47l did not leave the alternate screen"
    );
    assert_eq!(
        visible_rows(vt.grid()),
        before,
        "the primary screen was destroyed by an alignment test run under ?47"
    );
}

/// `?47` carries the cursor across, like `?1047` and unlike `?1049`.
#[test]
fn mode_47_carries_the_cursor_across_like_1047() {
    let mut vt = VirtualTerminal::new(6, 10);
    vt.process(b"\x1b[4;5H");
    vt.process(b"\x1b[?47h");
    assert_eq!(
        (vt.cursor().row, vt.cursor().col),
        (3, 4),
        "?47h should not home or park the cursor"
    );
    vt.process(DECALN); // homes it
    vt.process(b"\x1b[?47l");
    assert_eq!(
        (vt.cursor().row, vt.cursor().col),
        (0, 0),
        "?47l should not restore a cursor it never saved"
    );
}

/// Ordinary content under `?47` must reach the alternate screen too — the fill
/// was only the loudest symptom.
#[test]
fn mode_47_keeps_ordinary_writes_off_the_primary_screen() {
    let mut vt = VirtualTerminal::new(4, 12);
    vt.process(b"SECRET-LINE\r\nSECOND-LINE");
    let before = visible_rows(vt.grid());
    vt.process(b"\x1b[?47h");
    vt.process(b"\x1b[1;1HOVERWRITTEN");
    vt.process(b"\x1b[?47l");
    assert_eq!(visible_rows(vt.grid()), before);
}

/// A retired `?47` alternate screen goes through the same one-slot spare as
/// `?1047`/`?1049`, so the fill must not survive into the next application.
#[test]
fn a_retired_mode_47_screen_filled_by_decaln_is_not_recycled_as_blank() {
    let mut vt = VirtualTerminal::new(5, 10);
    vt.process(b"\x1b[?47h");
    vt.process(DECALN);
    vt.process(b"\x1b[?47l");
    vt.process(b"\x1b[?1049h");
    let rows = visible_rows(vt.grid());
    assert!(
        rows.iter().all(|r| r.trim().is_empty()),
        "the pattern survived a ?47 retirement into the next application: {rows:?}"
    );
}

/// `DECRQM ?47` must report the mode's real state now that it has one.
#[test]
fn decrqm_reports_mode_47() {
    let mut vt = VirtualTerminal::new(3, 6);
    let replies = vt.process_with_responses(b"\x1b[?47$p");
    let joined: Vec<String> = replies
        .iter()
        .map(|r| String::from_utf8_lossy(r).to_string())
        .collect();
    assert!(
        joined.iter().any(|r| r == "\x1b[?47;2$y"),
        "expected ?47 reported as reset(2), got {joined:?}"
    );

    vt.process(b"\x1b[?47h");
    let replies = vt.process_with_responses(b"\x1b[?47$p");
    let joined: Vec<String> = replies
        .iter()
        .map(|r| String::from_utf8_lossy(r).to_string())
        .collect();
    assert!(
        joined.iter().any(|r| r == "\x1b[?47;1$y"),
        "expected ?47 reported as set(1), got {joined:?}"
    );
}
