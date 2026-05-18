use std::collections::VecDeque;
use std::ops::{Index, IndexMut};

use crate::cell::{Cell, Color};

/// A single row of terminal cells.
#[derive(Debug, Clone)]
pub struct Row {
    /// Cell storage. `pub(crate)` for access from `Grid::insert_chars`/`Grid::delete_chars`.
    pub(crate) cells: Vec<Cell>,
    /// Whether this row was wrapped from the previous line (soft wrap).
    pub wrapped: bool,
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
}

impl Default for GridConfig {
    fn default() -> Self {
        GridConfig {
            max_scrollback: 5000,
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
#[derive(Debug, Clone)]
pub struct Grid {
    /// All lines: scrollback + visible area.
    raw: VecDeque<Row>,
    /// Number of visible rows (terminal height).
    rows: usize,
    /// Number of columns (terminal width).
    cols: usize,
    /// Configuration (max scrollback, etc.).
    config: GridConfig,
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
            config,
        }
    }

    /// Number of visible rows.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Number of columns.
    pub fn cols(&self) -> usize {
        self.cols
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
            config: GridConfig { max_scrollback: 0 },
        }
    }

    /// Mutably access a visible row (0 = top of visible area).
    pub fn visible_row_mut(&mut self, row: usize) -> &mut Row {
        let idx = self.scrollback_len() + row;
        &mut self.raw[idx]
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
    }

    /// Scroll the visible area down by one line within a scroll region.
    /// A new empty line appears at the top of the region.
    /// The bottom line of the region is discarded.
    pub fn scroll_down(&mut self, region_top: usize, region_bottom: usize) {
        let sb = self.scrollback_len();
        let abs_top = sb + region_top;
        let abs_bottom = sb + region_bottom;
        self.raw.remove(abs_bottom);
        self.raw.insert(abs_top, Row::new(self.cols));
    }

    /// Clear all visible rows (reset to empty with given background).
    pub fn clear_visible(&mut self, bg: Color) {
        let sb = self.scrollback_len();
        for i in sb..self.raw.len() {
            self.raw[i].reset(bg);
        }
    }

    /// Clear rows from `start_row` to the end of the visible area.
    pub fn clear_below(&mut self, start_row: usize, bg: Color) {
        let sb = self.scrollback_len();
        for i in (sb + start_row)..self.raw.len() {
            self.raw[i].reset(bg);
        }
    }

    /// Clear rows from the top of the visible area to `end_row` (inclusive).
    pub fn clear_above(&mut self, end_row: usize, bg: Color) {
        let sb = self.scrollback_len();
        for i in sb..=(sb + end_row) {
            self.raw[i].reset(bg);
        }
    }

    /// Resize the grid. Handles both growing and shrinking.
    ///
    /// On shrink (fewer rows): excess visible lines are kept (the caller
    /// adjusts the cursor; we simply reduce the visible window).
    /// On grow (more rows): lines are pulled back from scrollback if available,
    /// otherwise new blank lines are appended.
    /// Column resize: each row is resized (truncated or extended).
    pub fn resize(&mut self, new_rows: usize, new_cols: usize) {
        // Handle column resize first.
        if new_cols != self.cols {
            for row in self.raw.iter_mut() {
                row.resize(new_cols, Cell::default());
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
    }

    /// Clear the scrollback buffer entirely.
    pub fn clear_scrollback(&mut self) {
        let sb = self.scrollback_len();
        for _ in 0..sb {
            self.raw.pop_front();
        }
    }

    /// Erase `count` characters starting at `(row, col)` in the visible area.
    pub fn erase_chars(&mut self, row: usize, col: usize, count: usize, bg: Color) {
        let r = self.visible_row_mut(row);
        let end = (col + count).min(r.len());
        for c in col..end {
            r[c].reset(bg);
        }
    }

    /// Insert `count` blank cells at `(row, col)`, shifting existing cells right.
    /// Cells that shift past the right edge are lost.
    pub fn insert_chars(&mut self, row: usize, col: usize, count: usize) {
        let r = self.visible_row_mut(row);
        let len = r.len();
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
    }

    /// Delete `count` cells at `(row, col)`, shifting remaining cells left.
    /// New cells at the right edge are blank.
    pub fn delete_chars(&mut self, row: usize, col: usize, count: usize) {
        let r = self.visible_row_mut(row);
        let len = r.len();
        let actual = count.min(len.saturating_sub(col));
        // Shift left.
        for i in col..(len - actual) {
            r.cells[i] = r.cells[i + actual].clone();
        }
        // Fill right edge with blanks.
        for i in (len - actual)..len {
            r.cells[i] = Cell::default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let config = GridConfig { max_scrollback: 2 };
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
        // Column 9 is gone.
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
}
