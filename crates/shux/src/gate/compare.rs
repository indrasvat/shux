//! Productized golden compare (design D2). Mirrors the 080 `gate_status` template
//! (`crates/shux/tests/lens_gate_compare.rs`) with NO drift, then adapts the internal
//! `GateStatus` to a runner signal — 081 never emits a frozen status name.
//!
//! Resolution order (080 D6): resolve golden by tier → `golden_absent` (missing) /
//! `golden_untrusted` (stale sidecar OR content-pin tamper) → conjunctive
//! `evaluate_tier`. The tolerance is bound to the BLESSED sidecar (`sidecar.tol_params`),
//! NEVER a runtime value (080 adversarial: a loosened runtime tol must not slip a
//! regression past without tripping stale).

use std::path::Path;

use shux_raster::{Rasterizer, TierVerdict, evaluate_tier, os_arch, pixel_baseline_path};
use shux_vt::{
    Fingerprint, FrameEnvelope, GateStatus, StyleDelta, Tier, capture_sha256, style_deltas,
};

use super::signal::RunnerSignal;

/// The raw compare result for one frame: the trace SIGNAL (081's wire vocabulary) plus
/// the rich `TierVerdict` (regions + pixel metrics) the signal loses — `None` when no
/// compare ran (golden absent/untrusted). 082 shapes `verdict` into a `DiffReport`.
pub struct FrameCompare {
    pub signal: RunnerSignal,
    pub verdict: Option<TierVerdict>,
    /// Expected-vs-actual style at changed cells (084 F6) — computed here because this is
    /// where BOTH envelopes are in hand.
    pub style_deltas: Vec<StyleDelta>,
}

/// The golden files for a frame live at `<golden_dir>/<name>.capture.json` +
/// `<name>.fingerprint.json` (+ `<name>/<os>-<arch>/frame.png` for pixel/exact).
pub fn cell_json_path(dir: &Path, name: &str) -> std::path::PathBuf {
    dir.join(format!("{name}.capture.json"))
}
pub fn fp_path(dir: &Path, name: &str) -> std::path::PathBuf {
    dir.join(format!("{name}.fingerprint.json"))
}

fn tier_name(t: Tier) -> String {
    match t {
        Tier::Cell => "cell",
        Tier::Pixel => "pixel",
        Tier::Exact => "exact",
    }
    .to_string()
}

/// Compare a live captured frame against its golden and return the RAW runner signal
/// (design D2) PLUS the rich verdict (design D1). `current` is the freshly-computed
/// fingerprint for THIS build/scenario (with the real `scenario_hash`/`cmd_env_hash` 081
/// populates).
pub fn compare_frame(
    golden_dir: &Path,
    name: &str,
    tier: Tier,
    live: &FrameEnvelope,
    current: &Fingerprint,
    rasterizer: &Rasterizer,
) -> FrameCompare {
    let tname = tier_name(tier);
    // The two no-compare dispositions, as a `FrameCompare` with `verdict: None`.
    let absent = || FrameCompare {
        signal: RunnerSignal::GoldenAbsent {
            name: name.into(),
            tier: tname.clone(),
        },
        verdict: None,
        style_deltas: Vec::new(),
    };
    let untrusted = || FrameCompare {
        signal: RunnerSignal::GoldenUntrusted {
            name: name.into(),
            tier: tname.clone(),
        },
        verdict: None,
        style_deltas: Vec::new(),
    };

    let json_path = cell_json_path(golden_dir, name);
    let fp_path = fp_path(golden_dir, name);
    if !json_path.exists() || !fp_path.exists() {
        return absent();
    }
    let Ok(golden_text) = std::fs::read_to_string(&json_path) else {
        return untrusted();
    };
    let Ok(golden) = FrameEnvelope::from_canonical_json(&golden_text) else {
        return untrusted();
    };
    let Ok(sidecar_text) = std::fs::read_to_string(&fp_path) else {
        return untrusted();
    };
    let Ok(sidecar) = serde_json::from_str::<Fingerprint>(&sidecar_text) else {
        return untrusted();
    };

    // Stale: build/config drift OR a golden edited without re-bless (080 D6).
    if sidecar.is_stale_vs(current) || sidecar.capture_sha256 != capture_sha256(&golden) {
        return untrusted();
    }

    // Pixel/exact: resolve + content-pin the committed platform baseline.
    let golden_png = if tier != Tier::Cell {
        let p = pixel_baseline_path(golden_dir, name, &os_arch());
        if !p.exists() {
            return absent();
        }
        let Ok(bytes) = std::fs::read(&p) else {
            return untrusted();
        };
        // Enforce the baseline CONTENT PIN (080 impl-review BLOCKER): a swapped-but-valid
        // PNG must be refused; `capture_sha256` only pins the cell JSON.
        let pin_ok = match tier {
            Tier::Exact => sidecar.png_sha256.as_deref() == Some(&shux_raster::png_sha256(&bytes)),
            Tier::Pixel => match shux_raster::decode_png(&bytes) {
                Ok(img) => sidecar.rgba_sha256.as_deref() == Some(&shux_raster::rgba_sha256(&img)),
                Err(_) => false,
            },
            Tier::Cell => true,
        };
        if !pin_ok {
            return untrusted();
        }
        Some(bytes)
    } else {
        None
    };

    // Bind the tolerance to the BLESSED sidecar, never a runtime value (080 wiring).
    match evaluate_tier(
        tier,
        &golden,
        live,
        golden_png.as_deref(),
        rasterizer,
        &sidecar.tol_params,
    ) {
        Ok(v) => match v.status {
            GateStatus::Pass => FrameCompare {
                signal: RunnerSignal::FrameMatch {
                    name: name.into(),
                    tier: tname,
                },
                verdict: Some(v),
                style_deltas: Vec::new(),
            },
            GateStatus::Fail => FrameCompare {
                signal: RunnerSignal::FrameMismatch {
                    name: name.into(),
                    tier: tname,
                    reason: v.reason.clone(),
                    changed_cells: Some(v.cell.diff.cells_changed),
                },
                verdict: Some(v),
                // Only a FAIL needs the style story; a match has none.
                style_deltas: style_deltas(&golden, live),
            },
            // evaluate_tier only yields Pass/Fail (missing/stale resolved above); any other
            // status is refused conservatively as untrusted rather than silently passed.
            _ => untrusted(),
        },
        // A malformed golden/live at compare time is untrusted, never a silent pass.
        Err(_) => untrusted(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shux_vt::{
        FINGERPRINT_SCHEMA, MaskSet, RENDERER_FORMAT_VERSION, SCHEMA_VERSION, TolParams,
        VirtualTerminal, mask_hash, unicode_width_version,
    };

    fn rasterizer() -> Rasterizer {
        Rasterizer::new(16.0).expect("bundled rasterizer")
    }

    fn env(prog: &[u8], rows: usize, cols: usize) -> FrameEnvelope {
        let mut vt = VirtualTerminal::new(rows, cols);
        vt.process(prog);
        FrameEnvelope::from_terminal(&vt, &MaskSet::new())
    }

    fn current_fp(tier: Tier) -> Fingerprint {
        Fingerprint {
            fp_schema: FINGERPRINT_SCHEMA,
            schema: SCHEMA_VERSION,
            renderer_format_version: RENDERER_FORMAT_VERSION,
            raster_font_fingerprint: shux_raster::builtin_font_fingerprint(16.0),
            unicode_width_ver: unicode_width_version(),
            tol: tier,
            tol_params: TolParams::default(),
            mask_hash: mask_hash(&MaskSet::new()),
            platform: (tier != Tier::Cell).then(os_arch),
            shux_version: "test".into(),
            capture_sha256: String::new(),
            rgba_sha256: None,
            png_sha256: None,
            scenario_hash: "scn".into(),
            cmd_env_hash: "cmd".into(),
        }
    }

    fn bless_cell(dir: &Path, name: &str, golden: &FrameEnvelope) {
        std::fs::write(cell_json_path(dir, name), golden.to_canonical_json()).unwrap();
        let mut fp = current_fp(Tier::Cell);
        fp.capture_sha256 = capture_sha256(golden);
        std::fs::write(
            fp_path(dir, name),
            serde_json::to_string_pretty(&fp).unwrap(),
        )
        .unwrap();
    }

    fn bless_pixel(dir: &Path, name: &str, golden: &FrameEnvelope, tier: Tier, r: &Rasterizer) {
        std::fs::write(cell_json_path(dir, name), golden.to_canonical_json()).unwrap();
        let img = shux_raster::render_envelope(r, golden);
        let png = shux_raster::encode_png(&img).unwrap();
        let mut fp = current_fp(tier);
        fp.capture_sha256 = capture_sha256(golden);
        fp.rgba_sha256 = Some(shux_raster::rgba_sha256(&img));
        fp.png_sha256 = Some(shux_raster::png_sha256(&png));
        std::fs::write(
            fp_path(dir, name),
            serde_json::to_string_pretty(&fp).unwrap(),
        )
        .unwrap();
        let p = pixel_baseline_path(dir, name, &os_arch());
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, &png).unwrap();
    }

    #[test]
    fn match_absent_mismatch_untrusted_signals() {
        let dir = tempfile::tempdir().unwrap();
        let r = rasterizer();
        let golden = env(b"\x1b[38;2;9;9;9mhello\x1b[0m", 3, 20);

        // No golden → golden_absent.
        let s = compare_frame(
            dir.path(),
            "demo",
            Tier::Cell,
            &golden,
            &current_fp(Tier::Cell),
            &r,
        );
        assert_eq!(s.signal.kind(), "golden_absent");

        bless_cell(dir.path(), "demo", &golden);
        // Identical → frame_match.
        let s = compare_frame(
            dir.path(),
            "demo",
            Tier::Cell,
            &golden,
            &current_fp(Tier::Cell),
            &r,
        );
        assert_eq!(s.signal.kind(), "frame_match");

        // One-cell change → frame_mismatch with changed_cells.
        let live = env(b"\x1b[38;2;9;9;9mhellO\x1b[0m", 3, 20);
        let s = compare_frame(
            dir.path(),
            "demo",
            Tier::Cell,
            &live,
            &current_fp(Tier::Cell),
            &r,
        );
        assert_eq!(s.signal.kind(), "frame_mismatch");
        if let RunnerSignal::FrameMismatch { changed_cells, .. } = s.signal {
            assert_eq!(changed_cells, Some(1));
        } else {
            panic!("expected mismatch");
        }
    }

    #[test]
    fn font_bump_is_golden_untrusted_not_a_pass() {
        let dir = tempfile::tempdir().unwrap();
        let r = rasterizer();
        let golden = env(b"themed", 2, 12);
        bless_cell(dir.path(), "t", &golden);
        let mut stale = current_fp(Tier::Cell);
        stale.raster_font_fingerprint = "different-build".into();
        let s = compare_frame(dir.path(), "t", Tier::Cell, &golden, &stale, &r);
        assert_eq!(s.signal.kind(), "golden_untrusted");
    }

    #[test]
    fn tampered_golden_is_untrusted() {
        let dir = tempfile::tempdir().unwrap();
        let r = rasterizer();
        let golden = env(b"original", 2, 12);
        bless_cell(dir.path(), "t", &golden);
        // Edit the golden JSON without re-blessing the sidecar → content pin mismatch.
        let edited = env(b"tampered", 2, 12);
        std::fs::write(cell_json_path(dir.path(), "t"), edited.to_canonical_json()).unwrap();
        let s = compare_frame(
            dir.path(),
            "t",
            Tier::Cell,
            &edited,
            &current_fp(Tier::Cell),
            &r,
        );
        assert_eq!(s.signal.kind(), "golden_untrusted");
    }

    #[test]
    fn pixel_exact_baselines_are_pin_enforced_against_tamper() {
        // adv C recommendation: the runner adapter must ALSO enforce the content pin (a
        // swapped-but-valid baseline PNG → golden_untrusted), not rely solely on the 080
        // test. Mirrors `pixel_and_exact_baselines_are_pin_enforced_against_tamper`.
        let dir = tempfile::tempdir().unwrap();
        let r = rasterizer();
        let other =
            shux_raster::render_envelope_png(&r, &env(b"\x1b[31mTAMPERED\x1b[0m", 2, 10)).unwrap();
        for tier in [Tier::Pixel, Tier::Exact] {
            let golden = env(b"\x1b[34mORIGINAL\x1b[0m", 2, 10);
            bless_pixel(dir.path(), "pin", &golden, tier, &r);
            let cur = current_fp(tier);
            // Valid baseline → frame_match.
            assert_eq!(
                compare_frame(dir.path(), "pin", tier, &golden, &cur, &r)
                    .signal
                    .kind(),
                "frame_match"
            );
            // Swap the committed PNG with a DIFFERENT valid PNG (cell JSON + sidecar
            // untouched) → the content pin must refuse it as golden_untrusted.
            let p = pixel_baseline_path(dir.path(), "pin", &os_arch());
            std::fs::write(&p, &other).unwrap();
            assert_eq!(
                compare_frame(dir.path(), "pin", tier, &golden, &cur, &r)
                    .signal
                    .kind(),
                "golden_untrusted",
                "{tier:?}: a swapped baseline PNG must be refused via its content pin"
            );
            std::fs::remove_dir_all(dir.path().join("pin")).unwrap();
            std::fs::remove_file(cell_json_path(dir.path(), "pin")).unwrap();
            std::fs::remove_file(fp_path(dir.path(), "pin")).unwrap();
        }
    }

    #[test]
    fn tolerance_comes_from_sidecar_not_runtime() {
        // Bless with a strict (zero) tol; a live pixel differences would fail. The runtime
        // `current` fp carries a loose tol, but the compare uses the SIDECAR's — proving a
        // runtime tol cannot slip a regression through. (Cell tier here just proves the
        // sidecar is the source; pixel divergence is covered in lens_gate_compare.)
        let dir = tempfile::tempdir().unwrap();
        let r = rasterizer();
        let golden = env(b"abc", 2, 8);
        bless_cell(dir.path(), "t", &golden);
        let mut loose = current_fp(Tier::Cell);
        loose.tol_params = TolParams {
            max_channel_delta: 255,
            max_changed_frac: 1.0,
        };
        // The blessed sidecar tol is default (strict). is_stale_vs compares tol_params, so a
        // loosened runtime tol makes the golden STALE (refused), never a silent pass.
        let s = compare_frame(dir.path(), "t", Tier::Cell, &golden, &loose, &r);
        assert_eq!(s.signal.kind(), "golden_untrusted");
    }
}
