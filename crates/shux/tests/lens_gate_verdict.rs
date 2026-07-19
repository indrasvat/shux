//! Task 082 — verdict / report.json / summary / xfail / bless / init (`shux lens gate`).
//! GATE lane (`GATE-TEST-CHANGE:` to touch). `test = false` — run serially under the leak
//! guard via `make test-lens-gate-verdict`, NEVER in the default parallel run.
//!
//! Drives the REAL `shux` binary end-to-end (design D1). Asserts the OBSERVABLE 082
//! contract: the frozen `report.json` schema + exit map, the ASCII summary, first-run
//! `--on-missing`, xfail governance, `--update`/bless safety, `init`, and report privacy.

mod lens_common;

use std::path::Path;

use lens_common::Harness;
use shux_vt::{GateStatus, ScenarioReport};

/// A parsed gate invocation: raw streams + exit code.
struct Gate {
    stdout: String,
    stderr: String,
    exit: i32,
}

impl Gate {
    fn report(&self) -> Vec<ScenarioReport> {
        serde_json::from_str(self.stdout.trim()).unwrap_or_else(|e| {
            panic!(
                "stdout is not a report array: {e}\nstdout:\n{}",
                self.stdout
            )
        })
    }
}

/// Run `shux lens gate <args...>` with an optional CI flag; capture streams + exit.
fn gate(h: &Harness, args: &[&str], ci: bool) -> Gate {
    let mut cmd = h.shux();
    cmd.args(["lens", "gate"]);
    cmd.args(args);
    if ci {
        cmd.env("CI", "true");
    } else {
        cmd.env_remove("CI");
    }
    let out = cmd.output().expect("spawn shux lens gate");
    Gate {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        exit: out.status.code().unwrap_or(-1),
    }
}

/// Write a scenario TOML to a temp file and return its path string.
fn write_scenario(dir: &Path, name: &str, toml: &str) -> String {
    let p = dir.join(format!("{name}.toml"));
    std::fs::write(&p, toml).unwrap();
    p.to_string_lossy().into_owned()
}

/// A scenario that draws `text`, holds the frame, and expects golden `frame`. `xfail` is
/// an optional inline `[steps.xfail]` block appended to the expect_golden step.
fn scenario(name: &str, text: &str, xfail: Option<&str>) -> String {
    let xf = xfail.unwrap_or("");
    format!(
        r#"name = "{name}"
command = ["/bin/sh", "-c", "printf '{text}'; exec cat"]
[terminal]
rows = 12
cols = 40
[[steps]]
action = "wait_for_text"
text = "{text}"
timeout_ms = 5000
[[steps]]
action = "settle"
quiet_ms = 250
timeout_ms = 5000
[[steps]]
action = "expect_golden"
name = "frame"
tier = "cell"
{xf}
"#
    )
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

// ── L1 report + exit ──────────────────────────────────────────────────────────

#[test]
fn missing_golden_report_is_ci_safe_regression() {
    let h = Harness::new();
    let d = tmp();
    let g = tmp();
    let scn = write_scenario(d.path(), "rep", &scenario("rep", "HELLO", None));
    let out = gate(
        &h,
        &[
            "--report",
            "-",
            "--golden-dir",
            &g.path().to_string_lossy(),
            &scn,
        ],
        false,
    );
    assert_eq!(
        out.exit, 1,
        "missing golden is a regression; stderr:\n{}",
        out.stderr
    );
    let report = out.report();
    assert_eq!(report.len(), 1);
    assert_eq!(report[0].scenario, "rep");
    assert_eq!(report[0].status, GateStatus::MissingGolden);
    assert!(!report[0].os.is_empty() && !report[0].arch.is_empty());
    assert_eq!(report[0].frames[0].status, GateStatus::MissingGolden);
    // Exit equals the rolled-up status's frozen exit code.
    assert_eq!(out.exit as u8, report[0].status.exit_code());
}

#[test]
fn report_to_file_keeps_summary_on_stdout() {
    let h = Harness::new();
    let d = tmp();
    let g = tmp();
    let rep = d.path().join("report.json");
    let scn = write_scenario(d.path(), "f2", &scenario("f2", "HI", None));
    let out = gate(
        &h,
        &[
            "--report",
            &rep.to_string_lossy(),
            "--golden-dir",
            &g.path().to_string_lossy(),
            &scn,
        ],
        false,
    );
    // The report is a valid array in the file; the summary is on stdout.
    let file = std::fs::read_to_string(&rep).unwrap();
    let parsed: Vec<ScenarioReport> = serde_json::from_str(file.trim()).unwrap();
    assert_eq!(parsed[0].status, GateStatus::MissingGolden);
    assert!(
        out.stdout.contains("verdict=missing_golden"),
        "summary on stdout:\n{}",
        out.stdout
    );
}

#[test]
fn format_json_puts_report_on_stdout() {
    let h = Harness::new();
    let d = tmp();
    let g = tmp();
    let scn = write_scenario(d.path(), "fj", &scenario("fj", "HI", None));
    // Global --format is a leading flag.
    let mut cmd = h.shux();
    cmd.args([
        "--format",
        "json",
        "lens",
        "gate",
        "--golden-dir",
        &g.path().to_string_lossy(),
        &scn,
    ]);
    let raw = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&raw.stdout);
    let parsed: Vec<ScenarioReport> = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("--format json stdout not a report: {e}\n{stdout}"));
    assert_eq!(parsed[0].status, GateStatus::MissingGolden);
}

#[test]
fn summary_table_is_ansi_free() {
    let h = Harness::new();
    let d = tmp();
    let g = tmp();
    let scn = write_scenario(d.path(), "sm", &scenario("sm", "HI", None));
    let out = gate(
        &h,
        &["--golden-dir", &g.path().to_string_lossy(), &scn],
        false,
    );
    assert!(
        !out.stdout.contains('\u{1b}'),
        "summary must be ANSI-free:\n{}",
        out.stdout
    );
    assert!(out.stdout.contains("FRAME"));
    assert!(out.stdout.contains("STATUS"));
}

// ── L1 first-run / --on-missing ───────────────────────────────────────────────

#[test]
fn on_missing_create_blesses_then_passes() {
    let h = Harness::new();
    let d = tmp();
    let g = tmp();
    let scn = write_scenario(d.path(), "cr", &scenario("cr", "ALPHA", None));
    let gd = g.path().to_string_lossy().into_owned();
    // First run creates the golden locally.
    let created = gate(
        &h,
        &["--on-missing", "create", "--golden-dir", &gd, &scn],
        false,
    );
    assert_eq!(
        created.exit, 0,
        "create is green; stderr:\n{}",
        created.stderr
    );
    assert!(
        g.path().join("frame.capture.json").exists(),
        "golden was written"
    );
    assert!(
        g.path().join("BASELINE-APPROVAL.md").exists(),
        "approval log appended"
    );
    // Re-run: the golden now matches → pass, exit 0.
    let rerun = gate(&h, &["--report", "-", "--golden-dir", &gd, &scn], false);
    assert_eq!(rerun.exit, 0);
    assert_eq!(rerun.report()[0].status, GateStatus::Pass);
}

#[test]
fn on_missing_create_is_refused_in_ci() {
    let h = Harness::new();
    let d = tmp();
    let g = tmp();
    let scn = write_scenario(d.path(), "cci", &scenario("cci", "HI", None));
    let out = gate(
        &h,
        &[
            "--on-missing",
            "create",
            "--golden-dir",
            &g.path().to_string_lossy(),
            &scn,
        ],
        true,
    );
    assert_eq!(
        out.exit, 6,
        "create in CI is update_refused (exit 6); stderr:\n{}",
        out.stderr
    );
    assert!(
        !g.path().join("frame.capture.json").exists(),
        "no golden minted in CI"
    );
}

// ── L1 xfail governance ───────────────────────────────────────────────────────

/// Bless a golden for `frame` capturing `text`, into `golden_dir`.
fn bless_golden(h: &Harness, dir: &Path, golden_dir: &str, name: &str, text: &str) {
    let scn = write_scenario(dir, name, &scenario(name, text, None));
    let out = gate(
        h,
        &["--on-missing", "create", "--golden-dir", golden_dir, &scn],
        false,
    );
    assert_eq!(out.exit, 0, "bless setup failed: {}", out.stderr);
}

#[test]
fn xfail_valid_is_green() {
    let h = Harness::new();
    let d = tmp();
    let g = tmp();
    let gd = g.path().to_string_lossy().into_owned();
    bless_golden(&h, d.path(), &gd, "base", "ALPHA");
    // A DIFFERENT capture (BRAVO) vs the ALPHA golden → mismatch; a valid xfail → green.
    let xf = "[steps.xfail]\nreason = \"known\"\nowner = \"aria\"\nissue = \"#1\"\nexpiry = \"2099-12-31\"\n";
    let scn = write_scenario(d.path(), "xf", &scenario("xf", "BRAVO", Some(xf)));
    let out = gate(&h, &["--report", "-", "--golden-dir", &gd, &scn], false);
    assert_eq!(out.exit, 0, "valid xfail is green; stderr:\n{}", out.stderr);
    assert_eq!(out.report()[0].frames[0].status, GateStatus::Xfail);
}

#[test]
fn xfail_expired_is_a_regression() {
    let h = Harness::new();
    let d = tmp();
    let g = tmp();
    let gd = g.path().to_string_lossy().into_owned();
    bless_golden(&h, d.path(), &gd, "base", "ALPHA");
    let xf = "[steps.xfail]\nreason = \"known\"\nowner = \"aria\"\nissue = \"#1\"\nexpiry = \"2000-01-01\"\n";
    let scn = write_scenario(d.path(), "xe", &scenario("xe", "BRAVO", Some(xf)));
    let out = gate(&h, &["--report", "-", "--golden-dir", &gd, &scn], false);
    assert_eq!(
        out.exit, 1,
        "expired xfail is a regression; stderr:\n{}",
        out.stderr
    );
    assert_eq!(out.report()[0].frames[0].status, GateStatus::XfailExpired);
}

#[test]
fn xpass_forces_promotion() {
    let h = Harness::new();
    let d = tmp();
    let g = tmp();
    let gd = g.path().to_string_lossy().into_owned();
    bless_golden(&h, d.path(), &gd, "base", "ALPHA");
    // Same capture (ALPHA) MATCHES the golden, but an xfail is still declared → xpass.
    let xf = "[steps.xfail]\nreason = \"known\"\nowner = \"aria\"\nissue = \"#1\"\nexpiry = \"2099-12-31\"\n";
    let scn = write_scenario(d.path(), "xp", &scenario("xp", "ALPHA", Some(xf)));
    let out = gate(&h, &["--report", "-", "--golden-dir", &gd, &scn], false);
    assert_eq!(
        out.exit, 1,
        "xpass forces promotion (exit 1); stderr:\n{}",
        out.stderr
    );
    assert_eq!(out.report()[0].frames[0].status, GateStatus::Xpass);
}

// ── L1 stale golden ───────────────────────────────────────────────────────────

#[test]
fn tampered_golden_is_stale_not_fail() {
    let h = Harness::new();
    let d = tmp();
    let g = tmp();
    let gd = g.path().to_string_lossy().into_owned();
    bless_golden(&h, d.path(), &gd, "base", "ALPHA");
    // Drift the blessed sidecar's font fingerprint → the golden is refused as stale (a
    // build/config drift), distinct from a content fail.
    let fp = g.path().join("frame.fingerprint.json");
    let text = std::fs::read_to_string(&fp).unwrap();
    let mut sidecar: serde_json::Value = serde_json::from_str(&text).unwrap();
    sidecar["raster_font_fingerprint"] = serde_json::Value::String("drifted-build".into());
    std::fs::write(&fp, serde_json::to_string_pretty(&sidecar).unwrap()).unwrap();
    let scn = write_scenario(d.path(), "st", &scenario("st", "ALPHA", None));
    let out = gate(&h, &["--report", "-", "--golden-dir", &gd, &scn], false);
    assert_eq!(out.exit, 1);
    assert_eq!(
        out.report()[0].frames[0].status,
        GateStatus::StaleGolden,
        "a tampered golden is stale, distinct from a content fail"
    );
}

// ── L1 privacy ────────────────────────────────────────────────────────────────

#[test]
fn report_carries_no_raw_env_or_secret() {
    let h = Harness::new();
    let d = tmp();
    let g = tmp();
    // A scenario whose [env] sets a secret-looking value the child would print.
    let toml = r#"name = "priv"
command = ["/bin/sh", "-c", "printf 'READY'; exec cat"]
[terminal]
rows = 12
cols = 40
[env]
MY_SECRET = "AKIAIOSFODNN7EXAMPLE"
[[steps]]
action = "wait_for_text"
text = "READY"
timeout_ms = 5000
[[steps]]
action = "expect_golden"
name = "frame"
tier = "cell"
"#;
    let scn = write_scenario(d.path(), "priv", toml);
    let out = gate(
        &h,
        &[
            "--report",
            "-",
            "--golden-dir",
            &g.path().to_string_lossy(),
            &scn,
        ],
        false,
    );
    // The report + summary must carry neither the secret value nor the raw env key.
    let combined = format!("{}{}", out.stdout, out.stderr);
    assert!(
        !combined.contains("AKIAIOSFODNN7EXAMPLE"),
        "report leaked a secret env value"
    );
    assert!(
        !combined.contains("MY_SECRET"),
        "report leaked a raw env key"
    );
}

// ── crash-after-final-frame (adv Agent D BLOCKER regression) ───────────────────

#[test]
fn delayed_post_compare_crash_does_not_false_pass() {
    let h = Harness::new();
    let d = tmp();
    let g = tmp();
    let gd = g.path().to_string_lossy().into_owned();
    // Bless a golden capturing "READY".
    bless_golden(&h, d.path(), &gd, "base", "READY");
    // A scenario whose child paints the SAME frame (→ frame_match), then crashes ~1s later
    // (within the 2s post-compare grace). It must NOT false-pass: the crash → child_error.
    let crash = r#"name = "crash"
command = ["/bin/sh", "-c", "printf 'READY'; sleep 1; kill -SEGV $$"]
[terminal]
rows = 12
cols = 40
[[steps]]
action = "wait_for_text"
text = "READY"
timeout_ms = 5000
[[steps]]
action = "settle"
quiet_ms = 250
timeout_ms = 5000
[[steps]]
action = "expect_golden"
name = "frame"
tier = "cell"
"#;
    let scn = write_scenario(d.path(), "crash", crash);
    let out = gate(&h, &["--report", "-", "--golden-dir", &gd, &scn], false);
    assert_eq!(
        out.exit, 5,
        "a crash ~1s after the matching frame must surface as child_error (exit 5), not \
         false-pass; stderr:\n{}",
        out.stderr
    );
    assert_eq!(out.report()[0].status, GateStatus::ChildError);
}

#[test]
fn clean_exit_after_matching_frame_is_a_pass() {
    let h = Harness::new();
    let d = tmp();
    let g = tmp();
    let gd = g.path().to_string_lossy().into_owned();
    bless_golden(&h, d.path(), &gd, "base", "READY");
    // The child paints the matching frame, holds it, then exits CLEANLY (0). A graceful
    // shutdown after a successful compare is a PASS, not child_error (impl-review #6).
    let clean = r#"name = "clean"
command = ["/bin/sh", "-c", "printf 'READY'; sleep 1; exit 0"]
[terminal]
rows = 12
cols = 40
[[steps]]
action = "wait_for_text"
text = "READY"
timeout_ms = 5000
[[steps]]
action = "settle"
quiet_ms = 250
timeout_ms = 5000
[[steps]]
action = "expect_golden"
name = "frame"
tier = "cell"
"#;
    let scn = write_scenario(d.path(), "clean", clean);
    let out = gate(&h, &["--report", "-", "--golden-dir", &gd, &scn], false);
    assert_eq!(
        out.exit, 0,
        "a clean exit-0 after a matching frame is a pass; stderr:\n{}",
        out.stderr
    );
    assert_eq!(out.report()[0].status, GateStatus::Pass);
}

// ── L2 retries / init / review ────────────────────────────────────────────────

#[test]
fn retries_are_carried_into_the_report() {
    let h = Harness::new();
    let d = tmp();
    let g = tmp();
    let scn = write_scenario(d.path(), "rt", &scenario("rt", "HI", None));
    let out = gate(
        &h,
        &[
            "--report",
            "-",
            "--retries",
            "3",
            "--golden-dir",
            &g.path().to_string_lossy(),
            &scn,
        ],
        false,
    );
    let note = out.report()[0].note.clone().unwrap_or_default();
    assert!(
        note.contains("retries=3"),
        "retries must be reported; note={note:?}"
    );
}

#[test]
fn init_scaffolds_and_mints_first_goldens() {
    let h = Harness::new();
    let d = tmp();
    let out = {
        let mut cmd = h.shux();
        cmd.args([
            "lens",
            "gate",
            "init",
            "demoinit",
            "--dir",
            &d.path().to_string_lossy(),
        ]);
        cmd.env_remove("CI");
        cmd.output().unwrap()
    };
    assert!(
        out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        d.path().join("demoinit.toml").exists(),
        "scenario scaffolded"
    );
    // The scaffolded run mints goldens under the default golden dir next to the scenario.
    // The template's expect_golden frame is named `start`.
    assert!(
        d.path()
            .join("goldens/demoinit/start.capture.json")
            .exists(),
        "first golden minted"
    );
}

#[test]
fn init_is_refused_in_ci() {
    let h = Harness::new();
    let d = tmp();
    let mut cmd = h.shux();
    cmd.args([
        "lens",
        "gate",
        "init",
        "ciinit",
        "--dir",
        &d.path().to_string_lossy(),
    ]);
    cmd.env("CI", "true");
    let out = cmd.output().unwrap();
    assert_eq!(out.status.code(), Some(6), "init in CI is refused");
    assert!(!d.path().join("ciinit.toml").exists(), "no scaffold in CI");
}

#[test]
fn review_refuses_non_interactive() {
    let h = Harness::new();
    let d = tmp();
    let g = tmp();
    let scn = write_scenario(d.path(), "rv", &scenario("rv", "HI", None));
    // stdin is not a TTY under Command::output() → review refuses (exit 6).
    let mut cmd = h.shux();
    cmd.args([
        "lens",
        "gate",
        "review",
        &scn,
        "--golden-dir",
        &g.path().to_string_lossy(),
    ]);
    cmd.env_remove("CI");
    let out = cmd.output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(6),
        "non-interactive review is refused"
    );
}
