//! End-to-end: what a pane writes that is not a glyph or not a whole sequence
//! leaves no mark, through a real daemon and a real PTY (issues #126, #127).
//!
//! The VT-level suites are `crates/shux-vt/tests/ignored_sequences.rs` and
//! `crates/shux-vt/tests/del.rs`. This one drives the shipped `shux` binary, so
//! it sees what an operator sees: `pane capture` writes pane content straight to
//! their terminal, which is where a stored DEL used to arrive raw.

mod pane_e2e;

use pane_e2e::{COLOUR_PROBE, Env};

const COLS: usize = 40;
const ROWS: usize = 8;

/// `CSI 3 b` padded to forty parameters, past `vte`'s limit of 32.
fn overflowed_rep() -> String {
    let mut s = String::from("\\033[3");
    for _ in 1..40 {
        s.push_str(";1");
    }
    s.push('b');
    s
}

#[test]
fn a_real_pane_writing_del_captures_without_it() {
    let mut env = Env::new(COLS, ROWS);
    let pane = env.pane_running(
        "del",
        &format!(
            "printf '\\033[1;1H{COLOUR_PROBE}'\n\
             printf '\\033[3;1HA\\177B\\177C'\n\
             printf '\\033[5;1H%s\\177Y' \"$(printf 'x%.0s' $(seq 1 {COLS}))\"\n\
             printf '\\033[8;1HREADY'\n"
        ),
    );
    env.wait_for("del", &pane, "READY");

    let text = env.capture("del", &pane);
    assert!(
        !text.contains('\u{7f}'),
        "pane capture wrote DEL to the operator's terminal: {text:?}"
    );
    assert!(text.contains("ABC"), "{text:?}");
    for probe in ["TRUECOLOR", "INDEXED", "BASIC"] {
        assert!(text.contains(probe), "colour probe {probe} missing");
    }
    env.assert_colour_probe(&pane);

    let json = env.ok(&[
        "--format", "json", "pane", "capture", "-s", "del", "-p", &pane,
    ]);
    let v: serde_json::Value = serde_json::from_str(&json).expect("capture json");
    let text = v["result"]["text"]
        .as_str()
        .or_else(|| v["text"].as_str())
        .unwrap_or_else(|| panic!("no text in capture json: {json}"));
    assert!(text.contains("ABC") && !text.contains('\u{7f}'), "{text:?}");

    let rows = env.glance_rows(&pane);
    assert_eq!(rows[2].trim_end(), "ABC", "DEL occupied a column");
    assert_eq!(rows[4], "x".repeat(COLS), "DEL at the margin wrapped");
    assert_eq!(rows[5].trim_end(), "Y", "DEL cancelled the pending wrap");
}

#[test]
fn a_real_pane_sending_an_overflowed_sequence_has_it_discarded() {
    let mut env = Env::new(COLS, ROWS);
    let pane = env.pane_running(
        "overflow",
        &format!(
            "printf '\\033[1;1H{COLOUR_PROBE}'\n\
             printf '\\033[3;1HQ{rep}R\\033[2b'\n\
             printf '\\033[8;1HREADY'\n",
            rep = overflowed_rep()
        ),
    );
    env.wait_for("overflow", &pane, "READY");

    let rows = env.glance_rows(&pane);
    assert_eq!(
        rows[2].trim_end(),
        "QRRR",
        "the overflowed REP executed; screen was:\n{}",
        env.capture("overflow", &pane)
    );
    let screen = env.capture("overflow", &pane);
    for probe in ["TRUECOLOR", "INDEXED", "BASIC"] {
        assert!(screen.contains(probe), "colour probe {probe} missing");
    }
    env.assert_colour_probe(&pane);
}
