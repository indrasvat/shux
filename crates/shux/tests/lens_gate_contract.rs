//! FROZEN RED contract lane for `shux lens gate` (task 078).
//!
//! These tests pin the OBSERVABLE gate contract that task 078 owns — the closed
//! status set, the exit-code map, and the `report.json` schema — by driving the
//! (not-yet-built) `shux lens gate` verb and asserting its output conforms. They
//! are RED today (the verb does not exist) and each is annotated with the task
//! that will turn it GREEN **without editing it** (the freeze guard forbids
//! weakening). This file is `test = false`, so it never runs in `make check` /
//! CI `nextest --workspace`; run it explicitly with `make test-lens-gate-contract`
//! (EXPECTED to fail until 081/082 land). FROZEN — GATE-TEST-CHANGE: trailer.
//!
//! Retirement plan:
//!
//!   - 081 (runner)  makes the scenario actually execute.
//!   - 082 (verdict) makes the report/exit contract observable.
//!
//! A later task flips each case green by BUILDING the feature, never by changing
//! the assertion.

mod lens_common;

use lens_common::Harness;
use shux_vt::{GateStatus, ScenarioReport};

fn hello_scenario(h: &Harness) -> String {
    h.repo_root()
        .join(".shux/fixtures/lens-gate/scenarios/hello.toml")
        .to_string_lossy()
        .into_owned()
}

fn parse_report(stdout: &[u8]) -> Vec<ScenarioReport> {
    serde_json::from_slice(stdout).expect("gate stdout must be a report.json array (frozen schema)")
}

/// RETIRED BY: 081 (runner) + 082 (report). The gate verb runs a scenario and
/// emits a `report.json` that parses into the frozen schema.
#[test]
fn gate_emits_conforming_report_json() {
    let h = Harness::new();
    let out = h.cli(&["lens", "gate", "--report", "-", &hello_scenario(&h)]);
    let report = parse_report(&out.stdout);
    assert!(!report.is_empty(), "report has at least one scenario");
    assert_eq!(report[0].scenario, "hello");
    // Provenance is stamped (goldens are platform-sensitive).
    assert!(!report[0].os.is_empty() && !report[0].arch.is_empty());
}

/// RETIRED BY: 082. The process exit code equals the rolled-up worst-frame
/// status's frozen exit code — the exit contract (§7.4) is observable.
#[test]
fn gate_exit_code_matches_rolled_up_status() {
    let h = Harness::new();
    let out = h.cli(&["lens", "gate", "--report", "-", &hello_scenario(&h)]);
    let report = parse_report(&out.stdout);
    let worst = report
        .iter()
        .flat_map(|s| s.frames.iter())
        .fold(GateStatus::Pass, |acc, f| acc.worst(f.status));
    let code = out.status.code().expect("gate exited normally");
    assert_eq!(
        code as u8,
        worst.exit_code(),
        "exit {code} must match worst status {worst:?} → {}",
        worst.exit_code()
    );
}

/// RETIRED BY: 082. With no committed golden, a frame is `missing_golden` and
/// the run is a CI-safe regression (exit 1), never a silent pass.
#[test]
fn gate_missing_golden_fails_ci_safe() {
    let h = Harness::new();
    let out = h.cli(&["lens", "gate", "--report", "-", &hello_scenario(&h)]);
    let report = parse_report(&out.stdout);
    let has_missing = report
        .iter()
        .flat_map(|s| s.frames.iter())
        .any(|f| f.status == GateStatus::MissingGolden);
    assert!(has_missing, "a golden-less frame must be missing_golden");
    assert_eq!(
        out.status.code(),
        Some(1),
        "missing golden is a regression in CI"
    );
}

/// RETIRED BY: 082. A bless/update is refused in CI mode — exit 6, the
/// update_refused status — so a golden can never be self-minted in CI.
#[test]
fn gate_update_is_refused_in_ci_mode() {
    let h = Harness::new();
    let mut cmd = h.shux();
    cmd.args(["lens", "gate", "--update", "start", &hello_scenario(&h)]);
    cmd.env("CI", "true");
    let out = cmd.output().expect("spawn gate");
    assert_eq!(
        out.status.code(),
        Some(6),
        "an --update in CI mode must be refused with exit 6 (update_refused)"
    );
}

/// RETIRED BY: 082. `shux lens gate --help` documents the verb (so the gate is
/// discoverable, not a hidden surface).
#[test]
fn gate_help_documents_the_verb() {
    let h = Harness::new();
    let out = h.cli(&["lens", "gate", "--help"]);
    assert!(out.status.success(), "lens gate --help must succeed");
    let help = String::from_utf8_lossy(&out.stdout).to_lowercase();
    assert!(
        help.contains("golden") || help.contains("scenario"),
        "help must describe the gate (golden/scenario)"
    );
}
