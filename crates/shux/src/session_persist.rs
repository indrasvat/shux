//! Session save/restore helpers.
//!
//! The first persistence slice exports a live session into the existing
//! template TOML shape consumed by `shux state apply`. It intentionally
//! keeps restore explicit: users decide when to run the saved commands.

use serde::Serialize;
use shux_core::graph::SessionGraphSnapshot;
use shux_core::layout::{Direction, LayoutNode};
use shux_core::model::{Pane, SessionId};

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("session not found: {0}")]
    SessionNotFound(String),
    #[error("window not found while exporting session: {0}")]
    WindowNotFound(String),
    #[error("pane not found while exporting session: {0}")]
    PaneNotFound(String),
    #[error("template serialization failed: {0}")]
    Serialize(#[from] toml::ser::Error),
}

#[derive(Debug, Serialize)]
struct ExportTemplate {
    session: ExportSession,
    windows: Vec<ExportWindow>,
}

#[derive(Debug, Serialize)]
struct ExportSession {
    name: String,
    cwd: String,
}

#[derive(Debug, Serialize)]
struct ExportWindow {
    title: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    panes: Vec<ExportPane>,
}

#[derive(Debug, Serialize)]
struct ExportPane {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    command: Vec<String>,
    cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    direction: Option<Direction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ratio: Option<f64>,
}

pub fn export_session_template(
    snap: &SessionGraphSnapshot,
    session_id: SessionId,
) -> Result<String, ExportError> {
    let session = snap
        .sessions
        .get(&session_id)
        .ok_or_else(|| ExportError::SessionNotFound(session_id.to_string()))?;
    let first_pane = session
        .windows
        .iter()
        .filter_map(|wid| snap.windows.get(wid))
        .find_map(|w| snap.panes.get(&w.active_pane))
        .map(|p| p.cwd.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());

    let mut windows = Vec::new();
    for wid in &session.windows {
        let window = snap
            .windows
            .get(wid)
            .ok_or_else(|| ExportError::WindowNotFound(wid.to_string()))?;
        let mut panes = Vec::new();
        flatten_layout(&window.layout.tree, snap, None, &mut panes)?;
        windows.push(ExportWindow {
            title: window.title.clone(),
            panes,
        });
    }

    let tpl = ExportTemplate {
        session: ExportSession {
            name: session.name.clone(),
            cwd: first_pane,
        },
        windows,
    };
    Ok(toml::to_string_pretty(&tpl)?)
}

fn flatten_layout(
    node: &LayoutNode,
    snap: &SessionGraphSnapshot,
    split_from_parent: Option<(Direction, f32)>,
    out: &mut Vec<ExportPane>,
) -> Result<(), ExportError> {
    match node {
        LayoutNode::Leaf { pane } => {
            let pane = snap
                .panes
                .get(pane)
                .ok_or_else(|| ExportError::PaneNotFound(pane.to_string()))?;
            out.push(export_pane(pane, split_from_parent));
        }
        LayoutNode::Split { dir, ratio, a, b } => {
            flatten_layout(a, snap, split_from_parent, out)?;
            flatten_layout(b, snap, Some((*dir, *ratio)), out)?;
        }
    }
    Ok(())
}

fn export_pane(pane: &Pane, split: Option<(Direction, f32)>) -> ExportPane {
    let (direction, ratio) = split
        .map(|(dir, ratio)| (Some(dir), Some(round_ratio(ratio))))
        .unwrap_or((None, None));
    ExportPane {
        command: pane.command.clone(),
        cwd: pane.cwd.to_string_lossy().to_string(),
        direction,
        ratio,
    }
}

fn round_ratio(ratio: f32) -> f64 {
    ((ratio as f64) * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use super::*;
    use shux_core::layout::Direction;
    use shux_core::model::{PaneId, Session, Window, WindowId};

    struct PaneSpec {
        cwd: &'static str,
        command: Vec<String>,
        split: Option<(Direction, f32)>,
    }

    #[test]
    fn export_single_session_template_contains_window_and_command() {
        let (snap, sid) = snapshot_with_panes(
            "save-me",
            "main",
            vec![PaneSpec {
                cwd: "/tmp/demo",
                command: vec!["bash".into(), "-lc".into(), "echo hi".into()],
                split: None,
            }],
        );
        let text = export_session_template(&snap, sid).unwrap();
        assert!(text.contains("name = \"save-me\""));
        assert!(text.contains("command = ["));
        assert!(text.contains("\"bash\""));
        assert!(text.contains("\"echo hi\""));
    }

    #[test]
    fn export_split_layout_includes_direction_and_ratio() {
        let (snap, sid) = snapshot_with_panes(
            "split",
            "main",
            vec![
                PaneSpec {
                    cwd: "/tmp",
                    command: vec!["top".into()],
                    split: None,
                },
                PaneSpec {
                    cwd: "/tmp",
                    command: vec!["tail".into(), "-f".into(), "log".into()],
                    split: Some((Direction::Vertical, 0.4)),
                },
            ],
        );
        let text = export_session_template(&snap, sid).unwrap();
        assert!(text.contains("direction = \"vertical\""));
        assert!(text.contains("ratio = 0.4"));
    }

    fn snapshot_with_panes(
        session_name: &str,
        window_title: &str,
        specs: Vec<PaneSpec>,
    ) -> (SessionGraphSnapshot, SessionId) {
        let window_id = WindowId::new();
        let pane_ids: Vec<_> = specs.iter().map(|_| PaneId::new()).collect();
        let session = Session::new(session_name, window_id);
        let mut window = Window::new(session.id, window_title, pane_ids[0]);
        window.id = window_id;
        window.layout.tree = layout_for(&pane_ids, &specs);

        let panes = specs
            .into_iter()
            .zip(pane_ids)
            .map(|(spec, pane_id)| {
                let mut pane = Pane::with_command(window_id, PathBuf::from(spec.cwd), spec.command);
                pane.id = pane_id;
                (pane_id, pane)
            })
            .collect();

        let sid = session.id;
        let mut sessions = HashMap::new();
        sessions.insert(sid, session);
        let mut windows = HashMap::new();
        windows.insert(window_id, window);
        (
            SessionGraphSnapshot {
                sessions,
                windows,
                panes,
                version: 1,
            },
            sid,
        )
    }

    fn layout_for(pane_ids: &[PaneId], specs: &[PaneSpec]) -> LayoutNode {
        let mut layout = LayoutNode::leaf(pane_ids[0]);
        for (idx, spec) in specs.iter().enumerate().skip(1) {
            let (dir, ratio) = spec
                .split
                .expect("non-initial pane must include split metadata");
            layout = LayoutNode::split(dir, ratio, layout, LayoutNode::leaf(pane_ids[idx]));
        }
        layout
    }
}
