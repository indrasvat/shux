//! DEL (0x7F) is a control character and leaves no trace on the screen
//! (issue #127).
//!
//! `vte` hands DEL to `print`, and `unicode-width` has no width for it. A
//! scalar without a width is not a glyph: it occupies no cell, moves no cursor,
//! is not what REP repeats, and never reaches `capture_text`. Alacritty, on the
//! same `vte` release, drops it at the same decision point.

use shux_vt::VirtualTerminal;

fn vt() -> VirtualTerminal {
    VirtualTerminal::new(4, 10)
}

fn row(t: &VirtualTerminal, r: usize) -> String {
    let row = t.grid().visible_row(r);
    let s: String = (0..row.len()).map(|c| row[c].ch).collect();
    s.trim_end().to_string()
}

#[test]
fn del_occupies_no_cell_and_moves_no_cursor() {
    let mut t = vt();
    t.process(b"A\x7fB");
    assert_eq!(row(&t, 0), "AB");
    assert_eq!(t.cursor().col, 2);
}

#[test]
fn del_is_never_what_rep_repeats() {
    let mut t = vt();
    t.process(b"A\x7f\x1b[2b");
    assert_eq!(row(&t, 0), "AAA");
}

/// A pending wrap at the right margin is resolved by the next GLYPH. DEL is not
/// one, so it must neither wrap the line nor cancel the pending wrap.
#[test]
fn del_at_the_right_margin_neither_wraps_nor_cancels_the_wrap() {
    let mut t = vt();
    t.process(b"0123456789\x7f");
    assert_eq!(row(&t, 1), "", "DEL wrapped the line");
    assert!(
        t.cursor().auto_wrap_pending,
        "DEL cancelled the pending wrap"
    );
    t.process(b"X");
    assert_eq!(row(&t, 1), "X");
}

/// DEL between a base and its combining mark is invisible to the cluster.
#[test]
fn del_does_not_split_a_grapheme_cluster() {
    let mut t = vt();
    t.process("e\x7f\u{301}".as_bytes());
    assert_eq!(t.grid().visible_row(0)[0].grapheme(), Some("e\u{301}"));
    assert_eq!(t.cursor().col, 1);
}

/// vte executes a UTF-8 C1 decoded in one read but prints one whose two bytes
/// arrive in separate reads. Both must end a grapheme cluster the same way.
#[test]
fn a_split_c1_control_ends_the_cluster_as_a_whole_one_does() {
    let zwj_then = |chunks: &[&[u8]]| {
        let mut t = vt();
        t.process("\u{1F468}\u{200D}".as_bytes());
        for c in chunks {
            t.process(c);
        }
        t.process("\u{1F4BB}".as_bytes());
        let cell = &t.grid().visible_row(0)[0];
        (cell.grapheme().map(str::to_string), t.cursor().col)
    };
    assert_eq!(zwj_then(&[b"\xc2", b"\x85"]), zwj_then(&[b"\xc2\x85"]));
}

fn hyperlink_at(t: &VirtualTerminal, col: usize) -> Option<String> {
    t.grid().visible_row(0)[col]
        .extended
        .as_ref()
        .and_then(|e| e.hyperlink.clone())
}

/// A URI is replayed verbatim to the terminal an operator attaches from, so one
/// carrying a control character is dropped rather than stored. Stripping would
/// store a link to somewhere the sender never named.
#[test]
fn a_hyperlink_carrying_a_control_character_is_dropped() {
    for uri in [
        &b"http://a\x7fb"[..],
        &b"http://a\xc2\x9b2J"[..],
        &b"http://a\xc2\x9cb"[..],
    ] {
        let mut t = vt();
        let mut seq = b"\x1b]8;;".to_vec();
        seq.extend_from_slice(uri);
        seq.extend_from_slice(b"\x1b\\L\x1b]8;;\x1b\\");
        t.process(&seq);
        assert_eq!(hyperlink_at(&t, 0), None, "stored {uri:?}");
        assert_eq!(t.grid().visible_row(0)[0].ch, 'L');
    }

    let mut t = vt();
    t.process(b"\x1b]8;;https://example.invalid/\xc3\xa9\x1b\\L\x1b]8;;\x1b\\");
    assert_eq!(
        hyperlink_at(&t, 0).as_deref(),
        Some("https://example.invalid/\u{e9}")
    );
}
