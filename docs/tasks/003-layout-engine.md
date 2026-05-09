# 003 — Layout Engine

**Status:** Done
**Depends On:** 002
**Parallelizable With:** 004, 005

---

## Problem

When a user splits a pane, resizes a border, zooms a pane, or navigates directionally between panes, the layout engine must compute where every pane is positioned within the terminal viewport. Without a layout engine, shux cannot display more than one pane at a time. The engine must handle arbitrarily deep binary split trees, maintain ratio invariants (no invisible panes), support zoom/unzoom without losing layout state, and calculate pixel-perfect positions given terminal dimensions. It must also implement the "smart split" heuristic (wider -> vertical, taller -> horizontal) from PRD 15.

## PRD Reference

- **5.2** Layout tree — LayoutNode enum (Split | Leaf), ratio invariant [0.05, 0.95], zoom save/restore
- **4.4** Key abstractions — LayoutTree as binary split tree per window, arena-allocated
- **6.1** P0 features — Split (H/V), directional focus, resize, zoom/unzoom, swap
- **9.1** Tier 1 keybindings — `Alt+Enter` smart split (splits along longest edge)

---

## Files to Create

- `crates/shux-core/src/layout.rs` — LayoutNode enum, split/resize/remove/swap/zoom operations, size calculation, directional focus, smart split heuristic

## Files to Modify

- `crates/shux-core/src/lib.rs` — Add `pub mod layout;`
- `crates/shux-core/src/graph.rs` — Add per-window layout storage (`layouts: HashMap<WindowId, LayoutNode>`) and keep it synchronized with window/pane CRUD
- `crates/shux-core/Cargo.toml` — Add `proptest` as dev-dependency (if property tests are included)

---

## Execution Steps

### Step 1: Define the LayoutNode enum

The layout is a binary tree. Each internal node is a `Split` with a direction and ratio. Each leaf maps to exactly one pane.

```rust
use serde::{Deserialize, Serialize};

use crate::model::PaneId;

/// Split direction for layout nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    /// Horizontal split: children are stacked top/bottom.
    /// `a` is the top child, `b` is the bottom child.
    Horizontal,
    /// Vertical split: children are side by side left/right.
    /// `a` is the left child, `b` is the right child.
    Vertical,
}

impl Direction {
    /// Return the perpendicular direction.
    pub fn perpendicular(&self) -> Self {
        match self {
            Self::Horizontal => Self::Vertical,
            Self::Vertical => Self::Horizontal,
        }
    }
}

/// Minimum ratio for a split. Prevents invisible panes (PRD 5.2).
pub const MIN_RATIO: f32 = 0.05;
/// Maximum ratio for a split. Prevents invisible panes (PRD 5.2).
pub const MAX_RATIO: f32 = 0.95;

/// A node in the binary layout tree (PRD 5.2).
///
/// ```text
/// LayoutNode =
///   | Split { dir: H | V, ratio: f32, a: Box<LayoutNode>, b: Box<LayoutNode> }
///   | Leaf  { pane: PaneId }
/// ```
///
/// Invariants:
/// - ratio is in [MIN_RATIO, MAX_RATIO] (prevents invisible panes)
/// - Every Leaf maps to exactly one Pane
/// - Each PaneId appears in exactly one Leaf
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LayoutNode {
    Split {
        dir: Direction,
        /// Fraction of space allocated to child `a`. Must be in [MIN_RATIO, MAX_RATIO].
        /// Child `b` gets `1.0 - ratio`.
        ratio: f32,
        a: Box<LayoutNode>,
        b: Box<LayoutNode>,
    },
    Leaf {
        pane: PaneId,
    },
}
```

### Step 2: Define the Rect type for computed positions

```rust
/// A rectangle representing a pane's position and size within the terminal.
/// All values are in terminal cell coordinates (column, row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// X position (column, 0-based from left).
    pub x: u16,
    /// Y position (row, 0-based from top).
    pub y: u16,
    /// Width in columns.
    pub width: u16,
    /// Height in rows.
    pub height: u16,
}

impl Rect {
    pub fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self { x, y, width, height }
    }

    /// Whether this rect has usable area (at least 1x1).
    pub fn is_visible(&self) -> bool {
        self.width > 0 && self.height > 0
    }

    /// The aspect ratio (width / height). Used for smart split direction.
    pub fn aspect_ratio(&self) -> f32 {
        if self.height == 0 {
            return f32::INFINITY;
        }
        self.width as f32 / self.height as f32
    }
}
```

### Step 3: Implement core LayoutNode methods

```rust
impl LayoutNode {
    /// Create a leaf node for a single pane.
    pub fn leaf(pane: PaneId) -> Self {
        Self::Leaf { pane }
    }

    /// Create a split node. Clamps ratio to [MIN_RATIO, MAX_RATIO].
    pub fn split(dir: Direction, ratio: f32, a: LayoutNode, b: LayoutNode) -> Self {
        Self::Split {
            dir,
            ratio: clamp_ratio(ratio),
            a: Box::new(a),
            b: Box::new(b),
        }
    }

    /// Returns true if this is a leaf node.
    pub fn is_leaf(&self) -> bool {
        matches!(self, Self::Leaf { .. })
    }

    /// Returns the PaneId if this is a leaf node.
    pub fn pane_id(&self) -> Option<PaneId> {
        match self {
            Self::Leaf { pane } => Some(*pane),
            Self::Split { .. } => None,
        }
    }

    /// Collect all PaneIds in this subtree (in-order traversal).
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

    /// Count the total number of panes (leaves) in this subtree.
    pub fn pane_count(&self) -> usize {
        match self {
            Self::Leaf { .. } => 1,
            Self::Split { a, b, .. } => a.pane_count() + b.pane_count(),
        }
    }

    /// Find a pane in the tree and return true if it exists.
    pub fn contains_pane(&self, target: PaneId) -> bool {
        match self {
            Self::Leaf { pane } => *pane == target,
            Self::Split { a, b, .. } => {
                a.contains_pane(target) || b.contains_pane(target)
            }
        }
    }

    /// Find the leaf node for a pane and return its depth in the tree.
    pub fn depth_of(&self, target: PaneId) -> Option<usize> {
        self.depth_of_inner(target, 0)
    }

    fn depth_of_inner(&self, target: PaneId, current_depth: usize) -> Option<usize> {
        match self {
            Self::Leaf { pane } if *pane == target => Some(current_depth),
            Self::Leaf { .. } => None,
            Self::Split { a, b, .. } => {
                a.depth_of_inner(target, current_depth + 1)
                    .or_else(|| b.depth_of_inner(target, current_depth + 1))
            }
        }
    }
}

/// Clamp a ratio to the valid range [MIN_RATIO, MAX_RATIO].
fn clamp_ratio(ratio: f32) -> f32 {
    ratio.clamp(MIN_RATIO, MAX_RATIO)
}
```

### Step 4: Implement size calculation

Given a viewport `Rect`, recursively compute the `Rect` for each pane. This accounts for border separators (1 cell between split children).

```rust
/// Separator width between split panes (1 cell for the border line).
const SEPARATOR_SIZE: u16 = 1;

impl LayoutNode {
    /// Compute the position and size of each pane given the available viewport.
    ///
    /// Returns a Vec of (PaneId, Rect) pairs. The order matches in-order
    /// traversal of the tree (left/top first, then right/bottom).
    ///
    /// Separators (borders between panes) consume 1 cell each.
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
}

/// Split a rectangle into two parts based on direction and ratio.
///
/// Accounts for a 1-cell separator between the two halves.
/// The separator is placed between rect_a and rect_b.
fn split_rect(viewport: Rect, dir: Direction, ratio: f32) -> (Rect, Rect) {
    match dir {
        Direction::Vertical => {
            // Split left/right
            let total = viewport.width.saturating_sub(SEPARATOR_SIZE);
            let a_width = (total as f32 * ratio).round() as u16;
            let b_width = total.saturating_sub(a_width);

            let rect_a = Rect::new(viewport.x, viewport.y, a_width, viewport.height);
            let rect_b = Rect::new(
                viewport.x + a_width + SEPARATOR_SIZE,
                viewport.y,
                b_width,
                viewport.height,
            );
            (rect_a, rect_b)
        }
        Direction::Horizontal => {
            // Split top/bottom
            let total = viewport.height.saturating_sub(SEPARATOR_SIZE);
            let a_height = (total as f32 * ratio).round() as u16;
            let b_height = total.saturating_sub(a_height);

            let rect_a = Rect::new(viewport.x, viewport.y, viewport.width, a_height);
            let rect_b = Rect::new(
                viewport.x,
                viewport.y + a_height + SEPARATOR_SIZE,
                viewport.width,
                b_height,
            );
            (rect_a, rect_b)
        }
    }
}
```

### Step 5: Implement split operation

Splitting replaces a leaf with a Split node containing the original pane and a new pane.

```rust
impl LayoutNode {
    /// Split a pane in the given direction.
    ///
    /// Replaces the leaf `target_pane` with a Split containing:
    /// - `a`: the original pane (keeps its position)
    /// - `b`: the new pane
    ///
    /// Returns the modified tree, or None if the target pane was not found.
    pub fn split_pane(
        &self,
        target_pane: PaneId,
        new_pane: PaneId,
        dir: Direction,
        ratio: f32,
    ) -> Option<LayoutNode> {
        match self {
            Self::Leaf { pane } if *pane == target_pane => {
                Some(LayoutNode::split(
                    dir,
                    ratio,
                    LayoutNode::leaf(target_pane),
                    LayoutNode::leaf(new_pane),
                ))
            }
            Self::Leaf { .. } => None,
            Self::Split {
                dir: d,
                ratio: r,
                a,
                b,
            } => {
                if let Some(new_a) = a.split_pane(target_pane, new_pane, dir, ratio) {
                    Some(LayoutNode::split(*d, *r, new_a, (**b).clone()))
                } else if let Some(new_b) = b.split_pane(target_pane, new_pane, dir, ratio) {
                    Some(LayoutNode::split(*d, *r, (**a).clone(), new_b))
                } else {
                    None
                }
            }
        }
    }

    /// Smart split: choose direction based on the pane's current dimensions.
    ///
    /// PRD 9.1 / 15: "splits along the longest edge — if the pane is wider
    /// than tall, split vertically; if taller than wide, split horizontally.
    /// Tie-breaks to vertical."
    pub fn smart_split(
        &self,
        target_pane: PaneId,
        new_pane: PaneId,
        viewport: Rect,
    ) -> Option<LayoutNode> {
        // Find the target pane's current rect
        let rects = self.compute_rects(viewport);
        let pane_rect = rects.iter().find(|(id, _)| *id == target_pane)?.1;

        let dir = if pane_rect.width >= pane_rect.height {
            // Wider than tall (or square) -> split vertically (side by side)
            Direction::Vertical
        } else {
            // Taller than wide -> split horizontally (top/bottom)
            Direction::Horizontal
        };

        self.split_pane(target_pane, new_pane, dir, 0.5)
    }
}
```

### Step 6: Implement resize operation

Resize adjusts the ratio of the split node that is the nearest ancestor of the target pane in the given direction.

```rust
impl LayoutNode {
    /// Resize a pane by adjusting the ratio of its nearest ancestor split.
    ///
    /// `delta` is a fraction to add to the ratio (positive = grow pane,
    /// negative = shrink pane). The ratio is clamped to [MIN_RATIO, MAX_RATIO].
    ///
    /// `target_pane` identifies which pane is being resized.
    /// The split that is adjusted is the nearest ancestor of `target_pane`
    /// in the given direction.
    ///
    /// Returns the modified tree, or None if the pane was not found or
    /// the resize direction doesn't match any ancestor split.
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

                // If this split matches the requested direction and contains the pane
                if *split_dir == dir {
                    if in_a {
                        // Growing pane in `a` means increasing ratio
                        let new_ratio = clamp_ratio(*ratio + delta);
                        return Some(LayoutNode::split(
                            *split_dir,
                            new_ratio,
                            (**a).clone(),
                            (**b).clone(),
                        ));
                    } else {
                        // Growing pane in `b` means decreasing ratio
                        let new_ratio = clamp_ratio(*ratio - delta);
                        return Some(LayoutNode::split(
                            *split_dir,
                            new_ratio,
                            (**a).clone(),
                            (**b).clone(),
                        ));
                    }
                }

                // Direction doesn't match — recurse into the branch containing the pane
                if in_a {
                    a.resize_pane(target_pane, dir, delta).map(|new_a| {
                        LayoutNode::split(*split_dir, *ratio, new_a, (**b).clone())
                    })
                } else {
                    b.resize_pane(target_pane, dir, delta).map(|new_b| {
                        LayoutNode::split(*split_dir, *ratio, (**a).clone(), new_b)
                    })
                }
            }
        }
    }
}
```

### Step 7: Implement remove operation

Removing a pane collapses its parent split node, promoting the sibling to take its place.

```rust
impl LayoutNode {
    /// Remove a pane from the layout.
    ///
    /// When a leaf is removed, its parent Split is replaced by the sibling.
    /// Returns None if the tree is a single leaf (cannot remove the last pane)
    /// or if the pane was not found.
    ///
    /// Returns `Some(new_tree)` on success, or `None` on failure.
    pub fn remove_pane(&self, target: PaneId) -> Option<LayoutNode> {
        match self {
            Self::Leaf { pane } if *pane == target => {
                // Cannot remove the root leaf — caller should check
                None
            }
            Self::Leaf { .. } => None,
            Self::Split {
                dir,
                ratio,
                a,
                b,
            } => {
                // Check if target is a direct child
                if let Self::Leaf { pane } = a.as_ref() {
                    if *pane == target {
                        // Remove `a`, promote `b`
                        return Some((**b).clone());
                    }
                }
                if let Self::Leaf { pane } = b.as_ref() {
                    if *pane == target {
                        // Remove `b`, promote `a`
                        return Some((**a).clone());
                    }
                }

                // Recurse into the branch containing the target
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
}
```

### Step 8: Implement swap and directional focus

```rust
impl LayoutNode {
    /// Swap two panes in the layout tree.
    ///
    /// This exchanges the PaneIds of two leaf nodes without changing
    /// the tree structure. Both panes must exist in the tree.
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
            Self::Split { dir, ratio, a, b } => {
                LayoutNode::split(
                    *dir,
                    *ratio,
                    a.swap_inner(pane_a, pane_b),
                    b.swap_inner(pane_a, pane_b),
                )
            }
        }
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

impl LayoutNode {
    /// Find the pane in the given direction from `current_pane`.
    ///
    /// Uses the computed rects to find the nearest pane in the specified
    /// direction. "Nearest" is defined as: the pane whose center is closest
    /// to the current pane's center along the navigation axis, with a
    /// secondary sort on perpendicular distance.
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

        let mut candidates: Vec<(PaneId, i32)> = Vec::new();

        for (id, rect) in &rects {
            if *id == current_pane {
                continue;
            }

            let px = rect.x as i32 + rect.width as i32 / 2;
            let py = rect.y as i32 + rect.height as i32 / 2;

            let is_valid_direction = match direction {
                NavDirection::Left => px < cx,
                NavDirection::Right => px > cx,
                NavDirection::Up => py < cy,
                NavDirection::Down => py > cy,
            };

            if is_valid_direction {
                // Primary distance: along the navigation axis
                // Secondary distance: perpendicular axis (for tie-breaking)
                let primary = match direction {
                    NavDirection::Left | NavDirection::Right => (px - cx).abs(),
                    NavDirection::Up | NavDirection::Down => (py - cy).abs(),
                };
                let secondary = match direction {
                    NavDirection::Left | NavDirection::Right => (py - cy).abs(),
                    NavDirection::Up | NavDirection::Down => (px - cx).abs(),
                };
                // Composite distance: heavily weight primary axis
                let dist = primary * 1000 + secondary;
                candidates.push((*id, dist));
            }
        }

        candidates.sort_by_key(|(_, dist)| *dist);
        candidates.first().map(|(id, _)| *id)
    }
}
```

### Step 9: Implement zoom/unzoom

Zoom makes a single pane fill the entire viewport by temporarily replacing the layout tree. The original tree must be preserved for unzoom.

```rust
/// State for a zoomed pane.
///
/// When a pane is zoomed, the original layout tree is saved and the
/// zoomed pane becomes the sole leaf in the tree. Unzoom restores
/// the original tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoomState {
    /// The original layout tree before zooming.
    pub saved_layout: LayoutNode,
    /// The pane that is currently zoomed.
    pub zoomed_pane: PaneId,
}

/// Layout state for a window: the current tree plus optional zoom state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowLayout {
    /// The current layout tree (may be a single zoomed leaf).
    pub tree: LayoutNode,
    /// If a pane is zoomed, the original tree is saved here.
    pub zoom: Option<ZoomState>,
}

impl WindowLayout {
    /// Create a new layout with a single pane.
    pub fn new(pane: PaneId) -> Self {
        Self {
            tree: LayoutNode::leaf(pane),
            zoom: None,
        }
    }

    /// Whether a pane is currently zoomed.
    pub fn is_zoomed(&self) -> bool {
        self.zoom.is_some()
    }

    /// Toggle zoom on a pane.
    ///
    /// If the pane is already zoomed, unzoom (restore the original tree).
    /// If no pane is zoomed, zoom the given pane.
    /// If a different pane is zoomed, unzoom first, then zoom the new pane.
    pub fn toggle_zoom(&mut self, pane: PaneId) {
        if let Some(ref zoom) = self.zoom {
            if zoom.zoomed_pane == pane {
                // Unzoom: restore original tree
                self.tree = zoom.saved_layout.clone();
                self.zoom = None;
                return;
            }
            // Different pane — unzoom first
            self.tree = zoom.saved_layout.clone();
            self.zoom = None;
        }

        // Zoom: save the current tree and replace with a single leaf
        if self.tree.contains_pane(pane) && self.tree.pane_count() > 1 {
            self.zoom = Some(ZoomState {
                saved_layout: self.tree.clone(),
                zoomed_pane: pane,
            });
            self.tree = LayoutNode::leaf(pane);
        }
    }

    /// Compute pane rects for the current layout state.
    pub fn compute_rects(&self, viewport: Rect) -> Vec<(PaneId, Rect)> {
        self.tree.compute_rects(viewport)
    }

    /// Split a pane. If a pane is zoomed, unzoom first.
    pub fn split_pane(
        &mut self,
        target_pane: PaneId,
        new_pane: PaneId,
        dir: Direction,
        ratio: f32,
    ) -> bool {
        // Unzoom if needed
        if self.zoom.is_some() {
            let zoom = self.zoom.take().unwrap();
            self.tree = zoom.saved_layout;
        }

        if let Some(new_tree) = self.tree.split_pane(target_pane, new_pane, dir, ratio) {
            self.tree = new_tree;
            true
        } else {
            false
        }
    }

    /// Smart split a pane.
    pub fn smart_split(
        &mut self,
        target_pane: PaneId,
        new_pane: PaneId,
        viewport: Rect,
    ) -> bool {
        // Unzoom if needed
        if self.zoom.is_some() {
            let zoom = self.zoom.take().unwrap();
            self.tree = zoom.saved_layout;
        }

        if let Some(new_tree) = self.tree.smart_split(target_pane, new_pane, viewport) {
            self.tree = new_tree;
            true
        } else {
            false
        }
    }

    /// Remove a pane from the layout.
    pub fn remove_pane(&mut self, pane: PaneId) -> bool {
        // If the zoomed pane is being removed, unzoom first
        if let Some(ref zoom) = self.zoom {
            if zoom.zoomed_pane == pane {
                self.tree = zoom.saved_layout.clone();
                self.zoom = None;
            } else {
                // Unzoom anyway, then remove from the full tree
                self.tree = zoom.saved_layout.clone();
                self.zoom = None;
            }
        }

        if let Some(new_tree) = self.tree.remove_pane(pane) {
            self.tree = new_tree;
            true
        } else {
            false
        }
    }
}
```

### Step 10: Write comprehensive tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PaneId;

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
            0.01, // below MIN_RATIO
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
            0.99, // above MAX_RATIO
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
        // Total width = 101, minus 1 separator = 100, split 50/50
        assert_eq!(rects[0].1.width, 50);
        assert_eq!(rects[1].1.width, 50);
        assert_eq!(rects[1].1.x, 51); // 50 + 1 separator
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
        // Total height = 41, minus 1 separator = 40, split 50/50
        assert_eq!(rects[0].1.height, 20);
        assert_eq!(rects[1].1.height, 20);
        assert_eq!(rects[1].1.y, 21); // 20 + 1 separator
    }

    #[test]
    fn test_nested_splits() {
        // Layout:
        //   V(0.5)
        //   ├── P1
        //   └── H(0.5)
        //       ├── P2
        //       └── P3
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

        // P1: left half
        assert_eq!(rects[0].0, p(1));
        assert_eq!(rects[0].1.x, 0);
        assert_eq!(rects[0].1.width, 50);
        assert_eq!(rects[0].1.height, 41);

        // P2: top-right
        assert_eq!(rects[1].0, p(2));
        assert_eq!(rects[1].1.x, 51);

        // P3: bottom-right
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
        assert!(layout.split_pane(p(99), p(2), Direction::Vertical, 0.5).is_none());
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
        // 120 wide, 40 tall -> wider, so vertical split
        let new_layout = layout.smart_split(p(1), p(2), Rect::new(0, 0, 120, 40)).unwrap();

        if let LayoutNode::Split { dir, .. } = new_layout {
            assert_eq!(dir, Direction::Vertical);
        } else {
            panic!("Expected split node");
        }
    }

    #[test]
    fn test_smart_split_tall_viewport_splits_horizontal() {
        let layout = LayoutNode::leaf(p(1));
        // 40 wide, 120 tall -> taller, so horizontal split
        let new_layout = layout.smart_split(p(1), p(2), Rect::new(0, 0, 40, 120)).unwrap();

        if let LayoutNode::Split { dir, .. } = new_layout {
            assert_eq!(dir, Direction::Horizontal);
        } else {
            panic!("Expected split node");
        }
    }

    #[test]
    fn test_smart_split_square_defaults_to_vertical() {
        let layout = LayoutNode::leaf(p(1));
        // 80x80 -> square, tie-breaks to vertical (>= comparison)
        let new_layout = layout.smart_split(p(1), p(2), Rect::new(0, 0, 80, 80)).unwrap();

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

        // Grow pane 1 (in `a` of vertical split)
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

        // Trying to resize horizontally in a vertical-only split
        assert!(layout.resize_pane(p(1), Direction::Horizontal, 0.1).is_none());
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
        // After swap: left=p(2), right=p(1)
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

        // No pane to the left of p(1)
        let next = layout.directional_focus(p(1), NavDirection::Left, viewport());
        assert_eq!(next, None);
    }

    // ── Zoom ──────────────────────────────────────────────────

    #[test]
    fn test_zoom_and_unzoom() {
        let mut wl = WindowLayout::new(p(1));
        wl.split_pane(p(1), p(2), Direction::Vertical, 0.5);
        assert_eq!(wl.tree.pane_count(), 2);

        // Zoom pane 1
        wl.toggle_zoom(p(1));
        assert!(wl.is_zoomed());
        assert_eq!(wl.tree.pane_count(), 1);
        assert_eq!(wl.tree.pane_id(), Some(p(1)));

        // Unzoom
        wl.toggle_zoom(p(1));
        assert!(!wl.is_zoomed());
        assert_eq!(wl.tree.pane_count(), 2);
    }

    #[test]
    fn test_zoom_single_pane_does_nothing() {
        let mut wl = WindowLayout::new(p(1));
        wl.toggle_zoom(p(1));
        // Single pane — zoom is a no-op
        assert!(!wl.is_zoomed());
    }

    #[test]
    fn test_zoom_different_pane_switches() {
        let mut wl = WindowLayout::new(p(1));
        wl.split_pane(p(1), p(2), Direction::Vertical, 0.5);

        wl.toggle_zoom(p(1));
        assert_eq!(wl.tree.pane_id(), Some(p(1)));

        // Zoom pane 2 while pane 1 is zoomed -> unzoom 1, zoom 2
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

        // Splitting should unzoom and split in the full tree
        wl.split_pane(p(2), p(3), Direction::Horizontal, 0.5);
        assert!(!wl.is_zoomed());
        assert_eq!(wl.tree.pane_count(), 3);
    }

    #[test]
    fn test_remove_zoomed_pane() {
        let mut wl = WindowLayout::new(p(1));
        wl.split_pane(p(1), p(2), Direction::Vertical, 0.5);
        wl.toggle_zoom(p(1));

        // Remove the zoomed pane
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
```

---

## Verification

### Functional

```bash
# Build
cargo build --workspace

# Verify layout module is publicly accessible
cargo doc -p shux-core --no-deps 2>&1 | grep -i layout
```

### Tests

```bash
# Run all layout tests
cargo nextest run -p shux-core layout::tests
cargo nextest run -p shux-core layout::tests::split_ratio_clamp
cargo nextest run -p shux-core layout::tests::directional_focus_no_neighbor

# Run all workspace tests
cargo nextest run --workspace

# Clippy
cargo clippy --workspace --all-targets -- -D warnings
```

---

## Completion Criteria

- [ ] `crates/shux-core/src/layout.rs` exists with `LayoutNode`, `Direction`, `Rect`, `NavDirection`, `ZoomState`, `WindowLayout`
- [ ] `LayoutNode` enum has `Split { dir, ratio, a, b }` and `Leaf { pane }` variants
- [ ] Ratio invariant enforced: clamped to [0.05, 0.95]
- [ ] Ratio edge cases tested: caller-supplied ratios below/above bounds are clamped without panics
- [ ] `compute_rects()` correctly computes pane positions with 1-cell separators
- [ ] `split_pane()` replaces a leaf with a split containing original + new pane
- [ ] `smart_split()` chooses direction based on aspect ratio (wider -> vertical, taller -> horizontal, tie -> vertical)
- [ ] `resize_pane()` adjusts the ratio of the nearest ancestor split in the given direction
- [ ] `remove_pane()` collapses the parent split, promoting the sibling
- [ ] `swap_panes()` exchanges two pane IDs without changing tree structure
- [ ] `directional_focus()` finds the nearest pane in a cardinal direction
- [ ] `WindowLayout` handles zoom/unzoom with saved/restored tree state
- [ ] Splitting or removing while zoomed unzooms first
- [ ] All types are `Serialize` + `Deserialize` for snapshot support
- [ ] `directional_focus()` returns `None` (not panic/error) when no neighbor exists in a direction
- [ ] All tests pass (`cargo nextest run -p shux-core layout`)
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes

---

## Commit Message

```
feat: add layout engine with binary split tree and directional navigation

- LayoutNode enum: Split (H/V with ratio) and Leaf (PaneId)
- Ratio invariant [0.05, 0.95] prevents invisible panes
- Rect computation with 1-cell separators between split panes
- Operations: split, smart_split, resize, remove, swap
- Directional focus (up/down/left/right) with center-distance heuristic
- WindowLayout with zoom/unzoom and saved tree state
- Smart split: wider->vertical, taller->horizontal (PRD Alt+Enter)
- Comprehensive tests for all operations including edge cases
```

---

## Session Protocol

1. **Before starting:** Read `CLAUDE.md`, `docs/PRD.md` sections 5.2, 9.1. Verify task 000 is complete. Verify task 002 is complete (need `PaneId` from `model.rs`).
2. **During:** Build incrementally. Start with the enum and Rect type, add `compute_rects`, then operations one at a time. Run tests after each operation.
3. **Key design decisions:**
   - Using `Box<LayoutNode>` (not arena allocation) for the initial implementation. This is the PRD's recommended approach ("Box-based initially, with arena optimization later"). Arena allocation can be added in a performance optimization task if profiling shows it matters.
   - Separator size is hardcoded to 1 cell. This can be made configurable later.
   - The directional focus algorithm uses center-of-rect distance. This is simpler than edge-based algorithms and works well for binary split trees. Consider edge-based if user feedback indicates issues with deep nesting.
4. **After:** Run full verification suite. Update `docs/PROGRESS.md`. Update `CLAUDE.md` Learnings.
5. **Watch out for:**
   - Rounding in `split_rect`: using `.round()` on `f32` to get `u16`. This can cause off-by-one errors at certain viewport sizes. The total of a_width + separator + b_width must equal the original width. Verify with tests.
   - The `remove_pane` function must handle the case where the target is nested deeply. The current implementation handles this by recursing and then collapsing the parent split.
   - Zoom state references PaneIds that may have been removed. The `remove_pane` method on `WindowLayout` handles this by unzooming before removing.
