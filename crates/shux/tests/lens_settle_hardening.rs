//! Task 083 — `pane.wait_settled` frame-stability hardening (`hold_ms` / `stable_frames`).
//! LENS lane (`LENS-TEST-CHANGE:` to touch). `test = false` — run serially under the leak guard
//! via `make test-lens-settle-hardening`, NEVER in the default parallel `cargo test`/CI run.
//!
//! These drive the REAL daemon through `pane.wait_settled` on real token-paced fixtures. The
//! pure decision core (contiguous-revision reset, silence-tolerant hold, A→B→A alias defense) is
//! unit-tested in `shux-vt/src/settle.rs`; here we prove the daemon WIRES it correctly against
//! real panes, real revisions, and real masked-frame hashing. Council decisions:
//! `.local/dootsabha-083-design.json`.
//!
//! Timing note: the ONLY sleeps are the token-pump intervals (they PACE INPUT, never synchronise
//! on output) — the same §16.1 exception the frozen `lens_settle.rs` S3/S5 tests use.

mod lens_common;
use lens_common::*;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Run `check` while a background thread pumps one stdin token every `interval_ms` into the pane
/// (mirrors `lens_settle.rs::s3_check_under_pump`). The pump's lifetime is bound to the check and
/// deadline-capped so a panic cannot leave it spinning.
fn under_pump<T>(h: &Harness, pane_id: &str, interval_ms: u64, check: impl FnOnce() -> T) -> T {
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let pump = scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_millis(15_000);
            while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
                h.send_line_token(pane_id, "");
                std::thread::sleep(Duration::from_millis(interval_ms));
            }
        });
        let out = check();
        stop.store(true, Ordering::Relaxed);
        pump.join().expect("pump");
        out
    })
}

fn settled(env: &RpcEnvelope, ctx: &str) -> bool {
    env.expect_result(ctx)["settled"]
        .as_bool()
        .unwrap_or_else(|| panic!("{ctx}: missing settled bool"))
}

// ── stable_frames ────────────────────────────────────────────────────────────

/// A fast IDENTICAL repainter (f11) settles under `--stable-frames` where quiet-mode times out:
/// the pane never goes quiet while tokens flow, but the frame CONTENT holds (council #2/#3).
#[test]
fn stable_frames_settles_identical_repainter() {
    let h = Harness::new();
    let f = h.launch_fixture("f11_heartbeat.sh", 80, 24, "LENS-F11-HEARTBEAT");

    // stable_frames=3 settles: 3 contiguous identical revisions accumulate as tokens flow.
    let ok = under_pump(&h, &f.pane_id, 60, || {
        let env = h.rpc_raw(
            "pane.wait_settled",
            serde_json::json!({
                "pane_id": f.pane_id, "quiet_ms": 300, "timeout_ms": 8_000, "stable_frames": 3
            }),
        );
        settled(&env, "F11 stable_frames")
    });
    assert!(ok, "identical repainter must settle under stable_frames=3");

    // Quiet-mode (no stability criterion) under the SAME pump must NOT settle — proves
    // stable_frames enables what quiet cannot.
    let quiet_ok = under_pump(&h, &f.pane_id, 60, || {
        let env = h.rpc_raw(
            "pane.wait_settled",
            serde_json::json!({ "pane_id": f.pane_id, "quiet_ms": 300, "timeout_ms": 2_000 }),
        );
        settled(&env, "F11 quiet-mode")
    });
    assert!(
        !quiet_ok,
        "quiet-mode must TIME OUT on a never-quiet repainter"
    );

    h.kill_session(&f.session_id);
}

/// A perpetual animation (f3_flip alternates two frames) never gets K identical contiguous
/// revisions → `settled:false` → the runner maps this to `settle_never_stable` (a FAILURE).
#[test]
fn stable_frames_never_settles_perpetual_animation() {
    let h = Harness::new();
    let f = h.launch_fixture("f3_flip.sh", 80, 24, "AAAAAAAAAA");

    let ok = under_pump(&h, &f.pane_id, 60, || {
        let env = h.rpc_raw(
            "pane.wait_settled",
            serde_json::json!({
                "pane_id": f.pane_id, "quiet_ms": 300, "timeout_ms": 2_000, "stable_frames": 3
            }),
        );
        settled(&env, "F3 flip stable_frames")
    });
    assert!(
        !ok,
        "a genuine A/B animation must never settle under stable_frames"
    );

    h.kill_session(&f.session_id);
}

// ── hold_ms ──────────────────────────────────────────────────────────────────

/// `--hold-ms` settles an identical repainter (frame held) but NOT a changing pane (f8 repaints a
/// different glyph each token → the hold clock resets on every change; the slow-spinner fix).
#[test]
fn hold_ms_settles_identical_but_not_changing() {
    let h = Harness::new();

    // Identical repainter: the frame hash holds, so hold_ms=400 settles despite constant tokens.
    let f11 = h.launch_fixture("f11_heartbeat.sh", 80, 24, "LENS-F11-HEARTBEAT");
    let held = under_pump(&h, &f11.pane_id, 50, || {
        let env = h.rpc_raw(
            "pane.wait_settled",
            serde_json::json!({
                "pane_id": f11.pane_id, "quiet_ms": 300, "timeout_ms": 8_000, "hold_ms": 400
            }),
        );
        settled(&env, "F11 hold_ms")
    });
    assert!(
        held,
        "hold_ms must settle an identical repainter (frame held)"
    );
    h.kill_session(&f11.session_id);

    // Changing pane: f8 rewrites row 10 with a new glyph each token → hold clock resets → the
    // 400ms window (tokens every 80ms) never elapses without a content change → no settle.
    let f8 = h.launch_fixture("f8_repaint.sh", 80, 24, "LENS-F8-REPAINT");
    let changing = under_pump(&h, &f8.pane_id, 80, || {
        let env = h.rpc_raw(
            "pane.wait_settled",
            serde_json::json!({
                "pane_id": f8.pane_id, "quiet_ms": 300, "timeout_ms": 2_000, "hold_ms": 400
            }),
        );
        settled(&env, "F8 hold_ms")
    });
    assert!(
        !changing,
        "hold_ms must NOT settle a pane whose frame changes faster than the hold window"
    );
    h.kill_session(&f8.session_id);
}

// ── masks scope the stability hash (council #4) ──────────────────────────────

/// f8 churns ONLY row 10 and keeps the rest static. Masking that row makes the masked-frame hash
/// constant → `--stable-frames` settles. Without the mask the churn prevents settling. This is
/// the council's "masked dynamic region settles; unmasked dynamic region fails" test.
#[test]
fn masked_churn_settles_unmasked_does_not() {
    let h = Harness::new();
    let f = h.launch_fixture("f8_repaint.sh", 80, 24, "LENS-F8-REPAINT");

    // Mask the churning row (0-based row 10, whole 80-col span) → stable → settles.
    let masked = under_pump(&h, &f.pane_id, 60, || {
        let env = h.rpc_raw(
            "pane.wait_settled",
            serde_json::json!({
                "pane_id": f.pane_id, "quiet_ms": 300, "timeout_ms": 8_000, "stable_frames": 3,
                "masks": [{ "row": 10, "col": 0, "width": 80 }]
            }),
        );
        settled(&env, "F8 masked stable_frames")
    });
    assert!(
        masked,
        "masking the churning region must let stable_frames settle"
    );

    // Unmasked: the row-10 churn keeps changing the frame → never K identical → no settle.
    let unmasked = under_pump(&h, &f.pane_id, 60, || {
        let env = h.rpc_raw(
            "pane.wait_settled",
            serde_json::json!({
                "pane_id": f.pane_id, "quiet_ms": 300, "timeout_ms": 2_000, "stable_frames": 3
            }),
        );
        settled(&env, "F8 unmasked stable_frames")
    });
    assert!(
        !unmasked,
        "an unmasked churning region must prevent stable_frames from settling"
    );

    h.kill_session(&f.session_id);
}

// ── param validation + backward compat ───────────────────────────────────────

/// `hold_ms`/`stable_frames` out of range → INVALID_PARAMS (-32602) on the RPC and exit 2 on the
/// CLI twin (§10 exit table).
#[test]
fn stability_param_validation() {
    let h = Harness::new();
    let f = h.launch_fixture("f1_static.sh", 80, 24, "दृश्यते");

    // hold_ms below the [10, 60_000] window (and non-zero) → INVALID_PARAMS.
    h.rpc_raw(
        "pane.wait_settled",
        serde_json::json!({ "pane_id": f.pane_id, "quiet_ms": 300, "timeout_ms": 10_000, "hold_ms": 5 }),
    )
    .expect_error_code(-32602, "hold_ms below min");

    // hold_ms greater than timeout_ms can never succeed → INVALID_PARAMS.
    h.rpc_raw(
        "pane.wait_settled",
        serde_json::json!({ "pane_id": f.pane_id, "quiet_ms": 300, "timeout_ms": 1_000, "hold_ms": 2_000 }),
    )
    .expect_error_code(-32602, "hold_ms exceeds timeout");

    // stable_frames = 0 is out of range [1, 1000] → INVALID_PARAMS.
    h.rpc_raw(
        "pane.wait_settled",
        serde_json::json!({ "pane_id": f.pane_id, "quiet_ms": 300, "timeout_ms": 10_000, "stable_frames": 0 }),
    )
    .expect_error_code(-32602, "stable_frames zero");

    // CLI twin: out-of-range hold → exit 2, AND the error is ACTIONABLE — it surfaces the range
    // detail, not a bare "invalid_params" (dogfood 083: a first-timer must be told the range).
    let out = h.cli(&[
        "pane",
        "wait-settled",
        &f.pane_id,
        "--quiet",
        "300ms",
        "--timeout",
        "10s",
        "--hold-ms",
        "5ms",
    ]);
    assert_eq!(out.status.code(), Some(2), "CLI exit 2 on hold below min");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("hold_ms") && stderr.contains("out of range"),
        "the CLI must surface the actionable range detail, got: {stderr}"
    );

    h.kill_session(&f.session_id);
}

/// Default params (no `hold_ms`/`stable_frames`) leave quiet-mode UNCHANGED: a static pane still
/// settles immediately, and the CLI without the new flags behaves exactly as before (backward
/// compatibility — the non-goal).
#[test]
fn default_quiet_mode_unchanged() {
    let h = Harness::new();
    let f = h.launch_fixture("f1_static.sh", 80, 24, "दृश्यते");

    let env = h.rpc_raw(
        "pane.wait_settled",
        serde_json::json!({ "pane_id": f.pane_id, "quiet_ms": 300, "timeout_ms": 5_000 }),
    );
    assert!(
        settled(&env, "default quiet"),
        "static pane settles in default quiet mode"
    );

    // The new flags default to off on the CLI too — exit 0 for a quiet static pane.
    let out = h.cli(&[
        "pane",
        "wait-settled",
        &f.pane_id,
        "--quiet",
        "300ms",
        "--timeout",
        "5s",
    ]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "default CLI still settles a static pane"
    );

    h.kill_session(&f.session_id);
}

/// council-mandated + adv-083 Agent C: `stable_frames` requires K NEW contiguous identical
/// revisions, so a pane that reaches a STATIC steady state (paints once, then goes idle) never
/// reaches K and times out as `settled:false` (→ `settle_never_stable`) — WHILE `hold_ms` settles
/// the SAME pane because silence counts as held. This PINS the intentional mode split: a quiet
/// fallback for `stable_frames` was rejected by the design council (it would reintroduce the
/// slow-spinner false-settle), and the CLI/scenario docs steer a steady-state TUI to `hold_ms`.
#[test]
fn stable_frames_times_out_on_an_idle_pane_but_hold_ms_settles_it() {
    let h = Harness::new();
    let f = h.launch_fixture("f1_static.sh", 80, 24, "दृश्यते");

    // A pane that paints once then goes idle never yields K new revisions → stable_frames can't
    // settle (this is the accepted trade-off, not a bug — use hold_ms for a steady-state TUI).
    let env = h.rpc_raw(
        "pane.wait_settled",
        serde_json::json!({
            "pane_id": f.pane_id, "quiet_ms": 300, "timeout_ms": 1_500, "stable_frames": 3
        }),
    );
    assert!(
        !settled(&env, "F1 idle stable_frames"),
        "stable_frames must NOT settle a static idle pane (council-accepted; use hold_ms)"
    );

    // hold_ms settles the SAME idle pane — silence counts as held.
    let env = h.rpc_raw(
        "pane.wait_settled",
        serde_json::json!({
            "pane_id": f.pane_id, "quiet_ms": 300, "timeout_ms": 3_000, "hold_ms": 300
        }),
    );
    assert!(
        settled(&env, "F1 idle hold_ms"),
        "hold_ms MUST settle a static idle pane (silence counts as held)"
    );

    h.kill_session(&f.session_id);
}
