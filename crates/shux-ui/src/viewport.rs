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

use shux_core::layout::Rect;

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

/// Where one pane sat in a frame and which slice of its grid it showed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneView {
    pub rect: Rect,
    /// First grid row shown ([`pane_view_row_offset`]); columns start at 0.
    pub row_offset: usize,
    pub grid_rows: usize,
    pub grid_cols: usize,
}

impl PaneView {
    /// The view `vt` is drawn with inside `rect`. A grid taller than its rect
    /// (an oversized `pane set-size`, a split that shrank the rect, a transient
    /// resize-down) follows the cursor, so content and cursor agree (#108).
    pub fn of(rect: Rect, vt: &shux_vt::VirtualTerminal) -> Self {
        let grid = vt.grid();
        Self {
            rect,
            row_offset: pane_view_row_offset(grid.rows(), rect.height.into(), vt.cursor().row),
            grid_rows: grid.rows(),
            grid_cols: grid.cols(),
        }
    }
}

/// The 1-based grid cell a pointer at screen `(col, row)` names in `view`.
///
/// `None` when the point is on no grid cell shown there. `clamp` instead pins
/// it onto the nearest shown cell, for a gesture already underway; it is still
/// `None` when the view shows no cell at all.
pub fn pane_grid_cell(view: &PaneView, (col, row): (u16, u16), clamp: bool) -> Option<(u16, u16)> {
    let rect = view.rect;
    let shown_rows = view
        .grid_rows
        .saturating_sub(view.row_offset)
        .min(rect.height.into());
    let shown_cols = view.grid_cols.min(rect.width.into());
    if shown_rows == 0 || shown_cols == 0 {
        return None;
    }
    let x = usize::from(col.saturating_sub(rect.x));
    let y = usize::from(row.saturating_sub(rect.y));
    let on_grid = col >= rect.x && row >= rect.y && x < shown_cols && y < shown_rows;
    if !on_grid && !clamp {
        return None;
    }
    let x = x.min(shown_cols - 1) + 1;
    let y = view.row_offset + y.min(shown_rows - 1) + 1;
    Some((u16::try_from(x).ok()?, u16::try_from(y).ok()?))
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

    /// Every screen point, across grids taller, shorter and narrower than
    /// their rect, with and without scrollback, zoomed or not: through the view
    /// the compositor recorded, `pane_grid_cell` names exactly the character
    /// the attached client sees, rejects points that show none, and pins onto
    /// the nearest shown cell.
    #[test]
    fn pane_grid_cell_names_the_cell_the_compositor_drew() {
        use super::pane_grid_cell;
        use crate::BorderStyle;
        use crate::compositor::{
            CompositorConfig, MultiPaneFrame, RenderCompositor, pane_viewport,
        };
        use shux_core::layout::{Direction, LayoutNode, Rect, ZoomState};
        use shux_core::model::PaneId;
        use shux_vt::VirtualTerminal;
        use std::collections::HashMap;

        // Distinct for any row error under 94 lines.
        let sym = |line: usize, c: usize| char::from(33 + ((line * 31 + c) % 94) as u8);
        let (other, pane) = (PaneId::new(), PaneId::new());
        let layouts = [
            LayoutNode::leaf(pane),
            LayoutNode::split(
                Direction::Vertical,
                0.5,
                LayoutNode::leaf(other),
                LayoutNode::leaf(pane),
            ),
            LayoutNode::split(
                Direction::Horizontal,
                0.5,
                LayoutNode::leaf(other),
                LayoutNode::leaf(pane),
            ),
        ];
        let screens = [(17u16, 9u16), (12, 12), (30, 6), (302, 7)];
        let grids = [
            (1usize, 1usize),
            (3, 5),
            (7, 30),
            (13, 4),
            (20, 20),
            (10, 300),
        ];
        let filler = VirtualTerminal::new(2, 2);
        let mut checked = 0usize;
        for (w, h) in screens {
            for style in [BorderStyle::Rounded, BorderStyle::None] {
                for (layout, zoomed) in layouts.iter().flat_map(|l| [(l, false), (l, true)]) {
                    let zoom = zoomed.then(|| ZoomState {
                        saved_layout: layout.clone(),
                        zoomed_pane: pane,
                    });
                    for (rows, cols) in grids {
                        for (cursor, extra) in [(0, 0), (rows / 2, 3), (rows - 1, 0), (rows - 1, 5)]
                        {
                            // Line `extra + r` lands on grid row r.
                            let mut vt = VirtualTerminal::new(rows, cols);
                            let lines: Vec<String> = (0..rows + extra)
                                .map(|l| (0..cols).map(|c| sym(l, c)).collect())
                                .collect();
                            vt.process(lines.join("\r\n").as_bytes());
                            vt.process(format!("\x1b[{};1H", cursor + 1).as_bytes());
                            let mut vts = HashMap::new();
                            vts.insert(pane, &vt);
                            vts.insert(other, &filler);
                            let config = CompositorConfig {
                                status_bar_height: 1,
                                border_style: style,
                                ..CompositorConfig::default()
                            };
                            let mut comp = RenderCompositor::new(w, h, Vec::new(), config);
                            comp.render_multi_pane(MultiPaneFrame {
                                layout,
                                zoom: zoom.as_ref(),
                                focused: other,
                                vts: &vts,
                                chrome: None,
                                status_bar: None,
                            })
                            .unwrap();
                            let mut screen = VirtualTerminal::new(h.into(), w.into());
                            screen.process(comp.inner());
                            let seen = |x: u16, y: u16| {
                                screen.grid().visible_row(y.into())[usize::from(x)].ch
                            };
                            let view = comp.pane_views()[&pane];
                            // The rect the attach loop hit-tests (`pane_rect_in`).
                            let content = Rect::new(0, 0, w, h - 1);
                            let rect = if zoomed {
                                content
                            } else {
                                layout
                                    .compute_rects(pane_viewport(content, style, false))
                                    .into_iter()
                                    .find_map(|(pid, r)| (pid == pane).then_some(r))
                                    .expect("the pane is in its layout")
                            };
                            let ctx = format!(
                                "{w}x{h} {style:?} zoomed={zoomed} grid {rows}x{cols} \
                                 cursor {cursor} scrollback {extra} {rect:?}"
                            );
                            assert_eq!(view.rect, rect, "{ctx}: recorded view rect");
                            let shown_rows =
                                rows.saturating_sub(view.row_offset).min(rect.height.into());
                            let shown_cols = cols.min(rect.width.into());
                            for y in 0..h {
                                for x in 0..w {
                                    let inside = x >= rect.x
                                        && x < rect.x + rect.width
                                        && y >= rect.y
                                        && y < rect.y + rect.height;
                                    let exact = pane_grid_cell(&view, (x, y), false);
                                    match exact {
                                        Some((c, r)) => {
                                            assert!(inside, "{ctx}: ({x},{y}) is outside the rect");
                                            assert_eq!(
                                                seen(x, y),
                                                sym(extra + usize::from(r) - 1, usize::from(c) - 1),
                                                "{ctx}: ({x},{y}) named grid cell ({c},{r})"
                                            );
                                        }
                                        None if inside => assert_eq!(
                                            seen(x, y),
                                            ' ',
                                            "{ctx}: ({x},{y}) shows a grid cell but maps to none"
                                        ),
                                        None => {}
                                    }
                                    let Some((c, r)) = pane_grid_cell(&view, (x, y), true) else {
                                        assert!(
                                            shown_rows == 0 || shown_cols == 0,
                                            "{ctx}: clamp found no cell"
                                        );
                                        continue;
                                    };
                                    if let Some(exact) = exact {
                                        assert_eq!((c, r), exact, "{ctx}: ({x},{y})");
                                    }
                                    let near = (
                                        x.clamp(rect.x, rect.x + shown_cols as u16 - 1),
                                        y.clamp(rect.y, rect.y + shown_rows as u16 - 1),
                                    );
                                    let at = (
                                        rect.x + c - 1,
                                        rect.y + (usize::from(r) - 1 - view.row_offset) as u16,
                                    );
                                    assert_eq!(
                                        at, near,
                                        "{ctx}: ({x},{y}) not pinned to the nearest cell"
                                    );
                                    assert_eq!(
                                        seen(at.0, at.1),
                                        sym(extra + usize::from(r) - 1, usize::from(c) - 1),
                                        "{ctx}: ({x},{y}) pinned to a cell not shown"
                                    );
                                    checked += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(checked > 500_000, "the sweep covered only {checked} points");
    }
}
