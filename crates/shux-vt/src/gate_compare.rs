//! Lens-gate CELL-tier compare + golden fingerprint (task 080).
//!
//! The cell tier is the portable, authoritative comparison: two frames match iff
//! [`diff_frames`](crate::diff_frames) reports no cell/geometry/cursor change AND the
//! frame pair is portable (no indexed colour under an OSC-4 override — [`D8`]). It
//! lives HERE, not in the binary, for the same reason the gate vocabulary
//! ([`crate::gate`]) does: `shux` is a binary-only crate whose internals the frozen
//! contract tests (`crates/shux/tests/lens_gate_*`) cannot import, and this is the
//! lowest shared crate they and the eventual runner (081/082) both depend on
//! (design-review D1).
//!
//! The [`Fingerprint`] sidecar accompanies every golden and records the build/config
//! under which it was blessed. A stale-trigger mismatch (font stack, Unicode-width
//! table, schema, tolerance, mask policy — [`Fingerprint::is_stale_vs`]) yields the
//! [`GateStatus::StaleGolden`] verdict: the compare is REFUSED, never silently trusted
//! (design-review D5/D6). `shux_version` is stored but is NOT a stale trigger (keying
//! stale on the exact app version would churn every golden each release). The pixel/
//! exact PNG tiers layer ON TOP of this (in `shux-raster`, design-review D2:
//! conjunctive — a matching PNG never overrides a semantic cell fail).
//!
//! `080 asserts STATUSES only` — the `GateStatus -> exit_code` map is frozen in
//! [`crate::gate`] and owned by task 082; nothing here asserts a process exit.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::capture::{FrameEnvelope, MaskSet};
use crate::cell::Color;
use crate::diff::{CellGridView, FrameDiff, diff_frames};
use crate::gate::GateStatus;

/// Fingerprint sidecar format version. Distinct from the capture [`schema`] (078) — the
/// sidecar shape can evolve independently of the frame wire format (council fp change).
/// Bump only with a `GATE-TEST-CHANGE:` trailer.
///
/// [`schema`]: crate::capture::SCHEMA_VERSION
pub const FINGERPRINT_SCHEMA: u32 = 1;

/// Render-format version — bump when a raster change alters golden pixels for the SAME
/// cells (a new font asset, metric change, cursor-drawing change). A stale trigger so a
/// pixel/exact golden blessed under the old renderer is refused, not falsely failed.
pub const RENDERER_FORMAT_VERSION: u32 = 1;

/// The three tolerance tiers (task 080 §2). `cell` is portable + default; `pixel`/`exact`
/// are conjunctive PNG checks on top of the cell tier (design-review D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Cell,
    Pixel,
    Exact,
}

/// RGBA pixel tolerance (task 080 §2). `max_channel_delta`: the largest per-channel
/// absolute difference tolerated on ANY pixel; `max_changed_frac`: the fraction of
/// pixels allowed to differ at all. `exact` ignores both (byte identity). Defaults are
/// zero — the strictest, matching `pixel_verify.py`'s `--max-*` defaults.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TolParams {
    #[serde(default)]
    pub max_channel_delta: u16,
    #[serde(default)]
    pub max_changed_frac: f64,
}

impl TolParams {
    /// Reject a nonsensical tolerance BEFORE it is blessed into a golden (task 081 must
    /// call this when sourcing a tolerance from config/computation). A non-finite
    /// `max_changed_frac` (`NaN`/`±∞`) is meaningless AND unstable: `serde_json` coerces
    /// it to `null` on write, so the sidecar can't round-trip; and a `NaN` would defeat a
    /// naive `PartialEq` staleness check ([`Fingerprint::is_stale_vs`] guards against the
    /// latter, but a non-finite value is still garbage). A fraction outside `[0, 1]` can
    /// never gate anything meaningfully.
    pub fn validate(&self) -> Result<(), String> {
        if !self.max_changed_frac.is_finite() {
            return Err(format!(
                "max_changed_frac must be finite, got {}",
                self.max_changed_frac
            ));
        }
        if !(0.0..=1.0).contains(&self.max_changed_frac) {
            return Err(format!(
                "max_changed_frac must be in [0, 1], got {}",
                self.max_changed_frac
            ));
        }
        Ok(())
    }
}

/// Staleness equality for the two `TolParams`: bit-tolerant to `NaN` and `-0.0`. A golden
/// blessed under a config must NEVER be falsely stale against an identical config, but
/// derived `f64 == f64` reports `NaN != NaN` (task-080 adversarial: false-stale forever).
/// `a == b` already unifies `-0.0`/`0.0` and all finite values; the `|| both NaN` arm adds
/// the reflexive `NaN` case without regressing `-0.0`.
/// True when `t` is the default (unspecified) tolerance — see [`Fingerprint::is_stale_vs`].
fn tol_params_is_default(t: &TolParams) -> bool {
    tol_params_same(t, &TolParams::default())
}

fn tol_params_same(a: &TolParams, b: &TolParams) -> bool {
    a.max_channel_delta == b.max_channel_delta
        && (a.max_changed_frac == b.max_changed_frac
            || (a.max_changed_frac.is_nan() && b.max_changed_frac.is_nan()))
}

/// The committed sidecar next to each golden (task 080 §3). Stale-trigger fields (see
/// [`Fingerprint::is_stale_vs`]) are the build/config that actually changes output;
/// content pins (`capture_sha256`/`rgba_sha256`/`png_sha256`) detect a golden edited
/// without a re-bless; the remaining fields are informational or placeholders 081/082
/// populate (round-tripped now so slotting them in needs no schema bump).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fingerprint {
    /// Sidecar format version ([`FINGERPRINT_SCHEMA`]) — STALE TRIGGER.
    pub fp_schema: u32,
    /// Capture wire-format version (078 `SCHEMA_VERSION`) — STALE TRIGGER.
    pub schema: u32,
    /// Render-format version ([`RENDERER_FORMAT_VERSION`]) — STALE TRIGGER.
    pub renderer_format_version: u32,
    /// Ordered bundled font-asset SHA + size (from `shux-raster`) — STALE TRIGGER.
    pub raster_font_fingerprint: String,
    /// Unicode-width table version (`UNICODE_VERSION`) — STALE TRIGGER.
    pub unicode_width_ver: String,
    /// Which tier this golden was blessed for — STALE TRIGGER.
    pub tol: Tier,
    /// The pixel tolerance it was blessed with — STALE TRIGGER.
    pub tol_params: TolParams,
    /// Hash of the applied redaction policy ([`mask_hash`]) — STALE TRIGGER (a change to
    /// WHICH cells are masked must invalidate the golden).
    pub mask_hash: String,
    /// Platform partition `<os>-<arch>` for pixel/exact goldens; `None` for the portable
    /// cell tier — STALE TRIGGER (a misfiled cross-platform baseline is refused).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// The producing `shux` version — INFORMATIONAL, NOT a stale trigger (design-review
    /// D5: keying stale on the app version churns every golden each release).
    pub shux_version: String,
    /// SHA-256 of the golden's canonical capture JSON — CONTENT PIN (cell tier tamper
    /// detection). Compared to the golden FILE on disk, never to the live capture.
    pub capture_sha256: String,
    /// SHA-256 of the golden's raw (uncompressed) RGBA buffer — CONTENT PIN for the
    /// pixel tier (encoder-stable, unlike `png_sha256`; council fp change).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rgba_sha256: Option<String>,
    /// SHA-256 of the golden's PNG bytes — CONTENT PIN used ONLY by the exact tier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub png_sha256: Option<String>,
    /// Scenario identity — placeholder; 081 populates.
    #[serde(default)]
    pub scenario_hash: String,
    /// Command + environment identity — placeholder; 081 populates.
    #[serde(default)]
    pub cmd_env_hash: String,
}

impl Fingerprint {
    /// Whether this sidecar is STALE against a freshly-computed `current` fingerprint —
    /// i.e. any stale-trigger field (build/config that changes output) differs. Content
    /// pins, `shux_version`, and the scenario/cmd placeholders are DELIBERATELY excluded
    /// (a live capture differing from a valid golden is a `fail`, not stale — design
    /// D6; the app version is informational — D5). Golden-file tampering is a separate
    /// check on the content pins, done by the orchestrator.
    pub fn is_stale_vs(&self, current: &Fingerprint) -> bool {
        self.fp_schema != current.fp_schema
            || self.schema != current.schema
            || self.renderer_format_version != current.renderer_format_version
            || self.raster_font_fingerprint != current.raster_font_fingerprint
            || self.unicode_width_ver != current.unicode_width_ver
            || self.tol != current.tol
            // Tolerance: `--tol` is BLESS-only, so an ordinary run computes
            // `TolParams::default()` and the sidecar is authoritative. Comparing a default
            // runtime tol against a blessed non-default one made every `--tol` golden
            // `stale_golden` on the very next unchanged run — a baseline CI could never
            // trust (codex review, PR #95; reproduced). Only an EXPLICIT runtime tolerance
            // is compared, which preserves the task-080 guard it exists for: a loosened
            // runtime tol must make the golden stale, never silently pass a regression.
            || (!tol_params_is_default(&current.tol_params)
                && !tol_params_same(&self.tol_params, &current.tol_params))
            || self.mask_hash != current.mask_hash
            || self.platform != current.platform
    }
}

/// Hex SHA-256 of `bytes` (lowercase, 64 chars).
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut s = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// The content pin for a cell golden: SHA-256 of its canonical JSON.
pub fn capture_sha256(env: &FrameEnvelope) -> String {
    sha256_hex(env.to_canonical_json().as_bytes())
}

/// The stable-across-platforms hash of a redaction policy (task 080 `mask_hash`). Sorted
/// rects → deterministic string → SHA-256. An empty mask set hashes a fixed sentinel so
/// "no masks" is distinct from "one 0-width mask" (there are none — `MaskSet::with`
/// drops width-0) yet stable.
pub fn mask_hash(masks: &MaskSet) -> String {
    let mut repr = String::from("masks:v1");
    for r in masks.sorted_rects() {
        use std::fmt::Write;
        let _ = write!(repr, ";{},{},{}", r.row, r.col, r.width);
    }
    sha256_hex(repr.as_bytes())
}

/// The `unicode-width` crate's Unicode data version (`"MAJOR.MINOR.PATCH"`) — the table
/// that actually determines cell widths, more meaningful than the crate semver.
pub fn unicode_width_version() -> String {
    let (a, b, c) = unicode_width::UNICODE_VERSION;
    format!("{a}.{b}.{c}")
}

/// Does any cell in `view` carry an indexed (palette) colour that the rasterizer resolves
/// through the OSC-4-overridable palette — fg, bg, OR the extended underline colour? Half
/// of the per-frame portability check ([`palette_unportable`]).
///
/// The `underline_color` arm is load-bearing (task-080 adversarial BLOCKER): `shux-raster`
/// renders an indexed undercurl colour (`SGR 58;5;N`) through the SAME `indexed_to_rgb`
/// path as fg/bg, so an undercurl-only indexed cell (default fg/bg, e.g. an nvim/LSP error
/// squiggle) under an OSC-4 override is genuinely unportable. Scanning only fg/bg would
/// certify it as a portable match — the exact D8 silent false pass.
pub fn has_indexed_colors(view: &dyn CellGridView) -> bool {
    for r in 0..view.rows() {
        for c in 0..view.cols() {
            let cell = view.cell(r, c);
            if matches!(cell.fg(), Color::Indexed(_)) || matches!(cell.bg(), Color::Indexed(_)) {
                return true;
            }
            if let Some(ext) = cell.cell().extended.as_deref()
                && matches!(ext.underline_color, Some(Color::Indexed(_)))
            {
                return true;
            }
        }
    }
    false
}

/// Is this frame PAIR unportable at the cell tier (task 078 R1 / task 079 D2)? True iff
/// EITHER side both overrode its palette (sticky OSC-4 bit) AND uses an indexed colour —
/// then the indexed→RGB mapping is machine-specific and the cells alone cannot certify a
/// portable match. OR'd over both sides (NOT keyed on `palette_overridden_differs`: both
/// sides can be `true`, so `differs == false`, yet the pair is unportable).
pub fn palette_unportable(a: &dyn CellGridView, b: &dyn CellGridView) -> bool {
    (a.palette_overridden() && has_indexed_colors(a))
        || (b.palette_overridden() && has_indexed_colors(b))
}

/// The cell-tier verdict for one frame: the [`GateStatus`] (only `Pass`/`Fail` here —
/// missing/stale are orchestrator concerns), the underlying [`FrameDiff`], and a
/// diagnostic `reason` on a fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellVerdict {
    pub status: GateStatus,
    pub diff: FrameDiff,
    pub reason: Option<String>,
}

/// Compare a live captured frame `live` against a `golden` frame at the CELL tier. Both
/// are validated views ([`FrameEnvelope::try_view`]). Fail precedence:
///   1. unportable (`palette_unportable`) → `Fail` reason `"palette_unportable"` even if
///      cells match (else a silent false pass — D8).
///   2. alt/primary screen flip → `Fail` reason `"alt_screen_changed"` (the schema records
///      `alt_screen` and it is part of `capture_sha256`, so the cell verdict must gate it
///      too — impl-review; the frozen `diff_frames`/daemon path is untouched via the
///      trait default).
///   3. geometry / cells / cursor changed → `Fail` (no reason).
///   4. otherwise → `Pass`.
pub fn compare_cell(golden: &dyn CellGridView, live: &dyn CellGridView) -> CellVerdict {
    let diff = diff_frames(golden, live);
    if palette_unportable(golden, live) {
        return CellVerdict {
            status: GateStatus::Fail,
            diff,
            reason: Some("palette_unportable".to_string()),
        };
    }
    if golden.alt_screen() != live.alt_screen() {
        return CellVerdict {
            status: GateStatus::Fail,
            diff,
            reason: Some("alt_screen_changed".to_string()),
        };
    }
    let regressed = diff.geometry_changed || diff.cells_changed > 0 || diff.cursor_moved;
    CellVerdict {
        status: if regressed {
            GateStatus::Fail
        } else {
            GateStatus::Pass
        },
        diff,
        reason: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VirtualTerminal;

    fn env(prog: &[u8], rows: usize, cols: usize) -> FrameEnvelope {
        let mut vt = VirtualTerminal::new(rows, cols);
        vt.process(prog);
        FrameEnvelope::from_terminal(&vt, &MaskSet::new())
    }

    // ── capture_sha256 / mask_hash / unicode_width_version ──────────────────────
    #[test]
    fn capture_sha256_is_stable_and_content_sensitive() {
        let a = env(b"hello", 2, 10);
        let b = env(b"hello", 2, 10);
        let c = env(b"hellp", 2, 10);
        assert_eq!(
            capture_sha256(&a),
            capture_sha256(&b),
            "same content → same sha"
        );
        assert_ne!(
            capture_sha256(&a),
            capture_sha256(&c),
            "one char → different sha"
        );
        assert_eq!(capture_sha256(&a).len(), 64, "hex sha256 is 64 chars");
    }

    #[test]
    fn mask_hash_distinguishes_policies() {
        let none = mask_hash(&MaskSet::new());
        let m1 = mask_hash(&MaskSet::new().with(0, 0, 5));
        let m1b = mask_hash(&MaskSet::new().with(0, 0, 5));
        let m2 = mask_hash(&MaskSet::new().with(0, 0, 6));
        assert_ne!(none, m1, "no mask vs one mask differ");
        assert_eq!(m1, m1b, "same policy → same hash");
        assert_ne!(m1, m2, "widening the mask changes the hash");
        // Order-independent: the rects are sorted before hashing.
        let ab = mask_hash(&MaskSet::new().with(0, 0, 2).with(1, 0, 2));
        let ba = mask_hash(&MaskSet::new().with(1, 0, 2).with(0, 0, 2));
        assert_eq!(ab, ba, "mask hash is order-independent");
    }

    #[test]
    fn unicode_width_version_is_dotted() {
        let v = unicode_width_version();
        assert_eq!(v.split('.').count(), 3, "MAJOR.MINOR.PATCH, got {v:?}");
    }

    // ── has_indexed / palette_unportable (D8) ───────────────────────────────────
    #[test]
    fn has_indexed_detects_palette_cells() {
        let plain = env(b"AB", 1, 4);
        let rgb = env(b"\x1b[38;2;1;2;3mAB\x1b[0m", 1, 4);
        let idx = env(b"\x1b[31mAB\x1b[0m", 1, 4);
        assert!(
            !has_indexed_colors(&plain.try_view().unwrap()),
            "default colours"
        );
        assert!(
            !has_indexed_colors(&rgb.try_view().unwrap()),
            "truecolor is portable"
        );
        assert!(
            has_indexed_colors(&idx.try_view().unwrap()),
            "indexed-1 fg present"
        );
    }

    #[test]
    fn palette_unportable_only_when_override_and_indexed() {
        let idx_no_ovr = env(b"\x1b[31mAB\x1b[0m", 1, 4);
        let idx_ovr = env(b"\x1b[31mAB\x1b[0m\x1b]4;1;#00ff00\x07", 1, 4);
        let plain_ovr = env(b"AB\x1b]4;1;#00ff00\x07", 1, 4);
        let a = idx_no_ovr.try_view().unwrap();
        let b = idx_ovr.try_view().unwrap();
        let c = plain_ovr.try_view().unwrap();
        // one side overridden + indexed → unportable
        assert!(palette_unportable(&a, &b));
        assert!(palette_unportable(&b, &a), "OR is symmetric");
        // override but NO indexed cells → portable
        assert!(!palette_unportable(&c, &c));
        // indexed but NO override → portable
        assert!(!palette_unportable(&a, &a));
    }

    // ── impl-review MAJOR: alt/primary screen flip is a cell-tier fail ──────────
    #[test]
    fn compare_cell_gates_alt_screen_flip() {
        // Identical VISIBLE cells but a different alt/primary flag → Fail. The schema +
        // capture_sha256 record alt_screen, so the cell verdict must too.
        let mut primary = env(b"hello", 2, 8);
        let mut alt = env(b"hello", 2, 8);
        primary.alt_screen = false;
        alt.alt_screen = true;
        let vp = primary.try_view().unwrap();
        let va = alt.try_view().unwrap();
        // The frozen `diff_frames` path is UNCHANGED — it never reads alt_screen.
        assert_eq!(
            diff_frames(&vp, &va).cells_changed,
            0,
            "diff_frames stays alt-blind (frozen)"
        );
        // But `compare_cell` gates it.
        let v = compare_cell(&vp, &va);
        assert_eq!(v.status, GateStatus::Fail);
        assert_eq!(v.reason.as_deref(), Some("alt_screen_changed"));
        // Same alt flag → not gated.
        assert_eq!(compare_cell(&vp, &vp).status, GateStatus::Pass);
    }

    // ── D8 BLOCKER (task-080 adversarial): indexed UNDERLINE colour is unportable ──
    #[test]
    fn palette_unportable_flags_indexed_underline_color() {
        // Undercurl (SGR 4:3) + INDEXED underline colour (SGR 58;5;9), DEFAULT fg/bg,
        // under an OSC-4 override. shux-raster resolves the underline via `indexed_to_rgb`
        // (the overridable palette), so the frame is UNPORTABLE — but scanning only fg/bg
        // would miss it (the exact silent false pass the adversarial pass caught).
        let e = env(b"\x1b[4:3m\x1b[58;5;9mX\x1b[0m\x1b]4;9;#00ff00\x07", 1, 4);
        assert!(e.palette_overridden, "OSC-4 set the sticky bit");
        let v = e.try_view().unwrap();
        assert!(
            has_indexed_colors(&v),
            "indexed underline colour must count as indexed"
        );
        assert!(
            palette_unportable(&v, &v),
            "indexed undercurl under OSC-4 override is unportable (D8)"
        );
        assert_eq!(
            compare_cell(&v, &v).reason.as_deref(),
            Some("palette_unportable"),
            "the escalation must reach the cell verdict"
        );
        // Sanity: a TRUECOLOR underline colour is portable even under an override.
        let rgb = env(
            b"\x1b[4:3m\x1b[58;2;1;2;3mX\x1b[0m\x1b]4;9;#00ff00\x07",
            1,
            4,
        );
        assert!(
            !has_indexed_colors(&rgb.try_view().unwrap()),
            "truecolor underline is portable"
        );
    }

    // ── compare_cell verdicts ───────────────────────────────────────────────────
    #[test]
    fn compare_cell_pass_on_identical_portable_frame() {
        let g = env(b"\x1b[38;2;9;9;9mhello\x1b[0m", 2, 10);
        let v = compare_cell(&g.try_view().unwrap(), &g.try_view().unwrap());
        assert_eq!(v.status, GateStatus::Pass);
        assert!(v.reason.is_none());
        assert_eq!(v.diff.cells_changed, 0);
    }

    #[test]
    fn compare_cell_fail_on_one_cell_change() {
        let g = env(b"hello", 2, 10);
        let l = env(b"hellp", 2, 10);
        let v = compare_cell(&g.try_view().unwrap(), &l.try_view().unwrap());
        assert_eq!(v.status, GateStatus::Fail);
        assert!(
            v.reason.is_none(),
            "a plain cell change carries no special reason"
        );
        assert!(v.diff.cells_changed > 0);
    }

    #[test]
    fn compare_cell_fail_on_geometry_and_cursor() {
        let small = env(b"hi", 2, 5);
        let big = env(b"hi", 3, 5);
        let g = compare_cell(&small.try_view().unwrap(), &big.try_view().unwrap());
        assert_eq!(g.status, GateStatus::Fail);
        assert!(g.diff.geometry_changed);
        // cursor move alone fails
        let a = env(b"\x1b[1;1Hxy", 2, 6);
        let b = env(b"\x1b[1;1Hxy\x1b[1;1H", 2, 6);
        let c = compare_cell(&a.try_view().unwrap(), &b.try_view().unwrap());
        assert_eq!(c.status, GateStatus::Fail);
        assert!(c.diff.cursor_moved && c.diff.cells_changed == 0);
    }

    #[test]
    fn compare_cell_escalates_palette_unportable_even_when_cells_match() {
        // Same CELLS on both sides, but one overrode the palette with indexed colour in
        // play → must NOT silently pass (D8). cells_changed == 0, yet Fail(reason).
        let a = env(b"\x1b[31mAB\x1b[0m", 1, 4);
        let b = env(b"\x1b[31mAB\x1b[0m\x1b]4;1;#00ff00\x07", 1, 4);
        let v = compare_cell(&a.try_view().unwrap(), &b.try_view().unwrap());
        assert_eq!(v.status, GateStatus::Fail);
        assert_eq!(v.reason.as_deref(), Some("palette_unportable"));
        assert_eq!(
            v.diff.cells_changed, 0,
            "the CELLS are identical — the escalation is the reason"
        );
    }

    #[test]
    fn compare_cell_passes_palette_override_without_indexed() {
        // Override present but no indexed colours → portable → Pass (must NOT escalate).
        let a = env(b"AB", 1, 4);
        let b = env(b"AB\x1b]4;1;#00ff00\x07", 1, 4);
        let v = compare_cell(&a.try_view().unwrap(), &b.try_view().unwrap());
        assert_eq!(v.status, GateStatus::Pass);
        assert!(v.reason.is_none());
    }

    // ── Fingerprint stale semantics (D5/D6) — STATUS/predicate only, no exit ─────
    fn fp(font: &str, tol: Tier) -> Fingerprint {
        Fingerprint {
            fp_schema: FINGERPRINT_SCHEMA,
            schema: crate::capture::SCHEMA_VERSION,
            renderer_format_version: RENDERER_FORMAT_VERSION,
            raster_font_fingerprint: font.to_string(),
            unicode_width_ver: unicode_width_version(),
            tol,
            tol_params: TolParams::default(),
            mask_hash: mask_hash(&MaskSet::new()),
            platform: None,
            shux_version: "0.43.0".into(),
            capture_sha256: "deadbeef".into(),
            rgba_sha256: None,
            png_sha256: None,
            scenario_hash: String::new(),
            cmd_env_hash: String::new(),
        }
    }

    #[test]
    fn fingerprint_not_stale_when_env_matches() {
        let a = fp("font-abc", Tier::Cell);
        // A different content pin / shux_version / placeholder does NOT make it stale.
        let mut b = fp("font-abc", Tier::Cell);
        b.capture_sha256 = "different-content".into();
        b.shux_version = "0.44.0".into();
        b.scenario_hash = "scn-xyz".into();
        assert!(
            !a.is_stale_vs(&b),
            "content/version/scenario are not stale triggers"
        );
    }

    #[test]
    fn fingerprint_stale_on_font_or_width_or_schema_or_tol_or_mask() {
        let base = fp("font-abc", Tier::Cell);
        let mut font = fp("font-XYZ", Tier::Cell);
        assert!(base.is_stale_vs(&font), "font stack changed → stale");
        font = base.clone();
        font.fp_schema = 999;
        assert!(
            base.is_stale_vs(&font),
            "fingerprint schema changed → stale"
        );
        font = base.clone();
        font.unicode_width_ver = "1.2.3".into();
        assert!(
            base.is_stale_vs(&font),
            "unicode-width table changed → stale"
        );
        font = base.clone();
        font.schema = 99;
        assert!(base.is_stale_vs(&font), "capture schema changed → stale");
        font = base.clone();
        font.renderer_format_version = 99;
        assert!(base.is_stale_vs(&font), "renderer format changed → stale");
        font = base.clone();
        font.tol = Tier::Pixel;
        assert!(base.is_stale_vs(&font), "tier changed → stale");
        font = base.clone();
        font.tol_params = TolParams {
            max_channel_delta: 5,
            max_changed_frac: 0.0,
        };
        assert!(base.is_stale_vs(&font), "tolerance changed → stale");
        font = base.clone();
        font.mask_hash = "other-policy".into();
        assert!(base.is_stale_vs(&font), "mask policy changed → stale");
        font = base.clone();
        font.platform = Some("linux-x86_64".into());
        assert!(
            base.is_stale_vs(&font),
            "platform partition changed → stale"
        );
    }

    // ── task-080 adversarial: NaN/-0.0-robust staleness + validate ──────────────
    #[test]
    fn fingerprint_nan_tolerance_is_not_falsely_stale_but_negative_zero_still_equal() {
        // A same-config golden must NEVER be false-stale — even with a NaN tolerance
        // (derived `f64 == f64` reports NaN != NaN → eternal false-stale).
        let mut a = fp("font-abc", Tier::Pixel);
        let mut b = fp("font-abc", Tier::Pixel);
        a.tol_params.max_changed_frac = f64::NAN;
        b.tol_params.max_changed_frac = f64::NAN;
        assert!(
            !a.is_stale_vs(&b),
            "identical NaN tolerance must not be false-stale"
        );
        // -0.0 vs 0.0 are semantically equal and must stay NON-stale (a to_bits compare
        // would wrongly flag them — the regression this guards against).
        a.tol_params.max_changed_frac = -0.0;
        b.tol_params.max_changed_frac = 0.0;
        assert!(!a.is_stale_vs(&b), "-0.0 and 0.0 tolerance are equal");
        // A genuinely different finite tolerance IS stale.
        b.tol_params.max_changed_frac = 0.01;
        assert!(a.is_stale_vs(&b), "a different tolerance is stale");
    }

    #[test]
    fn tol_params_validate_rejects_non_finite_and_out_of_range() {
        assert!(TolParams::default().validate().is_ok());
        assert!(
            TolParams {
                max_channel_delta: 5,
                max_changed_frac: 0.25
            }
            .validate()
            .is_ok()
        );
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.1, 1.5] {
            assert!(
                TolParams {
                    max_channel_delta: 0,
                    max_changed_frac: bad
                }
                .validate()
                .is_err(),
                "max_changed_frac {bad} must be rejected"
            );
        }
    }

    #[test]
    fn fingerprint_round_trips_and_denies_unknown_fields() {
        let f = fp("font-abc", Tier::Pixel);
        let json = serde_json::to_string_pretty(&f).unwrap();
        let back: Fingerprint = serde_json::from_str(&json).unwrap();
        assert_eq!(f, back);
        let bad = json.replace("\"fp_schema\"", "\"surprise\": 1, \"fp_schema\"");
        assert!(
            serde_json::from_str::<Fingerprint>(&bad).is_err(),
            "fails closed"
        );
    }
}
