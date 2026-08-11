//! Lens raster helpers — the pieces `pane.glance` and `pane.diff_since` share.
//!
//! The pixel budget is checked here rather than at each call site because both
//! methods can be handed a pane large enough to allocate hundreds of megabytes
//! before any post-encode size check could fire.

/// Pre-render pixel budget shared by every lens rasterizing path
/// (`pane.glance` PNG, `pane.diff_since` heat PNG — PR #91 codex P1). The
/// same 16M-pixel cap `pane.snapshot` enforces, checked BEFORE any RGBA
/// allocation or rasterization: a 1000×1000-cell pane (valid per
/// `pane.set_size` limits) would otherwise allocate hundreds of MB before
/// the post-encode 8 MiB check could fire. Over budget →
/// `PAYLOAD_TOO_LARGE (-32013)` with `{pixels, max_pixels, hint}` — the
/// caller supplies the method-appropriate hint.
pub(crate) fn lens_pixel_budget_check(
    cols: usize,
    rows: usize,
    cell_w: u32,
    cell_h: u32,
    hint: &str,
) -> Result<(), shux_rpc::RpcError> {
    const MAX_PIXELS: u64 = 16_000_000;
    let pixel_count = (cols as u64)
        .saturating_mul(cell_w as u64)
        .saturating_mul(rows as u64)
        .saturating_mul(cell_h as u64);
    if pixel_count > MAX_PIXELS {
        return Err(shux_rpc::RpcError::with_message_and_data(
            shux_rpc::ErrorCode::PayloadTooLarge,
            "payload_too_large",
            serde_json::json!({
                "pixels": pixel_count,
                "max_pixels": MAX_PIXELS,
                "hint": hint,
            }),
        ));
    }
    Ok(())
}

/// Parse the `pane.glance` `masks` param (task 080): an array of
/// `{"row":r,"col":c,"width":w}` redaction rects into a [`shux_vt::MaskSet`]. Absent →
/// empty set. A present-but-wrong-typed `masks` (not an array, or a non-object /
/// out-of-range entry) is `INVALID_PARAMS` (-32602 → CLI exit 2), never a silent skip
/// that would leave a secret unredacted.
pub(crate) fn parse_glance_masks(
    params: &serde_json::Value,
) -> Result<shux_vt::MaskSet, shux_rpc::RpcError> {
    let Some(v) = params.get("masks") else {
        return Ok(shux_vt::MaskSet::new());
    };
    if v.is_null() {
        return Ok(shux_vt::MaskSet::new());
    }
    let arr = v
        .as_array()
        .ok_or_else(|| shux_rpc::RpcError::invalid_params("masks must be an array of rects"))?;
    let mut set = shux_vt::MaskSet::new();
    let field = |o: &serde_json::Value, k: &str| -> Result<u16, shux_rpc::RpcError> {
        let n = o.get(k).and_then(|x| x.as_u64()).ok_or_else(|| {
            shux_rpc::RpcError::invalid_params(&format!("mask rect needs u16 `{k}`"))
        })?;
        u16::try_from(n)
            .map_err(|_| shux_rpc::RpcError::invalid_params(&format!("mask `{k}` exceeds u16")))
    };
    for rect in arr {
        let row = field(rect, "row")?;
        let col = field(rect, "col")?;
        let width = field(rect, "width")?;
        // A zero-width mask redacts nothing — `MaskSet::with` would silently DROP it,
        // turning an intended redaction into an unmasked glance. Reject it (matching the
        // CLI's `parse_mask_rect`) so a typo fails loudly instead of leaking (council
        // impl-review MAJOR).
        if width == 0 {
            return Err(shux_rpc::RpcError::invalid_params(
                "mask width must be > 0 (a zero-width mask redacts nothing)",
            ));
        }
        set = set.with(row, col, width);
    }
    Ok(set)
}

/// The lens `pane.glance` text of a SINGLE grid row (LENS-R-012 byte-stability,
/// per-row): ANSI-free, wide-continuation cells skipped, full-width, trailing
/// whitespace preserved (no trim). Byte-identical to `Grid::glance_text`'s
/// `row`-th line so `changed_row_text[row]` lines up with the glance text.
pub(crate) fn glance_row_text(grid: &shux_vt::Grid, row_idx: usize) -> String {
    let row = grid.visible_row(row_idx);
    let mut line = String::with_capacity(grid.cols());
    for col in 0..row.len() {
        if let Some(cell) = row.get(col) {
            if cell.is_wide_continuation() {
                continue;
            }
            cell.push_display_text(&mut line);
        }
    }
    line
}

/// Render the `pane.diff_since` heat PNG (LENS-R-037): the current clone
/// through the standard rasterizer, then changed cells overlaid with
/// `rgba(163,38,56,128)` and unchanged cells desaturated 50%. Deterministic
/// integer math end-to-end (same inputs → byte-identical PNG). Runs on a
/// blocking worker; the base render intentionally draws no cursor (cursor is
/// excluded from the diff, so a cursor block would only add noise).
pub(crate) fn render_lens_heat_png(
    rasterizer: &shux_raster::Rasterizer,
    grid: &shux_vt::Grid,
    default_colors: shux_vt::TerminalDefaultColors,
    changed_mask: &[bool],
    rows: usize,
    cols: usize,
) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;

    let opts = shux_raster::RasterOptions {
        cursor: None,
        cursor_shape: shux_vt::CursorShape::default(),
        cursor_color: default_colors.cursor,
        fg_default: default_colors
            .fg
            .unwrap_or_else(|| shux_raster::RasterOptions::default().fg_default),
        bg_default: default_colors
            .bg
            .unwrap_or_else(|| shux_raster::RasterOptions::default().bg_default),
    };
    let mut img = rasterizer.render(grid, &opts);
    let (cw, ch) = rasterizer.cell_size();
    let (iw, ih) = (img.width(), img.height());

    // Overlay foreground colour + alpha for changed cells (LENS-R-037).
    const HEAT: [u32; 3] = [163, 38, 56];
    const ALPHA: u32 = 128;

    for r in 0..rows {
        for c in 0..cols {
            let cell_changed = changed_mask.get(r * cols + c).copied().unwrap_or(false);
            let x0 = c as u32 * cw;
            let y0 = r as u32 * ch;
            for y in y0..(y0 + ch).min(ih) {
                for x in x0..(x0 + cw).min(iw) {
                    let px = img.get_pixel_mut(x, y);
                    let [pr, pg, pb, _pa] = px.0;
                    if cell_changed {
                        // Alpha-blend HEAT over the pixel: integer, truncating.
                        px.0[0] = ((HEAT[0] * ALPHA + pr as u32 * (255 - ALPHA)) / 255) as u8;
                        px.0[1] = ((HEAT[1] * ALPHA + pg as u32 * (255 - ALPHA)) / 255) as u8;
                        px.0[2] = ((HEAT[2] * ALPHA + pb as u32 * (255 - ALPHA)) / 255) as u8;
                    } else {
                        // Desaturate 50%: move each channel halfway to luma.
                        // Weights 77/150/29 sum to 256 (≈ Rec.601), >>8.
                        let gray = (pr as u32 * 77 + pg as u32 * 150 + pb as u32 * 29) >> 8;
                        px.0[0] = ((pr as u32 + gray) / 2) as u8;
                        px.0[1] = ((pg as u32 + gray) / 2) as u8;
                        px.0[2] = ((pb as u32 + gray) / 2) as u8;
                    }
                }
            }
        }
    }

    let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    let encoder = image::codecs::png::PngEncoder::new(&mut buf);
    encoder
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| format!("heat PNG encode failed: {e}"))?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    /// PR #91 codex P1 — the shared pre-render pixel budget predicate: the
    /// SAME 16M-pixel cap `pane.glance` enforces, now also gating the diff
    /// heat path BEFORE any RGBA allocation. Over budget → -32013 with
    /// {pixels, max_pixels, hint}; under budget → Ok (no allocation happens
    /// in the guard itself).
    #[test]
    fn lens_pixel_budget_check_guard_predicate() {
        // Under budget: an 80×24 pane at 9×18px cells is ~311K pixels.
        assert!(lens_pixel_budget_check(80, 24, 9, 18, "hint").is_ok());

        // Over budget: 1000×1000 cells at 9×18px is 162M pixels — the
        // pane.set_size-valid size from the codex P1 report.
        let err = lens_pixel_budget_check(1000, 1000, 9, 18, "set heat_png=false")
            .expect_err("162M pixels must exceed the 16M budget");
        assert_eq!(err.code, shux_rpc::ErrorCode::PayloadTooLarge.code());
        let data = err.data.expect("budget error carries data");
        let pixels = data["pixels"].as_u64().expect("pixels");
        let max = data["max_pixels"].as_u64().expect("max_pixels");
        assert_eq!(pixels, 162_000_000);
        assert_eq!(max, 16_000_000);
        assert!(pixels > max);
        assert_eq!(data["hint"], "set heat_png=false", "hint passes through");
    }

    // The cell-diff semantics (default-color resolution, unchanged-defaults ==
    // raw Cell equality, wide-glyph pairing) moved to `shux-vt` with the
    // comparator in task 079; they are pinned by `shux_vt::diff::tests` and by
    // the frozen parity corpus (`crates/shux/tests/lens_gate_parity.rs`).

    /// LENS-R-038b test (c), unit half — the heat base is rendered with the
    /// defaults PASSED IN (the handler passes the pane's CURRENT defaults).
    /// Deterministic integer expectations: a changed blank cell is the heat
    /// colour alpha-blended over the passed bg default; an unchanged blank
    /// cell is that bg desaturated 50% (Rec.601 luma).
    #[test]
    fn heat_png_base_uses_passed_defaults() {
        let raster = shux_raster::Rasterizer::new(14.0).expect("bundled font");
        let vt = shux_vt::VirtualTerminal::new(2, 4);
        let grid = vt.grid().clone_visible();
        let mut mask = vec![false; 2 * 4];
        mask[0] = true; // (0,0) changed; (0,1) unchanged

        let defaults = shux_vt::TerminalDefaultColors {
            bg: Some([32, 64, 96]),
            ..shux_vt::TerminalDefaultColors::default()
        };
        let png = render_lens_heat_png(&raster, &grid, defaults, &mask, 2, 4).unwrap();
        let img = image::load_from_memory(&png)
            .expect("decode heat")
            .to_rgba8();
        let (cw, _ch) = raster.cell_size();

        // Changed cell (0,0): blend(HEAT=(163,38,56), α=128) over (32,64,96)
        // with truncating integer math = (97, 50, 75).
        let p = img.get_pixel(1, 1);
        assert_eq!((p[0], p[1], p[2]), (97, 50, 75), "heat over CURRENT bg");

        // Unchanged cell (0,1): desaturate((32,64,96)) — gray=(32·77+64·150+
        // 96·29)>>8 = 58 → ((32+58)/2, (64+58)/2, (96+58)/2) = (45, 61, 77).
        let p = img.get_pixel(cw + 1, 1);
        assert_eq!(
            (p[0], p[1], p[2]),
            (45, 61, 77),
            "desaturated CURRENT bg on unchanged cells"
        );
        // Same render with the builtin default bg (None) must differ — the
        // base provably derives from the passed defaults, not a constant.
        let png_builtin = render_lens_heat_png(
            &raster,
            &grid,
            shux_vt::TerminalDefaultColors::default(),
            &mask,
            2,
            4,
        )
        .unwrap();
        assert_ne!(png, png_builtin);
    }

    /// P4 DoD (council D2) — the diff is independent of `DirtyState`: it reads
    /// cell VALUES from `clone_visible` clones, never the render-drained dirty
    /// flags. Simulate a concurrently-attached render client by DRAINING the
    /// VT's dirty regions between the checkpoint clone and the current clone;
    /// the diff still reports the exact delta. (Name preserved: referenced by
    /// `crates/shux/tests/diff_concurrent_readers.rs`; now drives the task-079
    /// `shux_vt::diff_frames` through the same `GridFrame` adapter the daemon uses.)
    #[test]
    fn compute_lens_diff_independent_of_dirtystate_drains() {
        let mut vt = shux_vt::VirtualTerminal::new(6, 20);
        // Frame A: a truecolor 'X' at grid (1,1).
        vt.process(b"\x1b[2;2H\x1b[38;2;220;40;40mX\x1b[0m");
        let cp_grid = vt.grid().clone_visible();
        let cp_cursor = {
            let c = vt.cursor();
            shux_vt::CursorState {
                row: c.row,
                col: c.col,
                visible: c.visible,
            }
        };
        // A render client drains DirtyState (as the attach compositor would).
        let _ = vt.take_dirty_regions();
        assert!(!vt.is_dirty(), "drain cleared dirty flags");

        // Frame B: recolour that SAME cell (style-only) + add a second cell.
        vt.process(b"\x1b[2;2H\x1b[38;2;40;210;210mX\x1b[0m\x1b[3;5H\x1b[44mZ\x1b[0m");
        // Client drains AGAIN mid-flight — the diff must not care.
        let _ = vt.take_dirty_regions();

        let cur_grid = vt.grid().clone_visible();
        let cur_cursor = {
            let c = vt.cursor();
            shux_vt::CursorState {
                row: c.row,
                col: c.col,
                visible: c.visible,
            }
        };
        let d = shux_vt::TerminalDefaultColors::default();
        let a = shux_vt::GridFrame::new(&cp_grid, d, cp_cursor, false);
        let b = shux_vt::GridFrame::new(&cur_grid, d, cur_cursor, false);
        let diff = shux_vt::diff_frames(&a, &b);
        // (1,1) style change + (2,4) new glyph = exactly 2 cells, despite the
        // dirty drains straddling the checkpoint.
        assert_eq!(diff.cells_changed, 2, "value-based diff, dirty-independent");
        assert_eq!(diff.changed_rows, vec![1, 2]);
        assert!(diff.changed_mask[20 + 1], "recoloured cell (1,1) counts");
        assert!(diff.changed_mask[2 * 20 + 4], "new cell (2,4) counts");
        assert!(!diff.regions_truncated);
        // Half-open bbox spanning rows 1..3, cols 1..5.
        assert_eq!(diff.bounding_box, (1, 1, 3, 5));
    }

    /// LENS-R-037 heat PNG is deterministic: identical inputs → byte-identical
    /// PNG (the golden-stability contract).
    #[test]
    fn heat_png_is_deterministic() {
        let raster = shux_raster::Rasterizer::new(14.0).expect("bundled font");
        let mut vt = shux_vt::VirtualTerminal::new(4, 10);
        vt.process(b"\x1b[1;1H\x1b[41mAB\x1b[0m");
        let grid = vt.grid().clone_visible();
        let mask = {
            let mut m = vec![false; 4 * 10];
            m[0] = true; // mark (0,0) changed
            m
        };
        let a = render_lens_heat_png(&raster, &grid, vt.default_colors(), &mask, 4, 10).unwrap();
        let b = render_lens_heat_png(&raster, &grid, vt.default_colors(), &mask, 4, 10).unwrap();
        assert_eq!(a, b, "same inputs → byte-identical heat PNG");
        assert!(!a.is_empty());
    }
}
