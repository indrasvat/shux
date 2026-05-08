//! Border drawing for multi-pane layouts.
//!
//! Renders the 1-cell gap that the layout engine reserves between adjacent
//! panes (`shux_core::layout::SEPARATOR_SIZE`). Five styles supported per
//! PRD §6.1 / §10.2: thin, thick, double, rounded, none. Focused-pane
//! borders use the accent color; unfocused use a dim color.
//!
//! The algorithm:
//!   1. Compute per-pane rects via `LayoutNode::compute_rects` (already
//!      separator-aware).
//!   2. Build a sparse map of border cells: for each pane, mark its right
//!      column (if not at viewport edge) as a vertical border, its bottom
//!      row (if not at viewport edge) as a horizontal border.
//!   3. Where verticals and horizontals coincide, emit a `cross` glyph.
//!   4. At true corners (top-left, top-right, etc., relative to the
//!      bounding viewport), emit corner glyphs.
//!   5. Color each segment by whether either of its adjacent panes is
//!      focused (focus "wins" so the focused pane's outline is fully
//!      accented).

use crossterm::style::Color;

use shux_core::layout::Rect;
use shux_core::model::PaneId;

/// Pane border style (PRD §10.2: `pane_border_style` config key).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum BorderStyle {
    Thin,
    Thick,
    Double,
    #[default]
    Rounded,
    Ascii,
    None,
}

impl BorderStyle {
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "thin" => BorderStyle::Thin,
            "thick" => BorderStyle::Thick,
            "double" => BorderStyle::Double,
            "rounded" => BorderStyle::Rounded,
            "ascii" => BorderStyle::Ascii,
            "none" => BorderStyle::None,
            _ => BorderStyle::default(),
        }
    }
}

/// Glyph set for a border style.
#[derive(Debug, Clone, Copy)]
pub struct BorderChars {
    pub horizontal: char,
    pub vertical: char,
    pub top_left: char,
    pub top_right: char,
    pub bottom_left: char,
    pub bottom_right: char,
    pub tee_left: char,
    pub tee_right: char,
    pub tee_top: char,
    pub tee_bottom: char,
    pub cross: char,
}

impl BorderChars {
    pub const fn thin() -> Self {
        Self {
            horizontal: '─',
            vertical: '│',
            top_left: '┌',
            top_right: '┐',
            bottom_left: '└',
            bottom_right: '┘',
            tee_left: '├',
            tee_right: '┤',
            tee_top: '┬',
            tee_bottom: '┴',
            cross: '┼',
        }
    }

    pub const fn thick() -> Self {
        Self {
            horizontal: '━',
            vertical: '┃',
            top_left: '┏',
            top_right: '┓',
            bottom_left: '┗',
            bottom_right: '┛',
            tee_left: '┣',
            tee_right: '┫',
            tee_top: '┳',
            tee_bottom: '┻',
            cross: '╋',
        }
    }

    pub const fn double() -> Self {
        Self {
            horizontal: '═',
            vertical: '║',
            top_left: '╔',
            top_right: '╗',
            bottom_left: '╚',
            bottom_right: '╝',
            tee_left: '╠',
            tee_right: '╣',
            tee_top: '╦',
            tee_bottom: '╩',
            cross: '╬',
        }
    }

    pub const fn rounded() -> Self {
        Self {
            horizontal: '─',
            vertical: '│',
            top_left: '╭',
            top_right: '╮',
            bottom_left: '╰',
            bottom_right: '╯',
            tee_left: '├',
            tee_right: '┤',
            tee_top: '┬',
            tee_bottom: '┴',
            cross: '┼',
        }
    }

    pub const fn ascii() -> Self {
        Self {
            horizontal: '-',
            vertical: '|',
            top_left: '+',
            top_right: '+',
            bottom_left: '+',
            bottom_right: '+',
            tee_left: '+',
            tee_right: '+',
            tee_top: '+',
            tee_bottom: '+',
            cross: '+',
        }
    }
}

impl BorderStyle {
    pub fn chars(self) -> Option<BorderChars> {
        match self {
            BorderStyle::Thin => Some(BorderChars::thin()),
            BorderStyle::Thick => Some(BorderChars::thick()),
            BorderStyle::Double => Some(BorderChars::double()),
            BorderStyle::Rounded => Some(BorderChars::rounded()),
            BorderStyle::Ascii => Some(BorderChars::ascii()),
            BorderStyle::None => None,
        }
    }
}

/// Color palette for borders. Defaults are zero-config tasteful values that
/// will later be overridden by the theme engine (task 024).
#[derive(Debug, Clone, Copy)]
pub struct BorderColors {
    pub focused: Color,
    pub unfocused: Color,
}

impl Default for BorderColors {
    fn default() -> Self {
        Self {
            // Catppuccin Macchiato Sapphire — accent
            focused: Color::Rgb {
                r: 116,
                g: 199,
                b: 236,
            },
            // Catppuccin Macchiato Surface2 — dim
            unfocused: Color::Rgb {
                r: 91,
                g: 96,
                b: 120,
            },
        }
    }
}

/// One border glyph at an absolute screen position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BorderSegment {
    pub x: u16,
    pub y: u16,
    pub ch: char,
    pub focused: bool,
}

/// Compute border glyphs for a set of pane rects.
///
/// `viewport` is the bounding rectangle the panes were laid out in. Borders
/// at the viewport edge are drawn as outline (corners + edge T-pieces) so
/// the multiplexer always frames the panes; borders interior to the
/// viewport are drawn between adjacent panes only.
pub fn compute_borders(
    pane_rects: &[(PaneId, Rect)],
    focused: PaneId,
    viewport: Rect,
    style: BorderStyle,
) -> Vec<BorderSegment> {
    let chars = match style.chars() {
        Some(c) => c,
        None => return Vec::new(),
    };

    if viewport.width < 2 || viewport.height < 2 {
        return Vec::new();
    }

    let w = viewport.width as usize;
    let h = viewport.height as usize;
    let ox = viewport.x as i32;
    let oy = viewport.y as i32;

    // Sparse cell map: (rel_x, rel_y) -> (vert?, horiz?, focused?)
    // We deduce the glyph from which segments meet at each cell.
    #[derive(Default, Clone, Copy)]
    struct Mark {
        vert: bool,
        horiz: bool,
        focused: bool,
    }

    let mut grid: Vec<Mark> = vec![Mark::default(); w * h];
    let idx = |x: i32, y: i32| -> Option<usize> {
        let rx = x - ox;
        let ry = y - oy;
        if rx < 0 || ry < 0 {
            return None;
        }
        let (rx, ry) = (rx as usize, ry as usize);
        if rx >= w || ry >= h {
            return None;
        }
        Some(ry * w + rx)
    };
    let mark_v = |g: &mut [Mark], x: i32, y: i32, foc: bool| {
        if let Some(i) = idx(x, y) {
            g[i].vert = true;
            g[i].focused |= foc;
        }
    };
    let mark_h = |g: &mut [Mark], x: i32, y: i32, foc: bool| {
        if let Some(i) = idx(x, y) {
            g[i].horiz = true;
            g[i].focused |= foc;
        }
    };

    let v_right = (viewport.x as i32) + (viewport.width as i32) - 1;
    let v_bottom = (viewport.y as i32) + (viewport.height as i32) - 1;

    // Outline (always drawn so panes are framed).
    for x in viewport.x..viewport.x + viewport.width {
        // Top + bottom edges. Focused-ness of edge segments comes from
        // adjacent panes (set later) — for now mark as unfocused.
        mark_h(&mut grid, x as i32, viewport.y as i32, false);
        mark_h(&mut grid, x as i32, v_bottom, false);
    }
    for y in viewport.y..viewport.y + viewport.height {
        mark_v(&mut grid, viewport.x as i32, y as i32, false);
        mark_v(&mut grid, v_right, y as i32, false);
    }

    // For each pane, mark the border cells that surround it. The 1-cell
    // separator that `compute_rects` reserves between adjacent panes is
    // exactly where these markers land.
    for (pid, rect) in pane_rects {
        let foc = *pid == focused;
        let left = rect.x as i32 - 1;
        let right = (rect.x as i32) + (rect.width as i32);
        let top = rect.y as i32 - 1;
        let bottom = (rect.y as i32) + (rect.height as i32);

        // Vertical edges (left + right of pane) along the pane's height,
        // extending one row above and below to seal corners.
        for y in top..=bottom {
            mark_v(&mut grid, left, y, foc);
            mark_v(&mut grid, right, y, foc);
        }
        // Horizontal edges (top + bottom of pane) along the pane's width,
        // extending one column left and right to seal corners.
        for x in (left)..=(right) {
            mark_h(&mut grid, x, top, foc);
            mark_h(&mut grid, x, bottom, foc);
        }
    }

    let mut segments = Vec::with_capacity(64);
    for ry in 0..h {
        for rx in 0..w {
            let m = grid[ry * w + rx];
            if !m.vert && !m.horiz {
                continue;
            }
            let x = (rx as i32 + ox) as u16;
            let y = (ry as i32 + oy) as u16;

            let on_top = y == viewport.y;
            let on_bottom = y == viewport.y + viewport.height - 1;
            let on_left = x == viewport.x;
            let on_right = x == viewport.x + viewport.width - 1;

            // Decide glyph: corner > tee > cross > line.
            let ch = if on_top && on_left {
                chars.top_left
            } else if on_top && on_right {
                chars.top_right
            } else if on_bottom && on_left {
                chars.bottom_left
            } else if on_bottom && on_right {
                chars.bottom_right
            } else if on_top && m.vert {
                chars.tee_top
            } else if on_bottom && m.vert {
                chars.tee_bottom
            } else if on_left && m.horiz {
                chars.tee_left
            } else if on_right && m.horiz {
                chars.tee_right
            } else if m.vert && m.horiz {
                chars.cross
            } else if m.vert {
                chars.vertical
            } else {
                chars.horizontal
            };

            segments.push(BorderSegment {
                x,
                y,
                ch,
                focused: m.focused,
            });
        }
    }

    segments
}

#[cfg(test)]
mod tests {
    use super::*;
    use shux_core::layout::{Direction, LayoutNode, Rect};
    use uuid::Uuid;

    fn pane(n: u128) -> PaneId {
        PaneId::from_uuid(Uuid::from_u128(n))
    }

    #[test]
    fn test_parse_known_styles() {
        assert_eq!(BorderStyle::parse("thin"), BorderStyle::Thin);
        assert_eq!(BorderStyle::parse("THICK"), BorderStyle::Thick);
        assert_eq!(BorderStyle::parse("double"), BorderStyle::Double);
        assert_eq!(BorderStyle::parse("rounded"), BorderStyle::Rounded);
        assert_eq!(BorderStyle::parse("ascii"), BorderStyle::Ascii);
        assert_eq!(BorderStyle::parse("none"), BorderStyle::None);
        assert_eq!(BorderStyle::parse("garbage"), BorderStyle::Rounded);
    }

    #[test]
    fn test_chars_present_for_real_styles() {
        for s in [
            BorderStyle::Thin,
            BorderStyle::Thick,
            BorderStyle::Double,
            BorderStyle::Rounded,
            BorderStyle::Ascii,
        ] {
            assert!(s.chars().is_some(), "{:?}", s);
        }
        assert!(BorderStyle::None.chars().is_none());
    }

    #[test]
    fn test_no_borders_for_none_style() {
        let p = pane(1);
        let rects = vec![(p, Rect::new(0, 0, 80, 24))];
        let segs = compute_borders(&rects, p, Rect::new(0, 0, 80, 24), BorderStyle::None);
        assert!(segs.is_empty());
    }

    #[test]
    fn test_single_pane_outline() {
        let p = pane(1);
        let viewport = Rect::new(0, 0, 10, 5);
        let rects = vec![(p, viewport)];
        let segs = compute_borders(&rects, p, viewport, BorderStyle::Rounded);
        // Outline = perimeter cells. 10*5 box has 2*(10+5) - 4 = 26 perimeter cells.
        assert_eq!(segs.len(), 26);
        // Corners use rounded glyphs.
        assert!(segs.iter().any(|s| s.x == 0 && s.y == 0 && s.ch == '╭'));
        assert!(segs.iter().any(|s| s.x == 9 && s.y == 0 && s.ch == '╮'));
        assert!(segs.iter().any(|s| s.x == 0 && s.y == 4 && s.ch == '╰'));
        assert!(segs.iter().any(|s| s.x == 9 && s.y == 4 && s.ch == '╯'));
    }

    #[test]
    fn test_vertical_split_has_separator_column() {
        let a = pane(1);
        let b = pane(2);
        // 21-wide viewport split 50/50 with the layout engine's 1-col separator.
        let layout = LayoutNode::split(
            Direction::Vertical,
            0.5,
            LayoutNode::leaf(a),
            LayoutNode::leaf(b),
        );
        let viewport = Rect::new(0, 0, 21, 5);
        let rects = layout.compute_rects(viewport);
        let segs = compute_borders(&rects, a, viewport, BorderStyle::Thin);
        // Separator column lives between rect[0].x+rect[0].width and rect[1].x.
        let sep_x = rects[0].1.x + rects[0].1.width;
        let sep_segs: Vec<_> = segs
            .iter()
            .filter(|s| s.x == sep_x && s.y > 0 && s.y < viewport.height - 1)
            .collect();
        assert!(!sep_segs.is_empty(), "expected vertical separator segments");
        // Interior separator segments should be vertical glyphs.
        assert!(sep_segs.iter().all(|s| s.ch == '│'));
        // Top/bottom of separator should be tees.
        assert!(segs.iter().any(|s| s.x == sep_x && s.y == 0 && s.ch == '┬'));
        assert!(
            segs.iter()
                .any(|s| s.x == sep_x && s.y == viewport.height - 1 && s.ch == '┴')
        );
    }

    #[test]
    fn test_focus_wins_at_separator() {
        let a = pane(1);
        let b = pane(2);
        let layout = LayoutNode::split(
            Direction::Vertical,
            0.5,
            LayoutNode::leaf(a),
            LayoutNode::leaf(b),
        );
        let viewport = Rect::new(0, 0, 21, 5);
        let rects = layout.compute_rects(viewport);
        let segs = compute_borders(&rects, a, viewport, BorderStyle::Thin);
        // Some separator segment should be focused (since focused pane a is
        // adjacent to it).
        let sep_x = rects[0].1.x + rects[0].1.width;
        assert!(
            segs.iter()
                .any(|s| s.x == sep_x && s.y > 0 && s.y < 4 && s.focused)
        );
    }

    #[test]
    fn test_four_pane_grid_has_cross() {
        // 2x2 grid: split vertical, then split each child horizontal.
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
        let viewport = Rect::new(0, 0, 21, 11);
        let rects = layout.compute_rects(viewport);
        let segs = compute_borders(&rects, a, viewport, BorderStyle::Thin);
        // Should contain at least one cross intersection somewhere interior.
        assert!(
            segs.iter()
                .any(|s| s.ch == '┼' && s.x > 0 && s.x < 20 && s.y > 0 && s.y < 10),
            "expected cross intersection in 2x2 grid"
        );
    }
}
