//! `window.snapshot` and `session.snapshot` must draw a pane's inline image.
//!
//! Daemon-backed and black-box on purpose. `window_snapshot_images.rs` composes
//! and rasterizes in-process, so it cannot see the one production line that
//! wires compositing into the RPC — deleting that line leaves those tests green
//! while the real verb returns a picture-free frame. This test fails instead.

mod lens_common;
use lens_common::*;
use serde_json::json;

/// Solid magenta, delivered in 4096-byte base64 chunks with `a=T` repeated on
/// every continuation, the way real `kitten icat` sends anything over a few KB.
/// `C=1` keeps the cursor still so the picture stays where it was put.
fn kitty_apc(cols: u32, rows: u32) -> String {
    use base64::Engine;
    let (w, h) = (cols * 9, rows * 19);
    let rgba: Vec<u8> = (0..w * h).flat_map(|_| [255u8, 0, 255, 255]).collect();
    let b64 = base64::engine::general_purpose::STANDARD.encode(&rgba);
    let bytes = b64.as_bytes();
    let total = bytes.len().div_ceil(4096).max(1);
    let mut out = String::new();
    for (i, chunk) in bytes.chunks(4096).enumerate() {
        let more = u8::from(i + 1 < total);
        let payload = std::str::from_utf8(chunk).expect("base64 is ascii");
        if i == 0 {
            out.push_str(&format!(
                "\\033_Ga=T,f=32,t=d,s={w},v={h},i=1,C=1,m={more};{payload}\\033\\\\"
            ));
        } else {
            out.push_str(&format!("\\033_Ga=T,i=1,m={more};{payload}\\033\\\\"));
        }
    }
    out
}

fn magenta_px(img: &image::RgbaImage) -> usize {
    img.pixels()
        .filter(|p| p.0[0] > 200 && p.0[1] < 80 && p.0[2] > 200)
        .count()
}

fn decode(snap: &serde_json::Value) -> image::RgbaImage {
    use base64::Engine;
    let b64 = snap["png_base64"].as_str().expect("png_base64");
    decode_png(
        &base64::engine::general_purpose::STANDARD
            .decode(b64)
            .expect("decode png"),
    )
}

#[test]
fn window_and_session_snapshot_draw_a_panes_inline_image() {
    let h = Harness::new();
    // Colour probes ride alongside the picture (CLAUDE.md): truecolor, indexed
    // and basic, so a monochrome or overpaint regression cannot pass.
    let script = format!(
        "printf '\\033[38;2;0;200;90mTRUECOLOR\\033[0m \\033[38;5;208mIDX\\033[0m \\033[34mBASIC\\033[0m\\n'; \
         printf '{}'; exec cat",
        kitty_apc(10, 6)
    );
    let created = h.rpc_ok(
        "session.create",
        json!({
            "name": format!("wsimg-{}", unique()),
            "cwd": h.repo_root().display().to_string(),
            "command": ["sh", "-c", script],
        }),
    );
    let session_id = created["id"].as_str().expect("session id").to_string();
    let pane_id = created["pane_id"].as_str().expect("pane id").to_string();

    // The picture must actually be held before any of this means anything.
    h.rpc_ok(
        "pane.wait_settled",
        json!({ "pane_id": pane_id, "quiet_ms": 400, "timeout_ms": 15000 }),
    );
    let pane = decode(&h.rpc_ok("pane.snapshot", json!({ "pane_id": pane_id })));
    let pane_px = magenta_px(&pane);
    assert!(
        pane_px > 1000,
        "the pane itself never drew the picture ({pane_px} px) -- nothing below is meaningful"
    );

    for (verb, params) in [
        ("window.snapshot", json!({ "session_id": session_id })),
        ("session.snapshot", json!({ "session_id": session_id })),
    ] {
        let img = decode(&h.rpc_ok(verb, params));
        let px = magenta_px(&img);
        assert!(
            px > 1000,
            "{verb} returned a frame with no picture ({px} px); pane.snapshot has {pane_px}"
        );
    }
}
