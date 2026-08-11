//! Shared pane I/O state — the bridge between the RPC handlers and the
//! per-pane PTY read loops.
//!
//! One `PaneIoState` lives behind a `tokio::sync::Mutex` for the whole daemon:
//! write channels, resize channels, VTs, lens revisions and checkpoints, and
//! the recorders attached to each pane. Every mutation of a pane's I/O goes
//! through it, which is why the lens checkpoint FIFO and its invalidation
//! marker live here rather than beside the RPC method that writes them.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{Notify, mpsc, oneshot, watch};

use crate::pane_record::PaneRecorder;

/// A pane-resize message sent through `PaneIoState::resizers`.
///
/// Carries the requested `PtySize` and an optional one-shot ack that the
/// per-pane PTY task fires after applying TIOCSWINSZ + `vt.resize()`.
/// Synchronous callers (`pane.set_size` RPC) pass `Some(tx)` and await it
/// so the RPC only returns once `vt.grid().cols/rows` actually reflect the
/// new size; fire-and-forget producers (attach-client layout fan-out)
/// pass `None`.
pub struct ResizeRequest {
    pub size: shux_pty::handle::PtySize,
    pub ack: Option<tokio::sync::oneshot::Sender<()>>,
}

/// Lens ContentRevision publication payload (PRD §4, LENS-R-003). Published on
/// a per-pane `tokio::sync::watch` channel once per Class-A batch so late
/// subscribers (`pane.wait_settled`, P3) always read the current value — no
/// lost-edge races (a `watch`, deliberately NOT a `Notify`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneRevision {
    pub content_revision: u64,
    pub last_mutation_ns: u64,
}

/// A stored atomic clone of a pane's visible grid at one ContentRevision
/// (lens PRD §7 LENS-R-030/031). Created by `pane.glance{checkpoint:true}`
/// and `pane.checkpoint`; consumed by `pane.diff_since` (P4), which diffs a
/// fresh clone of the current grid against `grid`/`cursor` here. Resize and
/// alt-screen switches free these and record an invalidation marker
/// (LENS-R-032, `PaneIoState::invalidations`).
pub(crate) struct PaneCheckpoint {
    revision: u64,
    grid: shux_vt::Grid,
    /// (row, col, visible) at capture time — the §5.1 clone's cursor, not
    /// re-derived later.
    cursor: (usize, usize, bool),
    /// The pane's OSC 10/11/12 dynamic default colors at capture time
    /// (LENS-R-038b, PR #91 codex P2): `Color::Default` cells present
    /// differently when the defaults change, so the diff must resolve
    /// Default against EACH side's respective defaults — the checkpoint
    /// carries its own.
    default_colors: shux_vt::TerminalDefaultColors,
}

/// Shared state for pane I/O operations (PTY writes, VT state, command tracking).
///
/// This is the bridge between the RPC handlers and the per-pane PTY read loops.
/// Each pane gets a write channel; the read loop is a separate tokio task.
pub struct PaneIoState {
    /// Per-pane write channels: send bytes to the pane's PTY read/write task.
    pub writers: HashMap<shux_core::model::PaneId, mpsc::Sender<Vec<u8>>>,
    /// Per-pane resize channels: send `ResizeRequest` to trigger
    /// TIOCSWINSZ + VT resize. Use `ResizeRequest { ack: None }` for
    /// fire-and-forget; `ack: Some(tx)` for synchronous RPCs that must
    /// see the new dimensions on return.
    pub resizers: HashMap<shux_core::model::PaneId, mpsc::Sender<ResizeRequest>>,
    /// Per-pane cancellation tokens. These are child tokens of the daemon
    /// shutdown token, so daemon shutdown still cancels every pane, while
    /// explicit pane/window/session kills can target only the affected panes.
    pub shutdowns: HashMap<shux_core::model::PaneId, tokio_util::sync::CancellationToken>,
    /// Per-pane completion receivers used by daemon shutdown to wait until
    /// PTY tasks have actually signalled and reaped their children.
    pub pty_done: HashMap<shux_core::model::PaneId, oneshot::Receiver<()>>,
    /// Completion waiters for PTY tasks that were already explicitly torn
    /// down by pane/window/session kill. Daemon shutdown drains these too so
    /// it cannot exit while an earlier teardown is still in its reap/escalate
    /// path.
    pub teardown_waiters: Vec<tokio::task::JoinHandle<()>>,
    /// Per-pane VirtualTerminal instances for capturing output.
    pub vts: HashMap<shux_core::model::PaneId, shux_vt::VirtualTerminal>,
    /// Per-pane lens ContentRevision publishers (PRD §4, LENS-R-003). The
    /// single-writer PTY task publishes `(content_revision, last_mutation_ns)`
    /// here once per Class-A batch; `pane.wait_settled` (P3) subscribes. Same
    /// lifetime as `vts` (created with the pane, removed only on destroy).
    pub revisions: HashMap<shux_core::model::PaneId, watch::Sender<PaneRevision>>,
    /// Per-pane lens checkpoint FIFO (PRD §7, LENS-R-030/031). Writers:
    /// `pane.glance{checkpoint:true}` and `pane.checkpoint`. Same lifetime
    /// as `vts`: cleared on pane teardown.
    pub(crate) checkpoints:
        HashMap<shux_core::model::PaneId, std::collections::VecDeque<PaneCheckpoint>>,
    /// Per-pane lens checkpoint invalidation marker (PRD §7.1, LENS-R-032/033;
    /// DEC-4). The POST-mutation `content_revision` at which a resize or
    /// alt-screen switch freed every checkpoint of the pane. `pane.diff_since`
    /// reports `RESIZE_INVALIDATED (-32011)` for any `since_revision ≤` this
    /// marker that no longer has a live checkpoint. Monotonic (revisions only
    /// increase). Same lifetime as `vts`: cleared on pane teardown.
    pub(crate) invalidations: HashMap<shux_core::model::PaneId, u64>,
    /// Command execution engine for marker-based completion detection.
    pub cmd_engine: shux_pty::CommandEngine,
    /// Notify any attach-render loops that a pane's VT has new bytes to
    /// flush. Bumped after every PTY read so the renderer can wake up
    /// promptly (instead of polling a fixed interval).
    pub render_pulse: Arc<tokio::sync::Notify>,
    /// PR 2c — data-plane publisher. The per-pane PTY task forwards
    /// sampled PTY chunks here via `publish_pane_output`. `None` in
    /// test harnesses that don't wire an event bus. Cheap to clone
    /// (Arc internally).
    pub event_bus: Option<shux_core::bus::EventBus>,
    /// Lossless pane-output recorders, keyed by pane. The PTY read task
    /// awaits these sends before sampled publishing, so this path is
    /// byte-exact and intentionally applies backpressure.
    pub(crate) recorders: HashMap<shux_core::model::PaneId, Vec<PaneRecorder>>,
    /// Per-pane PTY child PID (== pgid; children are session leaders, see
    /// `shux-pty::handle::PtyHandle::terminate`). `lens.run` (P5) reads this
    /// to populate the scratch registry's `pgid` field (LENS-R-044) without
    /// needing its own handle to the spawned `PtyHandle`. Same lifetime as
    /// `vts`: cleared on pane teardown.
    pub pty_pids: HashMap<shux_core::model::PaneId, u32>,
}

impl Default for PaneIoState {
    fn default() -> Self {
        Self::new()
    }
}

impl PaneIoState {
    pub fn new() -> Self {
        Self {
            writers: HashMap::new(),
            resizers: HashMap::new(),
            shutdowns: HashMap::new(),
            pty_done: HashMap::new(),
            teardown_waiters: Vec::new(),
            vts: HashMap::new(),
            revisions: HashMap::new(),
            checkpoints: HashMap::new(),
            invalidations: HashMap::new(),
            cmd_engine: shux_pty::CommandEngine::new(),
            render_pulse: Arc::new(tokio::sync::Notify::new()),
            event_bus: None,
            recorders: HashMap::new(),
            pty_pids: HashMap::new(),
        }
    }

    pub fn with_event_bus(mut self, bus: shux_core::bus::EventBus) -> Self {
        self.event_bus = Some(bus);
        self
    }

    pub fn teardown_panes(
        &mut self,
        pane_ids: &[shux_core::model::PaneId],
        remove_vts: bool,
    ) -> Arc<Notify> {
        let (pulse, done) = self.teardown_panes_collecting(pane_ids, remove_vts);
        self.track_teardown_waiters(done);
        pulse
    }

    fn track_teardown_waiters(&mut self, done: Vec<oneshot::Receiver<()>>) {
        self.teardown_waiters.retain(|waiter| !waiter.is_finished());
        self.teardown_waiters.extend(done.into_iter().map(|rx| {
            tokio::spawn(async move {
                let _ = rx.await;
            })
        }));
    }

    pub fn teardown_panes_collecting(
        &mut self,
        pane_ids: &[shux_core::model::PaneId],
        remove_vts: bool,
    ) -> (Arc<Notify>, Vec<oneshot::Receiver<()>>) {
        let mut done = Vec::new();
        for pane_id in pane_ids {
            if let Some(token) = self.shutdowns.remove(pane_id) {
                token.cancel();
            }
            self.writers.remove(pane_id);
            self.resizers.remove(pane_id);
            if let Some(rx) = self.pty_done.remove(pane_id) {
                done.push(rx);
            }
            if remove_vts {
                self.vts.remove(pane_id);
                // The revision publisher has the same lifetime as the VT;
                // dropping the sender closes the watch for any settle waiter.
                self.revisions.remove(pane_id);
                self.checkpoints.remove(pane_id);
                self.invalidations.remove(pane_id);
                self.pty_pids.remove(pane_id);
            }
        }
        (self.render_pulse.clone(), done)
    }

    /// Publish a pane's lens ContentRevision on its watch channel, but only when
    /// `content_revision` advanced (LENS-R-003: once per Class-A batch; Class-B
    /// no-op batches leave the value — and settle waiters — untouched). No-op
    /// when the pane has no publisher (e.g. test-only VT inserts).
    pub(crate) fn publish_revision(&self, pane_id: shux_core::model::PaneId, rev: PaneRevision) {
        if let Some(tx) = self.revisions.get(&pane_id) {
            tx.send_if_modified(|cur| {
                if cur.content_revision != rev.content_revision {
                    *cur = rev;
                    true
                } else {
                    false
                }
            });
        }
    }

    /// Store (or dedup) a checkpoint clone (`pane.glance{checkpoint:true}` /
    /// `pane.checkpoint`; lens PRD §7, LENS-R-030/031). `grid`/`cursor` are
    /// the SAME atomic clone the caller already rendered/extracted text from
    /// (LENS-R-010), keyed by the revision read alongside it — not re-read
    /// here.
    ///
    /// Refuses panes with no live VT (codex P2 review major): the glance
    /// handler stores under a SECOND lock acquisition, so the pane can be
    /// torn down between the clone and the store — `entry().or_default()`
    /// would silently resurrect checkpoint state for a dead pane, leaking it
    /// until daemon shutdown (teardown has already run and won't re-run).
    ///
    /// Refuses revisions BELOW the pane's invalidation marker (codex P4
    /// convergence blocker — the same two-lock race, invalidation flavour):
    /// glance clones at revision R, a concurrent resize/alt-switch
    /// invalidates at R+1 (freeing all storage + recording the marker), then
    /// glance's late store would re-insert pre-invalidation content and a
    /// later `diff_since(R)` would silently diff stale-dimension frames
    /// instead of reporting RESIZE_INVALIDATED (violating LENS-R-032/033).
    /// The bound is STRICTLY-LESS-THAN, not ≤: LENS-R-033 pins "a checkpoint
    /// created AFTER the invalidation (revision ≥ marker) is found by rule
    /// (1)" — revision and clone are read in one io-lock critical section
    /// and invalidating events bump the revision inside that same lock, so a
    /// clone keyed at revision == marker depicts the POST-mutation frame
    /// (e.g. `pane.checkpoint` immediately after a resize reads exactly the
    /// marker revision; refusing it would orphan the post-resize frame and
    /// make `diff_since(marker)` wrongly -32011). Only revision < marker
    /// content predates the invalidation. Refusal is the same honest no-op
    /// as the teardown race: `(false, None)` → glance reports
    /// `checkpointed: false`.
    ///
    /// Unique per revision: a checkpoint already stored at `revision` is a
    /// no-op (stores nothing, evicts nothing). Otherwise inserts SORTED by
    /// revision and, past the 4-checkpoint cap, evicts the front — the
    /// LOWEST creation revision. LENS-R-031 orders the FIFO by CREATION
    /// REVISION, not arrival (claude P2 review minor c): two racing glances
    /// can reach their second lock windows out of revision order, and
    /// insertion-order eviction would then evict the newer frame. (DEC-22:
    /// reads never refresh recency.) Returns `(stored_or_present,
    /// evicted_revision)`: the flag is false only when the pane was gone or
    /// the revision predates an invalidation.
    pub(crate) fn store_checkpoint(
        &mut self,
        pane_id: shux_core::model::PaneId,
        revision: u64,
        grid: shux_vt::Grid,
        cursor: (usize, usize, bool),
        default_colors: shux_vt::TerminalDefaultColors,
    ) -> (bool, Option<u64>) {
        const MAX_CHECKPOINTS: usize = 4;
        if !self.vts.contains_key(&pane_id) {
            return (false, None);
        }
        if self
            .invalidations
            .get(&pane_id)
            .is_some_and(|&marker| revision < marker)
        {
            return (false, None);
        }
        let deque = self.checkpoints.entry(pane_id).or_default();
        if deque.iter().any(|c| c.revision == revision) {
            return (true, None);
        }
        // Sorted insert keeps the deque revision-ascending, so the front is
        // always the oldest-by-creation-revision eviction candidate.
        let at = deque
            .iter()
            .position(|c| c.revision > revision)
            .unwrap_or(deque.len());
        deque.insert(
            at,
            PaneCheckpoint {
                revision,
                grid,
                cursor,
                default_colors,
            },
        );
        if deque.len() > MAX_CHECKPOINTS {
            (true, deque.pop_front().map(|evicted| evicted.revision))
        } else {
            (true, None)
        }
    }

    /// Invalidate every checkpoint of a pane at the POST-mutation revision of a
    /// resize or alt-screen switch (lens PRD §7.1, DEC-4, LENS-R-032/033).
    /// Frees the stored frames and records the marker (kept monotonic — the
    /// highest invalidating revision wins) so `pane.diff_since` can tell
    /// "predates an invalidation" (`RESIZE_INVALIDATED`) apart from "never
    /// checkpointed / evicted" (`STALE_REVISION`). No-op for panes with no
    /// live VT (teardown already ran); a checkpoint created AFTER this marker
    /// (revision ≥ marker) is still found by the diff's existence-first rule.
    pub(crate) fn invalidate_checkpoints(
        &mut self,
        pane_id: shux_core::model::PaneId,
        at_revision: u64,
    ) {
        if !self.vts.contains_key(&pane_id) {
            return;
        }
        self.checkpoints.remove(&pane_id);
        let marker = self.invalidations.entry(pane_id).or_insert(0);
        *marker = (*marker).max(at_revision);
    }

    /// Live checkpoint revisions for a pane, ascending (the deque is kept
    /// revision-sorted by `store_checkpoint`). Used to populate
    /// `STALE_REVISION`'s `available` list (LENS-R-033).
    pub(crate) fn checkpoint_revisions(&self, pane_id: &shux_core::model::PaneId) -> Vec<u64> {
        self.checkpoints
            .get(pane_id)
            .map(|d| d.iter().map(|c| c.revision).collect())
            .unwrap_or_default()
    }
}

/// A checkpoint's stored clone as returned by `diff_lookup_checkpoint`:
/// (grid, cursor {row, col, visible}, OSC defaults at capture — LENS-R-038b).
pub(crate) type CheckpointClone = (
    shux_vt::Grid,
    (usize, usize, bool),
    shux_vt::TerminalDefaultColors,
);

/// Resolve a `pane.diff_since` `since_revision` against a pane's stored
/// checkpoints and invalidation marker (lens PRD §7.1, LENS-R-033). Existence
/// FIRST, which makes the rule off-by-one-proof:
///   (1) a stored checkpoint whose revision == `since` → return its clone;
///   (2) else `since ≤ last_invalidation` → `RESIZE_INVALIDATED (-32011)`;
///   (3) else → `STALE_REVISION (-32010)` with `{requested, available}`.
/// The pane's existence is checked by the caller BEFORE this (so a missing
/// pane is `PANE_NOT_FOUND`, never a diff error). Returns the checkpoint's
/// `(grid, cursor, default_colors)` clone on a hit (defaults per
/// LENS-R-038b — the diff resolves `Color::Default` against each side's own
/// defaults).
pub(crate) fn diff_lookup_checkpoint(
    state: &PaneIoState,
    pane_id: &shux_core::model::PaneId,
    since: u64,
) -> Result<CheckpointClone, shux_rpc::RpcError> {
    if let Some(cp) = state
        .checkpoints
        .get(pane_id)
        .and_then(|d| d.iter().find(|c| c.revision == since))
    {
        return Ok((cp.grid.clone(), cp.cursor, cp.default_colors));
    }
    if let Some(&marker) = state.invalidations.get(pane_id)
        && since <= marker
    {
        return Err(shux_rpc::RpcError::resize_invalidated(since, marker));
    }
    Err(shux_rpc::RpcError::stale_revision(
        since,
        &state.checkpoint_revisions(pane_id),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// codex P2 review major — checkpoint resurrection: `pane.glance` stores
    /// its checkpoint under a SECOND lock acquisition, so the pane can be
    /// torn down between the clone and the store. store_checkpoint must
    /// refuse VT-less panes instead of `entry().or_default()`-recreating
    /// checkpoint state that teardown already cleared (and will never clear
    /// again).
    #[test]
    fn checkpoint_store_refuses_resurrection_after_teardown() {
        let pane_id = shux_core::model::PaneId::new();
        let mut state = PaneIoState::new();
        let vt = shux_vt::VirtualTerminal::new(24, 80);
        let grid = vt.grid().clone_visible();

        // No VT registered at all → refuse, and do NOT create an entry.
        let (stored, evicted) = state.store_checkpoint(
            pane_id,
            1,
            grid.clone(),
            (0, 0, true),
            shux_vt::TerminalDefaultColors::default(),
        );
        assert!(!stored && evicted.is_none());
        assert!(!state.checkpoints.contains_key(&pane_id));

        // Live VT → stores; same-revision re-store is the LENS-R-030 no-op.
        state.vts.insert(pane_id, vt);
        let (stored, evicted) = state.store_checkpoint(
            pane_id,
            1,
            grid.clone(),
            (0, 0, true),
            shux_vt::TerminalDefaultColors::default(),
        );
        assert!(stored && evicted.is_none());
        let (stored, evicted) = state.store_checkpoint(
            pane_id,
            1,
            grid.clone(),
            (0, 0, true),
            shux_vt::TerminalDefaultColors::default(),
        );
        assert!(stored && evicted.is_none(), "same-revision no-op");
        assert_eq!(state.checkpoints[&pane_id].len(), 1);

        // Teardown clears VT + checkpoints; a late store (the glance race)
        // must refuse and must NOT resurrect the checkpoints entry.
        let _ = state.teardown_panes_collecting(&[pane_id], true);
        assert!(!state.checkpoints.contains_key(&pane_id));
        let (stored, evicted) = state.store_checkpoint(
            pane_id,
            2,
            grid,
            (0, 0, true),
            shux_vt::TerminalDefaultColors::default(),
        );
        assert!(!stored && evicted.is_none());
        assert!(
            !state.checkpoints.contains_key(&pane_id),
            "dead pane's checkpoint state must not be resurrected"
        );
    }

    /// claude P2 review minors (b)+(c) — LENS-R-031 FIFO eviction, unit level
    /// (the frozen D5 integration test stays red until P4): the cap-4 FIFO
    /// orders by CREATION REVISION, not arrival — two racing glances can
    /// reach their second lock windows out of revision order, and eviction
    /// must still pick the lowest revision.
    #[test]
    fn checkpoint_fifo_evicts_lowest_creation_revision() {
        let pane_id = shux_core::model::PaneId::new();
        let mut state = PaneIoState::new();
        let vt = shux_vt::VirtualTerminal::new(24, 80);
        let grid = vt.grid().clone_visible();
        state.vts.insert(pane_id, vt);

        // (b) Ascending stores: cap 4, the 5th evicts the first.
        for rev in [1_u64, 2, 3, 4] {
            let (stored, evicted) = state.store_checkpoint(
                pane_id,
                rev,
                grid.clone(),
                (0, 0, true),
                shux_vt::TerminalDefaultColors::default(),
            );
            assert!(
                stored && evicted.is_none(),
                "rev {rev} stores without eviction"
            );
        }
        let (stored, evicted) = state.store_checkpoint(
            pane_id,
            5,
            grid.clone(),
            (0, 0, true),
            shux_vt::TerminalDefaultColors::default(),
        );
        assert!(stored);
        assert_eq!(evicted, Some(1), "5th store evicts the FIFO-oldest (rev 1)");

        // (c) Out-of-order arrival (the two-lock race): live revisions are
        // now [2,3,4,5]. Evict 2 and 3 out from under it, then interleave.
        let mut state = PaneIoState::new();
        let vt = shux_vt::VirtualTerminal::new(24, 80);
        state.vts.insert(pane_id, vt);
        for rev in [10_u64, 5, 20, 30] {
            let (stored, evicted) = state.store_checkpoint(
                pane_id,
                rev,
                grid.clone(),
                (0, 0, true),
                shux_vt::TerminalDefaultColors::default(),
            );
            assert!(stored && evicted.is_none());
        }
        // Deque must be revision-ordered despite arrival order, so the next
        // store evicts revision 5 (oldest by CREATION REVISION) — a pure
        // insertion-order FIFO would wrongly evict 10 (the first arrival).
        let (stored, evicted) = state.store_checkpoint(
            pane_id,
            40,
            grid,
            (0, 0, true),
            shux_vt::TerminalDefaultColors::default(),
        );
        assert!(stored);
        assert_eq!(
            evicted,
            Some(5),
            "eviction is by lowest creation revision, not arrival order"
        );
        let live: Vec<u64> = state.checkpoints[&pane_id]
            .iter()
            .map(|c| c.revision)
            .collect();
        assert_eq!(live, vec![10, 20, 30, 40], "deque stays revision-ascending");
    }

    /// LENS-R-033 existence-first lookup + LENS-R-032 invalidation marker.
    /// Proves the -32011-vs-32010 disambiguation and that a checkpoint created
    /// AFTER an invalidation is still found (rule 1 before rule 2).
    #[test]
    fn diff_lookup_existence_first_and_invalidation_marker() {
        let pane_id = shux_core::model::PaneId::new();
        let mut state = PaneIoState::new();
        let vt = shux_vt::VirtualTerminal::new(24, 80);
        let grid = vt.grid().clone_visible();
        state.vts.insert(pane_id, vt);

        // One checkpoint at revision 5.
        state.store_checkpoint(
            pane_id,
            5,
            grid.clone(),
            (0, 0, true),
            shux_vt::TerminalDefaultColors::default(),
        );
        // (1) exact hit → Ok clone.
        assert!(diff_lookup_checkpoint(&state, &pane_id, 5).is_ok());
        // (3) no checkpoint, no marker → STALE with available:[5].
        let err = diff_lookup_checkpoint(&state, &pane_id, 6).unwrap_err();
        assert_eq!(err.code, shux_rpc::ErrorCode::StaleRevision.code());
        assert_eq!(
            err.data.as_ref().unwrap()["available"],
            serde_json::json!([5])
        );

        // Invalidate at revision 9 (resize/alt-switch): frees the deque, marks 9.
        state.invalidate_checkpoints(pane_id, 9);
        assert!(
            !state.checkpoints.contains_key(&pane_id) || state.checkpoints[&pane_id].is_empty()
        );
        // (2) since ≤ marker → RESIZE_INVALIDATED.
        let err = diff_lookup_checkpoint(&state, &pane_id, 5).unwrap_err();
        assert_eq!(err.code, shux_rpc::ErrorCode::ResizeInvalidated.code());
        // since > marker but no checkpoint → STALE (available now empty).
        let err = diff_lookup_checkpoint(&state, &pane_id, 12).unwrap_err();
        assert_eq!(err.code, shux_rpc::ErrorCode::StaleRevision.code());
        assert_eq!(
            err.data.as_ref().unwrap()["available"],
            serde_json::json!([])
        );

        // A checkpoint created AFTER the invalidation (rev 10 ≥ marker 9) is
        // found by rule (1) before rule (2) can misfire (LENS-R-033).
        state.store_checkpoint(
            pane_id,
            10,
            grid,
            (0, 0, true),
            shux_vt::TerminalDefaultColors::default(),
        );
        assert!(diff_lookup_checkpoint(&state, &pane_id, 10).is_ok());
        // The marker still shadows the freed pre-9 revisions.
        assert_eq!(
            diff_lookup_checkpoint(&state, &pane_id, 5)
                .unwrap_err()
                .code,
            shux_rpc::ErrorCode::ResizeInvalidated.code()
        );
    }

    /// Monotonic invalidation marker: a later invalidation never lowers it.
    #[test]
    fn invalidation_marker_is_monotonic() {
        let pane_id = shux_core::model::PaneId::new();
        let mut state = PaneIoState::new();
        state
            .vts
            .insert(pane_id, shux_vt::VirtualTerminal::new(24, 80));
        state.invalidate_checkpoints(pane_id, 9);
        state.invalidate_checkpoints(pane_id, 3); // stale/out-of-order
        assert_eq!(state.invalidations[&pane_id], 9);
    }

    /// codex P4 convergence blocker — checkpoint-resurrection across an
    /// invalidation: glance clones at revision R under lock #1, a concurrent
    /// resize invalidates at R+1, then glance's store under lock #2 arrives
    /// with the PRE-invalidation clone. store_checkpoint must refuse any
    /// revision BELOW the marker (deterministic — the race is replayed here
    /// as direct calls, no timing), so the later diff reports
    /// RESIZE_INVALIDATED instead of silently diffing stale-dimension
    /// frames. Revisions AT the marker stay storable (LENS-R-033: "a
    /// checkpoint created AFTER the invalidation (revision ≥ marker) is
    /// found by rule (1)" — same-lock reads make an ==marker clone the
    /// post-mutation frame).
    #[test]
    fn checkpoint_store_refuses_pre_invalidation_revisions() {
        let pane_id = shux_core::model::PaneId::new();
        let mut state = PaneIoState::new();
        let vt = shux_vt::VirtualTerminal::new(24, 80);
        let grid = vt.grid().clone_visible();
        state.vts.insert(pane_id, vt);

        // Baseline: a checkpoint at 5 stores and is diffable.
        let (stored, _) = state.store_checkpoint(
            pane_id,
            5,
            grid.clone(),
            (0, 0, true),
            shux_vt::TerminalDefaultColors::default(),
        );
        assert!(stored);

        // The invalidating event (resize/alt-switch) lands at revision 7:
        // frees all storage, records the marker.
        state.invalidate_checkpoints(pane_id, 7);

        // The racing glance's LATE store of the pre-invalidation clone at 5
        // must be refused — no checkpoint materializes.
        let (stored, evicted) = state.store_checkpoint(
            pane_id,
            5,
            grid.clone(),
            (0, 0, true),
            shux_vt::TerminalDefaultColors::default(),
        );
        assert!(!stored, "pre-invalidation revision must be refused");
        assert!(evicted.is_none());
        assert!(
            state.checkpoints.get(&pane_id).is_none_or(|d| d.is_empty()),
            "no checkpoint may materialize below the marker"
        );

        // The diff decision path then reports RESIZE_INVALIDATED for 5 —
        // never a stale-dimension diff (the blocker's observable).
        let err = diff_lookup_checkpoint(&state, &pane_id, 5).unwrap_err();
        assert_eq!(
            err.code,
            shux_rpc::ErrorCode::ResizeInvalidated.code(),
            "diff_since(R) after the refused store must be -32011"
        );

        // AT the marker (== 7): the post-mutation frame, storable and
        // diffable — refusing it would orphan the immediately-post-resize
        // pane.checkpoint and make diff_since(7) wrongly -32011.
        let (stored, _) = state.store_checkpoint(
            pane_id,
            7,
            grid.clone(),
            (0, 0, true),
            shux_vt::TerminalDefaultColors::default(),
        );
        assert!(stored, "revision == marker is the post-mutation frame");
        assert!(diff_lookup_checkpoint(&state, &pane_id, 7).is_ok());

        // Above the marker: normal storage.
        let (stored, _) = state.store_checkpoint(
            pane_id,
            8,
            grid,
            (0, 0, true),
            shux_vt::TerminalDefaultColors::default(),
        );
        assert!(stored);
        assert!(diff_lookup_checkpoint(&state, &pane_id, 8).is_ok());
    }
}
