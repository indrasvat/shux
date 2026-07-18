//! Task 080 — capture + compare throughput benchmark (GATE lane; `GATE-TEST-CHANGE:` to
//! touch). `test = false` → run via `make bench-lens-gate` (records numbers; NOT a
//! wall-clock hard gate — timing assertions flake in CI).
//!
//! Records capture + cell-compare + render throughput at 10 / 100 / 1000 frames (task
//! 080 §6). Deterministic work (no daemon); the only assertion is a sanity floor so a
//! catastrophic regression (a compare that never returns) still fails.

use std::time::Instant;

use shux_raster::{Rasterizer, render_envelope};
use shux_vt::{FrameEnvelope, MaskSet, TolParams, VirtualTerminal, compare_cell};

/// A dense, coloured 24×80 frame whose row `seed` differs (so consecutive frames diff).
fn frame(seed: usize) -> FrameEnvelope {
    let mut vt = VirtualTerminal::new(24, 80);
    for row in 0..24 {
        vt.process(format!("\x1b[{};1H", row + 1).as_bytes());
        vt.process(format!("\x1b[38;5;{}m", 16 + ((row + seed) % 200)).as_bytes());
        for c in 0..80 {
            vt.process(&[b'A' + ((row + c + seed) % 26) as u8]);
        }
    }
    FrameEnvelope::from_terminal(&vt, &MaskSet::new())
}

fn bench(n: usize) {
    // Capture N frames.
    let t0 = Instant::now();
    let frames: Vec<FrameEnvelope> = (0..n).map(frame).collect();
    let cap = t0.elapsed();

    // Cell-compare each frame against the next (wrap-around).
    let t1 = Instant::now();
    let mut changed = 0u64;
    for i in 0..n {
        let a = frames[i].try_view().unwrap();
        let b = frames[(i + 1) % n].try_view().unwrap();
        changed += u64::from(compare_cell(&a, &b).diff.cells_changed);
    }
    let cmp = t1.elapsed();

    // Render N frames (pixel-tier cost).
    let r = Rasterizer::new(16.0).unwrap();
    let t2 = Instant::now();
    let mut px = 0u64;
    for f in &frames {
        px += render_envelope(&r, f).as_raw().len() as u64;
    }
    let render = t2.elapsed();

    let _tol = TolParams::default();
    eprintln!(
        "lens-gate bench n={n:>4}: capture={cap:>10.2?} ({:>7.0}/s)  cell-compare={cmp:>10.2?} ({:>7.0}/s)  render={render:>10.2?} ({:>7.0}/s)  [changed={changed} px_bytes={px}]",
        n as f64 / cap.as_secs_f64(),
        n as f64 / cmp.as_secs_f64(),
        n as f64 / render.as_secs_f64(),
    );
    assert!(changed > 0 || n == 0, "sanity: consecutive frames differ");
}

#[test]
fn lens_gate_throughput_10_100_1000() {
    eprintln!("── lens-gate capture/compare/render throughput (task 080 §6) ──");
    for n in [10usize, 100, 1000] {
        bench(n);
    }
}
