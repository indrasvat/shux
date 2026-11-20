//! P5 convergence round 1, codex B3 + SIGHUP-vs-SIGTERM major (task 077):
//! the scratch reap path must (a) end the group itself at max_runtime —
//! killpg(SIGTERM), 500 ms grace, killpg(SIGKILL) — and (b) remove the
//! registry row ONLY after the process group is confirmed dead, so a daemon
//! crash mid-reap always leaves the row for the next incarnation's startup
//! reap.
//!
//! Observable proof, black-box, through a real daemon:
//! - The workload ignores SIGTERM and loops. Its `pane.exited` says how it
//!   died and that shux sent that signal: SIGTERM if it came before the
//!   workload ignored it, SIGKILL after the grace otherwise. The order is
//!   `PtyHandle::end_group`'s contract, tested in shux-pty.
//! - The registry row never goes while the process lives: every sample
//!   that reads the row as gone must then find the process gone too.
//!
//! NOT part of the frozen lens red suite (§16.2 freezes `lens_*` files).
//! Reuses the frozen `lens_common` harness READ-ONLY via `#[path]`.

#[path = "lens_common/mod.rs"]
mod lens_common;

use lens_common::{BUDGET, Harness, wait_until};

/// Count live processes whose argv contains `tag`. The tag is generated at
/// RUNTIME (never written to any file), so co-tenant processes that merely
/// mention test text in their argv (the p0-council-r3 false-match incident)
/// cannot collide with it.
fn count_procs_with_tag(tag: &str) -> usize {
    let out = std::process::Command::new("ps")
        .args(["-axo", "args="])
        .output()
        .expect("run ps");
    assert!(
        out.status.success(),
        "ps failed: a failed scan would count 0"
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains(tag))
        .count()
}

#[test]
fn reap_sends_sigterm_then_sigkill_and_removes_row_only_after_death() {
    let h = Harness::new();
    let tag = format!("LENSP5REAP{}", lens_common::unique().replace('-', ""));

    // The workload: ignore TERM (the loop's children inherit that), then a
    // colored banner (house color rule — truecolor + 256 + basic ANSI), then
    // loop. The unique tag rides in an argv variable assignment so `ps` can
    // count exactly this process tree and nothing else.
    let script = format!(
        "TAG={tag}; \
         trap '' TERM; \
         printf '\\033[38;2;255;100;50mTRAP-READY\\033[0m \\033[38;5;46mOK\\033[0m \\033[32mGO\\033[0m\\n'; \
         while :; do sleep 0.2; done",
    );

    let env = h.rpc_raw(
        "lens.run",
        serde_json::json!({
            "argv": ["sh", "-c", script],
            "cols": 40, "rows": 10,
            "max_runtime_ms": 5000,
        }),
    );
    let r = env.expect_result("lens.run trap-holder");
    let sid = r["session_id"].as_str().expect("session_id").to_string();
    let pane = r["pane_id"].as_str().expect("pane_id").to_string();
    let registry_path = h.runtime_dir().join("shux").join("scratch-registry.json");

    // (b) Row removal strictly follows death: read the row, THEN count the
    // processes, so a row read as gone with the group still alive afterwards
    // was removed while the group lived.
    let row_outlives_group = || {
        let row = std::fs::read_to_string(&registry_path)
            .map(|t| t.contains(&sid))
            .unwrap_or(false);
        let alive = count_procs_with_tag(&tag);
        assert!(
            row || alive == 0,
            "registry row removed while {alive} process(es) of the group still ran"
        );
        alive
    };
    assert!(
        wait_until(BUDGET, || row_outlives_group() == 0),
        "the group outlived its max_runtime reap"
    );

    // (a) shux ended it: by SIGTERM, or by SIGKILL once TERM was ignored.
    let mut exit = serde_json::Value::Null;
    assert!(
        wait_until(BUDGET, || {
            let history = h.rpc_ok(
                "events.history",
                serde_json::json!({ "filter": ["pane.exited"], "count": 1000 }),
            );
            exit = history["events"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|e| e["data"]["data"].clone())
                .find(|d| d["pane_id"] == pane.as_str() && !d["exit_status"].is_null())
                .unwrap_or_default();
            !exit.is_null()
        }),
        "no pane.exited with an exit status for {pane}"
    );
    assert_eq!(exit["by_shux"], true, "shux did not end the group: {exit}");
    assert!(
        exit["signal"] == 15 || exit["signal"] == 9,
        "ended by neither SIGTERM nor SIGKILL: {exit}"
    );

    // (b) Row removed only after death — and the session is gone with it.
    assert!(
        wait_until(BUDGET, || {
            !std::fs::read_to_string(&registry_path)
                .map(|t| t.contains(&sid))
                .unwrap_or(false)
        }),
        "registry row removed after the group died"
    );
    assert!(
        wait_until(BUDGET, || !h.session_listed(&sid, true)),
        "scratch session reaped from the graph"
    );
    assert!(
        h.audit_has(&["reap", "max_runtime"]),
        "audit reap(reason=max_runtime) present"
    );
}
