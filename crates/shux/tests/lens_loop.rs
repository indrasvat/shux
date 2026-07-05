//! Red suite — drive regression + whole-loop (§12 K1, E1).
//!
//! FROZEN after P0 (§16.2). K1 leads with a glance-checkpoint; E1 leads with
//! `lens.run`. Both fail at that first lens call with `method_not_found
//! (-32601)` in Phase P0.

mod lens_common;
use lens_common::*;

use std::time::{Duration, Instant};

fn f4_expected_regions() -> serde_json::Value {
    serde_json::json!([
        { "row": 2, "col_start": 2, "col_end": 3 },
        { "row": 5, "col_start": 10, "col_end": 19 },
    ])
}

fn glance_checkpoint(h: &Harness, pane: &str, ctx: &str) -> (u64, Vec<u8>) {
    let g = h
        .rpc_raw(
            "pane.glance",
            serde_json::json!({ "pane_id": pane, "checkpoint": true }),
        )
        .expect_result(ctx);
    use base64::Engine;
    let png = base64::engine::general_purpose::STANDARD
        .decode(g["png_base64"].as_str().expect("glance png"))
        .expect("decode png");
    (g["revision"].as_u64().expect("revision"), png)
}

fn settle(h: &Harness, pane: &str, ctx: &str) {
    let s = h
        .rpc_raw(
            "pane.wait_settled",
            serde_json::json!({ "pane_id": pane, "quiet_ms": 300, "timeout_ms": 5_000 }),
        )
        .expect_result(ctx);
    assert_eq!(
        s["settled"],
        serde_json::Value::Bool(true),
        "{ctx}: expected settle"
    );
}

// K1 — drive regression (Tab marker moves exactly 2 cells per press).
#[test]
fn k1_drive_regression() {
    let h = Harness::new();
    let f = h.launch_fixture("f4_keys.sh", 80, 24, "LENS-F4-KEYS");

    // Initial checkpoint (cp0) — with the three per-press checkpoints this uses
    // exactly the LENS-R-031 cap of 4 slots.
    let (mut prev, _) = glance_checkpoint(&h, &f.pane_id, "K1 cp0");

    for i in 1..=3 {
        h.send_raw(&f.pane_id, "\t");
        settle(&h, &f.pane_id, &format!("K1 settle #{i}"));
        let (rev, png) = glance_checkpoint(&h, &f.pane_id, &format!("K1 cp{i}"));

        let d = h
            .rpc_raw(
                "pane.diff_since",
                serde_json::json!({ "pane_id": f.pane_id, "since_revision": prev }),
            )
            .expect_result(&format!("K1 diff #{i}"));
        assert_eq!(d["cells_changed"], 2, "K1: each Tab moves exactly 2 cells");

        assert_png_golden(&h, &png, &format!("k1_pos{i}.png"));
        prev = rev;
    }

    h.kill_session(&f.session_id);
}

// E1 — the whole loop (run → settle → glance → drive → settle → diff).
#[test]
fn e1_whole_loop() {
    let h = Harness::new();
    let start = Instant::now();

    // run F4 in a scratch (the only lens.run in this file).
    let r = h
        .rpc_raw(
            "lens.run",
            serde_json::json!({
                "argv": ["sh", Harness::fixture_rel("f4_keys.sh")], "cols": 80, "rows": 24
            }),
        )
        .expect_result("E1 lens.run");
    let sid = r["session_id"].as_str().expect("session_id").to_string();
    let pane = r["pane_id"].as_str().expect("pane_id").to_string();

    h.wait_for(&pane, "LENS-F4-KEYS", 5_000)
        .expect("E1: fixture up");
    settle(&h, &pane, "E1 settle initial");
    let (rev, png) = glance_checkpoint(&h, &pane, "E1 glance");
    assert_png_golden(&h, &png, "e1_glance.png");

    h.send_raw(&pane, "a");
    settle(&h, &pane, "E1 settle after a");

    let d = h
        .rpc_raw(
            "pane.diff_since",
            serde_json::json!({ "pane_id": pane, "since_revision": rev, "heat_png": true }),
        )
        .expect_result("E1 diff");
    assert_eq!(d["cells_changed"], 10, "E1: exact 10-cell delta");
    assert_eq!(d["regions"], f4_expected_regions(), "E1: exact regions");
    use base64::Engine;
    let heat = base64::engine::general_purpose::STANDARD
        .decode(d["heat_png_base64"].as_str().expect("heat png"))
        .expect("decode heat png");
    assert_png_golden(&h, &heat, "e1_heat.png");

    assert!(
        start.elapsed() <= Duration::from_secs(10),
        "E1: the whole loop must finish within 10s (was {:?})",
        start.elapsed()
    );

    h.rpc_raw("session.kill", serde_json::json!({ "id": sid }));
}
