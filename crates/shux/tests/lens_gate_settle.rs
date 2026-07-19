//! Task 083 — settle hardening + cast at the GATE level (`shux lens gate`). GATE lane
//! (`GATE-TEST-CHANGE:` to touch). `test = false` — run serially under the leak guard via
//! `make test-lens-gate-settle`, NEVER in the default parallel `cargo test`/CI run.
//!
//! Proves the SCENARIO wiring: a `stable_frames`/`hold_settle` step (and `expect_golden`'s own
//! stability fields) drive the daemon settle modes; a genuine animation → `settle_never_stable`
//! (exit 1); the `retries` budget re-settles/re-captures and never masks a persistent regression
//! (the anti-masking audit surfaces); `--cast` writes a valid asciinema v2 with an honest resize
//! event, outside the golden dir. The settle SEMANTICS themselves are unit- + RPC-tested
//! (`shux-vt/src/settle.rs`, `lens_settle_hardening.rs`); this is the end-to-end proof.

mod lens_common;

use std::path::Path;

use lens_common::Harness;
use shux_vt::{
    FINGERPRINT_SCHEMA, Fingerprint, FrameEnvelope, MaskSet, RENDERER_FORMAT_VERSION,
    SCHEMA_VERSION, Tier, TolParams, capture_sha256, mask_hash, unicode_width_version,
};

struct GateRun {
    signals: Vec<serde_json::Value>,
    exit: i32,
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
}

/// Absolute path to a `.shux/fixtures/lens-gate/settle/` fixture, anchored at THIS harness's repo
/// root so it resolves from the sandbox cwd.
fn settle_fixture(h: &Harness, name: &str) -> String {
    h.repo_root()
        .join(".shux/fixtures/lens-gate/settle")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

/// Write a scenario + run `shux lens gate --trace`, parsing the trace. Optional `argv` override.
fn run_gate(
    h: &Harness,
    dir: &Path,
    scenario_toml: &str,
    golden_dir: &Path,
    argv: &[&str],
) -> GateRun {
    let scn = dir.join(format!("scn-{}.toml", lens_common::unique()));
    std::fs::write(&scn, scenario_toml).unwrap();
    let trace = dir.join(format!("trace-{}.ndjson", lens_common::unique()));
    let (scn_s, trace_s, gd_s) = (
        scn.to_string_lossy().into_owned(),
        trace.to_string_lossy().into_owned(),
        golden_dir.to_string_lossy().into_owned(),
    );
    let mut args: Vec<&str> = vec![
        "lens",
        "gate",
        &scn_s,
        "--golden-dir",
        &gd_s,
        "--trace",
        &trace_s,
    ];
    if !argv.is_empty() {
        args.push("--");
        args.extend_from_slice(argv);
    }
    let out = h.cli(&args);
    let text = std::fs::read_to_string(&trace).unwrap_or_default();
    let signals = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("trace line is JSON"))
        .collect();
    GateRun {
        signals,
        exit: out.status.code().unwrap_or(-1),
    }
}

/// Capture a settled cell golden from the self-animating `repaint.sh <text>` — settles via
/// `stable_frames` (the repainter never quiets), then glances the stable frame.
fn capture_repaint_golden(h: &Harness, text: &str) -> String {
    let fixture = settle_fixture(h, "repaint.sh");
    let run = h.rpc_ok(
        "lens.run",
        serde_json::json!({ "argv": ["sh", &fixture, text], "cols": 80, "rows": 24 }),
    );
    let pane = run["pane_id"].as_str().unwrap().to_string();
    let sid = run["session_id"].as_str().unwrap().to_string();
    h.wait_for(&pane, "GATE-REPAINT", 5000)
        .expect("repaint sentinel drawn");
    let s = h.rpc_raw(
        "pane.wait_settled",
        serde_json::json!({ "pane_id": pane, "quiet_ms": 300, "timeout_ms": 5000, "stable_frames": 3 }),
    );
    assert_eq!(
        s.expect_result("golden settle")["settled"],
        serde_json::Value::Bool(true),
        "repaint must settle under stable_frames for the golden capture"
    );
    let g = h.rpc_ok(
        "pane.glance",
        serde_json::json!({ "pane_id": pane, "include_cells": true, "include_png": false }),
    );
    let cells = g["cells"].clone();
    h.kill_session(&sid);
    serde_json::to_string_pretty(&cells).unwrap()
}

/// Bless a cell golden matching the runner's `current_fp` non-stale fields.
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

fn assert_no_scratch_leak(h: &Harness) {
    let list = h.rpc_ok(
        "session.list",
        serde_json::json!({ "include_scratch": true }),
    );
    let n = list["sessions"].as_array().map(|a| a.len()).unwrap_or(0);
    assert_eq!(n, 0, "gate left {n} scratch session(s) behind");
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

// ── stable_frames ────────────────────────────────────────────────────────────

/// `expect_golden` with `stable_frames` settles a self-animating IDENTICAL repainter that
/// quiet-mode could never settle, captures the stable frame, and matches its golden → exit 0.
#[test]
fn stable_frames_settles_repainter_and_passes() {
    let h = Harness::new();
    let d = tmp();
    let golden = tmp();
    let cap = capture_repaint_golden(&h, "HEARTBEAT");
    bless_cell(golden.path(), "hb", &cap);

    let fixture = settle_fixture(&h, "repaint.sh");
    let scn = format!(
        r#"name = "gate-stable"
command = ["sh", "{fixture}", "HEARTBEAT"]
[terminal]
rows = 24
cols = 80
[[steps]]
action = "wait_for_text"
text = "GATE-REPAINT"
timeout_ms = 5000
[[steps]]
action = "expect_golden"
name = "hb"
stable_frames = 3
timeout_ms = 5000
"#
    );
    let run = run_gate(&h, d.path(), &scn, golden.path(), &[]);
    assert!(
        run.has("frame_match"),
        "must settle + match: {:?}",
        run.kinds()
    );
    assert_eq!(
        run.exit, 0,
        "a stable repainter matching its golden is exit 0"
    );
    assert_no_scratch_leak(&h);
}

/// A genuine animation (spinner) never reaches `stable_frames` contiguous identical frames within
/// the settle budget → `settle_never_stable` (exit 1, a FAILURE — never infra).
#[test]
fn stable_frames_animation_is_never_stable() {
    let h = Harness::new();
    let d = tmp();
    let golden = tmp();
    let fixture = settle_fixture(&h, "spinner.sh");
    let scn = format!(
        r#"name = "gate-neverstable"
command = ["sh", "{fixture}"]
[terminal]
rows = 24
cols = 80
[[steps]]
action = "wait_for_text"
text = "GATE-SPINNER"
timeout_ms = 5000
[[steps]]
action = "expect_golden"
name = "sp"
stable_frames = 3
timeout_ms = 1500
"#
    );
    let run = run_gate(&h, d.path(), &scn, golden.path(), &[]);
    // A never-settling frame stops before any compare — the runner emits a settle timeout.
    assert!(
        run.has("timeout"),
        "a perpetual animation must time out settling: {:?}",
        run.kinds()
    );
    assert!(
        !run.has("frame_match"),
        "it must never capture/compare a frame"
    );
    assert_eq!(run.exit, 1, "settle_never_stable is a regression → exit 1");
    assert_no_scratch_leak(&h);
}

/// `expect_golden` with `hold_ms` settles the repainter (frame held) and passes — proving the
/// hold-mode wiring end-to-end.
#[test]
fn hold_settle_settles_repainter_and_passes() {
    let h = Harness::new();
    let d = tmp();
    let golden = tmp();
    let cap = capture_repaint_golden(&h, "HEARTBEAT");
    bless_cell(golden.path(), "hb", &cap);

    let fixture = settle_fixture(&h, "repaint.sh");
    let scn = format!(
        r#"name = "gate-hold"
command = ["sh", "{fixture}", "HEARTBEAT"]
[terminal]
rows = 24
cols = 80
[[steps]]
action = "wait_for_text"
text = "GATE-REPAINT"
timeout_ms = 5000
[[steps]]
action = "expect_golden"
name = "hb"
hold_ms = 300
timeout_ms = 5000
"#
    );
    let run = run_gate(&h, d.path(), &scn, golden.path(), &[]);
    assert!(
        run.has("frame_match"),
        "hold_ms must settle + match: {:?}",
        run.kinds()
    );
    assert_eq!(run.exit, 0);
    assert_no_scratch_leak(&h);
}

// ── retries (anti-masking) ───────────────────────────────────────────────────

/// A persistent regression (a stable-but-WRONG repaint) still FAILS after the retry budget, with
/// the anti-masking audit ("exhausted") surfaced — a retry never masks a real regression.
#[test]
fn retry_persistent_regression_fails_after_retries() {
    let h = Harness::new();
    let d = tmp();
    let golden = tmp();
    // Golden = REPAINT:HEARTBEAT; the run renders REPAINT:WRONGWRONG (stable but different).
    let cap = capture_repaint_golden(&h, "HEARTBEAT");
    bless_cell(golden.path(), "hb", &cap);

    let fixture = settle_fixture(&h, "repaint.sh");
    let scn = format!(
        r#"name = "gate-retry"
command = ["sh", "{fixture}", "HEARTBEAT"]
[terminal]
rows = 24
cols = 80
[[steps]]
action = "wait_for_text"
text = "GATE-REPAINT"
timeout_ms = 5000
[[steps]]
action = "expect_golden"
name = "hb"
stable_frames = 3
retries = 2
timeout_ms = 5000
"#
    );
    let run = run_gate(
        &h,
        d.path(),
        &scn,
        golden.path(),
        &["sh", &fixture, "WRONGWRONG"],
    );
    assert!(
        run.has("frame_mismatch"),
        "the wrong frame must mismatch: {:?}",
        run.kinds()
    );
    let ro = run
        .find("retry_outcome")
        .expect("a retry audit must be emitted");
    assert_eq!(
        ro["outcome"], "exhausted",
        "a persistent regression exhausts the budget"
    );
    assert_eq!(run.exit, 1, "a regression that survives retries is exit 1");
    assert_no_scratch_leak(&h);
}

// ── cast ─────────────────────────────────────────────────────────────────────

/// `--cast` writes a valid asciinema v2 with output events AND an honest resize event, OUTSIDE
/// the golden dir. (The verdict is irrelevant — the cast is armed at spawn, independent of it.)
#[test]
fn cast_produces_valid_asciinema_v2_with_resize() {
    let h = Harness::new();
    let d = tmp();
    let golden = tmp();
    let out = tmp();
    let fixture = settle_fixture(&h, "repaint.sh");
    let scn = format!(
        r#"name = "gate-cast"
command = ["sh", "{fixture}", "HEARTBEAT"]
[terminal]
rows = 24
cols = 80
[[steps]]
action = "wait_for_text"
text = "GATE-REPAINT"
timeout_ms = 5000
[[steps]]
action = "resize"
rows = 30
cols = 100
[[steps]]
action = "wait"
ms = 200
[[steps]]
action = "assert_contains"
text = "GATE-REPAINT"
"#
    );
    let scn_path = d.path().join("cast.toml");
    std::fs::write(&scn_path, &scn).unwrap();
    let cast_path = out.path().join("run.cast");
    let (scn_s, gd_s, out_s, cast_s) = (
        scn_path.to_string_lossy().into_owned(),
        golden.path().to_string_lossy().into_owned(),
        out.path().to_string_lossy().into_owned(),
        cast_path.to_string_lossy().into_owned(),
    );
    let _ = h.cli(&[
        "lens",
        "gate",
        &scn_s,
        "--golden-dir",
        &gd_s,
        "--out",
        &out_s,
        "--cast",
        &cast_s,
    ]);

    let cast = std::fs::read_to_string(&cast_path).expect("cast file written");
    let mut lines = cast.lines();

    // Line 1: asciinema v2 header with geometry.
    let header: serde_json::Value =
        serde_json::from_str(lines.next().expect("header line")).expect("header is JSON");
    assert_eq!(header["version"], 2, "asciinema v2 header");
    assert_eq!(header["width"], 80, "header geometry = spawn cols");
    assert_eq!(header["height"], 24, "header geometry = spawn rows");

    // Every event is a `[t, code, data]` array with a non-decreasing t.
    let mut saw_output = false;
    let mut saw_resize = false;
    let mut last_t = -1.0_f64;
    for l in lines {
        if l.trim().is_empty() {
            continue;
        }
        let ev: serde_json::Value = serde_json::from_str(l).expect("event line is a JSON array");
        let t = ev[0].as_f64().expect("event t is a number");
        assert!(
            t >= last_t,
            "cast timestamps must be non-decreasing ({t} < {last_t})"
        );
        last_t = t;
        match ev[1].as_str() {
            Some("o") => saw_output = true,
            Some("r") => {
                saw_resize = true;
                assert_eq!(ev[2], "100x30", "the resize event carries the new geometry");
            }
            other => panic!("unexpected cast event code {other:?}"),
        }
    }
    assert!(saw_output, "cast must contain output events");
    assert!(
        saw_resize,
        "cast must contain the resize event (grok's honesty fix)"
    );

    // Ephemeral: the cast is under the out dir, never the golden dir.
    assert!(
        !golden.path().join("run.cast").exists(),
        "cast must not land in the golden dir"
    );
    assert!(cast_path.starts_with(out.path()), "cast lives under --out");

    assert_no_scratch_leak(&h);
}

/// A `--cast` target pointed INSIDE `--golden-dir` is refused up front (exit 2) so a cast can
/// never pollute the golden tree (adv-083 Agent C MINOR).
#[test]
fn cast_into_the_golden_dir_is_refused() {
    let h = Harness::new();
    let d = tmp();
    let golden = tmp();
    std::fs::create_dir_all(golden.path()).unwrap();
    let fixture = settle_fixture(&h, "repaint.sh");
    let scn = format!(
        r#"name = "gate-cast-guard"
command = ["sh", "{fixture}", "HEARTBEAT"]
[terminal]
rows = 24
cols = 80
[[steps]]
action = "expect_golden"
name = "hb"
stable_frames = 3
timeout_ms = 5000
"#
    );
    let scn_path = d.path().join("guard.toml");
    std::fs::write(&scn_path, &scn).unwrap();
    let sneaky = golden.path().join("sneaky.cast");
    let (scn_s, gd_s, cast_s) = (
        scn_path.to_string_lossy().into_owned(),
        golden.path().to_string_lossy().into_owned(),
        sneaky.to_string_lossy().into_owned(),
    );
    let out = h.cli(&[
        "lens",
        "gate",
        &scn_s,
        "--golden-dir",
        &gd_s,
        "--cast",
        &cast_s,
    ]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a cast inside the golden dir is a usage error (exit 2)"
    );
    assert!(
        !sneaky.exists(),
        "the refused cast must not have been written into the golden dir"
    );
    assert_no_scratch_leak(&h);
}
