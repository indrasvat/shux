//! `shux config validate` — line-numbered diagnostics for the user
//! `~/.config/shux/config.toml` plus every inline `starship_config`
//! string nested under `[[statusbar.segment]]`.
//!
//! Exit code is 0 when the file is empty / valid, 1 when at least one
//! diagnostic was emitted. The CLI dispatcher in `main.rs` translates
//! that into `std::process::exit(1)` so shell pipelines can branch on
//! validation status.

use std::path::{Path, PathBuf};

use shux_core::config::Config;

use crate::style;

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

/// Locate the `starship_config = """..."""` block for `segment_index`
/// inside the outer config text. Returns the 1-based line on which the
/// triple-quoted string body STARTS — i.e. the line after the opening
/// `"""`. Used to lift inner-TOML diagnostics back into outer file
/// coordinates.
///
/// This is a deliberately small heuristic: a regex/scanner of `"""`
/// borders rather than a full TOML round-trip. Two consequences:
///   * It assumes `starship_config = """ ... """` literal syntax (the
///     style `shux config init` writes). Single-quote / non-multiline
///     forms return `None`.
///   * If the user reorders segments, indices still match because we
///     count occurrences in document order.
fn find_starship_config_start_line(content: &str, segment_index: usize) -> Option<usize> {
    let mut count = 0usize;
    for (line_idx, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("starship_config")
            && let Some(eq_pos) = trimmed.find('=')
        {
            let after = trimmed[eq_pos + 1..].trim_start();
            if after.starts_with("\"\"\"") {
                if count == segment_index {
                    // The string's first content line is the line AFTER
                    // the opening `"""`. lines() is 0-based, errors are
                    // 1-based, so add 2.
                    return Some(line_idx + 2);
                }
                count += 1;
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

    // ----- Stage 1: parse the outer config -----
    match toml::from_str::<Config>(&content) {
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
}
