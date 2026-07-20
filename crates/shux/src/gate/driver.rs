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
    /// `--cast [PATH]` (task 083): `None` = off; `Some("")` = default `<out>/<scenario>.cast`;
    /// `Some(path)` = that path. Always resolved under the gitignored out dir; never a golden.
    pub cast: Option<String>,
    pub trace: Option<String>,
    pub argv: Vec<String>,
    pub format: OutputFormat,
}

/// True when running under CI. A bless/create is refused here so a golden can never be
/// self-minted in CI (task §5/§7).
///
/// This FAILS CLOSED (085 adversarial): `CI` is treated as set unless it is empty or an
/// explicit falsey value. The previous exact-match allowlist (`1`/`true`/`TRUE`/`yes`/`YES`)
/// missed ordinary spellings — `CI=True`, `CI=Yes`, `CI=on` all read as "not CI", and
/// `--update` then blessed a real regression green, defeating the one guarantee this guard
/// exists to provide. For a guard whose job is refusing a privileged write, an unrecognised
/// value must mean "refuse", never "allow".
pub fn is_ci() -> bool {
    match std::env::var("CI") {
        Err(_) => false,
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            !matches!(v.as_str(), "" | "0" | "false" | "no" | "off")
        }
    }
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

/// A single `update_refused` report (exit 6) for a refusal that fires BEFORE the scenario
/// runs — today only the CI guard. There are no per-frame verdicts to preserve yet, so an
/// empty report is the whole truth. Once the run has happened, use [`apply_refusal`].
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

/// Fold a bless REFUSAL into the already-computed reports (085 F16).
///
/// This used to replace the reports with a synthetic empty `update_refused` one and return
/// early, which erased the run's real verdict: a genuine regression became exit 6 with
/// `frames: []` and no heat PNG, so `report.json` — the documented source of truth —
/// recorded that nothing had failed. `shux_vt`'s own
/// `worst_never_masks_a_regression_with_an_error` forbids precisely that; the refusal is an
/// operational error and ranks BELOW a regression. Roll it up through `worst()` instead, and
/// keep every frame so the evidence survives.
fn apply_refusal(reports: &mut [ScenarioReport], reason: &str) {
    for r in reports.iter_mut() {
        r.status = r.status.worst(GateStatus::UpdateRefused);
        let refusal = format!("update_refused: {reason}");
        r.note = Some(match r.note.take() {
            Some(existing) if !existing.is_empty() => {
                verdict::sanitize_note(&format!("{existing}; {refusal}"))
            }
            _ => verdict::sanitize_note(&refusal),
        });
    }
}

/// Exit code for a CLI-level I/O failure while writing the report or trace (085 F18).
///
/// This used to propagate as an `anyhow` error, which the client turned into exit **1** —
/// the FROZEN regression code — so a bad `--report` path on a perfectly green run told CI
/// "visual regression" and printed a bare errno. `4` is reserved for exactly this class
/// (`shux_vt`'s `exit_code_never_returns_four` pins that no gate VERDICT can produce it),
/// so it is unambiguous: the check itself did not fail, writing its output did.
fn report_io_failure(opts: &GateRunOptions, e: &std::io::Error) -> i32 {
    let target = match opts.report.as_deref() {
        Some("-") | None => "the report stream".to_string(),
        Some(p) => format!("the report file {p}"),
    };
    eprintln!(
        "{}",
        crate::style::error(format!(
            "lens gate: could not write {target}: {e}. The scenario itself ran; only its \
             output could not be written (exit 4, not a regression)."
        ))
    );
    4
}

#[cfg(test)]
mod ci_tests {
    /// 085 adversarial: the CI guard is what stops a golden self-minting in CI, so an
    /// unrecognised `CI` value must mean REFUSE. The old exact-match allowlist let
    /// `CI=True` / `CI=Yes` / `CI=on` through, and `--update` then blessed a real
    /// regression green — reproduced end-to-end before this fix.
    #[test]
    fn ci_detection_fails_closed_on_unfamiliar_truthy_values() {
        // Serialised via a mutex would be ideal, but these run in-process and only touch
        // this one var; `--test-threads=1` is enforced by the Makefile target.
        let truthy = [
            "1",
            "true",
            "TRUE",
            "True",
            "yes",
            "YES",
            "Yes",
            "on",
            "ON",
            "y",
            "enabled",
            "github-actions",
        ];
        for v in truthy {
            unsafe { std::env::set_var("CI", v) };
            assert!(
                super::is_ci(),
                "CI={v:?} must be treated as CI (fail closed)"
            );
        }
        for v in ["", "0", "false", "FALSE", "no", "off", "  "] {
            unsafe { std::env::set_var("CI", v) };
            assert!(!super::is_ci(), "CI={v:?} must NOT be treated as CI");
        }
        unsafe { std::env::remove_var("CI") };
        assert!(!super::is_ci(), "unset CI is not CI");
    }
}

/// Route `report.json` + the ASCII summary to the right streams. When the report goes to
/// stdout (`--report -` or `--format json`), stdout carries ONLY the JSON and the summary
/// moves to stderr so a `| tee report.json` stays valid.
fn emit(opts: &GateRunOptions, reports: &[ScenarioReport]) -> std::io::Result<()> {
    let stdout_is_json =
        matches!(opts.format, OutputFormat::Json) || opts.report.as_deref() == Some("-");
    let json = serde_json::to_string_pretty(reports).unwrap_or_else(|_| "[]".to_string());
    let write_err = match opts.report.as_deref() {
        Some("-") => {
            println!("{json}");
            None
        }
        Some(path) => std::fs::write(path, format!("{json}\n")).err(),
        None => {
            if stdout_is_json {
                println!("{json}");
            }
            None
        }
    };
    // The summary is emitted even when the report file could not be written (085 QA P2):
    // returning early left a user with an errno and NO verdict at all, on a run that had
    // actually completed and produced one. On that path it goes to stderr, since stdout
    // may still be a JSON stream a consumer is reading.
    let table = summary::render(reports);
    if stdout_is_json || write_err.is_some() {
        eprint!("{table}");
    } else {
        print!("{table}");
    }
    match write_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
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
            if let Err(e) = emit(&opts, &reports) {
                return Ok(report_io_failure(&opts, &e));
            }
            return Ok(GateStatus::ScenarioError.exit_code() as i32);
        }
    };
    let golden_dir = opts
        .golden_dir
        .clone()
        .unwrap_or_else(|| default_golden_dir(&opts.scenario_path, &scenario));

    // A bless/create is refused in CI up front, before spawning anything (task §5/§7):
    // a golden must never be self-minted in CI. This one keeps its early return: nothing has
    // run yet, so there is no verdict to preserve, and exit 6 fails the build loudly.
    if is_ci() && (opts.update.is_some() || opts.on_missing == crate::cli::OnMissing::Create) {
        let reports = refused_report(
            &scenario.name,
            "CI mode: goldens are never self-minted here",
        );
        if let Err(e) = emit(&opts, &reports) {
            return Ok(report_io_failure(&opts, &e));
        }
        return Ok(GateStatus::UpdateRefused.exit_code() as i32);
    }

    // 085: `--trace -` and a stdout JSON report both claim stdout, and the result was
    // NDJSON signals followed by the report array — unparseable, with no usage error and
    // exit 0. The report stream is promised to carry ONLY the JSON, so this is a usage
    // error, refused before anything spawns.
    let stdout_is_json =
        matches!(opts.format, OutputFormat::Json) || opts.report.as_deref() == Some("-");
    if stdout_is_json && opts.trace.as_deref() == Some("-") {
        eprintln!(
            "{}",
            crate::style::error(
                "lens gate: --trace - and the JSON report both write to stdout, which would \
                 make the report unparseable. Send the trace to a file instead (--trace \
                 run.jsonl)."
            )
        );
        let reports = parse_error_report(
            &opts.scenario_path,
            "--trace - collides with the stdout JSON report",
        );
        if let Err(e) = emit(&opts, &reports) {
            return Ok(report_io_failure(&opts, &e));
        }
        return Ok(GateStatus::ScenarioError.exit_code() as i32);
    }

    let argv = if opts.argv.is_empty() {
        scenario.command.clone()
    } else {
        opts.argv.clone()
    };

    // Resolve the ephemeral cast path (task 083). The daemon writes it, so pass an ABSOLUTE path
    // under the gitignored out dir — default `<out>/<scenario>.cast`.
    let cast_path = resolve_cast_path(&opts, &scenario.name);

    // A cast is EPHEMERAL evidence and must NEVER land in the golden tree (adv-083 Agent C MINOR):
    // it would pollute the goldens and trip the dirty-tree guard on a later `--update`. Refuse a
    // `--cast` target inside `--golden-dir` up front (exit 2, usage), before spawning anything.
    if let Some(cast) = &cast_path
        && cast_is_under(cast, &golden_dir)
    {
        eprintln!(
            "{}",
            crate::style::error(format!(
                "lens gate: --cast path {} is inside the golden dir {} — a cast is ephemeral \
                 evidence and must not pollute goldens (write it under --out instead)",
                cast.display(),
                golden_dir.display()
            ))
        );
        let reports = parse_error_report(
            &opts.scenario_path,
            "--cast path must not be inside the golden dir",
        );
        if let Err(e) = emit(&opts, &reports) {
            return Ok(report_io_failure(&opts, &e));
        }
        return Ok(GateStatus::ScenarioError.exit_code() as i32);
    }

    let outcome = runner::drive_scenario(
        socket_path,
        &scenario,
        &runner::scenario_dir_of(&opts.scenario_path),
        &argv,
        &golden_dir,
        trace_target(opts.trace.clone()),
        opts.retries.unwrap_or(0),
        cast_path.clone(),
    )
    .await?;

    // Fold the runner's per-frame retry audit notes (task 083 council #5) into the scenario note
    // BEFORE verdict rollup so a flake absorbed by a retry — or a non-deterministic frame that
    // FAILED — is never silent in `report.json`.
    let retry_notes: Vec<String> = outcome
        .frames
        .iter()
        .filter_map(|f| f.retry_note.clone())
        .collect();

    let today = chrono::Utc::now().date_naive();
    let mut reports = verdict::build_reports(&outcome, today);
    plumb_retries(&mut reports, opts.retries);
    plumb_retry_notes(&mut reports, &retry_notes);

    // `--update` and `--on-missing create` re-bless through the guarded writer, which may
    // refuse (dirty tree / secret hit) → update_refused, or rewrite the affected frames'
    // statuses after a successful bless.
    // A golden is a claim that "this is what correct looks like". It may only be minted from
    // a run that otherwise completed cleanly. 084's F4 stopped a bless from laundering the
    // scenario STATUS, but the WRITE still happened: a child that crashed after the capture
    // produced `child_error: exit 9; blessed 1 golden(s)` with the golden on disk — a
    // baseline taken from a broken run (found by the 085 implementation council, reproduced).
    let floor = verdict::scenario_floor(&outcome);
    let blessing = opts.update.is_some() || opts.on_missing == crate::cli::OnMissing::Create;
    if blessing && !floor.is_green() {
        apply_refusal(
            &mut reports,
            &format!(
                "the run did not complete cleanly ({}) - a golden must not be minted from it; \
                 fix the scenario first",
                summary::status_label(floor)
            ),
        );
    } else if let Some(selector) = &opts.update {
        match bless::run_update(&scenario, &outcome, &reports, &golden_dir, selector, &opts)? {
            bless::BlessOutcome::Refused(reason) => {
                apply_refusal(&mut reports, &reason);
            }
            bless::BlessOutcome::Blessed(manifest) => {
                bless::apply_blessed(&mut reports, &manifest, verdict::scenario_floor(&outcome));
            }
        }
    } else if opts.on_missing == crate::cli::OnMissing::Create {
        match bless::create_missing(&scenario, &outcome, &reports, &golden_dir, &opts)? {
            bless::BlessOutcome::Refused(reason) => {
                apply_refusal(&mut reports, &reason);
            }
            bless::BlessOutcome::Blessed(manifest) => {
                bless::apply_blessed(&mut reports, &manifest, verdict::scenario_floor(&outcome));
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
        let out = heat::out_dir(opts.out.as_deref(), &scenario.name);
        let heat_problems = match shux_raster::Rasterizer::new(16.0) {
            Ok(rasterizer) => {
                heat::emit_heat_for_fails(&outcome, &mut reports, &golden_dir, &out, &rasterizer)
            }
            Err(e) => {
                let msg = format!("heat evidence skipped: rasterizer unavailable: {e}");
                eprintln!("{}", crate::style::warning(format!("lens gate: {msg}")));
                vec![msg]
            }
        };
        plumb_retry_notes(&mut reports, &heat_problems);
    }

    // Note the produced cast beside the report (task 083) so a reviewer knows where to scrub.
    if let Some(cast) = &cast_path {
        plumb_cast_note(&mut reports, cast);
    }

    if let Err(e) = emit(&opts, &reports) {
        return Ok(report_io_failure(&opts, &e));
    }
    Ok(verdict::exit_code(&reports) as i32)
}

/// Resolve `--cast [PATH]` to an ABSOLUTE path under the gitignored out dir, or `None` when the
/// flag is absent (task 083). Bare `--cast` → `<out>/<scenario>.cast`; `--cast PATH` → PATH. The
/// daemon writes it, so it must be absolute (the daemon's cwd may differ from the CLI's).
fn resolve_cast_path(opts: &GateRunOptions, scenario_name: &str) -> Option<PathBuf> {
    let spec = opts.cast.as_ref()?;
    let rel = if spec.trim().is_empty() {
        heat::out_dir(opts.out.as_deref(), scenario_name).join(format!("{scenario_name}.cast"))
    } else {
        PathBuf::from(spec)
    };
    Some(if rel.is_absolute() {
        rel
    } else {
        std::env::current_dir().map(|d| d.join(&rel)).unwrap_or(rel)
    })
}

/// True when the `.cast` target resolves to a path inside `golden_dir` (task 083 guard). Compares
/// CANONICAL forms of the existing directories (`golden_dir`, and the cast's parent — both exist
/// by the time the gate runs) so a symlinked or `.`-spelled path (e.g. macOS `/tmp` → `/private/
/// tmp`) still compares; falls back to a lexical absolute compare when a dir does not yet exist.
fn cast_is_under(cast: &Path, golden_dir: &Path) -> bool {
    if let (Ok(g), Some(cp)) = (
        golden_dir.canonicalize(),
        cast.parent().and_then(|p| p.canonicalize().ok()),
    ) {
        return cp == g || cp.starts_with(&g);
    }
    let abs = |p: &Path| -> PathBuf {
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|d| d.join(p))
                .unwrap_or_else(|_| p.to_path_buf())
        }
    };
    abs(cast).starts_with(abs(golden_dir))
}

/// Note the produced `.cast` path in every scenario report (task 083) — only when the file
/// actually exists, so the note never claims an artifact the recorder failed to write.
fn plumb_cast_note(reports: &mut [ScenarioReport], cast: &Path) {
    if !cast.exists() {
        return;
    }
    let tag = verdict::sanitize_note(&format!("cast={}", cast.display()));
    for r in reports.iter_mut() {
        r.note = Some(match r.note.take() {
            Some(existing) => format!("{existing}; {tag}"),
            None => tag.clone(),
        });
    }
}

/// Carry `--retries` into the report (082: the retry BUDGET). The frozen schema has no retries
/// field, so it rides in the scenario `note`.
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

/// Fold the runner's per-frame retry AUDIT notes (task 083 council #5) into every scenario note,
/// sanitized (a note is user-facing report text; the same output-boundary hygiene as any note).
/// So a retry that absorbed a flake — or a non-deterministic frame that failed — is auditable in
/// `report.json`, never silent.
fn plumb_retry_notes(reports: &mut [ScenarioReport], notes: &[String]) {
    if notes.is_empty() {
        return;
    }
    let joined = notes
        .iter()
        .map(|n| verdict::sanitize_note(n))
        .collect::<Vec<_>>()
        .join("; ");
    for r in reports.iter_mut() {
        r.note = Some(match r.note.take() {
            Some(existing) => format!("{existing}; {joined}"),
            None => joined.clone(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cast_is_under_catches_a_cast_inside_the_golden_dir() {
        let dir = tempfile::tempdir().unwrap();
        let golden = dir.path().join("goldens");
        std::fs::create_dir_all(&golden).unwrap();
        // A cast written INTO the golden dir is caught (canonicalizes through /tmp symlinks).
        assert!(cast_is_under(&golden.join("sneaky.cast"), &golden));
        assert!(cast_is_under(&golden.join("sub").join("x.cast"), &golden));
        // A cast under a sibling out dir is allowed.
        let out = dir.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        assert!(!cast_is_under(&out.join("run.cast"), &golden));
        // A cast in the parent of the golden dir is allowed.
        assert!(!cast_is_under(&dir.path().join("run.cast"), &golden));
    }
}
