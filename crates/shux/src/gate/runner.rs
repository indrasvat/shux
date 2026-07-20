//! The daemon-backed scenario drive loop (task 081). Everything race-, timeout-, and
//! child-exit-sensitive lives here; the pure decisions are in the sibling modules.
//!
//! Ownership (design D1/D2): this emits RAW SIGNALS to the `--trace` channel (design
//! D3) and returns a provisional exit — task 082 installs the frozen `report.json` +
//! exit map. Nothing here prints the frozen stdout summary.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use shux_raster::Rasterizer;
use shux_vt::{
    FINGERPRINT_SCHEMA, Fingerprint, FrameEnvelope, MaskSet, RENDERER_FORMAT_VERSION,
    SCHEMA_VERSION, Tier, TolParams, capture_sha256, mask_hash, unicode_width_version,
};
use tokio::net::UnixStream;
use tokio::sync::{Mutex, Notify};

use super::compare::compare_frame;
use super::env_plan::{EnvPlan, SandboxDirs, build_env_plan, cmd_env_hash, scenario_hash};
use super::keys;
use super::outcome::{FrameKind, FrameOutcome, RunOutcome, TerminalOutcome};
use super::scenario::{MaskSpec, Scenario, Step};
use super::signal::{RunnerSignal, TimeoutClass};
use crate::cli::{RpcClientError, rpc_call};

const FONT_SIZE: f32 = 16.0;
/// The scratch quota (`lens_scratch::SCRATCH_QUOTA`) surfaced as a raw signal.
const SCRATCH_QUOTA: usize = 16;
const RESOURCE_EXHAUSTED: i64 = -32012;
/// How long, after the final step of a completed run, to watch for an unexpected child
/// exit (adv 082 Agent D: a fixed 500 ms missed a crash ~0.8 s after the final frame,
/// false-passing a crashing TUI). Bounded by the remaining scenario deadline. This is a
/// heuristic window; a robust liveness monitor (catch any exit before the deadline without
/// penalizing a held-forever frame) is task 083's settle-hardening domain.
const POST_COMPARE_GRACE_MS: u64 = 2000;

/// Where the NDJSON trace goes (design D3). `None` = no trace emitted.
pub enum TraceTarget {
    Stdout,
    Path(PathBuf),
}

/// Emit a single `parse_error` to the trace target (081 contract: a malformed scenario
/// still leaves a greppable trace). 082's driver calls this before it has a `Scenario`.
pub fn emit_parse_error_trace(trace_target: Option<TraceTarget>, message: &str) {
    if let Ok(mut trace) = Trace::open(trace_target) {
        trace.emit(RunnerSignal::ParseError {
            message: message.to_string(),
        });
    }
}

/// The NDJSON trace writer (design D3). Line-buffered + flushed so a crash still leaves
/// a partial, greppable trace.
struct Trace {
    sink: Option<Box<dyn std::io::Write + Send>>,
    /// Every signal, retained for the provisional exit decision + a minimal summary.
    signals: Vec<RunnerSignal>,
}

impl Trace {
    fn open(target: Option<TraceTarget>) -> std::io::Result<Self> {
        let sink: Option<Box<dyn std::io::Write + Send>> = match target {
            None => None,
            Some(TraceTarget::Stdout) => Some(Box::new(std::io::stdout())),
            Some(TraceTarget::Path(p)) => Some(Box::new(std::fs::File::create(p)?)),
        };
        Ok(Self {
            sink,
            signals: Vec::new(),
        })
    }

    fn emit(&mut self, sig: RunnerSignal) {
        if let Some(w) = self.sink.as_mut() {
            let _ = writeln!(w, "{}", sig.to_ndjson());
            let _ = w.flush();
        }
        self.signals.push(sig);
    }
}

/// A concurrent monitor for the scratch child's exit (design D7). Subscribes from a
/// PRE-SPAWN sequence cursor so a fast-exiting child cannot publish its exit before we
/// listen (codex's race). `notify_one` stores a permit so [`ExitMonitor::wait`] can
/// never miss the single exit event.
struct ExitMonitor {
    seen: Arc<Mutex<Option<Option<i32>>>>,
    notify: Arc<Notify>,
    handle: tokio::task::JoinHandle<()>,
}

impl ExitMonitor {
    fn spawn(mut stream: UnixStream, from_seq: u64, pane_id: String) -> Self {
        let seen: Arc<Mutex<Option<Option<i32>>>> = Arc::new(Mutex::new(None));
        let notify = Arc::new(Notify::new());
        let seen_task = seen.clone();
        let notify_task = notify.clone();
        let handle = tokio::spawn(async move {
            let mut seq = from_seq;
            loop {
                // `max_events: 1` makes `events.watch` return the INSTANT it has one
                // matching event (from history replay or the live tail) instead of
                // blocking the full `timeout_ms` collecting up to the default 100 — so a
                // fast-exiting child's `pane.exited` is seen promptly, not at the deadline.
                let params = serde_json::json!({
                    "from_seq": seq,
                    "filter": ["pane.exited"],
                    "max_events": 1,
                    "timeout_ms": 2000,
                });
                match rpc_call(&mut stream, "events.watch", params).await {
                    Ok(v) => {
                        if let Some(events) = v.get("events").and_then(|e| e.as_array()) {
                            for ev in events {
                                // Wire shape: `{type:"pane.exited", data:{type:"PaneExited",
                                // data:{pane_id, exit_status,…}}}` — the payload is
                                // double-nested because `EventData` is `#[serde(tag,content)]`.
                                let is_exit =
                                    ev.get("type").and_then(|t| t.as_str()) == Some("pane.exited");
                                let pid = ev.pointer("/data/data/pane_id").and_then(|p| p.as_str());
                                if is_exit && pid == Some(pane_id.as_str()) {
                                    // The daemon fires `-1` for a signal death (no POSIX
                                    // code); the runner's signal contract represents that
                                    // as `code: None` (signal.rs — a signal-kill omits the
                                    // code, never a fake `-1`). No real child exits -1, so
                                    // the mapping is unambiguous.
                                    let code = ev
                                        .pointer("/data/data/exit_status")
                                        .and_then(|c| c.as_i64())
                                        .filter(|&c| c != -1)
                                        .map(|c| c as i32);
                                    *seen_task.lock().await = Some(code);
                                    notify_task.notify_one();
                                    return;
                                }
                            }
                        }
                        seq = v.get("next_seq").and_then(|s| s.as_u64()).unwrap_or(seq);
                    }
                    Err(_) => {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                }
            }
        });
        Self {
            seen,
            notify,
            handle,
        }
    }

    /// The exit code if the child has exited (`Some(None)` = signal-kill), else `None`.
    async fn peek(&self) -> Option<Option<i32>> {
        *self.seen.lock().await
    }

    /// Resolve once the child has exited. Safe against the notify/set race via the
    /// stored `notify_one` permit.
    async fn wait(&self) -> Option<i32> {
        loop {
            if let Some(code) = *self.seen.lock().await {
                return code;
            }
            self.notify.notified().await;
        }
    }
}

impl Drop for ExitMonitor {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// The result of driving one step.
enum StepFlow {
    /// Continue to the next step.
    Continue,
    /// Stop the scenario (a fatal raw signal was emitted).
    Stop,
}

/// Build the freshly-computed fingerprint for THIS build/scenario at `tier`, with the
/// real `scenario_hash`/`cmd_env_hash` (design D5). The compare uses this for staleness.
fn current_fp(tier: Tier, masks: &MaskSet, scenario_hash: &str, cmd_env_hash: &str) -> Fingerprint {
    Fingerprint {
        fp_schema: FINGERPRINT_SCHEMA,
        schema: SCHEMA_VERSION,
        renderer_format_version: RENDERER_FORMAT_VERSION,
        raster_font_fingerprint: shux_raster::builtin_font_fingerprint(FONT_SIZE),
        unicode_width_ver: unicode_width_version(),
        tol: tier,
        tol_params: TolParams::default(),
        mask_hash: mask_hash(masks),
        platform: (tier != Tier::Cell).then(shux_raster::os_arch),
        shux_version: env!("CARGO_PKG_VERSION").to_string(),
        capture_sha256: String::new(),
        rgba_sha256: None,
        png_sha256: None,
        scenario_hash: scenario_hash.to_string(),
        cmd_env_hash: cmd_env_hash.to_string(),
    }
}

fn build_mask_set(masks: &[MaskSpec]) -> MaskSet {
    let mut m = MaskSet::new();
    for r in masks {
        m = m.with(r.row, r.col, r.width);
    }
    m
}

/// The default golden directory for a scenario: `<scenario-dir>/goldens/<scenario-name>/`.
///
/// Anchored through [`scenario_dir_of`], NOT a second hand-rolled `parent()` — the raw
/// expression here was the last copy of the empty-parent trap, and it is exactly how a
/// symlinked scenario minted a duplicate golden tree.
pub fn default_golden_dir(scenario_path: &Path, scenario: &Scenario) -> PathBuf {
    scenario_dir_of(scenario_path)
        .join("goldens")
        .join(&scenario.name)
}

/// Create the per-scenario sandbox directories under `root`.
fn make_sandbox(root: &Path) -> std::io::Result<SandboxDirs> {
    let sb = SandboxDirs {
        home: root.join("home"),
        tmpdir: root.join("tmp"),
        xdg_config: root.join("config"),
        xdg_state: root.join("state"),
        xdg_data: root.join("data"),
        xdg_cache: root.join("cache"),
        xdg_runtime: root.join("run"),
        shux_socket: root.join("run/shux.sock"),
    };
    for d in [
        &sb.home,
        &sb.tmpdir,
        &sb.xdg_config,
        &sb.xdg_state,
        &sb.xdg_data,
        &sb.xdg_cache,
        &sb.xdg_runtime,
    ] {
        std::fs::create_dir_all(d)?;
    }
    Ok(sb)
}

/// The directory a scenario is anchored to — what its relative `cwd` resolves against and
/// where its default goldens live. THE one place that answers this question.
///
/// Two traps, both found in production:
///
/// 1. `Path::parent()` on a BARE filename returns `Some("")`, not `None`, so an
///    `unwrap_or(".")` never fires and the empty path reaches `canonicalize()` as ENOENT.
///    That broke `shux lens gate scenario.toml` while `./scenario.toml` worked.
/// 2. Without canonicalizing, a SYMLINKED scenario file anchors to the link's directory,
///    not the real one — so `cwd` resolves somewhere unintended and, worse, a second
///    divergent golden tree gets minted beside the symlink while the real one sits
///    untouched (adversarial review).
///
/// Canonicalizing the FILE (not its parent) resolves both, plus any `.`/`..` in between.
/// If the path does not exist yet — `gate init` scaffolding a new scenario — fall back to
/// the lexical parent so scaffolding still works.
pub fn scenario_dir_of(scenario_path: &Path) -> PathBuf {
    let lexical = || match scenario_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    match scenario_path.canonicalize() {
        Ok(real) => real.parent().map(Path::to_path_buf).unwrap_or_else(lexical),
        Err(_) => lexical(),
    }
}

/// Resolve a scenario-relative `cwd` and prove it is CONTAINED in the scenario directory.
///
/// Parse-time validation rejects absolute paths and `..` components, but that is only
/// SYNTACTIC: a symlink inside the scenario directory can still point anywhere, and the
/// child would be spawned there (impl council). So both sides are canonicalized — which
/// resolves symlinks — and containment is re-checked on the real paths before spawn.
fn resolve_contained_cwd(scenario_dir: &Path, rel: &str) -> Result<PathBuf, String> {
    let joined = scenario_dir.join(rel);
    if !joined.is_dir() {
        // 085 F25: distinguish "not there" from "there, but not a directory". Reporting a
        // file as nonexistent sends the author looking for a missing path that is sitting
        // in front of them.
        return Err(if joined.exists() {
            format!(
                "scenario `cwd` is not a directory: '{}' exists but is a file",
                joined.display()
            )
        } else {
            format!(
                "scenario `cwd` does not exist: '{}' (resolved relative to the scenario directory)",
                joined.display()
            )
        });
    }
    let root = scenario_dir.canonicalize().map_err(|e| {
        format!(
            "cannot resolve the scenario directory '{}': {e}",
            scenario_dir.display()
        )
    })?;
    let real = joined
        .canonicalize()
        .map_err(|e| format!("cannot resolve scenario `cwd` '{}': {e}", joined.display()))?;
    if !real.starts_with(&root) {
        return Err(format!(
            "scenario `cwd` escapes the scenario directory: '{rel}' resolves to '{}', outside '{}' \
             (a symlink cannot be used to leave the scenario tree)",
            real.display(),
            root.display()
        ));
    }
    Ok(real)
}

/// An ENOENT spawn failure nearly always means `command[0]` is not on the sandbox PATH,
/// which is deliberately narrow and does NOT inherit the host's. The bare errno names
/// neither the program nor the path it was searched on, so a reader cannot act on it
/// (084 F1 — the same wall the 082 `bat` and 083 `htop`/`vim` dogfoods hit).
/// Returns a REPLACEMENT message, not a suffix: the note is capped at 240 chars, and the
/// bare errno preamble would eat the budget the actionable part needs. ASCII only — the
/// note is sanitized at the output boundary, so a non-ASCII dash would arrive as `?`.
fn program_not_found_message(err: &str, argv: &[String], plan: &EnvPlan) -> Option<String> {
    if !err.contains("No such file or directory") {
        return None;
    }
    let program = argv.first()?;
    // An explicit path failed on its own merits — the search path is not the story.
    if program.contains('/') {
        return None;
    }
    let path = plan
        .env
        .get("PATH")
        .map(String::as_str)
        .unwrap_or("<unset>");
    Some(format!(
        "lens.run failed: '{program}' not found on the sandbox PATH '{path}'. \
         The sandbox does not inherit the host PATH. Fix: set [env] PATH, \
         or [env] allow = [\"PATH\"], or an absolute path in `command`."
    ))
}

/// Assemble a [`RunOutcome`] from the accumulated drive state (design D1). Timing is
/// best-effort wall clock; provenance is the host + bundled-font identity goldens pin.
fn finish(
    scenario: &Scenario,
    started_at_ms: u128,
    start: Instant,
    frames: Vec<FrameOutcome>,
    terminal: Option<TerminalOutcome>,
    has_visual: bool,
    golden_dir: &Path,
) -> RunOutcome {
    RunOutcome {
        scenario_name: scenario.name.clone(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        font_chain_sha256: Some(shux_raster::builtin_font_fingerprint(FONT_SIZE)),
        font_size_px: FONT_SIZE as u16,
        started_at_ms,
        duration_ms: start.elapsed().as_millis() as u64,
        frames,
        terminal,
        has_visual_check: has_visual,
        golden_dir: golden_dir.display().to_string(),
    }
}

/// Derive the scenario-level terminal disposition from the ordered signal list (design
/// D1 — in-memory structured data, NOT trace-text re-parsing). The drive loop breaks on
/// the FIRST fatal signal, so the first match here is the terminal cause. Pre-loop setup
/// failures (quota / daemon spawn) set the terminal EXPLICITLY and never reach this.
fn derive_terminal(signals: &[RunnerSignal]) -> Option<TerminalOutcome> {
    for s in signals {
        match s {
            RunnerSignal::ChildExit { code } => {
                return Some(TerminalOutcome::ChildExit { code: *code });
            }
            RunnerSignal::Timeout {
                class,
                action,
                step_index,
                ..
            } => {
                return Some(match class {
                    TimeoutClass::Step => TerminalOutcome::StepTimeout {
                        action: action.clone().unwrap_or_default(),
                        step_index: step_index.unwrap_or(0),
                    },
                    TimeoutClass::Scenario => TerminalOutcome::ScenarioDeadline {
                        step_index: step_index.unwrap_or(0),
                    },
                    TimeoutClass::FrameSettle | TimeoutClass::NeverStabilized => {
                        TerminalOutcome::SettleNeverStable {
                            action: action.clone().unwrap_or_default(),
                        }
                    }
                });
            }
            RunnerSignal::ParseError { message } => {
                return Some(TerminalOutcome::ScenarioError {
                    message: message.clone(),
                });
            }
            _ => {}
        }
    }
    None
}

/// Drive a parsed scenario against a hidden scratch TUI, emit the raw-signal trace, and
/// return the STRUCTURED outcome (design D1). Owns 081 MECHANICS only — no verdict, no
/// stdout, no exit code. 082's `verdict` layer rolls the outcome into `report.json`.
#[allow(clippy::too_many_arguments)] // one knob per gate CLI flag; a params struct here
// would only rename the same list (established precedent: attach.rs, shux-rpc server.rs).
pub async fn drive_scenario(
    socket_path: &Path,
    scenario: &Scenario,
    scenario_dir: &Path,
    argv: &[String],
    golden_dir: &Path,
    trace_target: Option<TraceTarget>,
    cli_retries: u32,
    cast: Option<PathBuf>,
) -> anyhow::Result<RunOutcome> {
    let start = Instant::now();
    let started_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    // Sandbox + deterministic env plan + provenance hashes.
    let sandbox_root = tempfile::tempdir()?;
    let sandbox = make_sandbox(sandbox_root.path())?;
    let plan = build_env_plan(scenario, &sandbox, &|k| std::env::var(k).ok());
    let sc_hash = scenario_hash(scenario);
    let ce_hash = cmd_env_hash(&plan, &sandbox, argv, &scenario.terminal);
    let rasterizer = Rasterizer::new(FONT_SIZE)?;

    let mut trace = Trace::open(trace_target)?;
    trace.emit(RunnerSignal::ScenarioStart {
        scenario: scenario.name.clone(),
        scenario_hash: sc_hash.clone(),
        cmd_env_hash: ce_hash.clone(),
        rows: scenario.terminal.rows,
        cols: scenario.terminal.cols,
    });

    // A scenario with NO expect_golden can never prove a visual regression (design D6).
    let has_visual = scenario
        .steps
        .iter()
        .any(|s| matches!(s, Step::ExpectGolden { .. }));
    if !has_visual {
        trace.emit(RunnerSignal::NoVisualCheck);
    }

    let mut frames: Vec<FrameOutcome> = Vec::new();

    // Pre-spawn cursor (design D7): head seq BEFORE lens.run.
    let mut mstream = crate::client::ensure_daemon_running_at(socket_path).await?;
    let from_seq = rpc_call(
        &mut mstream,
        "events.watch",
        serde_json::json!({ "timeout_ms": 0 }),
    )
    .await?
    .get("next_seq")
    .and_then(|s| s.as_u64())
    .unwrap_or(0);

    // The child's working directory: the sandbox HOME unless the scenario asked for a
    // directory beside itself (084 F2). `cwd` is validated relative + contained at parse
    // time, so this join cannot escape the scenario dir.
    let child_cwd = match &scenario.cwd {
        Some(rel) => match resolve_contained_cwd(scenario_dir, rel) {
            Ok(p) => p,
            Err(message) => {
                return Ok(finish(
                    scenario,
                    started_at_ms,
                    start,
                    frames,
                    Some(TerminalOutcome::Infra { message }),
                    has_visual,
                    golden_dir,
                ));
            }
        },
        None => sandbox.home.clone(),
    };

    // 4. Spawn the child (deny-by-default env; async — the runner monitors exit).
    let mut stream = crate::client::ensure_daemon_running_at(socket_path).await?;
    let env_obj: serde_json::Map<String, serde_json::Value> = plan
        .env
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    let mut run_params = serde_json::json!({
        "argv": argv,
        "cols": scenario.terminal.cols,
        "rows": scenario.terminal.rows,
        "env": serde_json::Value::Object(env_obj),
        "env_clear": plan.env_clear,
        "cwd": child_cwd.display().to_string(),
        "wait": false,
    });
    // Task 083: arm the asciinema `.cast` recorder AT SPAWN (council) so the child's startup —
    // alt-screen setup, initial geometry, early output — is captured. Ephemeral (a gitignored
    // `.shux/out/` path); never a golden.
    if let Some(cast) = cast.as_deref() {
        run_params["cast"] = serde_json::Value::String(cast.display().to_string());
    }
    let (session_id, pane_id) = match rpc_call(&mut stream, "lens.run", run_params).await {
        Ok(v) => (
            v.get("session_id")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string(),
            v.get("pane_id")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string(),
        ),
        Err(RpcClientError::Rpc { code, .. }) if code == RESOURCE_EXHAUSTED => {
            trace.emit(RunnerSignal::QuotaExceeded {
                limit: SCRATCH_QUOTA,
            });
            return Ok(finish(
                scenario,
                started_at_ms,
                start,
                frames,
                Some(TerminalOutcome::QuotaExceeded {
                    limit: SCRATCH_QUOTA,
                }),
                has_visual,
                golden_dir,
            ));
        }
        Err(e) => {
            let raw = format!("lens.run failed: {e}");
            let message = program_not_found_message(&raw, argv, &plan).unwrap_or(raw);
            trace.emit(RunnerSignal::ParseError {
                message: message.clone(),
            });
            return Ok(finish(
                scenario,
                started_at_ms,
                start,
                frames,
                Some(TerminalOutcome::Infra { message }),
                has_visual,
                golden_dir,
            ));
        }
    };

    let monitor = ExitMonitor::spawn(mstream, from_seq, pane_id.clone());

    // 5. Drive the steps.
    let deadline = Instant::now() + Duration::from_millis(scenario.deadline_ms);
    let mut child_consumed = false;
    // Did the loop STOP early (a timeout / child_exit / parse_error gave a terminal
    // signal)? If so, the outcome is decided and we don't wait for a further exit.
    let mut stopped = false;
    for (idx, step) in scenario.steps.iter().enumerate() {
        // Whole-scenario budget (design D8) — a distinct raw signal.
        let scenario_timeout = |trace: &mut Trace| {
            trace.emit(RunnerSignal::Timeout {
                class: TimeoutClass::Scenario,
                step_index: Some(idx),
                action: None,
                name: None,
                elapsed_ms: Some(scenario.deadline_ms),
                budget_ms: Some(scenario.deadline_ms),
            });
        };
        let now = Instant::now();
        if now >= deadline {
            scenario_timeout(&mut trace);
            stopped = true;
            break;
        }

        // Unexpected child exit BEFORE any visual compare (design D7) — skip the check
        // for an `expect_exit` step (allowed to consume the exit) and when a prior
        // `expect_exit` already consumed it (a trailing `expect_golden` still glances
        // the surviving VT).
        if !child_consumed && !matches!(step, Step::ExpectExit { .. }) {
            if let Some(code) = monitor.peek().await {
                trace.emit(RunnerSignal::ChildExit { code });
                stopped = true;
                break;
            }
        }

        // Race the step against the REMAINING whole-scenario budget so a single long
        // blocking step (a big `wait`, a slow settle) cannot overrun the deadline.
        let flow = match tokio::time::timeout(
            deadline - now,
            drive_step(
                &mut stream,
                &monitor,
                idx,
                step,
                &pane_id,
                golden_dir,
                &sc_hash,
                &ce_hash,
                &rasterizer,
                &mut trace,
                &mut child_consumed,
                &mut frames,
                cli_retries,
            ),
        )
        .await
        {
            Ok(flow) => flow,
            Err(_) => {
                scenario_timeout(&mut trace);
                stopped = true;
                break;
            }
        };
        if matches!(flow, StepFlow::Stop) {
            stopped = true;
            break;
        }
    }

    // A child that exits UNEXPECTEDLY around the final step (e.g. a terminal
    // `expect_golden` whose child paints its golden, settles, then crashes after the
    // compare) must still surface — otherwise a crashing TUI whose last frame matches its
    // golden false-passes (adv MAJOR 2 / adv-082 Agent D). Only when the run COMPLETED
    // normally (not stopped by a terminal signal) and nothing consumed/reported an exit:
    // watch for a pending exit for `POST_COMPARE_GRACE_MS`, capped by the remaining
    // deadline. A child still alive at the end (an interactive TUI blocked on input) times
    // out the grace → no signal, correct. RESIDUAL (task 083 settle-hardening): a crash
    // beyond this window while the child is idle is still missed; the scenario deadline is
    // the ultimate bound.
    let exit_reported = trace.signals.iter().any(|s| {
        matches!(
            s,
            RunnerSignal::ChildExit { .. } | RunnerSignal::ExpectedChildExit { .. }
        )
    });
    if !stopped && !child_consumed && !exit_reported {
        let grace = Duration::from_millis(POST_COMPARE_GRACE_MS)
            .min(deadline.saturating_duration_since(Instant::now()));
        if let Ok(code) = tokio::time::timeout(grace, monitor.wait()).await {
            // A CLEAN exit-0 AFTER a successful compare is a healthy shutdown, not a crash —
            // the frame was already held long enough to capture + compare, so a graceful
            // exit is not a `child_error` (impl-review #6). Only an ABNORMAL exit (a
            // non-zero code or a signal-kill = `None`) is surfaced. NB the pre-step /
            // settle exit checks still treat ANY exit as fatal (design D7: the frame must be
            // HELD until the compare) — this leniency is post-compare only.
            if code != Some(0) {
                trace.emit(RunnerSignal::ChildExit { code });
            }
        }
    }

    // 6. Cleanup: kill the scratch session on a fresh connection (the drive stream may
    //    be mid-abort after a race). Leaves no scratch behind (design D10).
    if let Ok(mut clean) = crate::client::ensure_daemon_running_at(socket_path).await {
        let _ = rpc_call(
            &mut clean,
            "session.kill",
            serde_json::json!({ "id": session_id }),
        )
        .await;
    }
    drop(monitor);

    // The run completed (or stopped on a terminal signal). Derive the scenario-level
    // disposition from the ordered signals and hand 082 the structured outcome.
    let terminal = derive_terminal(&trace.signals);
    Ok(finish(
        scenario,
        started_at_ms,
        start,
        frames,
        terminal,
        has_visual,
        golden_dir,
    ))
}

/// Drive one step. Blocking waits race the exit monitor so a crash short-circuits
/// before any visual compare (design D7).
#[allow(clippy::too_many_arguments)]
async fn drive_step(
    stream: &mut UnixStream,
    monitor: &ExitMonitor,
    idx: usize,
    step: &Step,
    pane_id: &str,
    golden_dir: &Path,
    sc_hash: &str,
    ce_hash: &str,
    rasterizer: &Rasterizer,
    trace: &mut Trace,
    child_consumed: &mut bool,
    frames: &mut Vec<FrameOutcome>,
    cli_retries: u32,
) -> StepFlow {
    match step {
        Step::WaitForText {
            text,
            regex,
            absent,
            timeout_ms,
        } => {
            let mut params = serde_json::json!({
                "pane_id": pane_id,
                "absent": absent,
                "timeout_ms": timeout_ms,
            });
            if let Some(t) = text {
                params["text"] = serde_json::Value::String(t.clone());
            }
            if let Some(re) = regex {
                params["regex"] = serde_json::Value::String(re.clone());
            }
            let gone = *child_consumed;
            tokio::select! {
                biased;
                code = maybe_wait_exit(monitor, gone) => {
                    trace.emit(RunnerSignal::ChildExit { code });
                    StepFlow::Stop
                }
                r = rpc_call(stream, "pane.wait_for", params) => match r {
                    Ok(_) => StepFlow::Continue,
                    // pane.wait_for times out with a NotFound RPC error.
                    Err(_) => {
                        trace.emit(RunnerSignal::Timeout {
                            class: TimeoutClass::Step,
                            step_index: Some(idx),
                            action: Some("wait_for_text".into()),
                            name: None,
                            elapsed_ms: None,
                            budget_ms: Some(*timeout_ms),
                        });
                        StepFlow::Stop
                    }
                }
            }
        }

        Step::Wait { ms } => {
            let gone = *child_consumed;
            tokio::select! {
                biased;
                code = maybe_wait_exit(monitor, gone) => {
                    trace.emit(RunnerSignal::ChildExit { code });
                    StepFlow::Stop
                }
                _ = tokio::time::sleep(Duration::from_millis(*ms)) => StepFlow::Continue,
            }
        }

        Step::Settle {
            quiet_ms,
            timeout_ms,
        }
        | Step::HoldSettle {
            quiet_ms,
            timeout_ms,
            ..
        }
        | Step::StableFrames {
            quiet_ms,
            timeout_ms,
            ..
        } => {
            // Task 083: per-variant settle criteria. `settle` = quiet; `hold_settle` = frame held
            // for `hold_ms`; `stable_frames` = `n` contiguous identical frames. A settle that
            // never reaches its criterion within budget is `never_stabilized` → `settle_never_
            // stable` (a FAILURE, never infra). Standalone steps hash the FULL frame (no masks).
            let spec = SettleSpec {
                quiet_ms: *quiet_ms,
                timeout_ms: *timeout_ms,
                hold_ms: match step {
                    Step::HoldSettle { hold_ms, .. } => *hold_ms,
                    _ => 0,
                },
                stable_frames: match step {
                    Step::StableFrames { n, .. } => *n,
                    _ => 1,
                },
            };
            match settle(stream, monitor, pane_id, spec, &[], *child_consumed).await {
                SettleOutcome::Exited(code) => {
                    trace.emit(RunnerSignal::ChildExit { code });
                    StepFlow::Stop
                }
                SettleOutcome::Settled => StepFlow::Continue,
                SettleOutcome::Timeout => {
                    trace.emit(RunnerSignal::Timeout {
                        class: TimeoutClass::NeverStabilized,
                        step_index: Some(idx),
                        action: Some(step.action().into()),
                        name: None,
                        elapsed_ms: None,
                        budget_ms: Some(*timeout_ms),
                    });
                    StepFlow::Stop
                }
            }
        }

        Step::TypeText { text } => send_keys(stream, pane_id, text.as_bytes()).await,

        Step::Keys { keys } => match keys::encode_all(keys) {
            Ok(bytes) => send_keys(stream, pane_id, &bytes).await,
            Err(e) => {
                trace.emit(RunnerSignal::ParseError {
                    message: format!("step {idx}: {e}"),
                });
                StepFlow::Stop
            }
        },

        Step::Paste { text } => send_keys(stream, pane_id, text.as_bytes()).await,

        Step::Resize { rows, cols } => {
            let params = serde_json::json!({ "pane_id": pane_id, "cols": cols, "rows": rows });
            match rpc_call(stream, "pane.set_size", params).await {
                Ok(_) => StepFlow::Continue,
                Err(e) => {
                    trace.emit(RunnerSignal::ParseError {
                        message: format!("step {idx}: resize failed: {e}"),
                    });
                    StepFlow::Stop
                }
            }
        }

        Step::ExpectGolden {
            name,
            tier,
            retries,
            hold_ms,
            stable_frames,
            quiet_ms,
            timeout_ms,
            masks,
            xfail,
        } => {
            // Task 083 retry budget: CLI `--retries` raises the floor for every frame; a per-step
            // `retries` can raise it further for one flaky frame (monotonic — never lowers it).
            let eff_retries = (*retries).max(cli_retries);
            // The pre-capture settle honours the golden's masks (council #4) and its optional
            // frame-stability criteria (an animated-but-masked region does not block the capture).
            let spec = SettleSpec {
                quiet_ms: *quiet_ms,
                timeout_ms: *timeout_ms,
                hold_ms: *hold_ms,
                stable_frames: *stable_frames,
            };

            // Anti-masking (council #5): a retry redeems FAIL→PASS ONLY by matching the golden,
            // never by consensus among failing captures. Every failing attempt's fingerprint is
            // recorded; ≥2 DISTINCT failing fingerprints is a non-deterministic frame that FAILS
            // even if a later attempt matched.
            let mut fail_fps: Vec<String> = Vec::new();
            let mut last_fail: Option<(FrameOutcome, RunnerSignal)> = None;

            for attempt in 0..=eff_retries {
                // Each attempt RE-SETTLES + RE-CAPTURES. A settle that never reaches its criterion
                // is `settle_never_stable` (a settle failure, NOT a compare mismatch → not retried).
                match settle(stream, monitor, pane_id, spec, masks, *child_consumed).await {
                    SettleOutcome::Exited(code) => {
                        trace.emit(RunnerSignal::ChildExit { code });
                        return StepFlow::Stop;
                    }
                    SettleOutcome::Timeout => {
                        trace.emit(RunnerSignal::Timeout {
                            class: TimeoutClass::FrameSettle,
                            step_index: Some(idx),
                            action: Some("expect_golden".into()),
                            name: Some(name.clone()),
                            elapsed_ms: None,
                            budget_ms: Some(*timeout_ms),
                        });
                        return StepFlow::Stop;
                    }
                    SettleOutcome::Settled => {}
                }
                // Re-check for an unexpected exit AFTER settle and BEFORE the compare (adv MAJOR 2):
                // a child that paints, goes quiet, then exits must short-circuit the visual compare
                // (design D7), never false-pass it.
                if !*child_consumed {
                    if let Some(code) = monitor.peek().await {
                        trace.emit(RunnerSignal::ChildExit { code });
                        return StepFlow::Stop;
                    }
                }

                let (mut outcome, signal) = match capture_and_compare(
                    stream,
                    pane_id,
                    idx,
                    name,
                    *tier,
                    masks,
                    sc_hash,
                    ce_hash,
                    rasterizer,
                    golden_dir,
                    xfail.clone(),
                )
                .await
                {
                    CaptureResult::Ok(o, s) => (o, s),
                    CaptureResult::Stop(s) => {
                        trace.emit(s);
                        return StepFlow::Stop;
                    }
                };

                match outcome.kind {
                    FrameKind::Match => match retry_verdict(&fail_fps, true) {
                        RetryVerdict::Divergent => {
                            // A later attempt matched, but the failing attempts diverged — the
                            // frame is non-deterministic. Report the LAST real mismatch (concrete
                            // diff for the reviewer) as a FAIL; never silently pass (council #5).
                            let (mut fo, sig) = last_fail.take().expect("divergent implies fails");
                            fo.retry_note = Some(format!(
                                "expect_golden '{name}': FAIL — {attempt} retries diverged {:?}; a \
                                 later attempt matched but the frame is non-deterministic (fix the \
                                 settle, e.g. hold_ms/stable_frames)",
                                short_fps(&fail_fps)
                            ));
                            trace.emit(RunnerSignal::RetryOutcome {
                                name: name.clone(),
                                attempts_used: attempt,
                                outcome: "divergent".into(),
                                fingerprints: short_fps(&fail_fps),
                            });
                            frames.push(fo);
                            trace.emit(sig);
                            return StepFlow::Continue;
                        }
                        // Clean pass, or a single-fingerprint flake absorbed by a golden match.
                        RetryVerdict::CleanPass
                        | RetryVerdict::Absorbed
                        | RetryVerdict::Exhausted => {
                            if attempt > 0 {
                                let fp = fail_fps.first().map(|f| short_fp(f)).unwrap_or_default();
                                outcome.retry_note = Some(format!(
                                    "expect_golden '{name}': passed after {attempt} retr{} \
                                     (absorbed fp {fp})",
                                    if attempt == 1 { "y" } else { "ies" }
                                ));
                                trace.emit(RunnerSignal::RetryOutcome {
                                    name: name.clone(),
                                    attempts_used: attempt,
                                    outcome: "absorbed".into(),
                                    fingerprints: short_fps(&fail_fps),
                                });
                            }
                            frames.push(outcome);
                            trace.emit(signal);
                            return StepFlow::Continue;
                        }
                    },
                    FrameKind::Mismatch => {
                        fail_fps.push(outcome.live_capture_sha256.clone());
                        last_fail = Some((outcome, signal));
                        if attempt < eff_retries {
                            continue; // budget remains — re-settle + re-capture
                        }
                        // Budget exhausted — report the last mismatch as a FAIL.
                        let (mut fo, sig) = last_fail.take().expect("just set");
                        if eff_retries > 0 {
                            let kind = match retry_verdict(&fail_fps, false) {
                                RetryVerdict::Divergent => "divergent",
                                _ => "exhausted",
                            };
                            // 085 F7: say WHICH of the two situations this is. Retries exist
                            // to absorb a flake, so "FAIL after N retries" read as "flaky" —
                            // exactly backwards for the common case, where every attempt
                            // produced the SAME frame and the run is a stable, reproduced
                            // regression. The distinct fingerprint count already tells them
                            // apart; use it instead of making the reader guess.
                            let mut distinct: Vec<&String> = fail_fps.iter().collect();
                            distinct.sort();
                            distinct.dedup();
                            let attempts = eff_retries + 1;
                            fo.retry_note = Some(if distinct.len() == 1 {
                                format!(
                                    "expect_golden '{name}': FAIL - the same diff on all \
                                     {attempts} attempts (a stable regression, not a flake) \
                                     (fps {:?})",
                                    short_fps(&fail_fps)
                                )
                            } else {
                                format!(
                                    "expect_golden '{name}': FAIL - {attempts} attempts produced \
                                     {} different frames (output is non-deterministic; fix the \
                                     scenario's determinism before trusting any verdict) \
                                     ({kind} fps {:?})",
                                    distinct.len(),
                                    short_fps(&fail_fps)
                                )
                            });
                            trace.emit(RunnerSignal::RetryOutcome {
                                name: name.clone(),
                                attempts_used: eff_retries,
                                outcome: kind.into(),
                                fingerprints: short_fps(&fail_fps),
                            });
                        }
                        frames.push(fo);
                        trace.emit(sig);
                        return StepFlow::Continue;
                    }
                    // Not jitter — a missing / untrusted golden is never retried (design D6).
                    FrameKind::GoldenAbsent | FrameKind::GoldenUntrusted => {
                        frames.push(outcome);
                        trace.emit(signal);
                        return StepFlow::Continue;
                    }
                }
            }
            // The for loop returns on every path of its final iteration.
            unreachable!("expect_golden retry loop always returns within 0..=eff_retries")
        }

        Step::AssertContains { text } | Step::AssertNotContains { text } => {
            let want_present = matches!(step, Step::AssertContains { .. });
            match rpc_call(
                stream,
                "pane.capture",
                serde_json::json!({ "pane_id": pane_id }),
            )
            .await
            {
                Ok(v) => {
                    let captured = v.get("text").and_then(|t| t.as_str()).unwrap_or("");
                    let present = captured.contains(text.as_str());
                    if present == want_present {
                        trace.emit(RunnerSignal::AssertPassed { step_index: idx });
                    } else {
                        // Design D3 privacy: a BOUNDED excerpt + a hash, never the full
                        // screen. NB masks are not applied to this smoke excerpt (they
                        // cover visual goldens) — see `RunnerSignal::AssertFailed`.
                        trace.emit(RunnerSignal::AssertFailed {
                            step_index: idx,
                            needle: text.clone(),
                            excerpt: bounded_excerpt(captured, 120),
                            text_sha256: shux_vt::sha256_hex(captured.as_bytes()),
                        });
                    }
                    StepFlow::Continue
                }
                Err(e) => {
                    trace.emit(RunnerSignal::ParseError {
                        message: format!("step {idx}: capture failed: {e}"),
                    });
                    StepFlow::Stop
                }
            }
        }

        Step::ExpectExit { code, timeout_ms } => {
            tokio::select! {
                biased;
                got = monitor.wait() => {
                    *child_consumed = true;
                    match code {
                        Some(want) if Some(*want) != got => {
                            // The child DID exit (nothing timed out) but with the WRONG
                            // code — a code-bearing failure, not a timeout (adv MAJOR 4:
                            // the observed code must not be dropped). 082 maps child_exit
                            // → child_error.
                            trace.emit(RunnerSignal::ChildExit { code: got });
                            StepFlow::Stop
                        }
                        _ => {
                            trace.emit(RunnerSignal::ExpectedChildExit { code: got });
                            StepFlow::Continue
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(*timeout_ms)) => {
                    // The child did NOT exit within the step deadline.
                    trace.emit(RunnerSignal::Timeout {
                        class: TimeoutClass::Step,
                        step_index: Some(idx),
                        action: Some("expect_exit".into()),
                        name: None,
                        elapsed_ms: None,
                        budget_ms: Some(*timeout_ms),
                    });
                    StepFlow::Stop
                }
            }
        }
    }
}

enum SettleOutcome {
    Settled,
    Timeout,
    Exited(Option<i32>),
}

/// The settle criteria for one `pane.wait_settled` call (task 083). `hold_ms == 0` +
/// `stable_frames == 1` is the default QUIET mode (unchanged); a non-default value opts into a
/// frame-content stability criterion (§2 of `.local/083-design.md`).
#[derive(Clone, Copy)]
struct SettleSpec {
    quiet_ms: u64,
    timeout_ms: u64,
    hold_ms: u64,
    stable_frames: u32,
}

/// The masks for `expect_golden` / a stability settle, as the `pane.wait_settled`/`pane.glance`
/// `masks` array shape.
fn mask_params(masks: &[MaskSpec]) -> Vec<serde_json::Value> {
    masks
        .iter()
        .map(|m| serde_json::json!({ "row": m.row, "col": m.col, "width": m.width }))
        .collect()
}

/// A future that resolves on child exit, or never (when the exit was already consumed
/// by a prior `expect_exit`), so `select!` falls through to the driven operation.
async fn maybe_wait_exit(monitor: &ExitMonitor, gone: bool) -> Option<i32> {
    if gone {
        std::future::pending::<Option<i32>>().await
    } else {
        monitor.wait().await
    }
}

/// Settle via `pane.wait_settled`, racing the exit monitor. When the child was already
/// consumed by an `expect_exit`, the pane is final → settled immediately (no re-race). `masks`
/// scope the frame-content stability hash to the same masked domain the golden compare uses
/// (task 083, council #4); an empty slice hashes the full frame.
async fn settle(
    stream: &mut UnixStream,
    monitor: &ExitMonitor,
    pane_id: &str,
    spec: SettleSpec,
    masks: &[MaskSpec],
    child_gone: bool,
) -> SettleOutcome {
    if child_gone {
        return SettleOutcome::Settled;
    }
    let mut params = serde_json::json!({
        "pane_id": pane_id,
        "quiet_ms": spec.quiet_ms,
        "timeout_ms": spec.timeout_ms,
        "hold_ms": spec.hold_ms,
        "stable_frames": spec.stable_frames,
    });
    if !masks.is_empty() {
        params["masks"] = serde_json::Value::Array(mask_params(masks));
    }
    tokio::select! {
        biased;
        code = monitor.wait() => SettleOutcome::Exited(code),
        r = rpc_call(stream, "pane.wait_settled", params) => match r {
            Ok(v) if v.get("settled").and_then(|s| s.as_bool()).unwrap_or(false) => SettleOutcome::Settled,
            _ => SettleOutcome::Timeout,
        }
    }
}

/// Send bytes to the pane. An action that TRIGGERS an exit (typing a quit key) must NOT
/// report `child_exit` here — the exit is buffered by the monitor and classified by the
/// NEXT step: an `expect_exit` consumes it (`expected_child_exit`), anything else reports
/// it via the pre-step check (`child_exit`). Only `expect_exit` may bless an exit
/// (council resolution — "next step is expect_exit" never blesses an exit mid-action).
async fn send_keys(stream: &mut UnixStream, pane_id: &str, bytes: &[u8]) -> StepFlow {
    let text = String::from_utf8_lossy(bytes).to_string();
    let params = serde_json::json!({ "pane_id": pane_id, "text": text });
    let _ = rpc_call(stream, "pane.send_keys", params).await;
    StepFlow::Continue
}

/// The glance `cells` value → a validated `FrameEnvelope`.
fn envelope_from_glance(cells: &serde_json::Value) -> Result<FrameEnvelope, String> {
    let text = serde_json::to_string(cells).map_err(|e| e.to_string())?;
    FrameEnvelope::from_canonical_json(&text).map_err(|e| format!("{e:?}"))
}

/// The result of one `expect_golden` capture+compare (task 083): either a completed compare (the
/// `FrameOutcome.kind` says Match/Mismatch/GoldenAbsent/GoldenUntrusted) or a terminal drive
/// error whose signal the caller emits before stopping the run. `Ok` is the common path and is
/// destructured immediately, so boxing it to equalize variant size would only add a per-capture
/// allocation for no real benefit (same rationale as the `LensCommand` enum).
#[allow(clippy::large_enum_variant)]
enum CaptureResult {
    Ok(FrameOutcome, RunnerSignal),
    Stop(RunnerSignal),
}

/// One masked glance + golden compare for `expect_golden` (task 083 — the retry loop calls this
/// once per attempt; the caller settles + checks child-exit first). Never retries anything itself.
#[allow(clippy::too_many_arguments)]
async fn capture_and_compare(
    stream: &mut UnixStream,
    pane_id: &str,
    idx: usize,
    name: &str,
    tier: Tier,
    masks: &[MaskSpec],
    sc_hash: &str,
    ce_hash: &str,
    rasterizer: &Rasterizer,
    golden_dir: &Path,
    xfail: Option<shux_vt::XfailMeta>,
) -> CaptureResult {
    let params = serde_json::json!({
        "pane_id": pane_id,
        "include_cells": true,
        "include_png": false,
        "masks": mask_params(masks),
    });
    match rpc_call(stream, "pane.glance", params).await {
        Ok(v) => match v.get("cells") {
            Some(cells) => match envelope_from_glance(cells) {
                Ok(live) => {
                    let mset = build_mask_set(masks);
                    let fp = current_fp(tier, &mset, sc_hash, ce_hash);
                    let fc = compare_frame(golden_dir, name, tier, &live, &fp, rasterizer);
                    let outcome = FrameOutcome {
                        name: name.to_string(),
                        tier,
                        kind: frame_kind(&fc.signal),
                        reason: frame_reason(&fc.signal),
                        verdict: fc.verdict,
                        style_deltas: fc.style_deltas,
                        style_deltas_total: fc.style_deltas_total,
                        golden_json: format!("{name}.capture.json"),
                        live_capture_json: live.to_canonical_json(),
                        live_capture_sha256: capture_sha256(&live),
                        live_fingerprint: fp,
                        xfail,
                        retry_note: None,
                    };
                    CaptureResult::Ok(outcome, fc.signal)
                }
                Err(e) => CaptureResult::Stop(RunnerSignal::ParseError {
                    message: format!("step {idx}: glance cells: {e}"),
                }),
            },
            None => CaptureResult::Stop(RunnerSignal::ParseError {
                message: format!("step {idx}: glance returned no cells"),
            }),
        },
        Err(e) => CaptureResult::Stop(RunnerSignal::ParseError {
            message: format!("step {idx}: glance failed: {e}"),
        }),
    }
}

/// The anti-masking retry verdict (task 083, council #5), decided PURELY from the failing
/// fingerprints seen and whether a later attempt matched the golden. The load-bearing rule: a
/// match redeems FAIL→PASS ONLY when the failing attempts agreed on ONE fingerprint (a single
/// consistent flake); ≥2 DISTINCT failing fingerprints is a non-deterministic frame that FAILS
/// even if a later attempt matched (never silently pass a moving-target regression).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetryVerdict {
    /// Matched with no prior failure — a clean pass (no retry consumed).
    CleanPass,
    /// Matched after one consistent flake — pass, but noisily audited.
    Absorbed,
    /// A non-deterministic frame (≥2 distinct failing fingerprints) — FAIL regardless of a match.
    Divergent,
    /// Never matched within the budget with a single consistent fingerprint — a persistent
    /// regression, FAIL.
    Exhausted,
}

fn retry_verdict(fail_fps: &[String], matched: bool) -> RetryVerdict {
    let distinct = fail_fps
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len();
    match (matched, fail_fps.is_empty(), distinct >= 2) {
        (true, true, _) => RetryVerdict::CleanPass,
        (_, _, true) => RetryVerdict::Divergent,
        (true, false, false) => RetryVerdict::Absorbed,
        (false, _, false) => RetryVerdict::Exhausted,
    }
}

/// A short (12-hex) `capture_sha256` prefix — enough to identify a failing frame in a retry audit
/// without dumping the full pin (design D3 keeps trace payloads bounded).
fn short_fp(sha: &str) -> String {
    sha.chars().take(12).collect()
}

/// The DISTINCT short-fingerprints of the failing attempts, first-seen order, for the retry audit
/// note / trace signal — deduped so `retries=50` against one persistent regression records ONE
/// fingerprint, not fifty copies (adv-083 Agent C nitpick). The count-based divergence decision
/// still uses the full list via [`retry_verdict`]; this is display-only.
fn short_fps(fps: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    fps.iter()
        .filter(|f| seen.insert((*f).clone()))
        .map(|f| short_fp(f))
        .collect()
}

/// Map a compare signal to the structured frame disposition (design D1). `compare_frame`
/// only ever yields the four compare signals; any other is refused conservatively.
fn frame_kind(signal: &RunnerSignal) -> FrameKind {
    match signal {
        RunnerSignal::FrameMatch { .. } => FrameKind::Match,
        RunnerSignal::FrameMismatch { .. } => FrameKind::Mismatch,
        RunnerSignal::GoldenAbsent { .. } => FrameKind::GoldenAbsent,
        _ => FrameKind::GoldenUntrusted,
    }
}

/// The diagnostic reason a mismatch carries (never a status).
fn frame_reason(signal: &RunnerSignal) -> Option<String> {
    match signal {
        RunnerSignal::FrameMismatch { reason, .. } => reason.clone(),
        _ => None,
    }
}

/// A bounded, single-line excerpt for a failed assert (design D3 privacy).
fn bounded_excerpt(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c == '\n' { ' ' } else { c })
        .collect();
    flat.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::super::signal::RunnerSignal;
    use super::*;
    use crate::gate::scenario;

    fn fp(s: &str) -> String {
        s.to_string()
    }

    #[test]
    fn retry_verdict_anti_masking_rule() {
        // A clean pass (matched, no prior fail).
        assert_eq!(retry_verdict(&[], true), RetryVerdict::CleanPass);
        // Matched after ONE consistent flake → absorbed (pass, audited).
        assert_eq!(
            retry_verdict(&[fp("aa"), fp("aa")], true),
            RetryVerdict::Absorbed
        );
        // Matched but the failing attempts DIVERGED → FAIL (council #5: never silently pass).
        assert_eq!(
            retry_verdict(&[fp("aa"), fp("bb")], true),
            RetryVerdict::Divergent
        );
        // Never matched, one consistent fingerprint → a persistent regression.
        assert_eq!(
            retry_verdict(&[fp("aa"), fp("aa"), fp("aa")], false),
            RetryVerdict::Exhausted
        );
        // Never matched, divergent fingerprints → non-deterministic (still fails).
        assert_eq!(
            retry_verdict(&[fp("aa"), fp("bb")], false),
            RetryVerdict::Divergent
        );
    }

    #[test]
    fn retry_verdict_all_fail_paths_are_failures() {
        // Both non-match verdicts map to a failing outcome (Divergent | Exhausted), never a pass.
        for v in [
            retry_verdict(&[fp("x")], false),
            retry_verdict(&[fp("x"), fp("y")], false),
        ] {
            assert!(matches!(
                v,
                RetryVerdict::Exhausted | RetryVerdict::Divergent
            ));
        }
    }

    #[test]
    fn derive_terminal_maps_signals() {
        // 082's driver relies on this in-memory classification (not trace re-parsing).
        assert!(matches!(
            derive_terminal(&[RunnerSignal::ChildExit { code: Some(7) }]),
            Some(TerminalOutcome::ChildExit { code: Some(7) })
        ));
        assert!(matches!(
            derive_terminal(&[RunnerSignal::Timeout {
                class: TimeoutClass::FrameSettle,
                step_index: Some(1),
                action: Some("expect_golden".into()),
                name: None,
                elapsed_ms: None,
                budget_ms: None,
            }]),
            Some(TerminalOutcome::SettleNeverStable { .. })
        ));
        assert!(matches!(
            derive_terminal(&[RunnerSignal::Timeout {
                class: TimeoutClass::Scenario,
                step_index: Some(2),
                action: None,
                name: None,
                elapsed_ms: None,
                budget_ms: None,
            }]),
            Some(TerminalOutcome::ScenarioDeadline { step_index: 2 })
        ));
        assert!(derive_terminal(&[RunnerSignal::NoVisualCheck]).is_none());
    }

    #[test]
    fn bounded_excerpt_flattens_and_caps() {
        let e = bounded_excerpt("line1\nline2\nline3", 8);
        assert!(!e.contains('\n'));
        assert_eq!(e.chars().count(), 8);
    }

    #[test]
    fn default_golden_dir_layout() {
        let s = scenario::parse("name=\"demo\"\ncommand=[\"true\"]\n").unwrap();
        let d = default_golden_dir(Path::new("/x/scenarios/demo.toml"), &s);
        assert_eq!(d, Path::new("/x/scenarios/goldens/demo"));
    }

    // ── 084 F1: an ENOENT spawn must name the program, the PATH, and the remedies ──

    fn plan_with_path(path: &str) -> EnvPlan {
        let mut env = std::collections::BTreeMap::new();
        env.insert("PATH".to_string(), path.to_string());
        EnvPlan {
            env,
            env_clear: true,
        }
    }

    #[test]
    fn enoent_spawn_names_the_program_the_path_and_the_remedies() {
        let argv = vec!["uv".to_string(), "run".to_string()];
        let plan = plan_with_path("/usr/local/bin:/usr/bin:/bin");
        let msg = program_not_found_message(
            "lens.run failed: failed to spawn child process: No such file or directory (os error 2)",
            &argv,
            &plan,
        )
        .expect("ENOENT on a bare program name must produce the actionable message");

        assert!(msg.contains("'uv'"), "does not name the program: {msg}");
        assert!(
            msg.contains("/usr/local/bin:/usr/bin:/bin"),
            "does not name the PATH: {msg}"
        );
        assert!(
            msg.contains("[env] PATH"),
            "does not offer the PATH remedy: {msg}"
        );
        assert!(
            msg.contains("allow"),
            "does not offer the allow remedy: {msg}"
        );
        assert!(
            msg.is_ascii(),
            "note is sanitized to ASCII at the boundary: {msg}"
        );
        // Must survive `sanitize_note`'s 240-char cap intact.
        assert!(
            msg.chars().count() <= 240,
            "message is truncated: {} chars",
            msg.chars().count()
        );
    }

    #[test]
    fn an_absolute_command_path_and_other_errors_get_no_path_hint() {
        let plan = plan_with_path("/usr/bin");
        // An explicit path failed on its own merits; the search path is not the story.
        assert!(
            program_not_found_message(
                "No such file or directory",
                &["/opt/homebrew/bin/uv".to_string()],
                &plan
            )
            .is_none()
        );
        // A different failure must not be relabelled as a PATH problem.
        assert!(
            program_not_found_message("permission denied", &["uv".to_string()], &plan).is_none()
        );
    }

    // ── impl council: `cwd` containment must survive a SYMLINK, not just `..` ──

    #[test]
    fn a_symlinked_cwd_cannot_escape_the_scenario_directory() {
        let root = tempfile::tempdir().expect("tempdir");
        let scn = root.path().join("scn");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&scn).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        // Parse-time validation only rejects `..` SYNTACTICALLY; a symlink is a legal
        // relative name that still points anywhere on the filesystem.
        std::os::unix::fs::symlink(&outside, scn.join("sneaky")).unwrap();

        let err = resolve_contained_cwd(&scn, "sneaky")
            .expect_err("a symlink out of the scenario tree must be refused");
        assert!(err.contains("escapes the scenario directory"), "{err}");
    }

    #[test]
    fn a_real_subdirectory_cwd_resolves() {
        let root = tempfile::tempdir().expect("tempdir");
        let scn = root.path().join("scn");
        std::fs::create_dir_all(scn.join("sub")).unwrap();

        let got = resolve_contained_cwd(&scn, "sub").expect("a contained subdir is fine");
        assert!(got.ends_with("sub"), "{got:?}");
        assert_eq!(
            resolve_contained_cwd(&scn, ".").unwrap(),
            scn.canonicalize().unwrap()
        );
    }

    #[test]
    fn a_missing_cwd_is_reported_as_such() {
        let root = tempfile::tempdir().expect("tempdir");
        let err = resolve_contained_cwd(root.path(), "nope")
            .expect_err("a missing directory must be refused");
        assert!(err.contains("does not exist"), "{err}");
    }

    /// A SYMLINKED scenario file must anchor to the REAL file's directory. Anchoring to the
    /// link's directory silently mints a second, divergent golden tree beside the symlink
    /// while the real one goes untouched, and resolves `cwd` somewhere unintended
    /// (adversarial review).
    #[test]
    fn a_symlinked_scenario_file_anchors_to_the_real_directory() {
        let root = tempfile::tempdir().expect("tempdir");
        let real_dir = root.path().join("fixtures");
        let link_dir = root.path().join("ci");
        std::fs::create_dir_all(&real_dir).unwrap();
        std::fs::create_dir_all(&link_dir).unwrap();
        let real = real_dir.join("scenario.toml");
        std::fs::write(&real, "name=\"x\"\ncommand=[\"true\"]\n").unwrap();
        let link = link_dir.join("board.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        assert_eq!(
            scenario_dir_of(&link),
            real_dir.canonicalize().unwrap(),
            "a symlinked scenario anchored to the link's directory"
        );

        // And the golden dir follows it, so no duplicate tree can appear beside the link.
        let scn = scenario::parse("name=\"board\"\ncommand=[\"true\"]\n").unwrap();
        assert_eq!(
            default_golden_dir(&link, &scn),
            real_dir
                .canonicalize()
                .unwrap()
                .join("goldens")
                .join("board")
        );
    }

    /// `gate init` scaffolds a scenario that does not exist yet — anchoring must still work.
    #[test]
    fn a_nonexistent_scenario_path_falls_back_to_its_lexical_parent() {
        assert_eq!(
            scenario_dir_of(Path::new("/no/such/dir/scenario.toml")),
            Path::new("/no/such/dir")
        );
        assert_eq!(scenario_dir_of(Path::new("brand-new.toml")), Path::new("."));
    }

    /// `Path::parent()` on a bare filename returns `Some("")`, not `None` — the trap that
    /// broke `shux lens gate scenario.toml` while `./scenario.toml` worked (shux-tui-qa).
    #[test]
    fn a_bare_scenario_filename_resolves_to_the_current_directory() {
        assert_eq!(scenario_dir_of(Path::new("scenario.toml")), Path::new("."));
        assert_eq!(
            scenario_dir_of(Path::new("./scenario.toml")),
            Path::new(".")
        );
        assert_eq!(
            scenario_dir_of(Path::new("sub/scenario.toml")),
            Path::new("sub")
        );
        assert_eq!(
            scenario_dir_of(Path::new("/abs/dir/scenario.toml")),
            Path::new("/abs/dir")
        );
        // And the derived dir must be usable — an empty path is not.
        assert!(
            scenario_dir_of(Path::new("scenario.toml"))
                .canonicalize()
                .is_ok(),
            "the derived scenario dir must canonicalize; an empty path is ENOENT"
        );
    }
}
