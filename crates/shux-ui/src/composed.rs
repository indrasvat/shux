//! Composed window frame: a typed `shux_vt::Grid` carrying all the visual
//! composition that the ANSI emitter draws — pane contents, borders, title
//! overlays, status bar — ready for either ANSI emission or rasterization.
//!
//! Lets `shux-raster` see the same picture as `shux attach` without owning
//! the layout / border / status-bar logic itself.

use std::collections::HashMap;

use shux_core::layout::{LayoutNode, Rect, ZoomState};
use shux_core::model::PaneId;
use shux_vt::{
    Cell, CellFlags, CellRect, CellStyle, Color, ComposedPlacement, Cursor, Grid, GridConfig,
};

use crate::borders::{BorderColors, BorderStyle, compute_borders};
use crate::buffer::RenderCell;
use crate::statusbar::StatusBar;
use crate::viewport::{FrameGeometry, FrameInputs, PaneViewport, PresentedPane};
use crate::vt_convert::crossterm_to_vt;

/// A fully composed window frame ready for ANSI emission or rasterization.
pub struct ComposedFrame {
    /// Composed cells as a `shux_vt::Grid` (no scrollback).
    pub grid: Grid,
    /// Pictures, resolved into this frame and each carrying its pane as a clip.
    pub placements: Vec<ComposedPlacement>,
    /// Focused-pane cursor in `(row, col)` grid coordinates, or `None`
    /// when the focused pane's VT has the cursor hidden / out of bounds.
    pub cursor: Option<(usize, usize)>,
    /// The panes' rects and the status bar's, for
    /// `Rasterizer::render_regions`.
    pub regions: Vec<CellRect>,
    pub cols: u16,
    pub rows: u16,
}

/// Inputs to `compose`. Decoupled from `MultiPaneFrame` so a snapshot
/// caller can supply cloned grids + cursors without holding live VT refs.
pub struct ComposeInputs<'a> {
    pub layout: &'a LayoutNode,
    pub zoom: Option<&'a ZoomState>,
    pub focused: PaneId,
    /// Per-pane (grid, cursor) pairs.
    pub panes: &'a HashMap<PaneId, (&'a Grid, &'a Cursor)>,
    pub chrome: Option<&'a HashMap<PaneId, crate::chrome::PaneChrome>>,
    pub status_bar: Option<&'a StatusBar>,
    /// This frame's geometry, prepared for these panes at the size passed to
    /// [`compose`]; `None` prepares it there, now.
    pub geometry: Option<&'a FrameGeometry>,
}

/// Compose `inputs` into a `ComposedFrame` at the given outer dimensions.
/// Pure function — no I/O, no state.
pub fn compose(
    inputs: &ComposeInputs<'_>,
    cols: u16,
    rows: u16,
    border_style: BorderStyle,
    border_colors: BorderColors,
    status_bar_height: u16,
) -> ComposedFrame {
    let mut grid = Grid::new(
        rows as usize,
        cols as usize,
        GridConfig {
            max_scrollback: 0,
            ..GridConfig::default()
        },
    );

    // The live compositor's preparation, not a copy: snapshot/web-preview must
    // place and slice panes exactly where live attach does.
    let prepared;
    let geometry = match inputs.geometry {
        Some(geometry) => geometry,
        None => {
            prepared = crate::viewport::prepare_frame(&FrameInputs {
                layout: inputs.layout,
                zoom: inputs.zoom,
                cols,
                rows,
                status_bar_height,
                border_style,
                panes: &inputs
                    .panes
                    .iter()
                    .map(|(p, (g, c))| (*p, PresentedPane::of(g, c)))
                    .collect(),
                now_ns: shux_vt::monotonic_now_ns(),
            });
            &prepared
        }
    };
    let content = geometry.content;
    let borders_on = geometry.borders_on;
    let pane_rects = &geometry.rects;
    let surplus = crossterm_to_vt(border_colors.surplus);

    let mut placements: Vec<ComposedPlacement> = Vec::new();
    for (pid, rect) in pane_rects {
        if let (Some((src_grid, _)), Some(view)) = (inputs.panes.get(pid), geometry.views.get(pid))
        {
            compose_pane(&mut grid, view, src_grid, surplus);
            placements.extend(resolve_placements(view, src_grid));
        } else {
            compose_placeholder(&mut grid, *rect, "(no output)");
        }
    }

    if borders_on {
        let segments = compute_borders(pane_rects, inputs.focused, content, border_style);
        for seg in &segments {
            let fg = if seg.focused {
                crossterm_to_vt(border_colors.focused)
            } else {
                crossterm_to_vt(border_colors.unfocused)
            };
            put_cell(
                &mut grid,
                seg.x,
                seg.y,
                seg.ch,
                fg,
                Color::Default,
                CellFlags::default(),
            );
        }

        if let Some(chrome) = inputs.chrome {
            for (pid, rect) in pane_rects {
                let Some(pane_chrome) = chrome.get(pid) else {
                    continue;
                };
                let border = if *pid == inputs.focused {
                    crossterm_to_vt(border_colors.focused)
                } else {
                    crossterm_to_vt(border_colors.unfocused)
                };
                // Overlay on the pane's top border row, NOT its first content row.
                let y = rect.y.saturating_sub(1);
                for glyph in crate::chrome::title_overlay(pane_chrome, *rect, &segments) {
                    let fg = glyph.color.map(crossterm_to_vt).unwrap_or(border);
                    put_glyph(&mut grid, glyph.x, y, &glyph, fg);
                }
            }
        }
    }

    let mut regions: Vec<CellRect> = pane_rects.iter().map(|(_, r)| cell_rect(*r)).collect();
    if let Some(bar) = inputs.status_bar {
        let bar_top = rows.saturating_sub(status_bar_height);
        regions.push(cell_rect(Rect::new(0, bar_top, cols, status_bar_height)));
        for row_offset in 0..status_bar_height {
            let row = bar_top + row_offset;
            if row_offset + 1 == status_bar_height {
                let cells = bar.render_row(cols);
                for (col, rcell) in cells.iter().enumerate() {
                    let (fg, bg, flags) = render_attrs_to_vt(rcell);
                    put_cell(&mut grid, col as u16, row, rcell.ch, fg, bg, flags);
                    let mut vt_row = grid.visible_row_mut(row as usize);
                    if rcell.wide_continuation {
                        vt_row[col].width = 0;
                    } else {
                        if cells.get(col + 1).is_some_and(|c| c.wide_continuation) {
                            vt_row[col].width = 2;
                        }
                        vt_row[col].extended = rcell.extended.clone();
                    }
                }
            } else {
                for col in 0..cols {
                    put_cell(
                        &mut grid,
                        col,
                        row,
                        ' ',
                        Color::Default,
                        Color::Default,
                        CellFlags::default(),
                    );
                }
            }
        }
    }

    let cursor = inputs
        .panes
        .get(&inputs.focused)
        .zip(geometry.views.get(&inputs.focused))
        .and_then(|((_, cur), view)| view.cursor_cell(cur))
        .filter(|&(x, y)| x < cols && y < rows)
        .map(|(x, y)| (usize::from(y), usize::from(x)));

    ComposedFrame {
        grid,
        placements,
        cursor,
        regions,
        cols,
        rows,
    }
}

fn cell_rect(r: Rect) -> CellRect {
    CellRect {
        col: r.x.into(),
        row: r.y.into(),
        cols: r.width.into(),
        rows: r.height.into(),
    }
}

/// Rebase one pane's placements into the frame that shows it through `view`,
/// clipped to the cells it shows. A picture whose origin lies left of the
/// slice is omitted: placements cannot start at a negative column.
pub(crate) fn resolve_placements(view: &PaneViewport, src: &Grid) -> Vec<ComposedPlacement> {
    let rect = view.rect;
    let clip = CellRect {
        col: rect.x as usize,
        row: rect.y as usize,
        cols: view.shown_cols(),
        rows: view.shown_rows(),
    };
    src.placements()
        .iter()
        .filter_map(|p| {
            Some(ComposedPlacement {
                image: p.image.clone(),
                // `viewport_row` undoes the source grid's own base, the offset
                // undoes the slice, and `rect.y` rebases into this frame.
                row: p.viewport_row(src) - view.row_offset as i64 + i64::from(rect.y),
                col: p.col.checked_sub(view.col_offset)? + rect.x as usize,
                clip,
                geometry: p.geometry,
            })
        })
        .collect()
}

fn compose_pane(grid: &mut Grid, view: &PaneViewport, src: &Grid, surplus: Color) {
    let rect = view.rect;
    let (shown_rows, shown_cols) = (view.shown_rows(), view.shown_cols());
    for r in 0..usize::from(rect.height) {
        let dst_row_idx = rect.y as usize + r;
        if dst_row_idx >= grid.rows() {
            break;
        }
        let cells: Vec<Cell> = (0..usize::from(rect.width))
            .map(|c| {
                if r < shown_rows && c < shown_cols {
                    let row = src.visible_row(view.row_offset + r);
                    view.shown_cell(row, view.col_offset + c).into_owned()
                } else {
                    Cell {
                        style: CellStyle {
                            bg: surplus,
                            ..CellStyle::default()
                        },
                        ..Cell::default()
                    }
                }
            })
            .collect();
        let mut dst_row = grid.visible_row_mut(dst_row_idx);
        for (c, cell) in cells.into_iter().enumerate() {
            let dst_col = rect.x as usize + c;
            if dst_col >= dst_row.len() {
                break;
            }
            dst_row[dst_col] = cell;
        }
    }
}

fn compose_placeholder(grid: &mut Grid, rect: Rect, text: &str) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let chars: Vec<char> = text.chars().collect();
    let col_start = rect
        .x
        .saturating_add((((rect.width as usize).saturating_sub(chars.len())) as u16) / 2);
    let row = rect.y + rect.height / 2;
    let mut flags = CellFlags::default();
    flags.set(CellFlags::DIM);
    for (i, ch) in chars.iter().enumerate() {
        put_cell(
            grid,
            col_start + i as u16,
            row,
            *ch,
            Color::Default,
            Color::Default,
            flags,
        );
    }
}

fn put_cell(grid: &mut Grid, x: u16, y: u16, ch: char, fg: Color, bg: Color, flags: CellFlags) {
    if y as usize >= grid.rows() {
        return;
    }
    let mut row = grid.visible_row_mut(y as usize);
    if (x as usize) >= row.len() {
        return;
    }
    row[x as usize] = Cell {
        ch,
        width: 1,
        style: CellStyle { fg, bg, flags },
        extended: None,
    };
}

/// A title glyph as the VT stores text: multi-scalar graphemes in the cell's
/// extended attributes, a wide glyph followed by its continuation.
fn put_glyph(grid: &mut Grid, x: u16, y: u16, glyph: &crate::chrome::TitleGlyph, fg: Color) {
    let Some(ch) = glyph.text.chars().next() else {
        return;
    };
    put_cell(grid, x, y, ch, fg, Color::Default, CellFlags::default());
    if y as usize >= grid.rows() {
        return;
    }
    let mut row = grid.visible_row_mut(y as usize);
    let x = x as usize;
    if x >= row.len() {
        return;
    }
    row[x].width = glyph.width;
    row[x].set_grapheme_payload(glyph.text.clone());
    if glyph.width == 2 && x + 1 < row.len() {
        row[x + 1] = Cell::wide_continuation();
    }
}

fn render_attrs_to_vt(rcell: &RenderCell) -> (Color, Color, CellFlags) {
    let fg = rcell.fg.map(crossterm_to_vt).unwrap_or(Color::Default);
    let bg = rcell.bg.map(crossterm_to_vt).unwrap_or(Color::Default);
    let mut flags = CellFlags::default();
    if rcell.attrs.bold {
        flags.set(CellFlags::BOLD);
    }
    if rcell.attrs.dim {
        flags.set(CellFlags::DIM);
    }
    if rcell.attrs.italic {
        flags.set(CellFlags::ITALIC);
    }
    if rcell.attrs.underline {
        flags.set(CellFlags::UNDERLINE);
    }
    if rcell.attrs.blink {
        flags.set(CellFlags::BLINK);
    }
    if rcell.attrs.reverse {
        flags.set(CellFlags::INVERSE);
    }
    if rcell.attrs.hidden {
        flags.set(CellFlags::HIDDEN);
    }
    if rcell.attrs.strikethrough {
        flags.set(CellFlags::STRIKETHROUGH);
    }
    (fg, bg, flags)
}

#[cfg(test)]
mod tests {
    use shux_vt::VirtualTerminal;

    use super::*;
    use crate::statusbar::{StatusBar, StatusSegment};

    fn single_pane_layout(pane_id: PaneId) -> LayoutNode {
        LayoutNode::Leaf { pane: pane_id }
    }

    fn pane_map(pid: PaneId, vt: &VirtualTerminal) -> HashMap<PaneId, (&Grid, &Cursor)> {
        let mut m = HashMap::new();
        m.insert(pid, (vt.grid(), vt.cursor()));
        m
    }

    #[test]
    fn compose_writes_pane_cells_into_grid() {
        let pid = PaneId::new();
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"Hello");
        let layout = single_pane_layout(pid);
        let panes = pane_map(pid, &vt);
        let inputs = ComposeInputs {
            layout: &layout,
            zoom: None,
            focused: pid,
            panes: &panes,
            chrome: None,
            status_bar: None,
            geometry: None,
        };
        let composed = compose(
            &inputs,
            80,
            24,
            BorderStyle::None,
            BorderColors::default(),
            0,
        );
        assert_eq!(composed.cols, 80);
        assert_eq!(composed.rows, 24);
        let row = composed.grid.visible_row(0);
        let text: String = (0..5).map(|c| row[c].ch).collect();
        assert_eq!(text, "Hello");
    }

    /// A title stops before a junction where another pane's border meets its
    /// top border, so the divider above stays joined — at any split ratio,
    /// including a junction on the title's first column.
    #[test]
    fn compose_keeps_a_border_junction_under_a_long_title() {
        let (a, b, below) = (PaneId::new(), PaneId::new(), PaneId::new());
        let vt = VirtualTerminal::new(10, 38);
        let panes: HashMap<PaneId, (&Grid, &Cursor)> = [a, b, below]
            .into_iter()
            .map(|p| (p, (vt.grid(), vt.cursor())))
            .collect();
        let chrome = HashMap::from([(below, crate::chrome::PaneChrome::titled("x".repeat(60)))]);
        for cols in [12u16, 24, 40] {
            for ratio in [0.05, 0.1, 0.3, 0.5, 0.9] {
                let layout = LayoutNode::Split {
                    dir: shux_core::layout::Direction::Horizontal,
                    ratio: 0.5,
                    a: Box::new(LayoutNode::Split {
                        dir: shux_core::layout::Direction::Vertical,
                        ratio,
                        a: Box::new(LayoutNode::Leaf { pane: a }),
                        b: Box::new(LayoutNode::Leaf { pane: b }),
                    }),
                    b: Box::new(LayoutNode::Leaf { pane: below }),
                };
                let inputs = ComposeInputs {
                    layout: &layout,
                    zoom: None,
                    focused: below,
                    panes: &panes,
                    chrome: Some(&chrome),
                    status_bar: None,
                    geometry: None,
                };
                let plain = ComposeInputs {
                    chrome: None,
                    ..inputs
                };
                let draw = |i: &ComposeInputs<'_>| {
                    compose(i, cols, 16, BorderStyle::Thin, BorderColors::default(), 0)
                };
                let (without, with) = (draw(&plain), draw(&inputs));
                for y in 0..16 {
                    for x in 0..usize::from(cols) {
                        let ch = without.grid.visible_row(y)[x].ch;
                        if "┬┴┼├┤".contains(ch) {
                            assert_eq!(
                                with.grid.visible_row(y)[x].ch,
                                ch,
                                "{cols} cols, ratio {ratio}: junction at ({x},{y}) overwritten"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn compose_draws_border_when_enabled() {
        let pid = PaneId::new();
        let vt = VirtualTerminal::new(22, 78);
        let layout = single_pane_layout(pid);
        let panes = pane_map(pid, &vt);
        let inputs = ComposeInputs {
            layout: &layout,
            zoom: None,
            focused: pid,
            panes: &panes,
            chrome: None,
            status_bar: None,
            geometry: None,
        };
        let composed = compose(
            &inputs,
            80,
            24,
            BorderStyle::Rounded,
            BorderColors::default(),
            0,
        );
        assert_eq!(composed.grid.visible_row(0)[0].ch, '╭');
        assert_eq!(composed.grid.visible_row(0)[79].ch, '╮');
        assert_eq!(composed.grid.visible_row(23)[0].ch, '╰');
        assert_eq!(composed.grid.visible_row(23)[79].ch, '╯');
    }

    #[test]
    fn compose_hides_cursor_when_vt_marks_invisible() {
        let pid = PaneId::new();
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[?25l");
        let layout = single_pane_layout(pid);
        let panes = pane_map(pid, &vt);
        let inputs = ComposeInputs {
            layout: &layout,
            zoom: None,
            focused: pid,
            panes: &panes,
            chrome: None,
            status_bar: None,
            geometry: None,
        };
        let composed = compose(
            &inputs,
            80,
            24,
            BorderStyle::None,
            BorderColors::default(),
            0,
        );
        assert!(composed.cursor.is_none());
    }

    #[test]
    fn compose_two_pane_split_places_separator_and_overlays_titles_on_border() {
        // 2-pane vertical split: left and right halves with their own
        // borders, an inter-pane vertical separator, and per-pane titles
        // overlaid on the top border row (NOT the first content row).
        let left_pid = PaneId::new();
        let right_pid = PaneId::new();
        let mut left_vt = VirtualTerminal::new(28, 58);
        left_vt.process(b"LEFT");
        let mut right_vt = VirtualTerminal::new(28, 58);
        right_vt.process(b"RIGHT");

        let layout = LayoutNode::Split {
            dir: shux_core::layout::Direction::Vertical,
            ratio: 0.5,
            a: Box::new(LayoutNode::Leaf { pane: left_pid }),
            b: Box::new(LayoutNode::Leaf { pane: right_pid }),
        };

        let mut panes: HashMap<PaneId, (&Grid, &Cursor)> = HashMap::new();
        panes.insert(left_pid, (left_vt.grid(), left_vt.cursor()));
        panes.insert(right_pid, (right_vt.grid(), right_vt.cursor()));

        let mut titles = HashMap::new();
        titles.insert(left_pid, crate::chrome::PaneChrome::titled("lhs"));
        titles.insert(right_pid, crate::chrome::PaneChrome::titled("rhs"));

        let inputs = ComposeInputs {
            layout: &layout,
            zoom: None,
            focused: left_pid,
            panes: &panes,
            chrome: Some(&titles),
            status_bar: None,
            geometry: None,
        };
        let composed = compose(
            &inputs,
            120,
            30,
            BorderStyle::Rounded,
            BorderColors::default(),
            0,
        );

        // Top border row has rounded corners + horizontal rule + titles.
        let row0 = composed.grid.visible_row(0);
        assert_eq!(row0[0].ch, '╭', "top-left corner");
        assert_eq!(row0[119].ch, '╮', "top-right corner");

        // Both titles surface on the border row, NOT the content row.
        let row0_text: String = (0..120).map(|c| row0[c].ch).collect();
        assert!(
            row0_text.contains(" lhs "),
            "left title overlays border: {row0_text:?}"
        );
        assert!(
            row0_text.contains(" rhs "),
            "right title overlays border: {row0_text:?}"
        );

        // First content row (row 1) carries "LEFT" + "RIGHT" — neither
        // gets overdrawn by the title overlay anymore.
        let row1 = composed.grid.visible_row(1);
        let row1_left: String = (1..5).map(|c| row1[c].ch).collect();
        assert_eq!(row1_left, "LEFT");

        // The vertical separator between panes lives at column = left_width.
        // For a 0.5 split of a 118-wide pane_viewport (after 1-cell inset
        // on each side), the separator falls somewhere in the middle. We
        // just check that a `│` exists somewhere in row 14 (mid-height).
        let mid_row = composed.grid.visible_row(14);
        let mid_text: String = (0..120).map(|c| mid_row[c].ch).collect();
        assert!(
            mid_text.contains('│'),
            "vertical separator present in mid-row: {mid_text:?}"
        );
    }

    #[test]
    fn compose_status_bar_lands_in_last_row() {
        let pid = PaneId::new();
        let vt = VirtualTerminal::new(23, 80);
        let layout = single_pane_layout(pid);
        let panes = pane_map(pid, &vt);
        let mut bar = StatusBar::new();
        bar.left = vec![StatusSegment::plain("hello")];
        let inputs = ComposeInputs {
            layout: &layout,
            zoom: None,
            focused: pid,
            panes: &panes,
            chrome: None,
            status_bar: Some(&bar),
            geometry: None,
        };
        let composed = compose(
            &inputs,
            80,
            24,
            BorderStyle::None,
            BorderColors::default(),
            1,
        );
        let bottom = composed.grid.visible_row(23);
        let text: String = (0..5).map(|c| bottom[c].ch).collect();
        assert_eq!(text, "hello");
    }

    // ── issue #108: pane grid taller than the layout rect ────────────────

    /// A pane whose grid is TALLER than the window layout rect, with its
    /// content at the TOP, must render that content in `window snapshot` —
    /// clipped to the top-left region, never blank.
    ///
    /// Red before the fix: `compose_pane` bottom-anchored the grid
    /// (`row_offset = total_rows - visible_rows`), so the top content scrolled
    /// off and the content area came back blank while the top-clamped cursor
    /// still painted — exactly the reported "blank with a cursor" frame.
    #[test]
    fn compose_oversized_pane_top_content_is_not_blank() {
        let pid = PaneId::new();
        // Grid deliberately larger than any 120x36 window layout rect.
        let mut vt = VirtualTerminal::new(60, 200);
        vt.process(b"OVERSIZE\r\nrow2\r\n"); // cursor ends at row 2, col 0
        let layout = single_pane_layout(pid);
        let panes = pane_map(pid, &vt);
        let inputs = ComposeInputs {
            layout: &layout,
            zoom: None,
            focused: pid,
            panes: &panes,
            chrome: None,
            status_bar: None,
            geometry: None,
        };
        // Rounded border + 1 status row → pane viewport is (1,1,118,33): the
        // 60-row grid overflows its 33-row rect.
        let composed = compose(
            &inputs,
            120,
            36,
            BorderStyle::Rounded,
            BorderColors::default(),
            1,
        );

        // First content row (inside the top border) must carry the marker.
        let row1: String = (1..9).map(|c| composed.grid.visible_row(1)[c].ch).collect();
        assert_eq!(row1, "OVERSIZE", "top content dropped from window compose");
        let row2: String = (1..5).map(|c| composed.grid.visible_row(2)[c].ch).collect();
        assert_eq!(row2, "row2", "second content row dropped");

        // Cursor (VT row 2) maps into the same top-anchored region, not a
        // clamped position disconnected from the content.
        assert_eq!(
            composed.cursor,
            Some((3, 1)),
            "cursor must land on the row that shows VT row 2"
        );
    }

    /// When the cursor sits near the BOTTOM of an oversized grid (a shell
    /// prompt), the viewport follows it so the most-recent output stays
    /// visible. Guards that the fix does not regress the documented
    /// "resize-down shows recent output" behavior.
    #[test]
    fn compose_oversized_pane_cursor_at_bottom_keeps_recent_output() {
        let pid = PaneId::new();
        let mut vt = VirtualTerminal::new(60, 200);
        // Jump to the last grid row and write there; cursor ends at row 59.
        vt.process(b"\x1b[60;1HBOTTOMLINE");
        let layout = single_pane_layout(pid);
        let panes = pane_map(pid, &vt);
        let inputs = ComposeInputs {
            layout: &layout,
            zoom: None,
            focused: pid,
            panes: &panes,
            chrome: None,
            status_bar: None,
            geometry: None,
        };
        let composed = compose(
            &inputs,
            120,
            36,
            BorderStyle::Rounded,
            BorderColors::default(),
            1,
        );

        // With a 33-row viewport and the cursor on VT row 59, the offset is
        // 27, so VT row 59 lands on the last content row (dst row 33).
        let row33: String = (1..11)
            .map(|c| composed.grid.visible_row(33)[c].ch)
            .collect();
        assert_eq!(
            row33, "BOTTOMLINE",
            "recent output at the bottom must survive"
        );
        assert_eq!(
            composed.cursor,
            Some((33, 11)),
            "cursor must remain on the visible last row"
        );
    }

    /// The `pane split` case is the same defect: splitting shrinks the layout
    /// rect below the (unchanged) oversized pane grid. An oversized child of a
    /// split must still show its top content.
    #[test]
    fn compose_split_with_oversized_child_shows_content() {
        let left = PaneId::new();
        let right = PaneId::new();
        let mut left_vt = VirtualTerminal::new(60, 200);
        left_vt.process(b"SPLITMARK\r\n"); // cursor row 1, top content on row 0
        let right_vt = VirtualTerminal::new(10, 10);

        let layout = LayoutNode::Split {
            dir: shux_core::layout::Direction::Vertical,
            ratio: 0.5,
            a: Box::new(LayoutNode::Leaf { pane: left }),
            b: Box::new(LayoutNode::Leaf { pane: right }),
        };
        let mut panes: HashMap<PaneId, (&Grid, &Cursor)> = HashMap::new();
        panes.insert(left, (left_vt.grid(), left_vt.cursor()));
        panes.insert(right, (right_vt.grid(), right_vt.cursor()));

        let inputs = ComposeInputs {
            layout: &layout,
            zoom: None,
            focused: left,
            panes: &panes,
            chrome: None,
            status_bar: None,
            geometry: None,
        };
        let composed = compose(
            &inputs,
            120,
            36,
            BorderStyle::Rounded,
            BorderColors::default(),
            1,
        );

        // The left pane's top content sits at its content origin (col 1, row 1).
        let row1: String = (1..10)
            .map(|c| composed.grid.visible_row(1)[c].ch)
            .collect();
        assert_eq!(row1, "SPLITMARK", "oversized split child rendered blank");
    }
}
