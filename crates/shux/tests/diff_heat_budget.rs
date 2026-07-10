//! PR #91 codex P1 — `pane.diff_since{heat_png:true}` pre-render pixel
//! budget (task 077). A 1000×1000 pane is valid per `pane.set_size` limits,
//! but rasterizing it for the heat PNG would allocate hundreds of MB of RGBA
//! before the post-encode 8 MiB cap could fire. The heat path now enforces
//! the SAME 16M-pixel budget `pane.glance`/`pane.snapshot` apply, BEFORE any
//! allocation → `PAYLOAD_TOO_LARGE (-32013)`. The guard predicate itself is
//! unit-tested in `main.rs::tests::lens_pixel_budget_check_guard_predicate`;
//! this is the black-box half.
//!
//! NOT part of the frozen lens red suite (§16.2 freezes `lens_*` files).
//! Reuses the frozen `lens_common` harness READ-ONLY via `#[path]`.

#[path = "lens_common/mod.rs"]
mod lens_common;

use lens_common::Harness;

#[test]
fn oversized_pane_heat_diff_rejected_before_render() {
    let h = Harness::new();

    // F1's colored content (truecolor gradient + 256-color strip + basic
    // blocks — house color rule) on a pane grown to pane.set_size's maximum
    // BEFORE the fixture draws, so the checkpoint below is minted at the
    // oversized dimensions (a later resize would just invalidate it).
    let session_name = format!("heat-budget-{}", lens_common::unique());
    let created = h.rpc_ok(
        "session.create",
        serde_json::json!({
            "name": session_name,
            "cwd": h.repo_root().display().to_string(),
        }),
    );
    let session_id = created["id"].as_str().expect("session id").to_string();
    let pane_id = created["pane_id"].as_str().expect("pane id").to_string();
    h.rpc_ok(
        "pane.set_size",
        serde_json::json!({ "pane_id": pane_id, "cols": 1000, "rows": 1000 }),
    );
    let abs = h.fixture_abs("f1_static.sh");
    h.rpc_ok(
        "pane.send_keys",
        serde_json::json!({ "pane_id": pane_id, "text": format!("exec sh {abs}\n") }),
    );
    h.wait_for(&pane_id, "दृश्यते", 10_000).expect("F1 sentinel");

    // Checkpoint at the oversized size (cell-level storage — no raster).
    let env = h.rpc_raw("pane.checkpoint", serde_json::json!({ "pane_id": pane_id }));
    let r = env.expect_result("checkpoint at 1000x1000")["revision"]
        .as_u64()
        .expect("checkpoint revision");

    // heat_png=true → the pre-render budget rejects with -32013 BEFORE any
    // RGBA allocation (162M pixels at the bundled font's metrics >> 16M).
    let env = h.rpc_raw(
        "pane.diff_since",
        serde_json::json!({ "pane_id": pane_id, "since_revision": r, "heat_png": true }),
    );
    let err = env.expect_error_code(-32013, "oversized heat diff");
    let data = err.data.as_ref().expect("-32013 carries data");
    let pixels = data["pixels"].as_u64().expect("pixels");
    let max = data["max_pixels"].as_u64().expect("max_pixels");
    assert!(
        pixels > max,
        "reported pixel count ({pixels}) must exceed the budget ({max})"
    );
    assert!(
        data["hint"]
            .as_str()
            .is_some_and(|s| s.contains("heat_png=false")),
        "hint names the heat-specific escape hatch: {data}"
    );

    // CLI twin: `--heat` maps the same rejection to exit 5 (§10 table).
    let heat_path =
        std::env::temp_dir().join(format!("lens_heat_budget_{}.png", lens_common::unique()));
    let cli = h.cli_envelope(&[
        "pane",
        "diff",
        &pane_id,
        "--since",
        &r.to_string(),
        "--heat",
        heat_path.to_str().expect("tmp path utf8"),
    ]);
    cli.expect_error_code(-32013, "CLI oversized heat diff envelope");
    assert_eq!(cli.exit_code, 5, "PAYLOAD_TOO_LARGE maps to exit 5");
    assert!(
        !heat_path.exists(),
        "no heat file may be written on rejection"
    );

    // The budget gates ONLY the heat path: the cell-level diff on the same
    // oversized pane succeeds (zero delta — nothing changed since the
    // checkpoint; F1 blocks on read after drawing).
    let env = h.rpc_raw(
        "pane.diff_since",
        serde_json::json!({ "pane_id": pane_id, "since_revision": r }),
    );
    let d = env.expect_result("heat-less diff on the oversized pane");
    assert_eq!(d["cells_changed"], 0, "cell diff unaffected by the budget");
    assert_eq!(d["from_revision"], r);

    h.kill_session(&session_id);
}
