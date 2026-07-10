//! P4 DoD (PRD §7.4, council D2) black-box half: `pane.diff_since` must report
//! the exact delta even while OTHER readers hammer the same pane's render path
//! concurrently. The daemon serves `pane.snapshot` / `pane.glance` (the same
//! grid reads an attached client's render loop performs) from background
//! threads while the foreground does checkpoint → drive → settle → diff.
//!
//! The DirtyState-independence angle (the literal "attached client drains
//! DirtyState") is proven directly in-process by
//! `crates/shux/src/main.rs::tests::compute_lens_diff_independent_of_dirtystate_drains`
//! (drains the VT dirty regions between the checkpoint clone and the diff and
//! asserts the delta is unchanged). Together they are the full council-D2
//! proof: the diff reads cell VALUES via `clone_visible`, never render state.
//!
//! NOT part of the frozen lens red suite (§16.2 freezes `lens_*` files; this is
//! implementation-owned regression coverage). Reuses the frozen `lens_common`
//! harness READ-ONLY via `#[path]` — no frozen file is modified.

#[path = "lens_common/mod.rs"]
mod lens_common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lens_common::Harness;

// The exact F4 delta after `a` — identical to the frozen D2 assertion: red
// block at (2,2) + "A-PRESSED" at (5,10)..(5,18), exactly 10 cells.
fn f4_expected_regions() -> serde_json::Value {
    serde_json::json!([
        { "row": 2, "col_start": 2, "col_end": 3 },
        { "row": 5, "col_start": 10, "col_end": 19 },
    ])
}

#[test]
fn diff_exact_while_concurrent_readers_render_same_pane() {
    let h = Arc::new(Harness::new());
    let f = h.launch_fixture("f4_keys.sh", 80, 24, "LENS-F4-KEYS");

    // Checkpoint the pre-`a` frame.
    let env = h.rpc_raw(
        "pane.glance",
        serde_json::json!({ "pane_id": f.pane_id, "checkpoint": true }),
    );
    let r1 = env.expect_result("checkpoint glance")["revision"]
        .as_u64()
        .expect("checkpoint revision");

    // Spawn concurrent readers that continuously exercise the daemon's render
    // path against the SAME pane (snapshot rasterizes; glance clones + renders).
    // These are exactly the reads an attached client's compositor performs.
    let stop = Arc::new(AtomicBool::new(false));
    let mut readers = Vec::new();
    for _ in 0..4 {
        let hc = Arc::clone(&h);
        let stopc = Arc::clone(&stop);
        let pane = f.pane_id.clone();
        readers.push(std::thread::spawn(move || {
            while !stopc.load(Ordering::Relaxed) {
                let _ = hc.rpc_raw("pane.snapshot", serde_json::json!({ "pane_id": pane }));
                let _ = hc.rpc_raw("pane.glance", serde_json::json!({ "pane_id": pane }));
            }
        }));
    }

    // Drive `a` and settle while the readers churn.
    h.send_raw(&f.pane_id, "a");
    let env = h.rpc_raw(
        "pane.wait_settled",
        serde_json::json!({ "pane_id": f.pane_id, "quiet_ms": 300, "timeout_ms": 5_000 }),
    );
    assert_eq!(
        env.expect_result("settle after a")["settled"],
        serde_json::Value::Bool(true),
        "pane must settle after `a` despite concurrent readers"
    );

    // The diff must still be exact — concurrent render reads never corrupt it.
    let env = h.rpc_raw(
        "pane.diff_since",
        serde_json::json!({ "pane_id": f.pane_id, "since_revision": r1 }),
    );
    let d = env.expect_result("diff under concurrent readers");
    assert_eq!(
        d["cells_changed"], 10,
        "diff exact (10 cells) under concurrent render pressure"
    );
    assert_eq!(
        d["regions"],
        f4_expected_regions(),
        "diff regions exact under concurrent render pressure"
    );
    assert_eq!(d["from_revision"], r1, "from_revision");

    stop.store(true, Ordering::Relaxed);
    for r in readers {
        let _ = r.join();
    }
    h.kill_session(&f.session_id);
}
