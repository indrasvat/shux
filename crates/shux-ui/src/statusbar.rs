//! Hardcoded status bar for the M1 multiplexer (PRD §6.1, task 026).
//!
//! Renders a single bottom row containing left, center, and right segments
//! styled with the standard accent palette. Future task 026 will replace
//! this with a plugin-driven system; for now we keep the chrome
//! self-contained and dependency-free so the multiplexer is usable
//! immediately.

use crossterm::style::Color;

use crate::buffer::{RenderAttrs, RenderCell};

/// One styled segment of the status bar.
#[derive(Debug, Clone)]
pub struct StatusSegment {
    pub text: String,
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
}

impl StatusSegment {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            fg: None,
            bg: None,
            bold: false,
        }
    }

    pub fn styled(text: impl Into<String>, fg: Color, bold: bool) -> Self {
        Self {
            text: text.into(),
            fg: Some(fg),
            bg: None,
            bold,
        }
    }
}

/// Three-zone status bar (left/center/right).
#[derive(Debug, Clone, Default)]
pub struct StatusBar {
    pub left: Vec<StatusSegment>,
    pub center: Vec<StatusSegment>,
    pub right: Vec<StatusSegment>,
    pub bg: Option<Color>,
}

impl StatusBar {
    pub fn new() -> Self {
        Self::default()
    }

    /// Compute the width (in display columns) of a list of segments.
    /// Columns `segs` take when painted.
    pub fn segments_width(segs: &[StatusSegment]) -> usize {
        segs.iter()
            .flat_map(|s| crate::chrome::text_cells(&s.text))
            .map(|(_, w)| usize::from(w))
            .sum()
    }

    /// Render the status bar into a flat row of `width` `RenderCell`s.
    /// The returned vec is exactly `width` cells long, padded with spaces
    /// (with the bar's bg color) where there is no text.
    ///
    /// Zones never share a cell. Left starts at column 0 and right ends at the
    /// edge, and may touch; the centre is centred as near as they allow, one
    /// blank column from each. When they do not fit, left wins, then right,
    /// then centre: a zone short of columns first loses its trailing spaces,
    /// then shows a prefix of its text ending in `…`, and is dropped when not
    /// even one glyph other than a space and the `…` fit.
    pub fn render_row(&self, width: u16) -> Vec<RenderCell> {
        let w = width as usize;
        let blank = RenderCell {
            ch: ' ',
            fg: None,
            bg: self.bg,
            attrs: RenderAttrs::default(),
            extended: None,
            wide_continuation: false,
        };
        let mut row = vec![blank; w];

        let left_end =
            paint_zone(&mut row, 0, w, &self.left, self.bg, false).map_or(0, |(_, end)| end);
        let rw = Self::segments_width(&self.right);
        let right_start = paint_zone(
            &mut row,
            w.saturating_sub(rw).max(left_end),
            w,
            &self.right,
            self.bg,
            true,
        )
        .map(|(start, _)| start);
        let after_left = left_end + usize::from(left_end > 0);
        let before_right = right_start.map_or(w, |s| s.saturating_sub(1));

        let cw = Self::segments_width(&self.center);
        let start = if after_left + cw <= before_right {
            (w.saturating_sub(cw) / 2).clamp(after_left, before_right - cw)
        } else {
            after_left
        };
        paint_zone(&mut row, start, before_right, &self.center, self.bg, false);

        row
    }
}

/// Paint `segs` from `start`, within `start..end`. A zone wider than that
/// drops its trailing spaces (ending flush at `end` when `flush_end`), or
/// else shows the text that fits, less trailing spaces, and an `…`; with no
/// room for one glyph other than a space and the `…` it is not painted.
/// Returns the columns painted.
fn paint_zone(
    row: &mut [RenderCell],
    mut start: usize,
    end: usize,
    segs: &[StatusSegment],
    fallback_bg: Option<Color>,
    flush_end: bool,
) -> Option<(usize, usize)> {
    let avail = end.saturating_sub(start);
    let zw = StatusBar::segments_width(segs);
    if zw == 0 {
        return None;
    }
    let text: String = segs.iter().map(|s| s.text.as_str()).collect();
    let padding = text.len() - text.trim_end_matches(' ').len();
    let (limit, cut) = if zw <= avail {
        (end, false)
    } else if zw == padding {
        return None;
    } else if zw - padding <= avail {
        if flush_end {
            start = end - (zw - padding);
        }
        (start + zw - padding, false)
    } else if avail >= 2 {
        (end - 1, true)
    } else {
        return None;
    };
    let mut cursor = start;
    // Where an `…` would follow the text so far, past any spaces it ends in,
    // and the style of the glyph before it.
    let mut text_end = start;
    let mut text_style = (None, None, RenderAttrs::default());
    for seg in segs {
        let bg = seg.bg.or(fallback_bg);
        let attrs = RenderAttrs {
            bold: seg.bold,
            ..Default::default()
        };
        for (text, width) in crate::chrome::text_cells(&seg.text) {
            let width = usize::from(width);
            if cursor + width > limit {
                if !cut {
                    return Some((start, cursor));
                }
                row[text_end..cursor].fill(RenderCell {
                    bg: fallback_bg,
                    ..RenderCell::default()
                });
                if text_end == start {
                    return None;
                }
                let (fg, bg, attrs) = text_style;
                row[text_end] = RenderCell {
                    ch: '…',
                    fg,
                    bg,
                    attrs,
                    extended: None,
                    wide_continuation: false,
                };
                return Some((start, text_end + 1));
            }
            let mut chars = text.chars();
            let Some(ch) = chars.next() else {
                continue;
            };
            let extended = chars.next().map(|_| {
                std::sync::Arc::new(shux_vt::ExtendedAttrs {
                    grapheme: Some(text.clone()),
                    ..Default::default()
                })
            });
            row[cursor] = RenderCell {
                ch,
                fg: seg.fg,
                bg,
                attrs,
                extended,
                wide_continuation: false,
            };
            if width == 2 {
                row[cursor + 1] = RenderCell {
                    ch: ' ',
                    bg,
                    wide_continuation: true,
                    ..RenderCell::default()
                };
            }
            cursor += width;
            if ch != ' ' {
                text_end = cursor;
                text_style = (seg.fg, bg, attrs);
            }
        }
    }
    Some((start, cursor))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bar too narrow for its zones never leaves half a wide glyph.
    #[test]
    fn a_crowded_bar_never_leaves_half_a_wide_glyph() {
        for left in ["ab日cd", "ab👩\u{200d}💻cd", "a日bcd", "abc日"] {
            let mut bar = StatusBar::new();
            bar.left.push(StatusSegment::plain(left));
            let row = bar.render_row(4);
            for (i, cell) in row.iter().enumerate() {
                let wide = crate::chrome::text_cells(&cell.display_text())
                    .first()
                    .is_some_and(|(_, w)| *w == 2);
                let next_is_cont = row.get(i + 1).is_some_and(|c| c.wide_continuation);
                assert!(
                    cell.wide_continuation || wide == next_is_cont,
                    "{left:?}: col {i} {:?} orphaned",
                    cell.display_text()
                );
                if cell.wide_continuation {
                    assert!(
                        i > 0 && !row[i - 1].wide_continuation,
                        "{left:?}: col {i} stray continuation"
                    );
                }
            }
        }
    }

    #[test]
    fn test_empty_bar_is_blank_row() {
        let bar = StatusBar::new();
        let row = bar.render_row(10);
        assert_eq!(row.len(), 10);
        assert!(row.iter().all(|c| c.ch == ' '));
    }

    #[test]
    fn test_left_segment_paints_from_column_zero() {
        let mut bar = StatusBar::new();
        bar.left.push(StatusSegment::plain("hello"));
        let row = bar.render_row(20);
        let text: String = row.iter().take(5).map(|c| c.ch).collect();
        assert_eq!(text, "hello");
        assert!(row[5..].iter().all(|c| c.ch == ' '));
    }

    #[test]
    fn test_right_segment_anchors_to_right_edge() {
        let mut bar = StatusBar::new();
        bar.right.push(StatusSegment::plain("END"));
        let row = bar.render_row(10);
        let text: String = row.iter().skip(7).map(|c| c.ch).collect();
        assert_eq!(text, "END");
    }

    #[test]
    fn test_center_segment_centers() {
        let mut bar = StatusBar::new();
        bar.center.push(StatusSegment::plain("MID"));
        let row = bar.render_row(11);
        // 11 cols, 3 chars centered → starts at col 4.
        let text: String = row.iter().skip(4).take(3).map(|c| c.ch).collect();
        assert_eq!(text, "MID");
    }

    #[test]
    fn test_no_overlap_between_left_and_right() {
        let mut bar = StatusBar::new();
        bar.left.push(StatusSegment::plain("LEFT"));
        bar.right.push(StatusSegment::plain("RIGHT"));
        let row = bar.render_row(20);
        let text: String = row.iter().map(|c| c.ch).collect();
        assert!(text.starts_with("LEFT"));
        assert!(text.ends_with("RIGHT"));
    }

    #[test]
    fn test_bold_attribute_propagates() {
        let mut bar = StatusBar::new();
        bar.left.push(StatusSegment {
            text: "X".to_string(),
            fg: None,
            bg: None,
            bold: true,
        });
        let row = bar.render_row(5);
        assert!(row[0].attrs.bold);
    }
}
