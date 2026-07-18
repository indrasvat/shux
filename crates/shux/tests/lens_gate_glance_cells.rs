//! Task 080 — daemon-backed `pane.glance --cells` capture emission (GATE lane;
//! `GATE-TEST-CHANGE:` to touch). `test = false` → run via `make
//! test-lens-gate-glance-cells` under the leak guard, serially.
//!
//! Proves the L1-capture + L3-dogfood matrix rows: `pane.glance {include_cells:true}`
//! (RPC + `--cells` CLI) emits the canonical task-078 `FrameEnvelope` for the live
//! viewport — it validates, round-trips, carries the real colours, and matches the pane
//! geometry at both dogfood viewports (80×24 and 120×40). Masks redact the emitted
//! `cells` AND `text` so a secret never leaves the daemon (council D4). Leaves no daemon
//! (the leak guard enforces it; the test kills its sessions).

mod lens_common;
use lens_common::*;

use shux_vt::{FrameEnvelope, MaskSet};

/// Create a bare pane at `cols`×`rows`, returning `(session_id, pane_id)`.
fn open_pane(h: &Harness, cols: u16, rows: u16) -> (String, String) {
    let created = h.rpc_ok(
        "session.create",
        serde_json::json!({
            "name": format!("gate-cells-{}", unique()),
            "cwd": h.repo_root().display().to_string(),
        }),
    );
    let session_id = created["id"].as_str().expect("session id").to_string();
    let pane_id = created["pane_id"].as_str().expect("pane id").to_string();
    h.rpc_ok(
        "pane.set_size",
        serde_json::json!({ "pane_id": pane_id, "cols": cols, "rows": rows }),
    );
    (session_id, pane_id)
}

/// Draw a truecolor line + a sentinel and wait for it (a colour probe is mandatory in
/// shux automation — CLAUDE.md).
fn draw_colored(h: &Harness, pane_id: &str, sentinel: &str) {
    h.rpc_ok(
        "pane.send_keys",
        serde_json::json!({
            "pane_id": pane_id,
            // truecolor fg + indexed bg so a monochrome regression can't pass unnoticed.
            "text": format!("printf '\\033[38;2;255;120;0m\\033[48;5;28mTRUECOLOR\\033[0m {sentinel}\\n'\n"),
        }),
    );
    h.wait_for(pane_id, sentinel, 10_000)
        .unwrap_or_else(|e| panic!("pane never drew {sentinel:?}: {e}"));
}

/// Parse + validate the `cells` field of a glance result into a canonical envelope.
fn cells_envelope(g: &serde_json::Value, ctx: &str) -> FrameEnvelope {
    let cells = g
        .get("cells")
        .unwrap_or_else(|| panic!("{ctx}: no cells field"));
    let env: FrameEnvelope =
        serde_json::from_value(cells.clone()).unwrap_or_else(|e| panic!("{ctx}: cells parse: {e}"));
    env.validate()
        .unwrap_or_else(|e| panic!("{ctx}: emitted cells are not canonical: {e:?}"));
    // Round-trip: the canonical JSON re-parses to the same envelope, byte-stable.
    let json = env.to_canonical_json();
    let back = FrameEnvelope::from_canonical_json(&json)
        .unwrap_or_else(|e| panic!("{ctx}: canonical re-parse: {e:?}"));
    assert_eq!(env, back, "{ctx}: cells envelope must round-trip");
    env
}

// ── L1 capture + L3 dogfood: --cells emits the canonical envelope at both viewports ──
#[test]
fn glance_cells_emits_canonical_envelope_rpc_and_cli() {
    let h = Harness::new();
    for (cols, rows) in [(80u16, 24u16), (120, 40)] {
        let (session_id, pane_id) = open_pane(&h, cols, rows);
        let sentinel = format!("READY{cols}");
        draw_colored(&h, &pane_id, &sentinel);

        // RPC path.
        let env = h.rpc_raw(
            "pane.glance",
            serde_json::json!({ "pane_id": pane_id, "include_cells": true }),
        );
        let g = env.expect_result("glance include_cells rpc");
        let fe = cells_envelope(&g, "rpc");
        assert_eq!(fe.size.cols, cols, "cells cols match the pane");
        assert_eq!(fe.size.rows, rows, "cells rows match the pane");
        // The truecolor fg (255,120,0) must appear in the emitted golden (colour survives).
        let json = fe.to_canonical_json();
        assert!(
            json.contains("255") && json.contains("120"),
            "the truecolor probe must be captured in the golden"
        );
        // Default response (no include_cells) must NOT carry a cells field (frozen shape).
        let plain = h
            .rpc_raw("pane.glance", serde_json::json!({ "pane_id": pane_id }))
            .expect_result("plain glance");
        assert!(
            plain.get("cells").is_none(),
            "cells absent unless requested"
        );

        // CLI twin: `--cells --format json` carries the same envelope shape.
        let cli = h.cli_envelope(&["pane", "glance", &pane_id, "--cells"]);
        let cg = cli.expect_result("glance --cells cli");
        let cfe = cells_envelope(&cg, "cli");
        assert_eq!(cfe.size.cols, cols, "CLI cells cols parity");
        assert_eq!(cfe.size.rows, rows, "CLI cells rows parity");

        // CLI `--cells-out <path>` writes the canonical JSON to disk.
        let out_path = std::env::temp_dir().join(format!("gate_cells_{}.json", unique()));
        let out = h.cli(&[
            "pane",
            "glance",
            &pane_id,
            "--cells-out",
            out_path.to_str().unwrap(),
        ]);
        assert_eq!(out.status.code(), Some(0), "--cells-out exits 0");
        let written = std::fs::read_to_string(&out_path).expect("cells-out file");
        let disk = FrameEnvelope::from_canonical_json(written.trim())
            .expect("cells-out file is canonical");
        disk.validate().expect("cells-out envelope validates");
        let _ = std::fs::remove_file(&out_path);

        h.kill_session(&session_id);
    }
}

// ── L1 mask/redact absence (D4): --mask redacts the emitted cells AND text ──────────
#[test]
fn glance_cells_masks_redact_emitted_content() {
    let h = Harness::new();
    let (session_id, pane_id) = open_pane(&h, 80, 24);
    // Clear the screen (wipes the command echo, which also contains the secret) and
    // position the secret at a KNOWN cell: row 5 col 0 (`SECRET-TOKEN-42`, 15 cols), with
    // an unmasked tail at col 19 and the sentinel low enough to avoid a scroll.
    h.rpc_ok(
        "pane.send_keys",
        serde_json::json!({
            "pane_id": pane_id,
            "text": "printf '\\033[2J\\033[6;1HSECRET-TOKEN-42\\033[6;20Hvisible-tail\\033[9;1HREADYSEC\\n'\n",
        }),
    );
    h.wait_for(&pane_id, "READYSEC", 10_000).expect("sentinel");

    // Mask the 15 columns of row 5 that carry the secret.
    let masks = serde_json::json!([{ "row": 5, "col": 0, "width": 15 }]);
    let g = h
        .rpc_raw(
            "pane.glance",
            serde_json::json!({ "pane_id": pane_id, "include_cells": true, "masks": masks }),
        )
        .expect_result("masked glance");

    // The secret must not appear in cells OR text (D4). The tail stays visible.
    let cells_json = serde_json::to_string(g.get("cells").expect("cells")).unwrap();
    assert!(
        !cells_json.contains("SECRET"),
        "masked secret leaked into cells"
    );
    assert!(
        cells_json.contains("mask"),
        "a structural mask run is present"
    );
    let text = g["text"].as_str().expect("text");
    assert!(
        !text.contains("SECRET"),
        "masked secret leaked into text (D4)"
    );
    assert!(text.contains("visible-tail"), "unmasked tail survives");

    // The emitted cells still validate + round-trip with the mask applied.
    let fe = cells_envelope(&g, "masked");
    assert_eq!(fe.size.cols, 80);

    // A malformed masks param is INVALID_PARAMS (exit-mapped by the CLI), never a silent
    // skip that would leave the secret unredacted.
    let bad = h.rpc_raw(
        "pane.glance",
        serde_json::json!({ "pane_id": pane_id, "include_cells": true, "masks": "not-an-array" }),
    );
    bad.expect_error_code(-32602, "masks must be an array");
    // A zero-width mask must FAIL closed (impl-review MAJOR) — silently dropping it would
    // turn an intended redaction into an unmasked glance.
    let zero = h.rpc_raw(
        "pane.glance",
        serde_json::json!({
            "pane_id": pane_id,
            "masks": [{ "row": 0, "col": 0, "width": 0 }],
        }),
    );
    zero.expect_error_code(-32602, "zero-width mask rejected");

    // BLOCKER (impl-review): the RESPONSE cursor must be clamped when it falls inside a
    // mask — else the reported column leaks a masked secret's length. Learn the live
    // cursor, mask a window straddling it, and assert the reported col snaps to the mask
    // origin (which differs from the true column).
    let plain = h
        .rpc_raw("pane.glance", serde_json::json!({ "pane_id": pane_id }))
        .expect_result("cursor probe");
    let crow = plain["cursor"]["row"].as_u64().unwrap();
    let ccol = plain["cursor"]["col"].as_u64().unwrap();
    let mcol = ccol.saturating_sub(2); // mask origin strictly left of the true cursor
    let g2 = h
        .rpc_raw(
            "pane.glance",
            serde_json::json!({
                "pane_id": pane_id,
                "masks": [{ "row": crow, "col": mcol, "width": 5 }],
            }),
        )
        .expect_result("cursor-in-mask glance");
    assert_eq!(
        g2["cursor"]["col"].as_u64().unwrap(),
        mcol,
        "a cursor inside a mask must be reported at the mask origin, not its true column"
    );

    h.kill_session(&session_id);
    // Sanity: build a masked envelope offline and confirm the same redaction invariant
    // holds (defence in depth against a daemon-only redaction bug).
    let mut vt = shux_vt::VirtualTerminal::new(2, 40);
    vt.process(b"SECRET-TOKEN-42 tail");
    let off = FrameEnvelope::from_terminal(&vt, &MaskSet::new().with(0, 0, 15));
    assert!(!off.to_canonical_json().contains("SECRET"));
}
