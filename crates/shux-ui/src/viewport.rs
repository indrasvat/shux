//! Pane viewport clipping: which slice of a pane grid is shown when the grid
//! is larger than the layout rect it is composited into.
//!
//! A pane's VT grid can be **taller** than the rect it is drawn into:
//!
//! - `pane set-size --rows N` beyond the window geometry (an advertised path —
//!   "when you need the pane taller than the daemon default"),
//! - a split that shrinks a sibling's rect below its (unchanged) grid,
//! - a transient where the layout rect shrank before the PTY resize landed.
//!
//! When that happens the compositor must show *some* content, and it must show
//! the SAME region the pane cursor is mapped into. Historically the two
//! disagreed — content was bottom-anchored (`total_rows - visible_rows`) while
//! the cursor was top-clamped — so a pane with its content and cursor at the
//! top rendered blank-with-a-cursor in `window snapshot` (issue #108).
//!
//! Both the snapshot compose path (`composed::compose`) and the live-attach
//! path (`compositor::render_multi_pane`) route their vertical clipping AND
//! their cursor mapping through [`pane_view_row_offset`], so content and cursor
//! never disagree and the two render paths stay consistent.

/// The index of the first pane-grid row to display when the grid is composited
/// into a rect `visible_rows` tall, given the pane cursor's row.
///
/// Policy — a cursor-following viewport that keeps the cursor on screen:
///
/// - the whole grid fits (`total_rows <= visible_rows`) → `0`;
/// - the cursor is within the first `visible_rows` rows → `0` (the top-left
///   region — the reported case, where content sits at the top);
/// - the cursor is below that → scroll just far enough to keep it on the last
///   visible row, capped so the last window never runs past the grid.
///
/// This degrades to top-anchored for a cursor near the top (fixing #108) and to
/// bottom-anchored for a cursor near the bottom (a shell prompt — the "most
/// recent output stays visible" behavior). It is identical to a plain
/// `total_rows - visible_rows` bottom anchor exactly when the cursor is on the
/// last grid row, and identical to a plain `0` top anchor whenever the grid
/// fits, so it strictly dominates both fixed anchors.
///
/// Columns are always left-anchored (offset 0) by the callers — the "top-left"
/// half of the region — so only the row offset needs computing here.
pub fn pane_view_row_offset(total_rows: usize, visible_rows: usize, cursor_row: usize) -> usize {
    if visible_rows == 0 || total_rows <= visible_rows {
        return 0;
    }
    // Safe: total_rows > visible_rows in this branch.
    let max_offset = total_rows - visible_rows;
    if cursor_row < visible_rows {
        0
    } else {
        // `cursor_row + 1` is the count of rows up to and including the cursor;
        // subtract the window height to put the cursor on the last visible row.
        // Clamp so a cursor past the grid's end (defensive) never overruns.
        (cursor_row + 1 - visible_rows).min(max_offset)
    }
}

#[cfg(test)]
mod tests {
    use super::pane_view_row_offset;

    #[test]
    fn grid_fits_is_top_anchored() {
        assert_eq!(pane_view_row_offset(24, 24, 0), 0);
        assert_eq!(pane_view_row_offset(24, 24, 23), 0);
        assert_eq!(pane_view_row_offset(10, 24, 5), 0, "grid shorter than rect");
    }

    #[test]
    fn oversized_cursor_at_top_shows_top_left_region() {
        // The issue #108 shape: 60-row grid, 33-row rect, content+cursor at top.
        assert_eq!(pane_view_row_offset(60, 33, 0), 0);
        assert_eq!(pane_view_row_offset(60, 33, 2), 0);
        assert_eq!(
            pane_view_row_offset(60, 33, 32),
            0,
            "cursor on last visible row"
        );
    }

    #[test]
    fn oversized_cursor_below_the_fold_follows_cursor() {
        // Cursor one row past the fold → scroll by one.
        assert_eq!(pane_view_row_offset(60, 33, 33), 1);
        // Cursor near the bottom → offset keeps it on the last visible row.
        assert_eq!(pane_view_row_offset(60, 33, 40), 40 + 1 - 33);
    }

    #[test]
    fn oversized_cursor_at_bottom_is_bottom_anchored() {
        // Matches a plain `total_rows - visible_rows` anchor exactly.
        assert_eq!(pane_view_row_offset(60, 33, 59), 60 - 33);
    }

    #[test]
    fn cursor_past_grid_end_is_clamped_to_max_offset() {
        // Defensive: a cursor row beyond the grid never overruns the window.
        assert_eq!(pane_view_row_offset(60, 33, 1000), 60 - 33);
    }

    #[test]
    fn zero_visible_rows_is_zero() {
        assert_eq!(pane_view_row_offset(60, 0, 10), 0);
    }

    #[test]
    fn single_row_window_tracks_cursor() {
        assert_eq!(pane_view_row_offset(10, 1, 0), 0);
        assert_eq!(pane_view_row_offset(10, 1, 5), 5);
        assert_eq!(pane_view_row_offset(10, 1, 9), 9);
        assert_eq!(pane_view_row_offset(10, 1, 100), 9, "clamped to last row");
    }
}
