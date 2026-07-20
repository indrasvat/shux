//! Task 081 — daemon-backed scenario runner (`shux lens gate`). GATE lane
//! (`GATE-TEST-CHANGE:` to touch). `test = false` — run serially under the leak guard
//! via `make test-lens-gate-run`, NEVER in the default parallel `cargo test`/CI run.
//!
//! These drive the REAL system through the built `shux` binary (design D1): parse →
//! deny-by-default spawn → the agnostic step core → glance → compare, asserting on the
//! raw runner-signal trace (`--trace`) — NEVER a frozen `GateStatus` name (082 owns
//! that). Golden blessing is the cell-tier PLUMBING proof (080 D3): the test controls
//! the golden files; renderer correctness is not self-minted.

mod lens_common;

use std::path::Path;

use lens_common::Harness;
use shux_vt::{
    FINGERPRINT_SCHEMA, Fingerprint, FrameEnvelope, MaskSet, RENDERER_FORMAT_VERSION,
    SCHEMA_VERSION, Tier, TolParams, capture_sha256, mask_hash, unicode_width_version,
};

/// A parsed gate run: the raw signal trace + the process exit code.
struct GateRun {
    signals: Vec<serde_json::Value>,
    exit: i32,
    stderr: String,
}

impl GateRun {
    fn kinds(&self) -> Vec<String> {
        self.signals
            .iter()
            .filter_map(|s| s.get("signal").and_then(|v| v.as_str()).map(String::from))
            .collect()
    }
    fn has(&self, kind: &str) -> bool {
        self.kinds().iter().any(|k| k == kind)
    }
    fn find(&self, kind: &str) -> Option<&serde_json::Value> {
        self.signals
            .iter()
            .find(|s| s.get("signal").and_then(|v| v.as_str()) == Some(kind))
    }
    fn count(&self, kind: &str) -> usize {
        self.kinds().iter().filter(|k| *k == kind).count()
    }
}

/// Write a scenario to a temp file and run `shux lens gate` against it, tracing to a
/// temp file we then parse. `golden_dir` + trailing `argv` override are optional.
fn run_gate(
    h: &Harness,
    dir: &Path,
    scenario_toml: &str,
    golden_dir: Option<&Path>,
    argv_override: &[&str],
) -> GateRun {
    let scenario_path = dir.join(format!("scn-{}.toml", lens_common::unique()));
    std::fs::write(&scenario_path, scenario_toml).unwrap();
    let trace_path = dir.join(format!("trace-{}.ndjson", lens_common::unique()));

    let scenario_s = scenario_path.to_string_lossy().into_owned();
    let trace_s = trace_path.to_string_lossy().into_owned();
    let golden_s = golden_dir.map(|g| g.to_string_lossy().into_owned());

    let mut args: Vec<&str> = vec!["lens", "gate", &scenario_s, "--trace", &trace_s];
    if let Some(g) = golden_s.as_deref() {
        args.push("--golden-dir");
        args.push(g);
    }
    if !argv_override.is_empty() {
        args.push("--");
        args.extend_from_slice(argv_override);
    }

    let out = h.cli(&args);
    let trace = std::fs::read_to_string(&trace_path).unwrap_or_default();
    let signals = trace
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("trace line is JSON"))
        .collect();
    GateRun {
        signals,
        exit: out.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Capture a live cell frame (`FrameEnvelope` canonical JSON) via `lens.run` + glance —
/// the source for a blessed cell golden. Cell content is font-independent, so the
/// Harness daemon's font config does not affect it.
fn capture_cell_json(h: &Harness, argv: &[&str], cols: u16, rows: u16, sentinel: &str) -> String {
    let run = h.rpc_ok(
        "lens.run",
        serde_json::json!({ "argv": argv, "cols": cols, "rows": rows }),
    );
    let pane = run["pane_id"].as_str().unwrap().to_string();
    let sid = run["session_id"].as_str().unwrap().to_string();
    h.wait_for(&pane, sentinel, 5000)
        .unwrap_or_else(|e| panic!("capture sentinel {sentinel:?} never drew: {e}"));
    let _ = h.rpc_raw(
        "pane.wait_settled",
        serde_json::json!({ "pane_id": pane, "quiet_ms": 200, "timeout_ms": 2000 }),
    );
    let g = h.rpc_ok(
        "pane.glance",
        serde_json::json!({ "pane_id": pane, "include_cells": true, "include_png": false }),
    );
    let cells = g.get("cells").expect("glance cells").clone();
    h.kill_session(&sid);
    serde_json::to_string_pretty(&cells).unwrap()
}

/// Bless a cell golden matching the runner's `current_fp` (cell tier; the non-stale
/// fields — font fp / schema / tier / mask / platform — must agree). `scenario_hash`/
/// `cmd_env_hash` differ (not stale triggers).
fn bless_cell(golden_dir: &Path, name: &str, capture_json: &str) {
    std::fs::create_dir_all(golden_dir).unwrap();
    std::fs::write(
        golden_dir.join(format!("{name}.capture.json")),
        capture_json,
    )
    .unwrap();
    let env = FrameEnvelope::from_canonical_json(capture_json).expect("golden parses");
    let fp = Fingerprint {
        fp_schema: FINGERPRINT_SCHEMA,
        schema: SCHEMA_VERSION,
        renderer_format_version: RENDERER_FORMAT_VERSION,
        raster_font_fingerprint: shux_raster::builtin_font_fingerprint(16.0),
        unicode_width_ver: unicode_width_version(),
        tol: Tier::Cell,
        tol_params: TolParams::default(),
        mask_hash: mask_hash(&MaskSet::new()),
        platform: None,
        shux_version: "test".into(),
        capture_sha256: capture_sha256(&env),
        rgba_sha256: None,
        png_sha256: None,
        scenario_hash: String::new(),
        cmd_env_hash: String::new(),
    };
    std::fs::write(
        golden_dir.join(format!("{name}.fingerprint.json")),
        serde_json::to_string_pretty(&fp).unwrap(),
    )
    .unwrap();
}

/// No scratch session may survive a gate run (design D10).
fn assert_no_scratch_leak(h: &Harness) {
    let list = h.rpc_ok(
        "session.list",
        serde_json::json!({ "include_scratch": true }),
    );
    let n = list["sessions"].as_array().map(|a| a.len()).unwrap_or(0);
    assert_eq!(n, 0, "gate left {n} scratch session(s) behind: {list}");
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

// ══════════════════════════════════════════════════════════════════════════════
// L2 CLI
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn cli_help_documents_the_gate_verb() {
    let h = Harness::new();
    let out = h.cli(&["lens", "gate", "--help"]);
    assert!(out.status.success(), "lens gate --help must succeed");
    let help = String::from_utf8_lossy(&out.stdout).to_lowercase();
    assert!(
        help.contains("scenario") && help.contains("golden"),
        "help must describe the gate: {help}"
    );
}

#[test]
fn missing_scenario_file_is_actionable() {
    let h = Harness::new();
    let out = h.cli(&["lens", "gate", "/no/such/scenario.toml", "--trace", "-"]);
    // Provisional exit 2 (parse/usage) — 082 installs the frozen map.
    assert_eq!(out.status.code(), Some(2));
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(combined.contains("parse") || combined.to_lowercase().contains("read"));
}

// ══════════════════════════════════════════════════════════════════════════════
// L1 parse (mechanic; status/exit mapping asserted in 082)
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn malformed_scenario_is_parse_error() {
    let h = Harness::new();
    let d = tmp();
    let run = run_gate(&h, d.path(), "this is not valid toml {{{", None, &[]);
    assert!(run.has("parse_error"), "kinds: {:?}", run.kinds());
    assert_eq!(run.exit, 2);
}

#[test]
fn unknown_step_action_fails_closed() {
    let h = Harness::new();
    let d = tmp();
    let scn = r#"
name = "bad"
command = ["true"]
[[steps]]
action = "teleport"
"#;
    let run = run_gate(&h, d.path(), scn, None, &[]);
    assert!(run.has("parse_error"), "kinds: {:?}", run.kinds());
    assert!(
        run.find("parse_error")
            .and_then(|s| s.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .contains("teleport")
    );
}

#[test]
fn deferred_mouse_step_is_rejected_not_ignored() {
    // Design D10: mouse/focus/bracketed-paste are non-supported, rejected explicitly.
    let h = Harness::new();
    let d = tmp();
    let scn = "name=\"m\"\ncommand=[\"true\"]\n[[steps]]\naction=\"mouse\"\n";
    let run = run_gate(&h, d.path(), scn, None, &[]);
    assert!(run.has("parse_error"));
    assert!(
        run.find("parse_error")
            .and_then(|s| s.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .contains("not supported")
    );
}

// ══════════════════════════════════════════════════════════════════════════════
// L1 env — deterministic sanitation + deny-by-default (design D4/D5)
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn child_env_is_sanitized_and_denies_host() {
    let h = Harness::new();
    let d = tmp();
    // The child dumps its env. env_clear means it sees ONLY the deterministic plan:
    // the plan vars are present; the HOST home path (`/Users/...`, present in the
    // daemon's own env) never leaks in.
    let scn = r#"
name = "envcheck"
command = ["/bin/sh", "-c", "env | sort; echo ENVDONE; exec cat"]
[terminal]
rows = 24
cols = 80
[[steps]]
action = "wait_for_text"
text = "ENVDONE"
timeout_ms = 5000
[[steps]]
action = "assert_contains"
text = "LC_ALL=C.UTF-8"
[[steps]]
action = "assert_contains"
text = "TZ=UTC"
[[steps]]
action = "assert_contains"
text = "TERM=xterm-256color"
[[steps]]
action = "assert_not_contains"
text = "/Users/"
"#;
    let run = run_gate(&h, d.path(), scn, None, &[]);
    // All four smoke asserts passed → sanitized plan reached the child + no host home leak.
    assert_eq!(
        run.count("assert_passed"),
        4,
        "kinds: {:?} stderr: {}",
        run.kinds(),
        run.stderr
    );
    assert_eq!(run.count("assert_failed"), 0, "kinds: {:?}", run.kinds());
    // cmd_env_hash is recorded on scenario_start (provenance).
    let start = run.find("scenario_start").expect("scenario_start");
    assert!(
        !start["cmd_env_hash"].as_str().unwrap_or("").is_empty(),
        "cmd_env_hash must be recorded"
    );
    assert_no_scratch_leak(&h);
}

// ══════════════════════════════════════════════════════════════════════════════
// L3 dogfood — cell golden lifecycle (absent → match → mismatch → untrusted)
// ══════════════════════════════════════════════════════════════════════════════

fn hold_scenario(text: &str) -> String {
    format!(
        r#"
name = "hold"
command = ["/bin/sh", "-c", "printf '{text}'; exec cat"]
[terminal]
rows = 24
cols = 80
[[steps]]
action = "wait_for_text"
text = "MATCHME"
timeout_ms = 5000
[[steps]]
action = "settle"
quiet_ms = 250
timeout_ms = 5000
[[steps]]
action = "expect_golden"
name = "frame"
tier = "cell"
"#
    )
}

#[test]
fn cell_golden_absent_then_match_then_mismatch() {
    let h = Harness::new();
    let d = tmp();
    let golden = tmp();

    // 1. No golden → golden_absent (never a silent pass).
    let run = run_gate(
        &h,
        d.path(),
        &hold_scenario("MATCHME"),
        Some(golden.path()),
        &[],
    );
    assert!(run.has("golden_absent"), "kinds: {:?}", run.kinds());
    assert!(!run.has("frame_match"));
    assert_no_scratch_leak(&h);

    // 2. Bless from a live capture, then re-run → frame_match.
    let cap = capture_cell_json(
        &h,
        &["/bin/sh", "-c", "printf 'MATCHME'; exec cat"],
        80,
        24,
        "MATCHME",
    );
    bless_cell(golden.path(), "frame", &cap);
    let run = run_gate(
        &h,
        d.path(),
        &hold_scenario("MATCHME"),
        Some(golden.path()),
        &[],
    );
    assert!(
        run.has("frame_match"),
        "expected frame_match, got {:?} stderr {}",
        run.kinds(),
        run.stderr
    );
    assert_eq!(run.exit, 0, "a full match is provisionally green");
    assert_no_scratch_leak(&h);

    // 3. A different frame (extra cell) still matches the wait sentinel but differs →
    //    frame_mismatch (cell authoritative).
    let run = run_gate(
        &h,
        d.path(),
        &hold_scenario("MATCHMEX"),
        Some(golden.path()),
        &[],
    );
    assert!(
        run.has("frame_mismatch"),
        "expected frame_mismatch, got {:?}",
        run.kinds()
    );
    let mm = run.find("frame_mismatch").unwrap();
    assert!(mm["changed_cells"].as_u64().unwrap_or(0) >= 1);
    assert_no_scratch_leak(&h);
}

#[test]
fn tampered_golden_is_untrusted() {
    let h = Harness::new();
    let d = tmp();
    let golden = tmp();
    let cap = capture_cell_json(
        &h,
        &["/bin/sh", "-c", "printf 'MATCHME'; exec cat"],
        80,
        24,
        "MATCHME",
    );
    bless_cell(golden.path(), "frame", &cap);
    // Corrupt the fingerprint sidecar's font stamp without re-blessing → stale trigger.
    let fp_path = golden.path().join("frame.fingerprint.json");
    let mut fp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&fp_path).unwrap()).unwrap();
    fp["raster_font_fingerprint"] = serde_json::json!("from-a-different-build");
    std::fs::write(&fp_path, serde_json::to_string_pretty(&fp).unwrap()).unwrap();

    let run = run_gate(
        &h,
        d.path(),
        &hold_scenario("MATCHME"),
        Some(golden.path()),
        &[],
    );
    assert!(
        run.has("golden_untrusted"),
        "a font-bumped sidecar refuses the compare, got {:?}",
        run.kinds()
    );
    assert!(!run.has("frame_match"));
    assert_no_scratch_leak(&h);
}

#[test]
fn dogfood_at_120x40_reaches_compare() {
    // The wider dogfood viewport also drives end-to-end to compare (golden_absent).
    let h = Harness::new();
    let d = tmp();
    let golden = tmp();
    let scn = r#"
name = "wide"
command = ["/bin/sh", "-c", "printf 'WIDEFRAME'; exec cat"]
[terminal]
rows = 40
cols = 120
[[steps]]
action = "wait_for_text"
text = "WIDEFRAME"
timeout_ms = 5000
[[steps]]
action = "settle"
[[steps]]
action = "expect_golden"
name = "wide"
tier = "cell"
"#;
    let run = run_gate(&h, d.path(), scn, Some(golden.path()), &[]);
    assert!(run.has("golden_absent"), "kinds: {:?}", run.kinds());
    let start = run.find("scenario_start").unwrap();
    assert_eq!(start["cols"], 120);
    assert_eq!(start["rows"], 40);
    assert_no_scratch_leak(&h);
}

// ══════════════════════════════════════════════════════════════════════════════
// L1 child-exit (mechanic) — short-circuit before compare + expect_exit
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn unexpected_child_exit_short_circuits_before_compare() {
    let h = Harness::new();
    let d = tmp();
    let golden = tmp();
    // The child prints then EXITS 42 with no expect_exit — the exit must short-circuit
    // before any visual compare (so a crash cannot false-pass a golden).
    let scn = r#"
name = "crash"
command = ["/bin/sh", "-c", "printf 'CRASH'; exit 42"]
[terminal]
rows = 24
cols = 80
[[steps]]
action = "wait_for_text"
text = "CRASH"
timeout_ms = 5000
[[steps]]
action = "expect_golden"
name = "frame"
tier = "cell"
"#;
    let run = run_gate(&h, d.path(), scn, Some(golden.path()), &[]);
    assert!(run.has("child_exit"), "kinds: {:?}", run.kinds());
    assert_eq!(
        run.find("child_exit").unwrap()["code"].as_i64(),
        Some(42),
        "child_exit carries the exit code"
    );
    // No visual compare happened.
    assert!(!run.has("frame_match"));
    assert!(!run.has("frame_mismatch"));
    // 082 installed the frozen exit map: an unexpected child_exit → child_error → exit 5
    // (distinct from a visual regression's exit 1). GATE-TEST-CHANGE.
    assert_eq!(run.exit, 5, "child_exit maps to child_error (exit 5)");
    assert_no_scratch_leak(&h);
}

#[test]
fn expect_exit_records_expected_child_exit() {
    let h = Harness::new();
    let d = tmp();
    // The child blocks on stdin; a typed newline releases the read and it exits 42. An
    // `expect_exit` marks that as INTENDED → expected_child_exit, not child_exit.
    let scn = r#"
name = "clean-exit"
command = ["/bin/sh", "-c", "printf 'READY'; read x; exit 42"]
[terminal]
rows = 24
cols = 80
[[steps]]
action = "wait_for_text"
text = "READY"
timeout_ms = 5000
[[steps]]
action = "type_text"
text = "\n"
[[steps]]
action = "expect_exit"
code = 42
timeout_ms = 5000
"#;
    let run = run_gate(&h, d.path(), scn, None, &[]);
    assert!(
        run.has("expected_child_exit"),
        "kinds: {:?} stderr {}",
        run.kinds(),
        run.stderr
    );
    assert_eq!(
        run.find("expected_child_exit").unwrap()["code"].as_i64(),
        Some(42)
    );
    // A typed quit key that triggers the exit must NOT be misreported as child_exit.
    assert!(
        !run.has("child_exit"),
        "intended exit misreported: {:?}",
        run.kinds()
    );
    assert_no_scratch_leak(&h);
}

#[test]
fn signal_killed_child_short_circuits_before_compare() {
    // adv BLOCKER: a SIGNAL death (SIGKILL / SIGSEGV = "exit 139") fired no pane.exited,
    // so the runner compared the crash frame. It must short-circuit like a normal exit.
    let h = Harness::new();
    let d = tmp();
    let golden = tmp();
    let scn = r#"
name = "sigkill"
command = ["/bin/sh", "-c", "printf 'CRASHSCREEN'; kill -9 $$"]
[terminal]
rows = 24
cols = 80
[[steps]]
action = "wait_for_text"
text = "CRASHSCREEN"
timeout_ms = 5000
[[steps]]
action = "expect_golden"
name = "frame"
tier = "cell"
"#;
    let run = run_gate(&h, d.path(), scn, Some(golden.path()), &[]);
    let ce = run.find("child_exit").unwrap_or_else(|| {
        panic!(
            "a signal-killed child must surface child_exit: {:?} stderr {}",
            run.kinds(),
            run.stderr
        )
    });
    // Signal death carries NO code — the daemon's `-1` sentinel maps to an omitted
    // `code` (the signal.rs contract; impl-review: never a fake `-1`).
    assert!(
        ce.get("code").is_none(),
        "signal-kill child_exit must omit the code (got {ce})"
    );
    // The crash frame was NEVER compared.
    assert!(!run.has("frame_match") && !run.has("golden_absent"));
    assert_no_scratch_leak(&h);
}

#[test]
fn child_exit_after_terminal_expect_golden_is_not_dropped() {
    // adv MAJOR 2: a child that paints, settles, then exits while `expect_golden` is the
    // LAST step used to drop the exit entirely (false-pass on a matching golden). The
    // final post-loop check must surface it.
    let h = Harness::new();
    let d = tmp();
    let golden = tmp();
    let scn = r#"
name = "paint-settle-exit"
command = ["/bin/sh", "-c", "printf 'DONE'; sleep 0.4; exit 7"]
[terminal]
rows = 24
cols = 80
[[steps]]
action = "expect_golden"
name = "frame"
tier = "cell"
quiet_ms = 250
timeout_ms = 5000
"#;
    let run = run_gate(&h, d.path(), scn, Some(golden.path()), &[]);
    let ce = run.find("child_exit").unwrap_or_else(|| {
        panic!(
            "a post-compare unexpected exit must surface: {:?} stderr {}",
            run.kinds(),
            run.stderr
        )
    });
    assert_eq!(ce["code"].as_i64(), Some(7));
    // 082 frozen rollup: this frame has no committed golden (missing_golden, a regression,
    // exit 1); the trailing crash is a child_error (exit 5, an operational error). The
    // worst-frame rollup keeps the REGRESSION visible (M2 no-masking) → exit 1, and the
    // child_exit signal is still surfaced (asserted above). GATE-TEST-CHANGE.
    assert_eq!(
        run.exit, 1,
        "a surfaced regression is never masked by the trailing crash"
    );
    assert_no_scratch_leak(&h);
}

#[test]
fn single_long_step_is_cut_by_the_scenario_deadline() {
    // adv MAJOR 3: the whole-scenario deadline must interrupt a single in-progress step,
    // not just fire between steps (the old 2-step test was false coverage).
    let h = Harness::new();
    let d = tmp();
    let scn = r#"
name = "single-step-deadline"
deadline_ms = 500
command = ["/bin/sh", "-c", "printf 'HI'; exec cat"]
[terminal]
rows = 24
cols = 80
[[steps]]
action = "wait"
ms = 8000
"#;
    let start = std::time::Instant::now();
    let run = run_gate(&h, d.path(), scn, None, &[]);
    let elapsed = start.elapsed();
    assert!(
        run.find("timeout")
            .map(|t| t["class"] == "scenario")
            .unwrap_or(false),
        "expected a scenario timeout mid-step, got {:?}",
        run.kinds()
    );
    // It cut the 8s step well before completion (generous CI bound).
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "deadline did not interrupt the long step (took {elapsed:?})"
    );
    assert_no_scratch_leak(&h);
}

#[test]
fn wrong_expect_exit_code_is_a_code_bearing_child_exit() {
    // adv MAJOR 4: a wrong expect_exit code used to emit `timeout{class:step}`, dropping
    // the observed code. It must be a code-bearing child_exit (the child DID exit).
    let h = Harness::new();
    let d = tmp();
    let scn = r#"
name = "wrong-code"
command = ["/bin/sh", "-c", "printf 'READY'; read x; exit 42"]
[terminal]
rows = 24
cols = 80
[[steps]]
action = "wait_for_text"
text = "READY"
timeout_ms = 5000
[[steps]]
action = "type_text"
text = "\n"
[[steps]]
action = "expect_exit"
code = 99
timeout_ms = 5000
"#;
    let run = run_gate(&h, d.path(), scn, None, &[]);
    let ce = run
        .find("child_exit")
        .unwrap_or_else(|| panic!("wrong-code exit must be a child_exit: {:?}", run.kinds()));
    assert_eq!(
        ce["code"].as_i64(),
        Some(42),
        "the OBSERVED code is carried"
    );
    assert!(!run.has("timeout"), "not a timeout: {:?}", run.kinds());
    assert_no_scratch_leak(&h);
}

// ══════════════════════════════════════════════════════════════════════════════
// L1 timeouts (mechanic) — the four distinct raw causes (design D8)
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn wait_for_text_timeout_is_step_timeout() {
    let h = Harness::new();
    let d = tmp();
    let scn = r#"
name = "steptimeout"
command = ["/bin/sh", "-c", "printf 'HELLO'; exec cat"]
[terminal]
rows = 24
cols = 80
[[steps]]
action = "wait_for_text"
text = "NEVER_APPEARS"
timeout_ms = 800
"#;
    let run = run_gate(&h, d.path(), scn, None, &[]);
    let t = run.find("timeout").expect("a timeout signal");
    assert_eq!(t["class"], "step");
    assert_eq!(t["action"], "wait_for_text");
    assert_no_scratch_leak(&h);
}

#[test]
fn never_quiet_pane_is_never_stabilized() {
    let h = Harness::new();
    let d = tmp();
    // `yes` floods output continuously → the pane never reaches a quiet window.
    let scn = r#"
name = "neversettle"
command = ["yes"]
[terminal]
rows = 24
cols = 80
[[steps]]
action = "settle"
quiet_ms = 200
timeout_ms = 1500
"#;
    let run = run_gate(&h, d.path(), scn, None, &[]);
    let t = run.find("timeout").expect("a timeout signal");
    assert_eq!(t["class"], "never_stabilized", "kinds: {:?}", run.kinds());
    assert_no_scratch_leak(&h);
}

#[test]
fn whole_scenario_deadline_is_scenario_timeout() {
    let h = Harness::new();
    let d = tmp();
    // A tiny whole-scenario deadline trips before the (long) wait step completes.
    let scn = r#"
name = "scntimeout"
deadline_ms = 1
command = ["/bin/sh", "-c", "printf 'HELLO'; exec cat"]
[terminal]
rows = 24
cols = 80
[[steps]]
action = "wait"
ms = 200
[[steps]]
action = "wait"
ms = 200
"#;
    let run = run_gate(&h, d.path(), scn, None, &[]);
    assert!(
        run.find("timeout")
            .map(|t| t["class"] == "scenario")
            .unwrap_or(false),
        "expected a scenario timeout, got {:?}",
        run.kinds()
    );
    assert_no_scratch_leak(&h);
}

// ══════════════════════════════════════════════════════════════════════════════
// L1 no-visual-check + L3 edge (resize)
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn scenario_without_expect_golden_is_no_visual_check() {
    // Design D6: text asserts are smoke, not visual proof.
    let h = Harness::new();
    let d = tmp();
    let scn = r#"
name = "novisual"
command = ["/bin/sh", "-c", "printf 'HI'; exec cat"]
[[steps]]
action = "wait_for_text"
text = "HI"
timeout_ms = 5000
[[steps]]
action = "assert_contains"
text = "HI"
"#;
    let run = run_gate(&h, d.path(), scn, None, &[]);
    assert!(run.has("no_visual_check"), "kinds: {:?}", run.kinds());
    assert_no_scratch_leak(&h);
}

#[test]
fn resize_step_drives_a_winsize_aware_child() {
    // Edge: the resize step reaches the child (SIGWINCH-aware fixture reprints its size).
    let h = Harness::new();
    let d = tmp();
    let f7 = h.repo_root().join(".shux/fixtures/lens/f7_winsize.sh");
    let scn = format!(
        r#"
name = "resize"
command = ["/bin/sh", {:?}]
[terminal]
rows = 24
cols = 80
[[steps]]
action = "wait_for_text"
text = "SIZE=24 80"
timeout_ms = 5000
[[steps]]
action = "resize"
rows = 30
cols = 100
[[steps]]
action = "wait_for_text"
text = "SIZE=30 100"
timeout_ms = 5000
"#,
        f7.to_string_lossy()
    );
    let run = run_gate(&h, d.path(), &scn, None, &[]);
    // Both waits matched (no step_timeout) → the resize took effect in the child.
    assert!(
        !run.has("timeout") && !run.has("child_exit"),
        "resize drive failed: {:?} stderr {}",
        run.kinds(),
        run.stderr
    );
    assert_no_scratch_leak(&h);
}

// ══════════════════════════════════════════════════════════════════════════════
// L2 quota — the 17th concurrent scratch (constant 16) is refused (design D10)
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn seventeenth_scratch_is_quota_exceeded() {
    let h = Harness::new();
    let d = tmp();
    // Fill the quota (16) with held scratch sessions.
    let mut held = Vec::new();
    for _ in 0..16 {
        let r = h.rpc_ok(
            "lens.run",
            serde_json::json!({ "argv": ["/bin/sh", "-c", "exec cat"], "cols": 80, "rows": 24 }),
        );
        held.push(r["session_id"].as_str().unwrap().to_string());
    }
    // The gate's own scratch would be the 17th → a raw quota_exceeded signal, and the
    // gate creates NO scratch of its own (the reservation is refused).
    let scn = r#"
name = "quota"
command = ["/bin/sh", "-c", "printf 'HI'; exec cat"]
[[steps]]
action = "wait_for_text"
text = "HI"
timeout_ms = 5000
[[steps]]
action = "expect_golden"
name = "frame"
tier = "cell"
"#;
    let run = run_gate(&h, d.path(), scn, None, &[]);
    let q = run.find("quota_exceeded").expect("quota_exceeded");
    assert_eq!(q["limit"].as_u64(), Some(16));

    // Cleanup the 16 held sessions; the gate itself left nothing behind.
    for s in &held {
        h.kill_session(s);
    }
    assert!(
        lens_common::wait_until(std::time::Duration::from_secs(10), || {
            let list = h.rpc_ok(
                "session.list",
                serde_json::json!({ "include_scratch": true }),
            );
            list["sessions"]
                .as_array()
                .map(|a| a.is_empty())
                .unwrap_or(false)
        }),
        "held scratch sessions never reaped"
    );
}

/// A scenario referenced by a BARE FILENAME must behave exactly like `./name` and like an
/// absolute path. `Path::parent()` returns `Some("")` for a bare name, not `None`, so an
/// `unwrap_or(".")` silently yields the empty path — which `canonicalize()` rejects as
/// ENOENT the moment a scenario sets `cwd`. That broke the most natural invocation of all
/// (`cd` into the scenario's directory and run it) while `./scenario.toml` still worked.
/// Every other test here uses tempdir or absolute paths, so nothing covered this shape.
#[test]
fn a_cwd_scenario_runs_by_bare_filename_and_by_dot_slash() {
    let h = Harness::new();
    let dir = tmp();
    // The child must actually depend on `cwd` resolving correctly, so it reads a file that
    // exists ONLY in the scenario directory.
    std::fs::write(dir.path().join("marker.txt"), "cwd-resolved\n").unwrap();
    std::fs::write(
        dir.path().join("scn.toml"),
        r#"
name = "bare-cwd"
command = ["/bin/sh", "-c", "cat marker.txt; exec cat"]
cwd = "."

[[steps]]
action = "wait_for_text"
text = "cwd-resolved"
timeout_ms = 5000

[[steps]]
action = "assert_contains"
text = "cwd-resolved"
"#,
    )
    .unwrap();

    for form in ["scn.toml", "./scn.toml"] {
        let out = h
            .shux()
            .current_dir(dir.path())
            .args(["lens", "gate", form])
            .output()
            .expect("spawn shux");
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let exit = out.status.code().unwrap_or(-1);
        // `no_visual_check` (exit 2) is the expected verdict — the scenario compares no
        // frames. What must NOT happen is an infra failure resolving the scenario dir.
        assert!(
            !stderr.contains("cannot resolve the scenario directory"),
            "`{form}` failed to resolve its own directory: {stderr}"
        );
        assert_ne!(
            exit, 3,
            "`{form}` produced an infra error, so `cwd` did not resolve: {stderr}"
        );
    }
    assert_no_scratch_leak(&h);
}

/// The 084 F4 blocker guard, at the DRIVER boundary. The unit test passes
/// `scenario_floor(&outcome)` itself, so by construction it cannot catch the driver
/// handing `apply_blessed` the wrong floor — mutating both call sites back to
/// `GateStatus::Pass` leaves every unit and contract test green while the real binary
/// reports `pass`/exit 0 over a scenario that timed out and blessed nothing.
#[test]
fn blessing_cannot_launder_a_step_timeout_through_the_driver() {
    let h = Harness::new();
    let dir = tmp();
    let golden = dir.path().join("goldens");
    std::fs::create_dir_all(&golden).unwrap();
    let scn = dir.path().join("timeout.toml");
    std::fs::write(
        &scn,
        r#"
name = "driver-floor"
command = ["/bin/sh", "-c", "printf 'alpha\n'; exec cat"]

[[steps]]
action = "wait_for_text"
text = "THIS-NEVER-APPEARS"
timeout_ms = 1500

[[steps]]
action = "expect_golden"
name = "start"
tier = "cell"
"#,
    )
    .unwrap();

    let scn_s = scn.to_string_lossy().into_owned();
    let golden_s = golden.to_string_lossy().into_owned();
    let out = h.cli(&[
        "lens",
        "gate",
        &scn_s,
        "--golden-dir",
        &golden_s,
        "--on-missing",
        "create",
        "--reason",
        "driver floor probe",
    ]);
    let exit = out.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(
        exit, 1,
        "a step_timeout must stay a regression through the bless path, got exit {exit}: {stderr}"
    );
    assert!(
        !stderr.contains("verdict=pass"),
        "blessing laundered a step_timeout into pass: {stderr}"
    );
    assert_no_scratch_leak(&h);
}
