//! Issue #108 acceptance — `window snapshot` vs `pane snapshot` cross-path
//! parity when the pane grid exceeds the window layout rect.
//!
//! Daemon-backed, black-box: drives the real `shux` binary end to end. The pane
//! prints three colour-probed background bars (truecolor + indexed + basic) at
//! the TOP of a grid that is then grown far past any window layout rect. Before
//! the fix, `window snapshot` bottom-anchored that oversized grid and returned a
//! blank content area (a valid PNG with borders/title/status and a lone cursor),
//! while `pane snapshot` at the same instant returned the full content — the
//! silent, colour-blind content loss this test forbids.
//!
//! Colour probes are mandatory (CLAUDE.md): a monochrome/blank regression cannot
//! pass because each bar's exact RGB is asserted, in BOTH render paths.

mod lens_common;
use lens_common::*;
use serde_json::json;

// Pane-content background colours, one per SGR colour space. Values are the
// raster's pinned palette (shared with the lens F3 fixture: see
// `lens_common::f3_expected_bg`).
const TRUE_BG: (u8, u8, u8) = (10, 200, 30); // ESC[48;2;10;200;30m  truecolor
const IDX_BG: (u8, u8, u8) = (255, 0, 0); //     ESC[48;5;196m       xterm cube
const BASIC_BG: (u8, u8, u8) = (205, 49, 49); //  ESC[41m             palette index 1
const TOL: i32 = 8;

fn near(actual: (u8, u8, u8, u8), expected: (u8, u8, u8)) -> bool {
    (actual.0 as i32 - expected.0 as i32).abs() <= TOL
        && (actual.1 as i32 - expected.1 as i32).abs() <= TOL
        && (actual.2 as i32 - expected.2 as i32).abs() <= TOL
}

/// `session.create` running a shell that prints one plain text marker, then the
/// three colour bars, then `exec cat` so the pane stays live with the cursor
/// parked on the row below the content (near the TOP of the eventually-grown
/// grid). Returns `(session_id, pane_id)`.
fn spawn_marker_session(h: &Harness) -> (String, String) {
    // printf interprets the octal ESCs. Bars are glyph-filled (not spaces):
    // trailing whitespace with a background is trimmed on a grid reflow, so
    // spaces would vanish on set_size. `probe_cell_bg_img` samples each cell's
    // top-left interior, which reads the solid background even under a glyph.
    let script = "printf 'OVERSIZE-MARKER\\n\
        \\033[48;2;10;200;30mAAAAAAAAAAAA\\033[0m\\n\
        \\033[48;5;196mBBBBBBBBBBBB\\033[0m\\n\
        \\033[41mCCCCCCCCCCCC\\033[0m\\n'; exec cat";
    let created = h.rpc_ok(
        "session.create",
        json!({
            "name": format!("wsbug-{}", unique()),
            "cwd": h.repo_root().display().to_string(),
            "command": ["sh", "-c", script],
        }),
    );
    let session_id = created["id"].as_str().expect("session id").to_string();
    let pane_id = created["pane_id"].as_str().expect("pane id").to_string();
    (session_id, pane_id)
}

/// Decode a `window.snapshot` at explicit dims into `(image, cell_w, cell_h)`.
fn window_snapshot(
    h: &Harness,
    session_id: &str,
    cols: u16,
    rows: u16,
) -> (image::RgbaImage, u32, u32) {
    let snap = h.rpc_ok(
        "window.snapshot",
        json!({ "session_id": session_id, "cols": cols, "rows": rows }),
    );
    decode_snapshot(&snap)
}

fn pane_snapshot(h: &Harness, pane_id: &str) -> (image::RgbaImage, u32, u32) {
    let snap = h.rpc_ok("pane.snapshot", json!({ "pane_id": pane_id }));
    decode_snapshot(&snap)
}

fn decode_snapshot(snap: &serde_json::Value) -> (image::RgbaImage, u32, u32) {
    use base64::Engine;
    let b64 = snap["png_base64"].as_str().expect("png_base64");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .expect("decode png");
    let cw = snap["cell_width"].as_u64().expect("cell_width") as u32;
    let ch = snap["cell_height"].as_u64().expect("cell_height") as u32;
    assert!(
        cw > 0 && ch > 0,
        "zero cell metrics pin every probe to origin"
    );
    (decode_png(&bytes), cw, ch)
}

/// The three bar colours, probed at cells `(col, base_row + i)` for i in 0..3.
/// `col` samples a trailing space (pure background) inside each bar.
fn probe_bars(
    img: &image::RgbaImage,
    cw: u32,
    ch: u32,
    col: u32,
    base_row: u32,
) -> [(u8, u8, u8, u8); 3] {
    [
        probe_cell_bg_img(img, col, base_row, cw, ch),
        probe_cell_bg_img(img, col, base_row + 1, cw, ch),
        probe_cell_bg_img(img, col, base_row + 2, cw, ch),
    ]
}

/// The acceptance test: at a geometry where the grid exceeds the rect,
/// `window snapshot` and `pane snapshot` agree on presence (and colour) of
/// content for the same pane at the same revision.
#[test]
fn window_snapshot_agrees_with_pane_snapshot_when_grid_exceeds_rect() {
    let h = Harness::new();
    let (session_id, pane_id) = spawn_marker_session(&h);

    // Wait for the top marker to land, THEN grow the grid far past any window
    // layout rect. pane.set_size is synchronous: the next snapshot sees 200x60.
    h.wait_for(&pane_id, "OVERSIZE-MARKER", 10_000)
        .expect("pane never drew its top marker");
    h.rpc_ok(
        "pane.set_size",
        json!({ "pane_id": pane_id, "cols": 200, "rows": 60 }),
    );

    // pane.capture still returns the content (the always-correct control path).
    let text = h.capture_text(&pane_id);
    assert!(
        text.contains("OVERSIZE-MARKER"),
        "control path (pane.capture) lost content: {text:?}"
    );

    // Same-revision guard: the pane is idle on `cat`, so the content_revision
    // is identical across both snapshot calls — they see the same frame.
    let rev_before = h.content_revision(&session_id, &pane_id);

    // pane.snapshot renders the full grid: bars at grid rows 1,2,3, col 0-based.
    let (pane_img, pcw, pch) = pane_snapshot(&h, &pane_id);
    let pane_bars = probe_bars(&pane_img, pcw, pch, 4, 1);

    // window.snapshot composites into a 120x36 rect (< the 60-row grid). With a
    // Rounded border the pane content origin is (1,1), so the bars land on
    // window rows 2,3,4 at col 1+4.
    let (win_img, wcw, wch) = window_snapshot(&h, &session_id, 120, 36);
    let win_bars = probe_bars(&win_img, wcw, wch, 5, 2);

    let rev_after = h.content_revision(&session_id, &pane_id);
    assert_eq!(
        rev_before, rev_after,
        "content changed between snapshots — not the same-revision comparison the issue requires"
    );

    // A definitely-blank window cell (deep in the empty content area) gives the
    // default background, so we can assert the bars are NOT that.
    let blank = probe_cell_bg_img(&win_img, 40, 20, wcw, wch);

    // pane.snapshot — the always-correct path — must show the colours.
    assert!(
        near(pane_bars[0], TRUE_BG),
        "pane.snapshot truecolor bar wrong: {:?}",
        pane_bars[0]
    );
    assert!(
        near(pane_bars[1], IDX_BG),
        "pane.snapshot indexed bar wrong: {:?}",
        pane_bars[1]
    );
    assert!(
        near(pane_bars[2], BASIC_BG),
        "pane.snapshot basic bar wrong: {:?}",
        pane_bars[2]
    );

    // window.snapshot must AGREE: same colours, at the same revision. This is
    // the line that was red before the fix (bars came back as `blank`).
    assert!(
        near(win_bars[0], TRUE_BG),
        "window.snapshot dropped the truecolor bar: got {:?}, blank cell is {:?} (grid taller than rect → content clipped to nothing)",
        win_bars[0],
        blank
    );
    assert!(
        near(win_bars[1], IDX_BG),
        "window.snapshot dropped the indexed bar: got {:?}, blank cell is {:?}",
        win_bars[1],
        blank
    );
    assert!(
        near(win_bars[2], BASIC_BG),
        "window.snapshot dropped the basic bar: got {:?}, blank cell is {:?}",
        win_bars[2],
        blank
    );

    // Explicit cross-path presence agreement (the issue's core assertion).
    let win_has_content =
        near(win_bars[0], TRUE_BG) && near(win_bars[1], IDX_BG) && near(win_bars[2], BASIC_BG);
    let pane_has_content =
        near(pane_bars[0], TRUE_BG) && near(pane_bars[1], IDX_BG) && near(pane_bars[2], BASIC_BG);
    assert_eq!(
        win_has_content, pane_has_content,
        "window snapshot and pane snapshot disagree on presence of content"
    );

    h.kill_session(&session_id);
}

/// The `pane split` case is the same defect: the split shrinks the layout rect
/// below the (unchanged) oversized pane grid. The original pane must still
/// render its top content after the split.
#[test]
fn window_snapshot_shows_content_after_split_shrinks_rect() {
    let h = Harness::new();
    let (session_id, pane_id) = spawn_marker_session(&h);

    h.wait_for(&pane_id, "OVERSIZE-MARKER", 10_000)
        .expect("pane never drew its top marker");
    h.rpc_ok(
        "pane.set_size",
        json!({ "pane_id": pane_id, "cols": 200, "rows": 60 }),
    );

    // Sanity: the pane is oversized in the un-split window too.
    let (win_img0, cw0, ch0) = window_snapshot(&h, &session_id, 120, 36);
    let bars0 = probe_bars(&win_img0, cw0, ch0, 5, 2);
    assert!(
        near(bars0[0], TRUE_BG),
        "pre-split window snapshot already blank: {:?}",
        bars0[0]
    );

    // Horizontal split: the original pane keeps the TOP half, whose rect height
    // (~16 rows) is far below the 60-row grid. This is the reported split case.
    h.rpc_ok(
        "pane.split",
        json!({ "pane_id": pane_id, "direction": "horizontal", "ratio": 0.5 }),
    );

    let (win_img, cw, ch) = window_snapshot(&h, &session_id, 120, 36);
    let bars = probe_bars(&win_img, cw, ch, 5, 2);
    let blank = probe_cell_bg_img(&win_img, 40, 6, cw, ch);
    assert!(
        near(bars[0], TRUE_BG),
        "split shrank the rect below the grid and window snapshot went blank: got {:?}, blank {:?}",
        bars[0],
        blank
    );
    assert!(
        near(bars[1], IDX_BG),
        "split lost the indexed bar: {:?}",
        bars[1]
    );
    assert!(
        near(bars[2], BASIC_BG),
        "split lost the basic bar: {:?}",
        bars[2]
    );

    h.kill_session(&session_id);
}
