//! The three status-bar zones never paint over one another, at any width.
//!
//! Each zone is painted in its own colour so a rendered row says which zone
//! owns every cell. Priority when the zones do not fit: left, then right, then
//! centre; a zone that loses columns first drops its trailing spaces, then
//! keeps a prefix of its text ending in `…`, or is dropped when not even one
//! glyph other than a space and the `…` fit.

use crossterm::style::Color;
use shux_ui::{RenderCell, StatusBar, StatusSegment};

const LEFT: Color = Color::Rgb { r: 1, g: 0, b: 0 };
const CENTRE: Color = Color::Rgb { r: 0, g: 1, b: 0 };
const RIGHT: Color = Color::Rgb { r: 0, g: 0, b: 1 };

fn bar(left: &str, centre: &str, right: &str) -> StatusBar {
    let mut b = StatusBar::new();
    for (zone, text, fg) in [
        (&mut b.left, left, LEFT),
        (&mut b.center, centre, CENTRE),
        (&mut b.right, right, RIGHT),
    ] {
        if !text.is_empty() {
            zone.push(StatusSegment::styled(text, fg, false));
        }
    }
    b
}

fn cols(text: &str) -> usize {
    shux_ui::text_cells(text)
        .iter()
        .map(|(_, w)| usize::from(*w))
        .sum()
}

/// The columns a zone painted, and the text it shows there.
fn zone(row: &[RenderCell], fg: Color) -> Option<(usize, usize, String)> {
    let owned = |c: &RenderCell| c.fg == Some(fg);
    let first = row.iter().position(owned)?;
    let mut end = row.iter().rposition(owned)? + 1;
    if row.get(end).is_some_and(|c| c.wide_continuation) {
        end += 1;
    }
    let text = row[first..end]
        .iter()
        .filter(|c| !c.wide_continuation)
        .map(|c| c.display_text().into_owned())
        .collect();
    Some((first, end, text))
}

fn plain(row: &[RenderCell]) -> String {
    row.iter()
        .filter(|c| !c.wide_continuation)
        .map(|c| c.display_text().into_owned())
        .collect()
}

/// `shown` is `full`, `full` less its trailing spaces, or a prefix of it with
/// some text in it, not ending in a space, followed by `…`.
fn is_full_or_truncated(shown: &str, full: &str) -> bool {
    shown == full
        || shown == full.trim_end_matches(' ')
        || shown
            .strip_suffix('…')
            .is_some_and(|p| !p.trim().is_empty() && !p.ends_with(' ') && full.starts_with(p))
}

#[test]
fn issue_202_centre_no_longer_starts_inside_the_left_zone() {
    // Left 20, centre 15 and right 10 columns on a 40-column bar.
    let (left, centre, right) = ("a-very-long-session ", " ▶ #1/1 1 shell", " 12:34:56 ");
    assert_eq!((cols(left), cols(centre), cols(right)), (20, 15, 10));
    let row = bar(left, centre, right).render_row(40);
    let text = plain(&row);
    assert!(text.starts_with(left), "left zone overwritten: {text:?}");
    assert!(text.ends_with(right), "right zone overwritten: {text:?}");
    let (start, end, shown) = zone(&row, CENTRE).expect("8 free columns hold the centre");
    assert_eq!((start, end), (21, 29), "{text:?}");
    assert_eq!(shown, " ▶ #1/1…", "{text:?}");
}

#[test]
fn zones_never_overlap_at_any_width() {
    let cases = [
        ("a-very-long-session ", " ▶ #1/1 1 shell", " 12:34:56 "),
        (" ◆ main ", " ▶ #1/1 shell ", " ^Sp ?  help "),
        ("left", "centre", "right"),
        ("日本語のセッション", "中央のゾーン", "右側"),
        ("ab👩\u{200d}💻cd", "e\u{301}e\u{301}e\u{301}", "日r"),
        ("", "centre only", ""),
        ("L", "", "only left and right"),
        ("", "C", "R"),
        ("LEFT ", "", "RIGHT "),
        (" ◆ main ", "", " ^Sp ?  help "),
    ];
    for (left, centre, right) in cases {
        let (lw, cw, rw) = (cols(left), cols(centre), cols(right));
        for width in 0..=(lw + cw + rw + 4) as u16 {
            let w = usize::from(width);
            let row = bar(left, centre, right).render_row(width);
            let ctx = format!("{left:?}/{centre:?}/{right:?} at {w}: {:?}", plain(&row));
            assert_eq!(row.len(), w, "{ctx}");

            // Ownership: every painted cell belongs to exactly one zone, and
            // a zone's cells are one contiguous run.
            let zones: Vec<_> = [(LEFT, left), (CENTRE, centre), (RIGHT, right)]
                .into_iter()
                .filter_map(|(fg, full)| zone(&row, fg).map(|z| (z, full)))
                .collect();
            for ((start, end, shown), full) in &zones {
                for cell in &row[*start..*end] {
                    assert!(
                        cell.wide_continuation || cell.fg == row[*start].fg,
                        "{ctx}: another zone painted inside {full:?}"
                    );
                }
                assert!(
                    is_full_or_truncated(shown, full),
                    "{ctx}: {full:?} shows {shown:?}"
                );
                let least = 2.min(cols(full.trim_end_matches(' ')));
                assert!(end - start >= least, "{ctx}: {full:?} sliver");
            }
            // Zones stay in order. Left and right may touch; the centre keeps
            // a blank column from each.
            for pair in zones.windows(2) {
                let ((_, prev_end, _), prev) = &pair[0];
                let ((next_start, _, _), next) = &pair[1];
                let touch_ok = *prev == left && *next == right;
                assert!(
                    prev_end < next_start || (touch_ok && prev_end == next_start),
                    "{ctx}: zones overlap or the centre touches a neighbour"
                );
            }

            // Priority: left whole when it fits, right whole when it fits
            // beside the left, centre whole when all three fit.
            let shows = |fg, full: &str| zone(&row, fg).is_some_and(|(_, _, s)| s == full);
            if lw > 0 && lw <= w {
                assert!(shows(LEFT, left), "{ctx}: left not whole");
            }
            if rw > 0 && lw + rw <= w {
                assert!(shows(RIGHT, right), "{ctx}: right not whole");
                assert_eq!(zone(&row, RIGHT).map(|z| z.1), Some(w), "{ctx}: right edge");
            }
            if cw > 0 && lw + cw + rw + 2 <= w {
                assert!(shows(CENTRE, centre), "{ctx}: centre not whole");
            }
        }
    }
}

/// A zone one column short loses trailing padding before any text, and left
/// and right may touch.
#[test]
fn a_zone_short_of_room_drops_its_padding_first() {
    let row = bar(" ◆ main ", "", " ^Sp ?  help ").render_row(20);
    assert_eq!(plain(&row), " ◆ main  ^Sp ?  help");
    let row = bar("LEFT", "", "RIGHT").render_row(9);
    assert_eq!(plain(&row), "LEFTRIGHT");
    let row = bar("abc ", "", "").render_row(3);
    assert_eq!(plain(&row), "abc");
    let row = bar("LEFT", "", "ab   ").render_row(7);
    assert_eq!(
        plain(&row),
        "LEFT ab",
        "the right zone stays flush with the edge"
    );
}

/// A cut zone ends its text with the `…`, never a space and the `…`, and a
/// zone with room for nothing but a space and the `…` is dropped.
#[test]
fn a_cut_zone_never_shows_a_bare_ellipsis() {
    let row = bar("L", " ▶ #1/1 1 ", "R").render_row(10);
    assert_eq!(plain(&row), "L  ▶ #1… R");
    let row = bar("L", " ▶ #1/1 1 ", "R").render_row(6);
    assert_eq!(plain(&row), "L    R", "{:?}", plain(&row));
    let row = bar("L", "ab  cd", "R").render_row(9);
    assert_eq!(plain(&row), "L ab…   R");
    let row = bar("\u{3000}日x", "", "").render_row(3);
    assert_eq!(
        plain(&row),
        "   ",
        "an ideographic space is no glyph to show"
    );
}

/// Bars that fit keep their exact layout: the centre stays centred.
#[test]
fn a_bar_that_fits_is_laid_out_as_before() {
    let row = bar("LEFT", "MID", "RIGHT").render_row(21);
    assert_eq!(plain(&row), "LEFT     MID    RIGHT");
    let row = bar("LEFTLEFT", "MID", "R").render_row(16);
    assert_eq!(
        plain(&row),
        "LEFTLEFT MID   R",
        "pushed right of the left zone"
    );
    let row = bar("L", "MID", "RIGHTRIGHT").render_row(16);
    assert_eq!(
        plain(&row),
        "L MID RIGHTRIGHT",
        "pulled left of the right zone"
    );
}

/// Live attach (as a terminal reads its bytes) and snapshots draw the same
/// crowded bar, glyph, width and colour alike.
#[test]
fn a_crowded_bar_draws_identically_live_and_in_snapshots() {
    use std::collections::HashMap;
    use std::io::Cursor;

    use shux_core::layout::LayoutNode;
    use shux_core::model::PaneId;
    use shux_ui::{
        BorderColors, BorderStyle, ComposeInputs, CompositorConfig, MultiPaneFrame,
        RenderCompositor, compose,
    };
    use shux_vt::VirtualTerminal;

    let colors = BorderColors {
        focused: Color::Rgb { r: 9, g: 9, b: 9 },
        unfocused: Color::Rgb { r: 8, g: 8, b: 8 },
    };
    let cases = [
        ("a-very-long-session ", " ▶ #1/1 1 shell", " 12:34:56 "),
        ("日本語のセッション", "中央のゾーン", "右側"),
        ("ab👩\u{200d}💻cd", "e\u{301}e\u{301}e\u{301}", "日r"),
    ];
    for (left, centre, right) in cases {
        for cols in 4..=48u16 {
            let bar = bar(left, centre, right);
            let pane = PaneId::new();
            let layout = LayoutNode::Leaf { pane };
            let vt = VirtualTerminal::new(2, usize::from(cols));
            let vts: HashMap<PaneId, &VirtualTerminal> = HashMap::from([(pane, &vt)]);
            let mut live = RenderCompositor::new(
                cols,
                3,
                Cursor::new(Vec::new()),
                CompositorConfig {
                    show_border: false,
                    status_bar_height: 1,
                    border_style: BorderStyle::None,
                    border_colors: colors,
                },
            );
            live.render_multi_pane(MultiPaneFrame {
                layout: &layout,
                zoom: None,
                focused: pane,
                vts: &vts,
                chrome: None,
                status_bar: Some(&bar),
            })
            .expect("render into a Vec");
            let mut seen = VirtualTerminal::new(3, usize::from(cols));
            seen.process(live.inner_mut().get_ref());
            let panes = HashMap::from([(pane, (vt.grid(), vt.cursor()))]);
            let composed = compose(
                &ComposeInputs {
                    layout: &layout,
                    zoom: None,
                    focused: pane,
                    panes: &panes,
                    chrome: None,
                    status_bar: Some(&bar),
                },
                cols,
                3,
                BorderStyle::None,
                colors,
                1,
            );
            let cells = |g: &shux_vt::Grid| -> Vec<String> {
                (0..usize::from(cols))
                    .map(|c| &g.visible_row(2)[c])
                    .map(|cell| {
                        format!(
                            "{}{}{:?}",
                            cell.grapheme().map_or(cell.ch.to_string(), str::to_string),
                            cell.width,
                            cell.style
                        )
                    })
                    .collect()
            };
            assert_eq!(
                cells(seen.grid()),
                cells(&composed.grid),
                "{left:?}/{centre:?}/{right:?} at {cols}"
            );
            let drawn: String = (0..usize::from(cols))
                .map(|c| &composed.grid.visible_row(2)[c])
                .filter(|cell| cell.width != 0)
                .map(|cell| cell.grapheme().map_or(cell.ch.to_string(), str::to_string))
                .collect();
            assert_eq!(
                drawn,
                plain(&bar.render_row(cols)),
                "the snapshot draws render_row"
            );
        }
    }
}
