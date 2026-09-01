use std::collections::VecDeque;
use std::ops::{Deref, DerefMut, Index, IndexMut, Range};
use std::sync::Arc;

use crate::cell::{Cell, Color};

/// A half-open dirty cell range on one visible row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirtyRegion {
    /// Visible row index.
    pub row: usize,
    /// Dirty columns in `start..end` form.
    pub cols: Range<usize>,
}

#[derive(Debug, Clone)]
struct DirtyState {
    enabled: bool,
    full_frame: bool,
    any_dirty: bool,
    last_full_row: Option<usize>,
    full_rows: Vec<bool>,
    rows: Vec<Option<Range<usize>>>,
}

impl DirtyState {
    fn new(rows: usize, enabled: bool) -> Self {
        DirtyState {
            enabled,
            full_frame: false,
            any_dirty: false,
            last_full_row: None,
            full_rows: vec![false; rows],
            rows: vec![None; rows],
        }
    }

    fn is_dirty(&self) -> bool {
        self.enabled && self.any_dirty
    }

    fn resize_rows(&mut self, rows: usize) {
        self.full_rows.resize(rows, false);
        self.rows.resize(rows, None);
    }

    /// Return to the state of `DirtyState::new(rows, enabled)` without giving
    /// the two row vectors back to the allocator. Used when a retired grid is
    /// recycled (issue #106).
    fn reset(&mut self, rows: usize, enabled: bool) {
        self.enabled = enabled;
        self.full_frame = false;
        self.any_dirty = false;
        self.last_full_row = None;
        self.full_rows.clear();
        self.full_rows.resize(rows, false);
        self.rows.clear();
        self.rows.resize(rows, None);
    }

    fn mark_all(&mut self) {
        if !self.enabled {
            return;
        }
        self.last_full_row = None;
        self.full_frame = true;
        self.any_dirty = true;
    }

    fn mark_rows(&mut self, start: usize, end: usize, rows: usize, cols: usize) {
        if !self.enabled || self.full_frame {
            return;
        }
        let end = end.min(rows);
        for row in start.min(rows)..end {
            self.mark_row(row, rows, cols);
        }
    }

    fn mark_row(&mut self, row: usize, rows: usize, cols: usize) {
        if !self.enabled || self.full_frame || row >= rows || cols == 0 {
            return;
        }
        if self.last_full_row == Some(row) {
            return;
        }
        if self.row_is_fully_dirty(row, cols) {
            self.last_full_row = Some(row);
            return;
        }
        self.any_dirty = true;
        self.full_rows[row] = true;
        self.last_full_row = Some(row);
        self.rows[row] = Some(0..cols);
    }

    fn mark_range(&mut self, row: usize, range: Range<usize>, rows: usize, cols: usize) {
        if !self.enabled || self.full_frame || self.row_is_fully_dirty(row, cols) {
            return;
        }
        self.last_full_row = None;
        if row >= rows || cols == 0 {
            return;
        }
        let start = range.start.min(cols);
        let end = range.end.min(cols);
        if start >= end {
            return;
        }
        self.any_dirty = true;
        let slot = &mut self.rows[row];
        match slot {
            Some(existing) => {
                existing.start = existing.start.min(start);
                existing.end = existing.end.max(end);
                if existing.start == 0 && existing.end >= cols {
                    self.full_rows[row] = true;
                }
            }
            None => {
                if start == 0 && end >= cols {
                    self.full_rows[row] = true;
                }
                *slot = Some(start..end);
            }
        }
    }

    fn should_mark_row(&self, row: usize, rows: usize, cols: usize) -> bool {
        self.enabled
            && !self.full_frame
            && row < rows
            && cols > 0
            && !self.row_is_fully_dirty(row, cols)
    }

    fn row_is_fully_dirty(&self, row: usize, _cols: usize) -> bool {
        self.rows
            .get(row)
            .is_some_and(|_| self.full_rows.get(row).copied().unwrap_or(false))
    }

    fn take(&mut self, rows: usize, cols: usize) -> Vec<DirtyRegion> {
        if !self.enabled || !self.any_dirty {
            return Vec::new();
        }

        let regions = if self.full_frame {
            (0..rows)
                .filter(|_| cols > 0)
                .map(|row| DirtyRegion { row, cols: 0..cols })
                .collect()
        } else {
            self.rows
                .iter_mut()
                .enumerate()
                .filter_map(|(row, range)| {
                    range.take().and_then(|cols_range| {
                        let start = cols_range.start.min(cols);
                        let end = cols_range.end.min(cols);
                        (row < rows && start < end).then_some(DirtyRegion {
                            row,
                            cols: start..end,
                        })
                    })
                })
                .collect()
        };

        self.full_frame = false;
        self.any_dirty = false;
        self.last_full_row = None;
        self.full_rows.fill(false);
        self.rows.fill(None);
        regions
    }
}

#[derive(Debug, Default)]
struct LogicalLine {
    cells: Vec<Cell>,
    display_width: usize,
}

#[derive(Debug, Clone, Copy)]
struct CursorAnchor {
    logical_line: usize,
    display_offset: usize,
}

#[derive(Debug)]
struct ReflowedLineMap {
    range: std::ops::Range<usize>,
    cells: Vec<ReflowedCellPosition>,
    end_row: usize,
    end_col: usize,
    display_width: usize,
}

#[derive(Debug)]
struct ReflowedCellPosition {
    offset: usize,
    row: usize,
    col: usize,
    width: usize,
}

/// A single row of terminal cells.
///
/// ## Why the cells sit behind an `Arc` (issue #115)
///
/// A row is the unit shux copies. Cloning a `Grid` — which synchronized
/// output (`CSI ?2026h`), `pane capture` and every snapshot path do — used to
/// deep-copy every cell of every line, scrollback included: 29 MB for a
/// 240x64 pane holding 5000 lines of history, bought by sixteen bytes a pane
/// chooses to emit.
///
/// The cells are shared instead, and copied only when a shared row is written
/// to (`Row::cells_mut` -> `Arc::make_mut`). A grid clone is then a walk of
/// line pointers rather than of cells: proportional to the number of lines,
/// not to their contents. A row that is never written after a clone is never
/// copied at all, and a row that IS written pays exactly one copy of itself —
/// the same bytes the write was always going to touch.
///
/// The uniqueness check `Arc::make_mut` performs on every write is two
/// relaxed atomic loads on the uncontended path, which is the hot path: a row
/// with a refcount of one is mutated in place exactly as before.
#[derive(Debug, Clone)]
pub struct Row {
    /// Cell storage, shared copy-on-write with every clone of this row.
    /// Read through `Deref` (`row.cells.len()`, `row.cells.iter()`); written
    /// only through [`Row::cells_mut`], which unshares first.
    pub(crate) cells: Arc<Vec<Cell>>,
    /// Whether this row soft-wraps into the next row.
    pub wrapped: bool,
}

/// Mutable access to one visible row.
///
/// Dropping the guard marks the whole row dirty. This makes direct cell writes
/// in the parser dirty by construction instead of relying on a parallel mark
/// call that can drift from the actual mutation.
pub struct RowMut<'a> {
    row: &'a mut Row,
    dirty: &'a mut DirtyState,
    row_idx: usize,
    rows: usize,
    cols: usize,
    mark_on_drop: bool,
}

impl Deref for RowMut<'_> {
    type Target = Row;

    fn deref(&self) -> &Self::Target {
        self.row
    }
}

impl DerefMut for RowMut<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.row
    }
}

impl Drop for RowMut<'_> {
    fn drop(&mut self) {
        if self.mark_on_drop {
            self.dirty.mark_row(self.row_idx, self.rows, self.cols);
        }
    }
}

impl Row {
    pub fn new(cols: usize) -> Self {
        Row {
            cells: Arc::new(vec![Cell::default(); cols]),
            wrapped: false,
        }
    }

    /// Unshare and borrow the cells for writing.
    ///
    /// This is the ONLY way to mutate a row's cells, which is what makes the
    /// copy-on-write sharing safe: a row still shared with a frozen
    /// presentation or a snapshot is copied here, before the write lands, so
    /// no reader of the other side can ever see the write.
    #[inline]
    pub(crate) fn cells_mut(&mut self) -> &mut Vec<Cell> {
        Arc::make_mut(&mut self.cells)
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    pub fn get(&self, col: usize) -> Option<&Cell> {
        self.cells.get(col)
    }

    /// Resize the row, filling new cells with the given template.
    pub fn resize(&mut self, cols: usize, template: Cell) {
        self.cells_mut().resize(cols, template);
    }

    /// Return the row to the state of `Row::new(cols)`, keeping the cell
    /// vector's allocation. `clear` + `resize` reuses the existing buffer
    /// whenever it is already wide enough, which it is for every recycled
    /// alternate-screen buffer at an unchanged pane width.
    pub(crate) fn reset_blank(&mut self, cols: usize) {
        let cells = self.cells_mut();
        cells.clear();
        cells.resize(cols, Cell::default());
        self.wrapped = false;
    }

    /// Reset all cells in the row to the given background color.
    pub fn reset(&mut self, bg: Color) {
        for cell in self.cells_mut() {
            cell.reset(bg);
        }
        self.wrapped = false;
    }

    /// Overwrite every cell with the DECALN alignment pattern (issue #117).
    ///
    /// Structurally identical to [`Row::reset`], and deliberately so: it goes
    /// through `cells_mut`, so a row still shared with a frozen presentation or
    /// a snapshot is copied before the write lands, and it clears `wrapped`,
    /// because a screen of independent `E`s is not a soft-wrapped logical line
    /// and must not be reflowed back into one on the next resize.
    ///
    /// Writing a whole `Cell::ALIGNMENT` per cell — rather than assigning
    /// `ch` — is what drops the extended payload and the wide-pair widths that
    /// the previous contents may have carried.
    pub(crate) fn fill_alignment_pattern(&mut self) {
        self.cells_mut().fill(Cell::ALIGNMENT);
        self.wrapped = false;
    }

    pub(crate) fn clear_wide_pair_around(&mut self, col: usize, bg: Color) {
        if col >= self.cells.len() {
            return;
        }

        let cells = self.cells_mut();
        if cells[col].is_wide_continuation() {
            cells[col].reset(bg);
            if col > 0 && cells[col - 1].is_wide() {
                cells[col - 1].reset(bg);
            }
        } else if cells[col].is_wide() {
            cells[col].reset(bg);
            if col + 1 < cells.len() && cells[col + 1].is_wide_continuation() {
                cells[col + 1].reset(bg);
            }
        }
    }

    pub(crate) fn sanitize_wide_pairs(&mut self, bg: Color) {
        let cells = self.cells_mut();
        for col in 0..cells.len() {
            if cells[col].is_wide() {
                let has_tail = col + 1 < cells.len()
                    && cells[col + 1].is_wide_continuation()
                    && cells[col + 1].ch == ' ';
                if !has_tail {
                    cells[col].reset(bg);
                }
            } else if cells[col].is_wide_continuation() {
                let has_head = col > 0 && cells[col - 1].is_wide();
                if !has_head || cells[col].ch != ' ' {
                    cells[col].reset(bg);
                }
            }
        }
    }

    fn erase_chars_expanding_wide_pairs(
        &mut self,
        col: usize,
        count: usize,
        bg: Color,
    ) -> Option<Range<usize>> {
        let len = self.cells.len();
        let mut start = col.min(len);
        let mut end = col.saturating_add(count).min(len);
        if start >= end {
            return None;
        }

        if start > 0 && self.cells[start].is_wide_continuation() && self.cells[start - 1].is_wide()
        {
            start -= 1;
        }
        if end < len
            && end > 0
            && self.cells[end - 1].is_wide()
            && self.cells[end].is_wide_continuation()
        {
            end += 1;
        }

        let cells = self.cells_mut();
        for cell in &mut cells[start..end] {
            cell.reset(bg);
        }
        Some(start..end)
    }

    /// Check if the row is entirely empty (all default spaces).
    pub fn is_blank(&self) -> bool {
        self.cells
            .iter()
            .all(|c| c.ch == ' ' && c.style == Default::default())
    }
}

impl Index<usize> for Row {
    type Output = Cell;

    fn index(&self, col: usize) -> &Cell {
        &self.cells[col]
    }
}

impl IndexMut<usize> for Row {
    fn index_mut(&mut self, col: usize) -> &mut Cell {
        &mut self.cells_mut()[col]
    }
}

/// Configuration for the grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridConfig {
    /// Maximum number of scrollback lines. Default: 5000 (PRD 5.5).
    pub max_scrollback: usize,
    /// Track visible viewport dirtiness. Enabled for production grids; tests and
    /// benchmarks can disable it to measure tracking overhead directly.
    pub track_dirty: bool,
}

impl Default for GridConfig {
    fn default() -> Self {
        GridConfig {
            max_scrollback: 5000,
            track_dirty: true,
        }
    }
}

/// Placements one grid may hold.
const MAX_PLACEMENTS: usize = 256;

/// A rectangle in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellRect {
    pub col: usize,
    pub row: usize,
    pub cols: usize,
    pub rows: usize,
}

/// A placement resolved into a composed multi-pane frame.
///
/// Separate from [`Placement`] because a composed frame numbers rows from zero,
/// while any picture taller than its pane anchors above it. The clip is carried
/// rather than inferred: the rasterizer clips to the canvas, which in a
/// composed frame is the whole window rather than one pane.
#[derive(Debug, Clone)]
pub struct ComposedPlacement {
    pub image: std::sync::Arc<crate::graphics::image::StoredImage>,
    /// Composed-frame row of the image's top. Negative means the top is above
    /// `clip`: draw from `clip.row`, skipping that many cell rows of bitmap.
    pub row: i64,
    pub col: usize,
    pub clip: CellRect,
}

/// A picture placed in this grid, anchored to an absolute line.
///
/// The anchor is `evicted() + index in raw`, so it names the same LINE after
/// any scroll and needs no rebasing across `clone_visible` or a freeze: both
/// advance `evicted` by the history they drop.
#[derive(Debug, Clone)]
pub struct Placement {
    pub image: std::sync::Arc<crate::graphics::image::StoredImage>,
    pub abs_row: u64,
    pub col: usize,
}

impl Placement {
    /// Viewport row this placement starts on, NEGATIVE once its anchor has
    /// scrolled above the viewport. Signed rather than `Option`, because an
    /// image taller than the pane is ordinary and most of it is still on
    /// screen when its anchor line is not.
    pub fn viewport_row(&self, grid: &Grid) -> i64 {
        let first_visible = grid.evicted() + grid.scrollback_len() as u64;
        self.abs_row as i64 - first_visible as i64
    }
}

/// VecDeque-based terminal grid with scrollback.
///
/// The grid is organized as:
///   - scrollback lines (index 0..scrollback_len): lines that have scrolled off the top
///   - visible lines (index scrollback_len..scrollback_len+rows): the current viewport
///
/// The VecDeque allows O(1) push_front (for scrollback) and O(1) push_back (for new lines).
#[derive(Debug)]
pub struct Grid {
    /// All lines: scrollback + visible area.
    raw: VecDeque<Row>,
    /// Number of visible rows (terminal height).
    rows: usize,
    /// Number of columns (terminal width).
    cols: usize,
    /// Configuration (max scrollback, etc.).
    config: GridConfig,
    /// Dirty visible viewport state.
    dirty: DirtyState,
    /// Value-INDEPENDENT monotonic write tally (lens ContentRevision substrate,
    /// PRD §4). Bumped on every cell/scroll/erase/clear write regardless of
    /// whether the resulting value changed, so identical repaints still count
    /// (§4.2 "MUST NOT diff to decide"). Deliberately NOT `DirtyState`: it is
    /// never drained/coalesced, so a concurrently attached render client that
    /// drains dirty regions cannot make a lens reader miss a write (§4.4).
    mutations: u64,
    /// Pictures placed in this grid. The pixels sit behind an `Arc`, so a
    /// clone is a refcount bump, not a bitmap copy.
    placements: Vec<Placement>,
    /// Payload bytes the placements hold, to bound what one pane can retain.
    placed_bytes: usize,
    /// Monotonic count of lines that have fallen off the FRONT of `raw` —
    /// scrolled past the scrollback cap, reflowed away, or cleared.
    ///
    /// A frozen presentation (issue #115) holds only the viewport and reads
    /// history straight out of this grid, so it needs to know how far the
    /// history it remembers has shifted underneath it. Never decreases, so the
    /// difference between two readings is the number of lines that went.
    evicted: u64,
}

impl Clone for Grid {
    fn clone(&self) -> Self {
        Grid {
            raw: self.raw.clone(),
            rows: self.rows,
            cols: self.cols,
            config: self.config.clone(),
            dirty: DirtyState::new(self.rows, self.config.track_dirty),
            mutations: self.mutations,
            evicted: self.evicted,
            placements: self.placements.clone(),
            placed_bytes: self.placed_bytes,
        }
    }
}

impl Grid {
    /// Pictures placed in this grid.
    pub fn placements(&self) -> &[Placement] {
        &self.placements
    }

    /// Add a placement, refusing past either cap. A pane can emit unbounded
    /// `a=T`, and a tiny image charges almost no bytes while still costing a
    /// pass per snapshot, so neither cap alone suffices.
    pub(crate) fn place(&mut self, p: Placement) -> bool {
        self.prune_evicted_placements();
        let cost = p.image.payload.len();
        if self.placements.len() >= MAX_PLACEMENTS
            || self.placed_bytes + cost > crate::graphics::image::MAX_IMAGE_BYTES
        {
            return false;
        }
        self.placed_bytes += cost;
        self.placements.push(p);
        self.bump_mutations();
        true
    }

    /// Drop placements whose last row has fallen off the front of the grid.
    ///
    /// Scrolling advances `evicted` but never removed placements, so a
    /// long-lived pane spent a slot per image forever: after 256 `kitten icat`
    /// runs every further one was refused, none of them displayable. Pruning is
    /// conservative -- a placement goes only once even its bottom row is out of
    /// reach of scrollback.
    fn prune_evicted_placements(&mut self) {
        let evicted = self.evicted;
        let cell_h = u64::from(crate::DECLARED_CELL_PIXELS.1.max(1));
        let mut freed = 0usize;
        self.placements.retain(|p| {
            let rows = u64::from(p.image.height).div_ceil(cell_h).max(1);
            let live = p.abs_row.saturating_add(rows) > evicted;
            if !live {
                freed += p.image.payload.len();
            }
            live
        });
        self.placed_bytes = self.placed_bytes.saturating_sub(freed);
    }

    /// Drop every placement -- `a=d,d=A`, the only target a real client sends.
    pub(crate) fn unplace_all(&mut self) {
        if self.placements.is_empty() {
            return;
        }
        self.placements.clear();
        self.placed_bytes = 0;
        self.bump_mutations();
    }

    /// Lines that have fallen off the front of this grid over its lifetime —
    /// and so the absolute index of `raw[0]`, the coordinate every row here is
    /// numbered from.
    ///
    /// A row's absolute index is `evicted() + its index in raw`, so the first
    /// VISIBLE row sits at `evicted() + scrollback_len()`.
    ///
    /// The invariant every producer of a `Grid` owes: dropping rows off the
    /// front ADVANCES this.
    pub(crate) fn evicted(&self) -> u64 {
        self.evicted
    }

    /// Clone JUST the visible viewport, and its pending repaint state, as a
    /// grid of its own.
    ///
    /// The pending state is the half that is easy to drop. The ordinary clone
    /// hands back a grid nothing is known to be stale in, which is right for a
    /// snapshot that is about to be rendered once and thrown away. A freeze is
    /// the other case: the frozen buffer takes over as the thing a live
    /// renderer is incrementally tracking, so it has to inherit the rows that
    /// renderer has not drawn yet or they are never drawn at all.
    ///
    /// This is what a synchronized-output freeze keeps (issue #115). It
    /// deliberately does NOT keep history, for two reasons that are really the
    /// same reason:
    ///
    /// - **Taking it would cost history.** Copying the whole grid means one
    ///   pointer per retained line — 5,000 of them on a pane that has been
    ///   used — for every window a pane opens, and a pane opens them as fast
    ///   as it can write sixteen bytes.
    /// - **Holding it would cost history twice over.** Every line the frozen
    ///   frame keeps a reference to is a line the live grid can no longer
    ///   recycle as it scrolls, so it must allocate a replacement instead. A
    ///   pane that scrolls its whole history inside one window would pay for a
    ///   copy of all of it.
    ///
    /// Neither applies to the viewport: it is a fixed number of rows, and it
    /// is the only part of the grid the frozen frame actually has to hold
    /// still. History is not part of the presented FRAME — it is read live,
    /// through [`crate::VirtualTerminal::presented_row`], which shifts its
    /// indices by whatever has been evicted since the freeze.
    ///
    /// The scrollback budget is `rows` rather than zero so that a reflow
    /// landing inside a window (a resize) has somewhere to put lines that no
    /// longer fit, exactly as it would in the full grid.
    pub(crate) fn clone_presented_viewport(&self) -> Grid {
        let sb = self.scrollback_len();
        let mut raw = VecDeque::with_capacity(self.rows);
        for row in self.raw.iter().skip(sb) {
            raw.push_back(row.clone());
        }
        Grid {
            raw,
            rows: self.rows,
            cols: self.cols,
            config: GridConfig {
                max_scrollback: self.rows,
                track_dirty: self.config.track_dirty,
            },
            dirty: self.dirty.clone(),
            mutations: self.mutations,
            evicted: self.evicted.saturating_add(sb as u64),
            placements: self.placements.clone(),
            placed_bytes: self.placed_bytes,
        }
    }
}

impl Grid {
    /// Create a new grid with the given dimensions.
    pub fn new(rows: usize, cols: usize, config: GridConfig) -> Self {
        let mut raw = VecDeque::with_capacity(rows);
        for _ in 0..rows {
            raw.push_back(Row::new(cols));
        }
        Grid {
            raw,
            rows,
            cols,
            dirty: DirtyState::new(rows, config.track_dirty),
            evicted: 0,
            config,
            mutations: 0,
            placements: Vec::new(),
            placed_bytes: 0,
        }
    }

    /// Whether this grid is indistinguishable from a freshly built
    /// `Grid::new(rows, cols, config)`.
    ///
    /// `mutations` is the load-bearing half: it advances on every cell write,
    /// scroll, erase and clear, and it is value-INDEPENDENT, so it cannot be
    /// fooled by a write that happened to restore the previous value. A grid
    /// that starts blank and has a tally of zero therefore still IS blank. The
    /// dimensions and line count cover the two restructuring operations that
    /// deliberately do not advance the tally (`resize*` and `clear_scrollback`).
    ///
    /// Used to recycle a retired alternate-screen buffer without re-blanking
    /// it (issue #106): a pane that toggles the alternate screen without
    /// drawing gets the same buffer back untouched.
    ///
    /// The tally argument is about PROVENANCE, not content — it says "nothing
    /// wrote here since this grid was built blank", which is only a blankness
    /// proof for a grid that started blank. [`Grid::clone_visible`] produces a
    /// grid with content and a zero tally and would fail that premise; it
    /// cannot reach the recycling slot, and [`Grid::assert_blank`] is asserted
    /// on every reuse in debug builds so a future path that could is caught by
    /// the test suite rather than by a user.
    pub(crate) fn is_blank_canvas(&self, rows: usize, cols: usize, config: &GridConfig) -> bool {
        self.mutations == 0
            && self.rows == rows
            && self.cols == cols
            && self.raw.len() == rows
            && &self.config == config
    }

    /// Whether every cell really is a default cell on an unwrapped, full-width
    /// row. The direct O(cells) check that [`Grid::is_blank_canvas`] stands in
    /// for; asserted behind `debug_assertions` wherever the cheap check licenses
    /// skipping work, so the tests prove the two agree on every reuse instead
    /// of the invariant being argued in a comment.
    pub(crate) fn is_actually_blank(&self, cols: usize) -> bool {
        self.placements.is_empty()
            && self.raw.iter().all(|row| {
                !row.wrapped && row.len() == cols && row.cells.iter().all(|c| *c == Cell::EMPTY)
            })
    }

    /// Return this grid to the state of `Grid::new(rows, cols, config)`,
    /// reusing the row allocations it already holds.
    ///
    /// The point is the allocator, not the writes: blanking the cells costs
    /// the same as zeroing a fresh grid would, but the malloc/free round trip
    /// per row disappears — and that round trip is what a pane could buy in
    /// bulk with an eight-byte escape sequence (issue #106).
    pub(crate) fn reset_blank(&mut self, rows: usize, cols: usize, config: GridConfig) {
        self.raw.truncate(rows);
        for row in self.raw.iter_mut() {
            row.reset_blank(cols);
        }
        while self.raw.len() < rows {
            self.raw.push_back(Row::new(cols));
        }
        self.rows = rows;
        self.cols = cols;
        self.dirty.reset(rows, config.track_dirty);
        self.config = config;
        self.mutations = 0;
        // Part of "the state of `Grid::new`": see `evicted`. This function
        // assigns fields rather than building a literal, so the compiler does
        // not check it -- every new field must be reset here by hand, and a
        // recycled alt-screen buffer would otherwise carry placements across.
        self.evicted = 0;
        self.placements.clear();
        self.placed_bytes = 0;
    }

    /// Monotonic count of cell/scroll/clear write operations on this grid
    /// (lens ContentRevision substrate, PRD §4). Value-independent: identical
    /// repaints still advance it. The VT compares this before/after a
    /// `process()` batch to decide a Class-A bump; it is never drained.
    pub fn mutations(&self) -> u64 {
        self.mutations
    }

    #[inline]
    fn bump_mutations(&mut self) {
        // saturating_add to match the spec's "never wraps" (PRD §4 / PR #87
        // greptile P2) and record_class_a_batch's counter op. u64 exhaustion
        // is unreachable in practice; the batch compare uses !=, so even a
        // saturated tally only ever under-reports, never wraps backwards.
        self.mutations = self.mutations.saturating_add(1);
    }

    /// Number of visible rows.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Number of columns.
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// Whether the visible viewport has changed since the last dirty drain.
    pub fn is_dirty(&self) -> bool {
        self.dirty.is_dirty()
    }

    /// Consume and clear dirty regions for the visible viewport.
    ///
    /// Cursor movement is intentionally outside this grid dirty API; renderers
    /// that draw a cursor overlay must track cursor presentation separately.
    pub fn take_dirty_regions(&mut self) -> Vec<DirtyRegion> {
        self.dirty.take(self.rows, self.cols)
    }

    /// Mark the full visible viewport dirty.
    ///
    /// Deliberately does NOT advance the content tally (`mutations()`): this is
    /// generic RENDER invalidation. OSC 10/11/12 dynamic default colors and
    /// their 110/111/112 resets are Class A since the P2 re-adjudication, but
    /// their bump comes from the VT's before/after `default_colors` compare —
    /// never from this call. OSC 4 palette redefinition remains Class B
    /// (adjudicated known limitation); sync-mode leave defers through the
    /// VT's hidden-batch flag. Class-A repaints advance the tally through
    /// their own write calls (RIS via `clear_visible`, repaints via row
    /// writes) or are detected by the VT's alt-flag comparison.
    pub fn mark_all_dirty(&mut self) {
        self.dirty.mark_all();
    }

    /// Number of scrollback lines above the visible area.
    pub fn scrollback_len(&self) -> usize {
        self.raw.len().saturating_sub(self.rows)
    }

    /// Total number of lines (scrollback + visible).
    pub fn total_lines(&self) -> usize {
        self.raw.len()
    }

    /// Access a visible row (0 = top of visible area).
    pub fn visible_row(&self, row: usize) -> &Row {
        let idx = self.scrollback_len() + row;
        &self.raw[idx]
    }

    /// Clone just the visible viewport into a fresh `Grid` with no
    /// scrollback. Intended for `pane.snapshot` — `Clone` on the full
    /// grid would copy the entire scrollback (default 5000 rows) under
    /// the daemon's pane-IO mutex even though the rasterizer only ever
    /// reads `visible_row(0..rows)`. Codex review: the cost was paid
    /// even on snapshots later rejected by the pixel-count cap.
    pub fn clone_visible(&self) -> Grid {
        let mut raw = VecDeque::with_capacity(self.rows);
        for r in 0..self.rows {
            raw.push_back(self.visible_row(r).clone());
        }
        Grid {
            raw,
            rows: self.rows,
            cols: self.cols,
            // Snapshot grids never need scrollback — the parser isn't
            // going to feed them more rows.
            config: GridConfig {
                max_scrollback: 0,
                track_dirty: self.config.track_dirty,
            },
            dirty: DirtyState::new(self.rows, self.config.track_dirty),
            // A read-only clone for snapshotting; the tally is irrelevant here.
            mutations: 0,
            evicted: self.evicted.saturating_add(self.scrollback_len() as u64),
            placements: self.placements.clone(),
            placed_bytes: self.placed_bytes,
        }
    }

    /// Lens `pane.glance` text extraction (PRD §5, LENS-R-012): the
    /// ANSI-free viewport text of ALL `rows` (fixed count, no scrollback —
    /// callers pass a `clone_visible()` clone so there IS no scrollback to
    /// accidentally include), rows joined by `\n`. Blank cells push a
    /// literal `' '`, so every row comes out PADDED to its full display
    /// width — trailing whitespace is preserved, never trimmed. There is
    /// deliberately no trimming mechanism here: full-width padding IS the
    /// LENS-R-012 byte-stability contract.
    ///
    /// Deliberately distinct from `VirtualTerminal::capture_text`, which is
    /// tuned for "recent visible output" (drops trailing all-blank rows,
    /// `trim_end()`s each row) — glance wants byte-stable, fixed-shape text
    /// so `text.lines().nth(row)` always lines up with grid row `row`.
    pub fn glance_text(&self) -> String {
        (0..self.rows)
            .map(|row_idx| {
                let row = self.visible_row(row_idx);
                let mut line = String::with_capacity(self.cols);
                for cell in row.cells.iter() {
                    if cell.is_wide_continuation() {
                        continue;
                    }
                    cell.push_display_text(&mut line);
                }
                line
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Mutably access a visible row (0 = top of visible area).
    pub fn visible_row_mut(&mut self, row: usize) -> RowMut<'_> {
        self.bump_mutations();
        let idx = self.visible_abs_index(row);
        let mark_on_drop = self.dirty.should_mark_row(row, self.rows, self.cols);
        let row_ref = &mut self.raw[idx];
        RowMut {
            row: row_ref,
            dirty: &mut self.dirty,
            row_idx: row,
            rows: self.rows,
            cols: self.cols,
            mark_on_drop,
        }
    }

    fn visible_abs_index(&self, row: usize) -> usize {
        self.scrollback_len() + row
    }

    fn visible_row_mut_untracked(&mut self, row: usize) -> &mut Row {
        let idx = self.scrollback_len() + row;
        &mut self.raw[idx]
    }

    /// Mutably access a visible row after marking that row dirty.
    ///
    /// Parser hot paths use this to keep dirty tracking centralized in `Grid`
    /// without paying a drop-guard cost for every printable cell.
    pub(crate) fn visible_row_mut_marked(&mut self, row: usize) -> &mut Row {
        self.bump_mutations();
        self.dirty.mark_row(row, self.rows, self.cols);
        self.visible_row_mut_untracked(row)
    }

    /// Access a scrollback row (0 = oldest scrollback line).
    pub fn scrollback_row(&self, row: usize) -> Option<&Row> {
        if row < self.scrollback_len() {
            Some(&self.raw[row])
        } else {
            None
        }
    }

    /// Access a row by absolute line index across `scrollback + visible`.
    ///
    /// Index 0 is the oldest retained scrollback row. The last index is
    /// the bottom visible row. Copy mode uses this to build a historical
    /// viewport without cloning the whole grid.
    pub fn row(&self, row: usize) -> Option<&Row> {
        self.raw.get(row)
    }

    /// Clamp a caller-supplied scroll region to rows that actually exist.
    ///
    /// `Grid` is the last thing between an escape sequence and the backing
    /// deque, so it does not trust the region it is handed. A region naming
    /// rows the grid does not have used to make `scroll_up` remove nothing and
    /// insert anyway — the deque grew past `scrollback + rows`, or the insert
    /// index was past the end and the pane I/O task panicked (issue #107).
    ///
    /// Returns `None` when there is nothing to scroll: an empty grid, or a
    /// region that is inverted after clamping.
    fn clamp_region(&self, region_top: usize, region_bottom: usize) -> Option<(usize, usize)> {
        let last = self.rows.checked_sub(1)?;
        let bottom = region_bottom.min(last);
        if region_top > bottom {
            return None;
        }
        Some((region_top, bottom))
    }

    /// Blank `raw[idx]` in place, reusing its allocation when it already has
    /// the right width. Equivalent to storing a fresh `Row::new(cols)`:
    /// `Cell::reset(Color::Default)` is exactly `Cell::default()`, and
    /// `Row::reset` clears `wrapped`.
    fn blank_row_at(&mut self, idx: usize) {
        let cols = self.cols;
        let row = &mut self.raw[idx];
        if row.len() == cols {
            row.reset(Color::Default);
        } else {
            *row = Row::new(cols);
        }
    }

    /// Reverse `raw[start..end]` in place.
    fn reverse_range(&mut self, start: usize, end: usize) {
        if end <= start {
            return;
        }
        let (mut i, mut j) = (start, end - 1);
        while i < j {
            self.raw.swap(i, j);
            i += 1;
            j -= 1;
        }
    }

    /// Rotate `raw[start..end]` left by `n` using three reversals.
    ///
    /// This is the whole point of the bulk API: it touches `end - start` deque
    /// slots once, whatever `n` is. The old per-line `remove` + `insert` pair
    /// shifted up to O(rows) slots *per line*, so scrolling a whole region
    /// cost O(rows^2). Reversal-based rotation also works on a `VecDeque`
    /// without `make_contiguous()`, which would itself be O(scrollback).
    fn rotate_range_left(&mut self, start: usize, end: usize, n: usize) {
        let len = end - start;
        if len == 0 {
            return;
        }
        let n = n % len;
        if n == 0 {
            return;
        }
        self.reverse_range(start, start + n);
        self.reverse_range(start + n, end);
        self.reverse_range(start, end);
    }

    /// Mark the rows a scroll of `[top, bottom]` dirtied.
    fn mark_scrolled(&mut self, region_top: usize, region_bottom: usize) {
        if region_top == 0 && region_bottom == self.rows.saturating_sub(1) {
            self.dirty.mark_all();
        } else {
            self.dirty.mark_rows(
                region_top,
                region_bottom.saturating_add(1),
                self.rows,
                self.cols,
            );
        }
    }

    /// Scroll a region up by `n` lines in one bulk operation.
    ///
    /// `n` is clamped to the region height (issue #102): that is the bound on
    /// work an escape sequence can buy, and it is load-bearing, not an
    /// optimisation.
    ///
    /// For a PARTIAL region the clamp is invisible — scrolling further than
    /// the region's height only shuffles blank rows — so `scroll_up_n(t, b, n)`
    /// is exactly `n` calls to [`Grid::scroll_up`].
    ///
    /// For a FULL-SCREEN region it is deliberately NOT equivalent: `n` separate
    /// one-line scrolls would push `n` lines into scrollback, of which
    /// everything past the screen height is blank. This clamps to the screen
    /// height instead, so a single sequence cannot flush the scrollback buffer
    /// with blanks. `CSI S`/`T`/`L`/`M` already clamp `n` before calling, so no
    /// parser path can observe the difference; a direct `Grid` caller can.
    /// `region_scroll_beyond_screen_height_does_not_flood_scrollback` pins it.
    ///
    /// The mutation tally advances once per line actually scrolled.
    pub fn scroll_up_n(&mut self, region_top: usize, region_bottom: usize, n: usize) {
        let Some((top, bottom)) = self.clamp_region(region_top, region_bottom) else {
            return;
        };
        let height = bottom - top + 1;
        let n = n.min(height);
        if n == 0 {
            return;
        }
        self.mutations = self.mutations.saturating_add(n as u64);

        if top == 0 && bottom == self.rows - 1 {
            // Full-screen: the scrolled-off lines become scrollback, and the
            // deque is trimmed back to the cap.
            //
            // Split into the lines that genuinely extend the deque and the
            // lines that only displace an equally old one. Once scrollback is
            // full — the steady state for any long-lived pane — a scroll is a
            // pure recycle: pop the oldest row, blank it, push it back. That
            // is allocation-free and O(1) per line. Pushing all `n` first and
            // trimming afterwards costs the same number of deque operations
            // but allocates `n` fresh rows before freeing `n` old ones, which
            // measured ~1.8x slower on a 1000-row pane because the allocator
            // never gets to reuse the row it just freed.
            let max_total = self.rows + self.config.max_scrollback;
            let grow = n.min(max_total.saturating_sub(self.raw.len()));
            for _ in 0..grow {
                self.raw.push_back(Row::new(self.cols));
            }
            let cols = self.cols;
            for _ in 0..(n - grow) {
                let mut row = match self.raw.pop_front() {
                    Some(row) => row,
                    None => break,
                };
                self.evicted = self.evicted.saturating_add(1);
                if row.len() == cols {
                    row.reset(Color::Default);
                } else {
                    row = Row::new(cols);
                }
                self.raw.push_back(row);
            }
            // Restores the cap if it was already exceeded (a scrollback config
            // change can leave the deque over the line); a no-op otherwise.
            while self.raw.len() > max_total {
                self.raw.pop_front();
                self.evicted = self.evicted.saturating_add(1);
            }
        } else {
            let sb = self.scrollback_len();
            let (start, end) = (sb + top, sb + bottom + 1);
            self.rotate_range_left(start, end, n);
            for idx in (end - n)..end {
                self.blank_row_at(idx);
            }
        }
        self.mark_scrolled(top, bottom);
    }

    /// Scroll a region down by `n` lines in one bulk operation.
    ///
    /// The bottom `n` lines of the region are discarded (never retained as
    /// scrollback — scrollback is above the screen, not below it) and `n`
    /// blank lines appear at the top of the region. See [`Grid::scroll_up_n`]
    /// for the clamping and accounting contract.
    pub fn scroll_down_n(&mut self, region_top: usize, region_bottom: usize, n: usize) {
        let Some((top, bottom)) = self.clamp_region(region_top, region_bottom) else {
            return;
        };
        let height = bottom - top + 1;
        let n = n.min(height);
        if n == 0 {
            return;
        }
        self.mutations = self.mutations.saturating_add(n as u64);

        let sb = self.scrollback_len();
        let (start, end) = (sb + top, sb + bottom + 1);
        // Rotate right by n == rotate left by height - n.
        self.rotate_range_left(start, end, height - n);
        for idx in start..(start + n) {
            self.blank_row_at(idx);
        }
        self.mark_scrolled(top, bottom);
    }

    /// Scroll the visible area up by one line within a scroll region.
    /// The top line of the region moves into scrollback (if region starts at line 0).
    /// A new empty line appears at the bottom of the region.
    pub fn scroll_up(&mut self, region_top: usize, region_bottom: usize) {
        self.scroll_up_n(region_top, region_bottom, 1);
    }

    /// Scroll the visible area down by one line within a scroll region.
    /// A new empty line appears at the top of the region.
    /// The bottom line of the region is discarded.
    pub fn scroll_down(&mut self, region_top: usize, region_bottom: usize) {
        self.scroll_down_n(region_top, region_bottom, 1);
    }

    /// Clear all visible rows (reset to empty with given background).
    pub fn clear_visible(&mut self, bg: Color) {
        self.bump_mutations();
        let sb = self.scrollback_len();
        for i in sb..self.raw.len() {
            self.raw[i].reset(bg);
        }
        self.dirty.mark_rows(0, self.rows, self.rows, self.cols);
    }

    /// Fill the whole visible viewport with the DECALN alignment pattern
    /// (`ESC # 8`, issue #117). History is not part of the page and is left
    /// alone.
    ///
    /// Mirrors [`Grid::clear_visible`] exactly — same row range, same single
    /// tally bump, same full-viewport dirty mark. The tally bump is not
    /// bookkeeping: [`Grid::is_blank_canvas`] reads it to decide whether a
    /// retired alternate-screen buffer can be handed to the next application
    /// as-is, so a fill that did not advance it would leak a screen of `E`
    /// across that boundary.
    pub fn fill_alignment_pattern(&mut self) {
        self.bump_mutations();
        let sb = self.scrollback_len();
        for i in sb..self.raw.len() {
            self.raw[i].fill_alignment_pattern();
        }
        self.dirty.mark_rows(0, self.rows, self.rows, self.cols);
    }

    /// Clear rows from `start_row` to the end of the visible area.
    pub fn clear_below(&mut self, start_row: usize, bg: Color) {
        self.bump_mutations();
        let sb = self.scrollback_len();
        for i in (sb + start_row)..self.raw.len() {
            self.raw[i].reset(bg);
        }
        self.dirty
            .mark_rows(start_row, self.rows, self.rows, self.cols);
    }

    /// Clear rows from the top of the visible area to `end_row` (inclusive).
    pub fn clear_above(&mut self, end_row: usize, bg: Color) {
        self.bump_mutations();
        let sb = self.scrollback_len();
        for i in sb..=(sb + end_row) {
            self.raw[i].reset(bg);
        }
        self.dirty
            .mark_rows(0, end_row.saturating_add(1), self.rows, self.cols);
    }

    /// Resize the grid. Handles both growing and shrinking.
    ///
    /// On shrink (fewer rows): excess visible lines are kept (the caller
    /// adjusts the cursor; we simply reduce the visible window).
    /// On grow (more rows): lines are pulled back from scrollback if available,
    /// otherwise new blank lines are appended.
    /// Column resize: soft-wrapped logical lines are reflowed.
    pub fn resize(&mut self, new_rows: usize, new_cols: usize) {
        self.resize_with_cursor(new_rows, new_cols, None);
    }

    /// Resize the grid and remap an optional visible cursor position through
    /// column reflow. Returns the new visible cursor position when one was
    /// supplied.
    pub fn resize_with_cursor(
        &mut self,
        new_rows: usize,
        new_cols: usize,
        cursor: Option<(usize, usize)>,
    ) -> Option<(usize, usize)> {
        if new_cols != self.cols && new_cols > 0 && new_rows > 0 {
            return self.resize_reflowing_columns(new_rows, new_cols, cursor);
        }

        let old_abs_cursor = cursor.map(|(row, col)| (self.scrollback_len() + row, col));
        self.resize_canvas(new_rows, new_cols);
        old_abs_cursor.map(|(abs_row, col)| self.visible_cursor_from_abs(abs_row, col))
    }

    /// Resize without column reflow. This is used for fixed-canvas alternate
    /// screen buffers where fullscreen apps redraw after SIGWINCH.
    pub fn resize_canvas(&mut self, new_rows: usize, new_cols: usize) {
        let resized = new_rows != self.rows || new_cols != self.cols;
        if new_cols != self.cols {
            for row in self.raw.iter_mut() {
                row.resize(new_cols, Cell::default());
                row.sanitize_wide_pairs(Color::Default);
            }
            self.cols = new_cols;
        }

        // Handle row resize.
        if new_rows < self.rows {
            // Shrinking: remove excess visible rows from the bottom.
            // Blank rows at the bottom are discarded; non-blank rows are kept
            // as scrollback (they remain in the VecDeque and scrollback_len grows).
            let excess = self.rows - new_rows;
            let mut removed = 0;
            while removed < excess {
                // Remove blank rows from the bottom of the visible area.
                if let Some(back) = self.raw.back()
                    && back.is_blank()
                {
                    self.raw.pop_back();
                }
                removed += 1;
            }
            // Ensure we still have at least new_rows lines.
            while self.raw.len() < new_rows {
                self.raw.push_back(Row::new(self.cols));
            }
        } else if new_rows > self.rows {
            // Growing: pull lines from scrollback or add empty lines.
            let lines_needed = new_rows - self.rows;
            let from_scrollback = lines_needed.min(self.scrollback_len());
            // Lines from scrollback are already in the VecDeque -- we just
            // expand the "visible" window by adjusting self.rows.
            let new_lines = lines_needed - from_scrollback;
            for _ in 0..new_lines {
                self.raw.push_back(Row::new(self.cols));
            }
        }

        self.rows = new_rows;
        self.dirty.resize_rows(new_rows);
        if resized {
            self.dirty.mark_all();
        }
    }

    fn resize_reflowing_columns(
        &mut self,
        new_rows: usize,
        new_cols: usize,
        cursor: Option<(usize, usize)>,
    ) -> Option<(usize, usize)> {
        let old_scrollback_len = self.scrollback_len();
        let cursor_abs = cursor.map(|(row, col)| (old_scrollback_len + row, col));
        let mut cursor_anchor = None;
        let mut logical_lines = Vec::new();
        let mut current = LogicalLine::default();

        for (abs_row, row) in self.raw.iter().enumerate() {
            let is_tail = !row.wrapped;
            let row_cells = if is_tail {
                trim_default_trailing_cells(&row.cells)
            } else {
                row.cells.as_ref().clone()
            };

            if let Some((cursor_row, cursor_col)) = cursor_abs
                && cursor_row == abs_row
            {
                let row_offset = display_width_until(&row.cells, cursor_col);
                cursor_anchor = Some(CursorAnchor {
                    logical_line: logical_lines.len(),
                    display_offset: current.display_width + row_offset,
                });
            }

            current.display_width += display_width(&row_cells);
            current.cells.extend(row_cells);

            if is_tail {
                logical_lines.push(std::mem::take(&mut current));
            }
        }
        if !current.cells.is_empty() {
            logical_lines.push(current);
        }
        if logical_lines.is_empty() {
            logical_lines.push(LogicalLine::default());
        }

        if let Some(anchor) = &mut cursor_anchor
            && let Some(line) = logical_lines.get(anchor.logical_line)
        {
            anchor.display_offset = anchor.display_offset.min(line.display_width);
        }

        let mut reflowed = VecDeque::new();
        let mut line_ranges = Vec::with_capacity(logical_lines.len());
        for line in logical_lines {
            line_ranges.push(append_reflowed_line(
                &mut reflowed,
                line.cells,
                line.display_width,
                new_cols,
            ));
        }

        while reflowed.len() < new_rows {
            reflowed.push_back(Row::new(new_cols));
        }
        let min_total_to_keep = old_scrollback_len.min(self.config.max_scrollback) + new_rows;
        while reflowed.len() > min_total_to_keep && reflowed.back().is_some_and(Row::is_blank) {
            reflowed.pop_back();
        }
        while reflowed.len() < new_rows {
            reflowed.push_back(Row::new(new_cols));
        }
        let max_total = new_rows + self.config.max_scrollback;
        let mut dropped_rows = 0;
        while reflowed.len() > max_total {
            reflowed.pop_front();
            dropped_rows += 1;
        }

        self.evicted = self.evicted.saturating_add(dropped_rows as u64);
        self.raw = reflowed;
        self.rows = new_rows;
        self.cols = new_cols;
        self.dirty.resize_rows(new_rows);
        self.dirty.mark_all();

        cursor_anchor.map(|anchor| {
            let (abs_row, col) = self.abs_cursor_for_anchor(&line_ranges, anchor, dropped_rows);
            self.visible_cursor_from_abs(abs_row, col)
        })
    }

    fn visible_cursor_from_abs(&self, abs_row: usize, col: usize) -> (usize, usize) {
        let sb = self.scrollback_len();
        let visible_row = abs_row.saturating_sub(sb).min(self.rows.saturating_sub(1));
        let visible_col = col.min(self.cols.saturating_sub(1));
        (visible_row, visible_col)
    }

    fn abs_cursor_for_anchor(
        &self,
        line_ranges: &[ReflowedLineMap],
        anchor: CursorAnchor,
        dropped_rows: usize,
    ) -> (usize, usize) {
        let Some(line) = line_ranges.get(anchor.logical_line) else {
            return (self.scrollback_len(), 0);
        };
        let (abs_row, col) = line.position_for_offset(anchor.display_offset);
        (abs_row.saturating_sub(dropped_rows), col)
    }

    /// Clear the scrollback buffer entirely.
    pub fn clear_scrollback(&mut self) {
        let sb = self.scrollback_len();
        for _ in 0..sb {
            self.raw.pop_front();
        }
        self.evicted = self.evicted.saturating_add(sb as u64);
    }
}

impl ReflowedLineMap {
    fn position_for_offset(&self, offset: usize) -> (usize, usize) {
        let offset = offset.min(self.display_width);
        if offset == self.display_width {
            return (self.end_row, self.end_col);
        }

        for cell in &self.cells {
            if offset >= cell.offset && offset < cell.offset + cell.width {
                return (cell.row, cell.col + (offset - cell.offset));
            }
        }

        (self.range.start, 0)
    }
}

fn trim_default_trailing_cells(cells: &[Cell]) -> Vec<Cell> {
    let end = cells
        .iter()
        .rposition(|cell| cell.ch != ' ' || cell.is_wide_continuation())
        .map(|idx| idx + 1)
        .unwrap_or(0);
    cells[..end].to_vec()
}

fn display_width_until(cells: &[Cell], col: usize) -> usize {
    cells
        .iter()
        .take(col.min(cells.len()))
        .map(|cell| usize::from(cell.width))
        .sum()
}

fn display_width(cells: &[Cell]) -> usize {
    cells.iter().map(|cell| usize::from(cell.width)).sum()
}

fn append_reflowed_line(
    rows: &mut VecDeque<Row>,
    cells: Vec<Cell>,
    display_width: usize,
    cols: usize,
) -> ReflowedLineMap {
    debug_assert!(cols > 0);

    let start = rows.len();
    let mut row = Row::new(cols);
    let mut col = 0;
    let mut logical_offset = 0;
    let mut positions = Vec::new();

    for cell in cells
        .into_iter()
        .filter(|cell| !cell.is_wide_continuation())
    {
        let width = usize::from(cell.width).max(1);
        if col >= cols {
            row.wrapped = true;
            rows.push_back(row);
            row = Row::new(cols);
            col = 0;
        }
        if width > cols {
            let mut blank = cell;
            blank.reset(blank.style.bg);
            row[col] = blank;
            positions.push(ReflowedCellPosition {
                offset: logical_offset,
                row: rows.len(),
                col,
                width: 1,
            });
            logical_offset += width;
            col += 1;
            continue;
        }
        if col + width > cols && col > 0 {
            row.wrapped = true;
            rows.push_back(row);
            row = Row::new(cols);
            col = 0;
        }

        let abs_row = rows.len();
        positions.push(ReflowedCellPosition {
            offset: logical_offset,
            row: abs_row,
            col,
            width,
        });

        row[col] = cell;
        if width == 2 && col + 1 < cols {
            row[col + 1] = Cell::wide_continuation();
        }
        logical_offset += width;
        col += width.min(cols);
    }

    let end_row = rows.len();
    let end_col = col.min(cols.saturating_sub(1));
    rows.push_back(row);
    let end = rows.len();

    ReflowedLineMap {
        range: start..end,
        cells: positions,
        end_row,
        end_col,
        display_width,
    }
}

impl Grid {
    /// Erase `count` characters starting at `(row, col)` in the visible area.
    pub fn erase_chars(&mut self, row: usize, col: usize, count: usize, bg: Color) {
        self.bump_mutations();
        let dirty = {
            let r = self.visible_row_mut_untracked(row);
            r.erase_chars_expanding_wide_pairs(col, count, bg)
        };
        if let Some(range) = dirty {
            self.dirty.mark_range(row, range, self.rows, self.cols);
        }
    }

    /// Insert `count` blank cells at `(row, col)`, shifting existing cells right.
    /// Cells that shift past the right edge are lost.
    pub fn insert_chars(&mut self, row: usize, col: usize, count: usize) {
        self.bump_mutations();
        let r = self.visible_row_mut_untracked(row);
        let len = r.len();
        if col < len {
            r.clear_wide_pair_around(col, Color::Default);
        }
        // Shift right from the end.
        let cells = r.cells_mut();
        for i in (col..len).rev() {
            let target = i + count;
            if target < len {
                cells[target] = cells[i].clone();
            }
        }
        // Fill inserted positions with blanks.
        for cell in cells.iter_mut().take((col + count).min(len)).skip(col) {
            *cell = Cell::default();
        }
        r.sanitize_wide_pairs(Color::Default);
        if count > 0 && col < len {
            self.dirty
                .mark_range(row, col.saturating_sub(1)..len, self.rows, self.cols);
        }
    }

    /// Delete `count` cells at `(row, col)`, shifting remaining cells left.
    /// New cells at the right edge are blank.
    pub fn delete_chars(&mut self, row: usize, col: usize, count: usize) {
        self.bump_mutations();
        let r = self.visible_row_mut_untracked(row);
        let len = r.len();
        let actual = count.min(len.saturating_sub(col));
        if actual == 0 {
            return;
        }
        // Shift left.
        let cells = r.cells_mut();
        for i in col..(len - actual) {
            cells[i] = cells[i + actual].clone();
        }
        // Fill right edge with blanks.
        for cell in cells.iter_mut().skip(len - actual) {
            *cell = Cell::default();
        }
        r.sanitize_wide_pairs(Color::Default);
        self.dirty
            .mark_range(row, col.saturating_sub(1)..len, self.rows, self.cols);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::cell::{CellFlags, CellStyle, ExtendedAttrs, UnderlineStyle};

    fn write_text(mut row: impl std::ops::DerefMut<Target = Row>, text: &str) {
        for (idx, ch) in text.chars().enumerate() {
            row[idx].ch = ch;
        }
    }

    fn row_text(row: &Row) -> String {
        row.cells
            .iter()
            .filter(|cell| !cell.is_wide_continuation())
            .map(|cell| cell.ch)
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    fn put_wide(mut row: impl std::ops::DerefMut<Target = Row>, col: usize, ch: char) {
        row[col] = Cell {
            ch,
            width: 2,
            style: CellStyle::default(),
            extended: None,
        };
        row[col + 1] = Cell::wide_continuation();
    }

    fn assert_row_wide_invariants(row: &Row) {
        for col in 0..row.len() {
            let cell = &row[col];
            if cell.is_wide_continuation() {
                assert_eq!(
                    cell.ch, ' ',
                    "continuation at col {col} must not carry a glyph"
                );
                assert!(col > 0, "orphan continuation at col 0");
                assert!(row[col - 1].is_wide(), "orphan continuation at col {col}");
            }
            if cell.is_wide() {
                assert!(
                    col + 1 < row.len(),
                    "wide head at final col {col} is missing a tail"
                );
                assert!(
                    row[col + 1].is_wide_continuation(),
                    "wide head at col {col} is missing a tail"
                );
                assert_eq!(row[col + 1].ch, ' ', "wide tail at col {}", col + 1);
            }
        }
    }

    fn assert_grid_wide_invariants(grid: &Grid) {
        for row_idx in 0..grid.total_lines() {
            let row = grid.row(row_idx).expect("row exists");
            assert_row_wide_invariants(row);
        }
    }

    #[test]
    fn test_new_grid_dimensions() {
        let grid = Grid::new(24, 80, GridConfig::default());
        assert_eq!(grid.rows(), 24);
        assert_eq!(grid.cols(), 80);
        assert_eq!(grid.scrollback_len(), 0);
    }

    #[test]
    fn test_visible_row_access() {
        let mut grid = Grid::new(3, 10, GridConfig::default());
        grid.visible_row_mut(0)[0].ch = 'A';
        grid.visible_row_mut(1)[0].ch = 'B';
        grid.visible_row_mut(2)[0].ch = 'C';
        assert_eq!(grid.visible_row(0)[0].ch, 'A');
        assert_eq!(grid.visible_row(1)[0].ch, 'B');
        assert_eq!(grid.visible_row(2)[0].ch, 'C');
    }

    #[test]
    fn test_scroll_up_adds_scrollback() {
        let mut grid = Grid::new(3, 10, GridConfig::default());
        grid.visible_row_mut(0)[0].ch = 'A';
        grid.scroll_up(0, 2);
        // Row with 'A' should now be in scrollback.
        assert_eq!(grid.scrollback_len(), 1);
        assert_eq!(grid.scrollback_row(0).unwrap()[0].ch, 'A');
        // New bottom row should be empty.
        assert_eq!(grid.visible_row(2)[0].ch, ' ');
    }

    #[test]
    fn test_scrollback_limit() {
        let config = GridConfig {
            max_scrollback: 2,
            ..GridConfig::default()
        };
        let mut grid = Grid::new(3, 10, config);
        for i in 0..5u8 {
            grid.visible_row_mut(0)[0].ch = char::from(b'A' + i);
            grid.scroll_up(0, 2);
        }
        // Only 2 lines should be in scrollback.
        assert_eq!(grid.scrollback_len(), 2);
    }

    #[test]
    fn test_scroll_region() {
        let mut grid = Grid::new(5, 10, GridConfig::default());
        grid.visible_row_mut(1)[0].ch = 'X';
        grid.visible_row_mut(3)[0].ch = 'Y';
        // Scroll region 1..3 up: row 1 disappears, row 3 shifts to row 2.
        grid.scroll_up(1, 3);
        assert_eq!(grid.visible_row(2)[0].ch, 'Y');
        assert_eq!(grid.visible_row(3)[0].ch, ' '); // New empty row.
        assert_eq!(grid.scrollback_len(), 0); // No scrollback for region scroll.
    }

    #[test]
    fn test_scroll_down() {
        let mut grid = Grid::new(5, 10, GridConfig::default());
        grid.visible_row_mut(1)[0].ch = 'A';
        grid.visible_row_mut(2)[0].ch = 'B';
        grid.visible_row_mut(3)[0].ch = 'C';
        // Scroll down in region 1..3: insert blank at top, bottom row gone.
        grid.scroll_down(1, 3);
        assert_eq!(grid.visible_row(1)[0].ch, ' '); // New blank row.
        assert_eq!(grid.visible_row(2)[0].ch, 'A'); // Shifted down.
        assert_eq!(grid.visible_row(3)[0].ch, 'B'); // Shifted down.
        // 'C' was at row 3 (bottom of region) and is gone.
    }

    #[test]
    fn test_resize_grow() {
        let mut grid = Grid::new(3, 10, GridConfig::default());
        grid.resize(5, 15);
        assert_eq!(grid.rows(), 5);
        assert_eq!(grid.cols(), 15);
    }

    #[test]
    fn test_resize_shrink_columns() {
        let mut grid = Grid::new(3, 10, GridConfig::default());
        grid.visible_row_mut(0)[9].ch = 'Z';
        grid.resize(3, 5);
        assert_eq!(grid.cols(), 5);
        assert_eq!(grid.visible_row(0).len(), 5);
    }

    #[test]
    fn test_resize_grow_reclaims_scrollback() {
        let mut grid = Grid::new(3, 10, GridConfig::default());
        // Generate some scrollback.
        grid.visible_row_mut(0)[0].ch = 'S';
        grid.scroll_up(0, 2);
        assert_eq!(grid.scrollback_len(), 1);
        // Growing should reclaim scrollback lines.
        grid.resize(4, 10);
        assert_eq!(grid.rows(), 4);
        assert_eq!(grid.scrollback_len(), 0);
        // The scrollback line with 'S' is now a visible line.
        assert_eq!(grid.visible_row(0)[0].ch, 'S');
    }

    #[test]
    fn resize_reflows_source_row_wrapped_runs_on_shrink_and_grow() {
        let mut grid = Grid::new(4, 5, GridConfig::default());
        write_text(grid.visible_row_mut(0), "HELLO");
        grid.visible_row_mut(0).wrapped = true;
        write_text(grid.visible_row_mut(1), "WORLD");

        grid.resize(5, 4);

        assert_eq!(row_text(grid.visible_row(0)), "HELL");
        assert!(grid.visible_row(0).wrapped);
        assert_eq!(row_text(grid.visible_row(1)), "OWOR");
        assert!(grid.visible_row(1).wrapped);
        assert_eq!(row_text(grid.visible_row(2)), "LD");
        assert!(!grid.visible_row(2).wrapped);

        grid.resize(5, 8);

        assert_eq!(row_text(grid.visible_row(0)), "HELLOWOR");
        assert!(grid.visible_row(0).wrapped);
        assert_eq!(row_text(grid.visible_row(1)), "LD");
        assert!(!grid.visible_row(1).wrapped);
    }

    #[test]
    fn resize_keeps_hard_line_breaks_hard() {
        let mut grid = Grid::new(3, 5, GridConfig::default());
        write_text(grid.visible_row_mut(0), "AAA");
        write_text(grid.visible_row_mut(1), "BBB");

        grid.resize(3, 4);

        assert_eq!(row_text(grid.visible_row(0)), "AAA");
        assert!(!grid.visible_row(0).wrapped);
        assert_eq!(row_text(grid.visible_row(1)), "BBB");
        assert!(!grid.visible_row(1).wrapped);
    }

    #[test]
    fn resize_ignores_trailing_styled_blanks_on_hard_lines() {
        let mut grid = Grid::new(4, 8, GridConfig::default());
        write_text(grid.visible_row_mut(0), "AB");
        for col in 2..8 {
            grid.visible_row_mut(0)[col].reset(Color::Indexed(4));
        }
        write_text(grid.visible_row_mut(1), "CD");

        grid.resize(4, 3);

        assert_eq!(row_text(grid.visible_row(0)), "AB");
        assert!(!grid.visible_row(0).wrapped);
        assert_eq!(row_text(grid.visible_row(1)), "CD");
        assert!(!grid.visible_row(1).wrapped);
    }

    #[test]
    fn resize_preserves_cell_style_rgb_and_extended_attrs() {
        let mut grid = Grid::new(2, 3, GridConfig::default());
        write_text(grid.visible_row_mut(0), "ABC");
        grid.visible_row_mut(0).wrapped = true;

        let mut flags = CellFlags::default();
        flags.set(CellFlags::BOLD);
        flags.set(CellFlags::UNDERLINE);
        let ext = Arc::new(ExtendedAttrs {
            grapheme: None,
            hyperlink: Some("https://example.com".to_string()),
            underline_color: Some(Color::Rgb(9, 8, 7)),
            underline_style: UnderlineStyle::Curly,
        });
        grid.visible_row_mut(1)[0] = Cell {
            ch: 'D',
            width: 1,
            style: CellStyle {
                fg: Color::Rgb(1, 2, 3),
                bg: Color::Indexed(4),
                flags,
            },
            extended: Some(ext.clone()),
        };

        grid.resize(4, 2);

        let moved = &grid.visible_row(1)[1];
        assert_eq!(moved.ch, 'D');
        assert_eq!(moved.style.fg, Color::Rgb(1, 2, 3));
        assert_eq!(moved.style.bg, Color::Indexed(4));
        assert!(moved.style.flags.contains(CellFlags::BOLD));
        assert!(moved.style.flags.contains(CellFlags::UNDERLINE));
        assert_eq!(moved.extended.as_ref(), Some(&ext));
    }

    #[test]
    fn resize_keeps_wide_cells_atomic() {
        let mut grid = Grid::new(2, 3, GridConfig::default());
        grid.visible_row_mut(0)[0].ch = 'A';
        grid.visible_row_mut(0)[1] = Cell {
            ch: '\u{4f60}',
            width: 2,
            style: CellStyle::default(),
            extended: None,
        };
        grid.visible_row_mut(0)[2] = Cell::wide_continuation();
        grid.visible_row_mut(0).wrapped = true;
        grid.visible_row_mut(1)[0].ch = 'B';

        grid.resize(4, 2);

        assert_eq!(grid.visible_row(0)[0].ch, 'A');
        assert_eq!(grid.visible_row(0)[1], Cell::default());
        assert_eq!(grid.visible_row(1)[0].ch, '\u{4f60}');
        assert!(grid.visible_row(1)[0].is_wide());
        assert!(grid.visible_row(1)[1].is_wide_continuation());
        assert_eq!(grid.visible_row(2)[0].ch, 'B');

        assert_grid_wide_invariants(&grid);
    }

    #[test]
    fn erase_from_continuation_clears_entire_wide_pair() {
        let mut grid = Grid::new(1, 5, GridConfig::default());
        put_wide(grid.visible_row_mut(0), 1, '界');
        grid.visible_row_mut(0)[3].ch = 'A';

        grid.erase_chars(0, 2, 1, Color::Default);

        assert_eq!(grid.visible_row(0)[1].ch, ' ');
        assert_eq!(grid.visible_row(0)[2].ch, ' ');
        assert_eq!(grid.visible_row(0)[3].ch, 'A');
        assert_grid_wide_invariants(&grid);
    }

    #[test]
    fn delete_from_continuation_shifts_by_exactly_one_then_sanitizes() {
        let mut grid = Grid::new(1, 5, GridConfig::default());
        put_wide(grid.visible_row_mut(0), 0, '界');
        grid.visible_row_mut(0)[2].ch = 'A';
        grid.visible_row_mut(0)[3].ch = 'B';

        grid.delete_chars(0, 1, 1);

        assert_eq!(grid.visible_row(0)[0].ch, ' ');
        assert_eq!(grid.visible_row(0)[1].ch, 'A');
        assert_eq!(grid.visible_row(0)[2].ch, 'B');
        assert_grid_wide_invariants(&grid);
    }

    #[test]
    fn insert_sanitizes_wide_pair_pushed_off_right_edge() {
        let mut grid = Grid::new(1, 4, GridConfig::default());
        grid.visible_row_mut(0)[0].ch = 'A';
        put_wide(grid.visible_row_mut(0), 1, '界');

        grid.insert_chars(0, 1, 1);

        assert_eq!(grid.visible_row(0)[0].ch, 'A');
        assert_eq!(grid.visible_row(0)[1].ch, ' ');
        assert_grid_wide_invariants(&grid);
    }

    #[test]
    fn resize_canvas_sanitizes_truncated_wide_head() {
        let mut grid = Grid::new(1, 4, GridConfig::default());
        put_wide(grid.visible_row_mut(0), 2, '界');

        grid.resize_canvas(1, 3);

        assert_eq!(grid.visible_row(0)[2].ch, ' ');
        assert_grid_wide_invariants(&grid);
    }

    #[test]
    fn resize_reflow_preserves_scrollback_order_and_limit() {
        let config = GridConfig {
            max_scrollback: 2,
            ..GridConfig::default()
        };
        let mut grid = Grid::new(2, 5, config);

        for ch in ['A', 'B', 'C', 'D'] {
            grid.visible_row_mut(0)[0].ch = ch;
            grid.scroll_up(0, 1);
        }

        grid.resize(2, 3);

        assert_eq!(grid.scrollback_len(), 2);
        assert_eq!(grid.scrollback_row(0).unwrap()[0].ch, 'C');
        assert_eq!(grid.scrollback_row(1).unwrap()[0].ch, 'D');
    }

    #[test]
    fn test_clear_visible() {
        let mut grid = Grid::new(3, 10, GridConfig::default());
        grid.visible_row_mut(0)[0].ch = 'A';
        grid.visible_row_mut(1)[0].ch = 'B';
        grid.clear_visible(Color::Default);
        assert_eq!(grid.visible_row(0)[0].ch, ' ');
        assert_eq!(grid.visible_row(1)[0].ch, ' ');
    }

    #[test]
    fn test_clear_below() {
        let mut grid = Grid::new(3, 10, GridConfig::default());
        grid.visible_row_mut(0)[0].ch = 'A';
        grid.visible_row_mut(1)[0].ch = 'B';
        grid.visible_row_mut(2)[0].ch = 'C';
        grid.clear_below(1, Color::Default);
        assert_eq!(grid.visible_row(0)[0].ch, 'A');
        assert_eq!(grid.visible_row(1)[0].ch, ' ');
        assert_eq!(grid.visible_row(2)[0].ch, ' ');
    }

    #[test]
    fn test_clear_above() {
        let mut grid = Grid::new(3, 10, GridConfig::default());
        grid.visible_row_mut(0)[0].ch = 'A';
        grid.visible_row_mut(1)[0].ch = 'B';
        grid.visible_row_mut(2)[0].ch = 'C';
        grid.clear_above(1, Color::Default);
        assert_eq!(grid.visible_row(0)[0].ch, ' ');
        assert_eq!(grid.visible_row(1)[0].ch, ' ');
        assert_eq!(grid.visible_row(2)[0].ch, 'C');
    }

    #[test]
    fn test_erase_chars() {
        let mut grid = Grid::new(1, 10, GridConfig::default());
        for i in 0..10 {
            grid.visible_row_mut(0)[i].ch = char::from(b'A' + i as u8);
        }
        grid.erase_chars(0, 2, 3, Color::Default);
        assert_eq!(grid.visible_row(0)[0].ch, 'A');
        assert_eq!(grid.visible_row(0)[1].ch, 'B');
        assert_eq!(grid.visible_row(0)[2].ch, ' ');
        assert_eq!(grid.visible_row(0)[3].ch, ' ');
        assert_eq!(grid.visible_row(0)[4].ch, ' ');
        assert_eq!(grid.visible_row(0)[5].ch, 'F');
    }

    #[test]
    fn test_insert_delete_chars() {
        let mut grid = Grid::new(1, 5, GridConfig::default());
        grid.visible_row_mut(0)[0].ch = 'A';
        grid.visible_row_mut(0)[1].ch = 'B';
        grid.visible_row_mut(0)[2].ch = 'C';
        grid.insert_chars(0, 1, 1);
        assert_eq!(grid.visible_row(0)[0].ch, 'A');
        assert_eq!(grid.visible_row(0)[1].ch, ' '); // inserted
        assert_eq!(grid.visible_row(0)[2].ch, 'B'); // shifted
        assert_eq!(grid.visible_row(0)[3].ch, 'C'); // shifted

        grid.delete_chars(0, 1, 1);
        assert_eq!(grid.visible_row(0)[1].ch, 'B'); // shifted back
    }

    #[test]
    fn test_clear_scrollback() {
        let mut grid = Grid::new(3, 10, GridConfig::default());
        for _ in 0..5 {
            grid.scroll_up(0, 2);
        }
        assert!(grid.scrollback_len() > 0);
        grid.clear_scrollback();
        assert_eq!(grid.scrollback_len(), 0);
    }

    #[test]
    fn test_row_is_blank() {
        let row = Row::new(10);
        assert!(row.is_blank());
        let mut row2 = Row::new(10);
        row2[0].ch = 'X';
        assert!(!row2.is_blank());
    }

    #[test]
    fn test_total_lines() {
        let mut grid = Grid::new(3, 10, GridConfig::default());
        assert_eq!(grid.total_lines(), 3);
        grid.scroll_up(0, 2);
        assert_eq!(grid.total_lines(), 4);
    }

    #[test]
    fn test_clone_visible_drops_scrollback() {
        // Push scrollback in, then confirm clone_visible() keeps the
        // visible rows but discards the scrollback.
        let mut grid = Grid::new(3, 4, GridConfig::default());
        // Push scrollback via full-screen scroll-ups (the (0, rows-1)
        // shape is what hits the scrollback branch). Five iterations →
        // five scrollback rows on top of the three visible rows.
        for _ in 0..5 {
            grid.scroll_up(0, grid.rows() - 1);
        }
        assert!(grid.scrollback_len() >= 5, "scrollback was set up");
        // Mark a visible row so we can verify it survives the clone.
        grid.visible_row_mut(2)[0].ch = 'V';

        let snap = grid.clone_visible();
        assert_eq!(snap.rows(), 3);
        assert_eq!(snap.cols(), 4);
        assert_eq!(
            snap.scrollback_len(),
            0,
            "snapshot must not copy scrollback"
        );
        assert_eq!(snap.total_lines(), 3);
        // Visible content is preserved across the clone.
        assert_eq!(snap.visible_row(2).cells[0].ch, 'V');
    }

    #[test]
    fn dirty_direct_row_mutation_marks_the_row_and_take_clears() {
        let mut grid = Grid::new(2, 5, GridConfig::default());
        assert!(!grid.is_dirty());

        grid.visible_row_mut(0)[2].ch = 'X';
        assert!(grid.is_dirty());
        assert_eq!(
            grid.take_dirty_regions(),
            vec![DirtyRegion { row: 0, cols: 0..5 }]
        );
        assert!(!grid.is_dirty());
        assert!(grid.take_dirty_regions().is_empty());
    }

    #[test]
    fn dirty_erase_insert_delete_report_helper_ranges() {
        let mut grid = Grid::new(1, 6, GridConfig::default());
        write_text(grid.visible_row_mut(0), "ABCDEF");
        grid.take_dirty_regions();

        grid.erase_chars(0, 2, 2, Color::Default);
        assert_eq!(
            grid.take_dirty_regions(),
            vec![DirtyRegion { row: 0, cols: 2..4 }]
        );

        grid.insert_chars(0, 1, 2);
        assert_eq!(
            grid.take_dirty_regions(),
            vec![DirtyRegion { row: 0, cols: 0..6 }]
        );

        grid.delete_chars(0, 3, 1);
        assert_eq!(
            grid.take_dirty_regions(),
            vec![DirtyRegion { row: 0, cols: 2..6 }]
        );
    }

    #[test]
    fn dirty_insert_delete_include_repaired_wide_head_to_the_left() {
        let mut grid = Grid::new(1, 6, GridConfig::default());
        put_wide(grid.visible_row_mut(0), 1, '界');
        grid.take_dirty_regions();

        grid.insert_chars(0, 2, 1);
        assert_eq!(
            grid.take_dirty_regions(),
            vec![DirtyRegion { row: 0, cols: 1..6 }]
        );

        put_wide(grid.visible_row_mut(0), 1, '界');
        grid.take_dirty_regions();
        grid.delete_chars(0, 2, 1);
        assert_eq!(
            grid.take_dirty_regions(),
            vec![DirtyRegion { row: 0, cols: 1..6 }]
        );
    }

    #[test]
    fn dirty_scroll_and_resize_invalidate_visible_frame() {
        let mut grid = Grid::new(3, 4, GridConfig::default());
        grid.take_dirty_regions();

        grid.scroll_up(1, 2);
        assert_eq!(
            grid.take_dirty_regions(),
            vec![
                DirtyRegion { row: 1, cols: 0..4 },
                DirtyRegion { row: 2, cols: 0..4 },
            ]
        );

        grid.resize_canvas(4, 5);
        assert_eq!(
            grid.take_dirty_regions(),
            vec![
                DirtyRegion { row: 0, cols: 0..5 },
                DirtyRegion { row: 1, cols: 0..5 },
                DirtyRegion { row: 2, cols: 0..5 },
                DirtyRegion { row: 3, cols: 0..5 },
            ]
        );
    }

    // -----------------------------------------------------------------
    // Retired-buffer recycling primitives (issue #106)
    // -----------------------------------------------------------------

    /// Everything about a grid that a consumer can reach. `reset_blank` claims
    /// to produce a grid indistinguishable from a fresh one; this is what
    /// "indistinguishable" is checked against.
    #[derive(Debug, PartialEq, Eq)]
    struct GridSnapshot {
        rows: usize,
        cols: usize,
        total_lines: usize,
        mutations: u64,
        evicted: u64,
        dirty: bool,
        cells: Vec<(char, bool)>,
        config: GridConfig,
    }

    fn snapshot(grid: &Grid) -> GridSnapshot {
        GridSnapshot {
            rows: grid.rows(),
            cols: grid.cols(),
            total_lines: grid.total_lines(),
            mutations: grid.mutations(),
            evicted: grid.evicted(),
            dirty: grid.is_dirty(),
            cells: (0..grid.total_lines())
                .flat_map(|r| {
                    let row = grid.row(r).expect("row in range");
                    (0..row.len())
                        .map(|c| (row[c].ch, row.wrapped))
                        .collect::<Vec<_>>()
                })
                .collect(),
            config: grid.config.clone(),
        }
    }

    /// A grid that keeps only the viewport has dropped its scrollback off the
    /// front, so its absolute base has moved by exactly that many lines.
    #[test]
    fn a_viewport_clone_advances_the_eviction_count_by_the_history_it_drops() {
        let cfg = GridConfig {
            max_scrollback: 2,
            ..GridConfig::default()
        };
        let mut grid = Grid::new(4, 8, cfg);
        for _ in 0..10 {
            grid.scroll_up(0, 3);
        }
        assert_eq!(grid.scrollback_len(), 2, "history is capped, not empty");
        assert!(grid.evicted() > 0, "lines really fell off the front");

        // The line the clone's first row IS, counted in the original.
        let first_visible = grid.evicted() + grid.scrollback_len() as u64;

        assert_eq!(
            grid.clone_visible().evicted(),
            first_visible,
            "clone_visible kept the base of history it did not keep"
        );
        assert_eq!(
            grid.clone_presented_viewport().evicted(),
            first_visible,
            "clone_presented_viewport kept the base of history it did not keep"
        );
        assert_eq!(
            grid.clone().evicted(),
            grid.evicted(),
            "a full clone keeps history, so it keeps the base"
        );
    }

    /// `is_blank_canvas` licenses reusing a retired buffer WITHOUT blanking
    /// it, on the promise that it is "indistinguishable from a freshly built
    /// `Grid::new`". It does not compare the eviction base, so what makes that
    /// safe is a property, asserted here rather than argued: whatever the
    /// check admits counts from zero.
    #[test]
    fn a_grid_that_looks_blank_to_the_recycling_check_counts_from_zero() {
        let cfg = alt_config();

        let mut scrolled = Grid::new(4, 8, cfg.clone());
        for _ in 0..9 {
            scrolled.scroll_up(0, 3);
        }
        let mut reset = scrolled.clone();
        reset.reset_blank(4, 8, cfg.clone());

        let mut resized = Grid::new(8, 8, cfg.clone());
        resized.resize(2, 8);
        resized.resize(4, 8);

        // A grid whose scrollback arrived without a single scroll: the shrink
        // keeps non-blank rows, and `clear_scrollback` then moves the base.
        let mut restructured = Grid::new(4, 8, cfg.clone());
        for r in 0..4 {
            restructured.visible_row_mut(r)[0].ch = 'X';
        }
        restructured.resize_canvas(2, 8);
        restructured.clear_scrollback();
        assert!(restructured.evicted() > 0, "the base really moved");

        let candidates = [
            ("fresh", Grid::new(4, 8, cfg.clone())),
            ("scrolled", scrolled),
            ("reset", reset),
            ("resized", resized),
            ("restructured", restructured),
        ];

        // The reset grid is the one that discriminates: it is the state the
        // recycling branch actually hands back. A count alone stays green if
        // this specific candidate stops being admitted.
        assert!(
            candidates
                .iter()
                .any(|(name, g)| *name == "reset" && g.is_blank_canvas(4, 8, &cfg)),
            "the reset candidate is no longer admitted, so this test would \
             pass without exercising the recycling path at all"
        );

        for (name, grid) in &candidates {
            if grid.is_blank_canvas(4, 8, &cfg) {
                assert_eq!(
                    grid.evicted(),
                    0,
                    "{name}: a grid the recycling branch would reuse untouched \
                     is still counting from a discarded grid's base"
                );
            }
        }
    }

    /// What a caller actually sees. An absolute index taken in one
    /// alternate-screen session must not resolve onto a live row of the next:
    /// the recycled buffer discarded every line that index named.
    #[test]
    fn a_recycled_alternate_screen_buffer_does_not_alias_the_previous_session() {
        use crate::cursor::Cursor;

        let mut grid = Grid::new(4, 8, GridConfig::default());
        let mut cursor = Cursor::default();
        let (mut stashed_grid, mut stashed_cursor, mut spare) = (None, None, None);

        macro_rules! swap {
            ($m:ident) => {{
                crate::screen::ScreenSwap {
                    grid: &mut grid,
                    cursor: &mut cursor,
                    stashed_grid: &mut stashed_grid,
                    stashed_cursor: &mut stashed_cursor,
                    spare: &mut spare,
                    reuse: true,
                }
                .$m(true);
            }};
        }

        swap!(enter);
        for _ in 0..4 {
            grid.scroll_up(0, 3);
        }
        grid.visible_row_mut(1)[0].ch = 'X';
        let anchor = grid.evicted() + (grid.scrollback_len() + 1) as u64;
        swap!(leave);

        // Session two gets the recycled buffer. Nothing session one wrote survives.
        swap!(enter);
        grid.visible_row_mut(1)[0].ch = 'Y';

        let resolved = anchor
            .checked_sub(grid.evicted())
            .and_then(|local| grid.row(local as usize));
        assert!(
            resolved.is_none(),
            "an anchor from the previous alternate-screen session resolved to \
             a live row of this one: {:?}",
            resolved.map(row_text),
        );
    }

    fn alt_config() -> GridConfig {
        GridConfig {
            max_scrollback: 0,
            ..GridConfig::default()
        }
    }

    #[test]
    fn reset_blank_is_indistinguishable_from_a_fresh_grid() {
        let mut used = Grid::new(6, 10, alt_config());
        used.visible_row_mut(0)[0].ch = 'X';
        used.visible_row_mut(3).wrapped = true;
        used.scroll_up_n(0, 5, 4);
        used.take_dirty_regions();

        used.reset_blank(6, 10, alt_config());
        let fresh = Grid::new(6, 10, alt_config());
        assert_eq!(snapshot(&used), snapshot(&fresh));
    }

    #[test]
    fn reset_blank_retargets_geometry_in_both_directions() {
        for (from, to) in [((6usize, 10usize), (12usize, 30usize)), ((12, 30), (3, 4))] {
            let mut grid = Grid::new(from.0, from.1, alt_config());
            grid.visible_row_mut(0)[0].ch = 'X';
            grid.reset_blank(to.0, to.1, alt_config());
            assert_eq!(
                snapshot(&grid),
                snapshot(&Grid::new(to.0, to.1, alt_config())),
                "reset_blank {from:?} -> {to:?}"
            );
        }
    }

    /// A grid carrying scrollback must lose it: the alternate screen has none,
    /// and a recycled buffer that kept rows would report a history it does not
    /// have.
    #[test]
    fn reset_blank_discards_scrollback() {
        let mut grid = Grid::new(4, 8, GridConfig::default());
        for _ in 0..40 {
            grid.scroll_up(0, 3);
        }
        assert!(grid.scrollback_len() > 0);
        grid.reset_blank(4, 8, alt_config());
        assert_eq!(grid.scrollback_len(), 0);
        assert_eq!(grid.total_lines(), 4);
    }

    /// `is_blank_canvas` is the licence to skip re-blanking entirely, so it
    /// must say no to every grid that is not, in fact, a blank canvas —
    /// including one whose cells were written and then written back.
    #[test]
    fn is_blank_canvas_rejects_anything_that_was_touched() {
        let fresh = Grid::new(4, 8, alt_config());
        assert!(fresh.is_blank_canvas(4, 8, &alt_config()));

        // Wrong geometry.
        assert!(!fresh.is_blank_canvas(5, 8, &alt_config()));
        assert!(!fresh.is_blank_canvas(4, 9, &alt_config()));
        // Wrong scrollback budget: a primary buffer must never be mistaken
        // for a recyclable alternate one.
        assert!(!fresh.is_blank_canvas(4, 8, &GridConfig::default()));

        // Written, then restored to the original value. The write tally is
        // value-independent on purpose: this must still be rejected.
        let mut restored = Grid::new(4, 8, alt_config());
        let original = restored.visible_row(0)[0].ch;
        restored.visible_row_mut(0)[0].ch = 'X';
        restored.visible_row_mut(0)[0].ch = original;
        assert!(!restored.is_blank_canvas(4, 8, &alt_config()));

        // Scrolled but visually identical (blank rows shifting past blank rows).
        let mut scrolled = Grid::new(4, 8, alt_config());
        scrolled.scroll_up(0, 3);
        assert!(!scrolled.is_blank_canvas(4, 8, &alt_config()));

        // Cleared: `clear_visible` writes, so the tally moves.
        let mut cleared = Grid::new(4, 8, alt_config());
        cleared.clear_visible(Color::Default);
        assert!(!cleared.is_blank_canvas(4, 8, &alt_config()));
    }

    /// The one case the tally alone cannot see: restructuring operations do
    /// not advance it, so the geometry and line count carry that half of the
    /// proof.
    #[test]
    fn is_blank_canvas_rejects_restructured_grids() {
        let mut resized = Grid::new(4, 8, alt_config());
        resized.resize_canvas(6, 8);
        assert_eq!(resized.mutations(), 0, "resize is not a write, by design");
        assert!(
            !resized.is_blank_canvas(4, 8, &alt_config()),
            "a resized grid passed as a blank canvas at its OLD geometry"
        );

        let mut trimmed = Grid::new(4, 8, GridConfig::default());
        for _ in 0..10 {
            trimmed.scroll_up(0, 3);
        }
        trimmed.reset_blank(4, 8, alt_config());
        trimmed.clear_scrollback();
        assert!(trimmed.is_blank_canvas(4, 8, &alt_config()));
    }

    // ── DECALN alignment fill (issue #117) ──────────────────────────────

    #[test]
    fn fill_alignment_pattern_writes_every_visible_cell() {
        let mut grid = Grid::new(3, 5, GridConfig::default());
        write_text(grid.visible_row_mut(1), "abcde");
        grid.fill_alignment_pattern();

        for r in 0..3 {
            let row = grid.visible_row(r);
            assert!(!row.wrapped);
            for c in 0..5 {
                assert_eq!(row[c], Cell::ALIGNMENT, "cell ({r},{c})");
            }
        }
    }

    /// The fill is the page, not the pane's history.
    #[test]
    fn fill_alignment_pattern_leaves_scrollback_alone() {
        let mut grid = Grid::new(2, 6, GridConfig::default());
        write_text(grid.visible_row_mut(0), "keepme");
        grid.scroll_up(0, 1);
        assert_eq!(grid.scrollback_len(), 1);

        grid.fill_alignment_pattern();
        assert_eq!(row_text(grid.scrollback_row(0).expect("history")), "keepme");
    }

    /// A wide pair spans two cells; the fill replaces both with single-width
    /// `E`, so no continuation cell may be orphaned.
    #[test]
    fn fill_alignment_pattern_dissolves_wide_pairs() {
        let mut grid = Grid::new(1, 4, GridConfig::default());
        put_wide(grid.visible_row_mut(0), 0, '日');
        put_wide(grid.visible_row_mut(0), 2, '本');
        grid.fill_alignment_pattern();
        assert_row_wide_invariants(grid.visible_row(0));
        assert_eq!(row_text(grid.visible_row(0)), "EEEE");
    }

    /// Extended attributes are a heap payload hanging off a cell. The pattern
    /// must drop them rather than inherit them.
    #[test]
    fn fill_alignment_pattern_drops_extended_attributes() {
        let mut grid = Grid::new(1, 3, GridConfig::default());
        grid.visible_row_mut(0)[1].extended = Some(Arc::new(ExtendedAttrs {
            grapheme: Some("e\u{0301}".into()),
            hyperlink: Some("https://example.invalid".into()),
            underline_color: Some(Color::Indexed(9)),
            underline_style: UnderlineStyle::Curly,
        }));
        grid.fill_alignment_pattern();
        for c in 0..3 {
            assert!(grid.visible_row(0)[c].extended.is_none(), "cell {c}");
        }
    }

    /// The write tally is what licenses recycling a retired alternate-screen
    /// buffer as a blank canvas (issue #106). A fill that did not advance it
    /// would hand the next application a screen it never drew.
    #[test]
    fn fill_alignment_pattern_is_not_a_blank_canvas_afterwards() {
        let config = alt_config();
        let mut grid = Grid::new(4, 6, config.clone());
        assert!(grid.is_blank_canvas(4, 6, &config));

        let before = grid.mutations();
        grid.fill_alignment_pattern();
        assert!(grid.mutations() > before, "write tally did not advance");
        assert!(
            !grid.is_blank_canvas(4, 6, &config),
            "a screen full of `E` still reads as a blank canvas"
        );
        assert!(!grid.is_actually_blank(6));
    }

    /// A row still shared with a snapshot must be copied before the fill lands.
    #[test]
    fn fill_alignment_pattern_copies_shared_rows_first() {
        let mut grid = Grid::new(2, 4, GridConfig::default());
        write_text(grid.visible_row_mut(0), "held");
        let held = grid.clone();

        grid.fill_alignment_pattern();
        assert_eq!(row_text(held.visible_row(0)), "held");
        assert_eq!(row_text(grid.visible_row(0)), "EEEE");
    }

    #[test]
    fn fill_alignment_pattern_marks_the_viewport_dirty() {
        let mut grid = Grid::new(3, 5, GridConfig::default());
        grid.take_dirty_regions();
        assert!(!grid.is_dirty());
        grid.fill_alignment_pattern();
        let regions = grid.take_dirty_regions();
        for r in 0..3 {
            assert!(
                regions
                    .iter()
                    .any(|d| d.row == r && d.cols.start == 0 && d.cols.end >= 5),
                "row {r} not reported dirty: {regions:?}"
            );
        }
    }

    /// A grid with no cells has nothing to fill and must not panic doing it.
    #[test]
    fn fill_alignment_pattern_on_an_empty_grid_is_inert() {
        for (rows, cols) in [(0usize, 0usize), (0, 4), (4, 0)] {
            let mut grid = Grid::new(rows, cols, GridConfig::default());
            grid.fill_alignment_pattern();
            assert_eq!(grid.rows(), rows);
            assert_eq!(grid.cols(), cols);
        }
    }

    #[test]
    fn dirty_clone_and_clone_visible_start_clean() {
        let mut grid = Grid::new(2, 4, GridConfig::default());
        grid.visible_row_mut(0)[0].ch = 'X';

        let mut cloned = grid.clone();
        let mut visible = grid.clone_visible();
        assert!(grid.is_dirty());
        assert!(!cloned.is_dirty());
        assert!(!visible.is_dirty());
        assert!(cloned.take_dirty_regions().is_empty());
        assert!(visible.take_dirty_regions().is_empty());
    }
}
