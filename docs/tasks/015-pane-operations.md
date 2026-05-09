# 015 — Pane Operations (split, focus, resize, zoom, swap, kill)

**Status:** Done
**Depends On:** 014, 003
**Parallelizable With:** 020

---

## Problem

With sessions and windows in place, users and agents need to create and manage panes within windows. Panes are where actual terminal work happens: each pane has its own PTY, its own process, and its own slice of screen real estate. The core pane operations are split, focus, resize, zoom, swap, and kill.

Splitting is the primary way panes are created after the initial default pane. A split takes an existing pane, divides it in half (horizontally or vertically), and spawns a new PTY process in the new half. This modifies the window's LayoutTree from task 003, converting a `Leaf` node into a `Split` node with two children.

The "smart split" feature (Alt+Enter) examines the current pane's dimensions and splits along the longest edge. This makes pane creation feel natural without requiring users to think about H vs V direction.

Zoom is a toggle that makes one pane temporarily fill the entire window area, saving and restoring the previous layout ratios when toggled back. This is critical for reading logs or editing in a small pane.

## PRD Reference

- **PRD section 6.1 (Panes)**: Split (H/V), directional focus (up/down/left/right), resize (step + mouse drag), zoom/unzoom, close, swap.
- **PRD section 8.2 (pane.* methods)**: pane.split, pane.ensure, pane.focus, pane.resize, pane.zoom, pane.swap, pane.kill
- **PRD section 5.2 (Layout tree)**: LayoutNode = Split { dir, ratio, a, b } | Leaf { pane }; ratio in [0.05, 0.95]; zoom saves/restores ratios
- **PRD section 9.1 (Tier 1 keybindings)**: Alt+Enter = new pane (smart split direction)
- **PRD section 14.1 (Performance)**: Split pane operation p50 <= 25ms, p99 <= 80ms

---

## Files to Create

- `crates/shux-rpc/src/methods/pane.rs` — JSON-RPC handlers for pane.* structural methods
- `crates/shux/src/commands/pane.rs` — CLI pane subcommands
- `crates/shux-core/src/pane.rs` — Pane mutation operations
- `crates/shux-core/src/navigation.rs` — Directional focus navigation using layout geometry
- `crates/shux-rpc/tests/pane_api.rs` — L3 API contract tests

## Files to Modify

- `crates/shux-core/src/layout.rs` — Add split, remove, resize, zoom, swap operations to LayoutTree (extends task 003)
- `crates/shux-core/src/graph.rs` — Add pane mutation methods to SessionGraph
- `crates/shux-core/src/events.rs` — Add pane event types
- `crates/shux-rpc/src/methods/mod.rs` — Register pane module
- `crates/shux-rpc/src/router.rs` — Register pane.* method handlers
- `crates/shux/src/main.rs` — Wire pane subcommands
- `crates/shux/src/commands/mod.rs` — Register pane module

---

## Execution Steps

### Step 1: Extend LayoutTree with structural operations

The LayoutTree from task 003 needs methods for splitting, removing, resizing, zooming, and swapping. These are pure data structure operations on the binary tree; they do not interact with PTYs or the network.

In `crates/shux-core/src/layout.rs`, extend with:

```rust
use uuid::Uuid;

/// Direction of a split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SplitDirection {
    Horizontal, // top/bottom
    Vertical,   // left/right
}

/// A node in the layout binary tree.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum LayoutNode {
    Split {
        dir: SplitDirection,
        /// Ratio of space allocated to the first child (a).
        /// Invariant: ratio is in [0.05, 0.95].
        ratio: f32,
        a: Box<LayoutNode>,
        b: Box<LayoutNode>,
    },
    Leaf {
        pane: Uuid,
    },
}

/// Saved layout state for zoom restore.
#[derive(Debug, Clone)]
pub struct ZoomState {
    pub zoomed_pane: Uuid,
    pub saved_layout: LayoutNode,
}

impl LayoutNode {
    /// Collect all pane IDs in this subtree.
    pub fn pane_ids(&self) -> Vec<Uuid> {
        match self {
            LayoutNode::Leaf { pane } => vec![*pane],
            LayoutNode::Split { a, b, .. } => {
                let mut ids = a.pane_ids();
                ids.extend(b.pane_ids());
                ids
            }
        }
    }

    /// Count the total number of panes in this subtree.
    pub fn pane_count(&self) -> usize {
        match self {
            LayoutNode::Leaf { .. } => 1,
            LayoutNode::Split { a, b, .. } => a.pane_count() + b.pane_count(),
        }
    }

    /// Split a pane, replacing its Leaf with a Split containing the original
    /// pane and a new pane. Returns true if the split was performed.
    ///
    /// The `target_pane` becomes child `a` of the new Split node.
    /// The `new_pane` becomes child `b`.
    pub fn split_pane(
        &mut self,
        target_pane: Uuid,
        new_pane: Uuid,
        direction: SplitDirection,
        ratio: f32,
    ) -> bool {
        let clamped_ratio = ratio.clamp(0.05, 0.95);

        match self {
            LayoutNode::Leaf { pane } if *pane == target_pane => {
                // Replace this leaf with a split
                let original = LayoutNode::Leaf { pane: target_pane };
                let new_leaf = LayoutNode::Leaf { pane: new_pane };
                *self = LayoutNode::Split {
                    dir: direction,
                    ratio: clamped_ratio,
                    a: Box::new(original),
                    b: Box::new(new_leaf),
                };
                true
            }
            LayoutNode::Split { a, b, .. } => {
                a.split_pane(target_pane, new_pane, direction, ratio)
                    || b.split_pane(target_pane, new_pane, direction, ratio)
            }
            _ => false,
        }
    }

    /// Remove a pane from the layout tree.
    /// When a leaf is removed, its sibling "bubbles up" to replace the parent Split.
    /// Returns the modified tree (or None if the tree is now empty).
    pub fn remove_pane(&mut self, target_pane: Uuid) -> Option<LayoutNode> {
        match self {
            LayoutNode::Leaf { pane } if *pane == target_pane => None,
            LayoutNode::Leaf { .. } => Some(self.clone()),
            LayoutNode::Split { a, b, .. } => {
                // Check if one of the direct children is the target leaf
                if matches!(a.as_ref(), LayoutNode::Leaf { pane } if *pane == target_pane) {
                    return Some(*b.clone());
                }
                if matches!(b.as_ref(), LayoutNode::Leaf { pane } if *pane == target_pane) {
                    return Some(*a.clone());
                }

                // Recurse into children
                if let Some(new_a) = a.remove_pane(target_pane) {
                    *a = Box::new(new_a);
                    return Some(self.clone());
                }
                if let Some(new_b) = b.remove_pane(target_pane) {
                    *b = Box::new(new_b);
                    return Some(self.clone());
                }

                Some(self.clone())
            }
        }
    }

    /// Resize a pane by adjusting the ratio of its parent Split.
    /// `delta` is added to the ratio (positive = grow pane a, negative = grow pane b).
    /// Returns true if the resize was applied.
    pub fn resize_pane(&mut self, target_pane: Uuid, delta: f32) -> bool {
        match self {
            LayoutNode::Split { a, b, ratio, .. } => {
                let a_panes = a.pane_ids();
                let b_panes = b.pane_ids();

                if a_panes.contains(&target_pane) {
                    // If target is directly in child a, adjust this split's ratio
                    if matches!(a.as_ref(), LayoutNode::Leaf { pane } if *pane == target_pane)
                        || a_panes.contains(&target_pane)
                    {
                        let new_ratio = (*ratio + delta).clamp(0.05, 0.95);
                        if (new_ratio - *ratio).abs() > f32::EPSILON {
                            *ratio = new_ratio;
                            return true;
                        }
                    }
                    // Try recursing deeper
                    return a.resize_pane(target_pane, delta);
                }

                if b_panes.contains(&target_pane) {
                    // Target is in child b; resize means shrinking a
                    if matches!(b.as_ref(), LayoutNode::Leaf { pane } if *pane == target_pane)
                        || b_panes.contains(&target_pane)
                    {
                        let new_ratio = (*ratio - delta).clamp(0.05, 0.95);
                        if (new_ratio - *ratio).abs() > f32::EPSILON {
                            *ratio = new_ratio;
                            return true;
                        }
                    }
                    return b.resize_pane(target_pane, delta);
                }

                false
            }
            LayoutNode::Leaf { .. } => false,
        }
    }

    /// Swap two panes in the layout tree.
    /// Both panes must exist in the tree.
    pub fn swap_panes(&mut self, pane_a: Uuid, pane_b: Uuid) -> bool {
        // First pass: find and replace pane_a with a sentinel
        let sentinel = Uuid::nil();
        if !self.replace_pane(pane_a, sentinel) {
            return false;
        }
        // Replace pane_b with pane_a
        if !self.replace_pane(pane_b, pane_a) {
            // Rollback: restore pane_a
            self.replace_pane(sentinel, pane_a);
            return false;
        }
        // Replace sentinel with pane_b
        self.replace_pane(sentinel, pane_b);
        true
    }

    /// Replace a pane ID in the tree. Returns true if found.
    fn replace_pane(&mut self, old_id: Uuid, new_id: Uuid) -> bool {
        match self {
            LayoutNode::Leaf { pane } if *pane == old_id => {
                *pane = new_id;
                true
            }
            LayoutNode::Split { a, b, .. } => {
                a.replace_pane(old_id, new_id) || b.replace_pane(old_id, new_id)
            }
            _ => false,
        }
    }

    /// Compute the screen rectangle for each pane given a total area.
    /// Returns a map of PaneId -> Rect.
    pub fn compute_rects(&self, area: Rect) -> Vec<(Uuid, Rect)> {
        match self {
            LayoutNode::Leaf { pane } => vec![(*pane, area)],
            LayoutNode::Split { dir, ratio, a, b } => {
                let (area_a, area_b) = match dir {
                    SplitDirection::Horizontal => {
                        let split_y = area.y + (area.height as f32 * ratio) as u16;
                        let height_a = split_y - area.y;
                        let height_b = area.height - height_a;
                        (
                            Rect { x: area.x, y: area.y, width: area.width, height: height_a },
                            Rect { x: area.x, y: split_y, width: area.width, height: height_b },
                        )
                    }
                    SplitDirection::Vertical => {
                        let split_x = area.x + (area.width as f32 * ratio) as u16;
                        let width_a = split_x - area.x;
                        let width_b = area.width - width_a;
                        (
                            Rect { x: area.x, y: area.y, width: width_a, height: area.height },
                            Rect { x: split_x, y: area.y, width: width_b, height: area.height },
                        )
                    }
                };

                let mut rects = a.compute_rects(area_a);
                rects.extend(b.compute_rects(area_b));
                rects
            }
        }
    }
}

/// A simple rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub fn center_x(&self) -> u16 {
        self.x + self.width / 2
    }

    pub fn center_y(&self) -> u16 {
        self.y + self.height / 2
    }
}
```

### Step 2: Implement directional focus navigation

Directional navigation finds the nearest pane in a given direction using layout geometry. This requires computing screen rects for all panes and then finding the closest neighbor.

In `crates/shux-core/src/navigation.rs`:

```rust
use uuid::Uuid;
use crate::layout::{LayoutNode, Rect};

/// Direction for pane navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// Find the nearest pane in the given direction from the current pane.
/// Uses center-point distance with directional filtering.
pub fn find_neighbor(
    layout: &LayoutNode,
    area: Rect,
    current_pane: Uuid,
    direction: Direction,
) -> Option<Uuid> {
    let rects = layout.compute_rects(area);

    let current_rect = rects.iter()
        .find(|(id, _)| *id == current_pane)
        .map(|(_, r)| *r)?;

    let candidates: Vec<_> = rects.iter()
        .filter(|(id, _)| *id != current_pane)
        .filter(|(_, rect)| is_in_direction(&current_rect, rect, direction))
        .collect();

    // Find the nearest candidate by center-point distance
    candidates.iter()
        .min_by(|(_, a), (_, b)| {
            let dist_a = center_distance(&current_rect, a, direction);
            let dist_b = center_distance(&current_rect, b, direction);
            dist_a.partial_cmp(&dist_b).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(id, _)| *id)
}

/// Check if `target` is in the given direction relative to `from`.
fn is_in_direction(from: &Rect, target: &Rect, direction: Direction) -> bool {
    match direction {
        Direction::Left => target.center_x() < from.center_x(),
        Direction::Right => target.center_x() > from.center_x(),
        Direction::Up => target.center_y() < from.center_y(),
        Direction::Down => target.center_y() > from.center_y(),
    }
}

/// Compute a directional distance metric.
/// Primary axis distance is weighted more heavily than cross-axis.
fn center_distance(from: &Rect, to: &Rect, direction: Direction) -> f64 {
    let dx = (to.center_x() as f64) - (from.center_x() as f64);
    let dy = (to.center_y() as f64) - (from.center_y() as f64);

    match direction {
        Direction::Left | Direction::Right => {
            // Primary axis is X, cross-axis is Y
            dx.abs() + dy.abs() * 0.5
        }
        Direction::Up | Direction::Down => {
            // Primary axis is Y, cross-axis is X
            dy.abs() + dx.abs() * 0.5
        }
    }
}

/// Determine the smart split direction based on pane dimensions.
/// Wider than tall -> vertical split; taller than wide -> horizontal split.
/// Tie-breaks to vertical (PRD section 9.1).
pub fn smart_split_direction(width: u16, height: u16) -> crate::layout::SplitDirection {
    // Account for typical terminal aspect ratio: characters are ~2x taller than wide
    // So a 80x24 pane is visually about square. Multiply height by 2 for visual comparison.
    let visual_width = width as u32;
    let visual_height = (height as u32) * 2;

    if visual_width >= visual_height {
        crate::layout::SplitDirection::Vertical
    } else {
        crate::layout::SplitDirection::Horizontal
    }
}
```

### Step 3: Define pane mutation types

In `crates/shux-core/src/pane.rs`:

```rust
use uuid::Uuid;
use crate::layout::SplitDirection;

#[derive(Debug, Clone)]
pub enum PaneCommand {
    Split {
        target_pane_id: Uuid,
        direction: SplitDirection,
        ratio: f32,
        command: Vec<String>,
        cwd: Option<std::path::PathBuf>,
    },
    Focus {
        pane_id: Uuid,
    },
    FocusDirection {
        direction: crate::navigation::Direction,
    },
    Resize {
        pane_id: Uuid,
        /// Width delta in columns (positive = grow right, negative = shrink)
        width_delta: i16,
        /// Height delta in rows (positive = grow down, negative = shrink)
        height_delta: i16,
    },
    Zoom {
        pane_id: Uuid,
    },
    Swap {
        pane_id: Uuid,
        target_pane_id: Uuid,
    },
    Kill {
        pane_id: Uuid,
    },
    Ensure {
        window_id: Uuid,
        command: Vec<String>,
    },
}

#[derive(Debug, Clone)]
pub enum PaneResult {
    Split {
        new_pane_id: Uuid,
        window_id: Uuid,
    },
    Focused {
        pane_id: Uuid,
        previous_pane_id: Option<Uuid>,
    },
    Resized {
        pane_id: Uuid,
    },
    Zoomed {
        pane_id: Uuid,
        is_zoomed: bool,
    },
    Swapped {
        pane_a: Uuid,
        pane_b: Uuid,
    },
    Killed {
        pane_id: Uuid,
    },
    Ensured {
        pane_id: Uuid,
        created: bool,
    },
    Error(PaneError),
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum PaneError {
    #[error("pane not found: {0}")]
    NotFound(Uuid),

    #[error("window not found: {0}")]
    WindowNotFound(Uuid),

    #[error("cannot kill the last pane in a window (kill the window instead)")]
    LastPane,

    #[error("cannot split: resulting pane would be too small (minimum 2 columns x 1 row)")]
    TooSmall,

    #[error("no neighbor pane in direction {0:?}")]
    NoNeighbor(crate::navigation::Direction),

    #[error("cannot swap pane with itself")]
    SwapSelf,

    #[error("panes are not in the same window")]
    CrossWindow,

    #[error("PTY spawn failed: {0}")]
    PtySpawnFailed(String),

    #[error("internal error: {0}")]
    Internal(String),
}
```

### Step 4: Implement pane mutations on SessionGraph

In `crates/shux-core/src/graph.rs`, add pane methods:

```rust
impl SessionGraph {
    /// Split an existing pane, creating a new pane alongside it.
    /// Returns the new pane's ID and the window ID.
    pub fn split_pane(
        &mut self,
        target_pane_id: Uuid,
        direction: SplitDirection,
        ratio: f32,
        command: Vec<String>,
        cwd: Option<std::path::PathBuf>,
    ) -> Result<(Uuid, Uuid), PaneError> {
        // Find the window containing this pane
        let target_pane = self.panes.get(&target_pane_id)
            .ok_or(PaneError::NotFound(target_pane_id))?;
        let window_id = target_pane.window;

        let window = self.windows.get_mut(&window_id)
            .ok_or(PaneError::WindowNotFound(window_id))?;

        // Create the new pane
        let new_pane_id = Uuid::new_v4();
        let new_pane = Pane {
            id: new_pane_id,
            window: window_id,
            title: String::new(),
            auto_title: true,
            cwd: cwd.unwrap_or_else(|| {
                self.panes.get(&target_pane_id)
                    .map(|p| p.cwd.clone())
                    .unwrap_or_default()
            }),
            command,
            exit_status: None,
            restart: RestartPolicy::Never,
            theme: None,
            tags: HashMap::new(),
            version: 1,
        };

        // Insert the new pane into the layout tree
        if !window.layout.split_pane(target_pane_id, new_pane_id, direction, ratio) {
            return Err(PaneError::Internal("failed to split layout".into()));
        }

        // Store the new pane and focus it
        self.panes.insert(new_pane_id, new_pane);
        window.active_pane = new_pane_id;
        window.version += 1;

        Ok((new_pane_id, window_id))
    }

    /// Focus a specific pane within its window.
    pub fn focus_pane(&mut self, pane_id: Uuid) -> Result<Option<Uuid>, PaneError> {
        let pane = self.panes.get(&pane_id)
            .ok_or(PaneError::NotFound(pane_id))?;
        let window_id = pane.window;

        let window = self.windows.get_mut(&window_id)
            .ok_or(PaneError::WindowNotFound(window_id))?;

        let previous = if window.active_pane != pane_id {
            Some(window.active_pane)
        } else {
            None
        };

        window.active_pane = pane_id;
        window.version += 1;
        Ok(previous)
    }

    /// Zoom or unzoom a pane. When zoomed, the pane fills the entire window.
    /// Returns whether the pane is now zoomed.
    pub fn toggle_zoom(
        &mut self,
        pane_id: Uuid,
        zoom_state: &mut Option<ZoomState>,
    ) -> Result<bool, PaneError> {
        let pane = self.panes.get(&pane_id)
            .ok_or(PaneError::NotFound(pane_id))?;
        let window_id = pane.window;

        let window = self.windows.get_mut(&window_id)
            .ok_or(PaneError::WindowNotFound(window_id))?;

        if let Some(saved) = zoom_state.take() {
            // Unzoom: restore saved layout
            if saved.zoomed_pane == pane_id {
                window.layout = saved.saved_layout;
                window.version += 1;
                return Ok(false);
            }
            // Different pane was zoomed; restore and re-zoom this one
            window.layout = saved.saved_layout;
        }

        // Zoom: save current layout and replace with single pane
        if window.layout.pane_count() > 1 {
            *zoom_state = Some(ZoomState {
                zoomed_pane: pane_id,
                saved_layout: window.layout.clone(),
            });
            window.layout = LayoutNode::Leaf { pane: pane_id };
            window.active_pane = pane_id;
            window.version += 1;
            Ok(true)
        } else {
            // Only one pane, nothing to zoom
            Ok(false)
        }
    }

    /// Kill a pane: remove from layout, clean up.
    /// Returns the pane ID. Caller must kill the PTY.
    pub fn kill_pane(&mut self, pane_id: Uuid) -> Result<Uuid, PaneError> {
        let pane = self.panes.get(&pane_id)
            .ok_or(PaneError::NotFound(pane_id))?;
        let window_id = pane.window;

        let window = self.windows.get_mut(&window_id)
            .ok_or(PaneError::WindowNotFound(window_id))?;

        // Cannot kill the last pane
        if window.layout.pane_count() <= 1 {
            return Err(PaneError::LastPane);
        }

        // Remove from layout
        if let Some(new_layout) = window.layout.remove_pane(pane_id) {
            window.layout = new_layout;
        } else {
            return Err(PaneError::Internal("layout removal failed".into()));
        }

        // Update active pane if we killed the focused one
        if window.active_pane == pane_id {
            window.active_pane = window.layout.pane_ids()
                .first()
                .copied()
                .ok_or(PaneError::Internal("no panes after removal".into()))?;
        }

        window.version += 1;
        self.panes.remove(&pane_id);
        Ok(pane_id)
    }

    /// Swap two panes within the same window.
    pub fn swap_panes(
        &mut self,
        pane_a: Uuid,
        pane_b: Uuid,
    ) -> Result<(), PaneError> {
        if pane_a == pane_b {
            return Err(PaneError::SwapSelf);
        }

        let window_a = self.panes.get(&pane_a)
            .ok_or(PaneError::NotFound(pane_a))?.window;
        let window_b = self.panes.get(&pane_b)
            .ok_or(PaneError::NotFound(pane_b))?.window;

        if window_a != window_b {
            return Err(PaneError::CrossWindow);
        }

        let window = self.windows.get_mut(&window_a)
            .ok_or(PaneError::WindowNotFound(window_a))?;

        if !window.layout.swap_panes(pane_a, pane_b) {
            return Err(PaneError::Internal("swap failed in layout".into()));
        }

        window.version += 1;
        Ok(())
    }
}
```

### Step 5: Add pane event types

In `crates/shux-core/src/events.rs`:

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum PaneEvent {
    #[serde(rename = "pane.created")]
    Created {
        pane_id: Uuid,
        window_id: Uuid,
        session_id: Uuid,
        split_from: Option<Uuid>,
        direction: Option<String>,
    },

    #[serde(rename = "pane.focused")]
    Focused {
        pane_id: Uuid,
        previous: Option<Uuid>,
        window_id: Uuid,
    },

    #[serde(rename = "pane.resized")]
    Resized {
        pane_id: Uuid,
        width: u16,
        height: u16,
    },

    #[serde(rename = "pane.zoomed")]
    Zoomed {
        pane_id: Uuid,
        is_zoomed: bool,
        window_id: Uuid,
    },

    #[serde(rename = "pane.swapped")]
    Swapped {
        pane_a: Uuid,
        pane_b: Uuid,
        window_id: Uuid,
    },

    #[serde(rename = "pane.exited")]
    Exited {
        pane_id: Uuid,
        window_id: Uuid,
        exit_code: Option<i32>,
    },
}
```

### Step 6: Implement JSON-RPC handlers for pane operations

In `crates/shux-rpc/src/methods/pane.rs`:

```rust
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct PaneSplitParams {
    pub pane_id: Uuid,
    /// "horizontal" or "vertical"
    pub direction: String,
    #[serde(default = "default_ratio")]
    pub ratio: f32,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
}

fn default_ratio() -> f32 { 0.5 }

#[derive(Debug, Deserialize)]
pub struct PaneFocusParams {
    pub pane_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct PaneResizeParams {
    pub pane_id: Uuid,
    #[serde(default)]
    pub width: Option<i16>,
    #[serde(default)]
    pub height: Option<i16>,
}

#[derive(Debug, Deserialize)]
pub struct PaneZoomParams {
    pub pane_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct PaneSwapParams {
    pub pane_id: Uuid,
    pub target_pane_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct PaneKillParams {
    pub pane_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct PaneEnsureParams {
    pub window_id: Uuid,
    #[serde(default)]
    pub command: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct PaneInfo {
    pub id: Uuid,
    pub window_id: Uuid,
    pub title: String,
    pub cwd: String,
    pub command: Vec<String>,
    pub exit_status: Option<i32>,
    pub is_focused: bool,
    pub version: u64,
}

#[derive(Debug, Serialize)]
pub struct PaneSplitResult {
    pub pane: PaneInfo,
    pub split_from: Uuid,
}

#[derive(Debug, Serialize)]
pub struct PaneZoomResult {
    pub pane_id: Uuid,
    pub is_zoomed: bool,
}

pub async fn handle_pane_split(
    state: &AppState,
    params: PaneSplitParams,
) -> Result<PaneSplitResult, RpcError> {
    let direction = parse_direction(&params.direction)?;

    let (tx, rx) = tokio::sync::oneshot::channel();
    state.mutation_tx.send(Mutation::Pane(
        PaneCommand::Split {
            target_pane_id: params.pane_id,
            direction,
            ratio: params.ratio,
            command: params.command,
            cwd: params.cwd.map(std::path::PathBuf::from),
        },
        tx,
    )).await.map_err(|_| RpcError::internal("state owner gone"))?;

    match rx.await.map_err(|_| RpcError::internal("dropped"))? {
        PaneResult::Split { new_pane_id, window_id } => {
            let snapshot = state.graph.load();
            let pane = snapshot.panes.get(&new_pane_id)
                .ok_or_else(|| RpcError::internal("pane not in snapshot"))?;
            let window = snapshot.windows.get(&window_id)
                .ok_or_else(|| RpcError::internal("window not in snapshot"))?;
            Ok(PaneSplitResult {
                pane: pane_to_info(pane, window),
                split_from: params.pane_id,
            })
        }
        PaneResult::Error(e) => Err(pane_error_to_rpc(e)),
        _ => Err(RpcError::internal("unexpected result")),
    }
}

// Additional handlers: handle_pane_focus, handle_pane_resize,
// handle_pane_zoom, handle_pane_swap, handle_pane_kill, handle_pane_ensure
// follow the same pattern: deserialize params, send mutation, await result.

fn parse_direction(s: &str) -> Result<SplitDirection, RpcError> {
    match s.to_lowercase().as_str() {
        "horizontal" | "h" => Ok(SplitDirection::Horizontal),
        "vertical" | "v" => Ok(SplitDirection::Vertical),
        _ => Err(RpcError::invalid_params(format!(
            "invalid direction '{}', expected 'horizontal' or 'vertical'", s
        ))),
    }
}
```

### Step 7: Write L3 API contract tests

In `crates/shux-rpc/tests/pane_api.rs`:

```rust
#[tokio::test]
async fn test_pane_split_vertical() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let session = client.call("session.create", serde_json::json!({ "name": "test" })).await.unwrap();
    let pane_id = session["pane_id"].as_str().unwrap();

    let result = client.call("pane.split", serde_json::json!({
        "pane_id": pane_id,
        "direction": "vertical",
    })).await.unwrap();

    assert!(result["pane"]["id"].is_string());
    assert_ne!(result["pane"]["id"].as_str().unwrap(), pane_id);
    assert_eq!(result["split_from"].as_str().unwrap(), pane_id);
}

#[tokio::test]
async fn test_pane_split_horizontal() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let session = client.call("session.create", serde_json::json!({ "name": "test" })).await.unwrap();
    let pane_id = session["pane_id"].as_str().unwrap();

    let result = client.call("pane.split", serde_json::json!({
        "pane_id": pane_id,
        "direction": "horizontal",
    })).await.unwrap();

    assert!(result["pane"]["id"].is_string());
}

#[tokio::test]
async fn test_pane_focus() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let session = client.call("session.create", serde_json::json!({ "name": "test" })).await.unwrap();
    let pane_id = session["pane_id"].as_str().unwrap();

    let split = client.call("pane.split", serde_json::json!({
        "pane_id": pane_id, "direction": "vertical"
    })).await.unwrap();
    let new_pane_id = split["pane"]["id"].as_str().unwrap();

    // Focus back to original pane
    let result = client.call("pane.focus", serde_json::json!({
        "pane_id": pane_id,
    })).await.unwrap();

    assert_eq!(result["pane_id"].as_str().unwrap(), pane_id);
    assert_eq!(result["previous_pane_id"].as_str().unwrap(), new_pane_id);
}

#[tokio::test]
async fn test_pane_zoom_toggle() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let session = client.call("session.create", serde_json::json!({ "name": "test" })).await.unwrap();
    let pane_id = session["pane_id"].as_str().unwrap();

    // Create a second pane so zoom is meaningful
    client.call("pane.split", serde_json::json!({
        "pane_id": pane_id, "direction": "vertical"
    })).await.unwrap();

    // Zoom
    let zoom_on = client.call("pane.zoom", serde_json::json!({
        "pane_id": pane_id,
    })).await.unwrap();
    assert_eq!(zoom_on["is_zoomed"], true);

    // Unzoom
    let zoom_off = client.call("pane.zoom", serde_json::json!({
        "pane_id": pane_id,
    })).await.unwrap();
    assert_eq!(zoom_off["is_zoomed"], false);
}

#[tokio::test]
async fn test_pane_swap() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let session = client.call("session.create", serde_json::json!({ "name": "test" })).await.unwrap();
    let pane_a = session["pane_id"].as_str().unwrap();

    let split = client.call("pane.split", serde_json::json!({
        "pane_id": pane_a, "direction": "vertical"
    })).await.unwrap();
    let pane_b = split["pane"]["id"].as_str().unwrap();

    let result = client.call("pane.swap", serde_json::json!({
        "pane_id": pane_a,
        "target_pane_id": pane_b,
    })).await.unwrap();

    assert_eq!(result["pane_a"].as_str().unwrap(), pane_a);
    assert_eq!(result["pane_b"].as_str().unwrap(), pane_b);
}

#[tokio::test]
async fn test_pane_kill() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let session = client.call("session.create", serde_json::json!({ "name": "test" })).await.unwrap();
    let pane_id = session["pane_id"].as_str().unwrap();

    let split = client.call("pane.split", serde_json::json!({
        "pane_id": pane_id, "direction": "vertical"
    })).await.unwrap();
    let new_pane_id = split["pane"]["id"].as_str().unwrap();

    client.call("pane.kill", serde_json::json!({
        "pane_id": new_pane_id,
    })).await.unwrap();

    // Verify the window is back to 1 pane
    let window_id = session["window_id"].as_str().unwrap();
    // (Verify via window info or pane list)
}

#[tokio::test]
async fn test_pane_kill_last_pane_fails() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let session = client.call("session.create", serde_json::json!({ "name": "test" })).await.unwrap();
    let pane_id = session["pane_id"].as_str().unwrap();

    let err = client.call("pane.kill", serde_json::json!({
        "pane_id": pane_id,
    })).await.unwrap_err();

    // Cannot kill last pane
    assert!(err.message.contains("last pane"));
}

#[tokio::test]
async fn test_pane_swap_self_fails() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let session = client.call("session.create", serde_json::json!({ "name": "test" })).await.unwrap();
    let pane_id = session["pane_id"].as_str().unwrap();

    let err = client.call("pane.swap", serde_json::json!({
        "pane_id": pane_id, "target_pane_id": pane_id,
    })).await.unwrap_err();

    assert!(err.message.contains("itself"));
}
```

### Step 8: Unit tests for layout and navigation

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_pane_creates_two_children() {
        let pane_a = Uuid::new_v4();
        let pane_b = Uuid::new_v4();
        let mut layout = LayoutNode::Leaf { pane: pane_a };

        assert!(layout.split_pane(pane_a, pane_b, SplitDirection::Vertical, 0.5));
        assert_eq!(layout.pane_count(), 2);
        assert!(layout.pane_ids().contains(&pane_a));
        assert!(layout.pane_ids().contains(&pane_b));
    }

    #[test]
    fn test_remove_pane_sibling_bubbles_up() {
        let pane_a = Uuid::new_v4();
        let pane_b = Uuid::new_v4();
        let mut layout = LayoutNode::Leaf { pane: pane_a };
        layout.split_pane(pane_a, pane_b, SplitDirection::Vertical, 0.5);

        let result = layout.remove_pane(pane_b);
        assert!(result.is_some());
        let new_layout = result.unwrap();
        assert_eq!(new_layout.pane_count(), 1);
        assert!(matches!(new_layout, LayoutNode::Leaf { pane } if pane == pane_a));
    }

    #[test]
    fn test_ratio_clamped_to_bounds() {
        let pane_a = Uuid::new_v4();
        let pane_b = Uuid::new_v4();
        let mut layout = LayoutNode::Leaf { pane: pane_a };
        layout.split_pane(pane_a, pane_b, SplitDirection::Vertical, 0.01);

        // Ratio should be clamped to 0.05
        if let LayoutNode::Split { ratio, .. } = &layout {
            assert!(*ratio >= 0.05);
        }
    }

    #[test]
    fn test_smart_split_wide_pane_is_vertical() {
        assert_eq!(
            smart_split_direction(120, 30),
            SplitDirection::Vertical
        );
    }

    #[test]
    fn test_smart_split_tall_pane_is_horizontal() {
        assert_eq!(
            smart_split_direction(40, 60),
            SplitDirection::Horizontal
        );
    }

    #[test]
    fn test_swap_panes() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut layout = LayoutNode::Leaf { pane: a };
        layout.split_pane(a, b, SplitDirection::Vertical, 0.5);

        assert!(layout.swap_panes(a, b));
        // After swap, the layout positions are reversed
    }

    #[test]
    fn test_directional_focus_finds_neighbor() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut layout = LayoutNode::Leaf { pane: a };
        layout.split_pane(a, b, SplitDirection::Vertical, 0.5);

        let area = Rect { x: 0, y: 0, width: 80, height: 24 };
        let neighbor = find_neighbor(&layout, area, a, Direction::Right);
        assert_eq!(neighbor, Some(b));

        let neighbor_left = find_neighbor(&layout, area, b, Direction::Left);
        assert_eq!(neighbor_left, Some(a));
    }
}
```

---

## Verification

### Functional

```bash
# Create a session and split panes
cargo run -p shux -- new -s test --no-attach
# (pane CLI wrappers are introduced later; use API calls in this task)
echo '{"jsonrpc":"2.0","id":1,"method":"pane.split","params":{"pane_id":"<id>","direction":"vertical"}}' | shux api call
echo '{"jsonrpc":"2.0","id":2,"method":"pane.split","params":{"pane_id":"<id>","direction":"horizontal"}}' | shux api call

# Focus navigation (via API)
echo '{"jsonrpc":"2.0","id":1,"method":"pane.focus","params":{"pane_id":"<id>"}}' | shux api call

# Zoom toggle
echo '{"jsonrpc":"2.0","id":3,"method":"pane.zoom","params":{"pane_id":"<id>"}}' | shux api call   # zoom
echo '{"jsonrpc":"2.0","id":4,"method":"pane.zoom","params":{"pane_id":"<id>"}}' | shux api call   # unzoom

# Kill a pane
echo '{"jsonrpc":"2.0","id":5,"method":"pane.kill","params":{"pane_id":"<id>"}}' | shux api call   # sibling expands
```

### Tests

```bash
# Unit tests for layout tree operations
cargo nextest run -p shux-core --lib -- layout

# Unit tests for navigation
cargo nextest run -p shux-core --lib -- navigation

# API contract tests
cargo nextest run -p shux-rpc --test pane_api

# All tests
cargo nextest run --workspace

# Clippy
cargo clippy --workspace --all-targets -- -D warnings
```

---

## Completion Criteria

- [ ] `pane.split` splits a pane (H or V), spawns PTY for new pane, updates LayoutTree
- [ ] `pane.focus` updates the window's active_pane
- [ ] Directional focus navigation works (left, right, up, down) using layout geometry
- [ ] `pane.resize` adjusts the split ratio of the parent Split node, clamped to [0.05, 0.95]
- [ ] `pane.zoom` toggles zoom: saves layout, replaces with single pane, restores on re-toggle
- [ ] `pane.swap` swaps two panes in the layout tree within the same window
- [ ] `pane.kill` removes a pane from layout (sibling bubbles up), kills PTY, cannot kill last pane
- [ ] `pane.ensure` creates a pane in a window if none with matching command exists
- [ ] Smart split direction: wider->vertical, taller->horizontal (accounting for character aspect ratio)
- [ ] Split pane operation completes within p50 <= 25ms
- [ ] Events emitted: pane.created, pane.focused, pane.resized, pane.zoomed, pane.exited
- [ ] Layout tree unit tests: split, remove (sibling bubbles up), resize, swap, compute_rects
- [ ] Navigation unit tests: directional focus with various layouts
- [ ] L3 API contract tests pass for all pane operations
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] `cargo nextest run --workspace` passes

---

## Commit Message

```
feat: implement pane operations with layout tree mutations

- Add pane.split, pane.focus, pane.resize, pane.zoom, pane.swap,
  pane.kill, pane.ensure JSON-RPC methods
- Extend LayoutTree with split, remove, resize, swap operations
- Implement directional focus navigation using layout geometry
- Smart split direction based on pane aspect ratio (Alt+Enter)
- Zoom toggles save/restore previous layout state
- Sibling bubbles up when a pane is removed from the layout
- Unit tests for layout tree and navigation, L3 API contract tests
```

---

## Session Protocol

1. **Before starting:** Verify tasks 014 and 003 are complete. Window CRUD must work. The LayoutTree from task 003 must have its basic structure. Read `CLAUDE.md`.
2. **During:** Start with layout tree operations (step 1) as they are pure data structures with no I/O. Write and run unit tests early. Then build the mutation types, graph methods, RPC handlers, and API tests in order.
3. **Key patterns:**
   - The layout tree is a recursive data structure. Use `Box<LayoutNode>` for the split children. Mutations are done in-place with `&mut self`.
   - The `remove_pane` operation must "bubble up" the sibling: when a `Split { a, b }` has one child removed, the other child replaces the entire Split node. This is critical for maintaining a clean tree.
   - Zoom uses a side-channel `ZoomState` stored per-window. The zoomed layout replaces the window's layout; the saved layout is restored on unzoom.
   - PTY spawn for new panes is done by the caller (state-owner task) after the graph mutation succeeds, not inside the graph mutation itself.
4. **After:** Run full verification. Update `docs/PROGRESS.md`. Verify tasks 016, 017, and 018 can build on this.
