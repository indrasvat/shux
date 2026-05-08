//! Hardcoded status bar for the M1 multiplexer (PRD §6.1, task 026).
//!
//! Renders a single bottom row containing left, center, and right segments
//! styled with the standard accent palette. Future task 026 will replace
//! this with a plugin-driven system; for now we keep the chrome
//! self-contained and dependency-free so the multiplexer is usable
//! immediately.

use crossterm::style::Color;
use unicode_width::UnicodeWidthStr;

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
    fn segments_width(segs: &[StatusSegment]) -> usize {
        segs.iter()
            .map(|s| UnicodeWidthStr::width(s.text.as_str()))
            .sum()
    }

    /// Render the status bar into a flat row of `width` `RenderCell`s.
    /// The returned vec is exactly `width` cells long, padded with spaces
    /// (with the bar's bg color) where there is no text.
    pub fn render_row(&self, width: u16) -> Vec<RenderCell> {
        let w = width as usize;
        let blank = RenderCell {
            ch: ' ',
            fg: None,
            bg: self.bg,
            attrs: RenderAttrs::default(),
            wide_continuation: false,
        };
        let mut row = vec![blank.clone(); w];

        let lw = Self::segments_width(&self.left);
        let cw = Self::segments_width(&self.center);
        let rw = Self::segments_width(&self.right);

        // Left at column 0.
        if lw > 0 {
            paint_segments(&mut row, 0, &self.left, self.bg);
        }

        // Right anchored to the right edge.
        if rw > 0 {
            let start = w.saturating_sub(rw);
            paint_segments(&mut row, start, &self.right, self.bg);
        }

        // Center anchored as best we can without overlapping left/right.
        if cw > 0 {
            let mut start = (w.saturating_sub(cw)) / 2;
            let left_end = lw + 1; // 1-cell gap.
            let right_start = w.saturating_sub(rw + 1);
            if start < left_end {
                start = left_end.min(right_start.saturating_sub(cw));
            }
            if start + cw > right_start {
                start = right_start.saturating_sub(cw);
            }
            paint_segments(&mut row, start, &self.center, self.bg);
        }

        row
    }
}

fn paint_segments(
    row: &mut [RenderCell],
    start: usize,
    segs: &[StatusSegment],
    fallback_bg: Option<Color>,
) {
    let mut cursor = start;
    for seg in segs {
        let bg = seg.bg.or(fallback_bg);
        for ch in seg.text.chars() {
            if cursor >= row.len() {
                return;
            }
            // Ignore chars that would produce 0 or 2 column width: we
            // approximate width 1; the segments are always ASCII-ish in
            // practice (icons + identifiers).
            let cell = RenderCell {
                ch,
                fg: seg.fg,
                bg,
                attrs: RenderAttrs {
                    bold: seg.bold,
                    ..Default::default()
                },
                wide_continuation: false,
            };
            row[cursor] = cell;
            cursor += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
