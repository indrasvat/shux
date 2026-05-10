# 020 — Mouse Support

**Status:** Done (2026-05-08 spike). crossterm `Event::Mouse` forwarded as `AttachClientFrame::Mouse { kind, button, col, row }`. Daemon implements `pane_at(col, row)` for click-to-focus and `border_at()` + `DragState` for drag-to-resize. Scroll variants reserved for copy mode (task 021). Verified via `test_017_full_verify.py` V5 (synthetic SGR-1006 click moves focus across border).
**Depends On:** 017
**Parallelizable With:** 018, 019, 022

---

## Problem

A modern terminal multiplexer must support mouse interaction. Users expect to click on a pane to focus it, drag borders to resize splits, and use the scroll wheel to browse scrollback. Without mouse support, shux forces keyboard-only interaction for spatial operations that are naturally mouse-driven (like resizing a split to an approximate ratio). The PRD explicitly requires mouse support as a P0 feature with three core behaviors: click-to-focus, drag-to-resize, and scroll-for-scrollback.

Critically, mouse events must be intelligently routed: when the user clicks on a border or scrolls while hovering over a pane, shux consumes the event. When the user clicks or scrolls inside a pane that is running a mouse-aware application (vim, htop, less with mouse), the mouse event must be forwarded to the PTY as an escape sequence so the application can handle it. This dual-routing is essential for not breaking programs that use mouse input.

## PRD Reference

- **SS 6.1** (Mouse support: click to focus, drag to resize, scroll for scrollback)
- **SS 6.1** (Mouse support toggleable globally, enabled by default)
- **SS 10.2** (`[ui] mouse = true` configuration)
- **SS 2.4** point 3 (Graded keybindings -- mouse as complementary input)

---

## Files to Create

- `crates/shux-ui/src/mouse.rs` -- Mouse event processing, hit testing, drag state machine
- `crates/shux-ui/src/mouse_encode.rs` -- Encoding mouse events as terminal escape sequences for PTY forwarding
- `crates/shux-ui/tests/mouse_test.rs` -- Unit and integration tests for mouse handling

## Files to Modify

- `crates/shux-ui/src/compositor.rs` -- Add hit-testing methods for pane regions and borders
- `crates/shux-ui/src/lib.rs` -- Register mouse module
- `crates/shux-ui/src/event_loop.rs` -- Route mouse events through mouse handler
- `crates/shux-ui/Cargo.toml` -- Ensure crossterm mouse features are enabled

---

## Execution Steps

### Step 1: Enable crossterm mouse capture

When the TUI client starts, enable mouse capture. When it exits (detach, quit), disable it. This is done via crossterm's `EnableMouseCapture` / `DisableMouseCapture` commands.

```rust
use crossterm::{
    event::{EnableMouseCapture, DisableMouseCapture},
    execute,
};
use std::io::stdout;

pub fn enable_mouse() -> std::io::Result<()> {
    execute!(stdout(), EnableMouseCapture)
}

pub fn disable_mouse() -> std::io::Result<()> {
    execute!(stdout(), DisableMouseCapture)
}
```

Mouse capture should be conditional on the `[ui] mouse = true` config setting. When mouse is disabled, no mouse events are captured and all mouse interaction is ignored.

### Step 2: Implement the hit-testing system

The compositor knows the exact pixel/cell geometry of every pane and every border. Build a hit-testing module that, given a (column, row) coordinate, determines what the mouse is hovering over.

```rust
// crates/shux-ui/src/mouse.rs

use uuid::Uuid;

/// What the mouse is pointing at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitTarget {
    /// Mouse is over a pane's content area.
    PaneContent {
        pane_id: Uuid,
        /// Local coordinates within the pane (0-based).
        local_col: u16,
        local_row: u16,
    },
    /// Mouse is over a horizontal border between two panes.
    HorizontalBorder {
        /// The pane above the border.
        above: Uuid,
        /// The pane below the border.
        below: Uuid,
        /// The layout node that owns this split.
        split_node_id: usize,
    },
    /// Mouse is over a vertical border between two panes.
    VerticalBorder {
        /// The pane to the left of the border.
        left: Uuid,
        /// The pane to the right of the border.
        right: Uuid,
        /// The layout node that owns this split.
        split_node_id: usize,
    },
    /// Mouse is over the status bar area.
    StatusBar { col: u16 },
    /// Mouse is outside any known region (shouldn't happen normally).
    None,
}

/// Perform hit testing against the current layout.
pub fn hit_test(
    col: u16,
    row: u16,
    layout: &LayoutGeometry,
    status_bar_row: Option<u16>,
) -> HitTarget {
    // Check status bar first (typically the last row).
    if let Some(sb_row) = status_bar_row {
        if row == sb_row {
            return HitTarget::StatusBar { col };
        }
    }

    // Check pane borders (1-cell-wide boundaries between panes).
    if let Some(border) = layout.border_at(col, row) {
        return border;
    }

    // Check pane content areas.
    if let Some(pane_hit) = layout.pane_at(col, row) {
        return pane_hit;
    }

    HitTarget::None
}
```

### Step 3: Add hit-testing methods to compositor/layout

Extend the compositor's layout geometry to support efficient hit testing. The layout stores a list of `PaneRect` structures describing each pane's screen region.

```rust
// In crates/shux-ui/src/compositor.rs (additions)

#[derive(Debug, Clone)]
pub struct PaneRect {
    pub pane_id: Uuid,
    pub x: u16,      // left column (inclusive)
    pub y: u16,      // top row (inclusive)
    pub width: u16,
    pub height: u16,
}

#[derive(Debug, Clone)]
pub struct BorderSegment {
    pub orientation: BorderOrientation,
    pub split_node_id: usize,
    pub pane_a: Uuid,  // above/left
    pub pane_b: Uuid,  // below/right
    pub x: u16,
    pub y: u16,
    pub length: u16,
}

#[derive(Debug, Clone, Copy)]
pub enum BorderOrientation {
    Horizontal,
    Vertical,
}

pub struct LayoutGeometry {
    pub pane_rects: Vec<PaneRect>,
    pub borders: Vec<BorderSegment>,
}

impl LayoutGeometry {
    /// Find the pane at a given screen coordinate.
    pub fn pane_at(&self, col: u16, row: u16) -> Option<HitTarget> {
        for rect in &self.pane_rects {
            if col >= rect.x
                && col < rect.x + rect.width
                && row >= rect.y
                && row < rect.y + rect.height
            {
                return Some(HitTarget::PaneContent {
                    pane_id: rect.pane_id,
                    local_col: col - rect.x,
                    local_row: row - rect.y,
                });
            }
        }
        None
    }

    /// Find the border at a given screen coordinate.
    pub fn border_at(&self, col: u16, row: u16) -> Option<HitTarget> {
        for border in &self.borders {
            let hit = match border.orientation {
                BorderOrientation::Horizontal => {
                    row == border.y && col >= border.x && col < border.x + border.length
                }
                BorderOrientation::Vertical => {
                    col == border.x && row >= border.y && row < border.y + border.length
                }
            };
            if hit {
                return Some(match border.orientation {
                    BorderOrientation::Horizontal => HitTarget::HorizontalBorder {
                        above: border.pane_a,
                        below: border.pane_b,
                        split_node_id: border.split_node_id,
                    },
                    BorderOrientation::Vertical => HitTarget::VerticalBorder {
                        left: border.pane_a,
                        right: border.pane_b,
                        split_node_id: border.split_node_id,
                    },
                });
            }
        }
        None
    }
}
```

### Step 4: Implement click-to-focus

When the user clicks inside a pane's content area, focus that pane. If it is already focused, do nothing. Click events that land on pane content of mouse-aware applications should also be forwarded to the PTY.

```rust
use crossterm::event::{MouseEvent, MouseEventKind, MouseButton};

pub struct MouseHandler {
    /// Current drag state (None if not dragging).
    drag_state: Option<DragState>,
    /// Whether mouse support is enabled.
    enabled: bool,
}

impl MouseHandler {
    pub fn new(enabled: bool) -> Self {
        Self {
            drag_state: None,
            enabled,
        }
    }

    pub async fn handle_mouse(
        &mut self,
        event: MouseEvent,
        layout: &LayoutGeometry,
        ctx: &mut UiContext,
    ) -> MouseResult {
        if !self.enabled {
            return MouseResult::Ignored;
        }

        let target = hit_test(event.column, event.row, layout, ctx.status_bar_row());

        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.handle_left_click(target, event, ctx).await
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                self.handle_drag(event, layout, ctx).await
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.handle_drag_end(ctx).await
            }
            MouseEventKind::ScrollUp => {
                self.handle_scroll(target, ScrollDirection::Up, ctx).await
            }
            MouseEventKind::ScrollDown => {
                self.handle_scroll(target, ScrollDirection::Down, ctx).await
            }
            _ => MouseResult::Ignored,
        }
    }

    async fn handle_left_click(
        &mut self,
        target: HitTarget,
        event: MouseEvent,
        ctx: &mut UiContext,
    ) -> MouseResult {
        match target {
            HitTarget::PaneContent { pane_id, local_col, local_row } => {
                // Focus the pane if it's not already focused.
                if ctx.active_pane_id() != pane_id {
                    ctx.focus_pane(pane_id).await;
                }
                // Forward click to PTY if the pane's application uses mouse.
                if ctx.pane_uses_mouse(pane_id) {
                    let encoded = encode_mouse_click(
                        MouseButton::Left,
                        local_col,
                        local_row,
                        ctx.pane_mouse_protocol(pane_id),
                    );
                    ctx.send_to_pty(pane_id, &encoded).await;
                }
                MouseResult::Consumed
            }
            HitTarget::HorizontalBorder { split_node_id, .. }
            | HitTarget::VerticalBorder { split_node_id, .. } => {
                // Start a border drag.
                self.drag_state = Some(DragState {
                    split_node_id,
                    start_col: event.column,
                    start_row: event.row,
                    last_col: event.column,
                    last_row: event.row,
                    orientation: match target {
                        HitTarget::HorizontalBorder { .. } => BorderOrientation::Horizontal,
                        HitTarget::VerticalBorder { .. } => BorderOrientation::Vertical,
                        _ => unreachable!(),
                    },
                });
                MouseResult::Consumed
            }
            HitTarget::StatusBar { col } => {
                ctx.handle_status_bar_click(col).await;
                MouseResult::Consumed
            }
            HitTarget::None => MouseResult::Ignored,
        }
    }
}
```

### Step 5: Implement border drag to resize

When the user presses the mouse button on a border and drags, update the split ratio in real time. The ratio change is proportional to the mouse movement.

```rust
#[derive(Debug, Clone)]
pub struct DragState {
    pub split_node_id: usize,
    pub start_col: u16,
    pub start_row: u16,
    pub last_col: u16,
    pub last_row: u16,
    pub orientation: BorderOrientation,
}

impl MouseHandler {
    async fn handle_drag(
        &mut self,
        event: MouseEvent,
        layout: &LayoutGeometry,
        ctx: &mut UiContext,
    ) -> MouseResult {
        let drag = match &mut self.drag_state {
            Some(d) => d,
            None => return MouseResult::Ignored,
        };

        let delta = match drag.orientation {
            BorderOrientation::Horizontal => {
                // Vertical movement (row delta) controls horizontal split.
                event.row as i32 - drag.last_row as i32
            }
            BorderOrientation::Vertical => {
                // Horizontal movement (col delta) controls vertical split.
                event.column as i32 - drag.last_col as i32
            }
        };

        if delta == 0 {
            return MouseResult::Consumed;
        }

        // Compute the total dimension of the split region.
        let total_size = layout.split_total_size(drag.split_node_id, drag.orientation);
        if total_size == 0 {
            return MouseResult::Consumed;
        }

        // Convert pixel delta to ratio delta.
        let ratio_delta = delta as f32 / total_size as f32;

        // Apply the ratio change, clamping to [0.05, 0.95].
        ctx.adjust_split_ratio(drag.split_node_id, ratio_delta).await;

        drag.last_col = event.column;
        drag.last_row = event.row;

        MouseResult::Consumed
    }

    async fn handle_drag_end(&mut self, ctx: &mut UiContext) -> MouseResult {
        if self.drag_state.take().is_some() {
            ctx.request_redraw();
            MouseResult::Consumed
        } else {
            MouseResult::Ignored
        }
    }
}
```

### Step 6: Implement scroll wheel for scrollback

When the scroll wheel is used while hovering over a pane, scroll that pane's scrollback buffer. If the pane's application is mouse-aware, forward the scroll event to the PTY instead.

```rust
#[derive(Debug, Clone, Copy)]
pub enum ScrollDirection {
    Up,
    Down,
}

impl MouseHandler {
    async fn handle_scroll(
        &self,
        target: HitTarget,
        direction: ScrollDirection,
        ctx: &mut UiContext,
    ) -> MouseResult {
        match target {
            HitTarget::PaneContent { pane_id, local_col, local_row } => {
                if ctx.pane_uses_mouse(pane_id) {
                    // Forward scroll to PTY as mouse escape sequence.
                    let button = match direction {
                        ScrollDirection::Up => 64,   // Scroll up button code
                        ScrollDirection::Down => 65,  // Scroll down button code
                    };
                    let encoded = encode_mouse_scroll(
                        button,
                        local_col,
                        local_row,
                        ctx.pane_mouse_protocol(pane_id),
                    );
                    ctx.send_to_pty(pane_id, &encoded).await;
                } else {
                    // Scroll the pane's scrollback.
                    let lines = 3; // Default scroll step.
                    match direction {
                        ScrollDirection::Up => ctx.scroll_pane_up(pane_id, lines).await,
                        ScrollDirection::Down => ctx.scroll_pane_down(pane_id, lines).await,
                    }
                }
                MouseResult::Consumed
            }
            _ => MouseResult::Ignored,
        }
    }
}

#[derive(Debug)]
pub enum MouseResult {
    Consumed,
    Ignored,
}
```

### Step 7: Implement mouse event encoding for PTY forwarding

When mouse events need to be forwarded to the PTY (because the pane's application uses mouse), encode them according to the terminal's mouse protocol. The main protocols are X10, Normal (X11), SGR, and URxvt.

```rust
// crates/shux-ui/src/mouse_encode.rs

/// Mouse protocol modes that the pane's application may have enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseProtocol {
    /// No mouse protocol enabled -- application does not use mouse.
    None,
    /// X10 compatibility mode (press only).
    X10,
    /// Normal tracking (VT200, press + release).
    Normal,
    /// SGR extended mode (supports coordinates > 223).
    Sgr,
    /// URxvt extended mode.
    Urxvt,
}

/// Encode a mouse press event for the given protocol.
pub fn encode_mouse_press(
    button: u8,
    col: u16,
    row: u16,
    protocol: MouseProtocol,
) -> Vec<u8> {
    match protocol {
        MouseProtocol::None => vec![],
        MouseProtocol::X10 | MouseProtocol::Normal => {
            // \x1b[M <button+32> <col+33> <row+33>
            // This encoding supports coordinates up to 222.
            let cb = button + 32;
            let cx = (col + 33).min(255) as u8;
            let cy = (row + 33).min(255) as u8;
            vec![0x1b, b'[', b'M', cb, cx, cy]
        }
        MouseProtocol::Sgr => {
            // \x1b[< <button> ; <col+1> ; <row+1> M
            format!("\x1b[<{};{};{}M", button, col + 1, row + 1)
                .into_bytes()
        }
        MouseProtocol::Urxvt => {
            // \x1b[ <button+32> ; <col+1> ; <row+1> M
            format!("\x1b[{};{};{}M", button + 32, col + 1, row + 1)
                .into_bytes()
        }
    }
}

/// Encode a mouse release event (SGR uses 'm' instead of 'M').
pub fn encode_mouse_release(
    button: u8,
    col: u16,
    row: u16,
    protocol: MouseProtocol,
) -> Vec<u8> {
    match protocol {
        MouseProtocol::None | MouseProtocol::X10 => vec![],
        MouseProtocol::Normal => {
            // Release is button 3 in normal mode.
            let cb = 3 + 32;
            let cx = (col + 33).min(255) as u8;
            let cy = (row + 33).min(255) as u8;
            vec![0x1b, b'[', b'M', cb, cx, cy]
        }
        MouseProtocol::Sgr => {
            // SGR uses lowercase 'm' for release.
            format!("\x1b[<{};{};{}m", button, col + 1, row + 1)
                .into_bytes()
        }
        MouseProtocol::Urxvt => {
            // URxvt doesn't distinguish release well; use normal release.
            let cb = 3 + 32;
            format!("\x1b[{};{};{}M", cb, col + 1, row + 1)
                .into_bytes()
        }
    }
}

/// Encode a scroll event.
pub fn encode_mouse_scroll(
    button: u8,  // 64 = up, 65 = down
    col: u16,
    row: u16,
    protocol: MouseProtocol,
) -> Vec<u8> {
    encode_mouse_press(button, col, row, protocol)
}
```

### Step 8: Track mouse protocol mode per pane

The VT parser (from task 005) must detect when applications enable/disable mouse tracking via DECSET/DECRST escape sequences. These modes are stored per-pane and queried by the mouse handler.

```rust
/// Mouse tracking modes set by the application via DECSET/DECRST.
/// Tracked per-pane in the VirtualTerminal state.
#[derive(Debug, Clone, Default)]
pub struct PaneMouseState {
    /// Application has enabled mouse tracking (any mode).
    pub tracking_enabled: bool,
    /// The specific protocol in use.
    pub protocol: MouseProtocol,
    /// Mouse focus events (mode 1004).
    pub focus_events: bool,
}

impl PaneMouseState {
    /// Called when VT parser encounters DECSET for mouse modes.
    pub fn handle_decset(&mut self, mode: u16) {
        match mode {
            9 => {
                // X10 mouse press tracking.
                self.tracking_enabled = true;
                self.protocol = MouseProtocol::X10;
            }
            1000 => {
                // Normal tracking (press + release).
                self.tracking_enabled = true;
                self.protocol = MouseProtocol::Normal;
            }
            1002 => {
                // Button-event tracking (press + release + drag).
                self.tracking_enabled = true;
                self.protocol = MouseProtocol::Normal;
            }
            1003 => {
                // Any-event tracking (all motion).
                self.tracking_enabled = true;
                self.protocol = MouseProtocol::Normal;
            }
            1004 => {
                // Focus events.
                self.focus_events = true;
            }
            1006 => {
                // SGR extended mode.
                self.protocol = MouseProtocol::Sgr;
            }
            1015 => {
                // URxvt extended mode.
                self.protocol = MouseProtocol::Urxvt;
            }
            _ => {}
        }
    }

    /// Called when VT parser encounters DECRST for mouse modes.
    pub fn handle_decrst(&mut self, mode: u16) {
        match mode {
            9 | 1000 | 1002 | 1003 => {
                self.tracking_enabled = false;
                self.protocol = MouseProtocol::None;
            }
            1004 => {
                self.focus_events = false;
            }
            1006 => {
                if self.protocol == MouseProtocol::Sgr {
                    self.protocol = MouseProtocol::Normal;
                }
            }
            1015 => {
                if self.protocol == MouseProtocol::Urxvt {
                    self.protocol = MouseProtocol::Normal;
                }
            }
            _ => {}
        }
    }
}
```

### Step 9: Integrate mouse handler into the event loop

Route crossterm mouse events through the mouse handler in the main event loop.

```rust
// In crates/shux-ui/src/event_loop.rs

use crossterm::event::Event;

// Inside the main event loop:
match event {
    Event::Key(key) => {
        handle_key_event(key, &mut prefix, &mut ctx).await;
    }
    Event::Mouse(mouse) => {
        if mouse_enabled {
            mouse_handler.handle_mouse(mouse, &layout_geometry, &mut ctx).await;
        }
    }
    Event::Resize(cols, rows) => {
        // Handle terminal resize.
        ctx.handle_resize(cols, rows).await;
    }
    _ => {}
}
```

### Step 10: Implement mouse toggle via config

The mouse can be toggled globally via `[ui] mouse = true/false`. When live config reload (task 023) changes this value, mouse capture is enabled or disabled dynamically.

```rust
impl MouseHandler {
    pub fn set_enabled(&mut self, enabled: bool) -> std::io::Result<()> {
        if enabled && !self.enabled {
            enable_mouse()?;
        } else if !enabled && self.enabled {
            disable_mouse()?;
            // Cancel any in-progress drag.
            self.drag_state = None;
        }
        self.enabled = enabled;
        Ok(())
    }
}
```

### Step 11: Handle edge cases

Several edge cases need careful handling:

- **Drag across pane boundaries**: If the user starts dragging a border and moves the mouse off it, continue tracking the drag until mouse-up.
- **Scroll on border**: Ignore scroll events on borders.
- **Right-click / middle-click**: Currently ignored by shux; could be extended later for context menus.
- **Multiple monitors / tmux-in-shux**: Mouse coordinates are relative to the shux terminal window; no special handling needed.
- **Status bar clicks**: Route to status bar handler (for future use by status bar plugin, task 048).

```rust
impl MouseHandler {
    /// Called when the terminal is resized to invalidate any cached geometry.
    pub fn handle_resize(&mut self) {
        self.drag_state = None;
    }
}
```

---

## Verification

### Functional

```bash
# Build the project
cargo build --workspace

# Run with a multi-pane layout
cargo run -p shux -- new -s test
# Split into 4 panes using keyboard or API

# Mouse tests:
# 1. Click on an unfocused pane -- it should become focused (border color changes)
# 2. Click on a border between two panes -- cursor should change to resize indicator
# 3. Drag a border -- split ratio should update in real time
# 4. Release drag -- final ratio should be committed
# 5. Scroll wheel over a pane -- scrollback should move up/down
# 6. Start vim in one pane, click inside it -- vim should respond to the click
# 7. Scroll in vim pane -- vim should scroll (not shux scrollback)
# 8. Exit vim, scroll in the same pane -- now shux scrollback should scroll
# 9. Toggle mouse off via config -- all mouse events should be ignored
# 10. Toggle mouse back on -- mouse interaction restored
```

### Tests

```bash
# Unit tests for hit testing
cargo nextest run -p shux-ui --lib mouse

# Unit tests for mouse encoding
cargo nextest run -p shux-ui --lib mouse_encode

# Integration tests
cargo nextest run -p shux-ui --test mouse_test

# Test scenarios:
# - Hit testing correctly identifies pane content, borders, status bar, empty space
# - Click-to-focus changes active pane
# - Drag state machine: start, move, end
# - Split ratio clamped to [0.05, 0.95] during drag
# - Scroll events route to scrollback or PTY based on mouse mode
# - Mouse encoding produces correct escape sequences for X10, Normal, SGR, URxvt
# - DECSET/DECRST correctly toggles per-pane mouse state
# - Mouse handler respects enabled/disabled flag
```

---

## Completion Criteria

- [ ] Mouse capture enabled on TUI start, disabled on exit (via crossterm)
- [ ] Click on pane content focuses that pane
- [ ] Click on focused pane with mouse-aware application forwards event to PTY
- [ ] Border drag resizes split ratio in real time
- [ ] Split ratio clamped to [0.05, 0.95] during drag (PRD layout invariant)
- [ ] Drag release commits the final ratio
- [ ] Scroll wheel over non-mouse-aware pane scrolls scrollback
- [ ] Scroll wheel over mouse-aware pane forwards scroll event to PTY
- [ ] Mouse encoding correct for X10, Normal, SGR, URxvt protocols
- [ ] Per-pane mouse mode tracked via DECSET/DECRST from VT parser
- [ ] Mouse globally toggleable via `[ui] mouse` config (default: enabled)
- [ ] Status bar clicks routed to status bar handler (stub for task 048)
- [ ] No mouse events forwarded to PTY when consumed by shux (borders, scrollback)
- [ ] Drag state cancelled on terminal resize
- [ ] Unit tests for hit testing, encoding, and state machine
- [ ] Integration tests for end-to-end mouse interaction

---

## Commit Message

```
feat(ui): implement mouse support (click-to-focus, drag-resize, scroll)

- Enable crossterm mouse capture with global toggle (PRD §6.1)
- Implement hit-testing system for pane content, borders, status bar
- Click-to-focus: clicking pane content focuses it
- Border drag: drag to resize split ratios in real time
- Scroll wheel: scrollback for normal panes, PTY forward for mouse-aware
- Per-pane mouse protocol tracking (X10, Normal, SGR, URxvt)
- Mouse event encoding for PTY forwarding to mouse-aware applications
```

---

## Session Protocol

1. **Before starting:** Read task 017 (multi-pane rendering) to understand `LayoutGeometry` and pane rect computation. Read task 005 (VT grid) to understand where DECSET/DECRST modes are tracked. Verify crossterm 0.29 mouse event types.
2. **During:** Implement in order: mouse capture (Step 1), hit testing (Steps 2-3), click handling (Step 4), drag (Step 5), scroll (Step 6), PTY encoding (Step 7), per-pane state (Step 8), event loop integration (Step 9), toggle (Step 10), edge cases (Step 11). Run `cargo check` after each step. Test mouse interaction manually after Steps 4, 5, and 6.
3. **After:** Run full verification suite. Manually test with vim, htop, and less to verify PTY forwarding. Update `docs/PROGRESS.md` (mark 020 done). Update `CLAUDE.md` Learnings with any mouse encoding discoveries.
