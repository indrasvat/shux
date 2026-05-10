//! Generic batch operations for `state.apply` (PR 3a, task 030).
//!
//! Codex review of the original PR 3 plan flagged that `state.apply` should
//! NOT take a TOML-template-shaped object — that would bake CLI grammar into
//! the daemon API. Instead this module defines a generic `Op` delta language
//! that the CLI compiles TOML into, and that any other agent (MCP server,
//! Python SDK, raw curl) can target directly.
//!
//! Key correctness notes (from the council review):
//! - Atomicity is **graph/control-plane only**. PTY spawn happens after
//!   commit and is reported per-op in the result; spawn failure does not
//!   roll back the graph (see [`OpOutput::spawn_status`]).
//! - All events fired by an apply share a single `correlation_id`; subscribers
//!   can attribute the burst by filtering on it.
//! - Validation is staged: clone the snapshot, mutate the staging copy,
//!   commit ONCE if every op validates. No partial commits ever.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::layout::Direction;
use crate::model::{PaneId, SessionId, WindowId};

/// A reference to an entity, either by absolute ID or by back-reference to
/// an earlier op in the same batch ("the session created by op 0").
///
/// Back-references let templates express "create a window in the session I
/// just created" without two round-trips. Resolved during validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SessionRef {
    /// Absolute SessionId.
    Id(SessionId),
    /// Look up the SessionId produced by the op at this index.
    BackRef {
        #[serde(rename = "ref_op")]
        op_index: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PaneRef {
    Id(PaneId),
    BackRef {
        #[serde(rename = "ref_op")]
        op_index: usize,
    },
}

/// A single operation in a batch. Each op is validated against the staging
/// snapshot (with prior ops' outputs already applied) and then mutates the
/// staged snapshot. If any op fails validation, the entire batch rolls back
/// (no commit, no events).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Create a session with an initial window + pane.
    CreateSession {
        /// Optional name. None → auto-generate `session-N`.
        name: Option<String>,
        /// Default cwd for the initial pane.
        cwd: PathBuf,
        /// Initial pane's command. Empty Vec → default shell at PTY spawn.
        /// Persisted on the Pane and surfaced in the PaneCreated event so
        /// subscribers see what the pane is actually running.
        #[serde(default)]
        initial_command: Vec<String>,
    },
    /// Create a window inside a session, with an initial pane.
    CreateWindow {
        session: SessionRef,
        /// Window title.
        title: String,
        /// Initial pane's cwd. None → inherit from session default.
        cwd: Option<PathBuf>,
        #[serde(default)]
        initial_command: Vec<String>,
    },
    /// Split an existing pane to spawn a new one.
    SplitPane {
        target: PaneRef,
        direction: Direction,
        /// Ratio in (0.0, 1.0). 0.5 = even split.
        ratio: f32,
        #[serde(default)]
        command: Vec<String>,
        /// Optional cwd for the new pane. None → inherit from target pane
        /// (matches tmux's split-window default behavior).
        #[serde(default)]
        cwd: Option<PathBuf>,
    },
}

/// Per-op result, indexed positionally with the input ops vec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpOutput {
    pub op_index: usize,
    /// New SessionId if this op created a session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// New WindowId if this op created a window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_id: Option<WindowId>,
    /// New PaneId if this op created a pane (directly or via split).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<PaneId>,
}

/// Result of `apply_batch`. Includes per-op outputs (so the caller can
/// resolve backrefs after the fact), the correlation_id used for events,
/// and the seq of the last event published (useful for `events.watch`
/// resumption from immediately-after).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchResult {
    pub outputs: Vec<OpOutput>,
    pub correlation_id: String,
    /// Sequence number of the last event published in this batch. The next
    /// `events.watch { from_seq: this+1, ... }` resumes immediately after.
    pub last_event_seq: u64,
    /// PTY spawn results, populated by the daemon AFTER the graph commits.
    /// Keyed by op_index. Apply atomicity is graph-only; spawn failures are
    /// reported here but do NOT roll back the graph (codex review P0 #1).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spawn_results: Vec<SpawnResult>,
}

/// Per-pane spawn outcome reported by the daemon after the graph commits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnResult {
    pub op_index: usize,
    pub pane_id: PaneId,
    pub spawned: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Errors specific to batch application.
#[derive(Debug, Clone, thiserror::Error)]
pub enum BatchError {
    #[error("op {op_index} references op {ref_op} but only {prior} ops have run")]
    BackRefOutOfRange {
        op_index: usize,
        ref_op: usize,
        prior: usize,
    },
    #[error("op {op_index} ref {ref_op} did not produce a {expected}")]
    BackRefWrongType {
        op_index: usize,
        ref_op: usize,
        expected: &'static str,
    },
    #[error("op {op_index} failed validation: {source}")]
    OpFailed {
        op_index: usize,
        #[source]
        source: crate::graph::GraphError,
    },
    #[error("apply must contain at least one op")]
    Empty,
}

/// Generate a fresh correlation_id for an apply batch.
///
/// Format: `apply-<uuid>` so it sorts and greps cleanly when mixed with
/// other potential correlation prefixes (event interceptors, plugin batches).
pub fn new_correlation_id() -> String {
    format!("apply-{}", uuid::Uuid::new_v4())
}
