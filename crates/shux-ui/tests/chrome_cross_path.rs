//! Live attach (`render_multi_pane`, as a terminal reads its bytes) and every
//! snapshot (`compose`) must draw the same border chrome: titles, badges and
//! the status bar, character, width and colour alike.

use std::collections::HashMap;
use std::io::Cursor;

use crossterm::style::Color;
use shux_core::layout::{Direction, LayoutNode};
use shux_core::model::PaneId;
use shux_ui::{
    BorderColors, BorderStyle, ComposeInputs, CompositorConfig, MultiPaneFrame, PaneChrome,
    RenderCompositor, StatusBadge, StatusBar, StatusSegment, compose,
};
use shux_vt::VirtualTerminal;

const COLS: u16 = 60;
const ROWS: u16 = 12;

fn colors() -> BorderColors {
    BorderColors {
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
    }
}

#[test]
fn attach_and_snapshot_draw_identical_chrome() {
    let (left, right) = (PaneId::new(), PaneId::new());
    let layout = LayoutNode::Split {
        dir: Direction::Vertical,
        ratio: 0.5,
        a: Box::new(LayoutNode::Leaf { pane: left }),
        b: Box::new(LayoutNode::Leaf { pane: right }),
    };
    let mut left_vt = VirtualTerminal::new(9, 28);
    left_vt.process(
        b"\x1b[38;2;120;220;180mtrue\x1b[0m \x1b[38;5;208midx\x1b[0m \x1b[31mbasic\x1b[0m",
    );
    let right_vt = VirtualTerminal::new(9, 28);
    let chrome = HashMap::from([
        (
            left,
            PaneChrome {
                title: "build".into(),
                badge: Some(StatusBadge {
                    glyph: '◆',
                    color: Color::Rgb {
                        r: 238,
                        g: 212,
                        b: 159,
                    },
                }),
            },
        ),
        (right, PaneChrome::titled("日本 e\u{301}")),
    ]);
    let mut bar = StatusBar::new();
    bar.left.push(StatusSegment::plain("session 日本 e\u{301}"));
    bar.right.push(StatusSegment::styled(
        "◆ 1 waiting ",
        Color::Rgb {
            r: 238,
            g: 212,
            b: 159,
        },
        true,
    ));

    let vts: HashMap<PaneId, &VirtualTerminal> =
        HashMap::from([(left, &left_vt), (right, &right_vt)]);
    let mut live = RenderCompositor::new(
        COLS,
        ROWS,
        Cursor::new(Vec::new()),
        CompositorConfig {
            show_border: true,
            status_bar_height: 1,
            border_style: BorderStyle::Rounded,
            border_colors: colors(),
        },
    );
    live.render_multi_pane(MultiPaneFrame {
        layout: &layout,
        zoom: None,
        focused: left,
        vts: &vts,
        chrome: Some(&chrome),
        status_bar: Some(&bar),
    })
    .unwrap();
    let mut seen = VirtualTerminal::new(ROWS as usize, COLS as usize);
    seen.process(live.inner_mut().get_ref());

    let panes = HashMap::from([
        (left, (left_vt.grid(), left_vt.cursor())),
        (right, (right_vt.grid(), right_vt.cursor())),
    ]);
    let composed = compose(
        &ComposeInputs {
            layout: &layout,
            zoom: None,
            focused: left,
            panes: &panes,
            chrome: Some(&chrome),
            status_bar: Some(&bar),
        },
        COLS,
        ROWS,
        BorderStyle::Rounded,
        colors(),
        1,
    );

    for row in [0, usize::from(ROWS) - 1] {
        let (a, b) = (seen.grid().visible_row(row), composed.grid.visible_row(row));
        for col in 0..usize::from(COLS) {
            let (x, y) = (&a[col], &b[col]);
            assert_eq!(
                (x.ch, x.width, x.grapheme(), x.style.fg),
                (y.ch, y.width, y.grapheme(), y.style.fg),
                "row {row} col {col} differs between attach and snapshot"
            );
        }
    }
    let title: String = (0..usize::from(COLS))
        .map(|c| composed.grid.visible_row(0)[c].ch)
        .collect();
    assert!(title.contains("◆ build"), "badge missing: {title}");
    assert!(title.contains('日'), "wide title missing: {title}");
}
