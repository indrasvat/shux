//! Red suite — `pane.glance` (§5 SPEC-B; tests G1, G2, G2w from §12).
//!
//! FROZEN after P0 (§16.2). Black-box: drives only `shux rpc call` / the `shux`
//! CLI. In Phase P0 `pane.glance` is unregistered, so every test here fails at
//! its first glance call with `method_not_found (-32601)` (RPC) or the missing
//! `pane glance` CLI verb — the red receipt. Golden-backed assertions (G2/G2w)
//! come AFTER the method call, so once glance exists they fail on the missing
//! golden until it is approved (§16.3).

mod lens_common;
use lens_common::*;

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

/// Assert one glance envelope is an internally-consistent single F3 frame.
/// Decodes the PNG exactly once (p0-council-r1 minor 14) and validates probe
/// colors exactly (minor 13).
fn assert_untorn_f3_glance(env: &RpcEnvelope, cw: u32, ch: u32, ctx: &str) {
    let g = env.expect_result(ctx);
    let text = g["text"].as_str().expect("glance text");
    let png = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(g["png_base64"].as_str().expect("glance png"))
            .expect("decode glance png")
    };
    let img = decode_png(&png);

    // (a) PNG background at three cells → frame identity (exact colors).
    let pa = classify_frame_exact(probe_cell_bg_img(&img, 0, 0, cw, ch), 0);
    let pb = classify_frame_exact(probe_cell_bg_img(&img, 40, 12, cw, ch), 12);
    let pc = classify_frame_exact(probe_cell_bg_img(&img, 40, 23, cw, ch), 23);
    assert!(
        pa == pb && pb == pc,
        "{ctx}: PNG probes disagree: {pa}{pb}{pc}"
    );

    // (b) text chars at the same three cells.
    let ta = text_cell(text, 0, 0).unwrap();
    let tb = text_cell(text, 12, 40).unwrap();
    let tc = text_cell(text, 23, 40).unwrap();
    assert!(
        ta == tb && tb == tc,
        "{ctx}: text cells disagree: {ta}{tb}{tc}"
    );

    // (c) checksum column matches that frame.
    let ck = checksum_frame(text, 24).expect("checksum column not a clean frame");
    assert_eq!(ck, ta, "{ctx}: checksum frame != text frame");

    // (d) PNG identity == text identity.
    assert_eq!(
        pa, ta,
        "{ctx}: PNG frame {pa} != text frame {ta} (torn glance)"
    );
}

// G1 ⇄ — glance atomicity (50 RPC + 50 CLI glances race the pump; M9 parity).
#[test]
fn g1_glance_atomicity_under_concurrent_flips() {
    let h = Harness::new();
    let f = h.launch_fixture("f3_flip.sh", 80, 24, "AAAAAAAAAA");
    let (_, cw, ch) = h.snapshot_png(&f.pane_id);

    let mut rpc_envelopes: Vec<RpcEnvelope> = Vec::new();
    let mut cli_envelopes: Vec<RpcEnvelope> = Vec::new();

    std::thread::scope(|scope| {
        // Token pump: exactly 200 tokens at max rate (§12 G1 — bounded by
        // construction; each 5-token batch is a tiny PTY write, so the pump can
        // never wedge on a full input buffer and always terminates on its own,
        // even if a glance thread panics mid-scope).
        let pump = scope.spawn(|| {
            for _ in 0..40 {
                h.pump_line_tokens(&f.pane_id, 5);
            }
        });

        // 100 concurrent glances — 50 via `shux rpc call`, 50 via the CLI verb
        // (p0-council-r1 major 3: full CLI/RPC parity under concurrency).
        let rpc_handles: Vec<_> = (0..50)
            .map(|_| {
                scope.spawn(|| {
                    h.rpc_raw(
                        "pane.glance",
                        serde_json::json!({ "pane_id": f.pane_id, "include_png": true }),
                    )
                })
            })
            .collect();
        let cli_handles: Vec<_> = (0..50)
            .map(|_| scope.spawn(|| h.cli_envelope(&["pane", "glance", &f.pane_id])))
            .collect();
        for handle in rpc_handles {
            rpc_envelopes.push(handle.join().expect("rpc glance thread"));
        }
        for handle in cli_handles {
            cli_envelopes.push(handle.join().expect("cli glance thread"));
        }
        pump.join().expect("pump thread");
    });

    // Every glance (both paths) must be an internally-consistent single frame.
    // The first `expect_result` is the P0 red-receipt failure (-32601).
    for (i, env) in rpc_envelopes.iter().enumerate() {
        assert_untorn_f3_glance(env, cw, ch, &format!("G1 rpc glance #{i}"));
    }
    for (i, env) in cli_envelopes.iter().enumerate() {
        assert_untorn_f3_glance(env, cw, ch, &format!("G1 cli glance #{i}"));
    }

    h.kill_session(&f.session_id);
}

/// Shared field assertions for a G2/G2w glance result (p0-council-r1 major 6).
fn assert_glance_fields(
    g: &serde_json::Value,
    cols: u64,
    rows: u64,
    cursor: (u64, u64),
    ctx: &str,
) {
    assert_eq!(g["cols"], cols, "{ctx}: cols");
    assert_eq!(g["rows"], rows, "{ctx}: rows");
    assert_eq!(
        g["alt_screen"],
        serde_json::Value::Bool(false),
        "{ctx}: alt_screen"
    );
    assert_eq!(g["cursor"]["row"], cursor.0, "{ctx}: cursor.row");
    assert_eq!(g["cursor"]["col"], cursor.1, "{ctx}: cursor.col");
    assert_eq!(
        g["cursor"]["visible"],
        serde_json::Value::Bool(false),
        "{ctx}: cursor.visible"
    );
    assert_eq!(
        g["checkpointed"],
        serde_json::Value::Bool(true),
        "{ctx}: checkpoint:true must report checkpointed"
    );
    assert_eq!(
        g["evicted_revision"],
        serde_json::Value::Null,
        "{ctx}: early checkpoints must evict nothing"
    );
    assert!(
        g["text"].as_str().is_some_and(|t| !t.is_empty()),
        "{ctx}: text present"
    );
    assert!(
        g["png_base64"].as_str().is_some_and(|p| !p.is_empty()),
        "{ctx}: png present"
    );
    assert!(g["revision"].is_u64(), "{ctx}: revision present");
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

    // F1 parks a hidden cursor at grid (23,79).
    assert_glance_fields(&g, 80, 24, (23, 79), "G2 rpc");

    // Substrate cross-check: glance.revision == session.snapshot content_revision.
    let rev = g["revision"].as_u64().expect("glance revision");
    assert_eq!(rev, h.content_revision(&f.session_id, &f.pane_id));

    // Byte-identical goldens (mint per §16.3).
    let text = g["text"].as_str().expect("glance text");
    assert_text_golden(&h, text, "g2_f1_80x24.txt");
    let png = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(g["png_base64"].as_str().expect("glance png"))
            .expect("decode png")
    };
    assert_png_golden(&h, &png, "g2_f1_80x24.png");

    // ⇄ CLI twin (p0-council-r1 major 3/6): full field + fidelity parity. The
    // pane is static, so the CLI glance must byte-match the same goldens.
    let cli = h.cli_envelope(&["pane", "glance", &f.pane_id, "--checkpoint"]);
    let cg = cli.expect_result("G2 glance cli");
    assert_glance_fields(&cg, 80, 24, (23, 79), "G2 cli");
    assert_eq!(
        cg["revision"].as_u64(),
        Some(rev),
        "G2: CLI glance revision must match RPC glance revision (M9 parity)"
    );
    assert_text_golden(
        &h,
        cg["text"].as_str().expect("cli text"),
        "g2_f1_80x24.txt",
    );

    // CLI file-writing surface: text format + `--png <path>` writes the PNG.
    let png_path = std::env::temp_dir().join(format!("lens_g2_cli_{}.png", unique()));
    let out = h.cli(&[
        "pane",
        "glance",
        &f.pane_id,
        "--png",
        png_path.to_str().expect("tmp path utf8"),
    ]);
    assert_eq!(out.status.code(), Some(0), "G2: CLI --png exit 0");
    let written = std::fs::read(&png_path)
        .unwrap_or_else(|e| panic!("G2: CLI --png did not write {}: {e}", png_path.display()));
    assert_png_golden(&h, &written, "g2_f1_80x24.png");

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

    // F5 parks a hidden cursor at grid (29,99).
    assert_glance_fields(&g, 100, 30, (29, 99), "G2w rpc");
    let rev = g["revision"].as_u64().expect("glance revision");
    assert_eq!(rev, h.content_revision(&f.session_id, &f.pane_id));

    let text = g["text"].as_str().expect("glance text");
    assert_text_golden(&h, text, "g2w_f5_100x30.txt");
    {
        use base64::Engine;
        let png = base64::engine::general_purpose::STANDARD
            .decode(g["png_base64"].as_str().expect("glance png"))
            .expect("decode png");
        assert_png_golden(&h, &png, "g2w_f5_100x30.png");
    }

    // ⇄ CLI twin (p0-council-r1 major 3): fields + text fidelity parity.
    let cli = h.cli_envelope(&["pane", "glance", &f.pane_id, "--checkpoint"]);
    let cg = cli.expect_result("G2w glance cli");
    assert_glance_fields(&cg, 100, 30, (29, 99), "G2w cli");
    assert_eq!(
        cg["revision"].as_u64(),
        Some(rev),
        "G2w: CLI/RPC revision parity"
    );
    assert_text_golden(
        &h,
        cg["text"].as_str().expect("cli text"),
        "g2w_f5_100x30.txt",
    );

    h.kill_session(&f.session_id);
}
