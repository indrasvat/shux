//! The stdout summary table (task 082 §3). DELIBERATELY plain ASCII: no ANSI, no
//! box-drawing / middle-dots — it must survive a `NO_COLOR`, non-UTF-8 CI log and a
//! `| tee` (council #4). Rendered from the frozen `report.json` (the source of truth), so
//! per-frame TIME is unavailable (the schema has no per-frame timing) — TIME is the
//! scenario duration, shown in the header line.
//!
//! Pure: `render` returns the exact string the orchestrator prints (a test pins it).

use shux_vt::{GateStatus, ScenarioReport};

/// The status label as it appears in `report.json` (frozen snake_case), for the table.
fn status_label(s: GateStatus) -> &'static str {
    match s {
        GateStatus::Pass => "pass",
        GateStatus::Fail => "fail",
        GateStatus::Xfail => "xfail",
        GateStatus::Xpass => "xpass",
        GateStatus::MissingGolden => "missing_golden",
        GateStatus::XfailExpired => "xfail_expired",
        GateStatus::StaleGolden => "stale_golden",
        GateStatus::ChildError => "child_error",
        GateStatus::SettleNeverStable => "settle_never_stable",
        GateStatus::ScenarioError => "scenario_error",
        GateStatus::InfraError => "infra_error",
        GateStatus::UpdateRefused => "update_refused",
    }
}

/// Right-pad `s` to `w` columns with ASCII spaces. Callers pass only `ascii_cell`-
/// sanitized (single-byte-per-char) strings, so byte length == display width.
fn pad(s: &str, w: usize) -> String {
    let mut out = String::with_capacity(w.max(s.len()));
    out.push_str(s);
    for _ in s.len()..w {
        out.push(' ');
    }
    out
}

/// Sanitize an arbitrary field to a safe single-byte-per-char ASCII token for the table
/// (adv Agent C, MAJOR-2): a `|` is remapped (else it forges a column boundary), a control
/// char (ESC/newline) and any non-ASCII byte become `?` (else the table stops being
/// ANSI-free ASCII and byte-based padding misaligns). Names/notes are user-authored and
/// the parser admits `|` + non-ASCII, so this is the output-boundary guard.
fn ascii_cell(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c == '|' {
                '/'
            } else if c == ' ' || c.is_ascii_graphic() {
                c
            } else {
                '?'
            }
        })
        .collect()
}

/// Render the deterministic ASCII summary for a set of scenario reports. One header line
/// per scenario (verdict + frame count + scenario time + host) followed by an aligned,
/// pipe-separated `FRAME | STATUS | CHANGED | DETAIL` table. Every user-authored field
/// (scenario/frame name, note, detail) is `ascii_cell`-sanitized so the output is ALWAYS
/// ANSI-free ASCII and column-aligned regardless of hostile names.
pub fn render(reports: &[ScenarioReport]) -> String {
    let mut out = String::new();
    for sr in reports {
        let time = sr
            .duration_ms
            .map(|d| format!("{d}ms"))
            .unwrap_or_else(|| "-".to_string());
        out.push_str(&format!(
            "lens gate  scenario={}  verdict={}  frames={}  time={}  host={}-{}\n",
            ascii_cell(&sr.scenario),
            status_label(sr.status),
            sr.frames.len(),
            time,
            ascii_cell(&sr.os),
            ascii_cell(&sr.arch),
        ));
        if let Some(note) = &sr.note {
            out.push_str(&format!("  note: {}\n", ascii_cell(note)));
        }

        // Column widths from the data (min widths keep short tables readable).
        let names: Vec<String> = sr.frames.iter().map(|f| ascii_cell(&f.name)).collect();
        let statuses: Vec<&str> = sr.frames.iter().map(|f| status_label(f.status)).collect();
        let changed: Vec<String> = sr
            .frames
            .iter()
            .map(|f| match &f.diff {
                Some(d) => d.changed_cells.to_string(),
                None => "-".to_string(),
            })
            .collect();
        let details: Vec<String> = sr
            .frames
            .iter()
            .map(|f| ascii_cell(f.reason.as_deref().unwrap_or_default()))
            .collect();

        let w_name = names.iter().map(|s| s.len()).chain([5]).max().unwrap_or(5);
        let w_stat = statuses
            .iter()
            .map(|s| s.len())
            .chain([6])
            .max()
            .unwrap_or(6);
        let w_chg = changed
            .iter()
            .map(|s| s.len())
            .chain([7])
            .max()
            .unwrap_or(7);

        // Header row + rule.
        out.push_str(&format!(
            "{} | {} | {} | {}\n",
            pad("FRAME", w_name),
            pad("STATUS", w_stat),
            pad("CHANGED", w_chg),
            "DETAIL"
        ));
        out.push_str(&format!(
            "{}-+-{}-+-{}-+-{}\n",
            "-".repeat(w_name),
            "-".repeat(w_stat),
            "-".repeat(w_chg),
            "-".repeat(6),
        ));
        for i in 0..sr.frames.len() {
            out.push_str(&format!(
                "{} | {} | {} | {}\n",
                pad(&names[i], w_name),
                pad(statuses[i], w_stat),
                pad(&changed[i], w_chg),
                details[i],
            ));
        }
    }
    // Invariant (adv Agent C): the summary is ALWAYS ANSI-free ASCII, so byte-based
    // padding aligns and `| tee` in a non-UTF-8 CI log is safe.
    debug_assert!(
        out.is_ascii(),
        "summary must be ASCII-only after sanitization"
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use shux_vt::{DiffReport, FrameReport};

    fn frame(
        name: &str,
        status: GateStatus,
        changed: Option<u32>,
        reason: Option<&str>,
    ) -> FrameReport {
        FrameReport {
            name: name.into(),
            status,
            golden: Some(format!("{name}.capture.json")),
            diff: changed.map(|c| DiffReport {
                changed_cells: c,
                total_cells: 1920,
                max_channel_delta: None,
                heat_png: None,
                regions: None,
                style_deltas: None,
            }),
            reason: reason.map(String::from),
            capture_json: None,
            capture_png: None,
            child_exit: None,
        }
    }

    fn report(status: GateStatus, frames: Vec<FrameReport>) -> ScenarioReport {
        ScenarioReport {
            scenario: "demo".into(),
            status,
            os: "macos".into(),
            arch: "aarch64".into(),
            font_chain_sha256: None,
            font_size_px: Some(16),
            started_at_ms: Some(1),
            duration_ms: Some(1234),
            frames,
            note: None,
        }
    }

    #[test]
    fn table_is_ansi_free_and_ascii_only() {
        let r = report(
            GateStatus::Fail,
            vec![
                frame("start", GateStatus::Pass, None, None),
                frame("edit", GateStatus::Fail, Some(12), Some("cell_diff")),
            ],
        );
        let s = render(&[r]);
        assert!(!s.contains('\x1b'), "summary must be ANSI-free");
        assert!(
            s.is_ascii(),
            "summary must be ASCII-only (tee/NO_COLOR safe)"
        );
        assert!(s.contains("verdict=fail"));
        assert!(s.contains("start"));
        assert!(s.contains("cell_diff"));
    }

    #[test]
    fn columns_are_aligned() {
        // Every data row's pipe positions match the header's.
        let r = report(
            GateStatus::MissingGolden,
            vec![
                frame("a", GateStatus::MissingGolden, None, None),
                frame("a-much-longer-name", GateStatus::Pass, Some(3), None),
            ],
        );
        let s = render(&[r]);
        let lines: Vec<&str> = s.lines().collect();
        // Find the header row (starts with "FRAME").
        let header = lines.iter().find(|l| l.starts_with("FRAME")).unwrap();
        let pipe_cols: Vec<usize> = header.match_indices('|').map(|(i, _)| i).collect();
        for l in lines
            .iter()
            .filter(|l| l.contains('|') && !l.starts_with("FRAME"))
        {
            let cols: Vec<usize> = l.match_indices('|').map(|(i, _)| i).collect();
            assert_eq!(cols, pipe_cols, "row {l:?} misaligned vs header {header:?}");
        }
    }

    #[test]
    fn deterministic_same_input_same_output() {
        let mk = || {
            render(&[report(
                GateStatus::Pass,
                vec![frame("f", GateStatus::Pass, None, None)],
            )])
        };
        assert_eq!(mk(), mk());
    }

    #[test]
    fn hostile_name_cannot_inject_or_break_ascii() {
        // adv Agent C MAJOR-2: a `|` must not forge a column boundary; ESC/newline/non-ASCII
        // must not survive; the table stays ASCII + aligned.
        let mut sr = report(
            GateStatus::Fail,
            vec![
                frame("evil|col", GateStatus::Fail, Some(1), Some("a\x1b[31mb")),
                frame("café-ｗ", GateStatus::Pass, None, None),
            ],
        );
        sr.scenario = "s\x1b]0;pwn\x07|x".into();
        sr.note = Some("boom\n  note: FORGED verdict=pass".into());
        let s = render(&[sr]);
        assert!(s.is_ascii(), "hostile names must not break ASCII: {s:?}");
        assert!(!s.contains('\x1b'), "no raw ANSI");
        // Alignment holds despite the `|` in a name.
        let lines: Vec<&str> = s.lines().collect();
        let header = lines.iter().find(|l| l.starts_with("FRAME")).unwrap();
        let hp: Vec<usize> = header.match_indices('|').map(|(i, _)| i).collect();
        for l in lines
            .iter()
            .filter(|l| l.contains('|') && !l.starts_with("FRAME") && !l.starts_with('-'))
        {
            let cp: Vec<usize> = l.match_indices('|').map(|(i, _)| i).collect();
            assert_eq!(cp, hp, "row {l:?} misaligned");
        }
        // The forged second note LINE cannot appear (the note is flattened to one line).
        assert_eq!(
            s.lines().filter(|l| l.starts_with("  note:")).count(),
            1,
            "note newline forged a separate line: {s:?}"
        );
    }
}
