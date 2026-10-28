//! What a pane draws on its top border — its title and program-status badge —
//! laid out once, so every renderer draws the same cells.

use crossterm::style::Color;
use shux_core::layout::Rect;
use unicode_width::UnicodeWidthChar;

/// A pane's top-border content.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PaneChrome {
    pub title: String,
    pub badge: Option<StatusBadge>,
}

impl PaneChrome {
    pub fn titled(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            badge: None,
        }
    }
}

/// A program-status glyph drawn in its own colour ahead of the title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusBadge {
    pub glyph: char,
    pub color: Color,
}

/// One laid-out overlay cell. `text` is a whole grapheme; a `width` 2 glyph
/// also owns the next column. `color` is `None` for the border's colour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleGlyph {
    pub x: u16,
    pub text: String,
    pub width: u8,
    pub color: Option<Color>,
}

/// Columns a title takes at most, however wide the pane.
const MAX_TITLE_COLUMNS: usize = 64;

/// ` ◆ title ` for the pane whose viewport is `viewport`, starting one column
/// in from its left border. Empty when the pane is narrower than six columns
/// or has neither a title nor a badge. A badge with no room for a title cell
/// after it is drawn alone; a wide grapheme that does not fit is dropped, not
/// split.
pub fn title_overlay(chrome: &PaneChrome, viewport: Rect) -> Vec<TitleGlyph> {
    if viewport.width < 6 {
        return Vec::new();
    }
    let avail = MAX_TITLE_COLUMNS.min(usize::from(viewport.width) - 4);
    let mut body: Vec<(String, u8, Option<Color>)> = Vec::new();
    let mut used = 0usize;
    if let Some(badge) = chrome.badge {
        let w = badge.glyph.width().unwrap_or(1).clamp(1, 2);
        if w > avail {
            return Vec::new();
        }
        body.push((badge.glyph.to_string(), w as u8, Some(badge.color)));
        used = w;
    }
    let separator = usize::from(chrome.badge.is_some());
    let mut title = Vec::new();
    let mut title_used = 0usize;
    for (text, w) in graphemes(&chrome.title) {
        if used + separator + title_used + usize::from(w) > avail {
            break;
        }
        title_used += usize::from(w);
        title.push((text, w, None));
    }
    if !title.is_empty() {
        if separator == 1 {
            body.push((" ".to_string(), 1, None));
        }
        body.extend(title);
    }
    if body.is_empty() {
        return Vec::new();
    }

    let mut x = viewport.x.saturating_add(1);
    let mut out = vec![TitleGlyph {
        x,
        text: " ".to_string(),
        width: 1,
        color: None,
    }];
    x += 1;
    for (text, width, color) in body {
        out.push(TitleGlyph {
            x,
            text,
            width,
            color,
        });
        x += u16::from(width);
    }
    out.push(TitleGlyph {
        x,
        text: " ".to_string(),
        width: 1,
        color: None,
    });
    out
}

/// Cells as the VT makes them: a zero-width scalar joins the cell before it.
fn graphemes(s: &str) -> Vec<(String, u8)> {
    let mut out: Vec<(String, u8)> = Vec::new();
    for c in s.chars() {
        match c.width().unwrap_or(0) {
            0 => {
                if let Some((text, _)) = out.last_mut() {
                    text.push(c);
                }
            }
            w => out.push((c.to_string(), w.min(2) as u8)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewport(width: u16) -> Rect {
        Rect {
            x: 1,
            y: 1,
            width,
            height: 5,
        }
    }

    fn text(glyphs: &[TitleGlyph]) -> String {
        glyphs.iter().map(|g| g.text.as_str()).collect()
    }

    const BLOCKED: StatusBadge = StatusBadge {
        glyph: '◆',
        color: Color::Yellow,
    };

    #[test]
    fn a_badge_leads_the_title_in_its_own_colour() {
        let chrome = PaneChrome {
            title: "build".into(),
            badge: Some(BLOCKED),
        };
        let glyphs = title_overlay(&chrome, viewport(40));
        assert_eq!(text(&glyphs), " ◆ build ");
        assert_eq!(glyphs[1].color, Some(Color::Yellow));
        assert!(glyphs[2..].iter().all(|g| g.color.is_none()));
        assert_eq!(glyphs[0].x, 2);
    }

    #[test]
    fn a_badge_without_a_title_is_drawn_alone() {
        let chrome = PaneChrome {
            title: String::new(),
            badge: Some(BLOCKED),
        };
        assert_eq!(text(&title_overlay(&chrome, viewport(40))), " ◆ ");
    }

    #[test]
    fn a_pane_too_narrow_for_badge_and_title_keeps_the_badge() {
        let chrome = PaneChrome {
            title: "build".into(),
            badge: Some(BLOCKED),
        };
        // Six columns leave two: the badge, and no room for space + title.
        assert_eq!(text(&title_overlay(&chrome, viewport(6))), " ◆ ");
        assert_eq!(text(&title_overlay(&chrome, viewport(7))), " ◆ b ");
    }

    #[test]
    fn the_badge_comes_out_of_the_title_budget_in_columns() {
        let chrome = PaneChrome {
            title: "x".repeat(80),
            badge: Some(BLOCKED),
        };
        let glyphs = title_overlay(&chrome, viewport(200));
        let columns: usize = glyphs[1..glyphs.len() - 1]
            .iter()
            .map(|g| usize::from(g.width))
            .sum();
        assert_eq!(columns, MAX_TITLE_COLUMNS);
    }

    #[test]
    fn a_wide_grapheme_that_does_not_fit_is_dropped_whole() {
        let chrome = PaneChrome::titled("ab日");
        // Seven columns leave three: "ab" fits, "日" would need two more.
        let glyphs = title_overlay(&chrome, viewport(7));
        assert_eq!(text(&glyphs), " ab ");
    }

    #[test]
    fn combining_marks_stay_with_their_base() {
        let chrome = PaneChrome::titled("e\u{301}x");
        let glyphs = title_overlay(&chrome, viewport(20));
        assert_eq!(glyphs[1].text, "e\u{301}");
        assert_eq!(glyphs[2].x, glyphs[1].x + 1);
    }

    #[test]
    fn nothing_is_drawn_without_a_title_or_badge() {
        assert!(title_overlay(&PaneChrome::default(), viewport(40)).is_empty());
        assert!(title_overlay(&PaneChrome::titled("x"), viewport(5)).is_empty());
    }
}
