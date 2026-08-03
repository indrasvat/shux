//! Resource bounds on attacker-controlled pane input (issue #102).
//!
//! Pane output is untrusted whenever untrusted software runs in a pane. These
//! tests assert that a hostile sequence cannot buy unbounded work or unbounded
//! retained memory, and — just as importantly — that the bounds never fire on
//! input a real terminal application produces.
//!
//! Work is measured with `Grid::mutations()`, a monotonic counter bumped once
//! per scroll/clear/cell-write. That is an exact bound, unlike wall-clock
//! timing, which is flaky under load.

use shux_vt::VirtualTerminal;

const ROWS: usize = 24;
const COLS: usize = 80;

fn vt() -> VirtualTerminal {
    VirtualTerminal::new(ROWS, COLS)
}

/// Grid mutations performed while processing `bytes`.
fn work(vt: &mut VirtualTerminal, bytes: &[u8]) -> u64 {
    let before = vt.grid().mutations();
    vt.process(bytes);
    vt.grid().mutations() - before
}

// ---------------------------------------------------------------------------
// CSI line operations: IL / DL / SU / SD
// ---------------------------------------------------------------------------

#[test]
fn su_clamps_work_to_region_height() {
    let mut t = vt();
    let did = work(&mut t, b"\x1b[65535S");
    assert!(
        did <= ROWS as u64,
        "SU count=65535 did {did} grid mutations on a {ROWS}-row grid; expected <= {ROWS}"
    );
}

#[test]
fn sd_clamps_work_to_region_height() {
    let mut t = vt();
    let did = work(&mut t, b"\x1b[65535T");
    assert!(
        did <= ROWS as u64,
        "SD count=65535 did {did} grid mutations; expected <= {ROWS}"
    );
}

#[test]
fn il_clamps_work_to_region_height() {
    let mut t = vt();
    let did = work(&mut t, b"\x1b[65535L");
    assert!(
        did <= ROWS as u64,
        "IL count=65535 did {did} grid mutations; expected <= {ROWS}"
    );
}

#[test]
fn dl_clamps_work_to_region_height() {
    let mut t = vt();
    let did = work(&mut t, b"\x1b[65535M");
    assert!(
        did <= ROWS as u64,
        "DL count=65535 did {did} grid mutations; expected <= {ROWS}"
    );
}

#[test]
fn scroll_clamps_to_decstbm_region_not_screen() {
    // DECSTBM rows 5..20 (1-based) => 16-row region.
    let mut t = vt();
    t.process(b"\x1b[5;20r");
    let did = work(&mut t, b"\x1b[65535S");
    assert!(
        did <= 16,
        "SU inside a 16-row DECSTBM region did {did} mutations; expected <= 16"
    );
}

/// Clamping must not change what the user sees: a huge count and a
/// region-height count must leave identical grids.
#[test]
fn huge_scroll_is_visually_identical_to_region_height_scroll() {
    let mut huge = vt();
    let mut exact = vt();
    for t in [&mut huge, &mut exact] {
        t.process(b"\x1b[5;20r");
        for row in 0..ROWS {
            t.process(format!("\x1b[{};1Hline-{row}", row + 1).as_bytes());
        }
    }
    huge.process(b"\x1b[65535S");
    exact.process(b"\x1b[16S");

    for row in 0..ROWS {
        let a: String = (0..COLS)
            .map(|c| huge.grid().visible_row(row)[c].ch)
            .collect();
        let b: String = (0..COLS)
            .map(|c| exact.grid().visible_row(row)[c].ch)
            .collect();
        assert_eq!(a, b, "row {row} differs between huge and exact scroll");
    }
}

/// A cursor outside the scroll region must make IL/DL a no-op. Deriving the
/// clamp from `cursor.row` without this guard underflows and invents mutation
/// where a real terminal does nothing.
#[test]
fn il_dl_with_cursor_outside_scroll_region_is_noop() {
    for seq in [b"\x1b[65535L".as_slice(), b"\x1b[65535M".as_slice()] {
        let mut t = vt();
        t.process(b"\x1b[10;20r"); // region rows 10..20 (1-based)
        t.process(b"\x1b[1;1H"); // cursor at row 0 — above the region
        let did = work(&mut t, seq);
        assert_eq!(
            did,
            0,
            "{:?} with cursor above the scroll region did {did} mutations; expected 0",
            std::str::from_utf8(seq).unwrap()
        );
    }
}

// ---------------------------------------------------------------------------
// REP (CSI b) — repeat preceding character
// ---------------------------------------------------------------------------

#[test]
fn rep_clamps_work_to_one_screen() {
    let mut t = vt();
    t.process(b"A");
    // One screen of cell writes, plus the scrolls those writes cause as they
    // wrap off the bottom (at most one per row), plus the seed write.
    let budget = (ROWS * COLS) as u64 + ROWS as u64 + 1;
    let did = work(&mut t, b"\x1b[65535b");
    assert!(
        did <= budget,
        "REP count=65535 did {did} mutations; expected <= one screen plus wrap \
         scrolls ({budget}). Unclamped this is 65535+."
    );
}

/// Negative control: REP must still wrap onto following lines. Clamping to the
/// current line would be simpler but breaks legitimate REP semantics.
#[test]
fn rep_still_wraps_onto_following_lines() {
    let mut t = vt();
    t.process(b"\x1b[1;1HX");
    t.process(b"\x1b[100b"); // 1 + 100 X's spills past an 80-col row
    let row0: String = (0..COLS).map(|c| t.grid().visible_row(0)[c].ch).collect();
    let row1: String = (0..COLS).map(|c| t.grid().visible_row(1)[c].ch).collect();
    assert!(
        row0.starts_with(&"X".repeat(COLS)),
        "REP did not fill the first row: {row0:?}"
    );
    assert!(
        row1.starts_with("XX"),
        "REP did not wrap onto the second row: {row1:?}"
    );
}

// ---------------------------------------------------------------------------
// DCS — unterminated / oversized control strings
// ---------------------------------------------------------------------------

/// An oversized DCS payload must be discarded, not answered. Answering a
/// truncated capability query is worse than not answering at all.
#[test]
fn oversized_dcs_is_dropped_without_dispatch() {
    let mut t = vt();
    let mut seq = Vec::from(&b"\x1bP+q"[..]);
    seq.extend(std::iter::repeat_n(b'A', 64 * 1024));
    seq.extend_from_slice(b"\x1b\\");
    let responses = t.process_with_responses(&seq);
    assert!(
        responses.is_empty(),
        "oversized DCS produced {} response(s); expected none",
        responses.len()
    );
}

/// Recovery: state must reset cleanly so a valid sequence after an overflow is
/// still parsed. This is what a naive discard implementation gets wrong.
#[test]
fn valid_dcs_after_overflow_still_parses() {
    let mut t = vt();
    let mut overflow = Vec::from(&b"\x1bP+q"[..]);
    overflow.extend(std::iter::repeat_n(b'A', 64 * 1024));
    overflow.extend_from_slice(b"\x1b\\");
    let _ = t.process_with_responses(&overflow);

    // "TN" (terminal name) hex-encoded — a normal XTGETTCAP query.
    let responses = t.process_with_responses(b"\x1bP+q544e\x1b\\");
    assert!(
        !responses.is_empty(),
        "a valid XTGETTCAP query after a DCS overflow produced no response"
    );
}

/// Chunk boundaries are where a naive cap breaks: the payload arrives split
/// across many `process()` calls, so the bound must be on accumulated state.
#[test]
fn unterminated_dcs_split_across_chunks_stays_bounded() {
    let mut t = vt();
    t.process(b"\x1bP+q");
    let chunk = vec![b'A'; 8 * 1024];
    for _ in 0..64 {
        t.process(&chunk);
    }
    // Terminate; an over-cap payload must not be answered.
    let responses = t.process_with_responses(b"\x1b\\");
    assert!(
        responses.is_empty(),
        "chunked oversized DCS produced {} response(s); expected none",
        responses.len()
    );
}

// ---------------------------------------------------------------------------
// OSC — titles and OSC 8 hyperlinks
// ---------------------------------------------------------------------------

fn hyperlink_at(t: &VirtualTerminal, row: usize, col: usize) -> Option<String> {
    t.grid().visible_row(row)[col]
        .extended
        .as_ref()
        .and_then(|e| e.hyperlink.clone())
}

/// Negative control: ordinary hyperlinks must keep working untouched.
#[test]
fn normal_osc8_hyperlink_is_stored_intact() {
    let mut t = vt();
    t.process(b"\x1b]8;;https://example.invalid/a/b/c\x1b\\L\x1b]8;;\x1b\\");
    assert_eq!(
        hyperlink_at(&t, 0, 0).as_deref(),
        Some("https://example.invalid/a/b/c"),
        "a normal OSC 8 hyperlink was not stored intact"
    );
}

/// An over-cap URI must be DROPPED, never stored truncated. A truncated URI is
/// a valid-looking link pointing somewhere the sender never specified.
#[test]
fn oversized_osc8_hyperlink_is_dropped_not_truncated() {
    let mut t = vt();
    let mut seq = Vec::from(&b"\x1b]8;;https://good.invalid/"[..]);
    seq.extend(std::iter::repeat_n(b'A', 64 * 1024));
    seq.extend_from_slice(b"/final\x1b\\L\x1b]8;;\x1b\\");
    t.process(&seq);

    match hyperlink_at(&t, 0, 0) {
        None => {}
        Some(uri) => panic!(
            "oversized OSC 8 stored a {}-byte hyperlink instead of dropping it; \
             ends with {:?} — a truncated URI is a wrong destination, not a safe one",
            uri.len(),
            &uri[uri.len().saturating_sub(24)..]
        ),
    }
}

/// Negative control: a normal title still round-trips.
#[test]
fn normal_title_is_stored() {
    let mut t = vt();
    t.process(b"\x1b]0;my-shell\x07");
    assert_eq!(t.title(), Some("my-shell"));
}

/// VT-side title retention must be bounded regardless of downstream clamping.
#[test]
fn oversized_title_does_not_retain_unbounded_string() {
    let mut t = vt();
    let mut seq = Vec::from(&b"\x1b]0;"[..]);
    seq.extend(std::iter::repeat_n(b'T', 64 * 1024));
    seq.extend_from_slice(b"\x07");
    t.process(&seq);
    let len = t.title().map(str::len).unwrap_or(0);
    assert!(
        len <= 256,
        "VT retained a {len}-byte title; expected it clamped at parse time (<= 256)"
    );
}

#[test]
fn valid_osc_after_overflow_still_parses() {
    let mut t = vt();
    let mut overflow = Vec::from(&b"\x1b]0;"[..]);
    overflow.extend(std::iter::repeat_n(b'T', 64 * 1024));
    overflow.extend_from_slice(b"\x07");
    t.process(&overflow);

    t.process(b"\x1b]0;after\x07");
    assert_eq!(
        t.title(),
        Some("after"),
        "a valid OSC title after an overflow was not parsed"
    );
}

// ---------------------------------------------------------------------------
// Grapheme accumulation
// ---------------------------------------------------------------------------

#[test]
fn grapheme_payload_is_capped() {
    let mut t = vt();
    t.process(b"A");
    let mut buf = Vec::new();
    for _ in 0..40_000 {
        buf.extend_from_slice("\u{0301}".as_bytes());
    }
    t.process(&buf);

    let stored = t.grid().visible_row(0)[0]
        .grapheme()
        .map(|s| s.chars().count())
        .unwrap_or(1);
    assert!(
        stored <= 32,
        "one cell retained a {stored}-scalar grapheme after 40,000 combining marks; \
         expected it capped at 32"
    );
}

/// Negative control: a real ZWJ family emoji must survive intact. This is the
/// test that fails if the cap is set too aggressively.
#[test]
fn real_zwj_family_emoji_is_preserved() {
    let mut t = vt();
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
    t.process(family.as_bytes());
    let stored = t.grid().visible_row(0)[0]
        .grapheme()
        .unwrap_or("")
        .to_string();
    assert_eq!(
        stored, family,
        "a real ZWJ family emoji was mangled by the grapheme cap"
    );
}

/// Negative control: the real-world worst case must sit well under the cap.
///
/// `MAX_GRAPHEME_SCALARS` has no prior art to lean on — alacritty_terminal
/// 0.26 caps grapheme accumulation not at all — so the cap's safety rests
/// entirely on this test. Each cluster below is one a real application emits;
/// none may be clipped, and the headroom is recorded so a future reduction of
/// the cap fails loudly rather than silently truncating someone's text.
#[test]
fn real_world_grapheme_clusters_are_never_clipped() {
    const ZWJ: &str = "\u{200D}";
    const VS16: &str = "\u{FE0F}";
    let cases: Vec<(&str, String)> = vec![
        (
            "ZWJ family",
            format!("\u{1F468}{ZWJ}\u{1F469}{ZWJ}\u{1F467}{ZWJ}\u{1F466}"),
        ),
        (
            "England tag flag",
            "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}".to_string(),
        ),
        ("rainbow flag", format!("\u{1F3F3}{VS16}{ZWJ}\u{1F308}")),
        ("Vietnamese decomposed", "e\u{0302}\u{0323}".to_string()),
        ("stacked accents", "a\u{0301}\u{0302}\u{0303}".to_string()),
    ];

    for (name, cluster) in &cases {
        let mut t = vt();
        t.process(cluster.as_bytes());
        let stored = t.grid().visible_row(0)[0].grapheme().unwrap_or("");
        assert_eq!(
            stored,
            cluster,
            "{name} ({} scalars) was clipped by the {}-scalar cap",
            cluster.chars().count(),
            32
        );
        assert!(
            cluster.chars().count() < 32,
            "{name} needs {} scalars, which leaves no headroom under the cap",
            cluster.chars().count()
        );
    }
}

/// Negative control: ordinary combining marks must still compose.
#[test]
fn ordinary_combining_marks_still_compose() {
    let mut t = vt();
    t.process("e\u{0301}".as_bytes());
    let stored = t.grid().visible_row(0)[0]
        .grapheme()
        .unwrap_or("")
        .to_string();
    assert_eq!(
        stored, "e\u{0301}",
        "a simple combining mark did not compose"
    );
}

// ---------------------------------------------------------------------------
// Terminal response amplification
// ---------------------------------------------------------------------------

/// A single read can carry thousands of query sequences. Each pushes a reply,
/// so the reply count per batch must be bounded.
#[test]
fn response_count_is_bounded_per_batch() {
    let mut t = vt();
    let mut seq = Vec::new();
    for _ in 0..5_000 {
        seq.extend_from_slice(b"\x1b[6n"); // DSR — cursor position report
    }
    let responses = t.process_with_responses(&seq);
    assert!(
        responses.len() <= 256,
        "5,000 DSR queries in one batch produced {} responses; expected a bounded reply budget",
        responses.len()
    );
}
