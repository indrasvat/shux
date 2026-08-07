//! REP — repeat the preceding character, `CSI Pn b` (issue #122).
//!
//! ECMA-48 §8.3.103 defines REP against the **data stream**: it repeats "the
//! preceding character in the data stream". shux derived it from the **screen**
//! instead — it re-read the cell to the LEFT OF THE CURSOR — which answers
//! correctly only while nothing has moved the cursor since the character was
//! printed. At column 0 there is no cell to the left at all, so `checked_sub(1)`
//! returned `None` and the repeat was dropped without a trace:
//!
//! ```text
//! X               print a graphic character
//! ESC[1;1H        home the cursor
//! ESC[3b          REP 3   ->  no-op, the screen still reads "X"
//!
//! X ESC[3b        (no cursor move)  ->  "XXXX", works
//! ```
//!
//! Column 0 is the loud case; the quiet one is worse. ANY cursor move between
//! the character and the `CSI b` made REP repeat whatever happened to be parked
//! to the left of wherever the cursor ended up — a blank, a wide-character
//! continuation, or a completely unrelated glyph from an earlier frame.
//!
//! ## The contract these tests hold the implementation to
//!
//! **REP(n) after a character C is exactly C sent n more times in the byte
//! stream.** That is the whole specification, and it is also the oracle the
//! property test at the bottom of this file uses: for a random program, the
//! stream ending in `CSI n b` must produce a byte-identical screen to the same
//! stream with the cluster written out n more times.
//!
//! Three consequences follow, and each is a separate way to get it wrong:
//!
//!   * the remembered character survives control sequences — cursor moves,
//!     erases, line feeds, scrolls, screen switches, pen changes;
//!   * the repeats are painted with the pen that is current at the `CSI b`, not
//!     the pen the original was painted with, because the pen belongs to the
//!     terminal and not to the remembered character; and
//!   * each repeat is an INDEPENDENT grapheme cluster. A source cluster that
//!     ends in a zero-width joiner, or a lone regional indicator, must not fuse
//!     with the repeat before it.
//!
//! **Zero-width scalars are the one precondition on that oracle.** A combining
//! mark extends the remembered character when it joins the character that was
//! just printed — so `e` + U+0301 then REP repeats `é`. A mark that lands
//! somewhere else does not. shux attaches a stray mark to whatever cell is left
//! of the cursor, which after a cursor move is a cell the data stream moved on
//! from long ago; letting that redefine "the preceding character" is how REP
//! came to repeat a character four positions back and overwrite three cells that
//! had nothing to do with it. So the rule is xterm's: **REP repeats the last
//! character that occupied at least one column**, together with the marks that
//! joined it. A mark that goes anywhere else is not repeatable, and for those
//! streams REP is deliberately not the same as re-sending the bytes.

use shux_vt::{Cell, Color, VirtualTerminal};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn vt(rows: usize, cols: usize) -> VirtualTerminal {
    VirtualTerminal::new(rows, cols)
}

/// One row rendered as base scalars, trailing blanks trimmed.
fn row(t: &VirtualTerminal, r: usize) -> String {
    let row = t.grid().visible_row(r);
    let s: String = (0..row.len()).map(|c| row[c].ch).collect();
    s.trim_end().to_string()
}

/// Every visible row, trailing blanks trimmed.
fn screen(t: &VirtualTerminal) -> Vec<String> {
    (0..t.grid().rows()).map(|r| row(t, r)).collect()
}

/// The full cell grid, which is what the differential oracle compares: base
/// scalar, grapheme payload, width, style and extended attributes all included.
fn cells(t: &VirtualTerminal) -> Vec<Vec<Cell>> {
    (0..t.grid().rows())
        .map(|r| {
            let row = t.grid().visible_row(r);
            (0..row.len()).map(|c| row[c].clone()).collect()
        })
        .collect()
}

/// Wide-cell pairing invariant: a width-2 head is always followed by exactly one
/// continuation, and a continuation is always preceded by a width-2 head. A
/// repeat that writes half a pair corrupts every consumer downstream — capture,
/// resize reflow, the rasterizer.
fn assert_wide_pairs_intact(t: &VirtualTerminal, what: &str) {
    for r in 0..t.grid().rows() {
        let row = t.grid().visible_row(r);
        for c in 0..row.len() {
            if row[c].is_wide_continuation() {
                assert!(
                    c > 0 && row[c - 1].is_wide(),
                    "{what}: orphan continuation at ({r},{c})"
                );
            }
            if row[c].is_wide() {
                assert!(
                    c + 1 < row.len() && row[c + 1].is_wide_continuation(),
                    "{what}: width-2 head at ({r},{c}) has no continuation"
                );
            }
        }
    }
}

/// The oracle. `prefix` is fed to both terminals; then one gets `CSI n b` and
/// the other gets `repeat` written out `n` times. The two screens must be
/// identical, cell for cell, and so must the cursor.
fn assert_rep_equals_literal(rows: usize, cols: usize, prefix: &[u8], repeat: &str, n: usize) {
    let mut with_rep = vt(rows, cols);
    with_rep.process(prefix);
    with_rep.process(format!("\x1b[{n}b").as_bytes());

    let mut literal = vt(rows, cols);
    literal.process(prefix);
    for _ in 0..n {
        literal.process(repeat.as_bytes());
    }

    let ctx = format!(
        "prefix={:?} repeat={:?} n={n} ({rows}x{cols})",
        String::from_utf8_lossy(prefix),
        repeat
    );
    assert_eq!(cells(&with_rep), cells(&literal), "screen differs: {ctx}");
    assert_eq!(
        (with_rep.cursor().row, with_rep.cursor().col),
        (literal.cursor().row, literal.cursor().col),
        "cursor differs: {ctx}"
    );
    assert_eq!(
        with_rep.cursor().auto_wrap_pending,
        literal.cursor().auto_wrap_pending,
        "pending wrap differs: {ctx}"
    );
}

// ---------------------------------------------------------------------------
// 1. The data stream, not the screen — the issue itself
// ---------------------------------------------------------------------------

/// The reproduction from issue #122, verbatim. Homing the cursor puts it at
/// column 0, where the old screen-derived source had nothing to read.
#[test]
fn rep_survives_homing_the_cursor() {
    let mut t = vt(4, 10);
    t.process(b"X\x1b[1;1H\x1b[3b");
    assert_eq!(row(&t, 0), "XXX", "REP after home was dropped");
}

/// Column 0 is the loud failure. The quiet one is any other cursor move: the
/// old code repeated whatever was parked to the left of the new position.
#[test]
fn rep_survives_a_cursor_move_within_the_line() {
    let mut t = vt(4, 20);
    t.process(b"X\x1b[1;10H\x1b[3b");
    assert_eq!(row(&t, 0), "X        XXX");
}

/// The cursor moved onto a row that holds unrelated text. The old code repeated
/// that unrelated glyph.
#[test]
fn rep_does_not_repeat_an_unrelated_neighbour() {
    let mut t = vt(4, 20);
    t.process(b"\x1b[2;1HQQQQ");
    t.process(b"\x1b[1;1HX");
    t.process(b"\x1b[2;5H\x1b[3b");
    assert_eq!(row(&t, 1), "QQQQXXX", "REP repeated the neighbour, not X");
}

#[test]
fn rep_survives_a_carriage_return_and_line_feed() {
    let mut t = vt(4, 10);
    t.process(b"ab\r\n\x1b[3b");
    assert_eq!(screen(&t)[..2], ["ab".to_string(), "bbb".to_string()]);
}

/// The character is gone from the screen entirely. It is still the preceding
/// character in the data stream.
#[test]
fn rep_survives_erasing_the_character_it_repeats() {
    let mut t = vt(4, 10);
    t.process(b"X\x1b[2J\x1b[1;1H\x1b[3b");
    assert_eq!(row(&t, 0), "XXX");
}

/// The character has scrolled off the visible page.
#[test]
fn rep_survives_scrolling_the_character_away() {
    let mut t = vt(3, 10);
    t.process(b"Z\r\n\r\n\r\n\r\n\x1b[2b");
    assert_eq!(row(&t, 2), "ZZ");
}

/// A full-screen repaint's worth of control sequences between the character and
/// the repeat: erases, cursor addressing, line inserts and deletes.
#[test]
fn rep_survives_a_screenful_of_control_sequences() {
    let mut t = vt(5, 10);
    t.process(b"\x1b[1;1H!");
    for r in 1..=5 {
        t.process(format!("\x1b[{r};1H\x1b[K\x1b[L\x1b[M\x1b[X").as_bytes());
    }
    t.process(b"\x1b[2J\x1b[1;1H\x1b[4b");
    assert_eq!(row(&t, 0), "!!!!");
}

#[test]
fn rep_survives_an_alternate_screen_round_trip() {
    let mut t = vt(4, 10);
    t.process(b"W");
    t.process(b"\x1b[?1049h");
    t.process(b"\x1b[?1049l");
    t.process(b"\x1b[1;1H\x1b[3b");
    assert_eq!(row(&t, 0), "WWW");
}

/// The remembered character is terminal state, so it is visible from the
/// alternate screen too — the data stream is one stream.
#[test]
fn rep_on_the_alternate_screen_repeats_the_primary_screens_character() {
    let mut t = vt(4, 10);
    t.process(b"P");
    t.process(b"\x1b[?1049h\x1b[3b");
    assert_eq!(row(&t, 0), "PPP");
}

/// The adjacent case has always worked and must keep working: it is by far the
/// most common way applications emit REP.
#[test]
fn rep_adjacent_to_its_character_still_works() {
    let mut t = vt(4, 10);
    t.process(b"X\x1b[3b");
    assert_eq!(row(&t, 0), "XXXX");
}

/// The interaction that surfaced the bug (issue #117): DECALN homes the cursor,
/// which put the following REP at column 0.
#[test]
fn rep_after_decaln_repeats_the_character_from_before_the_fill() {
    let mut t = vt(3, 6);
    t.process(b"\x1b[1;1Hq");
    t.process(b"\x1b#8");
    t.process(b"\x1b[3b");
    assert_eq!(
        row(&t, 0),
        "qqqEEE",
        "REP at the home position after DECALN"
    );
}

// ---------------------------------------------------------------------------
// 2. Nothing to repeat
// ---------------------------------------------------------------------------

/// A fresh terminal has no preceding character. REP must do nothing — not
/// repeat the blank the cursor happens to be sitting next to.
#[test]
fn rep_with_no_preceding_character_is_a_no_op() {
    let mut t = vt(4, 10);
    let before = cells(&t);
    t.process(b"\x1b[5b");
    assert_eq!(
        cells(&t),
        before,
        "REP wrote something with nothing to repeat"
    );
    assert_eq!((t.cursor().row, t.cursor().col), (0, 0));
}

/// Control sequences alone are not characters. A stream of nothing but escapes
/// leaves REP with nothing to repeat.
#[test]
fn rep_after_only_control_sequences_is_a_no_op() {
    let mut t = vt(4, 10);
    t.process(b"\x1b[2J\x1b[3;3H\x1b[31m\x1b[?25l\x1b[4;5r");
    let before = cells(&t);
    t.process(b"\x1b[9b");
    assert_eq!(cells(&t), before);
}

/// A combining mark with nothing to attach to is dropped from the stream, and
/// leaves nothing behind for REP either.
#[test]
fn rep_after_a_dropped_combining_mark_is_a_no_op() {
    let mut t = vt(4, 10);
    t.process("\u{0301}".as_bytes());
    let before = cells(&t);
    t.process(b"\x1b[4b");
    assert_eq!(cells(&t), before);
}

/// A combining mark with no cluster of its own attaches to whatever cell is left
/// of the cursor. After a cursor move that is a cell the data stream left behind
/// several characters ago — and letting it redefine the remembered character made
/// REP repeat that stale cell and overwrite everything between. The mark joins a
/// cell on the screen; it does not join the stream's current character.
#[test]
fn a_stray_combining_mark_does_not_change_what_rep_repeats() {
    let mut t = vt(2, 10);
    t.process("ABCZ\x1b[1;2H\u{0301}\x1b[3b".as_bytes());

    let row = t.grid().visible_row(0);
    assert_eq!(row[0].grapheme(), Some("A\u{0301}"), "the mark landed on A");
    assert_eq!(
        (row[1].ch, row[2].ch, row[3].ch),
        ('Z', 'Z', 'Z'),
        "REP repeated a character four back in the stream, not the last one"
    );
}

/// Same rule, reached without any cursor move. With auto-wrap off, a wide
/// character in the last column has nowhere to go and is dropped — which clears
/// the active cell, so the variation selector behind it falls back to an earlier
/// cell. The dropped character is still the last one the stream carried.
#[test]
fn a_mark_stranded_by_a_dropped_character_does_not_change_what_rep_repeats() {
    let mut t = vt(1, 2);
    t.process("\x1b[?7le\u{1F600}\u{FE0F}\x1b[1b".as_bytes());

    let row = t.grid().visible_row(0);
    assert_eq!(
        row[0].grapheme(),
        Some("e\u{FE0F}"),
        "the selector landed on e"
    );
    assert_eq!(
        row[1].ch, ' ',
        "REP repeated the stranded cell instead of the dropped character"
    );
}

/// RIS is a full reset: the terminal comes up as if it had just been switched
/// on, and a freshly switched-on terminal has no preceding character.
#[test]
fn ris_forgets_the_preceding_character() {
    let mut t = vt(4, 10);
    t.process(b"S\x1bc\x1b[3;7H");
    let before = cells(&t);
    let writes = t.grid().mutations();
    t.process(b"\x1b[5b");
    assert_eq!(cells(&t), before, "RIS left a character behind for REP");
    assert_eq!(
        t.grid().mutations(),
        writes,
        "REP wrote blanks after a full reset"
    );
    assert_eq!((t.cursor().row, t.cursor().col), (2, 6), "the cursor moved");
}

/// A resize is not a reset. The stream continues across it.
#[test]
fn resize_does_not_forget_the_preceding_character() {
    let mut t = vt(4, 10);
    t.process(b"R");
    t.resize(6, 20);
    t.process(b"\x1b[3;1H\x1b[3b");
    assert_eq!(row(&t, 2), "RRR");
}

// ---------------------------------------------------------------------------
// 3. The pen belongs to the terminal, not to the remembered character
// ---------------------------------------------------------------------------

fn fg_at(t: &VirtualTerminal, r: usize, c: usize) -> Color {
    t.grid().visible_row(r)[c].style.fg
}

/// The repeats are painted with the pen in effect at the `CSI b`.
///
/// This has always been true and is NOT part of the fix — the old code cloned the
/// source cell but took only its character, width and payload from it. It is
/// pinned because the new record stores no style at all, and a future change that
/// reintroduced one would break it silently.
#[test]
fn rep_paints_with_the_current_pen_not_the_originals() {
    let mut t = vt(2, 10);
    t.process(b"\x1b[31mX\x1b[32m\x1b[2b");
    assert_eq!(fg_at(&t, 0, 0), Color::Indexed(1), "original lost its red");
    assert_eq!(fg_at(&t, 0, 1), Color::Indexed(2), "repeat 1 is not green");
    assert_eq!(fg_at(&t, 0, 2), Color::Indexed(2), "repeat 2 is not green");
}

/// Truecolor and 256-indexed, so a repeat that silently downgrades colour depth
/// cannot pass. (CLAUDE.md: every colour-bearing check probes all three.)
#[test]
fn rep_carries_truecolor_and_indexed_pens() {
    let mut t = vt(2, 20);
    t.process(b"\x1b[38;2;120;220;180mT\x1b[1;1H\x1b[3b");
    for c in 0..3 {
        assert_eq!(fg_at(&t, 0, c), Color::Rgb(120, 220, 180), "cell {c}");
    }

    let mut t = vt(2, 20);
    t.process(b"\x1b[38;5;208mI\x1b[1;1H\x1b[3b");
    for c in 0..3 {
        assert_eq!(fg_at(&t, 0, c), Color::Indexed(208), "cell {c}");
    }
}

/// The background under the repeats comes from the current pen too — a repeat
/// with a default background where the pen says otherwise leaves a hole an
/// operator can see.
#[test]
fn rep_carries_the_current_background() {
    let mut t = vt(2, 10);
    t.process(b"A\x1b[44m\x1b[2b");
    assert_eq!(t.grid().visible_row(0)[0].style.bg, Color::Default);
    assert_eq!(t.grid().visible_row(0)[1].style.bg, Color::Indexed(4));
    assert_eq!(t.grid().visible_row(0)[2].style.bg, Color::Indexed(4));
}

/// A hyperlink is pen state (OSC 8 opens and closes it), so it applies to the
/// repeats exactly as it applies to typed text.
#[test]
fn rep_carries_the_current_hyperlink() {
    let mut t = vt(2, 20);
    t.process(b"L\x1b]8;;https://shux.dev\x1b\\\x1b[1;1H\x1b[3b");
    let row = t.grid().visible_row(0);
    for c in 0..3 {
        let link = row[c]
            .extended
            .as_ref()
            .and_then(|ext| ext.hyperlink.as_deref());
        assert_eq!(
            link,
            Some("https://shux.dev"),
            "cell {c} lost the current hyperlink"
        );
    }
}

/// SGR does not clear the remembered character. It is the one sequence the
/// parser deliberately does not treat as a grapheme break, so it is worth
/// pinning that REP still sees through it.
#[test]
fn rep_sees_through_a_bold_underline_reverse_run() {
    let mut t = vt(2, 20);
    t.process(b"B\x1b[1m\x1b[4m\x1b[7m\x1b[3b");
    assert_eq!(row(&t, 0), "BBBB");
}

// ---------------------------------------------------------------------------
// 4. Character sets
// ---------------------------------------------------------------------------

/// The remembered character is the one that was DISPLAYED, after charset
/// translation: `ESC ( 0 q` draws a horizontal line, so REP draws more line.
#[test]
fn rep_repeats_the_translated_glyph() {
    let mut t = vt(2, 20);
    t.process(b"\x1b(0q\x1b[1;1H\x1b[3b");
    assert_eq!(row(&t, 0), "───");
}

/// Switching back to ASCII after the line-drawing character does not
/// retranslate what was already drawn.
#[test]
fn rep_after_leaving_the_line_drawing_set_still_repeats_the_line() {
    let mut t = vt(2, 20);
    t.process(b"\x1b(0q\x1b(B\x1b[1;1H\x1b[3b");
    assert_eq!(row(&t, 0), "───");
}

/// The reverse: a plain `q` printed in ASCII stays a `q` even if the terminal
/// has since been switched into the line-drawing set.
#[test]
fn rep_after_entering_the_line_drawing_set_still_repeats_the_letter() {
    let mut t = vt(2, 20);
    t.process(b"q\x1b(0\x1b[1;1H\x1b[3b");
    assert_eq!(row(&t, 0), "qqq");
}

// ---------------------------------------------------------------------------
// 5. Grapheme clusters and wide characters
// ---------------------------------------------------------------------------

#[test]
fn rep_repeats_a_combining_cluster_as_one_unit() {
    let mut t = vt(2, 20);
    t.process("e\u{0301}\x1b[1;1H\x1b[3b".as_bytes());
    let row = t.grid().visible_row(0);
    for c in 0..3 {
        assert_eq!(row[c].grapheme(), Some("e\u{0301}"), "cell {c}");
    }
    assert_wide_pairs_intact(&t, "combining cluster");
}

/// A source cluster that ends in a zero-width joiner is an INCOMPLETE cluster:
/// the joiner is a promise that another scalar is coming. Repeating it is
/// therefore the same as sending those scalars again, and they fuse — which is
/// exactly what `a ZWJ a ZWJ a ZWJ a ZWJ` does when it arrives as data. What
/// must NOT happen is the old behaviour, where the fused result also swallowed
/// the repeat count: the screen and the cursor both have to land where the
/// literal stream leaves them.
#[test]
fn rep_of_a_zwj_terminated_cluster_matches_the_literal_stream() {
    assert_rep_equals_literal(2, 20, "a\u{200d}".as_bytes(), "a\u{200d}", 3);
    let mut t = vt(2, 20);
    t.process("a\u{200d}\x1b[3b".as_bytes());
    assert_eq!(
        t.grid().visible_row(0)[0].grapheme(),
        Some("a\u{200d}a\u{200d}a\u{200d}a\u{200d}"),
        "the repeats did not join the incomplete cluster"
    );
}

/// A lone regional indicator is half a flag. Two of them arriving in sequence
/// fuse into one, and a repeat is an arrival, so they fuse here too — three
/// repeats of one indicator are two flag cells, exactly as four indicators in
/// the stream would be. Wide pairing has to survive that.
#[test]
fn rep_of_a_lone_regional_indicator_matches_the_literal_stream() {
    assert_rep_equals_literal(2, 20, "\u{1F1FA}".as_bytes(), "\u{1F1FA}", 3);
    let mut t = vt(2, 20);
    t.process("\u{1F1FA}\x1b[3b".as_bytes());
    assert_wide_pairs_intact(&t, "lone regional indicator");
}

#[test]
fn rep_of_a_complete_flag_repeats_the_pair() {
    let mut t = vt(2, 20);
    t.process("\u{1F1FA}\u{1F1F8}\x1b[1;1H\x1b[3b".as_bytes());
    let row = t.grid().visible_row(0);
    for c in [0, 2, 4] {
        assert_eq!(row[c].grapheme(), Some("\u{1F1FA}\u{1F1F8}"), "cell {c}");
        assert!(row[c].is_wide(), "cell {c} lost width 2");
        assert!(row[c + 1].is_wide_continuation(), "cell {} ", c + 1);
    }
    assert_wide_pairs_intact(&t, "flag");
}

#[test]
fn rep_of_a_width_expanded_zwj_cluster_keeps_its_pairs() {
    let mut t = vt(2, 20);
    t.process("a\u{200d}\u{1F468}\x1b[1;1H\x1b[3b".as_bytes());
    let row = t.grid().visible_row(0);
    for c in [0, 2, 4] {
        assert_eq!(row[c].grapheme(), Some("a\u{200d}\u{1F468}"), "cell {c}");
        assert!(row[c].is_wide(), "cell {c} lost width 2");
        assert!(row[c + 1].is_wide_continuation(), "cell {}", c + 1);
    }
    assert_wide_pairs_intact(&t, "zwj cluster");
}

/// Documented consequence of a defect that lives elsewhere. shux tears an
/// incrementally built cluster in half at the right margin — `a` + ZWJ + emoji
/// started in the last column leaves `a` + ZWJ there and puts the emoji on the
/// next line — so the last cell the terminal actually holds contains the emoji
/// alone, and that is what REP goes on to repeat. The tearing reproduces with no
/// REP anywhere in the stream and belongs to the grapheme printing path; this
/// pins REP's side of it so a fix there shows up here rather than silently
/// changing what REP repeats.
#[test]
fn rep_after_a_cluster_torn_by_the_right_margin_repeats_the_surviving_half() {
    let mut t = vt(3, 5);
    t.process("\x1b[1;5Ha\u{200d}\u{1F468}".as_bytes());
    assert_eq!(
        t.grid().visible_row(0)[4].grapheme(),
        Some("a\u{200d}"),
        "precondition: the cluster is torn at the margin"
    );
    t.process(b"\x1b[2b");
    // Two more emoji: one fits beside the first, the second wraps whole.
    assert_eq!(t.grid().visible_row(1)[2].ch, '\u{1F468}');
    assert_eq!(t.grid().visible_row(2)[0].ch, '\u{1F468}');
    assert_wide_pairs_intact(&t, "torn cluster");
}

#[test]
fn rep_of_a_wide_character_keeps_wide_pairs() {
    let mut t = vt(2, 12);
    t.process("界\x1b[1;1H\x1b[3b".as_bytes());
    // `row()` renders the continuation half of a wide pair as a blank, so three
    // two-column glyphs read as "界 界 界".
    assert_eq!(row(&t, 0), "界 界 界");
    assert_wide_pairs_intact(&t, "wide char");
}

/// A wide repeat that will not fit in the last column wraps whole rather than
/// splitting across the line break.
#[test]
fn rep_of_a_wide_character_wraps_whole_at_the_right_edge() {
    let mut t = vt(3, 5);
    t.process("界\x1b[1;4H\x1b[2b".as_bytes());
    assert_wide_pairs_intact(&t, "wide wrap");
    assert_eq!(t.grid().visible_row(0)[3].ch, '界');
    assert_eq!(t.grid().visible_row(1)[0].ch, '界');
}

/// A wide character has nowhere to go on a one-column terminal. The repeat has
/// to behave exactly as another copy of the character in the stream would —
/// including doing the same harmless nothing.
#[test]
fn rep_of_a_wide_character_on_a_one_column_terminal_matches_the_literal() {
    assert_rep_equals_literal(3, 1, "界".as_bytes(), "界", 3);
}

// ---------------------------------------------------------------------------
// 6. Cursor, wrapping and scrolling
// ---------------------------------------------------------------------------

/// The character sits in the last column with a wrap pending. The repeats
/// continue onto the next row.
#[test]
fn rep_honours_a_pending_auto_wrap() {
    let mut t = vt(3, 2);
    t.process(b"\x1b[2GA\x1b[2b");
    assert_eq!(t.grid().visible_row(0)[1].ch, 'A');
    assert_eq!(row(&t, 1), "AA");
}

#[test]
fn rep_wraps_onto_following_lines() {
    let mut t = vt(4, 8);
    t.process(b"\x1b[1;1HX\x1b[12b");
    assert_eq!(row(&t, 0), "XXXXXXXX");
    assert_eq!(row(&t, 1), "XXXXX");
}

/// With auto-wrap off, a repeat that reaches the right margin keeps overwriting
/// the last column instead of spilling onto the next row.
#[test]
fn rep_with_autowrap_off_stays_on_the_last_column() {
    let mut t = vt(3, 5);
    t.process(b"\x1b[?7lX\x1b[10b");
    assert_eq!(row(&t, 0), "XXXXX");
    assert_eq!(row(&t, 1), "", "REP spilled past the right margin");
}

/// Repeats that run off the bottom of a scroll region scroll the region, not
/// the page.
#[test]
fn rep_scrolls_only_within_the_scroll_region() {
    let mut t = vt(6, 4);
    t.process(b"\x1b[1;1HTOP");
    t.process(b"\x1b[6;1HBOT");
    t.process(b"\x1b[3;5r");
    t.process(b"\x1b[3;1H#\x1b[40b");
    assert_eq!(
        row(&t, 0),
        "TOP",
        "the scroll region did not contain the repeat"
    );
    assert_eq!(row(&t, 5), "BOT");
}

/// Under origin mode the cursor is addressed relative to the scroll region, and
/// the repeats land there.
#[test]
fn rep_respects_origin_mode() {
    let mut t = vt(6, 6);
    t.process(b"M");
    t.process(b"\x1b[3;5r\x1b[?6h");
    t.process(b"\x1b[1;1H\x1b[3b");
    assert_eq!(
        row(&t, 2),
        "MMM",
        "origin-mode home is the top of the region"
    );
    assert_eq!(row(&t, 0), "M");
}

/// Insert mode shifts existing text right for each repeat, exactly as it would
/// for each typed character.
#[test]
fn rep_under_insert_mode_shifts_existing_text() {
    let mut t = vt(2, 10);
    t.process(b"\x1b[1;1HTAIL");
    t.process(b"\x1b[1;1H>");
    t.process(b"\x1b[4h\x1b[2b");
    // `>` OVERWROTE the `T` -- insert mode is only switched on afterwards -- so
    // the tail the two inserted repeats push right is `AIL`.
    assert_eq!(row(&t, 0), ">>>AIL");
}

/// The cursor ends where n more copies of the character would have left it.
#[test]
fn rep_leaves_the_cursor_where_the_literal_would() {
    for (n, expected) in [(1usize, 2usize), (3, 4), (7, 8)] {
        let mut t = vt(3, 20);
        t.process(b"\x1b[1;1Hx");
        t.process(format!("\x1b[{n}b").as_bytes());
        assert_eq!(t.cursor().col, expected, "n={n}");
    }
}

// ---------------------------------------------------------------------------
// 7. Counts, defaults and bounds
// ---------------------------------------------------------------------------

#[test]
fn rep_with_no_parameter_repeats_once() {
    let mut t = vt(2, 10);
    t.process(b"\x1b[1;1Hy\x1b[b");
    assert_eq!(row(&t, 0), "yy");
}

/// ECMA-48 gives REP a default of 1, and a parameter of 0 selects the default —
/// it is not "repeat zero times".
#[test]
fn rep_with_an_explicit_zero_repeats_once() {
    let mut t = vt(2, 10);
    t.process(b"\x1b[1;1Hy\x1b[0b");
    assert_eq!(row(&t, 0), "yy");
}

/// A repeat of one screenful is the largest that can leave a visible mark; past
/// that a pane could bill the daemon for arbitrary work with ten bytes
/// (issue #102). The clamp is on iterations and must survive the new source.
#[test]
fn rep_is_clamped_to_one_screenful() {
    let mut t = vt(6, 10);
    t.process(b"\x1b[1;1HC");
    let before = t.grid().mutations();
    t.process(b"\x1b[65535b");
    let did = t.grid().mutations() - before;
    let budget = (6 * 10) as u64 + 6 + 1;
    assert!(did <= budget, "REP 65535 did {did} writes; budget {budget}");
}

/// The clamp counts iterations, but a cluster source writes more per iteration.
#[test]
fn rep_is_clamped_for_cluster_sources_too() {
    for seed in ["C", "界", "a\u{200d}\u{1F468}", "e\u{0301}"] {
        let mut t = vt(6, 10);
        t.process(seed.as_bytes());
        t.process(b"\x1b[1;1H");
        let before = t.grid().mutations();
        t.process(b"\x1b[65535b");
        let did = t.grid().mutations() - before;
        let budget = 4 * (6 * 10) as u64;
        assert!(did <= budget, "{seed:?}: {did} writes, budget {budget}");
    }
}

/// The iteration clamp alone is not enough. A grapheme cluster carries up to 32
/// scalars, and each one is a write, so bounding only the number of COPIES still
/// let ten bytes buy 32 screenfuls of work. The total number of scalars written
/// is bounded too. Measured on this 6x10 pane: 93 writes with the bound, 1,865
/// without it.
#[test]
fn rep_bounds_the_scalars_it_writes_not_just_the_copies() {
    let mut seed = String::from("a");
    for _ in 0..15 {
        seed.push('\u{200d}');
        seed.push('b');
    }
    let mut t = vt(6, 10);
    t.process(seed.as_bytes());
    assert!(
        t.grid().visible_row(0)[0]
            .grapheme()
            .is_some_and(|g| g.chars().count() > 16),
        "precondition: the seed is a long cluster, not a single scalar"
    );

    t.process(b"\x1b[1;1H");
    let before = t.grid().mutations();
    t.process(b"\x1b[65535b");
    let did = t.grid().mutations() - before;
    let budget = 4 * (6 * 10) as u64;
    assert!(
        did <= budget,
        "REP of a {}-scalar cluster did {did} writes; budget {budget}",
        seed.chars().count()
    );
}

/// Repeating does not change WHICH character is the preceding one. `X REP2
/// REP2` is five `X`s, not three and then two of whatever the replay left
/// behind.
#[test]
fn a_repeat_is_still_the_same_preceding_character() {
    let mut t = vt(2, 20);
    t.process(b"X\x1b[2b\x1b[2b");
    assert_eq!(row(&t, 0), "XXXXX");
}

/// Same, for a cluster: the second REP must still see the whole cluster, not
/// just the base scalar the replay wrote first.
#[test]
fn a_cluster_repeat_is_still_the_whole_cluster() {
    let mut t = vt(2, 20);
    t.process("e\u{0301}\x1b[2b\x1b[1;1H\x1b[2b".as_bytes());
    let row = t.grid().visible_row(0);
    for c in 0..2 {
        assert_eq!(row[c].grapheme(), Some("e\u{0301}"), "cell {c}");
    }
}

/// The character printed most recently wins, including one printed by an
/// earlier REP's neighbour.
#[test]
fn rep_repeats_the_most_recent_character() {
    let mut t = vt(2, 20);
    t.process(b"A\x1b[2bB\x1b[1;1H\x1b[3b");
    assert_eq!(row(&t, 0), "BBBB");
}

// ---------------------------------------------------------------------------
// 8. The sequence space around `CSI b`
// ---------------------------------------------------------------------------

/// `CSI b` is REP only with no intermediate byte and no private marker.
/// Everything that merely looks like it must leave the screen alone.
#[test]
fn only_bare_csi_b_repeats() {
    let near_misses: &[&[u8]] = &[
        b"\x1b[?3b",
        b"\x1b[>3b",
        b"\x1b[<3b",
        b"\x1b[=3b",
        b"\x1b[3 b",
        b"\x1b[3$b",
        b"\x1b[3!b",
        b"\x1b[3\"b",
        b"\x1b[3#b",
        b"\x1bb",
        b"\x1b#b",
        b"\x1b(b",
    ];
    for seq in near_misses {
        let mut t = vt(3, 10);
        t.process(b"\x1b[1;1HN");
        t.process(b"\x1b[1;5H");
        let before = cells(&t);
        t.process(seq);
        assert_eq!(
            cells(&t),
            before,
            "{:?} repeated a character",
            String::from_utf8_lossy(seq)
        );
    }
}

/// A REP split across `process()` calls at every byte boundary is still one
/// REP. Applications write when their buffer fills, not when a sequence ends.
#[test]
fn rep_survives_being_split_across_reads() {
    let seq = b"\x1b[1;1HZ\x1b[4b";
    for split in 0..seq.len() {
        let mut t = vt(3, 10);
        t.process(&seq[..split]);
        t.process(&seq[split..]);
        assert_eq!(row(&t, 0), "ZZZZZ", "split at {split}");
    }
}

// ---------------------------------------------------------------------------
// 9. Grid invariants
// ---------------------------------------------------------------------------

/// A repeat is a content mutation: it advances the write tally that licenses
/// recycling a retired alternate-screen buffer as blank (issue #106).
#[test]
fn rep_advances_the_write_tally() {
    let mut t = vt(3, 10);
    t.process(b"T\x1b[1;1H");
    let before = t.grid().mutations();
    t.process(b"\x1b[3b");
    assert!(
        t.grid().mutations() > before,
        "REP wrote without a tally bump"
    );
}

/// A REP that lands on rows still shared with a synchronized-output freeze must
/// copy them first (issue #115) — the frozen frame is being read by `glance`.
#[test]
fn rep_inside_a_sync_window_does_not_disturb_the_frozen_frame() {
    let mut t = vt(3, 10);
    t.process(b"\x1b[1;1Hbase");
    t.process(b"\x1b[?2026h");
    let frozen: Vec<String> = screen(&t);
    t.process(b"\x1b[1;1H\x1b[6b");
    assert_eq!(screen(&t), frozen, "the frozen frame moved");
    t.process(b"\x1b[?2026l");
    assert_eq!(
        row(&t, 0),
        "eeeeee",
        "the repeat did not land after release"
    );
}

/// A held clone of the grid is a separate frame and must not change under a
/// later repeat.
#[test]
fn rep_does_not_write_through_a_held_grid_clone() {
    let mut t = vt(3, 10);
    t.process(b"\x1b[1;1Hhold");
    let held = t.grid().clone();
    let before: Vec<char> = (0..10).map(|c| held.visible_row(0)[c].ch).collect();
    t.process(b"\x1b[1;1H\x1b[8b");
    let after: Vec<char> = (0..10).map(|c| held.visible_row(0)[c].ch).collect();
    assert_eq!(before, after, "REP wrote through a shared row");
}

/// The rows a repeat touches are reported dirty, or the renderer never repaints
/// them.
#[test]
fn rep_marks_every_row_it_touches_dirty() {
    let mut t = vt(4, 8);
    t.process(b"\x1b[1;1HD");
    t.take_dirty_regions();
    t.process(b"\x1b[10b");
    let dirty: Vec<usize> = t.take_dirty_regions().iter().map(|r| r.row).collect();
    assert!(dirty.contains(&0), "row 0 not dirty: {dirty:?}");
    assert!(dirty.contains(&1), "row 1 not dirty: {dirty:?}");
}

// ---------------------------------------------------------------------------
// 10. Differential oracle: REP(n) == the character n more times
// ---------------------------------------------------------------------------

/// The whole specification, checked directly, over the interesting shapes.
#[test]
fn rep_matches_the_literal_stream_for_every_source_shape() {
    let sources: &[&str] = &[
        "X",
        " ",
        "界",
        "e\u{0301}",
        "a\u{200d}\u{1F468}",
        "\u{1F1FA}\u{1F1F8}",
        "\u{1F1FA}",
        "a\u{200d}",
    ];
    let prefixes: &[&[u8]] = &[
        b"",
        b"\x1b[1;1H",
        b"\x1b[2;3H",
        b"\x1b[31;44m",
        b"\x1b[4h",
        b"\x1b[?7l",
        b"\x1b[2;4r\x1b[?6h",
    ];
    for src in sources {
        for prefix in prefixes {
            for n in [1usize, 2, 5, 13] {
                let mut program = prefix.to_vec();
                program.extend_from_slice(src.as_bytes());
                assert_rep_equals_literal(6, 9, &program, src, n);
            }
        }
    }
}

/// The same oracle, with a control sequence wedged between the character and
/// the repeat. That gap is the entire bug: the screen-derived source lost the
/// character there, the data-stream source does not.
#[test]
fn rep_matches_the_literal_stream_across_an_intervening_sequence() {
    let gaps: &[&[u8]] = &[
        b"\x1b[1;1H",
        b"\x1b[H",
        b"\r",
        b"\r\n",
        b"\x1b[2J",
        b"\x1b[K",
        b"\x1b[5C",
        b"\x1b[3D",
        b"\x1b[2A",
        b"\x1b[2B",
        b"\x1b[32m",
        b"\x1b[?25l",
        b"\x1b7\x1b8",
        b"\x1b[?1049h\x1b[?1049l",
        b"\x1bM",
        b"\x1bD",
        b"\x1b[L",
        b"\x1b[M",
        b"\x1b[X",
        b"\x08",
        b"\t",
        b"\x1b]0;title\x07",
        b"\x1b[?2026h\x1b[?2026l",
    ];
    for src in ["X", "界", "e\u{0301}"] {
        for gap in gaps {
            let mut program = Vec::new();
            program.extend_from_slice(b"\x1b[2;2Hseed\x1b[1;1H");
            program.extend_from_slice(src.as_bytes());
            program.extend_from_slice(gap);
            assert_rep_equals_literal(6, 9, &program, src, 3);
        }
    }
}

// ---------------------------------------------------------------------------
// 11. Property: random programs, same oracle
// ---------------------------------------------------------------------------

mod properties {
    use super::*;

    /// A deterministic xorshift, so a failure is reproducible from its seed
    /// without pulling a generator crate into an integration test.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
            &items[(self.next() % items.len() as u64) as usize]
        }
    }

    /// Control sequences only — no printable characters, so the last graphic
    /// character in the generated program is always the one the oracle expects.
    ///
    /// Anything here may appear BEFORE the character. Only [`AFTER`] may appear
    /// between the character and the repeat.
    const BEFORE: &[&[u8]] = &[
        b"\x1b[H",
        b"\x1b[3;4H",
        b"\x1b[2J",
        b"\x1b[K",
        b"\x1b[1J",
        b"\x1b[2C",
        b"\x1b[1D",
        b"\x1b[1A",
        b"\x1b[1B",
        b"\r",
        b"\n",
        b"\x08",
        b"\t",
        b"\x1b[31m",
        b"\x1b[0m",
        b"\x1b[7m",
        b"\x1b[44m",
        b"\x1b[4h",
        b"\x1b[4l",
        b"\x1b[?7h",
        b"\x1b[?7l",
        b"\x1b[?6h",
        b"\x1b[?6l",
        b"\x1b[2;5r",
        b"\x1b[r",
        b"\x1b7",
        b"\x1b8",
        b"\x1bM",
        b"\x1bD",
        b"\x1bE",
        b"\x1b[L",
        b"\x1b[M",
        b"\x1b[P",
        b"\x1b[@",
        b"\x1b[X",
        b"\x1b[S",
        b"\x1b[T",
        b"\x1b[?1049h",
        b"\x1b[?1049l",
        b"\x1b[?25l",
        b"\x1b[?25h",
        b"\x1b#8",
        b"\x1b(0",
        b"\x1b(B",
        b"\x1b[?2026h",
        b"\x1b[?2026l",
    ];

    /// The noise that may sit between the character and the `CSI b`.
    ///
    /// It excludes the four sequences that change what a subsequent byte MEANS:
    /// `ESC ( 0` / `ESC ( B` designate a character set, and `ESC 7` / `ESC 8`
    /// save and restore one. The oracle's literal arm re-sends the character's
    /// bytes, so under a changed character set it would be sending a DIFFERENT
    /// character and the comparison would be measuring the charset rather than
    /// REP. shux remembers the character as PRINTED — a line-drawing glyph stays
    /// a line-drawing glyph — which is pinned by its own tests above.
    const AFTER: &[&[u8]] = &[
        b"\x1b[H",
        b"\x1b[3;4H",
        b"\x1b[2J",
        b"\x1b[K",
        b"\x1b[1J",
        b"\x1b[2C",
        b"\x1b[1D",
        b"\x1b[1A",
        b"\x1b[1B",
        b"\r",
        b"\n",
        b"\x08",
        b"\t",
        b"\x1b[31m",
        b"\x1b[0m",
        b"\x1b[7m",
        b"\x1b[44m",
        b"\x1b[4h",
        b"\x1b[4l",
        b"\x1b[?7h",
        b"\x1b[?7l",
        b"\x1b[?6h",
        b"\x1b[?6l",
        b"\x1b[2;5r",
        b"\x1b[r",
        b"\x1bM",
        b"\x1bD",
        b"\x1bE",
        b"\x1b[L",
        b"\x1b[M",
        b"\x1b[P",
        b"\x1b[@",
        b"\x1b[X",
        b"\x1b[S",
        b"\x1b[T",
        b"\x1b[?1049h",
        b"\x1b[?1049l",
        b"\x1b[?25l",
        b"\x1b[?25h",
        b"\x1b#8",
        b"\x1b[?2026h",
        b"\x1b[?2026l",
    ];

    const SOURCES: &[&str] = &[
        "A",
        " ",
        "~",
        "界",
        "e\u{0301}",
        "a\u{200d}\u{1F468}",
        "\u{1F1FA}\u{1F1F8}",
        "\u{1F1FA}",
        "a\u{200d}",
    ];

    /// 512 random programs: arbitrary control-sequence noise, then a character,
    /// then more noise, then `CSI n b`. Each must match the same program with
    /// the character written out n more times.
    ///
    /// The `CSI 1 G` before the character parks it at column 1, where a
    /// two-column cluster always fits. It is not there to make the test easier:
    /// shux tears an incrementally built cluster in half at the right margin —
    /// `a` + ZWJ + emoji started in the last column leaves `a` + ZWJ behind and
    /// puts the emoji on the next line — so the terminal's idea of "the
    /// preceding character" there is the emoji alone, and the oracle would be
    /// comparing against a cluster the terminal never actually held. That
    /// tearing happens with no REP anywhere in the stream and belongs to the
    /// grapheme printing path, not here. Repeats that run INTO the margin are
    /// unconstrained and still checked, which is the case REP itself owns.
    #[test]
    fn random_programs_match_the_literal_stream() {
        let mut rng = Rng(0x5EED_1122_3344_5566);
        for case in 0..512 {
            let src = *rng.pick(SOURCES);
            let mut program = Vec::new();
            for _ in 0..(rng.next() % 5) {
                program.extend_from_slice(rng.pick(BEFORE));
            }
            program.extend_from_slice(b"\x1b[1G");
            program.extend_from_slice(src.as_bytes());
            for _ in 0..(rng.next() % 5) {
                program.extend_from_slice(rng.pick(AFTER));
            }
            let n = 1 + (rng.next() % 6) as usize;
            let rows = 3 + (rng.next() % 4) as usize;
            let cols = 4 + (rng.next() % 8) as usize;
            let ctx = format!("case {case}");
            std::panic::catch_unwind(|| {
                assert_rep_equals_literal(rows, cols, &program, src, n);
            })
            .unwrap_or_else(|e| {
                eprintln!("{ctx}: program={:?}", String::from_utf8_lossy(&program));
                std::panic::resume_unwind(e)
            });
        }
    }

    /// The same programs chunked at random byte boundaries. A sequence split
    /// across two writes must behave identically to one delivered whole.
    #[test]
    fn chunked_delivery_matches_whole_delivery() {
        let mut rng = Rng(0xC0FF_EE00_1234_5678);
        for _ in 0..256 {
            let src = *rng.pick(SOURCES);
            let mut program = Vec::new();
            for _ in 0..(rng.next() % 4) {
                program.extend_from_slice(rng.pick(BEFORE));
            }
            program.extend_from_slice(b"\x1b[1G");
            program.extend_from_slice(src.as_bytes());
            for _ in 0..(rng.next() % 4) {
                program.extend_from_slice(rng.pick(AFTER));
            }
            let n = 1 + (rng.next() % 5) as usize;
            program.extend_from_slice(format!("\x1b[{n}b").as_bytes());

            let mut whole = vt(5, 7);
            whole.process(&program);

            let mut chunked = vt(5, 7);
            let mut at = 0;
            while at < program.len() {
                let take = 1 + (rng.next() % 4) as usize;
                let end = (at + take).min(program.len());
                chunked.process(&program[at..end]);
                at = end;
            }

            assert_eq!(
                cells(&whole),
                cells(&chunked),
                "chunking changed the result: {:?}",
                String::from_utf8_lossy(&program)
            );
        }
    }
}
