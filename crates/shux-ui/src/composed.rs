//! Composed window frame: a typed `shux_vt::Grid` carrying all the visual
//! composition that the ANSI emitter draws — pane contents, borders, title
//! overlays, status bar — ready for either ANSI emission or rasterization.
//!
//! Lets `shux-raster` see the same picture as `shux attach` without owning
//! the layout / border / status-bar logic itself.

use std::collections::HashMap;

use shux_core::layout::{LayoutNode, Rect, ZoomState};
use shux_core::model::PaneId;
use shux_vt::{Cell, CellFlags, CellStyle, Color, Cursor, Grid, GridConfig};

use crate::borders::{BorderColors, BorderStyle, compute_borders};
use crate::buffer::RenderCell;
use crate::statusbar::StatusBar;
use crate::vt_convert::crossterm_to_vt;

/// A fully composed window frame ready for ANSI emission or rasterization.
pub struct ComposedFrame {
    /// Composed cells as a `shux_vt::Grid` (no scrollback).
    pub grid: Grid,
    /// Focused-pane cursor in `(row, col)` grid coordinates, or `None`
    /// when the focused pane's VT has the cursor hidden / out of bounds.
    pub cursor: Option<(usize, usize)>,
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
    pub titles: Option<&'a HashMap<PaneId, String>>,
    pub status_bar: Option<&'a StatusBar>,
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

    let content_height = rows.saturating_sub(status_bar_height);
    let content = Rect::new(0, 0, cols, content_height);

    let zoomed = inputs.zoom.is_some();
    let borders_on =
        !zoomed && border_style != BorderStyle::None && content.width >= 3 && content.height >= 3;
    let pane_viewport = if borders_on {
        Rect::new(
            content.x + 1,
            content.y + 1,
            content.width - 2,
            content.height - 2,
        )
    } else {
        content
    };

    let pane_rects: Vec<(PaneId, Rect)> = if let Some(zoom) = inputs.zoom {
        vec![(zoom.zoomed_pane, content)]
    } else {
        inputs.layout.compute_rects(pane_viewport)
    };

    for (pid, rect) in &pane_rects {
        if let Some((src_grid, _)) = inputs.panes.get(pid) {
            compose_pane(&mut grid, *rect, src_grid);
        } else {
            compose_placeholder(&mut grid, *rect, "(no output)");
        }
    }

    if borders_on {
        let segments = compute_borders(&pane_rects, inputs.focused, content, border_style);
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

        if let Some(titles) = inputs.titles {
            for (pid, rect) in &pane_rects {
                let title = titles.get(pid).map(|s| s.as_str()).unwrap_or("");
                if title.is_empty() || rect.width < 6 {
                    continue;
                }
                let fg = if *pid == inputs.focused {
                    crossterm_to_vt(border_colors.focused)
                } else {
                    crossterm_to_vt(border_colors.unfocused)
                };
                let max_chars = rect.width.saturating_sub(4) as usize;
                let truncated: String = title.chars().take(max_chars).collect();
                let label = format!(" {truncated} ");
                let mut x = rect.x.saturating_add(1);
                // Overlay on the pane's top border row, NOT its first content row.
                let y = rect.y.saturating_sub(1);
                for ch in label.chars() {
                    if x >= rect.x.saturating_add(rect.width).saturating_sub(1) {
                        break;
                    }
                    put_cell(
                        &mut grid,
                        x,
                        y,
                        ch,
                        fg,
                        Color::Default,
                        CellFlags::default(),
                    );
                    x = x.saturating_add(1);
                }
            }
        }
    }

    if let Some(bar) = inputs.status_bar {
        let bar_top = rows.saturating_sub(status_bar_height);
        for row_offset in 0..status_bar_height {
            let row = bar_top + row_offset;
            if row_offset + 1 == status_bar_height {
                let cells = bar.render_row(cols);
                for (col, rcell) in cells.into_iter().enumerate() {
                    let (fg, bg, flags) = render_attrs_to_vt(&rcell);
                    put_cell(&mut grid, col as u16, row, rcell.ch, fg, bg, flags);
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

    let cursor = pane_rects
        .iter()
        .find(|(id, _)| *id == inputs.focused)
        .and_then(|(_, rect)| {
            let (_, cur) = inputs.panes.get(&inputs.focused)?;
            if !cur.visible {
                return None;
            }
            let sx = rect
                .x
                .saturating_add((cur.col as u16).min(rect.width.saturating_sub(1)));
            let sy = rect
                .y
                .saturating_add((cur.row as u16).min(rect.height.saturating_sub(1)));
            (sx < rect.x.saturating_add(rect.width)
                && sy < rect.y.saturating_add(rect.height)
                && sx < cols
                && sy < rows)
                .then_some((sy as usize, sx as usize))
        });

    ComposedFrame {
        grid,
        cursor,
        cols,
        rows,
    }
}

fn compose_pane(grid: &mut Grid, rect: Rect, src: &Grid) {
    let total_rows = src.rows();
    let total_cols = src.cols();
    let visible_rows = rect.height as usize;
    let visible_cols = rect.width as usize;
    let row_offset = total_rows.saturating_sub(visible_rows);

    for r in 0..visible_rows {
        let src_row_idx = row_offset + r;
        if src_row_idx >= total_rows {
            continue;
        }
        let dst_row_idx = rect.y as usize + r;
        if dst_row_idx >= grid.rows() {
            break;
        }
        let src_row_cells: Vec<Cell> = (0..visible_cols.min(total_cols))
            .map(|c| src.visible_row(src_row_idx)[c].clone())
            .collect();
        let mut dst_row = grid.visible_row_mut(dst_row_idx);
        for (c, cell) in src_row_cells.into_iter().enumerate() {
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
            titles: None,
            status_bar: None,
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
            titles: None,
            status_bar: None,
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
            titles: None,
            status_bar: None,
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

        let mut titles: HashMap<PaneId, String> = HashMap::new();
        titles.insert(left_pid, "lhs".into());
        titles.insert(right_pid, "rhs".into());

        let inputs = ComposeInputs {
            layout: &layout,
            zoom: None,
            focused: left_pid,
            panes: &panes,
            titles: Some(&titles),
            status_bar: None,
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
            titles: None,
            status_bar: Some(&bar),
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
}
