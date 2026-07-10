//! PR #91 codex P2, adjudicated as LENS-R-038b (task 077) — OSC 10/11/12
//! default-color changes bump ContentRevision (P2 presented-frame ruling),
//! so `pane.diff_since` must see them: the checkpoint captures the pane's
//! defaults alongside the grid and the diff resolves `Color::Default`
//! against EACH side's respective defaults. A default-color-only repaint
//! marks every Default-colored cell changed; concrete-colored cells stay
//! unmarked; unchanged defaults keep the comparison byte-identical to plain
//! cell equality (pinned at unit level by
//! `compute_lens_diff_unchanged_defaults_matches_raw` and end-to-end by the
//! frozen D-tier, whose F-fixtures never touch defaults).
//!
//! This is the black-box half: a REAL `sh` pane, a REAL OSC 11 sequence
//! through the PTY, and pixel probes proving the heat base renders with the
//! pane's CURRENT defaults (test c).
//!
//! NOT part of the frozen lens red suite (§16.2 freezes `lens_*` files).
//! Reuses the frozen `lens_common` harness READ-ONLY via `#[path]`.

#[path = "lens_common/mod.rs"]
mod lens_common;

use lens_common::{Harness, decode_png, probe_cell_bg_img, unique};

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
fn osc_default_bg_change_marks_default_cells_and_heat_uses_current_defaults() {
    let h = Harness::new();

    // An INLINE token-paced script `exec`'d over the pane shell (the frozen
    // fixtures' pattern — no shell prompt exists after exec, so no prompt
    // wrap/scroll can shift rows; every write is absolute-positioned). The
    // echoed exec line itself scrolls, then the script clears screen +
    // scrollback. Setup frame: one truecolor-fg+bg word (house color rule —
    // and the negative control: concrete-colored cells must NOT be marked)
    // + the wait_for sentinel. Token `O` emits the REAL OSC 11 followed by
    // a visible sentinel from the SAME write. EOF-safe: `read` fails on
    // PTY close and the loop exits (no busy spin).
    let session_name = format!("osc-defaults-{}", unique());
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
    let script = concat!(
        "exec sh -c 'stty -echo; ",
        "printf \"\\033[2J\\033[3J\\033[H\"; ",
        "printf \"\\033[5;3H\\033[48;2;30;120;60m\\033[38;2;255;255;0mCOLOR\\033[0m\"; ",
        "printf \"\\033[1;1HREADY-038B\"; ",
        "while read -r t; do ",
        "if [ \"$t\" = O ]; then ",
        "printf \"\\033]11;#204060\\007\"; printf \"\\033[10;1HOSC-DONE\"; ",
        "fi; done'\n",
    );
    h.rpc_ok(
        "pane.send_keys",
        serde_json::json!({ "pane_id": pane_id, "text": script }),
    );
    h.wait_for(&pane_id, "READY-038B", 10_000)
        .expect("setup sentinel");
    settle(&h, &pane_id, "settle after setup");

    // Checkpoint (captures the pane's CURRENT defaults: none set yet).
    let env = h.rpc_raw("pane.checkpoint", serde_json::json!({ "pane_id": pane_id }));
    let r = env.expect_result("checkpoint before OSC 11")["revision"]
        .as_u64()
        .expect("checkpoint revision");

    // Pin (b) inline: nothing changed and defaults are equal → zero delta.
    let d = h
        .rpc_raw(
            "pane.diff_since",
            serde_json::json!({ "pane_id": pane_id, "since_revision": r }),
        )
        .expect_result("pre-OSC zero diff");
    assert_eq!(
        d["cells_changed"], 0,
        "unchanged defaults + unchanged cells"
    );

    // Token `O`: the REAL OSC 11 through the PTY, followed by a visible
    // sentinel from the SAME write — when OSC-DONE is on screen the earlier
    // OSC bytes are guaranteed parsed (single ordered byte stream).
    h.send_line_token(&pane_id, "O");
    h.wait_for(&pane_id, "OSC-DONE", 10_000)
        .expect("OSC landed");
    settle(&h, &pane_id, "settle after OSC 11");

    // LENS-R-038b (a): the bg default changed, so EVERY cell whose bg is
    // Default on both sides is marked — the whole 40×10 grid except the 5
    // concrete-bg COLOR cells at grid (4, 2..7). The OSC-DONE sentinel's
    // raw cell changes (row 9, default-colored) are absorbed by the same
    // rule, so the expected shape is exact.
    let d = h
        .rpc_raw(
            "pane.diff_since",
            serde_json::json!({ "pane_id": pane_id, "since_revision": r, "heat_png": true }),
        )
        .expect_result("diff after OSC 11");
    assert_eq!(
        d["cells_changed"],
        40 * 10 - 5,
        "every Default-bg cell marked; the 5 concrete-bg cells NOT marked"
    );
    let mut want_regions: Vec<serde_json::Value> = Vec::new();
    for row in 0..10 {
        if row == 4 {
            want_regions.push(serde_json::json!({ "row": 4, "col_start": 0, "col_end": 2 }));
            want_regions.push(serde_json::json!({ "row": 4, "col_start": 7, "col_end": 40 }));
        } else {
            want_regions.push(serde_json::json!({ "row": row, "col_start": 0, "col_end": 40 }));
        }
    }
    assert_eq!(
        d["regions"],
        serde_json::Value::Array(want_regions),
        "full-width spans everywhere, split around the concrete COLOR cells"
    );
    assert_eq!(d["bounding_box"]["row_start"], 0);
    assert_eq!(d["bounding_box"]["col_start"], 0);
    assert_eq!(d["bounding_box"]["row_end"], 10);
    assert_eq!(d["bounding_box"]["col_end"], 40);
    assert_eq!(d["from_revision"], r);

    // LENS-R-038b (c): the heat BASE renders with the pane's CURRENT
    // defaults (bg #204060 = (32,64,96)), never the checkpoint's. Exact
    // integer expectations (same math as the heat unit tests):
    //  - changed blank cell: heat(163,38,56)@α128 over (32,64,96) → (97,50,75)
    //  - unchanged COLOR cell: desat50((30,120,60)) → (58,103,73)
    // Were the base rendered with the CHECKPOINT defaults (builtin dark bg),
    // the changed-cell probe would differ — pinning current-defaults use.
    use base64::Engine;
    let heat = base64::engine::general_purpose::STANDARD
        .decode(d["heat_png_base64"].as_str().expect("heat png"))
        .expect("decode heat png");
    let img = decode_png(&heat);
    let (_snap_png, cw, ch) = h.snapshot_png(&pane_id);

    let changed_blank = probe_cell_bg_img(&img, 20, 8, cw, ch); // grid (8,20)
    assert_eq!(
        (changed_blank.0, changed_blank.1, changed_blank.2),
        (97, 50, 75),
        "changed cell = heat over the CURRENT (new) default bg"
    );
    let unchanged_color = probe_cell_bg_img(&img, 3, 4, cw, ch); // grid (4,3)
    assert_eq!(
        (unchanged_color.0, unchanged_color.1, unchanged_color.2),
        (58, 103, 73),
        "unchanged concrete-bg cell = desaturated truecolor bg"
    );

    h.kill_session(&session_id);
}
