//! Headless heat-overlay evidence for a failing frame (dogfood: the "pixel-perfect proof"
//! must be producible in CI / by an agent, not only in the interactive `gate review`).
//!
//! On a `Mismatch`, the gate renders the LIVE frame and overlays the diff, then writes
//! `<out>/<name>.heat.png` and records the path in `report.json`'s `diff.heat_png`. A CELL
//! diff (`changed_cells > 0`) highlights the changed cells in heat red and desaturates the
//! rest — the same visualization `pane.diff_since` produces. A PIXEL-ONLY diff
//! (`changed_cells == 0`, pixel/exact tier) instead diffs the live render against the
//! committed golden PNG per pixel and highlights the differing pixels — the sub-cell
//! regression a cell mask can't show. Best-effort: a render/decode/write failure is
//! skipped, never fatal to the verdict.

use std::path::{Path, PathBuf};

use image::RgbaImage;
use shux_raster::{
    Rasterizer, decode_png, encode_png, os_arch, pixel_baseline_path, render_envelope,
};
use shux_vt::{FrameEnvelope, GateStatus, ScenarioReport, Tier};

use super::outcome::{FrameKind, RunOutcome};

const HEAT: [u32; 3] = [163, 38, 56];
const ALPHA: u32 = 128;

/// Write a heat PNG for every fail frame into `out_dir` and set `diff.heat_png` on the
/// matching report frame. `golden_dir` supplies the committed baseline for a pixel-only
/// heat.
pub fn emit_heat_for_fails(
    outcome: &RunOutcome,
    reports: &mut [ScenarioReport],
    golden_dir: &Path,
    out_dir: &Path,
    rasterizer: &Rasterizer,
) -> Vec<String> {
    // Returned so the caller can also put them in `report.json` — CI reads the report, not
    // stderr, and evidence that silently failed to appear is exactly what a machine consumer
    // needs told (085 F23).
    let mut problems: Vec<String> = Vec::new();
    // Only a frame whose FINAL report status is `fail` gets a heat — a blessed (now-pass),
    // xfail (green), or stale/missing frame is not a live regression to visualize.
    let fail_names: std::collections::HashSet<String> = reports
        .iter()
        .flat_map(|s| s.frames.iter())
        .filter(|f| f.status == GateStatus::Fail)
        .map(|f| f.name.clone())
        .collect();

    for f in &outcome.frames {
        if f.kind != FrameKind::Mismatch || !fail_names.contains(&f.name) {
            continue;
        }
        let Some(verdict) = &f.verdict else { continue };
        let Ok(live) = FrameEnvelope::from_canonical_json(&f.live_capture_json) else {
            continue;
        };
        let mut img = render_envelope(rasterizer, &live);
        let d = &verdict.cell.diff;

        if d.cells_changed > 0 {
            overlay_changed_cells(&mut img, rasterizer, &d.changed_mask, d.rows, d.cols);
        } else if f.tier != Tier::Cell {
            // A pixel-only regression: diff the live render against the committed golden.
            let baseline = pixel_baseline_path(golden_dir, &f.name, &os_arch());
            match std::fs::read(&baseline)
                .ok()
                .and_then(|b| decode_png(&b).ok())
            {
                Some(golden) => overlay_pixel_diff(&mut img, &golden),
                None => continue, // no baseline to diff against
            }
        } else {
            continue; // cell tier with no cell change: nothing meaningful to draw
        }

        // 085 F23: still best-effort — a heat failure must never change the verdict — but
        // never SILENT. The heat PNG is the whole pixel-perfect proof; dropping it with no
        // diagnostic leaves the reader believing the gate simply produced no evidence.
        let png = match encode_png(&img) {
            Ok(p) => p,
            Err(e) => {
                problems.push(warn_heat_skipped(
                    &f.name,
                    &format!("could not encode the overlay: {e}"),
                ));
                continue;
            }
        };
        if let Err(e) = std::fs::create_dir_all(out_dir) {
            problems.push(warn_heat_skipped(
                &f.name,
                &format!("could not create {} ({e})", out_dir.display()),
            ));
            continue;
        }
        let path = out_dir.join(format!("{}.heat.png", f.name));
        if let Err(e) = std::fs::write(&path, &png) {
            problems.push(warn_heat_skipped(
                &f.name,
                &format!("could not write {} ({e})", path.display()),
            ));
            continue;
        }
        set_heat_path(reports, &f.name, &path);
    }
    problems
}

/// Tell the reader the heat evidence was skipped, and why (085 F23). Warnings go to
/// STDERR so the `--report -` stdout-purity contract is untouched.
fn warn_heat_skipped(frame: &str, why: &str) -> String {
    let msg = format!("no heat PNG for frame '{frame}': {why}");
    eprintln!("{}", crate::style::warning(format!("lens gate: {msg}")));
    msg
}

/// Overlay the changed CELLS (heat red) and desaturate the rest — cell-granularity heat.
fn overlay_changed_cells(
    img: &mut RgbaImage,
    rasterizer: &Rasterizer,
    changed_mask: &[bool],
    rows: usize,
    cols: usize,
) {
    let (cw, ch) = rasterizer.cell_size();
    let (iw, ih) = (img.width(), img.height());
    for r in 0..rows {
        for c in 0..cols {
            let changed = changed_mask.get(r * cols + c).copied().unwrap_or(false);
            let x0 = c as u32 * cw;
            let y0 = r as u32 * ch;
            for y in y0..(y0 + ch).min(ih) {
                for x in x0..(x0 + cw).min(iw) {
                    blend_pixel(img.get_pixel_mut(x, y), changed);
                }
            }
        }
    }
}

/// Overlay per-PIXEL differences (heat red where the live render differs from the golden,
/// desaturated where identical) — sub-cell heat for a pixel-only regression.
fn overlay_pixel_diff(img: &mut RgbaImage, golden: &RgbaImage) {
    let (iw, ih) = (img.width(), img.height());
    for y in 0..ih {
        for x in 0..iw {
            let differs = golden
                .get_pixel_checked(x, y)
                .map(|g| {
                    let p = img.get_pixel(x, y);
                    (0..3).any(|i| p.0[i].abs_diff(g.0[i]) > 0)
                })
                .unwrap_or(true); // outside the golden (geometry diff) → highlight
            blend_pixel(img.get_pixel_mut(x, y), differs);
        }
    }
}

/// Heat-blend a pixel: `changed` → alpha-blend HEAT over it; else desaturate 50% (mirrors
/// the daemon `pane.diff_since` heat math — deterministic integer arithmetic).
fn blend_pixel(px: &mut image::Rgba<u8>, changed: bool) {
    let [pr, pg, pb, _pa] = px.0;
    if changed {
        px.0[0] = ((HEAT[0] * ALPHA + pr as u32 * (255 - ALPHA)) / 255) as u8;
        px.0[1] = ((HEAT[1] * ALPHA + pg as u32 * (255 - ALPHA)) / 255) as u8;
        px.0[2] = ((HEAT[2] * ALPHA + pb as u32 * (255 - ALPHA)) / 255) as u8;
    } else {
        let gray = (pr as u32 * 77 + pg as u32 * 150 + pb as u32 * 29) >> 8;
        px.0[0] = ((pr as u32 + gray) / 2) as u8;
        px.0[1] = ((pg as u32 + gray) / 2) as u8;
        px.0[2] = ((pb as u32 + gray) / 2) as u8;
    }
}

/// Record the written heat path on the report frame whose name matches.
fn set_heat_path(reports: &mut [ScenarioReport], name: &str, path: &Path) {
    let display = path.display().to_string();
    for sr in reports.iter_mut() {
        for fr in sr.frames.iter_mut() {
            if fr.name == name
                && let Some(diff) = fr.diff.as_mut()
            {
                diff.heat_png = Some(display.clone());
            }
        }
    }
}

/// Resolve the evidence output directory: an explicit `--out`, else `.shux/out/<scenario>/`.
pub fn out_dir(explicit: Option<&Path>, scenario: &str) -> PathBuf {
    explicit
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(".shux/out").join(scenario))
}
