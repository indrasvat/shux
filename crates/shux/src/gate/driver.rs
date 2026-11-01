//! The 082 orchestrator for `shux lens gate` (the default run verb). Ties the 081 runner
//! mechanics to the frozen verdict/report/exit contract: parse → drive → roll up →
//! emit `report.json` + the ASCII summary → return the frozen exit code. `--on-missing
//! create` and `--update` route through the approval-gated [`super::bless`] writer.

use std::path::{Path, PathBuf};

use crate::gate::vocab::{GateStatus, ScenarioReport};

use super::runner::{Trace, TraceTarget, default_golden_dir};
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
    pub tol: Option<crate::gate::cell_compare::TolParams>,
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

/// Exit code for a CLI-level I/O failure while writing the report (085 F18).
///
/// This used to propagate as an `anyhow` error, which the client turned into exit **1** —
/// the FROZEN regression code — so a bad `--report` path on a perfectly green run told CI
/// "visual regression" and printed a bare errno. `4` is reserved for exactly this class
/// (`shux_vt`'s `exit_code_never_returns_four` pins that no gate VERDICT can produce it),
/// so it is unambiguous: the check itself did not fail, writing its output did.
fn report_io_failure(failure: OutputFailure) -> anyhow::Result<i32> {
    let OutputFailure { what, error } = failure;
    if error.kind() == std::io::ErrorKind::BrokenPipe {
        return Err(crate::style::OutputError(error).into());
    }
    eprintln!(
        "{}",
        crate::style::error(format!(
            "lens gate: could not write {what}: {error}. The scenario itself ran; only its \
             output could not be written (exit 4, not a regression)."
        ))
    );
    Ok(4)
}

/// Whether `e` is a write to a reader that left, which ends the client by SIGPIPE.
pub fn reader_left(e: &anyhow::Error) -> bool {
    e.chain()
        .filter_map(|c| c.downcast_ref::<crate::style::OutputError>())
        .any(|o| o.0.kind() == std::io::ErrorKind::BrokenPipe)
}

fn broken_pipe() -> anyhow::Error {
    crate::style::OutputError(std::io::ErrorKind::BrokenPipe.into()).into()
}

/// An output of the gate that could not be written, and which one.
struct OutputFailure {
    what: String,
    error: std::io::Error,
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
fn emit(opts: &GateRunOptions, reports: &[ScenarioReport]) -> Result<(), OutputFailure> {
    use std::io::Write;
    let stdout_is_json =
        matches!(opts.format, OutputFormat::Json) || opts.report.as_deref() == Some("-");
    let json = serde_json::to_string_pretty(reports).unwrap_or_else(|_| "[]".to_string());
    let to_stdout = |what: &str, text: &str| {
        let mut out = std::io::stdout().lock();
        out.write_all(text.as_bytes())
            .and_then(|()| out.flush())
            .map_err(|e| OutputFailure {
                what: what.to_string(),
                error: e,
            })
    };
    let report_err = match opts.report.as_deref() {
        Some(path) if path != "-" => {
            std::fs::write(path, format!("{json}\n"))
                .err()
                .map(|e| OutputFailure {
                    what: format!("the report file {path}"),
                    error: e,
                })
        }
        _ if stdout_is_json => to_stdout("the report stream", &format!("{json}\n")).err(),
        _ => None,
    };
    // The summary is emitted even when the report file could not be written (085 QA P2):
    // returning early left a user with an errno and NO verdict at all, on a run that had
    // actually completed and produced one. On that path it goes to stderr, since stdout
    // may still be a JSON stream a consumer is reading. stderr is best-effort.
    let table = summary::render(reports);
    if stdout_is_json || report_err.is_some() {
        let _ = std::io::stderr().write_all(table.as_bytes());
        report_err.map_or(Ok(()), Err)
    } else {
        to_stdout("the summary", &table)
    }
}

/// Run the gate on one scenario and return the frozen exit code (task §4). This is the
/// 082 entry `main.rs` dispatches to for the default `shux lens gate <scenario>`.
pub async fn run_gate(socket_path: &Path, opts: GateRunOptions) -> anyhow::Result<i32> {
    // Before anything is written, refusals included. A bare `--cast` is checked again
    // once the scenario names its file.
    if let Some(code) = refuse_shared_outputs(&opts, explicit_cast(&opts)) {
        return Ok(code);
    }
    // The trace and report are output, never goldens: one written into the golden tree
    // replaces a golden, and the run then reports the damage as a regression. Without
    // `--golden-dir`, every default golden dir sits under `<scenario dir>/goldens`.
    let goldens = opts
        .golden_dir
        .clone()
        .unwrap_or_else(|| runner::scenario_dir_of(&opts.scenario_path).join("goldens"));
    for (flag, path) in [
        ("--trace", opts.trace.as_deref()),
        ("--report", opts.report.as_deref()),
    ] {
        if let Some(path) = path.filter(|p| *p != "-").map(Path::new)
            && path_is_under(path, &goldens)
        {
            eprintln!(
                "{}",
                crate::style::error(format!(
                    "lens gate: {flag} {} is inside the golden dir {}. Write it elsewhere.",
                    path.display(),
                    goldens.display()
                ))
            );
            return Ok(GateStatus::ScenarioError.exit_code() as i32);
        }
    }
    // 085: `--trace -` and a stdout JSON report both claim stdout, and the result was
    // NDJSON signals followed by the report array — unparseable, with no usage error and
    // exit 0. The report stream is promised to carry ONLY the JSON, so this is a usage
    // error, refused before anything else is read or written.
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
            return report_io_failure(e);
        }
        return Ok(GateStatus::ScenarioError.exit_code() as i32);
    }
    let scenario = match scenario::load(&opts.scenario_path) {
        Ok(s) => s,
        Err(e) => {
            // Preserve the 081 trace contract: a malformed scenario still leaves a
            // greppable `parse_error` in the trace.
            let (lost_trace, reader_left) = match Trace::open(trace_target(opts.trace.clone())) {
                Ok(mut trace) => {
                    trace.parse_error(&e.to_string());
                    if trace.reader_left() {
                        (None, true)
                    } else {
                        (trace.failure(), false)
                    }
                }
                Err(why) => (Some(why), false),
            };
            eprintln!("{}", crate::style::error(format!("lens gate: {e}")));
            let mut reports = parse_error_report(&opts.scenario_path, &e.to_string());
            if let Some(why) = &lost_trace {
                plumb_retry_notes(&mut reports, &[format!("trace incomplete: {why}")]);
            }
            if let Err(e) = emit(&opts, &reports) {
                return report_io_failure(e);
            }
            if reader_left {
                return Err(broken_pipe());
            }
            let code = GateStatus::ScenarioError.exit_code() as i32;
            return Ok(lost_trace.map_or(code, |why| trace_failure(&why, code)));
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
            return report_io_failure(e);
        }
        return Ok(GateStatus::UpdateRefused.exit_code() as i32);
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
        && path_is_under(cast, &golden_dir)
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
            return report_io_failure(e);
        }
        return Ok(GateStatus::ScenarioError.exit_code() as i32);
    }

    if let Some(code) = refuse_shared_outputs(&opts, cast_path.as_deref()) {
        return Ok(code);
    }
    let mut trace = match Trace::open(trace_target(opts.trace.clone())) {
        Ok(trace) => trace,
        Err(why) => {
            eprintln!(
                "{}",
                crate::style::error(format!(
                    "lens gate: {why}. The scenario was not run (exit 4, not a regression)."
                ))
            );
            return Ok(4);
        }
    };

    let outcome = match runner::drive_scenario(
        socket_path,
        &scenario,
        &runner::scenario_dir_of(&opts.scenario_path),
        &argv,
        &golden_dir,
        &mut trace,
        opts.retries.unwrap_or(0),
        cast_path.clone(),
    )
    .await
    {
        Ok(outcome) => outcome,
        // No verdict: the error exits 3, but the trace still ends as it would.
        Err(_) if trace.reader_left() => return Err(broken_pipe()),
        Err(e) => {
            if let Some(why) = trace.failure() {
                report_lost_trace(&why);
            }
            return Err(e);
        }
    };

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
    let mut bless_failure = None;
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
    } else {
        let blessed = if let Some(selector) = &opts.update {
            Some(bless::run_update(
                &scenario,
                &outcome,
                &reports,
                &golden_dir,
                selector,
                &opts,
            ))
        } else if opts.on_missing == crate::cli::OnMissing::Create {
            Some(bless::create_missing(
                &scenario,
                &outcome,
                &reports,
                &golden_dir,
                &opts,
            ))
        } else {
            None
        };
        match blessed {
            Some(Ok(bless::BlessOutcome::Refused(reason))) => apply_refusal(&mut reports, &reason),
            Some(Ok(bless::BlessOutcome::Blessed(manifest))) => {
                bless::apply_blessed(&mut reports, &manifest, verdict::scenario_floor(&outcome));
            }
            Some(Err(e)) => {
                plumb_retry_notes(&mut reports, &[format!("goldens not written: {e:#}")]);
                bless_failure = Some(e);
            }
            None => {}
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

    let lost_trace = trace.failure();
    if let Some(why) = &lost_trace {
        plumb_retry_notes(&mut reports, &[format!("trace incomplete: {why}")]);
    }
    if let Err(e) = emit(&opts, &reports) {
        if let Some(why) = lost_trace.as_ref().filter(|_| !trace.reader_left()) {
            report_lost_trace(why);
        }
        return report_io_failure(e);
    }
    if trace.reader_left() {
        return Err(broken_pipe());
    }
    let code = verdict::exit_code(&reports) as i32;
    let code = lost_trace.map_or(code, |why| trace_failure(&why, code));
    Ok(bless_failure.map_or(code, |e| bless_failure_exit(&e, code)))
}

/// Writing the goldens failed after the run: the report's verdict stands, and a run that
/// was not a regression exits 3.
fn bless_failure_exit(e: &anyhow::Error, code: i32) -> i32 {
    eprintln!(
        "{}",
        crate::style::error(format!(
            "lens gate: could not write the goldens: {e:#}. The verdict in the report stands; \
             no golden was blessed."
        ))
    );
    if code == GateStatus::Fail.exit_code() as i32 {
        code
    } else {
        GateStatus::InfraError.exit_code() as i32
    }
}

/// Refuses, before anything is written, a run in which two of its named outputs (the
/// trace, the report, the cast) are the same file, or one of them is where stdout or
/// stderr already goes: one would overwrite the other. Nothing is opened or created.
/// An existing file is compared by identity, following links; an absent one by its
/// deepest existing ancestor, resolved by the kernel, and the rest as spelled. A link to a file that does not exist is
/// refused outright, since only creating its target would say where it leads. FIFOs
/// and devices are never compared, and stdout and stderr are not compared with each
/// other, so `> log 2>&1` stays legal.
fn refuse_shared_outputs(opts: &GateRunOptions, cast: Option<&Path>) -> Option<i32> {
    use std::os::fd::AsFd;
    use std::os::unix::fs::MetadataExt;

    #[derive(PartialEq)]
    enum Dest {
        File(u64, u64),
        Absent(PathBuf),
    }
    let refuse = |msg: String| {
        eprintln!("{}", crate::style::error(format!("lens gate: {msg}")));
        Some(GateStatus::ScenarioError.exit_code() as i32)
    };
    let stream = |fd: std::os::fd::BorrowedFd<'_>| {
        let meta = std::fs::File::from(fd.try_clone_to_owned().ok()?)
            .metadata()
            .ok()?;
        meta.is_file().then(|| Dest::File(meta.dev(), meta.ino()))
    };
    let stdout = stream(std::io::stdout().as_fd());
    let stderr = stream(std::io::stderr().as_fd());

    let mut named: Vec<(String, Dest)> = Vec::new();
    if opts.trace.as_deref() == Some("-")
        && let Some(Dest::File(dev, ino)) = stdout
    {
        named.push(("--trace -".to_string(), Dest::File(dev, ino)));
    }
    for (flag, path) in [
        (
            "--trace",
            opts.trace.as_deref().filter(|t| *t != "-").map(Path::new),
        ),
        (
            "--report",
            opts.report.as_deref().filter(|r| *r != "-").map(Path::new),
        ),
        ("--cast", cast),
    ] {
        let Some(path) = path else { continue };
        let label = format!("{flag} {}", path.display());
        let dest = match std::fs::metadata(path) {
            Ok(meta) if meta.is_file() => Dest::File(meta.dev(), meta.ino()),
            Ok(_) => continue,
            Err(_) if std::fs::symlink_metadata(path).is_ok() => {
                return refuse(format!(
                    "{label} is a link to a file that does not exist. Name the file itself."
                ));
            }
            Err(_) => Dest::Absent(resolve_existing_prefix(path)),
        };
        if let Some((earlier, _)) = named.iter().find(|(_, d)| *d == dest) {
            return refuse(format!(
                "{label} is the same file as {earlier}, so one would overwrite the other. \
                 Give them different files."
            ));
        }
        for (stream_name, stream_dest) in [("stdout", &stdout), ("stderr", &stderr)] {
            if stream_dest.as_ref() == Some(&dest) {
                return refuse(format!(
                    "{label} is the file {stream_name} already writes to, so one would \
                     overwrite the other. Give them different files."
                ));
            }
        }
        named.push((label, dest));
    }
    None
}

/// `path` with its deepest existing ancestor resolved by the kernel and the rest, which
/// does not exist and so holds no links, appended as spelled.
fn resolve_existing_prefix(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut ancestor = path;
    loop {
        let dir = if ancestor.as_os_str().is_empty() {
            Path::new(".")
        } else {
            ancestor
        };
        if let Ok(resolved) = std::fs::canonicalize(dir) {
            return rest.iter().rev().fold(resolved, |p, c| p.join(c));
        }
        match (ancestor.file_name(), ancestor.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name.to_os_string());
                ancestor = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// `--cast PATH` as given; a bare `--cast` names a file only once the scenario does.
fn explicit_cast(opts: &GateRunOptions) -> Option<&Path> {
    opts.cast
        .as_deref()
        .filter(|c| !c.trim().is_empty())
        .map(Path::new)
}

/// A trace that could not be written: exit 4, unless the run is a regression, which
/// keeps exit 1 so CI still blocks on it.
fn trace_failure(why: &str, code: i32) -> i32 {
    report_lost_trace(why);
    if code == GateStatus::Fail.exit_code() as i32 {
        code
    } else {
        4
    }
}

fn report_lost_trace(why: &str) {
    eprintln!(
        "{}",
        crate::style::error(format!("lens gate: {why}. The trace is incomplete."))
    );
}

/// The gate's own machinery failed, so there is no verdict: exit 3, never 1.
pub fn infra_failure(e: &anyhow::Error) -> i32 {
    eprintln!(
        "{}",
        crate::style::error(format!(
            "lens gate: {e:#}. The gate itself failed, so the scenario has no verdict \
             (exit 3, infrastructure, not a regression)."
        ))
    );
    GateStatus::InfraError.exit_code() as i32
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

/// True when an output path resolves to a place inside `golden_dir` (task 083 guard). Compares
/// CANONICAL forms of the existing directories (`golden_dir`, and the path's parent) so a
/// symlinked or `.`-spelled path (e.g. macOS `/tmp` → `/private/tmp`) still compares; falls back
/// to a lexical absolute compare when a dir does not yet exist.
fn path_is_under(path: &Path, golden_dir: &Path) -> bool {
    // Where the write lands and where the name sits both count: a link outside the tree
    // pointing in, and a link inside it pointing out, are both inside.
    if let (Ok(g), Ok(p)) = (golden_dir.canonicalize(), path.canonicalize())
        && p.starts_with(&g)
    {
        return true;
    }
    if let (Ok(g), Some(cp)) = (
        golden_dir.canonicalize(),
        path.parent().and_then(|p| p.canonicalize().ok()),
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
    abs(path).starts_with(abs(golden_dir))
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
        assert!(path_is_under(&golden.join("sneaky.cast"), &golden));
        assert!(path_is_under(&golden.join("sub").join("x.cast"), &golden));
        // A cast under a sibling out dir is allowed.
        let out = dir.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        assert!(!path_is_under(&out.join("run.cast"), &golden));
        // A cast in the parent of the golden dir is allowed.
        assert!(!path_is_under(&dir.path().join("run.cast"), &golden));
    }
}
