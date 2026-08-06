//! THROWAWAY adversarial probe — delete before finishing.
use proptest::prelude::*;
use shux_vt::{Grid, VirtualTerminal};
use unicode_width::UnicodeWidthChar;

const MAX_GRAPHEME_SCALARS: usize = 32;

fn check_grid(grid: &Grid, ctx: &str) {
    for row_idx in 0..grid.total_lines() {
        let row = grid.row(row_idx).expect("row exists");
        for col in 0..row.len() {
            let cell = &row[col];
            if cell.is_wide_continuation() {
                assert!(col > 0, "{ctx}: orphan continuation r{row_idx} c{col}");
                assert!(
                    row[col - 1].is_wide(),
                    "{ctx}: orphan continuation r{row_idx} c{col}"
                );
                assert!(
                    cell.grapheme().is_none(),
                    "{ctx}: continuation carries payload r{row_idx} c{col}"
                );
            }
            if cell.is_wide() {
                assert!(
                    col + 1 < row.len(),
                    "{ctx}: wide head at final col r{row_idx} c{col}"
                );
                assert!(
                    row[col + 1].is_wide_continuation(),
                    "{ctx}: wide head missing tail r{row_idx} c{col}"
                );
            }
            if let Some(text) = cell.grapheme() {
                let n = text.chars().count();
                assert!(
                    n <= MAX_GRAPHEME_SCALARS,
                    "{ctx}: payload {n} scalars > cap at r{row_idx} c{col}: {text:?}"
                );
                assert_eq!(
                    text.chars().next(),
                    Some(cell.ch),
                    "{ctx}: payload head != cell.ch at r{row_idx} c{col}: {text:?} ch={:?}",
                    cell.ch
                );
                let implied = text
                    .chars()
                    .filter_map(UnicodeWidthChar::width)
                    .max()
                    .unwrap_or(1)
                    .clamp(1, 2);
                assert!(
                    usize::from(cell.width) >= implied,
                    "{ctx}: cell.width {} < implied {implied} for payload {text:?} at r{row_idx} c{col}",
                    cell.width
                );
            }
        }
    }
}

fn dump(grid: &Grid) -> Vec<(char, u8, Option<String>)> {
    let mut out = Vec::new();
    for r in 0..grid.total_lines() {
        let row = grid.row(r).expect("row");
        for c in 0..row.len() {
            let cell = &row[c];
            out.push((cell.ch, cell.width, cell.grapheme().map(str::to_owned)));
        }
    }
    out
}

const ATOMS: &[&str] = &[
    "A",
    "界",
    "e",
    "\u{301}",   // combining acute
    "\u{200d}",  // ZWJ
    "\u{1f600}", // emoji (wide)
    "\u{1f1e6}", // regional indicator A
    "\u{1f1e7}", // regional indicator B
    "\u{fe0f}",  // VS16 (zero width)
];

fn build(ops: &[(u8, u8)], rows: usize, cols: usize) -> Vec<u8> {
    let mut prog = Vec::new();
    for &(op, arg) in ops {
        let a = arg as usize;
        match op {
            0..=8 => prog.extend_from_slice(ATOMS[op as usize].as_bytes()),
            9 => prog.extend_from_slice(
                format!("\x1b[{};{}H", (a % rows) + 1, (a % cols) + 1).as_bytes(),
            ),
            10 => prog.extend_from_slice(format!("\x1b[{}b", (a % 5) + 1).as_bytes()),
            11 => prog.extend_from_slice(b"\r\n"),
            12 => prog.extend_from_slice(format!("\x1b[{}@", (a % 4) + 1).as_bytes()),
            13 => prog.extend_from_slice(format!("\x1b[{}P", (a % 4) + 1).as_bytes()),
            14 => prog.extend_from_slice(b"\x1b[4h"),
            15 => prog.extend_from_slice(b"\x1b[4l"),
            16 => prog.extend_from_slice(b"\x1b[?7l"),
            17 => prog.extend_from_slice(b"\x1b[?7h"),
            18 => prog.extend_from_slice(b"\x1b[?6h"), // origin mode
            19 => prog.extend_from_slice(
                format!("\x1b[{};{}r", (a % rows) + 1, rows.min((a % rows) + 2)).as_bytes(),
            ),
            20 => prog.extend_from_slice(b"\x1b[?2026h"),
            21 => prog.extend_from_slice(b"\x1b[?2026l"),
            22 => prog.extend_from_slice(b"\x1b[?1049h"),
            23 => prog.extend_from_slice(b"\x1b[?1049l"),
            24 => prog.extend_from_slice(b"\x1b(0"),
            25 => prog.extend_from_slice(b"\x1b(B"),
            26 => prog.extend_from_slice(format!("\x1b[{}X", (a % 4) + 1).as_bytes()),
            27 => prog.extend_from_slice(b"\x08"),
            _ => prog.extend_from_slice(b"\x1b[K"),
        }
    }
    prog
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 3000, failure_persistence: None, .. ProptestConfig::default() })]

    /// Invariants must hold at every step, including across resize reflow.
    #[test]
    fn invariants_hold_over_grapheme_and_rep_programs(
        ops in prop::collection::vec((0u8..29, 0u8..40), 1..60),
        rows in 1usize..5,
        cols in 1usize..9,
        rz in prop::collection::vec((1usize..6, 1usize..10), 0..4),
    ) {
        let mut vt = VirtualTerminal::new(rows, cols);
        for i in 0..ops.len() {
            vt.process(&build(&ops[i..=i], rows, cols));
            check_grid(vt.grid(), "prog");
        }
        for (r, c) in rz {
            vt.resize(r, c);
            check_grid(vt.grid(), "resize");
        }
    }

    /// The contract: REP(n) is byte-identical to the source arriving n more
    /// times. Source is explicit here so the oracle knows what "the character"
    /// is; the grid is big enough that the issue-#102 clamp never binds.
    #[test]
    fn rep_equals_literal_copies(
        ops in prop::collection::vec((0u8..29, 0u8..40), 0..24),
        src in 0usize..SOURCES.len(),
        mid in 0usize..MIDS.len(),
        n in 1usize..4,
        rows in 3usize..7,
        cols in 3usize..13,
    ) {
        let mut prog = build(&ops, rows, cols);
        prog.extend_from_slice(SOURCES[src].as_bytes());
        prog.extend_from_slice(MIDS[mid].as_bytes());

        let mut a = VirtualTerminal::new(rows, cols);
        a.process(&prog);
        a.process(format!("\x1b[{n}b").as_bytes());
        check_grid(a.grid(), "rep-side");

        let mut b = VirtualTerminal::new(rows, cols);
        b.process(&prog);
        for _ in 0..n {
            b.process(SOURCES[src].as_bytes());
        }
        check_grid(b.grid(), "literal-side");

        prop_assert_eq!(
            dump(a.grid()), dump(b.grid()),
            "REP({}) != {} literal copies of {:?} after mid {:?}", n, n, SOURCES[src], MIDS[mid]
        );
    }
}

/// Candidate "preceding characters in the data stream".
const SOURCES: &[&str] = &[
    "A",
    "界",
    "e\u{301}",
    "\u{1f600}",
    "\u{1f1e6}\u{1f1e7}",
    "a\u{200d}\u{1f600}",
    "\u{1f600}\u{fe0f}",
    "q",
];

/// Sequences between the source and the `CSI b`.
const MIDS: &[&str] = &[
    "",
    "\x1b[1;1H",
    "\x1b[2;3H",
    "\x1b[2J",
    "\r\n",
    "\x1b[31m",
    "\x1b[4h",
    "\x1b[?7l",
    "\x1b[?2026h",
    "\x1b[?1049h",
    "\x1b[2;4r\x1b[?6h",
    "\x08",
    "\x1b[K",
    "\x1b[3X",
    "\x1b[2@",
];
