//! Scenario TOML model + parse + validate (design A/D6). Pure, no daemon.
//!
//! Two-phase parse: (1) serde reads the envelope with steps as raw `toml::Table`s so
//! the TOML structure is validated; (2) each step dispatches on `action` into a
//! per-action `#[serde(deny_unknown_fields)]` struct, so an unknown action OR a typo'd
//! field fails CLOSED with an actionable, step-indexed message. This sidesteps toml's
//! internally-tagged-enum limitations and gives better errors than a giant union struct.
//!
//! Deferred steps (mouse/focus/bracketed_paste) are REJECTED with a clear message
//! (design D10 — non-support is explicit, never silently ignored). `xfail` is parsed opaquely
//! into the 082 `XfailMeta` shape and governed by 082.
//!
//! Task 083 makes the settle-hardening fields REAL: `hold_settle.hold_ms` (frame-content hold),
//! `stable_frames.n` (K contiguous identical frames), `expect_golden.retries` (re-settle/
//! re-capture jitter budget with an anti-masking fingerprint rule), and `expect_golden`'s own
//! optional `hold_ms`/`stable_frames` (its pre-capture settle can be frame-stability, honoring
//! the golden's masks). See `crates/shux/src/gate/runner.rs` + `.local/083-design.md`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use shux_vt::{Tier, XfailMeta};

/// Default whole-scenario wall-clock budget when `deadline_ms` is omitted.
pub const DEFAULT_DEADLINE_MS: u64 = 60_000;
/// Default settle quiet window (ms) for `settle`/`stable_frames`/`expect_golden`.
pub const DEFAULT_QUIET_MS: u64 = 300;
/// Default settle timeout (ms).
pub const DEFAULT_SETTLE_TIMEOUT_MS: u64 = 10_000;
/// Default per-step wait timeout (ms).
pub const DEFAULT_WAIT_TIMEOUT_MS: u64 = 10_000;

/// A parsed, validated scenario.
#[derive(Debug, Clone, PartialEq)]
pub struct Scenario {
    pub name: String,
    pub description: String,
    pub command: Vec<String>,
    /// Optional working directory for the child, RELATIVE to the scenario's own
    /// directory (084 F2). `None` keeps the historical behaviour: the child starts in
    /// the sandbox HOME. A relative path is the only portable way to point a scenario at
    /// a project sitting beside it — an absolute host path would land in `cmd_env_hash`
    /// and make the committed golden `untrusted` on every other machine.
    pub cwd: Option<String>,
    pub terminal: TerminalCfg,
    pub env: EnvBlock,
    pub deadline_ms: u64,
    pub steps: Vec<Step>,
}

/// Terminal geometry + query-response policy (design D9).
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalCfg {
    pub rows: u16,
    pub cols: u16,
    /// Reserved-honest (design D9): the shux terminal answers OSC 11/DA/XTVERSION
    /// deterministically regardless; `false` means "does not rely on query responses".
    pub respond_to_queries: bool,
}

/// The env allow/deny block (design D4). `vars` SET (incl. empty string — never
/// overloaded as unset); `allow` opts specific host vars through under `env_clear`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EnvBlock {
    pub vars: BTreeMap<String, String>,
    pub allow: Vec<String>,
}

/// A redaction rect: a row-span `ROW,COL,WIDTH` (aligned with `shux_vt::MaskRect` —
/// NOT `[row,col,width,height]`; codex mask-shape catch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MaskSpec {
    pub row: u16,
    pub col: u16,
    pub width: u16,
}

/// The agnostic step core (design D6). Domain asserts get a plugin seam later.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Step {
    WaitForText {
        text: Option<String>,
        regex: Option<String>,
        absent: bool,
        timeout_ms: u64,
    },
    Wait {
        ms: u64,
    },
    Settle {
        quiet_ms: u64,
        timeout_ms: u64,
    },
    /// Frame-content HOLD settle (task 083): settle once the presented frame has stayed UNCHANGED
    /// for `hold_ms` (silence counts as held), even while output keeps pumping. The RECOMMENDED
    /// animated-TUI settle — handles both a continuous repainter AND a TUI that reaches a static
    /// steady state, and rejects a slow spinner that quiet-mode false-settles between frames.
    HoldSettle {
        hold_ms: u64,
        quiet_ms: u64,
        timeout_ms: u64,
    },
    /// Frame-content STABLE-FRAMES settle (task 083): settle once `n` CONTIGUOUS revisions render
    /// an identical frame — a count-based alternative to `hold_settle` for a CONTINUOUSLY-
    /// repainting TUI. NOTE: a pane that reaches a static steady state (STOPS repainting) never
    /// produces `n` new revisions → `settle_never_stable`; for such a TUI use `hold_settle` (or
    /// plain `settle`). Never reaching `n` within budget is `settle_never_stable` (a failure,
    /// never infra).
    StableFrames {
        n: u32,
        quiet_ms: u64,
        timeout_ms: u64,
    },
    TypeText {
        text: String,
    },
    /// Vim-notation key chords (e.g. `["<C-c>","gg"]`), decoded to bytes.
    Keys {
        keys: Vec<String>,
    },
    Paste {
        text: String,
    },
    Resize {
        rows: u16,
        cols: u16,
    },
    ExpectGolden {
        name: String,
        tier: Tier,
        /// Retry budget (task 083): on a compare MISMATCH, re-settle + re-capture up to `retries`
        /// times before declaring `fail`, with an anti-masking fingerprint rule (a retry redeems
        /// FAIL→PASS only by matching the golden; divergent failing fingerprints never pass).
        retries: u32,
        /// Optional frame-content HOLD for the pre-capture settle (task 083; 0 = quiet settle).
        hold_ms: u64,
        /// Optional frame-content STABLE-FRAMES for the pre-capture settle (task 083; 1 = quiet
        /// settle). When set, the settle honours `masks` so an animated-but-masked region does
        /// not block the capture.
        stable_frames: u32,
        quiet_ms: u64,
        timeout_ms: u64,
        masks: Vec<MaskSpec>,
        /// Parsed into the 082 `XfailMeta` shape; governed by 082.
        xfail: Option<XfailMeta>,
    },
    AssertContains {
        text: String,
    },
    AssertNotContains {
        text: String,
    },
    ExpectExit {
        code: Option<i32>,
        timeout_ms: u64,
    },
}

impl Step {
    /// The `action` tag — for signals + error messages.
    pub fn action(&self) -> &'static str {
        match self {
            Step::WaitForText { .. } => "wait_for_text",
            Step::Wait { .. } => "wait",
            Step::Settle { .. } => "settle",
            Step::HoldSettle { .. } => "hold_settle",
            Step::StableFrames { .. } => "stable_frames",
            Step::TypeText { .. } => "type_text",
            Step::Keys { .. } => "keys",
            Step::Paste { .. } => "paste",
            Step::Resize { .. } => "resize",
            Step::ExpectGolden { .. } => "expect_golden",
            Step::AssertContains { .. } => "assert_contains",
            Step::AssertNotContains { .. } => "assert_not_contains",
            Step::ExpectExit { .. } => "expect_exit",
        }
    }
}

/// A scenario parse/validation error — always actionable (design D6, fails closed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioError(pub String);

impl std::fmt::Display for ScenarioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ScenarioError {}

impl ScenarioError {
    fn at(idx: usize, msg: impl std::fmt::Display) -> Self {
        ScenarioError(format!("step {idx}: {msg}"))
    }
}

/// A scenario / golden `name` becomes a filesystem path component
/// (`<golden_dir>/<name>.capture.json`, `<goldens>/<scenario>/`). It MUST be a safe
/// single component: no path separators, no `..`, no absolute path, no control chars,
/// bounded length (adv MAJOR: an unvalidated name escapes the golden dir via
/// `Path::join` — a read oracle today, a latent arbitrary-write when 082/083 wire a
/// bless writer through the same name; the parser is the choke point).
fn validate_name(kind: &str, name: &str) -> Result<(), ScenarioError> {
    if name.trim().is_empty() {
        return Err(ScenarioError(format!("{kind} `name` must not be empty")));
    }
    if name.len() > 128 {
        return Err(ScenarioError(format!(
            "{kind} `name` too long ({} bytes, max 128)",
            name.len()
        )));
    }
    if name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.chars().any(|c| c.is_control())
    {
        return Err(ScenarioError(format!(
            "{kind} `name` {name:?} must be a single path component \
             (no '/', '\\', '..', or control characters)"
        )));
    }
    Ok(())
}

/// Validate a `hold_ms` frame-content hold (task 083): `[10, 60_000]` ms and ≤ `timeout_ms` (a
/// hold longer than the budget can never succeed). Mirrors the daemon's `validate_stability_params`
/// so a bad scenario fails at parse (`scenario_error`, exit 2), not mid-drive.
fn validate_hold_ms(idx: usize, hold_ms: u64, timeout_ms: u64) -> Result<(), ScenarioError> {
    if !(HOLD_MS_MIN..=HOLD_MS_MAX).contains(&hold_ms) {
        return Err(ScenarioError::at(
            idx,
            format!("hold_ms {hold_ms} out of range [{HOLD_MS_MIN}, {HOLD_MS_MAX}]"),
        ));
    }
    if hold_ms > timeout_ms {
        return Err(ScenarioError::at(
            idx,
            format!("hold_ms {hold_ms} must be <= timeout_ms {timeout_ms}"),
        ));
    }
    Ok(())
}

/// Validate a `stable_frames` count (task 083): `[2, 1000]`. `1` is a no-op (any single frame is
/// "1 stable frame") — a stable_frames STEP with `n<2` is a likely typo; reject it. (The
/// `expect_golden.stable_frames` field uses `1` as its OFF sentinel and validates only when `>1`.)
fn validate_stable_frames(idx: usize, n: u32) -> Result<(), ScenarioError> {
    if !(2..=STABLE_FRAMES_MAX).contains(&n) {
        return Err(ScenarioError::at(
            idx,
            format!("stable_frames `n` out of range [2, {STABLE_FRAMES_MAX}]"),
        ));
    }
    Ok(())
}

/// Convert + validate a redaction rect. Width 0 is a no-op redaction — a typo that would
/// silently leak the region into a golden (adv MINOR); reject it.
fn mask_from(idx: usize, m: &RawMask) -> Result<MaskSpec, ScenarioError> {
    if m.width == 0 {
        return Err(ScenarioError::at(
            idx,
            "mask `width` must be >= 1 (a width-0 mask redacts nothing)",
        ));
    }
    Ok(MaskSpec {
        row: m.row,
        col: m.col,
        width: m.width,
    })
}

// ── raw serde envelope (phase 1) ─────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawScenario {
    name: String,
    #[serde(default)]
    description: String,
    command: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    terminal: RawTerminal,
    #[serde(default)]
    env: RawEnv,
    #[serde(default)]
    deadline_ms: Option<u64>,
    #[serde(default)]
    steps: Vec<toml::Table>,
    /// Scenario-level masks applied to EVERY `expect_golden` (design D6).
    #[serde(default)]
    mask: Vec<RawMask>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTerminal {
    #[serde(default = "default_rows")]
    rows: u16,
    #[serde(default = "default_cols")]
    cols: u16,
    #[serde(default)]
    respond_to_queries: bool,
}
impl Default for RawTerminal {
    fn default() -> Self {
        Self {
            rows: default_rows(),
            cols: default_cols(),
            respond_to_queries: false,
        }
    }
}
fn default_rows() -> u16 {
    24
}
fn default_cols() -> u16 {
    80
}

/// `[env]` — `allow` is the one reserved key; everything else is a `KEY = "v"` set.
#[derive(Debug, Default, Deserialize)]
struct RawEnv {
    #[serde(default)]
    allow: Vec<String>,
    #[serde(flatten)]
    vars: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMask {
    row: u16,
    col: u16,
    width: u16,
}

// ── per-action arg structs (phase 2) ─────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitForTextArgs {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    regex: Option<String>,
    #[serde(default)]
    absent: bool,
    #[serde(default = "d_wait_timeout")]
    timeout_ms: u64,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitArgs {
    ms: u64,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettleArgs {
    #[serde(default = "d_quiet")]
    quiet_ms: u64,
    #[serde(default = "d_settle_timeout")]
    timeout_ms: u64,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HoldSettleArgs {
    hold_ms: u64,
    #[serde(default = "d_quiet")]
    quiet_ms: u64,
    #[serde(default = "d_settle_timeout")]
    timeout_ms: u64,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StableFramesArgs {
    n: u32,
    #[serde(default = "d_quiet")]
    quiet_ms: u64,
    #[serde(default = "d_settle_timeout")]
    timeout_ms: u64,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TypeTextArgs {
    text: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeysArgs {
    keys: Vec<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PasteArgs {
    text: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResizeArgs {
    rows: u16,
    cols: u16,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectGoldenArgs {
    name: String,
    #[serde(default = "d_tier")]
    tier: Tier,
    #[serde(default)]
    retries: u32,
    #[serde(default)]
    hold_ms: u64,
    #[serde(default = "d_stable_frames")]
    stable_frames: u32,
    #[serde(default = "d_quiet")]
    quiet_ms: u64,
    #[serde(default = "d_settle_timeout")]
    timeout_ms: u64,
    #[serde(default)]
    mask: Vec<RawMask>,
    #[serde(default)]
    xfail: Option<XfailMeta>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AssertArgs {
    text: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectExitArgs {
    #[serde(default)]
    code: Option<i32>,
    #[serde(default = "d_wait_timeout")]
    timeout_ms: u64,
}

fn d_quiet() -> u64 {
    DEFAULT_QUIET_MS
}
fn d_settle_timeout() -> u64 {
    DEFAULT_SETTLE_TIMEOUT_MS
}
fn d_wait_timeout() -> u64 {
    DEFAULT_WAIT_TIMEOUT_MS
}
fn d_tier() -> Tier {
    Tier::Cell
}
fn d_stable_frames() -> u32 {
    1
}
/// Shared bounds for the frame-stability settle params (task 083) — mirror the daemon's
/// `validate_stability_params` so a bad scenario fails at parse (scenario_error, exit 2) instead
/// of surfacing as an RPC error mid-drive.
const HOLD_MS_MIN: u64 = 10;
const HOLD_MS_MAX: u64 = 60_000;
const STABLE_FRAMES_MAX: u32 = 1_000;

// ── parse ────────────────────────────────────────────────────────────────────

/// Parse + validate a scenario from a TOML string. Every error is actionable and
/// fails closed (design D6).
pub fn parse(text: &str) -> Result<Scenario, ScenarioError> {
    let raw: RawScenario = toml::from_str(text)
        .map_err(|e| ScenarioError(format!("scenario TOML parse error: {e}")))?;

    validate_name("scenario", &raw.name)?;
    if raw.command.is_empty() {
        return Err(ScenarioError(
            "scenario `command` must be a non-empty argv array".into(),
        ));
    }
    let rows = raw.terminal.rows;
    let cols = raw.terminal.cols;
    if !(5..=200).contains(&rows) || !(20..=500).contains(&cols) {
        return Err(ScenarioError(format!(
            "[terminal] size {cols}x{rows} out of range (cols 20..=500, rows 5..=200)"
        )));
    }

    // `[env]` values must be strings (secrets/typing hygiene); reject a stray table/int.
    let mut vars = BTreeMap::new();
    for (k, v) in raw.env.vars {
        match v {
            toml::Value::String(s) => {
                vars.insert(k, s);
            }
            other => {
                return Err(ScenarioError(format!(
                    "[env] {k} must be a string, got {}",
                    other.type_str()
                )));
            }
        }
    }
    let env = EnvBlock {
        vars,
        allow: raw.env.allow,
    };

    // `cwd` is scenario-dir-relative and contained (084 F2). An absolute path is refused
    // rather than silently honoured: it would be baked into the run identity and make the
    // committed golden untrusted on any other machine. `..` is refused so a scenario
    // cannot walk the runner out of its own directory.
    let cwd = match raw.cwd {
        None => None,
        Some(c) => {
            let p = std::path::Path::new(&c);
            if p.is_absolute() {
                return Err(ScenarioError(format!(
                    "`cwd` must be relative to the scenario directory, got absolute `{c}` \
                     (an absolute host path would make the golden untrusted elsewhere)"
                )));
            }
            if p.components()
                .any(|comp| matches!(comp, std::path::Component::ParentDir))
            {
                return Err(ScenarioError(format!(
                    "`cwd` must stay inside the scenario directory, got `{c}`"
                )));
            }
            Some(c)
        }
    };

    let scenario_masks: Vec<MaskSpec> = raw
        .mask
        .iter()
        .map(|m| mask_from(0, m))
        .collect::<Result<_, _>>()?;

    let mut steps = Vec::with_capacity(raw.steps.len());
    for (idx, tbl) in raw.steps.into_iter().enumerate() {
        steps.push(parse_step(idx, tbl, &scenario_masks)?);
    }

    // Frame names must be unique (a duplicate golden name is ambiguous).
    let mut seen = std::collections::HashSet::new();
    for (idx, s) in steps.iter().enumerate() {
        if let Step::ExpectGolden { name, .. } = s {
            if !seen.insert(name.clone()) {
                return Err(ScenarioError::at(
                    idx,
                    format!("duplicate expect_golden name {name:?}"),
                ));
            }
        }
    }

    Ok(Scenario {
        name: raw.name,
        description: raw.description,
        command: raw.command,
        cwd,
        terminal: TerminalCfg {
            rows,
            cols,
            respond_to_queries: raw.terminal.respond_to_queries,
        },
        env,
        deadline_ms: raw.deadline_ms.unwrap_or(DEFAULT_DEADLINE_MS),
        steps,
    })
}

fn parse_step(
    idx: usize,
    tbl: toml::Table,
    scenario_masks: &[MaskSpec],
) -> Result<Step, ScenarioError> {
    // Distinguish an ABSENT tag from a PRESENT-BUT-WRONG-TYPE one (adv MINOR: an
    // `action = 42` used to misreport "missing action").
    let action = match tbl.get("action") {
        Some(v) if v.is_str() => v.as_str().unwrap().to_string(),
        Some(_) => return Err(ScenarioError::at(idx, "`action` must be a string")),
        None => return Err(ScenarioError::at(idx, "missing `action` (string) tag")),
    };

    // The remaining fields, minus the tag, deserialize into the per-action struct.
    let mut rest = tbl;
    rest.remove("action");
    let val = toml::Value::Table(rest);

    macro_rules! args {
        ($t:ty) => {
            val.try_into::<$t>()
                .map_err(|e| ScenarioError::at(idx, format!("`{action}`: {e}")))?
        };
    }

    let step = match action.as_str() {
        "wait_for_text" => {
            let a: WaitForTextArgs = args!(WaitForTextArgs);
            if a.text.is_some() == a.regex.is_some() {
                return Err(ScenarioError::at(
                    idx,
                    "wait_for_text needs exactly one of `text` or `regex`",
                ));
            }
            Step::WaitForText {
                text: a.text,
                regex: a.regex,
                absent: a.absent,
                timeout_ms: a.timeout_ms,
            }
        }
        "wait" => {
            let a: WaitArgs = args!(WaitArgs);
            Step::Wait { ms: a.ms }
        }
        "settle" => {
            let a: SettleArgs = args!(SettleArgs);
            Step::Settle {
                quiet_ms: a.quiet_ms,
                timeout_ms: a.timeout_ms,
            }
        }
        "hold_settle" => {
            let a: HoldSettleArgs = args!(HoldSettleArgs);
            validate_hold_ms(idx, a.hold_ms, a.timeout_ms)?;
            Step::HoldSettle {
                hold_ms: a.hold_ms,
                quiet_ms: a.quiet_ms,
                timeout_ms: a.timeout_ms,
            }
        }
        "stable_frames" => {
            let a: StableFramesArgs = args!(StableFramesArgs);
            validate_stable_frames(idx, a.n)?;
            Step::StableFrames {
                n: a.n,
                quiet_ms: a.quiet_ms,
                timeout_ms: a.timeout_ms,
            }
        }
        "type_text" => {
            let a: TypeTextArgs = args!(TypeTextArgs);
            Step::TypeText { text: a.text }
        }
        "keys" => {
            let a: KeysArgs = args!(KeysArgs);
            if a.keys.is_empty() {
                return Err(ScenarioError::at(idx, "keys must be a non-empty array"));
            }
            if a.keys.iter().any(|k| k.is_empty()) {
                return Err(ScenarioError::at(idx, "keys entries must be non-empty"));
            }
            Step::Keys { keys: a.keys }
        }
        "paste" => {
            let a: PasteArgs = args!(PasteArgs);
            Step::Paste { text: a.text }
        }
        "resize" => {
            let a: ResizeArgs = args!(ResizeArgs);
            if !(5..=200).contains(&a.rows) || !(20..=500).contains(&a.cols) {
                return Err(ScenarioError::at(
                    idx,
                    format!(
                        "resize {}x{} out of range (cols 20..=500, rows 5..=200)",
                        a.cols, a.rows
                    ),
                ));
            }
            Step::Resize {
                rows: a.rows,
                cols: a.cols,
            }
        }
        "expect_golden" => {
            let a: ExpectGoldenArgs = args!(ExpectGoldenArgs);
            // The golden name becomes a filesystem path component — must be safe.
            validate_name("expect_golden", &a.name).map_err(|e| ScenarioError::at(idx, e))?;
            // Optional frame-stability for the pre-capture settle (task 083). hold_ms==0 /
            // stable_frames==1 keep the default quiet settle.
            if a.hold_ms != 0 {
                validate_hold_ms(idx, a.hold_ms, a.timeout_ms)?;
            }
            if a.stable_frames != 1 {
                validate_stable_frames(idx, a.stable_frames)?;
            }
            // Scenario-level masks precede the per-step masks.
            let mut masks = scenario_masks.to_vec();
            for m in &a.mask {
                masks.push(mask_from(idx, m)?);
            }
            Step::ExpectGolden {
                name: a.name,
                tier: a.tier,
                retries: a.retries,
                hold_ms: a.hold_ms,
                stable_frames: a.stable_frames,
                quiet_ms: a.quiet_ms,
                timeout_ms: a.timeout_ms,
                masks,
                xfail: a.xfail,
            }
        }
        "assert_contains" => {
            let a: AssertArgs = args!(AssertArgs);
            Step::AssertContains { text: a.text }
        }
        "assert_not_contains" => {
            let a: AssertArgs = args!(AssertArgs);
            Step::AssertNotContains { text: a.text }
        }
        "expect_exit" => {
            let a: ExpectExitArgs = args!(ExpectExitArgs);
            Step::ExpectExit {
                code: a.code,
                timeout_ms: a.timeout_ms,
            }
        }
        // Explicitly deferred (design D10) — rejected, never silently ignored.
        "mouse" | "focus" | "bracketed_paste" => {
            return Err(ScenarioError::at(
                idx,
                format!(
                    "`{action}` steps are not supported in this runner \
                     (mouse/focus/bracketed-paste are deferred; tracked in docs/tasks/081)"
                ),
            ));
        }
        other => {
            return Err(ScenarioError::at(idx, format!("unknown action {other:?}")));
        }
    };
    Ok(step)
}

/// Load + parse a scenario file.
pub fn load(path: &std::path::Path) -> Result<Scenario, ScenarioError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| ScenarioError(format!("read {}: {e}", path.display())))?;
    parse(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELLO: &str = r#"
name = "hello"
description = "demo"
command = ["/bin/sh", "-c", "printf hi"]

[terminal]
rows = 24
cols = 80
respond_to_queries = false

[env]
LC_ALL = "C.UTF-8"

[[steps]]
action = "wait_for_text"
text = "hi"
timeout_ms = 5000

[[steps]]
action = "expect_golden"
name = "start"
tier = "cell"
"#;

    #[test]
    fn valid_scenario_parses() {
        let s = parse(HELLO).unwrap();
        assert_eq!(s.name, "hello");
        assert_eq!(s.command, ["/bin/sh", "-c", "printf hi"]);
        assert_eq!(s.terminal.rows, 24);
        assert_eq!(s.terminal.cols, 80);
        assert_eq!(
            s.env.vars.get("LC_ALL").map(String::as_str),
            Some("C.UTF-8")
        );
        assert_eq!(s.deadline_ms, DEFAULT_DEADLINE_MS);
        assert_eq!(s.steps.len(), 2);
        assert_eq!(s.steps[0].action(), "wait_for_text");
        assert!(matches!(
            &s.steps[1],
            Step::ExpectGolden { name, tier: Tier::Cell, .. } if name == "start"
        ));
    }

    #[test]
    fn unknown_action_fails_closed() {
        let toml = r#"
name="x"
command=["true"]
[[steps]]
action = "teleport"
"#;
        let e = parse(toml).unwrap_err();
        assert!(e.0.contains("unknown action"), "{e}");
        assert!(e.0.contains("teleport"), "{e}");
    }

    #[test]
    fn typoed_field_fails_closed_via_deny_unknown_fields() {
        let toml = r#"
name="x"
command=["true"]
[[steps]]
action = "wait_for_text"
txt = "oops"
"#;
        // `txt` is unknown for wait_for_text → deny_unknown_fields rejects.
        assert!(parse(toml).is_err());
    }

    #[test]
    fn deferred_steps_are_rejected_not_ignored() {
        for act in ["mouse", "focus", "bracketed_paste"] {
            let toml = format!("name=\"x\"\ncommand=[\"true\"]\n[[steps]]\naction=\"{act}\"\n");
            let e = parse(&toml).unwrap_err();
            assert!(e.0.contains("not supported"), "{act}: {e}");
        }
    }

    #[test]
    fn expect_golden_defaults_to_cell_tier() {
        let toml = r#"
name="x"
command=["true"]
[[steps]]
action="expect_golden"
name="f"
"#;
        let s = parse(toml).unwrap();
        assert!(matches!(
            &s.steps[0],
            Step::ExpectGolden {
                tier: Tier::Cell,
                retries: 0,
                ..
            }
        ));
    }

    #[test]
    fn tier_names_parse_all_three() {
        for (t, want) in [
            ("cell", Tier::Cell),
            ("pixel", Tier::Pixel),
            ("exact", Tier::Exact),
        ] {
            let toml = format!(
                "name=\"x\"\ncommand=[\"true\"]\n[[steps]]\naction=\"expect_golden\"\nname=\"f\"\ntier=\"{t}\"\n"
            );
            let s = parse(&toml).unwrap();
            assert!(matches!(&s.steps[0], Step::ExpectGolden { tier, .. } if *tier == want));
        }
    }

    #[test]
    fn wait_for_text_needs_exactly_one_of_text_or_regex() {
        let both = r#"name="x"
command=["true"]
[[steps]]
action="wait_for_text"
text="a"
regex="b"
"#;
        assert!(parse(both).unwrap_err().0.contains("exactly one"));
        let neither = r#"name="x"
command=["true"]
[[steps]]
action="wait_for_text"
"#;
        assert!(parse(neither).unwrap_err().0.contains("exactly one"));
    }

    #[test]
    fn duplicate_golden_names_rejected() {
        let toml = r#"name="x"
command=["true"]
[[steps]]
action="expect_golden"
name="dup"
[[steps]]
action="expect_golden"
name="dup"
"#;
        assert!(parse(toml).unwrap_err().0.contains("duplicate"));
    }

    #[test]
    fn empty_command_rejected() {
        let toml = "name=\"x\"\ncommand=[]\n";
        assert!(parse(toml).unwrap_err().0.contains("command"));
    }

    #[test]
    fn out_of_range_terminal_rejected() {
        let toml = "name=\"x\"\ncommand=[\"true\"]\n[terminal]\nrows=1\ncols=80\n";
        assert!(parse(toml).unwrap_err().0.contains("out of range"));
    }

    #[test]
    fn env_non_string_value_rejected() {
        let toml = "name=\"x\"\ncommand=[\"true\"]\n[env]\nFOO=3\n";
        assert!(parse(toml).unwrap_err().0.contains("must be a string"));
    }

    #[test]
    fn env_empty_string_is_a_set_not_unset() {
        // Design D4: `KEY = ""` SETS an empty var; it is NOT overloaded as unset.
        let toml = "name=\"x\"\ncommand=[\"true\"]\n[env]\nNO_COLOR=\"\"\n";
        let s = parse(toml).unwrap();
        assert_eq!(s.env.vars.get("NO_COLOR").map(String::as_str), Some(""));
    }

    #[test]
    fn cwd_defaults_to_absent_so_the_child_keeps_the_sandbox_home() {
        let s = parse("name=\"x\"\ncommand=[\"true\"]\n").unwrap();
        assert_eq!(s.cwd, None);
    }

    #[test]
    fn a_relative_cwd_parses_and_is_kept_verbatim() {
        let s = parse("name=\"x\"\ncommand=[\"true\"]\ncwd=\"sub/dir\"\n").unwrap();
        assert_eq!(s.cwd.as_deref(), Some("sub/dir"));
        // `.` is the common case (the scenario's own directory) and must survive.
        let dot = parse("name=\"x\"\ncommand=[\"true\"]\ncwd=\".\"\n").unwrap();
        assert_eq!(dot.cwd.as_deref(), Some("."));
    }

    /// An absolute `cwd` would be baked into the run identity, making the committed golden
    /// `untrusted` on every other machine — so it is refused, never silently honoured.
    #[test]
    fn an_absolute_cwd_is_refused() {
        let e = parse("name=\"x\"\ncommand=[\"true\"]\ncwd=\"/tmp\"\n").unwrap_err();
        assert!(e.0.contains("relative"), "unhelpful error: {}", e.0);
        assert!(
            e.0.contains("/tmp"),
            "error does not name the offending path: {}",
            e.0
        );
    }

    /// Containment: a scenario must not be able to walk the runner out of its own directory.
    /// This invariant is the reason `cwd` is safe to join onto the scenario dir unchecked.
    #[test]
    fn a_cwd_escaping_the_scenario_dir_is_refused() {
        for bad in ["..", "../elsewhere", "sub/../../elsewhere", "./../x"] {
            let toml = format!("name=\"x\"\ncommand=[\"true\"]\ncwd=\"{bad}\"\n");
            let e = match parse(&toml) {
                Err(e) => e,
                Ok(_) => panic!("`cwd = {bad:?}` escaped containment"),
            };
            assert!(
                e.0.contains("inside the scenario directory"),
                "wrong error for {bad:?}: {}",
                e.0
            );
        }
    }

    #[test]
    fn env_allow_list_parses() {
        let toml =
            "name=\"x\"\ncommand=[\"true\"]\n[env]\nallow=[\"PATH\",\"HOME\"]\nLC_ALL=\"C\"\n";
        let s = parse(toml).unwrap();
        assert_eq!(s.env.allow, ["PATH", "HOME"]);
        assert_eq!(s.env.vars.get("LC_ALL").map(String::as_str), Some("C"));
        assert!(
            !s.env.vars.contains_key("allow"),
            "`allow` is not an env var"
        );
    }

    #[test]
    fn stable_frames_and_retries_parse() {
        let toml = r#"name="x"
command=["true"]
[[steps]]
action="stable_frames"
n=3
[[steps]]
action="expect_golden"
name="f"
retries=2
"#;
        let s = parse(toml).unwrap();
        assert!(matches!(&s.steps[0], Step::StableFrames { n: 3, .. }));
        assert!(matches!(&s.steps[1], Step::ExpectGolden { retries: 2, .. }));
    }

    #[test]
    fn hold_settle_parses_hold_ms() {
        let toml = r#"name="x"
command=["true"]
[[steps]]
action="hold_settle"
hold_ms=600
"#;
        let s = parse(toml).unwrap();
        assert!(matches!(&s.steps[0], Step::HoldSettle { hold_ms: 600, .. }));
    }

    #[test]
    fn hold_settle_requires_hold_ms() {
        // `hold_ms` is the whole point of hold_settle — a missing field fails closed.
        let toml = "name=\"x\"\ncommand=[\"true\"]\n[[steps]]\naction=\"hold_settle\"\n";
        assert!(parse(toml).is_err());
    }

    #[test]
    fn hold_ms_out_of_range_rejected() {
        for (hold, tmo) in [("5", "10000"), ("70000", "80000"), ("2000", "1000")] {
            let toml = format!(
                "name=\"x\"\ncommand=[\"true\"]\n[[steps]]\naction=\"hold_settle\"\nhold_ms={hold}\ntimeout_ms={tmo}\n"
            );
            assert!(
                parse(&toml).unwrap_err().0.contains("hold_ms"),
                "hold={hold} tmo={tmo}"
            );
        }
    }

    #[test]
    fn stable_frames_step_requires_n_ge_2() {
        for n in ["0", "1"] {
            let toml = format!(
                "name=\"x\"\ncommand=[\"true\"]\n[[steps]]\naction=\"stable_frames\"\nn={n}\n"
            );
            assert!(
                parse(&toml).unwrap_err().0.contains("stable_frames"),
                "n={n}"
            );
        }
    }

    #[test]
    fn expect_golden_stability_fields_parse_and_default_off() {
        let toml = r#"name="x"
command=["true"]
[[steps]]
action="expect_golden"
name="a"
[[steps]]
action="expect_golden"
name="b"
hold_ms=400
stable_frames=3
"#;
        let s = parse(toml).unwrap();
        assert!(matches!(
            &s.steps[0],
            Step::ExpectGolden {
                hold_ms: 0,
                stable_frames: 1,
                ..
            }
        ));
        assert!(matches!(
            &s.steps[1],
            Step::ExpectGolden {
                hold_ms: 400,
                stable_frames: 3,
                ..
            }
        ));
    }

    #[test]
    fn masks_merge_scenario_and_step_level() {
        let toml = r#"name="x"
command=["true"]
[[mask]]
row=0
col=0
width=10
[[steps]]
action="expect_golden"
name="f"
[[steps.mask]]
row=1
col=2
width=3
"#;
        let s = parse(toml).unwrap();
        let Step::ExpectGolden { masks, .. } = &s.steps[0] else {
            panic!("expected expect_golden")
        };
        assert_eq!(masks.len(), 2);
        assert_eq!(
            masks[0],
            MaskSpec {
                row: 0,
                col: 0,
                width: 10
            }
        );
        assert_eq!(
            masks[1],
            MaskSpec {
                row: 1,
                col: 2,
                width: 3
            }
        );
    }

    #[test]
    fn expect_exit_parses_optional_code() {
        let toml = r#"name="x"
command=["true"]
[[steps]]
action="expect_exit"
code=42
[[steps]]
action="expect_exit"
"#;
        let s = parse(toml).unwrap();
        assert!(matches!(
            &s.steps[0],
            Step::ExpectExit { code: Some(42), .. }
        ));
        assert!(matches!(&s.steps[1], Step::ExpectExit { code: None, .. }));
    }

    #[test]
    fn xfail_reserved_opaque_parse() {
        // Design D6: xfail parses into the 082 shape, 081 takes no action.
        let toml = r##"name="x"
command=["true"]
[[steps]]
action="expect_golden"
name="f"
[steps.xfail]
reason="known"
owner="aria"
issue="#1"
expiry="2026-12-31"
"##;
        let s = parse(toml).unwrap();
        let Step::ExpectGolden { xfail, .. } = &s.steps[0] else {
            panic!()
        };
        assert_eq!(xfail.as_ref().unwrap().owner, "aria");
    }

    #[test]
    fn deadline_ms_parsed() {
        let toml = "name=\"x\"\ncommand=[\"true\"]\ndeadline_ms=1234\n";
        assert_eq!(parse(toml).unwrap().deadline_ms, 1234);
    }

    #[test]
    fn hostile_names_rejected() {
        // adv MAJOR: a name that becomes a filesystem path component must be a safe single
        // component — no traversal / absolute / control chars.
        let golden_traversal = r#"name="x"
command=["true"]
[[steps]]
action="expect_golden"
name="../../../../tmp/pwned"
"#;
        assert!(
            parse(golden_traversal)
                .unwrap_err()
                .0
                .contains("path component")
        );
        assert!(parse("name=\"/etc/passwd\"\ncommand=[\"true\"]\n").is_err());
        assert!(parse("name=\"a/b\"\ncommand=[\"true\"]\n").is_err());
        assert!(parse("name=\"a\\nb\"\ncommand=[\"true\"]\n").is_err());
        let long = format!("name=\"{}\"\ncommand=[\"true\"]\n", "x".repeat(200));
        assert!(parse(&long).unwrap_err().0.contains("too long"));
        // A plain name still parses.
        assert!(parse("name=\"ok-name_1\"\ncommand=[\"true\"]\n").is_ok());
    }

    #[test]
    fn zero_width_mask_rejected() {
        // adv MINOR: a width-0 mask redacts nothing — a typo that leaks a region.
        let toml = r#"name="x"
command=["true"]
[[steps]]
action="expect_golden"
name="f"
[[steps.mask]]
row=0
col=0
width=0
"#;
        assert!(parse(toml).unwrap_err().0.contains("width"));
        // A scenario-level width-0 mask is rejected too.
        let toml2 = "name=\"x\"\ncommand=[\"true\"]\n[[mask]]\nrow=0\ncol=0\nwidth=0\n";
        assert!(parse(toml2).unwrap_err().0.contains("width"));
    }

    #[test]
    fn non_string_action_reports_wrong_type_not_missing() {
        // adv MINOR: `action = 42` misreported as "missing"; it is present-but-wrong-type.
        let toml = "name=\"x\"\ncommand=[\"true\"]\n[[steps]]\naction=42\n";
        let e = parse(toml).unwrap_err();
        assert!(e.0.contains("must be a string"), "{e}");
        assert!(!e.0.contains("missing"), "{e}");
    }

    #[test]
    fn empty_keys_element_rejected() {
        // adv MINOR: an empty key string is a silent no-op.
        let toml =
            "name=\"x\"\ncommand=[\"true\"]\n[[steps]]\naction=\"keys\"\nkeys=[\"gg\",\"\"]\n";
        assert!(parse(toml).unwrap_err().0.contains("non-empty"));
    }
}
