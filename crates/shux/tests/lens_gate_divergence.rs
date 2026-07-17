//! Task 079 — divergence fixtures (GATE lane; `GATE-TEST-CHANGE:` to touch).
//!
//! Committed frame pairs proving the tiers' boundaries (council #1 MAJOR / design
//! D5–D7). Each case asserts the CELL-tier verdict `diff_frames` reports
//! (`cells_changed`, `cursor_moved`, `palette_overridden_differs`,
//! `geometry_changed`) against a HAND-DERIVED expectation committed in
//! `.shux/fixtures/lens-gate/divergence/<name>.expect.json` — the independent
//! oracle, NOT an echo of `diff_frames`. The `pixel_diverges` / `note` fields are
//! documentation for task 080's pixel tier; 079 renders no pixels and asserts none.
//!
//! Honest tier split (codex correction): blink IS a cell-tier signal (a `CellFlags`
//! bit) but shux's STATIC raster does not render blink, so it is NOT a pixel-tier
//! signal; a cursor-shape-only change is INVISIBLE to the cell tier (a documented
//! blind spot 080 covers); a palette override with no indexed colour present is a
//! `palette_overridden_differs` diagnostic that 080 must NOT escalate.

use std::path::{Path, PathBuf};

use shux_vt::{FrameDiff, FrameEnvelope, MaskSet, VirtualTerminal};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.shux/fixtures/lens-gate/divergence")
}

fn frame(vt: &VirtualTerminal) -> FrameEnvelope {
    FrameEnvelope::from_terminal(vt, &MaskSet::new())
}

fn vt(rows: usize, cols: usize, prog: &[u8]) -> VirtualTerminal {
    let mut v = VirtualTerminal::new(rows, cols);
    v.process(prog);
    v
}

/// The four cell-tier fields a divergence case pins.
struct Verdict {
    cells_changed: u32,
    cursor_moved: bool,
    palette_overridden_differs: bool,
    geometry_changed: bool,
}

fn expect_json(v: &Verdict, pixel_diverges: bool, note: &str) -> serde_json::Value {
    serde_json::json!({
        "cells_changed": v.cells_changed,
        "cursor_moved": v.cursor_moved,
        "palette_overridden_differs": v.palette_overridden_differs,
        "geometry_changed": v.geometry_changed,
        "pixel_diverges": pixel_diverges,
        "note": note,
    })
}

/// Canonical (sorted-key) full `FrameDiff` — pins every field (not just the four
/// verdict summaries) so tampering a frame or a behavior drift is caught (C-MINOR-1).
fn canon_fd(fd: &FrameDiff) -> String {
    serde_json::to_string_pretty(&serde_json::to_value(fd).unwrap()).unwrap()
}

fn assert_matches(name: &str, diff: &FrameDiff, want: &serde_json::Value) {
    assert_eq!(
        diff.cells_changed,
        want["cells_changed"].as_u64().unwrap() as u32,
        "{name}: cells_changed"
    );
    assert_eq!(
        diff.cursor_moved,
        want["cursor_moved"].as_bool().unwrap(),
        "{name}: cursor_moved"
    );
    assert_eq!(
        diff.palette_overridden_differs,
        want["palette_overridden_differs"].as_bool().unwrap(),
        "{name}: palette_overridden_differs"
    );
    assert_eq!(
        diff.geometry_changed,
        want["geometry_changed"].as_bool().unwrap(),
        "{name}: geometry_changed"
    );
}

/// The nine cases, as `(name, frame_a, frame_b, hand-derived verdict, pixel_diverges, note)`.
fn cases() -> Vec<(
    &'static str,
    FrameEnvelope,
    FrameEnvelope,
    Verdict,
    bool,
    &'static str,
)> {
    vec![
        (
            "cursor-position",
            frame(&vt(3, 8, b"\x1b[1;1Hxy")),
            frame(&vt(3, 8, b"\x1b[1;1Hxy\x1b[1;1H")),
            Verdict {
                cells_changed: 0,
                cursor_moved: true,
                palette_overridden_differs: false,
                geometry_changed: false,
            },
            true,
            "cell tier catches cursor position; pixel tier sees the block move",
        ),
        (
            "cursor-visibility",
            frame(&vt(3, 8, b"\x1b[1;1Hxy")),
            frame(&vt(3, 8, b"\x1b[1;1Hxy\x1b[?25l")),
            Verdict {
                cells_changed: 0,
                cursor_moved: true,
                palette_overridden_differs: false,
                geometry_changed: false,
            },
            true,
            "visibility flip is a cursor move",
        ),
        (
            "cursor-shape-only",
            frame(&vt(3, 8, b"\x1b[2 qxy")),
            frame(&vt(3, 8, b"\x1b[6 qxy")),
            Verdict {
                cells_changed: 0,
                cursor_moved: false,
                palette_overridden_differs: false,
                geometry_changed: false,
            },
            true,
            "CELL-TIER BLIND SPOT: CursorState carries no shape (parity); block↔bar is pixel-tier only (080)",
        ),
        (
            "palette-with-indexed",
            frame(&vt(2, 6, b"\x1b[31mAB\x1b[0m")),
            frame(&vt(2, 6, b"\x1b[31mAB\x1b[0m\x1b]4;1;#00ff00\x07")),
            Verdict {
                cells_changed: 0,
                cursor_moved: false,
                palette_overridden_differs: true,
                geometry_changed: false,
            },
            true,
            "OSC-4 override with indexed cells present: 080 escalates to palette_unportable",
        ),
        (
            "palette-no-indexed",
            frame(&vt(2, 6, b"AB")),
            frame(&vt(2, 6, b"AB\x1b]4;1;#00ff00\x07")),
            Verdict {
                cells_changed: 0,
                cursor_moved: false,
                palette_overridden_differs: true,
                geometry_changed: false,
            },
            false,
            "diagnostic fires but NO indexed colour present: 080 must NOT escalate (per-frame overridden && has_indexed)",
        ),
        (
            "blink-only",
            frame(&vt(2, 4, b"AB")),
            frame(&vt(2, 4, b"\x1b[5mAB\x1b[0m")),
            Verdict {
                cells_changed: 2,
                cursor_moved: false,
                palette_overridden_differs: false,
                geometry_changed: false,
            },
            false,
            "BLINK is a CellFlags bit → caught by CELL; shux's static raster does not render blink → NOT a pixel-tier signal",
        ),
        (
            "default-color-only",
            frame(&vt(2, 4, b"AB")),
            frame(&vt(2, 4, b"AB\x1b]11;#204060\x07")),
            Verdict {
                cells_changed: 8,
                cursor_moved: false,
                palette_overridden_differs: false,
                geometry_changed: false,
            },
            true,
            "OSC-11 default bg differs; every Default-bg cell (all 8) counts; pixel tier repaints the field",
        ),
        (
            "size-mismatch",
            frame(&vt(2, 5, b"hi")),
            frame(&vt(3, 5, b"hi")),
            Verdict {
                cells_changed: 0,
                cursor_moved: false,
                palette_overridden_differs: false,
                geometry_changed: true,
            },
            true,
            "row-count mismatch: geometry_changed is decisive; the min-overlap diff is diagnostic only",
        ),
        (
            "glyph-identical-pixel-boundary",
            frame(&vt(2, 6, "\u{2764}\u{fe0f}X".as_bytes())),
            frame(&vt(2, 6, "\u{2764}\u{fe0f}X".as_bytes())),
            Verdict {
                cells_changed: 0,
                cursor_moved: false,
                palette_overridden_differs: false,
                geometry_changed: false,
            },
            true,
            "cell-identical (same codepoints): the CELL tier is blind to font-fallback/antialias differences — 080's pixel tier proves those",
        ),
    ]
}

/// Regenerate the committed divergence frame pairs + hand-derived expectations.
///   cargo test -p shux --test lens_gate_divergence gen_ -- --ignored
#[test]
#[ignore = "generator: writes frozen divergence fixtures; run explicitly"]
fn gen_lens_gate_divergence_fixtures() {
    let dir = dir();
    std::fs::create_dir_all(&dir).unwrap();
    for (name, a, b, verdict, pixel, note) in cases() {
        // Assert the hand-derived verdict actually matches diff_frames BEFORE
        // freezing it — the fixture is human-verified, not an echo.
        let diff = shux_vt::diff_frames(&a.try_view().unwrap(), &b.try_view().unwrap());
        let want = expect_json(&verdict, pixel, note);
        assert_matches(name, &diff, &want);
        std::fs::write(dir.join(format!("{name}.a.json")), a.to_canonical_json()).unwrap();
        std::fs::write(dir.join(format!("{name}.b.json")), b.to_canonical_json()).unwrap();
        std::fs::write(
            dir.join(format!("{name}.expect.json")),
            serde_json::to_string_pretty(&want).unwrap(),
        )
        .unwrap();
        // Full-FrameDiff pin (C-MINOR-1): catches drift in the 6 fields the
        // verdict summary does not name (regions/bbox/changed_mask/rows/cols/…).
        std::fs::write(dir.join(format!("{name}.diff.json")), canon_fd(&diff)).unwrap();
    }
    eprintln!(
        "wrote {} divergence fixtures to {}",
        cases().len(),
        dir.display()
    );
}

fn load_env(path: &Path) -> FrameEnvelope {
    let json =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    FrameEnvelope::from_canonical_json(&json)
        .unwrap_or_else(|e| panic!("parse {}: {e:?}", path.display()))
}

/// Each committed divergence pair yields exactly its committed cell-tier verdict.
#[test]
fn divergence_fixtures_assert_cell_tier() {
    let dir = dir();
    let names: Vec<&str> = cases().iter().map(|c| c.0).collect();
    for name in names {
        let a = load_env(&dir.join(format!("{name}.a.json")));
        let b = load_env(&dir.join(format!("{name}.b.json")));
        let want: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(format!("{name}.expect.json")))
                .unwrap_or_else(|e| panic!("read {name}.expect.json: {e}")),
        )
        .unwrap();
        let diff = shux_vt::diff_frames(&a.try_view().unwrap(), &b.try_view().unwrap());
        // (a) the hand-derived cell-tier verdict (independent oracle).
        assert_matches(name, &diff, &want);
        // (b) the full FrameDiff pin — every field, catches frame tampering / drift.
        let want_full = std::fs::read_to_string(dir.join(format!("{name}.diff.json")))
            .unwrap_or_else(|e| panic!("read {name}.diff.json: {e}"));
        assert_eq!(canon_fd(&diff), want_full, "{name}: full FrameDiff pin");
        // (c) the documentation fields must stay well-formed (not silently rot).
        assert!(
            want["pixel_diverges"].is_boolean(),
            "{name}: pixel_diverges must be a bool"
        );
        assert!(
            want["note"].as_str().is_some_and(|s| !s.is_empty()),
            "{name}: note must be a non-empty string"
        );
    }
}
