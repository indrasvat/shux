//! Declarative workspace template parsing for `shux apply` (PR 3a, task 030).
//!
//! Templates use the PRD §10.3 shape:
//!
//! ```toml
//! [session]
//! name = "agent-conductor"           # optional → auto-generated
//! cwd  = "~/code/my-project"
//!
//! [[windows]]
//! title = "editor"
//!
//! [[windows.panes]]
//! command = ["nvim", "src/main.rs"]
//!
//! [[windows.panes]]
//! direction = "vertical"             # split off the prior pane in this window
//! ratio = 0.4
//! command = ["bash"]
//!
//! [[windows]]
//! title = "agent-1"
//!
//! [[windows.panes]]
//! command = ["claude", "-p", "refactor auth"]
//! ```
//!
//! The first pane in each window becomes that window's initial pane via
//! `Op::CreateWindow { initial_command }`. Subsequent panes lower to
//! `Op::SplitPane` against the prior pane (back-referenced positionally).
//!
//! Codex P1 #7: env handling uses explicit modes only — no magic
//! `$VAR` interpolation. PR 3a does not implement env in the lowered ops
//! yet (it's queued for a follow-up); attempting to set env in a template
//! returns a clear error rather than silently leaking secrets via dry-run
//! output or event history.

use std::path::PathBuf;

use serde::Deserialize;
use shux_core::apply::{Op, PaneRef, SessionRef};
use shux_core::layout::Direction;

/// Top-level template document. Field names match PRD §10.3.
#[derive(Debug, Deserialize)]
pub struct Template {
    /// Session block (required — every template defines exactly one session).
    pub session: SessionTpl,
    /// Window definitions in display order.
    #[serde(default)]
    pub windows: Vec<WindowTpl>,
}

#[derive(Debug, Deserialize)]
pub struct SessionTpl {
    /// Session name. None → daemon auto-generates `session-N`.
    #[serde(default)]
    pub name: Option<String>,
    /// Default cwd for the initial pane (and inherited by windows that
    /// don't override).
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WindowTpl {
    /// Window title. Required.
    pub title: String,
    /// Optional cwd override for this window's panes.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Panes in this window. The FIRST pane becomes the window's initial
    /// pane (created with the window). Subsequent panes lower to splits.
    #[serde(default)]
    pub panes: Vec<PaneTpl>,
}

#[derive(Debug, Deserialize)]
pub struct PaneTpl {
    /// Command to run. Empty → default shell at PTY spawn.
    #[serde(default)]
    pub command: Vec<String>,
    /// Optional cwd for THIS pane (overrides window/session cwd).
    #[serde(default)]
    pub cwd: Option<String>,
    /// Split direction relative to the prior pane in the same window.
    /// Ignored on the first pane (which is the window's initial pane).
    /// Accepts "horizontal"/"vertical".
    #[serde(default)]
    pub direction: Option<Direction>,
    /// Split ratio in (0.0, 1.0). Default 0.5. Ignored on the first pane.
    #[serde(default = "default_ratio")]
    pub ratio: f32,
}

fn default_ratio() -> f32 {
    0.5
}

/// Errors raised while parsing or lowering a template to apply ops.
#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    #[error("template file read error: {0}")]
    Io(#[from] std::io::Error),
    #[error("template TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("template must define at least one window")]
    NoWindows,
    #[error("window {window_index} ({title:?}) must define at least one pane")]
    NoPanes { window_index: usize, title: String },
}

/// Read a template TOML file and lower it to a Vec of apply ops, ready to
/// ship as `state.apply { ops: [...] }`.
pub fn load_and_lower(path: &std::path::Path) -> Result<Vec<Op>, TemplateError> {
    let text = std::fs::read_to_string(path)?;
    let tpl: Template = toml::from_str(&text)?;
    lower(tpl)
}

/// Lower an in-memory template to the apply ops vector.
///
/// Op numbering (used for back-references):
///   - op[0] = CreateSession (the implicit first window + first pane of the
///     first template window inherit from this op).
///   - op[1] = CreateWindow for windows[0] (skipped: windows[0] IS the
///     session's initial window — we just need to retitle it; for PR 3a we
///     leave it titled "1" and emit additional CreateWindow ops only for
///     windows[1..]).
///
/// For PR 3a the simplest correct lowering: every window in the template
/// becomes its own CreateWindow op (the session's auto-created "1" window
/// is left in place; agents typically don't care). Pane semantics:
///   - First pane of each window: lowered into the window's initial pane
///     via `CreateWindow { initial_command }`.
///   - Subsequent panes: SplitPane against the back-ref to the prior pane
///     in the same window (back-ref to that window's CreateWindow op or
///     prior SplitPane op).
fn lower(tpl: Template) -> Result<Vec<Op>, TemplateError> {
    if tpl.windows.is_empty() {
        return Err(TemplateError::NoWindows);
    }
    for (i, w) in tpl.windows.iter().enumerate() {
        if w.panes.is_empty() {
            return Err(TemplateError::NoPanes {
                window_index: i,
                title: w.title.clone(),
            });
        }
    }

    let mut ops: Vec<Op> = Vec::new();
    let session_cwd = expand_cwd(tpl.session.cwd.as_deref());

    // Fold `windows[0]` into the CreateSession op: the session's
    // auto-created initial window inherits the user's first-window title
    // and its initial pane runs the user's first command. Closes the
    // pre-existing apply gap where `shux apply foo.toml` ended up creating
    // a phantom default-shell auto-window AND the user's first window
    // alongside it.
    let first_window = &tpl.windows[0];
    let first_pane = &first_window.panes[0];
    let window0_cwd_fallback = first_window
        .cwd
        .as_deref()
        .map(expand_string)
        .unwrap_or_else(|| session_cwd.clone());
    let first_pane_cwd = first_pane
        .cwd
        .as_deref()
        .map(expand_string)
        .unwrap_or_else(|| window0_cwd_fallback.clone());
    ops.push(Op::CreateSession {
        name: tpl.session.name.clone(),
        cwd: first_pane_cwd,
        initial_command: first_pane.command.clone(),
        initial_window_title: Some(first_window.title.clone()),
    });

    // windows[0].panes[1..]: split off the session's initial pane (op 0).
    let mut prior_pane_op = 0usize;
    for pane in first_window.panes.iter().skip(1) {
        let split_op_index = ops.len();
        ops.push(Op::SplitPane {
            target: PaneRef::BackRef {
                op_index: prior_pane_op,
            },
            direction: pane.direction.unwrap_or(Direction::Vertical),
            ratio: pane.ratio,
            command: pane.command.clone(),
            cwd: pane.cwd.as_deref().map(expand_string),
        });
        prior_pane_op = split_op_index;
    }

    // windows[1..]: emit CreateWindow + chained SplitPane as before.
    for window in tpl.windows.iter().skip(1) {
        let window_cwd = window
            .cwd
            .as_deref()
            .map(expand_string)
            .unwrap_or_else(|| session_cwd.clone());

        let first_pane = &window.panes[0];
        let first_pane_cwd = first_pane.cwd.as_deref().map(expand_string);

        let create_window_index = ops.len();
        ops.push(Op::CreateWindow {
            session: SessionRef::BackRef { op_index: 0 },
            title: window.title.clone(),
            cwd: first_pane_cwd.or(Some(window_cwd.clone())),
            initial_command: first_pane.command.clone(),
        });

        let mut prior_pane_op = create_window_index;
        for pane in window.panes.iter().skip(1) {
            let split_op_index = ops.len();
            ops.push(Op::SplitPane {
                target: PaneRef::BackRef {
                    op_index: prior_pane_op,
                },
                direction: pane.direction.unwrap_or(Direction::Vertical),
                ratio: pane.ratio,
                command: pane.command.clone(),
                // Codex review of PR #10: thread the per-pane cwd through to
                // the SplitPane op. Previously it was silently dropped and
                // stage_split_pane fell back to the target pane's cwd, so
                // a user-supplied `cwd` on a non-first pane was ignored.
                cwd: pane.cwd.as_deref().map(expand_string),
            });
            prior_pane_op = split_op_index;
        }
    }

    Ok(ops)
}

/// Expand `~` to the user's home dir. No env-var interpolation
/// (codex P1 #7: explicit env modes only; cwd doesn't need them).
fn expand_string(s: &str) -> PathBuf {
    if let Some(stripped) = s.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(stripped);
    }
    if s == "~"
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home);
    }
    PathBuf::from(s)
}

fn expand_cwd(opt: Option<&str>) -> PathBuf {
    match opt {
        Some(s) => expand_string(s),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lower_minimal_template() {
        // After folding windows[0] into CreateSession, a single-window
        // single-pane template lowers to ONE op (no phantom auto-window).
        let tpl: Template = toml::from_str(
            r#"
[session]
name = "ws"

[[windows]]
title = "editor"
[[windows.panes]]
command = ["nvim"]
"#,
        )
        .unwrap();
        let ops = lower(tpl).unwrap();
        assert_eq!(
            ops.len(),
            1,
            "minimal template should fold to a single CreateSession op"
        );
        match &ops[0] {
            Op::CreateSession {
                name,
                initial_command,
                initial_window_title,
                ..
            } => {
                assert_eq!(name.as_deref(), Some("ws"));
                assert_eq!(initial_command, &vec!["nvim".to_string()]);
                assert_eq!(initial_window_title.as_deref(), Some("editor"));
            }
            _ => panic!("op 0 should be CreateSession"),
        }
    }

    #[test]
    fn test_lower_window_with_split() {
        // windows[0] folds into CreateSession; the second pane in the same
        // window is a SplitPane against op[0].
        let tpl: Template = toml::from_str(
            r#"
[session]
name = "agent-conductor"

[[windows]]
title = "editor"
[[windows.panes]]
command = ["nvim"]
[[windows.panes]]
direction = "vertical"
ratio = 0.4
command = ["bash"]
"#,
        )
        .unwrap();
        let ops = lower(tpl).unwrap();
        assert_eq!(
            ops.len(),
            2,
            "single window + 2 panes lowers to CreateSession + SplitPane"
        );

        match &ops[1] {
            Op::SplitPane {
                target,
                direction,
                ratio,
                command,
                cwd: _,
            } => {
                assert!(matches!(target, PaneRef::BackRef { op_index: 0 }));
                assert_eq!(*direction, Direction::Vertical);
                assert!((ratio - 0.4).abs() < 1e-6);
                assert_eq!(command, &vec!["bash".to_string()]);
            }
            _ => panic!("op 1 should be SplitPane"),
        }
    }

    #[test]
    fn test_lower_three_window_workspace() {
        // windows[0] folds into CreateSession; windows[1..] still emit
        // CreateWindow with their titles preserved.
        let tpl: Template = toml::from_str(
            r#"
[session]
name = "swarm"
cwd = "~/code/x"

[[windows]]
title = "editor"
[[windows.panes]]
command = ["nvim"]

[[windows]]
title = "agent-1"
[[windows.panes]]
command = ["claude"]

[[windows]]
title = "agent-2"
[[windows.panes]]
command = ["codex"]
"#,
        )
        .unwrap();
        let ops = lower(tpl).unwrap();
        assert_eq!(
            ops.len(),
            3,
            "session + 2 follow-up windows (windows[0] folded)"
        );
        match &ops[0] {
            Op::CreateSession {
                initial_command,
                initial_window_title,
                ..
            } => {
                assert_eq!(initial_command, &vec!["nvim".to_string()]);
                assert_eq!(initial_window_title.as_deref(), Some("editor"));
            }
            _ => panic!("op 0 should be CreateSession"),
        }
        match &ops[1] {
            Op::CreateWindow {
                title,
                initial_command,
                ..
            } => {
                assert_eq!(title, "agent-1");
                assert_eq!(initial_command, &vec!["claude".to_string()]);
            }
            _ => panic!("op 1 should be CreateWindow for agent-1"),
        }
        match &ops[2] {
            Op::CreateWindow { title, .. } => assert_eq!(title, "agent-2"),
            _ => panic!("op 2 should be CreateWindow for agent-2"),
        }
    }

    #[test]
    fn test_lower_rejects_no_windows() {
        let tpl: Template = toml::from_str(r#"[session]"#).unwrap();
        let r = lower(tpl);
        assert!(matches!(r, Err(TemplateError::NoWindows)));
    }

    /// Codex review of PR #10: a per-pane `cwd` on a non-first pane was
    /// being silently dropped during lowering. Verify it now threads through
    /// to `Op::SplitPane.cwd`.
    #[test]
    fn test_lower_threads_split_pane_cwd() {
        let tpl: Template = toml::from_str(
            r#"
[session]
name = "cwd-test"

[[windows]]
title = "editor"
[[windows.panes]]
command = ["bash"]
[[windows.panes]]
direction = "vertical"
cwd = "/tmp/explicit-split"
command = ["bash"]
"#,
        )
        .unwrap();
        let ops = lower(tpl).unwrap();
        // windows[0] folds into CreateSession, so the second pane is op[1].
        assert_eq!(ops.len(), 2);
        match &ops[1] {
            Op::SplitPane { cwd, .. } => {
                assert_eq!(
                    cwd.as_deref().map(|p| p.to_string_lossy().to_string()),
                    Some("/tmp/explicit-split".to_string()),
                    "split-pane cwd must be threaded through to the op"
                );
            }
            _ => panic!("op 1 should be SplitPane"),
        }
    }

    #[test]
    fn test_lower_rejects_window_with_no_panes() {
        let tpl: Template = toml::from_str(
            r#"
[session]
[[windows]]
title = "empty"
"#,
        )
        .unwrap();
        let r = lower(tpl);
        assert!(matches!(r, Err(TemplateError::NoPanes { .. })));
    }
}
