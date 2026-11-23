//! Which slice of each pane's grid a frame shows, decided once per frame and
//! shared by every consumer: live attach, snapshots, mouse input, selection.
//!
//! A pane's grid and the rect it is drawn into differ whenever another client
//! sizes the panes (#190), a pane is pinned with `pane set-size`, or a resize
//! is still in flight. Each axis is then handled on its own:
//!
//! - the grid fits the rect → offset 0, and the rest of the rect is surplus,
//!   painted with the theme's `status_bg`;
//! - the grid overflows it → the slice follows the cursor ([`follow_offset`]),
//!   except that a cursor hidden for [`HIDDEN_CURSOR_LATCH_NS`] anchors both
//!   axes at the top-left: full-screen TUIs (btop, htop) hide the cursor for
//!   good, while vim hides it only around each redraw.

use std::borrow::Cow;
use std::collections::HashMap;

use shux_core::layout::{LayoutNode, Rect, ZoomState};
use shux_core::model::PaneId;
use shux_vt::{Cell, CellFlags, CellStyle, Cursor, Row};

use crate::borders::BorderStyle;

/// How long a cursor stays hidden before an overflowing pane anchors at its
/// top-left. Measured: vim hides it for at most 32 ms around a redraw.
pub const HIDDEN_CURSOR_LATCH_NS: u64 = 150_000_000;

/// The first grid line (row or column) to show of `total` in a window
/// `visible` long, keeping `cursor` in view: 0 while it fits or the cursor is
/// within the first window, else just far enough to put the cursor on the
/// window's last line, never past the grid's end.
pub fn follow_offset(total: usize, visible: usize, cursor: usize) -> usize {
    if visible == 0 || total <= visible || cursor < visible {
        return 0;
    }
    (cursor + 1 - visible).min(total - visible)
}

/// Where one pane sits in a frame and which slice of its grid it shows.
/// Shown dimensions and surplus derive from these fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneViewport {
    pub rect: Rect,
    pub grid_rows: usize,
    pub grid_cols: usize,
    pub row_offset: usize,
    pub col_offset: usize,
}

impl PaneViewport {
    /// The viewport a `grid_rows`x`grid_cols` grid with `cursor` gets inside
    /// `rect` at `now_ns` on the [`shux_vt::monotonic_now_ns`] clock.
    pub fn new(
        rect: Rect,
        grid_rows: usize,
        grid_cols: usize,
        cursor: &Cursor,
        now_ns: u64,
    ) -> Self {
        let (rows, cols) = (usize::from(rect.height), usize::from(rect.width));
        let latched = cursor
            .hidden_for_ns(now_ns)
            .is_some_and(|hidden| hidden >= HIDDEN_CURSOR_LATCH_NS);
        let (row_offset, col_offset) = if latched {
            (0, 0)
        } else {
            (
                follow_offset(grid_rows, rows, cursor.row),
                follow_offset(grid_cols, cols, cursor.col),
            )
        };
        Self {
            rect,
            grid_rows,
            grid_cols,
            row_offset,
            col_offset,
        }
    }

    pub fn of(rect: Rect, vt: &shux_vt::VirtualTerminal, now_ns: u64) -> Self {
        Self::new(
            rect,
            vt.grid().rows(),
            vt.grid().cols(),
            vt.cursor(),
            now_ns,
        )
    }

    /// This viewport at other offsets, kept within the grid.
    pub fn with_offsets(self, row_offset: usize, col_offset: usize) -> Self {
        let max_row = self.grid_rows.saturating_sub(self.rect.height.into());
        let max_col = self.grid_cols.saturating_sub(self.rect.width.into());
        Self {
            row_offset: row_offset.min(max_row),
            col_offset: col_offset.min(max_col),
            ..self
        }
    }

    pub fn shown_rows(&self) -> usize {
        self.grid_rows
            .saturating_sub(self.row_offset)
            .min(self.rect.height.into())
    }

    pub fn shown_cols(&self) -> usize {
        self.grid_cols
            .saturating_sub(self.col_offset)
            .min(self.rect.width.into())
    }

    /// Whether the grid overflows the rect on either axis.
    pub fn overflows(&self) -> bool {
        self.grid_rows > usize::from(self.rect.height)
            || self.grid_cols > usize::from(self.rect.width)
    }

    /// Whether the grid is exactly the rect's size.
    pub fn fits_exactly(&self) -> bool {
        (self.grid_cols, self.grid_rows) == (self.rect.width.into(), self.rect.height.into())
    }

    /// When the hidden-cursor latch will move this viewport, if it is pending.
    pub fn latch_deadline(&self, cursor: &Cursor, now_ns: u64) -> Option<u64> {
        let hidden = cursor.hidden_for_ns(now_ns)?;
        (self.overflows() && hidden < HIDDEN_CURSOR_LATCH_NS)
            .then(|| cursor.hidden_since_ns + HIDDEN_CURSOR_LATCH_NS)
    }

    /// The screen `(col, row)` the pane cursor is drawn at: `None` when it is
    /// hidden or outside the shown slice. Never clamped onto an edge.
    pub fn cursor_cell(&self, cursor: &Cursor) -> Option<(u16, u16)> {
        if !cursor.visible {
            return None;
        }
        let row = cursor.row.checked_sub(self.row_offset)?;
        let col = cursor.col.checked_sub(self.col_offset)?;
        if row >= self.shown_rows() || col >= self.shown_cols() {
            return None;
        }
        Some((self.rect.x + col as u16, self.rect.y + row as u16))
    }

    /// The cell drawn at grid column `col` of `row`. A wide glyph cut by an
    /// edge of the slice cannot be drawn whole: its half inside becomes a
    /// blank cell with the glyph's visible background.
    pub fn shown_cell<'r>(&self, row: &'r Row, col: usize) -> Cow<'r, Cell> {
        let cell = &row[col];
        let last = self.col_offset + self.shown_cols().saturating_sub(1);
        if col == self.col_offset && col > 0 && cell.is_wide_continuation() {
            Cow::Owned(blank_like(&row[col - 1]))
        } else if col == last && cell.width == 2 {
            Cow::Owned(blank_like(cell))
        } else {
            Cow::Borrowed(cell)
        }
    }
}

/// A space showing `cell`'s background, inverse video included. Nothing of
/// the glyph survives: no other attribute, grapheme or hyperlink.
fn blank_like(cell: &Cell) -> Cell {
    let mut flags = CellFlags::default();
    if cell.style.flags.contains(CellFlags::INVERSE) {
        flags.set(CellFlags::INVERSE);
    }
    Cell {
        ch: ' ',
        width: 1,
        style: CellStyle {
            fg: cell.style.fg,
            bg: cell.style.bg,
            flags,
        },
        extended: None,
    }
}

/// `pinned W×H` or `⤡ W×H` for a pane whose grid is not its rect's size.
pub fn size_label(view: &PaneViewport, pinned: bool) -> Option<String> {
    if view.fits_exactly() {
        return None;
    }
    let size = format!("{}×{}", view.grid_cols, view.grid_rows);
    Some(if pinned {
        format!("pinned {size}")
    } else {
        format!("⤡ {size}")
    })
}

/// What one pane presents to a frame: its grid's size and cursor.
#[derive(Debug, Clone, Copy)]
pub struct PresentedPane<'a> {
    pub rows: usize,
    pub cols: usize,
    pub cursor: &'a Cursor,
}

/// What each pane in `vts` presents.
pub fn presented_panes<'a>(
    vts: &HashMap<PaneId, &'a shux_vt::VirtualTerminal>,
) -> HashMap<PaneId, PresentedPane<'a>> {
    vts.iter()
        .map(|(pid, vt)| {
            let pane = PresentedPane {
                rows: vt.grid().rows(),
                cols: vt.grid().cols(),
                cursor: vt.cursor(),
            };
            (*pid, pane)
        })
        .collect()
}

/// Everything about one frame's geometry, prepared once before anything is
/// drawn so chrome, cells, cursor and mouse input agree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameGeometry {
    /// Everything above the status bar.
    pub content: Rect,
    pub borders_on: bool,
    /// Every shown pane's rect, in layout order, grid or not.
    pub rects: Vec<(PaneId, Rect)>,
    /// Viewports of the panes that presented a grid.
    pub views: HashMap<PaneId, PaneViewport>,
    /// The earliest pending hidden-cursor latch: a frame drawn then differs.
    pub deadline_ns: Option<u64>,
}

/// Inputs to [`prepare_frame`].
pub struct FrameInputs<'a> {
    pub layout: &'a LayoutNode,
    pub zoom: Option<&'a ZoomState>,
    pub cols: u16,
    pub rows: u16,
    pub status_bar_height: u16,
    pub border_style: BorderStyle,
    pub panes: &'a HashMap<PaneId, PresentedPane<'a>>,
    pub now_ns: u64,
}

pub fn prepare_frame(inputs: &FrameInputs<'_>) -> FrameGeometry {
    let content = Rect::new(
        0,
        0,
        inputs.cols,
        inputs.rows.saturating_sub(inputs.status_bar_height),
    );
    let zoomed = inputs.zoom.is_some();
    let borders_on = crate::compositor::borders_visible(content, inputs.border_style, zoomed);
    let rects = match inputs.zoom {
        Some(zoom) => vec![(zoom.zoomed_pane, content)],
        None => inputs
            .layout
            .compute_rects(crate::compositor::pane_viewport(
                content,
                inputs.border_style,
                false,
            )),
    };
    let mut views = HashMap::new();
    let mut deadline_ns: Option<u64> = None;
    for (pid, rect) in &rects {
        let Some(pane) = inputs.panes.get(pid) else {
            continue;
        };
        let view = PaneViewport::new(*rect, pane.rows, pane.cols, pane.cursor, inputs.now_ns);
        if let Some(at) = view.latch_deadline(pane.cursor, inputs.now_ns) {
            deadline_ns = Some(deadline_ns.map_or(at, |d| d.min(at)));
        }
        views.insert(*pid, view);
    }
    FrameGeometry {
        content,
        borders_on,
        rects,
        views,
        deadline_ns,
    }
}

/// The 1-based grid cell a pointer at screen `(col, row)` names in `view`.
///
/// `None` when the point is on no grid cell shown there. `clamp` instead pins
/// it onto the nearest shown cell, for a gesture already underway; it is still
/// `None` when the view shows no cell at all.
pub fn pane_grid_cell(
    view: &PaneViewport,
    (col, row): (u16, u16),
    clamp: bool,
) -> Option<(u16, u16)> {
    let rect = view.rect;
    let (shown_rows, shown_cols) = (view.shown_rows(), view.shown_cols());
    if shown_rows == 0 || shown_cols == 0 {
        return None;
    }
    let x = usize::from(col.saturating_sub(rect.x));
    let y = usize::from(row.saturating_sub(rect.y));
    let on_grid = col >= rect.x && row >= rect.y && x < shown_cols && y < shown_rows;
    if !on_grid && !clamp {
        return None;
    }
    let x = view.col_offset + x.min(shown_cols - 1) + 1;
    let y = view.row_offset + y.min(shown_rows - 1) + 1;
    Some((u16::try_from(x).ok()?, u16::try_from(y).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor_at(row: usize, col: usize) -> Cursor {
        Cursor {
            row,
            col,
            ..Cursor::new()
        }
    }

    fn hidden_since(mut cursor: Cursor, at: u64) -> Cursor {
        cursor.visible = false;
        cursor.hidden_since_ns = at;
        cursor
    }

    const MS: u64 = 1_000_000;

    /// A 40x100 grid in a 20x50 rect follows a visible cursor on both axes.
    /// Hidden for less than the latch (vim around a redraw) it keeps
    /// following; hidden for the latch (btop) it anchors top-left, and the
    /// moment it shows again it follows once more.
    #[test]
    fn the_latch_moves_the_slice_only_after_the_cursor_stays_hidden() {
        let rect = Rect::new(3, 2, 50, 20);
        let at = |cursor: &Cursor, now| {
            let v = PaneViewport::new(rect, 40, 100, cursor, now);
            (v.row_offset, v.col_offset)
        };
        let shown = cursor_at(30, 70);
        assert_eq!(at(&shown, 0), (11, 21));
        let t0 = 1_000 * MS;
        let hidden = hidden_since(shown.clone(), t0);
        assert_eq!(at(&hidden, t0 + 149 * MS), (11, 21), "a redraw's flicker");
        assert_eq!(at(&hidden, t0 + 150 * MS), (0, 0), "hidden for the latch");
        assert_eq!(at(&shown, t0 + 500 * MS), (11, 21), "shown again");

        let view = PaneViewport::new(rect, 40, 100, &hidden, t0 + 10 * MS);
        assert_eq!(
            view.latch_deadline(&hidden, t0 + 10 * MS),
            Some(t0 + 150 * MS)
        );
        let latched = PaneViewport::new(rect, 40, 100, &hidden, t0 + 150 * MS);
        assert_eq!(latched.latch_deadline(&hidden, t0 + 150 * MS), None);
        let fits = PaneViewport::new(rect, 20, 50, &hidden, t0);
        assert_eq!(fits.latch_deadline(&hidden, t0), None, "nothing to move");
    }

    /// Each axis on its own: a grid wider but shorter than its rect follows
    /// the cursor across and stays at row 0, with the rows below it surplus.
    #[test]
    fn the_axes_are_independent() {
        let view = PaneViewport::new(Rect::new(0, 0, 10, 8), 4, 30, &cursor_at(3, 25), 0);
        assert_eq!((view.row_offset, view.col_offset), (0, 16));
        assert_eq!((view.shown_rows(), view.shown_cols()), (4, 10));
        assert!(view.overflows() && !view.fits_exactly());
    }

    /// The cursor lands where its cell is drawn on both axes, and is not drawn
    /// at all when the slice does not show its cell: never clamped to an edge.
    #[test]
    fn the_cursor_is_mapped_through_both_offsets_and_hidden_outside() {
        let rect = Rect::new(5, 1, 10, 4);
        let view = PaneViewport::new(rect, 12, 30, &cursor_at(9, 22), 0);
        assert_eq!((view.row_offset, view.col_offset), (6, 13));
        assert_eq!(view.cursor_cell(&cursor_at(9, 22)), Some((5 + 9, 1 + 3)));
        assert_eq!(view.cursor_cell(&cursor_at(6, 13)), Some((5, 1)));
        assert_eq!(view.cursor_cell(&cursor_at(5, 13)), None, "above the slice");
        assert_eq!(view.cursor_cell(&cursor_at(9, 12)), None, "left of it");
        assert_eq!(view.cursor_cell(&cursor_at(9, 23)), None, "right of it");
        let mut hidden = cursor_at(9, 22);
        hidden.visible = false;
        assert_eq!(view.cursor_cell(&hidden), None);
    }

    /// A wide glyph cut by either edge draws as a blank cell carrying the
    /// background the glyph showed there, inverse video included, and nothing
    /// else of it.
    #[test]
    fn a_wide_glyph_cut_by_an_edge_draws_its_background_only() {
        let mut vt = shux_vt::VirtualTerminal::new(1, 12);
        // Cols 0-1 plain wide, 2-3 inverse+bold wide on red, 4-9 ascii, 10-11 wide.
        vt.process(
            "界\x1b[1;7;41m界\x1b[m\x1b]8;;https://x\x1b\\abcdef界\x1b]8;;\x1b\\".as_bytes(),
        );
        let row = vt.grid().visible_row(0);
        assert!(row[3].is_wide_continuation() && row[10].width == 2);
        // Show cols 3..=10: the slice starts on a continuation, ends on a lead.
        let view = PaneViewport {
            rect: Rect::new(0, 0, 8, 1),
            grid_rows: 1,
            grid_cols: 12,
            row_offset: 0,
            col_offset: 3,
        };
        let left = view.shown_cell(row, 3);
        assert_eq!(
            (left.ch, left.width, left.extended.is_none()),
            (' ', 1, true)
        );
        assert_eq!(left.style.bg, row[2].style.bg);
        assert_eq!(left.style.fg, row[2].style.fg);
        assert!(left.style.flags.contains(CellFlags::INVERSE));
        assert!(!left.style.flags.contains(CellFlags::BOLD));
        let right = view.shown_cell(row, 10);
        assert_eq!(
            (right.ch, right.width, right.extended.is_none()),
            (' ', 1, true)
        );
        assert!(row[10].extended.is_some(), "the glyph carried a hyperlink");
        assert!(matches!(view.shown_cell(row, 4), Cow::Borrowed(_)));
        let whole = PaneViewport {
            col_offset: 2,
            ..view
        };
        assert!(matches!(whole.shown_cell(row, 2), Cow::Borrowed(_)));
        assert!(matches!(whole.shown_cell(row, 3), Cow::Borrowed(_)));
    }

    #[test]
    fn a_label_names_a_grid_that_is_not_its_rects_size() {
        let view = |rows, cols| {
            PaneViewport::new(Rect::new(0, 0, 80, 24), rows, cols, &cursor_at(0, 0), 0)
        };
        assert_eq!(size_label(&view(24, 80), true), None);
        assert_eq!(size_label(&view(24, 80), false), None);
        assert_eq!(
            size_label(&view(37, 118), false).as_deref(),
            Some("⤡ 118×37")
        );
        assert_eq!(
            size_label(&view(50, 200), true).as_deref(),
            Some("pinned 200×50")
        );
        assert_eq!(size_label(&view(24, 79), false).as_deref(), Some("⤡ 79×24"));
    }

    /// The frame's deadline is the earliest pending latch; a latched or
    /// fitting pane contributes none.
    #[test]
    fn the_frame_deadline_is_the_earliest_pending_latch() {
        let (a, b, c) = (PaneId::new(), PaneId::new(), PaneId::new());
        let layout = LayoutNode::split(
            shux_core::layout::Direction::Vertical,
            0.5,
            LayoutNode::leaf(a),
            LayoutNode::split(
                shux_core::layout::Direction::Vertical,
                0.5,
                LayoutNode::leaf(b),
                LayoutNode::leaf(c),
            ),
        );
        let now = 1_000 * MS;
        let (ca, cb, cc) = (
            hidden_since(cursor_at(0, 0), now - 100 * MS),
            hidden_since(cursor_at(0, 0), now - 20 * MS),
            hidden_since(cursor_at(0, 0), now - 400 * MS),
        );
        let big = |cursor| PresentedPane {
            rows: 200,
            cols: 200,
            cursor,
        };
        let panes = HashMap::from([(a, big(&ca)), (b, big(&cb)), (c, big(&cc))]);
        let geometry = prepare_frame(&FrameInputs {
            layout: &layout,
            zoom: None,
            cols: 90,
            rows: 30,
            status_bar_height: 1,
            border_style: BorderStyle::Rounded,
            panes: &panes,
            now_ns: now,
        });
        assert_eq!(geometry.deadline_ns, Some(now + 50 * MS));
        assert_eq!(geometry.views.len(), 3);
        assert!(geometry.borders_on);
        assert_eq!(
            (geometry.views[&c].row_offset, geometry.views[&c].col_offset),
            (0, 0)
        );
    }

    #[test]
    fn grid_fits_is_top_anchored() {
        assert_eq!(follow_offset(24, 24, 0), 0);
        assert_eq!(follow_offset(24, 24, 23), 0);
        assert_eq!(follow_offset(10, 24, 5), 0, "grid shorter than rect");
    }

    #[test]
    fn oversized_cursor_at_top_shows_top_left_region() {
        // The issue #108 shape: 60-row grid, 33-row rect, content+cursor at top.
        assert_eq!(follow_offset(60, 33, 0), 0);
        assert_eq!(follow_offset(60, 33, 2), 0);
        assert_eq!(follow_offset(60, 33, 32), 0, "cursor on last visible row");
    }

    #[test]
    fn oversized_cursor_below_the_fold_follows_cursor() {
        // Cursor one row past the fold → scroll by one.
        assert_eq!(follow_offset(60, 33, 33), 1);
        // Cursor near the bottom → offset keeps it on the last visible row.
        assert_eq!(follow_offset(60, 33, 40), 40 + 1 - 33);
    }

    #[test]
    fn oversized_cursor_at_bottom_is_bottom_anchored() {
        // Matches a plain `total_rows - visible_rows` anchor exactly.
        assert_eq!(follow_offset(60, 33, 59), 60 - 33);
    }

    #[test]
    fn cursor_past_grid_end_is_clamped_to_max_offset() {
        // Defensive: a cursor row beyond the grid never overruns the window.
        assert_eq!(follow_offset(60, 33, 1000), 60 - 33);
    }

    #[test]
    fn zero_visible_rows_is_zero() {
        assert_eq!(follow_offset(60, 0, 10), 0);
    }

    #[test]
    fn single_row_window_tracks_cursor() {
        assert_eq!(follow_offset(10, 1, 0), 0);
        assert_eq!(follow_offset(10, 1, 5), 5);
        assert_eq!(follow_offset(10, 1, 9), 9);
        assert_eq!(follow_offset(10, 1, 100), 9, "clamped to last row");
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
                                geometry: None,
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

    /// One captured state through both renderers: the attach ANSI, decoded,
    /// and the snapshot composer's grid show the same cells, the same cursor
    /// and the same surplus, for a pane cropped on both axes with wide glyphs
    /// cut at both edges next to a pane smaller than its rect.
    #[test]
    fn attach_and_snapshot_show_the_same_viewport() {
        use crate::compositor::{CompositorConfig, MultiPaneFrame, RenderCompositor};
        use crate::{BorderColors, ComposeInputs, compose};
        use shux_core::layout::Direction;
        use shux_vt::{Color, VirtualTerminal};

        let (big, small) = (PaneId::new(), PaneId::new());
        let layout = LayoutNode::split(
            Direction::Vertical,
            0.5,
            LayoutNode::leaf(big),
            LayoutNode::leaf(small),
        );
        let rows = 12u16;
        // A window whose left pane is an even width, so a slice starting on a
        // continuation also ends on a lead.
        let home = Cursor::new();
        let probe = HashMap::from([(
            big,
            PresentedPane {
                rows: 20,
                cols: 60,
                cursor: &home,
            },
        )]);
        let width_at = |cols| {
            prepare_frame(&FrameInputs {
                layout: &layout,
                zoom: None,
                cols,
                rows,
                status_bar_height: 1,
                border_style: BorderStyle::Rounded,
                panes: &probe,
                now_ns: 0,
            })
            .views[&big]
                .rect
                .width
        };
        let cols = (40..48)
            .find(|&c| width_at(c).is_multiple_of(2))
            .expect("some width splits evenly");
        let width = usize::from(width_at(cols));
        let mut big_vt = shux_vt::VirtualTerminal::new(20, 60);
        for r in 0..20 {
            // Truecolor, indexed and basic runs; wide glyphs from column 20,
            // so the slice's odd offset cuts one at each edge.
            big_vt.process(
                format!(
                    "\x1b[{};1H\x1b[38;2;200;{};40m{r:02}ab\x1b[48;5;{}mcd\x1b[31;7mef\x1b[m\
                     gh{}\x1b[44m界界界界界界界界界界界界界界界界界界界界\x1b[m",
                    r + 1,
                    r * 10,
                    16 + r,
                    "-".repeat(10),
                )
                .as_bytes(),
            );
        }
        // Column offset `cursor + 1 - width`: 21, odd.
        let cursor_col = width + 20;
        big_vt.process(format!("\x1b[16;{}H", cursor_col + 1).as_bytes());
        let mut small_vt = VirtualTerminal::new(4, 6);
        small_vt.process(b"\x1b[32msmall\x1b[m");
        let vts = HashMap::from([(big, &big_vt), (small, &small_vt)]);
        let colors = BorderColors {
            surplus: crossterm::style::Color::Rgb { r: 1, g: 2, b: 3 },
            ..BorderColors::default()
        };
        let geometry = prepare_frame(&FrameInputs {
            layout: &layout,
            zoom: None,
            cols,
            rows,
            status_bar_height: 1,
            border_style: BorderStyle::Rounded,
            panes: &presented_panes(&vts),
            now_ns: 0,
        });
        let view = geometry.views[&big];
        assert!(view.row_offset > 0 && view.col_offset > 0, "{view:?}");
        assert!(
            !view.col_offset.is_multiple_of(2) && view.shown_cols().is_multiple_of(2),
            "the slice must cut a wide glyph at both edges: {view:?}"
        );

        let mut live = RenderCompositor::new(
            cols,
            rows,
            Vec::new(),
            CompositorConfig {
                status_bar_height: 1,
                border_colors: colors,
                ..CompositorConfig::default()
            },
        );
        live.render_multi_pane(MultiPaneFrame {
            layout: &layout,
            zoom: None,
            focused: big,
            vts: &vts,
            chrome: None,
            status_bar: None,
            geometry: Some(&geometry),
        })
        .unwrap();
        let mut screen = VirtualTerminal::new(rows.into(), cols.into());
        screen.process(live.inner());

        let grids = HashMap::from([
            (big, (big_vt.grid(), big_vt.cursor())),
            (small, (small_vt.grid(), small_vt.cursor())),
        ]);
        let composed = compose(
            &ComposeInputs {
                layout: &layout,
                zoom: None,
                focused: big,
                panes: &grids,
                chrome: None,
                status_bar: None,
                geometry: Some(&geometry),
            },
            cols,
            rows,
            BorderStyle::Rounded,
            colors,
            1,
        );

        let surplus = Color::Rgb(1, 2, 3);
        let mut surplus_cells = 0;
        for y in 0..usize::from(rows - 1) {
            for x in 0..usize::from(cols) {
                let a = &screen.grid().visible_row(y)[x];
                let b = &composed.grid.visible_row(y)[x];
                let at = format!("({x},{y})");
                assert_eq!((a.ch, a.width), (b.ch, b.width), "{at}");
                assert_eq!(a.style, b.style, "{at}");
                surplus_cells += usize::from(b.style.bg == surplus);
            }
        }
        let small_rect = geometry.views[&small].rect;
        let expected = usize::from(small_rect.width) * usize::from(small_rect.height) - 4 * 6;
        assert_eq!(surplus_cells, expected, "the small pane's surplus, in both");

        // Content and cursor land where the viewport says, cut glyphs blanked.
        let rect = view.rect;
        let src = big_vt.grid();
        for r in 0..view.shown_rows() {
            let row = composed.grid.visible_row(usize::from(rect.y) + r);
            for c in 0..view.shown_cols() {
                let want =
                    view.shown_cell(src.visible_row(view.row_offset + r), view.col_offset + c);
                let got = &row[usize::from(rect.x) + c];
                assert_eq!((got.ch, got.style), (want.ch, want.style), "cell ({c},{r})");
            }
            assert_ne!(
                row[usize::from(rect.x)].width,
                0,
                "row {r} starts on half a glyph"
            );
            let last = &row[usize::from(rect.x) + view.shown_cols() - 1];
            assert_ne!(last.width, 2, "row {r} ends on half a glyph");
        }
        let cursor = view
            .cursor_cell(big_vt.cursor())
            .expect("the cursor is shown");
        assert_eq!(
            cursor,
            (
                rect.x + (cursor_col - view.col_offset) as u16,
                rect.y + 15 - view.row_offset as u16
            )
        );
        assert_eq!(
            composed.cursor,
            Some((usize::from(cursor.1), usize::from(cursor.0)))
        );
        assert_eq!(
            (screen.cursor().col, screen.cursor().row),
            (usize::from(cursor.0), usize::from(cursor.1)),
            "the attach cursor"
        );
    }
}
