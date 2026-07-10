//! P5 convergence round 1, codex B3 + SIGHUP-vs-SIGTERM major (task 077):
//! the scratch reap path must (a) deliver LENS-R-042's exact signal
//! sequence — killpg(SIGTERM), 500 ms grace, killpg(SIGKILL) — and (b)
//! remove the registry row ONLY after the process group is confirmed dead,
//! so a daemon crash mid-reap always leaves the row for the next
//! incarnation's startup reap.
//!
//! Observable proof, black-box, through a real daemon:
//! - The scratch workload TRAPS SIGTERM, writes a marker file, and keeps
//!   running. Marker present + process later gone ⇒ TERM was delivered
//!   first (the trap fired) AND KILL followed (nothing else could end a
//!   TERM-ignoring loop).
//! - While the trap-holder rides out the 500 ms grace window, the registry
//!   file still carries its row; the row disappears only after the process
//!   dies.
//!
//! NOT part of the frozen lens red suite (§16.2 freezes `lens_*` files).
//! Reuses the frozen `lens_common` harness READ-ONLY via `#[path]`.

#[path = "lens_common/mod.rs"]
mod lens_common;

use std::time::Duration;

use lens_common::{Harness, wait_until};

/// Count live processes whose argv contains `tag`. The tag is generated at
/// RUNTIME (never written to any file), so co-tenant processes that merely
/// mention test text in their argv (the p0-council-r3 false-match incident)
/// cannot collide with it.
fn count_procs_with_tag(tag: &str) -> usize {
    let out = std::process::Command::new("ps")
        .args(["-axo", "args="])
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter(|l| l.contains(tag))
            .count(),
        Err(_) => 0,
    }
}

#[test]
fn reap_sends_sigterm_then_sigkill_and_removes_row_only_after_death() {
    let h = Harness::new();
    let tag = format!("LENSP5REAP{}", lens_common::unique().replace('-', ""));
    let marker = std::env::temp_dir().join(format!("lens_reap_marker_{tag}"));
    let _ = std::fs::remove_file(&marker);

    // The workload: colored banner (house color rule — truecolor + 256 +
    // basic ANSI), then trap TERM → write marker → KEEP RUNNING (a plain
    // TERM can never end it; only the follow-up KILL can). The unique tag
    // rides in an argv variable assignment so `ps` can count exactly this
    // process tree and nothing else.
    let script = format!(
        "TAG={tag}; \
         printf '\\033[38;2;255;100;50mTRAP-READY\\033[0m \\033[38;5;46mOK\\033[0m \\033[32mGO\\033[0m\\n'; \
         trap 'echo TERMED > {marker}' TERM; \
         while :; do sleep 0.2; done",
        marker = marker.display(),
    );

    // max_runtime at its 1000 ms minimum: the reap (TERM) fires ~1 s in.
    let env = h.rpc_raw(
        "lens.run",
        serde_json::json!({
            "argv": ["sh", "-c", script],
            "cols": 40, "rows": 10,
            "max_runtime_ms": 1000,
        }),
    );
    let r = env.expect_result("lens.run trap-holder");
    let sid = r["session_id"].as_str().expect("session_id").to_string();
    h.wait_for(r["pane_id"].as_str().expect("pane_id"), "TRAP-READY", 5_000)
        .expect("workload up before the reap window");
    assert!(
        count_procs_with_tag(&tag) >= 1,
        "trap-holder process running"
    );

    let registry_path = h.runtime_dir().join("shux").join("scratch-registry.json");
    assert!(
        std::fs::read_to_string(&registry_path)
            .expect("registry persisted while scratch is live")
            .contains(&sid),
        "registry row present before the reap"
    );

    // (a) SIGTERM first: the trap writes the marker at max_runtime (~1 s).
    assert!(
        wait_until(
            Duration::from_millis(1000 + lens_common::LENS_TEST_TOL_MS),
            || { marker.exists() }
        ),
        "SIGTERM must be delivered at max_runtime (trap never fired)"
    );

    // (b) Row survives the grace window: the trap-holder ignores TERM, so
    // the group stays alive for the full 500 ms grace — the registry row
    // must still be there right after the marker appears (row removal
    // strictly follows death confirmation).
    assert!(
        std::fs::read_to_string(&registry_path)
            .expect("registry still on disk during the grace window")
            .contains(&sid),
        "registry row must survive until the group is confirmed dead"
    );

    // (a) SIGKILL second: only KILL can end a TERM-trapping loop.
    assert!(
        wait_until(
            Duration::from_millis(2500 + lens_common::LENS_TEST_TOL_MS),
            || { count_procs_with_tag(&tag) == 0 }
        ),
        "SIGKILL must follow the grace window (TERM-ignoring workload survived)"
    );
    assert_eq!(
        std::fs::read_to_string(&marker)
            .expect("marker readable")
            .trim(),
        "TERMED",
        "the marker carries the trap's payload (TERM, not KILL, fired it)"
    );

    // (b) Row removed only after death — and the session is gone with it.
    assert!(
        wait_until(Duration::from_secs(3), || {
            !std::fs::read_to_string(&registry_path)
                .map(|t| t.contains(&sid))
                .unwrap_or(false)
        }),
        "registry row removed after the group died"
    );
    assert!(
        wait_until(Duration::from_secs(3), || !h.session_listed(&sid, true)),
        "scratch session reaped from the graph"
    );
    assert!(
        h.audit_has(&["reap", "max_runtime"]),
        "audit reap(reason=max_runtime) present"
    );

    let _ = std::fs::remove_file(&marker);
}
