//! Raw runner signals — the ONLY observable vocabulary task 081 emits (design D2).
//!
//! 081 owns runner MECHANICS; it must NEVER emit a frozen `GateStatus` name or an
//! exit code — 082 maps these raw signals to statuses/exits. So the compare tier's
//! internal `GateStatus` (from 080's `evaluate_tier`) is adapted to a runner signal
//! here (`frame_match`/`frame_mismatch`/`golden_absent`/`golden_untrusted`), and the
//! four timeout causes stay DISTINCT (design D8) even though 082 folds two of them
//! into `settle_never_stable`.
//!
//! The trace is line-delimited JSON behind `--trace` (design D3) — never the default
//! stdout (082 owns that). Payloads carry HASHES + bounded excerpts, never raw env,
//! argv, or a full screen dump (codex privacy catch).

use serde::{Deserialize, Serialize};

/// The four DISTINCT timeout causes (design D8). 082 may map `FrameSettle` +
/// `NeverStabilized` to `settle_never_stable`, but 081 keeps them separate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeoutClass {
    /// A driving step (`wait_for_text`/`wait`) blew its own `timeout_ms`.
    Step,
    /// The settle preceding an `expect_golden` never reached quiet.
    FrameSettle,
    /// The whole scenario exceeded its `deadline_ms` budget.
    Scenario,
    /// A `settle`/`stable_frames` step's pane never stopped changing.
    NeverStabilized,
}

/// One raw runner signal. `kind` is the wire tag; the rest are optional context.
/// This is the trace record AND the unit-test assertion surface. Deliberately NOT
/// `GateStatus` (design D2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "signal", rename_all = "snake_case")]
pub enum RunnerSignal {
    /// The scenario started. Carries provenance HASHES only (never raw env/argv).
    ScenarioStart {
        scenario: String,
        scenario_hash: String,
        cmd_env_hash: String,
        rows: u16,
        cols: u16,
    },
    /// A capture matched its golden (082 → `pass`).
    FrameMatch { name: String, tier: String },
    /// A capture differed from its golden (082 → `fail`). `reason` is a diagnostic
    /// (e.g. `palette_unportable`, `exact_diff`), never a status.
    FrameMismatch {
        name: String,
        tier: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        changed_cells: Option<u32>,
    },
    /// No golden exists for this frame (082 → `missing_golden`).
    GoldenAbsent { name: String, tier: String },
    /// The golden's sidecar is stale, or a content pin was tampered — the compare is
    /// refused (082 → `stale_golden`).
    GoldenUntrusted { name: String, tier: String },
    /// The child exited UNEXPECTEDLY (no `expect_exit` consumed it) — short-circuits
    /// before any visual compare (082 → `child_error`). `code` is `None` for a
    /// signal-kill.
    ChildExit {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<i32>,
    },
    /// An `expect_exit` step consumed an intended exit.
    ExpectedChildExit {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<i32>,
    },
    /// A timeout fired; `class` distinguishes the four causes (design D8).
    Timeout {
        class: TimeoutClass,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step_index: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        elapsed_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        budget_ms: Option<u64>,
    },
    /// The scratch quota (16/daemon) was exhausted (082 → `infra_error`).
    QuotaExceeded { limit: usize },
    /// The scenario file is malformed / references an unknown or unsupported step
    /// (082 → `scenario_error`). `message` is actionable.
    ParseError { message: String },
    /// The scenario finished having compared ZERO frames — text asserts are smoke,
    /// not visual proof (design D6).
    NoVisualCheck,
    /// A cheap `assert_contains`/`assert_not_contains` passed.
    AssertPassed { step_index: usize },
    /// A cheap text assert FAILED. Carries a BOUNDED (≤120-char) excerpt + a hash of the
    /// captured text — never the full screen (design D3 privacy). NOTE: masks are applied
    /// to VISUAL goldens (`expect_golden`), NOT to this text-assert smoke excerpt (`pane.
    /// capture` returns unmasked text); a scenario that redacts a secret for its golden
    /// should not rely on the same region being redacted in an assert excerpt (adv MINOR).
    AssertFailed {
        step_index: usize,
        needle: String,
        excerpt: String,
        text_sha256: String,
    },
}

impl RunnerSignal {
    /// The wire tag (`signal` field value) — handy for tests (compare/bless assertions).
    #[allow(dead_code)]
    pub fn kind(&self) -> &'static str {
        match self {
            RunnerSignal::ScenarioStart { .. } => "scenario_start",
            RunnerSignal::FrameMatch { .. } => "frame_match",
            RunnerSignal::FrameMismatch { .. } => "frame_mismatch",
            RunnerSignal::GoldenAbsent { .. } => "golden_absent",
            RunnerSignal::GoldenUntrusted { .. } => "golden_untrusted",
            RunnerSignal::ChildExit { .. } => "child_exit",
            RunnerSignal::ExpectedChildExit { .. } => "expected_child_exit",
            RunnerSignal::Timeout { .. } => "timeout",
            RunnerSignal::QuotaExceeded { .. } => "quota_exceeded",
            RunnerSignal::ParseError { .. } => "parse_error",
            RunnerSignal::NoVisualCheck => "no_visual_check",
            RunnerSignal::AssertPassed { .. } => "assert_passed",
            RunnerSignal::AssertFailed { .. } => "assert_failed",
        }
    }

    /// One NDJSON line (no trailing newline). The trace writer joins with `\n`.
    pub fn to_ndjson(&self) -> String {
        serde_json::to_string(self).expect("RunnerSignal serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_matches_wire_tag() {
        let cases = [
            (
                RunnerSignal::FrameMatch {
                    name: "f".into(),
                    tier: "cell".into(),
                },
                "frame_match",
            ),
            (RunnerSignal::ChildExit { code: Some(42) }, "child_exit"),
            (RunnerSignal::NoVisualCheck, "no_visual_check"),
            (RunnerSignal::QuotaExceeded { limit: 16 }, "quota_exceeded"),
        ];
        for (sig, want) in cases {
            assert_eq!(sig.kind(), want);
            // The serialized `signal` tag agrees with `kind()`.
            let v: serde_json::Value = serde_json::from_str(&sig.to_ndjson()).unwrap();
            assert_eq!(v.get("signal").and_then(|s| s.as_str()), Some(want));
        }
    }

    #[test]
    fn timeout_classes_are_distinct_on_the_wire() {
        // Design D8: 081 never collapses the four causes.
        let classes = [
            TimeoutClass::Step,
            TimeoutClass::FrameSettle,
            TimeoutClass::Scenario,
            TimeoutClass::NeverStabilized,
        ];
        let mut seen = std::collections::HashSet::new();
        for c in classes {
            let s = serde_json::to_string(&c).unwrap();
            assert!(seen.insert(s), "timeout class must serialize distinctly");
        }
        assert_eq!(seen.len(), 4);
    }

    #[test]
    fn child_exit_signal_kill_has_null_code() {
        let sig = RunnerSignal::ChildExit { code: None };
        let json = sig.to_ndjson();
        // A signal-kill carries no code; the field is omitted, not `code: -1`.
        assert!(!json.contains("code"), "signal-kill omits code: {json}");
    }

    #[test]
    fn round_trips_through_ndjson() {
        let sig = RunnerSignal::Timeout {
            class: TimeoutClass::FrameSettle,
            step_index: Some(3),
            action: Some("expect_golden".into()),
            name: Some("main".into()),
            elapsed_ms: Some(1200),
            budget_ms: Some(1000),
        };
        let back: RunnerSignal = serde_json::from_str(&sig.to_ndjson()).unwrap();
        assert_eq!(sig, back);
    }

    #[test]
    fn assert_failed_never_dumps_full_text() {
        // Design D3 privacy: the payload is a bounded excerpt + a hash, not the screen.
        let sig = RunnerSignal::AssertFailed {
            step_index: 1,
            needle: "READY".into(),
            excerpt: "…bounded…".into(),
            text_sha256: "abc".into(),
        };
        let v: serde_json::Value = serde_json::from_str(&sig.to_ndjson()).unwrap();
        assert!(v.get("text_sha256").is_some());
        assert!(v.get("excerpt").is_some());
        // No `text`/`full`/`screen` key that would carry a raw dump.
        for k in ["text", "full", "screen", "raw"] {
            assert!(v.get(k).is_none(), "must not carry raw `{k}`");
        }
    }
}
