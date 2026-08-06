//! THROWAWAY minimiser — delete before finishing.
use shux_vt::VirtualTerminal;

fn show(tag: &str, prog: &[u8], rows: usize, cols: usize) {
    let mut vt = VirtualTerminal::new(rows, cols);
    vt.process(prog);
    let g = vt.grid();
    println!("--- {tag}");
    for r in 0..g.total_lines() {
        let row = g.row(r).expect("row");
        let mut line = String::new();
        for c in 0..row.len() {
            let cell = &row[c];
            line.push_str(&format!(
                "[{}{}]",
                cell.grapheme()
                    .map(|s| s.escape_unicode().to_string())
                    .unwrap_or_else(|| cell.ch.escape_unicode().to_string()),
                if cell.is_wide() {
                    "W"
                } else if cell.is_wide_continuation() {
                    "c"
                } else {
                    ""
                }
            ));
        }
        println!("r{r}: {line}");
    }
}

#[test]
fn minimal_dropped_wide_then_combining() {
    // DECAWM off, cols=2. `e` fills col0. The emoji has nowhere to go and is
    // dropped (blank at col1). VS16 then attaches to `e` at col0 -- so the
    // cluster REP records is `e VS16`, not the `emoji VS16` the stream carried.
    let base: &[u8] = b"\x1b[?7le\xf0\x9f\x98\x80\xef\xb8\x8f";
    let mut rep = base.to_vec();
    rep.extend_from_slice(b"\x1b[1b");
    let mut lit = base.to_vec();
    lit.extend_from_slice("\u{1f600}\u{fe0f}".as_bytes());
    show("REP(1)", &rep, 1, 2);
    show("literal +1", &lit, 1, 2);
}

#[test]
fn minimal_no_decawm_wide_at_margin() {
    // Same shape but with plain auto-wrap: does the emoji wrap and take the
    // VS16 with it?
    let base = "e\u{1f600}\u{fe0f}".as_bytes();
    let mut rep = base.to_vec();
    rep.extend_from_slice(b"\x1b[1b");
    let mut lit = base.to_vec();
    lit.extend_from_slice("\u{1f600}\u{fe0f}".as_bytes());
    show("REP(1) wrap", &rep, 2, 2);
    show("literal wrap", &lit, 2, 2);
}

#[test]
fn combining_after_a_cursor_move_hijacks_rep_source() {
    // No dropping at all: `Z` is the last graphic in the stream, the cursor
    // moves, a combining mark lands on `A`, and REP now repeats `A` + mark.
    let prog = b"ABCZ\x1b[1;2H\xcc\x81\x1b[3b";
    show("hijack", prog, 1, 10);
}
