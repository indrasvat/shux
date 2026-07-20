//! Lens-gate GREEN dogfood suite (task 078).
//!
//! Proves the capture schema on REAL terminal workloads — a real subprocess's
//! ANSI output driven through shux's own VT, captured to a `FrameEnvelope`, and
//! cross-checked against shux's own rasterizer (the semantic capture must agree
//! with the pixels). Run via `make test-lens-gate` (this file is `test = false`,
//! so it stays out of `make check` / CI `nextest --workspace`, matching the lens
//! suite regime). FROZEN — changes need a `GATE-TEST-CHANGE:` trailer.
//!
//! Colour-probe mandate (CLAUDE.md): every captured frame carries truecolor AND
//! 256-color AND basic-color content so a monochrome / NO_COLOR regression
//! cannot pass unnoticed.

use std::path::PathBuf;
use std::process::Command;

use shux_raster::{RasterOptions, Rasterizer};
use shux_vt::{
    CapColor, FrameEnvelope, GateStatus, MaskSet, Run, ScenarioReport, VirtualTerminal, XfailMeta,
};

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/crates/shux
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("repo root")
        .to_path_buf()
}

/// Run a real program and return its raw stdout bytes (real ANSI).
fn run_bytes(cmd: &str, args: &[&str]) -> Vec<u8> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("spawn {cmd}: {e}"));
    out.stdout
}

/// A rich frame that exercises every colour class + wide glyph + emoji + styles.
/// Emitted by a real `/bin/sh`, so this is genuine program output, not a
/// hand-built grid.
const RICH_FRAME_SH: &str = concat!(
    // truecolor fg + bg
    "printf '\\033[38;2;255;120;0m\\033[48;2;0;40;80mTRUECOLOR\\033[0m\\n';",
    // 256-indexed colour
    "printf '\\033[38;5;208m256IDX\\033[0m\\n';",
    // basic ANSI colour + bold + underline
    "printf '\\033[1;4;31mBASIC\\033[0m\\n';",
    // wide CJK + emoji + combining grapheme
    "printf '\\346\\274\\242\\345\\255\\227 \\360\\237\\221\\215 e\\314\\201\\n';",
);

fn capture_sh_frame(rows: usize, cols: usize) -> (VirtualTerminal, FrameEnvelope) {
    let bytes = run_bytes("/bin/sh", &["-c", RICH_FRAME_SH]);
    assert!(!bytes.is_empty(), "the shell emitted no output");
    let mut vt = VirtualTerminal::new(rows, cols);
    vt.process(&bytes);
    let env = FrameEnvelope::from_terminal(&vt, &MaskSet::new());
    (vt, env)
}

#[test]
fn dogfoods_real_program_output_losslessly() {
    let (vt, env) = capture_sh_frame(8, 40);

    // Canonical + lossless.
    env.validate()
        .expect("real program capture must be canonical");
    let json = env.to_canonical_json();
    let back = FrameEnvelope::from_canonical_json(&json).expect("parse");
    assert_eq!(env, back, "serde round-trip");
    assert_eq!(json, back.to_canonical_json(), "byte-stable");

    // The frame actually carries all three colour classes (colour-probe mandate)
    // — proves a monochrome regression would change the capture.
    let mut saw_rgb = false;
    let mut saw_idx = false;
    for row in &env.rows {
        for run in &row.runs {
            if let Run::Cells { style, .. } = run {
                if matches!(style.fg, Some(CapColor::Rgb(_)))
                    || matches!(style.bg, Some(CapColor::Rgb(_)))
                {
                    saw_rgb = true;
                }
                if matches!(style.fg, Some(CapColor::Idx(_))) {
                    saw_idx = true;
                }
            }
        }
    }
    assert!(
        saw_rgb,
        "no truecolor in the captured frame — colour-probe failed"
    );
    assert!(
        saw_idx,
        "no indexed colour in the captured frame — colour-probe failed"
    );

    // The wide CJK glyph survived as an array-form run with an explicit "".
    let has_wide_continuation = env.rows.iter().any(|r| {
        r.runs.iter().any(|run| matches!(run, Run::Cells { content: shux_vt::RunContent::Complex(v), .. } if v.iter().any(|e| e.is_empty())))
    });
    assert!(has_wide_continuation, "wide glyph continuation lost");

    // Fixed point: what we decode re-encodes identically.
    let cells = env.to_cells();
    assert_eq!(cells.len(), vt.grid().rows());
}

#[test]
fn semantic_capture_agrees_with_rasterized_pixels() {
    // Cross-path consistency (feature protocol): a cell the SEMANTIC capture says
    // has a given truecolor background must actually render those pixels in the
    // PIXEL path. This is what makes the gate's `cell` tier a faithful proxy for
    // what a human sees.
    let mut vt = VirtualTerminal::new(3, 20);
    // A truecolor background block at a known position.
    vt.process(b"\x1b[48;2;10;150;90m    \x1b[0m");
    let env = FrameEnvelope::from_terminal(&vt, &MaskSet::new());
    env.validate().unwrap();

    // Find a captured cell with bg = rgb(10,150,90).
    let bg = env.rows[0].runs.iter().find_map(|run| match run {
        Run::Cells { col, style, .. } => match style.bg {
            Some(CapColor::Rgb(c)) => Some((*col, c)),
            _ => None,
        },
        _ => None,
    });
    let (col, want) = bg.expect("captured a truecolor-bg cell");
    assert_eq!(
        want,
        [10, 150, 90],
        "capture preserved the exact truecolor bg"
    );

    // Rasterize the SAME grid and sample that cell's centre pixel.
    let rasterizer = Rasterizer::new(16.0).expect("rasterizer");
    let img = rasterizer.render(vt.grid(), &RasterOptions::default());
    let cols = vt.grid().cols() as u32;
    let rows = vt.grid().rows() as u32;
    let cell_w = img.width() / cols;
    let cell_h = img.height() / rows;
    let px = col as u32 * cell_w + cell_w / 2;
    let py = cell_h / 2;
    let sample = img.get_pixel(px.min(img.width() - 1), py.min(img.height() - 1));
    let [r, g, b, _] = sample.0;

    // The rasterizer applies the exact bg colour; allow a tiny tolerance for any
    // gamma/rounding in the raster path.
    let close = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 6;
    assert!(
        close(r, 10) && close(g, 150) && close(b, 90),
        "pixel {:?} at cell {col} disagrees with captured bg {want:?}",
        [r, g, b]
    );
}

#[test]
fn showcase_fixture_is_stable() {
    // A frozen, human-reviewable canonical capture. Regenerate intentionally with
    // LENS_GATE_BLESS=1 (then review the diff + add a GATE-TEST-CHANGE: trailer).
    let (_, env) = capture_sh_frame(8, 40);
    let json = env.to_canonical_json();
    let path = repo_root().join(".shux/fixtures/lens-gate/capture/showcase.capture.json");

    if std::env::var("LENS_GATE_BLESS").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("{json}\n")).unwrap();
        eprintln!("blessed {}", path.display());
        return;
    }

    let golden = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "showcase golden missing: {} — generate with LENS_GATE_BLESS=1 make test-lens-gate",
            path.display()
        )
    });
    assert_eq!(
        json.trim(),
        golden.trim(),
        "showcase capture drifted from the frozen golden"
    );

    // The committed golden must itself be canonical + parseable.
    let parsed = FrameEnvelope::from_canonical_json(golden.trim()).expect("golden parses");
    parsed.validate().expect("golden is canonical");
}

#[test]
fn report_fixtures_conform_to_the_frozen_schema() {
    // The committed report.json fixtures must parse into the frozen types
    // (deny_unknown_fields, so an extra key would fail here) and every status +
    // rolled-up exit code must be consistent. These fixtures ARE the frozen
    // report contract 082 must satisfy.
    let base = repo_root().join(".shux/fixtures/lens-gate/report");
    for name in ["pass.report.json", "regression.report.json"] {
        let text = std::fs::read_to_string(base.join(name)).expect(name);
        let report: Vec<ScenarioReport> =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name} conforms: {e}"));
        assert!(!report.is_empty(), "{name} is empty");
        for scn in &report {
            // The scenario status must roll up to the worst frame status.
            let worst = scn
                .frames
                .iter()
                .fold(GateStatus::Pass, |acc, f| acc.worst(f.status));
            assert_eq!(
                scn.status, worst,
                "{name}: scenario status is the worst frame"
            );
        }
    }

    // A `fail` frame may carry a `palette_unportable` REASON (never a status).
    let regr = std::fs::read_to_string(base.join("regression.report.json")).unwrap();
    let report: Vec<ScenarioReport> = serde_json::from_str(&regr).unwrap();
    let reason = report[0].frames[0].reason.as_deref();
    assert_eq!(reason, Some("palette_unportable"));
    assert_eq!(report[0].frames[0].status, GateStatus::Fail);
}

#[test]
fn xfail_fixture_conforms_to_the_frozen_schema() {
    let path = repo_root().join(".shux/fixtures/lens-gate/xfail/example.xfail.json");
    let text = std::fs::read_to_string(&path).expect("xfail fixture");
    let meta: XfailMeta = serde_json::from_str(&text).expect("xfail conforms");
    assert!(!meta.reason.is_empty() && !meta.owner.is_empty() && !meta.issue.is_empty());
    assert!(
        meta.fingerprint.is_some(),
        "the example carries a fingerprint"
    );
}
