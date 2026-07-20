//! `shux lens gate init <name>` (task §7) — scaffold a starter scenario `.toml`, then
//! mint its first goldens through the approval-gated create path. Refused in CI (a golden
//! must never be self-minted there). Golden creation is 082's domain; 085 only documents.

use std::path::{Path, PathBuf};

use crate::cli::{OnMissing, OutputFormat};
use crate::gate::driver::{self, GateRunOptions};
use crate::gate::scenario;
use crate::style;

/// A runnable starter scenario: draws a coloured line, holds the frame, expects a golden.
/// The command is self-contained (`printf … ; exec cat`) so the first `gate init` run
/// produces a stable, quiet frame to bless.
fn template(name: &str) -> String {
    format!(
        r#"# Scenario scaffolded by `shux lens gate init {name}`.
# Edit `command` + steps to drive your real TUI, then re-run to bless updated goldens.
name = "{name}"
description = "Scaffolded lens-gate scenario for {name}."
command = ["/bin/sh", "-c", "printf '\\033[38;2;120;200;255m{name} ready\\033[0m\\n'; exec cat"]

[terminal]
rows = 24
cols = 80

[env]
LC_ALL = "C.UTF-8"
TZ = "UTC"
TERM = "xterm-256color"
COLORTERM = "truecolor"

[[steps]]
action = "wait_for_text"
text = "{name} ready"
timeout_ms = 5000

[[steps]]
action = "settle"
quiet_ms = 300
timeout_ms = 5000

[[steps]]
action = "expect_golden"
name = "start"
tier = "cell"
"#
    )
}

/// Scaffold the scenario file, then run once with `--on-missing create` to mint goldens.
pub async fn run_init(
    socket_path: &Path,
    name: String,
    dir: Option<PathBuf>,
) -> anyhow::Result<i32> {
    if driver::is_ci() {
        eprintln!(
            "{}",
            style::warning("lens gate init is refused in CI: goldens are never self-minted here")
        );
        return Ok(shux_vt::GateStatus::UpdateRefused.exit_code() as i32);
    }
    // The scenario name is a filesystem component — validate via the same parser guard by
    // building a minimal scenario and letting `parse` reject a hostile name.
    if scenario::parse(&format!("name = \"{name}\"\ncommand = [\"true\"]\n")).is_err() {
        eprintln!(
            "{}",
            style::error(format!(
                "invalid scenario name {name:?}: must be a safe single path component"
            ))
        );
        return Ok(2);
    }

    let dir = dir.unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir)?;
    let scenario_path = dir.join(format!("{name}.toml"));
    if scenario_path.exists() {
        eprintln!(
            "{}",
            style::error(format!(
                "{} already exists — refusing to overwrite",
                scenario_path.display()
            ))
        );
        return Ok(2);
    }
    std::fs::write(&scenario_path, template(&name))?;
    println!(
        "{} {}",
        style::success("scaffolded scenario"),
        style::bold(scenario_path.display())
    );

    // Mint the first goldens through the guarded create path.
    let opts = GateRunOptions {
        scenario_path: scenario_path.clone(),
        golden_dir: None,
        report: None,
        on_missing: OnMissing::Create,
        update: None,
        reason: Some(format!("first goldens via `lens gate init {name}`")),
        tol: None,
        out: None,
        retries: None,
        cast: None,
        trace: None,
        argv: vec![],
        format: OutputFormat::Text,
    };
    let code = driver::run_gate(socket_path, opts).await?;
    if code == 0 {
        println!(
            "{}",
            style::success(
                "first goldens minted - review them before committing (a `cell` golden is \
                 <name>.capture.json, not a PNG; see references/gate.md)",
            )
        );
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses_as_a_valid_scenario() {
        let s = scenario::parse(&template("demo")).expect("template must parse");
        assert_eq!(s.name, "demo");
        assert!(
            s.steps
                .iter()
                .any(|st| matches!(st, scenario::Step::ExpectGolden { .. })),
            "template must have a visual check"
        );
    }
}
