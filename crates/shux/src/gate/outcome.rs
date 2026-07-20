//! The structured run outcome — the 081→082 handoff surface (design D1).
//!
//! 081's runner drives a scenario and emits RAW `RunnerSignal`s to `--trace`. But the
//! trace loses richness (a `frame_mismatch` carries only `changed_cells`, not the regions
//! / pixel metrics / live capture needed to build `report.json` or to bless a golden). So
//! the runner ALSO returns this structured outcome: the ordered signal list PLUS a
//! per-frame `FrameOutcome` (the raw compare verdict + the live capture) PLUS the
//! scenario-level terminal disposition. 082's `verdict` layer rolls this up into the
//! frozen `report.json` schema — it never re-parses the trace text (handoff requirement).

use shux_raster::TierVerdict;
use shux_vt::{Fingerprint, StyleDelta, Tier, XfailMeta};

/// The compare disposition of one `expect_golden` frame (the four raw compare outcomes
/// 081 can produce; 082 maps these + xfail to the frozen `GateStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// The capture matched its golden.
    Match,
    /// The capture differed from its golden.
    Mismatch,
    /// No golden exists for this frame.
    GoldenAbsent,
    /// The golden's sidecar/baseline is stale or tampered — the compare was refused.
    GoldenUntrusted,
}

/// What happened to one `expect_golden` frame. Carries everything 082 needs to build a
/// `FrameReport` (regions/pixel metrics via `verdict`) and to bless a golden (the live
/// capture JSON + its content pin + the freshly-computed fingerprint).
#[derive(Debug, Clone)]
pub struct FrameOutcome {
    pub name: String,
    pub tier: Tier,
    pub kind: FrameKind,
    /// A diagnostic on a mismatch (e.g. `palette_unportable`, `pixel_diff`). Never a
    /// status.
    pub reason: Option<String>,
    /// The raw compare verdict (cell `FrameDiff` regions + optional `PixelMetrics`).
    /// `None` for `GoldenAbsent`/`GoldenUntrusted` (no compare ran).
    pub verdict: Option<TierVerdict>,
    /// The golden capture path RELATIVE to the golden dir (`<name>.capture.json`) — a
    /// display/provenance string for the report, never an absolute host path.
    pub golden_json: String,
    /// The live captured frame's canonical JSON — the source bytes for a bless write.
    pub live_capture_json: String,
    /// `capture_sha256` of the live frame — the bless content pin AND the fingerprint an
    /// xfail is pinned to (a fingerprinted xfail holds only for THIS diff).
    pub live_capture_sha256: String,
    /// The freshly-computed fingerprint for this frame/build (cell fields real, with the
    /// run's real `scenario_hash`/`cmd_env_hash`; `capture_sha256`/`rgba_sha256`/
    /// `png_sha256` left empty — the bless writer fills them from the live capture).
    pub live_fingerprint: Fingerprint,
    /// Expected-vs-actual STYLE at changed cells (084 F6), bounded. Empty when the frame
    /// matched, when no compare ran, or when only TEXT changed — a colour-only regression
    /// is invisible in a text diff, so the report must be able to name the colours.
    pub style_deltas: Vec<StyleDelta>,
    /// Runs FOUND (may exceed `style_deltas.len()` when the report cap truncates).
    pub style_deltas_total: u32,
    /// The frame's declared xfail metadata (from the scenario step), if any. 081 parses
    /// it opaque-reserved; 082 governs it.
    pub xfail: Option<XfailMeta>,
    /// Task 083 retry audit: a concise human note when a retry budget was exercised —
    /// `passed after N retries (absorbed fp …)` or `failed after N retries; divergent fps …`.
    /// The driver folds it into the scenario `report.json` note so a flake is never silently
    /// absorbed (council #5). `None` when no retry ran.
    pub retry_note: Option<String>,
}

/// The scenario-level terminal disposition — the single fatal raw signal that ended the
/// run (or characterized it), mapped from 081's raw signals by the runner. `None` means
/// the scenario ran to completion with no fatal terminal event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalOutcome {
    /// The child exited unexpectedly (`None` = signal-kill). 082 → `child_error`.
    ChildExit { code: Option<i32> },
    /// A driving step (`wait_for_text`/`expect_exit`) blew its own timeout. 082 → `fail`
    /// (the scripted state never occurred — a behavioural regression, exit 1).
    StepTimeout { action: String, step_index: usize },
    /// The whole-scenario deadline was exceeded. 082 → `fail` (exit 1; a hang must block,
    /// never be masked as retryable infra).
    ScenarioDeadline { step_index: usize },
    /// A settle / `expect_golden` frame never reached quiet. 082 → `settle_never_stable`.
    SettleNeverStable { action: String },
    /// The scratch quota was exhausted. 082 → `infra_error`.
    QuotaExceeded { limit: usize },
    /// The daemon could not spawn/serve the scenario (an environmental failure before
    /// any step ran). 082 → `infra_error`.
    Infra { message: String },
    /// A step / scenario was malformed at drive time (an unexpected RPC failure, a
    /// glance parse error, an un-encodable key). 082 → `scenario_error`.
    ScenarioError { message: String },
}

/// The full structured outcome of one scenario run. Provenance (`os`/`arch`/font) exists
/// because goldens are platform-sensitive; timing is best-effort wall clock.
pub struct RunOutcome {
    pub scenario_name: String,
    pub os: String,
    pub arch: String,
    pub font_chain_sha256: Option<String>,
    pub font_size_px: u16,
    pub started_at_ms: u128,
    pub duration_ms: u64,
    /// Every `expect_golden` frame compared, in scenario order.
    pub frames: Vec<FrameOutcome>,
    /// The fatal terminal disposition, if the run ended on one.
    pub terminal: Option<TerminalOutcome>,
    /// Did the scenario contain at least one `expect_golden` (a real visual check)? A
    /// scenario that compared zero frames proves nothing (082 → `scenario_error`).
    pub has_visual_check: bool,
}
