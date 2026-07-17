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

use serde::{Deserialize, Serialize};

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

    /// The worst (most-severe) of two statuses, for a worst-frame scenario
    /// rollup. Severity is defined by declaration order in [`GateStatus::ALL`]
    /// AFTER the greens — a regression always outranks a pass/xfail. The full
    /// rollup ordering is 082's to finalize; this is the frozen tie-break seed.
    pub fn worst(self, other: GateStatus) -> GateStatus {
        fn rank(s: GateStatus) -> u8 {
            if s.is_green() {
                0
            } else {
                // any non-green outranks any green; among non-greens keep a
                // stable order by ALL-index so the rollup is deterministic.
                1 + GateStatus::ALL.iter().position(|&x| x == s).unwrap_or(0) as u8
            }
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

/// The diff detail for a failing frame. Populated by 079's comparator (cell
/// counts, regions) and 080's pixel tier (`max_channel_delta`, `heat_png`).
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

#[cfg(test)]
mod tests {
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
        assert_eq!(GateStatus::Pass.worst(GateStatus::Xfail), GateStatus::Pass);
        assert_eq!(GateStatus::Xpass.worst(GateStatus::Pass), GateStatus::Xpass);
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
