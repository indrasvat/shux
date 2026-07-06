//! Red suite — checkpoints + `pane.diff_since` (§7 SPEC-D; D1–D5, A1 from §12).
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

/// Zero-delta diff assertions shared by the D1 RPC and CLI twins.
fn assert_zero_delta(d: &serde_json::Value, rev: u64, ctx: &str) {
    assert_eq!(d["cells_changed"], 0, "{ctx}: cells_changed");
    assert_eq!(d["regions"], serde_json::json!([]), "{ctx}: regions");
    assert_eq!(
        d["changed_row_text"],
        serde_json::json!({}),
        "{ctx}: row text"
    );
    assert_eq!(
        d["cursor_moved"],
        serde_json::Value::Bool(false),
        "{ctx}: cursor"
    );
    assert_eq!(
        d["regions_truncated"],
        serde_json::Value::Bool(false),
        "{ctx}: truncated"
    );
    // p0-council-r1 minor 15: from/to revision fields.
    assert_eq!(d["from_revision"], rev, "{ctx}: from_revision");
    assert_eq!(d["to_revision"], rev, "{ctx}: to_revision (no mutations)");
    // Zero-delta bounding box is all zeros (delta 5).
    assert_eq!(d["bounding_box"]["row_start"], 0, "{ctx}: bbox");
    assert_eq!(d["bounding_box"]["col_start"], 0, "{ctx}: bbox");
    assert_eq!(d["bounding_box"]["row_end"], 0, "{ctx}: bbox");
    assert_eq!(d["bounding_box"]["col_end"], 0, "{ctx}: bbox");
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
    let d = env.expect_result("D1 diff rpc");
    assert_zero_delta(&d, rev, "D1 rpc");

    // ⇄ CLI twin, successful path (p0-council-r1 major 3): same zero delta,
    // exit 0 (diff is data, not a verdict — §10).
    let cli = h.cli_envelope(&["pane", "diff", &f.pane_id, "--since", &rev.to_string()]);
    let cd = cli.expect_result("D1 diff cli");
    assert_zero_delta(&cd, rev, "D1 cli");
    assert_eq!(cli.exit_code, 0, "D1: CLI exit 0 on zero delta");

    h.kill_session(&f.session_id);
}

/// The exact FULL-WIDTH (80-cell) new text of F4's changed rows after `a`
/// (p0-council-r1 major 4 / LENS-R-036: byte-stable full-width rows,
/// trailing cells included — NO trim).
fn f4_expected_row2() -> String {
    let mut s = format!("{}█", " ".repeat(2));
    while s.chars().count() < 80 {
        s.push(' ');
    }
    s
}

fn f4_expected_row5() -> String {
    let mut s = format!("{}A-PRESSED", " ".repeat(10));
    while s.chars().count() < 80 {
        s.push(' ');
    }
    s
}

/// Exact-delta diff assertions shared by the D2 RPC and CLI twins.
fn assert_f4_exact_delta(d: &serde_json::Value, since: u64, ctx: &str) {
    assert_eq!(d["cells_changed"], 10, "{ctx}: exactly 10 cells");
    assert_eq!(d["regions"], f4_expected_regions(), "{ctx}: exact regions");
    // p0-council-r1 minor 15: from/to revision fields.
    assert_eq!(d["from_revision"], since, "{ctx}: from_revision");
    assert!(
        d["to_revision"].as_u64().is_some_and(|t| t > since),
        "{ctx}: to_revision must exceed since_revision after `a`"
    );

    let rows = d["changed_row_text"].as_object().expect("changed_row_text");
    let keys: std::collections::BTreeSet<&String> = rows.keys().collect();
    let want: std::collections::BTreeSet<String> = ["2".to_string(), "5".to_string()].into();
    assert_eq!(
        keys,
        want.iter().collect(),
        "{ctx}: changed rows must be exactly {{2,5}}"
    );
    // Byte-exact FULL-WIDTH rows (major 4): trailing cells preserved, row
    // width == pane width.
    for (key, expected) in [("2", f4_expected_row2()), ("5", f4_expected_row5())] {
        let actual = rows[key].as_str().expect("row text");
        assert_eq!(
            actual.chars().count(),
            80,
            "{ctx}: changed_row_text[{key}] must be full pane width (80 cells)"
        );
        assert_eq!(
            actual, expected,
            "{ctx}: changed_row_text[{key}] byte-exact"
        );
    }
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
    let d = env.expect_result("D2 diff rpc");
    assert_f4_exact_delta(&d, r1, "D2 rpc");

    // Heat PNG golden (mint per §16.3).
    use base64::Engine;
    let heat = base64::engine::general_purpose::STANDARD
        .decode(d["heat_png_base64"].as_str().expect("heat png"))
        .expect("decode heat png");
    assert_png_golden(&h, &heat, "d2_heat.png");

    // ⇄ CLI twin, successful path (p0-council-r1 major 3): the pane is still,
    // so the CLI diff against the same checkpoint reports the same exact delta.
    let cli = h.cli_envelope(&["pane", "diff", &f.pane_id, "--since", &r1.to_string()]);
    let cd = cli.expect_result("D2 diff cli");
    assert_f4_exact_delta(&cd, r1, "D2 cli");
    assert_eq!(
        cli.exit_code, 0,
        "D2: CLI exit 0 (diff is data, not a verdict)"
    );

    // CLI heat-file surface: `--heat <path>` writes the heat PNG.
    let heat_path = std::env::temp_dir().join(format!("lens_d2_heat_{}.png", unique()));
    let out = h.cli(&[
        "pane",
        "diff",
        &f.pane_id,
        "--since",
        &r1.to_string(),
        "--heat",
        heat_path.to_str().expect("tmp path utf8"),
    ]);
    assert_eq!(out.status.code(), Some(0), "D2: CLI --heat exit 0");
    let written = std::fs::read(&heat_path)
        .unwrap_or_else(|e| panic!("D2: CLI --heat did not write {}: {e}", heat_path.display()));
    assert_png_golden(&h, &written, "d2_heat.png");

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
    // CLI twin (p0-council-r1 major 3): exit 5 AND the json-format error
    // envelope carries the same code (§10: json emits the raw RPC envelope).
    let cli = h.cli_envelope(&["pane", "diff", &f.pane_id, "--since", &(c + 1).to_string()]);
    cli.expect_error_code(-32010, "D3a cli stale envelope");
    assert_eq!(cli.exit_code, 5, "D3a: CLI exit 5 on stale");

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
    let cli = h.cli_envelope(&["pane", "diff", &f.pane_id, "--since", &c2.to_string()]);
    cli.expect_error_code(-32011, "D3b cli invalidated envelope");
    assert_eq!(cli.exit_code, 5, "D3b: CLI exit 5 on invalidated");

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
    // p0-council-r1 minor 15: from/to revision fields.
    assert_eq!(d["from_revision"], rev, "D4: from_revision");
    assert!(
        d["to_revision"].as_u64().is_some_and(|t| t > rev),
        "D4: to_revision must exceed the checkpoint after `s`"
    );

    h.kill_session(&f.session_id);
}

// D5 — checkpoint FIFO eviction + same-revision no-op (LENS-R-030/031, DEC-22;
// p0-council-r1 major 7).
#[test]
fn d5_checkpoint_fifo_eviction_and_noop() {
    let h = Harness::new();
    let f = h.launch_fixture("f8_repaint.sh", 80, 24, "LENS-F8-REPAINT");

    // Five checkpoints at five DISTINCT revisions (token + wait_for between).
    let glyphs = b"0123456789";
    let mut revs: Vec<u64> = Vec::new();
    let mut evictions: Vec<Option<u64>> = Vec::new();
    for i in 0..5 {
        if i > 0 {
            h.send_line_token(&f.pane_id, "");
            h.wait_for(
                &f.pane_id,
                &format!("FRAME:{}", glyphs[i - 1] as char),
                5_000,
            )
            .unwrap_or_else(|e| panic!("D5: repaint {i} never landed: {e}"));
        }
        let env = h.rpc_raw(
            "pane.checkpoint",
            serde_json::json!({ "pane_id": f.pane_id }),
        );
        let r = env.expect_result(&format!("D5 checkpoint #{i}"));
        revs.push(r["revision"].as_u64().expect("checkpoint revision"));
        evictions.push(r["evicted_revision"].as_u64());
    }
    let uniq: std::collections::BTreeSet<&u64> = revs.iter().collect();
    assert_eq!(
        uniq.len(),
        5,
        "D5: five checkpoints at five DISTINCT revisions"
    );

    // First four evict nothing; the 5th evicts the FIFO-oldest (the 1st).
    for (i, ev) in evictions.iter().take(4).enumerate() {
        assert_eq!(*ev, None, "D5: checkpoint #{i} must evict nothing");
    }
    assert_eq!(
        evictions[4],
        Some(revs[0]),
        "D5: the 5th checkpoint must evict the FIFO-oldest (the 1st)"
    );

    // Diff against the evicted 1st → STALE_REVISION with exactly the 4 live.
    let env = h.rpc_raw(
        "pane.diff_since",
        serde_json::json!({ "pane_id": f.pane_id, "since_revision": revs[0] }),
    );
    let err = env.expect_error_code(-32010, "D5 diff against evicted");
    assert_eq!(
        err.data.as_ref().and_then(|d| d.get("available")).cloned(),
        Some(serde_json::json!([revs[1], revs[2], revs[3], revs[4]])),
        "D5: available must list exactly the 4 live revisions"
    );

    // Re-checkpoint at the CURRENT revision with no intervening mutation:
    // no-op — same revision back, nothing evicted (LENS-R-030).
    let env = h.rpc_raw(
        "pane.checkpoint",
        serde_json::json!({ "pane_id": f.pane_id }),
    );
    let r = env.expect_result("D5 re-checkpoint no-op");
    assert_eq!(
        r["revision"].as_u64(),
        Some(revs[4]),
        "D5: same-revision re-checkpoint returns the same revision"
    );
    assert_eq!(
        r["evicted_revision"],
        serde_json::Value::Null,
        "D5: same-revision re-checkpoint evicts nothing"
    );

    // The same 4 revisions are still diffable (minor 15: from_revision agrees).
    for want in [revs[1], revs[2], revs[3], revs[4]] {
        let env = h.rpc_raw(
            "pane.diff_since",
            serde_json::json!({ "pane_id": f.pane_id, "since_revision": want }),
        );
        let d = env.expect_result(&format!("D5 diff since {want}"));
        assert_eq!(
            d["from_revision"], want,
            "D5: from_revision for live checkpoint"
        );
    }

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
