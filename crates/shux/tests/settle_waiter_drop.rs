//! P3 codex B2 lens-side proof (black-box half): SIGKILL a real `shux pane
//! wait-settled` CLI mid-wait and prove the daemon stays healthy and the pane
//! stays serviceable. The waiter-GONE observable (the pane's revision-watch
//! receiver_count dropping back to zero) requires in-process access and lives
//! in `crates/shux/src/main.rs::tests::
//! production_settle_waiter_dropped_on_client_disconnect`; together the two
//! tests are the full disconnect-drops-waiter proof.
//!
//! NOT part of the frozen lens red suite (§16.2 freezes `lens_*` files; this
//! is implementation-owned regression coverage). Reuses the frozen
//! `lens_common` harness READ-ONLY via `#[path]` — no frozen file is modified.

#[path = "lens_common/mod.rs"]
mod lens_common;

use std::time::Duration;

use lens_common::{Harness, unique};

#[test]
fn killed_cli_mid_wait_leaves_daemon_healthy() {
    let h = Harness::new();

    // Ordinary session; the fresh pane's last_mutation_ns is stamped at
    // spawn (LENS-R-002), so with quiet=60s (the LENS-R-025 max) the waiter
    // cannot settle during this test — a "never-settling" wait with no
    // output pump needed.
    let session_name = format!("waiter-drop-{}", unique());
    let created = h.rpc_ok(
        "session.create",
        serde_json::json!({
            "name": session_name,
            "cwd": h.repo_root().display().to_string(),
        }),
    );
    let session_id = created["id"].as_str().expect("session id").to_string();
    let pane_id = created["pane_id"].as_str().expect("pane id").to_string();

    // A real CLI waiter as a child process.
    let mut waiter = h
        .shux()
        .args([
            "pane",
            "wait-settled",
            &pane_id,
            "--quiet",
            "60s",
            "--timeout",
            "600s",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn wait-settled CLI");

    // Action pacing, mirroring the frozen R4 spec's "SIGKILLed after ~500 ms"
    // pattern (§16.1: this paces the KILL action so the CLI has issued its
    // RPC; no assertion below synchronizes on this sleep).
    std::thread::sleep(Duration::from_millis(500));

    // SIGKILL — the client vanishes without any protocol goodbye.
    waiter.kill().expect("SIGKILL wait-settled CLI");
    let status = waiter.wait().expect("reap killed CLI");
    assert!(
        !status.success(),
        "killed CLI must not report success: {status:?}"
    );

    // (a) The daemon stays healthy and responsive — the dropped waiter did
    // not wedge it.
    assert!(
        h.system_health_ok(),
        "daemon must answer system.health after the CLI kill"
    );

    // (b) The pane is fully serviceable: a fresh waiter subscribes and
    // settles (the pane has been idle since spawn, so a 1s quiet window
    // closes quickly). Before the B2 fix the abandoned 60s waiter would
    // still be parked server-side; after it, this waiter is the only one.
    let env = h.rpc_raw(
        "pane.wait_settled",
        serde_json::json!({ "pane_id": pane_id, "quiet_ms": 1_000, "timeout_ms": 10_000 }),
    );
    let settled = env.expect_result("post-kill wait_settled");
    assert_eq!(
        settled["settled"],
        serde_json::Value::Bool(true),
        "an idle pane must settle for a fresh waiter after the kill"
    );

    h.kill_session(&session_id);
}
