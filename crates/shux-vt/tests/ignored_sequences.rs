//! A control sequence `vte` could not represent is discarded, not executed
//! (issue #126).
//!
//! `vte` raises its `ignore` flag when a sequence carries more parameters than
//! `MAX_PARAMS` (32, sub-parameters included) or more than two intermediates,
//! and hands over whatever survived the overflow. shux discards such a
//! sequence whole, as Alacritty does on the same `vte` release. Each test sends
//! the sequence at the limit, where it must still work, and past it, where it
//! must change nothing on screen. A discarded sequence still ends a grapheme
//! cluster, exactly as an unrecognised one does.

use shux_vt::{Color, VirtualTerminal};

/// `vte`'s `MAX_PARAMS`.
const MAX_PARAMS: usize = 32;

fn vt() -> VirtualTerminal {
    VirtualTerminal::new(6, 20)
}

fn row(t: &VirtualTerminal, r: usize) -> String {
    let row = t.grid().visible_row(r);
    let s: String = (0..row.len()).map(|c| row[c].ch).collect();
    s.trim_end().to_string()
}

/// `first`, padded with `1`s to exactly `count` parameters.
fn params(first: &str, count: usize) -> String {
    let mut s = first.to_string();
    for _ in 1..count {
        s.push_str(";1");
    }
    s
}

fn csi(first: &str, count: usize, tail: &str) -> Vec<u8> {
    format!("\x1b[{}{tail}", params(first, count)).into_bytes()
}

#[test]
fn rep_at_the_parameter_limit_repeats() {
    let mut t = vt();
    t.process(b"Q");
    t.process(&csi("3", MAX_PARAMS, "b"));
    assert_eq!(row(&t, 0), "QQQQ");
}

#[test]
fn rep_past_the_parameter_limit_is_discarded() {
    let mut t = vt();
    t.process(b"Q");
    t.process(&csi("3", MAX_PARAMS + 1, "b"));
    assert_eq!(row(&t, 0), "Q", "an overflowed REP executed");
    assert_eq!(t.cursor().col, 1);
}

/// The issue's own reproduction: forty parameters, final count 3.
#[test]
fn rep_from_the_issue_report_is_discarded() {
    let mut t = vt();
    let mut seq = String::from("Q\x1b[");
    for n in 1..40 {
        seq.push_str(&format!("{n};"));
    }
    seq.push_str("3b");
    t.process(seq.as_bytes());
    assert_eq!(row(&t, 0), "Q");
}

#[test]
fn a_discarded_sequence_does_not_swallow_what_follows() {
    let mut t = vt();
    t.process(b"Q");
    t.process(&csi("3", MAX_PARAMS + 1, "b"));
    t.process(b"R\x1b[2b");
    assert_eq!(row(&t, 0), "QRRR");
}

#[test]
fn erase_in_display_past_the_parameter_limit_is_discarded() {
    let mut at = vt();
    at.process(b"keep\r\n");
    at.process(&csi("2", MAX_PARAMS, "J"));
    assert_eq!(row(&at, 0), "", "ED at the limit must still erase");

    let mut past = vt();
    past.process(b"keep\r\n");
    past.process(&csi("2", MAX_PARAMS + 1, "J"));
    assert_eq!(row(&past, 0), "keep", "an overflowed ED erased the screen");
}

#[test]
fn set_scroll_region_past_the_parameter_limit_is_discarded() {
    let mut at = vt();
    at.process(&csi("2;4", MAX_PARAMS - 1, "r"));
    assert_eq!(
        (at.scroll_region().top, at.scroll_region().bottom),
        (1, 3),
        "DECSTBM at the limit must still set the region"
    );

    let mut past = vt();
    past.process(&csi("2;4", MAX_PARAMS, "r"));
    assert_eq!(
        (past.scroll_region().top, past.scroll_region().bottom),
        (0, 5),
        "an overflowed DECSTBM moved the margins"
    );
}

#[test]
fn sgr_past_the_parameter_limit_leaves_the_pen_alone() {
    let mut at = vt();
    at.process(&csi("31", MAX_PARAMS, "m"));
    at.process(b"X");
    assert_eq!(at.grid().visible_row(0)[0].style.fg, Color::Indexed(1));

    let mut past = vt();
    past.process(&csi("31", MAX_PARAMS + 1, "m"));
    past.process(b"X");
    assert_eq!(
        past.grid().visible_row(0)[0].style.fg,
        Color::Default,
        "an overflowed SGR changed the pen"
    );
}

/// Sub-parameters count toward the same limit, so a colon-separated overflow
/// is discarded exactly like a semicolon-separated one.
#[test]
fn subparameters_count_toward_the_limit() {
    let mut at = vt();
    let mut seq = String::from("\x1b[");
    for _ in 0..26 {
        seq.push_str("0;");
    }
    seq.push_str("38:2::255:0:0mX");
    at.process(seq.as_bytes());
    assert_eq!(
        at.grid().visible_row(0)[0].style.fg,
        Color::Rgb(255, 0, 0),
        "32 values with sub-parameters must still apply"
    );

    let mut t = vt();
    let mut seq = String::from("\x1b[38:2:255:0:0");
    for _ in 0..MAX_PARAMS {
        seq.push_str(":1");
    }
    seq.push_str("mX");
    t.process(seq.as_bytes());
    assert_eq!(t.grid().visible_row(0)[0].style.fg, Color::Default);
}

/// `vte` keeps the first two intermediates and drops the rest, so a third one
/// can leave behind a pair shux DOES handle. `CSI ? Ps $ $ p` arrives as
/// DECRQM with the ignore flag set.
#[test]
fn decrqm_with_a_third_intermediate_is_not_answered() {
    let mut t = vt();
    let answered = t.process_with_responses(b"\x1b[?2026$p");
    assert!(!answered.is_empty(), "DECRQM within the limit must answer");

    let mut t = vt();
    let answered = t.process_with_responses(b"\x1b[?2026$$p");
    assert!(
        answered.is_empty(),
        "an overflowed DECRQM answered: {answered:?}"
    );
}

/// XTGETTCAP for `TN`.
const XTGETTCAP_TN: &str = "+q544e\x1b\\";

#[test]
fn dcs_at_the_parameter_limit_is_answered() {
    let mut t = vt();
    let seq = format!("\x1bP{}{XTGETTCAP_TN}", params("1", MAX_PARAMS));
    let answered = t.process_with_responses(seq.as_bytes());
    assert!(!answered.is_empty(), "XTGETTCAP at the limit must answer");
}

#[test]
fn dcs_past_the_parameter_limit_is_discarded() {
    let mut t = vt();
    let seq = format!("\x1bP{}{XTGETTCAP_TN}", params("1", MAX_PARAMS + 1));
    let answered = t.process_with_responses(seq.as_bytes());
    assert!(
        answered.is_empty(),
        "an overflowed DCS was answered: {answered:?}"
    );
}

#[test]
fn a_discarded_dcs_does_not_poison_the_next_one() {
    let mut t = vt();
    let overflowed = format!("\x1bP{}{XTGETTCAP_TN}", params("1", MAX_PARAMS + 1));
    let seq = format!("{overflowed}A\x1bP{XTGETTCAP_TN}B");
    let answered = t.process_with_responses(seq.as_bytes());
    assert_eq!(answered.len(), 1, "exactly the valid query is answered");
    assert_eq!(row(&t, 0), "AB");
}

/// The cell at column 0 after `man ZWJ`, `middle`, `laptop`.
fn cluster_across(middle: &[u8]) -> (Option<String>, usize) {
    let mut t = vt();
    t.process("\u{1F468}\u{200D}".as_bytes());
    t.process(middle);
    t.process("\u{1F4BB}".as_bytes());
    let cell = &t.grid().visible_row(0)[0];
    (cell.grapheme().map(str::to_string), t.cursor().col)
}

#[test]
fn a_discarded_csi_ends_the_cluster_like_an_unrecognised_one() {
    assert_eq!(
        cluster_across(&csi("1", MAX_PARAMS + 1, "z")),
        cluster_across(b"\x1b[1z")
    );
}

#[test]
fn a_discarded_esc_ends_the_cluster_like_an_unrecognised_one() {
    assert_eq!(cluster_across(b"\x1b###9"), cluster_across(b"\x1b##9"));
}
