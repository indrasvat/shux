//! The 082 orchestrator for `shux lens gate` (the default run verb). Ties the 081 runner
//! mechanics to the frozen verdict/report/exit contract: parse → drive → roll up →
//! emit `report.json` + the ASCII summary → return the frozen exit code. `--on-missing
//! create` and `--update` route through the approval-gated [`super::bless`] writer.

use std::path::{Path, PathBuf};

use shux_vt::{GateStatus, ScenarioReport};

use super::runner::{TraceTarget, default_golden_dir};
use super::scenario;
use super::{bless, heat, runner, summary, verdict};
use crate::cli::OutputFormat;

/// Everything the run verb needs, lifted from the CLI (agent-first noun-verb; no inline
/// JSON). Constructed in `main.rs` from `LensCommand::Gate`.
pub struct GateRunOptions {
    pub scenario_path: PathBuf,
    pub golden_dir: Option<PathBuf>,
    pub report: Option<String>,
    pub on_missing: crate::cli::OnMissing,
    pub update: Option<String>,
    pub reason: Option<String>,
    pub tol: Option<shux_vt::TolParams>,
    pub out: Option<PathBuf>,
    pub retries: Option<u32>,
    pub trace: Option<String>,
    pub argv: Vec<String>,
    pub format: OutputFormat,
}

/// True when running under CI (any truthy `CI` env). A bless/create is refused here so a
/// golden can never be self-minted in CI (task §5/§7).
pub fn is_ci() -> bool {
    matches!(
        std::env::var("CI").ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

fn trace_target(spec: Option<String>) -> Option<TraceTarget> {
    spec.map(|t| {
        if t == "-" {
            TraceTarget::Stdout
        } else {
            TraceTarget::Path(PathBuf::from(t))
        }
    })
}

/// A single `scenario_error` report for a scenario that could not even be parsed (the
/// name falls back to the file stem — there is no valid scenario name yet).
fn parse_error_report(path: &Path, message: &str) -> Vec<ScenarioReport> {
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("scenario")
        .to_string();
    vec![ScenarioReport {
        scenario: name,
        status: GateStatus::ScenarioError,
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        font_chain_sha256: None,
        font_size_px: None,
        started_at_ms: None,
        duration_ms: None,
        frames: vec![],
        note: Some(verdict::sanitize_note(&format!(
            "scenario_error: {message}"
        ))),
    }]
}

/// A single `update_refused` report (exit 6) — a bless/create was refused by a guard.
fn refused_report(scenario: &str, reason: &str) -> Vec<ScenarioReport> {
    vec![ScenarioReport {
        scenario: scenario.to_string(),
        status: GateStatus::UpdateRefused,
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        font_chain_sha256: None,
        font_size_px: None,
        started_at_ms: None,
        duration_ms: None,
        frames: vec![],
        note: Some(verdict::sanitize_note(&format!("update_refused: {reason}"))),
    }]
}

/// Route `report.json` + the ASCII summary to the right streams. When the report goes to
/// stdout (`--report -` or `--format json`), stdout carries ONLY the JSON and the summary
/// moves to stderr so a `| tee report.json` stays valid.
fn emit(opts: &GateRunOptions, reports: &[ScenarioReport]) -> std::io::Result<()> {
    let stdout_is_json =
        matches!(opts.format, OutputFormat::Json) || opts.report.as_deref() == Some("-");
    let json = serde_json::to_string_pretty(reports).unwrap_or_else(|_| "[]".to_string());
    match opts.report.as_deref() {
        Some("-") => println!("{json}"),
        Some(path) => std::fs::write(path, format!("{json}\n"))?,
        None => {
            if stdout_is_json {
                println!("{json}");
            }
        }
    }
    let table = summary::render(reports);
    if stdout_is_json {
        eprint!("{table}");
    } else {
        print!("{table}");
    }
    Ok(())
}

/// Run the gate on one scenario and return the frozen exit code (task §4). This is the
/// 082 entry `main.rs` dispatches to for the default `shux lens gate <scenario>`.
pub async fn run_gate(socket_path: &Path, opts: GateRunOptions) -> anyhow::Result<i32> {
    let scenario = match scenario::load(&opts.scenario_path) {
        Ok(s) => s,
        Err(e) => {
            // Preserve the 081 trace contract: a malformed scenario still leaves a
            // greppable `parse_error` in the trace.
            runner::emit_parse_error_trace(trace_target(opts.trace.clone()), &e.to_string());
            eprintln!("{}", crate::style::error(format!("lens gate: {e}")));
            let reports = parse_error_report(&opts.scenario_path, &e.to_string());
            emit(&opts, &reports)?;
            return Ok(GateStatus::ScenarioError.exit_code() as i32);
        }
    };
    let golden_dir = opts
        .golden_dir
        .clone()
        .unwrap_or_else(|| default_golden_dir(&opts.scenario_path, &scenario));

    // A bless/create is refused in CI up front, before spawning anything (task §5/§7):
    // a golden must never be self-minted in CI.
    if is_ci() && (opts.update.is_some() || opts.on_missing == crate::cli::OnMissing::Create) {
        let reports = refused_report(
            &scenario.name,
            "CI mode: goldens are never self-minted here",
        );
        emit(&opts, &reports)?;
        return Ok(GateStatus::UpdateRefused.exit_code() as i32);
    }

    let argv = if opts.argv.is_empty() {
        scenario.command.clone()
    } else {
        opts.argv.clone()
    };

    let outcome = runner::drive_scenario(
        socket_path,
        &scenario,
        &argv,
        &golden_dir,
        trace_target(opts.trace.clone()),
    )
    .await?;

    let today = chrono::Utc::now().date_naive();
    let mut reports = verdict::build_reports(&outcome, today);
    plumb_retries(&mut reports, opts.retries);

    // `--update` and `--on-missing create` re-bless through the guarded writer, which may
    // refuse (dirty tree / secret hit) → update_refused, or rewrite the affected frames'
    // statuses after a successful bless.
    if let Some(selector) = &opts.update {
        match bless::run_update(&scenario, &outcome, &reports, &golden_dir, selector, &opts)? {
            bless::BlessOutcome::Refused(reason) => {
                let reports = refused_report(&scenario.name, &reason);
                emit(&opts, &reports)?;
                return Ok(GateStatus::UpdateRefused.exit_code() as i32);
            }
            bless::BlessOutcome::Blessed(manifest) => {
                bless::apply_blessed(&mut reports, &manifest);
            }
        }
    } else if opts.on_missing == crate::cli::OnMissing::Create {
        match bless::create_missing(&scenario, &outcome, &reports, &golden_dir, &opts)? {
            bless::BlessOutcome::Refused(reason) => {
                let reports = refused_report(&scenario.name, &reason);
                emit(&opts, &reports)?;
                return Ok(GateStatus::UpdateRefused.exit_code() as i32);
            }
            bless::BlessOutcome::Blessed(manifest) => {
                bless::apply_blessed(&mut reports, &manifest);
            }
        }
    }

    // Write headless heat-overlay evidence for any remaining fail frame (dogfood: the
    // pixel-perfect proof must be producible in CI / by an agent, not only in `gate
    // review`). Best-effort; sets `diff.heat_png` in the report.
    if reports
        .iter()
        .flat_map(|s| s.frames.iter())
        .any(|f| f.status == GateStatus::Fail)
    {
        if let Ok(rasterizer) = shux_raster::Rasterizer::new(16.0) {
            let out = heat::out_dir(opts.out.as_deref(), &scenario.name);
            heat::emit_heat_for_fails(&outcome, &mut reports, &golden_dir, &out, &rasterizer);
        }
    }

    emit(&opts, &reports)?;
    Ok(verdict::exit_code(&reports) as i32)
}

/// Carry `--retries` into the report (task L2: parsed + reported; retry BEHAVIOUR is 083).
/// The frozen schema has no retries field, so it rides in the scenario `note`.
fn plumb_retries(reports: &mut [ScenarioReport], retries: Option<u32>) {
    if let Some(n) = retries {
        for r in reports.iter_mut() {
            let tag = format!("retries={n}");
            r.note = Some(match r.note.take() {
                Some(existing) => format!("{existing}; {tag}"),
                None => tag,
            });
        }
    }
}
