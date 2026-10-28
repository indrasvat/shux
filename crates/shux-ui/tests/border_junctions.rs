//! Border junctions draw exactly the lines that meet there: a glyph never
//! shows an arm that no neighbouring border line continues, and every line
//! that reaches a cell shows up as an arm of its glyph.

use std::collections::HashMap;
use std::io::Cursor;

use crossterm::style::Color;
use shux_core::layout::{Direction, LayoutNode, Rect};
use shux_core::model::PaneId;
use shux_ui::{
    BorderChars, BorderColors, BorderSegment, BorderStyle, ComposeInputs, CompositorConfig,
    MultiPaneFrame, RenderCompositor, compose, compute_borders, pane_viewport,
};
use shux_vt::VirtualTerminal;
use uuid::Uuid;

const STYLES: [BorderStyle; 4] = [
    BorderStyle::Thin,
    BorderStyle::Thick,
    BorderStyle::Double,
    BorderStyle::Rounded,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Arms {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
}

const fn arms(up: bool, down: bool, left: bool, right: bool) -> Arms {
    Arms {
        up,
        down,
        left,
        right,
    }
}

/// The lines a glyph of `chars` draws, read off the glyph itself.
fn arms_of(chars: &BorderChars, ch: char) -> Arms {
    let table = [
        (chars.horizontal, arms(false, false, true, true)),
        (chars.vertical, arms(true, true, false, false)),
        (chars.top_left, arms(false, true, false, true)),
        (chars.top_right, arms(false, true, true, false)),
        (chars.bottom_left, arms(true, false, false, true)),
        (chars.bottom_right, arms(true, false, true, false)),
        (chars.tee_left, arms(true, true, false, true)),
        (chars.tee_right, arms(true, true, true, false)),
        (chars.tee_top, arms(false, true, true, true)),
        (chars.tee_bottom, arms(true, false, true, true)),
        (chars.cross, arms(true, true, true, true)),
    ];
    table
        .iter()
        .find(|(g, _)| *g == ch)
        .map(|(_, a)| *a)
        .unwrap_or_else(|| panic!("{ch:?} is not a border glyph of this style"))
}

fn pane(n: u128) -> PaneId {
    PaneId::from_uuid(Uuid::from_u128(n))
}

fn leaf(n: u128) -> LayoutNode {
    LayoutNode::leaf(pane(n))
}

fn split(dir: Direction, ratio: f32, a: LayoutNode, b: LayoutNode) -> LayoutNode {
    LayoutNode::split(dir, ratio, a, b)
}

use Direction::{Horizontal as Stack, Vertical as Side};

/// Borders exactly as attach and snapshot compute them: the outline on the
/// content area, panes laid out in the area inset by it.
fn borders(layout: &LayoutNode, content: Rect, style: BorderStyle) -> Vec<BorderSegment> {
    let rects = layout.compute_rects(pane_viewport(content, style, false));
    compute_borders(&rects, pane(1), content, style)
}

fn glyph_at(segs: &[BorderSegment], x: u16, y: u16) -> Option<char> {
    segs.iter().find(|s| s.x == x && s.y == y).map(|s| s.ch)
}

/// An arm, the neighbour it points at, and the arm that neighbour must show.
type ArmCheck = (bool, (i32, i32), &'static str, fn(Arms) -> bool);

/// A layout, its junction glyph's name, and that glyph in a given style.
type TeeCase = (LayoutNode, &'static str, fn(&BorderChars) -> char);

/// Every arm a glyph draws meets the opposite arm of the neighbouring glyph.
fn assert_arms_join(segs: &[BorderSegment], style: BorderStyle, what: &str) {
    let chars = style.chars().expect("style draws borders");
    let at: HashMap<(i32, i32), Arms> = segs
        .iter()
        .map(|s| ((s.x as i32, s.y as i32), arms_of(&chars, s.ch)))
        .collect();
    for s in segs {
        let (x, y) = (s.x as i32, s.y as i32);
        let a = at[&(x, y)];
        let checks: [ArmCheck; 4] = [
            (a.up, (x, y - 1), "up", |n: Arms| n.down),
            (a.down, (x, y + 1), "down", |n: Arms| n.up),
            (a.left, (x - 1, y), "left", |n: Arms| n.right),
            (a.right, (x + 1, y), "right", |n: Arms| n.left),
        ];
        for (has, pos, dir, opposite) in checks {
            assert!(
                !has || at.get(&pos).is_some_and(|n| opposite(*n)),
                "{what} {style:?}: {:?} at ({x},{y}) draws a line {dir} that nothing continues\n{}",
                s.ch,
                picture(segs)
            );
        }
    }
}

fn picture(segs: &[BorderSegment]) -> String {
    let w = segs.iter().map(|s| s.x).max().unwrap_or(0) as usize + 1;
    let h = segs.iter().map(|s| s.y).max().unwrap_or(0) as usize + 1;
    let mut rows = vec![vec![' '; w]; h];
    for s in segs {
        rows[s.y as usize][s.x as usize] = s.ch;
    }
    rows.into_iter()
        .map(|r| r.into_iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The cells on the outline of `r`, the ring a pane's edges draw around it.
fn on_ring(r: Rect, x: i32, y: i32) -> bool {
    let (l, t) = (r.x as i32 - 1, r.y as i32 - 1);
    let (rt, b) = (r.x as i32 + r.width as i32, r.y as i32 + r.height as i32);
    (l..=rt).contains(&x) && (t..=b).contains(&y) && (x == l || x == rt || y == t || y == b)
}

/// Exactly the outline and every pane's ring are drawn, and a cell is
/// focused exactly when it is on the focused pane's ring.
fn assert_rings_drawn(layout: &LayoutNode, content: Rect, style: BorderStyle, what: &str) {
    let rects = layout.compute_rects(pane_viewport(content, style, false));
    let segs = borders(layout, content, style);
    let drawn: HashMap<(i32, i32), bool> = segs
        .iter()
        .map(|s| ((s.x as i32, s.y as i32), s.focused))
        .collect();
    let (cx, cy) = (content.x as i32, content.y as i32);
    let (cr, cb) = (
        cx + content.width as i32 - 1,
        cy + content.height as i32 - 1,
    );
    for y in cy..=cb {
        for x in cx..=cr {
            let outline = x == cx || x == cr || y == cy || y == cb;
            let ring = outline || rects.iter().any(|(_, r)| on_ring(*r, x, y));
            let focused = rects
                .iter()
                .any(|(p, r)| *p == pane(1) && on_ring(*r, x, y));
            assert_eq!(
                drawn.get(&(x, y)).copied(),
                ring.then_some(focused),
                "{what} {style:?}: cell ({x},{y}) drawn/focused wrong\n{}",
                picture(&segs)
            );
        }
    }
}

/// Two panes on top, one wide pane below — the layout from issue #203.
fn two_over_one() -> LayoutNode {
    split(Stack, 0.5, split(Side, 0.5, leaf(1), leaf(2)), leaf(3))
}

/// The cell where the top panes' separator meets the line above the wide
/// pane, and the separator's top end on the outline.
fn two_over_one_junctions(content: Rect, style: BorderStyle) -> ((u16, u16), (u16, u16)) {
    let rects = two_over_one().compute_rects(pane_viewport(content, style, false));
    let top_left = rects[0].1;
    let sep_x = top_left.x + top_left.width;
    let sep_y = top_left.y + top_left.height;
    ((sep_x, sep_y), (sep_x, content.y))
}

#[test]
fn three_way_junction_is_a_tee_not_a_cross() {
    let content = Rect::new(0, 0, 41, 13);
    for style in STYLES {
        let chars = style.chars().unwrap();
        let segs = borders(&two_over_one(), content, style);
        let ((jx, jy), (tx, ty)) = two_over_one_junctions(content, style);
        assert_eq!(
            glyph_at(&segs, jx, jy),
            Some(chars.tee_bottom),
            "{style:?}: lines meet from left, right and above only\n{}",
            picture(&segs)
        );
        assert_eq!(glyph_at(&segs, tx, ty), Some(chars.tee_top), "{style:?}");
        assert_eq!(glyph_at(&segs, 0, jy), Some(chars.tee_left), "{style:?}");
        assert_eq!(
            glyph_at(&segs, content.width - 1, jy),
            Some(chars.tee_right),
            "{style:?}"
        );
    }
}

#[test]
fn each_tee_orientation_matches_its_layout() {
    let content = Rect::new(0, 0, 41, 13);
    let inner = pane_viewport(content, BorderStyle::Thin, false);
    let cases: [TeeCase; 4] = [
        (two_over_one(), "┴", |c| c.tee_bottom),
        (
            split(Stack, 0.5, leaf(1), split(Side, 0.5, leaf(2), leaf(3))),
            "┬",
            |c| c.tee_top,
        ),
        (
            split(Side, 0.5, leaf(1), split(Stack, 0.5, leaf(2), leaf(3))),
            "├",
            |c| c.tee_left,
        ),
        (
            split(Side, 0.5, split(Stack, 0.5, leaf(1), leaf(2)), leaf(3)),
            "┤",
            |c| c.tee_right,
        ),
    ];
    for (layout, name, want) in cases {
        let rects = layout.compute_rects(inner);
        // Every interior junction is where the two separators meet.
        let seps_x: Vec<u16> = rects
            .iter()
            .map(|(_, r)| r.x + r.width)
            .filter(|x| *x < content.width - 1)
            .collect();
        let seps_y: Vec<u16> = rects
            .iter()
            .map(|(_, r)| r.y + r.height)
            .filter(|y| *y < content.height - 1)
            .collect();
        for style in STYLES {
            let chars = style.chars().unwrap();
            let segs = borders(&layout, content, style);
            let junctions: Vec<char> = segs
                .iter()
                .filter(|s| seps_x.contains(&s.x) && seps_y.contains(&s.y))
                .map(|s| s.ch)
                .collect();
            assert_eq!(
                junctions,
                vec![want(&chars)],
                "{name} layout, {style:?}\n{}",
                picture(&segs)
            );
            assert!(
                segs.iter().all(|s| s.ch != chars.cross),
                "{name} layout, {style:?}: no four lines meet anywhere\n{}",
                picture(&segs)
            );
        }
    }
}

#[test]
fn aligned_grid_crosses_and_offset_grid_does_not() {
    let content = Rect::new(0, 0, 41, 13);
    let grid = |top: f32, bottom: f32| {
        split(
            Side,
            0.5,
            split(Stack, top, leaf(1), leaf(2)),
            split(Stack, bottom, leaf(3), leaf(4)),
        )
    };
    for style in STYLES {
        let chars = style.chars().unwrap();
        let aligned = borders(&grid(0.5, 0.5), content, style);
        let crosses = aligned.iter().filter(|s| s.ch == chars.cross).count();
        assert_eq!(crosses, 1, "{style:?}\n{}", picture(&aligned));

        let offset = borders(&grid(0.3, 0.7), content, style);
        assert!(
            offset.iter().all(|s| s.ch != chars.cross),
            "{style:?}: offset rows meet the separator from one side each\n{}",
            picture(&offset)
        );
        let tee_r = offset.iter().filter(|s| s.ch == chars.tee_right).count();
        let tee_l = offset.iter().filter(|s| s.ch == chars.tee_left).count();
        // One of each on the separator, one of each on the outline.
        assert_eq!((tee_r, tee_l), (2, 2), "{style:?}\n{}", picture(&offset));
    }
}

/// A small deterministic generator so the invariant sees layouts nobody
/// thought to write down, without a new dev-dependency.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn random_layout(rng: &mut Lcg, next_id: &mut u128, depth: u32) -> LayoutNode {
    if depth == 0 || rng.below(3) == 0 {
        *next_id += 1;
        return leaf(*next_id);
    }
    let dir = if rng.below(2) == 0 { Stack } else { Side };
    let ratio = 0.2 + (rng.below(61) as f32) / 100.0;
    let a = random_layout(rng, next_id, depth - 1);
    let b = random_layout(rng, next_id, depth - 1);
    split(dir, ratio, a, b)
}

#[test]
fn every_arm_joins_and_every_ring_is_drawn_across_layouts_sizes_and_styles() {
    let named = [
        ("single", leaf(1)),
        ("side", split(Side, 0.5, leaf(1), leaf(2))),
        ("stack", split(Stack, 0.5, leaf(1), leaf(2))),
        ("two over one", two_over_one()),
        (
            "three columns over one",
            split(
                Stack,
                0.6,
                split(Side, 0.33, leaf(1), split(Side, 0.5, leaf(2), leaf(3))),
                leaf(4),
            ),
        ),
        (
            "nested",
            split(
                Side,
                0.4,
                split(Stack, 0.5, leaf(1), split(Side, 0.5, leaf(2), leaf(3))),
                split(Stack, 0.3, leaf(4), split(Stack, 0.5, leaf(5), leaf(6))),
            ),
        ),
    ];
    let sizes = [(41, 13), (80, 24), (120, 40), (23, 9), (9, 7)];
    for (name, layout) in &named {
        for (w, h) in sizes {
            for style in STYLES {
                let segs = borders(layout, Rect::new(0, 0, w, h), style);
                let what = format!("{name} at {w}x{h}");
                assert_arms_join(&segs, style, &what);
                assert_rings_drawn(layout, Rect::new(0, 0, w, h), style, &what);
            }
        }
    }

    let mut rng = Lcg(0x5348_5558_0203);
    for i in 0..300 {
        let mut ids = 0;
        let layout = random_layout(&mut rng, &mut ids, 4);
        let (w, h) = (20 + rng.below(100) as u16, 10 + rng.below(40) as u16);
        let style = STYLES[rng.below(STYLES.len() as u64) as usize];
        let segs = borders(&layout, Rect::new(0, 0, w, h), style);
        let what = format!("random layout {i} at {w}x{h}");
        assert_arms_join(&segs, style, &what);
        assert_rings_drawn(&layout, Rect::new(0, 0, w, h), style, &what);
    }
}

/// Live attach and `window snapshot` both draw the tee, not just the border
/// computation.
#[test]
fn attach_and_snapshot_draw_the_tee() {
    const COLS: u16 = 41;
    const ROWS: u16 = 14;
    let colors = BorderColors {
        focused: Color::Rgb {
            r: 10,
            g: 200,
            b: 240,
        },
        unfocused: Color::Rgb {
            r: 90,
            g: 90,
            b: 120,
        },
    };
    let layout = two_over_one();
    let content = Rect::new(0, 0, COLS, ROWS - 1);
    let mut probe = VirtualTerminal::new(5, 19);
    probe.process(
        b"\x1b[38;2;120;220;180mtrue\x1b[0m \x1b[38;5;208midx\x1b[0m \x1b[31mbasic\x1b[0m",
    );
    for style in STYLES {
        let chars = style.chars().unwrap();
        let ((jx, jy), _) = two_over_one_junctions(content, style);

        let rects = layout.compute_rects(pane_viewport(content, style, false));
        let vts: HashMap<PaneId, &VirtualTerminal> =
            rects.iter().map(|(p, _)| (*p, &probe)).collect();
        let mut live = RenderCompositor::new(
            COLS,
            ROWS,
            Cursor::new(Vec::new()),
            CompositorConfig {
                show_border: true,
                status_bar_height: 1,
                border_style: style,
                border_colors: colors,
            },
        );
        live.render_multi_pane(MultiPaneFrame {
            layout: &layout,
            zoom: None,
            focused: pane(1),
            vts: &vts,
            chrome: None,
            status_bar: None,
        })
        .unwrap();
        let mut seen = VirtualTerminal::new(ROWS as usize, COLS as usize);
        seen.process(live.inner_mut().get_ref());

        let panes: HashMap<PaneId, _> = rects
            .iter()
            .map(|(p, _)| (*p, (probe.grid(), probe.cursor())))
            .collect();
        let composed = compose(
            &ComposeInputs {
                layout: &layout,
                zoom: None,
                focused: pane(1),
                panes: &panes,
                chrome: None,
                status_bar: None,
            },
            COLS,
            ROWS,
            style,
            colors,
            1,
        );

        let attach_cell = seen.grid().visible_row(jy as usize)[jx as usize].ch;
        let snapshot_cell = composed.grid.visible_row(jy as usize)[jx as usize].ch;
        assert_eq!(attach_cell, chars.tee_bottom, "attach, {style:?}");
        assert_eq!(snapshot_cell, chars.tee_bottom, "snapshot, {style:?}");
        // The probe's colours reached both paths, so neither is monochrome.
        let row = (rects[0].1.y) as usize;
        let col = (rects[0].1.x) as usize;
        for (path, g) in [("attach", seen.grid()), ("snapshot", &composed.grid)] {
            let fgs: Vec<_> = [0, 5, 9]
                .iter()
                .map(|d| g.visible_row(row)[col + d].style.fg)
                .collect();
            assert!(
                fgs.iter().all(|c| *c != shux_vt::Color::Default) && fgs[0] != fgs[1],
                "{path}, {style:?}: colour probe lost: {fgs:?}"
            );
        }
    }
}
