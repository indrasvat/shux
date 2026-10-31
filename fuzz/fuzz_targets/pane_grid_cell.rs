//! A forwarded mouse event names the grid cell the attached client sees under
//! the pointer: for every screen point, `pane_grid_cell` agrees with what the
//! live compositor drew there, and a point it rejects shows no grid cell.
#![no_main]
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use shux_core::layout::{Direction, LayoutNode, Rect, ZoomState};
use shux_core::model::PaneId;
use shux_ui::BorderStyle;
use shux_ui::compositor::{CompositorConfig, MultiPaneFrame, RenderCompositor, pane_viewport};
use shux_ui::viewport::{pane_grid_cell, pane_view_row_offset};
use shux_vt::VirtualTerminal;
use std::collections::HashMap;

#[derive(Debug, Arbitrary)]
struct Case {
    width: u8,
    height: u8,
    rows: u8,
    cols: u8,
    cursor: u8,
    style: u8,
    layout: u8,
    ratio: u8,
    zoomed: bool,
}

fn sym(r: usize, c: usize) -> char {
    char::from(33 + ((r * 31 + c) % 94) as u8)
}

fuzz_target!(|case: Case| {
    let w = u16::from(case.width % 60) + 2;
    let h = u16::from(case.height % 40) + 2;
    let rows = usize::from(case.rows % 80) + 1;
    let cols = usize::from(case.cols % 80) + 1;
    let cursor = usize::from(case.cursor) % rows;
    let style = [
        BorderStyle::Rounded,
        BorderStyle::Thin,
        BorderStyle::Ascii,
        BorderStyle::None,
    ][usize::from(case.style % 4)];
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

    let mut vt = VirtualTerminal::new(rows, cols);
    for r in 0..rows {
        let line: String = (0..cols).map(|c| sym(r, c)).collect();
        vt.process(format!("\x1b[{};1H{line}", r + 1).as_bytes());
    }
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
        focused: pane,
        vts: &vts,
        chrome: None,
        status_bar: None,
    })
    .expect("render to a Vec cannot fail");
    let mut screen = VirtualTerminal::new(h.into(), w.into());
    screen.process(comp.inner());
    let seen = |x: u16, y: u16| screen.grid().visible_row(y.into())[usize::from(x)].ch;

    // The rect the attach loop hit-tests against (`pane_rect_in`).
    let content = Rect::new(0, 0, w, h - 1);
    let rect = if zoom.is_some() {
        Some(content)
    } else {
        layout
            .compute_rects(pane_viewport(content, style, false))
            .into_iter()
            .find_map(|(pid, r)| (pid == pane).then_some(r))
    };
    let Some(rect) = rect else { return };
    let offset = pane_view_row_offset(rows, rect.height.into(), cursor);

    for y in 0..h {
        for x in 0..w {
            let inside =
                x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height;
            let exact = pane_grid_cell(rect, &vt, (x, y), false);
            match exact {
                Some((c, r)) => {
                    assert!(inside, "({x},{y}) outside {rect:?} named ({c},{r})");
                    assert!(usize::from(c) <= cols && usize::from(r) <= rows);
                    assert_eq!(
                        seen(x, y),
                        sym(usize::from(r) - 1, usize::from(c) - 1),
                        "({x},{y}) named ({c},{r})"
                    );
                }
                None if inside => {
                    assert_eq!(seen(x, y), ' ', "({x},{y}) shows a grid cell but maps to none")
                }
                None => {}
            }
            if rect.width == 0 || rect.height == 0 {
                continue;
            }
            let (c, r) = pane_grid_cell(rect, &vt, (x, y), true).expect("clamp always finds a cell");
            if let Some(e) = exact {
                assert_eq!((c, r), e);
            }
            let (c, r) = (c - 1, usize::from(r) - 1);
            let at = (rect.x + c, rect.y + u16::try_from(r - offset).expect("shown row"));
            assert_eq!(seen(at.0, at.1), sym(r, c.into()), "pinned to a cell not shown");
        }
    }
});
