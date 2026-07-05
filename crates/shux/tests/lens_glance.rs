//! Red suite — `pane.glance` (§5 SPEC-B; tests G1, G2, G2w from §12).
//!
//! FROZEN after P0 (§16.2). Black-box: drives only `shux rpc call` / the `shux`
//! CLI. In Phase P0 `pane.glance` is unregistered, so every test here fails at
//! its first glance call with `method_not_found (-32601)` — the red receipt.
//! Golden-backed assertions (G2/G2w) come AFTER the method call, so once glance
//! exists they fail on the missing golden until it is approved (§16.3).

mod lens_common;
use lens_common::*;

use std::sync::atomic::{AtomicBool, Ordering};

/// The character at display column `col` of glance row `row` (fixtures F1/F3
/// use single-width ASCII in these regions, so char index == column).
fn text_cell(text: &str, row: usize, col: usize) -> Option<char> {
    text.lines().nth(row).and_then(|r| r.chars().nth(col))
}

/// Frame identity from the F3 checksum column (col 79) of a glance's text:
/// frame A digits are r%10, frame B digits are (r+5)%10.
fn checksum_frame(text: &str, rows: usize) -> Option<char> {
    let mut is_a = true;
    let mut is_b = true;
    for r in 0..rows {
        let want_a = std::char::from_digit((r % 10) as u32, 10).unwrap();
        let want_b = std::char::from_digit(((r + 5) % 10) as u32, 10).unwrap();
        match text_cell(text, r, 79) {
            Some(c) if c == want_a && c != want_b => is_b = false,
            Some(c) if c == want_b && c != want_a => is_a = false,
            Some(c) if c == want_a && c == want_b => {}
            _ => {
                is_a = false;
                is_b = false;
            }
        }
    }
    match (is_a, is_b) {
        (true, false) => Some('A'),
        (false, true) => Some('B'),
        _ => None,
    }
}

// G1 ⇄ — glance atomicity.
#[test]
fn g1_glance_atomicity_under_concurrent_flips() {
    let h = Harness::new();
    let f = h.launch_fixture("f3_flip.sh", 80, 24, "AAAAAAAAAA");
    let (_, cw, ch) = h.snapshot_png(&f.pane_id);

    let stop = AtomicBool::new(false);
    let mut envelopes: Vec<RpcEnvelope> = Vec::new();

    std::thread::scope(|scope| {
        // Token pump: keep F3 flipping at max rate while glances race it.
        // Deadline-bounded so it self-terminates even if a glance thread panics
        // before `stop` is set (defensive — no infinite spin).
        let pump = scope.spawn(|| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while std::time::Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
                h.pump_line_tokens(&f.pane_id, 40);
            }
        });

        // 100 concurrent glances, each an independent `shux rpc call`.
        let handles: Vec<_> = (0..100)
            .map(|_| {
                scope.spawn(|| {
                    h.rpc_raw(
                        "pane.glance",
                        serde_json::json!({ "pane_id": f.pane_id, "include_png": true }),
                    )
                })
            })
            .collect();
        for handle in handles {
            envelopes.push(handle.join().expect("glance thread"));
        }
        stop.store(true, Ordering::Relaxed);
        pump.join().expect("pump thread");
    });

    // Every glance must be an internally-consistent single frame. The first
    // `expect_result` is the P0 red-receipt failure (-32601).
    for (i, env) in envelopes.iter().enumerate() {
        let g = env.expect_result(&format!("G1 glance #{i}"));
        let text = g["text"].as_str().expect("glance text");
        let png = {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD
                .decode(g["png_base64"].as_str().expect("glance png"))
                .expect("decode glance png")
        };

        // (a) PNG background at three cells → frame identity.
        let pa = classify_frame(probe_cell_bg(&png, 0, 0, cw, ch));
        let pb = classify_frame(probe_cell_bg(&png, 40, 12, cw, ch));
        let pc = classify_frame(probe_cell_bg(&png, 40, 23, cw, ch));
        assert!(
            pa == pb && pb == pc,
            "G1 #{i}: PNG probes disagree: {pa}{pb}{pc}"
        );

        // (b) text chars at the same three cells.
        let ta = text_cell(text, 0, 0).unwrap();
        let tb = text_cell(text, 12, 40).unwrap();
        let tc = text_cell(text, 23, 40).unwrap();
        assert!(
            ta == tb && tb == tc,
            "G1 #{i}: text cells disagree: {ta}{tb}{tc}"
        );

        // (c) checksum column matches that frame.
        let ck = checksum_frame(text, 24).expect("G1: checksum column not a clean frame");
        assert_eq!(ck, ta, "G1 #{i}: checksum frame != text frame");

        // (d) PNG identity == text identity.
        assert_eq!(
            pa, ta,
            "G1 #{i}: PNG frame {pa} != text frame {ta} (torn glance)"
        );
    }

    h.kill_session(&f.session_id);
}

// G2 ⇄ — glance fidelity (F1, 80x24).
#[test]
fn g2_glance_fidelity_f1() {
    let h = Harness::new();
    let f = h.launch_fixture("f1_static.sh", 80, 24, "दृश्यते");

    let env = h.rpc_raw(
        "pane.glance",
        serde_json::json!({ "pane_id": f.pane_id, "checkpoint": true }),
    );
    let g = env.expect_result("G2 glance rpc");

    assert_eq!(g["cols"], 80);
    assert_eq!(g["rows"], 24);
    assert_eq!(g["alt_screen"], serde_json::Value::Bool(false));
    assert_eq!(g["cursor"]["visible"], serde_json::Value::Bool(false));
    // First checkpoint on this pane evicts nothing (delta 5).
    assert_eq!(g["evicted_revision"], serde_json::Value::Null);

    // Substrate cross-check: glance.revision == session.snapshot content_revision.
    let rev = g["revision"].as_u64().expect("glance revision");
    assert_eq!(rev, h.content_revision(&f.session_id, &f.pane_id));

    // Byte-identical goldens (mint per §16.3).
    let text = g["text"].as_str().expect("glance text");
    assert_text_golden(&h, text, "g2_f1_80x24.txt");
    {
        use base64::Engine;
        let png = base64::engine::general_purpose::STANDARD
            .decode(g["png_base64"].as_str().expect("glance png"))
            .expect("decode png");
        assert_png_golden(&h, &png, "g2_f1_80x24.png");
    }

    // ⇄ CLI twin: `shux pane glance` must agree on the revision.
    let out = h.cli(&[
        "--format",
        "json",
        "pane",
        "glance",
        &f.pane_id,
        "--checkpoint",
    ]);
    let env: serde_json::Value = serde_json::from_slice(&out.stdout).expect("pane glance CLI json");
    assert_eq!(
        env["result"]["revision"].as_u64(),
        Some(rev),
        "CLI glance revision must match RPC glance revision (M9 parity)"
    );

    h.kill_session(&f.session_id);
}

// G2w ⇄ — glance fidelity, Unicode-width torture (F5, 100x30).
#[test]
fn g2w_glance_fidelity_f5_wide() {
    let h = Harness::new();
    let f = h.launch_fixture("f5_wide.sh", 100, 30, "LENS-F5-WIDE");

    let env = h.rpc_raw(
        "pane.glance",
        serde_json::json!({ "pane_id": f.pane_id, "checkpoint": true }),
    );
    let g = env.expect_result("G2w glance rpc");

    assert_eq!(g["cols"], 100);
    assert_eq!(g["rows"], 30);
    assert_eq!(g["alt_screen"], serde_json::Value::Bool(false));

    let text = g["text"].as_str().expect("glance text");
    assert_text_golden(&h, text, "g2w_f5_100x30.txt");
    {
        use base64::Engine;
        let png = base64::engine::general_purpose::STANDARD
            .decode(g["png_base64"].as_str().expect("glance png"))
            .expect("decode png");
        assert_png_golden(&h, &png, "g2w_f5_100x30.png");
    }

    h.kill_session(&f.session_id);
}
