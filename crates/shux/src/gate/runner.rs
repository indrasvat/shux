//! The daemon-backed scenario drive loop (task 081). Everything race-, timeout-, and
//! child-exit-sensitive lives here; the pure decisions are in the sibling modules.
//!
//! Ownership (design D1/D2): this emits RAW SIGNALS to the `--trace` channel (design
//! D3) and returns a provisional exit — task 082 installs the frozen `report.json` +
//! exit map. Nothing here prints the frozen stdout summary.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use shux_raster::Rasterizer;
use shux_vt::{
    FINGERPRINT_SCHEMA, Fingerprint, FrameEnvelope, MaskSet, RENDERER_FORMAT_VERSION,
    SCHEMA_VERSION, Tier, TolParams, mask_hash, unicode_width_version,
};
use tokio::net::UnixStream;
use tokio::sync::{Mutex, Notify};

use super::compare::compare_frame;
use super::env_plan::{SandboxDirs, build_env_plan, cmd_env_hash, scenario_hash};
use super::keys;
use super::scenario::{self, MaskSpec, Scenario, Step};
use super::signal::{RunnerSignal, TimeoutClass};
use crate::cli::{RpcClientError, rpc_call};

const FONT_SIZE: f32 = 16.0;
/// The scratch quota (`lens_scratch::SCRATCH_QUOTA`) surfaced as a raw signal.
const SCRATCH_QUOTA: usize = 16;
const RESOURCE_EXHAUSTED: i64 = -32012;

/// Where the NDJSON trace goes (design D3). `None` = no trace emitted.
pub enum TraceTarget {
    Stdout,
    Path(PathBuf),
}

/// Options resolved from the CLI verb.
pub struct GateOptions {
    pub scenario_path: PathBuf,
    /// `-- <argv>` override of the scenario `command`.
    pub argv_override: Option<Vec<String>>,
    /// Golden directory; defaults to `<scenario-dir>/goldens/<scenario-name>/`.
    pub golden_dir: Option<PathBuf>,
    pub trace: Option<TraceTarget>,
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

/// Provisional exit classes (design D3 — 082 installs the frozen map). Not the frozen
/// `GateStatus::exit_code`; just enough that 081 never greens a real failure in CI.
fn provisional_exit(signals: &[RunnerSignal]) -> i32 {
    let mut had_failure = false;
    let mut infra = false;
    for s in signals {
        match s {
            RunnerSignal::ParseError { .. } => return 2,
            RunnerSignal::QuotaExceeded { .. } => infra = true,
            RunnerSignal::FrameMismatch { .. }
            | RunnerSignal::GoldenAbsent { .. }
            | RunnerSignal::GoldenUntrusted { .. }
            | RunnerSignal::ChildExit { .. }
            | RunnerSignal::Timeout { .. }
            | RunnerSignal::AssertFailed { .. }
            | RunnerSignal::NoVisualCheck => had_failure = true,
            _ => {}
        }
    }
    if infra {
        3
    } else if had_failure {
        1
    } else {
        0
    }
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

fn default_golden_dir(scenario_path: &Path, scenario: &Scenario) -> PathBuf {
    scenario_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
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

/// The CLI entry (`shux lens gate`). Parses, drives, traces raw signals, returns a
/// provisional exit code.
pub async fn handle_lens_gate(socket_path: &Path, opts: GateOptions) -> anyhow::Result<i32> {
    // 1. Parse (a malformed scenario is a raw parse_error → provisional exit 2).
    let scenario = match scenario::load(&opts.scenario_path) {
        Ok(s) => s,
        Err(e) => {
            let mut trace = Trace::open(opts.trace)?;
            trace.emit(RunnerSignal::ParseError {
                message: e.to_string(),
            });
            eprintln!("lens gate: {e}");
            return Ok(2);
        }
    };

    let argv = opts
        .argv_override
        .clone()
        .unwrap_or_else(|| scenario.command.clone());
    let golden_dir = opts
        .golden_dir
        .clone()
        .unwrap_or_else(|| default_golden_dir(&opts.scenario_path, &scenario));

    // 2. Sandbox + deterministic env plan + provenance hashes.
    let sandbox_root = tempfile::tempdir()?;
    let sandbox = make_sandbox(sandbox_root.path())?;
    let plan = build_env_plan(&scenario, &sandbox, &|k| std::env::var(k).ok());
    let sc_hash = scenario_hash(&scenario);
    let ce_hash = cmd_env_hash(&plan, &sandbox, &argv, &scenario.terminal);
    let rasterizer = Rasterizer::new(FONT_SIZE)?;

    let mut trace = Trace::open(opts.trace)?;
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

    // 3. Pre-spawn cursor (design D7): head seq BEFORE lens.run.
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

    // 4. Spawn the child (deny-by-default env; async — the runner monitors exit).
    let mut stream = crate::client::ensure_daemon_running_at(socket_path).await?;
    let env_obj: serde_json::Map<String, serde_json::Value> = plan
        .env
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    let run_params = serde_json::json!({
        "argv": argv,
        "cols": scenario.terminal.cols,
        "rows": scenario.terminal.rows,
        "env": serde_json::Value::Object(env_obj),
        "env_clear": plan.env_clear,
        "cwd": sandbox.home.display().to_string(),
        "wait": false,
    });
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
            return Ok(provisional_exit(&trace.signals));
        }
        Err(e) => {
            trace.emit(RunnerSignal::ParseError {
                message: format!("lens.run failed: {e}"),
            });
            return Ok(3);
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
                &golden_dir,
                &sc_hash,
                &ce_hash,
                &rasterizer,
                &mut trace,
                &mut child_consumed,
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
    // `expect_golden` whose child paints its golden, settles, then crashes shortly after
    // the compare) must still surface — otherwise a crashing TUI whose last frame matches
    // its golden false-passes (adv MAJOR 2). Only when the run COMPLETED normally (not
    // stopped by a terminal signal) and nothing consumed/reported an exit: give a bounded
    // grace, capped by the remaining deadline, for a pending exit. A child still alive at
    // the end (an interactive TUI blocked on input) simply times out the grace → no
    // signal, correct.
    let exit_reported = trace.signals.iter().any(|s| {
        matches!(
            s,
            RunnerSignal::ChildExit { .. } | RunnerSignal::ExpectedChildExit { .. }
        )
    });
    if !stopped && !child_consumed && !exit_reported {
        let grace =
            Duration::from_millis(500).min(deadline.saturating_duration_since(Instant::now()));
        if let Ok(code) = tokio::time::timeout(grace, monitor.wait()).await {
            trace.emit(RunnerSignal::ChildExit { code });
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

    // A terse per-kind tally to STDERR only (diagnostic). stdout stays reserved for the
    // 082 summary/report contract (design D3).
    summarize_to_stderr(&scenario.name, &trace.signals);
    Ok(provisional_exit(&trace.signals))
}

/// A one-line, per-signal-kind tally to stderr (not the 082 stdout contract).
fn summarize_to_stderr(scenario: &str, signals: &[RunnerSignal]) {
    use std::collections::BTreeMap;
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for s in signals {
        *counts.entry(s.kind()).or_default() += 1;
    }
    let tally: Vec<String> = counts.iter().map(|(k, n)| format!("{k}={n}")).collect();
    eprintln!("lens gate [{scenario}]: {}", tally.join(" "));
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
        }
        | Step::StableFrames {
            quiet_ms,
            timeout_ms,
            ..
        } => {
            // `stable_frames` is a documented placeholder wired to the `--quiet` settle
            // until 083 (design D6). A settle that never quiets is `never_stabilized`.
            match settle(
                stream,
                monitor,
                pane_id,
                *quiet_ms,
                *timeout_ms,
                *child_consumed,
            )
            .await
            {
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
            quiet_ms,
            timeout_ms,
            masks,
            ..
        } => {
            // Settle FIRST — a frame that never quiets is `frame_settle_timeout`.
            match settle(
                stream,
                monitor,
                pane_id,
                *quiet_ms,
                *timeout_ms,
                *child_consumed,
            )
            .await
            {
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
            // Re-check for an unexpected exit AFTER settle and BEFORE the compare (adv
            // MAJOR 2): a child that paints, goes quiet, then exits would otherwise be
            // compared on its final frame — an unexpected exit must short-circuit the
            // visual compare (design D7), never false-pass it.
            if !*child_consumed {
                if let Some(code) = monitor.peek().await {
                    trace.emit(RunnerSignal::ChildExit { code });
                    return StepFlow::Stop;
                }
            }
            // Capture the masked cell envelope, then compare against the golden.
            let mask_params: Vec<serde_json::Value> = masks
                .iter()
                .map(|m| serde_json::json!({ "row": m.row, "col": m.col, "width": m.width }))
                .collect();
            let params = serde_json::json!({
                "pane_id": pane_id,
                "include_cells": true,
                "include_png": false,
                "masks": mask_params,
            });
            match rpc_call(stream, "pane.glance", params).await {
                Ok(v) => match v.get("cells") {
                    Some(cells) => match envelope_from_glance(cells) {
                        Ok(live) => {
                            let mset = build_mask_set(masks);
                            let fp = current_fp(*tier, &mset, sc_hash, ce_hash);
                            let sig =
                                compare_frame(golden_dir, name, *tier, &live, &fp, rasterizer);
                            trace.emit(sig);
                            StepFlow::Continue
                        }
                        Err(e) => {
                            trace.emit(RunnerSignal::ParseError {
                                message: format!("step {idx}: glance cells: {e}"),
                            });
                            StepFlow::Stop
                        }
                    },
                    None => {
                        trace.emit(RunnerSignal::ParseError {
                            message: format!("step {idx}: glance returned no cells"),
                        });
                        StepFlow::Stop
                    }
                },
                Err(e) => {
                    trace.emit(RunnerSignal::ParseError {
                        message: format!("step {idx}: glance failed: {e}"),
                    });
                    StepFlow::Stop
                }
            }
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
/// consumed by an `expect_exit`, the pane is final → settled immediately (no re-race).
async fn settle(
    stream: &mut UnixStream,
    monitor: &ExitMonitor,
    pane_id: &str,
    quiet_ms: u64,
    timeout_ms: u64,
    child_gone: bool,
) -> SettleOutcome {
    if child_gone {
        return SettleOutcome::Settled;
    }
    let params = serde_json::json!({
        "pane_id": pane_id,
        "quiet_ms": quiet_ms,
        "timeout_ms": timeout_ms,
    });
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

    #[test]
    fn provisional_exit_maps() {
        assert_eq!(provisional_exit(&[RunnerSignal::NoVisualCheck]), 1);
        assert_eq!(
            provisional_exit(&[RunnerSignal::FrameMatch {
                name: "f".into(),
                tier: "cell".into()
            }]),
            0
        );
        assert_eq!(
            provisional_exit(&[RunnerSignal::ParseError {
                message: "x".into()
            }]),
            2
        );
        assert_eq!(
            provisional_exit(&[RunnerSignal::QuotaExceeded { limit: 16 }]),
            3
        );
        // A regression among greens still fails.
        assert_eq!(
            provisional_exit(&[
                RunnerSignal::FrameMatch {
                    name: "a".into(),
                    tier: "cell".into()
                },
                RunnerSignal::FrameMismatch {
                    name: "b".into(),
                    tier: "cell".into(),
                    reason: None,
                    changed_cells: Some(1)
                },
            ]),
            1
        );
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
}
