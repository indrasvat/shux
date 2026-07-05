//! Red suite — scratch sessions + `lens.run` (§8 SPEC-E; R1–R7 from §12).
//!
//! FROZEN after P0 (§16.2). Scratch is created ONLY by `lens.run` (DEC-21 /
//! delta 6). Every test leads with an RPC-form `lens.run`, so in Phase P0 it
//! fails with `method_not_found (-32601)` — the red receipt — before any
//! scratch is created.
//!
//! These are the ONLY tests that create scratch; they run serially under the
//! leak guard (`make test-lens`).

mod lens_common;
use lens_common::*;

use std::time::Duration;

fn f_argv(name: &str) -> serde_json::Value {
    serde_json::json!(["sh", Harness::fixture_rel(name)])
}

/// Field lookup tolerant of either a bare result or a `{result:{...}}` envelope.
fn field<'a>(v: &'a serde_json::Value, key: &str) -> &'a serde_json::Value {
    if v.get(key).is_some() {
        &v[key]
    } else {
        &v["result"][key]
    }
}

// R1 ⇄ — scratch lifecycle.
#[test]
fn r1_scratch_lifecycle() {
    let h = Harness::new();

    // RPC twin + P0 receipt: async run of F6 (exits 42) with a short ttl.
    let env = h.rpc_raw(
        "lens.run",
        serde_json::json!({
            "argv": f_argv("f6_exit42.sh"), "cols": 80, "rows": 24,
            "post_exit_ttl_ms": 1000
        }),
    );
    let r = env.expect_result("R1 lens.run rpc");
    let sid = r["session_id"].as_str().expect("session_id").to_string();
    assert!(r["pane_id"].is_string(), "R1: result carries pane_id");
    assert!(r["revision"].is_u64(), "R1: result carries revision");

    // F6 exits immediately → reap post_exit_ttl_ms later (bounded wait).
    assert!(
        wait_until(Duration::from_millis(1000 + LENS_TEST_TOL_MS), || !h
            .session_listed(&sid, true)),
        "R1: scratch must vanish from `session list --include-scratch` after ttl"
    );
    assert_eq!(
        Harness::count_procs("f6_exit42.sh"),
        0,
        "R1: no fixture procs remain"
    );
    assert!(
        h.audit_has(&["scratch", "create"]),
        "R1: audit scratch-create"
    );
    assert!(
        h.audit_has(&["reap", "exit"]),
        "R1: audit scratch-reap(reason=exit)"
    );

    // CLI --wait form: surfaces the child's exit code (42) and JSON fields.
    let rel = Harness::fixture_rel("f6_exit42.sh");
    let out = h.cli(&[
        "--format", "json", "lens", "run", "--size", "80x24", "--ttl", "1s", "--wait", "--", "sh",
        &rel,
    ]);
    assert_eq!(
        out.status.code(),
        Some(42),
        "R1: --wait exits with the child code (42)"
    );
    let j: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("R1: lens run --wait JSON");
    assert_eq!(field(&j, "exit_code"), &serde_json::json!(42));
    assert!(field(&j, "pane_id").is_string());
}

// R2 — hidden but authorized.
#[test]
fn r2_hidden_but_authorized() {
    let h = Harness::new();
    let env = h.rpc_raw(
        "lens.run",
        serde_json::json!({ "argv": f_argv("f1_static.sh"), "cols": 80, "rows": 24 }),
    );
    let r = env.expect_result("R2 lens.run rpc");
    let sid = r["session_id"].as_str().expect("session_id").to_string();
    let pane = r["pane_id"].as_str().expect("pane_id").to_string();

    assert!(
        !h.session_listed(&sid, false),
        "R2: default list omits scratch"
    );
    let entry = h
        .scratch_entry(&sid)
        .expect("R2: --include-scratch shows it");
    assert_eq!(
        entry["scratch"],
        serde_json::Value::Bool(true),
        "R2: flagged scratch:true"
    );

    // Glance works on the scratch pane.
    let g = h
        .rpc_raw("pane.glance", serde_json::json!({ "pane_id": pane }))
        .expect_result("R2 glance scratch");
    assert_eq!(g["cols"], 80);

    h.rpc_raw("session.kill", serde_json::json!({ "id": sid }));
}

// R3 ⇄ — PTY sizing truth.
#[test]
fn r3_pty_sizing_truth() {
    let h = Harness::new();

    for (cols, rows, want) in [(80u16, 24u16, "SIZE=24 80"), (120, 40, "SIZE=40 120")] {
        let env = h.rpc_raw(
            "lens.run",
            serde_json::json!({ "argv": f_argv("f7_winsize.sh"), "cols": cols, "rows": rows }),
        );
        let r = env.expect_result("R3 lens.run rpc");
        let sid = r["session_id"].as_str().expect("session_id").to_string();
        let pane = r["pane_id"].as_str().expect("pane_id").to_string();

        h.wait_for(&pane, want, 5_000)
            .unwrap_or_else(|e| panic!("R3: F7 never printed {want:?}: {e}"));
        let text = h.capture_text(&pane);
        assert!(
            text.lines().any(|l| l == want),
            "R3: captured text must contain the exact line {want:?}\n{text}"
        );
        h.rpc_raw("session.kill", serde_json::json!({ "id": sid }));
    }
}

// R4 — orphan proof + waiter drop.
#[test]
fn r4_orphan_proof_and_waiter_drop() {
    let h = Harness::new();

    // P0 receipt: RPC lens.run must exist. In P5 clean the probe scratch.
    let probe = h.rpc_raw(
        "lens.run",
        serde_json::json!({
            "argv": f_argv("f1_static.sh"), "cols": 80, "rows": 24,
            "max_runtime_ms": 2000, "post_exit_ttl_ms": 1000
        }),
    );
    let pr = probe.expect_result("R4 lens.run probe");
    h.rpc_raw(
        "session.kill",
        serde_json::json!({ "id": pr["session_id"] }),
    );
    assert!(
        wait_until(Duration::from_millis(2000), || Harness::count_procs(
            "f1_static.sh"
        ) == 0),
        "R4: probe scratch must be reaped before the real scenario"
    );

    // The real scenario: `lens run --wait` blocks (F1 never exits); launch it as
    // a child of the test, then SIGKILL the client.
    let rel = Harness::fixture_rel("f1_static.sh");
    let mut child = h
        .shux()
        .args([
            "lens",
            "run",
            "--max-runtime",
            "2s",
            "--wait",
            "--",
            "sh",
            &rel,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn lens run --wait child");

    assert!(
        wait_until(Duration::from_secs(5), || Harness::count_procs(
            "f1_static.sh"
        ) >= 1),
        "R4: scratch F1 should come up under --wait"
    );
    // Kill the CLIENT (the waiter) — the scratch must outlive it.
    let _ = child.kill();
    let _ = child.wait();

    assert!(
        h.system_health_ok(),
        "R4(a): dropped waiter must not wedge the daemon"
    );
    assert!(
        Harness::count_procs("f1_static.sh") >= 1,
        "R4(b): scratch F1 must survive the client's death"
    );

    // R4(c): after max_runtime the daemon reaps the scratch.
    assert!(
        wait_until(
            Duration::from_millis(2000 + LENS_TEST_TOL_MS),
            || Harness::count_procs("f1_static.sh") == 0
        ),
        "R4(c): scratch must be reaped at max_runtime"
    );
    assert!(
        h.audit_has(&["reap", "max_runtime"]),
        "R4(c): audit reap(reason=max_runtime)"
    );
}

// R5 — live resize.
#[test]
fn r5_live_resize() {
    let h = Harness::new();
    let env = h.rpc_raw(
        "lens.run",
        serde_json::json!({ "argv": f_argv("f7_winsize.sh"), "cols": 80, "rows": 24 }),
    );
    let r = env.expect_result("R5 lens.run rpc");
    let sid = r["session_id"].as_str().expect("session_id").to_string();
    let pane = r["pane_id"].as_str().expect("pane_id").to_string();

    // Checkpoint before the resize (must be invalidated after).
    let c = h
        .rpc_raw("pane.checkpoint", serde_json::json!({ "pane_id": pane }))
        .expect_result("R5 checkpoint")["revision"]
        .as_u64()
        .expect("checkpoint rev");

    // Absolute sizing (NOT `pane resize`, which is relative/axis-based).
    h.rpc_ok(
        "pane.set_size",
        serde_json::json!({ "pane_id": pane, "cols": 120, "rows": 40 }),
    );
    h.wait_for(&pane, "SIZE=40 120", 5_000)
        .expect("R5: SIGWINCH reprint");

    let g = h
        .rpc_raw("pane.glance", serde_json::json!({ "pane_id": pane }))
        .expect_result("R5 glance after resize");
    assert_eq!(g["cols"], 120);
    assert_eq!(g["rows"], 40);
    assert!(
        g["revision"].as_u64().unwrap() > c,
        "R5: resize must bump the revision"
    );

    let diff = h.rpc_raw(
        "pane.diff_since",
        serde_json::json!({ "pane_id": pane, "since_revision": c }),
    );
    diff.expect_error_code(-32011, "R5: prior checkpoint invalidated by resize");

    h.rpc_raw("session.kill", serde_json::json!({ "id": sid }));
}

// R6 — quota.
#[test]
fn r6_quota() {
    let h = Harness::new();
    let mut ids = Vec::new();

    // 16 scratch sessions.
    for i in 0..16 {
        let env = h.rpc_raw(
            "lens.run",
            serde_json::json!({ "argv": f_argv("f1_static.sh"), "cols": 80, "rows": 24 }),
        );
        let r = env.expect_result(&format!("R6 lens.run #{i}"));
        ids.push(r["session_id"].as_str().expect("session_id").to_string());
    }

    // The 17th exceeds the quota.
    let env = h.rpc_raw(
        "lens.run",
        serde_json::json!({ "argv": f_argv("f1_static.sh"), "cols": 80, "rows": 24 }),
    );
    env.expect_error_code(-32012, "R6: 17th scratch → RESOURCE_EXHAUSTED");
    let rel = Harness::fixture_rel("f1_static.sh");
    let out = h.cli(&["lens", "run", "--size", "80x24", "--", "sh", &rel]);
    assert_eq!(
        out.status.code(),
        Some(5),
        "R6: CLI exit 5 on RESOURCE_EXHAUSTED"
    );

    // Kill one, retry succeeds.
    let victim = ids.pop().expect("victim");
    h.rpc_raw("session.kill", serde_json::json!({ "id": victim }));
    assert!(
        wait_until(Duration::from_secs(3), || !h.session_listed(&victim, true)),
        "R6: killed scratch must free a slot"
    );
    let env = h.rpc_raw(
        "lens.run",
        serde_json::json!({ "argv": f_argv("f1_static.sh"), "cols": 80, "rows": 24 }),
    );
    let r = env.expect_result("R6 retry after kill");
    ids.push(r["session_id"].as_str().expect("session_id").to_string());

    for id in ids {
        h.rpc_raw("session.kill", serde_json::json!({ "id": id }));
    }
}

// R7 — registry reap on daemon restart.
#[test]
fn r7_registry_reap() {
    let h = Harness::new();
    let env = h.rpc_raw(
        "lens.run",
        serde_json::json!({ "argv": f_argv("f1_static.sh"), "cols": 80, "rows": 24 }),
    );
    let _ = env.expect_result("R7 lens.run rpc");
    assert!(
        wait_until(Duration::from_secs(5), || Harness::count_procs(
            "f1_static.sh"
        ) >= 1),
        "R7: scratch F1 should be running"
    );

    // SIGKILL the daemon (no graceful reap of children).
    let dpid = h.daemon_pid().expect("R7: daemon pid");
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    kill(Pid::from_raw(dpid), Signal::SIGKILL).expect("kill daemon");
    assert!(
        wait_until(Duration::from_secs(5), || kill(Pid::from_raw(dpid), None)
            .is_err()),
        "R7: daemon must be gone after SIGKILL"
    );

    // Any command auto-starts a fresh daemon → startup registry reap.
    assert!(h.system_health_ok(), "R7: fresh daemon must come up");
    assert!(
        wait_until(Duration::from_secs(5), || Harness::count_procs(
            "f1_static.sh"
        ) == 0),
        "R7: startup must kill the registered scratch pgid (zero F1 procs)"
    );

    let registry = h.runtime_dir().join("shux").join("scratch-registry.json");
    assert!(
        !registry.exists(),
        "R7: registry file removed after reap (LENS-R-044)"
    );
    assert!(
        h.audit_has(&["reap", "registry"]),
        "R7: audit reap(reason=registry)"
    );
}
