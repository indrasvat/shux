//! Lens-gate PIXEL / EXACT tiers + golden render + font fingerprint (task 080).
//!
//! This is the raster half of the compare (design-review D1): the cell tier +
//! [`Fingerprint`](shux_vt::Fingerprint) live in `shux-vt` (importable by the frozen
//! contract tests); rendering a golden, comparing PNGs, and fingerprinting the bundled
//! font chain live HERE — the lowest crate that can render AND that those tests import.
//!
//! Two load-bearing rulings:
//!
//! - **Conjunctive tiers (D2).** [`evaluate_tier`] runs the CELL compare FIRST and
//!   treats it as authoritative: a semantic cell fail is never overridden by a matching
//!   PNG. The cell tier gates visible cells, cursor position/visibility, geometry, palette
//!   portability, AND the alt/primary screen flag (`compare_cell`); the `pixel`/`exact`
//!   PNG check is an ADDITIONAL gate on top that catches what the cell tier is still blind
//!   to — a cursor-SHAPE change or a font-fallback pixel difference (same cells, different
//!   glyphs).
//! - **Mask before render (D4).** Every rendered gate artifact goes through
//!   [`FrameEnvelope::to_grid`](shux_vt::FrameEnvelope::to_grid) on the ALREADY-MASKED
//!   envelope, whose masked cells decode to the styleless `▮` placeholder — a secret can
//!   never reach a rendered PNG or heat overlay. This module never renders a raw live
//!   `Grid`.
//!
//! The RGBA compare productizes `.claude/automations/pixel_verify.py` (no shelling): a
//! per-channel int16 abs-delta, a size mismatch is a hard fail, and `changed_frac` /
//! `max_channel_delta` gate against a [`TolParams`](shux_vt::TolParams).

use image::RgbaImage;
use serde::{Deserialize, Serialize};
use shux_vt::{
    CellVerdict, CursorShape, FrameEnvelope, GateStatus, Tier, TolParams, compare_cell, sha256_hex,
};

use crate::{DEFAULT_FALLBACK_FONT_SPECS, RasterOptions, Rasterizer, builtin_font_bytes};

/// Errors from a tier evaluation — a malformed golden or a render/decode failure. Kept
/// distinct from a `GateStatus` so the caller (081) can classify (`scenario_error` /
/// `infra_error`) rather than confusing it with a `fail`.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GateError {
    #[error("golden capture is not canonical: {0}")]
    MalformedGolden(String),
    #[error("live capture is not canonical: {0}")]
    MalformedLive(String),
    #[error("render failed: {0}")]
    Render(String),
    #[error("PNG decode failed: {0}")]
    Decode(String),
    #[error("{tier:?} tier requires a committed PNG baseline, none was provided")]
    MissingBaseline { tier: Tier },
}

/// The stable, ordered fingerprint of the bundled font chain (task 080
/// `raster_font_fingerprint`). SHA-256 over each builtin font asset's bytes in
/// [`DEFAULT_FALLBACK_FONT_SPECS`] order, plus the render font size — so a font-asset
/// swap, a re-ordering, or a size change all invalidate a golden (a stale trigger).
/// Bundled-only (no host-local fonts), so it is identical on every platform for the same
/// build → a cell golden's sidecar is portable.
pub fn builtin_font_fingerprint(font_size: f32) -> String {
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(b"raster-font:v1;size=");
    buf.extend_from_slice(&font_size.to_le_bytes());
    for spec in DEFAULT_FALLBACK_FONT_SPECS {
        buf.extend_from_slice(spec.as_bytes());
        buf.push(b':');
        // Unwrap is safe: DEFAULT_FALLBACK_FONT_SPECS are all builtin tokens.
        let bytes = builtin_font_bytes(spec).unwrap_or(&[]);
        buf.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        buf.extend_from_slice(bytes);
    }
    sha256_hex(&buf)
}

/// Render a MASKED [`FrameEnvelope`] to an RGBA image (design-review D4: through
/// `to_grid`, never a raw live grid). Draws the cursor from the envelope (position +
/// shape + visibility) — a cursor-shape change is a pixel-tier-only signal — and
/// resolves `Color::Default` against the envelope's OSC 10/11/12 defaults.
pub fn render_envelope(rasterizer: &Rasterizer, env: &FrameEnvelope) -> RgbaImage {
    let grid = env.to_grid();
    let render_cursor = env.cursor.visible;
    let cursor_pos = render_cursor.then_some((env.cursor.row as usize, env.cursor.col as usize));
    let cursor_shape = if render_cursor {
        env.cursor.shape.to_vt()
    } else {
        CursorShape::default()
    };
    let default_opts = RasterOptions::default();
    let opts = RasterOptions {
        fg_default: env.defaults.fg.unwrap_or(default_opts.fg_default),
        bg_default: env.defaults.bg.unwrap_or(default_opts.bg_default),
        cursor: cursor_pos,
        cursor_shape,
        cursor_color: env.defaults.cursor,
    };
    rasterizer.render(&grid, &opts)
}

/// PNG-encode an RGBA image (deterministic — integer math end-to-end, same input → same
/// bytes).
pub fn encode_png(img: &RgbaImage) -> Result<Vec<u8>, GateError> {
    use image::ImageEncoder;
    let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    image::codecs::png::PngEncoder::new(&mut buf)
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| GateError::Render(e.to_string()))?;
    Ok(buf)
}

/// Render a masked envelope straight to PNG bytes (the gate's pixel/exact capture PNG).
pub fn render_envelope_png(
    rasterizer: &Rasterizer,
    env: &FrameEnvelope,
) -> Result<Vec<u8>, GateError> {
    encode_png(&render_envelope(rasterizer, env))
}

/// SHA-256 of PNG bytes — the EXACT tier's content pin (byte-identity).
pub fn png_sha256(png_bytes: &[u8]) -> String {
    sha256_hex(png_bytes)
}

/// SHA-256 of an image's RAW (uncompressed) RGBA buffer — the PIXEL tier's content pin.
/// Encoder-stable, unlike [`png_sha256`] (a PNG re-encode can change compressed bytes
/// while pixels are identical; council fp change).
pub fn rgba_sha256(img: &RgbaImage) -> String {
    sha256_hex(img.as_raw())
}

/// Decode PNG bytes into an RGBA image.
pub fn decode_png(png_bytes: &[u8]) -> Result<RgbaImage, GateError> {
    Ok(image::load_from_memory(png_bytes)
        .map_err(|e| GateError::Decode(e.to_string()))?
        .to_rgba8())
}

/// Pixel-compare metrics. The RGBA delta COMPUTATION is productized from
/// `.claude/automations/pixel_verify.py` (no shelling): `changed_pixels`,
/// `total_pixels`, `pixel_diff_ratio`, and `mean_rgba_channel_delta` are byte-for-byte
/// equal to the Python oracle's fields on the same PNGs (verified in the task-080 QA
/// evidence). `max_channel_delta` is an ADDITIONAL diagnostic the oracle does not report.
///
/// The pass/fail DECISION intentionally differs from `pixel_verify.py`: this gates on the
/// task-080 §2 tolerance `{max_channel_delta (MAX per-channel), max_changed_frac (ratio)}`,
/// whereas `pixel_verify.py` gates on `{max_pixel_diff_ratio, max_mean_channel_delta
/// (MEAN)}`. A MAX-channel bound is the stricter, better visual-regression gate — a single
/// wildly-wrong pixel fails here but a MEAN bound would wash it out. Because `mean <= max`
/// always, at an equal numeric threshold this is never more permissive than the oracle
/// (never a false pass), and at the default zero tolerance the two decisions are identical.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PixelMetrics {
    /// `"pass"` iff within tolerance AND no size mismatch.
    pub status: String,
    pub size_mismatch: bool,
    pub changed_pixels: u64,
    pub total_pixels: u64,
    pub pixel_diff_ratio: f64,
    pub mean_rgba_channel_delta: f64,
    pub max_channel_delta: u16,
}

/// RGBA compare of `actual` vs `baseline` against `tol`. A size mismatch is a hard fail
/// (mirrors `pixel_verify.py` without `--allow-size-mismatch`). Otherwise: `changed_pixels`
/// = pixels with ANY channel delta; `max_channel_delta` = the largest per-channel abs
/// delta; pass iff `pixel_diff_ratio <= tol.max_changed_frac` AND `max_channel_delta <=
/// tol.max_channel_delta` (see [`PixelMetrics`] for the deliberate max-vs-mean divergence
/// from the `pixel_verify.py` decision).
pub fn compare_pixels(actual: &RgbaImage, baseline: &RgbaImage, tol: &TolParams) -> PixelMetrics {
    if actual.dimensions() != baseline.dimensions() {
        return PixelMetrics {
            status: "fail".to_string(),
            size_mismatch: true,
            changed_pixels: 0,
            total_pixels: (actual.width() as u64) * (actual.height() as u64),
            pixel_diff_ratio: 1.0,
            mean_rgba_channel_delta: f64::INFINITY,
            max_channel_delta: u16::MAX,
        };
    }
    let a = actual.as_raw();
    let b = baseline.as_raw();
    let total_pixels = (actual.width() as u64) * (actual.height() as u64);
    let mut changed_pixels: u64 = 0;
    let mut sum_delta: u64 = 0;
    let mut max_channel_delta: u16 = 0;
    // RGBA, 4 channels per pixel; both buffers are equal length (dims match).
    for px in 0..(total_pixels as usize) {
        let mut pixel_changed = false;
        for ch in 0..4 {
            let i = px * 4 + ch;
            let d = (a[i] as i16 - b[i] as i16).unsigned_abs();
            sum_delta += d as u64;
            if d > 0 {
                pixel_changed = true;
            }
            if d > max_channel_delta {
                max_channel_delta = d;
            }
        }
        if pixel_changed {
            changed_pixels += 1;
        }
    }
    let pixel_diff_ratio = if total_pixels == 0 {
        0.0
    } else {
        changed_pixels as f64 / total_pixels as f64
    };
    let mean_rgba_channel_delta = if total_pixels == 0 {
        0.0
    } else {
        sum_delta as f64 / (total_pixels as f64 * 4.0)
    };
    let passed =
        pixel_diff_ratio <= tol.max_changed_frac && max_channel_delta <= tol.max_channel_delta;
    PixelMetrics {
        status: if passed { "pass" } else { "fail" }.to_string(),
        size_mismatch: false,
        changed_pixels,
        total_pixels,
        pixel_diff_ratio,
        mean_rgba_channel_delta,
        max_channel_delta,
    }
}

/// The result of evaluating one tier for one frame: the final [`GateStatus`], the
/// authoritative cell verdict, and the pixel metrics (pixel tier only).
#[derive(Debug, Clone, PartialEq)]
pub struct TierVerdict {
    pub status: GateStatus,
    pub cell: CellVerdict,
    pub pixel: Option<PixelMetrics>,
    pub reason: Option<String>,
}

/// Evaluate `tier` for a live capture against a PRESENT, NON-STALE golden (the caller
/// resolves `missing_golden`/`stale_golden` first — design-review D6). Conjunctive
/// (D2): the cell compare is authoritative; a cell fail short-circuits and the PNG check
/// can only ADD a failure, never remove one.
///
/// - `cell`: the cell verdict is the tier verdict.
/// - `pixel`: cell must pass, then RGBA-compare the re-rendered live frame vs the golden
///   PNG within `tol`.
/// - `exact`: cell must pass, then the re-rendered live PNG must be byte-identical to the
///   golden PNG.
pub fn evaluate_tier(
    tier: Tier,
    golden_env: &FrameEnvelope,
    live_env: &FrameEnvelope,
    golden_png: Option<&[u8]>,
    rasterizer: &Rasterizer,
    tol: &TolParams,
) -> Result<TierVerdict, GateError> {
    let golden_view = golden_env
        .try_view()
        .map_err(|e| GateError::MalformedGolden(format!("{e:?}")))?;
    let live_view = live_env
        .try_view()
        .map_err(|e| GateError::MalformedLive(format!("{e:?}")))?;
    let cell = compare_cell(&golden_view, &live_view);

    if tier == Tier::Cell {
        let reason = cell.reason.clone();
        return Ok(TierVerdict {
            status: cell.status,
            cell,
            pixel: None,
            reason,
        });
    }

    // Pixel/exact: a semantic cell fail is authoritative — the PNG can never override it.
    if cell.status == GateStatus::Fail {
        let reason = cell.reason.clone();
        return Ok(TierVerdict {
            status: GateStatus::Fail,
            cell,
            pixel: None,
            reason,
        });
    }

    let golden_png = golden_png.ok_or(GateError::MissingBaseline { tier })?;
    let live_img = render_envelope(rasterizer, live_env);

    match tier {
        Tier::Exact => {
            // A corrupt/empty baseline is a BROKEN ARTIFACT (infra), not a content
            // regression — decode-check it so the error taxonomy matches the pixel arm
            // (task-080 adversarial MINOR: else garbage bytes read as an `exact_diff`
            // content fail). The decoded image is discarded; the compare is byte-exact.
            decode_png(golden_png)?;
            let live_png = encode_png(&live_img)?;
            let identical = live_png == golden_png;
            Ok(TierVerdict {
                status: if identical {
                    GateStatus::Pass
                } else {
                    GateStatus::Fail
                },
                cell,
                pixel: None,
                reason: (!identical).then(|| "exact_diff".to_string()),
            })
        }
        Tier::Pixel => {
            let golden_img = decode_png(golden_png)?;
            let metrics = compare_pixels(&live_img, &golden_img, tol);
            let passed = metrics.status == "pass";
            Ok(TierVerdict {
                status: if passed {
                    GateStatus::Pass
                } else {
                    GateStatus::Fail
                },
                cell,
                pixel: Some(metrics),
                reason: (!passed).then(|| "pixel_diff".to_string()),
            })
        }
        Tier::Cell => unreachable!("cell handled above"),
    }
}

/// The current host's `<os>-<arch>` partition key for pixel/exact baselines. Uses
/// `std::env::consts` so it matches the committed directory layout.
pub fn os_arch() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// The committed pixel/exact baseline path: `<dir>/<name>/<os>-<arch>/frame.png`. A
/// platform with no such directory yields `missing_golden` (the file simply does not
/// exist), never a silent pass (task 080 §2).
pub fn pixel_baseline_path(dir: &std::path::Path, name: &str, os_arch: &str) -> std::path::PathBuf {
    dir.join(name).join(os_arch).join("frame.png")
}

/// Convenience: the golden content pin for a rendered image, per tier — `rgba_sha256`
/// for pixel (encoder-stable), `png_sha256` for exact. Cell goldens use
/// [`capture_sha256`] on the JSON instead.
pub fn content_pin(tier: Tier, img: &RgbaImage, png_bytes: &[u8]) -> String {
    match tier {
        Tier::Exact => png_sha256(png_bytes),
        _ => rgba_sha256(img),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shux_vt::{MaskSet, VirtualTerminal, capture_sha256};

    fn env(prog: &[u8], rows: usize, cols: usize) -> FrameEnvelope {
        let mut vt = VirtualTerminal::new(rows, cols);
        vt.process(prog);
        FrameEnvelope::from_terminal(&vt, &MaskSet::new())
    }

    fn rasterizer() -> Rasterizer {
        Rasterizer::new(16.0).expect("bundled rasterizer")
    }

    // ── font fingerprint ────────────────────────────────────────────────────────
    #[test]
    fn font_fingerprint_is_deterministic_and_size_sensitive() {
        let a = builtin_font_fingerprint(16.0);
        let b = builtin_font_fingerprint(16.0);
        let c = builtin_font_fingerprint(18.0);
        assert_eq!(a, b, "same size → same fingerprint");
        assert_ne!(a, c, "font size is part of the fingerprint");
        assert_eq!(a.len(), 64);
    }

    // ── render determinism (D3: plumbing only, not golden proof) ─────────────────
    #[test]
    fn render_is_byte_deterministic_for_same_envelope() {
        let e = env(b"\x1b[38;2;255;120;0mSHUX\x1b[0m", 3, 10);
        let r = rasterizer();
        let p1 = render_envelope_png(&r, &e).unwrap();
        let p2 = render_envelope_png(&r, &e).unwrap();
        assert_eq!(p1, p2, "same envelope + rasterizer → byte-identical PNG");
        // rgba pin is stable too.
        assert_eq!(
            rgba_sha256(&render_envelope(&r, &e)),
            rgba_sha256(&render_envelope(&r, &e))
        );
    }

    // ── compare_pixels productizes pixel_verify.py ──────────────────────────────
    #[test]
    fn compare_pixels_zero_on_identical() {
        let r = rasterizer();
        let img = render_envelope(&r, &env(b"hello", 2, 8));
        let m = compare_pixels(&img, &img, &TolParams::default());
        assert_eq!(m.status, "pass");
        assert_eq!(m.changed_pixels, 0);
        assert_eq!(m.max_channel_delta, 0);
        assert_eq!(m.pixel_diff_ratio, 0.0);
        assert!(!m.size_mismatch);
    }

    #[test]
    fn compare_pixels_catches_one_subpixel_and_respects_tolerance() {
        let r = rasterizer();
        let base = render_envelope(&r, &env(b"hello", 2, 8));
        let mut actual = base.clone();
        // Flip one channel of one pixel by 3.
        let p = actual.get_pixel_mut(1, 1);
        p.0[0] = p.0[0].wrapping_add(3);
        // Zero tolerance → fail.
        let strict = compare_pixels(&actual, &base, &TolParams::default());
        assert_eq!(strict.status, "fail");
        assert_eq!(strict.changed_pixels, 1);
        assert_eq!(strict.max_channel_delta, 3);
        // Tolerance that admits a 3-delta on a small fraction → pass.
        let loose = compare_pixels(
            &actual,
            &base,
            &TolParams {
                max_channel_delta: 3,
                max_changed_frac: 1.0,
            },
        );
        assert_eq!(loose.status, "pass");
    }

    #[test]
    fn compare_pixels_size_mismatch_is_hard_fail() {
        let r = rasterizer();
        let a = render_envelope(&r, &env(b"hi", 2, 5));
        let b = render_envelope(&r, &env(b"hi", 3, 5));
        let m = compare_pixels(
            &a,
            &b,
            &TolParams {
                max_channel_delta: u16::MAX,
                max_changed_frac: 1.0,
            },
        );
        assert!(m.size_mismatch);
        assert_eq!(
            m.status, "fail",
            "size mismatch fails even at max tolerance"
        );
    }

    // ── evaluate_tier: conjunctive (D2) ─────────────────────────────────────────
    #[test]
    fn cell_tier_matches_compare_cell() {
        let g = env(b"hello", 2, 8);
        let l = env(b"hellp", 2, 8);
        let v = evaluate_tier(
            Tier::Cell,
            &g,
            &l,
            None,
            &rasterizer(),
            &TolParams::default(),
        )
        .unwrap();
        assert_eq!(v.status, GateStatus::Fail);
        assert!(v.pixel.is_none());
    }

    #[test]
    fn exact_tier_passes_on_identical_and_fails_on_perturbed_png() {
        let r = rasterizer();
        let g = env(b"\x1b[32mSHUX\x1b[0m", 2, 8);
        let golden_png = render_envelope_png(&r, &g).unwrap();
        // identical live → pass
        let ok = evaluate_tier(
            Tier::Exact,
            &g,
            &g,
            Some(&golden_png),
            &r,
            &TolParams::default(),
        )
        .unwrap();
        assert_eq!(ok.status, GateStatus::Pass);
        // a golden PNG that differs by a byte → exact fail (cells still match)
        let mut bad = golden_png.clone();
        *bad.last_mut().unwrap() ^= 0x01;
        let fail = evaluate_tier(Tier::Exact, &g, &g, Some(&bad), &r, &TolParams::default());
        // decoding a corrupted PNG tail may error; a truncated-vs-valid mismatch is a
        // fail. Accept either the decode error path OR a clean exact_diff.
        match fail {
            Ok(v) => {
                assert_eq!(v.status, GateStatus::Fail);
                assert_eq!(v.reason.as_deref(), Some("exact_diff"));
            }
            Err(GateError::Decode(_)) => {}
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn pixel_tier_needs_baseline_and_passes_on_match() {
        let r = rasterizer();
        let g = env(b"\x1b[34mSHUX\x1b[0m", 2, 8);
        let baseline = render_envelope_png(&r, &g).unwrap();
        let ok = evaluate_tier(
            Tier::Pixel,
            &g,
            &g,
            Some(&baseline),
            &r,
            &TolParams::default(),
        )
        .unwrap();
        assert_eq!(ok.status, GateStatus::Pass);
        assert_eq!(ok.pixel.as_ref().unwrap().changed_pixels, 0);
        // Missing baseline → error (caller should have raised missing_golden first).
        let missing = evaluate_tier(Tier::Pixel, &g, &g, None, &r, &TolParams::default());
        assert!(matches!(missing, Err(GateError::MissingBaseline { .. })));
    }

    #[test]
    fn pixel_tier_never_overrides_a_cell_fail() {
        // Cells differ (regression) but we hand the pixel tier a golden PNG of the LIVE
        // frame so the PNG "matches" — the cell fail must still surface (D2).
        let r = rasterizer();
        let golden = env(b"hello", 2, 8);
        let live = env(b"hellp", 2, 8);
        let live_png = render_envelope_png(&r, &live).unwrap();
        let v = evaluate_tier(
            Tier::Pixel,
            &golden,
            &live,
            Some(&live_png),
            &r,
            &TolParams::default(),
        )
        .unwrap();
        assert_eq!(
            v.status,
            GateStatus::Fail,
            "a matching PNG must not hide a cell regression"
        );
        assert!(
            v.pixel.is_none(),
            "cell fail short-circuits before the PNG check"
        );
    }

    #[test]
    fn os_arch_and_baseline_path_layout() {
        let oa = os_arch();
        assert!(oa.contains('-'), "os-arch is hyphenated, got {oa:?}");
        let p = pixel_baseline_path(std::path::Path::new("/goldens"), "demo", &oa);
        assert!(p.ends_with("frame.png"));
        assert!(p.to_string_lossy().contains(&format!("demo/{oa}")));
    }

    // ── task-080 adversarial: exact-tier corrupt baseline is an infra Decode error ──
    #[test]
    fn exact_tier_corrupt_baseline_is_decode_error_not_content_fail() {
        let r = rasterizer();
        let g = env(b"\x1b[32mSHUX\x1b[0m", 2, 8);
        // A cell-PASS scenario, but the committed baseline is garbage — that is a broken
        // artifact (infra), not a content regression. Both tiers must agree on Decode.
        for bad in [&b""[..], &b"\x89PNGnot-really"[..], &b"plain text"[..]] {
            let px = evaluate_tier(Tier::Pixel, &g, &g, Some(bad), &r, &TolParams::default());
            assert!(matches!(px, Err(GateError::Decode(_))), "pixel: {bad:?}");
            let ex = evaluate_tier(Tier::Exact, &g, &g, Some(bad), &r, &TolParams::default());
            assert!(
                matches!(ex, Err(GateError::Decode(_))),
                "exact must classify a corrupt baseline as Decode, not exact_diff: {bad:?}"
            );
        }
    }

    // ── task-080 adversarial: the pixel gate is MAX-channel (stricter than the mean) ──
    #[test]
    fn pixel_gate_is_max_channel_not_mean() {
        let r = rasterizer();
        let base = render_envelope(&r, &env(b"hello world", 3, 20));
        let mut actual = base.clone();
        // One channel of one pixel off by 40 — a large LOCAL delta with a tiny MEAN.
        let p = actual.get_pixel_mut(2, 1);
        p.0[1] = p.0[1].wrapping_add(40);
        let m = compare_pixels(&actual, &base, &TolParams::default());
        assert_eq!(m.max_channel_delta, 40);
        assert!(m.mean_rgba_channel_delta < 1.0, "the MEAN is tiny");
        // A mean-based gate at threshold 1 would PASS; the max-channel gate FAILS at the
        // same numeric threshold — the deliberate stricter behavior.
        let tol = TolParams {
            max_channel_delta: 1,
            max_changed_frac: 1.0,
        };
        assert_eq!(compare_pixels(&actual, &base, &tol).status, "fail");
        // Admitting the 40-delta passes.
        let tol_ok = TolParams {
            max_channel_delta: 40,
            max_changed_frac: 1.0,
        };
        assert_eq!(compare_pixels(&actual, &base, &tol_ok).status, "pass");
    }

    #[test]
    fn content_pin_selects_rgba_or_png_by_tier() {
        let r = rasterizer();
        let e = env(b"pin", 1, 4);
        let img = render_envelope(&r, &e);
        let png = encode_png(&img).unwrap();
        assert_eq!(content_pin(Tier::Pixel, &img, &png), rgba_sha256(&img));
        assert_eq!(content_pin(Tier::Exact, &img, &png), png_sha256(&png));
        // capture_sha256 is the cell-tier pin (JSON, not pixels).
        assert_eq!(capture_sha256(&e).len(), 64);
    }
}
