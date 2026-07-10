use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use clap::{CommandFactory, FromArgMatches};
use shux_rpc::{Policy, Sensitivity};
use tokio::sync::{Mutex, Notify, mpsc, oneshot, watch};
use tracing_subscriber::EnvFilter;

mod attach;
mod cli;
mod client;
mod config_validate;
mod daemon;
mod features;
mod lens_scratch;
mod onboarding;
mod session_meta;
mod session_persist;
mod statusbar_build;
mod statusbar_runner;
mod style;
mod template;

use cli::{Cli, Command, OutputFormat, PaneCommand, WindowCommand};

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

const PANE_RECORD_CHANNEL_CAPACITY: usize = 128;
const PANE_RECORD_COMPLETED_TTL: Duration = Duration::from_secs(60);

#[derive(Debug)]
enum PaneRecordChunk {
    Bytes(Vec<u8>),
    Finish { status: PaneRecordStatus },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PaneRecordStatus {
    Recording,
    Complete,
    Error,
    Aborted,
}

impl PaneRecordStatus {
    fn as_str(self) -> &'static str {
        match self {
            PaneRecordStatus::Recording => "recording",
            PaneRecordStatus::Complete => "complete",
            PaneRecordStatus::Error => "error",
            PaneRecordStatus::Aborted => "aborted",
        }
    }
}

#[derive(Clone, Debug)]
struct PaneRecordResult {
    status: PaneRecordStatus,
    bytes_written: u64,
    error: Option<String>,
}

struct PaneRecorder {
    id: uuid::Uuid,
    path: PathBuf,
    sender: mpsc::Sender<PaneRecordChunk>,
    outcome: Arc<StdMutex<PaneRecordResult>>,
    task: tokio::task::JoinHandle<()>,
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
struct PaneCheckpoint {
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
    checkpoints: HashMap<shux_core::model::PaneId, std::collections::VecDeque<PaneCheckpoint>>,
    /// Per-pane lens checkpoint invalidation marker (PRD §7.1, LENS-R-032/033;
    /// DEC-4). The POST-mutation `content_revision` at which a resize or
    /// alt-screen switch freed every checkpoint of the pane. `pane.diff_since`
    /// reports `RESIZE_INVALIDATED (-32011)` for any `since_revision ≤` this
    /// marker that no longer has a live checkpoint. Monotonic (revisions only
    /// increase). Same lifetime as `vts`: cleared on pane teardown.
    invalidations: HashMap<shux_core::model::PaneId, u64>,
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
    recorders: HashMap<shux_core::model::PaneId, Vec<PaneRecorder>>,
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
    fn publish_revision(&self, pane_id: shux_core::model::PaneId, rev: PaneRevision) {
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
    fn store_checkpoint(
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
    fn invalidate_checkpoints(&mut self, pane_id: shux_core::model::PaneId, at_revision: u64) {
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
    fn checkpoint_revisions(&self, pane_id: &shux_core::model::PaneId) -> Vec<u64> {
        self.checkpoints
            .get(pane_id)
            .map(|d| d.iter().map(|c| c.revision).collect())
            .unwrap_or_default()
    }
}

/// A checkpoint's stored clone as returned by `diff_lookup_checkpoint`:
/// (grid, cursor {row, col, visible}, OSC defaults at capture — LENS-R-038b).
type CheckpointClone = (
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
fn diff_lookup_checkpoint(
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

/// Pre-render pixel budget shared by every lens rasterizing path
/// (`pane.glance` PNG, `pane.diff_since` heat PNG — PR #91 codex P1). The
/// same 16M-pixel cap `pane.snapshot` enforces, checked BEFORE any RGBA
/// allocation or rasterization: a 1000×1000-cell pane (valid per
/// `pane.set_size` limits) would otherwise allocate hundreds of MB before
/// the post-encode 8 MiB check could fire. Over budget →
/// `PAYLOAD_TOO_LARGE (-32013)` with `{pixels, max_pixels, hint}` — the
/// caller supplies the method-appropriate hint.
fn lens_pixel_budget_check(
    cols: usize,
    rows: usize,
    cell_w: u32,
    cell_h: u32,
    hint: &str,
) -> Result<(), shux_rpc::RpcError> {
    const MAX_PIXELS: u64 = 16_000_000;
    let pixel_count = (cols as u64)
        .saturating_mul(cell_w as u64)
        .saturating_mul(rows as u64)
        .saturating_mul(cell_h as u64);
    if pixel_count > MAX_PIXELS {
        return Err(shux_rpc::RpcError::with_message_and_data(
            shux_rpc::ErrorCode::PayloadTooLarge,
            "payload_too_large",
            serde_json::json!({
                "pixels": pixel_count,
                "max_pixels": MAX_PIXELS,
                "hint": hint,
            }),
        ));
    }
    Ok(())
}

/// One per-row changed-column span in a `pane.diff_since` result
/// (LENS-R-035): 0-based half-open `[col_start, col_end)`.
struct LensRowSpan {
    row: u16,
    col_start: u16,
    col_end: u16,
}

/// The structured delta between a checkpoint clone and the current clone
/// (lens PRD §7.2, LENS-R-034..036).
struct LensDiff {
    cells_changed: u32,
    regions: Vec<LensRowSpan>,
    regions_truncated: bool,
    /// (row_start, col_start, row_end, col_end) — 0-based HALF-OPEN in both
    /// axes; all zeros when nothing changed (the empty range, LENS-R schema).
    bounding_box: (u16, u16, u16, u16),
    cursor_moved: bool,
    /// Rows (grid index) with ≥1 changed cell, ascending — the keys of
    /// `changed_row_text`.
    changed_rows: Vec<usize>,
    /// Flat `rows × cols` changed mask for the heat overlay (LENS-R-037).
    changed_mask: Vec<bool>,
    rows: usize,
    cols: usize,
}

/// The lens `pane.glance` text of a SINGLE grid row (LENS-R-012 byte-stability,
/// per-row): ANSI-free, wide-continuation cells skipped, full-width, trailing
/// whitespace preserved (no trim). Byte-identical to `Grid::glance_text`'s
/// `row`-th line so `changed_row_text[row]` lines up with the glance text.
fn glance_row_text(grid: &shux_vt::Grid, row_idx: usize) -> String {
    let row = grid.visible_row(row_idx);
    let mut line = String::with_capacity(grid.cols());
    for col in 0..row.len() {
        if let Some(cell) = row.get(col) {
            if cell.is_wide_continuation() {
                continue;
            }
            cell.push_display_text(&mut line);
        }
    }
    line
}

/// Compute the structured diff of `cur` against the checkpoint `cp` clone
/// (lens PRD §7.2). A cell counts as changed iff its UNDERLYING cell data
/// differs (glyph, fg, bg, attrs — `Cell`'s full value equality, no cursor
/// overlay: the clones carry none), with `Color::Default` RESOLVED against
/// each side's respective OSC 10/11/12 defaults (LENS-R-038b, PR #91 codex
/// P2): a default-color-only repaint presents every Default-colored cell
/// differently, so when the two sides' fg (or bg) defaults differ, a cell
/// whose fg (or bg) is `Default` on BOTH sides counts as changed. When the
/// defaults are unchanged the extra clauses never fire and the comparison is
/// byte-identical to plain `Cell` equality (D-tier gates + ratified goldens
/// unaffected). The cursor default (OSC 12) is deliberately NOT part of the
/// cell comparison — the cursor overlay is excluded from diffs entirely
/// (DEC-11). Wide glyphs pair with their spacer: if either half changed,
/// both count (LENS-R-034). Cursor position/visibility is never in the grid
/// cells, so it is excluded from the count/regions by construction; a
/// content change under the cursor's cell still counts. `cursor_moved` is
/// reported separately.
///
/// Max 256 spans (LENS-R-035): past the cap `regions_truncated` is set and the
/// caller emits only `bounding_box`.
fn compute_lens_diff(
    cp: &shux_vt::Grid,
    cur: &shux_vt::Grid,
    cp_cursor: (usize, usize, bool),
    cur_cursor: (usize, usize, bool),
    cp_defaults: shux_vt::TerminalDefaultColors,
    cur_defaults: shux_vt::TerminalDefaultColors,
) -> LensDiff {
    // A valid diff (existence-first lookup hit) implies equal dims — resize
    // invalidates checkpoints. `min` is a defensive guard, not a happy path.
    let rows = cp.rows().min(cur.rows());
    let cols = cp.cols().min(cur.cols());

    // LENS-R-038b: which default channels changed between the two frames.
    // `Default`-colored cells resolve through these, so a changed channel
    // marks every cell that is `Default` in that channel on both sides
    // (asymmetric Default-vs-concrete pairs already differ by raw equality).
    let fg_default_changed = cp_defaults.fg != cur_defaults.fg;
    let bg_default_changed = cp_defaults.bg != cur_defaults.bg;

    let mut changed = vec![false; rows * cols];
    for r in 0..rows {
        let cp_row = cp.visible_row(r);
        let cur_row = cur.visible_row(r);
        for c in 0..cols {
            let differ = match (cp_row.get(c), cur_row.get(c)) {
                (Some(a), Some(b)) => {
                    a != b
                        || (fg_default_changed
                            && a.style.fg == shux_vt::Color::Default
                            && b.style.fg == shux_vt::Color::Default)
                        || (bg_default_changed
                            && a.style.bg == shux_vt::Color::Default
                            && b.style.bg == shux_vt::Color::Default)
                }
                (None, None) => false,
                _ => true,
            };
            if differ {
                changed[r * cols + c] = true;
            }
        }
    }

    // Wide-glyph pairing (LENS-R-034): a wide head and its spacer are one
    // visual unit — if either half changed, both cells count.
    for r in 0..rows {
        let cp_row = cp.visible_row(r);
        let cur_row = cur.visible_row(r);
        for c in 0..cols.saturating_sub(1) {
            let wide = cp_row.get(c).is_some_and(|x| x.is_wide())
                || cur_row.get(c).is_some_and(|x| x.is_wide());
            if wide {
                let i = r * cols + c;
                if changed[i] || changed[i + 1] {
                    changed[i] = true;
                    changed[i + 1] = true;
                }
            }
        }
    }

    // Build spans (per row, contiguous runs → merged), count, bbox, rows.
    const MAX_SPANS: usize = 256;
    let mut regions: Vec<LensRowSpan> = Vec::new();
    let mut changed_rows: Vec<usize> = Vec::new();
    let mut cells_changed: u32 = 0;
    let (mut min_row, mut min_col, mut max_row, mut max_col) =
        (usize::MAX, usize::MAX, 0usize, 0usize);

    for r in 0..rows {
        let mut row_had_change = false;
        let mut c = 0;
        while c < cols {
            if changed[r * cols + c] {
                let start = c;
                while c < cols && changed[r * cols + c] {
                    cells_changed += 1;
                    c += 1;
                }
                // `c` is now one past the run — half-open [start, c).
                regions.push(LensRowSpan {
                    row: r as u16,
                    col_start: start as u16,
                    col_end: c as u16,
                });
                row_had_change = true;
                min_row = min_row.min(r);
                max_row = max_row.max(r);
                min_col = min_col.min(start);
                max_col = max_col.max(c - 1);
            } else {
                c += 1;
            }
        }
        if row_had_change {
            changed_rows.push(r);
        }
    }

    let regions_truncated = regions.len() > MAX_SPANS;
    if regions_truncated {
        regions.clear();
    }

    let bounding_box = if cells_changed == 0 {
        (0, 0, 0, 0)
    } else {
        // Half-open in both axes: [min, max+1).
        (
            min_row as u16,
            min_col as u16,
            (max_row + 1) as u16,
            (max_col + 1) as u16,
        )
    };

    LensDiff {
        cells_changed,
        regions,
        regions_truncated,
        bounding_box,
        cursor_moved: cp_cursor != cur_cursor,
        changed_rows,
        changed_mask: changed,
        rows,
        cols,
    }
}

/// Render the `pane.diff_since` heat PNG (LENS-R-037): the current clone
/// through the standard rasterizer, then changed cells overlaid with
/// `rgba(163,38,56,128)` and unchanged cells desaturated 50%. Deterministic
/// integer math end-to-end (same inputs → byte-identical PNG). Runs on a
/// blocking worker; the base render intentionally draws no cursor (cursor is
/// excluded from the diff, so a cursor block would only add noise).
fn render_lens_heat_png(
    rasterizer: &shux_raster::Rasterizer,
    grid: &shux_vt::Grid,
    default_colors: shux_vt::TerminalDefaultColors,
    changed_mask: &[bool],
    rows: usize,
    cols: usize,
) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;

    let opts = shux_raster::RasterOptions {
        cursor: None,
        cursor_shape: shux_vt::CursorShape::default(),
        cursor_color: default_colors.cursor,
        fg_default: default_colors
            .fg
            .unwrap_or_else(|| shux_raster::RasterOptions::default().fg_default),
        bg_default: default_colors
            .bg
            .unwrap_or_else(|| shux_raster::RasterOptions::default().bg_default),
    };
    let mut img = rasterizer.render(grid, &opts);
    let (cw, ch) = rasterizer.cell_size();
    let (iw, ih) = (img.width(), img.height());

    // Overlay foreground colour + alpha for changed cells (LENS-R-037).
    const HEAT: [u32; 3] = [163, 38, 56];
    const ALPHA: u32 = 128;

    for r in 0..rows {
        for c in 0..cols {
            let cell_changed = changed_mask.get(r * cols + c).copied().unwrap_or(false);
            let x0 = c as u32 * cw;
            let y0 = r as u32 * ch;
            for y in y0..(y0 + ch).min(ih) {
                for x in x0..(x0 + cw).min(iw) {
                    let px = img.get_pixel_mut(x, y);
                    let [pr, pg, pb, _pa] = px.0;
                    if cell_changed {
                        // Alpha-blend HEAT over the pixel: integer, truncating.
                        px.0[0] = ((HEAT[0] * ALPHA + pr as u32 * (255 - ALPHA)) / 255) as u8;
                        px.0[1] = ((HEAT[1] * ALPHA + pg as u32 * (255 - ALPHA)) / 255) as u8;
                        px.0[2] = ((HEAT[2] * ALPHA + pb as u32 * (255 - ALPHA)) / 255) as u8;
                    } else {
                        // Desaturate 50%: move each channel halfway to luma.
                        // Weights 77/150/29 sum to 256 (≈ Rec.601), >>8.
                        let gray = (pr as u32 * 77 + pg as u32 * 150 + pb as u32 * 29) >> 8;
                        px.0[0] = ((pr as u32 + gray) / 2) as u8;
                        px.0[1] = ((pg as u32 + gray) / 2) as u8;
                        px.0[2] = ((pb as u32 + gray) / 2) as u8;
                    }
                }
            }
        }
    }

    let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    let encoder = image::codecs::png::PngEncoder::new(&mut buf);
    encoder
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| format!("heat PNG encode failed: {e}"))?;
    Ok(buf)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PtyTaskExit {
    Natural,
    RequestedTeardown,
}

async fn spawn_pane_recorder(
    path: PathBuf,
    overwrite: bool,
) -> Result<
    (
        mpsc::Sender<PaneRecordChunk>,
        Arc<StdMutex<PaneRecordResult>>,
        tokio::task::JoinHandle<()>,
    ),
    String,
> {
    use tokio::fs::OpenOptions;
    use tokio::io::AsyncWriteExt;

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            format!(
                "failed to create parent directory {}: {e}",
                parent.display()
            )
        })?;
    }

    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        options.custom_flags(nix::libc::O_NOFOLLOW);
    }
    if overwrite {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    let file = options
        .open(&path)
        .await
        .map_err(|e| format!("failed to open record file {}: {e}", path.display()))?;

    let (tx, mut rx) = mpsc::channel::<PaneRecordChunk>(PANE_RECORD_CHANNEL_CAPACITY);
    let outcome = Arc::new(StdMutex::new(PaneRecordResult {
        status: PaneRecordStatus::Recording,
        bytes_written: 0,
        error: None,
    }));
    let writer_outcome = outcome.clone();
    let task = tokio::spawn(async move {
        let mut file = file;
        let mut bytes_written = 0u64;
        let mut final_status = PaneRecordStatus::Complete;
        while let Some(chunk) = rx.recv().await {
            match chunk {
                PaneRecordChunk::Bytes(bytes) => {
                    if let Err(e) = file.write_all(&bytes).await {
                        let mut outcome = writer_outcome.lock().expect("record outcome poisoned");
                        outcome.status = PaneRecordStatus::Error;
                        outcome.bytes_written = bytes_written;
                        outcome.error = Some(format!("failed to write record chunk: {e}"));
                        return;
                    }
                    bytes_written += bytes.len() as u64;
                }
                PaneRecordChunk::Finish { status } => {
                    final_status = status;
                    break;
                }
            }
        }
        let flush_error = file.flush().await.err();
        let mut outcome = writer_outcome.lock().expect("record outcome poisoned");
        outcome.bytes_written = bytes_written;
        if let Some(e) = flush_error {
            outcome.status = PaneRecordStatus::Error;
            outcome.error = Some(format!("failed to flush record file: {e}"));
        } else if outcome.status == PaneRecordStatus::Recording {
            outcome.status = final_status;
        }
    });

    Ok((tx, outcome, task))
}

async fn tee_pane_recorders(
    io_state: &Arc<Mutex<PaneIoState>>,
    pane_id: shux_core::model::PaneId,
    data: &[u8],
    shutdown: &tokio_util::sync::CancellationToken,
) {
    let sinks: Vec<(
        mpsc::Sender<PaneRecordChunk>,
        Arc<StdMutex<PaneRecordResult>>,
    )> = {
        let state = io_state.lock().await;
        state
            .recorders
            .get(&pane_id)
            .map(|recorders| {
                recorders
                    .iter()
                    .filter(|r| {
                        r.outcome.lock().expect("record outcome poisoned").status
                            == PaneRecordStatus::Recording
                    })
                    .map(|r| (r.sender.clone(), r.outcome.clone()))
                    .collect()
            })
            .unwrap_or_default()
    };

    for (sender, outcome) in sinks {
        tokio::select! {
            result = sender.send(PaneRecordChunk::Bytes(data.to_vec())) => {
                if result.is_err() {
                    let mut outcome = outcome.lock().expect("record outcome poisoned");
                    if outcome.status == PaneRecordStatus::Recording {
                        outcome.status = PaneRecordStatus::Error;
                        outcome.error = Some("pane recorder writer closed before accepting bytes".to_string());
                    }
                }
            }
            _ = shutdown.cancelled() => {
                let mut outcome = outcome.lock().expect("record outcome poisoned");
                if outcome.status == PaneRecordStatus::Recording {
                    outcome.status = PaneRecordStatus::Aborted;
                    outcome.error = Some("pane shutdown interrupted recorder backpressure".to_string());
                }
                return;
            }
        }
    }
}

async fn finish_pane_recorders(
    io_state: &Arc<Mutex<PaneIoState>>,
    pane_id: shux_core::model::PaneId,
) {
    let senders: Vec<mpsc::Sender<PaneRecordChunk>> = {
        let state = io_state.lock().await;
        state
            .recorders
            .get(&pane_id)
            .map(|recorders| recorders.iter().map(|r| r.sender.clone()).collect())
            .unwrap_or_default()
    };

    for sender in senders {
        let _ = sender
            .send(PaneRecordChunk::Finish {
                status: PaneRecordStatus::Complete,
            })
            .await;
    }
}

struct PtyTaskControl {
    write_rx: mpsc::Receiver<Vec<u8>>,
    resize_rx: mpsc::Receiver<ResizeRequest>,
    shutdown: tokio_util::sync::CancellationToken,
    done_tx: oneshot::Sender<()>,
}

/// Per-pane async task that owns the PtyHandle and handles both reads and writes.
///
/// Reads from the PTY and feeds VT + CommandEngine. Receives writes from the
/// channel. Receives resize requests on a separate channel and applies them
/// via TIOCSWINSZ + VT resize.
async fn run_pane_pty_task(
    pane_id: shux_core::model::PaneId,
    mut handle: shux_pty::handle::PtyHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    control: PtyTaskControl,
    graph: shux_core::graph::GraphHandle,
) {
    use base64::Engine;
    let PtyTaskControl {
        mut write_rx,
        mut resize_rx,
        shutdown,
        done_tx,
    } = control;
    let mut buf = vec![0u8; 8192];
    // Track the last OSC title we forwarded to the graph so we only
    // call set_pane_osc_title when it actually changes. bash's
    // PROMPT_COMMAND re-emits OSC 2 on every prompt; without this
    // local diff we'd flood the graph + event bus.
    let mut last_osc_title: Option<String> = None;

    // PR 2c — sampled pane.output data-plane publishing.
    //
    // Coalesce PTY chunks into a single broadcast per
    // `output_sample_interval`. Without this rate limit a noisy pane
    // (npm install, cargo build, tail -F) would saturate the data-plane
    // channel and lag every subscriber. Trade-off: subscribers see at
    // most ~10 chunks/sec/pane and `sampled=true` whenever bytes were
    // dropped between intervals.
    let output_sample_interval = std::time::Duration::from_millis(100);
    let mut output_pending: Vec<u8> = Vec::new();
    let mut output_last_published_at = std::time::Instant::now()
        .checked_sub(output_sample_interval)
        .unwrap_or_else(std::time::Instant::now);
    let mut output_dropped_any = false;
    let mut task_exit = PtyTaskExit::Natural;

    loop {
        tokio::select! {
            biased;

            _ = shutdown.cancelled() => {
                tracing::debug!(%pane_id, "PTY task cancelled");
                task_exit = PtyTaskExit::RequestedTeardown;
                break;
            }
            result = handle.read(&mut buf) => {
                match result {
                    Ok(0) => {
                        tracing::debug!(%pane_id, "PTY read EOF");
                        break;
                    }
                    Ok(n) => {
                        let data = &buf[..n];
                        tee_pane_recorders(&io_state, pane_id, data, &shutdown).await;
                        let (pulse, vt_title, bus_opt, terminal_responses) = {
                            let mut state = io_state.lock().await;
                            let (vt_title, terminal_responses, rev_state, alt_switched) =
                                if let Some(vt) = state.vts.get_mut(&pane_id) {
                                    // LENS-R-032/DEC-4: detect an alt-screen
                                    // switch by comparing the PRESENTED alt flag
                                    // across the batch (same presented source as
                                    // grid()/glance — frozen under DEC 2026 sync,
                                    // so a switch is seen at the presented frame,
                                    // never mid-sync). A net-zero enter+leave in
                                    // one batch leaves the flag equal → no switch,
                                    // matching §4.2's "nets to no bump".
                                    let alt_before = vt.is_alternate_screen();
                                    let responses = vt.process_with_responses(data);
                                    let alt_after = vt.is_alternate_screen();
                                    let rev = PaneRevision {
                                        content_revision: vt.content_revision(),
                                        last_mutation_ns: vt.last_mutation_ns(),
                                    };
                                    (
                                        vt.title().map(|s| s.to_string()),
                                        responses,
                                        Some(rev),
                                        alt_before != alt_after,
                                    )
                                } else {
                                    (None, Vec::new(), None, false)
                                };
                            // LENS-R-003: publish in the same critical section as
                            // the grid mutation, once per Class-A batch.
                            if let Some(rev) = rev_state {
                                state.publish_revision(pane_id, rev);
                                // LENS-R-032/DEC-4: an alt-screen switch
                                // invalidates every checkpoint of this pane
                                // (marker at the POST-switch revision,
                                // LENS-R-033).
                                if alt_switched {
                                    state.invalidate_checkpoints(pane_id, rev.content_revision);
                                }
                            }
                            let output = String::from_utf8_lossy(data);
                            let _completed = state.cmd_engine.process_output(pane_id.0, &output);
                            (
                                state.render_pulse.clone(),
                                vt_title,
                                state.event_bus.clone(),
                                terminal_responses,
                            )
                        };

                        let mut response_write_failed = false;
                        for response in &terminal_responses {
                            if let Err(e) = handle.write(response).await {
                                tracing::error!(%pane_id, error = %e, "PTY terminal response write error");
                                response_write_failed = true;
                                break;
                            }
                        }
                        if response_write_failed {
                            break;
                        }
                        if !terminal_responses.is_empty() {
                            if let Err(e) = handle.flush().await {
                                tracing::error!(%pane_id, error = %e, "PTY terminal response flush error");
                                break;
                            }
                        }

                        // Stage these bytes for the next sampled publish.
                        // We cap the buffered chunk at 64KB to avoid
                        // unbounded growth if the sampling interval
                        // races with a huge burst of output — anything
                        // older gets dropped (sampled=true signals that).
                        const MAX_PENDING: usize = 64 * 1024;
                        if output_pending.len() + data.len() > MAX_PENDING {
                            let overflow =
                                output_pending.len() + data.len() - MAX_PENDING;
                            let drop = overflow.min(output_pending.len());
                            output_pending.drain(..drop);
                            output_dropped_any = true;
                        }
                        output_pending.extend_from_slice(data);
                        // Publish if the sample interval has elapsed AND
                        // there's a bus + at least one buffered byte.
                        if let Some(bus) = bus_opt {
                            let now = std::time::Instant::now();
                            if !output_pending.is_empty()
                                && now.duration_since(output_last_published_at)
                                    >= output_sample_interval
                            {
                                // Resolve (window_id, session_id) outside
                                // the io_state lock to avoid holding it
                                // across the broadcast send.
                                let snap = graph.snapshot();
                                let pane = snap.panes.get(&pane_id);
                                if let Some(p) = pane {
                                    let wid = p.window_id;
                                    let sid = snap
                                        .windows
                                        .get(&wid)
                                        .map(|w| w.session_id);
                                    if let Some(sid) = sid {
                                        let chunk = std::mem::take(&mut output_pending);
                                        let b64 =
                                            base64::engine::general_purpose::STANDARD
                                                .encode(&chunk);
                                        bus.publish_pane_output(
                                            pane_id,
                                            wid,
                                            sid,
                                            b64,
                                            output_dropped_any,
                                        );
                                        output_last_published_at = now;
                                        output_dropped_any = false;
                                    }
                                }
                            }
                        }
                        // Wake any attach-render loops outside the lock.
                        // notify_one queues a permit that survives even if
                        // the renderer happens to be mid-render and not
                        // yet awaiting; notify_waiters would silently drop
                        // the wakeup in that window.
                        pulse.notify_one();
                        // Forward OSC 0/2 title changes to the graph
                        // (outside the io_state lock). Don't hold the
                        // mutex across the mpsc send — that's the
                        // deadlock pattern from PR #7. Skip empty
                        // titles entirely; some apps clear with OSC 2
                        // and we don't want a blank border title.
                        if vt_title != last_osc_title {
                            if let Some(t) = vt_title.clone() {
                                if !t.is_empty() {
                                    if let Err(e) =
                                        graph.set_pane_osc_title(pane_id, t).await
                                    {
                                        tracing::warn!(
                                            %pane_id,
                                            error = %e,
                                            "set_pane_osc_title failed",
                                        );
                                    }
                                }
                            }
                            last_osc_title = vt_title;
                        }
                    }
                    Err(e) => {
                        tracing::error!(%pane_id, error = %e, "PTY read error");
                        break;
                    }
                }
            }
            res = write_rx.recv() => {
                let data = match res {
                    Some(d) => d,
                    None => {
                        // Sender dropped -- the pane was destroyed.
                        // Exit so we can kill() the child shell.
                        tracing::debug!(%pane_id, "writer channel closed");
                        task_exit = PtyTaskExit::RequestedTeardown;
                        break;
                    }
                };
                if let Err(e) = handle.write(&data).await {
                    tracing::error!(%pane_id, error = %e, "PTY write error");
                    break;
                }
                if let Err(e) = handle.flush().await {
                    tracing::error!(%pane_id, error = %e, "PTY flush error");
                    break;
                }
            }
            res = resize_rx.recv() => {
                let req = match res {
                    Some(r) => r,
                    None => {
                        tracing::debug!(%pane_id, "resizer channel closed");
                        task_exit = PtyTaskExit::RequestedTeardown;
                        break;
                    }
                };
                if let Err(e) = handle.resize(req.size) {
                    tracing::warn!(%pane_id, error = %e, "PTY resize failed");
                }
                let pulse = {
                    let mut state = io_state.lock().await;
                    let rev_state = if let Some(vt) = state.vts.get_mut(&pane_id) {
                        // P4 convergence round 1 (claude blocker): gate the
                        // invalidation on an ACTUAL dimension change, exactly
                        // like the process branch gates on alt_switched. The
                        // attach render loop re-fans EVERY pane of the active
                        // window at its computed size on attach, client
                        // resize, window switch, and zoom toggle — at an
                        // unchanged client size those are no-op resizes, and
                        // ungated invalidation made merely attaching (or a
                        // same-size pane.set_size) destroy every checkpoint.
                        // §4.2: only a dims change is the Class-A "pane
                        // resize"; only that invalidates (LENS-R-032).
                        let dims_before = (vt.grid().rows(), vt.grid().cols());
                        vt.resize(req.size.rows as usize, req.size.cols as usize);
                        let dims_changed = (vt.grid().rows(), vt.grid().cols()) != dims_before;
                        Some((
                            PaneRevision {
                                content_revision: vt.content_revision(),
                                last_mutation_ns: vt.last_mutation_ns(),
                            },
                            dims_changed,
                        ))
                    } else {
                        None
                    };
                    // LENS-R-003: resize is Class-A — publish the bumped revision.
                    if let Some((rev, dims_changed)) = rev_state {
                        state.publish_revision(pane_id, rev);
                        // LENS-R-032/DEC-4: a REAL resize invalidates every
                        // checkpoint of this pane. Record the marker at the
                        // POST-resize revision (LENS-R-033) BEFORE the ack, so
                        // a synchronous `pane.set_size` caller that immediately
                        // diffs an older checkpoint gets RESIZE_INVALIDATED.
                        // Same-size requests never reach here (dims_changed
                        // false): the frame did not change, checkpoints stay
                        // valid.
                        if dims_changed {
                            state.invalidate_checkpoints(pane_id, rev.content_revision);
                        }
                    }
                    state.render_pulse.clone()
                };
                pulse.notify_one();
                // Fire the ack AFTER vt + render_pulse so a synchronous
                // caller (pane.set_size RPC) is guaranteed that the next
                // pane.snapshot it issues sees the new dimensions.
                if let Some(ack) = req.ack {
                    let _ = ack.send(());
                }
            }
        }
    }

    finish_pane_recorders(&io_state, pane_id).await;

    // Reap the child cleanly so plugins and `events.history` see the
    // real exit code on `pane.exited`. The loop exits for several
    // reasons (EOF, read error, channel close, shutdown cancel); only
    // the EOF / read-error paths leave a still-alive child needing a
    // proper wait, while the channel-close and shutdown paths require
    // an explicit kill before waiting will return. Bound both stages
    // with timeouts so a wedged child can't stall pane teardown.
    let exit_code = if task_exit == PtyTaskExit::RequestedTeardown {
        let _ = handle.terminate();
        match tokio::time::timeout(std::time::Duration::from_millis(500), handle.wait()).await {
            Ok(Ok(status)) => status.code(),
            Ok(Err(e)) => {
                tracing::warn!(%pane_id, error = %e, "PTY child wait after teardown failed");
                None
            }
            Err(_) => {
                let _ = handle.kill();
                match tokio::time::timeout(std::time::Duration::from_secs(1), handle.wait()).await {
                    Ok(Ok(status)) => status.code(),
                    _ => None,
                }
            }
        }
    } else {
        match tokio::time::timeout(std::time::Duration::from_secs(2), handle.wait()).await {
            Ok(Ok(status)) => status.code(),
            Ok(Err(e)) => {
                tracing::warn!(%pane_id, error = %e, "PTY child wait failed");
                None
            }
            Err(_) => {
                // Still alive after 2s — send a PTY-style hangup to the
                // process group, then escalate if it refuses to exit.
                let _ = handle.terminate();
                match tokio::time::timeout(std::time::Duration::from_millis(500), handle.wait())
                    .await
                {
                    Ok(Ok(status)) => status.code(),
                    _ => {
                        let _ = handle.kill();
                        match tokio::time::timeout(std::time::Duration::from_secs(1), handle.wait())
                            .await
                        {
                            Ok(Ok(status)) => status.code(),
                            _ => None,
                        }
                    }
                }
            }
        }
    };

    // Propagate the captured exit code so the daemon's PaneExited
    // event carries it. set_pane_exit_status both updates the pane
    // and fires the lifecycle event with the populated field — the
    // alternative path (graph.destroy_pane via API) fires PaneExited
    // with None, which is the right thing for "killed by user", and
    // the cascade paths in destroy_session/destroy_window do the same.
    if let Some(code) = exit_code {
        if let Err(e) = graph.set_pane_exit_status(pane_id, code).await {
            tracing::debug!(%pane_id, error = %e, "set_pane_exit_status failed (pane may already be gone)");
        }
    }

    // Drop only the PTY-bound handles. The VT (grid + scrollback) stays
    // until the pane is explicitly destroyed via pane.kill / window.kill
    // / session.kill — agents and humans alike need pane.capture and
    // pane.snapshot to keep working against the frozen output of a
    // short-lived command. The Pane's exit_status is the "dead" flag;
    // tmux does the same with its `remain-on-exit` model.
    let mut state = io_state.lock().await;
    state.writers.remove(&pane_id);
    state.resizers.remove(&pane_id);
    state.shutdowns.remove(&pane_id);
    state.pty_done.remove(&pane_id);
    let pulse = state.render_pulse.clone();
    drop(state);
    pulse.notify_one();
    let _ = done_tx.send(());
}

/// Spawn a PTY process and VT instance for a pane.
///
/// When `command` is empty, spawns the user's default login+interactive
/// shell (via `PtyConfig::default_shell`). When non-empty, runs that
/// argv directly — this is what `shux new -s X -- vim foo.rs` lands on,
/// so the pane runs `vim foo.rs` instead of a shell. The pane lifetime
/// becomes the lifetime of that command (when it exits, the pane EOFs).
///
/// `size`/`extra_env` let `lens.run` (P5, LENS-R-040) request a non-default
/// PTY size and environment additions for a scratch pane; every other
/// caller passes `PtySize::default()` / an empty env, which reproduces the
/// exact pre-P5 behavior byte-for-byte (`config.size` defaults to 80×24,
/// same as the VT construction this replaces). Returns the raw
/// `shux_pty::PtyError` (rather than an `RpcError`) so callers can map it
/// to their own error code — `lens.run` needs `SPAWN_FAILED (-32014)`,
/// every other caller keeps mapping to `internal()`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn spawn_pane_pty(
    pane_id: shux_core::model::PaneId,
    cwd: PathBuf,
    command: Vec<String>,
    size: shux_pty::handle::PtySize,
    extra_env: Vec<(String, String)>,
    io_state: Arc<Mutex<PaneIoState>>,
    shutdown: tokio_util::sync::CancellationToken,
    graph: shux_core::graph::GraphHandle,
) -> Result<(), shux_pty::PtyError> {
    let mut config = if command.is_empty() {
        shux_pty::handle::PtyConfig::default_shell(cwd)
    } else {
        shux_pty::handle::PtyConfig::with_command(command, cwd)
    };
    config.size = size;
    config.env = extra_env;
    let handle = shux_pty::handle::PtyHandle::spawn(&config)?;
    let pid = handle.pid();

    let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>(256);
    let (resize_tx, resize_rx) = mpsc::channel::<ResizeRequest>(16);
    let (done_tx, done_rx) = oneshot::channel::<()>();
    let pane_shutdown = shutdown.child_token();
    let vt = shux_vt::VirtualTerminal::new(size.rows as usize, size.cols as usize);
    // LENS-R-003: seed the per-pane revision watch with the VT's initial
    // (content_revision=1, last_mutation_ns=creation time). The initial
    // receiver is dropped; P3 subscribers call `subscribe()` on the stored
    // sender (send_if_modified still updates the retained value with no
    // receivers attached).
    let initial_rev = PaneRevision {
        content_revision: vt.content_revision(),
        last_mutation_ns: vt.last_mutation_ns(),
    };
    let (rev_tx, _rev_rx) = watch::channel(initial_rev);

    {
        let mut state = io_state.lock().await;
        if let Some(old) = state.shutdowns.insert(pane_id, pane_shutdown.clone()) {
            old.cancel();
        }
        state.writers.insert(pane_id, write_tx);
        state.resizers.insert(pane_id, resize_tx);
        state.pty_done.insert(pane_id, done_rx);
        state.vts.insert(pane_id, vt);
        state.revisions.insert(pane_id, rev_tx);
        state.pty_pids.insert(pane_id, pid);
    }

    tokio::spawn(run_pane_pty_task(
        pane_id,
        handle,
        io_state,
        PtyTaskControl {
            write_rx,
            resize_rx,
            shutdown: pane_shutdown,
            done_tx,
        },
        graph,
    ));

    Ok(())
}

fn main() -> anyhow::Result<()> {
    // Inject the colorised agent reference at runtime so it honours
    // NO_COLOR + the IsTerminal piped-stdout check. clap's derive macro
    // only accepts a `&'static str` literal there, so we set it here.
    let cmd = Cli::command()
        .before_help(style::banner())
        .long_about(cli::long_about())
        .after_long_help(cli::agent_help());
    let matches = cmd.get_matches();
    let args = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());

    // Internal daemon subcommand — called by auto-start
    if matches!(args.command, Some(Command::__daemon)) {
        return run_daemon();
    }

    // Normal CLI client mode
    run_client(args)
}

/// Daemon entry point.
///
/// 1. Daemonize (double-fork) — BEFORE tokio runtime
/// 2. Create tokio runtime
/// 3. Set up CancellationToken tree
/// 4. Start signal handlers
/// 5. Bind UDS
/// 6. Run daemon state loop
fn run_daemon() -> anyhow::Result<()> {
    // Step 1: Daemonize BEFORE tokio
    if !daemon::daemonize()? {
        // We are the parent — exit cleanly
        return Ok(());
    }

    // Step 2: Now we are the daemon process — create tokio runtime
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        // Initialize tracing (to file, since stdio is /dev/null)
        // TODO: Set up file-based tracing subscriber
        tracing_subscriber::fmt()
            .with_env_filter("shux=info")
            .with_target(false)
            .init();

        let tokens = shux_core::daemon::ShutdownTokens::new();
        let config_reload_notify = Arc::new(Notify::new());
        let (cmd_tx, cmd_rx) = mpsc::channel(64);

        // Start signal handler
        daemon::spawn_signal_handler(cmd_tx.clone(), tokens.clone()).await?;

        // Ensure runtime dir and clean up stale socket
        let runtime_dir = daemon::ensure_runtime_dir()?;
        daemon::remove_socket_file()?;

        // Daemon-level lens audit log (LENS-R-052): ONE instance for the
        // whole daemon — the chain head is cached in memory, so a second
        // opener would fork the chain. Built before the startup reap so
        // reap(reason=registry) entries chain onto the same log.
        let lens_audit = lens_scratch::LensAuditLog::open_default();

        // Lens scratch registry startup reap (LENS-R-044, DEC-7): scratch
        // sessions never survive a restart. Kill any process groups a prior
        // daemon incarnation left registered BEFORE the RPC server starts
        // accepting `lens.run` calls that would populate a fresh registry.
        let reaped = lens_scratch::ScratchRegistry::startup_reap(&runtime_dir, &lens_audit).await;
        if reaped > 0 {
            tracing::info!(
                reaped,
                "startup: reaped orphaned scratch sessions from a prior daemon"
            );
        }

        // Set up SessionGraph + graph loop
        let sock_path = daemon::socket_path()?;
        let cancel = tokens.root.clone();
        let io_state = run_rpc_server(sock_path, cancel.clone(), lens_audit).await?;

        // Run the daemon state loop (blocks until shutdown)
        shux_core::daemon::run_daemon_state_loop(cmd_rx, tokens.clone(), config_reload_notify)
            .await;

        // Root cancellation is idempotent. Do it here as a final guard,
        // then wait for pane PTY tasks to signal and reap their process
        // groups before the runtime starts dropping tasks.
        tokens.root.cancel();
        shutdown_all_pane_io(io_state).await;

        // Cleanup
        daemon::remove_pid_file()?;
        daemon::remove_socket_file()?;
        tracing::info!("Daemon shut down cleanly");

        Ok::<(), anyhow::Error>(())
    })?;

    Ok(())
}

/// Client entry point — parse CLI args, ensure daemon is running, dispatch.
fn run_client(args: Cli) -> anyhow::Result<()> {
    // Set up logging
    let filter = if args.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::from_default_env()
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    let rt = tokio::runtime::Runtime::new()?;
    let result = rt.block_on(async { dispatch(args).await });
    if let Err(ref e) = result {
        // Format error: strip "RPC error" prefix if present, avoid "error: Error:" duplication
        let msg = format!("{e:#}");
        style::print_error(&msg);
    }
    result
}

/// Start the RPC server with a SessionGraph backing session methods.
///
/// Spawns:
/// 1. The SessionGraph graph loop (single-writer task)
/// 2. The RPC Server accept loop
///
/// Both run until `cancel` is triggered.
async fn run_rpc_server(
    socket_path: PathBuf,
    cancel: tokio_util::sync::CancellationToken,
    lens_audit: Arc<lens_scratch::LensAuditLog>,
) -> anyhow::Result<Arc<Mutex<PaneIoState>>> {
    // EventBus: typed pub/sub for lifecycle events. Wired into SessionGraph
    // so every successful mutation publishes a typed event to subscribers.
    // events.watch / events.history RPC methods read from here.
    let event_bus = shux_core::bus::EventBus::new();

    // Create SessionGraph + graph loop. Pass the bus so mutations fire events.
    let (graph, state) =
        shux_core::graph::SessionGraph::new_with_event_bus(Some(event_bus.clone()));
    let (graph_tx, graph_rx) = mpsc::channel(256);
    let graph_handle = shux_core::graph::GraphHandle::new(graph_tx, state);

    let graph_cancel = cancel.clone();
    tokio::spawn(async move {
        shux_core::graph::run_graph_loop(graph, graph_rx, graph_cancel).await;
    });

    // Create shared pane I/O state (PTY writers, VTs, command engine,
    // data-plane publisher). The event bus is the SAME bus the
    // control-plane events.watch RPC reads — but per-pane output
    // chunks land on its data plane, separate from
    // `events.history`. See `docs/PR2c-DESIGN.md`.
    let io_state = Arc::new(Mutex::new(
        PaneIoState::new().with_event_bus(event_bus.clone()),
    ));
    let shutdown_io_state = io_state.clone();

    // Lens scratch registry (§8 SPEC-E, LENS-R-040..046). One per daemon
    // incarnation — the startup reap in `run_daemon` already cleared any
    // leftover registry file from a prior incarnation before this runs.
    let scratch_registry =
        lens_scratch::ScratchRegistry::new(&daemon::runtime_dir()?, lens_audit.clone());

    // Load user config (~/.config/shux/config.toml). Missing file is
    // valid — defaults match current hardcoded behavior. Spawn a watcher
    // task so edits to the file are picked up live.
    let config_path = shux_core::config::default_config_path();
    let config_handle = shux_core::config::ConfigHandle::load_or_default(&config_path);
    let cfg_watcher_handle = config_handle.clone();
    let cfg_watcher_path = config_path.clone();
    let cfg_watcher_cancel = cancel.clone();
    tokio::spawn(async move {
        shux_core::config::run_hot_reload(cfg_watcher_path, cfg_watcher_handle, cfg_watcher_cancel)
            .await;
    });

    // Status-bar segment cache + runners. One runner task per
    // `[[statusbar.segment]]` in config; restarts when config reloads.
    let segment_cache = statusbar_runner::SegmentCache::new();
    statusbar_runner::spawn_segment_runners(
        config_handle.clone(),
        segment_cache.clone(),
        cancel.clone(),
    );

    // Per-session decorations (git branch, SSH context). Non-persisted,
    // populated on session.create / .ensure, cleared on session.kill.
    // The OOTB status bar reads this on every render — must stay cheap.
    let session_meta_cache = session_meta::SessionMetaCache::new();

    // First-run onboarding state (prefix-discovered, welcome-toast-seen).
    // Single state file under XDG_STATE_HOME loaded once at daemon start.
    let onboarding = onboarding::OnboardingHandle::load();

    // Daemon start instant — drives the "up Nh Nm" segment in the right
    // zone post-hint-dismissal.
    let daemon_start = std::time::Instant::now();

    // Spawn the attach UDS listener (separate socket, dedicated streaming
    // protocol). The JSON-RPC socket below stays request-response.
    let attach_path = daemon::attach_socket_path()?;
    let attach_graph = graph_handle.clone();
    let attach_io = io_state.clone();
    let attach_cancel = cancel.clone();
    let attach_config = config_handle.clone();
    let attach_segments = segment_cache.clone();
    let attach_meta = session_meta_cache.clone();
    let attach_onboarding = onboarding.clone();
    tokio::spawn(async move {
        if let Err(e) = attach::run_attach_server(
            attach_path,
            attach_graph,
            attach_io,
            attach_config,
            attach_segments,
            attach_meta,
            attach_onboarding,
            daemon_start,
            attach_cancel,
        )
        .await
        {
            tracing::error!(error = %e, "attach server error");
        }
    });

    // Spawn timeout checker (1s interval)
    let timeout_io = io_state.clone();
    let timeout_cancel = cancel.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let mut state = timeout_io.lock().await;
                    let _timed_out = state.cmd_engine.check_timeouts();
                }
                _ = timeout_cancel.cancelled() => break,
            }
        }
    });

    // Plugin host (task 044a phase 0). One PluginManager shared by
    // the plugin RPC handlers and every spawned plugin's I/O task.
    // We set the router on it AFTER `.build()` below, breaking the
    // circular dependency (manager holds Arc<OnceCell<Router>>).
    let plugins = shux_plugin::PluginManager::new(event_bus.clone());

    // Build router: system builtins + session + window + pane + pane I/O + lens.run + events + state + plugin methods
    let router = register_plugin_methods(
        register_state_methods(
            register_events_methods(
                lens_scratch::register_lens_run_method(
                    register_pane_io_methods(
                        register_pane_methods(
                            register_window_methods(
                                register_session_methods(
                                    shux_rpc::server::register_builtin_methods(
                                        shux_rpc::Router::builder(),
                                    ),
                                    graph_handle.clone(),
                                    io_state.clone(),
                                    cancel.clone(),
                                    session_meta_cache.clone(),
                                    scratch_registry.clone(),
                                ),
                                graph_handle.clone(),
                                io_state.clone(),
                                cancel.clone(),
                            ),
                            graph_handle.clone(),
                            io_state.clone(),
                            cancel.clone(),
                        ),
                        graph_handle.clone(),
                        io_state.clone(),
                        cancel.clone(),
                        config_handle.clone(),
                        session_meta_cache.clone(),
                        onboarding.clone(),
                        segment_cache.clone(),
                        lens_audit.clone(),
                    ),
                    graph_handle.clone(),
                    io_state.clone(),
                    cancel.clone(),
                    event_bus.clone(),
                    scratch_registry.clone(),
                ),
                event_bus,
            ),
            graph_handle.clone(),
            io_state,
            cancel.clone(),
        ),
        plugins.clone(),
    )
    .build();

    // Startup assertion: every registered RPC method must declare a
    // sensitivity policy. Catches "added a new method, forgot to
    // classify it" at boot. See
    // `docs/designs/permissions/README.md` §9.6.
    router.assert_every_route_has_policy();

    // Plugin → daemon RPC calls dispatch through this router clone.
    // Setting it now (post-build) is what lets plugins call any
    // method registered above. Also wire in the graph handle so the
    // permission enforcer can look up entity ownership.
    plugins.set_router(router.clone());
    plugins.set_graph(graph_handle.clone()).await;

    // LENS-R-052 denial entries: mirror plugin permission DENIALS of the
    // lens methods into the daemon-level lens audit log (the per-plugin
    // audit log records every denial regardless; this adds the lens view).
    // The caller field here is the one place the identity IS known.
    {
        let audit = lens_audit.clone();
        plugins.set_denial_hook(std::sync::Arc::new(move |_name, uuid, method| {
            const LENS_METHODS: [&str; 5] = [
                "pane.glance",
                "pane.wait_settled",
                "pane.checkpoint",
                "pane.diff_since",
                "lens.run",
            ];
            if LENS_METHODS.contains(&method) {
                audit.append(serde_json::json!({
                    "ts": lens_scratch::iso_now(),
                    "caller": format!("plugin:{uuid}"),
                    "method": method,
                    "decision": "deny",
                }));
            }
        }));
    }

    let config = shux_rpc::ServerConfig {
        socket_path,
        tcp_addr: String::new(),
        auth_token: None,
    };

    let server = shux_rpc::Server::new(config, router, cancel);

    tokio::spawn(async move {
        if let Err(e) = server.run().await {
            tracing::error!(error = %e, "RPC server error");
        }
    });

    Ok(shutdown_io_state)
}

async fn shutdown_all_pane_io(io_state: Arc<Mutex<PaneIoState>>) {
    let (done, teardown_waiters) = {
        let mut state = io_state.lock().await;
        let pane_ids: Vec<_> = state
            .shutdowns
            .keys()
            .chain(state.writers.keys())
            .chain(state.resizers.keys())
            .chain(state.pty_done.keys())
            .copied()
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        state
            .teardown_waiters
            .retain(|waiter| !waiter.is_finished());
        let teardown_waiters = std::mem::take(&mut state.teardown_waiters);
        let (pulse, done) = state.teardown_panes_collecting(&pane_ids, true);
        drop(state);
        pulse.notify_waiters();
        (done, teardown_waiters)
    };

    let wait_all = async move {
        for rx in done {
            let _ = rx.await;
        }
        for waiter in teardown_waiters {
            let _ = waiter.await;
        }
    };
    if tokio::time::timeout(Duration::from_secs(3), wait_all)
        .await
        .is_err()
    {
        tracing::warn!("timed out waiting for pane PTY tasks during daemon shutdown");
    }
}

/// Map GraphError to appropriate RPC error codes.
fn graph_error_to_rpc(e: shux_core::graph::GraphError) -> shux_rpc::RpcError {
    use shux_core::graph::GraphError;
    match e {
        GraphError::SessionNotFound(_) => shux_rpc::RpcError::not_found("session", &e.to_string()),
        GraphError::WindowNotFound(_) => shux_rpc::RpcError::not_found("window", &e.to_string()),
        GraphError::PaneNotFound(_) => shux_rpc::RpcError::not_found("pane", &e.to_string()),
        GraphError::SessionNameExists(ref name) => {
            shux_rpc::RpcError::name_conflict("session", name)
        }
        GraphError::WindowNameConflict(ref name) => {
            shux_rpc::RpcError::name_conflict("window", name)
        }
        GraphError::EmptySessionName
        | GraphError::SessionNameTooLong(_)
        | GraphError::InvalidSessionName(_) => shux_rpc::RpcError::invalid_params(&e.to_string()),
        GraphError::EmptyWindowName | GraphError::WindowIndexOutOfRange { .. } => {
            shux_rpc::RpcError::invalid_params(&e.to_string())
        }
        GraphError::LastWindow | GraphError::LastPane => {
            shux_rpc::RpcError::invalid_params(&e.to_string())
        }
        GraphError::PaneSwapSelf | GraphError::PaneCrossWindow | GraphError::NoNeighbor(_) => {
            shux_rpc::RpcError::invalid_params(&e.to_string())
        }
        GraphError::LayoutError(_) => shux_rpc::RpcError::internal(&e.to_string()),
        GraphError::VersionConflict {
            resource,
            ref id,
            expected,
            actual,
        } => shux_rpc::RpcError::version_conflict(resource, id, expected, actual),
        GraphError::Shutdown => shux_rpc::RpcError::internal(&e.to_string()),
    }
}

/// Build session info JSON from a Session, including window/pane IDs.
fn session_to_json(
    s: &shux_core::model::Session,
    snap: &shux_core::graph::SessionGraphSnapshot,
) -> serde_json::Value {
    let window_count = s.windows.len();
    let active_window_id = s.active_window.to_string();

    // Find window_id and pane_id for the first window
    let first_window_id = s.windows.first().map(|w| w.to_string());
    let first_pane_id = s
        .windows
        .first()
        .and_then(|wid| snap.windows.get(wid).map(|w| w.active_pane.to_string()));

    serde_json::json!({
        "id": s.id.to_string(),
        "name": s.name,
        "windows": s.windows.iter().map(|w| w.to_string()).collect::<Vec<_>>(),
        "window_count": window_count,
        "active_window_id": active_window_id,
        "window_id": first_window_id,
        "pane_id": first_pane_id,
        "created_at": s.created_at.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
    })
}

/// Build window info JSON from a Window.
fn window_to_json(
    w: &shux_core::model::Window,
    index: usize,
    is_active: bool,
    snap: &shux_core::graph::SessionGraphSnapshot,
) -> serde_json::Value {
    let pane_count = snap.panes.values().filter(|p| p.window_id == w.id).count();
    serde_json::json!({
        "id": w.id.to_string(),
        "session_id": w.session_id.to_string(),
        "title": w.title,
        "pane_count": pane_count,
        "active_pane_id": w.active_pane.to_string(),
        "index": index,
        "is_active": is_active,
        "version": w.version,
    })
}

/// Build pane info JSON from a Pane.
fn pane_to_json(
    p: &shux_core::model::Pane,
    window: &shux_core::model::Window,
) -> serde_json::Value {
    let is_focused = window.active_pane == p.id;
    let is_zoomed = window.layout.is_zoomed()
        && window
            .layout
            .zoom
            .as_ref()
            .is_some_and(|z| z.zoomed_pane == p.id);
    serde_json::json!({
        "id": p.id.to_string(),
        "window_id": p.window_id.to_string(),
        "title": p.title,
        "manual_title": p.manual_title,
        "osc_title": p.osc_title,
        "auto_title": p.auto_title,
        "cwd": p.cwd.to_string_lossy(),
        "command": p.command,
        "exit_status": p.exit_status,
        "is_focused": is_focused,
        "is_zoomed": is_zoomed,
        "version": p.version,
    })
}

/// Serialize an `Event` for JSON-RPC transport. Includes the typed payload
/// AND meta (seq, timestamp, type) at the top level so consumers can route
/// without recursing into a nested envelope.
fn event_to_json(event: &shux_core::event::Event) -> serde_json::Value {
    event.to_wire_json()
}

/// Register `events.watch` and `events.history` RPC methods.
///
/// `events.watch` is the agent-facing subscription: long-poll style, since
/// the JSON-RPC Handler trait is single-response. The handler:
///   1. Subscribes to the bus FIRST (so concurrent publishes can't slip
///      between history snapshot and subscription start — the race that
///      Codex and Gemini both flagged as the load-bearing correctness
///      requirement).
///   2. Snapshots history from `from_seq` SECOND.
///   3. Drains the subscription with `timeout_ms` until either a matching
///      event arrives or the deadline lapses.
///   4. Returns history + tail, deduped by `seq` (the overlap between the
///      two streams is real — an event published in step 2 might appear in
///      both the history snapshot and the subscription receiver buffer).
///
/// Register `state.apply` RPC method.
///
/// Takes a generic `Op` delta (NOT a TOML template — codex P0 #2: keeping
/// the daemon API agnostic to template grammar means future SDKs / MCP
/// servers / agents can target the same primitive).
///
/// Atomicity: graph-level all-or-nothing. PTY spawns happen AFTER the
/// graph commits and per-pane spawn outcomes are reported in
/// `BatchResult::spawn_results`. Spawn failure does NOT roll back the
/// graph (codex P0 #1: rolling back PTY-spawned commands would mean
/// killing already-launched subprocesses, which has its own side effects;
/// honest reporting beats dishonest atomicity).
fn register_state_methods(
    builder: shux_rpc::RouterBuilder,
    graph: shux_core::graph::GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    cancel: tokio_util::sync::CancellationToken,
) -> shux_rpc::RouterBuilder {
    builder.register_with_policy(
        "state.apply",
        Policy::fixed(Sensitivity::Grantable),
        move |params: Option<serde_json::Value>| {
            let gh = graph.clone();
            let io = io_state.clone();
            let ct = cancel.clone();
            async move {
                // Parse `{ ops: [...] }`.
                let params = params.unwrap_or_default();
                let ops_value = params
                    .get("ops")
                    .cloned()
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'ops' array"))?;
                let ops: Vec<shux_core::apply::Op> =
                    serde_json::from_value(ops_value).map_err(|e| {
                        shux_rpc::RpcError::invalid_params(&format!("ops parse error: {e}"))
                    })?;

                // Run the staged transaction through the single-writer task.
                let mut result = gh.apply_batch(ops).await.map_err(batch_error_to_rpc)?;

                // Graph commit succeeded. Now spawn PTYs for each new pane.
                // Per codex P0 #1: spawn outcomes are reported per-pane in
                // `spawn_results` and do NOT roll back the graph.
                let snap = gh.snapshot();
                let mut spawn_results = Vec::new();
                for output in &result.outputs {
                    if let Some(pane_id) = output.pane_id {
                        if let Some(pane) = snap.panes.get(&pane_id) {
                            let cwd = pane.cwd.clone();
                            let command = pane.command.clone();
                            let spawn_io = io.clone();
                            let spawn_ct = ct.clone();
                            match spawn_pane_pty(
                                pane_id,
                                cwd,
                                command,
                                shux_pty::handle::PtySize::default(),
                                Vec::new(),
                                spawn_io,
                                spawn_ct,
                                gh.clone(),
                            )
                            .await
                            {
                                Ok(()) => spawn_results.push(shux_core::apply::SpawnResult {
                                    op_index: output.op_index,
                                    pane_id,
                                    spawned: true,
                                    error: None,
                                }),
                                Err(e) => spawn_results.push(shux_core::apply::SpawnResult {
                                    op_index: output.op_index,
                                    pane_id,
                                    spawned: false,
                                    error: Some(e.to_string()),
                                }),
                            }
                        }
                    }
                }
                result.spawn_results = spawn_results;

                serde_json::to_value(&result).map_err(|e| {
                    shux_rpc::RpcError::internal(&format!("apply result serialize error: {e}"))
                })
            }
        },
    )
}

/// Map BatchError to an appropriate RPC error.
fn batch_error_to_rpc(e: shux_core::apply::BatchError) -> shux_rpc::RpcError {
    use shux_core::apply::BatchError;
    match e {
        BatchError::Empty => shux_rpc::RpcError::invalid_params("ops array is empty"),
        BatchError::BackRefOutOfRange { .. } | BatchError::BackRefWrongType { .. } => {
            shux_rpc::RpcError::invalid_params(&e.to_string())
        }
        BatchError::OpFailed { source, .. } => graph_error_to_rpc(source),
    }
}

/// Map PluginError → RpcError so plugin RPC handlers report
/// human-readable failures (NotFound + NameConflict reuse the
/// canonical PRD §8.3 error envelopes, everything else is
/// internal).
fn plugin_error_to_rpc(e: shux_plugin::PluginError) -> shux_rpc::RpcError {
    use shux_plugin::PluginError;
    match e {
        PluginError::NotFound(ref name) => shux_rpc::RpcError::not_found("plugin", name),
        PluginError::NameConflict(ref name) => shux_rpc::RpcError::name_conflict("plugin", name),
        PluginError::HandshakeFailed(_) | PluginError::Proto(_) => {
            shux_rpc::RpcError::invalid_params(&e.to_string())
        }
        PluginError::Io(_) => shux_rpc::RpcError::internal(&e.to_string()),
    }
}

/// Plugin RPC surface (task 044a, phase 0).
///
/// - `plugin.install` — spawn a plugin from a `path` (+ optional
///   `args`, `cwd`). Performs the handshake synchronously and
///   returns the resolved `PluginInfo`.
/// - `plugin.list` — snapshot of every running plugin.
/// - `plugin.kill` — graceful shutdown + child cleanup.
fn register_plugin_methods(
    builder: shux_rpc::RouterBuilder,
    plugins: shux_plugin::PluginManager,
) -> shux_rpc::RouterBuilder {
    let p1 = plugins.clone();
    let p2 = plugins.clone();
    let p3 = plugins.clone();
    let p4 = plugins.clone();
    let p5 = plugins.clone();
    let p6 = plugins.clone();
    let p7 = plugins.clone();
    let p8 = plugins;

    builder
        .register_with_policy(
            "plugin.install",
            Policy::fixed(Sensitivity::PluginsForbidden),
            move |params: Option<serde_json::Value>| {
                let mgr = p1.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let path = params
                        .get("path")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'path'"))?;
                    let args: Vec<String> = params
                        .get("args")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();
                    let cwd = params
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .map(PathBuf::from);
                    // Watch defaults to ON — the dogfood loop showed
                    // every iteration without hot reload felt long.
                    // Callers opt out with `"watch": false`.
                    let watch = params
                        .get("watch")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    // Per-install state root (codex P2 review on PR #32):
                    // a daemon shared across project checkouts must
                    // pin each plugin's state to the calling client's
                    // project, not to the daemon's own cwd. The CLI
                    // passes the resolved `.shux/plugins` path here.
                    let state_root = params
                        .get("state_root")
                        .and_then(|v| v.as_str())
                        .map(PathBuf::from);
                    let expected_name = params
                        .get("expected_name")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    let expected_version = params
                        .get("expected_version")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);

                    let source = shux_plugin::PluginSource {
                        path: PathBuf::from(path),
                        args,
                        cwd,
                        watch,
                        state_root,
                        expected_name,
                        expected_version,
                    };
                    let info = mgr.install(source).await.map_err(plugin_error_to_rpc)?;
                    serde_json::to_value(&info).map_err(|e| {
                        shux_rpc::RpcError::internal(&format!("plugin info serialize: {e}"))
                    })
                }
            },
        )
        .register_with_policy("plugin.list", Policy::fixed(Sensitivity::Public), move |_params: Option<serde_json::Value>| {
            let mgr = p2.clone();
            async move {
                let infos = mgr.list().await;
                Ok(serde_json::json!({ "plugins": infos }))
            }
        })
        .register_with_policy("plugin.kill", Policy::fixed(Sensitivity::PluginsForbidden), move |params: Option<serde_json::Value>| {
            let mgr = p3.clone();
            async move {
                let params = params.unwrap_or_default();
                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'name'"))?
                    .to_string();
                mgr.kill(&name).await.map_err(plugin_error_to_rpc)?;
                Ok(serde_json::json!({ "killed": name }))
            }
        })
        .register_with_policy("plugin.reload", Policy::fixed(Sensitivity::PluginsForbidden), move |params: Option<serde_json::Value>| {
            let mgr = p4.clone();
            async move {
                let params = params.unwrap_or_default();
                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'name'"))?
                    .to_string();
                let info = mgr.reload(&name).await.map_err(plugin_error_to_rpc)?;
                serde_json::to_value(&info).map_err(|e| {
                    shux_rpc::RpcError::internal(&format!("plugin info serialize: {e}"))
                })
            }
        })
        .register_with_policy("plugin.grant", Policy::fixed(Sensitivity::PluginsForbidden), move |params: Option<serde_json::Value>| {
            let mgr = p5.clone();
            async move {
                let p = params.unwrap_or_default();
                let plugin = p.get("plugin").and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'plugin'"))?.to_string();
                let method = p.get("method").and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'method'"))?.to_string();
                let target = p.get("target").and_then(|v| v.as_str()).map(String::from);
                let subscribe = p.get("subscribe").and_then(|v| v.as_bool()).unwrap_or(false);
                mgr.grant(&plugin, &method, target.as_deref(), subscribe).await.map_err(plugin_error_to_rpc)?;
                Ok(serde_json::json!({"granted": true, "plugin": plugin, "method": method, "target": target, "subscribe": subscribe}))
            }
        })
        .register_with_policy("plugin.revoke", Policy::fixed(Sensitivity::PluginsForbidden), move |params: Option<serde_json::Value>| {
            let mgr = p6.clone();
            async move {
                let p = params.unwrap_or_default();
                let plugin = p.get("plugin").and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'plugin'"))?.to_string();
                let method = p.get("method").and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'method'"))?.to_string();
                let target = p.get("target").and_then(|v| v.as_str()).map(String::from);
                let subscribe = p.get("subscribe").and_then(|v| v.as_bool()).unwrap_or(false);
                mgr.revoke(&plugin, &method, target.as_deref(), subscribe).await.map_err(plugin_error_to_rpc)?;
                Ok(serde_json::json!({"revoked": true, "plugin": plugin, "method": method, "target": target, "subscribe": subscribe}))
            }
        })
        .register_with_policy("plugin.grants", Policy::fixed(Sensitivity::PluginsForbidden), move |params: Option<serde_json::Value>| {
            let mgr = p7.clone();
            async move {
                let p = params.unwrap_or_default();
                let plugin = p.get("plugin").and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'plugin'"))?.to_string();
                let grants = mgr.grants_for(&plugin).await.map_err(plugin_error_to_rpc)?;
                serde_json::to_value(&grants).map_err(|e| shux_rpc::RpcError::internal(&format!("grants serialize: {e}")))
            }
        })
        .register_with_policy("plugin.audit", Policy::fixed(Sensitivity::PluginsForbidden), move |params: Option<serde_json::Value>| {
            let mgr = p8.clone();
            async move {
                let p = params.unwrap_or_default();
                let plugin = p.get("plugin").and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'plugin'"))?.to_string();
                let tail = p.get("tail").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
                let path = mgr.audit_path(&plugin).await.map_err(plugin_error_to_rpc)?;
                let body = match std::fs::read_to_string(&path) {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                    Err(e) => return Err(shux_rpc::RpcError::internal(&format!("read audit log {}: {e}", path.display()))),
                };
                let mut entries: Vec<serde_json::Value> = body
                    .lines()
                    .filter(|l| !l.is_empty())
                    .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                    .collect();
                if tail > 0 && entries.len() > tail {
                    entries = entries.split_off(entries.len() - tail);
                }
                Ok(serde_json::json!({
                    "plugin": plugin,
                    "path": path.display().to_string(),
                    "entries": entries,
                }))
            }
        })
}

/// `events.history` is a simple bus.history_filtered() wrapper.
fn register_events_methods(
    builder: shux_rpc::RouterBuilder,
    bus: shux_core::bus::EventBus,
) -> shux_rpc::RouterBuilder {
    let bus_watch = bus.clone();
    let bus_hist = bus.clone();
    let bus_pane_output = bus;

    builder
        .register_with_policy(
            "events.watch",
            Policy::param_aware(|params, plugin_id| {
                // Self-namespaced filters are Public — a plugin can
                // always watch its own published events. Anything broader
                // (firehose or other plugins' namespaces) is ContentRead
                // and needs an explicit grant.
                let prefix = format!("plugin.{plugin_id}.");
                let filters = params
                    .and_then(|p| p.get("filters"))
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|f| f.as_str())
                            .map(|s| s.to_string())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if !filters.is_empty() && filters.iter().all(|f| f.starts_with(&prefix)) {
                    Sensitivity::Public
                } else {
                    Sensitivity::ContentRead
                }
            }),
            move |params: Option<serde_json::Value>| {
                let bus = bus_watch.clone();
                async move {
                    let params = params.unwrap_or_default();

                    let from_seq = params.get("from_seq").and_then(|v| v.as_u64());
                    let max_events = params
                        .get("max_events")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as usize)
                        .unwrap_or(100)
                        .min(1000);
                    let timeout_ms = params
                        .get("timeout_ms")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(5_000)
                        .min(30_000);
                    let filters: Vec<String> = params
                        .get("filter")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();

                    // 1. Subscribe FIRST so any publish during step 2 lands in the
                    //    receiver buffer, not the void.
                    let mut sub = bus.subscribe_filtered(filters.clone());

                    // 2. Snapshot history from from_seq.
                    let (history, gap) = match from_seq {
                        Some(s) => {
                            let (events, gap) = bus.events_from_seq(s);
                            let filtered: Vec<_> = if filters.is_empty() {
                                events
                            } else {
                                events
                                    .into_iter()
                                    .filter(|e| filters.iter().any(|f| e.matches_filter(f)))
                                    .collect()
                            };
                            (filtered, gap)
                        }
                        None => (Vec::new(), 0),
                    };

                    let mut collected: Vec<shux_core::event::Event> = history;
                    let mut lagged = false;

                    // 3. Tail: drain up to (max_events - history_len) events from
                    //    the subscription with timeout. If from_seq was None and
                    //    we have no history, block until at least one event or
                    //    timeout. If we already have history, just opportunistically
                    //    grab anything queued without blocking past the deadline.
                    let deadline =
                        tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
                    while collected.len() < max_events {
                        let now = tokio::time::Instant::now();
                        if now >= deadline {
                            break;
                        }
                        let remaining = deadline - now;
                        match tokio::time::timeout(remaining, sub.recv()).await {
                            Ok(Some(shux_core::bus::SubscriptionEvent::Event(e))) => {
                                collected.push(e)
                            }
                            Ok(Some(shux_core::bus::SubscriptionEvent::Lagged(_))) => {
                                // Subscriber fell behind broadcast capacity. Surface
                                // to the client so it knows the stream is degraded.
                                lagged = true;
                                break;
                            }
                            Ok(None) => break, // bus shut down
                            Err(_) => break,   // deadline reached
                        }
                    }

                    // 4. Dedup by seq. History + subscription tail can legitimately
                    //    overlap; the subscription started before history was
                    //    snapshotted, so any event published in between can land in
                    //    both streams.
                    collected.sort_by_key(|e| e.meta.seq);
                    collected.dedup_by_key(|e| e.meta.seq);
                    if collected.len() > max_events {
                        collected.truncate(max_events);
                    }

                    let next_seq = collected
                        .last()
                        .map(|e| e.meta.seq + 1)
                        .or(from_seq)
                        .unwrap_or_else(|| bus.current_seq());

                    let events: Vec<serde_json::Value> =
                        collected.iter().map(event_to_json).collect();

                    Ok(serde_json::json!({
                        "events": events,
                        "next_seq": next_seq,
                        "gap": gap,
                        "lagged": lagged,
                    }))
                }
            },
        )
        .register_with_policy(
            "events.history",
            Policy::fixed(Sensitivity::ContentRead),
            move |params: Option<serde_json::Value>| {
                let bus = bus_hist.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let count = params
                        .get("count")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as usize)
                        .unwrap_or(50)
                        .min(1000);
                    let filters: Vec<String> = params
                        .get("filter")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();

                    let events = bus.history_filtered(count, &filters);
                    let json: Vec<serde_json::Value> = events.iter().map(event_to_json).collect();

                    Ok(serde_json::json!({
                        "events": json,
                        "current_seq": bus.current_seq(),
                    }))
                }
            },
        )
        .register_with_policy(
            "pane.output.watch",
            Policy::fixed(Sensitivity::ContentRead),
            // PR 2c — sampled pane.output data-plane watch.
            //
            // Long-polls the data-plane broadcast channel for chunks
            // matching the given `pane_id`. Unlike `events.watch`,
            // there is no history snapshot — the data plane is
            // intentionally lossy to prevent secret leak via stored
            // PTY bytes and to give control-plane subscribers
            // priority. See `docs/PR2c-DESIGN.md`.
            move |params: Option<serde_json::Value>| {
                let bus = bus_pane_output.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id_str =
                        params
                            .get("pane_id")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| {
                                shux_rpc::RpcError::invalid_params("missing 'pane_id' parameter")
                            })?;
                    let pane_id: shux_core::model::PaneId = pane_id_str.parse().map_err(|_| {
                        shux_rpc::RpcError::invalid_params("invalid pane_id format")
                    })?;
                    let from_seq = params.get("from_seq").and_then(|v| v.as_u64());
                    let timeout_ms = params
                        .get("timeout_ms")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(5_000)
                        .clamp(100, 30_000);
                    let limit = params
                        .get("limit")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as usize)
                        .unwrap_or(50)
                        .min(500);

                    // Subscribe BEFORE returning any chunks so a chunk
                    // published while we're parsing params doesn't get
                    // missed. The data plane has no history, so the
                    // subscribe-first invariant from events.watch
                    // applies even more strictly here.
                    let mut sub = bus.subscribe_pane_output();

                    let mut collected: Vec<shux_core::bus::PaneOutputEvent> = Vec::new();
                    let mut lagged = false;
                    let deadline =
                        tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);

                    while collected.len() < limit {
                        let now = tokio::time::Instant::now();
                        if now >= deadline {
                            break;
                        }
                        let remaining = deadline - now;
                        match tokio::time::timeout(remaining, sub.recv()).await {
                            Ok(Some(shux_core::bus::PaneOutputSubscriptionEvent::Chunk(c))) => {
                                if c.pane_id != pane_id {
                                    continue; // not for this subscriber
                                }
                                if let Some(s) = from_seq {
                                    if c.seq < s {
                                        continue;
                                    }
                                }
                                collected.push(c);
                            }
                            Ok(Some(shux_core::bus::PaneOutputSubscriptionEvent::Lagged(_))) => {
                                lagged = true;
                                break;
                            }
                            Ok(None) => break,
                            Err(_) => break,
                        }
                    }

                    let next_seq = collected
                        .last()
                        .map(|c| c.seq + 1)
                        .or(from_seq)
                        .unwrap_or_else(|| bus.current_data_seq());

                    let chunks: Vec<serde_json::Value> = collected
                        .into_iter()
                        .map(|c| {
                            serde_json::json!({
                                "seq": c.seq,
                                "pane_id": c.pane_id.to_string(),
                                "window_id": c.window_id.to_string(),
                                "session_id": c.session_id.to_string(),
                                "timestamp": c
                                    .timestamp
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_millis() as u64)
                                    .unwrap_or(0),
                                "bytes": c.bytes,
                                "sampled": c.sampled,
                            })
                        })
                        .collect();

                    Ok(serde_json::json!({
                        "chunks": chunks,
                        "next_seq": next_seq,
                        "lagged": lagged,
                    }))
                }
            },
        )
}

/// Register pane operation methods on the router builder.
fn register_pane_methods(
    builder: shux_rpc::RouterBuilder,
    graph: shux_core::graph::GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    cancel: tokio_util::sync::CancellationToken,
) -> shux_rpc::RouterBuilder {
    let g1 = graph.clone();
    let g2 = graph.clone();
    let g3 = graph.clone();
    let g4 = graph.clone();
    let g5 = graph.clone();
    let g6 = graph.clone();
    let g7 = graph.clone();
    let g8 = graph.clone();
    let g9 = graph.clone();

    let io_split = io_state.clone();
    let io_kill = io_state;
    let cancel_split = cancel;

    builder
        .register_with_policy("pane.list", Policy::fixed(Sensitivity::Public), move |params: Option<serde_json::Value>| {
            let gh = g1.clone();
            async move {
                let params = params.unwrap_or_default();

                // Resolve window_id — either provided directly or via session_id + active_window
                let window_id = resolve_window_id_from_params(&gh, &params)?;

                let snap = gh.snapshot();
                let window = snap
                    .windows
                    .get(&window_id)
                    .ok_or_else(|| shux_rpc::RpcError::not_found("window", &window_id.to_string()))?;

                let panes: Vec<serde_json::Value> = snap
                    .panes
                    .values()
                    .filter(|p| p.window_id == window_id)
                    .map(|p| pane_to_json(p, window))
                    .collect();

                Ok(serde_json::json!(panes))
            }
        })
        .register_with_policy("pane.split", Policy::fixed(Sensitivity::OwnedMutation), move |params: Option<serde_json::Value>| {
            let gh = g2.clone();
            let io = io_split.clone();
            let ct = cancel_split.clone();
            async move {
                let params = params.unwrap_or_default();

                // Resolve the target pane — either provided or active pane of window
                let pane_id = resolve_pane_id_from_params(&gh, &params)?;

                let direction = match params.get("direction").and_then(|v| v.as_str()) {
                    Some("horizontal") | Some("h") => shux_core::layout::Direction::Horizontal,
                    Some("vertical") | Some("v") => shux_core::layout::Direction::Vertical,
                    None | Some("auto") => shux_core::layout::Direction::Vertical, // default to vertical
                    Some(other) => {
                        return Err(shux_rpc::RpcError::invalid_params(&format!(
                            "invalid direction '{other}', expected 'horizontal', 'vertical', or 'auto'"
                        )));
                    }
                };

                let ratio = params
                    .get("ratio")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.5) as f32;

                let new_pane_id = gh
                    .split_pane(pane_id, direction, ratio)
                    .await
                    .map_err(graph_error_to_rpc)?;

                let command: Vec<String> = params
                    .get("command")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|s| s.as_str().map(|x| x.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let cwd = params
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"))
                    });
                let _ = spawn_pane_pty(
                    new_pane_id,
                    cwd,
                    command,
                    shux_pty::handle::PtySize::default(),
                    Vec::new(),
                    io,
                    ct,
                    gh.clone(),
                )
                .await;

                let snap = gh.snapshot();
                let new_pane = snap
                    .panes
                    .get(&new_pane_id)
                    .ok_or_else(|| shux_rpc::RpcError::internal("pane not in snapshot"))?;
                let window = snap
                    .windows
                    .get(&new_pane.window_id)
                    .ok_or_else(|| shux_rpc::RpcError::internal("window not in snapshot"))?;

                Ok(serde_json::json!({
                    "pane": pane_to_json(new_pane, window),
                    "split_from": pane_id.to_string(),
                }))
            }
        })
        .register_with_policy("pane.focus", Policy::fixed(Sensitivity::OwnedMutation), move |params: Option<serde_json::Value>| {
            let gh = g3.clone();
            async move {
                let params = params.unwrap_or_default();
                let pane_id_str = params
                    .get("pane_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'pane_id' parameter"))?;

                let pane_id: shux_core::model::PaneId = pane_id_str
                    .parse()
                    .map_err(|_| shux_rpc::RpcError::invalid_params("invalid pane_id format"))?;

                let previous = gh
                    .focus_pane(pane_id)
                    .await
                    .map_err(graph_error_to_rpc)?;

                Ok(serde_json::json!({
                    "pane_id": pane_id.to_string(),
                    "previous_pane_id": previous.map(|id| id.to_string()),
                }))
            }
        })
        .register_with_policy(
            "pane.focus_direction",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g4.clone();
                async move {
                    let params = params.unwrap_or_default();

                    let window_id = resolve_window_id_from_params(&gh, &params)?;

                    let dir_str = params
                        .get("direction")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'direction' parameter")
                        })?;

                    let direction = match dir_str.to_lowercase().as_str() {
                        "up" => shux_core::layout::NavDirection::Up,
                        "down" => shux_core::layout::NavDirection::Down,
                        "left" => shux_core::layout::NavDirection::Left,
                        "right" => shux_core::layout::NavDirection::Right,
                        other => {
                            return Err(shux_rpc::RpcError::invalid_params(&format!(
                                "invalid direction '{other}', expected 'up', 'down', 'left', or 'right'"
                            )));
                        }
                    };

                    // Use a default viewport — the actual viewport would come from the TUI client
                    let viewport = shux_core::layout::Rect::new(0, 0, 120, 40);

                    let snap = gh.snapshot();
                    let window = snap
                        .windows
                        .get(&window_id)
                        .ok_or_else(|| {
                            shux_rpc::RpcError::not_found("window", &window_id.to_string())
                        })?;
                    let previous_pane = window.active_pane;

                    let target = gh
                        .focus_pane_direction(window_id, direction, viewport)
                        .await
                        .map_err(graph_error_to_rpc)?;

                    match target {
                        Some(pane_id) => Ok(serde_json::json!({
                            "pane_id": pane_id.to_string(),
                            "previous_pane_id": previous_pane.to_string(),
                        })),
                        None => Err(shux_rpc::RpcError::invalid_params(&format!(
                            "no neighbor pane in direction {dir_str}"
                        ))),
                    }
                }
            },
        )
        .register_with_policy("pane.resize", Policy::fixed(Sensitivity::OwnedMutation), move |params: Option<serde_json::Value>| {
            let gh = g5.clone();
            async move {
                let params = params.unwrap_or_default();
                let pane_id = resolve_pane_id_from_params(&gh, &params)?;

                let dir_str = params
                    .get("direction")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        shux_rpc::RpcError::invalid_params("missing 'direction' parameter")
                    })?;

                let direction = match dir_str.to_lowercase().as_str() {
                    "horizontal" | "h" => shux_core::layout::Direction::Horizontal,
                    "vertical" | "v" => shux_core::layout::Direction::Vertical,
                    other => {
                        return Err(shux_rpc::RpcError::invalid_params(&format!(
                            "invalid direction '{other}', expected 'horizontal' or 'vertical'"
                        )));
                    }
                };

                let delta = params
                    .get("delta")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.1) as f32;

                let expected_version = parse_expected_version(&params)?;

                gh.resize_pane(pane_id, direction, delta, expected_version)
                    .await
                    .map_err(graph_error_to_rpc)?;

                Ok(serde_json::json!({ "pane_id": pane_id.to_string() }))
            }
        })
        .register_with_policy("pane.zoom", Policy::fixed(Sensitivity::OwnedMutation), move |params: Option<serde_json::Value>| {
            let gh = g6.clone();
            async move {
                let params = params.unwrap_or_default();
                let pane_id = resolve_pane_id_from_params(&gh, &params)?;
                let expected_version = parse_expected_version(&params)?;

                let is_zoomed = gh
                    .zoom_pane(pane_id, expected_version)
                    .await
                    .map_err(graph_error_to_rpc)?;

                Ok(serde_json::json!({
                    "pane_id": pane_id.to_string(),
                    "is_zoomed": is_zoomed,
                }))
            }
        })
        .register_with_policy("pane.swap", Policy::fixed(Sensitivity::OwnedMutation), move |params: Option<serde_json::Value>| {
            let gh = g7.clone();
            async move {
                let params = params.unwrap_or_default();
                let pane_id_str = params
                    .get("pane_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'pane_id' parameter"))?;
                let target_str = params
                    .get("target_pane_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        shux_rpc::RpcError::invalid_params("missing 'target_pane_id' parameter")
                    })?;

                let pane_a: shux_core::model::PaneId = pane_id_str
                    .parse()
                    .map_err(|_| shux_rpc::RpcError::invalid_params("invalid pane_id format"))?;
                let pane_b: shux_core::model::PaneId = target_str
                    .parse()
                    .map_err(|_| shux_rpc::RpcError::invalid_params("invalid target_pane_id format"))?;

                let expected_version = parse_expected_version(&params)?;

                gh.swap_panes(pane_a, pane_b, expected_version)
                    .await
                    .map_err(graph_error_to_rpc)?;

                Ok(serde_json::json!({
                    "pane_a": pane_a.to_string(),
                    "pane_b": pane_b.to_string(),
                }))
            }
        })
        .register_with_policy("pane.kill", Policy::fixed(Sensitivity::OwnedMutation), move |params: Option<serde_json::Value>| {
            let gh = g8.clone();
            let io = io_kill.clone();
            async move {
                let params = params.unwrap_or_default();
                let pane_id_str = params
                    .get("pane_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'pane_id' parameter"))?;

                let pane_id: shux_core::model::PaneId = pane_id_str
                    .parse()
                    .map_err(|_| shux_rpc::RpcError::invalid_params("invalid pane_id format"))?;

                let expected_version = parse_expected_version(&params)?;

                // Order-of-operations matters: destroy_pane() can return
                // LastPane (refusing to remove the only pane in a window).
                // If we tear down writers/resizers/vts FIRST and then the
                // graph mutation fails, the pane stays in the graph but
                // its IO state is gone — the session ends up with an
                // active pane that has no PTY. Mutate the graph first;
                // only purge IO state on success. Same reason
                // expected_version is checked inside destroy_pane — a stale
                // version must error out BEFORE we touch IO state.
                gh.destroy_pane(pane_id, expected_version)
                    .await
                    .map_err(graph_error_to_rpc)?;
                {
                    let mut state = io.lock().await;
                    let pulse = state.teardown_panes(&[pane_id], true);
                    drop(state);
                    pulse.notify_one();
                }

                Ok(serde_json::json!({ "killed": pane_id_str }))
            }
        })
        .register_with_policy(
            "pane.set_title",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g9.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;
                    // `title: null` clears the manual override, letting
                    // OSC + command-derived auto-titles flow back into
                    // the displayed pane title. `title: "text"` pins it.
                    // Omitted entirely leaves the manual title unchanged
                    // — useful when toggling only `auto`.
                    let title: Option<Option<String>> = match params.get("title") {
                        Some(serde_json::Value::Null) => Some(None),
                        Some(serde_json::Value::String(s)) => Some(Some(s.clone())),
                        Some(other) => {
                            return Err(shux_rpc::RpcError::invalid_params(&format!(
                                "'title' must be string or null, got {other}"
                            )));
                        }
                        None => None,
                    };
                    let auto = match params.get("auto") {
                        Some(serde_json::Value::Bool(b)) => Some(*b),
                        Some(serde_json::Value::Null) | None => None,
                        Some(other) => {
                            return Err(shux_rpc::RpcError::invalid_params(&format!(
                                "'auto' must be boolean or null, got {other}"
                            )));
                        }
                    };
                    // If neither was provided, the caller is asking us
                    // to do nothing — surface that as invalid_params
                    // rather than a silent success.
                    if title.is_none() && auto.is_none() {
                        return Err(shux_rpc::RpcError::invalid_params(
                            "must provide at least one of 'title' or 'auto'",
                        ));
                    }
                    // `title: None` (omitted) → don't touch manual_title.
                    // `title: Some(None)` (explicit null) → clear it.
                    // `title: Some(Some(...))` → set it.
                    let title_arg = title.unwrap_or_else(|| {
                        // Caller only set `auto`; leave manual_title alone.
                        // Re-read the current value so set_pane_title's
                        // unconditional set_manual_title doesn't wipe it.
                        gh.snapshot()
                            .panes
                            .get(&pane_id)
                            .and_then(|p| p.manual_title.clone())
                    });
                    gh.set_pane_title(pane_id, title_arg, auto)
                        .await
                        .map_err(graph_error_to_rpc)?;
                    let snap = gh.snapshot();
                    let pane = snap.panes.get(&pane_id).ok_or_else(|| {
                        shux_rpc::RpcError::internal("pane vanished after set_title")
                    })?;
                    Ok(serde_json::json!({
                        "pane_id": pane_id.to_string(),
                        "title": pane.title,
                        "auto_title": pane.auto_title,
                        "manual_title": pane.manual_title,
                        "osc_title": pane.osc_title,
                    }))
                }
            },
        )
}

/// Extract optional `expected_version` from RPC params (PR 3b — optimistic
/// concurrency). Returns `Ok(None)` when the field is absent or null,
/// `Ok(Some(v))` when it's a valid non-negative integer, and an
/// `invalid_params` RpcError if it's the wrong type or out of range. The
/// daemon then plumbs the Option through to SessionGraph mutations, which
/// reject the request with `version_conflict` (-32002) if the entity has
/// moved since the client last read it.
fn parse_expected_version(params: &serde_json::Value) -> Result<Option<u64>, shux_rpc::RpcError> {
    match params.get("expected_version") {
        None => Ok(None),
        Some(v) if v.is_null() => Ok(None),
        Some(v) => v.as_u64().map(Some).ok_or_else(|| {
            shux_rpc::RpcError::invalid_params("'expected_version' must be a non-negative integer")
        }),
    }
}

/// Extract optional initial pane title from session.create/session.ensure params.
fn parse_initial_pane_title(
    params: &serde_json::Value,
) -> Result<Option<String>, shux_rpc::RpcError> {
    match params.get("pane_title") {
        None => Ok(None),
        Some(v) if v.is_null() => Ok(None),
        Some(v) => {
            let title = v.as_str().ok_or_else(|| {
                shux_rpc::RpcError::invalid_params("'pane_title' must be a string")
            })?;
            if title.trim().is_empty() {
                return Err(shux_rpc::RpcError::invalid_params(
                    "'pane_title' must not be empty",
                ));
            }
            Ok(Some(title.to_string()))
        }
    }
}

fn initial_pane_id_for_session(
    snap: &shux_core::graph::SessionGraphSnapshot,
    session_id: shux_core::model::SessionId,
) -> Result<shux_core::model::PaneId, shux_rpc::RpcError> {
    let session = snap
        .sessions
        .get(&session_id)
        .ok_or_else(|| shux_rpc::RpcError::internal("session vanished after create"))?;
    let window_id = session
        .windows
        .first()
        .ok_or_else(|| shux_rpc::RpcError::internal("created session has no windows"))?;
    let window = snap
        .windows
        .get(window_id)
        .ok_or_else(|| shux_rpc::RpcError::internal("initial window vanished after create"))?;

    window
        .layout
        .tree
        .pane_ids()
        .into_iter()
        .next()
        .ok_or_else(|| shux_rpc::RpcError::internal("initial window has no panes"))
}

async fn set_initial_pane_title(
    gh: &shux_core::graph::GraphHandle,
    session_id: shux_core::model::SessionId,
    title: Option<String>,
) -> Result<(), shux_rpc::RpcError> {
    let Some(title) = title else {
        return Ok(());
    };

    let pane_id = {
        let snap = gh.snapshot();
        initial_pane_id_for_session(&snap, session_id)?
    };

    gh.set_pane_title(pane_id, Some(title), None)
        .await
        .map_err(graph_error_to_rpc)
}

/// Resolve a pane_id from params: either explicit `pane_id` or active pane of resolved window.
fn resolve_pane_id_from_params(
    gh: &shux_core::graph::GraphHandle,
    params: &serde_json::Value,
) -> Result<shux_core::model::PaneId, shux_rpc::RpcError> {
    if let Some(pane_id_str) = params.get("pane_id").and_then(|v| v.as_str()) {
        return pane_id_str
            .parse()
            .map_err(|_| shux_rpc::RpcError::invalid_params("invalid pane_id format"));
    }

    // Fall back to active pane of the resolved window
    let window_id = resolve_window_id_from_params(gh, params)?;
    let snap = gh.snapshot();
    let window = snap
        .windows
        .get(&window_id)
        .ok_or_else(|| shux_rpc::RpcError::not_found("window", &window_id.to_string()))?;
    Ok(window.active_pane)
}

/// Resolve a window_id from params: either explicit `window_id` or active window of session.
fn resolve_window_id_from_params(
    gh: &shux_core::graph::GraphHandle,
    params: &serde_json::Value,
) -> Result<shux_core::model::WindowId, shux_rpc::RpcError> {
    if let Some(wid_str) = params.get("window_id").and_then(|v| v.as_str()) {
        return wid_str
            .parse()
            .map_err(|_| shux_rpc::RpcError::invalid_params("invalid window_id format"));
    }

    // Resolve from session
    let session_id_str = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            shux_rpc::RpcError::invalid_params(
                "missing 'pane_id' or 'window_id' or 'session_id' parameter",
            )
        })?;

    let session_id: shux_core::model::SessionId = session_id_str
        .parse()
        .map_err(|_| shux_rpc::RpcError::invalid_params("invalid session_id format"))?;

    let snap = gh.snapshot();
    let session = snap
        .sessions
        .get(&session_id)
        .ok_or_else(|| shux_rpc::RpcError::not_found("session", session_id_str))?;

    Ok(session.active_window)
}

/// Build the OOTB status bar for a snapshot frame, using the same
/// `statusbar_build::build` renderer the live attach path uses so the
/// PNG matches what a fresh attached client would see.
///
/// The snapshot path doesn't have a live attach context, so we
/// synthesize a `StatusBarCtx` with defaults that mirror "fresh OOTB
/// experience": onboarding state read from the daemon-loaded handle,
/// session_meta read from the cache, no live `last_action`, no
/// copy-mode flag, no daemon-uptime (snapshots are stateless).
///
/// `segments` carries the latest script-driven `[[statusbar.segment]]`
/// outputs; `populate_bar` appends them into the same StatusBar the
/// attach loop assembles, so PNG snapshots match what an attached
/// client renders.
#[allow(clippy::too_many_arguments)]
async fn build_snapshot_status_bar(
    snap: &shux_core::graph::SessionGraphSnapshot,
    session_id: &shux_core::model::SessionId,
    window_id: shux_core::model::WindowId,
    cols: u16,
    config: &shux_core::config::ConfigHandle,
    meta_cache: &session_meta::SessionMetaCache,
    onboarding: &onboarding::OnboardingHandle,
    segments: &statusbar_runner::SegmentCache,
) -> shux_ui::StatusBar {
    let theme = {
        let cfg = config.current();
        shux_core::theme::Theme::resolve(&cfg.theme)
    };
    let live_cfg = config.current();
    let nerd_fonts = live_cfg.appearance.nerd_fonts;
    let prefix_label = statusbar_build::prefix_display(&live_cfg.keys.prefix);
    let session_meta = meta_cache.get(*session_id).await;
    let onboarding_state = onboarding.current().await;

    // The active pane id is what the live attach path would show as
    // the focus. For the snapshot we read from the graph.
    let active_pane_id = snap
        .windows
        .get(&window_id)
        .map(|w| w.active_pane)
        .unwrap_or_default();

    let session_name = snap
        .sessions
        .get(session_id)
        .map(|s| s.name.clone())
        .unwrap_or_default();

    let ctx = statusbar_build::StatusBarCtx {
        session_id: *session_id,
        session_name: &session_name,
        active_window_id: window_id,
        active_pane_id,
        session_meta: &session_meta,
        onboarding: &onboarding_state,
        daemon_uptime: std::time::Duration::from_secs(0),
        nerd_fonts,
        prefix_label: &prefix_label,
        client_cols: cols,
        copy_mode_active: false,
        last_action: None,
    };
    // Bridge the cold-start race the attach path doesn't suffer from:
    // when a snapshot fires right after daemon start or a config reload,
    // the runner tasks may not have completed their first tick yet, so
    // `populate_bar` would read an empty cache and silently emit no
    // segments. Wait up to 1.2s for every configured segment index to
    // have a cache entry; on timeout we proceed anyway so a slow / hung
    // command can't wedge the RPC. The 1.2s budget slightly exceeds the
    // runner's per-command 1s timeout so the runner's fallback-bytes
    // write has room to land before we give up (codex round-4 nit).
    // Codex-bot P2, PR #45.
    let segment_count = live_cfg.statusbar.segment.len();
    if segment_count > 0 {
        let _ = segments
            .wait_for_first_outputs(segment_count, std::time::Duration::from_millis(1200))
            .await;
    }
    let mut bar = statusbar_build::build(snap, &theme, &ctx);
    // Append script-driven `[[statusbar.segment]]` outputs the same
    // way the attach render loop does. Without this, PNG snapshots
    // would only show the built-in OOTB segments and silently drop
    // every user-configured segment.
    statusbar_runner::populate_bar(&mut bar, config, segments).await;
    bar
}

/// Tail-clip captured text for inclusion in wait_for response previews.
/// Keeps the LAST `n` chars (matches are usually near the bottom of the
/// captured viewport) and trims leading whitespace.
fn preview_for_log(s: &str, n: usize) -> String {
    let bytes = s.as_bytes();
    if bytes.len() <= n {
        return s.trim_start().to_string();
    }
    let start = bytes.len() - n;
    let mut s = std::str::from_utf8(&bytes[start..])
        .unwrap_or("")
        .to_string();
    if let Some(idx) = s.find('\n') {
        s = s.split_off(idx + 1);
    }
    s.trim_start().to_string()
}

/// Parse optional `cols` / `rows` from snapshot params. Defaults: 120x36.
/// Same range guard as `pane.set_size`.
fn parse_snapshot_dims(params: &serde_json::Value) -> Result<(u16, u16), shux_rpc::RpcError> {
    let cols = params.get("cols").and_then(|v| v.as_u64()).unwrap_or(120);
    let rows = params.get("rows").and_then(|v| v.as_u64()).unwrap_or(36);
    if !(4..=1000).contains(&cols) || !(2..=1000).contains(&rows) {
        return Err(shux_rpc::RpcError::invalid_params(&format!(
            "rows/cols out of range (got rows={rows} cols={cols}; \
             valid: 4..=1000 cols, 2..=1000 rows)"
        )));
    }
    Ok((cols as u16, rows as u16))
}

/// One pane's render-clone payload, captured under the pane-IO lock:
/// (id, grid clone with resolved defaults, cursor, dynamic default colors).
type PaneSnapshotData = (
    shux_core::model::PaneId,
    shux_vt::Grid,
    shux_vt::Cursor,
    shux_vt::TerminalDefaultColors,
);

/// Per-pane lens ContentRevision map, captured in the SAME io-lock critical
/// section as the grid clones (PR #87 bot P1: same-lock, no VT-side tear).
type PaneRevisions = std::collections::HashMap<shux_core::model::PaneId, u64>;

/// Compose every pane in `window_id` into a single ComposedFrame at
/// `cols × rows`, rasterize it, and return the JSON `pane.snapshot`-shaped
/// response (with `window_id` in place of `pane_id`).
#[allow(clippy::too_many_arguments)]
async fn snapshot_window(
    // The caller's graph snapshot — taken ONCE and shared with any metadata
    // the caller derives (lens council P1 major 5: session.snapshot's
    // session_version/panes[] and the rendered window must come from the
    // same snapshot, or concurrent structural mutation yields torn output).
    snap: &shux_core::graph::SessionGraphSnapshot,
    io: &Arc<Mutex<PaneIoState>>,
    window_id: shux_core::model::WindowId,
    cols: u16,
    rows: u16,
    rasterizer: Arc<shux_raster::Rasterizer>,
    config: &shux_core::config::ConfigHandle,
    meta_cache: &session_meta::SessionMetaCache,
    onboarding: &onboarding::OnboardingHandle,
    segments: &statusbar_runner::SegmentCache,
    // Pane ids whose `content_revision` must be captured in the SAME io-lock
    // critical section as the VT grid clones (PR #87 bot P1: a second lock
    // read at T2 let an old PNG pair with a newer revision). Returned as the
    // second tuple element; pass `&[]` when revisions aren't needed.
    revision_panes: &[shux_core::model::PaneId],
) -> Result<(serde_json::Value, PaneRevisions), shux_rpc::RpcError> {
    let (cw, ch) = rasterizer.cell_size();
    let pixel_count = (cols as u64)
        .saturating_mul(cw as u64)
        .saturating_mul(rows as u64)
        .saturating_mul(ch as u64);
    const MAX_PIXELS: u64 = 16_000_000;
    if pixel_count > MAX_PIXELS {
        return Err(shux_rpc::RpcError::invalid_params(&format!(
            "snapshot would be {pixel_count} pixels — exceeds cap of {MAX_PIXELS}"
        )));
    }

    let window = snap
        .windows
        .get(&window_id)
        .ok_or_else(|| shux_rpc::RpcError::not_found("window", &window_id.to_string()))?;

    // Build per-pane title map from the graph (priority-resolved values).
    let mut titles: std::collections::HashMap<shux_core::model::PaneId, String> =
        std::collections::HashMap::new();
    for pid in window.layout.tree.pane_ids() {
        if let Some(p) = snap.panes.get(&pid) {
            if !p.title.is_empty() {
                titles.insert(pid, p.title.clone());
            }
        }
    }

    // Snapshot just the (Grid, Cursor, dynamic colors) per pane under the io
    // lock — VT itself isn't Clone and we want to release the lock before
    // rasterizing. The caller's `revision_panes` content_revisions are read
    // inside the SAME critical section so the rendered pixels and the
    // published revisions are provably same-lock (no VT-side tear).
    let (pane_data, revisions): (Vec<PaneSnapshotData>, PaneRevisions) = {
        let state = io.lock().await;
        let pane_data = window
            .layout
            .tree
            .pane_ids()
            .into_iter()
            .filter_map(|pid| {
                state.vts.get(&pid).map(|vt| {
                    let mut grid = vt.grid().clone();
                    let default_colors = vt.default_colors();
                    resolve_grid_default_colors(&mut grid, default_colors);
                    (pid, grid, vt.cursor().clone(), default_colors)
                })
            })
            .collect();
        let revisions = revision_panes
            .iter()
            .filter_map(|pid| state.vts.get(pid).map(|vt| (*pid, vt.content_revision())))
            .collect();
        (pane_data, revisions)
    };

    let focused = window.active_pane;
    let layout_tree = window.layout.tree.clone();
    let zoom_state = window.layout.zoom.clone();

    // Build the same status bar `shux attach` would render so the snapshot
    // matches what a user sees attached. We don't have the live attached
    // state here, so we synthesize the StatusBarCtx with snapshot-time
    // defaults — every signal that does have a daemon-side source
    // (git branch, onboarding hint, theme, nerd-fonts toggle) IS still
    // populated, so PNGs honestly reflect the OOTB experience.
    let status_bar = build_snapshot_status_bar(
        snap,
        &window.session_id,
        window_id,
        cols,
        config,
        meta_cache,
        onboarding,
        segments,
    )
    .await;
    const STATUS_BAR_ROWS: u16 = 1;

    let (img, png_buf) = tokio::task::spawn_blocking(move || {
        let panes: std::collections::HashMap<
            shux_core::model::PaneId,
            (&shux_vt::Grid, &shux_vt::Cursor),
        > = pane_data.iter().map(|(p, g, c, _)| (*p, (g, c))).collect();
        let focused_defaults = pane_data
            .iter()
            .find(|(pid, _, _, _)| *pid == focused)
            .map(|(_, _, _, defaults)| *defaults)
            .unwrap_or_default();
        let focused_cursor_shape = pane_data
            .iter()
            .find(|(pid, _, _, _)| *pid == focused)
            .map(|(_, _, cursor, _)| cursor.shape)
            .unwrap_or_default();
        let inputs = shux_ui::ComposeInputs {
            layout: &layout_tree,
            zoom: zoom_state.as_ref(),
            focused,
            panes: &panes,
            titles: Some(&titles),
            status_bar: Some(&status_bar),
        };
        let composed = shux_ui::compose(
            &inputs,
            cols,
            rows,
            shux_ui::BorderStyle::Rounded,
            shux_ui::BorderColors::default(),
            STATUS_BAR_ROWS,
        );
        let opts = shux_raster::RasterOptions {
            cursor: composed.cursor,
            cursor_shape: focused_cursor_shape,
            cursor_color: focused_defaults.cursor,
            ..Default::default()
        };
        let img = rasterizer.render(&composed.grid, &opts);
        let mut buf: Vec<u8> = Vec::with_capacity(128 * 1024);
        {
            use image::ImageEncoder;
            let encoder = image::codecs::png::PngEncoder::new(&mut buf);
            encoder
                .write_image(
                    img.as_raw(),
                    img.width(),
                    img.height(),
                    image::ExtendedColorType::Rgba8,
                )
                .map_err(|e| format!("PNG encode failed: {e}"))?;
        }
        Ok::<_, String>((img, buf))
    })
    .await
    .map_err(|e| shux_rpc::RpcError::internal(&format!("rasterize join: {e}")))?
    .map_err(|e| shux_rpc::RpcError::internal(&e))?;

    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png_buf);

    Ok((
        serde_json::json!({
            "window_id": window_id.to_string(),
            "png_base64": b64,
            "width": img.width(),
            "height": img.height(),
            "cell_width": cw,
            "cell_height": ch,
            "cols": cols,
            "rows": rows,
            "format": "png",
        }),
        revisions,
    ))
}

fn resolve_grid_default_colors(grid: &mut shux_vt::Grid, defaults: shux_vt::TerminalDefaultColors) {
    if defaults.fg.is_none() && defaults.bg.is_none() {
        return;
    }
    for row_idx in 0..grid.rows() {
        let mut row = grid.visible_row_mut(row_idx);
        for col_idx in 0..row.len() {
            let cell = &mut row[col_idx];
            if cell.style.fg == shux_vt::Color::Default
                && let Some([r, g, b]) = defaults.fg
            {
                cell.style.fg = shux_vt::Color::Rgb(r, g, b);
            }
            if cell.style.bg == shux_vt::Color::Default
                && let Some([r, g, b]) = defaults.bg
            {
                cell.style.bg = shux_vt::Color::Rgb(r, g, b);
            }
        }
    }
}

/// Register window CRUD methods on the router builder.
fn register_window_methods(
    builder: shux_rpc::RouterBuilder,
    graph: shux_core::graph::GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    cancel: tokio_util::sync::CancellationToken,
) -> shux_rpc::RouterBuilder {
    let g1 = graph.clone();
    let g2 = graph.clone();
    let g3 = graph.clone();
    let g4 = graph.clone();
    let g5 = graph.clone();
    let g6 = graph.clone();
    let g7 = graph.clone();

    let io_create = io_state.clone();
    let io_ensure = io_state.clone();
    let io_kill = io_state;
    let cancel_create = cancel.clone();
    let cancel_ensure = cancel.clone();

    builder
        .register_with_policy(
            "window.create",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g1.clone();
                let io = io_create.clone();
                let ct = cancel_create.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let session_id_str = params
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'session_id' parameter")
                        })?;

                    let session_id: shux_core::model::SessionId =
                        session_id_str.parse().map_err(|_| {
                            shux_rpc::RpcError::invalid_params("invalid session_id format")
                        })?;

                    let name = params.get("name").and_then(|v| v.as_str());

                    // Auto-generate window name if not provided
                    let title = match name {
                        Some(n) => n.to_string(),
                        None => {
                            let snap = gh.snapshot();
                            let session = snap.sessions.get(&session_id).ok_or_else(|| {
                                shux_rpc::RpcError::not_found("session", session_id_str)
                            })?;
                            let mut idx = session.windows.len();
                            loop {
                                let candidate = format!("{idx}");
                                if !snap.window_name_exists_in_session(&session_id, &candidate) {
                                    break candidate;
                                }
                                idx += 1;
                            }
                        }
                    };

                    let cwd = params
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .map(PathBuf::from)
                        .unwrap_or_else(|| {
                            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"))
                        });

                    let command: Vec<String> = params
                        .get("command")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|s| s.as_str().map(|x| x.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();

                    let window_id = gh
                        .create_window(session_id, title, cwd.clone())
                        .await
                        .map_err(graph_error_to_rpc)?;

                    let snap = gh.snapshot();
                    let window = snap
                        .windows
                        .get(&window_id)
                        .ok_or_else(|| shux_rpc::RpcError::internal("window not in snapshot"))?;
                    let session = snap
                        .sessions
                        .get(&window.session_id)
                        .ok_or_else(|| shux_rpc::RpcError::internal("session not in snapshot"))?;
                    let index = session
                        .windows
                        .iter()
                        .position(|id| *id == window_id)
                        .unwrap_or(0);
                    let is_active = session.active_window == window_id;
                    let pane_id = window.active_pane.to_string();

                    // Spawn PTY for the new pane
                    let _ = spawn_pane_pty(
                        window.active_pane,
                        cwd,
                        command,
                        shux_pty::handle::PtySize::default(),
                        Vec::new(),
                        io,
                        ct,
                        gh.clone(),
                    )
                    .await;

                    let mut result = window_to_json(window, index, is_active, &snap);
                    // Include pane_id at top level for convenience
                    result["pane_id"] = serde_json::Value::String(pane_id);

                    Ok(result)
                }
            },
        )
        .register_with_policy(
            "window.list",
            Policy::fixed(Sensitivity::Public),
            move |params: Option<serde_json::Value>| {
                let gh = g2.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let session_id_str = params
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'session_id' parameter")
                        })?;

                    let session_id: shux_core::model::SessionId =
                        session_id_str.parse().map_err(|_| {
                            shux_rpc::RpcError::invalid_params("invalid session_id format")
                        })?;

                    let snap = gh.snapshot();
                    let session = snap
                        .sessions
                        .get(&session_id)
                        .ok_or_else(|| shux_rpc::RpcError::not_found("session", session_id_str))?;

                    let windows: Vec<serde_json::Value> = session
                        .windows
                        .iter()
                        .enumerate()
                        .filter_map(|(index, wid)| {
                            snap.windows.get(wid).map(|w| {
                                window_to_json(w, index, session.active_window == *wid, &snap)
                            })
                        })
                        .collect();

                    Ok(serde_json::json!(windows))
                }
            },
        )
        .register_with_policy(
            "window.ensure",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g3.clone();
                let io = io_ensure.clone();
                let ct = cancel_ensure.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let session_id_str = params
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'session_id' parameter")
                        })?;

                    let session_id: shux_core::model::SessionId =
                        session_id_str.parse().map_err(|_| {
                            shux_rpc::RpcError::invalid_params("invalid session_id format")
                        })?;

                    let name = params
                        .get("name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'name' parameter")
                        })?
                        .to_string();

                    // Check if window with this name already exists
                    let snap = gh.snapshot();
                    if let Some(w) = snap.find_window_by_name(&session_id, &name) {
                        let session = snap.sessions.get(&session_id).ok_or_else(|| {
                            shux_rpc::RpcError::not_found("session", session_id_str)
                        })?;
                        let index = session
                            .windows
                            .iter()
                            .position(|id| *id == w.id)
                            .unwrap_or(0);
                        let is_active = session.active_window == w.id;
                        let mut result = window_to_json(w, index, is_active, &snap);
                        result["created"] = serde_json::Value::Bool(false);
                        return Ok(result);
                    }

                    let cwd = params
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .map(PathBuf::from)
                        .unwrap_or_else(|| {
                            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"))
                        });
                    let command: Vec<String> = params
                        .get("command")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|s| s.as_str().map(|x| x.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();
                    let window_id = gh
                        .create_window(session_id, name, cwd.clone())
                        .await
                        .map_err(graph_error_to_rpc)?;

                    let snap = gh.snapshot();
                    let window = snap
                        .windows
                        .get(&window_id)
                        .ok_or_else(|| shux_rpc::RpcError::internal("window not in snapshot"))?;

                    // Spawn PTY for the new pane
                    let _ = spawn_pane_pty(
                        window.active_pane,
                        cwd,
                        command,
                        shux_pty::handle::PtySize::default(),
                        Vec::new(),
                        io,
                        ct,
                        gh.clone(),
                    )
                    .await;

                    let session = snap
                        .sessions
                        .get(&session_id)
                        .ok_or_else(|| shux_rpc::RpcError::internal("session not in snapshot"))?;
                    let index = session
                        .windows
                        .iter()
                        .position(|id| *id == window_id)
                        .unwrap_or(0);
                    let is_active = session.active_window == window_id;
                    let mut result = window_to_json(window, index, is_active, &snap);
                    result["created"] = serde_json::Value::Bool(true);
                    Ok(result)
                }
            },
        )
        .register_with_policy(
            "window.rename",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g4.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let window_id_str =
                        params.get("id").and_then(|v| v.as_str()).ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'id' parameter")
                        })?;

                    let window_id: shux_core::model::WindowId =
                        window_id_str.parse().map_err(|_| {
                            shux_rpc::RpcError::invalid_params("invalid window id format")
                        })?;

                    let new_title = params
                        .get("name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'name' parameter")
                        })?
                        .to_string();

                    let expected_version = parse_expected_version(&params)?;

                    gh.rename_window(window_id, new_title, expected_version)
                        .await
                        .map_err(graph_error_to_rpc)?;

                    let snap = gh.snapshot();
                    let window = snap.windows.get(&window_id).ok_or_else(|| {
                        shux_rpc::RpcError::internal("window vanished after rename")
                    })?;
                    let session = snap
                        .sessions
                        .get(&window.session_id)
                        .ok_or_else(|| shux_rpc::RpcError::internal("session not in snapshot"))?;
                    let index = session
                        .windows
                        .iter()
                        .position(|id| *id == window_id)
                        .unwrap_or(0);
                    let is_active = session.active_window == window_id;
                    Ok(window_to_json(window, index, is_active, &snap))
                }
            },
        )
        .register_with_policy(
            "window.focus",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g5.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let window_id_str =
                        params.get("id").and_then(|v| v.as_str()).ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'id' parameter")
                        })?;

                    let window_id: shux_core::model::WindowId =
                        window_id_str.parse().map_err(|_| {
                            shux_rpc::RpcError::invalid_params("invalid window id format")
                        })?;

                    let expected_version = parse_expected_version(&params)?;

                    let previous = gh
                        .focus_window(window_id, expected_version)
                        .await
                        .map_err(graph_error_to_rpc)?;

                    let snap = gh.snapshot();
                    let window = snap.windows.get(&window_id).ok_or_else(|| {
                        shux_rpc::RpcError::internal("window vanished after focus")
                    })?;
                    let session = snap
                        .sessions
                        .get(&window.session_id)
                        .ok_or_else(|| shux_rpc::RpcError::internal("session not in snapshot"))?;
                    let index = session
                        .windows
                        .iter()
                        .position(|id| *id == window_id)
                        .unwrap_or(0);
                    let mut result = window_to_json(window, index, true, &snap);
                    result["previous_window_id"] = match previous {
                        Some(id) => serde_json::Value::String(id.to_string()),
                        None => serde_json::Value::Null,
                    };
                    Ok(result)
                }
            },
        )
        .register_with_policy(
            "window.reorder",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g6.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let window_id_str =
                        params.get("id").and_then(|v| v.as_str()).ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'id' parameter")
                        })?;

                    let window_id: shux_core::model::WindowId =
                        window_id_str.parse().map_err(|_| {
                            shux_rpc::RpcError::invalid_params("invalid window id format")
                        })?;

                    let new_index = params
                        .get("new_index")
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'new_index' parameter")
                        })? as usize;

                    let expected_version = parse_expected_version(&params)?;

                    gh.reorder_window(window_id, new_index, expected_version)
                        .await
                        .map_err(graph_error_to_rpc)?;

                    let snap = gh.snapshot();
                    let window = snap.windows.get(&window_id).ok_or_else(|| {
                        shux_rpc::RpcError::internal("window vanished after reorder")
                    })?;
                    let session = snap
                        .sessions
                        .get(&window.session_id)
                        .ok_or_else(|| shux_rpc::RpcError::internal("session not in snapshot"))?;
                    let index = session
                        .windows
                        .iter()
                        .position(|id| *id == window_id)
                        .unwrap_or(0);
                    let is_active = session.active_window == window_id;
                    Ok(window_to_json(window, index, is_active, &snap))
                }
            },
        )
        .register_with_policy(
            "window.kill",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g7.clone();
                let io = io_kill.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let window_id_str =
                        params.get("id").and_then(|v| v.as_str()).ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'id' parameter")
                        })?;

                    let window_id: shux_core::model::WindowId =
                        window_id_str.parse().map_err(|_| {
                            shux_rpc::RpcError::invalid_params("invalid window id format")
                        })?;

                    let expected_version = parse_expected_version(&params)?;

                    // Snapshot pane IDs BEFORE mutation so we can tear down IO
                    // after the destroy succeeds. Mutate the graph first so a
                    // stale `expected_version` (or LastWindow refusal) errors
                    // out without leaving the window with orphaned VTs/PTYs.
                    let pane_ids: Vec<_> = {
                        let snap = gh.snapshot();
                        snap.panes
                            .values()
                            .filter(|p| p.window_id == window_id)
                            .map(|p| p.id)
                            .collect()
                    };

                    gh.destroy_window(window_id, expected_version)
                        .await
                        .map_err(graph_error_to_rpc)?;

                    {
                        let mut state = io.lock().await;
                        let pulse = state.teardown_panes(&pane_ids, true);
                        drop(state);
                        pulse.notify_one();
                    }

                    Ok(serde_json::json!({ "killed": window_id_str }))
                }
            },
        )
}

/// Register session CRUD methods on the router builder.
///
/// These methods use a `GraphHandle` to interact with the SessionGraph.
/// They are registered here (in the binary crate) because shux-rpc
/// intentionally does not depend on shux-core.
fn register_session_methods(
    builder: shux_rpc::RouterBuilder,
    graph: shux_core::graph::GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    cancel: tokio_util::sync::CancellationToken,
    meta_cache: session_meta::SessionMetaCache,
    scratch_registry: lens_scratch::ScratchRegistry,
) -> shux_rpc::RouterBuilder {
    let g1 = graph.clone();
    let g2 = graph.clone();
    let g3 = graph.clone();
    let g4 = graph.clone();
    let g5 = graph.clone();
    let g6 = graph.clone();

    let io_create = io_state.clone();
    let io_kill = io_state.clone();
    let io_ensure = io_state;
    let cancel_create = cancel.clone();
    let cancel_ensure = cancel;

    let meta_create = meta_cache.clone();
    let meta_kill = meta_cache.clone();
    let meta_ensure = meta_cache;

    let scratch_list = scratch_registry.clone();
    let scratch_kill = scratch_registry;

    builder
        .register_with_policy(
            "session.list",
            Policy::fixed(Sensitivity::Public),
            move |params: Option<serde_json::Value>| {
                let gh = g1.clone();
                let registry = scratch_list.clone();
                async move {
                    // LENS-R-041: scratch sessions are excluded from the
                    // default listing; `include_scratch: true` reveals them
                    // flagged `scratch: true`. Visibility is not
                    // authorization — an id is always resolvable directly
                    // (session.kill/snapshot/etc. never consult this filter).
                    let include_scratch = params
                        .as_ref()
                        .and_then(|p| p.get("include_scratch"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let scratch_ids = registry.ids();

                    let snap = gh.snapshot();
                    let mut sessions: Vec<_> = snap.sessions.values().collect();
                    sessions.sort_by_key(|s| s.created_at);
                    let sessions: Vec<serde_json::Value> = sessions
                        .iter()
                        .filter(|s| include_scratch || !scratch_ids.contains(&s.id))
                        .map(|s| {
                            let mut json = session_to_json(s, &snap);
                            if include_scratch {
                                json["scratch"] =
                                    serde_json::Value::Bool(scratch_ids.contains(&s.id));
                            }
                            json
                        })
                        .collect();
                    Ok(serde_json::json!({ "sessions": sessions }))
                }
            },
        )
        .register_with_policy(
            "session.create",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g2.clone();
                let io = io_create.clone();
                let ct = cancel_create.clone();
                let meta = meta_create.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let name = params
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    // Optional pane command. Accepts:
                    //   {"command": ["vim", "foo.rs"]}     — preferred (passthrough)
                    //   {"command": "top"}                 — convenience: split on whitespace
                    //   omitted / null                     — spawn the user's default shell
                    let command: Vec<String> = match params.get("command") {
                        Some(serde_json::Value::Array(arr)) => arr
                            .iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect(),
                        Some(serde_json::Value::String(s)) if !s.trim().is_empty() => {
                            s.split_whitespace().map(|s| s.to_string()).collect()
                        }
                        _ => Vec::new(),
                    };
                    let pane_title = parse_initial_pane_title(&params)?;

                    // Auto-generate name if not provided (None).
                    // Explicit empty string (Some("")) flows through to validation.
                    let name = match name {
                        Some(n) => n,
                        None => {
                            let snap = gh.snapshot();
                            let mut idx = snap.sessions.len();
                            loop {
                                let candidate = format!("session-{idx}");
                                if !snap.session_name_exists(&candidate) {
                                    break candidate;
                                }
                                idx += 1;
                            }
                        }
                    };

                    let cwd = params
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .map(PathBuf::from)
                        .unwrap_or_else(|| {
                            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"))
                        });

                    // PR followup (codex P2 #10): persist `command` onto
                    // the initial pane so subscribers + auto-title see
                    // the truth. Pre-followup this RPC stored an empty
                    // `Pane.command` and only the PTY layer knew about
                    // the user's --cmd arg.
                    match gh
                        .create_session_with_command(name, cwd.clone(), command.clone())
                        .await
                    {
                        Ok(session_id) => {
                            set_initial_pane_title(&gh, session_id, pane_title).await?;

                            // Populate session-meta cache: git branch from
                            // the spawn cwd, SSH context from the daemon
                            // env. spawn_blocking because detect_git_branch
                            // shells out to `git`; using async tokio for
                            // this would force the runtime to wait for git.
                            let meta_cache_clone = meta.clone();
                            let cwd_for_meta = cwd.clone();
                            tokio::task::spawn_blocking(move || {
                                let branch = session_meta::detect_git_branch(&cwd_for_meta);
                                let over_ssh = session_meta::detect_over_ssh();
                                let snapshot = session_meta::SessionMeta {
                                    git_branch: branch,
                                    over_ssh,
                                };
                                // Tiny tokio block to write the cache —
                                // SessionMetaCache.set is async because the
                                // inner RwLock is tokio::sync.
                                tokio::runtime::Handle::current().block_on(async move {
                                    meta_cache_clone.set(session_id, snapshot).await;
                                });
                            });

                            let snap = gh.snapshot();
                            // Spawn PTY for the initial pane
                            if let Some(s) = snap.sessions.get(&session_id) {
                                if let Some(wid) = s.windows.first() {
                                    if let Some(w) = snap.windows.get(wid) {
                                        let _ = spawn_pane_pty(
                                            w.active_pane,
                                            cwd,
                                            command.clone(),
                                            shux_pty::handle::PtySize::default(),
                                            Vec::new(),
                                            io,
                                            ct,
                                            gh.clone(),
                                        )
                                        .await;
                                    }
                                }
                                Ok(session_to_json(s, &snap))
                            } else {
                                Ok(serde_json::json!({
                                    "id": session_id.to_string(),
                                }))
                            }
                        }
                        Err(e) => Err(graph_error_to_rpc(e)),
                    }
                }
            },
        )
        .register_with_policy(
            "session.kill",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g3.clone();
                let io = io_kill.clone();
                let meta = meta_kill.clone();
                let registry = scratch_kill.clone();
                async move {
                    let params = params.unwrap_or_default();

                    // Accept name or id — try UUID parse first, fall back to name lookup
                    let session_id = if let Some(id_str) = params.get("id").and_then(|v| v.as_str())
                    {
                        let parsed: shux_core::model::SessionId = id_str.parse().map_err(|_| {
                            shux_rpc::RpcError::invalid_params("invalid session ID format")
                        })?;
                        // Verify it exists
                        let snap = gh.snapshot();
                        if !snap.sessions.contains_key(&parsed) {
                            return Err(shux_rpc::RpcError::not_found("session", id_str));
                        }
                        parsed
                    } else if let Some(name) = params.get("name").and_then(|v| v.as_str()) {
                        let snap = gh.snapshot();
                        let session = snap
                            .find_session_by_name(name)
                            .ok_or_else(|| shux_rpc::RpcError::not_found("session", name))?;
                        session.id
                    } else {
                        return Err(shux_rpc::RpcError::invalid_params(
                            "missing 'name' or 'id' parameter",
                        ));
                    };

                    // Snapshot the session BEFORE destroying it so we know
                    // which panes belong to it. After destroy_session the
                    // graph entries are gone; without this snapshot we'd
                    // have no way to find the orphaned PTY tasks to clean up.
                    let pre_snap = gh.snapshot();
                    let name = pre_snap
                        .sessions
                        .get(&session_id)
                        .map(|s| s.name.clone())
                        .unwrap_or_default();
                    let pane_ids: Vec<shux_core::model::PaneId> = pre_snap
                        .sessions
                        .get(&session_id)
                        .map(|s| {
                            s.windows
                                .iter()
                                .flat_map(|wid| {
                                    pre_snap
                                        .windows
                                        .get(wid)
                                        .map(|w| w.layout.tree.pane_ids())
                                        .unwrap_or_default()
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    drop(pre_snap);

                    let expected_version = parse_expected_version(&params)?;

                    // Mutate the graph FIRST. If destroy_session errors, we
                    // leave PTY/VT state untouched (otherwise we'd kill PTYs
                    // for a session that's still in the graph). Same applies
                    // to a stale `expected_version` — the check inside
                    // destroy_session rejects the request before IO teardown.
                    gh.destroy_session(session_id, expected_version)
                        .await
                        .map_err(graph_error_to_rpc)?;

                    // Tear down every pane that belonged to the session.
                    // The explicit per-pane shutdown token is the hard
                    // lifecycle contract; writer/resizer removal is only
                    // bookkeeping. The PTY task prioritizes cancellation
                    // and signals the pane's process group before reaping,
                    // so rich TUIs do not survive as unreachable children.
                    {
                        let mut state = io.lock().await;
                        let pulse = state.teardown_panes(&pane_ids, true);
                        drop(state);
                        pulse.notify_one();
                    }

                    meta.remove(session_id).await;

                    // LENS-R-042c: explicit session.kill reaps a scratch
                    // session IMMEDIATELY (no post_exit_ttl_ms wait) — this
                    // is a no-op for ordinary sessions (registry lookup
                    // misses). For scratch it enforces the LENS-R-042 kill
                    // sequence + death confirmation before the registry row
                    // is dropped (P5 round-1 codex B3).
                    lens_scratch::on_session_killed(&registry, &io, session_id).await;

                    Ok(serde_json::json!({ "killed": name }))
                }
            },
        )
        .register_with_policy(
            "session.ensure",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g4.clone();
                let io = io_ensure.clone();
                let ct = cancel_ensure.clone();
                let meta = meta_ensure.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let name = params
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("default")
                        .to_string();

                    // Optional pane command (same shape as session.create).
                    let command: Vec<String> = match params.get("command") {
                        Some(serde_json::Value::Array(arr)) => arr
                            .iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect(),
                        Some(serde_json::Value::String(s)) if !s.trim().is_empty() => {
                            s.split_whitespace().map(|s| s.to_string()).collect()
                        }
                        _ => Vec::new(),
                    };
                    let pane_title = parse_initial_pane_title(&params)?;

                    // Check if session already exists
                    let snap = gh.snapshot();
                    if let Some(s) = snap.find_session_by_name(&name) {
                        let mut json = session_to_json(s, &snap);
                        json["created"] = serde_json::Value::Bool(false);
                        return Ok(json);
                    }

                    let cwd = params
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .map(PathBuf::from)
                        .unwrap_or_else(|| {
                            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"))
                        });

                    match gh
                        .create_session_with_command(name, cwd.clone(), command.clone())
                        .await
                    {
                        Ok(session_id) => {
                            set_initial_pane_title(&gh, session_id, pane_title).await?;

                            // Populate session-meta cache (git branch, SSH).
                            // Same pattern as session.create above.
                            let meta_cache_clone = meta.clone();
                            let cwd_for_meta = cwd.clone();
                            tokio::task::spawn_blocking(move || {
                                let branch = session_meta::detect_git_branch(&cwd_for_meta);
                                let over_ssh = session_meta::detect_over_ssh();
                                let snapshot = session_meta::SessionMeta {
                                    git_branch: branch,
                                    over_ssh,
                                };
                                tokio::runtime::Handle::current().block_on(async move {
                                    meta_cache_clone.set(session_id, snapshot).await;
                                });
                            });

                            let snap = gh.snapshot();
                            // Spawn PTY for the initial pane
                            if let Some(s) = snap.sessions.get(&session_id) {
                                if let Some(wid) = s.windows.first() {
                                    if let Some(w) = snap.windows.get(wid) {
                                        let _ = spawn_pane_pty(
                                            w.active_pane,
                                            cwd,
                                            command.clone(),
                                            shux_pty::handle::PtySize::default(),
                                            Vec::new(),
                                            io,
                                            ct,
                                            gh.clone(),
                                        )
                                        .await;
                                    }
                                }
                                let mut json = session_to_json(s, &snap);
                                json["created"] = serde_json::Value::Bool(true);
                                Ok(json)
                            } else {
                                Ok(serde_json::json!({
                                    "id": session_id.to_string(),
                                    "created": true,
                                }))
                            }
                        }
                        Err(e) => Err(graph_error_to_rpc(e)),
                    }
                }
            },
        )
        .register_with_policy(
            "session.export_template",
            Policy::fixed(Sensitivity::Public),
            move |params: Option<serde_json::Value>| {
                let gh = g6.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let snap = gh.snapshot();
                    let session_id = if let Some(id) = params.get("id").and_then(|v| v.as_str()) {
                        id.parse::<shux_core::model::SessionId>()
                            .map_err(|_| shux_rpc::RpcError::invalid_params("invalid session id"))?
                    } else if let Some(name) = params.get("name").and_then(|v| v.as_str()) {
                        snap.find_session_by_name(name)
                            .ok_or_else(|| shux_rpc::RpcError::not_found("session", name))?
                            .id
                    } else {
                        return Err(shux_rpc::RpcError::invalid_params(
                            "missing 'name' or 'id' parameter",
                        ));
                    };
                    let template = session_persist::export_session_template(&snap, session_id)
                        .map_err(|e| shux_rpc::RpcError::internal(&format!("{e}")))?;
                    Ok(serde_json::json!({ "template": template }))
                }
            },
        )
        .register_with_policy(
            "session.rename",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g5.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let new_name = params
                        .get("new_name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'new_name' parameter")
                        })?
                        .to_string();

                    // Resolve session by name or id
                    let session_id = if let Some(name) = params.get("name").and_then(|v| v.as_str())
                    {
                        let snap = gh.snapshot();
                        let session = snap
                            .find_session_by_name(name)
                            .ok_or_else(|| shux_rpc::RpcError::not_found("session", name))?;
                        session.id
                    } else if let Some(id_str) = params.get("id").and_then(|v| v.as_str()) {
                        id_str.parse().map_err(|_| {
                            shux_rpc::RpcError::invalid_params("invalid session ID format")
                        })?
                    } else {
                        return Err(shux_rpc::RpcError::invalid_params(
                            "missing 'name' or 'id' parameter",
                        ));
                    };

                    let expected_version = parse_expected_version(&params)?;

                    gh.rename_session(session_id, new_name, expected_version)
                        .await
                        .map_err(graph_error_to_rpc)?;

                    let snap = gh.snapshot();
                    if let Some(s) = snap.sessions.get(&session_id) {
                        Ok(session_to_json(s, &snap))
                    } else {
                        Err(shux_rpc::RpcError::internal(
                            "session vanished after rename",
                        ))
                    }
                }
            },
        )
}

enum SnapshotFontBytes {
    Static(&'static [u8]),
    Owned(Vec<u8>),
}

impl SnapshotFontBytes {
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Static(bytes) => bytes,
            Self::Owned(bytes) => bytes,
        }
    }
}

/// Build the snapshot rasterizer from current config.
///
/// - `appearance.font` unset → bundled JBM-NF primary + default fallback chain.
/// - `appearance.font` set + file readable + font parseable → that
///   font as primary, then configured/default fallbacks.
/// - `appearance.font` set BUT unreadable or unparseable → returns
///   `Err`. The hot-reload caller's `Err` branch keeps the last-good
///   rasterizer; the startup caller logs the error and falls back to the
///   bundled chain so snapshot RPCs still return PNGs.
/// - `appearance.font_fallbacks` omitted → default builtin fallback
///   tokens. Set explicitly → exact ordered fallback chain after the
///   primary font. Empty lists are rejected. When `appearance.font` is
///   unset, the bundled JBM-NF font remains the primary metrics anchor
///   and the explicit list is used strictly as glyph fallback coverage.
///
/// Council review (PR #46): the previous behaviour silently fell back
/// to the bundled chain on bad custom-font paths, contradicting the
/// "keep last good rasterizer" comment in the hot-reload spawn and
/// making the `Err` branch of the reload loop unreachable for the
/// most common failure mode.
fn build_snapshot_rasterizer(
    cfg: &shux_core::config::Config,
) -> Result<shux_raster::Rasterizer, shux_raster::RasterError> {
    let primary = match cfg.appearance.font.as_ref() {
        None => None,
        Some(path) => Some(std::fs::read(path).map_err(|e| {
            shux_raster::RasterError::Font(format!(
                "appearance.font: read {} failed: {e}",
                path.display()
            ))
        })?),
    };
    let explicit_fallback_specs = cfg.appearance.font_fallbacks.clone();
    if explicit_fallback_specs.as_ref().is_some_and(Vec::is_empty) {
        return Err(shux_raster::RasterError::Font(
            "appearance.font_fallbacks must not be empty; omit it to use the default fallback chain"
                .into(),
        ));
    }
    let mut fallback_specs = explicit_fallback_specs.unwrap_or_else(|| {
        shux_raster::DEFAULT_FALLBACK_FONT_SPECS
            .iter()
            .map(|spec| (*spec).to_string())
            .collect()
    });
    let mut bundled_primary = None;
    if primary.is_none() {
        bundled_primary = Some(
            shux_raster::builtin_font_bytes(shux_raster::BUILTIN_NERD_FONT)
                .expect("builtin nerd font token should resolve"),
        );
        if fallback_specs
            .first()
            .is_some_and(|spec| spec == shux_raster::BUILTIN_NERD_FONT)
        {
            fallback_specs.remove(0);
        }
    }
    let fallback_fonts: Vec<SnapshotFontBytes> = fallback_specs
        .iter()
        .map(|spec| {
            if let Some(bytes) = shux_raster::builtin_font_bytes(spec) {
                Ok(SnapshotFontBytes::Static(bytes))
            } else if spec.starts_with("builtin:") {
                Err(shux_raster::RasterError::Font(format!(
                    "appearance.font_fallbacks: unknown builtin font token {spec:?}; expected one of {}",
                    shux_raster::DEFAULT_FALLBACK_FONT_SPECS.join(", ")
                )))
            } else {
                std::fs::read(spec)
                    .map(SnapshotFontBytes::Owned)
                    .map_err(|e| {
                        shux_raster::RasterError::Font(format!(
                            "appearance.font_fallbacks: read {spec} failed: {e}"
                        ))
                    })
            }
        })
        .collect::<Result<_, _>>()?;

    let fallback_refs = fallback_fonts.iter().map(SnapshotFontBytes::as_slice);
    let primary_ref = primary.as_deref().or(bundled_primary);
    shux_raster::Rasterizer::with_primary_and_fallback_fonts(14.0, primary_ref, fallback_refs)
}

/// Cheap equality-key for the subset of config that affects the
/// rasterizer chain. Returning the same value across two config
/// reloads means we can skip the rebuild — most reloads (border
/// styles, theme tweaks, statusbar segments) don't touch fonts.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SnapshotFontKey {
    primary: Option<std::path::PathBuf>,
    fallbacks: Option<Vec<String>>,
}

fn snapshot_font_key(cfg: &shux_core::config::Config) -> SnapshotFontKey {
    SnapshotFontKey {
        primary: cfg.appearance.font.clone(),
        fallbacks: cfg.appearance.font_fallbacks.clone(),
    }
}

// ── `pane.wait_settled` settle math (§6 SPEC-C; pure, unit-tested) ────────
//
// LENS-R-025 parameter bounds. Kept as named constants so the RPC handler and
// the unit tests share one source of truth.
const SETTLE_QUIET_MIN_MS: u64 = 10;
const SETTLE_QUIET_MAX_MS: u64 = 60_000;
const SETTLE_TIMEOUT_MAX_MS: u64 = 600_000;

/// LENS-R-020: a pane is settled once it has been quiet for `quiet_ms`, i.e.
/// `monotonic_now_ns − last_mutation_ns ≥ quiet_ms × 1_000_000`. The unit
/// conversion is EXPLICIT and both sides are nanoseconds — the ns↔ms mixup is
/// the councils-caught deadline-math bug class. `saturating_*` keeps a clock
/// that briefly reads below `last_mutation_ns` (never happens on a monotonic
/// clock, but cheap insurance) from underflowing into "settled".
fn settle_is_quiet(now_ns: u64, last_mutation_ns: u64, quiet_ms: u64) -> bool {
    now_ns.saturating_sub(last_mutation_ns) >= quiet_ms.saturating_mul(1_000_000)
}

/// Nanoseconds of quiet still owed before settle (0 once already settled). Used
/// to size the event-driven sleep so it is never shorter than the remaining
/// deadline (LENS-R-021: no polling).
fn settle_remaining_quiet_ns(now_ns: u64, last_mutation_ns: u64, quiet_ms: u64) -> u64 {
    quiet_ms
        .saturating_mul(1_000_000)
        .saturating_sub(now_ns.saturating_sub(last_mutation_ns))
}

/// One wake of the `pane.wait_settled` loop, decided as a pure function so the
/// precedence rules are unit-testable (codex P3 B1 + claude TOCTOU guard).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettleWake {
    /// Quiet window satisfied on a snapshot with no pending revision.
    Settled,
    /// Quiet still unsatisfied and the timeout deadline has elapsed.
    TimedOut,
    /// Keep waiting (or restart evaluation on a fresh snapshot).
    KeepWaiting,
}

/// Decide one wake of the settle loop. Priority order is the whole fix:
///
/// 1. `pending_revision` (claude P3 TOCTOU guard): a revision published AFTER
///    the `borrow_and_update` snapshot was taken must RESTART evaluation on
///    the fresh value — never settle on the stale snapshot, never report a
///    stale revision in a timeout.
/// 2. `quiet` (codex P3 B1: quiet precedence): on ANY wake — sleep expiry,
///    watch wake, or late scheduler wake — a satisfied quiet window returns
///    `settled:true` even if the timeout deadline has ALSO elapsed. With
///    `timeout_ms == quiet_ms` allowed (LENS-R-025 lower bound), a pane quiet
///    exactly at the shared deadline must settle, not time out.
/// 3. `past_timeout`: only when quiet is still false may the deadline expire
///    the wait (`settled:false` — a RESULT, not an error; DEC-19).
fn settle_decide(quiet: bool, past_timeout: bool, pending_revision: bool) -> SettleWake {
    if pending_revision {
        return SettleWake::KeepWaiting;
    }
    if quiet {
        return SettleWake::Settled;
    }
    if past_timeout {
        return SettleWake::TimedOut;
    }
    SettleWake::KeepWaiting
}

/// Strict optional-u64 parameter parse (codex P3 M2): absent → default;
/// PRESENT but not an unsigned integer (string `"5ms"`, float `5.5`, `null`,
/// negative) → INVALID_PARAMS (-32602). The previous `and_then(as_u64)
/// .unwrap_or(default)` silently replaced mistyped values with the default —
/// a caller sending `quiet_ms: "5ms"` got a 300 ms wait instead of an error.
fn settle_u64_param(
    params: &serde_json::Value,
    key: &str,
    default: u64,
) -> Result<u64, shux_rpc::RpcError> {
    match params.get(key) {
        None => Ok(default),
        Some(v) => v.as_u64().ok_or_else(|| {
            shux_rpc::RpcError::invalid_params(&format!(
                "{key} must be an unsigned integer of milliseconds, got {v}"
            ))
        }),
    }
}

/// The settle waiter's watch sender dropped mid-wait (codex P3 M1): pane
/// teardown removes the VT and its revision publisher together, so the normal
/// outcome is pane-gone → NOT_FOUND (-32004) — never a `settled` verdict on a
/// frozen value from a dead pane. The re-subscribe arm is defensive: if a
/// publisher somehow exists again for this pane id, the waiter continues on
/// the live channel instead of erroring spuriously.
async fn settle_reacquire_watch(
    io: &Arc<Mutex<PaneIoState>>,
    pane_id: shux_core::model::PaneId,
) -> Result<watch::Receiver<PaneRevision>, shux_rpc::RpcError> {
    let state = io.lock().await;
    state
        .revisions
        .get(&pane_id)
        .map(|tx| tx.subscribe())
        .ok_or_else(|| shux_rpc::RpcError::not_found("pane VT", &pane_id.to_string()))
}

/// LENS-R-025 parameter validation: `quiet_ms ∈ [10, 60_000]`,
/// `timeout_ms ∈ [quiet_ms, 600_000]`. Violations → INVALID_PARAMS (-32602),
/// which the CLI maps to exit 2 (§10 exit table, V1).
fn validate_wait_settled_params(quiet_ms: u64, timeout_ms: u64) -> Result<(), shux_rpc::RpcError> {
    if !(SETTLE_QUIET_MIN_MS..=SETTLE_QUIET_MAX_MS).contains(&quiet_ms) {
        return Err(shux_rpc::RpcError::invalid_params(&format!(
            "quiet_ms {quiet_ms} out of range [{SETTLE_QUIET_MIN_MS}, {SETTLE_QUIET_MAX_MS}]"
        )));
    }
    if !(quiet_ms..=SETTLE_TIMEOUT_MAX_MS).contains(&timeout_ms) {
        return Err(shux_rpc::RpcError::invalid_params(&format!(
            "timeout_ms {timeout_ms} out of range [quiet_ms={quiet_ms}, {SETTLE_TIMEOUT_MAX_MS}]"
        )));
    }
    Ok(())
}

/// Register pane I/O methods (send_keys, run_command, command_status, command_cancel, capture).
#[allow(clippy::too_many_arguments)]
fn register_pane_io_methods(
    builder: shux_rpc::RouterBuilder,
    graph: shux_core::graph::GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    _cancel: tokio_util::sync::CancellationToken,
    config: shux_core::config::ConfigHandle,
    meta_cache: session_meta::SessionMetaCache,
    onboarding: onboarding::OnboardingHandle,
    segments: statusbar_runner::SegmentCache,
    lens_audit: Arc<lens_scratch::LensAuditLog>,
) -> shux_rpc::RouterBuilder {
    // LENS-R-052 audit handles for the three lens observation methods
    // (glance / checkpoint / diff). `caller` comes from
    // `shux_rpc::current_caller()` — the task-local the plugin dispatch
    // wrapper scopes to `plugin:<uuid>`; UDS requests default to "uds"
    // (P5 round-1 claude N3, adjudicated IMPLEMENT).
    let audit_glance = lens_audit.clone();
    let audit_checkpoint = lens_audit.clone();
    let audit_diff = lens_audit;

    let g1 = graph.clone();
    let g2 = graph.clone();
    let g5 = graph.clone();
    let g6 = graph.clone();
    let g7 = graph.clone();
    let g8 = graph.clone();
    let g9 = graph.clone();
    let g10 = graph.clone();
    let g12 = graph.clone(); // pane.glance (LENS-R-010..016)
    let g13 = graph.clone(); // pane.wait_settled (LENS-R-020..025)
    let g14 = graph.clone(); // pane.checkpoint (LENS-R-030/031)
    let g15 = graph.clone(); // pane.diff_since (LENS-R-033..038)
    let g11 = graph;

    let io1 = io_state.clone();
    let io2 = io_state.clone();
    let io3 = io_state.clone();
    let io4 = io_state.clone();
    let io5 = io_state.clone();
    let io6 = io_state.clone();
    let io7 = io_state.clone();
    let io8 = io_state.clone();
    let io9 = io_state.clone();
    let io10 = io_state.clone();
    let io11 = io_state.clone();
    let io13 = io_state.clone(); // pane.glance
    let io14 = io_state.clone(); // pane.wait_settled
    let io15 = io_state.clone(); // pane.checkpoint
    let io16 = io_state.clone(); // pane.diff_since
    let io12 = io_state;

    // Shared rasterizer for `pane.snapshot` / `window.snapshot` / `session.snapshot`.
    // Wrapped in an `ArcSwap` so the snapshot handlers can pick up
    // `appearance.font` changes via the existing config hot-reload
    // signal without a daemon restart. On reload failure (bad font
    // path, corrupt file) the last-good rasterizer is kept and the
    // error logged — snapshots never produce blank PNGs because of a
    // misconfiguration. PR #46.
    //
    // Race-window note (council review, PR #46): we capture the
    // build-time config snapshot ONCE here and pass it INTO the reload
    // task. The task starts from that exact same snapshot's font key
    // and re-checks the current config before entering its `notified`
    // loop. This closes the TOCTOU between (a) the initial build and
    // (b) the spawned task starting to await — without it, a config
    // change in that gap would be silently lost because
    // `ConfigHandle::replace` uses `notify_waiters()` which only wakes
    // tasks ALREADY parked on `notified()`.
    let build_snap = config.current();
    let initial_font_key = snapshot_font_key(&build_snap);
    let rasterizer: Arc<arc_swap::ArcSwap<shux_raster::Rasterizer>> =
        Arc::new(arc_swap::ArcSwap::from(Arc::new(
            build_snapshot_rasterizer(&build_snap).unwrap_or_else(|e| {
                tracing::warn!(
                    error = %e,
                    "appearance.font invalid at startup, falling back to bundled chain"
                );
                shux_raster::Rasterizer::new(14.0)
                    .expect("shux-raster: bundled font corrupt — should be unreachable")
            }),
        )));
    {
        let raster_handle = rasterizer.clone();
        let config_for_reload = config.clone();
        let notify = config_for_reload.change_notify();
        tokio::spawn(async move {
            let mut last_font_key = initial_font_key;
            // Catch any change that landed between the initial build
            // and this task taking its first scheduling slot. Without
            // this, the racing change is silently swallowed and the
            // user sees a stale rasterizer until they edit the config
            // again. Council review (PR #46).
            let bootstrap_snap = config_for_reload.current();
            let bootstrap_key = snapshot_font_key(&bootstrap_snap);
            if bootstrap_key != last_font_key {
                match build_snapshot_rasterizer(&bootstrap_snap) {
                    Ok(new_raster) => {
                        raster_handle.store(Arc::new(new_raster));
                        last_font_key = bootstrap_key;
                        tracing::info!(
                            "snapshot rasterizer caught a config change \
                             that raced the daemon startup"
                        );
                    }
                    Err(e) => tracing::warn!(
                        error = %e,
                        "snapshot rasterizer bootstrap reload failed; keeping initial"
                    ),
                }
            }
            loop {
                notify.notified().await;
                let cfg_snap = config_for_reload.current();
                let new_key = snapshot_font_key(&cfg_snap);
                if new_key == last_font_key {
                    continue;
                }
                match build_snapshot_rasterizer(&cfg_snap) {
                    Ok(new_raster) => {
                        raster_handle.store(Arc::new(new_raster));
                        last_font_key = new_key;
                        tracing::info!("snapshot rasterizer rebuilt after config change");
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "snapshot rasterizer rebuild failed; keeping last good"
                        );
                    }
                }
            }
        });
    }
    let rasterizer_pane = rasterizer.clone();
    let rasterizer_window = rasterizer.clone();
    let rasterizer_session = rasterizer.clone();
    let rasterizer_glance = rasterizer.clone();
    let rasterizer_diff = rasterizer.clone();

    builder
        .register_with_policy(
            "pane.send_keys",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g1.clone();
                let io = io1.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;

                    let bytes = if let Some(text) = params.get("text").and_then(|v| v.as_str()) {
                        text.as_bytes().to_vec()
                    } else if let Some(b64) = params.get("data").and_then(|v| v.as_str()) {
                        use base64::Engine;
                        base64::engine::general_purpose::STANDARD
                            .decode(b64)
                            .map_err(|e| {
                                shux_rpc::RpcError::invalid_params(&format!("invalid base64: {e}"))
                            })?
                    } else {
                        return Err(shux_rpc::RpcError::invalid_params(
                            "missing 'text' or 'data' parameter",
                        ));
                    };

                    let state = io.lock().await;
                    let writer = state
                        .writers
                        .get(&pane_id)
                        .ok_or_else(|| {
                            shux_rpc::RpcError::not_found("pane PTY", &pane_id.to_string())
                        })?
                        .clone();
                    drop(state);

                    let len = bytes.len();
                    writer
                        .send(bytes)
                        .await
                        .map_err(|_| shux_rpc::RpcError::internal("PTY write channel closed"))?;

                    Ok(serde_json::json!({
                        "pane_id": pane_id.to_string(),
                        "bytes_written": len,
                    }))
                }
            },
        )
        .register_with_policy(
            "pane.run_command",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g2.clone();
                let io = io2.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;

                    let command =
                        params
                            .get("command")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| {
                                shux_rpc::RpcError::invalid_params("missing 'command' parameter")
                            })?;

                    let args: Vec<String> = params
                        .get("args")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();

                    let timeout_secs = params.get("timeout").and_then(|v| v.as_u64()).unwrap_or(30);
                    let timeout = Duration::from_secs(timeout_secs);

                    let is_async = params
                        .get("async")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);

                    let (completion_tx, completion_rx) = if !is_async {
                        let (tx, rx) = tokio::sync::oneshot::channel();
                        (Some(tx), Some(rx))
                    } else {
                        (None, None)
                    };

                    let (command_id, pty_command) = {
                        let mut state = io.lock().await;
                        state.cmd_engine.start_command(
                            pane_id.0,
                            command,
                            &args,
                            timeout,
                            completion_tx,
                        )
                    };

                    // Write the PTY command
                    {
                        let state = io.lock().await;
                        let writer = state
                            .writers
                            .get(&pane_id)
                            .ok_or_else(|| {
                                shux_rpc::RpcError::not_found("pane PTY", &pane_id.to_string())
                            })?
                            .clone();
                        drop(state);

                        writer.send(pty_command.into_bytes()).await.map_err(|_| {
                            shux_rpc::RpcError::internal("PTY write channel closed")
                        })?;
                    }

                    if is_async {
                        return Ok(serde_json::json!({
                            "command_id": command_id.to_string(),
                            "state": "running",
                        }));
                    }

                    // Sync mode: wait for completion
                    let result = completion_rx
                        .unwrap() // safe: created above when !is_async
                        .await
                        .map_err(|_| {
                            shux_rpc::RpcError::internal("command completion channel dropped")
                        })?;

                    // Capture text from VT
                    let stdout = {
                        let state = io.lock().await;
                        state
                            .vts
                            .get(&pane_id)
                            .map(|vt| {
                                let text = vt.capture_text(Some(50));
                                shux_pty::strip_ansi(&text)
                            })
                            .unwrap_or_default()
                    };

                    Ok(serde_json::json!({
                        "command_id": result.command_id.to_string(),
                        "state": result.state,
                        "exit_code": result.exit_code,
                        "stdout": stdout,
                        "runtime_ms": result.runtime_ms,
                    }))
                }
            },
        )
        .register_with_policy(
            "pane.command_status",
            Policy::fixed(Sensitivity::ContentRead),
            move |params: Option<serde_json::Value>| {
                let io = io3.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let cmd_id_str = params
                        .get("command_id")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'command_id' parameter")
                        })?;

                    let command_id: uuid::Uuid = cmd_id_str.parse().map_err(|_| {
                        shux_rpc::RpcError::invalid_params("invalid command_id format")
                    })?;

                    let state = io.lock().await;
                    let result = state
                        .cmd_engine
                        .get_status(command_id)
                        .ok_or_else(|| shux_rpc::RpcError::not_found("command", cmd_id_str))?;

                    Ok(serde_json::json!({
                        "command_id": result.command_id.to_string(),
                        "state": result.state,
                        "exit_code": result.exit_code,
                        "runtime_ms": result.runtime_ms,
                    }))
                }
            },
        )
        .register_with_policy(
            "pane.command_cancel",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let io = io4.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let cmd_id_str = params
                        .get("command_id")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'command_id' parameter")
                        })?;

                    let command_id: uuid::Uuid = cmd_id_str.parse().map_err(|_| {
                        shux_rpc::RpcError::invalid_params("invalid command_id format")
                    })?;

                    let mut state = io.lock().await;
                    let pane_uuid = state
                        .cmd_engine
                        .cancel_command(command_id)
                        .ok_or_else(|| shux_rpc::RpcError::not_found("command", cmd_id_str))?;

                    // Send Ctrl-C to the pane
                    let pane_id = shux_core::model::PaneId::from_uuid(pane_uuid);
                    if let Some(writer) = state.writers.get(&pane_id) {
                        let _ = writer.send(vec![0x03]).await; // ETX = Ctrl-C
                    }

                    Ok(serde_json::json!({
                        "command_id": cmd_id_str,
                        "state": "cancelled",
                    }))
                }
            },
        )
        .register_with_policy(
            "pane.capture",
            Policy::fixed(Sensitivity::ContentRead),
            move |params: Option<serde_json::Value>| {
                let gh = g5.clone();
                let io = io5.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;

                    // None → entire visible viewport (iTerm2 get_screen_contents
                    // shape). Some(N) → tail N non-blank rows.
                    let lines = params
                        .get("lines")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as usize);

                    let state = io.lock().await;
                    let vt = state.vts.get(&pane_id).ok_or_else(|| {
                        shux_rpc::RpcError::not_found("pane VT", &pane_id.to_string())
                    })?;

                    let text = vt.capture_text(lines);
                    let clean = shux_pty::strip_ansi(&text);
                    let cursor = vt.cursor();
                    let cols = vt.grid().cols();
                    let rows = vt.grid().rows();

                    let mut body = serde_json::json!({
                        "pane_id": pane_id.to_string(),
                        "text": clean,
                        "lines": clean.lines().count(),
                        "cols": cols,
                        "rows": rows,
                        "cursor": {
                            "row": cursor.row,
                            "col": cursor.col,
                            "visible": cursor.visible,
                        },
                    });
                    if let Some(requested) = lines {
                        body["requested_lines"] = serde_json::Value::from(requested);
                    }
                    Ok(body)
                }
            },
        )
        .register_with_policy(
            "pane.wait_for",
            Policy::fixed(Sensitivity::ContentRead),
            move |params: Option<serde_json::Value>| {
                let gh = g10.clone();
                let io = io10.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;

                    let needle_text = params
                        .get("text")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    let needle_regex_raw = params.get("regex").and_then(|v| v.as_str());
                    let absent = params
                        .get("absent")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let lines =
                        params.get("lines").and_then(|v| v.as_u64()).unwrap_or(200) as usize;
                    let timeout_ms = params
                        .get("timeout_ms")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(10_000)
                        .min(60_000);
                    let poll_ms = params
                        .get("poll_ms")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(100)
                        .clamp(20, 1_000);

                    if needle_text.is_none() && needle_regex_raw.is_none() {
                        return Err(shux_rpc::RpcError::invalid_params(
                            "missing 'text' or 'regex' parameter",
                        ));
                    }
                    let needle_regex = match needle_regex_raw {
                        Some(r) => Some(regex::Regex::new(r).map_err(|e| {
                            shux_rpc::RpcError::invalid_params(&format!("invalid regex: {e}"))
                        })?),
                        None => None,
                    };

                    let start = std::time::Instant::now();
                    let deadline = start + std::time::Duration::from_millis(timeout_ms);
                    let mut last_capture;

                    loop {
                        last_capture = {
                            let state = io.lock().await;
                            let vt = state.vts.get(&pane_id).ok_or_else(|| {
                                shux_rpc::RpcError::not_found("pane VT", &pane_id.to_string())
                            })?;
                            let raw = vt.capture_text(Some(lines));
                            shux_pty::strip_ansi(&raw)
                        };

                        let hit = if let Some(re) = needle_regex.as_ref() {
                            re.is_match(&last_capture)
                        } else if let Some(t) = needle_text.as_ref() {
                            last_capture.contains(t.as_str())
                        } else {
                            false
                        };
                        let matched = if absent { !hit } else { hit };

                        if matched {
                            let elapsed = start.elapsed().as_millis() as u64;
                            return Ok(serde_json::json!({
                                "pane_id": pane_id.to_string(),
                                "matched": true,
                                "elapsed_ms": elapsed,
                                "absent": absent,
                                "text_preview": preview_for_log(&last_capture, 240),
                            }));
                        }

                        if std::time::Instant::now() >= deadline {
                            return Err(shux_rpc::RpcError::with_message_and_data(
                                shux_rpc::ErrorCode::NotFound,
                                "wait_for timed out".to_string(),
                                serde_json::json!({
                                    "pane_id": pane_id.to_string(),
                                    "absent": absent,
                                    "timeout_ms": timeout_ms,
                                    "elapsed_ms": start.elapsed().as_millis() as u64,
                                    "last_capture_preview": preview_for_log(&last_capture, 480),
                                }),
                            ));
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(poll_ms)).await;
                    }
                }
            },
        )
        .register_with_policy(
            "pane.record.start",
            Policy::fixed(Sensitivity::PluginsForbidden),
            move |params: Option<serde_json::Value>| {
                let gh = g11.clone();
                let io = io11.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;
                    let path_str =
                        params.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'path' parameter")
                        })?;
                    if path_str.trim().is_empty() {
                        return Err(shux_rpc::RpcError::invalid_params(
                            "'path' parameter must not be empty",
                        ));
                    }
                    let path = PathBuf::from(path_str);
                    let overwrite = params
                        .get("overwrite")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let duration_ms = params.get("duration_ms").and_then(|v| v.as_u64());
                    if !overwrite {
                        match tokio::fs::try_exists(&path).await {
                            Ok(true) => {
                                return Err(shux_rpc::RpcError::invalid_params(
                                    "record file already exists; pass overwrite=true to replace it",
                                ));
                            }
                            Ok(false) => {}
                            Err(e) => {
                                return Err(shux_rpc::RpcError::internal(&format!(
                                    "failed to inspect record file {}: {e}",
                                    path.display()
                                )));
                            }
                        }
                    }

                    {
                        let state = io.lock().await;
                        if !state.writers.contains_key(&pane_id) {
                            return Err(shux_rpc::RpcError::not_found(
                                "live pane PTY",
                                &pane_id.to_string(),
                            ));
                        }
                        if state.recorders.get(&pane_id).is_some_and(|recorders| {
                            recorders.iter().any(|r| {
                                r.outcome.lock().expect("record outcome poisoned").status
                                    == PaneRecordStatus::Recording
                            })
                        }) {
                            return Err(shux_rpc::RpcError::name_conflict(
                                "pane recording",
                                &pane_id.to_string(),
                            ));
                        }
                    }

                    let (sender, outcome, task) = spawn_pane_recorder(path.clone(), overwrite)
                        .await
                        .map_err(|e| shux_rpc::RpcError::internal(&e))?;
                    let recording_id = uuid::Uuid::new_v4();
                    let mut state = io.lock().await;
                    if !state.writers.contains_key(&pane_id) {
                        drop(state);
                        let _ = sender
                            .send(PaneRecordChunk::Finish {
                                status: PaneRecordStatus::Aborted,
                            })
                            .await;
                        let _ = task.await;
                        return Err(shux_rpc::RpcError::not_found(
                            "live pane PTY",
                            &pane_id.to_string(),
                        ));
                    }
                    state
                        .recorders
                        .entry(pane_id)
                        .or_default()
                        .push(PaneRecorder {
                            id: recording_id,
                            path: path.clone(),
                            sender: sender.clone(),
                            outcome: outcome.clone(),
                            task,
                        });
                    drop(state);

                    if let Some(ms) = duration_ms {
                        let io_for_deadline = io.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                            let deadline_sender = {
                                let state = io_for_deadline.lock().await;
                                state.recorders.get(&pane_id).and_then(|recorders| {
                                    recorders
                                        .iter()
                                        .find(|r| r.id == recording_id)
                                        .and_then(|r| {
                                            let status = r
                                                .outcome
                                                .lock()
                                                .expect("record outcome poisoned")
                                                .status;
                                            (status == PaneRecordStatus::Recording)
                                                .then(|| r.sender.clone())
                                        })
                                })
                            };
                            if let Some(sender) = deadline_sender {
                                let _ = sender
                                    .send(PaneRecordChunk::Finish {
                                        status: PaneRecordStatus::Complete,
                                    })
                                    .await;
                            }
                            tokio::time::sleep(PANE_RECORD_COMPLETED_TTL).await;
                            let mut state = io_for_deadline.lock().await;
                            for recorders in state.recorders.values_mut() {
                                if let Some(pos) = recorders.iter().position(|r| {
                                    r.id == recording_id
                                        && r.outcome.lock().expect("record outcome poisoned").status
                                            != PaneRecordStatus::Recording
                                }) {
                                    recorders.remove(pos);
                                    break;
                                }
                            }
                            state.recorders.retain(|_, recorders| !recorders.is_empty());
                        });
                    }

                    Ok(serde_json::json!({
                        "recording_id": recording_id.to_string(),
                        "pane_id": pane_id.to_string(),
                        "path": path.display().to_string(),
                        "duration_ms": duration_ms,
                        "lossless": true,
                        "backpressure": true,
                    }))
                }
            },
        )
        .register_with_policy(
            "pane.record.stop",
            Policy::fixed(Sensitivity::PluginsForbidden),
            move |params: Option<serde_json::Value>| {
                let io = io12.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let recording_id_str = params
                        .get("recording_id")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'recording_id' parameter")
                        })?;
                    let recording_id: uuid::Uuid = recording_id_str.parse().map_err(|_| {
                        shux_rpc::RpcError::invalid_params("invalid recording_id format")
                    })?;

                    let recorder = {
                        let mut state = io.lock().await;
                        let mut found = None;
                        for recorders in state.recorders.values_mut() {
                            if let Some(pos) = recorders.iter().position(|r| r.id == recording_id) {
                                found = Some(recorders.remove(pos));
                                break;
                            }
                        }
                        state.recorders.retain(|_, recorders| !recorders.is_empty());
                        found
                    }
                    .ok_or_else(|| {
                        shux_rpc::RpcError::not_found("pane recording", recording_id_str)
                    })?;

                    let should_finish = recorder
                        .outcome
                        .lock()
                        .expect("record outcome poisoned")
                        .status
                        == PaneRecordStatus::Recording;
                    if should_finish {
                        let _ = recorder
                            .sender
                            .send(PaneRecordChunk::Finish {
                                status: PaneRecordStatus::Complete,
                            })
                            .await;
                    }
                    drop(recorder.sender);
                    let join_result =
                        tokio::time::timeout(std::time::Duration::from_secs(5), recorder.task)
                            .await;
                    match join_result {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            let mut outcome =
                                recorder.outcome.lock().expect("record outcome poisoned");
                            outcome.status = PaneRecordStatus::Error;
                            outcome.error = Some(format!("pane recorder task failed: {e}"));
                        }
                        Err(_) => {
                            let mut outcome =
                                recorder.outcome.lock().expect("record outcome poisoned");
                            if outcome.status == PaneRecordStatus::Recording {
                                outcome.status = PaneRecordStatus::Error;
                                outcome.error =
                                    Some("timed out while finalizing pane recording".to_string());
                            }
                        }
                    }
                    let result = recorder
                        .outcome
                        .lock()
                        .expect("record outcome poisoned")
                        .clone();

                    Ok(serde_json::json!({
                        "recording_id": recording_id.to_string(),
                        "path": recorder.path.display().to_string(),
                        "bytes_written": result.bytes_written,
                        "status": result.status.as_str(),
                        "lossless": result.status == PaneRecordStatus::Complete,
                        "error": result.error,
                    }))
                }
            },
        )
        .register_with_policy(
            "pane.snapshot",
            Policy::fixed(Sensitivity::ContentRead),
            move |params: Option<serde_json::Value>| {
                let gh = g6.clone();
                let io = io6.clone();
                let r = rasterizer_pane.load_full();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;

                    // Read visible dims FIRST, validate the pixel budget BEFORE
                    // any allocation, then clone only the visible viewport (not
                    // scrollback). Codex review (PR #16): cloning the full Grid
                    // under lock paid hundreds of MB of allocations even on
                    // snapshots that were about to be rejected by the cap,
                    // because the default 5000-line scrollback was being copied
                    // unconditionally.
                    let (cw, ch) = r.cell_size();
                    let (grid_snapshot, cursor_pos, snap_cols, snap_rows, default_colors) = {
                        let state = io.lock().await;
                        let vt = state.vts.get(&pane_id).ok_or_else(|| {
                            shux_rpc::RpcError::not_found("pane VT", &pane_id.to_string())
                        })?;
                        let cols = vt.grid().cols();
                        let rows = vt.grid().rows();
                        // 16 M output pixels (~64 MB RGBA, ~4000x4000 px).
                        let pixel_count = (cols as u64)
                            .saturating_mul(cw as u64)
                            .saturating_mul(rows as u64)
                            .saturating_mul(ch as u64);
                        const MAX_PIXELS: u64 = 16_000_000;
                        if pixel_count > MAX_PIXELS {
                            return Err(shux_rpc::RpcError::invalid_params(&format!(
                                "snapshot would be {pixel_count} pixels — exceeds cap of \
                            {MAX_PIXELS}; resize the pane first via pane.set_size"
                            )));
                        }
                        let cur = vt.cursor();
                        let cursor_pos = cur.visible.then_some((cur.row, cur.col, cur.shape));
                        let default_colors = vt.default_colors();
                        // Visible-only clone — does NOT copy scrollback.
                        let grid_clone = vt.grid().clone_visible();
                        (grid_clone, cursor_pos, cols, rows, default_colors)
                    };

                    // Rasterize + PNG-encode off the runtime worker. Both are
                    // pure-CPU and don't yield, so we route them to a blocking
                    // worker that won't starve other RPC handlers.
                    let (img, png_buf) = tokio::task::spawn_blocking(move || {
                        let opts = shux_raster::RasterOptions {
                            cursor: cursor_pos.map(|(row, col, _)| (row, col)),
                            cursor_shape: cursor_pos.map(|(_, _, shape)| shape).unwrap_or_default(),
                            cursor_color: default_colors.cursor,
                            fg_default: default_colors.fg.unwrap_or_else(|| {
                                shux_raster::RasterOptions::default().fg_default
                            }),
                            bg_default: default_colors.bg.unwrap_or_else(|| {
                                shux_raster::RasterOptions::default().bg_default
                            }),
                        };
                        let img = r.render(&grid_snapshot, &opts);
                        let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
                        {
                            use image::ImageEncoder;
                            let encoder = image::codecs::png::PngEncoder::new(&mut buf);
                            encoder
                                .write_image(
                                    img.as_raw(),
                                    img.width(),
                                    img.height(),
                                    image::ExtendedColorType::Rgba8,
                                )
                                .map_err(|e| format!("PNG encode failed: {e}"))?;
                        }
                        Ok::<_, String>((img, buf))
                    })
                    .await
                    .map_err(|e| shux_rpc::RpcError::internal(&format!("rasterize join: {e}")))?
                    .map_err(|e| shux_rpc::RpcError::internal(&e))?;

                    use base64::Engine;
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&png_buf);

                    Ok(serde_json::json!({
                        "pane_id": pane_id.to_string(),
                        "png_base64": b64,
                        "width": img.width(),
                        "height": img.height(),
                        "cell_width": cw,
                        "cell_height": ch,
                        "cols": snap_cols,
                        "rows": snap_rows,
                        "format": "png",
                    }))
                }
            },
        )
        .register_with_policy(
            "pane.glance",
            Policy::fixed(Sensitivity::ContentRead),
            move |params: Option<serde_json::Value>| {
                let gh = g12.clone();
                let io = io13.clone();
                let r = rasterizer_glance.load_full();
                let audit = audit_glance.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;

                    let include_cursor = params
                        .get("include_cursor")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    let include_png = params
                        .get("include_png")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    let want_checkpoint = params
                        .get("checkpoint")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);

                    // LENS-R-010: ONE atomic clone under the pane's state
                    // lock — grid, cursor {row,col,visible}, size,
                    // alt_screen, dynamic default colors, ContentRevision —
                    // all read from the same critical section. Render + text
                    // extraction happen from THIS clone, outside the lock
                    // (LENS-R-011): same revision guaranteed for both.
                    let (cw, ch) = r.cell_size();
                    let (
                        revision,
                        cursor_row,
                        cursor_col,
                        cursor_visible,
                        cursor_shape,
                        alt_screen,
                        snap_cols,
                        snap_rows,
                        default_colors,
                        grid_snapshot,
                    ) = {
                        let state = io.lock().await;
                        let vt = state.vts.get(&pane_id).ok_or_else(|| {
                            shux_rpc::RpcError::not_found("pane VT", &pane_id.to_string())
                        })?;
                        let cols = vt.grid().cols();
                        let rows = vt.grid().rows();
                        // Pre-render pixel budget (codex PR #89 P1): reject
                        // over-budget panes BEFORE any clone/render/encode —
                        // same 16M-pixel cap as `pane.snapshot`, but mapped
                        // to PAYLOAD_TOO_LARGE (-32013) per §5.2. Without
                        // this, a 1000×1000 pane forced the daemon to
                        // allocate + encode hundreds of MB of RGBA before
                        // the post-encode 8 MiB check could fire. Text-only
                        // glances skip it: no PNG payload exists to cap.
                        // Shared with the diff heat path (PR #91 codex P1).
                        if include_png {
                            lens_pixel_budget_check(
                                cols,
                                rows,
                                cw,
                                ch,
                                "shrink the pane (pane.set_size) or set include_png=false",
                            )?;
                        }
                        let cur = vt.cursor();
                        let default_colors = vt.default_colors();
                        // Visible-only clone — no scrollback (LENS-R-012).
                        let grid_clone = vt.grid().clone_visible();
                        (
                            vt.content_revision(),
                            cur.row,
                            cur.col,
                            cur.visible,
                            cur.shape,
                            vt.is_alternate_screen(),
                            cols,
                            rows,
                            default_colors,
                            grid_clone,
                        )
                    };

                    // Text extraction (LENS-R-012), from the clone, outside
                    // the lock: ANSI-free, full-width rows (no trim), joined
                    // by `\n`, no scrollback.
                    let text = grid_snapshot.glance_text();

                    // Route the clone to its consumers without a spare copy
                    // (greptile PR #89 P2): only the PNG+checkpoint
                    // combination needs two owners; every other shape MOVES
                    // the clone (the text-only+checkpoint path previously
                    // cloned it and dropped the original).
                    let (render_grid, checkpoint_grid) = match (include_png, want_checkpoint) {
                        (true, true) => {
                            let cp = grid_snapshot.clone();
                            (Some(grid_snapshot), Some(cp))
                        }
                        (true, false) => (Some(grid_snapshot), None),
                        (false, true) => (None, Some(grid_snapshot)),
                        (false, false) => (None, None),
                    };

                    // PNG rendering (LENS-R-013): reuses shux-raster
                    // unchanged, cursor drawn iff visible AND include_cursor
                    // (default true) — identical policy to `pane.snapshot`.
                    //
                    // `default_colors` below comes from OSC 10/11/12 — the
                    // exact same wiring `pane.snapshot` already uses
                    // (vt.default_colors() → RasterOptions.{fg,bg,cursor}
                    // _default). Per the P2 re-adjudication of §4.2's OSC
                    // row, dynamic-default-color changes are Class A (they
                    // bump ContentRevision — revision tracks the PRESENTED
                    // frame), so a revision-watching caller can no longer
                    // miss a color-only repaint. Residual known limitation:
                    // OSC 4 palette redefinition remains Class B.
                    let png_base64 = if let Some(render_grid) = render_grid {
                        let render_cursor = include_cursor && cursor_visible;
                        let cursor_pos = render_cursor.then_some((cursor_row, cursor_col));
                        let cursor_shape = if render_cursor {
                            cursor_shape
                        } else {
                            shux_vt::CursorShape::default()
                        };
                        let opts = shux_raster::RasterOptions {
                            cursor: cursor_pos,
                            cursor_shape,
                            cursor_color: default_colors.cursor,
                            fg_default: default_colors.fg.unwrap_or_else(|| {
                                shux_raster::RasterOptions::default().fg_default
                            }),
                            bg_default: default_colors.bg.unwrap_or_else(|| {
                                shux_raster::RasterOptions::default().bg_default
                            }),
                        };
                        let png_buf = tokio::task::spawn_blocking(move || {
                            let img = r.render(&render_grid, &opts);
                            let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
                            {
                                use image::ImageEncoder;
                                let encoder = image::codecs::png::PngEncoder::new(&mut buf);
                                encoder
                                    .write_image(
                                        img.as_raw(),
                                        img.width(),
                                        img.height(),
                                        image::ExtendedColorType::Rgba8,
                                    )
                                    .map_err(|e| format!("PNG encode failed: {e}"))?;
                            }
                            Ok::<_, String>(buf)
                        })
                        .await
                        .map_err(|e| shux_rpc::RpcError::internal(&format!("rasterize join: {e}")))?
                        .map_err(|e| shux_rpc::RpcError::internal(&e))?;

                        // §5.2: PAYLOAD_TOO_LARGE at 8 MiB DECODED (before
                        // base64, which would inflate it further).
                        const MAX_PNG_BYTES: usize = 8 * 1024 * 1024;
                        if png_buf.len() > MAX_PNG_BYTES {
                            return Err(shux_rpc::RpcError::payload_too_large(
                                png_buf.len(),
                                MAX_PNG_BYTES,
                            ));
                        }

                        use base64::Engine;
                        Some(base64::engine::general_purpose::STANDARD.encode(&png_buf))
                    } else {
                        None
                    };

                    // Checkpoint storage (§7 LENS-R-030/031): a second, short
                    // lock acquisition — keyed by `revision`, the SAME value
                    // read alongside the clone above (not re-read here); that
                    // clone IS the checkpoint (LENS-R-014). store_checkpoint
                    // refuses if the pane was torn down between the two lock
                    // windows (codex P2 review major: no resurrection of
                    // checkpoint state for a dead pane); `checkpointed` then
                    // honestly reports false.
                    let (checkpointed, evicted_revision) = if let Some(grid) = checkpoint_grid {
                        let mut state = io.lock().await;
                        state.store_checkpoint(
                            pane_id,
                            revision,
                            grid,
                            (cursor_row, cursor_col, cursor_visible),
                            // The §5.1 clone's OSC defaults (LENS-R-038b) —
                            // read in the SAME critical section as the grid.
                            default_colors,
                        )
                    } else {
                        (false, None)
                    };

                    // LENS-R-052: audit the successful glance (P5 round-1
                    // codex M2a — the spec's field list: ts, caller, method,
                    // pane_id, revision(s), bytes_returned). bytes_returned
                    // counts the DECODED payload (viewport text + PNG bytes
                    // before base64).
                    let png_decoded_len = png_base64
                        .as_ref()
                        .map(|b64| b64.len() / 4 * 3)
                        .unwrap_or(0);
                    audit.append(serde_json::json!({
                        "ts": lens_scratch::iso_now(),
                        "caller": shux_rpc::current_caller(),
                        "method": "pane.glance",
                        "pane_id": pane_id.to_string(),
                        "revision": revision,
                        "bytes_returned": text.len() + png_decoded_len,
                    }));

                    Ok(serde_json::json!({
                        "revision": revision,
                        "cols": snap_cols,
                        "rows": snap_rows,
                        "cursor": {
                            "row": cursor_row,
                            "col": cursor_col,
                            "visible": cursor_visible,
                        },
                        "alt_screen": alt_screen,
                        "text": text,
                        "png_base64": png_base64,
                        "checkpointed": checkpointed,
                        "evicted_revision": evicted_revision,
                    }))
                }
            },
        )
        .register_with_policy(
            // `pane.wait_settled` (§6 SPEC-C, LENS-R-020..025): block until the
            // pane has been quiet for `quiet_ms`, or return `settled=false` on
            // the server-side timeout deadline. Pure observation — same read
            // sensitivity as glance.
            "pane.wait_settled",
            Policy::fixed(Sensitivity::ContentRead),
            move |params: Option<serde_json::Value>| {
                let gh = g13.clone();
                let io = io14.clone();
                async move {
                    let params = params.unwrap_or_default();

                    // LENS-R-025 + codex P3 M2: strict typing first (absent →
                    // default; wrong type → INVALID_PARAMS, never a silent
                    // default), then bounds. CLI maps -32602 to exit 2 (§10).
                    let quiet_ms = settle_u64_param(&params, "quiet_ms", 300)?;
                    let timeout_ms = settle_u64_param(&params, "timeout_ms", 10_000)?;
                    validate_wait_settled_params(quiet_ms, timeout_ms)?;

                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;

                    // LENS-R-003/021: subscribe to the pane's revision watch.
                    // `subscribe()` seeds the receiver with the CURRENT published
                    // value AND catches every value published after — no
                    // lost-edge race (this is why the substrate is a `watch`,
                    // not a `Notify`). Clone the receiver out UNDER the io lock,
                    // then drop the lock before any `.await` (never hold the io
                    // mutex across an await — the daemon's cardinal deadlock).
                    let mut rx = {
                        let state = io.lock().await;
                        state
                            .revisions
                            .get(&pane_id)
                            .map(|tx| tx.subscribe())
                            .ok_or_else(|| {
                                shux_rpc::RpcError::not_found("pane VT", &pane_id.to_string())
                            })?
                    };

                    // LENS-R-022: server-side monotonic deadline measured from
                    // request acceptance. `waited_ms` is elapsed against the
                    // SAME instant. LENS-R-023: `rx` and every sleep below live
                    // inside this future, so a client disconnect (the router
                    // drops the future) drops the waiter — no daemon growth.
                    let accept = tokio::time::Instant::now();
                    let timeout_deadline = accept + std::time::Duration::from_millis(timeout_ms);
                    let waited_ms = |now: tokio::time::Instant| -> u64 {
                        now.saturating_duration_since(accept)
                            .as_millis()
                            .min(u128::from(u32::MAX)) as u64
                    };

                    // Event-driven loop (LENS-R-021): no polling, and each sleep
                    // is exactly the remaining quiet interval (capped by the
                    // timeout), woken early only by a genuine Class-A revision.
                    //
                    // Every wake — sleep expiry, watch wake, or a late
                    // scheduler wake — re-enters the top of this loop, so the
                    // quiet condition is ALWAYS re-evaluated before the
                    // timeout can fire (`settle_decide` precedence — codex P3
                    // B1: timeout returns only when quiet is still false at
                    // the deadline).
                    loop {
                        // `borrow_and_update` copies the latest (revision, ns)
                        // and marks it seen, so the next `changed()` fires only
                        // on a strictly newer Class-A batch (Class-B never
                        // publishes — LENS-R-024, S5 comes free).
                        let rev = *rx.borrow_and_update();
                        let now_ns = shux_vt::monotonic_now_ns();
                        let quiet = settle_is_quiet(now_ns, rev.last_mutation_ns, quiet_ms);
                        // TOCTOU guard (claude P3 review): a revision published
                        // AFTER the snapshot above must restart the evaluation
                        // — returning `settled:true` from the stale snapshot
                        // would report a pane as still that has already
                        // mutated again.
                        let pending = match rx.has_changed() {
                            Ok(p) => p,
                            Err(_) => {
                                // codex P3 M1: sender dropped ⇒ pane torn down
                                // mid-wait → NOT_FOUND (never settle on a
                                // frozen value); re-subscribe if a publisher
                                // somehow lives again (defensive).
                                rx = settle_reacquire_watch(&io, pane_id).await?;
                                continue;
                            }
                        };
                        let past_timeout = tokio::time::Instant::now() >= timeout_deadline;
                        match settle_decide(quiet, past_timeout, pending) {
                            SettleWake::Settled => {
                                return Ok(serde_json::json!({
                                    "settled": true,
                                    "revision": rev.content_revision,
                                    "waited_ms": waited_ms(tokio::time::Instant::now()),
                                }));
                            }
                            SettleWake::TimedOut => {
                                return Ok(serde_json::json!({
                                    "settled": false,
                                    "revision": rev.content_revision,
                                    "waited_ms": waited_ms(tokio::time::Instant::now()),
                                }));
                            }
                            SettleWake::KeepWaiting => {}
                        }
                        if pending {
                            // Fresh revision already queued — restart on it
                            // immediately (no sleep, no select).
                            continue;
                        }

                        let remaining = std::time::Duration::from_nanos(settle_remaining_quiet_ns(
                            now_ns,
                            rev.last_mutation_ns,
                            quiet_ms,
                        ));
                        let quiet_deadline = tokio::time::Instant::now() + remaining;
                        let wake = quiet_deadline.min(timeout_deadline);

                        tokio::select! {
                            changed = rx.changed() => {
                                if changed.is_err() {
                                    // codex P3 M1 (same rule as above): pane
                                    // teardown mid-wait → NOT_FOUND.
                                    rx = settle_reacquire_watch(&io, pane_id).await?;
                                }
                                // Loop re-evaluates on the fresh value.
                            }
                            _ = tokio::time::sleep_until(wake) => {
                                // Loop re-evaluates: quiet first, then
                                // timeout (settle_decide precedence).
                            }
                        }
                    }
                }
            },
        )
        .register_with_policy(
            // `pane.checkpoint` (§7 SPEC-D, LENS-R-030/031, DEC-22): capture the
            // pane's current visible grid clone keyed by its `content_revision`
            // for a later `pane.diff_since`. Cap 4 per pane, FIFO by creation
            // revision; re-checkpointing the same revision is a no-op
            // (`evicted_revision: null`). Pure observation of pane content plus
            // bounded daemon-side storage — same read sensitivity as glance.
            "pane.checkpoint",
            Policy::fixed(Sensitivity::ContentRead),
            move |params: Option<serde_json::Value>| {
                let gh = g14.clone();
                let io = io15.clone();
                let audit = audit_checkpoint.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;

                    // One lock: verify the VT exists (PANE_NOT_FOUND otherwise),
                    // clone the current visible grid + cursor keyed by the
                    // revision read in the SAME critical section, and store.
                    // store_checkpoint dedups the same-revision no-op and evicts
                    // the FIFO-oldest past the cap (LENS-R-030/031).
                    let (revision, evicted) = {
                        let mut state = io.lock().await;
                        let (revision, grid, cursor, default_colors) = {
                            let vt = state.vts.get(&pane_id).ok_or_else(|| {
                                shux_rpc::RpcError::not_found("pane VT", &pane_id.to_string())
                            })?;
                            let cur = vt.cursor();
                            (
                                vt.content_revision(),
                                vt.grid().clone_visible(),
                                (cur.row, cur.col, cur.visible),
                                // OSC defaults at capture time (LENS-R-038b),
                                // same critical section as the grid clone.
                                vt.default_colors(),
                            )
                        };
                        let (_stored, evicted) =
                            state.store_checkpoint(pane_id, revision, grid, cursor, default_colors);
                        (revision, evicted)
                    };

                    // LENS-R-052: audit the checkpoint. A checkpoint returns
                    // no pane content, so bytes_returned is 0 by definition.
                    audit.append(serde_json::json!({
                        "ts": lens_scratch::iso_now(),
                        "caller": shux_rpc::current_caller(),
                        "method": "pane.checkpoint",
                        "pane_id": pane_id.to_string(),
                        "revision": revision,
                        "bytes_returned": 0,
                    }));

                    Ok(serde_json::json!({
                        "revision": revision,
                        "evicted_revision": evicted,
                    }))
                }
            },
        )
        .register_with_policy(
            // `pane.diff_since` (§7 SPEC-D, LENS-R-033..038): diff the pane's
            // current visible grid against a checkpointed revision. Existence
            // FIRST — a missing pane is PANE_NOT_FOUND before any checkpoint
            // lookup; then the LENS-R-033 rule (exact checkpoint → diff; else
            // ≤ invalidation marker → RESIZE_INVALIDATED -32011; else
            // STALE_REVISION -32010 with `available`). Pure observation.
            "pane.diff_since",
            Policy::fixed(Sensitivity::ContentRead),
            move |params: Option<serde_json::Value>| {
                let gh = g15.clone();
                let io = io16.clone();
                let r = rasterizer_diff.load_full();
                let audit = audit_diff.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;

                    // `since_revision` is required; strict typing (missing /
                    // wrong type → INVALID_PARAMS, CLI exit 2).
                    let since_revision = match params.get("since_revision") {
                        Some(v) => v.as_u64().ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params(
                                "since_revision must be a non-negative integer",
                            )
                        })?,
                        None => {
                            return Err(shux_rpc::RpcError::invalid_params(
                                "since_revision is required",
                            ));
                        }
                    };
                    let want_row_text = params
                        .get("changed_row_text")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    let want_heat = params
                        .get("heat_png")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);

                    // One lock: existence check, checkpoint lookup (LENS-R-033),
                    // and the atomic current-grid clone all in one critical
                    // section so `to_revision`/grid/cursor agree.
                    let (cw, ch) = r.cell_size();
                    let (
                        cp_grid,
                        cp_cursor,
                        cp_defaults,
                        cur_grid,
                        cur_cursor,
                        to_revision,
                        default_colors,
                    ) = {
                        let state = io.lock().await;
                        let vt = state.vts.get(&pane_id).ok_or_else(|| {
                            shux_rpc::RpcError::not_found("pane VT", &pane_id.to_string())
                        })?;
                        // Existence-first lookup runs AFTER the pane check.
                        let (cp_grid, cp_cursor, cp_defaults) =
                            diff_lookup_checkpoint(&state, &pane_id, since_revision)?;
                        // Pre-render pixel budget for the heat path (PR #91
                        // codex P1): the SAME 16M-pixel cap glance enforces,
                        // checked BEFORE any RGBA allocation/rasterization —
                        // a 1000×1000 pane (valid per pane.set_size) would
                        // otherwise allocate hundreds of MB in
                        // render_lens_heat_png before the post-encode 8 MiB
                        // check could fire. Runs AFTER the LENS-R-033 lookup
                        // so stale/invalidated (more actionable) wins over
                        // the payload error; heat-less diffs skip it — the
                        // cell-level diff never rasterizes.
                        if want_heat {
                            lens_pixel_budget_check(
                                vt.grid().cols(),
                                vt.grid().rows(),
                                cw,
                                ch,
                                "shrink the pane (pane.set_size) or set heat_png=false",
                            )?;
                        }
                        let cur = vt.cursor();
                        (
                            cp_grid,
                            cp_cursor,
                            cp_defaults,
                            vt.grid().clone_visible(),
                            (cur.row, cur.col, cur.visible),
                            vt.content_revision(),
                            vt.default_colors(),
                        )
                    };

                    // Diff computation outside the lock (LENS-R-034..036;
                    // LENS-R-038b: Default colors resolve against each
                    // side's own defaults — the checkpoint's captured
                    // defaults vs the pane's CURRENT defaults).
                    let diff = compute_lens_diff(
                        &cp_grid,
                        &cur_grid,
                        cp_cursor,
                        cur_cursor,
                        cp_defaults,
                        default_colors,
                    );

                    let regions: Vec<serde_json::Value> = diff
                        .regions
                        .iter()
                        .map(|s| {
                            serde_json::json!({
                                "row": s.row,
                                "col_start": s.col_start,
                                "col_end": s.col_end,
                            })
                        })
                        .collect();

                    let changed_row_text = if want_row_text {
                        let mut map = serde_json::Map::new();
                        for &row in &diff.changed_rows {
                            map.insert(
                                row.to_string(),
                                serde_json::Value::String(glance_row_text(&cur_grid, row)),
                            );
                        }
                        serde_json::Value::Object(map)
                    } else {
                        serde_json::Value::Object(serde_json::Map::new())
                    };

                    // Heat PNG (LENS-R-037): render off the runtime worker.
                    // The base frame uses the pane's CURRENT defaults
                    // (`default_colors`, read in the lock above) — the heat
                    // map depicts the PRESENTED current frame, never the
                    // checkpoint's colors (LENS-R-038b test c). The mask is
                    // MOVED into the closure — nothing reads it afterwards
                    // (greptile PR #91: the clone was a needless heap copy
                    // of rows×cols booleans).
                    let heat_png_base64 = if want_heat {
                        let changed_mask = diff.changed_mask;
                        let (rows, cols) = (diff.rows, diff.cols);
                        let heat = tokio::task::spawn_blocking(move || {
                            render_lens_heat_png(
                                &r,
                                &cur_grid,
                                default_colors,
                                &changed_mask,
                                rows,
                                cols,
                            )
                        })
                        .await
                        .map_err(|e| {
                            shux_rpc::RpcError::internal(&format!("heat rasterize join: {e}"))
                        })?
                        .map_err(|e| shux_rpc::RpcError::internal(&e))?;

                        // §7.3 shares glance's 8 MiB decoded-PNG cap.
                        const MAX_PNG_BYTES: usize = 8 * 1024 * 1024;
                        if heat.len() > MAX_PNG_BYTES {
                            return Err(shux_rpc::RpcError::payload_too_large(
                                heat.len(),
                                MAX_PNG_BYTES,
                            ));
                        }
                        use base64::Engine;
                        Some(base64::engine::general_purpose::STANDARD.encode(&heat))
                    } else {
                        None
                    };

                    // LENS-R-052: audit the diff with BOTH revisions
                    // ("revision(s)" per the spec's field list).
                    // bytes_returned counts the decoded payload: changed row
                    // text + heat PNG bytes before base64.
                    let row_text_len: usize = changed_row_text
                        .as_object()
                        .map(|m| m.values().filter_map(|v| v.as_str()).map(str::len).sum())
                        .unwrap_or(0);
                    let heat_decoded_len = heat_png_base64
                        .as_ref()
                        .map(|b64| b64.len() / 4 * 3)
                        .unwrap_or(0);
                    audit.append(serde_json::json!({
                        "ts": lens_scratch::iso_now(),
                        "caller": shux_rpc::current_caller(),
                        "method": "pane.diff_since",
                        "pane_id": pane_id.to_string(),
                        "from_revision": since_revision,
                        "to_revision": to_revision,
                        "bytes_returned": row_text_len + heat_decoded_len,
                    }));

                    let (bb_rs, bb_cs, bb_re, bb_ce) = diff.bounding_box;
                    Ok(serde_json::json!({
                        "from_revision": since_revision,
                        "to_revision": to_revision,
                        "cells_changed": diff.cells_changed,
                        "cursor_moved": diff.cursor_moved,
                        "regions": regions,
                        "regions_truncated": diff.regions_truncated,
                        "bounding_box": {
                            "row_start": bb_rs,
                            "col_start": bb_cs,
                            "row_end": bb_re,
                            "col_end": bb_ce,
                        },
                        "changed_row_text": changed_row_text,
                        "heat_png_base64": heat_png_base64,
                    }))
                }
            },
        )
        .register_with_policy(
            "window.snapshot",
            Policy::fixed(Sensitivity::ContentRead),
            {
                let cfg = config.clone();
                let meta = meta_cache.clone();
                let onb = onboarding.clone();
                let segs = segments.clone();
                move |params: Option<serde_json::Value>| {
                    let gh = g8.clone();
                    let io = io8.clone();
                    let r = rasterizer_window.load_full();
                    let cfg = cfg.clone();
                    let meta = meta.clone();
                    let onb = onb.clone();
                    let segs = segs.clone();
                    async move {
                        let params = params.unwrap_or_default();
                        let window_id = resolve_window_id_from_params(&gh, &params)?;
                        let (cols, rows) = parse_snapshot_dims(&params)?;
                        let snap = gh.snapshot();
                        let (result, _revisions) = snapshot_window(
                            &snap,
                            &io,
                            window_id,
                            cols,
                            rows,
                            r,
                            &cfg,
                            &meta,
                            &onb,
                            &segs,
                            &[],
                        )
                        .await?;
                        Ok(result)
                    }
                }
            },
        )
        .register_with_policy(
            "session.snapshot",
            Policy::fixed(Sensitivity::ContentRead),
            {
                let cfg = config.clone();
                let meta = meta_cache.clone();
                let onb = onboarding.clone();
                let segs = segments.clone();
                move |params: Option<serde_json::Value>| {
                    let gh = g9.clone();
                    let io = io9.clone();
                    let r = rasterizer_session.load_full();
                    let cfg = cfg.clone();
                    let meta = meta.clone();
                    let onb = onb.clone();
                    let segs = segs.clone();
                    async move {
                        let params = params.unwrap_or_default();
                        let session_id_str = params
                            .get("session_id")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| {
                                shux_rpc::RpcError::invalid_params("missing 'session_id' parameter")
                            })?;
                        let session_id: shux_core::model::SessionId =
                            session_id_str.parse().map_err(|_| {
                                shux_rpc::RpcError::invalid_params("invalid session_id format")
                            })?;
                        let snap = gh.snapshot();
                        let session = snap.sessions.get(&session_id).ok_or_else(|| {
                            shux_rpc::RpcError::not_found("session", session_id_str)
                        })?;
                        let window_id = session.active_window;
                        // LENS-R-006 (P1): collect every pane in the session with
                        // its structural entity `version`. The `content_revision`
                        // (§4 substrate) is read from each pane's VT below. This is
                        // the ONLY public exposure of the counter until pane.glance
                        // ships in P2 — it is what lets G3/G4 go green.
                        let session_version: u64 = session.version;
                        let pane_meta: Vec<(shux_core::model::PaneId, u64)> = session
                            .windows
                            .iter()
                            .filter_map(|wid| snap.windows.get(wid))
                            .flat_map(|win| win.layout.tree.pane_ids())
                            .filter_map(|pid| snap.panes.get(&pid).map(|p| (pid, p.version)))
                            .collect();
                        let (cols, rows) = parse_snapshot_dims(&params)?;
                        // Council major 5: render from the SAME snapshot the
                        // session_version/panes[] metadata above came from —
                        // a second gh.snapshot() here could interleave with a
                        // concurrent structural mutation and tear the result.
                        //
                        // PR #87 bot P1 (codex + greptile): content_revisions
                        // are captured INSIDE snapshot_window's io-lock clone
                        // pass — the same critical section that clones the VT
                        // grids for rendering — so pixels and revisions are
                        // provably same-lock (a second lock read here let an
                        // old PNG pair with a newer revision). Plain reads:
                        // never touches DirtyState or render-consumed state
                        // (LENS-R-004).
                        let revision_pane_ids: Vec<shux_core::model::PaneId> =
                            pane_meta.iter().map(|(pid, _)| *pid).collect();
                        let (mut result, content_revs) = snapshot_window(
                            &snap,
                            &io,
                            window_id,
                            cols,
                            rows,
                            r,
                            &cfg,
                            &meta,
                            &onb,
                            &segs,
                            &revision_pane_ids,
                        )
                        .await?;
                        // A graph pane without a VT is REACHABLE via a
                        // snapshot/kill race (TOCTOU): the graph snapshot is a
                        // point-in-time copy, so a pane killed between
                        // `gh.snapshot()` and this PaneIoState read has its VT
                        // already removed. Skip is the correct behavior — OMIT
                        // the entry rather than emit content_revision: 0
                        // (LENS-R-001 starts the counter at 1, so 0 is a lie),
                        // matching snapshot_window's established filter_map
                        // handling of VT-less panes. No assert: panicking on a
                        // legitimate race is wrong (council claude-r2 major).
                        let panes_json: Vec<serde_json::Value> = pane_meta
                            .iter()
                            .filter_map(|(pid, version)| match content_revs.get(pid) {
                                Some(rev) => Some(serde_json::json!({
                                    "pane_id": pid.to_string(),
                                    "version": version,
                                    "content_revision": rev,
                                })),
                                None => {
                                    tracing::warn!(
                                        %pid,
                                        "session.snapshot: graph pane has no VT \
                                         (killed since snapshot); omitting from \
                                         panes[] (never emit revision 0)"
                                    );
                                    None
                                }
                            })
                            .collect();
                        if let Some(obj) = result.as_object_mut() {
                            obj.insert(
                                "session_version".to_string(),
                                serde_json::json!(session_version),
                            );
                            obj.insert("panes".to_string(), serde_json::json!(panes_json));
                        }
                        Ok(result)
                    }
                }
            },
        )
        .register_with_policy(
            "pane.set_size",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g7.clone();
                let io = io7.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;
                    // Validate in u64-space BEFORE narrowing — `as u16` silently
                    // wraps `cols=66536` to 1000 and lets it through (codex
                    // review). Sanity bounds: 4..=1000 cols, 2..=1000 rows.
                    let cols_u64 = params
                        .get("cols")
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'cols'"))?;
                    let rows_u64 = params
                        .get("rows")
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'rows'"))?;
                    if !(4..=1000).contains(&cols_u64) || !(2..=1000).contains(&rows_u64) {
                        return Err(shux_rpc::RpcError::invalid_params(&format!(
                            "rows/cols out of range (got rows={rows_u64} cols={cols_u64}; \
                        valid: 4..=1000 cols, 2..=1000 rows)"
                        )));
                    }
                    let cols = cols_u64 as u16;
                    let rows = rows_u64 as u16;
                    let pty_size = shux_pty::handle::PtySize { rows, cols };

                    // Construct a oneshot ack and await it (with a short timeout
                    // so a deadlocked PTY task can't hang the RPC). Synchronous
                    // semantics: when this RPC returns Ok, `vt.grid().cols/rows`
                    // already reflect the new size and a follow-up pane.snapshot
                    // will capture at the requested resolution.
                    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel::<()>();
                    let resizer = {
                        let state = io.lock().await;
                        state.resizers.get(&pane_id).cloned()
                    };
                    let resizer = resizer.ok_or_else(|| {
                        shux_rpc::RpcError::not_found("pane resizer", &pane_id.to_string())
                    })?;
                    resizer
                        .send(ResizeRequest {
                            size: pty_size,
                            ack: Some(ack_tx),
                        })
                        .await
                        .map_err(|_| shux_rpc::RpcError::internal("pane resize channel closed"))?;
                    tokio::time::timeout(std::time::Duration::from_secs(2), ack_rx)
                        .await
                        .map_err(|_| {
                            shux_rpc::RpcError::internal("pane resize ack timed out after 2s")
                        })?
                        .map_err(|_| {
                            shux_rpc::RpcError::internal("pane resize ack channel dropped")
                        })?;
                    Ok(serde_json::json!({
                        "pane_id": pane_id.to_string(),
                        "rows": rows,
                        "cols": cols,
                    }))
                }
            },
        )
}

/// Decide which session `shux` (no args) should attach to.
///
/// Strategy: query the daemon for sessions; if any exist, pick the most
/// recently created. If none, fall through to "default" (which the
/// daemon-side attach handler will create on demand).
/// Choose the session a bare `shux` / `shux attach` lands on from a
/// `session.list` result. NEVER a scratch session (lens PRD LENS-R-041 —
/// P5 round-1 minor: scratch panes are agent working surfaces; a human
/// attaching blind must not land inside one). Defense in depth: the
/// default `session.list` already omits scratch, and this filter also
/// rejects `scratch: true` flags and the reserved `__scratch-` name prefix
/// in case a future caller feeds an `--include-scratch` listing through.
fn choose_attach_session(sessions: &[serde_json::Value]) -> Option<String> {
    sessions
        .iter()
        .filter(|s| s.get("scratch").and_then(|v| v.as_bool()) != Some(true))
        .filter_map(|s| s.get("name")?.as_str())
        .find(|name| !name.starts_with("__scratch-"))
        .map(str::to_string)
}

async fn pick_attach_target(socket_path: &std::path::Path) -> String {
    if let Ok(mut stream) = client::try_connect(socket_path).await {
        if let Ok(value) = cli::rpc_call(&mut stream, "session.list", serde_json::json!({})).await {
            if let Some(arr) = value.get("sessions").and_then(|v| v.as_array()) {
                if let Some(target) = choose_attach_session(arr) {
                    return target;
                }
            }
        }
    }
    "default".to_string()
}

fn default_session_name() -> String {
    "default".to_string()
}

/// Run the attach TUI client. Translates the daemon's `attach.sock` into
/// real keystrokes / ANSI bytes on the user's terminal. Restores the
/// terminal on every exit path via `TerminalGuard`'s Drop.
async fn run_attach(_jsonrpc_socket: &std::path::Path, session_name: String) -> anyhow::Result<()> {
    let attach_path = daemon::attach_socket_path()?;
    let cfg_snapshot =
        shux_core::config::ConfigHandle::load_or_default(&shux_core::config::default_config_path())
            .current();
    let cfg = shux_ui::ClientConfig {
        socket_path: attach_path.to_string_lossy().to_string(),
        session_name: session_name.clone(),
        prefix: cfg_snapshot.keys.prefix.clone(),
        prefix_key: crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(' '),
            crossterm::event::KeyModifiers::CONTROL,
        ),
        keybindings: cfg_snapshot.keybindings.clone(),
    };
    match shux_ui::attach::run_attach(&attach_path, cfg).await {
        Ok(reason) => {
            match reason {
                shux_ui::ExitReason::Detached => {
                    println!("[detached from session '{session_name}']");
                }
                shux_ui::ExitReason::SessionEnded => {
                    println!("[session '{session_name}' ended]");
                }
                shux_ui::ExitReason::ConnectionLost => {
                    eprintln!("[connection to daemon lost]");
                }
                shux_ui::ExitReason::Error(msg) => {
                    eprintln!("[attach error: {msg}]");
                }
            }
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Dispatch CLI subcommands.
async fn dispatch(args: Cli) -> anyhow::Result<()> {
    let socket_path = args.socket_path();

    match args.command {
        // No subcommand: attach to last session (TTY only). On a
        // non-TTY stdin OR stdout (piped, CI, redirected), don't
        // block — print structured help so scripts get a deterministic
        // response. Attach drives crossterm raw-mode keyboard input,
        // so `shux </dev/null` (stdout-tty + stdin-piped) would
        // hang on the input thread. Guard on BOTH. (Codex council
        // May 2026 + codex bot review of PR #24.)
        None => {
            use std::io::IsTerminal;
            if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
                let help = serde_json::json!({
                    "shux": env!("CARGO_PKG_VERSION"),
                    "help": "Run `shux --help` to see commands. \
                             `shux` with no args attaches to the last session — \
                             but only when BOTH stdin and stdout are a TTY. Try \
                             `shux session list` or `shux rpc call session.list`.",
                    "common_commands": [
                        "shux session create <NAME>",
                        "shux session list",
                        "shux session attach <NAME>",
                        "shux session kill <NAME>",
                        "shux window create -s <SESSION>",
                        "shux pane send-keys -s <SESSION> --text '...'",
                        "shux pane snapshot",
                        "shux plugin install <PATH>",
                        "shux state apply <template.toml>",
                        "shux rpc call <method> --params @file"
                    ]
                });
                println!("{}", serde_json::to_string_pretty(&help)?);
                return Ok(());
            }
            // Recursion guard. Every pane shux spawns gets `SHUX=1`
            // injected (mirrors tmux's `TMUX` env var, see
            // crates/shux-pty defaults). Without a guard here, bare
            // `shux` inside a pane attaches the current TTY to its own
            // daemon — instant render-loop hall-of-mirrors. Mirrors
            // tmux's terse refusal: one line, suggest the env unset.
            if std::env::var_os("SHUX").is_some() {
                eprintln!("sessions should be nested with care, unset $SHUX to force");
                std::process::exit(1);
            }
            let _ = client::ensure_daemon_running_at(&socket_path).await?;
            let session_name = pick_attach_target(&socket_path).await;
            run_attach(&socket_path, session_name).await
        }

        // `shux session <verb>` — canonical session lifecycle.
        // Mirrors `session.*` RPC namespace (`session.create` ↔
        // `shux session create`, etc.). Codex council May 2026
        // established this as the agent-first invariant: RPC dots
        // become CLI spaces, no top-level shortcut verbs.
        Some(Command::Session { command: sc }) => match sc {
            cli::SessionCommand::Create {
                name,
                session,
                ensure,
                detached,
                cwd,
                title,
                cmd,
                argv,
            } => {
                let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
                let resolved = name.or(session);
                let session_name = resolved.clone().unwrap_or_else(default_session_name);
                let _ = cli::handle_new(
                    &mut stream,
                    cli::SessionCreateOptions {
                        session_name: resolved,
                        cwd,
                        title,
                        cmd,
                        argv,
                        ensure,
                    },
                    args.format,
                )
                .await?;
                drop(stream);
                if !detached {
                    run_attach(&socket_path, session_name).await
                } else {
                    Ok(())
                }
            }
            cli::SessionCommand::List { include_scratch } => {
                let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
                cli::handle_ls(&mut stream, include_scratch, args.format).await
            }
            cli::SessionCommand::Kill {
                name_pos,
                session,
                expected_version,
            } => {
                let resolved = name_pos.or(session).ok_or_else(|| {
                    anyhow::anyhow!(
                        "missing session name: pass it as a positional or via -s/--session"
                    )
                })?;
                let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
                cli::handle_kill(&mut stream, &resolved, expected_version, args.format).await
            }
            cli::SessionCommand::Rename {
                session,
                name,
                expected_version,
            } => {
                let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
                cli::handle_rename(&mut stream, &session, &name, expected_version, args.format)
                    .await
            }
            cli::SessionCommand::Attach { name_pos, session } => {
                let _ = client::ensure_daemon_running_at(&socket_path).await?;
                let session_name = name_pos
                    .or(session)
                    .unwrap_or_else(|| "default".to_string());
                run_attach(&socket_path, session_name).await
            }
            cli::SessionCommand::Snapshot {
                session,
                output,
                cols,
                rows,
            } => {
                let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
                // session.snapshot dispatch: (Some(session), None) → handle_snapshot
                // routes to session.snapshot RPC.
                cli::handle_snapshot(
                    &mut stream,
                    Some(&session),
                    None,
                    output,
                    cols,
                    rows,
                    args.format,
                )
                .await
            }
            cli::SessionCommand::Save { session, output } => {
                let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
                cli::handle_session_save(&mut stream, &session, output).await
            }
            cli::SessionCommand::Restore {
                template,
                dry_run,
                watch,
            } => {
                let ops = template::load_and_lower(&template)?;
                if dry_run {
                    println!("{}", serde_json::to_string_pretty(&ops)?);
                    Ok(())
                } else {
                    let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
                    cli::handle_apply(&mut stream, ops, watch, &socket_path).await
                }
            }
        },

        Some(Command::Window { command }) => {
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            match command {
                WindowCommand::List { session } => {
                    cli::handle_window_list(&mut stream, &session, args.format).await
                }
                WindowCommand::Create {
                    session,
                    name,
                    cwd,
                    cmd,
                    ensure,
                    argv,
                } => {
                    cli::handle_window_new(
                        &mut stream,
                        &session,
                        name,
                        cwd,
                        cmd,
                        argv,
                        ensure,
                        args.format,
                    )
                    .await
                }
                WindowCommand::Kill {
                    session,
                    window,
                    expected_version,
                } => {
                    cli::handle_window_kill(
                        &mut stream,
                        &session,
                        &window,
                        expected_version,
                        args.format,
                    )
                    .await
                }
                WindowCommand::Rename {
                    session,
                    window,
                    name,
                    expected_version,
                } => {
                    cli::handle_window_rename(
                        &mut stream,
                        &session,
                        &window,
                        &name,
                        expected_version,
                        args.format,
                    )
                    .await
                }
                WindowCommand::Focus {
                    session,
                    window,
                    expected_version,
                } => {
                    cli::handle_window_focus(
                        &mut stream,
                        &session,
                        &window,
                        expected_version,
                        args.format,
                    )
                    .await
                }
                WindowCommand::Reorder {
                    session,
                    window,
                    index,
                    expected_version,
                } => {
                    cli::handle_window_reorder(
                        &mut stream,
                        &session,
                        &window,
                        index,
                        expected_version,
                        args.format,
                    )
                    .await
                }
                WindowCommand::Snapshot {
                    session,
                    window,
                    output,
                    cols,
                    rows,
                } => {
                    cli::handle_snapshot(
                        &mut stream,
                        session.as_deref(),
                        window.as_deref(),
                        output,
                        cols,
                        rows,
                        args.format,
                    )
                    .await
                }
            }
        }

        Some(Command::Pane { command }) => {
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            match command {
                PaneCommand::List { session, window } => {
                    cli::handle_pane_list(&mut stream, &session, window.as_deref(), args.format)
                        .await
                }
                PaneCommand::Split {
                    session,
                    window,
                    pane,
                    direction,
                    ratio,
                } => {
                    cli::handle_pane_split(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        direction.as_deref(),
                        ratio,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Focus {
                    session,
                    window,
                    pane,
                } => {
                    cli::handle_pane_focus(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        &pane,
                        args.format,
                    )
                    .await
                }
                PaneCommand::FocusDir {
                    session,
                    window,
                    direction,
                } => {
                    cli::handle_pane_focus_dir(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        &direction,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Resize {
                    session,
                    window,
                    pane,
                    direction,
                    delta,
                    expected_version,
                } => {
                    cli::handle_pane_resize(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        &direction,
                        delta,
                        expected_version,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Zoom {
                    session,
                    window,
                    pane,
                    expected_version,
                } => {
                    cli::handle_pane_zoom(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        expected_version,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Swap {
                    session,
                    window,
                    pane,
                    target,
                    expected_version,
                } => {
                    cli::handle_pane_swap(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        &pane,
                        &target,
                        expected_version,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Kill {
                    session,
                    window,
                    pane,
                    expected_version,
                } => {
                    cli::handle_pane_kill(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        &pane,
                        expected_version,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Title {
                    session,
                    window,
                    pane,
                    title,
                    clear,
                    auto,
                    no_auto,
                } => {
                    cli::handle_pane_title(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        title.as_deref(),
                        clear,
                        auto,
                        no_auto,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Watch {
                    session,
                    pane,
                    timeout_ms,
                    limit,
                } => {
                    cli::handle_pane_watch(
                        &mut stream,
                        &session,
                        &pane,
                        timeout_ms,
                        limit,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Record {
                    session,
                    pane,
                    to,
                    force,
                    duration_ms,
                } => {
                    cli::handle_pane_record(
                        &mut stream,
                        &session,
                        &pane,
                        &to,
                        force,
                        duration_ms,
                        args.format,
                    )
                    .await
                }
                PaneCommand::SendKeys {
                    session,
                    window,
                    pane,
                    text,
                    data,
                } => {
                    cli::handle_pane_send_keys(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        text.as_deref(),
                        data.as_deref(),
                        args.format,
                    )
                    .await
                }
                PaneCommand::Run {
                    session,
                    window,
                    pane,
                    command,
                    timeout,
                    is_async,
                } => {
                    cli::handle_pane_run(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        &command,
                        timeout,
                        is_async,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Capture {
                    session,
                    window,
                    pane,
                    lines,
                } => {
                    cli::handle_pane_capture(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        lines,
                        args.format,
                    )
                    .await
                }
                PaneCommand::WaitFor {
                    session,
                    window,
                    pane,
                    text,
                    regex,
                    absent,
                    lines,
                    timeout_ms,
                    poll_ms,
                } => {
                    cli::handle_wait_for(
                        &mut stream,
                        session.as_deref(),
                        window.as_deref(),
                        pane.as_deref(),
                        text.as_deref(),
                        regex.as_deref(),
                        absent,
                        lines,
                        timeout_ms,
                        poll_ms,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Snapshot {
                    session,
                    window,
                    pane,
                    output,
                } => {
                    cli::handle_pane_snapshot(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        output,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Glance {
                    pane,
                    png,
                    text_only,
                    no_cursor,
                    checkpoint,
                } => {
                    cli::handle_pane_glance(
                        &mut stream,
                        &pane,
                        png,
                        text_only,
                        no_cursor,
                        checkpoint,
                        args.format,
                    )
                    .await
                }
                PaneCommand::WaitSettled {
                    pane,
                    quiet,
                    timeout,
                } => {
                    cli::handle_pane_wait_settled(&mut stream, &pane, quiet, timeout, args.format)
                        .await
                }
                PaneCommand::Checkpoint { pane } => {
                    cli::handle_pane_checkpoint(&mut stream, &pane, args.format).await
                }
                PaneCommand::Diff {
                    pane,
                    since,
                    heat,
                    no_row_text,
                } => {
                    cli::handle_pane_diff(&mut stream, &pane, since, heat, no_row_text, args.format)
                        .await
                }
                PaneCommand::SetSize {
                    session,
                    window,
                    pane,
                    cols,
                    rows,
                } => {
                    cli::handle_pane_set_size(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        cols,
                        rows,
                        args.format,
                    )
                    .await
                }
            }
        }

        Some(Command::Lens {
            command:
                cli::LensCommand::Run {
                    size,
                    ttl,
                    max_runtime,
                    env,
                    cwd,
                    wait,
                    argv,
                },
        }) => {
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            cli::handle_lens_run(
                &mut stream,
                &argv,
                size,
                ttl,
                max_runtime,
                &env,
                cwd.as_deref(),
                wait,
                args.format,
            )
            .await
        }

        Some(Command::Rpc {
            command: cli::RpcCommand::Call { method, params },
        }) => {
            // Resolve `--params` source: inline JSON, `@<file>`, or `-` (stdin).
            // Codex council May 2026: eliminate shell-escaping bait for JSON.
            let resolved = if params == "-" {
                let mut buf = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
                buf
            } else if let Some(path) = params.strip_prefix('@') {
                std::fs::read_to_string(path)?
            } else {
                params
            };
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            cli::handle_api(&mut stream, &method, &resolved, args.format).await
        }

        Some(Command::Version) => {
            // Quick probe — don't auto-start daemon just for version
            match client::try_connect(&socket_path).await {
                Ok(mut stream) => cli::handle_version(&mut stream, args.format).await,
                Err(_) => {
                    match args.format {
                        OutputFormat::Json => {
                            println!(
                                "{{\"version\": \"{}\", \"git_sha\": \"{}\"}}",
                                env!("CARGO_PKG_VERSION"),
                                env!("SHUX_GIT_SHA"),
                            );
                        }
                        OutputFormat::Text | OutputFormat::Plain => {
                            style::print_version(
                                env!("CARGO_PKG_VERSION"),
                                Some(env!("SHUX_GIT_SHA")),
                                Some("daemon not running"),
                            );
                        }
                    }
                    Ok(())
                }
            }
        }

        Some(Command::Config { command: cfg_cmd }) => match cfg_cmd {
            cli::ConfigCommand::Init { force } => cli::handle_config_init(force),
            cli::ConfigCommand::ResetHints => cli::handle_config_reset_hints(),
            cli::ConfigCommand::Path => cli::handle_config_path(),
            cli::ConfigCommand::Show => cli::handle_config_show(),
            cli::ConfigCommand::Validate { path, config } => {
                // Either positional or `--config` (mutually exclusive at the
                // clap layer); fold to a single Option for the handler.
                let code = cli::handle_config_validate(path.or(config))?;
                std::process::exit(code);
            }
        },

        Some(Command::Plugin { command: pl_cmd }) => {
            features::plugin::dispatch(pl_cmd, &socket_path, args.format).await
        }

        Some(Command::Events { command: ev_cmd }) => {
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            match ev_cmd {
                cli::EventsCommand::Watch {
                    filter,
                    from_seq,
                    timeout_ms,
                    limit,
                } => {
                    cli::handle_events_watch(&mut stream, filter, from_seq, timeout_ms, limit).await
                }
                cli::EventsCommand::History { filter, count } => {
                    cli::handle_events_history(&mut stream, filter, count).await
                }
            }
        }

        Some(Command::State {
            command:
                cli::StateCommand::Apply {
                    template,
                    dry_run,
                    watch,
                },
        }) => {
            // Lower the TOML template to apply ops first (no daemon needed
            // for parse / validate). If --dry-run, print the lowered ops as
            // pretty JSON and exit.
            let ops = match template::load_and_lower(&template) {
                Ok(ops) => ops,
                Err(e) => {
                    eprintln!("{} {e}", style::error("✗ template error:"));
                    std::process::exit(1);
                }
            };

            if dry_run {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({"ops": ops}))?
                );
                return Ok(());
            }

            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            cli::handle_apply(&mut stream, ops, watch, &socket_path).await
        }

        Some(Command::Init { dir }) => {
            let root = dir.unwrap_or_else(|| std::path::PathBuf::from("."));
            cli::handle_init(&root, args.format)
        }

        Some(Command::__daemon) => unreachable!("handled above"),
    }
}

#[cfg(test)]
mod tests {
    //! Snapshot-path regression tests.
    //!
    //! These exercise the seam between `snapshot_window` and the
    //! script-driven `[[statusbar.segment]]` runner: PR #43 shipped the
    //! attach path with `populate_bar` but the snapshot path silently
    //! dropped every user segment. The test below pre-populates a
    //! `SegmentCache` and drives `build_snapshot_status_bar` directly,
    //! asserting the segment text survives into the rendered StatusBar.
    //! If anyone removes the `populate_bar` call from
    //! `build_snapshot_status_bar` it breaks here.
    use super::*;
    use shux_core::config::{Config, ConfigHandle, SegmentDef, StatusBarConfig};
    use shux_core::graph::{GraphHandle, SessionGraph, SessionGraphSnapshot, run_graph_loop};
    use shux_core::layout::Direction;
    use shux_core::model::{Pane, Session, Window};
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    fn config_with_segment(zone: &str) -> ConfigHandle {
        let cfg = Config {
            statusbar: StatusBarConfig {
                left: None,
                center: None,
                right: None,
                segment: vec![SegmentDef {
                    zone: zone.to_string(),
                    command: vec!["echo".to_string()],
                    env: Default::default(),
                    starship_config: None,
                    interval_ms: 1_000,
                    fallback: None,
                }],
            },
            ..Default::default()
        };
        // The cache is pre-populated by the test so the command never
        // runs; we only need `handle.current()` to return our cfg. Use
        // `replace()` to seed it directly — avoids round-tripping a
        // tempfile through TOML serialize/parse just to exercise an
        // in-memory accessor. Pass a never-existing path so
        // `load_or_default` takes the NotFound branch on every platform.
        let nonexistent = std::env::temp_dir().join("__shux_test_no_such_config__.toml");
        let handle = ConfigHandle::load_or_default(&nonexistent);
        handle.replace(cfg);
        handle
    }

    fn snap_with_one_session() -> (
        SessionGraphSnapshot,
        shux_core::model::SessionId,
        shux_core::model::WindowId,
    ) {
        let pane = Pane::new(shux_core::model::WindowId::new(), "/");
        let mut window = Window::new(shux_core::model::SessionId::new(), "0", pane.id);
        // Fix up cross-refs: Window::new and Pane::new each minted their
        // own ids; pane.window_id must match window.id, window.session_id
        // must match session.id.
        let session_id = shux_core::model::SessionId::new();
        window.session_id = session_id;
        let mut pane = pane;
        pane.window_id = window.id;
        let session = Session::new("test", window.id);
        let session = Session {
            id: session_id,
            ..session
        };

        let mut snap = SessionGraphSnapshot::default();
        snap.sessions.insert(session.id, session);
        snap.windows.insert(window.id, window.clone());
        snap.panes.insert(pane.id, pane);
        (snap, session_id, window.id)
    }

    fn shux_raster_asset(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../shux-raster/assets")
            .join(name)
    }

    #[test]
    fn snapshot_rasterizer_default_chain_covers_tui_text_symbols() {
        let cfg = Config::default();
        let rasterizer = build_snapshot_rasterizer(&cfg).expect("rasterizer");
        for ch in ['\u{21bb}', '\u{2839}'] {
            assert!(
                rasterizer.has_glyph(ch),
                "default snapshot chain should resolve {ch:?}"
            );
            assert!(
                rasterizer.glyph_pixel_count(ch) >= 8,
                "default snapshot chain should render {ch:?} as non-empty pixels"
            );
        }
    }

    #[test]
    fn snapshot_rasterizer_accepts_ordered_builtin_fallbacks() {
        let mut cfg = Config::default();
        cfg.appearance.font_fallbacks = Some(vec![
            shux_raster::BUILTIN_SYMBOLS.to_string(),
            shux_raster::BUILTIN_MATH.to_string(),
            shux_raster::BUILTIN_SYMBOLS_LEGACY.to_string(),
            shux_raster::BUILTIN_EMOJI.to_string(),
        ]);

        let rasterizer = build_snapshot_rasterizer(&cfg).expect("rasterizer");
        let baseline = shux_raster::Rasterizer::new(14.0).expect("baseline rasterizer");
        assert_eq!(
            rasterizer.cell_size(),
            baseline.cell_size(),
            "custom fallbacks without appearance.font must not replace primary metrics"
        );
        assert!(rasterizer.has_glyph('\u{21bb}'));
        assert!(rasterizer.has_glyph('\u{2839}'));
        assert!(rasterizer.has_glyph('\u{1f37a}'));
    }

    #[test]
    fn snapshot_rasterizer_accepts_path_fallback_without_replacing_primary_metrics() {
        let mut cfg = Config::default();
        cfg.appearance.font = Some(shux_raster_asset("JetBrainsMonoNerdFontMono-Regular.ttf"));
        cfg.appearance.font_fallbacks = Some(vec![
            shux_raster_asset("NotoSansSymbols2-Regular.ttf")
                .display()
                .to_string(),
        ]);

        let rasterizer = build_snapshot_rasterizer(&cfg).expect("rasterizer");
        let baseline = shux_raster::Rasterizer::with_primary_font(
            14.0,
            include_bytes!("../../shux-raster/assets/JetBrainsMonoNerdFontMono-Regular.ttf"),
        )
        .expect("baseline rasterizer");
        assert_eq!(
            rasterizer.cell_size(),
            baseline.cell_size(),
            "fallback chain must not change the explicit primary font metrics"
        );
        assert!(rasterizer.has_glyph('A'));
        assert!(rasterizer.has_glyph('\u{2839}'));
    }

    #[test]
    fn snapshot_rasterizer_rejects_empty_fallbacks() {
        let mut cfg = Config::default();
        cfg.appearance.font_fallbacks = Some(vec![]);

        let err = match build_snapshot_rasterizer(&cfg) {
            Ok(_) => panic!("empty fallback list should fail"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("must not be empty"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn snapshot_rasterizer_rejects_unknown_builtin_fallback_token() {
        let mut cfg = Config::default();
        cfg.appearance.font_fallbacks = Some(vec!["builtin:symbol".to_string()]);

        let err = match build_snapshot_rasterizer(&cfg) {
            Ok(_) => panic!("unknown builtin fallback token should fail"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("unknown builtin font token"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn snapshot_rasterizer_rejects_missing_fallback_path() {
        let mut cfg = Config::default();
        cfg.appearance.font_fallbacks = Some(vec![
            "/tmp/this-shux-font-fallback-does-not-exist.ttf".to_string(),
        ]);

        let err = match build_snapshot_rasterizer(&cfg) {
            Ok(_) => panic!("missing fallback path should fail"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("appearance.font_fallbacks"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn snapshot_font_key_tracks_fallback_changes() {
        let mut before = Config::default();
        before.appearance.font = Some(std::path::PathBuf::from("/tmp/primary.ttf"));
        let mut after = before.clone();
        after.appearance.font_fallbacks = Some(vec![shux_raster::BUILTIN_SYMBOLS.to_string()]);

        assert_ne!(snapshot_font_key(&before), snapshot_font_key(&after));
    }

    #[tokio::test]
    async fn snapshot_statusbar_includes_script_segments() {
        // Use the test-only OnboardingHandle constructor — no env
        // mutation, no filesystem. Process env is shared mutable state
        // across `cargo test` threads, so any env-mutating test risks
        // racing every other env-mutating test in the same binary
        // (codex round-2 P1: this would race
        // `onboarding::tests::round_trip_dismissal`).
        let onb = onboarding::OnboardingHandle::from_state_for_test(
            onboarding::OnboardingState::default(),
        );
        let config = config_with_segment("right");
        let meta = session_meta::SessionMetaCache::new();
        let segments = statusbar_runner::SegmentCache::new();
        segments
            .set_for_test(0, b"shux-test-sentinel".to_vec())
            .await;

        let (snap, session_id, window_id) = snap_with_one_session();

        let bar = build_snapshot_status_bar(
            &snap,
            &session_id,
            window_id,
            120,
            &config,
            &meta,
            &onb,
            &segments,
        )
        .await;

        let right_text: String = bar.right.iter().map(|s| s.text.clone()).collect();
        assert!(
            right_text.contains("shux-test-sentinel"),
            "expected snapshot status bar's right zone to contain the \
             segment sentinel, got: {right_text:?}"
        );
    }

    #[tokio::test]
    async fn set_initial_pane_title_targets_original_pane_after_focus_changes() {
        let (graph, state) = SessionGraph::new();
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        let token = CancellationToken::new();
        let token_clone = token.clone();
        let handle = tokio::spawn(async move {
            run_graph_loop(graph, cmd_rx, token_clone).await;
        });
        let gh = GraphHandle::new(cmd_tx, state);

        let session_id = gh
            .create_session_with_command(
                "title-race".to_string(),
                std::path::PathBuf::from("/tmp"),
                vec!["codex".to_string(), "--yolo".to_string()],
            )
            .await
            .unwrap();

        let original_pane = {
            let snap = gh.snapshot();
            initial_pane_id_for_session(&snap, session_id).unwrap()
        };
        let new_active = gh
            .split_pane(original_pane, Direction::Vertical, 0.5)
            .await
            .unwrap();

        set_initial_pane_title(&gh, session_id, Some("aww-shux".to_string()))
            .await
            .unwrap();

        let snap = gh.snapshot();
        assert_eq!(
            snap.panes[&original_pane].manual_title.as_deref(),
            Some("aww-shux")
        );
        assert_eq!(snap.panes[&original_pane].title, "aww-shux");
        assert_eq!(snap.panes[&new_active].manual_title, None);

        token.cancel();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_awaits_explicit_teardown_waiters() {
        let pane_id = shux_core::model::PaneId::new();
        let pane_shutdown = CancellationToken::new();
        let (done_tx, done_rx) = oneshot::channel();
        let io_state = Arc::new(Mutex::new(PaneIoState::new()));

        {
            let mut state = io_state.lock().await;
            state.shutdowns.insert(pane_id, pane_shutdown.clone());
            state.pty_done.insert(pane_id, done_rx);
            let pulse = state.teardown_panes(&[pane_id], true);
            pulse.notify_one();

            assert!(pane_shutdown.is_cancelled());
            assert!(state.pty_done.is_empty());
            assert_eq!(state.teardown_waiters.len(), 1);
        }

        let mut shutdown = tokio::spawn(shutdown_all_pane_io(io_state.clone()));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut shutdown)
                .await
                .is_err(),
            "daemon shutdown must wait for explicit teardown PTY completion"
        );

        done_tx.send(()).unwrap();
        shutdown.await.unwrap();
        assert!(io_state.lock().await.teardown_waiters.is_empty());
    }

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

    /// codex PR #89 P1 — the glance pixel-budget guard must fire BEFORE any
    /// render/encode work (pane.snapshot's MAX_PIXELS equivalent, mapped to
    /// PAYLOAD_TOO_LARGE -32013), and a text-only glance on the same
    /// oversized pane must still succeed (no PNG payload exists to cap).
    #[tokio::test]
    async fn production_glance_rejects_over_budget_panes_before_render() {
        let harness = RpcHarness::new();
        let (_sid, _wid, pane_id) = harness.seed_session("glance-budget").await;
        let _writer_rx = harness.seed_io(pane_id, b"budget probe").await;

        // Grow the pane to pane.set_size's maximum: 1000x1000 cells is far
        // beyond the 16M-pixel raster budget at the bundled font's metrics.
        let resized = dispatch_ok(
            &harness.router,
            "pane.set_size",
            serde_json::json!({"pane_id": pane_id.to_string(), "cols": 1000, "rows": 1000}),
        )
        .await;
        assert_eq!(resized["cols"], 1000);

        let err = dispatch_err(
            &harness.router,
            "pane.glance",
            serde_json::json!({"pane_id": pane_id.to_string()}),
        )
        .await;
        assert_eq!(
            err.code,
            shux_rpc::ErrorCode::PayloadTooLarge.code(),
            "over-budget glance must map to PAYLOAD_TOO_LARGE (-32013)"
        );
        let data = err.data.expect("guard error carries data");
        assert!(data["pixels"].as_u64().unwrap() > data["max_pixels"].as_u64().unwrap());

        // Text-only glance on the SAME oversized pane succeeds — the guard
        // only protects the render path.
        let ok = dispatch_ok(
            &harness.router,
            "pane.glance",
            serde_json::json!({"pane_id": pane_id.to_string(), "include_png": false}),
        )
        .await;
        assert_eq!(ok["cols"], 1000);
        assert!(ok["png_base64"].is_null());
        assert!(ok["text"].as_str().unwrap().contains("budget probe"));

        harness.stop().await;
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

    /// PR #91 codex P1 — the shared pre-render pixel budget predicate: the
    /// SAME 16M-pixel cap `pane.glance` enforces, now also gating the diff
    /// heat path BEFORE any RGBA allocation. Over budget → -32013 with
    /// {pixels, max_pixels, hint}; under budget → Ok (no allocation happens
    /// in the guard itself).
    #[test]
    fn lens_pixel_budget_check_guard_predicate() {
        // Under budget: an 80×24 pane at 9×18px cells is ~311K pixels.
        assert!(lens_pixel_budget_check(80, 24, 9, 18, "hint").is_ok());

        // Over budget: 1000×1000 cells at 9×18px is 162M pixels — the
        // pane.set_size-valid size from the codex P1 report.
        let err = lens_pixel_budget_check(1000, 1000, 9, 18, "set heat_png=false")
            .expect_err("162M pixels must exceed the 16M budget");
        assert_eq!(err.code, shux_rpc::ErrorCode::PayloadTooLarge.code());
        let data = err.data.expect("budget error carries data");
        let pixels = data["pixels"].as_u64().expect("pixels");
        let max = data["max_pixels"].as_u64().expect("max_pixels");
        assert_eq!(pixels, 162_000_000);
        assert_eq!(max, 16_000_000);
        assert!(pixels > max);
        assert_eq!(data["hint"], "set heat_png=false", "hint passes through");
    }

    /// LENS-R-038b (PR #91 codex P2, adjudicated) — a default-color-only
    /// change marks every cell whose changed channel is `Color::Default` on
    /// both sides; concrete-colored cells stay unmarked. Exercises both the
    /// bg (OSC 11) and fg (OSC 10) channels against a grid mixing blank
    /// cells, default-colored text, and one fully concrete-colored cell.
    #[test]
    fn compute_lens_diff_default_color_change_marks_default_cells() {
        let mut vt = shux_vt::VirtualTerminal::new(3, 10);
        // Default-colored text at (0,0..2) + one cell at (1,2) with CONCRETE
        // fg AND bg (never resolves through either default channel).
        vt.process(b"\x1b[1;1HAB\x1b[2;3H\x1b[38;2;1;2;3m\x1b[48;2;4;5;6mX\x1b[0m");
        let grid = vt.grid().clone_visible();
        let cursor = (0, 0, true);
        let base = shux_vt::TerminalDefaultColors::default();

        // bg default changed (OSC 11): every cell except (1,2) counts.
        let bg_changed = shux_vt::TerminalDefaultColors {
            bg: Some([32, 64, 96]),
            ..base
        };
        let diff = compute_lens_diff(&grid, &grid, cursor, cursor, base, bg_changed);
        assert_eq!(
            diff.cells_changed, 29,
            "3×10 grid minus the one concrete-bg cell"
        );
        assert!(!diff.changed_mask[10 + 2], "concrete-bg cell NOT marked");
        assert!(diff.changed_mask[0], "default-colored glyph cell marked");
        assert!(diff.changed_mask[9], "blank cell marked");
        // Row 1 splits around the concrete cell: [0,2) + [3,10).
        let spans: Vec<(u16, u16, u16)> = diff
            .regions
            .iter()
            .map(|s| (s.row, s.col_start, s.col_end))
            .collect();
        assert_eq!(spans, vec![(0, 0, 10), (1, 0, 2), (1, 3, 10), (2, 0, 10)]);
        assert_eq!(diff.bounding_box, (0, 0, 3, 10));

        // fg default changed (OSC 10): same shape — the concrete-fg cell is
        // the only one not resolving through the fg default.
        let fg_changed = shux_vt::TerminalDefaultColors {
            fg: Some([200, 10, 10]),
            ..base
        };
        let diff = compute_lens_diff(&grid, &grid, cursor, cursor, base, fg_changed);
        assert_eq!(diff.cells_changed, 29);
        assert!(!diff.changed_mask[10 + 2], "concrete-fg cell NOT marked");

        // Cursor default (OSC 12) is NOT part of the cell comparison
        // (DEC-11: the cursor overlay is excluded from diffs entirely).
        let cursor_changed = shux_vt::TerminalDefaultColors {
            cursor: Some([255, 0, 0]),
            ..base
        };
        let diff = compute_lens_diff(&grid, &grid, cursor, cursor, base, cursor_changed);
        assert_eq!(diff.cells_changed, 0, "OSC 12 never marks cells");
    }

    /// LENS-R-038b test (b): with UNCHANGED defaults the comparison is
    /// byte-identical to plain raw `Cell` equality — whether the shared
    /// defaults are the builtin fallback (None) or an OSC-set value. Pins
    /// that the D-tier gates and ratified goldens are unaffected.
    #[test]
    fn compute_lens_diff_unchanged_defaults_matches_raw() {
        let mut vt = shux_vt::VirtualTerminal::new(3, 10);
        vt.process(b"\x1b[1;1Hhello");
        let cp = vt.grid().clone_visible();
        vt.process(b"\x1b[2;4H\x1b[48;5;28mZW\x1b[0m");
        let cur = vt.grid().clone_visible();
        let cursor = (0, 0, true);

        let none = shux_vt::TerminalDefaultColors::default();
        let osc_set = shux_vt::TerminalDefaultColors {
            fg: Some([250, 250, 250]),
            bg: Some([32, 64, 96]),
            cursor: Some([255, 128, 0]),
        };

        let raw = compute_lens_diff(&cp, &cur, cursor, cursor, none, none);
        let same_osc = compute_lens_diff(&cp, &cur, cursor, cursor, osc_set, osc_set);
        assert_eq!(raw.cells_changed, 2, "exactly the ZW cells");
        assert_eq!(same_osc.cells_changed, raw.cells_changed);
        assert_eq!(same_osc.changed_mask, raw.changed_mask);
        assert_eq!(same_osc.bounding_box, raw.bounding_box);
        let spans = |d: &LensDiff| -> Vec<(u16, u16, u16)> {
            d.regions
                .iter()
                .map(|s| (s.row, s.col_start, s.col_end))
                .collect()
        };
        assert_eq!(spans(&same_osc), spans(&raw));
    }

    /// LENS-R-038b test (c), unit half — the heat base is rendered with the
    /// defaults PASSED IN (the handler passes the pane's CURRENT defaults).
    /// Deterministic integer expectations: a changed blank cell is the heat
    /// colour alpha-blended over the passed bg default; an unchanged blank
    /// cell is that bg desaturated 50% (Rec.601 luma).
    #[test]
    fn heat_png_base_uses_passed_defaults() {
        let raster = shux_raster::Rasterizer::new(14.0).expect("bundled font");
        let vt = shux_vt::VirtualTerminal::new(2, 4);
        let grid = vt.grid().clone_visible();
        let mut mask = vec![false; 2 * 4];
        mask[0] = true; // (0,0) changed; (0,1) unchanged

        let defaults = shux_vt::TerminalDefaultColors {
            bg: Some([32, 64, 96]),
            ..shux_vt::TerminalDefaultColors::default()
        };
        let png = render_lens_heat_png(&raster, &grid, defaults, &mask, 2, 4).unwrap();
        let img = image::load_from_memory(&png)
            .expect("decode heat")
            .to_rgba8();
        let (cw, _ch) = raster.cell_size();

        // Changed cell (0,0): blend(HEAT=(163,38,56), α=128) over (32,64,96)
        // with truncating integer math = (97, 50, 75).
        let p = img.get_pixel(1, 1);
        assert_eq!((p[0], p[1], p[2]), (97, 50, 75), "heat over CURRENT bg");

        // Unchanged cell (0,1): desaturate((32,64,96)) — gray=(32·77+64·150+
        // 96·29)>>8 = 58 → ((32+58)/2, (64+58)/2, (96+58)/2) = (45, 61, 77).
        let p = img.get_pixel(cw + 1, 1);
        assert_eq!(
            (p[0], p[1], p[2]),
            (45, 61, 77),
            "desaturated CURRENT bg on unchanged cells"
        );
        // Same render with the builtin default bg (None) must differ — the
        // base provably derives from the passed defaults, not a constant.
        let png_builtin = render_lens_heat_png(
            &raster,
            &grid,
            shux_vt::TerminalDefaultColors::default(),
            &mask,
            2,
            4,
        )
        .unwrap();
        assert_ne!(png, png_builtin);
    }

    /// P4 DoD (council D2) — the diff is independent of `DirtyState`: it reads
    /// cell VALUES from `clone_visible` clones, never the render-drained dirty
    /// flags. Simulate a concurrently-attached render client by DRAINING the
    /// VT's dirty regions between the checkpoint clone and the current clone;
    /// the diff still reports the exact delta.
    #[test]
    fn compute_lens_diff_independent_of_dirtystate_drains() {
        let mut vt = shux_vt::VirtualTerminal::new(6, 20);
        // Frame A: a truecolor 'X' at grid (1,1).
        vt.process(b"\x1b[2;2H\x1b[38;2;220;40;40mX\x1b[0m");
        let cp_grid = vt.grid().clone_visible();
        let cp_cursor = {
            let c = vt.cursor();
            (c.row, c.col, c.visible)
        };
        // A render client drains DirtyState (as the attach compositor would).
        let _ = vt.take_dirty_regions();
        assert!(!vt.is_dirty(), "drain cleared dirty flags");

        // Frame B: recolour that SAME cell (style-only) + add a second cell.
        vt.process(b"\x1b[2;2H\x1b[38;2;40;210;210mX\x1b[0m\x1b[3;5H\x1b[44mZ\x1b[0m");
        // Client drains AGAIN mid-flight — the diff must not care.
        let _ = vt.take_dirty_regions();

        let cur_grid = vt.grid().clone_visible();
        let cur_cursor = {
            let c = vt.cursor();
            (c.row, c.col, c.visible)
        };
        let diff = compute_lens_diff(
            &cp_grid,
            &cur_grid,
            cp_cursor,
            cur_cursor,
            shux_vt::TerminalDefaultColors::default(),
            shux_vt::TerminalDefaultColors::default(),
        );
        // (1,1) style change + (2,4) new glyph = exactly 2 cells, despite the
        // dirty drains straddling the checkpoint.
        assert_eq!(diff.cells_changed, 2, "value-based diff, dirty-independent");
        assert_eq!(diff.changed_rows, vec![1, 2]);
        assert!(diff.changed_mask[20 + 1], "recoloured cell (1,1) counts");
        assert!(diff.changed_mask[2 * 20 + 4], "new cell (2,4) counts");
        assert!(!diff.regions_truncated);
        // Half-open bbox spanning rows 1..3, cols 1..5.
        assert_eq!(diff.bounding_box, (1, 1, 3, 5));
    }

    /// LENS-R-034 wide-glyph pairing: if either half of a wide glyph changes,
    /// both the head and its spacer cell count.
    #[test]
    fn compute_lens_diff_wide_glyph_pairs_spacer() {
        let mut vt = shux_vt::VirtualTerminal::new(4, 20);
        let cp_grid = vt.grid().clone_visible();
        let cp_cursor = (0, 0, true);
        // Draw a fullwidth CJK glyph at (0,0) — occupies cols 0 (head) + 1
        // (spacer).
        vt.process("\x1b[1;1H\u{7d42}".as_bytes()); // 終 (width 2)
        let cur_grid = vt.grid().clone_visible();
        let diff = compute_lens_diff(
            &cp_grid,
            &cur_grid,
            cp_cursor,
            (0, 2, true),
            shux_vt::TerminalDefaultColors::default(),
            shux_vt::TerminalDefaultColors::default(),
        );
        assert_eq!(diff.cells_changed, 2, "wide head + spacer both count");
        assert!(diff.changed_mask[0], "head counts");
        assert!(diff.changed_mask[1], "spacer counts");
        // One merged span [0,2) on row 0.
        assert_eq!(diff.regions.len(), 1);
        assert_eq!(
            (
                diff.regions[0].row,
                diff.regions[0].col_start,
                diff.regions[0].col_end
            ),
            (0, 0, 2)
        );
    }

    /// LENS-R-037 heat PNG is deterministic: identical inputs → byte-identical
    /// PNG (the golden-stability contract).
    #[test]
    fn heat_png_is_deterministic() {
        let raster = shux_raster::Rasterizer::new(14.0).expect("bundled font");
        let mut vt = shux_vt::VirtualTerminal::new(4, 10);
        vt.process(b"\x1b[1;1H\x1b[41mAB\x1b[0m");
        let grid = vt.grid().clone_visible();
        let mask = {
            let mut m = vec![false; 4 * 10];
            m[0] = true; // mark (0,0) changed
            m
        };
        let a = render_lens_heat_png(&raster, &grid, vt.default_colors(), &mask, 4, 10).unwrap();
        let b = render_lens_heat_png(&raster, &grid, vt.default_colors(), &mask, 4, 10).unwrap();
        assert_eq!(a, b, "same inputs → byte-identical heat PNG");
        assert!(!a.is_empty());
    }

    fn write_plugin_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[test]
    fn daemon_utility_mappers_cover_error_preview_snapshot_and_color_edges() {
        use shux_core::graph::GraphError;

        assert_eq!(
            graph_error_to_rpc(GraphError::SessionNotFound(
                shux_core::model::SessionId::new()
            ))
            .code,
            shux_rpc::ErrorCode::NotFound.code()
        );
        assert_eq!(
            graph_error_to_rpc(GraphError::WindowNotFound(shux_core::model::WindowId::new())).code,
            shux_rpc::ErrorCode::NotFound.code()
        );
        assert_eq!(
            graph_error_to_rpc(GraphError::PaneNotFound(shux_core::model::PaneId::new())).code,
            shux_rpc::ErrorCode::NotFound.code()
        );
        assert_eq!(
            graph_error_to_rpc(GraphError::WindowNameConflict("logs".to_string())).code,
            shux_rpc::ErrorCode::NameConflict.code()
        );
        assert_eq!(
            graph_error_to_rpc(GraphError::InvalidSessionName("bad/name".to_string())).code,
            shux_rpc::ErrorCode::InvalidParams.code()
        );
        assert_eq!(
            graph_error_to_rpc(GraphError::EmptyWindowName).code,
            shux_rpc::ErrorCode::InvalidParams.code()
        );
        assert_eq!(
            graph_error_to_rpc(GraphError::LastPane).code,
            shux_rpc::ErrorCode::InvalidParams.code()
        );
        assert_eq!(
            graph_error_to_rpc(GraphError::LayoutError("split failed".to_string())).code,
            shux_rpc::ErrorCode::InternalError.code()
        );
        assert_eq!(
            graph_error_to_rpc(GraphError::Shutdown).code,
            shux_rpc::ErrorCode::InternalError.code()
        );
        assert_eq!(
            graph_error_to_rpc(GraphError::VersionConflict {
                resource: "pane",
                id: "p1".to_string(),
                expected: 1,
                actual: 2,
            })
            .code,
            shux_rpc::ErrorCode::VersionConflict.code()
        );

        assert_eq!(
            parse_snapshot_dims(&serde_json::json!({"cols": 80, "rows": 24})).unwrap(),
            (80, 24)
        );
        assert_eq!(
            parse_snapshot_dims(&serde_json::json!({})).unwrap(),
            (120, 36)
        );
        assert_eq!(
            parse_snapshot_dims(&serde_json::json!({"cols": 3, "rows": 24}))
                .unwrap_err()
                .code,
            shux_rpc::ErrorCode::InvalidParams.code()
        );
        assert_eq!(
            parse_expected_version(&serde_json::json!({})).unwrap(),
            None
        );
        assert_eq!(
            parse_expected_version(&serde_json::json!({"expected_version": null})).unwrap(),
            None
        );
        assert_eq!(
            parse_expected_version(&serde_json::json!({"expected_version": 7})).unwrap(),
            Some(7)
        );
        assert_eq!(
            parse_expected_version(&serde_json::json!({"expected_version": "old"}))
                .unwrap_err()
                .code,
            shux_rpc::ErrorCode::InvalidParams.code()
        );
        assert_eq!(
            parse_initial_pane_title(&serde_json::json!({"pane_title": "editor"})).unwrap(),
            Some("editor".to_string())
        );
        assert_eq!(
            parse_initial_pane_title(&serde_json::json!({"pane_title": ""}))
                .unwrap_err()
                .code,
            shux_rpc::ErrorCode::InvalidParams.code()
        );
        assert_eq!(preview_for_log("short", 20), "short");
        assert_eq!(preview_for_log("one\ntwo\nthree", 5), "three");

        let default_io = PaneIoState::default();
        assert!(default_io.writers.is_empty());

        let mut grid = shux_vt::Grid::new(1, 2, shux_vt::GridConfig::default());
        resolve_grid_default_colors(&mut grid, shux_vt::TerminalDefaultColors::default());
        grid.visible_row_mut(0)[0].style.fg = shux_vt::Color::Default;
        grid.visible_row_mut(0)[1].style.bg = shux_vt::Color::Default;
        resolve_grid_default_colors(
            &mut grid,
            shux_vt::TerminalDefaultColors {
                fg: Some([1, 2, 3]),
                bg: Some([4, 5, 6]),
                cursor: None,
            },
        );
        assert_eq!(
            grid.visible_row(0)[0].style.fg,
            shux_vt::Color::Rgb(1, 2, 3)
        );
        assert_eq!(
            grid.visible_row(0)[1].style.bg,
            shux_vt::Color::Rgb(4, 5, 6)
        );
    }

    struct RpcHarness {
        router: shux_rpc::Router,
        graph: GraphHandle,
        io: Arc<Mutex<PaneIoState>>,
        bus: shux_core::bus::EventBus,
        cancel: CancellationToken,
        graph_task: tokio::task::JoinHandle<()>,
        scratch_registry: lens_scratch::ScratchRegistry,
        /// Keeps the isolated scratch/audit dir alive for the harness's
        /// lifetime (registry + lens-audit files live inside).
        _scratch_dir: tempfile::TempDir,
    }

    impl RpcHarness {
        fn new() -> Self {
            let bus = shux_core::bus::EventBus::new();
            let (graph, state) = SessionGraph::new_with_event_bus(Some(bus.clone()));
            let (cmd_tx, cmd_rx) = mpsc::channel(128);
            let cancel = CancellationToken::new();
            let graph_cancel = cancel.clone();
            let graph_task = tokio::spawn(async move {
                run_graph_loop(graph, cmd_rx, graph_cancel).await;
            });
            let graph = GraphHandle::new(cmd_tx, state);
            let io = Arc::new(Mutex::new(PaneIoState::new().with_event_bus(bus.clone())));
            let meta = session_meta::SessionMetaCache::new();
            let config_path =
                std::env::temp_dir().join(format!("shux-rpc-test-{}.toml", uuid::Uuid::new_v4()));
            let config = ConfigHandle::load_or_default(&config_path);
            let onboarding = onboarding::OnboardingHandle::from_state_for_test(Default::default());
            let segments = statusbar_runner::SegmentCache::new();
            let scratch_dir = tempfile::tempdir().expect("scratch dir");
            let lens_audit = lens_scratch::LensAuditLog::open(scratch_dir.path());
            let scratch_registry =
                lens_scratch::ScratchRegistry::new(scratch_dir.path(), lens_audit.clone());

            let builder = register_session_methods(
                shux_rpc::Router::builder(),
                graph.clone(),
                io.clone(),
                cancel.clone(),
                meta.clone(),
                scratch_registry.clone(),
            );
            let builder =
                register_window_methods(builder, graph.clone(), io.clone(), cancel.clone());
            let builder = register_pane_methods(builder, graph.clone(), io.clone(), cancel.clone());
            let builder =
                register_state_methods(builder, graph.clone(), io.clone(), cancel.clone());
            let builder = register_pane_io_methods(
                builder,
                graph.clone(),
                io.clone(),
                cancel.clone(),
                config,
                meta,
                onboarding,
                segments,
                lens_audit,
            );
            let builder = lens_scratch::register_lens_run_method(
                builder,
                graph.clone(),
                io.clone(),
                cancel.clone(),
                bus.clone(),
                scratch_registry.clone(),
            );
            let router = register_events_methods(builder, bus.clone()).build();
            router.assert_every_route_has_policy();

            Self {
                router,
                graph,
                io,
                bus,
                cancel,
                graph_task,
                scratch_registry,
                _scratch_dir: scratch_dir,
            }
        }

        async fn stop(self) {
            self.cancel.cancel();
            self.graph_task.await.unwrap();
        }

        async fn seed_session(
            &self,
            name: &str,
        ) -> (
            shux_core::model::SessionId,
            shux_core::model::WindowId,
            shux_core::model::PaneId,
        ) {
            let session_id = self
                .graph
                .create_session_with_command(
                    name.to_string(),
                    std::path::PathBuf::from("/tmp"),
                    vec!["bash".to_string()],
                )
                .await
                .unwrap();
            let snap = self.graph.snapshot();
            let session = snap.sessions.get(&session_id).unwrap();
            let window_id = session.active_window;
            let pane_id = snap.windows.get(&window_id).unwrap().active_pane;
            (session_id, window_id, pane_id)
        }

        async fn seed_io(
            &self,
            pane_id: shux_core::model::PaneId,
            text: &[u8],
        ) -> mpsc::Receiver<Vec<u8>> {
            let (write_tx, write_rx) = mpsc::channel(16);
            let (resize_tx, mut resize_rx) = mpsc::channel::<ResizeRequest>(8);
            let io = self.io.clone();
            tokio::spawn(async move {
                while let Some(req) = resize_rx.recv().await {
                    let mut state = io.lock().await;
                    if let Some(vt) = state.vts.get_mut(&pane_id) {
                        vt.resize(req.size.rows as usize, req.size.cols as usize);
                    }
                    if let Some(ack) = req.ack {
                        let _ = ack.send(());
                    }
                }
            });

            let mut vt = shux_vt::VirtualTerminal::new(6, 40);
            if !text.is_empty() {
                vt.process(text);
            }
            let mut state = self.io.lock().await;
            state.writers.insert(pane_id, write_tx);
            state.resizers.insert(pane_id, resize_tx);
            state.shutdowns.insert(pane_id, self.cancel.child_token());
            state.vts.insert(pane_id, vt);
            write_rx
        }
    }

    async fn dispatch_ok(
        router: &shux_rpc::Router,
        method: &str,
        params: serde_json::Value,
    ) -> serde_json::Value {
        router.dispatch(method, Some(params)).await.unwrap()
    }

    async fn dispatch_err(
        router: &shux_rpc::Router,
        method: &str,
        params: serde_json::Value,
    ) -> shux_rpc::RpcError {
        router.dispatch(method, Some(params)).await.unwrap_err()
    }

    /// Kill a lens.run scratch session through the production route and
    /// wait for its registry slot to free (the explicit-kill reap confirms
    /// group death before dropping the row).
    async fn kill_scratch_and_wait(harness: &RpcHarness, session_id: &str) {
        let _ = dispatch_ok(
            &harness.router,
            "session.kill",
            serde_json::json!({"id": session_id}),
        )
        .await;
        let sid: shux_core::model::SessionId = session_id.parse().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while harness.scratch_registry.ids().contains(&sid) {
            assert!(
                std::time::Instant::now() < deadline,
                "scratch {session_id} not reaped within 5s of explicit kill"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// LENS-R-043 atomicity through the PRODUCTION lens.run route (P5
    /// round-1 codex B1): with 15/16 slots occupied, two CONCURRENT
    /// lens.run calls race for the last slot — exactly one may win,
    /// deterministically (the check-and-reserve is one critical section;
    /// no sleeps, no retries).
    #[tokio::test]
    async fn production_lens_run_quota_is_atomic_under_concurrent_calls() {
        let harness = RpcHarness::new();
        harness
            .scratch_registry
            .test_occupy(lens_scratch::SCRATCH_QUOTA - 1);

        let params = serde_json::json!({"argv": ["sleep", "30"], "cols": 80, "rows": 24});
        let (a, b) = tokio::join!(
            harness.router.dispatch("lens.run", Some(params.clone())),
            harness.router.dispatch("lens.run", Some(params.clone())),
        );
        let ok_count = [a.is_ok(), b.is_ok()].iter().filter(|&&x| x).count();
        assert_eq!(ok_count, 1, "exactly one racer wins the 16th slot");
        let (winner, loser_err) = match (a, b) {
            (Ok(w), Err(e)) | (Err(e), Ok(w)) => (w, e),
            _ => unreachable!("asserted exactly-one above"),
        };
        assert_eq!(
            loser_err.code,
            shux_rpc::ErrorCode::ResourceExhausted.code(),
            "loser gets RESOURCE_EXHAUSTED (-32012)"
        );

        // A third call while full is also rejected.
        let third = dispatch_err(&harness.router, "lens.run", params.clone()).await;
        assert_eq!(third.code, shux_rpc::ErrorCode::ResourceExhausted.code());

        // Cleanup: kill the winner; its freed slot admits a new run.
        let sid = winner["session_id"].as_str().unwrap().to_string();
        kill_scratch_and_wait(&harness, &sid).await;
        let retry = dispatch_ok(&harness.router, "lens.run", params).await;
        let sid = retry["session_id"].as_str().unwrap().to_string();
        kill_scratch_and_wait(&harness, &sid).await;

        harness.stop().await;
    }

    /// Every lens.run failure path releases its quota reservation (codex
    /// B1's rollback requirement): a SPAWN_FAILED at 15/16 must leave the
    /// 16th slot reusable.
    #[tokio::test]
    async fn production_lens_run_failed_spawn_releases_its_reservation() {
        let harness = RpcHarness::new();
        harness
            .scratch_registry
            .test_occupy(lens_scratch::SCRATCH_QUOTA - 1);

        let bad = dispatch_err(
            &harness.router,
            "lens.run",
            serde_json::json!({"argv": ["/nonexistent-lens-p5-binary"]}),
        )
        .await;
        assert_eq!(bad.code, shux_rpc::ErrorCode::SpawnFailed.code());
        assert_eq!(
            harness.scratch_registry.test_total(),
            lens_scratch::SCRATCH_QUOTA - 1,
            "failed spawn released its reservation"
        );

        // The freed slot admits a real run.
        let ok = dispatch_ok(
            &harness.router,
            "lens.run",
            serde_json::json!({"argv": ["sleep", "30"]}),
        )
        .await;
        let sid = ok["session_id"].as_str().unwrap().to_string();
        kill_scratch_and_wait(&harness, &sid).await;

        harness.stop().await;
    }

    /// LENS-R-040 bounds are validated on the FULL u64 before any
    /// narrowing cast (P5 round-1 codex M3): raw RPC shapes that would
    /// wrap through `as u16`/`as u32` into legal-looking values must be
    /// INVALID_PARAMS. (Unit twins live in lens_scratch::tests; these are
    /// the raw-RPC-shape halves through the production router.)
    #[tokio::test]
    async fn production_lens_run_rejects_wrapping_params_before_cast() {
        let harness = RpcHarness::new();

        // 66000 wraps to 464 through `as u16` — inside [20,500].
        let err = dispatch_err(
            &harness.router,
            "lens.run",
            serde_json::json!({"argv": ["sleep"], "cols": 66000}),
        )
        .await;
        assert_eq!(err.code, shux_rpc::ErrorCode::InvalidParams.code());

        // 2^32 + 1 wraps to 1 through `as u32` — inside [0, 300000].
        let err = dispatch_err(
            &harness.router,
            "lens.run",
            serde_json::json!({"argv": ["sleep"], "post_exit_ttl_ms": 4_294_967_297u64}),
        )
        .await;
        assert_eq!(err.code, shux_rpc::ErrorCode::InvalidParams.code());

        // And no scratch leaked from the rejected calls.
        assert_eq!(harness.scratch_registry.test_total(), 0);
        harness.stop().await;
    }

    /// LENS-R-052 caller identity through the production audit path (P5
    /// round-1 claude N3, adjudicated task-local): a lens.run dispatched
    /// inside `shux_rpc::with_caller("plugin:<uuid>", …)` — the exact
    /// wrapper shux-plugin's dispatch path applies — audits
    /// `caller: plugin:<uuid>`; a plain dispatch (the UDS server shape)
    /// audits the `"uds"` default. (The wrapper's plugin-side half is
    /// pinned in shux-plugin's `dispatch_plugin_frame_scopes_caller_identity`.)
    #[tokio::test]
    async fn production_lens_audit_caller_identity() {
        let harness = RpcHarness::new();
        let params = serde_json::json!({"argv": ["sleep", "30"]});

        // UDS shape: no scope.
        let uds_run = dispatch_ok(&harness.router, "lens.run", params.clone()).await;
        let uds_sid = uds_run["session_id"].as_str().unwrap().to_string();

        // Plugin shape: the same scope wrapper dispatch_plugin_frame uses.
        let plugin_run = shux_rpc::with_caller(
            "plugin:test-uuid-1234".to_string(),
            harness.router.dispatch("lens.run", Some(params)),
        )
        .await
        .unwrap();
        let plugin_sid = plugin_run["session_id"].as_str().unwrap().to_string();

        let audit_path = harness._scratch_dir.path().join("lens-audit.ndjson");
        let text = std::fs::read_to_string(&audit_path).expect("audit log written");
        let creates: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .filter(|e| e["method"] == "scratch.create")
            .collect();
        assert_eq!(creates.len(), 2, "one create entry per run:\n{text}");
        let caller_for = |sid: &str| {
            creates
                .iter()
                .find(|e| e["session_id"] == sid)
                .unwrap_or_else(|| panic!("no create entry for {sid}"))["caller"]
                .clone()
        };
        assert_eq!(caller_for(&uds_sid), "uds", "UDS path defaults");
        assert_eq!(
            caller_for(&plugin_sid),
            "plugin:test-uuid-1234",
            "plugin-scoped dispatch carries the identity"
        );
        // The chain survives mixed-caller appends.
        lens_scratch::verify_chain(&audit_path).expect("audit chain verifies");

        kill_scratch_and_wait(&harness, &uds_sid).await;
        kill_scratch_and_wait(&harness, &plugin_sid).await;
        harness.stop().await;
    }

    /// Bare `shux` / `shux attach` target choice never lands on a scratch
    /// session (P5 round-1 minor — attach guard), whether flagged
    /// `scratch: true` or recognizable by the reserved name prefix.
    #[test]
    fn choose_attach_session_never_picks_scratch() {
        // Scratch-only listing → None (fall through to "default").
        let scratch_only = vec![
            serde_json::json!({"name": "__scratch-abc", "scratch": true}),
            serde_json::json!({"name": "__scratch-def"}),
        ];
        assert_eq!(choose_attach_session(&scratch_only), None);

        // Mixed listing → first NON-scratch name, regardless of order.
        let mixed = vec![
            serde_json::json!({"name": "__scratch-abc", "scratch": true}),
            serde_json::json!({"name": "work"}),
            serde_json::json!({"name": "other"}),
        ];
        assert_eq!(choose_attach_session(&mixed), Some("work".to_string()));

        // Flag wins even when the name looks ordinary.
        let flagged = vec![
            serde_json::json!({"name": "sneaky", "scratch": true}),
            serde_json::json!({"name": "real"}),
        ];
        assert_eq!(choose_attach_session(&flagged), Some("real".to_string()));

        assert_eq!(choose_attach_session(&[]), None);
    }

    #[tokio::test]
    async fn production_state_apply_reports_validation_and_spawn_results() {
        let harness = RpcHarness::new();

        let missing_ops = dispatch_err(&harness.router, "state.apply", serde_json::json!({})).await;
        assert_eq!(missing_ops.code, shux_rpc::ErrorCode::InvalidParams.code());

        let malformed_ops = dispatch_err(
            &harness.router,
            "state.apply",
            serde_json::json!({"ops": "not-an-array"}),
        )
        .await;
        assert_eq!(
            malformed_ops.code,
            shux_rpc::ErrorCode::InvalidParams.code()
        );

        let empty_ops = dispatch_err(
            &harness.router,
            "state.apply",
            serde_json::json!({"ops": []}),
        )
        .await;
        assert_eq!(empty_ops.code, shux_rpc::ErrorCode::InvalidParams.code());

        let applied = dispatch_ok(
            &harness.router,
            "state.apply",
            serde_json::json!({
                "ops": [{
                    "op": "create_session",
                    "name": "applied",
                    "cwd": "/tmp",
                    "initial_command": ["true"],
                    "initial_window_title": "dev"
                }]
            }),
        )
        .await;
        assert_eq!(applied["outputs"].as_array().unwrap().len(), 1);
        assert_eq!(applied["spawn_results"].as_array().unwrap().len(), 1);
        assert!(
            applied["correlation_id"]
                .as_str()
                .unwrap()
                .starts_with("apply-")
        );
        let snap = harness.graph.snapshot();
        let session = snap
            .find_session_by_name("applied")
            .expect("applied session");
        let session_id = session.id;
        let window_id = session.active_window;
        let pane_id = snap.windows[&window_id].active_pane;
        drop(snap);
        assert_eq!(
            resolve_window_id_from_params(
                &harness.graph,
                &serde_json::json!({"session_id": session_id.to_string()})
            )
            .unwrap(),
            window_id
        );
        assert_eq!(
            resolve_pane_id_from_params(
                &harness.graph,
                &serde_json::json!({"pane_id": pane_id.to_string()})
            )
            .unwrap(),
            pane_id
        );
        assert_eq!(
            resolve_window_id_from_params(&harness.graph, &serde_json::json!({}))
                .unwrap_err()
                .code,
            shux_rpc::ErrorCode::InvalidParams.code()
        );

        harness.stop().await;
    }

    #[tokio::test]
    async fn production_plugin_router_covers_install_grants_audit_reload_and_kill() {
        const NOOP_PLUGIN: &str = r#"#!/usr/bin/env bash
set -u
IFS= read -r _ || exit 1
printf '%s\n' '{"jsonrpc":"2.0","id":"init","result":{"name":"noop","version":"0.1.0","subscribes":[],"provides":[],"capabilities":[]}}'
while IFS= read -r line; do
  case "$line" in
    *'"plugin.shutdown"'*) exit 0 ;;
  esac
done
"#;

        let tmp = tempfile::tempdir().unwrap();
        let script = write_plugin_script(tmp.path(), "noop.sh", NOOP_PLUGIN);
        let manager = shux_plugin::PluginManager::with_state_root(
            shux_core::bus::EventBus::new(),
            tmp.path().join("plugins"),
        );
        let router = register_plugin_methods(shux_rpc::Router::builder(), manager.clone()).build();
        router.assert_every_route_has_policy();

        let missing_path = dispatch_err(&router, "plugin.install", serde_json::json!({})).await;
        assert_eq!(missing_path.code, shux_rpc::ErrorCode::InvalidParams.code());

        let identity_mismatch = dispatch_err(
            &router,
            "plugin.install",
            serde_json::json!({
                "path": script.clone(),
                "watch": false,
                "expected_name": "not-noop",
                "expected_version": "0.1.0",
            }),
        )
        .await;
        assert_eq!(
            identity_mismatch.code,
            shux_rpc::ErrorCode::InvalidParams.code()
        );
        assert!(
            identity_mismatch
                .data
                .as_ref()
                .and_then(|data| data.get("detail"))
                .and_then(|detail| detail.as_str())
                .unwrap_or_default()
                .contains("plugin manifest name mismatch")
        );

        let installed = dispatch_ok(
            &router,
            "plugin.install",
            serde_json::json!({
                "path": script,
                "watch": false,
                "state_root": tmp.path().join("plugin-state"),
            }),
        )
        .await;
        assert_eq!(installed["name"], "noop");
        assert_eq!(installed["watching"], false);

        let listed = dispatch_ok(&router, "plugin.list", serde_json::json!({})).await;
        assert_eq!(listed["plugins"].as_array().unwrap().len(), 1);

        let granted = dispatch_ok(
            &router,
            "plugin.grant",
            serde_json::json!({"plugin": "noop", "method": "pane.capture", "target": "pane-1"}),
        )
        .await;
        assert_eq!(granted["granted"], true);
        let subscribe_grant = dispatch_ok(
            &router,
            "plugin.grant",
            serde_json::json!({"plugin": "noop", "method": "pane.output.", "subscribe": true}),
        )
        .await;
        assert_eq!(subscribe_grant["subscribe"], true);

        let grants = dispatch_ok(
            &router,
            "plugin.grants",
            serde_json::json!({"plugin": "noop"}),
        )
        .await;
        assert!(
            grants["grants"]
                .as_object()
                .unwrap()
                .contains_key("pane.capture")
        );

        let audit_path = manager.audit_path("noop").await.unwrap();
        std::fs::create_dir_all(audit_path.parent().unwrap()).unwrap();
        std::fs::write(
            &audit_path,
            r#"{"seq":1,"method":"old"}
{"seq":2,"method":"new"}
"#,
        )
        .unwrap();
        let audit = dispatch_ok(
            &router,
            "plugin.audit",
            serde_json::json!({"plugin": "noop", "tail": 1}),
        )
        .await;
        assert_eq!(audit["entries"].as_array().unwrap().len(), 1);
        assert_eq!(audit["entries"][0]["method"], "new");

        let revoked = dispatch_ok(
            &router,
            "plugin.revoke",
            serde_json::json!({"plugin": "noop", "method": "pane.capture", "target": "pane-1"}),
        )
        .await;
        assert_eq!(revoked["revoked"], true);

        let reloaded = dispatch_ok(
            &router,
            "plugin.reload",
            serde_json::json!({"name": "noop"}),
        )
        .await;
        assert_eq!(reloaded["name"], "noop");

        let killed = dispatch_ok(&router, "plugin.kill", serde_json::json!({"name": "noop"})).await;
        assert_eq!(killed["killed"], "noop");
        let missing_plugin =
            dispatch_err(&router, "plugin.kill", serde_json::json!({"name": "noop"})).await;
        assert_eq!(missing_plugin.code, shux_rpc::ErrorCode::NotFound.code());
    }

    #[tokio::test]
    async fn production_router_session_window_pane_routes_mutate_graph_and_cleanup_io() {
        let harness = RpcHarness::new();
        let (session_id, window_id, first_pane) = harness.seed_session("alpha").await;
        let second_pane = harness
            .graph
            .split_pane(first_pane, Direction::Vertical, 0.5)
            .await
            .unwrap();
        let second_window = harness
            .graph
            .create_window(
                session_id,
                "logs".to_string(),
                std::path::PathBuf::from("/tmp"),
            )
            .await
            .unwrap();
        let second_window_pane = {
            let snap = harness.graph.snapshot();
            snap.windows[&second_window].active_pane
        };
        let _first_rx = harness.seed_io(first_pane, b"alpha ready\n").await;
        let _second_rx = harness.seed_io(second_pane, b"beta ready\n").await;
        let _window_rx = harness.seed_io(second_window_pane, b"logs ready\n").await;

        let listed = dispatch_ok(&harness.router, "session.list", serde_json::json!({})).await;
        assert_eq!(listed["sessions"][0]["name"], "alpha");
        assert_eq!(listed["sessions"][0]["window_count"], 2);

        let renamed = dispatch_ok(
            &harness.router,
            "session.rename",
            serde_json::json!({"name": "alpha", "new_name": "beta"}),
        )
        .await;
        assert_eq!(renamed["name"], "beta");

        let _other = harness.seed_session("other").await;
        let conflict = dispatch_err(
            &harness.router,
            "session.rename",
            serde_json::json!({"id": session_id.to_string(), "new_name": "other"}),
        )
        .await;
        assert_eq!(conflict.code, shux_rpc::ErrorCode::NameConflict.code());

        let windows = dispatch_ok(
            &harness.router,
            "window.list",
            serde_json::json!({"session_id": session_id.to_string()}),
        )
        .await;
        assert_eq!(windows.as_array().unwrap().len(), 2);

        let renamed_window = dispatch_ok(
            &harness.router,
            "window.rename",
            serde_json::json!({"id": second_window.to_string(), "name": "ops"}),
        )
        .await;
        assert_eq!(renamed_window["title"], "ops");

        let refocused_first = dispatch_ok(
            &harness.router,
            "window.focus",
            serde_json::json!({"id": window_id.to_string()}),
        )
        .await;
        assert_eq!(
            refocused_first["previous_window_id"],
            second_window.to_string()
        );

        let focused_window = dispatch_ok(
            &harness.router,
            "window.focus",
            serde_json::json!({"id": second_window.to_string()}),
        )
        .await;
        assert_eq!(focused_window["previous_window_id"], window_id.to_string());

        let reordered = dispatch_ok(
            &harness.router,
            "window.reorder",
            serde_json::json!({"id": second_window.to_string(), "new_index": 0}),
        )
        .await;
        assert_eq!(reordered["index"], 0);

        let panes = dispatch_ok(
            &harness.router,
            "pane.list",
            serde_json::json!({"window_id": window_id.to_string()}),
        )
        .await;
        assert_eq!(panes.as_array().unwrap().len(), 2);

        let focused_pane = dispatch_ok(
            &harness.router,
            "pane.focus",
            serde_json::json!({"pane_id": second_pane.to_string()}),
        )
        .await;
        assert_eq!(focused_pane["pane_id"], second_pane.to_string());

        let titled = dispatch_ok(
            &harness.router,
            "pane.set_title",
            serde_json::json!({"pane_id": second_pane.to_string(), "title": "editor", "auto": false}),
        )
        .await;
        assert_eq!(titled["manual_title"], "editor");
        assert_eq!(titled["auto_title"], false);

        let zoomed = dispatch_ok(
            &harness.router,
            "pane.zoom",
            serde_json::json!({"pane_id": second_pane.to_string()}),
        )
        .await;
        assert_eq!(zoomed["is_zoomed"], true);
        let _ = dispatch_ok(
            &harness.router,
            "pane.zoom",
            serde_json::json!({"pane_id": second_pane.to_string()}),
        )
        .await;

        let swapped = dispatch_ok(
            &harness.router,
            "pane.swap",
            serde_json::json!({"pane_id": first_pane.to_string(), "target_pane_id": second_pane.to_string()}),
        )
        .await;
        assert_eq!(swapped["pane_a"], first_pane.to_string());

        let stale = dispatch_err(
            &harness.router,
            "pane.resize",
            serde_json::json!({"pane_id": second_pane.to_string(), "direction": "horizontal", "delta": 0.2, "expected_version": 999_999}),
        )
        .await;
        assert_eq!(stale.code, shux_rpc::ErrorCode::VersionConflict.code());
        assert!(
            harness.io.lock().await.vts.contains_key(&second_pane),
            "stale pane.resize must not tear down pane IO"
        );

        let killed_pane = dispatch_ok(
            &harness.router,
            "pane.kill",
            serde_json::json!({"pane_id": second_pane.to_string()}),
        )
        .await;
        assert_eq!(killed_pane["killed"], second_pane.to_string());
        assert!(!harness.io.lock().await.vts.contains_key(&second_pane));

        let killed_window = dispatch_ok(
            &harness.router,
            "window.kill",
            serde_json::json!({"id": second_window.to_string()}),
        )
        .await;
        assert_eq!(killed_window["killed"], second_window.to_string());
        assert!(
            !harness
                .io
                .lock()
                .await
                .vts
                .contains_key(&second_window_pane)
        );

        let killed_session = dispatch_ok(
            &harness.router,
            "session.kill",
            serde_json::json!({"id": session_id.to_string()}),
        )
        .await;
        assert_eq!(killed_session["killed"], "beta");
        assert!(!harness.io.lock().await.vts.contains_key(&first_pane));
        assert!(!harness.graph.snapshot().sessions.contains_key(&session_id));

        harness.stop().await;
    }

    #[tokio::test]
    async fn production_events_routes_filter_history_and_live_data_plane() {
        let harness = RpcHarness::new();
        let (session_id, window_id, pane_id) = harness.seed_session("events").await;

        let seq = harness
            .bus
            .publish(shux_core::event::EventData::PluginEvent {
                plugin_id: "mine".to_string(),
                event_type: "tick".to_string(),
                data: serde_json::json!({"ok": true}),
            });
        harness
            .bus
            .publish(shux_core::event::EventData::PluginEvent {
                plugin_id: "other".to_string(),
                event_type: "tick".to_string(),
                data: serde_json::json!({"ok": false}),
            });

        let history = dispatch_ok(
            &harness.router,
            "events.history",
            serde_json::json!({"filter": ["plugin.mine."], "count": 10}),
        )
        .await;
        let events = history["events"].as_array().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "plugin.mine.tick");

        let watched = dispatch_ok(
            &harness.router,
            "events.watch",
            serde_json::json!({"from_seq": seq, "filter": ["plugin.mine."], "max_events": 5, "timeout_ms": 25}),
        )
        .await;
        assert_eq!(watched["events"].as_array().unwrap().len(), 1);
        assert_eq!(watched["next_seq"], seq + 1);
        assert_eq!(watched["lagged"], false);

        let router = harness.router.clone();
        let pane_str = pane_id.to_string();
        let watch = tokio::spawn(async move {
            router
                .dispatch(
                    "pane.output.watch",
                    Some(serde_json::json!({
                        "pane_id": pane_str,
                        "timeout_ms": 500,
                        "limit": 2,
                    })),
                )
                .await
                .unwrap()
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let output_seq = harness.bus.publish_pane_output(
            pane_id,
            window_id,
            session_id,
            "aGVsbG8=".to_string(),
            false,
        );
        let output = watch.await.unwrap();
        assert_eq!(output["chunks"][0]["seq"], output_seq);
        assert_eq!(output["chunks"][0]["bytes"], "aGVsbG8=");
        assert_eq!(output["chunks"][0]["sampled"], false);

        let missing_pane = dispatch_err(
            &harness.router,
            "pane.output.watch",
            serde_json::json!({"timeout_ms": 100}),
        )
        .await;
        assert_eq!(missing_pane.code, shux_rpc::ErrorCode::InvalidParams.code());

        harness.stop().await;
    }

    #[tokio::test]
    async fn pane_record_routes_capture_source_bytes_losslessly() {
        let harness = RpcHarness::new();
        let (_session_id, _window_id, pane_id) = harness.seed_session("record").await;
        let _writer_rx = harness.seed_io(pane_id, b"ready\n").await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("raw-output.bin");

        let started = dispatch_ok(
            &harness.router,
            "pane.record.start",
            serde_json::json!({
                "pane_id": pane_id.to_string(),
                "path": path.display().to_string(),
            }),
        )
        .await;
        assert_eq!(started["lossless"], true);
        assert_eq!(started["backpressure"], true);
        let recording_id = started["recording_id"].as_str().unwrap().to_string();

        let mut payload = Vec::new();
        for i in 0..8192u32 {
            payload.extend_from_slice(b"\x1b[2Jframe:");
            payload.extend_from_slice(i.to_string().as_bytes());
            payload.push(b'\n');
        }
        assert!(
            payload.len() > 64 * 1024,
            "payload should exceed sampled pane.output pending cap"
        );
        tee_pane_recorders(&harness.io, pane_id, &payload, &harness.cancel).await;

        let stopped = dispatch_ok(
            &harness.router,
            "pane.record.stop",
            serde_json::json!({
                "recording_id": recording_id,
            }),
        )
        .await;
        assert_eq!(stopped["status"], "complete");
        assert_eq!(stopped["lossless"], true);
        assert_eq!(stopped["bytes_written"], payload.len() as u64);
        let recorded = tokio::fs::read(dir.path().join("raw-output.bin"))
            .await
            .unwrap();
        assert_eq!(recorded, payload);

        harness.stop().await;
    }

    #[tokio::test]
    async fn pane_record_start_rejects_duplicate_active_recorder_for_pane() {
        let harness = RpcHarness::new();
        let (_session_id, _window_id, pane_id) = harness.seed_session("record-duplicate").await;
        let _writer_rx = harness.seed_io(pane_id, b"ready\n").await;
        let dir = tempfile::tempdir().unwrap();
        let first_path = dir.path().join("first.bin");
        let second_path = dir.path().join("second.bin");

        let started = dispatch_ok(
            &harness.router,
            "pane.record.start",
            serde_json::json!({
                "pane_id": pane_id.to_string(),
                "path": first_path.display().to_string(),
            }),
        )
        .await;
        let err = dispatch_err(
            &harness.router,
            "pane.record.start",
            serde_json::json!({
                "pane_id": pane_id.to_string(),
                "path": second_path.display().to_string(),
            }),
        )
        .await;
        assert_eq!(err.code, shux_rpc::ErrorCode::NameConflict.code());

        let _ = dispatch_ok(
            &harness.router,
            "pane.record.stop",
            serde_json::json!({
                "recording_id": started["recording_id"].as_str().unwrap(),
            }),
        )
        .await;
        harness.stop().await;
    }

    #[tokio::test]
    async fn pane_record_duration_stops_on_daemon_side() {
        let harness = RpcHarness::new();
        let (_session_id, _window_id, pane_id) = harness.seed_session("record-duration").await;
        let _writer_rx = harness.seed_io(pane_id, b"ready\n").await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("duration.bin");

        let started = dispatch_ok(
            &harness.router,
            "pane.record.start",
            serde_json::json!({
                "pane_id": pane_id.to_string(),
                "path": path.display().to_string(),
                "duration_ms": 25,
            }),
        )
        .await;
        let recording_id = started["recording_id"].as_str().unwrap().to_string();
        tee_pane_recorders(&harness.io, pane_id, b"before-deadline", &harness.cancel).await;
        tokio::time::sleep(std::time::Duration::from_millis(75)).await;
        tee_pane_recorders(&harness.io, pane_id, b"after-deadline", &harness.cancel).await;

        let stopped = dispatch_ok(
            &harness.router,
            "pane.record.stop",
            serde_json::json!({
                "recording_id": recording_id,
            }),
        )
        .await;
        assert_eq!(stopped["status"], "complete");
        assert_eq!(stopped["bytes_written"], "before-deadline".len() as u64);
        assert_eq!(
            tokio::fs::read(dir.path().join("duration.bin"))
                .await
                .unwrap(),
            b"before-deadline"
        );

        harness.stop().await;
    }

    #[tokio::test]
    async fn pane_record_start_refuses_existing_file_without_overwrite() {
        let harness = RpcHarness::new();
        let (_session_id, _window_id, pane_id) = harness.seed_session("record-exists").await;
        let _writer_rx = harness.seed_io(pane_id, b"ready\n").await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing.bin");
        tokio::fs::write(&path, b"keep").await.unwrap();

        let err = dispatch_err(
            &harness.router,
            "pane.record.start",
            serde_json::json!({
                "pane_id": pane_id.to_string(),
                "path": path.display().to_string(),
            }),
        )
        .await;
        assert_eq!(err.code, shux_rpc::ErrorCode::InvalidParams.code());
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"keep");

        harness.stop().await;
    }

    #[tokio::test]
    async fn production_pane_io_routes_cover_writes_capture_wait_resize_and_commands() {
        let harness = RpcHarness::new();
        let (session_id, window_id, pane_id) = harness.seed_session("io").await;
        let mut writer_rx = harness
            .seed_io(pane_id, b"boot complete\nagent-ready\n")
            .await;

        let sent = dispatch_ok(
            &harness.router,
            "pane.send_keys",
            serde_json::json!({"pane_id": pane_id.to_string(), "text": "echo hi\n"}),
        )
        .await;
        assert_eq!(sent["bytes_written"], 8);
        assert_eq!(writer_rx.recv().await.unwrap(), b"echo hi\n");

        let sent_b64 = dispatch_ok(
            &harness.router,
            "pane.send_keys",
            serde_json::json!({"pane_id": pane_id.to_string(), "data": "A03/"}),
        )
        .await;
        assert_eq!(sent_b64["bytes_written"], 3);
        assert_eq!(writer_rx.recv().await.unwrap(), vec![3, 77, 255]);

        let capture = dispatch_ok(
            &harness.router,
            "pane.capture",
            serde_json::json!({"session_id": session_id.to_string(), "lines": 2}),
        )
        .await;
        assert!(capture["text"].as_str().unwrap().contains("agent-ready"));
        assert_eq!(capture["requested_lines"], 2);

        let wait_text = dispatch_ok(
            &harness.router,
            "pane.wait_for",
            serde_json::json!({"window_id": window_id.to_string(), "text": "agent-ready", "timeout_ms": 40, "poll_ms": 20}),
        )
        .await;
        assert_eq!(wait_text["matched"], true);
        assert_eq!(wait_text["absent"], false);

        let wait_absent = dispatch_ok(
            &harness.router,
            "pane.wait_for",
            serde_json::json!({"pane_id": pane_id.to_string(), "regex": "panic|error", "absent": true, "timeout_ms": 40}),
        )
        .await;
        assert_eq!(wait_absent["matched"], true);
        assert_eq!(wait_absent["absent"], true);

        let timeout = dispatch_err(
            &harness.router,
            "pane.wait_for",
            serde_json::json!({"pane_id": pane_id.to_string(), "text": "never-happens", "timeout_ms": 20, "poll_ms": 20}),
        )
        .await;
        assert_eq!(timeout.code, shux_rpc::ErrorCode::NotFound.code());
        assert!(
            timeout.data.unwrap()["last_capture_preview"]
                .as_str()
                .unwrap()
                .contains("agent-ready")
        );

        let resized = dispatch_ok(
            &harness.router,
            "pane.set_size",
            serde_json::json!({"pane_id": pane_id.to_string(), "cols": 24, "rows": 4}),
        )
        .await;
        assert_eq!(resized["cols"], 24);
        assert_eq!(resized["rows"], 4);
        {
            let state = harness.io.lock().await;
            let vt = state.vts.get(&pane_id).unwrap();
            assert_eq!(vt.grid().cols(), 24);
            assert_eq!(vt.grid().rows(), 4);
        }

        let bad_size = dispatch_err(
            &harness.router,
            "pane.set_size",
            serde_json::json!({"pane_id": pane_id.to_string(), "cols": 1001, "rows": 4}),
        )
        .await;
        assert_eq!(bad_size.code, shux_rpc::ErrorCode::InvalidParams.code());

        let running = dispatch_ok(
            &harness.router,
            "pane.run_command",
            serde_json::json!({
                "pane_id": pane_id.to_string(),
                "command": "printf",
                "args": ["hello world"],
                "async": true,
                "timeout": 5,
            }),
        )
        .await;
        assert_eq!(running["state"], "running");
        let command_id = running["command_id"].as_str().unwrap().to_string();
        let pty_command = String::from_utf8(writer_rx.recv().await.unwrap()).unwrap();
        assert!(pty_command.contains("printf"));
        assert!(pty_command.contains("hello"));
        assert!(pty_command.contains("SHUX_MAR"));

        let status = dispatch_ok(
            &harness.router,
            "pane.command_status",
            serde_json::json!({"command_id": command_id}),
        )
        .await;
        assert_eq!(status["state"], "running");

        let cancelled = dispatch_ok(
            &harness.router,
            "pane.command_cancel",
            serde_json::json!({"command_id": command_id}),
        )
        .await;
        assert_eq!(cancelled["state"], "cancelled");
        assert_eq!(writer_rx.recv().await.unwrap(), vec![0x03]);

        let status = dispatch_ok(
            &harness.router,
            "pane.command_status",
            serde_json::json!({"command_id": command_id}),
        )
        .await;
        assert_eq!(status["state"], "cancelled");

        let bad_command = dispatch_err(
            &harness.router,
            "pane.run_command",
            serde_json::json!({"pane_id": pane_id.to_string()}),
        )
        .await;
        assert_eq!(bad_command.code, shux_rpc::ErrorCode::InvalidParams.code());

        harness.stop().await;
    }

    /// Deadline-bounded wait for the pane's settle-waiter count (the watch
    /// publisher's receiver_count — receivers exist ONLY while a
    /// `pane.wait_settled` handler is subscribed, so this IS the waiter
    /// registry). §16.1 permits deadline-bounded event waits.
    async fn wait_for_settle_waiters(
        io: &Arc<Mutex<PaneIoState>>,
        pane_id: shux_core::model::PaneId,
        expected: usize,
    ) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let count = {
                let state = io.lock().await;
                state
                    .revisions
                    .get(&pane_id)
                    .map(|tx| tx.receiver_count())
                    .unwrap_or(0)
            };
            if count == expected {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("settle waiter count never reached {expected} (last saw {count})");
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// P3 codex B2 lens-side proof (in-process half): a client that
    /// disconnects mid-`pane.wait_settled` must have its waiter DROPPED — the
    /// real observable is the pane's revision-watch receiver_count, which is
    /// exactly the set of live settle subscriptions (LENS-R-023). Runs the
    /// PRODUCTION router behind a REAL shux-rpc UDS server, so the
    /// connection-level cancellation path (serve_connection) is what drops
    /// the handler future. The black-box CLI-SIGKILL half lives in
    /// crates/shux/tests/settle_waiter_drop.rs.
    #[tokio::test]
    async fn production_settle_waiter_dropped_on_client_disconnect() {
        use futures::{SinkExt, StreamExt};

        let harness = RpcHarness::new();
        let (_sid, _wid, pane_id) = harness.seed_session("settle-drop").await;
        let _write_rx = harness.seed_io(pane_id, b"boot").await;
        // Seed the revision publisher the per-pane PTY task normally owns.
        // last_mutation_ns == now → the pane cannot become quiet within the
        // 60s quiet window, so the waiter lives until dropped.
        {
            let mut state = harness.io.lock().await;
            let (tx, rx0) = watch::channel(PaneRevision {
                content_revision: 1,
                last_mutation_ns: shux_vt::monotonic_now_ns(),
            });
            drop(rx0); // receiver_count now counts ONLY settle waiters
            state.revisions.insert(pane_id, tx);
        }

        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("settle-drop.sock");
        let server_cancel = CancellationToken::new();
        let server = shux_rpc::Server::new(
            shux_rpc::ServerConfig {
                socket_path: socket_path.clone(),
                tcp_addr: String::new(),
                auth_token: None,
            },
            harness.router.clone(),
            server_cancel.clone(),
        );
        let server_task = tokio::spawn(async move {
            server.run().await.unwrap();
        });

        // Connect (bounded retry — no fixed bind sleep) and park a waiter.
        let stream = {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                match tokio::net::UnixStream::connect(&socket_path).await {
                    Ok(s) => break s,
                    Err(e) => {
                        if tokio::time::Instant::now() >= deadline {
                            panic!("settle-drop server never bound: {e}");
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    }
                }
            }
        };
        let mut framed = tokio_util::codec::Framed::new(stream, shux_rpc::create_codec());
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "pane.wait_settled",
            "params": {
                "pane_id": pane_id.to_string(),
                "quiet_ms": 60_000,
                "timeout_ms": 600_000,
            },
        });
        framed
            .send(bytes::Bytes::from(serde_json::to_vec(&request).unwrap()))
            .await
            .unwrap();

        // The waiter subscribes: receiver_count 0 → 1.
        wait_for_settle_waiters(&harness.io, pane_id, 1).await;

        // Client disconnect (socket-level equivalent of SIGKILLing the CLI).
        drop(framed);

        // The waiter must be GONE — not parked until settle or the 600s cap.
        wait_for_settle_waiters(&harness.io, pane_id, 0).await;

        // Daemon healthy: a fresh connection serves a normal request.
        let stream2 = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
        let mut framed2 = tokio_util::codec::Framed::new(stream2, shux_rpc::create_codec());
        let list_req = serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "session.list", "params": {}
        });
        framed2
            .send(bytes::Bytes::from(serde_json::to_vec(&list_req).unwrap()))
            .await
            .unwrap();
        let response: serde_json::Value =
            serde_json::from_slice(&framed2.next().await.unwrap().unwrap()).unwrap();
        assert!(
            response["result"]["sessions"].is_array(),
            "daemon must stay responsive after the waiter drop: {response}"
        );

        server_cancel.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_task).await;
        harness.stop().await;
    }

    /// P3 codex M1: a pane killed while a settle waiter is parked on it must
    /// resolve that waiter with NOT_FOUND (-32004) — never `settled:true` on
    /// the frozen last value of a dead pane's channel. Teardown drops the
    /// revision publisher, the waiter's `changed()` errors, and the re-check
    /// finds the pane gone.
    #[tokio::test]
    async fn production_wait_settled_pane_killed_mid_wait_returns_not_found() {
        let harness = RpcHarness::new();
        let (_sid, _wid, pane_id) = harness.seed_session("settle-kill").await;
        let _write_rx = harness.seed_io(pane_id, b"boot").await;
        {
            let mut state = harness.io.lock().await;
            let (tx, rx0) = watch::channel(PaneRevision {
                content_revision: 1,
                // Fresh mutation stamp: cannot become quiet within 60s, so
                // the waiter is guaranteed parked when the pane dies.
                last_mutation_ns: shux_vt::monotonic_now_ns(),
            });
            drop(rx0);
            state.revisions.insert(pane_id, tx);
        }

        let router = harness.router.clone();
        let waiter = tokio::spawn(async move {
            router
                .dispatch(
                    "pane.wait_settled",
                    Some(serde_json::json!({
                        "pane_id": pane_id.to_string(),
                        "quiet_ms": 60_000,
                        "timeout_ms": 600_000,
                    })),
                )
                .await
        });

        // Deterministic: the waiter has subscribed (receiver_count 0 → 1).
        wait_for_settle_waiters(&harness.io, pane_id, 1).await;

        // Kill the pane exactly the way pane/window/session kill does:
        // teardown with remove_vts drops the VT AND the revision publisher.
        {
            let mut state = harness.io.lock().await;
            let _ = state.teardown_panes(&[pane_id], true);
        }

        let result = tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
            .await
            .expect("wait_settled must resolve promptly after pane teardown")
            .expect("waiter task must not panic");
        let err = result.expect_err("must error, not settle on a dead pane's frozen value");
        assert_eq!(
            err.code,
            shux_rpc::ErrorCode::NotFound.code(),
            "pane killed mid-wait must surface NOT_FOUND (-32004): {err:?}"
        );

        harness.stop().await;
    }

    /// P3 codex M2: mistyped `quiet_ms`/`timeout_ms` (string, float, null,
    /// negative) must surface INVALID_PARAMS (-32602) via the raw RPC path —
    /// never silently fall back to the defaults. Absent params still default.
    #[tokio::test]
    async fn production_wait_settled_rejects_mistyped_params() {
        let harness = RpcHarness::new();
        let (_sid, _wid, pane_id) = harness.seed_session("settle-types").await;
        let _write_rx = harness.seed_io(pane_id, b"boot").await;
        {
            let mut state = harness.io.lock().await;
            let (tx, rx0) = watch::channel(PaneRevision {
                content_revision: 1,
                // Ancient mutation stamp → the defaults-path call below
                // settles immediately instead of really waiting.
                last_mutation_ns: 1,
            });
            drop(rx0);
            state.revisions.insert(pane_id, tx);
        }

        for bad in [
            serde_json::json!("5ms"),
            serde_json::json!(5.5),
            serde_json::json!(null),
            serde_json::json!(-5),
        ] {
            let err = dispatch_err(
                &harness.router,
                "pane.wait_settled",
                serde_json::json!({ "pane_id": pane_id.to_string(), "quiet_ms": bad }),
            )
            .await;
            assert_eq!(
                err.code,
                shux_rpc::ErrorCode::InvalidParams.code(),
                "quiet_ms={bad} must be INVALID_PARAMS, got {err:?}"
            );
            let err = dispatch_err(
                &harness.router,
                "pane.wait_settled",
                serde_json::json!({ "pane_id": pane_id.to_string(), "timeout_ms": bad }),
            )
            .await;
            assert_eq!(
                err.code,
                shux_rpc::ErrorCode::InvalidParams.code(),
                "timeout_ms={bad} must be INVALID_PARAMS, got {err:?}"
            );
        }

        // Absent params → documented defaults (quiet 300 / timeout 10_000);
        // the ancient mutation stamp makes this an immediate settled return.
        let result = dispatch_ok(
            &harness.router,
            "pane.wait_settled",
            serde_json::json!({ "pane_id": pane_id.to_string() }),
        )
        .await;
        assert_eq!(result["settled"], serde_json::json!(true));

        harness.stop().await;
    }

    #[test]
    fn settle_u64_param_strict_typing() {
        let params = serde_json::json!({
            "ok": 250,
            "str": "5ms",
            "float": 5.5,
            "null": null,
            "neg": -5,
        });
        // Absent → default.
        assert_eq!(settle_u64_param(&params, "missing", 300).unwrap(), 300);
        // Present u64 → the value.
        assert_eq!(settle_u64_param(&params, "ok", 300).unwrap(), 250);
        // Present-but-wrong-type → INVALID_PARAMS, never the default.
        for key in ["str", "float", "null", "neg"] {
            let err = settle_u64_param(&params, key, 300).unwrap_err();
            assert_eq!(
                err.code,
                shux_rpc::ErrorCode::InvalidParams.code(),
                "{key} must be rejected"
            );
        }
    }

    // ── `pane.wait_settled` settle-math unit tests (§6, L0 supporting) ────
    //
    // These pin the pure decision layer the RPC handler leans on. The
    // black-box behavior (S1–S5, V1) is proven by the frozen red suite; these
    // guard the ns↔ms conversion, the bounds table, the already-quiet fast
    // path, and the waiter-drop primitive against silent regression.

    #[test]
    fn settle_math_ns_conversion_is_explicit_ms_times_million() {
        // 300 ms of quiet == 300_000_000 ns. Exactly at the boundary settles;
        // one ns short does not. This is the councils-caught bug class: a
        // handler comparing raw ms against ns (or forgetting ×1_000_000)
        // would settle ~a million times too eagerly.
        let last = 1_000_000_000u64;
        let quiet_ms = 300u64;
        assert!(!settle_is_quiet(last + 299_999_999, last, quiet_ms));
        assert!(settle_is_quiet(last + 300_000_000, last, quiet_ms));
        assert!(settle_is_quiet(last + 300_000_001, last, quiet_ms));
    }

    #[test]
    fn settle_math_already_quiet_returns_true_immediately() {
        // A pane whose last mutation is far in the past is settled at call
        // time (LENS-R-020 immediate return; S4's second call). `now` well
        // beyond last + quiet.
        let last = 5_000_000_000u64;
        assert!(settle_is_quiet(last + 2_000_000_000, last, 300));
    }

    #[test]
    fn settle_math_remaining_quiet_shrinks_then_zeroes() {
        let last = 1_000_000_000u64;
        let quiet_ms = 300u64; // 300_000_000 ns
        // Just after a mutation: nearly the whole window remains.
        assert_eq!(settle_remaining_quiet_ns(last, last, quiet_ms), 300_000_000);
        // Half elapsed → half remains.
        assert_eq!(
            settle_remaining_quiet_ns(last + 150_000_000, last, quiet_ms),
            150_000_000
        );
        // Fully elapsed → zero (never negative/underflow).
        assert_eq!(
            settle_remaining_quiet_ns(last + 300_000_000, last, quiet_ms),
            0
        );
        assert_eq!(
            settle_remaining_quiet_ns(last + 900_000_000, last, quiet_ms),
            0
        );
    }

    #[test]
    fn settle_math_saturates_on_backwards_clock() {
        // A `now` below `last_mutation_ns` (impossible on a monotonic clock,
        // but the guard must not underflow into a bogus "quiet forever").
        assert!(!settle_is_quiet(500, 1_000, 300));
        assert_eq!(settle_remaining_quiet_ns(500, 1_000, 300), 300_000_000);
    }

    #[test]
    fn settle_param_bounds_accept_valid_and_defaults() {
        // Defaults (300 / 10_000) are valid.
        assert!(validate_wait_settled_params(300, 10_000).is_ok());
        // Exact boundaries are inclusive.
        assert!(validate_wait_settled_params(SETTLE_QUIET_MIN_MS, SETTLE_QUIET_MIN_MS).is_ok());
        assert!(validate_wait_settled_params(SETTLE_QUIET_MAX_MS, SETTLE_TIMEOUT_MAX_MS).is_ok());
        // timeout == quiet is allowed (range is [quiet, 600_000]).
        assert!(validate_wait_settled_params(500, 500).is_ok());
    }

    #[test]
    fn settle_param_bounds_reject_out_of_range() {
        // quiet below min (V1 case: 5 ms) → INVALID_PARAMS.
        let e = validate_wait_settled_params(5, 10_000).unwrap_err();
        assert_eq!(e.code, shux_rpc::ErrorCode::InvalidParams.code());
        // quiet above max.
        assert!(validate_wait_settled_params(60_001, 60_001).is_err());
        // timeout below quiet (V1 case: quiet 300, timeout 100) → INVALID_PARAMS.
        let e = validate_wait_settled_params(300, 100).unwrap_err();
        assert_eq!(e.code, shux_rpc::ErrorCode::InvalidParams.code());
        // timeout above max.
        assert!(validate_wait_settled_params(300, 600_001).is_err());
    }

    #[test]
    fn settle_equal_deadlines_prefers_settled() {
        // codex P3 B1: with timeout_ms == quiet_ms (allowed — LENS-R-025's
        // timeout lower bound IS quiet_ms), a pane quiet exactly at the shared
        // deadline must return settled:true, not a timeout. Model the wake at
        // the exact shared deadline: quiet satisfied to the nanosecond AND the
        // timeout elapsed — quiet wins.
        let last = 1_000_000_000u64;
        let quiet_ms = 300u64;
        let now_ns = last + 300_000_000; // exactly quiet
        let quiet = settle_is_quiet(now_ns, last, quiet_ms);
        assert!(quiet);
        assert_eq!(
            settle_decide(quiet, /*past_timeout*/ true, /*pending*/ false),
            SettleWake::Settled,
            "quiet at the shared deadline must settle, not time out"
        );
    }

    #[test]
    fn settle_late_wake_past_timeout_with_quiet_satisfied_settles() {
        // codex P3 B1 second face: a scheduler that wakes the loop LATE (well
        // past the timeout deadline) must still report settled when the quiet
        // window was satisfied — the old code returned timeout on any
        // post-deadline wake without re-evaluating quiet first.
        let last = 1_000_000_000u64;
        let quiet_ms = 300u64;
        let now_ns = last + 5_000_000_000; // woke 4.7s late; quiet long since satisfied
        let quiet = settle_is_quiet(now_ns, last, quiet_ms);
        assert!(quiet);
        assert_eq!(
            settle_decide(quiet, /*past_timeout*/ true, /*pending*/ false),
            SettleWake::Settled,
            "late wake after the deadline with quiet satisfied must settle"
        );
    }

    #[test]
    fn settle_revision_in_return_window_does_not_settle() {
        // claude P3 TOCTOU guard: a revision published AFTER the
        // borrow_and_update snapshot but BEFORE the settled return must
        // restart the evaluation. Drive the REAL mechanism: a watch channel
        // whose pending state is read exactly the way the handler reads it.
        let (tx, mut rx) = watch::channel(PaneRevision {
            content_revision: 7,
            last_mutation_ns: 1_000,
        });
        let snapshot = *rx.borrow_and_update();
        // Quiet is satisfied ON THE SNAPSHOT (stale view says "still")...
        let now_ns = snapshot.last_mutation_ns + 400_000_000;
        let quiet = settle_is_quiet(now_ns, snapshot.last_mutation_ns, 300);
        assert!(quiet);
        // ...but a new revision lands in the return window.
        tx.send(PaneRevision {
            content_revision: 8,
            last_mutation_ns: now_ns,
        })
        .expect("send");
        let pending = rx.has_changed().expect("channel open");
        assert!(pending, "the in-window revision must be visible as pending");
        assert_eq!(
            settle_decide(quiet, false, pending),
            SettleWake::KeepWaiting,
            "a pending revision must restart evaluation, never settle stale"
        );
        // The restart sees the fresh value: no longer quiet at `now_ns`.
        let fresh = *rx.borrow_and_update();
        assert_eq!(fresh.content_revision, 8);
        assert!(!settle_is_quiet(now_ns, fresh.last_mutation_ns, 300));
        // And if the timeout has ALSO elapsed by then, the restart reports an
        // honest timeout on the fresh revision (not a stale settled).
        assert_eq!(settle_decide(false, true, false), SettleWake::TimedOut);
    }

    #[test]
    fn settle_decide_priority_table() {
        use SettleWake::*;
        // pending > quiet > timeout > wait — the full truth table.
        assert_eq!(settle_decide(true, true, true), KeepWaiting);
        assert_eq!(settle_decide(true, false, true), KeepWaiting);
        assert_eq!(settle_decide(false, true, true), KeepWaiting);
        assert_eq!(settle_decide(false, false, true), KeepWaiting);
        assert_eq!(settle_decide(true, true, false), Settled);
        assert_eq!(settle_decide(true, false, false), Settled);
        assert_eq!(settle_decide(false, true, false), TimedOut);
        assert_eq!(settle_decide(false, false, false), KeepWaiting);
    }

    #[test]
    fn settle_waiter_subscribe_and_drop_is_bounded() {
        // LENS-R-023: a waiter is just a `watch::Receiver` subscription; when
        // the waiter future is dropped (client disconnect), the receiver
        // drops with it and the daemon does NOT grow. Prove the primitive:
        // subscribing adds a receiver, dropping removes it, and the sender
        // survives with zero receivers (a torn-down waiter never wedges the
        // pane's publisher).
        let (tx, rx0) = watch::channel(PaneRevision {
            content_revision: 1,
            last_mutation_ns: 1,
        });
        assert_eq!(tx.receiver_count(), 1);
        let waiter_a = tx.subscribe();
        let waiter_b = tx.subscribe();
        assert_eq!(tx.receiver_count(), 3);
        drop(waiter_a);
        drop(waiter_b);
        assert_eq!(tx.receiver_count(), 1);
        drop(rx0);
        assert_eq!(tx.receiver_count(), 0);
        // Publisher still usable with no waiters — send_if_modified reports a
        // real change and does not error on the receiver-less channel.
        let changed = tx.send_if_modified(|cur| {
            cur.content_revision = 2;
            true
        });
        assert!(changed);
        assert_eq!(tx.borrow().content_revision, 2);
    }
}
