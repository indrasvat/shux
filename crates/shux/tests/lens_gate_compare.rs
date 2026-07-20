//! Task 080 — golden compare: 3 tiers, fingerprint/stale, mask invariance, divergence
//! pixel proofs, artifact-size (GATE lane; `GATE-TEST-CHANGE:` to touch).
//!
//! Pure — no daemon (runs in CI alongside `lens_gate_parity`/`lens_gate_divergence`).
//! Imports `shux_vt` (cell tier + `Fingerprint`) and `shux_raster` (pixel/exact +
//! render) — the split that keeps the tiers inside the frozen-contract test boundary
//! (design-review D1). Nothing here asserts a CLI process exit — `080 asserts STATUSES
//! only`; the `GateStatus -> exit` map is 082's (D6).
//!
//! The little `gate_status` orchestrator below is what task 081's runner will do: resolve
//! the golden by tier, decide `missing_golden`/`stale_golden`, then run the conjunctive
//! tier (D2). It lives in the test (not a crate) so 080 keeps its crate surface minimal
//! and the golden-proof I/O stays honest (the test controls every file).

use std::path::{Path, PathBuf};

use shux_raster::{
    Rasterizer, encode_png, evaluate_tier, os_arch, pixel_baseline_path, render_envelope,
    render_envelope_png,
};
use shux_vt::{
    FINGERPRINT_SCHEMA, Fingerprint, FrameEnvelope, GateStatus, MaskSet, RENDERER_FORMAT_VERSION,
    SCHEMA_VERSION, Tier, TolParams, VirtualTerminal, capture_sha256, compare_cell, diff_frames,
    mask_hash, unicode_width_version,
};

const FONT_SIZE: f32 = 16.0;

fn rasterizer() -> Rasterizer {
    Rasterizer::new(FONT_SIZE).expect("bundled rasterizer")
}

fn font_fp() -> String {
    shux_raster::builtin_font_fingerprint(FONT_SIZE)
}

fn env(prog: &[u8], rows: usize, cols: usize) -> FrameEnvelope {
    let mut vt = VirtualTerminal::new(rows, cols);
    vt.process(prog);
    FrameEnvelope::from_terminal(&vt, &MaskSet::new())
}

fn env_masked(prog: &[u8], rows: usize, cols: usize, masks: &MaskSet) -> FrameEnvelope {
    let mut vt = VirtualTerminal::new(rows, cols);
    vt.process(prog);
    FrameEnvelope::from_terminal(&vt, masks)
}

/// A freshly-computed fingerprint for the CURRENT build/config — what the sidecar is
/// compared against for staleness. Content pins are placeholders (excluded from
/// `is_stale_vs`).
fn current_fp(
    tier: Tier,
    tol: TolParams,
    masks: &MaskSet,
    platform: Option<String>,
) -> Fingerprint {
    Fingerprint {
        fp_schema: FINGERPRINT_SCHEMA,
        schema: SCHEMA_VERSION,
        renderer_format_version: RENDERER_FORMAT_VERSION,
        raster_font_fingerprint: font_fp(),
        unicode_width_ver: unicode_width_version(),
        tol: tier,
        tol_params: tol,
        mask_hash: mask_hash(masks),
        platform,
        shux_version: "test".into(),
        capture_sha256: String::new(),
        rgba_sha256: None,
        png_sha256: None,
        scenario_hash: String::new(),
        cmd_env_hash: String::new(),
    }
}

/// Bless a cell golden: write `<name>.capture.json` + `<name>.fingerprint.json`.
fn bless_cell(dir: &Path, name: &str, golden: &FrameEnvelope, tol: TolParams, masks: &MaskSet) {
    std::fs::write(
        dir.join(format!("{name}.capture.json")),
        golden.to_canonical_json(),
    )
    .unwrap();
    let mut fp = current_fp(Tier::Cell, tol, masks, None);
    fp.capture_sha256 = capture_sha256(golden);
    std::fs::write(
        dir.join(format!("{name}.fingerprint.json")),
        serde_json::to_string_pretty(&fp).unwrap(),
    )
    .unwrap();
}

/// Bless a pixel/exact golden: the cell JSON + sidecar (for the conjunctive cell
/// compare) PLUS the committed `<name>/<os>-<arch>/frame.png` baseline.
fn bless_pixel(
    dir: &Path,
    name: &str,
    golden: &FrameEnvelope,
    tier: Tier,
    tol: TolParams,
    r: &Rasterizer,
) {
    std::fs::write(
        dir.join(format!("{name}.capture.json")),
        golden.to_canonical_json(),
    )
    .unwrap();
    let img = render_envelope(r, golden);
    let png = encode_png(&img).unwrap();
    let mut fp = current_fp(tier, tol, &MaskSet::new(), Some(os_arch()));
    fp.capture_sha256 = capture_sha256(golden);
    fp.rgba_sha256 = Some(shux_raster::rgba_sha256(&img));
    fp.png_sha256 = Some(shux_raster::png_sha256(&png));
    std::fs::write(
        dir.join(format!("{name}.fingerprint.json")),
        serde_json::to_string_pretty(&fp).unwrap(),
    )
    .unwrap();
    let png_path = pixel_baseline_path(dir, name, &os_arch());
    std::fs::create_dir_all(png_path.parent().unwrap()).unwrap();
    std::fs::write(&png_path, &png).unwrap();
}

/// The orchestration task 081 will own: resolve the golden by tier, classify
/// missing/stale, then evaluate the conjunctive tier. Returns the final [`GateStatus`]
/// and (when it got that far) the tier verdict.
fn gate_status(
    dir: &Path,
    name: &str,
    tier: Tier,
    live: &FrameEnvelope,
    current: &Fingerprint,
    r: &Rasterizer,
) -> (GateStatus, Option<shux_raster::TierVerdict>) {
    let json_path = dir.join(format!("{name}.capture.json"));
    let fp_path = dir.join(format!("{name}.fingerprint.json"));
    if !json_path.exists() || !fp_path.exists() {
        return (GateStatus::MissingGolden, None);
    }
    let golden = FrameEnvelope::from_canonical_json(&std::fs::read_to_string(&json_path).unwrap())
        .expect("golden parses");
    let sidecar: Fingerprint =
        serde_json::from_str(&std::fs::read_to_string(&fp_path).unwrap()).expect("sidecar parses");
    // Stale: build/config drift OR a golden edited without re-bless (D6).
    if sidecar.is_stale_vs(current) || sidecar.capture_sha256 != capture_sha256(&golden) {
        return (GateStatus::StaleGolden, None);
    }
    let golden_png = if tier != Tier::Cell {
        let p = pixel_baseline_path(dir, name, &os_arch());
        if !p.exists() {
            return (GateStatus::MissingGolden, None);
        }
        let bytes = std::fs::read(&p).unwrap();
        // Enforce the baseline CONTENT PIN (impl-review BLOCKER): a PNG replaced without
        // re-blessing — even a valid, decodable PNG — must be refused. `capture_sha256`
        // only pins the cell JSON; the PNG has its OWN pins (exact → `png_sha256` of the
        // bytes; pixel → `rgba_sha256` of the decoded RGBA, encoder-stable). A mismatch
        // or an undecodable baseline is `stale_golden`, never silently accepted.
        let pin_ok = match tier {
            Tier::Exact => sidecar.png_sha256.as_deref() == Some(&shux_raster::png_sha256(&bytes)),
            Tier::Pixel => match shux_raster::decode_png(&bytes) {
                Ok(img) => sidecar.rgba_sha256.as_deref() == Some(&shux_raster::rgba_sha256(&img)),
                Err(_) => false,
            },
            Tier::Cell => true,
        };
        if !pin_ok {
            return (GateStatus::StaleGolden, None);
        }
        Some(bytes)
    } else {
        None
    };
    // Bind the tolerance to the BLESSED sidecar, never a runtime value (task-080
    // adversarial: a loosened runtime tol must not silently pass a real regression without
    // tripping stale — the sidecar's `tol`/`tol_params` are stale triggers, so the
    // tolerance the compare uses is exactly the one that was blessed). This is the wiring
    // task 081's runner must adopt.
    let v = evaluate_tier(
        tier,
        &golden,
        live,
        golden_png.as_deref(),
        r,
        &sidecar.tol_params,
    )
    .expect("evaluate");
    (v.status, Some(v))
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

// ══════════════════════════════════════════════════════════════════════════════
// L1 tiers — cell
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn cell_tier_pass_fail_missing() {
    let dir = tmp();
    let r = rasterizer();
    let golden = env(b"\x1b[38;2;9;9;9mhello world\x1b[0m", 3, 20);
    bless_cell(
        dir.path(),
        "demo",
        &golden,
        TolParams::default(),
        &MaskSet::new(),
    );
    let cur = current_fp(Tier::Cell, TolParams::default(), &MaskSet::new(), None);

    // match → pass
    let (s, _) = gate_status(dir.path(), "demo", Tier::Cell, &golden, &cur, &r);
    assert_eq!(s, GateStatus::Pass, "identical capture passes");

    // seeded one-cell mismatch → fail
    let live = env(b"\x1b[38;2;9;9;9mhello worlD\x1b[0m", 3, 20);
    let (s, _) = gate_status(dir.path(), "demo", Tier::Cell, &live, &cur, &r);
    assert_eq!(s, GateStatus::Fail, "a one-cell change fails");

    // no golden → missing_golden (never a silent pass)
    let (s, _) = gate_status(dir.path(), "absent", Tier::Cell, &golden, &cur, &r);
    assert_eq!(s, GateStatus::MissingGolden);
}

#[test]
fn cell_tier_palette_unportable_is_fail_not_silent_pass() {
    let dir = tmp();
    let r = rasterizer();
    // Golden: indexed fg, no override (portable when blessed). Live: same cells but an
    // OSC-4 override — cells match, yet the golden cannot certify a portable match (D8).
    let golden = env(b"\x1b[31mAB\x1b[0m", 1, 4);
    let live = env(b"\x1b[31mAB\x1b[0m\x1b]4;1;#00ff00\x07", 1, 4);
    bless_cell(
        dir.path(),
        "pal",
        &golden,
        TolParams::default(),
        &MaskSet::new(),
    );
    let cur = current_fp(Tier::Cell, TolParams::default(), &MaskSet::new(), None);
    let (s, v) = gate_status(dir.path(), "pal", Tier::Cell, &live, &cur, &r);
    assert_eq!(s, GateStatus::Fail);
    assert_eq!(v.unwrap().reason.as_deref(), Some("palette_unportable"));
}

// ══════════════════════════════════════════════════════════════════════════════
// L1 sidecar — fingerprint / stale (STATUS only, no exit — D6)
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn stale_golden_on_font_bump_not_a_pass_or_false_fail() {
    let dir = tmp();
    let r = rasterizer();
    let golden = env(b"themed text", 2, 20);
    bless_cell(
        dir.path(),
        "thm",
        &golden,
        TolParams::default(),
        &MaskSet::new(),
    );
    // A build whose font stack differs from the blessed sidecar.
    let mut stale = current_fp(Tier::Cell, TolParams::default(), &MaskSet::new(), None);
    stale.raster_font_fingerprint = "font-from-a-different-build".into();
    let (s, v) = gate_status(dir.path(), "thm", Tier::Cell, &golden, &stale, &r);
    assert_eq!(
        s,
        GateStatus::StaleGolden,
        "a font bump refuses the compare — not a silent pass, not a false fail"
    );
    assert!(v.is_none(), "stale short-circuits before any cell compare");
    // The very same golden with the MATCHING build passes (proves the bump caused it).
    let cur = current_fp(Tier::Cell, TolParams::default(), &MaskSet::new(), None);
    let (s, _) = gate_status(dir.path(), "thm", Tier::Cell, &golden, &cur, &r);
    assert_eq!(s, GateStatus::Pass);
}

#[test]
fn stale_golden_on_tampered_golden_file() {
    let dir = tmp();
    let r = rasterizer();
    let golden = env(b"original", 2, 12);
    bless_cell(
        dir.path(),
        "tmp",
        &golden,
        TolParams::default(),
        &MaskSet::new(),
    );
    // Edit the golden JSON on disk WITHOUT re-blessing the sidecar → content pin mismatch.
    let edited = env(b"tampered", 2, 12);
    std::fs::write(
        dir.path().join("tmp.capture.json"),
        edited.to_canonical_json(),
    )
    .unwrap();
    let cur = current_fp(Tier::Cell, TolParams::default(), &MaskSet::new(), None);
    let (s, _) = gate_status(dir.path(), "tmp", Tier::Cell, &edited, &cur, &r);
    assert_eq!(
        s,
        GateStatus::StaleGolden,
        "edited golden w/o re-bless is stale"
    );
}

// ══════════════════════════════════════════════════════════════════════════════
// L1 tiers — pixel / exact (self-rendered tempdir baselines = PLUMBING proof, D3)
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn pixel_tier_pass_fail_missing_platform() {
    let dir = tmp();
    let r = rasterizer();
    let golden = env(b"\x1b[34mpixel golden\x1b[0m", 3, 20);
    bless_pixel(
        dir.path(),
        "px",
        &golden,
        Tier::Pixel,
        TolParams::default(),
        &r,
    );
    let cur = current_fp(
        Tier::Pixel,
        TolParams::default(),
        &MaskSet::new(),
        Some(os_arch()),
    );

    // identical live → pass (0 changed pixels)
    let (s, v) = gate_status(dir.path(), "px", Tier::Pixel, &golden, &cur, &r);
    assert_eq!(s, GateStatus::Pass);
    assert_eq!(v.unwrap().pixel.unwrap().changed_pixels, 0);

    // seeded cell change → fail (cell authoritative; PNG never rescues it — D2)
    let live = env(b"\x1b[34mpixel goldeN\x1b[0m", 3, 20);
    let (s, _) = gate_status(dir.path(), "px", Tier::Pixel, &live, &cur, &r);
    assert_eq!(s, GateStatus::Fail);

    // a platform with no committed baseline → missing_golden, never a silent pass.
    // (Removing this host's baseline dir models running the gate on a platform the golden
    // was never blessed for — the `<os>-<arch>` partition simply has no `frame.png`.)
    std::fs::remove_dir_all(dir.path().join("px")).unwrap();
    let (s, _) = gate_status(dir.path(), "px", Tier::Pixel, &golden, &cur, &r);
    assert_eq!(s, GateStatus::MissingGolden, "absent platform baseline");
}

#[test]
fn exact_tier_pass_and_pixel_only_fail() {
    let dir = tmp();
    let r = rasterizer();
    // Golden with a BLOCK cursor. A live capture identical in cells but with a BAR cursor
    // passes the cell tier (shape is a cell-tier blind spot) yet its render differs — a
    // genuine exact FAIL against a VALID golden (distinct from the tamper case below).
    let golden = env(b"\x1b[2 q\x1b[32mexact\x1b[0m", 2, 10);
    let live_bar = env(b"\x1b[6 q\x1b[32mexact\x1b[0m", 2, 10);
    bless_pixel(
        dir.path(),
        "ex",
        &golden,
        Tier::Exact,
        TolParams::default(),
        &r,
    );
    let cur = current_fp(
        Tier::Exact,
        TolParams::default(),
        &MaskSet::new(),
        Some(os_arch()),
    );
    // byte-identical render → pass
    let (s, _) = gate_status(dir.path(), "ex", Tier::Exact, &golden, &cur, &r);
    assert_eq!(s, GateStatus::Pass);
    // cursor-shape-only live → cell passes, exact PNG differs → Fail (exact_diff)
    let (s, v) = gate_status(dir.path(), "ex", Tier::Exact, &live_bar, &cur, &r);
    assert_eq!(s, GateStatus::Fail);
    assert_eq!(v.unwrap().reason.as_deref(), Some("exact_diff"));
}

// ── impl-review BLOCKER: pixel/exact baseline CONTENT PINS are enforced ──────────
#[test]
fn pixel_and_exact_baselines_are_pin_enforced_against_tamper() {
    let dir = tmp();
    let r = rasterizer();
    // A DIFFERENT valid render to swap the baseline with (leaving capture_sha256 valid).
    let other = render_envelope_png(&r, &env(b"\x1b[31mTAMPERED\x1b[0m", 2, 10)).unwrap();
    for tier in [Tier::Pixel, Tier::Exact] {
        let golden = env(b"\x1b[34mORIGINAL\x1b[0m", 2, 10);
        bless_pixel(dir.path(), "pin", &golden, tier, TolParams::default(), &r);
        let cur = current_fp(tier, TolParams::default(), &MaskSet::new(), Some(os_arch()));
        // Valid baseline → pass.
        assert_eq!(
            gate_status(dir.path(), "pin", tier, &golden, &cur, &r).0,
            GateStatus::Pass
        );
        // Replace the committed PNG with a DIFFERENT VALID png, cell JSON + sidecar
        // untouched → the content pin (rgba/png sha) must refuse it as stale, never
        // accept a swapped baseline. (This is what `capture_sha256` alone cannot catch.)
        let png_path = pixel_baseline_path(dir.path(), "pin", &os_arch());
        std::fs::write(&png_path, &other).unwrap();
        assert_eq!(
            gate_status(dir.path(), "pin", tier, &golden, &cur, &r).0,
            GateStatus::StaleGolden,
            "{tier:?}: a swapped baseline PNG must be refused via its content pin"
        );
        std::fs::remove_dir_all(dir.path().join("pin")).unwrap();
        std::fs::remove_file(dir.path().join("pin.capture.json")).unwrap();
        std::fs::remove_file(dir.path().join("pin.fingerprint.json")).unwrap();
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// L1 mask absence + invariance (080-owned artifacts, D4)
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn mask_absence_secret_never_in_golden_or_diff() {
    let masks = MaskSet::new().with(0, 0, 12);
    let secret = env_masked(b"SECRET-TKN-9 tail", 2, 24, &masks);
    let json = secret.to_canonical_json();
    assert!(
        !json.contains("SECRET"),
        "masked secret must not enter the golden JSON"
    );
    assert!(json.contains("mask"), "a structural mask run is emitted");
    // The diff against another masked capture never surfaces the secret either.
    let other = env_masked(b"HUNTER2-XYZ0 tail", 2, 24, &masks);
    let d = diff_frames(&secret.try_view().unwrap(), &other.try_view().unwrap());
    assert_eq!(
        d.cells_changed, 0,
        "differing secrets behind the mask are invisible"
    );
}

#[test]
fn mask_invariance_across_capture_hash_compare_and_pixels() {
    let dir = tmp();
    let r = rasterizer();
    let masks = MaskSet::new().with(0, 0, 10);
    let a = env_masked(b"AAAAAAAAAA visible", 2, 20, &masks);
    let b = env_masked(b"BBBBBBBBBB visible", 2, 20, &masks);
    // (1) capture_sha256 is stable across masked content.
    assert_eq!(
        capture_sha256(&a),
        capture_sha256(&b),
        "mask hides the content sha"
    );
    // (2) the compare outcome is stable: bless from A, compare live B → pass.
    bless_cell(dir.path(), "mk", &a, TolParams::default(), &masks);
    let cur = current_fp(Tier::Cell, TolParams::default(), &masks, None);
    let (s, _) = gate_status(dir.path(), "mk", Tier::Cell, &b, &cur, &r);
    assert_eq!(
        s,
        GateStatus::Pass,
        "changing masked content does not change the verdict"
    );
    // (3) the pixel render is stable: the masked region renders the same placeholder on
    // both sides, so the RGBA is identical (no false pixel mismatch — D4/agy #3).
    let ra = render_envelope(&r, &a);
    let rb = render_envelope(&r, &b);
    let m = shux_raster::compare_pixels(&ra, &rb, &TolParams::default());
    assert_eq!(
        m.changed_pixels, 0,
        "masked pixels are identical across secrets"
    );
    assert_eq!(shux_raster::rgba_sha256(&ra), shux_raster::rgba_sha256(&rb));
}

#[test]
fn mask_does_not_leak_secret_length_via_cursor() {
    // The cursor lands just after a printed secret, so its column encodes the secret's
    // LENGTH. With a mask covering the whole field, capturing the cursor UNMASKED would
    // leak that length into capture_sha256 AND rgba_sha256 (task-080 adversarial MAJOR).
    // Clamping the cursor to the mask origin makes different-length secrets invariant.
    let masks = MaskSet::new().with(0, 0, 10);
    let long = env_masked(b"SECRET", 2, 20, &masks); // cursor col 6 → clamped to 0
    let short = env_masked(b"SEC", 2, 20, &masks); // cursor col 3 → clamped to 0
    assert_eq!(
        long.cursor.col, 0,
        "cursor inside the mask is clamped to its origin"
    );
    assert_eq!(short.cursor.col, 0);
    assert_eq!(
        capture_sha256(&long),
        capture_sha256(&short),
        "secret length must not leak via cursor into capture_sha256"
    );
    let r = rasterizer();
    assert_eq!(
        shux_raster::rgba_sha256(&render_envelope(&r, &long)),
        shux_raster::rgba_sha256(&render_envelope(&r, &short)),
        "secret length must not leak via cursor into rgba_sha256"
    );
    // No OVER-clamping: a cursor OUTSIDE every masked rect keeps its real column.
    let outside = env_masked(b"hello", 2, 20, &MaskSet::new().with(0, 0, 3));
    assert_eq!(outside.cursor.col, 5, "cursor past the mask is untouched");
}

// ══════════════════════════════════════════════════════════════════════════════
// L1 divergence — PROVE the 079 fixtures' pixel_diverges notes with real rendering
// ══════════════════════════════════════════════════════════════════════════════

fn div_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.shux/fixtures/lens-gate/divergence")
}

fn load_env(path: &Path) -> FrameEnvelope {
    FrameEnvelope::from_canonical_json(&std::fs::read_to_string(path).unwrap())
        .unwrap_or_else(|e| panic!("parse {}: {e:?}", path.display()))
}

fn div_pair(name: &str) -> (FrameEnvelope, FrameEnvelope) {
    let d = div_dir();
    (
        load_env(&d.join(format!("{name}.a.json"))),
        load_env(&d.join(format!("{name}.b.json"))),
    )
}

/// changed pixels rendering a vs b through the SAME rasterizer.
fn pixel_delta(a: &FrameEnvelope, b: &FrameEnvelope) -> shux_raster::PixelMetrics {
    let r = rasterizer();
    shux_raster::compare_pixels(
        &render_envelope(&r, a),
        &render_envelope(&r, b),
        &TolParams::default(),
    )
}

#[test]
fn divergence_blink_is_cell_signal_but_not_pixel() {
    // pixel_diverges=false: CELL catches 2 changed cells; the static raster does NOT
    // render blink → the two frames render byte-identically.
    let (a, b) = div_pair("blink-only");
    assert_eq!(
        diff_frames(&a.try_view().unwrap(), &b.try_view().unwrap()).cells_changed,
        2
    );
    let m = pixel_delta(&a, &b);
    assert_eq!(
        m.changed_pixels, 0,
        "blink is invisible to shux's static raster"
    );
}

#[test]
fn divergence_cursor_shape_is_pixel_only() {
    // pixel_diverges=true: the CELL tier is blind (CursorState carries no shape), but the
    // rendered cursor differs block↔bar.
    let (a, b) = div_pair("cursor-shape-only");
    let d = diff_frames(&a.try_view().unwrap(), &b.try_view().unwrap());
    assert_eq!(d.cells_changed, 0);
    assert!(!d.cursor_moved, "cell tier is blind to shape");
    assert!(
        pixel_delta(&a, &b).changed_pixels > 0,
        "block vs bar differs at the pixel tier"
    );
}

#[test]
fn divergence_cursor_position_and_visibility_diverge_at_pixel() {
    for name in ["cursor-position", "cursor-visibility"] {
        let (a, b) = div_pair(name);
        assert!(
            diff_frames(&a.try_view().unwrap(), &b.try_view().unwrap()).cursor_moved,
            "{name}: cell tier catches the cursor move"
        );
        assert!(
            pixel_delta(&a, &b).changed_pixels > 0,
            "{name}: the drawn cursor differs"
        );
    }
}

#[test]
fn divergence_default_color_repaints_the_field() {
    // pixel_diverges=true: OSC-11 bg default differs → every default-bg cell repaints.
    let (a, b) = div_pair("default-color-only");
    assert_eq!(
        diff_frames(&a.try_view().unwrap(), &b.try_view().unwrap()).cells_changed,
        8
    );
    assert!(
        pixel_delta(&a, &b).changed_pixels > 0,
        "the default-bg field is repainted"
    );
}

#[test]
fn divergence_size_mismatch_is_hard_pixel_fail() {
    let (a, b) = div_pair("size-mismatch");
    assert!(diff_frames(&a.try_view().unwrap(), &b.try_view().unwrap()).geometry_changed);
    assert!(
        pixel_delta(&a, &b).size_mismatch,
        "different geometry → pixel size mismatch"
    );
}

#[test]
fn divergence_palette_indexed_escalates_no_indexed_does_not() {
    // palette-with-indexed (pixel_diverges=true → palette_unportable): the cell verdict
    // is Fail(palette_unportable). shux's raster is OSC-4-blind, so render(a)==render(b)
    // — the divergence is a PORTABILITY verdict, not a shux-render pixel delta.
    let (a, b) = div_pair("palette-with-indexed");
    let v = compare_cell(&a.try_view().unwrap(), &b.try_view().unwrap());
    assert_eq!(v.status, GateStatus::Fail);
    assert_eq!(v.reason.as_deref(), Some("palette_unportable"));
    assert_eq!(
        pixel_delta(&a, &b).changed_pixels,
        0,
        "shux raster does not track OSC-4"
    );

    // palette-no-indexed (pixel_diverges=false): overridden but no indexed cells →
    // portable → the cell verdict must NOT escalate.
    let (a, b) = div_pair("palette-no-indexed");
    let v = compare_cell(&a.try_view().unwrap(), &b.try_view().unwrap());
    assert_eq!(v.status, GateStatus::Pass, "no indexed colour → portable");
    assert_eq!(pixel_delta(&a, &b).changed_pixels, 0);
}

#[test]
fn divergence_glyph_fallback_is_pixel_only_under_a_different_font_stack() {
    // The `glyph-identical-pixel-boundary` fixture (❤️X) documents that cell-identical
    // frames can render to different pixels on a different FONT STACK — which is exactly
    // why pixel baselines are <os>-<arch>-partitioned. shux's own raster renders ❤️
    // identically with/without the emoji font (a bundled symbols font covers U+2764), so
    // the faithful proof uses an emoji-font-ONLY glyph (🦀 U+1F980): SAME cells, DIFFERENT
    // font chain → different pixels, while `diff_frames == 0`.
    use shux_raster::{
        BUILTIN_MATH, BUILTIN_NERD_FONT, BUILTIN_SYMBOLS, BUILTIN_SYMBOLS_LEGACY,
        builtin_font_bytes,
    };
    let crab = env("🦀X".as_bytes(), 2, 6);
    let full = rasterizer();
    let no_emoji = Rasterizer::with_fonts(
        FONT_SIZE,
        [
            builtin_font_bytes(BUILTIN_NERD_FONT).unwrap(),
            builtin_font_bytes(BUILTIN_MATH).unwrap(),
            builtin_font_bytes(BUILTIN_SYMBOLS).unwrap(),
            builtin_font_bytes(BUILTIN_SYMBOLS_LEGACY).unwrap(),
        ],
    )
    .unwrap();
    // cell tier: identical frame → zero.
    assert_eq!(
        diff_frames(&crab.try_view().unwrap(), &crab.try_view().unwrap()).cells_changed,
        0
    );
    // pixel tier: same cells, different font stack → the emoji glyph renders differently.
    let with_emoji = render_envelope(&full, &crab);
    let without = render_envelope(&no_emoji, &crab);
    let m = shux_raster::compare_pixels(&with_emoji, &without, &TolParams::default());
    assert!(
        m.changed_pixels > 0,
        "an emoji-only glyph renders differently without the emoji fallback (font-stack pixel divergence)"
    );
}

// ══════════════════════════════════════════════════════════════════════════════
// L2 perf — max-artifact-size regression (deterministic, no timing)
// ══════════════════════════════════════════════════════════════════════════════

/// Documented budgets: a captured frame's canonical JSON and rendered PNG must stay well
/// under the `pane.glance` RPC's 8 MiB decoded PNG cap so a full `include_png +
/// include_cells` response fits the wire budget. These are per-frame caps at the two
/// dogfood viewports; a regression that bloats either artifact trips here.
#[test]
fn artifact_sizes_stay_within_budget() {
    const JSON_CAP: usize = 256 * 1024; // 256 KiB canonical JSON per frame
    const PNG_CAP: usize = 8 * 1024 * 1024; // 8 MiB decoded PNG (the glance wire cap)
    let r = rasterizer();
    // A dense, coloured frame at both dogfood viewports.
    for (rows, cols) in [(24usize, 80usize), (40, 120)] {
        let mut vt = VirtualTerminal::new(rows, cols);
        for row in 0..rows {
            vt.process(format!("\x1b[{};1H", row + 1).as_bytes());
            vt.process(format!("\x1b[38;5;{}m", 16 + (row % 200)).as_bytes());
            for c in 0..cols {
                vt.process(&[b'A' + ((row + c) % 26) as u8]);
            }
        }
        let e = FrameEnvelope::from_terminal(&vt, &MaskSet::new());
        let json = e.to_canonical_json();
        assert!(
            json.len() < JSON_CAP,
            "{rows}x{cols} canonical JSON {} exceeds {JSON_CAP}",
            json.len()
        );
        let png = render_envelope_png(&r, &e).unwrap();
        assert!(
            png.len() < PNG_CAP,
            "{rows}x{cols} PNG {} exceeds {PNG_CAP}",
            png.len()
        );
        // The combined glance payload (cells JSON + PNG) stays within the wire budget.
        assert!(
            json.len() + png.len() < PNG_CAP,
            "combined payload within 8 MiB"
        );
    }
}
