//! Red suite — checkpoints + `pane.diff_since` (§7 SPEC-D; D1–D4, A1 from §12).
//!
//! FROZEN after P0 (§16.2). In Phase P0 `pane.glance` / `pane.checkpoint` /
//! `pane.diff_since` are unregistered, so each test fails at its first lens
//! call with `method_not_found (-32601)`.

mod lens_common;
use lens_common::*;

/// The exact F4 delta after `a` (and after the style-only `s`): the red block
/// at grid (2,2) and green-bold "A-PRESSED" at grid (5,10)..(5,18). D2 and D4
/// MUST report identical regions.
fn f4_expected_regions() -> serde_json::Value {
    serde_json::json!([
        { "row": 2, "col_start": 2, "col_end": 3 },
        { "row": 5, "col_start": 10, "col_end": 19 },
    ])
}

fn glance_checkpoint_revision(h: &Harness, pane: &str, ctx: &str) -> u64 {
    let env = h.rpc_raw(
        "pane.glance",
        serde_json::json!({ "pane_id": pane, "checkpoint": true }),
    );
    env.expect_result(ctx)["revision"]
        .as_u64()
        .expect("glance revision")
}

fn checkpoint_revision(h: &Harness, pane: &str, ctx: &str) -> u64 {
    let env = h.rpc_raw("pane.checkpoint", serde_json::json!({ "pane_id": pane }));
    env.expect_result(ctx)["revision"]
        .as_u64()
        .expect("checkpoint revision")
}

fn settle(h: &Harness, pane: &str, ctx: &str) {
    let env = h.rpc_raw(
        "pane.wait_settled",
        serde_json::json!({ "pane_id": pane, "quiet_ms": 300, "timeout_ms": 5_000 }),
    );
    let s = env.expect_result(ctx);
    assert_eq!(
        s["settled"],
        serde_json::Value::Bool(true),
        "{ctx}: expected settle"
    );
}

// D1 ⇄ — diff zero delta.
#[test]
fn d1_diff_zero_delta() {
    let h = Harness::new();
    let f = h.launch_fixture("f1_static.sh", 80, 24, "दृश्यते");

    let rev = glance_checkpoint_revision(&h, &f.pane_id, "D1 glance checkpoint");
    // No input between checkpoint and diff.
    let env = h.rpc_raw(
        "pane.diff_since",
        serde_json::json!({ "pane_id": f.pane_id, "since_revision": rev }),
    );
    let d = env.expect_result("D1 diff");

    assert_eq!(d["cells_changed"], 0);
    assert_eq!(d["regions"], serde_json::json!([]));
    assert_eq!(d["changed_row_text"], serde_json::json!({}));
    assert_eq!(d["cursor_moved"], serde_json::Value::Bool(false));
    assert_eq!(d["regions_truncated"], serde_json::Value::Bool(false));
    // Zero-delta bounding box is all zeros (delta 5).
    assert_eq!(d["bounding_box"]["row_start"], 0);
    assert_eq!(d["bounding_box"]["col_start"], 0);
    assert_eq!(d["bounding_box"]["row_end"], 0);
    assert_eq!(d["bounding_box"]["col_end"], 0);

    h.kill_session(&f.session_id);
}

// D2 ⇄ — diff exact delta.
#[test]
fn d2_diff_exact_delta() {
    let h = Harness::new();
    let f = h.launch_fixture("f4_keys.sh", 80, 24, "LENS-F4-KEYS");

    let r1 = glance_checkpoint_revision(&h, &f.pane_id, "D2 glance checkpoint");
    h.send_raw(&f.pane_id, "a");
    settle(&h, &f.pane_id, "D2 settle after a");

    let env = h.rpc_raw(
        "pane.diff_since",
        serde_json::json!({ "pane_id": f.pane_id, "since_revision": r1, "heat_png": true }),
    );
    let d = env.expect_result("D2 diff");

    assert_eq!(d["cells_changed"], 10, "D2: exactly 10 cells");
    assert_eq!(d["regions"], f4_expected_regions(), "D2: exact regions");

    let rows = d["changed_row_text"].as_object().expect("changed_row_text");
    let keys: std::collections::BTreeSet<&String> = rows.keys().collect();
    let want: std::collections::BTreeSet<String> = ["2".to_string(), "5".to_string()].into();
    assert_eq!(
        keys,
        want.iter().collect(),
        "D2: changed rows must be exactly {{2,5}}"
    );
    assert_eq!(
        rows["2"].as_str().map(str::trim_end),
        Some(format!("{}█", " ".repeat(2)).as_str())
    );
    assert_eq!(
        rows["5"].as_str().map(str::trim_end),
        Some(format!("{}A-PRESSED", " ".repeat(10)).as_str())
    );

    // Heat PNG golden (mint per §16.3).
    use base64::Engine;
    let heat = base64::engine::general_purpose::STANDARD
        .decode(d["heat_png_base64"].as_str().expect("heat png"))
        .expect("decode heat png");
    assert_png_golden(&h, &heat, "d2_heat.png");

    h.kill_session(&f.session_id);
}

// D3 — stale & invalidated.
#[test]
fn d3_stale_and_invalidated() {
    // (a) STALE_REVISION -32010 with available:[C].
    let h = Harness::new();
    let f = h.launch_fixture("f1_static.sh", 80, 24, "दृश्यते");
    let c = checkpoint_revision(&h, &f.pane_id, "D3a checkpoint");

    let env = h.rpc_raw(
        "pane.diff_since",
        serde_json::json!({ "pane_id": f.pane_id, "since_revision": c + 1 }),
    );
    let err = env.expect_error_code(-32010, "D3a diff stale");
    assert_eq!(
        err.data.as_ref().and_then(|d| d.get("available")).cloned(),
        Some(serde_json::json!([c])),
        "D3a: STALE_REVISION must report available:[C]"
    );
    let out = h.cli(&["pane", "diff", &f.pane_id, "--since", &(c + 1).to_string()]);
    assert_eq!(out.status.code(), Some(5), "D3a: CLI exit 5 on stale");

    // (b) RESIZE_INVALIDATED -32011 after a resize (LENS-R-033 ordering).
    let c2 = checkpoint_revision(&h, &f.pane_id, "D3b checkpoint");
    h.rpc_ok(
        "pane.set_size",
        serde_json::json!({ "pane_id": f.pane_id, "cols": 100, "rows": 30 }),
    );
    let env = h.rpc_raw(
        "pane.diff_since",
        serde_json::json!({ "pane_id": f.pane_id, "since_revision": c2 }),
    );
    env.expect_error_code(-32011, "D3b diff invalidated");
    let out = h.cli(&["pane", "diff", &f.pane_id, "--since", &c2.to_string()]);
    assert_eq!(out.status.code(), Some(5), "D3b: CLI exit 5 on invalidated");

    h.kill_session(&f.session_id);
}

// D4 — style-only delta (delta 3 sequence: a -> settle -> checkpoint -> s).
#[test]
fn d4_style_only_delta() {
    let h = Harness::new();
    let f = h.launch_fixture("f4_keys.sh", 80, 24, "LENS-F4-KEYS");

    // The 10 cells must EXIST before recolouring: `a` first (`s` before `a` is
    // a no-op).
    h.send_raw(&f.pane_id, "a");
    settle(&h, &f.pane_id, "D4 settle after a");
    let rev = checkpoint_revision(&h, &f.pane_id, "D4 checkpoint");

    // `s` recolours exactly those 10 cells (identical glyphs, new fg/bg).
    h.send_raw(&f.pane_id, "s");
    settle(&h, &f.pane_id, "D4 settle after s");

    let env = h.rpc_raw(
        "pane.diff_since",
        serde_json::json!({ "pane_id": f.pane_id, "since_revision": rev }),
    );
    let d = env.expect_result("D4 diff");
    assert_eq!(
        d["cells_changed"], 10,
        "D4: style-only change counts all 10 cells"
    );
    assert_eq!(
        d["regions"],
        f4_expected_regions(),
        "D4: regions identical to D2 — style change == glyph change"
    );

    h.kill_session(&f.session_id);
}

// A1 — alt-screen semantics.
#[test]
fn a1_altscreen_semantics() {
    let h = Harness::new();
    let f = h.launch_fixture("f10_altscreen.sh", 80, 24, "LENS-F10-ALT");

    // Checkpoint on the normal screen.
    let pre_e = checkpoint_revision(&h, &f.pane_id, "A1 checkpoint normal");

    // Enter the alternate screen.
    h.send_line_token(&f.pane_id, "E");
    h.wait_for(&f.pane_id, "ALT-SCREEN", 5_000)
        .expect("A1 alt drawn");

    let env = h.rpc_raw("pane.glance", serde_json::json!({ "pane_id": f.pane_id }));
    let g = env.expect_result("A1 glance alt");
    assert_eq!(
        g["alt_screen"],
        serde_json::Value::Bool(true),
        "A1: alt_screen true"
    );
    use base64::Engine;
    let alt_png = base64::engine::general_purpose::STANDARD
        .decode(g["png_base64"].as_str().expect("alt png"))
        .expect("decode alt png");
    assert_png_golden(&h, &alt_png, "a1_alt.png");

    // The alt-screen switch invalidated the pre-E checkpoint (DEC-4).
    let env = h.rpc_raw(
        "pane.diff_since",
        serde_json::json!({ "pane_id": f.pane_id, "since_revision": pre_e }),
    );
    env.expect_error_code(-32011, "A1 diff invalidated by alt switch");

    // Leaving restores the normal-screen glance.
    h.send_line_token(&f.pane_id, "L");
    h.wait_for(&f.pane_id, "NORMAL-SCREEN", 5_000)
        .expect("A1 normal restored");
    let env = h.rpc_raw("pane.glance", serde_json::json!({ "pane_id": f.pane_id }));
    let g = env.expect_result("A1 glance normal");
    assert_eq!(g["alt_screen"], serde_json::Value::Bool(false));
    let normal_png = base64::engine::general_purpose::STANDARD
        .decode(g["png_base64"].as_str().expect("normal png"))
        .expect("decode normal png");
    assert_png_golden(&h, &normal_png, "a1_normal.png");

    h.kill_session(&f.session_id);
}
