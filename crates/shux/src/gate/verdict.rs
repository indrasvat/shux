//! The verdict layer (task 082) — maps a structured [`RunOutcome`] to the frozen
//! `report.json` schema (078) and the rolled-up exit code. PURE: no daemon, no I/O, an
//! injected `today` for deterministic xfail-expiry tests.
//!
//! Ownership: 081 produces raw mechanics ([`RunOutcome`]); THIS module owns every
//! `GateStatus` decision — the signal→status map, xfail governance, and the worst-frame
//! rollup. The exit code is `worst().exit_code()`; `report.json` is the source of truth.

use chrono::NaiveDate;
use shux_vt::{DiffRegion, DiffReport, FrameReport, GateStatus, ScenarioReport};

use super::outcome::{FrameKind, FrameOutcome, RunOutcome, TerminalOutcome};

/// Build the `report.json` array (one `ScenarioReport`; the schema is a `Vec` for the
/// eventual multi-scenario surface). `today` is injected (UTC) so xfail-expiry is
/// deterministic under test.
pub fn build_reports(outcome: &RunOutcome, today: NaiveDate) -> Vec<ScenarioReport> {
    let mut acc = GateStatus::Pass;
    let mut frames = Vec::with_capacity(outcome.frames.len());

    for f in &outcome.frames {
        let (status, reason) = frame_status(f, today);
        acc = acc.worst(status);
        frames.push(FrameReport {
            name: f.name.clone(),
            status,
            golden: Some(f.golden_json.clone()),
            diff: diff_report(f),
            reason,
            capture_json: None,
            capture_png: None,
            child_exit: None,
        });
    }

    // Scenario-level terminal disposition (child died / timed out / infra / bad step).
    // The note is SANITIZED (adv Agent C): the terminal message is built from internal
    // error Displays that can carry captured text / argv — never emit it raw.
    let mut note: Option<String> = None;
    if let Some(t) = &outcome.terminal {
        let (_, tnote) = terminal_status(t);
        note = Some(sanitize_note(&tnote));
    }

    // A scenario with NO `expect_golden` proves nothing visually (council: unblessable,
    // exit 2). Non-visual ASSERTS do NOT suppress it — but a real TERMINAL error (a crash /
    // timeout) IS the story and must surface, so `no_visual_check` applies only when the
    // run reached the end with no terminal disposition (adv Agent A, Finding D).
    if !outcome.has_visual_check && outcome.terminal.is_none() {
        note = Some("no_visual_check: scenario compared 0 frames".to_string());
    }

    // Both scenario-level contributions fold in through the ONE floor helper, so the
    // post-bless re-roll cannot diverge from this rollup (084 F4).
    acc = acc.worst(scenario_floor(outcome));

    vec![ScenarioReport {
        scenario: outcome.scenario_name.clone(),
        status: acc,
        os: outcome.os.clone(),
        arch: outcome.arch.clone(),
        font_chain_sha256: outcome.font_chain_sha256.clone(),
        font_size_px: Some(outcome.font_size_px),
        started_at_ms: Some(outcome.started_at_ms),
        duration_ms: Some(outcome.duration_ms),
        frames,
        note,
    }]
}

/// The process exit code for a set of scenario reports: the frozen `exit_code` of the
/// worst scenario status. Empty → 0 (nothing to gate).
pub fn exit_code(reports: &[ScenarioReport]) -> u8 {
    reports
        .iter()
        .fold(GateStatus::Pass, |acc, r| acc.worst(r.status))
        .exit_code()
}

/// The per-frame status + optional diagnostic reason, applying xfail governance (council
/// strict precedence). `today` (UTC) is injected for deterministic expiry.
fn frame_status(f: &FrameOutcome, today: NaiveDate) -> (GateStatus, Option<String>) {
    match f.kind {
        FrameKind::Match => match &f.xfail {
            // A frame in the xfail set now MATCHES — force-promote (the bug is fixed or
            // the golden rotted); NEVER silently absorbed. Applies even to a fingerprinted
            // xfail (design-council rule 1). Xfail metadata VALIDATION (expiry /
            // accountability / blank fingerprint) governs only the MISMATCH path below: on a
            // match the xfail is obsolete and being removed regardless, so `xpass` ("remove
            // the xfail") is the accurate primary signal — and it is exit 1, so no
            // regression can escape either way. (Impl-review preferred validating first; the
            // design council's rule is kept as the more actionable, equally-safe choice.)
            Some(_) => (
                GateStatus::Xpass,
                Some("xpass: frame matches — remove the xfail".to_string()),
            ),
            None => (GateStatus::Pass, None),
        },
        FrameKind::Mismatch => match &f.xfail {
            None => (GateStatus::Fail, f.reason.clone()),
            Some(x) => match parse_expiry(&x.expiry) {
                // A malformed xfail is a scenario authoring error, not a silent pass
                // (council rule 2).
                Err(why) => (
                    GateStatus::ScenarioError,
                    Some(format!("xfail_malformed: {why}")),
                ),
                Ok(expiry) => {
                    if !xfail_is_accountable(x) {
                        (
                            GateStatus::ScenarioError,
                            Some(
                                "xfail_malformed: reason/owner/issue must be non-empty".to_string(),
                            ),
                        )
                    } else if expiry < today {
                        // Valid THROUGH the expiry date; expired only when strictly past.
                        (
                            GateStatus::XfailExpired,
                            Some(format!("xfail expired {} (owner {})", x.expiry, x.owner)),
                        )
                    } else if let Some(fp) = &x.fingerprint {
                        // A fingerprinted xfail licenses only THAT specific diff; a
                        // different mismatch still fails (council rules 5/6). The
                        // fingerprint is the POST-MASK live capture identity.
                        if fp.trim().is_empty() {
                            // A blank fingerprint pins nothing — malformed, not a licence
                            // to differ (adv Agent A, Finding A).
                            (
                                GateStatus::ScenarioError,
                                Some("xfail_malformed: empty fingerprint".to_string()),
                            )
                        } else if fp == &f.live_capture_sha256 {
                            (GateStatus::Xfail, None)
                        } else {
                            (
                                GateStatus::Fail,
                                Some(
                                    "xfail fingerprint mismatch: a different regression"
                                        .to_string(),
                                ),
                            )
                        }
                    } else {
                        (GateStatus::Xfail, None)
                    }
                }
            },
        },
        FrameKind::GoldenAbsent => (
            GateStatus::MissingGolden,
            // A first-timer's DETAIL hint (dogfood: the blank column gave no next step).
            // ASCII only: the summary sanitizes every non-ASCII char to `?` at the output
            // boundary, so an em-dash here reaches the reader as `no committed golden ?`.
            Some("no committed golden - run with `--on-missing create`".to_string()),
        ),
        FrameKind::GoldenUntrusted => (
            GateStatus::StaleGolden,
            Some("golden fingerprint/baseline stale — re-bless".to_string()),
        ),
    }
}

/// The scenario-level (non-frame) status floor: the terminal disposition (child died /
/// timed out / infra / bad step) plus the `no_visual_check` guard.
///
/// A blessing re-roll MUST start from this floor rather than from `Pass` — a terminal
/// failure produces NO frames, so folding over frames alone would launder a crash, a
/// `step_timeout`, or a no-visual scenario into `pass`/exit 0 while blessing nothing
/// (084 F4: `--on-missing create` and `--update` both hit this through `apply_blessed`).
pub(crate) fn scenario_floor(outcome: &RunOutcome) -> GateStatus {
    let mut acc = GateStatus::Pass;
    if let Some(t) = &outcome.terminal {
        acc = acc.worst(terminal_status(t).0);
    }
    if !outcome.has_visual_check && outcome.terminal.is_none() {
        acc = acc.worst(GateStatus::ScenarioError);
    }
    acc
}

/// Map a scenario-level terminal disposition to its status + a terse, privacy-safe note.
fn terminal_status(t: &TerminalOutcome) -> (GateStatus, String) {
    match t {
        TerminalOutcome::ChildExit { code } => (
            GateStatus::ChildError,
            match code {
                Some(c) => format!("child_error: exit {c}"),
                None => "child_error: signal-kill".to_string(),
            },
        ),
        TerminalOutcome::StepTimeout { action, step_index } => (
            GateStatus::Fail,
            format!("step_timeout: {action} (step {step_index})"),
        ),
        TerminalOutcome::ScenarioDeadline { step_index } => (
            GateStatus::Fail,
            format!("scenario_deadline (at step {step_index})"),
        ),
        TerminalOutcome::SettleNeverStable { action } => (
            GateStatus::SettleNeverStable,
            format!("settle_never_stable: {action}"),
        ),
        TerminalOutcome::QuotaExceeded { limit } => (
            GateStatus::InfraError,
            format!("quota_exceeded: scratch limit {limit}"),
        ),
        TerminalOutcome::Infra { message } => (GateStatus::InfraError, format!("infra: {message}")),
        TerminalOutcome::ScenarioError { message } => (
            GateStatus::ScenarioError,
            format!("scenario_error: {message}"),
        ),
    }
}

/// Shape a compare verdict into the report's `DiffReport` for a MISMATCH frame (regions
/// truncated → omitted; pixel metrics present only at the pixel tier). `None` for a
/// match/absent/untrusted frame. A pixel-only mismatch (cells identical, pixels differ)
/// has `changed_cells == 0` but MUST still carry its `max_channel_delta` — the sole
/// quantitative evidence of a pixel-tier regression (adv Agent A, Finding C).
fn diff_report(f: &FrameOutcome) -> Option<DiffReport> {
    if f.kind != FrameKind::Mismatch {
        return None;
    }
    let v = f.verdict.as_ref()?;
    let d = &v.cell.diff;
    let regions = if d.regions_truncated || d.cells_changed == 0 {
        None
    } else {
        Some(
            d.regions
                .iter()
                .map(|r| DiffRegion {
                    row: r.row,
                    col_start: r.col_start,
                    col_end: r.col_end,
                })
                .collect(),
        )
    };
    Some(DiffReport {
        changed_cells: d.cells_changed,
        total_cells: (d.rows as u32).saturating_mul(d.cols as u32),
        max_channel_delta: v.pixel.as_ref().map(|p| p.max_channel_delta),
        heat_png: None,
        regions,
        // 084 F6: a colour-only regression is byte-identical as text, so coordinates
        // alone leave a text-only reader with nothing to act on.
        style_deltas: if f.style_deltas.is_empty() {
            None
        } else {
            Some(f.style_deltas.clone())
        },
        // Reported ONLY when the cap actually truncated, so a full list stays quiet.
        style_deltas_total: (f.style_deltas_total as usize > f.style_deltas.len())
            .then_some(f.style_deltas_total),
    })
}

/// Parse a canonical `YYYY-MM-DD` expiry (UTC). Rejects non-canonical forms (e.g.
/// `2026-1-1`, RFC-3339 timestamps) via a reformat round-trip so a sloppy date can never
/// silently extend an xfail.
fn parse_expiry(s: &str) -> Result<NaiveDate, String> {
    // No trimming: surrounding whitespace is non-canonical and must be rejected, not
    // silently accepted (adv Agent A, Finding B — the "strict canonical" contract).
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|_| format!("expiry {s:?} is not a canonical YYYY-MM-DD date"))?;
    if d.format("%Y-%m-%d").to_string() != s {
        return Err(format!(
            "expiry {s:?} must be canonical zero-padded YYYY-MM-DD"
        ));
    }
    Ok(d)
}

/// An xfail must name a reason, an owner, and a tracking issue (council #1: mandatory
/// accountability). Blank fields are an authoring error, not a licence to differ.
fn xfail_is_accountable(x: &shux_vt::XfailMeta) -> bool {
    !x.reason.trim().is_empty() && !x.owner.trim().is_empty() && !x.issue.trim().is_empty()
}

/// Sanitize a scenario `note` before it reaches `report.json` / the summary (adv Agent C,
/// MAJOR-1/3). The note is built from internal error `Display`s (`lens.run failed: {e}`,
/// `glance cells: {e}`) that can carry captured terminal text, argv, or an env value.
/// Flatten to one line, strip control chars (ESC / newline injection), bound the length,
/// and REDACT if any secret-shaped content survives — never emit the raw value.
pub(crate) fn sanitize_note(raw: &str) -> String {
    let flat: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let collapsed = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    let bounded: String = collapsed.chars().take(240).collect();
    if crate::gate::secrets::scan(&bounded).is_empty() {
        bounded
    } else {
        "[redacted: note carried secret-shaped content]".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shux_raster::{PixelMetrics, TierVerdict};
    use shux_vt::{CellVerdict, FrameDiff, LensRowSpan, Tier, XfailMeta};

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 7, 18).unwrap()
    }

    fn xfail(expiry: &str, fingerprint: Option<&str>) -> XfailMeta {
        XfailMeta {
            reason: "known heat drift".into(),
            owner: "aria".into(),
            issue: "#123".into(),
            expiry: expiry.into(),
            fingerprint: fingerprint.map(String::from),
        }
    }

    fn frame(kind: FrameKind, xfail: Option<XfailMeta>, sha: &str) -> FrameOutcome {
        FrameOutcome {
            style_deltas: Vec::new(),
            style_deltas_total: 0,
            name: "main".into(),
            tier: Tier::Cell,
            kind,
            reason: matches!(kind, FrameKind::Mismatch).then(|| "cell_diff".to_string()),
            verdict: None,
            golden_json: "main.capture.json".into(),
            live_capture_json: "{}".into(),
            live_capture_sha256: sha.into(),
            live_fingerprint: dummy_fp(),
            xfail,
            retry_note: None,
        }
    }

    fn dummy_fp() -> shux_vt::Fingerprint {
        use shux_vt::{
            FINGERPRINT_SCHEMA, MaskSet, RENDERER_FORMAT_VERSION, SCHEMA_VERSION, TolParams,
            mask_hash, unicode_width_version,
        };
        shux_vt::Fingerprint {
            fp_schema: FINGERPRINT_SCHEMA,
            schema: SCHEMA_VERSION,
            renderer_format_version: RENDERER_FORMAT_VERSION,
            raster_font_fingerprint: "font".into(),
            unicode_width_ver: unicode_width_version(),
            tol: Tier::Cell,
            tol_params: TolParams::default(),
            mask_hash: mask_hash(&MaskSet::new()),
            platform: None,
            shux_version: "test".into(),
            capture_sha256: String::new(),
            rgba_sha256: None,
            png_sha256: None,
            scenario_hash: "scn".into(),
            cmd_env_hash: "cmd".into(),
        }
    }

    fn outcome(frames: Vec<FrameOutcome>, terminal: Option<TerminalOutcome>) -> RunOutcome {
        let has_visual = !frames.is_empty();
        RunOutcome {
            scenario_name: "demo".into(),
            os: "macos".into(),
            arch: "aarch64".into(),
            font_chain_sha256: Some("fc".into()),
            font_size_px: 16,
            started_at_ms: 1,
            duration_ms: 2,
            frames,
            terminal,
            has_visual_check: has_visual,
        }
    }

    fn status_of(f: FrameOutcome) -> GateStatus {
        frame_status(&f, today()).0
    }

    // ── the council's xfail precedence table ─────────────────────────────────

    #[test]
    fn match_no_xfail_is_pass() {
        assert_eq!(
            status_of(frame(FrameKind::Match, None, "sha")),
            GateStatus::Pass
        );
    }

    #[test]
    fn match_with_xfail_is_xpass_always() {
        // Rule 1: a match while an xfail is declared is xpass (force-promote), even for a
        // fingerprinted xfail.
        assert_eq!(
            status_of(frame(
                FrameKind::Match,
                Some(xfail("2099-01-01", None)),
                "sha"
            )),
            GateStatus::Xpass
        );
        assert_eq!(
            status_of(frame(
                FrameKind::Match,
                Some(xfail("2099-01-01", Some("sha"))),
                "sha"
            )),
            GateStatus::Xpass
        );
    }

    #[test]
    fn mismatch_no_xfail_is_fail() {
        assert_eq!(
            status_of(frame(FrameKind::Mismatch, None, "sha")),
            GateStatus::Fail
        );
    }

    #[test]
    fn mismatch_malformed_expiry_is_scenario_error() {
        // Rule 2: a malformed expiry is an authoring error, never a silent pass.
        for bad in ["2026-1-1", "not-a-date", "2026/07/18", "2026-13-40", ""] {
            assert_eq!(
                status_of(frame(FrameKind::Mismatch, Some(xfail(bad, None)), "sha")),
                GateStatus::ScenarioError,
                "expiry {bad:?} must be malformed → scenario_error"
            );
        }
    }

    #[test]
    fn mismatch_blank_accountability_is_scenario_error() {
        let mut x = xfail("2099-01-01", None);
        x.owner = "  ".into();
        assert_eq!(
            status_of(frame(FrameKind::Mismatch, Some(x), "sha")),
            GateStatus::ScenarioError
        );
    }

    #[test]
    fn mismatch_expired_xfail_is_xfail_expired() {
        // Rule 3: expiry strictly before today.
        assert_eq!(
            status_of(frame(
                FrameKind::Mismatch,
                Some(xfail("2026-07-17", None)),
                "sha"
            )),
            GateStatus::XfailExpired
        );
    }

    #[test]
    fn xfail_valid_through_the_expiry_date() {
        // Expiry == today is still valid (valid THROUGH that date).
        assert_eq!(
            status_of(frame(
                FrameKind::Mismatch,
                Some(xfail("2026-07-18", None)),
                "sha"
            )),
            GateStatus::Xfail
        );
    }

    #[test]
    fn mismatch_valid_unfingerprinted_xfail_is_green() {
        // Rule 4.
        assert_eq!(
            status_of(frame(
                FrameKind::Mismatch,
                Some(xfail("2099-01-01", None)),
                "sha"
            )),
            GateStatus::Xfail
        );
    }

    #[test]
    fn mismatch_fingerprint_match_is_xfail_mismatch_is_fail() {
        // Rules 5 + 6: the xfail licenses only THAT diff.
        assert_eq!(
            status_of(frame(
                FrameKind::Mismatch,
                Some(xfail("2099-01-01", Some("live-sha"))),
                "live-sha"
            )),
            GateStatus::Xfail
        );
        assert_eq!(
            status_of(frame(
                FrameKind::Mismatch,
                Some(xfail("2099-01-01", Some("old-sha"))),
                "live-sha"
            )),
            GateStatus::Fail
        );
    }

    #[test]
    fn absent_is_missing_untrusted_is_stale() {
        assert_eq!(
            status_of(frame(FrameKind::GoldenAbsent, None, "sha")),
            GateStatus::MissingGolden
        );
        assert_eq!(
            status_of(frame(FrameKind::GoldenUntrusted, None, "sha")),
            GateStatus::StaleGolden
        );
    }

    // ── rollup + terminal mapping ────────────────────────────────────────────

    #[test]
    fn all_pass_rolls_up_green() {
        let r = build_reports(
            &outcome(vec![frame(FrameKind::Match, None, "s")], None),
            today(),
        );
        assert_eq!(r[0].status, GateStatus::Pass);
        assert_eq!(exit_code(&r), 0);
    }

    #[test]
    fn a_fail_among_passes_surfaces() {
        let r = build_reports(
            &outcome(
                vec![
                    frame(FrameKind::Match, None, "a"),
                    frame(FrameKind::Mismatch, None, "b"),
                ],
                None,
            ),
            today(),
        );
        assert_eq!(r[0].status, GateStatus::Fail);
        assert_eq!(exit_code(&r), 1);
    }

    #[test]
    fn child_error_terminal_maps_to_exit_5() {
        let r = build_reports(
            &outcome(
                vec![frame(FrameKind::Match, None, "a")],
                Some(TerminalOutcome::ChildExit { code: Some(139) }),
            ),
            today(),
        );
        assert_eq!(r[0].status, GateStatus::ChildError);
        assert_eq!(exit_code(&r), 5);
        assert!(r[0].note.as_deref().unwrap().contains("139"));
    }

    #[test]
    fn regression_is_never_masked_by_child_error() {
        // M2: a fail sharing the scenario with a child crash must stay exit 1.
        let r = build_reports(
            &outcome(
                vec![frame(FrameKind::Mismatch, None, "b")],
                Some(TerminalOutcome::ChildExit { code: None }),
            ),
            today(),
        );
        assert_eq!(exit_code(&r), 1);
    }

    #[test]
    fn step_and_scenario_timeouts_are_exit_1() {
        for t in [
            TerminalOutcome::StepTimeout {
                action: "wait_for_text".into(),
                step_index: 0,
            },
            TerminalOutcome::ScenarioDeadline { step_index: 2 },
        ] {
            let r = build_reports(&outcome(vec![], Some(t)), today());
            assert_eq!(exit_code(&r), 1);
        }
    }

    #[test]
    fn settle_never_stable_maps_to_exit_1() {
        let r = build_reports(
            &outcome(
                vec![],
                Some(TerminalOutcome::SettleNeverStable {
                    action: "expect_golden".into(),
                }),
            ),
            today(),
        );
        assert_eq!(r[0].status, GateStatus::SettleNeverStable);
        assert_eq!(exit_code(&r), 1);
    }

    #[test]
    fn quota_and_infra_map_to_exit_3() {
        for t in [
            TerminalOutcome::QuotaExceeded { limit: 16 },
            TerminalOutcome::Infra {
                message: "daemon down".into(),
            },
        ] {
            let r = build_reports(&outcome(vec![], Some(t)), today());
            assert_eq!(r[0].status, GateStatus::InfraError);
            assert_eq!(exit_code(&r), 3);
        }
    }

    #[test]
    fn no_visual_check_is_scenario_error_exit_2() {
        // Empty frames, no terminal → a scenario that proved nothing.
        let mut o = outcome(vec![], None);
        o.has_visual_check = false;
        let r = build_reports(&o, today());
        assert_eq!(r[0].status, GateStatus::ScenarioError);
        assert_eq!(exit_code(&r), 2);
        assert!(r[0].note.as_deref().unwrap().contains("no_visual_check"));
    }

    #[test]
    fn diff_report_carries_regions_and_pixel_delta() {
        let mut f = frame(FrameKind::Mismatch, None, "b");
        f.verdict = Some(TierVerdict {
            status: GateStatus::Fail,
            cell: CellVerdict {
                status: GateStatus::Fail,
                diff: FrameDiff {
                    cells_changed: 3,
                    regions: vec![LensRowSpan {
                        row: 0,
                        col_start: 2,
                        col_end: 5,
                    }],
                    regions_truncated: false,
                    bounding_box: (0, 2, 1, 5),
                    cursor_moved: false,
                    palette_overridden_differs: false,
                    geometry_changed: false,
                    changed_rows: vec![0],
                    changed_mask: vec![false; 1920],
                    rows: 24,
                    cols: 80,
                },
                reason: None,
            },
            pixel: Some(PixelMetrics {
                status: "fail".into(),
                size_mismatch: false,
                changed_pixels: 12,
                total_pixels: 1200,
                pixel_diff_ratio: 0.01,
                mean_rgba_channel_delta: 5.0,
                max_channel_delta: 40,
            }),
            reason: Some("pixel_diff".into()),
        });
        let d = diff_report(&f).unwrap();
        assert_eq!(d.changed_cells, 3);
        assert_eq!(d.total_cells, 24 * 80);
        assert_eq!(d.max_channel_delta, Some(40));
        assert_eq!(d.regions.as_ref().unwrap().len(), 1);
    }

    /// A `TierVerdict` with the given cell-diff + optional pixel `max_channel_delta`.
    fn tier_verdict(cells_changed: u32, max_delta: Option<u16>) -> TierVerdict {
        TierVerdict {
            status: GateStatus::Fail,
            cell: CellVerdict {
                status: GateStatus::Fail,
                diff: FrameDiff {
                    cells_changed,
                    regions: vec![],
                    regions_truncated: false,
                    bounding_box: (0, 0, 0, 0),
                    cursor_moved: false,
                    palette_overridden_differs: false,
                    geometry_changed: false,
                    changed_rows: vec![],
                    changed_mask: vec![],
                    rows: 24,
                    cols: 80,
                },
                reason: None,
            },
            pixel: max_delta.map(|m| PixelMetrics {
                status: "fail".into(),
                size_mismatch: false,
                changed_pixels: 1,
                total_pixels: 1000,
                pixel_diff_ratio: 0.001,
                mean_rgba_channel_delta: 1.0,
                max_channel_delta: m,
            }),
            reason: Some("pixel_diff".into()),
        }
    }

    // ── adversarial regressions (verdict) ────────────────────────────────────

    #[test]
    fn pixel_only_mismatch_keeps_max_channel_delta() {
        // adv Finding C: a Tier::Pixel mismatch (cells identical, pixels differ) has
        // cells_changed==0 but MUST still carry its max_channel_delta in report.json.
        let mut f = frame(FrameKind::Mismatch, None, "b");
        f.verdict = Some(tier_verdict(0, Some(200)));
        let d = diff_report(&f).expect("a pixel-only mismatch must still carry a DiffReport");
        assert_eq!(d.changed_cells, 0);
        assert_eq!(d.max_channel_delta, Some(200));
        // A passing frame (Match) never gets a diff even with pixel metrics.
        let mut m = frame(FrameKind::Match, None, "a");
        m.verdict = Some(tier_verdict(0, Some(5)));
        assert!(diff_report(&m).is_none());
    }

    #[test]
    fn child_crash_in_no_visual_surfaces_as_child_error() {
        // adv Finding D: a no-visual scenario whose child crashed must surface child_error
        // (exit 5), not be relabeled scenario_error (exit 2).
        let mut o = outcome(vec![], Some(TerminalOutcome::ChildExit { code: Some(1) }));
        o.has_visual_check = false;
        let r = build_reports(&o, today());
        assert_eq!(r[0].status, GateStatus::ChildError);
        assert_eq!(exit_code(&r), 5);
    }

    #[test]
    fn empty_fingerprint_is_malformed_not_green() {
        // adv Finding A: a fingerprinted xfail with a blank fingerprint pins nothing.
        let x = xfail("2099-01-01", Some(""));
        assert_eq!(
            status_of(frame(FrameKind::Mismatch, Some(x), "")),
            GateStatus::ScenarioError
        );
        let ws = xfail("2099-01-01", Some("   "));
        assert_eq!(
            status_of(frame(FrameKind::Mismatch, Some(ws), "livesha")),
            GateStatus::ScenarioError
        );
    }

    #[test]
    fn whitespace_or_noncanonical_expiry_is_rejected() {
        // adv Finding B: surrounding whitespace / non-canonical forms are not accepted.
        for bad in [" 2099-01-01", "2099-01-01 ", "2099-01-01\t", "2099-1-01"] {
            assert_eq!(
                status_of(frame(FrameKind::Mismatch, Some(xfail(bad, None)), "s")),
                GateStatus::ScenarioError,
                "expiry {bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn terminal_note_redacts_secret_and_flattens() {
        // adv Agent C MAJOR-1/3: an internal error message carrying a secret / newline / ESC
        // must be redacted + flattened in the report note. Split so no literal token is in
        // source (GitHub push-protection).
        let secret = concat!("ghp", "_0123456789abcdefghijklmnopqrstuvwxyzAB");
        let o = outcome(
            vec![],
            Some(TerminalOutcome::Infra {
                message: format!("lens.run failed:\n\x1b[31m--token={secret}"),
            }),
        );
        let r = build_reports(&o, today());
        let note = r[0].note.clone().unwrap();
        assert!(!note.contains(secret), "note leaked the secret: {note}");
        assert!(
            !note.contains('\n') && !note.contains('\u{1b}'),
            "note not flattened: {note:?}"
        );
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains(secret), "report.json leaked the secret");
    }

    // ── 084 F4: blessing must never launder a scenario-level failure ─────────

    #[test]
    fn scenario_floor_carries_a_terminal_failure_with_no_frames() {
        let o = outcome(
            vec![],
            Some(TerminalOutcome::StepTimeout {
                action: "wait_for_text".into(),
                step_index: 0,
            }),
        );
        assert_eq!(scenario_floor(&o), GateStatus::Fail);
        assert_eq!(build_reports(&o, today())[0].status, GateStatus::Fail);
    }

    #[test]
    fn scenario_floor_carries_a_child_error_and_no_visual_check() {
        let crashed = outcome(vec![], Some(TerminalOutcome::ChildExit { code: Some(2) }));
        assert_eq!(scenario_floor(&crashed), GateStatus::ChildError);

        // No terminal disposition and no `expect_golden` → the no-visual guard.
        let silent = outcome(vec![], None);
        assert_eq!(scenario_floor(&silent), GateStatus::ScenarioError);
    }

    #[test]
    fn scenario_floor_is_pass_when_only_frames_decide() {
        let o = outcome(vec![frame(FrameKind::Match, None, "sha")], None);
        assert_eq!(scenario_floor(&o), GateStatus::Pass);
    }

    /// The 084 F4 blocker: `--on-missing create` / `--update` re-roll the scenario status
    /// through `apply_blessed`. A `step_timeout` yields ZERO frames, so a fold seeded at
    /// `Pass` returned `pass`/exit 0 while blessing nothing — CI keying on the exit code
    /// went green over a scenario that never rendered. The re-roll must start at the floor.
    #[test]
    fn blessing_nothing_cannot_launder_a_step_timeout_into_pass() {
        use super::super::bless::{BlessManifest, apply_blessed};

        let o = outcome(
            vec![],
            Some(TerminalOutcome::StepTimeout {
                action: "wait_for_text".into(),
                step_index: 0,
            }),
        );
        let mut reports = build_reports(&o, today());
        assert_eq!(reports[0].status, GateStatus::Fail);

        apply_blessed(&mut reports, &BlessManifest::default(), scenario_floor(&o));

        assert_eq!(
            reports[0].status,
            GateStatus::Fail,
            "blessing 0 goldens laundered a step_timeout into {:?}",
            reports[0].status
        );
        assert_eq!(reports[0].status.exit_code(), 1);
    }
}
