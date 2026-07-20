//! Lens-gate verdict / report contract (task 078).
//!
//! **Placement note.** These are lens-gate *vocabulary* types (the closed status
//! set, the exit map, the xfail metadata shape, and the `report.json` schema).
//! They live in `shux-vt` — not because they are virtual-terminal concepts, but
//! because `shux` is a binary-only crate whose internals integration tests cannot
//! import, and this is the lowest shared crate that both the eventual gate
//! implementation (in the `shux` binary, tasks 081/082) and the frozen contract
//! tests (`crates/shux/tests/lens_gate_*`) can depend on. They are colocated with
//! [`crate::capture`] and the task-079 comparator as the "lens core" surface.
//!
//! **078 freezes the SHAPES; 082 fills in the COMPUTATION.** The one behaviour
//! frozen here is the total, pure [`GateStatus::exit_code`] mapping (§7.4). The
//! verdict rollup, report emission, and CLI dispatch are owned by 082.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::capture::{CapColor, CapStyle, FrameEnvelope, RowRepr, Run, RunContent};

/// `report.json` schema version. Bump only with a `GATE-TEST-CHANGE:` trailer.
pub const GATE_REPORT_SCHEMA: u32 = 1;

/// The complete, **closed** gate status set (council #3). No open-ended variant.
/// `palette_unportable` is deliberately absent — it is a `fail` *reason*
/// ([`FrameReport::reason`]), never a status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateStatus {
    /// Capture matched its golden.
    Pass,
    /// Capture differed from its golden (a regression).
    Fail,
    /// A known-failing frame failed as expected — GREEN.
    Xfail,
    /// A frame in the xfail set now matches — the bug is fixed (or the golden
    /// rotted). Force-promote out of xfail; never silently absorb (grok's crown
    /// jewel).
    Xpass,
    /// No golden exists for this frame.
    MissingGolden,
    /// An xfail entry is past its expiry date.
    XfailExpired,
    /// The golden's fingerprint no longer matches this build (font / shux
    /// version / unicode-width drift). The compare is refused, not trusted.
    StaleGolden,
    /// The scenario's child process exited unexpectedly before/around capture.
    ChildError,
    /// A frame never stabilized within the settle budget (a failure, not infra).
    SettleNeverStable,
    /// The scenario file is malformed or references unknown steps.
    ScenarioError,
    /// The gate could not run for an environmental reason (quota, socket, …).
    InfraError,
    /// An `--update`/bless was refused (e.g. CI mode, or the guardrail tripped).
    UpdateRefused,
}

impl GateStatus {
    /// Every status, in declaration order. A frozen test asserts this stays the
    /// closed set of 12.
    pub const ALL: [GateStatus; 12] = [
        GateStatus::Pass,
        GateStatus::Fail,
        GateStatus::Xfail,
        GateStatus::Xpass,
        GateStatus::MissingGolden,
        GateStatus::XfailExpired,
        GateStatus::StaleGolden,
        GateStatus::ChildError,
        GateStatus::SettleNeverStable,
        GateStatus::ScenarioError,
        GateStatus::InfraError,
        GateStatus::UpdateRefused,
    ];

    /// The frozen exit-code map (§7.4). Total over the closed set. Exit code 4
    /// (permission) is intentionally NOT produced by any status — it is a
    /// CLI-level error emitted before verdict computation (e.g. golden dir not
    /// writable), so the status→exit function never returns 4.
    pub fn exit_code(self) -> u8 {
        match self {
            // Green: a match, or an expected failure.
            GateStatus::Pass | GateStatus::Xfail => 0,
            // Regression class.
            GateStatus::Fail
            | GateStatus::Xpass
            | GateStatus::MissingGolden
            | GateStatus::XfailExpired
            | GateStatus::StaleGolden
            | GateStatus::SettleNeverStable => 1,
            // Usage (bad scenario).
            GateStatus::ScenarioError => 2,
            // Infrastructure.
            GateStatus::InfraError => 3,
            // Child process died.
            GateStatus::ChildError => 5,
            // Refused to write a golden.
            GateStatus::UpdateRefused => 6,
        }
    }

    /// Whether this status is a "green" outcome (exit 0).
    pub fn is_green(self) -> bool {
        self.exit_code() == 0
    }

    /// Severity tier for the worst-frame rollup. The load-bearing invariant
    /// (adv-gate M2): a **regression** (any exit-1 status) must never be masked
    /// by a higher-exit operational error, or CI keyed on "exit 1 = block, exit 3
    /// = retry" would let a real regression through. So:
    ///
    ///   0 = green (pass / xfail)
    ///   1 = operational error (child_error / scenario_error / infra_error /
    ///       update_refused) — beats green, loses to a regression
    ///   2 = regression (exit 1) — always surfaces
    ///
    /// 082 may refine the intra-tier *labels*, but MUST preserve tier ordering:
    /// a regression can never roll up to a non-regression exit code.
    fn severity_tier(self) -> u8 {
        if self.is_green() {
            0
        } else if self.exit_code() == 1 {
            2
        } else {
            1
        }
    }

    /// The worst (most-severe) of two statuses, for a worst-frame scenario
    /// rollup. Tier-ordered (see [`GateStatus::severity_tier`]); ties broken by
    /// declaration order in [`GateStatus::ALL`] so the rollup is deterministic.
    pub fn worst(self, other: GateStatus) -> GateStatus {
        fn rank(s: GateStatus) -> u16 {
            let idx = GateStatus::ALL.iter().position(|&x| x == s).unwrap_or(0) as u16;
            (s.severity_tier() as u16) * 100 + idx
        }
        if rank(other) > rank(self) {
            other
        } else {
            self
        }
    }
}

/// Static xfail metadata attached to a frame that is expected to differ. Parsed
/// from the scenario; validated (expiry, fingerprint match) by 082.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XfailMeta {
    /// Human explanation of why this frame is allowed to differ.
    pub reason: String,
    /// Who owns getting this back to green.
    pub owner: String,
    /// Tracking issue reference.
    pub issue: String,
    /// ISO-8601 date after which the xfail is `xfail_expired` (a regression).
    pub expiry: String,
    /// Optional capture fingerprint: the xfail holds only for THIS diff; a
    /// different mismatch fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

/// A changed row-span within a frame diff (`[col_start, col_end)`, half-open).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffRegion {
    pub row: u16,
    pub col_start: u16,
    pub col_end: u16,
}

/// The expected-vs-actual STYLE at one changed cell (084 F6).
///
/// A colour-only regression is byte-identical as TEXT, so a report carrying only
/// coordinates tells a text-only reader *where* something changed while every text diff
/// of the same frames shows nothing at all. `expected`/`actual` are terse human-readable
/// descriptors (`"fg=bright_green"`, `"fg=green bold"`, `"default"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyleDelta {
    pub row: u16,
    /// First column of the run (inclusive).
    pub col: u16,
    /// One past the last column of the run — so a consumer sees the run's EXTENT, not
    /// just where it began (impl council).
    pub col_end: u16,
    pub expected: String,
    pub actual: String,
}

/// The diff detail for a failing frame. Populated by 079's comparator (cell
/// counts, regions), 080's pixel tier (`max_channel_delta`, `heat_png`), and 084's
/// `style_deltas`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffReport {
    pub changed_cells: u32,
    pub total_cells: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_channel_delta: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heat_png: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regions: Option<Vec<DiffRegion>>,
    /// Style changes, one entry per contiguous run, capped so a full-screen recolour
    /// cannot bloat the report. Absent when nothing but text changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style_deltas: Option<Vec<StyleDelta>>,
    /// Total number of style-delta runs found, present ONLY when `style_deltas` was
    /// truncated by the cap — so a partial list can never be mistaken for the whole
    /// story (impl council: no silent caps).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style_deltas_total: Option<u32>,
}

/// Per-frame record in `report.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameReport {
    pub name: String,
    pub status: GateStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub golden: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<DiffReport>,
    /// A diagnostic reason on a `fail` frame, e.g. `"palette_unportable"`. Never
    /// a status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_json: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_png: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_exit: Option<i32>,
}

/// Per-scenario record in `report.json`. The top-level report is a
/// `Vec<ScenarioReport>` (snake_case, pretty, CI-greppable). Provenance fields
/// (`os`/`arch`/font) exist because goldens are platform-sensitive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioReport {
    pub scenario: String,
    pub status: GateStatus,
    pub os: String,
    pub arch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_chain_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size_px: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    pub frames: Vec<FrameReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Cap on emitted style deltas: enough to characterise a regression, bounded so a
/// full-screen recolour cannot bloat `report.json`.
const MAX_STYLE_DELTAS: usize = 16;

/// The 16 SGR colour names, so a report reads `fg=bright_green` rather than `fg=idx(10)`.
const BASIC_COLOR_NAMES: [&str; 16] = [
    "black",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "white",
    "bright_black",
    "bright_red",
    "bright_green",
    "bright_yellow",
    "bright_blue",
    "bright_magenta",
    "bright_cyan",
    "bright_white",
];

fn color_name(c: &CapColor) -> String {
    match c {
        CapColor::Idx(i) => match BASIC_COLOR_NAMES.get(*i as usize) {
            Some(n) => (*n).to_string(),
            None => format!("idx({i})"),
        },
        CapColor::Rgb([r, g, b]) => format!("#{r:02x}{g:02x}{b:02x}"),
    }
}

/// A terse descriptor of a cell's style: `"fg=bright_green bold"`, `"default"`.
fn describe_style(st: &CapStyle) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(fg) = &st.fg {
        parts.push(format!("fg={}", color_name(fg)));
    }
    if let Some(bg) = &st.bg {
        parts.push(format!("bg={}", color_name(bg)));
    }
    for (on, name) in [
        (st.bold, "bold"),
        (st.dim, "dim"),
        (st.italic, "italic"),
        (st.underline, "underline"),
        (st.blink, "blink"),
        (st.inverse, "inverse"),
        (st.hidden, "hidden"),
    ] {
        if on {
            parts.push(name.to_string());
        }
    }
    if parts.is_empty() {
        "default".to_string()
    } else {
        parts.join(" ")
    }
}

/// Style-by-column for one row: every column a `Cells` run covers maps to that run's
/// style. Mask runs contribute nothing (a masked cell is excluded from the compare).
///
/// A column ABSENT from this map is a default-styled blank — the capture format does not
/// emit runs for them. Callers MUST treat absence as [`CapStyle::default()`] rather than
/// skipping the column, or a change that only paints blanks (a selection bar, a status-bar
/// highlight — the canonical TUI style regression) is invisible.
fn row_styles(row: &RowRepr) -> BTreeMap<u16, &CapStyle> {
    let mut out = BTreeMap::new();
    for run in &row.runs {
        if let Run::Cells {
            col,
            content,
            style,
        } = run
        {
            let width = match content {
                RunContent::Simple(s) => s.chars().count(),
                RunContent::Complex(v) => v.len(),
            };
            for i in 0..width {
                out.insert(col.saturating_add(i as u16), style);
            }
        }
    }
    out
}

/// Style changes between two frames, in row-major order, capped at [`MAX_STYLE_DELTAS`]
/// (084 F6). Only cells whose STYLE differs are reported — a pure text change yields an
/// empty vec, because the coordinates already tell that story and the text is visible.
pub fn style_deltas(expected: &FrameEnvelope, actual: &FrameEnvelope) -> (Vec<StyleDelta>, u32) {
    // A column absent from a row's runs is a default-styled blank, not "no data".
    let blank = CapStyle::default();
    let mut out: Vec<StyleDelta> = Vec::new();
    let mut total: u32 = 0;
    // Whether the run currently being walked was actually pushed (false once capped).
    let mut emitting_current = false;
    let actual_rows: BTreeMap<u16, &RowRepr> = actual.rows.iter().map(|r| (r.row, r)).collect();

    for erow in &expected.rows {
        let Some(arow) = actual_rows.get(&erow.row) else {
            continue;
        };
        let (e_styles, a_styles) = (row_styles(erow), row_styles(arow));
        // Walk the UNION of both sides' columns, not just the expected frame's. A blank
        // default-styled cell is absent from the runs entirely, so iterating one side alone
        // both MISSES a blanks-only change and FRAGMENTS a run that spans blanks into
        // pieces that contradict `changed_cells` (adversarial review).
        let columns: std::collections::BTreeSet<u16> =
            e_styles.keys().chain(a_styles.keys()).copied().collect();
        // One entry per CONTIGUOUS run of the same (expected, actual) pair: a 10-column
        // recolour is one fact, and spending the cap on ten copies of it would hide the
        // other affected rows entirely.
        let mut prev: Option<(u16, &CapStyle, &CapStyle)> = None;
        for col in &columns {
            let e_style = e_styles.get(col).copied().unwrap_or(&blank);
            let a_style = a_styles.get(col).copied().unwrap_or(&blank);
            if e_style == a_style {
                prev = None;
                continue;
            }
            let continues = matches!(
                prev,
                Some((pcol, pe, pa))
                    if pcol + 1 == *col && pe == e_style && pa == a_style
            );
            prev = Some((*col, e_style, a_style));
            if continues {
                // Extend the run in place so `col_end` covers the whole span — but ONLY if
                // this run is the one we actually emitted. Past the cap a run is counted
                // and dropped, and blindly extending `out.last_mut()` would rewrite the
                // final emitted entry's `col_end` with a column from a different row.
                if emitting_current {
                    if let Some(last) = out.last_mut() {
                        last.col_end = col.saturating_add(1);
                    }
                }
                continue;
            }
            // Count EVERY run, even past the cap, so truncation can be reported honestly.
            total = total.saturating_add(1);
            emitting_current = out.len() < MAX_STYLE_DELTAS;
            if emitting_current {
                out.push(StyleDelta {
                    row: erow.row,
                    col: *col,
                    col_end: col.saturating_add(1),
                    expected: describe_style(e_style),
                    actual: describe_style(a_style),
                });
            }
        }
    }
    (out, total)
}

#[cfg(test)]
mod tests {
    // ── 084 F6: style deltas name WHAT changed, not just where ──────────────

    fn env_json(runs: &str) -> FrameEnvelope {
        let j = format!(
            r#"{{"schema":1,"size":{{"rows":1,"cols":10}},"alt_screen":false,"defaults":{{}},
                 "cursor":{{"row":0,"col":0,"visible":true,"shape":"block"}},
                 "palette_overridden":false,
                 "rows":[{{"row":0,"runs":{runs}}}]}}"#
        );
        FrameEnvelope::from_canonical_json(&j).expect("test envelope parses")
    }

    #[test]
    fn a_colour_only_change_is_named_expected_vs_actual() {
        // Identical TEXT, different fg: exactly the CR-B regression shape.
        let expected = env_json(r#"[[0,"healthy",{"fg":{"idx":10}}]]"#);
        let actual = env_json(r#"[[0,"healthy",{"fg":{"idx":2}}]]"#);

        let (d, total) = style_deltas(&expected, &actual);
        assert_eq!(d.len(), 1, "a contiguous run collapses to one delta: {d:?}");
        assert_eq!(total, 1, "nothing was truncated");
        assert_eq!(d[0].row, 0);
        assert_eq!(d[0].col, 0);
        // `healthy` is 7 cells wide, so the run must span [0, 7) — the extent matters, not
        // just where it started.
        assert_eq!(d[0].col_end, 7);
        assert_eq!(d[0].expected, "fg=bright_green");
        assert_eq!(d[0].actual, "fg=green");
    }

    /// The capture omits runs for default-styled BLANK cells, so iterating only the
    /// expected frame's runs made a blanks-only style change invisible — and a selection
    /// bar or status-bar highlight is exactly that: a background painted on blanks
    /// (adversarial review). Absence must read as "default", not "no data".
    #[test]
    fn a_background_change_on_blank_cells_is_reported() {
        // Expected omits the blanks entirely, mirroring what the capture format emits.
        let expected = env_json(r#"[[0,"AA"],[7,"BB"]]"#);
        let actual = env_json(r#"[[0,"AA"],[2,"     ",{"bg":{"idx":1}}],[7,"BB"]]"#);

        let (d, total) = style_deltas(&expected, &actual);
        assert_eq!(
            d.len(),
            1,
            "a blanks-only background change was dropped: {d:?}"
        );
        assert_eq!(total, 1);
        assert_eq!((d[0].row, d[0].col, d[0].col_end), (0, 2, 7));
        assert_eq!(d[0].expected, "default");
        assert_eq!(d[0].actual, "bg=red");
    }

    /// A run spanning blanks is ONE fact. Skipping the blanks fragmented it into pieces
    /// that positively imply the middle cells were unchanged — contradicting the
    /// comparator, which counts them.
    #[test]
    fn a_style_run_spanning_blanks_stays_one_entry() {
        let expected = env_json(r#"[[0,"AAA    BBB"]]"#);
        let actual = env_json(r#"[[0,"AAA    BBB",{"bg":{"idx":1}}]]"#);

        let (d, _) = style_deltas(&expected, &actual);
        assert_eq!(d.len(), 1, "run spanning blanks fragmented: {d:?}");
        assert_eq!((d[0].col, d[0].col_end), (0, 10));
    }

    /// Styled content appearing where the expected frame had NOTHING stored (a trimmed
    /// blank row) must still be described — previously the union was one-sided.
    #[test]
    fn styling_added_where_the_golden_stored_nothing_is_reported() {
        let expected = env_json(r#"[[0,"AAA"]]"#);
        let actual = env_json(r#"[[0,"AAABBBB",{"fg":{"idx":9}}]]"#);

        let (d, _) = style_deltas(&expected, &actual);
        assert!(!d.is_empty(), "additive styled cells produced no deltas");
        let last = d.last().unwrap();
        assert_eq!(last.col_end, 7, "extent must cover the added cells: {d:?}");
    }

    #[test]
    fn identical_styles_yield_no_deltas() {
        let a = env_json(r#"[[0,"healthy",{"fg":{"idx":10}}]]"#);
        assert!(style_deltas(&a, &a).0.is_empty());
    }

    #[test]
    fn a_pure_text_change_yields_no_style_deltas() {
        // The coordinates already tell that story, and the text is visible in a diff.
        let expected = env_json(r#"[[0,"healthy",{"fg":{"idx":10}}]]"#);
        let actual = env_json(r#"[[0,"HEALTHY",{"fg":{"idx":10}}]]"#);
        assert!(style_deltas(&expected, &actual).0.is_empty());
    }

    #[test]
    fn rgb_and_attributes_are_described_readably() {
        let expected = env_json(r#"[[0,"x",{"fg":{"rgb":[255,122,24]},"bold":true}]]"#);
        let actual = env_json(r#"[[0,"x",{"fg":{"idx":200},"italic":true}]]"#);
        let (d, _) = style_deltas(&expected, &actual);
        assert_eq!(d[0].expected, "fg=#ff7a18 bold");
        assert_eq!(d[0].actual, "fg=idx(200) italic");
    }

    /// The cap test above uses 1-cell runs, so the continuation branch never fires and it
    /// structurally cannot see this: past the cap, a dropped run's continuation cells were
    /// still rewriting the LAST EMITTED entry's `col_end` — with a column from a different
    /// row entirely. A truncated list must never corrupt what it did emit (shux-tui-qa).
    #[test]
    fn a_truncated_list_does_not_corrupt_the_last_emitted_run() {
        // 20 rows. Rows 0..16 are a 5-wide run at [0,5); rows 16..20 sit at [20,25), so a
        // post-cap continuation would be visible as a bogus col_end on the last entry.
        let frame = |idx: u8| {
            let rows: Vec<String> = (0..20u16)
                .map(|r| {
                    let start = if r < 16 { 0 } else { 20 };
                    let runs: Vec<String> = (0..5u16)
                        .map(|i| format!(r#"[{},"x",{{"fg":{{"idx":{idx}}}}}]"#, start + i))
                        .collect();
                    format!(r#"{{"row":{r},"runs":[{}]}}"#, runs.join(","))
                })
                .collect();
            FrameEnvelope::from_canonical_json(&format!(
                r#"{{"schema":1,"size":{{"rows":20,"cols":30}},"alt_screen":false,"defaults":{{}},
                     "cursor":{{"row":0,"col":0,"visible":true,"shape":"block"}},
                     "palette_overridden":false,"rows":[{}]}}"#,
                rows.join(",")
            ))
            .expect("parses")
        };

        let (d, total) = style_deltas(&frame(100), &frame(200));
        assert_eq!(
            d.len(),
            MAX_STYLE_DELTAS,
            "must truncate for this to prove anything"
        );
        assert_eq!(total, 20, "every run must still be counted");

        let last = d.last().expect("at least one delta");
        assert_eq!(last.row, 15);
        assert_eq!(last.col, 0);
        assert_eq!(
            last.col_end, 5,
            "a dropped post-cap run corrupted the last emitted entry: {last:?}"
        );
        // And every emitted entry must describe a real 5-wide run on its own row.
        for e in &d {
            assert_eq!(e.col_end - e.col, 5, "bad extent on {e:?}");
        }
    }
    #[test]
    fn deltas_are_capped_when_each_change_is_a_distinct_fact() {
        // Each column gets a DIFFERENT colour, so no two neighbours share an
        // (expected, actual) pair and nothing collapses -> 40 distinct facts, capped.
        let mk = |base: u8| {
            let runs: Vec<String> = (0..40)
                .map(|c| format!(r#"[{c},"x",{{"fg":{{"idx":{}}}}}]"#, base + (c % 5) as u8))
                .collect();
            format!("[{}]", runs.join(","))
        };
        let j = |body: String| {
            FrameEnvelope::from_canonical_json(&format!(
                r#"{{"schema":1,"size":{{"rows":1,"cols":40}},"alt_screen":false,"defaults":{{}},
                     "cursor":{{"row":0,"col":0,"visible":true,"shape":"block"}},
                     "palette_overridden":false,"rows":[{{"row":0,"runs":{body}}}]}}"#
            ))
            .expect("parses")
        };
        let (d, total) = style_deltas(&j(mk(100)), &j(mk(200)));
        assert_eq!(d.len(), MAX_STYLE_DELTAS, "must be capped: got {}", d.len());
        // No silent caps: the TRUE run count must survive truncation so a partial list
        // can never be mistaken for the whole story (impl council).
        assert_eq!(total, 40, "truncation must still report every run it found");
        assert!(
            total as usize > d.len(),
            "this case must actually truncate, or it proves nothing"
        );
    }

    use super::*;

    #[test]
    fn status_set_is_closed_at_twelve() {
        assert_eq!(GateStatus::ALL.len(), 12);
        // No duplicates.
        for (i, a) in GateStatus::ALL.iter().enumerate() {
            for b in &GateStatus::ALL[i + 1..] {
                assert_ne!(a, b, "duplicate status in ALL");
            }
        }
    }

    #[test]
    fn exit_map_is_total_and_frozen() {
        use GateStatus::*;
        let expect = [
            (Pass, 0),
            (Xfail, 0),
            (Fail, 1),
            (Xpass, 1),
            (MissingGolden, 1),
            (XfailExpired, 1),
            (StaleGolden, 1),
            (SettleNeverStable, 1),
            (ScenarioError, 2),
            (InfraError, 3),
            (ChildError, 5),
            (UpdateRefused, 6),
        ];
        for (s, code) in expect {
            assert_eq!(s.exit_code(), code, "exit code for {s:?}");
        }
    }

    #[test]
    fn exit_code_never_returns_four() {
        // Exit 4 (permission) is a CLI-level code, produced by no status.
        for s in GateStatus::ALL {
            assert_ne!(
                s.exit_code(),
                4,
                "{s:?} must not map to the reserved perm code"
            );
        }
    }

    #[test]
    fn only_pass_and_xfail_are_green() {
        for s in GateStatus::ALL {
            let green = matches!(s, GateStatus::Pass | GateStatus::Xfail);
            assert_eq!(s.is_green(), green, "{s:?}");
        }
    }

    #[test]
    fn status_serializes_snake_case() {
        let cases = [
            (GateStatus::Pass, "\"pass\""),
            (GateStatus::Xfail, "\"xfail\""),
            (GateStatus::Xpass, "\"xpass\""),
            (GateStatus::MissingGolden, "\"missing_golden\""),
            (GateStatus::XfailExpired, "\"xfail_expired\""),
            (GateStatus::StaleGolden, "\"stale_golden\""),
            (GateStatus::ChildError, "\"child_error\""),
            (GateStatus::SettleNeverStable, "\"settle_never_stable\""),
            (GateStatus::ScenarioError, "\"scenario_error\""),
            (GateStatus::InfraError, "\"infra_error\""),
            (GateStatus::UpdateRefused, "\"update_refused\""),
        ];
        for (s, want) in cases {
            let got = serde_json::to_string(&s).unwrap();
            assert_eq!(got, want, "{s:?}");
            let back: GateStatus = serde_json::from_str(&got).unwrap();
            assert_eq!(back, s);
        }
    }

    #[test]
    fn palette_unportable_is_not_a_status() {
        // It must never deserialize as a status — it is a `fail` reason string.
        assert!(serde_json::from_str::<GateStatus>("\"palette_unportable\"").is_err());
    }

    #[test]
    fn worst_prefers_regression_over_green() {
        assert_eq!(GateStatus::Pass.worst(GateStatus::Fail), GateStatus::Fail);
        assert_eq!(GateStatus::Fail.worst(GateStatus::Pass), GateStatus::Fail);
        // Two greens roll up to a green (the exact label is a deterministic
        // tie-break and exit-neutral).
        assert!(GateStatus::Pass.worst(GateStatus::Xfail).is_green());
        assert_eq!(GateStatus::Xpass.worst(GateStatus::Pass), GateStatus::Xpass);
    }

    #[test]
    fn worst_never_masks_a_regression_with_an_error(/* adv-gate M2 */) {
        use GateStatus::*;
        // A regression sharing a scenario with an operational error must still
        // roll up to a regression (exit 1) — never to the error's exit code.
        let errors = [ChildError, ScenarioError, InfraError, UpdateRefused];
        let regressions = [
            Fail,
            Xpass,
            MissingGolden,
            XfailExpired,
            StaleGolden,
            SettleNeverStable,
        ];
        for r in regressions {
            for e in errors {
                assert_eq!(
                    r.worst(e).exit_code(),
                    1,
                    "worst({r:?}, {e:?}) must stay a regression (exit 1), got {:?}",
                    r.worst(e)
                );
                assert_eq!(e.worst(r).exit_code(), 1, "worst is order-independent");
            }
        }
        // And an error still beats a green.
        assert_eq!(Pass.worst(InfraError), InfraError);
        assert_eq!(Xfail.worst(ChildError), ChildError);
    }

    #[test]
    fn worst_never_returns_green_when_either_is_non_green() {
        for a in GateStatus::ALL {
            for b in GateStatus::ALL {
                if !a.is_green() || !b.is_green() {
                    assert!(
                        !a.worst(b).is_green(),
                        "worst({a:?}, {b:?}) masked a non-green as green"
                    );
                }
            }
        }
    }

    #[test]
    fn report_round_trips_and_denies_unknown_fields() {
        let report = vec![ScenarioReport {
            scenario: "demo".into(),
            status: GateStatus::Fail,
            os: "macos".into(),
            arch: "aarch64".into(),
            font_chain_sha256: None,
            font_size_px: Some(16),
            started_at_ms: Some(1_700_000_000_000),
            duration_ms: Some(1234),
            frames: vec![FrameReport {
                name: "main".into(),
                status: GateStatus::Fail,
                golden: Some("main.json".into()),
                diff: Some(DiffReport {
                    changed_cells: 3,
                    total_cells: 1920,
                    max_channel_delta: None,
                    heat_png: None,
                    regions: Some(vec![DiffRegion {
                        row: 0,
                        col_start: 2,
                        col_end: 5,
                    }]),
                    style_deltas: None,
                    style_deltas_total: None,
                }),
                reason: Some("palette_unportable".into()),
                capture_json: Some("main.capture.json".into()),
                capture_png: None,
                child_exit: None,
            }],
            note: None,
        }];
        let json = serde_json::to_string_pretty(&report).unwrap();
        let back: Vec<ScenarioReport> = serde_json::from_str(&json).unwrap();
        assert_eq!(report, back);

        // deny_unknown_fields: an extra key fails closed.
        let bad = json.replace("\"scenario\"", "\"scenario_x\": 1, \"scenario\"");
        assert!(serde_json::from_str::<Vec<ScenarioReport>>(&bad).is_err());
    }

    #[test]
    fn xfail_meta_parses() {
        let json = r##"{"reason":"known heat drift","owner":"aria","issue":"#123","expiry":"2026-12-31","fingerprint":"abc"}"##;
        let m: XfailMeta = serde_json::from_str(json).unwrap();
        assert_eq!(m.owner, "aria");
        assert_eq!(m.fingerprint.as_deref(), Some("abc"));
        // fingerprint is optional.
        let json2 = r#"{"reason":"r","owner":"o","issue":"i","expiry":"2026-01-01"}"#;
        let m2: XfailMeta = serde_json::from_str(json2).unwrap();
        assert_eq!(m2.fingerprint, None);
    }
}
