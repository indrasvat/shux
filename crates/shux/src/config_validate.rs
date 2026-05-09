//! `shux config validate` — line-numbered diagnostics for the user
//! `~/.config/shux/config.toml` plus every inline `starship_config`
//! string nested under `[[statusbar.segment]]`.
//!
//! Exit code is 0 when the file is empty / valid, 1 when at least one
//! diagnostic was emitted. The CLI dispatcher in `main.rs` translates
//! that into `std::process::exit(1)` so shell pipelines can branch on
//! validation status.

use std::path::{Path, PathBuf};

use crate::style;

// ─── Strict mirror schema ────────────────────────────────────────────
//
// The runtime `Config` in shux-core is intentionally lenient — extra
// keys are silently ignored so a typo never bricks a user's daemon
// startup. The validator wants the *opposite* discipline: a field
// like `[appearence]` (typo) or `borderstyle = "rounded"` (missing
// underscore) MUST surface, otherwise `shux config validate` cheerfully
// reports success while the user's intent is being thrown away.
//
// We keep the strict mirror here, in the binary crate, so the daemon's
// schema stays lenient. Both shapes share the toml file format; the
// strict variant adds `deny_unknown_fields` everywhere AND keeps the
// same `#[serde(default)]` so missing sections still parse.

// All fields below are populated by the toml deserializer; nothing
// reads them directly because we discard the typed Config after
// validation. Suppress the dead-field warnings rather than #[allow]ing
// each one.
#[allow(dead_code)]
mod strict {
    use serde::Deserialize;
    use std::collections::HashMap;

    #[derive(Debug, Default, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Config {
        #[serde(default)]
        pub appearance: Appearance,
        #[serde(default)]
        pub keys: Keys,
        #[serde(default)]
        pub shell: Shell,
        #[serde(default)]
        pub statusbar: StatusBar,
    }

    #[derive(Debug, Default, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Appearance {
        #[serde(default)]
        pub border_style: Option<String>,
    }

    #[derive(Debug, Default, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Keys {
        #[serde(default)]
        pub prefix: Option<String>,
    }

    #[derive(Debug, Default, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Shell {
        #[serde(default)]
        pub command: Vec<String>,
        #[serde(default)]
        pub env: HashMap<String, String>,
    }

    #[derive(Debug, Default, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct StatusBar {
        #[serde(default)]
        pub left: Option<String>,
        #[serde(default)]
        pub center: Option<String>,
        #[serde(default)]
        pub right: Option<String>,
        #[serde(default)]
        pub segment: Vec<Segment>,
    }

    #[derive(Debug, Default, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Segment {
        #[serde(default)]
        pub zone: Option<String>,
        #[serde(default)]
        pub command: Vec<String>,
        #[serde(default)]
        pub env: HashMap<String, String>,
        #[serde(default)]
        pub starship_config: Option<String>,
        #[serde(default)]
        pub interval_ms: Option<u64>,
        #[serde(default)]
        pub fallback: Option<String>,
    }
}

/// One diagnostic, ready to print.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    /// Display path of the file the error came from.
    pub file: PathBuf,
    /// 1-based line number; `None` when the error has no span (rare).
    pub line: Option<usize>,
    /// 1-based column number; only meaningful when `line.is_some()`.
    pub column: Option<usize>,
    /// Free-form context string ("appearance" / "statusbar.segment[2]
    /// .starship_config") describing where in the document the error
    /// surfaced. Empty for top-level errors.
    pub context: String,
    /// Human-readable message.
    pub message: String,
}

/// Convert a byte offset inside `content` into a 1-based (line, column)
/// pair. Used to translate `toml::de::Error` spans into editor-friendly
/// coordinates. Tab characters count as one column — matching what
/// most editors show.
fn byte_to_line_col(content: &str, byte_offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    let bytes = content.as_bytes();
    let target = byte_offset.min(bytes.len());
    for &b in &bytes[..target] {
        if b == b'\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Locate the `starship_config = """..."""` block belonging to the
/// `segment_index`-th `[[statusbar.segment]]` header in the file.
/// Returns the 1-based outer line on which the triple-quoted string
/// body STARTS — i.e. the line after the opening `"""`.
///
/// This is a deliberately small heuristic — a line scanner rather than
/// a full TOML round-trip — but it tracks the **segment** index, not
/// the count of starship blocks. Counting blocks would be wrong when
/// an earlier segment has no `starship_config`: index 1 would target
/// the only block in the document instead of returning `None` for an
/// orphan inner-TOML error.
///
/// Returns `None` when the segment has no inline `starship_config` (or
/// uses a non-multiline form) — the caller falls back to reporting
/// without an outer line.
fn find_starship_config_start_line(content: &str, segment_index: usize) -> Option<usize> {
    // -1 means "before any [[statusbar.segment]] header has been seen".
    let mut current_segment: i64 = -1;
    for (line_idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        // A new statusbar.segment table header bumps the segment cursor.
        if trimmed == "[[statusbar.segment]]" {
            current_segment += 1;
            continue;
        }
        // `starship_config = """` lines belong to whatever segment we
        // are currently inside.
        if current_segment >= 0 && (current_segment as usize) == segment_index {
            let lstripped = line.trim_start();
            if lstripped.starts_with("starship_config")
                && let Some(eq_pos) = lstripped.find('=')
            {
                let after = lstripped[eq_pos + 1..].trim_start();
                if after.starts_with("\"\"\"") {
                    // 0-based line index → 1-based line + skip opener.
                    return Some(line_idx + 2);
                }
            }
        }
    }
    None
}

/// Validate one config file. Returns the collected diagnostics; the
/// caller decides how to render them and what exit code to use.
pub fn validate(path: &Path) -> std::io::Result<Vec<Diagnostic>> {
    let mut diags = Vec::new();
    let content = std::fs::read_to_string(path)?;

    // ----- Stage 1: parse the outer config (strict — denies unknown keys) -----
    match toml::from_str::<strict::Config>(&content) {
        Ok(cfg) => {
            // Stage 2 only runs if the outer parse succeeded — there's
            // no point validating inner snippets when we cannot even
            // locate them.
            for (idx, segment) in cfg.statusbar.segment.iter().enumerate() {
                let Some(starship_text) = segment.starship_config.as_deref() else {
                    continue;
                };
                if let Err(err) = toml::from_str::<toml::Value>(starship_text) {
                    let outer_line_offset =
                        find_starship_config_start_line(&content, idx).unwrap_or(0);
                    let (inner_line, inner_col) = err
                        .span()
                        .map(|s| byte_to_line_col(starship_text, s.start))
                        .unwrap_or((1, 1));
                    let line = if outer_line_offset > 0 {
                        // Map inner (line, col) into outer file. Inner
                        // line 1 lands on outer_line_offset.
                        Some(outer_line_offset + inner_line - 1)
                    } else {
                        None
                    };
                    diags.push(Diagnostic {
                        file: path.to_path_buf(),
                        line,
                        column: line.map(|_| inner_col),
                        context: format!("statusbar.segment[{idx}].starship_config"),
                        message: err.message().to_string(),
                    });
                }
            }
        }
        Err(err) => {
            let (line, col) = err
                .span()
                .map(|s| byte_to_line_col(&content, s.start))
                .map(|(l, c)| (Some(l), Some(c)))
                .unwrap_or((None, None));
            diags.push(Diagnostic {
                file: path.to_path_buf(),
                line,
                column: col,
                context: String::new(),
                message: err.message().to_string(),
            });
        }
    }

    Ok(diags)
}

/// Render diagnostics to stderr in the canonical `path:line:col:
/// context: message` shape. Returns 0 if no diagnostics, 1 otherwise.
pub fn print_diagnostics(diags: &[Diagnostic], path: &Path) -> i32 {
    use std::io::Write;
    let mut err = std::io::stderr().lock();

    if diags.is_empty() {
        // Use stdout for the "ok" message — pairs nicely with `&&`.
        style::print_success("Config valid:", path.display().to_string().as_str(), None);
        return 0;
    }

    for d in diags {
        let mut prefix = format!("{}", d.file.display());
        if let (Some(l), Some(c)) = (d.line, d.column) {
            prefix.push_str(&format!(":{l}:{c}"));
        }
        if !d.context.is_empty() {
            prefix.push_str(&format!(": [{}]", d.context));
        }
        let _ = writeln!(
            err,
            "{} {}: {}",
            style::error("\u{2717}"),
            prefix,
            d.message,
        );
    }

    let n = diags.len();
    let _ = writeln!(
        err,
        "\n{} {} {} found.",
        style::error("\u{2717}"),
        n,
        if n == 1 { "error" } else { "errors" }
    );
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    fn write_tmp(contents: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        f
    }

    #[test]
    fn validate_empty_file_is_ok() {
        let f = write_tmp("");
        let diags = validate(f.path()).unwrap();
        assert!(diags.is_empty());
    }

    #[test]
    fn validate_default_config_is_ok() {
        // The DEFAULT_CONFIG_TOML in cli.rs is the canonical happy
        // path — the validator must accept it. We pull a minimal
        // representative subset here so the test does not depend on
        // the cli module.
        let f = write_tmp(
            r#"
[appearance]
border_style = "rounded"

[keys]
prefix = "ctrl-space"

[[statusbar.segment]]
zone = "left"
command = ["echo", "hi"]
interval_ms = 2000
"#,
        );
        let diags = validate(f.path()).unwrap();
        assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    }

    #[test]
    fn validate_top_level_typo_reports_line_col() {
        let f = write_tmp("[appearance]\nborder_style 42\n");
        let diags = validate(f.path()).unwrap();
        assert_eq!(diags.len(), 1);
        assert!(diags[0].line.is_some());
        assert!(diags[0].column.is_some());
        // Error lives on line 2 ("border_style 42" — missing `=`).
        assert_eq!(diags[0].line, Some(2));
    }

    #[test]
    fn validate_unknown_field_with_invalid_value() {
        let f = write_tmp("[appearance]\nborder_style = 42\n");
        let diags = validate(f.path()).unwrap();
        assert_eq!(diags.len(), 1);
        // `42` is on line 2 — the integer is at column 16.
        assert_eq!(diags[0].line, Some(2));
    }

    #[test]
    fn validate_inner_starship_config_with_bad_toml() {
        let cfg = r#"
[[statusbar.segment]]
zone = "left"
command = ["starship", "prompt"]
interval_ms = 1000
starship_config = """
[character]
this is not valid toml
"""
"#;
        let f = write_tmp(cfg);
        let diags = validate(f.path()).unwrap();
        assert_eq!(diags.len(), 1, "expected one inner-TOML diagnostic");
        let d = &diags[0];
        assert!(
            d.context.contains("statusbar.segment[0].starship_config"),
            "context: {}",
            d.context
        );
        // Layout of the test config (1-based outer lines):
        //   1: (blank)
        //   2: [[statusbar.segment]]
        //   3: zone = ...
        //   4: command = ...
        //   5: interval_ms = ...
        //   6: starship_config = """          <- opening fence
        //   7: [character]                    <- inner line 1
        //   8: this is not valid toml         <- inner line 2 (error)
        //   9: """
        // outer_line_offset for segment 0 is 7 (line after the fence),
        // inner_line is 2, mapped → 7 + 2 - 1 = 8.
        assert_eq!(d.line, Some(8), "diag: {d:?}");
    }

    #[test]
    fn validate_two_segments_only_one_invalid_inner() {
        let cfg = r#"
[[statusbar.segment]]
zone = "left"
command = ["echo", "first"]
starship_config = """
[character]
success_symbol = "❯"
"""

[[statusbar.segment]]
zone = "right"
command = ["echo", "second"]
starship_config = """
not = ok
not = ok
"""
"#;
        let f = write_tmp(cfg);
        let diags = validate(f.path()).unwrap();
        assert_eq!(diags.len(), 1);
        assert!(
            diags[0]
                .context
                .contains("statusbar.segment[1].starship_config")
        );
    }

    #[test]
    fn print_diagnostics_returns_correct_exit_code() {
        let f = write_tmp("");
        let path = f.path();
        let none: Vec<Diagnostic> = Vec::new();
        assert_eq!(print_diagnostics(&none, path), 0);

        let one = vec![Diagnostic {
            file: path.to_path_buf(),
            line: Some(1),
            column: Some(1),
            context: String::new(),
            message: "fake".to_string(),
        }];
        assert_eq!(print_diagnostics(&one, path), 1);
    }

    #[test]
    fn byte_to_line_col_basic() {
        let s = "hello\nworld\n!";
        assert_eq!(byte_to_line_col(s, 0), (1, 1));
        assert_eq!(byte_to_line_col(s, 5), (1, 6));
        assert_eq!(byte_to_line_col(s, 6), (2, 1));
        assert_eq!(byte_to_line_col(s, 12), (3, 1));
    }

    #[test]
    fn find_starship_block_handles_multiple_segments() {
        let cfg = "\n[[statusbar.segment]]\nstarship_config = \"\"\"\nx\n\"\"\"\n\n[[statusbar.segment]]\nstarship_config = \"\"\"\ny\n\"\"\"\n";
        // First segment opens on line 3 → content line 4
        assert_eq!(find_starship_config_start_line(cfg, 0), Some(4));
        // Second segment opens on line 8 → content line 9
        assert_eq!(find_starship_config_start_line(cfg, 1), Some(9));
        // Out of range
        assert_eq!(find_starship_config_start_line(cfg, 2), None);
    }

    /// Regression for codex P2 finding: when an EARLIER segment lacks
    /// `starship_config`, the per-occurrence counting heuristic would
    /// either point at the wrong segment or return None spuriously. The
    /// fixed implementation tracks segment index by counting
    /// `[[statusbar.segment]]` headers, so it correctly resolves the
    /// later segment's block.
    #[test]
    fn find_starship_block_skips_starship_less_segments() {
        let cfg = "\
[[statusbar.segment]]
zone = \"left\"
command = [\"echo\", \"first\"]

[[statusbar.segment]]
zone = \"right\"
command = [\"starship\", \"prompt\"]
starship_config = \"\"\"
[character]
\"\"\"
";
        // Segment 0 has no inline starship — should be None.
        assert_eq!(find_starship_config_start_line(cfg, 0), None);
        // Segment 1's `"""` opens on line 8 → content starts at line 9.
        assert_eq!(find_starship_config_start_line(cfg, 1), Some(9));
    }

    /// Validator must reject typo'd top-level sections, e.g. the user
    /// types `[appearence]` instead of `[appearance]`. The lenient
    /// runtime parser silently ignores such keys, so the validator's
    /// strict mirror is the only thing that catches it.
    #[test]
    fn validate_unknown_top_level_section_is_rejected() {
        let f = write_tmp("[appearence]\nborder_style = \"thick\"\n");
        let diags = validate(f.path()).unwrap();
        assert_eq!(diags.len(), 1, "expected one diagnostic");
        assert!(
            diags[0].message.contains("appearence"),
            "diag message should name the offending key: {}",
            diags[0].message
        );
    }

    /// Same as above but for a typo'd field inside a known section
    /// (`borderstyle` missing the underscore). Without
    /// `deny_unknown_fields` this would parse cleanly and the daemon
    /// would silently use the default border style.
    #[test]
    fn validate_unknown_field_in_section_is_rejected() {
        let f = write_tmp("[appearance]\nborderstyle = \"thick\"\n");
        let diags = validate(f.path()).unwrap();
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("borderstyle"));
    }
}
