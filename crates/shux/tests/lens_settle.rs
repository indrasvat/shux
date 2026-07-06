//! Red suite — `pane.wait_settled` (§6 SPEC-C; tests S1–S5, V1 from §12).
//!
//! FROZEN after P0 (§16.2). In Phase P0 `pane.wait_settled` is unregistered, so
//! every test fails at its first settle call with `method_not_found (-32601)`.
//!
//! Timing note: the ONLY sleeps permitted anywhere in the lens work are the
//! S3/S5 harness token-pump intervals below (§16.1 exception) — they pace
//! INPUT, never synchronise on output.

mod lens_common;
use lens_common::*;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

fn glance_png(h: &Harness, pane: &str) -> Vec<u8> {
    let env = h.rpc_raw("pane.glance", serde_json::json!({ "pane_id": pane }));
    let g = env.expect_result("settle: glance");
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(g["png_base64"].as_str().expect("glance png"))
        .expect("decode png")
}

/// F2: pump 20 spinner tokens, request READY, wait for settle, glance.
/// Returns the settle result value.
fn settle_ready_body(h: &Harness) -> String {
    let f = h.launch_fixture("f2_spinner.sh", 80, 24, "LENS-F2-SPIN");
    for _ in 0..20 {
        h.send_line_token(&f.pane_id, "");
    }
    h.send_line_token(&f.pane_id, "R");
    h.wait_for(&f.pane_id, "READY", 5_000).expect("READY drawn");

    let env = h.rpc_raw(
        "pane.wait_settled",
        serde_json::json!({ "pane_id": f.pane_id, "quiet_ms": 300, "timeout_ms": 10_000 }),
    );
    let settled = env.expect_result("S1 wait_settled rpc");
    assert_eq!(
        settled["settled"],
        serde_json::Value::Bool(true),
        "S1: must settle"
    );

    // CLI twin exits 0 when settled.
    let out = h.cli(&[
        "pane",
        "wait-settled",
        &f.pane_id,
        "--quiet",
        "300ms",
        "--timeout",
        "10s",
    ]);
    assert_eq!(out.status.code(), Some(0), "S1: CLI exit 0 when settled");

    let png = glance_png(h, &f.pane_id);
    assert_png_golden(h, &png, "s1_ready.png");
    let pane = f.pane_id.clone();
    h.kill_session(&f.session_id);
    pane
}

// S1 ⇄ — settle happy path.
#[test]
fn s1_settle_happy_path() {
    let h = Harness::new();
    let _ = settle_ready_body(&h);
}

// S2 — flake gate: S1 body 100 times, byte-identical PNG each iteration.
#[test]
fn s2_settle_flake_gate_100x() {
    let h = Harness::new();
    for iter in 0..100 {
        // Fresh session each iteration; premature-settle vs timeout both fail.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| settle_ready_body(&h)))
            .unwrap_or_else(|_| panic!("S2: iteration {iter} failed the settle+golden body"));
    }
}

/// Run one S3 check under its OWN pump whose lifetime is bound to the check
/// (p0-council-r1 BLOCKER 1: a shared pump dying early would let a later check
/// observe a quiet pane and false-green). The pump stops right after the check
/// completes; its deadline (check timeout + 4s margin) only bounds the panic
/// path so a P0 `-32601` panic cannot leave it spinning.
fn s3_check_under_pump<T>(h: &Harness, pane_id: &str, check: impl FnOnce() -> T) -> T {
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        // §16.1 exception: this sleep paces INPUT (a token every 100 ms), it
        // never synchronises on output.
        let pump = scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_millis(6_000);
            while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
                h.send_line_token(pane_id, "");
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        let out = check();
        stop.store(true, Ordering::Relaxed);
        pump.join().expect("pump");
        out
    })
}

// S3 — settle timeout under a continuous 100 ms input pump.
#[test]
fn s3_settle_timeout() {
    let h = Harness::new();
    let f = h.launch_fixture("f3_flip.sh", 80, 24, "AAAAAAAAAA");

    // RPC check under its own pump.
    s3_check_under_pump(&h, &f.pane_id, || {
        let env = h.rpc_raw(
            "pane.wait_settled",
            serde_json::json!({ "pane_id": f.pane_id, "quiet_ms": 300, "timeout_ms": 2_000 }),
        );
        let settled = env.expect_result("S3 wait_settled rpc");
        assert_eq!(
            settled["settled"],
            serde_json::Value::Bool(false),
            "S3: must NOT settle while flipping"
        );
        let waited = settled["waited_ms"].as_u64().expect("waited_ms");
        assert!(
            (2_000..=2_000 + LENS_TEST_TOL_MS).contains(&waited),
            "S3: waited_ms {waited} out of [2000, 2000+{LENS_TEST_TOL_MS}]"
        );
    });

    // CLI twin under a FRESH pump that outlives the whole CLI call — it must
    // also observe a never-quiet pane and exit 1.
    s3_check_under_pump(&h, &f.pane_id, || {
        let out = h.cli(&[
            "pane",
            "wait-settled",
            &f.pane_id,
            "--quiet",
            "300ms",
            "--timeout",
            "2s",
        ]);
        assert_eq!(out.status.code(), Some(1), "S3: CLI exit 1 on timeout");
    });

    h.kill_session(&f.session_id);
}

// V1 — wait_settled parameter validation (LENS-R-025; p0-council-r1 major 8).
#[test]
fn v1_wait_settled_param_validation() {
    let h = Harness::new();
    let f = h.launch_fixture("f1_static.sh", 80, 24, "दृश्यते");

    // RPC: quiet_ms below the [10, 60_000] minimum → INVALID_PARAMS (-32602).
    let env = h.rpc_raw(
        "pane.wait_settled",
        serde_json::json!({ "pane_id": f.pane_id, "quiet_ms": 5, "timeout_ms": 10_000 }),
    );
    env.expect_error_code(-32602, "V1 rpc quiet below min");

    // RPC: timeout_ms < quiet_ms violates timeout ∈ [quiet_ms, 600_000].
    let env = h.rpc_raw(
        "pane.wait_settled",
        serde_json::json!({ "pane_id": f.pane_id, "quiet_ms": 300, "timeout_ms": 100 }),
    );
    env.expect_error_code(-32602, "V1 rpc timeout below quiet");

    // CLI twins: usage / INVALID_PARAMS → exit 2 (§10 exit table).
    let out = h.cli(&[
        "pane",
        "wait-settled",
        &f.pane_id,
        "--quiet",
        "5ms",
        "--timeout",
        "10s",
    ]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "V1: CLI exit 2 on quiet below min"
    );
    let out = h.cli(&[
        "pane",
        "wait-settled",
        &f.pane_id,
        "--quiet",
        "300ms",
        "--timeout",
        "100ms",
    ]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "V1: CLI exit 2 on timeout < quiet"
    );

    h.kill_session(&f.session_id);
}

// S4 — already-still returns immediately (two-call, no-sleep pattern).
#[test]
fn s4_already_still_returns_immediately() {
    let h = Harness::new();
    let f = h.launch_fixture("f1_static.sh", 80, 24, "दृश्यते");

    // First call legitimately waits and establishes quiet.
    let env1 = h.rpc_raw(
        "pane.wait_settled",
        serde_json::json!({ "pane_id": f.pane_id, "quiet_ms": 2_000, "timeout_ms": 5_000 }),
    );
    let s1 = env1.expect_result("S4 wait_settled #1");
    assert_eq!(s1["settled"], serde_json::Value::Bool(true));

    // Second call must return settled immediately (watch-channel race-free).
    let env2 = h.rpc_raw(
        "pane.wait_settled",
        serde_json::json!({ "pane_id": f.pane_id, "quiet_ms": 300, "timeout_ms": 5_000 }),
    );
    let s2 = env2.expect_result("S4 wait_settled #2");
    assert_eq!(s2["settled"], serde_json::Value::Bool(true));
    let waited = s2["waited_ms"].as_u64().expect("waited_ms");
    assert!(
        waited <= LENS_TEST_TOL_MS,
        "S4: already-still must return within {LENS_TEST_TOL_MS}ms, waited {waited}"
    );

    h.kill_session(&f.session_id);
}

// S5 — Class-B immunity: metadata spam must not reset quiet.
#[test]
fn s5_class_b_immunity() {
    let h = Harness::new();
    let f = h.launch_fixture("f9_metadata.sh", 80, 24, "LENS-F9-META");

    let stop = AtomicBool::new(false);
    let settled_rev = std::thread::scope(|scope| {
        // §16.1 exception: 100 ms INPUT pacing of Class-B tokens. Deadline-
        // bounded so a P0 panic (before `stop`) cannot leave it spinning.
        let pump = scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_millis(5_500);
            while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
                h.send_line_token(&f.pane_id, "");
                std::thread::sleep(Duration::from_millis(100));
            }
        });

        let env = h.rpc_raw(
            "pane.wait_settled",
            serde_json::json!({ "pane_id": f.pane_id, "quiet_ms": 500, "timeout_ms": 5_000 }),
        );
        let s = env.expect_result("S5 wait_settled rpc");
        assert_eq!(
            s["settled"],
            serde_json::Value::Bool(true),
            "S5: title/bell/cursor-shape spam must NOT reset quiet"
        );
        let rev = s["revision"].as_u64().expect("revision");
        stop.store(true, Ordering::Relaxed);
        pump.join().expect("pump");
        rev
    });

    // A real visible cell (V) must be observed as a NEW revision.
    h.send_line_token(&f.pane_id, "V");
    h.wait_for(&f.pane_id, "▮", 5_000).expect("V mark drawn");
    let env = h.rpc_raw(
        "pane.wait_settled",
        serde_json::json!({ "pane_id": f.pane_id, "quiet_ms": 300, "timeout_ms": 5_000 }),
    );
    let s = env.expect_result("S5 wait_settled after V");
    let rev_after = s["revision"].as_u64().expect("revision");
    assert!(
        rev_after > settled_rev,
        "S5: the visible V cell must bump the revision ({settled_rev} -> {rev_after})"
    );

    h.kill_session(&f.session_id);
}
