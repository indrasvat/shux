//! Layout engine: binary split tree with directional navigation (PRD 5.2).
//!
//! Each window has a `WindowLayout` containing a `LayoutNode` tree.
//! Operations: split, smart_split, resize, remove, swap, zoom, directional focus.

use serde::{Deserialize, Serialize};

use crate::model::PaneId;

/// Split direction for layout nodes. JSON-serialized as `"horizontal"` /
/// `"vertical"` to match the CLI's existing string convention (apply ops,
/// attach client, m0 integration tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Children are stacked top/bottom. `a` is top, `b` is bottom.
    Horizontal,
    /// Children are side by side. `a` is left, `b` is right.
    Vertical,
}

impl Direction {
    pub fn perpendicular(&self) -> Self {
        match self {
            Self::Horizontal => Self::Vertical,
            Self::Vertical => Self::Horizontal,
        }
    }
}

/// Minimum ratio for a split. Prevents invisible panes (PRD 5.2).
pub const MIN_RATIO: f32 = 0.05;
/// Maximum ratio for a split.
pub const MAX_RATIO: f32 = 0.95;

/// Separator width between split panes (1 cell for the border line).
const SEPARATOR_SIZE: u16 = 1;

/// A node in the binary layout tree (PRD 5.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LayoutNode {
    Split {
        dir: Direction,
        /// Fraction of space allocated to child `a`. In [MIN_RATIO, MAX_RATIO].
        ratio: f32,
        a: Box<LayoutNode>,
        b: Box<LayoutNode>,
    },
    Leaf {
        pane: PaneId,
    },
}

/// A rectangle representing a pane's position and size in terminal cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn is_visible(&self) -> bool {
        self.width > 0 && self.height > 0
    }
}

/// Cardinal directions for pane navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavDirection {
    Up,
    Down,
    Left,
    Right,
}

fn clamp_ratio(ratio: f32) -> f32 {
    ratio.clamp(MIN_RATIO, MAX_RATIO)
}

fn split_rect(viewport: Rect, dir: Direction, ratio: f32) -> (Rect, Rect) {
    match dir {
        Direction::Vertical => {
            let total = viewport.width.saturating_sub(SEPARATOR_SIZE);
            let a_width = (total as f32 * ratio).round() as u16;
            let b_width = total.saturating_sub(a_width);
            (
                Rect::new(viewport.x, viewport.y, a_width, viewport.height),
                Rect::new(
                    viewport.x + a_width + SEPARATOR_SIZE,
                    viewport.y,
                    b_width,
                    viewport.height,
                ),
            )
        }
        Direction::Horizontal => {
            let total = viewport.height.saturating_sub(SEPARATOR_SIZE);
            let a_height = (total as f32 * ratio).round() as u16;
            let b_height = total.saturating_sub(a_height);
            (
                Rect::new(viewport.x, viewport.y, viewport.width, a_height),
                Rect::new(
                    viewport.x,
                    viewport.y + a_height + SEPARATOR_SIZE,
                    viewport.width,
                    b_height,
                ),
            )
        }
    }
}

impl LayoutNode {
    pub fn leaf(pane: PaneId) -> Self {
        Self::Leaf { pane }
    }

    pub fn split(dir: Direction, ratio: f32, a: LayoutNode, b: LayoutNode) -> Self {
        Self::Split {
            dir,
            ratio: clamp_ratio(ratio),
            a: Box::new(a),
            b: Box::new(b),
        }
    }

    pub fn is_leaf(&self) -> bool {
        matches!(self, Self::Leaf { .. })
    }

    pub fn pane_id(&self) -> Option<PaneId> {
        match self {
            Self::Leaf { pane } => Some(*pane),
            Self::Split { .. } => None,
        }
    }

    pub fn pane_ids(&self) -> Vec<PaneId> {
        let mut result = Vec::new();
        self.collect_pane_ids(&mut result);
        result
    }

    fn collect_pane_ids(&self, result: &mut Vec<PaneId>) {
        match self {
            Self::Leaf { pane } => result.push(*pane),
            Self::Split { a, b, .. } => {
                a.collect_pane_ids(result);
                b.collect_pane_ids(result);
            }
        }
    }

    pub fn pane_count(&self) -> usize {
        match self {
            Self::Leaf { .. } => 1,
            Self::Split { a, b, .. } => a.pane_count() + b.pane_count(),
        }
    }

    pub fn contains_pane(&self, target: PaneId) -> bool {
        match self {
            Self::Leaf { pane } => *pane == target,
            Self::Split { a, b, .. } => a.contains_pane(target) || b.contains_pane(target),
        }
    }

    pub fn depth_of(&self, target: PaneId) -> Option<usize> {
        self.depth_of_inner(target, 0)
    }

    fn depth_of_inner(&self, target: PaneId, current_depth: usize) -> Option<usize> {
        match self {
            Self::Leaf { pane } if *pane == target => Some(current_depth),
            Self::Leaf { .. } => None,
            Self::Split { a, b, .. } => a
                .depth_of_inner(target, current_depth + 1)
                .or_else(|| b.depth_of_inner(target, current_depth + 1)),
        }
    }

    /// Compute the position and size of each pane given the available viewport.
    pub fn compute_rects(&self, viewport: Rect) -> Vec<(PaneId, Rect)> {
        let mut result = Vec::new();
        self.compute_rects_inner(viewport, &mut result);
        result
    }

    fn compute_rects_inner(&self, viewport: Rect, result: &mut Vec<(PaneId, Rect)>) {
        match self {
            Self::Leaf { pane } => {
                result.push((*pane, viewport));
            }
            Self::Split { dir, ratio, a, b } => {
                let (rect_a, rect_b) = split_rect(viewport, *dir, *ratio);
                a.compute_rects_inner(rect_a, result);
                b.compute_rects_inner(rect_b, result);
            }
        }
    }

    /// Split a pane: replace the target leaf with a Split containing original + new pane.
    pub fn split_pane(
        &self,
        target_pane: PaneId,
        new_pane: PaneId,
        dir: Direction,
        ratio: f32,
    ) -> Option<LayoutNode> {
        match self {
            Self::Leaf { pane } if *pane == target_pane => Some(LayoutNode::split(
                dir,
                ratio,
                LayoutNode::leaf(target_pane),
                LayoutNode::leaf(new_pane),
            )),
            Self::Leaf { .. } => None,
            Self::Split {
                dir: d,
                ratio: r,
                a,
                b,
            } => {
                if let Some(new_a) = a.split_pane(target_pane, new_pane, dir, ratio) {
                    Some(LayoutNode::split(*d, *r, new_a, (**b).clone()))
                } else {
                    b.split_pane(target_pane, new_pane, dir, ratio)
                        .map(|new_b| LayoutNode::split(*d, *r, (**a).clone(), new_b))
                }
            }
        }
    }

    /// Smart split: wider -> vertical, taller -> horizontal, tie -> vertical (PRD 9.1).
    pub fn smart_split(
        &self,
        target_pane: PaneId,
        new_pane: PaneId,
        viewport: Rect,
    ) -> Option<LayoutNode> {
        let rects = self.compute_rects(viewport);
        let pane_rect = rects.iter().find(|(id, _)| *id == target_pane)?.1;

        let dir = if pane_rect.width >= pane_rect.height {
            Direction::Vertical
        } else {
            Direction::Horizontal
        };

        self.split_pane(target_pane, new_pane, dir, 0.5)
    }

    /// Resize a pane by adjusting the ratio of its nearest ancestor split in the given direction.
    pub fn resize_pane(
        &self,
        target_pane: PaneId,
        dir: Direction,
        delta: f32,
    ) -> Option<LayoutNode> {
        match self {
            Self::Leaf { .. } => None,
            Self::Split {
                dir: split_dir,
                ratio,
                a,
                b,
            } => {
                let in_a = a.contains_pane(target_pane);
                let in_b = b.contains_pane(target_pane);

                if !in_a && !in_b {
                    return None;
                }

                if *split_dir == dir {
                    let new_ratio = if in_a {
                        clamp_ratio(*ratio + delta)
                    } else {
                        clamp_ratio(*ratio - delta)
                    };
                    return Some(LayoutNode::split(
                        *split_dir,
                        new_ratio,
                        (**a).clone(),
                        (**b).clone(),
                    ));
                }

                if in_a {
                    a.resize_pane(target_pane, dir, delta)
                        .map(|new_a| LayoutNode::split(*split_dir, *ratio, new_a, (**b).clone()))
                } else {
                    b.resize_pane(target_pane, dir, delta)
                        .map(|new_b| LayoutNode::split(*split_dir, *ratio, (**a).clone(), new_b))
                }
            }
        }
    }

    /// Remove a pane from the layout. Parent split is collapsed, sibling promoted.
    pub fn remove_pane(&self, target: PaneId) -> Option<LayoutNode> {
        match self {
            Self::Leaf { pane } if *pane == target => None,
            Self::Leaf { .. } => None,
            Self::Split { dir, ratio, a, b } => {
                // Direct child removal
                if let Self::Leaf { pane } = a.as_ref() {
                    if *pane == target {
                        return Some((**b).clone());
                    }
                }
                if let Self::Leaf { pane } = b.as_ref() {
                    if *pane == target {
                        return Some((**a).clone());
                    }
                }

                // Recurse
                if a.contains_pane(target) {
                    let new_a = a.remove_pane(target)?;
                    Some(LayoutNode::split(*dir, *ratio, new_a, (**b).clone()))
                } else if b.contains_pane(target) {
                    let new_b = b.remove_pane(target)?;
                    Some(LayoutNode::split(*dir, *ratio, (**a).clone(), new_b))
                } else {
                    None
                }
            }
        }
    }

    /// Swap two panes in the layout tree without changing tree structure.
    pub fn swap_panes(&self, pane_a: PaneId, pane_b: PaneId) -> Option<LayoutNode> {
        if !self.contains_pane(pane_a) || !self.contains_pane(pane_b) {
            return None;
        }
        Some(self.swap_inner(pane_a, pane_b))
    }

    fn swap_inner(&self, pane_a: PaneId, pane_b: PaneId) -> LayoutNode {
        match self {
            Self::Leaf { pane } => {
                if *pane == pane_a {
                    LayoutNode::leaf(pane_b)
                } else if *pane == pane_b {
                    LayoutNode::leaf(pane_a)
                } else {
                    self.clone()
                }
            }
            Self::Split { dir, ratio, a, b } => LayoutNode::split(
                *dir,
                *ratio,
                a.swap_inner(pane_a, pane_b),
                b.swap_inner(pane_a, pane_b),
            ),
        }
    }

    /// Find the pane in the given direction from `current_pane`.
    pub fn directional_focus(
        &self,
        current_pane: PaneId,
        direction: NavDirection,
        viewport: Rect,
    ) -> Option<PaneId> {
        let rects = self.compute_rects(viewport);

        let current_rect = rects.iter().find(|(id, _)| *id == current_pane)?.1;
        let cx = current_rect.x as i32 + current_rect.width as i32 / 2;
        let cy = current_rect.y as i32 + current_rect.height as i32 / 2;

        let mut best: Option<(PaneId, i32)> = None;

        for (id, rect) in &rects {
            if *id == current_pane {
                continue;
            }

            let px = rect.x as i32 + rect.width as i32 / 2;
            let py = rect.y as i32 + rect.height as i32 / 2;

            let is_valid = match direction {
                NavDirection::Left => px < cx,
                NavDirection::Right => px > cx,
                NavDirection::Up => py < cy,
                NavDirection::Down => py > cy,
            };

            if is_valid {
                let primary = match direction {
                    NavDirection::Left | NavDirection::Right => (px - cx).abs(),
                    NavDirection::Up | NavDirection::Down => (py - cy).abs(),
                };
                let secondary = match direction {
                    NavDirection::Left | NavDirection::Right => (py - cy).abs(),
                    NavDirection::Up | NavDirection::Down => (px - cx).abs(),
                };
                let dist = primary * 1000 + secondary;

                if best.is_none() || dist < best.unwrap().1 {
                    best = Some((*id, dist));
                }
            }
        }

        best.map(|(id, _)| id)
    }
}

/// State for a zoomed pane.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoomState {
    pub saved_layout: LayoutNode,
    pub zoomed_pane: PaneId,
}

/// Layout state for a window: current tree plus optional zoom state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowLayout {
    pub tree: LayoutNode,
    pub zoom: Option<ZoomState>,
}

impl WindowLayout {
    pub fn new(pane: PaneId) -> Self {
        Self {
            tree: LayoutNode::leaf(pane),
            zoom: None,
        }
    }

    pub fn is_zoomed(&self) -> bool {
        self.zoom.is_some()
    }

    pub fn toggle_zoom(&mut self, pane: PaneId) {
        if let Some(ref zoom) = self.zoom {
            if zoom.zoomed_pane == pane {
                self.tree = zoom.saved_layout.clone();
                self.zoom = None;
                return;
            }
            // Different pane — unzoom first
            self.tree = zoom.saved_layout.clone();
            self.zoom = None;
        }

        if self.tree.contains_pane(pane) && self.tree.pane_count() > 1 {
            self.zoom = Some(ZoomState {
                saved_layout: self.tree.clone(),
                zoomed_pane: pane,
            });
            self.tree = LayoutNode::leaf(pane);
        }
    }

    pub fn compute_rects(&self, viewport: Rect) -> Vec<(PaneId, Rect)> {
        self.tree.compute_rects(viewport)
    }

    /// Split a pane. Unzooms first if needed.
    pub fn split_pane(
        &mut self,
        target_pane: PaneId,
        new_pane: PaneId,
        dir: Direction,
        ratio: f32,
    ) -> bool {
        if let Some(zoom) = self.zoom.take() {
            self.tree = zoom.saved_layout;
        }

        if let Some(new_tree) = self.tree.split_pane(target_pane, new_pane, dir, ratio) {
            self.tree = new_tree;
            true
        } else {
            false
        }
    }

    pub fn smart_split(&mut self, target_pane: PaneId, new_pane: PaneId, viewport: Rect) -> bool {
        if let Some(zoom) = self.zoom.take() {
            self.tree = zoom.saved_layout;
        }

        if let Some(new_tree) = self.tree.smart_split(target_pane, new_pane, viewport) {
            self.tree = new_tree;
            true
        } else {
            false
        }
    }

    pub fn remove_pane(&mut self, pane: PaneId) -> bool {
        if let Some(zoom) = self.zoom.take() {
            self.tree = zoom.saved_layout;
        }

        if let Some(new_tree) = self.tree.remove_pane(pane) {
            self.tree = new_tree;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(n: u128) -> PaneId {
        PaneId::from_uuid(uuid::Uuid::from_u128(n))
    }

    fn viewport() -> Rect {
        Rect::new(0, 0, 120, 40)
    }

    // ── Basic construction ────────────────────────────────────

    #[test]
    fn test_single_leaf() {
        let layout = LayoutNode::leaf(p(1));
        assert!(layout.is_leaf());
        assert_eq!(layout.pane_id(), Some(p(1)));
        assert_eq!(layout.pane_count(), 1);
    }

    #[test]
    fn test_split_creates_two_panes() {
        let layout = LayoutNode::split(
            Direction::Vertical,
            0.5,
            LayoutNode::leaf(p(1)),
            LayoutNode::leaf(p(2)),
        );
        assert!(!layout.is_leaf());
        assert_eq!(layout.pane_count(), 2);
        assert!(layout.contains_pane(p(1)));
        assert!(layout.contains_pane(p(2)));
        assert!(!layout.contains_pane(p(3)));
    }

    #[test]
    fn test_pane_ids_order() {
        let layout = LayoutNode::split(
            Direction::Vertical,
            0.5,
            LayoutNode::leaf(p(1)),
            LayoutNode::split(
                Direction::Horizontal,
                0.5,
                LayoutNode::leaf(p(2)),
                LayoutNode::leaf(p(3)),
            ),
        );
        assert_eq!(layout.pane_ids(), vec![p(1), p(2), p(3)]);
    }

    // ── Ratio clamping ────────────────────────────────────────

    #[test]
    fn test_ratio_clamped_to_min() {
        let layout = LayoutNode::split(
            Direction::Vertical,
            0.01,
            LayoutNode::leaf(p(1)),
            LayoutNode::leaf(p(2)),
        );
        if let LayoutNode::Split { ratio, .. } = layout {
            assert!((ratio - MIN_RATIO).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn test_ratio_clamped_to_max() {
        let layout = LayoutNode::split(
            Direction::Vertical,
            0.99,
            LayoutNode::leaf(p(1)),
            LayoutNode::leaf(p(2)),
        );
        if let LayoutNode::Split { ratio, .. } = layout {
            assert!((ratio - MAX_RATIO).abs() < f32::EPSILON);
        }
    }

    // ── Size calculation ──────────────────────────────────────

    #[test]
    fn test_single_pane_fills_viewport() {
        let layout = LayoutNode::leaf(p(1));
        let rects = layout.compute_rects(viewport());
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0], (p(1), viewport()));
    }

    #[test]
    fn test_vertical_split_divides_width() {
        let layout = LayoutNode::split(
            Direction::Vertical,
            0.5,
            LayoutNode::leaf(p(1)),
            LayoutNode::leaf(p(2)),
        );
        let rects = layout.compute_rects(Rect::new(0, 0, 101, 40));
        assert_eq!(rects[0].1.width, 50);
        assert_eq!(rects[1].1.width, 50);
        assert_eq!(rects[1].1.x, 51);
        assert_eq!(rects[0].1.height, 40);
        assert_eq!(rects[1].1.height, 40);
    }

    #[test]
    fn test_horizontal_split_divides_height() {
        let layout = LayoutNode::split(
            Direction::Horizontal,
            0.5,
            LayoutNode::leaf(p(1)),
            LayoutNode::leaf(p(2)),
        );
        let rects = layout.compute_rects(Rect::new(0, 0, 120, 41));
        assert_eq!(rects[0].1.height, 20);
        assert_eq!(rects[1].1.height, 20);
        assert_eq!(rects[1].1.y, 21);
    }

    #[test]
    fn test_nested_splits() {
        let layout = LayoutNode::split(
            Direction::Vertical,
            0.5,
            LayoutNode::leaf(p(1)),
            LayoutNode::split(
                Direction::Horizontal,
                0.5,
                LayoutNode::leaf(p(2)),
                LayoutNode::leaf(p(3)),
            ),
        );
        let rects = layout.compute_rects(Rect::new(0, 0, 101, 41));

        assert_eq!(rects.len(), 3);
        assert_eq!(rects[0].0, p(1));
        assert_eq!(rects[0].1.x, 0);
        assert_eq!(rects[0].1.width, 50);
        assert_eq!(rects[0].1.height, 41);

        assert_eq!(rects[1].0, p(2));
        assert_eq!(rects[1].1.x, 51);

        assert_eq!(rects[2].0, p(3));
        assert_eq!(rects[2].1.x, 51);
        assert!(rects[2].1.y > rects[1].1.y);
    }

    // ── Split operation ───────────────────────────────────────

    #[test]
    fn test_split_pane() {
        let layout = LayoutNode::leaf(p(1));
        let new_layout = layout
            .split_pane(p(1), p(2), Direction::Vertical, 0.5)
            .unwrap();

        assert_eq!(new_layout.pane_count(), 2);
        assert!(new_layout.contains_pane(p(1)));
        assert!(new_layout.contains_pane(p(2)));
    }

    #[test]
    fn test_split_nonexistent_pane_returns_none() {
        let layout = LayoutNode::leaf(p(1));
        assert!(
            layout
                .split_pane(p(99), p(2), Direction::Vertical, 0.5)
                .is_none()
        );
    }

    #[test]
    fn test_split_nested_pane() {
        let layout = LayoutNode::split(
            Direction::Vertical,
            0.5,
            LayoutNode::leaf(p(1)),
            LayoutNode::leaf(p(2)),
        );
        let new_layout = layout
            .split_pane(p(2), p(3), Direction::Horizontal, 0.5)
            .unwrap();

        assert_eq!(new_layout.pane_count(), 3);
        assert!(new_layout.contains_pane(p(3)));
    }

    // ── Smart split ───────────────────────────────────────────

    #[test]
    fn test_smart_split_wide_viewport_splits_vertical() {
        let layout = LayoutNode::leaf(p(1));
        let new_layout = layout
            .smart_split(p(1), p(2), Rect::new(0, 0, 120, 40))
            .unwrap();

        if let LayoutNode::Split { dir, .. } = new_layout {
            assert_eq!(dir, Direction::Vertical);
        } else {
            panic!("Expected split node");
        }
    }

    #[test]
    fn test_smart_split_tall_viewport_splits_horizontal() {
        let layout = LayoutNode::leaf(p(1));
        let new_layout = layout
            .smart_split(p(1), p(2), Rect::new(0, 0, 40, 120))
            .unwrap();

        if let LayoutNode::Split { dir, .. } = new_layout {
            assert_eq!(dir, Direction::Horizontal);
        } else {
            panic!("Expected split node");
        }
    }

    #[test]
    fn test_smart_split_square_defaults_to_vertical() {
        let layout = LayoutNode::leaf(p(1));
        let new_layout = layout
            .smart_split(p(1), p(2), Rect::new(0, 0, 80, 80))
            .unwrap();

        if let LayoutNode::Split { dir, .. } = new_layout {
            assert_eq!(dir, Direction::Vertical);
        } else {
            panic!("Expected split node");
        }
    }

    // ── Resize ────────────────────────────────────────────────

    #[test]
    fn test_resize_pane() {
        let layout = LayoutNode::split(
            Direction::Vertical,
            0.5,
            LayoutNode::leaf(p(1)),
            LayoutNode::leaf(p(2)),
        );

        let resized = layout.resize_pane(p(1), Direction::Vertical, 0.1).unwrap();

        if let LayoutNode::Split { ratio, .. } = resized {
            assert!((ratio - 0.6).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn test_resize_clamps_ratio() {
        let layout = LayoutNode::split(
            Direction::Vertical,
            0.9,
            LayoutNode::leaf(p(1)),
            LayoutNode::leaf(p(2)),
        );

        let resized = layout.resize_pane(p(1), Direction::Vertical, 0.5).unwrap();

        if let LayoutNode::Split { ratio, .. } = resized {
            assert!((ratio - MAX_RATIO).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn test_resize_wrong_direction_returns_none() {
        let layout = LayoutNode::split(
            Direction::Vertical,
            0.5,
            LayoutNode::leaf(p(1)),
            LayoutNode::leaf(p(2)),
        );

        assert!(
            layout
                .resize_pane(p(1), Direction::Horizontal, 0.1)
                .is_none()
        );
    }

    // ── Remove ────────────────────────────────────────────────

    #[test]
    fn test_remove_pane_from_split() {
        let layout = LayoutNode::split(
            Direction::Vertical,
            0.5,
            LayoutNode::leaf(p(1)),
            LayoutNode::leaf(p(2)),
        );

        let new_layout = layout.remove_pane(p(1)).unwrap();
        assert!(new_layout.is_leaf());
        assert_eq!(new_layout.pane_id(), Some(p(2)));
    }

    #[test]
    fn test_remove_single_pane_returns_none() {
        let layout = LayoutNode::leaf(p(1));
        assert!(layout.remove_pane(p(1)).is_none());
    }

    #[test]
    fn test_remove_from_nested() {
        let layout = LayoutNode::split(
            Direction::Vertical,
            0.5,
            LayoutNode::leaf(p(1)),
            LayoutNode::split(
                Direction::Horizontal,
                0.5,
                LayoutNode::leaf(p(2)),
                LayoutNode::leaf(p(3)),
            ),
        );

        let new_layout = layout.remove_pane(p(2)).unwrap();
        assert_eq!(new_layout.pane_count(), 2);
        assert!(new_layout.contains_pane(p(1)));
        assert!(new_layout.contains_pane(p(3)));
        assert!(!new_layout.contains_pane(p(2)));
    }

    // ── Swap ──────────────────────────────────────────────────

    #[test]
    fn test_swap_panes() {
        let layout = LayoutNode::split(
            Direction::Vertical,
            0.5,
            LayoutNode::leaf(p(1)),
            LayoutNode::leaf(p(2)),
        );

        let swapped = layout.swap_panes(p(1), p(2)).unwrap();
        let ids = swapped.pane_ids();
        assert_eq!(ids, vec![p(2), p(1)]);
    }

    #[test]
    fn test_swap_nonexistent_pane_returns_none() {
        let layout = LayoutNode::leaf(p(1));
        assert!(layout.swap_panes(p(1), p(99)).is_none());
    }

    // ── Directional focus ─────────────────────────────────────

    #[test]
    fn test_directional_focus_right() {
        let layout = LayoutNode::split(
            Direction::Vertical,
            0.5,
            LayoutNode::leaf(p(1)),
            LayoutNode::leaf(p(2)),
        );

        let next = layout.directional_focus(p(1), NavDirection::Right, viewport());
        assert_eq!(next, Some(p(2)));
    }

    #[test]
    fn test_directional_focus_left() {
        let layout = LayoutNode::split(
            Direction::Vertical,
            0.5,
            LayoutNode::leaf(p(1)),
            LayoutNode::leaf(p(2)),
        );

        let next = layout.directional_focus(p(2), NavDirection::Left, viewport());
        assert_eq!(next, Some(p(1)));
    }

    #[test]
    fn test_directional_focus_down() {
        let layout = LayoutNode::split(
            Direction::Horizontal,
            0.5,
            LayoutNode::leaf(p(1)),
            LayoutNode::leaf(p(2)),
        );

        let next = layout.directional_focus(p(1), NavDirection::Down, viewport());
        assert_eq!(next, Some(p(2)));
    }

    #[test]
    fn test_directional_focus_no_pane_in_direction() {
        let layout = LayoutNode::split(
            Direction::Vertical,
            0.5,
            LayoutNode::leaf(p(1)),
            LayoutNode::leaf(p(2)),
        );

        let next = layout.directional_focus(p(1), NavDirection::Left, viewport());
        assert_eq!(next, None);
    }

    // ── Zoom ──────────────────────────────────────────────────

    #[test]
    fn test_zoom_and_unzoom() {
        let mut wl = WindowLayout::new(p(1));
        wl.split_pane(p(1), p(2), Direction::Vertical, 0.5);
        assert_eq!(wl.tree.pane_count(), 2);

        wl.toggle_zoom(p(1));
        assert!(wl.is_zoomed());
        assert_eq!(wl.tree.pane_count(), 1);
        assert_eq!(wl.tree.pane_id(), Some(p(1)));

        wl.toggle_zoom(p(1));
        assert!(!wl.is_zoomed());
        assert_eq!(wl.tree.pane_count(), 2);
    }

    #[test]
    fn test_zoom_single_pane_does_nothing() {
        let mut wl = WindowLayout::new(p(1));
        wl.toggle_zoom(p(1));
        assert!(!wl.is_zoomed());
    }

    #[test]
    fn test_zoom_different_pane_switches() {
        let mut wl = WindowLayout::new(p(1));
        wl.split_pane(p(1), p(2), Direction::Vertical, 0.5);

        wl.toggle_zoom(p(1));
        assert_eq!(wl.tree.pane_id(), Some(p(1)));

        wl.toggle_zoom(p(2));
        assert!(wl.is_zoomed());
        assert_eq!(wl.tree.pane_id(), Some(p(2)));
    }

    #[test]
    fn test_split_while_zoomed_unzooms_first() {
        let mut wl = WindowLayout::new(p(1));
        wl.split_pane(p(1), p(2), Direction::Vertical, 0.5);
        wl.toggle_zoom(p(1));
        assert!(wl.is_zoomed());

        wl.split_pane(p(2), p(3), Direction::Horizontal, 0.5);
        assert!(!wl.is_zoomed());
        assert_eq!(wl.tree.pane_count(), 3);
    }

    #[test]
    fn test_remove_zoomed_pane() {
        let mut wl = WindowLayout::new(p(1));
        wl.split_pane(p(1), p(2), Direction::Vertical, 0.5);
        wl.toggle_zoom(p(1));

        wl.remove_pane(p(1));
        assert!(!wl.is_zoomed());
        assert_eq!(wl.tree.pane_count(), 1);
        assert_eq!(wl.tree.pane_id(), Some(p(2)));
    }

    // ── Depth ─────────────────────────────────────────────────

    #[test]
    fn test_depth() {
        let layout = LayoutNode::split(
            Direction::Vertical,
            0.5,
            LayoutNode::leaf(p(1)),
            LayoutNode::split(
                Direction::Horizontal,
                0.5,
                LayoutNode::leaf(p(2)),
                LayoutNode::leaf(p(3)),
            ),
        );

        assert_eq!(layout.depth_of(p(1)), Some(1));
        assert_eq!(layout.depth_of(p(2)), Some(2));
        assert_eq!(layout.depth_of(p(3)), Some(2));
        assert_eq!(layout.depth_of(p(99)), None);
    }

    // ── Serialization ─────────────────────────────────────────

    #[test]
    fn test_layout_serialization_roundtrip() {
        let layout = LayoutNode::split(
            Direction::Vertical,
            0.5,
            LayoutNode::leaf(p(1)),
            LayoutNode::leaf(p(2)),
        );

        let json = serde_json::to_string(&layout).unwrap();
        let deserialized: LayoutNode = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.pane_count(), 2);
        assert!(deserialized.contains_pane(p(1)));
        assert!(deserialized.contains_pane(p(2)));
    }

    #[test]
    fn test_window_layout_serialization() {
        let mut wl = WindowLayout::new(p(1));
        wl.split_pane(p(1), p(2), Direction::Vertical, 0.5);
        wl.toggle_zoom(p(1));

        let json = serde_json::to_string(&wl).unwrap();
        let deserialized: WindowLayout = serde_json::from_str(&json).unwrap();

        assert!(deserialized.is_zoomed());
    }
}
