use std::collections::VecDeque;
use std::ops::{Deref, DerefMut, Index, IndexMut, Range};

use crate::cell::{Cell, Color};

/// A half-open dirty cell range on one visible row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirtyRegion {
    /// Visible row index.
    pub row: usize,
    /// Dirty columns in `start..end` form.
    pub cols: Range<usize>,
}

#[derive(Debug)]
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
        for row in &mut self.full_rows {
            *row = false;
        }
        for row in &mut self.rows {
            *row = None;
        }
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
#[derive(Debug, Clone)]
pub struct Row {
    /// Cell storage. `pub(crate)` for access from `Grid::insert_chars`/`Grid::delete_chars`.
    pub(crate) cells: Vec<Cell>,
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
            cells: vec![Cell::default(); cols],
            wrapped: false,
        }
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
        self.cells.resize(cols, template);
    }

    /// Reset all cells in the row to the given background color.
    pub fn reset(&mut self, bg: Color) {
        for cell in &mut self.cells {
            cell.reset(bg);
        }
        self.wrapped = false;
    }

    pub(crate) fn clear_wide_pair_around(&mut self, col: usize, bg: Color) {
        if col >= self.cells.len() {
            return;
        }

        if self.cells[col].is_wide_continuation() {
            self.cells[col].reset(bg);
            if col > 0 && self.cells[col - 1].is_wide() {
                self.cells[col - 1].reset(bg);
            }
        } else if self.cells[col].is_wide() {
            self.cells[col].reset(bg);
            if col + 1 < self.cells.len() && self.cells[col + 1].is_wide_continuation() {
                self.cells[col + 1].reset(bg);
            }
        }
    }

    pub(crate) fn sanitize_wide_pairs(&mut self, bg: Color) {
        for col in 0..self.cells.len() {
            if self.cells[col].is_wide() {
                let has_tail = col + 1 < self.cells.len()
                    && self.cells[col + 1].is_wide_continuation()
                    && self.cells[col + 1].ch == ' ';
                if !has_tail {
                    self.cells[col].reset(bg);
                }
            } else if self.cells[col].is_wide_continuation() {
                let has_head = col > 0 && self.cells[col - 1].is_wide();
                if !has_head || self.cells[col].ch != ' ' {
                    self.cells[col].reset(bg);
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

        for col in start..end {
            self.cells[col].reset(bg);
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
        &mut self.cells[col]
    }
}

/// Configuration for the grid.
#[derive(Debug, Clone)]
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
            config,
            mutations: 0,
        }
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
        // wrapping is unreachable in practice (u64 write ops); we never rely on
        // the absolute value, only on before != after within one batch.
        self.mutations = self.mutations.wrapping_add(1);
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
    pub fn mark_all_dirty(&mut self) {
        self.bump_mutations();
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
        }
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

    /// Scroll the visible area up by one line within a scroll region.
    /// The top line of the region moves into scrollback (if region starts at line 0).
    /// A new empty line appears at the bottom of the region.
    pub fn scroll_up(&mut self, region_top: usize, region_bottom: usize) {
        self.bump_mutations();
        if region_top == 0 && region_bottom == self.rows - 1 {
            // Full-screen scroll: top line goes to scrollback.
            // We already have it in the VecDeque -- just add a new line at the bottom.
            self.raw.push_back(Row::new(self.cols));
            // Trim scrollback if over limit.
            let max_total = self.rows + self.config.max_scrollback;
            while self.raw.len() > max_total {
                self.raw.pop_front();
            }
        } else {
            // Scroll region: remove top of region, insert blank at bottom of region.
            let sb = self.scrollback_len();
            let abs_top = sb + region_top;
            let abs_bottom = sb + region_bottom;
            self.raw.remove(abs_top);
            self.raw.insert(abs_bottom, Row::new(self.cols));
        }
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

    /// Scroll the visible area down by one line within a scroll region.
    /// A new empty line appears at the top of the region.
    /// The bottom line of the region is discarded.
    pub fn scroll_down(&mut self, region_top: usize, region_bottom: usize) {
        self.bump_mutations();
        let sb = self.scrollback_len();
        let abs_top = sb + region_top;
        let abs_bottom = sb + region_bottom;
        self.raw.remove(abs_bottom);
        self.raw.insert(abs_top, Row::new(self.cols));
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

    /// Clear all visible rows (reset to empty with given background).
    pub fn clear_visible(&mut self, bg: Color) {
        self.bump_mutations();
        let sb = self.scrollback_len();
        for i in sb..self.raw.len() {
            self.raw[i].reset(bg);
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
                if let Some(back) = self.raw.back() {
                    if back.is_blank() {
                        self.raw.pop_back();
                    }
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
                row.cells.clone()
            };

            if let Some((cursor_row, cursor_col)) = cursor_abs {
                if cursor_row == abs_row {
                    let row_offset = display_width_until(&row.cells, cursor_col);
                    cursor_anchor = Some(CursorAnchor {
                        logical_line: logical_lines.len(),
                        display_offset: current.display_width + row_offset,
                    });
                }
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

        if let Some(anchor) = &mut cursor_anchor {
            if let Some(line) = logical_lines.get(anchor.logical_line) {
                anchor.display_offset = anchor.display_offset.min(line.display_width);
            }
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
        for i in (col..len).rev() {
            let target = i + count;
            if target < len {
                r.cells[target] = r.cells[i].clone();
            }
        }
        // Fill inserted positions with blanks.
        for i in col..(col + count).min(len) {
            r.cells[i] = Cell::default();
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
        for i in col..(len - actual) {
            r.cells[i] = r.cells[i + actual].clone();
        }
        // Fill right edge with blanks.
        for i in (len - actual)..len {
            r.cells[i] = Cell::default();
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
        grid.visible_row_mut(2).cells[0].ch = 'V';

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
