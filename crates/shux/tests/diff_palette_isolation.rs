//! Task 079 (design R7 / council #8) — the `pane.diff_since` daemon adapter must
//! stay byte-shaped-identical after the `compute_lens_diff` → `shux_vt::diff_frames`
//! extraction, EVEN on a pane that has applied an OSC 4 palette override. The
//! daemon hardcodes `palette_overridden = false` on both `GridFrame`s (the
//! checkpoint never stored palette history and the RPC has no palette field), so
//! `palette_overridden_differs` / `geometry_changed` are never computed into the
//! response — and OSC 4 (which changes no cells and does not bump ContentRevision,
//! task 078 R1) must not inflate the diff.
//!
//! Black-box: a REAL `sh` pane, a REAL OSC 4 SET through the PTY. NOT part of the
//! frozen lens red suite (§16.2 freezes `lens_*`); reuses the frozen `lens_common`
//! harness READ-ONLY via `#[path]`.

#[path = "lens_common/mod.rs"]
mod lens_common;

use lens_common::{Harness, unique};

fn settle(h: &Harness, pane: &str, ctx: &str) {
    let env = h.rpc_raw(
        "pane.wait_settled",
        serde_json::json!({ "pane_id": pane, "quiet_ms": 300, "timeout_ms": 5_000 }),
    );
    assert_eq!(
        env.expect_result(ctx)["settled"],
        serde_json::Value::Bool(true),
        "{ctx}: expected settle"
    );
}

#[test]
fn diff_since_has_no_palette_field_and_osc4_does_not_inflate() {
    let h = Harness::new();
    let session_name = format!("palette-iso-{}", unique());
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
        serde_json::json!({ "pane_id": pane_id, "cols": 40, "rows": 10 }),
    );

    // Absolute-positioned writes only (no prompt to scroll rows). Token `P`
    // emits a REAL OSC 4 SET (redefine palette index 1) then a visible sentinel
    // "PALMARK" at row 8 from the SAME write — when PALMARK is on screen the OSC 4
    // bytes are guaranteed parsed (single ordered byte stream).
    let script = concat!(
        "exec sh -c 'stty -echo; ",
        "printf \"\\033[2J\\033[3J\\033[H\"; ",
        "printf \"\\033[1;1HREADY-PAL\"; ",
        "while read -r t; do ",
        "if [ \"$t\" = P ]; then ",
        "printf \"\\033]4;1;#00ff00\\007\"; printf \"\\033[8;1HPALMARK\"; ",
        "fi; done'\n",
    );
    h.rpc_ok(
        "pane.send_keys",
        serde_json::json!({ "pane_id": pane_id, "text": script }),
    );
    h.wait_for(&pane_id, "READY-PAL", 10_000)
        .expect("setup sentinel");
    settle(&h, &pane_id, "settle after setup");

    // Checkpoint BEFORE the OSC 4 (palette not yet overridden on the checkpoint).
    let env = h.rpc_raw("pane.checkpoint", serde_json::json!({ "pane_id": pane_id }));
    let rev = env.expect_result("checkpoint")["revision"]
        .as_u64()
        .expect("checkpoint revision");

    // Apply the OSC 4 override + the PALMARK sentinel.
    h.send_line_token(&pane_id, "P");
    h.wait_for(&pane_id, "PALMARK", 10_000)
        .expect("OSC 4 landed");
    settle(&h, &pane_id, "settle after OSC 4");

    let d = h
        .rpc_raw(
            "pane.diff_since",
            serde_json::json!({ "pane_id": pane_id, "since_revision": rev }),
        )
        .expect_result("diff after OSC 4");

    // (1) OSC 4 changed NO cells — only the 7 PALMARK glyphs at row 8 differ. A
    // palette override must not repaint the grid the way a default-COLOUR change
    // (OSC 11) does; the diagnostic is per-frame history, never a cell delta.
    assert_eq!(
        d["cells_changed"], 7,
        "only PALMARK (7 cells) changed; OSC 4 did not inflate the diff"
    );
    assert_eq!(
        d["regions"],
        serde_json::json!([{ "row": 7, "col_start": 0, "col_end": 7 }]),
        "exactly the PALMARK span"
    );

    // (2) The response object carries the EXACT pre-refactor field set — no
    // `palette`, `palette_overridden_differs`, or `geometry_changed` leaked into
    // the RPC shape.
    let obj = d.as_object().expect("diff is an object");
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "bounding_box",
            "cells_changed",
            "changed_row_text",
            "cursor_moved",
            "from_revision",
            "heat_png_base64",
            "regions",
            "regions_truncated",
            "to_revision",
        ],
        "pane.diff_since must expose the exact pre-refactor field set (no palette field)"
    );

    h.kill_session(&session_id);
}
