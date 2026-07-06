//! Fixture smoke tests (§11 acceptance): prove each lens fixture's contract
//! using ONLY pre-lens machinery (session/pane CRUD, pane.set_size, send-keys,
//! pane.capture, pane.snapshot). These MUST be GREEN in Phase P0 — the red
//! suite trusts these fixtures only once they are proven deterministic here.
//!
//! FROZEN after P0 (§16.2).

mod lens_common;
use lens_common::*;

/// A background pixel that is not pure grayscale (proves colour rendering).
fn assert_has_colour(png: &[u8], col: u32, row: u32, cw: u32, ch: u32, ctx: &str) {
    let (r, g, b, _) = probe_cell_bg(png, col, row, cw, ch);
    assert!(
        r != g || g != b,
        "{ctx}: expected coloured background at cell ({col},{row}), got grayscale ({r},{g},{b})"
    );
}

#[test]
fn f1_static_draws_unicode_and_colour() {
    let h = Harness::new();
    let f = h.launch_fixture("f1_static.sh", 80, 24, "दृश्यते");
    let text = h.capture_text(&f.pane_id);
    assert!(text.contains("दृश्यते"), "F1 Devanagari missing:\n{text}");
    assert!(text.contains("終端"), "F1 CJK missing");
    assert!(text.contains('✓'), "F1 emoji missing");

    // Hidden parked cursor (LENS-R-011 / F1 contract).
    let cap = h.rpc_ok("pane.capture", serde_json::json!({ "pane_id": f.pane_id }));
    assert_eq!(cap["cursor"]["visible"], serde_json::Value::Bool(false));

    let (png, cw, ch) = h.snapshot_png(&f.pane_id);
    // Truecolor gradient bar is on grid row 2.
    assert_has_colour(&png, 10, 2, cw, ch, "F1 gradient");
    h.kill_session(&f.session_id);
}

#[test]
fn f2_spinner_advances_and_signals_ready() {
    let h = Harness::new();
    let f = h.launch_fixture("f2_spinner.sh", 80, 24, "LENS-F2-SPIN");
    // Advance a few frames, then request READY.
    for _ in 0..5 {
        h.send_line_token(&f.pane_id, "");
    }
    h.send_line_token(&f.pane_id, "R");
    h.wait_for(&f.pane_id, "READY", 5_000).expect("F2 READY");
    let ready_text = h.capture_text(&f.pane_id);
    assert!(ready_text.contains("READY"));

    // p0-council-r1 minor 12: after READY the fixture drains further stdin
    // SILENTLY — more tokens must not change a single cell. Bounded negative
    // check: poll for any divergence from the READY frame and require none.
    for _ in 0..5 {
        h.send_line_token(&f.pane_id, "");
    }
    let changed = wait_until(std::time::Duration::from_millis(1_000), || {
        h.capture_text(&f.pane_id) != ready_text
    });
    assert!(
        !changed,
        "F2: output changed after READY — post-READY tokens must be drained silently"
    );
    h.kill_session(&f.session_id);
}

#[test]
fn f3_flip_alternates_frames_and_colours() {
    let h = Harness::new();
    let f = h.launch_fixture("f3_flip.sh", 80, 24, "AAAAAAAAAA");
    let (png_a, cw, ch) = h.snapshot_png(&f.pane_id);
    assert_eq!(
        classify_frame_exact(probe_cell_bg(&png_a, 0, 0, cw, ch), 0),
        'A'
    );

    // One token flips to frame B.
    h.send_line_token(&f.pane_id, "");
    h.wait_for(&f.pane_id, "BBBBBBBBBB", 5_000)
        .expect("F3 flip to B");
    let (png_b, _, _) = h.snapshot_png(&f.pane_id);
    assert_eq!(
        classify_frame_exact(probe_cell_bg(&png_b, 0, 0, cw, ch), 0),
        'B'
    );
    // Exact-color validation must hold on every probe row class G1 uses:
    // rows 0/12 (truecolor) and 23 (basic ANSI palette).
    let img_b = decode_png(&png_b);
    assert_eq!(
        classify_frame_exact(probe_cell_bg_img(&img_b, 40, 12, cw, ch), 12),
        'B'
    );
    assert_eq!(
        classify_frame_exact(probe_cell_bg_img(&img_b, 40, 23, cw, ch), 23),
        'B'
    );
    h.kill_session(&f.session_id);
}

#[test]
fn f4_keys_press_recolour_and_marker() {
    let h = Harness::new();
    let f = h.launch_fixture("f4_keys.sh", 80, 24, "LENS-F4-KEYS");

    // `s` before any `a` is a documented NO-OP (delta 1).
    h.send_raw(&f.pane_id, "s");
    // Give the fixture a wait_for gate on something that must NOT appear: use a
    // short absent-check via a fresh capture after a marker round-trip.
    h.send_raw(&f.pane_id, "\t"); // harmless marker move to force a redraw
    h.wait_for(&f.pane_id, "▶", 5_000).expect("marker present");
    assert!(
        !h.capture_text(&f.pane_id).contains("A-PRESSED"),
        "F4: `s` before `a` must be a no-op"
    );

    // Now `a` paints, `s` recolours the same glyphs (still A-PRESSED).
    h.send_raw(&f.pane_id, "a");
    h.wait_for(&f.pane_id, "A-PRESSED", 5_000)
        .expect("F4 A-PRESSED");
    h.send_raw(&f.pane_id, "s");
    assert!(h.capture_text(&f.pane_id).contains("A-PRESSED"));
    h.kill_session(&f.session_id);
}

#[test]
fn f5_wide_unicode_at_100x30() {
    let h = Harness::new();
    let f = h.launch_fixture("f5_wide.sh", 100, 30, "LENS-F5-WIDE");
    let text = h.capture_text(&f.pane_id);
    assert!(text.contains("終端"), "F5 CJK missing");
    assert!(text.contains("क्षत्रिय"), "F5 combining Devanagari missing");
    let snap = h.rpc_ok("pane.snapshot", serde_json::json!({ "pane_id": f.pane_id }));
    assert_eq!(snap["cols"], 100);
    assert_eq!(snap["rows"], 30);
    h.kill_session(&f.session_id);
}

#[test]
fn f6_exit42_prints_bye_and_exits_42() {
    // F6 exits, so verify it directly as a subprocess (no daemon needed).
    let repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("repo root");
    let out = std::process::Command::new("sh")
        .arg(".shux/fixtures/lens/f6_exit42.sh")
        .current_dir(&repo)
        .output()
        .expect("run f6");
    assert_eq!(out.status.code(), Some(42), "F6 must exit 42");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("BYE"),
        "F6 must print BYE"
    );
}

#[test]
fn f7_winsize_reports_and_reprints_on_resize() {
    let h = Harness::new();
    let f = h.launch_fixture("f7_winsize.sh", 80, 24, "SIZE=24 80");
    assert!(h.capture_text(&f.pane_id).contains("SIZE=24 80"));

    // Live resize must deliver SIGWINCH; the WINCH trap reprints (F7 note).
    h.rpc_ok(
        "pane.set_size",
        serde_json::json!({ "pane_id": f.pane_id, "cols": 120, "rows": 40 }),
    );
    h.wait_for(&f.pane_id, "SIZE=40 120", 5_000)
        .expect("F7 WINCH reprint");
    h.kill_session(&f.session_id);
}

#[test]
fn f8_repaint_progresses_glyphs() {
    let h = Harness::new();
    let f = h.launch_fixture("f8_repaint.sh", 80, 24, "LENS-F8-REPAINT");
    h.send_line_token(&f.pane_id, "");
    h.wait_for(&f.pane_id, "FRAME:0", 5_000)
        .expect("F8 FRAME:0");
    h.send_line_token(&f.pane_id, "");
    h.wait_for(&f.pane_id, "FRAME:1", 5_000)
        .expect("F8 FRAME:1");
    h.kill_session(&f.session_id);
}

#[test]
fn f9_metadata_only_v_draws_a_cell() {
    let h = Harness::new();
    let f = h.launch_fixture("f9_metadata.sh", 80, 24, "LENS-F9-META");
    // Several Class-B tokens must not add the visible mark.
    for _ in 0..5 {
        h.send_line_token(&f.pane_id, "");
    }
    assert!(
        !h.capture_text(&f.pane_id).contains('▮'),
        "F9: metadata tokens must not draw a visible cell"
    );
    // `V` draws the one visible green cell.
    h.send_line_token(&f.pane_id, "V");
    h.wait_for(&f.pane_id, "▮", 5_000).expect("F9 V mark");
    h.kill_session(&f.session_id);
}

#[test]
fn f10_altscreen_enter_and_leave() {
    let h = Harness::new();
    let f = h.launch_fixture("f10_altscreen.sh", 80, 24, "LENS-F10-ALT");
    assert!(h.capture_text(&f.pane_id).contains("NORMAL-SCREEN"));

    h.send_line_token(&f.pane_id, "E");
    h.wait_for(&f.pane_id, "ALT-SCREEN", 5_000)
        .expect("F10 enter alt");
    assert!(h.capture_text(&f.pane_id).contains("ALT-SCREEN"));

    h.send_line_token(&f.pane_id, "L");
    h.wait_for(&f.pane_id, "NORMAL-SCREEN", 5_000)
        .expect("F10 leave alt restores normal");
    h.kill_session(&f.session_id);
}
