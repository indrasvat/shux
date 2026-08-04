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

/// REP's clamp is on ITERATIONS, but mutations per iteration vary with the
/// source cell — a wide char writes two cells, a grapheme cluster also writes a
/// payload. The narrow-char test above would not catch a blow-up on those, so
/// pin every width. Measured: narrow 1945, wide 1993, ZWJ cluster 3913 —
/// against 65535+ unclamped.
#[test]
fn rep_stays_bounded_for_wide_and_cluster_sources() {
    let budget = 4 * (ROWS * COLS) as u64;
    for (name, seed) in [
        ("narrow", "A"),
        ("wide CJK", "\u{4F60}"),
        ("ZWJ cluster", "\u{1F468}\u{200D}\u{1F469}"),
    ] {
        let mut t = vt();
        t.process(seed.as_bytes());
        let did = work(&mut t, b"\x1b[65535b");
        assert!(
            did <= budget,
            "REP with a {name} source did {did} mutations; expected <= {budget}"
        );
    }
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

/// vte caps the OSC *parameter list* at 16 independently of its byte buffer, so
/// a semicolon flood truncates the parameter list without filling the buffer.
/// The byte-length check alone does not see that, and the result was a cell
/// carrying `";;;;;;;;;;;;;"` as its hyperlink.
///
/// Pre-existing (identical before this issue's fix), but it is junk stored as a
/// link, and nothing shux handles legitimately uses that many parameters — so
/// fail closed on a truncated parameter list too.
#[test]
fn osc8_with_truncated_parameter_list_is_dropped() {
    for (name, seq) in [
        ("semicolon flood", {
            let mut s = Vec::from(&b"\x1b]8;"[..]);
            s.extend(std::iter::repeat_n(b';', 4000));
            s.extend_from_slice(b"https://good.invalid/tail\x1b\\");
            s
        }),
        ("many named params", {
            let mut s = Vec::from(&b"\x1b]8"[..]);
            for i in 0..40 {
                s.push(b';');
                s.extend_from_slice(format!("p{i}").as_bytes());
            }
            s.extend_from_slice(b";https://good.invalid/tail\x1b\\");
            s
        }),
    ] {
        let mut t = vt();
        t.process(&seq);
        t.process(b"L");
        assert_eq!(
            hyperlink_at(&t, 0, 0),
            None,
            "{name}: a truncated OSC 8 parameter list was stored as a hyperlink"
        );
    }
}

/// OSC 4 (palette set/query) legitimately carries `1 + 2N` parameters — its
/// handler is written to loop over `params[1..].as_chunks::<2>()`. A blanket
/// parameter-count guard therefore silently voids any batch of 8 or more pairs.
///
/// This is not cosmetic: `palette_overridden` feeds `has_indexed_colors` in
/// `gate_compare.rs`, the non-portability signal for `shux lens gate`. Voiding
/// it lets a capture that IS non-portable be judged portable — a defect inside
/// the verification machinery.
///
/// The truncation-drop rule exists to stop a truncated OSC 8 URI becoming a
/// valid-looking link to somewhere the sender never specified. Losing trailing
/// palette pairs carries no such hazard, so OSC 4 must degrade to partial
/// application, exactly as it did before the bounds work.
#[test]
fn osc4_palette_batches_survive_the_truncation_guard() {
    for pairs in [1usize, 4, 7, 8, 9, 16] {
        let mut set = Vec::from(&b"\x1b]4"[..]);
        for i in 1..=pairs {
            set.extend_from_slice(format!(";{i};rgb:00/ff/00").as_bytes());
        }
        set.push(0x07);

        let mut t = vt();
        t.process(&set);
        assert!(
            t.palette_overridden(),
            "OSC 4 with {pairs} pairs did not set palette_overridden; \
             lens gate would judge a non-portable capture portable"
        );

        let mut query = Vec::from(&b"\x1b]4"[..]);
        for i in 1..=pairs {
            query.extend_from_slice(format!(";{i};?").as_bytes());
        }
        query.push(0x07);

        let mut t = vt();
        let replies = t.process_with_responses(&query);
        assert!(
            !replies.is_empty(),
            "OSC 4 query with {pairs} pairs produced no reply at all"
        );
    }
}

/// Pins the semicolon boundary for OSC 8 URIs, including the deliberate false
/// positive at the parameter cap.
///
/// A URI path may legally contain semicolons, and vte splits on them. Up to 12
/// the link round-trips. At 13 it produces exactly 16 parameters — complete,
/// nothing lost — and is nonetheless DROPPED, because vte discards everything
/// past its cap with no signal, so a complete 14-segment URI and a truncated
/// 30-segment one arrive as byte-identical dispatches. Verified: both give 16
/// params, 33 bytes, identical values.
///
/// Dropping a rare valid link degrades to plain text; storing a truncated one
/// is a wrong destination a user may click. This test exists so that trade-off
/// is visible and cannot be "fixed" without re-admitting truncated URIs.
#[test]
fn osc8_semicolon_boundary_is_pinned_including_the_false_positive() {
    let build = |segments: usize| {
        let uri = (0..segments)
            .map(|i| format!("s{i}"))
            .collect::<Vec<_>>()
            .join(";");
        (format!("\x1b]8;;{uri}\x1b\\"), uri)
    };

    // Up to 12 semicolons (15 params) — must round-trip intact.
    for segments in [2usize, 6, 13] {
        let (seq, uri) = build(segments);
        let mut t = vt();
        t.process(seq.as_bytes());
        t.process(b"L");
        assert_eq!(
            hyperlink_at(&t, 0, 0).as_deref(),
            Some(uri.as_str()),
            "a URI with {} semicolons ({} params) should round-trip intact",
            segments - 1,
            segments + 2
        );
    }

    // 14 segments (16 params) — complete, but indistinguishable from truncated,
    // so dropped. Documented trade-off, not an oversight.
    let (seq, _) = build(14);
    let mut t = vt();
    t.process(seq.as_bytes());
    t.process(b"L");
    assert_eq!(
        hyperlink_at(&t, 0, 0),
        None,
        "a 16-param OSC 8 must fail closed: it cannot be told apart from a \
         truncated one, and storing a truncated URI is a wrong destination"
    );

    // Genuinely truncated — must also be dropped.
    let (seq, _) = build(30);
    let mut t = vt();
    t.process(seq.as_bytes());
    t.process(b"L");
    assert_eq!(
        hyperlink_at(&t, 0, 0),
        None,
        "a truncated OSC 8 URI was stored"
    );
}

/// Direct behavioural probe of vte's OSC parameter cap, via a selector that has
/// NO parameter guard of its own.
///
/// The OSC 8 boundary test below catches a future cap DECREASE (a truncated
/// sequence would be stored and fail the assertion) but is blind to an
/// INCREASE: if vte raised its cap, shux would keep over-dropping at the
/// mirrored 16 and every assertion would still pass. OSC 4 replies count one
/// per surviving pair, so this observes the real cap directly.
#[test]
fn vte_osc_param_cap_observed_directly_via_osc4() {
    // 20 pairs sent. With params capped at 16, params[1..] holds 15 entries,
    // which is 7 whole pairs -> 7 replies. A larger cap yields more.
    let mut query = Vec::from(&b"\x1b]4"[..]);
    for i in 1..=20 {
        query.extend_from_slice(format!(";{i};?").as_bytes());
    }
    query.push(0x07);

    let mut t = vt();
    let replies = t.process_with_responses(&query).len();
    assert_eq!(
        replies, 7,
        "vte returned {replies} OSC 4 replies for 20 pairs; 7 means a 16-param \
         cap. A different count means vte's MAX_OSC_PARAMS moved and the \
         mirrored VTE_MAX_OSC_PARAMS in parser.rs is now wrong — which would \
         make shux over-drop (cap raised) or under-drop (cap lowered) OSC 8."
    );
}

/// Drift guard for the mirrored `VTE_MAX_OSC_PARAMS`.
///
/// vte does not export its parameter cap, so the parser hardcodes 16. If a vte
/// upgrade changes that, the drop rule silently starts rejecting valid
/// sequences or accepting truncated ones. Pin both sides of the boundary so the
/// mismatch fails here rather than in production.
#[test]
fn vte_osc_param_cap_is_still_sixteen() {
    // Probe through OSC 8, which is where the parameter-count guard lives.
    // `ESC]8` + N `;seg` gives N+1 total parameters.
    let build = |segments: usize| {
        let mut s = Vec::from(&b"\x1b]8"[..]);
        for i in 0..segments {
            s.push(b';');
            s.extend_from_slice(format!("s{i}").as_bytes());
        }
        s.extend_from_slice(b"\x1b\\");
        s
    };

    // 15 total params — under the cap, must be accepted and stored.
    let mut t = vt();
    t.process(&build(14));
    t.process(b"L");
    assert!(
        hyperlink_at(&t, 0, 0).is_some(),
        "a 15-param OSC 8 was dropped — vte's parameter cap may have moved above 16"
    );

    // 16 total params — at the cap, the list is truncated, must be dropped.
    let mut t = vt();
    t.process(&build(15));
    t.process(b"L");
    assert_eq!(
        hyperlink_at(&t, 0, 0),
        None,
        "a 16-param OSC 8 was stored — vte's parameter cap may have moved below 16"
    );
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

/// Reaching the grapheme cap must never swallow subsequent output.
///
/// The cap's early return left the stored grapheme ending in ZWJ, and the join
/// state machine treats "ends with ZWJ" as "keep joining" — so every following
/// printable character was handed to a no-op append and reported as consumed.
/// A pane could lose arbitrary text after 32 scalars. Found by review on #109.
#[test]
fn hitting_the_grapheme_cap_does_not_swallow_following_text() {
    // base + 30 combining marks + ZWJ == exactly the 32-scalar cap, with the
    // payload ending in ZWJ — the state the join machine wants to continue.
    let mut seq = String::from("A");
    for _ in 0..30 {
        seq.push('\u{0301}');
    }
    seq.push('\u{200D}');

    let mut t = vt();
    t.process(seq.as_bytes());
    t.process(b"HELLO");

    let text = t.capture_text(None);
    assert!(
        text.contains("HELLO"),
        "text after a capped trailing-ZWJ grapheme was swallowed; captured: {text:?}"
    );
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
        responses.len() <= 512,
        "5,000 DSR queries in one batch produced {} responses; expected a bounded reply budget",
        responses.len()
    );
}

/// Negative control for the reply budget: the largest LEGITIMATE burst must not
/// be clipped. An application probing the full 256-colour palette emits exactly
/// 256 replies in one batch — that sat exactly on an earlier 256 budget, so any
/// additional startup query would have silently truncated a valid probe.
#[test]
fn full_palette_probe_is_not_clipped_by_the_reply_budget() {
    let mut seq = Vec::new();
    let mut idx = 0u32;
    while idx < 256 {
        let mut s = String::from("\x1b]4");
        // vte caps OSC parameters at 16, so at most 7 pairs fit per sequence.
        for _ in 0..7 {
            if idx < 256 {
                s.push_str(&format!(";{idx};?"));
                idx += 1;
            }
        }
        s.push('\x07');
        seq.extend_from_slice(s.as_bytes());
    }

    let mut t = vt();
    let replies = t.process_with_responses(&seq);
    assert_eq!(
        replies.len(),
        256,
        "a full 256-colour palette probe was clipped to {} replies",
        replies.len()
    );

    // Plus ordinary startup chatter in the same batch — still must not clip.
    let mut with_chatter = seq.clone();
    for _ in 0..64 {
        with_chatter.extend_from_slice(b"\x1b[6n");
    }
    let mut t = vt();
    let replies = t.process_with_responses(&with_chatter);
    assert_eq!(
        replies.len(),
        256 + 64,
        "a palette probe plus 64 DSR queries was clipped to {} replies",
        replies.len()
    );
}
