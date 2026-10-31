//! A forwarded mouse event names the grid cell the attached client sees under
//! the pointer: for every screen point, `pane_grid_cell` over the view the
//! compositor recorded agrees with what it drew there, a point it rejects
//! shows no grid cell, and pinning lands on the nearest shown cell.
#![no_main]
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use shux_core::layout::{Direction, LayoutNode, Rect, ZoomState};
use shux_core::model::PaneId;
use shux_ui::BorderStyle;
use shux_ui::compositor::{CompositorConfig, MultiPaneFrame, RenderCompositor, pane_viewport};
use shux_ui::viewport::pane_grid_cell;
use shux_vt::VirtualTerminal;
use std::collections::HashMap;

#[derive(Debug, Arbitrary)]
struct Case {
    width: u16,
    height: u8,
    rows: u16,
    cols: u16,
    cursor: u16,
    scrollback: u8,
    style: u8,
    layout: u8,
    ratio: u8,
    zoomed: bool,
    focus_other: bool,
}

/// Distinct for any row error under 94 lines.
fn sym(line: usize, c: usize) -> char {
    char::from(33 + ((line * 31 + c) % 94) as u8)
}

fuzz_target!(|case: Case| {
    let w = case.width % 300 + 2;
    let h = u16::from(case.height % 120) + 2;
    let rows = usize::from(case.rows % 300) + 1;
    let cols = usize::from(case.cols % 300) + 1;
    let cursor = usize::from(case.cursor) % rows;
    let extra = usize::from(case.scrollback % 8);
    let style = [BorderStyle::Rounded, BorderStyle::None][usize::from(case.style % 2)];
    let (other, pane) = (PaneId::new(), PaneId::new());
    let ratio = 0.1 + f32::from(case.ratio) / 255.0 * 0.8;
    let layout = match case.layout % 3 {
        0 => LayoutNode::leaf(pane),
        1 => LayoutNode::split(Direction::Vertical, ratio, LayoutNode::leaf(other), LayoutNode::leaf(pane)),
        _ => LayoutNode::split(Direction::Horizontal, ratio, LayoutNode::leaf(pane), LayoutNode::leaf(other)),
    };
    let zoom = case.zoomed.then(|| ZoomState {
        saved_layout: layout.clone(),
        zoomed_pane: pane,
    });

    // Line `extra + r` lands on grid row r; the first `extra` scroll off.
    let mut vt = VirtualTerminal::new(rows, cols);
    let lines: Vec<String> = (0..rows + extra)
        .map(|l| (0..cols).map(|c| sym(l, c)).collect())
        .collect();
    vt.process(lines.join("\r\n").as_bytes());
    vt.process(format!("\x1b[{};1H", cursor + 1).as_bytes());
    let filler = VirtualTerminal::new(2, 2);
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
        layout: &layout,
        zoom: zoom.as_ref(),
        focused: if case.focus_other { other } else { pane },
        vts: &vts,
        chrome: None,
        status_bar: None,
    })
    .expect("render to a Vec cannot fail");
    let mut screen = VirtualTerminal::new(h.into(), w.into());
    screen.process(comp.inner());
    let seen = |x: u16, y: u16| screen.grid().visible_row(y.into())[usize::from(x)].ch;

    let view = comp.pane_views()[&pane];
    // The rect the attach loop hit-tests against (`pane_rect_in`).
    let content = Rect::new(0, 0, w, h - 1);
    let rect = if zoom.is_some() {
        content
    } else {
        layout
            .compute_rects(pane_viewport(content, style, false))
            .into_iter()
            .find_map(|(pid, r)| (pid == pane).then_some(r))
            .expect("the pane is in its layout")
    };
    assert_eq!(view.rect, rect, "the recorded view sits where attach hit-tests");
    let shown_rows = rows.saturating_sub(view.row_offset).min(rect.height.into());
    let shown_cols = cols.min(rect.width.into());

    for y in 0..h {
        for x in 0..w {
            let inside =
                x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height;
            let exact = pane_grid_cell(&view, (x, y), false);
            match exact {
                Some((c, r)) => {
                    assert!(inside, "({x},{y}) outside {rect:?} named ({c},{r})");
                    assert_eq!(
                        seen(x, y),
                        sym(extra + usize::from(r) - 1, usize::from(c) - 1),
                        "({x},{y}) named ({c},{r})"
                    );
                }
                None if inside => {
                    assert_eq!(seen(x, y), ' ', "({x},{y}) shows a grid cell but maps to none")
                }
                None => {}
            }
            let Some((c, r)) = pane_grid_cell(&view, (x, y), true) else {
                assert!(shown_rows == 0 || shown_cols == 0, "clamp found no cell in {rect:?}");
                continue;
            };
            if let Some(e) = exact {
                assert_eq!((c, r), e);
            }
            let near = (
                x.clamp(rect.x, rect.x + shown_cols as u16 - 1),
                y.clamp(rect.y, rect.y + shown_rows as u16 - 1),
            );
            let at = (rect.x + c - 1, rect.y + (usize::from(r) - 1 - view.row_offset) as u16);
            assert_eq!(at, near, "({x},{y}) pinned to ({c},{r}), not the nearest shown cell");
            assert_eq!(seen(at.0, at.1), sym(extra + usize::from(r) - 1, usize::from(c) - 1));
        }
    }
});
