//! `session snapshot` and `window snapshot` draw the window at the size its
//! panes really are, unless the caller asks for another size.
//!
//! The pane outline is drawn around the layout rect, so a frame wider than the
//! panes draws a box with dead space inside it that no terminal ever showed.
//!
//! Daemon-backed and black-box. Every pane prints the three colour bars
//! (truecolor + indexed + basic) so a monochrome regression cannot pass.

mod lens_common;
use lens_common::*;
use serde_json::json;

const TRUE_BG: (u8, u8, u8) = (10, 200, 30); // ESC[48;2;10;200;30m
const IDX_BG: (u8, u8, u8) = (255, 0, 0); //     ESC[48;5;196m
const BASIC_BG: (u8, u8, u8) = (205, 49, 49); //  ESC[41m
const TOL: i32 = 8;

/// A pane's grid when nothing has resized it.
const PANE_COLS: u32 = 80;
const PANE_ROWS: u32 = 24;

/// The bars sit at grid rows 0-2 and end with `exec cat`, which prints nothing
/// until it reads, so `BARS-DONE` is the last thing the pane ever draws.
const SCRIPT: &str = "printf '\\033[48;2;10;200;30mAAAAAAAAAAAA\\033[0m\\n\
    \\033[48;5;196mBBBBBBBBBBBB\\033[0m\\n\
    \\033[41mCCCCCCCCCCCC\\033[0m\\nBARS-%s\\n' DONE; exec cat";

fn near(actual: (u8, u8, u8, u8), expected: (u8, u8, u8)) -> bool {
    (actual.0 as i32 - expected.0 as i32).abs() <= TOL
        && (actual.1 as i32 - expected.1 as i32).abs() <= TOL
        && (actual.2 as i32 - expected.2 as i32).abs() <= TOL
}

fn spawn(h: &Harness) -> (String, String) {
    let created = h.rpc_ok(
        "session.create",
        json!({
            "name": format!("natsize-{}", unique()),
            "cwd": h.repo_root().display().to_string(),
            "command": ["sh", "-c", SCRIPT],
        }),
    );
    let session_id = created["id"].as_str().expect("session id").to_string();
    let pane_id = created["pane_id"].as_str().expect("pane id").to_string();
    h.wait_for(&pane_id, "BARS-DONE", shux_testkit::BUDGET_MS)
        .expect("pane never drew its bars");
    (session_id, pane_id)
}

fn decode(snap: &serde_json::Value) -> (image::RgbaImage, u32, u32) {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(snap["png_base64"].as_str().expect("png_base64"))
        .expect("decode png");
    let cw = snap["cell_width"].as_u64().expect("cell_width") as u32;
    let ch = snap["cell_height"].as_u64().expect("cell_height") as u32;
    assert!(cw > 0 && ch > 0, "zero cell metrics");
    (decode_png(&bytes), cw, ch)
}

/// The bars of the pane whose first content cell is `(col, row)` of the frame.
#[track_caller]
fn assert_bars(img: &image::RgbaImage, cw: u32, ch: u32, col: u32, row: u32, what: &str) {
    for (i, want) in [TRUE_BG, IDX_BG, BASIC_BG].into_iter().enumerate() {
        let got = probe_cell_bg_img(img, col + 4, row + i as u32, cw, ch);
        assert!(near(got, want), "{what}: bar {i} is {got:?}, want {want:?}");
    }
}

/// Three panes side by side, split right then right again, so the live
/// ratios are 1/2 and 1/2 over three 80-column grids. Returns the session id.
fn spawn_chain(h: &Harness) -> String {
    let (session_id, mut target) = spawn(h);
    for _ in 0..2 {
        let split = h.rpc_ok(
            "pane.split",
            json!({
                "pane_id": target,
                "direction": "vertical",
                "command": ["sh", "-c", SCRIPT],
            }),
        );
        target = split["pane"]["id"]
            .as_str()
            .expect("new pane id")
            .to_string();
        h.wait_for(&target, "BARS-DONE", shux_testkit::BUDGET_MS)
            .expect("split pane never drew its bars");
    }
    session_id
}

/// Each pane of [`spawn_chain`] whole, one separator column between them.
#[track_caller]
fn assert_chain_bars(img: &image::RgbaImage, cw: u32, ch: u32) {
    for (i, col) in [1, 2 + PANE_COLS, 3 + 2 * PANE_COLS]
        .into_iter()
        .enumerate()
    {
        assert_bars(img, cw, ch, col, 1, &format!("pane {i}"));
    }
}

/// Outline + one status row around `cols x rows` of panes.
fn frame_px(cols: u32, rows: u32, cw: u32, ch: u32) -> (u32, u32) {
    ((cols + 2) * cw, (rows + 2 + 1) * ch)
}

#[test]
fn a_snapshot_without_dims_frames_the_pane_at_its_real_size() {
    let h = Harness::new();
    let (session_id, _pane) = spawn(&h);

    for method in ["session.snapshot", "window.snapshot"] {
        let (img, cw, ch) = decode(&h.rpc_ok(method, json!({ "session_id": session_id })));
        assert_eq!(
            (img.width(), img.height()),
            frame_px(PANE_COLS, PANE_ROWS, cw, ch),
            "{method} framed an {PANE_COLS}x{PANE_ROWS} pane at a size it never had"
        );
        assert_bars(&img, cw, ch, 1, 1, method);
    }

    // The CLI is what people type; it must not send dims of its own.
    let dir = tempfile::tempdir().expect("tempdir");
    let out_png = dir.path().join("session.png");
    let out = h.cli(&[
        "session",
        "snapshot",
        "-s",
        &session_id,
        "-o",
        out_png.to_str().expect("utf-8 path"),
    ]);
    assert!(
        out.status.success(),
        "session snapshot failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let img = decode_png(&std::fs::read(&out_png).expect("read png"));
    let (cw, ch) = (
        img.width() / (PANE_COLS + 2),
        img.height() / (PANE_ROWS + 3),
    );
    assert_eq!(
        (img.width(), img.height()),
        frame_px(PANE_COLS, PANE_ROWS, cw, ch),
        "`shux session snapshot` without --cols/--rows framed the pane at a size it never had"
    );
    assert_bars(&img, cw, ch, 1, 1, "cli");
}

#[test]
fn a_snapshot_without_dims_fits_every_pane_of_a_split() {
    let h = Harness::new();
    let (session_id, pane_id) = spawn(&h);
    let split = h.rpc_ok(
        "pane.split",
        json!({
            "pane_id": pane_id,
            "direction": "vertical",
            "command": ["sh", "-c", SCRIPT],
        }),
    );
    let right = split["pane"]["id"]
        .as_str()
        .expect("new pane id")
        .to_string();
    h.wait_for(&right, "BARS-DONE", shux_testkit::BUDGET_MS)
        .expect("split pane never drew its bars");

    // Two 80-column panes side by side, one separator column between them.
    let (img, cw, ch) = decode(&h.rpc_ok("session.snapshot", json!({ "session_id": session_id })));
    assert_eq!(
        (img.width(), img.height()),
        frame_px(PANE_COLS * 2 + 1, PANE_ROWS, cw, ch),
        "a split of two {PANE_COLS}-column panes was not framed at their size"
    );
    assert_bars(&img, cw, ch, 1, 1, "left pane");
    assert_bars(&img, cw, ch, 1 + PANE_COLS + 1, 1, "right pane");
}

/// Drawn at the live ratios the first pane of the chain gets half the frame
/// and the other two crowd into the rest.
#[test]
fn a_snapshot_without_dims_fits_a_chain_of_splits_without_a_dead_band() {
    let h = Harness::new();
    let session_id = spawn_chain(&h);

    let (img, cw, ch) = decode(&h.rpc_ok("session.snapshot", json!({ "session_id": session_id })));
    assert_eq!(
        (img.width(), img.height()),
        frame_px(PANE_COLS * 3 + 2, PANE_ROWS, cw, ch),
        "three {PANE_COLS}-column panes were not framed at their size"
    );
    assert_chain_bars(&img, cw, ch);
}

/// Giving one axis leaves the other at the window's own size, which must
/// still hold every pane whole along it.
#[test]
fn a_snapshot_with_one_dim_still_fits_the_other_axis_of_a_chain() {
    let h = Harness::new();
    let session_id = spawn_chain(&h);

    let (img, cw, ch) = decode(&h.rpc_ok(
        "session.snapshot",
        json!({ "session_id": session_id, "rows": 40 }),
    ));
    assert_eq!(
        img.width(),
        frame_px(PANE_COLS * 3 + 2, PANE_ROWS, cw, ch).0,
        "--rows alone changed the window's width"
    );
    assert_chain_bars(&img, cw, ch);
}

#[test]
fn explicit_dims_still_win() {
    let h = Harness::new();
    let (session_id, _pane) = spawn(&h);
    let (img, cw, ch) = decode(&h.rpc_ok(
        "session.snapshot",
        json!({ "session_id": session_id, "cols": 120, "rows": 36 }),
    ));
    assert_eq!((img.width(), img.height()), (120 * cw, 36 * ch));
    assert_bars(&img, cw, ch, 1, 1, "explicit 120x36");
}

/// A window wider than the explicit-dims limit is still drawn whole when no
/// dims are given; only the pixel cap bounds it.
#[test]
fn a_wide_window_is_not_clipped_to_the_explicit_dims_limit() {
    let h = Harness::new();
    let (session_id, pane_id) = spawn(&h);
    h.rpc_ok(
        "pane.set_size",
        json!({ "pane_id": pane_id, "cols": 1000, "rows": 30 }),
    );
    let (img, cw, ch) = decode(&h.rpc_ok("session.snapshot", json!({ "session_id": session_id })));
    assert_eq!(
        (img.width(), img.height()),
        frame_px(1000, 30, cw, ch),
        "a 1000-column pane was framed narrower than itself"
    );
}

/// The cap refusal for a default-sized snapshot says how to get a smaller one.
#[test]
fn a_window_over_the_pixel_cap_names_the_dims_that_get_under_it() {
    let h = Harness::new();
    let (session_id, pane_id) = spawn(&h);
    h.rpc_ok(
        "pane.set_size",
        json!({ "pane_id": pane_id, "cols": 600, "rows": 400 }),
    );
    let err = h
        .rpc_raw("session.snapshot", json!({ "session_id": session_id }))
        .error
        .expect("a snapshot over the pixel cap is refused");
    let err = format!("{} {}", err.message, err.data.unwrap_or_default());
    assert!(
        err.contains("cap") && err.contains("--cols"),
        "cap error does not say how to shrink the frame: {err}"
    );
}
