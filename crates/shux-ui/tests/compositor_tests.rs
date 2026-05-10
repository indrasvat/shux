//! Integration tests for multi-pane rendering.
//!
//! These tests assemble a layout + per-pane VTs and exercise the
//! `RenderCompositor::render_multi_pane` path end-to-end (compose → diff →
//! emit ANSI bytes). The output is captured into a `Cursor<Vec<u8>>` so we
//! can assert on the produced byte stream.

use std::collections::HashMap;
use std::io::Cursor;

use shux_core::layout::{Direction, LayoutNode, Rect, WindowLayout};
use shux_core::model::PaneId;
use shux_ui::{
    BorderStyle, CompositorConfig, MultiPaneFrame, RenderCompositor, StatusBar, StatusSegment,
};
use shux_vt::VirtualTerminal;
use uuid::Uuid;

fn pane(n: u128) -> PaneId {
    PaneId::from_uuid(Uuid::from_u128(n))
}

fn make_compositor(width: u16, height: u16) -> RenderCompositor<Cursor<Vec<u8>>> {
    let cfg = CompositorConfig {
        show_border: false,
        status_bar_height: 0,
        border_style: BorderStyle::Rounded,
        ..Default::default()
    };
    RenderCompositor::new(width, height, Cursor::new(Vec::new()), cfg)
}

#[test]
fn test_single_pane_renders_grid_content() {
    let p = pane(1);
    let layout = LayoutNode::leaf(p);
    let mut vt = VirtualTerminal::new(5, 10);
    vt.process(b"hello");

    let mut vts: HashMap<PaneId, &VirtualTerminal> = HashMap::new();
    vts.insert(p, &vt);

    let mut compositor = make_compositor(10, 5);
    let stats = compositor
        .render_multi_pane(MultiPaneFrame {
            layout: &layout,
            zoom: None,
            focused: p,
            vts: &vts,
            titles: None,
            status_bar: None,
        })
        .unwrap();

    // First render must be a full redraw — every cell is dirty.
    assert_eq!(stats.dirty_cells, 50);
    // Render time should comfortably beat the PRD 8ms budget.
    assert!(stats.total_time_us < 8000);
}

#[test]
fn test_two_panes_split_vertical_have_borders() {
    let a = pane(1);
    let b = pane(2);
    let mut wl = WindowLayout::new(a);
    wl.split_pane(a, b, Direction::Vertical, 0.5);

    let mut vt_a = VirtualTerminal::new(5, 10);
    vt_a.process(b"AAA");
    let mut vt_b = VirtualTerminal::new(5, 10);
    vt_b.process(b"BBB");

    let mut vts: HashMap<PaneId, &VirtualTerminal> = HashMap::new();
    vts.insert(a, &vt_a);
    vts.insert(b, &vt_b);

    let cfg = CompositorConfig {
        border_style: BorderStyle::Thin,
        ..Default::default()
    };
    let mut compositor = RenderCompositor::new(21, 5, Cursor::new(Vec::new()), cfg);
    let _stats = compositor
        .render_multi_pane(MultiPaneFrame {
            layout: &wl.tree,
            zoom: wl.zoom.as_ref(),
            focused: a,
            vts: &vts,
            titles: None,
            status_bar: None,
        })
        .unwrap();

    // The output should contain at least one box-drawing character.
    let cursor: &Cursor<Vec<u8>> = compositor.inner();
    let raw = String::from_utf8_lossy(cursor.get_ref());
    assert!(
        raw.contains('│') || raw.contains('─'),
        "expected box-drawing chars in output"
    );
}

#[test]
fn test_zoom_renders_only_zoomed_pane() {
    let a = pane(1);
    let b = pane(2);
    let mut wl = WindowLayout::new(a);
    wl.split_pane(a, b, Direction::Vertical, 0.5);
    wl.toggle_zoom(a);
    assert!(wl.is_zoomed());

    let mut vt_a = VirtualTerminal::new(5, 21);
    vt_a.process(b"ZZZZZZZZZ");
    let mut vt_b = VirtualTerminal::new(5, 21);
    vt_b.process(b"BBB");

    let mut vts: HashMap<PaneId, &VirtualTerminal> = HashMap::new();
    vts.insert(a, &vt_a);
    vts.insert(b, &vt_b);

    let mut compositor = make_compositor(21, 5);
    let _stats = compositor
        .render_multi_pane(MultiPaneFrame {
            layout: &wl.tree,
            zoom: wl.zoom.as_ref(),
            focused: a,
            vts: &vts,
            titles: None,
            status_bar: None,
        })
        .unwrap();

    // Zoomed pane fills the whole viewport: no border characters.
    let cursor: &Cursor<Vec<u8>> = compositor.inner();
    let raw = String::from_utf8_lossy(cursor.get_ref());
    assert!(!raw.contains('│'), "zoom should suppress vertical borders");
    // The compositor emits ANSI escapes between each cell, so we count
    // 'Z' occurrences instead of looking for a substring.
    let z_count = raw.chars().filter(|c| *c == 'Z').count();
    assert!(z_count >= 9, "expected ≥9 Z chars in output, got {z_count}");
}

#[test]
fn test_status_bar_renders_at_bottom_row() {
    let p = pane(1);
    let layout = LayoutNode::leaf(p);
    let vt = VirtualTerminal::new(4, 20);
    let mut vts: HashMap<PaneId, &VirtualTerminal> = HashMap::new();
    vts.insert(p, &vt);

    let mut bar = StatusBar::new();
    bar.left.push(StatusSegment::plain("[shux]"));
    bar.right.push(StatusSegment::plain("12:00"));

    let cfg = CompositorConfig {
        status_bar_height: 1,
        border_style: BorderStyle::None,
        ..Default::default()
    };
    let mut compositor = RenderCompositor::new(20, 5, Cursor::new(Vec::new()), cfg);
    let _stats = compositor
        .render_multi_pane(MultiPaneFrame {
            layout: &layout,
            zoom: None,
            focused: p,
            vts: &vts,
            titles: None,
            status_bar: Some(&bar),
        })
        .unwrap();

    let cursor: &Cursor<Vec<u8>> = compositor.inner();
    let raw = String::from_utf8_lossy(cursor.get_ref());
    // ANSI escapes between cells; assert each character appears.
    for ch in "[shux]".chars() {
        assert!(raw.contains(ch), "missing {ch} in left segment");
    }
    for ch in "12:00".chars() {
        assert!(raw.contains(ch), "missing {ch} in right segment");
    }
}

#[test]
fn test_diff_render_skips_unchanged_cells() {
    let p = pane(1);
    let layout = LayoutNode::leaf(p);
    let mut vt = VirtualTerminal::new(5, 10);
    vt.process(b"hello");
    let mut vts: HashMap<PaneId, &VirtualTerminal> = HashMap::new();
    vts.insert(p, &vt);

    let mut compositor = make_compositor(10, 5);
    let _ = compositor
        .render_multi_pane(MultiPaneFrame {
            layout: &layout,
            zoom: None,
            focused: p,
            vts: &vts,
            titles: None,
            status_bar: None,
        })
        .unwrap();
    let stats = compositor
        .render_multi_pane(MultiPaneFrame {
            layout: &layout,
            zoom: None,
            focused: p,
            vts: &vts,
            titles: None,
            status_bar: None,
        })
        .unwrap();
    // Second identical frame: zero dirty cells.
    assert_eq!(stats.dirty_cells, 0);
}

#[test]
fn test_resize_invalidates_buffer() {
    let p = pane(1);
    let layout = LayoutNode::leaf(p);
    let vt = VirtualTerminal::new(5, 10);
    let mut vts: HashMap<PaneId, &VirtualTerminal> = HashMap::new();
    vts.insert(p, &vt);

    let mut compositor = make_compositor(10, 5);
    let _ = compositor
        .render_multi_pane(MultiPaneFrame {
            layout: &layout,
            zoom: None,
            focused: p,
            vts: &vts,
            titles: None,
            status_bar: None,
        })
        .unwrap();
    compositor.resize(20, 10);
    let stats = compositor
        .render_multi_pane(MultiPaneFrame {
            layout: &layout,
            zoom: None,
            focused: p,
            vts: &vts,
            titles: None,
            status_bar: None,
        })
        .unwrap();
    assert_eq!(stats.dirty_cells, 200);
}

#[test]
fn test_complex_layout_renders_under_budget() {
    // 4-pane 2x2 grid — verify perf budget holds with multiple panes.
    let a = pane(1);
    let b = pane(2);
    let c = pane(3);
    let d = pane(4);
    let layout = LayoutNode::split(
        Direction::Vertical,
        0.5,
        LayoutNode::split(
            Direction::Horizontal,
            0.5,
            LayoutNode::leaf(a),
            LayoutNode::leaf(c),
        ),
        LayoutNode::split(
            Direction::Horizontal,
            0.5,
            LayoutNode::leaf(b),
            LayoutNode::leaf(d),
        ),
    );

    let mut vt_a = VirtualTerminal::new(11, 39);
    vt_a.process(b"alpha");
    let mut vt_b = VirtualTerminal::new(11, 40);
    vt_b.process(b"bravo");
    let mut vt_c = VirtualTerminal::new(12, 39);
    vt_c.process(b"charlie");
    let mut vt_d = VirtualTerminal::new(12, 40);
    vt_d.process(b"delta");

    let mut vts: HashMap<PaneId, &VirtualTerminal> = HashMap::new();
    vts.insert(a, &vt_a);
    vts.insert(b, &vt_b);
    vts.insert(c, &vt_c);
    vts.insert(d, &vt_d);

    let cfg = CompositorConfig {
        border_style: BorderStyle::Rounded,
        ..Default::default()
    };
    let mut compositor = RenderCompositor::new(80, 24, Cursor::new(Vec::new()), cfg);
    let stats = compositor
        .render_multi_pane(MultiPaneFrame {
            layout: &layout,
            zoom: None,
            focused: a,
            vts: &vts,
            titles: None,
            status_bar: None,
        })
        .unwrap();
    assert!(
        stats.total_time_us < 8000,
        "4-pane 80x24 took {}us — over PRD 8ms budget",
        stats.total_time_us
    );
}

// Test the rect computation independently for completeness.
#[test]
fn test_rect_layout_with_separator_reserves_one_column() {
    let a = pane(1);
    let b = pane(2);
    let layout = LayoutNode::split(
        Direction::Vertical,
        0.5,
        LayoutNode::leaf(a),
        LayoutNode::leaf(b),
    );
    let rects = layout.compute_rects(Rect::new(0, 0, 21, 5));
    assert_eq!(rects.len(), 2);
    let (_, ra) = rects[0];
    let (_, rb) = rects[1];
    // 21 - 1 separator = 20, split 50/50.
    assert!(ra.width + rb.width <= 20);
    // Second pane must start at least 1 column past the first.
    assert!(rb.x > ra.x + ra.width);
}
