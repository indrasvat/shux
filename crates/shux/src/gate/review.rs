//! `shux lens gate review` (task §6) — insta's step-through accept/reject/skip model made
//! visual, WITHOUT insta's cargo/`#[test]` coupling. For each changed (fail/missing/stale)
//! frame it renders before/after + a heat overlay, then prompts. Inline kitty/iTerm2
//! graphics is a thin follow-on; here the PNGs are written to `--out` and their paths
//! printed (the always-correct fallback). Accept blesses through the guarded writer; a
//! rejected frame stays failing.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use shux_raster::{Rasterizer, encode_png, render_envelope};
use shux_vt::{FrameEnvelope, GateStatus};

use super::runner::{default_golden_dir, drive_scenario};
use super::scenario;
use super::{bless, compare, verdict};
use crate::cli::{OnMissing, OutputFormat};
use crate::style;

const FONT_SIZE: f32 = 16.0;

/// The interactive review loop. Returns a process exit code (0 = all changed frames
/// resolved; 1 = some left failing; 6 = refused, e.g. non-interactive).
pub async fn run_review(
    socket_path: &Path,
    scenario_path: PathBuf,
    golden_dir_opt: Option<PathBuf>,
    out_opt: Option<PathBuf>,
) -> anyhow::Result<i32> {
    if super::driver::is_ci() || !std::io::stdin().is_terminal() {
        eprintln!(
            "{}",
            style::warning(
                "lens gate review is interactive; use `shux lens gate` (fails on drift) or a \
                 guarded `--update` in CI/non-interactive contexts"
            )
        );
        return Ok(GateStatus::UpdateRefused.exit_code() as i32);
    }

    let scenario = scenario::load(&scenario_path)?;
    let golden_dir = golden_dir_opt
        .clone()
        .unwrap_or_else(|| default_golden_dir(&scenario_path, &scenario));
    let out_dir = out_opt
        .clone()
        .unwrap_or_else(|| PathBuf::from(".shux/out").join(&scenario.name));

    let outcome = drive_scenario(
        socket_path,
        &scenario,
        scenario_path.parent().unwrap_or(Path::new(".")),
        &scenario.command.clone(),
        &golden_dir,
        None,
        // `gate review` has no --retries flag; per-step `expect_golden.retries` still applies.
        0,
        // `gate review` does not record a cast.
        None,
    )
    .await?;
    let today = chrono::Utc::now().date_naive();
    let reports = verdict::build_reports(&outcome, today);
    let statuses: Vec<GateStatus> = reports
        .first()
        .map(|r| r.frames.iter().map(|f| f.status).collect())
        .unwrap_or_default();

    let changed: Vec<usize> = (0..outcome.frames.len())
        .filter(|&i| {
            matches!(
                statuses.get(i),
                Some(GateStatus::Fail | GateStatus::MissingGolden | GateStatus::StaleGolden)
            )
        })
        .collect();

    if changed.is_empty() {
        println!(
            "{}",
            style::success("lens gate review: no changed frames — all goldens match")
        );
        return Ok(0);
    }

    std::fs::create_dir_all(&out_dir)?;
    let rasterizer = Rasterizer::new(FONT_SIZE)?;
    let stdin = std::io::stdin();
    let mut remaining_failing = 0usize;

    for (n, &i) in changed.iter().enumerate() {
        let f = &outcome.frames[i];
        println!(
            "\n{} [{}/{}]  {}  ({})",
            style::accent("review"),
            n + 1,
            changed.len(),
            style::bold(&f.name),
            status_label(statuses[i]),
        );
        // Write the evidence PNGs (before/after/heat) and print their paths.
        match write_evidence(&out_dir, &golden_dir, f, &rasterizer) {
            Ok(paths) => {
                for p in paths {
                    println!("  {}", style::muted(p.display()));
                }
            }
            Err(e) => println!(
                "  {}",
                style::warning(format!("(evidence unavailable: {e})"))
            ),
        }

        print!("  {} ", style::bold("accept / reject / skip [a/r/s]?"));
        std::io::stdout().flush().ok();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break; // EOF — stop.
        }
        match line.trim().to_ascii_lowercase().as_str() {
            "a" | "accept" => {
                let bopts = review_opts(&scenario_path, &golden_dir_opt, &out_opt);
                match bless::run_update(
                    &scenario,
                    &outcome,
                    &reports,
                    &golden_dir,
                    &f.name,
                    &bopts,
                )? {
                    bless::BlessOutcome::Blessed(_) => {
                        println!("  {}", style::success("accepted — golden blessed"));
                    }
                    bless::BlessOutcome::Refused(reason) => {
                        println!("  {}", style::error(format!("refused: {reason}")));
                        remaining_failing += 1;
                    }
                }
            }
            "r" | "reject" => {
                println!("  {}", style::muted("rejected — frame stays failing"));
                remaining_failing += 1;
            }
            _ => {
                println!("  {}", style::muted("skipped"));
                remaining_failing += 1;
            }
        }
    }

    if remaining_failing == 0 { Ok(0) } else { Ok(1) }
}

/// Render before (golden) + after (live) PNGs into `out_dir`; return their paths. A missing
/// golden omits the before frame. (The heat overlay is written by the non-interactive gate
/// report path — `gate::heat` — so it is available headless, not only here.)
fn write_evidence(
    out_dir: &Path,
    golden_dir: &Path,
    f: &super::outcome::FrameOutcome,
    rasterizer: &Rasterizer,
) -> Result<Vec<PathBuf>, String> {
    let live = FrameEnvelope::from_canonical_json(&f.live_capture_json)
        .map_err(|e| format!("live capture parse: {e:?}"))?;
    let mut paths = Vec::new();

    let after = encode_png(&render_envelope(rasterizer, &live)).map_err(|e| e.to_string())?;
    let after_path = out_dir.join(format!("{}.after.png", f.name));
    std::fs::write(&after_path, &after).map_err(|e| e.to_string())?;
    paths.push(after_path);

    // Before + heat only when a readable golden exists.
    if let Ok(text) = std::fs::read_to_string(compare::cell_json_path(golden_dir, &f.name)) {
        if let Ok(golden) = FrameEnvelope::from_canonical_json(&text) {
            let before =
                encode_png(&render_envelope(rasterizer, &golden)).map_err(|e| e.to_string())?;
            let before_path = out_dir.join(format!("{}.before.png", f.name));
            std::fs::write(&before_path, &before).map_err(|e| e.to_string())?;
            paths.push(before_path);
        }
    }
    Ok(paths)
}

/// A `GateRunOptions` for the single-frame bless a review accept performs.
fn review_opts(
    scenario_path: &Path,
    golden_dir_opt: &Option<PathBuf>,
    out_opt: &Option<PathBuf>,
) -> super::driver::GateRunOptions {
    super::driver::GateRunOptions {
        scenario_path: scenario_path.to_path_buf(),
        golden_dir: golden_dir_opt.clone(),
        report: None,
        on_missing: OnMissing::Fail,
        update: None,
        reason: Some("accepted via `lens gate review`".to_string()),
        tol: None,
        out: out_opt.clone(),
        retries: None,
        cast: None,
        trace: None,
        argv: vec![],
        format: OutputFormat::Text,
    }
}

fn status_label(s: GateStatus) -> &'static str {
    match s {
        GateStatus::Fail => "fail",
        GateStatus::MissingGolden => "missing_golden",
        GateStatus::StaleGolden => "stale_golden",
        _ => "changed",
    }
}
