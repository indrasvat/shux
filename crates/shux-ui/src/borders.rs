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
//!   2. Draw the viewport outline and each pane's four edges as lines,
//!      recording per cell which of up/down/left/right a line leaves it in.
//!   3. Pick each cell's glyph from those arms: two opposite arms are a
//!      line, two adjacent ones a corner, three a tee, four a cross.
//!   4. Color each segment by whether either of its adjacent panes is
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

/// Color palette for borders. Built from the theme engine's resolved
/// tokens (`shux_core::theme::Theme`); the `Default` impl mirrors
/// `Theme::DEFAULT` for code paths that don't have a theme yet (tests
/// + the `terminal_demo` example).
#[derive(Debug, Clone, Copy)]
pub struct BorderColors {
    pub focused: Color,
    pub unfocused: Color,
}

impl BorderColors {
    /// Build from a resolved theme. Translates `shux_core::theme::Rgb`
    /// triples into `crossterm::style::Color::Rgb` so the renderer can
    /// emit them directly.
    pub fn from_theme(theme: &shux_core::theme::Theme) -> Self {
        Self {
            focused: rgb(theme.border_focused),
            unfocused: rgb(theme.border_unfocused),
        }
    }
}

fn rgb(c: shux_core::theme::Rgb) -> Color {
    Color::Rgb {
        r: c.r,
        g: c.g,
        b: c.b,
    }
}

impl Default for BorderColors {
    fn default() -> Self {
        Self::from_theme(&shux_core::theme::Theme::DEFAULT)
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
    let v_right = ox + viewport.width as i32 - 1;
    let v_bottom = oy + viewport.height as i32 - 1;

    // Which of the four directions a line leaves each cell in. A cell's glyph
    // is read off its arms, so a junction draws exactly the lines that meet.
    #[derive(Default, Clone, Copy)]
    struct Mark {
        up: bool,
        down: bool,
        left: bool,
        right: bool,
        vert: bool,
        horiz: bool,
        focused: bool,
    }

    let mut grid: Vec<Mark> = vec![Mark::default(); w * h];
    let at = |x: i32, y: i32| ((y - oy) as usize) * w + (x - ox) as usize;

    // A line runs from its first cell to its last, clipped to the viewport;
    // only its interior cells point both ways along it.
    let mut line_v = |x: i32, y0: i32, y1: i32, foc: bool| {
        let (y0, y1) = (y0.max(oy), y1.min(v_bottom));
        if x < ox || x > v_right || y0 > y1 {
            return;
        }
        for y in y0..=y1 {
            let m = &mut grid[at(x, y)];
            m.vert = true;
            m.up |= y > y0;
            m.down |= y < y1;
            m.focused |= foc;
        }
    };
    let mut horizontals: Vec<(i32, i32, i32, bool)> = Vec::new();

    // Outline (always drawn so panes are framed). Focus comes from the panes
    // whose edges coincide with it.
    line_v(ox, oy, v_bottom, false);
    line_v(v_right, oy, v_bottom, false);
    horizontals.push((oy, ox, v_right, false));
    horizontals.push((v_bottom, ox, v_right, false));

    // Each pane's edges run one cell past it on both ends, so they reach the
    // lines framing its neighbours. The 1-cell separator that
    // `compute_rects` reserves between adjacent panes is where they land.
    for (pid, rect) in pane_rects {
        let foc = *pid == focused;
        let left = rect.x as i32 - 1;
        let right = rect.x as i32 + rect.width as i32;
        let top = rect.y as i32 - 1;
        let bottom = rect.y as i32 + rect.height as i32;
        line_v(left, top, bottom, foc);
        line_v(right, top, bottom, foc);
        horizontals.push((top, left, right, foc));
        horizontals.push((bottom, left, right, foc));
    }

    for (y, x0, x1, foc) in horizontals {
        let (x0, x1) = (x0.max(ox), x1.min(v_right));
        if y < oy || y > v_bottom || x0 > x1 {
            continue;
        }
        for x in x0..=x1 {
            let m = &mut grid[at(x, y)];
            m.horiz = true;
            m.left |= x > x0;
            m.right |= x < x1;
            m.focused |= foc;
        }
    }

    let mut segments = Vec::with_capacity(64);
    for ry in 0..h {
        for rx in 0..w {
            let m = grid[ry * w + rx];
            if !m.vert && !m.horiz {
                continue;
            }
            let ch = match (m.up, m.down, m.left, m.right) {
                (true, true, true, true) => chars.cross,
                (true, true, false, true) => chars.tee_left,
                (true, true, true, false) => chars.tee_right,
                (false, true, true, true) => chars.tee_top,
                (true, false, true, true) => chars.tee_bottom,
                (false, true, false, true) => chars.top_left,
                (false, true, true, false) => chars.top_right,
                (true, false, false, true) => chars.bottom_left,
                (true, false, true, false) => chars.bottom_right,
                (_, _, false, false) if m.up || m.down => chars.vertical,
                (false, false, _, _) if m.left || m.right => chars.horizontal,
                // A line clipped to a single cell has no arms.
                _ if m.vert => chars.vertical,
                _ => chars.horizontal,
            };
            segments.push(BorderSegment {
                x: (rx as i32 + ox) as u16,
                y: (ry as i32 + oy) as u16,
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
