//! The on-disk workspace (issue #329): what a daemon writes so the next one can
//! rebuild its sessions, windows and panes after a restart.
//!
//! The format is its own type, not [`SessionGraphSnapshot`], so the graph can
//! change shape between releases without making an upgrade unreadable. Layout
//! trees are stored as flat node tables: a recursive encoding would hit the JSON
//! decoder's depth limit long before the pane cap.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::graph::{MAX_PANES_PER_SESSION, SessionGraphSnapshot};
use crate::layout::{Direction, LayoutNode, MAX_RATIO, MIN_RATIO, WindowLayout, ZoomState};
use crate::model::{
    Pane, PaneId, Session, SessionId, Window, WindowId, sanitize_title, sanitize_title_clamped,
};

pub const SCHEMA: u32 = 1;
/// Checked on the file's size before reading it.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_TOTAL_PANES: usize = 4 * MAX_PANES_PER_SESSION;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    pub schema: u32,
    /// The socket this workspace belongs to; a file found under another
    /// socket's key is refused.
    pub socket: String,
    pub written_by: String,
    pub sessions: Vec<PSession>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PSession {
    pub id: SessionId,
    pub name: String,
    pub created_seq: u64,
    pub created_at_ms: u64,
    pub active_window: WindowId,
    pub windows: Vec<PWindow>,
    pub tags: BTreeMap<String, String>,
    pub theme: Option<String>,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PWindow {
    pub id: WindowId,
    pub title: String,
    pub active_pane: PaneId,
    /// The unzoomed tree; `root` indexes into it.
    pub nodes: Vec<PNode>,
    pub root: u32,
    pub zoomed: Option<PaneId>,
    pub panes: Vec<PPane>,
    pub tags: BTreeMap<String, String>,
    pub theme: Option<String>,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase", deny_unknown_fields)]
pub enum PNode {
    Split {
        dir: Direction,
        ratio: f32,
        a: u32,
        b: u32,
    },
    Leaf {
        pane: PaneId,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PPane {
    pub id: PaneId,
    pub manual_title: Option<String>,
    /// The title a pane keeps with auto-title off and no manual title.
    pub pinned_title: Option<String>,
    pub auto_title: bool,
    pub spawn_cwd: PathRepr,
    pub last_cwd: Option<PathRepr>,
    /// The shell that ran in the pane, so the next daemon can reap it if a
    /// `kill -9` left it alive.
    pub process: Option<ProcessId>,
    pub tags: BTreeMap<String, String>,
    pub theme: Option<String>,
    pub version: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessId {
    pub pid: i32,
    /// The process's start time, so a reused pid is never taken for it.
    pub start_token: u64,
}

/// A path, byte for byte: UTF-8 as a string, anything else as hex.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PathRepr {
    Utf8(String),
    Bytes { hex: String },
}

impl PathRepr {
    pub fn from_path(path: &std::path::Path) -> Self {
        use std::os::unix::ffi::OsStrExt;
        match path.to_str() {
            Some(s) => Self::Utf8(s.to_string()),
            None => Self::Bytes {
                hex: path
                    .as_os_str()
                    .as_bytes()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect(),
            },
        }
    }

    pub fn to_path(&self) -> Option<PathBuf> {
        use std::os::unix::ffi::OsStringExt;
        match self {
            Self::Utf8(s) => Some(PathBuf::from(s)),
            Self::Bytes { hex } => {
                if hex.len() % 2 != 0 {
                    return None;
                }
                let bytes = (0..hex.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
                    .collect::<Option<Vec<u8>>>()?;
                Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
            }
        }
    }
}

/// What the graph does not hold about a pane but a workspace records.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PaneExtras {
    pub last_cwd: Option<PathBuf>,
    pub process: Option<ProcessId>,
}

/// A restored graph plus what its panes need to start again.
#[derive(Debug, Clone)]
pub struct Restored {
    pub snapshot: SessionGraphSnapshot,
    pub extras: HashMap<PaneId, PaneExtras>,
    pub max_created_seq: u64,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RestoreError {
    #[error("the workspace file is {0} bytes, over the {MAX_FILE_BYTES}-byte limit")]
    TooLarge(u64),
    #[error("the workspace file is not valid: {0}")]
    Decode(String),
    #[error("the workspace uses schema {found}; this build reads schema {SCHEMA}")]
    FutureSchema { found: u32 },
    #[error("the workspace belongs to socket {found}, not {expected}")]
    WrongSocket { found: String, expected: String },
    #[error("the workspace is inconsistent: {0}")]
    Invalid(String),
}

fn invalid(msg: impl Into<String>) -> RestoreError {
    RestoreError::Invalid(msg.into())
}

fn millis(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn sorted(tags: &HashMap<String, String>) -> BTreeMap<String, String> {
    tags.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

/// Flatten `tree` into `nodes`, returning the root's index. Iterative: a
/// legal tree can be 1023 splits deep.
pub fn flatten(tree: &LayoutNode, nodes: &mut Vec<PNode>) -> u32 {
    let placeholder = || PNode::Leaf {
        pane: PaneId::from_uuid(uuid::Uuid::nil()),
    };
    let root = nodes.len();
    nodes.push(placeholder());
    let mut stack = vec![(tree, root)];
    while let Some((node, slot)) = stack.pop() {
        nodes[slot] = match node {
            LayoutNode::Leaf { pane } => PNode::Leaf { pane: *pane },
            LayoutNode::Split { dir, ratio, a, b } => {
                let (ia, ib) = (nodes.len(), nodes.len() + 1);
                nodes.push(placeholder());
                nodes.push(placeholder());
                stack.push((b, ib));
                stack.push((a, ia));
                PNode::Split {
                    dir: *dir,
                    ratio: *ratio,
                    a: ia as u32,
                    b: ib as u32,
                }
            }
        };
    }
    root as u32
}

/// Capture `snapshot` as a workspace, leaving out scratch sessions. Output is
/// canonical: the same graph always gives the same bytes.
pub fn capture(
    snapshot: &SessionGraphSnapshot,
    socket: &str,
    extras: &dyn Fn(PaneId) -> PaneExtras,
) -> Workspace {
    let mut sessions: Vec<&Session> = snapshot.sessions.values().filter(|s| !s.scratch).collect();
    sessions.sort_by_key(|s| (s.created_seq, s.id.0));
    let sessions = sessions
        .into_iter()
        .map(|s| PSession {
            id: s.id,
            name: s.name.clone(),
            created_seq: s.created_seq,
            created_at_ms: millis(s.created_at),
            active_window: s.active_window,
            windows: s
                .windows
                .iter()
                .filter_map(|wid| snapshot.windows.get(wid))
                .map(|w| capture_window(snapshot, w, extras))
                .collect(),
            tags: sorted(&s.tags),
            theme: s.theme.clone(),
            version: s.version,
        })
        .collect();
    Workspace {
        schema: SCHEMA,
        socket: socket.to_string(),
        written_by: env!("CARGO_PKG_VERSION").to_string(),
        sessions,
    }
}

fn capture_window(
    snapshot: &SessionGraphSnapshot,
    w: &Window,
    extras: &dyn Fn(PaneId) -> PaneExtras,
) -> PWindow {
    let (tree, zoomed) = match &w.layout.zoom {
        Some(z) => (&z.saved_layout, Some(z.zoomed_pane)),
        None => (&w.layout.tree, None),
    };
    let mut nodes = Vec::new();
    let root = flatten(tree, &mut nodes);
    let panes = tree
        .pane_ids()
        .into_iter()
        .filter_map(|pid| snapshot.panes.get(&pid))
        .map(|p| capture_pane(p, extras(p.id)))
        .collect();
    PWindow {
        id: w.id,
        title: w.title.clone(),
        active_pane: w.active_pane,
        nodes,
        root,
        zoomed,
        panes,
        tags: sorted(&w.tags),
        theme: w.theme.clone(),
        version: w.version,
    }
}

fn capture_pane(p: &Pane, extras: PaneExtras) -> PPane {
    PPane {
        id: p.id,
        manual_title: p.manual_title.clone(),
        pinned_title: (!p.auto_title && p.manual_title.is_none()).then(|| p.title.clone()),
        auto_title: p.auto_title,
        spawn_cwd: PathRepr::from_path(&p.cwd),
        last_cwd: extras.last_cwd.as_deref().map(PathRepr::from_path),
        process: extras.process,
        tags: sorted(&p.tags),
        theme: p.theme.clone(),
        version: p.version,
    }
}

/// Canonical bytes for `ws`.
pub fn encode(ws: &Workspace) -> Vec<u8> {
    // Serializing these types cannot fail: no maps with non-string keys, and
    // non-finite ratios never enter a layout (they are clamped on the way in).
    serde_json::to_vec(ws).unwrap_or_default()
}

/// Parse and fully validate a workspace read from disk.
pub fn decode(bytes: &[u8], expected_socket: &str) -> Result<Workspace, RestoreError> {
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(RestoreError::TooLarge(bytes.len() as u64));
    }
    // Read the schema first so a newer file is reported as newer, not as
    // malformed under this build's rules.
    #[derive(Deserialize)]
    struct Header {
        schema: u32,
    }
    let header: Header =
        serde_json::from_slice(bytes).map_err(|e| RestoreError::Decode(e.to_string()))?;
    if header.schema > SCHEMA {
        return Err(RestoreError::FutureSchema {
            found: header.schema,
        });
    }
    if header.schema != SCHEMA {
        return Err(RestoreError::Decode(format!(
            "unknown schema {}",
            header.schema
        )));
    }
    let ws: Workspace =
        serde_json::from_slice(bytes).map_err(|e| RestoreError::Decode(e.to_string()))?;
    if ws.socket != expected_socket {
        return Err(RestoreError::WrongSocket {
            found: ws.socket,
            expected: expected_socket.to_string(),
        });
    }
    validate(&ws)?;
    Ok(ws)
}

/// Every rule a workspace must meet before any of it is installed.
pub fn validate(ws: &Workspace) -> Result<(), RestoreError> {
    let mut ids: HashSet<uuid::Uuid> = HashSet::new();
    let mut names: HashSet<&str> = HashSet::new();
    let mut total_panes = 0usize;
    for s in &ws.sessions {
        if !ids.insert(s.id.0) {
            return Err(invalid(format!("duplicate id {}", s.id)));
        }
        if s.name.is_empty() || !names.insert(&s.name) {
            return Err(invalid(format!(
                "session name {:?} is empty or repeated",
                s.name
            )));
        }
        crate::graph::SessionGraph::validate_session_name(&s.name)
            .map_err(|e| invalid(format!("session {:?}: {e}", s.name)))?;
        if s.created_seq > u64::MAX - MAX_TOTAL_PANES as u64 {
            return Err(invalid("session sequence out of range"));
        }
        if s.windows.is_empty() {
            return Err(invalid(format!("session {:?} has no window", s.name)));
        }
        if !s.windows.iter().any(|w| w.id == s.active_window) {
            return Err(invalid(format!(
                "session {:?}: active window is not one of its windows",
                s.name
            )));
        }
        let mut session_panes = 0usize;
        for w in &s.windows {
            if !ids.insert(w.id.0) {
                return Err(invalid(format!("duplicate id {}", w.id)));
            }
            crate::graph::SessionGraph::check_window_title(&w.title)
                .map_err(|e| invalid(format!("window {}: {e}", w.id)))?;
            let leaves = validate_tree(w)?;
            if leaves.len() != w.panes.len() {
                return Err(invalid(format!(
                    "window {}: {} leaves but {} panes",
                    w.id,
                    leaves.len(),
                    w.panes.len()
                )));
            }
            let mut listed = HashSet::new();
            for p in &w.panes {
                if !ids.insert(p.id.0) {
                    return Err(invalid(format!("duplicate id {}", p.id)));
                }
                listed.insert(p.id);
                if p.spawn_cwd.to_path().is_none()
                    || p.last_cwd.as_ref().is_some_and(|c| c.to_path().is_none())
                {
                    return Err(invalid(format!("pane {}: undecodable path", p.id)));
                }
            }
            if listed != leaves {
                return Err(invalid(format!(
                    "window {}: tree and pane list differ",
                    w.id
                )));
            }
            if !listed.contains(&w.active_pane) {
                return Err(invalid(format!(
                    "window {}: active pane is not in it",
                    w.id
                )));
            }
            if let Some(z) = w.zoomed
                && (w.panes.len() < 2 || !listed.contains(&z) || w.active_pane != z)
            {
                return Err(invalid(format!("window {}: inconsistent zoom", w.id)));
            }
            session_panes += w.panes.len();
        }
        if session_panes > MAX_PANES_PER_SESSION {
            return Err(invalid(format!(
                "session {:?} has {session_panes} panes",
                s.name
            )));
        }
        total_panes += session_panes;
    }
    if total_panes > MAX_TOTAL_PANES {
        return Err(invalid(format!("{total_panes} panes in total")));
    }
    Ok(())
}

/// Check a window's node table is one tree over its nodes, returning its leaves.
fn validate_tree(w: &PWindow) -> Result<HashSet<PaneId>, RestoreError> {
    let mut seen = vec![false; w.nodes.len()];
    let mut leaves = HashSet::new();
    let mut stack = vec![w.root as usize];
    while let Some(i) = stack.pop() {
        let node = w
            .nodes
            .get(i)
            .ok_or_else(|| invalid(format!("window {}: node {i} out of range", w.id)))?;
        if std::mem::replace(&mut seen[i], true) {
            return Err(invalid(format!("window {}: node {i} reached twice", w.id)));
        }
        match node {
            PNode::Leaf { pane } => {
                if !leaves.insert(*pane) {
                    return Err(invalid(format!(
                        "window {}: pane {pane} appears twice",
                        w.id
                    )));
                }
            }
            PNode::Split { ratio, a, b, .. } => {
                if !ratio.is_finite() || *ratio < MIN_RATIO || *ratio > MAX_RATIO {
                    return Err(invalid(format!("window {}: ratio {ratio}", w.id)));
                }
                stack.push(*b as usize);
                stack.push(*a as usize);
            }
        }
    }
    if seen.iter().any(|s| !s) {
        return Err(invalid(format!(
            "window {}: unreachable layout nodes",
            w.id
        )));
    }
    Ok(leaves)
}

/// Rebuild a window's tree. Only call on a validated window.
fn unflatten(w: &PWindow) -> LayoutNode {
    // Post-order without recursion: children are built before their parent.
    let mut built: Vec<Option<LayoutNode>> = vec![None; w.nodes.len()];
    let mut stack = vec![(w.root as usize, false)];
    while let Some((i, expanded)) = stack.pop() {
        match (&w.nodes[i], expanded) {
            (PNode::Leaf { pane }, _) => built[i] = Some(LayoutNode::leaf(*pane)),
            (PNode::Split { a, b, .. }, false) => {
                stack.push((i, true));
                stack.push((*b as usize, false));
                stack.push((*a as usize, false));
            }
            (PNode::Split { dir, ratio, a, b }, true) => {
                let a = built[*a as usize]
                    .take()
                    .unwrap_or(LayoutNode::leaf(PaneId::new()));
                let b = built[*b as usize]
                    .take()
                    .unwrap_or(LayoutNode::leaf(PaneId::new()));
                built[i] = Some(LayoutNode::Split {
                    dir: *dir,
                    ratio: *ratio,
                    a: Box::new(a),
                    b: Box::new(b),
                });
            }
        }
    }
    built[w.root as usize]
        .take()
        .unwrap_or(LayoutNode::leaf(PaneId::new()))
}

/// Build the graph a validated workspace describes. Every version is one past
/// the saved one, so a request carrying a version from before the restart is
/// refused rather than applied to the restored entity.
pub fn rebuild(ws: &Workspace) -> Restored {
    let mut snapshot = SessionGraphSnapshot::default();
    let mut extras = HashMap::new();
    let mut max_created_seq = 0;
    for s in &ws.sessions {
        max_created_seq = max_created_seq.max(s.created_seq);
        let mut session = Session::new(s.name.clone(), s.active_window);
        session.id = s.id;
        session.created_seq = s.created_seq;
        session.created_at = UNIX_EPOCH + Duration::from_millis(s.created_at_ms);
        session.windows = s.windows.iter().map(|w| w.id).collect();
        session.active_window = s.active_window;
        session.tags = s.tags.clone().into_iter().collect();
        session.theme = s.theme.clone();
        session.version = s.version.saturating_add(1);
        for w in &s.windows {
            let tree = unflatten(w);
            let layout = match w.zoomed {
                Some(z) => WindowLayout {
                    tree: LayoutNode::leaf(z),
                    zoom: Some(ZoomState {
                        saved_layout: tree,
                        zoomed_pane: z,
                    }),
                },
                None => WindowLayout { tree, zoom: None },
            };
            let mut window = Window::new(s.id, sanitize_title(&w.title), w.active_pane);
            window.id = w.id;
            window.layout = layout;
            window.tags = w.tags.clone().into_iter().collect();
            window.theme = w.theme.clone();
            window.version = w.version.saturating_add(1);
            for p in &w.panes {
                // Validated: both paths decode.
                let spawn_cwd = p.spawn_cwd.to_path().unwrap_or_default();
                let mut pane = Pane::new(w.id, spawn_cwd);
                pane.id = p.id;
                pane.auto_title = p.auto_title;
                if let Some(t) = &p.pinned_title {
                    pane.title = sanitize_title_clamped(t);
                }
                pane.set_manual_title(p.manual_title.clone());
                pane.tags = p.tags.clone().into_iter().collect();
                pane.theme = p.theme.clone();
                pane.version = p.version.saturating_add(1);
                extras.insert(
                    p.id,
                    PaneExtras {
                        last_cwd: p.last_cwd.as_ref().and_then(PathRepr::to_path),
                        process: p.process,
                    },
                );
                snapshot.panes.insert(p.id, pane);
            }
            snapshot.windows.insert(w.id, window);
        }
        snapshot.sessions.insert(s.id, session);
    }
    snapshot.version = 1;
    Restored {
        snapshot,
        extras,
        max_created_seq,
    }
}

/// One fuzz case: any bytes a daemon might find on disk. Decoding never
/// panics; a workspace it accepts rebuilds into a graph whose own workspace
/// decodes again, to the same sessions, windows and panes.
pub fn fuzz_one(bytes: &[u8]) {
    let Ok(ws) = decode(bytes, "/sock") else {
        return;
    };
    let restored = rebuild(&ws);
    let extras = |p: PaneId| restored.extras.get(&p).cloned().unwrap_or_default();
    let again = capture(&restored.snapshot, "/sock", &extras);
    let back = decode(&encode(&again), "/sock")
        .unwrap_or_else(|e| panic!("an accepted workspace rebuilt into one refused: {e}"));
    let ids = |w: &Workspace| -> Vec<uuid::Uuid> {
        let mut ids: Vec<_> =
            w.sessions
                .iter()
                .flat_map(|s| {
                    std::iter::once(s.id.0).chain(s.windows.iter().flat_map(|w| {
                        std::iter::once(w.id.0).chain(w.panes.iter().map(|p| p.id.0))
                    }))
                })
                .collect();
        ids.sort();
        ids
    };
    assert_eq!(
        ids(&back),
        ids(&ws),
        "rebuilding changed what the workspace holds"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::SessionGraph;
    use crate::layout::Rect;
    use proptest::prelude::*;
    use std::os::unix::ffi::OsStringExt;

    #[derive(Debug, Clone)]
    enum Op {
        Session(u8),
        Window(u8),
        Split(u8, bool, f32),
        Resize(u8, bool, f32),
        Zoom(u8),
        Title(u8, Option<u8>, Option<bool>),
        Osc(u8),
        Focus(u8),
        FocusWindow(u8),
        Reorder(u8, u8),
        Tag(u8, u8),
        Theme(u8),
        Kill(u8),
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            any::<u8>().prop_map(Op::Session),
            any::<u8>().prop_map(Op::Window),
            (any::<u8>(), any::<bool>(), 0.05f32..0.95).prop_map(|(p, d, r)| Op::Split(p, d, r)),
            (any::<u8>(), any::<bool>(), -0.4f32..0.4).prop_map(|(p, d, r)| Op::Resize(p, d, r)),
            any::<u8>().prop_map(Op::Zoom),
            (any::<u8>(), any::<Option<u8>>(), any::<Option<bool>>())
                .prop_map(|(p, t, a)| Op::Title(p, t, a)),
            any::<u8>().prop_map(Op::Osc),
            any::<u8>().prop_map(Op::Focus),
            any::<u8>().prop_map(Op::FocusWindow),
            (any::<u8>(), any::<u8>()).prop_map(|(w, i)| Op::Reorder(w, i)),
            (any::<u8>(), any::<u8>()).prop_map(|(e, v)| Op::Tag(e, v)),
            any::<u8>().prop_map(Op::Theme),
            any::<u8>().prop_map(Op::Kill),
        ]
    }

    fn dir(d: bool) -> Direction {
        if d {
            Direction::Horizontal
        } else {
            Direction::Vertical
        }
    }

    /// A cwd that is sometimes not UTF-8.
    fn cwd(n: u8) -> PathBuf {
        if n.is_multiple_of(5) {
            PathBuf::from(std::ffi::OsString::from_vec(vec![b'/', b't', 0xff, n]))
        } else {
            PathBuf::from(format!("/tmp/d{n}"))
        }
    }

    fn pick<T: Copy>(items: &[T], n: u8) -> Option<T> {
        (!items.is_empty()).then(|| items[n as usize % items.len()])
    }

    fn build(ops: &[Op]) -> SessionGraph {
        let (g, _) = SessionGraph::new();
        for op in ops {
            let s = g.current();
            let mut sessions: Vec<_> = s.sessions.keys().copied().collect();
            let mut windows: Vec<_> = s.windows.keys().copied().collect();
            let mut panes: Vec<_> = s.panes.keys().copied().collect();
            sessions.sort_by_key(|i| i.0);
            windows.sort_by_key(|i| i.0);
            panes.sort_by_key(|i| i.0);
            // Errors are part of the space: the graph refuses what it refuses.
            let _ = match op {
                Op::Session(n) => g
                    .create_session_with_command(format!("s{n}"), cwd(*n), vec![])
                    .map(drop),
                Op::Window(n) => pick(&sessions, *n).map_or(Ok(()), |sid| {
                    g.create_window_with_command(sid, format!("w{n}"), cwd(*n), vec![])
                        .map(drop)
                }),
                Op::Split(n, d, r) => {
                    pick(&panes, *n).map_or(Ok(()), |p| g.split_pane(p, dir(*d), *r).map(drop))
                }
                Op::Resize(n, d, r) => {
                    pick(&panes, *n).map_or(Ok(()), |p| g.resize_pane(p, dir(*d), *r, None))
                }
                Op::Zoom(n) => pick(&panes, *n).map_or(Ok(()), |p| g.zoom_pane(p, None).map(drop)),
                Op::Title(n, t, a) => pick(&panes, *n).map_or(Ok(()), |p| {
                    g.set_pane_title(p, t.map(|t| format!("title {t}")), *a)
                }),
                Op::Osc(n) => {
                    pick(&panes, *n).map_or(Ok(()), |p| g.set_pane_osc_title(p, format!("osc {n}")))
                }
                Op::Focus(n) => pick(&panes, *n).map_or(Ok(()), |p| g.focus_pane(p).map(drop)),
                Op::FocusWindow(n) => {
                    pick(&windows, *n).map_or(Ok(()), |w| g.focus_window(w, None).map(drop))
                }
                Op::Reorder(n, i) => pick(&windows, *n)
                    .map_or(Ok(()), |w| g.reorder_window(w, *i as usize % 4, None)),
                Op::Tag(n, v) => match (pick(&sessions, *n), pick(&panes, *v)) {
                    (Some(sid), _) if n.is_multiple_of(2) => {
                        g.set_session_tag(sid, format!("k{v}"), format!("{n}"))
                    }
                    (_, Some(p)) => g.set_pane_tag(p, format!("k{n}"), format!("{v}")),
                    _ => Ok(()),
                },
                Op::Theme(n) => pick(&panes, *n)
                    .map_or(Ok(()), |p| g.set_pane_theme(p, Some(format!("theme{n}")))),
                Op::Kill(n) => pick(&panes, *n).map_or(Ok(()), |p| g.destroy_pane(p, None)),
            };
        }
        g
    }

    fn extras_for(p: PaneId) -> PaneExtras {
        let b = p.0.as_bytes()[0];
        PaneExtras {
            last_cwd: (!b.is_multiple_of(3)).then(|| cwd(b.wrapping_add(1))),
            process: (b.is_multiple_of(2)).then_some(ProcessId {
                pid: i32::from(b) + 100,
                start_token: u64::from(b) * 7,
            }),
        }
    }

    /// The title a pane shows once its OSC title is gone: the one thing a
    /// restart is allowed to change.
    fn title_without_osc(p: &Pane) -> String {
        let mut p = p.clone();
        p.osc_title = None;
        p.recalculate_title();
        p.title
    }

    fn rects(l: &WindowLayout, w: u16, h: u16) -> Vec<(PaneId, Rect)> {
        let mut r = l.compute_rects(Rect::new(0, 0, w, h));
        r.sort_by_key(|(p, _)| p.0);
        r
    }

    /// Two sessions; the first's window is split three ways and zoomed.
    fn good() -> Workspace {
        let (g, _) = SessionGraph::new();
        let s = g
            .create_session_with_command("a".into(), "/a".into(), vec![])
            .unwrap();
        let p0 = g.current().panes.keys().copied().next().unwrap();
        let p1 = g.split_pane(p0, Direction::Vertical, 0.5).unwrap();
        g.split_pane(p1, Direction::Horizontal, 0.3).unwrap();
        g.focus_pane(p1).unwrap();
        g.zoom_pane(p1, None).unwrap();
        g.create_window_with_command(s, "w2".into(), "/b".into(), vec![])
            .unwrap();
        g.create_session_with_command("b".into(), "/c".into(), vec![])
            .unwrap();
        capture(&g.current(), "/sock", &|_| PaneExtras::default())
    }

    fn zoomed(ws: &mut Workspace) -> &mut PWindow {
        ws.sessions[0]
            .windows
            .iter_mut()
            .find(|w| w.zoomed.is_some())
            .unwrap()
    }

    /// Each row breaks one rule of a valid workspace; every rule must hold on
    /// its own, so no row is caught by another row's check.
    #[test]
    fn each_inconsistency_is_refused() {
        type Break = fn(&mut Workspace);
        let rows: &[(&str, Break)] = &[
            ("duplicate session id", |ws| {
                ws.sessions[1].id = ws.sessions[0].id
            }),
            ("duplicate pane id", |ws| {
                let id = ws.sessions[0].windows[0].panes[0].id;
                ws.sessions[1].windows[0].panes[0].id = id;
                ws.sessions[1].windows[0].nodes[0] = PNode::Leaf { pane: id };
                ws.sessions[1].windows[0].active_pane = id;
            }),
            ("duplicate window id", |ws| {
                ws.sessions[1].windows[0].id = ws.sessions[0].windows[0].id
            }),
            ("repeated name", |ws| ws.sessions[1].name = "a".into()),
            ("invalid name", |ws| ws.sessions[1].name = "a b".into()),
            ("sequence near the end", |ws| {
                ws.sessions[0].created_seq = u64::MAX
            }),
            ("no window", |ws| ws.sessions[1].windows.clear()),
            ("active window elsewhere", |ws| {
                ws.sessions[1].active_window = ws.sessions[0].windows[0].id;
            }),
            ("invalid window title", |ws| {
                ws.sessions[1].windows[0].title = "7".into()
            }),
            ("leaf missing from pane list", |ws| {
                zoomed(ws).panes.pop();
            }),
            ("pane list names a pane not in the tree", |ws| {
                let w = zoomed(ws);
                w.panes[0].id = PaneId::new();
            }),
            ("active pane not in window", |ws| {
                let w = zoomed(ws);
                w.active_pane = PaneId::new();
            }),
            ("zoom with focus elsewhere", |ws| {
                let w = zoomed(ws);
                w.active_pane = w
                    .panes
                    .iter()
                    .map(|p| p.id)
                    .find(|p| Some(*p) != w.zoomed)
                    .unwrap();
            }),
            ("zoom on a pane outside the window", |ws| {
                zoomed(ws).zoomed = Some(PaneId::new())
            }),
            ("zoom on a single pane", |ws| {
                let w = &mut ws.sessions[1].windows[0];
                w.zoomed = Some(w.active_pane);
            }),
            ("node index out of range", |ws| {
                if let PNode::Split { b, .. } = &mut zoomed(ws).nodes[0] {
                    *b = 99;
                }
            }),
            ("node reached twice", |ws| {
                if let PNode::Split { a, b, .. } = &mut zoomed(ws).nodes[0] {
                    *b = *a;
                }
            }),
            ("ratio out of range", |ws| {
                if let PNode::Split { ratio, .. } = &mut zoomed(ws).nodes[0] {
                    *ratio = 0.99;
                }
            }),
            ("ratio not a number", |ws| {
                if let PNode::Split { ratio, .. } = &mut zoomed(ws).nodes[0] {
                    *ratio = f32::NAN;
                }
            }),
            ("unreachable node", |ws| {
                zoomed(ws).nodes.push(PNode::Leaf {
                    pane: PaneId::new(),
                });
            }),
            ("empty node table", |ws| {
                ws.sessions[1].windows[0].nodes.clear()
            }),
            ("undecodable path", |ws| {
                ws.sessions[1].windows[0].panes[0].spawn_cwd = PathRepr::Bytes { hex: "zz".into() };
            }),
            ("odd-length path", |ws| {
                ws.sessions[1].windows[0].panes[0].last_cwd =
                    Some(PathRepr::Bytes { hex: "abc".into() });
            }),
        ];
        assert!(validate(&good()).is_ok());
        for (name, corrupt) in rows {
            let mut ws = good();
            corrupt(&mut ws);
            assert!(
                matches!(validate(&ws), Err(RestoreError::Invalid(_))),
                "{name}: accepted"
            );
        }
    }

    /// `sessions` sessions of one-pane windows, `panes` panes in all.
    fn wide(sessions: usize, panes: usize) -> Workspace {
        let template = good().sessions[1].clone();
        let per = panes.div_ceil(sessions);
        let mut left = panes;
        let sessions = (0..sessions)
            .map(|s| {
                let n = per.min(left);
                left -= n;
                let mut session = template.clone();
                session.id = SessionId::new();
                session.name = format!("s{s}");
                session.windows = (0..n)
                    .map(|i| {
                        let mut w = template.windows[0].clone();
                        w.id = WindowId::new();
                        w.title = format!("w{i}");
                        let p = PaneId::new();
                        w.panes[0].id = p;
                        w.nodes = vec![PNode::Leaf { pane: p }];
                        w.active_pane = p;
                        w
                    })
                    .collect();
                session.active_window = session.windows[0].id;
                session
            })
            .collect();
        Workspace { sessions, ..good() }
    }

    /// The pane caps are exact: a workspace at a cap restores, one past it is
    /// refused. A cap set too low would quarantine a legitimate workspace.
    #[test]
    fn the_pane_caps_are_exact() {
        assert!(validate(&wide(1, MAX_PANES_PER_SESSION)).is_ok());
        assert!(validate(&wide(1, MAX_PANES_PER_SESSION + 1)).is_err());
        assert!(validate(&wide(4, MAX_TOTAL_PANES)).is_ok());
        assert!(validate(&wide(5, MAX_TOTAL_PANES + 1)).is_err());
    }

    /// The decoder's own refusals: before validation can even run.
    #[test]
    fn decode_refuses_what_it_cannot_trust() {
        let bytes = encode(&good());
        assert!(matches!(
            decode(&bytes, "/other"),
            Err(RestoreError::WrongSocket { .. })
        ));
        let mut future = good();
        future.schema = SCHEMA + 1;
        assert_eq!(
            decode(&encode(&future), "/sock"),
            Err(RestoreError::FutureSchema { found: SCHEMA + 1 })
        );
        let mut old = good();
        old.schema = 0;
        assert!(matches!(
            decode(&encode(&old), "/sock"),
            Err(RestoreError::Decode(_))
        ));
        let big = vec![b' '; MAX_FILE_BYTES as usize + 1];
        assert!(matches!(
            decode(&big, "/sock"),
            Err(RestoreError::TooLarge(_))
        ));
        let mut extra = serde_json::to_value(good()).unwrap();
        extra["surprise"] = serde_json::json!(1);
        let extra = serde_json::to_vec(&extra).unwrap();
        assert!(matches!(
            decode(&extra, "/sock"),
            Err(RestoreError::Decode(_))
        ));
    }

    /// A layout as deep as the pane cap allows survives: a recursive encoding
    /// would stop at the decoder's depth limit.
    #[test]
    fn the_deepest_legal_layout_round_trips() {
        let (g, _) = SessionGraph::new();
        g.create_session_with_command("deep".into(), "/".into(), vec![])
            .unwrap();
        let mut p = g.current().panes.keys().copied().next().unwrap();
        for i in 1..MAX_PANES_PER_SESSION {
            let d = if i.is_multiple_of(2) {
                Direction::Vertical
            } else {
                Direction::Horizontal
            };
            p = g.split_pane(p, d, 0.5).unwrap();
        }
        let before = g.current();
        let ws = capture(&before, "/sock", &|_| PaneExtras::default());
        let back = rebuild(&decode(&encode(&ws), "/sock").unwrap());
        let (w, win) = before.windows.iter().next().unwrap();
        assert_eq!(
            format!("{:?}", back.snapshot.windows[w].layout),
            format!("{:?}", win.layout)
        );
    }

    /// A scratch session is never written.
    #[test]
    fn scratch_sessions_are_left_out() {
        let (g, _) = SessionGraph::new();
        let keep = g
            .create_session_with_command("keep".into(), "/".into(), vec![])
            .unwrap();
        g.create_session_flagged("x".into(), "/".into(), vec![], true)
            .unwrap();
        let ws = capture(&g.current(), "/sock", &|_| PaneExtras::default());
        assert_eq!(
            ws.sessions.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![keep]
        );
    }

    /// A corrupted file: bytes overwritten, removed, inserted or cut short,
    /// starting from a real workspace so the damage lands inside valid JSON.
    fn damaged() -> impl Strategy<Value = Vec<u8>> {
        let base = encode(&good());
        let n = base.len();
        proptest::collection::vec((0..n, any::<u8>(), 0u8..4), 1..8).prop_map(move |edits| {
            let mut b = base.clone();
            for (at, byte, kind) in edits {
                let at = at.min(b.len().saturating_sub(1));
                match kind {
                    0 => b[at] = byte,
                    1 if !b.is_empty() => {
                        b.remove(at);
                    }
                    2 => b.insert(at, byte),
                    _ => b.truncate(at),
                }
            }
            b
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// The fuzz entry point's properties hold on damaged real workspaces.
        #[test]
        fn a_damaged_workspace_is_refused_or_restored_whole(bytes in damaged()) {
            fuzz_one(&bytes);
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Everything a workspace promises to keep comes back from its bytes.
        #[test]
        fn a_saved_graph_comes_back_whole(
            ops in proptest::collection::vec(op(), 1..60),
            w in 2u16..300,
            h in 2u16..120,
        ) {
            let g = build(&ops);
            let before = g.current();
            let ws = capture(&before, "/sock", &extras_for);
            let bytes = encode(&ws);
            let decoded = decode(&bytes, "/sock").expect("a captured workspace decodes");
            prop_assert_eq!(&decoded, &ws);
            let r = rebuild(&decoded);
            let after = &r.snapshot;

            prop_assert_eq!(after.sessions.len(), before.sessions.len());
            prop_assert_eq!(after.windows.len(), before.windows.len());
            prop_assert_eq!(after.panes.len(), before.panes.len());
            for (id, s) in &before.sessions {
                let t = &after.sessions[id];
                prop_assert_eq!(&t.name, &s.name);
                prop_assert_eq!(t.created_seq, s.created_seq);
                prop_assert_eq!(&t.windows, &s.windows);
                prop_assert_eq!(t.active_window, s.active_window);
                prop_assert_eq!(&t.tags, &s.tags);
                prop_assert_eq!(&t.theme, &s.theme);
                prop_assert_eq!(t.version, s.version + 1);
                prop_assert_eq!(millis(t.created_at), millis(s.created_at));
            }
            for (id, win) in &before.windows {
                let t = &after.windows[id];
                prop_assert_eq!(&t.title, &win.title);
                prop_assert_eq!(t.session_id, win.session_id);
                prop_assert_eq!(t.active_pane, win.active_pane);
                prop_assert_eq!(format!("{:?}", t.layout), format!("{:?}", win.layout));
                prop_assert_eq!(rects(&t.layout, w, h), rects(&win.layout, w, h));
                prop_assert_eq!(&t.tags, &win.tags);
                prop_assert_eq!(t.version, win.version + 1);
            }
            for (id, p) in &before.panes {
                let t = &after.panes[id];
                prop_assert_eq!(t.window_id, p.window_id);
                prop_assert_eq!(&t.cwd, &p.cwd);
                prop_assert_eq!(&t.manual_title, &p.manual_title);
                prop_assert_eq!(t.auto_title, p.auto_title);
                prop_assert_eq!(t.title.clone(), title_without_osc(p));
                prop_assert_eq!(&t.tags, &p.tags);
                prop_assert_eq!(&t.theme, &p.theme);
                prop_assert_eq!(t.version, p.version + 1);
                prop_assert_eq!(r.extras.get(id), Some(&extras_for(*id)));
            }
        }
    }
}
