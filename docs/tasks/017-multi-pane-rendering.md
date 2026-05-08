# 017 — Multi-Pane Rendering

**Status:** In Progress
**Depends On:** 015, 009
**Parallelizable With:** 020

---

## Problem

The M0 render compositor (task 009) renders a single pane filling the entire terminal area. With pane operations in place (task 015), the compositor must now handle multiple panes: computing layout rects from the LayoutTree, rendering each pane's VirtualTerminal grid into its assigned rect, drawing borders between panes, highlighting the focused pane, supporting zoom mode, and maintaining the p50 <= 8ms render target even with 10+ panes.

Multi-pane rendering is what transforms shux from a "terminal inside a terminal" into a real multiplexer. The visual quality of borders, the accuracy of layout computation, and the performance of diff-based rendering across many panes define the user's moment-to-moment experience.

The compositor must handle several edge cases: panes too small to render usefully (minimum 2 columns x 1 row), terminal resizes that require recomputing all pane rects and resizing VT grids, zoom mode where only one pane fills the window, and per-pane borders with focused/unfocused styling.

## PRD Reference

- **PRD section 4.4 (RenderCompositor)**: Composes VirtualTerminal grids + chrome (borders, status bar, overlays) into per-client output. Diff-based incremental rendering.
- **PRD section 5.2 (Layout tree)**: Binary split tree per window. Arena-allocated. Used to compute screen rects.
- **PRD section 6.1 (pane border style)**: Focused border = accent color, unfocused = dim. Configurable: thin, thick, double, rounded, none.
- **PRD section 10.2 (Config)**: `pane_border_style = "rounded"` (default). Options: "thin", "thick", "double", "rounded", "none".
- **PRD section 14.1 (Performance budgets)**: Keypress to visible update p50 <= 8ms, p99 <= 25ms.

---

## Files to Create

- `crates/shux-ui/src/borders.rs` — Border drawing with multiple styles (thin, thick, double, rounded, none)
- `crates/shux-ui/src/pane_renderer.rs` — Renders a single pane's VT grid content into a screen rect
- `crates/shux-ui/tests/compositor_tests.rs` — Unit tests for layout computation and rendering

## Files to Modify

- `crates/shux-ui/src/compositor.rs` — Extend from single-pane to multi-pane rendering with layout integration
- `crates/shux-ui/src/lib.rs` — Export new modules
- `crates/shux-core/src/layout.rs` — Add border-aware rect computation (account for border pixels)

---

## Execution Steps

### Step 1: Define border styles and characters

Create the border module with all supported border styles. Each style defines the characters used for corners, horizontal lines, vertical lines, and T-intersections.

In `crates/shux-ui/src/borders.rs`:

```rust
/// Border style for pane boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BorderStyle {
    Thin,
    Thick,
    Double,
    Rounded,
    None,
}

impl BorderStyle {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "thin" => BorderStyle::Thin,
            "thick" => BorderStyle::Thick,
            "double" => BorderStyle::Double,
            "rounded" => BorderStyle::Rounded,
            "none" => BorderStyle::None,
            _ => BorderStyle::Rounded, // default
        }
    }
}

/// The character set for a border style.
#[derive(Debug, Clone, Copy)]
pub struct BorderChars {
    pub horizontal: char,
    pub vertical: char,
    pub top_left: char,
    pub top_right: char,
    pub bottom_left: char,
    pub bottom_right: char,
    pub tee_left: char,    // ├
    pub tee_right: char,   // ┤
    pub tee_top: char,     // ┬
    pub tee_bottom: char,  // ┴
    pub cross: char,       // ┼
}

impl BorderStyle {
    pub fn chars(self) -> Option<BorderChars> {
        match self {
            BorderStyle::None => None,
            BorderStyle::Thin => Some(BorderChars {
                horizontal: '─', vertical: '│',
                top_left: '┌', top_right: '┐',
                bottom_left: '└', bottom_right: '┘',
                tee_left: '├', tee_right: '┤',
                tee_top: '┬', tee_bottom: '┴',
                cross: '┼',
            }),
            BorderStyle::Thick => Some(BorderChars {
                horizontal: '━', vertical: '┃',
                top_left: '┏', top_right: '┓',
                bottom_left: '┗', bottom_right: '┛',
                tee_left: '┣', tee_right: '┫',
                tee_top: '┳', tee_bottom: '┻',
                cross: '╋',
            }),
            BorderStyle::Double => Some(BorderChars {
                horizontal: '═', vertical: '║',
                top_left: '╔', top_right: '╗',
                bottom_left: '╚', bottom_right: '╝',
                tee_left: '╠', tee_right: '╣',
                tee_top: '╦', tee_bottom: '╩',
                cross: '╬',
            }),
            BorderStyle::Rounded => Some(BorderChars {
                horizontal: '─', vertical: '│',
                top_left: '╭', top_right: '╮',
                bottom_left: '╰', bottom_right: '╯',
                tee_left: '├', tee_right: '┤',
                tee_top: '┬', tee_bottom: '┴',
                cross: '┼',
            }),
        }
    }
}

/// Colors for pane borders.
#[derive(Debug, Clone, Copy)]
pub struct BorderColors {
    pub focused_fg: crossterm::style::Color,
    pub unfocused_fg: crossterm::style::Color,
}

impl Default for BorderColors {
    fn default() -> Self {
        Self {
            focused_fg: crossterm::style::Color::Rgb { r: 137, g: 180, b: 250 },   // accent blue
            unfocused_fg: crossterm::style::Color::Rgb { r: 88, g: 91, b: 112 },    // dim gray
        }
    }
}

/// Represents a border segment to draw on screen.
#[derive(Debug, Clone)]
pub struct BorderSegment {
    pub x: u16,
    pub y: u16,
    pub char: char,
    pub is_focused: bool,
}

/// Compute border segments for a set of pane rects.
/// Borders are drawn between adjacent panes, not around each pane individually.
/// This avoids double-thick borders where two panes share an edge.
pub fn compute_borders(
    pane_rects: &[(uuid::Uuid, Rect)],
    focused_pane: uuid::Uuid,
    total_area: Rect,
    style: BorderStyle,
) -> Vec<BorderSegment> {
    let chars = match style.chars() {
        Some(c) => c,
        None => return vec![], // No borders
    };

    let mut segments = Vec::new();

    // Strategy: find all split boundaries by examining adjacent pane rects.
    // A vertical boundary exists where one pane's right edge meets another's left edge.
    // A horizontal boundary exists where one pane's bottom edge meets another's top edge.

    // Build a grid of border characters
    let width = total_area.width as usize;
    let height = total_area.height as usize;
    let mut border_grid: Vec<Vec<Option<(char, bool)>>> = vec![vec![None; width]; height];

    for (pane_id, rect) in pane_rects {
        let is_focused = *pane_id == focused_pane;

        // Right border (if not at total area edge)
        let right_x = rect.x + rect.width;
        if right_x < total_area.x + total_area.width {
            for y in rect.y..rect.y + rect.height {
                let bx = right_x.saturating_sub(total_area.x) as usize;
                let by = y.saturating_sub(total_area.y) as usize;
                if bx < width && by < height {
                    let existing = border_grid[by][bx];
                    let focused = is_focused || existing.map(|(_, f)| f).unwrap_or(false);
                    border_grid[by][bx] = Some((chars.vertical, focused));
                }
            }
        }

        // Bottom border (if not at total area edge)
        let bottom_y = rect.y + rect.height;
        if bottom_y < total_area.y + total_area.height {
            for x in rect.x..rect.x + rect.width {
                let bx = x.saturating_sub(total_area.x) as usize;
                let by = bottom_y.saturating_sub(total_area.y) as usize;
                if bx < width && by < height {
                    let existing = border_grid[by][bx];
                    let focused = is_focused || existing.map(|(_, f)| f).unwrap_or(false);
                    // Check if there's already a vertical border here -> intersection
                    if let Some((existing_char, _)) = existing {
                        if existing_char == chars.vertical {
                            border_grid[by][bx] = Some((chars.cross, focused));
                        } else {
                            border_grid[by][bx] = Some((chars.horizontal, focused));
                        }
                    } else {
                        border_grid[by][bx] = Some((chars.horizontal, focused));
                    }
                }
            }
        }
    }

    // Convert grid to segments
    for (y, row) in border_grid.iter().enumerate() {
        for (x, cell) in row.iter().enumerate() {
            if let Some((ch, is_focused)) = cell {
                segments.push(BorderSegment {
                    x: total_area.x + x as u16,
                    y: total_area.y + y as u16,
                    char: *ch,
                    is_focused: *is_focused,
                });
            }
        }
    }

    segments
}

use crate::layout::Rect;
```

### Step 2: Implement border-aware layout rect computation

The LayoutTree's `compute_rects` method must account for border pixels. When borders are enabled, a 1-cell gap is reserved between adjacent panes for the border character.

In `crates/shux-core/src/layout.rs`, add a border-aware variant:

```rust
impl LayoutNode {
    /// Compute pane rects with space reserved for borders between panes.
    /// Each split reserves 1 cell for the border line.
    pub fn compute_rects_with_borders(&self, area: Rect) -> Vec<(Uuid, Rect)> {
        match self {
            LayoutNode::Leaf { pane } => vec![(*pane, area)],
            LayoutNode::Split { dir, ratio, a, b } => {
                let (area_a, area_b) = match dir {
                    SplitDirection::Horizontal => {
                        // Reserve 1 row for the horizontal border
                        let usable_height = area.height.saturating_sub(1);
                        let height_a = (usable_height as f32 * ratio) as u16;
                        let height_b = usable_height - height_a;
                        (
                            Rect {
                                x: area.x, y: area.y,
                                width: area.width, height: height_a,
                            },
                            Rect {
                                x: area.x, y: area.y + height_a + 1, // +1 for border
                                width: area.width, height: height_b,
                            },
                        )
                    }
                    SplitDirection::Vertical => {
                        // Reserve 1 column for the vertical border
                        let usable_width = area.width.saturating_sub(1);
                        let width_a = (usable_width as f32 * ratio) as u16;
                        let width_b = usable_width - width_a;
                        (
                            Rect {
                                x: area.x, y: area.y,
                                width: width_a, height: area.height,
                            },
                            Rect {
                                x: area.x + width_a + 1, // +1 for border
                                y: area.y, width: width_b, height: area.height,
                            },
                        )
                    }
                };

                let mut rects = a.compute_rects_with_borders(area_a);
                rects.extend(b.compute_rects_with_borders(area_b));
                rects
            }
        }
    }
}
```

### Step 3: Implement single-pane renderer

The pane renderer reads a VirtualTerminal grid and writes its content into a specific screen rect. It handles clipping, cell-by-cell styling, and wide characters.

In `crates/shux-ui/src/pane_renderer.rs`:

```rust
use crossterm::style::{Color, SetForegroundColor, SetBackgroundColor, SetAttribute, Attribute};
use crossterm::cursor::MoveTo;
use crossterm::QueueableCommand;
use std::io::Write;

use crate::layout::Rect;

/// Render a pane's VT grid content into a screen rect.
/// Returns the number of cells rendered (for metrics).
pub fn render_pane<W: Write>(
    writer: &mut W,
    vt: &shux_vt::VirtualTerminal,
    rect: Rect,
    is_focused: bool,
) -> std::io::Result<usize> {
    let grid = vt.grid();
    let mut cells_rendered = 0;

    // The VT grid coordinates are 0-based. We map them to screen coordinates
    // starting at (rect.x, rect.y).
    let visible_rows = rect.height as usize;
    let visible_cols = rect.width as usize;

    // Determine the starting line in the VT grid.
    // If the grid has more lines than the rect height, show the bottom portion
    // (most recent output).
    let total_lines = grid.visible_line_count();
    let start_line = total_lines.saturating_sub(visible_rows);

    for row in 0..visible_rows {
        let grid_line = start_line + row;
        let screen_y = rect.y + row as u16;

        writer.queue(MoveTo(rect.x, screen_y))?;

        if grid_line < total_lines {
            let line = grid.get_line(grid_line);

            for col in 0..visible_cols {
                let cell = line.get_cell(col);

                // Apply cell styling
                if let Some(fg) = cell.fg_color() {
                    writer.queue(SetForegroundColor(vt_color_to_crossterm(fg)))?;
                }
                if let Some(bg) = cell.bg_color() {
                    writer.queue(SetBackgroundColor(vt_color_to_crossterm(bg)))?;
                }
                if cell.is_bold() {
                    writer.queue(SetAttribute(Attribute::Bold))?;
                }
                if cell.is_italic() {
                    writer.queue(SetAttribute(Attribute::Italic))?;
                }
                if cell.is_underlined() {
                    writer.queue(SetAttribute(Attribute::Underlined))?;
                }

                // Write the character
                let ch = cell.char();
                if ch == '\0' || ch == ' ' {
                    write!(writer, " ")?;
                } else {
                    write!(writer, "{}", ch)?;
                }

                // Reset attributes after each cell (simple approach;
                // optimize later with dirty tracking)
                writer.queue(SetAttribute(Attribute::Reset))?;

                cells_rendered += 1;
            }
        } else {
            // Empty line: fill with spaces
            for _ in 0..visible_cols {
                write!(writer, " ")?;
            }
            cells_rendered += visible_cols;
        }
    }

    Ok(cells_rendered)
}

fn vt_color_to_crossterm(color: shux_vt::Color) -> Color {
    match color {
        shux_vt::Color::Indexed(idx) => Color::AnsiValue(idx),
        shux_vt::Color::Rgb(r, g, b) => Color::Rgb { r, g, b },
        shux_vt::Color::Default => Color::Reset,
    }
}
```

### Step 4: Extend the RenderCompositor for multi-pane rendering

The compositor orchestrates the full render cycle: compute layout, render each pane, draw borders, handle zoom, and perform diff-based output.

In `crates/shux-ui/src/compositor.rs`:

```rust
use std::io::Write;
use crossterm::cursor::{Hide, Show, MoveTo};
use crossterm::terminal::{Clear, ClearType};
use crossterm::style::{SetForegroundColor, SetAttribute, Attribute};
use crossterm::QueueableCommand;

use crate::borders::{BorderStyle, BorderColors, compute_borders};
use crate::pane_renderer::render_pane;
use crate::layout::Rect;

/// The render compositor manages the full screen output.
pub struct RenderCompositor {
    /// Current terminal dimensions.
    width: u16,
    height: u16,

    /// Border style configuration.
    border_style: BorderStyle,
    border_colors: BorderColors,

    /// Previous frame buffer for diff-based rendering.
    /// Each cell is (char, fg_color, bg_color, attributes).
    prev_frame: Vec<Vec<Cell>>,

    /// Current frame buffer being built.
    curr_frame: Vec<Vec<Cell>>,

    /// Whether a full redraw is needed (e.g., after resize).
    force_redraw: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cell {
    ch: char,
    fg: crossterm::style::Color,
    bg: crossterm::style::Color,
    bold: bool,
    italic: bool,
    underline: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: crossterm::style::Color::Reset,
            bg: crossterm::style::Color::Reset,
            bold: false,
            italic: false,
            underline: false,
        }
    }
}

impl RenderCompositor {
    pub fn new(width: u16, height: u16, border_style: BorderStyle) -> Self {
        let frame = vec![vec![Cell::default(); width as usize]; height as usize];
        Self {
            width,
            height,
            border_style,
            border_colors: BorderColors::default(),
            prev_frame: frame.clone(),
            curr_frame: frame,
            force_redraw: true,
        }
    }

    /// Render a complete frame for a window.
    pub fn render<W: Write>(
        &mut self,
        writer: &mut W,
        layout: &shux_core::layout::LayoutNode,
        focused_pane: uuid::Uuid,
        pane_vts: &std::collections::HashMap<uuid::Uuid, &shux_vt::VirtualTerminal>,
        zoom_state: Option<&shux_core::layout::ZoomState>,
        status_bar_height: u16,
    ) -> std::io::Result<RenderMetrics> {
        let start = std::time::Instant::now();

        // Reserve space for status bar at the bottom
        let content_area = Rect {
            x: 0,
            y: 0,
            width: self.width,
            height: self.height.saturating_sub(status_bar_height),
        };

        // Compute pane rects from layout
        let (pane_rects, effective_layout) = if let Some(zoom) = zoom_state {
            // Zoom mode: only the zoomed pane fills the content area
            let rects = vec![(zoom.zoomed_pane, content_area)];
            (rects, None)
        } else if self.border_style == BorderStyle::None {
            (layout.compute_rects(content_area), Some(layout))
        } else {
            (layout.compute_rects_with_borders(content_area), Some(layout))
        };

        // Clear current frame
        for row in &mut self.curr_frame {
            for cell in row.iter_mut() {
                *cell = Cell::default();
            }
        }

        // Render each pane into the current frame buffer
        let mut total_cells = 0;
        for (pane_id, rect) in &pane_rects {
            if let Some(vt) = pane_vts.get(pane_id) {
                let cells = self.render_pane_to_buffer(*pane_id, vt, *rect, *pane_id == focused_pane);
                total_cells += cells;
            }
        }

        // Draw borders (only if not zoomed and borders enabled)
        let border_cells = if zoom_state.is_none() && self.border_style != BorderStyle::None {
            let segments = compute_borders(&pane_rects, focused_pane, content_area, self.border_style);
            for seg in &segments {
                let x = seg.x as usize;
                let y = seg.y as usize;
                if y < self.curr_frame.len() && x < self.curr_frame[y].len() {
                    self.curr_frame[y][x] = Cell {
                        ch: seg.char,
                        fg: if seg.is_focused {
                            self.border_colors.focused_fg
                        } else {
                            self.border_colors.unfocused_fg
                        },
                        bg: crossterm::style::Color::Reset,
                        bold: false,
                        italic: false,
                        underline: false,
                    };
                }
            }
            segments.len()
        } else {
            0
        };

        // Diff-based rendering: only write cells that changed
        writer.queue(Hide)?;

        let mut changed_cells = 0;
        if self.force_redraw {
            // Full redraw
            for (y, row) in self.curr_frame.iter().enumerate() {
                writer.queue(MoveTo(0, y as u16))?;
                for cell in row {
                    self.write_cell(writer, cell)?;
                    changed_cells += 1;
                }
            }
            self.force_redraw = false;
        } else {
            // Diff only changed cells
            for (y, (curr_row, prev_row)) in
                self.curr_frame.iter().zip(self.prev_frame.iter()).enumerate()
            {
                for (x, (curr, prev)) in
                    curr_row.iter().zip(prev_row.iter()).enumerate()
                {
                    if curr != prev {
                        writer.queue(MoveTo(x as u16, y as u16))?;
                        self.write_cell(writer, curr)?;
                        changed_cells += 1;
                    }
                }
            }
        }

        // Position cursor in the focused pane
        if let Some((_, rect)) = pane_rects.iter().find(|(id, _)| *id == focused_pane) {
            if let Some(vt) = pane_vts.get(&focused_pane) {
                let cursor = vt.cursor_position();
                let screen_x = rect.x + cursor.col as u16;
                let screen_y = rect.y + cursor.row as u16;
                if screen_x < rect.x + rect.width && screen_y < rect.y + rect.height {
                    writer.queue(MoveTo(screen_x, screen_y))?;
                    writer.queue(Show)?;
                }
            }
        }

        writer.flush()?;

        // Swap buffers
        std::mem::swap(&mut self.prev_frame, &mut self.curr_frame);

        let elapsed = start.elapsed();
        Ok(RenderMetrics {
            total_cells,
            changed_cells,
            border_cells,
            pane_count: pane_rects.len(),
            render_time: elapsed,
        })
    }

    /// Render a single pane's VT content into the frame buffer.
    fn render_pane_to_buffer(
        &mut self,
        _pane_id: uuid::Uuid,
        vt: &shux_vt::VirtualTerminal,
        rect: Rect,
        _is_focused: bool,
    ) -> usize {
        let grid = vt.grid();
        let visible_rows = rect.height as usize;
        let visible_cols = rect.width as usize;
        let total_lines = grid.visible_line_count();
        let start_line = total_lines.saturating_sub(visible_rows);
        let mut cells = 0;

        for row in 0..visible_rows {
            let grid_line = start_line + row;
            let screen_y = rect.y as usize + row;

            if screen_y >= self.curr_frame.len() {
                break;
            }

            for col in 0..visible_cols {
                let screen_x = rect.x as usize + col;
                if screen_x >= self.curr_frame[screen_y].len() {
                    break;
                }

                if grid_line < total_lines {
                    let line = grid.get_line(grid_line);
                    let vt_cell = line.get_cell(col);

                    self.curr_frame[screen_y][screen_x] = Cell {
                        ch: if vt_cell.char() == '\0' { ' ' } else { vt_cell.char() },
                        fg: vt_color_to_crossterm(vt_cell.fg_color()),
                        bg: vt_color_to_crossterm(vt_cell.bg_color()),
                        bold: vt_cell.is_bold(),
                        italic: vt_cell.is_italic(),
                        underline: vt_cell.is_underlined(),
                    };
                }
                // else: already default (space)
                cells += 1;
            }
        }

        cells
    }

    fn write_cell<W: Write>(&self, writer: &mut W, cell: &Cell) -> std::io::Result<()> {
        // Set colors
        writer.queue(SetForegroundColor(cell.fg))?;
        writer.queue(crossterm::style::SetBackgroundColor(cell.bg))?;

        if cell.bold {
            writer.queue(SetAttribute(Attribute::Bold))?;
        }
        if cell.italic {
            writer.queue(SetAttribute(Attribute::Italic))?;
        }
        if cell.underline {
            writer.queue(SetAttribute(Attribute::Underlined))?;
        }

        write!(writer, "{}", cell.ch)?;
        writer.queue(SetAttribute(Attribute::Reset))?;
        Ok(())
    }

    /// Handle terminal resize: reallocate buffers and force a full redraw.
    pub fn resize(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
        self.prev_frame = vec![vec![Cell::default(); width as usize]; height as usize];
        self.curr_frame = vec![vec![Cell::default(); width as usize]; height as usize];
        self.force_redraw = true;
    }

    /// Force a full redraw on the next render cycle.
    pub fn invalidate(&mut self) {
        self.force_redraw = true;
    }
}

#[derive(Debug, Clone)]
pub struct RenderMetrics {
    pub total_cells: usize,
    pub changed_cells: usize,
    pub border_cells: usize,
    pub pane_count: usize,
    pub render_time: std::time::Duration,
}

fn vt_color_to_crossterm(color: Option<shux_vt::Color>) -> crossterm::style::Color {
    match color {
        Some(shux_vt::Color::Indexed(idx)) => crossterm::style::Color::AnsiValue(idx),
        Some(shux_vt::Color::Rgb(r, g, b)) => crossterm::style::Color::Rgb { r, g, b },
        _ => crossterm::style::Color::Reset,
    }
}
```

### Step 5: Handle terminal resize events

When the host terminal is resized, the compositor must recompute all pane rects and notify each pane's PTY of its new dimensions via TIOCSWINSZ.

```rust
/// Handle a terminal resize event from crossterm.
pub async fn handle_resize(
    compositor: &mut RenderCompositor,
    layout: &LayoutNode,
    pty_manager: &PtyManager,
    new_width: u16,
    new_height: u16,
    border_style: BorderStyle,
    status_bar_height: u16,
) {
    compositor.resize(new_width, new_height);

    let content_area = Rect {
        x: 0, y: 0,
        width: new_width,
        height: new_height.saturating_sub(status_bar_height),
    };

    // Recompute pane rects
    let pane_rects = if border_style == BorderStyle::None {
        layout.compute_rects(content_area)
    } else {
        layout.compute_rects_with_borders(content_area)
    };

    // Notify each PTY of its new size
    for (pane_id, rect) in &pane_rects {
        if rect.width >= 2 && rect.height >= 1 {
            pty_manager.resize_pane(*pane_id, rect.width, rect.height).await;
        }
    }
}
```

### Step 6: Integrate synchronized output (Mode 2026)

Wrap each render cycle in synchronized output markers to prevent tearing. This uses crossterm's `BeginSynchronizedUpdate` / `EndSynchronizedUpdate` when the terminal supports it.

```rust
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};

/// Render with synchronized output for tear-free display.
pub fn render_synchronized<W: Write>(
    compositor: &mut RenderCompositor,
    writer: &mut W,
    layout: &LayoutNode,
    focused_pane: uuid::Uuid,
    pane_vts: &HashMap<uuid::Uuid, &VirtualTerminal>,
    zoom_state: Option<&ZoomState>,
    status_bar_height: u16,
    use_sync: bool,
) -> std::io::Result<RenderMetrics> {
    if use_sync {
        writer.queue(BeginSynchronizedUpdate)?;
    }

    let metrics = compositor.render(
        writer, layout, focused_pane, pane_vts,
        zoom_state, status_bar_height,
    )?;

    if use_sync {
        writer.queue(EndSynchronizedUpdate)?;
        writer.flush()?;
    }

    Ok(metrics)
}
```

### Step 7: Write compositor tests

In `crates/shux-ui/tests/compositor_tests.rs`:

```rust
use shux_core::layout::{LayoutNode, SplitDirection, Rect};
use shux_ui::borders::{BorderStyle, compute_borders};

#[test]
fn test_compute_rects_single_pane() {
    let pane = uuid::Uuid::new_v4();
    let layout = LayoutNode::Leaf { pane };
    let area = Rect { x: 0, y: 0, width: 80, height: 24 };

    let rects = layout.compute_rects(area);
    assert_eq!(rects.len(), 1);
    assert_eq!(rects[0].0, pane);
    assert_eq!(rects[0].1, area);
}

#[test]
fn test_compute_rects_vertical_split() {
    let a = uuid::Uuid::new_v4();
    let b = uuid::Uuid::new_v4();
    let mut layout = LayoutNode::Leaf { pane: a };
    layout.split_pane(a, b, SplitDirection::Vertical, 0.5);

    let area = Rect { x: 0, y: 0, width: 80, height: 24 };
    let rects = layout.compute_rects(area);

    assert_eq!(rects.len(), 2);
    // Both panes should have full height
    assert_eq!(rects[0].1.height, 24);
    assert_eq!(rects[1].1.height, 24);
    // Widths should sum to total width
    assert_eq!(rects[0].1.width + rects[1].1.width, 80);
}

#[test]
fn test_compute_rects_with_borders_reserves_space() {
    let a = uuid::Uuid::new_v4();
    let b = uuid::Uuid::new_v4();
    let mut layout = LayoutNode::Leaf { pane: a };
    layout.split_pane(a, b, SplitDirection::Vertical, 0.5);

    let area = Rect { x: 0, y: 0, width: 81, height: 24 };
    let rects = layout.compute_rects_with_borders(area);

    assert_eq!(rects.len(), 2);
    // Widths should sum to total width minus 1 (border)
    assert_eq!(rects[0].1.width + rects[1].1.width + 1, 81);
    // The second pane should start 1 cell after the first ends
    assert_eq!(rects[1].1.x, rects[0].1.x + rects[0].1.width + 1);
}

#[test]
fn test_compute_rects_horizontal_split_with_borders() {
    let a = uuid::Uuid::new_v4();
    let b = uuid::Uuid::new_v4();
    let mut layout = LayoutNode::Leaf { pane: a };
    layout.split_pane(a, b, SplitDirection::Horizontal, 0.5);

    let area = Rect { x: 0, y: 0, width: 80, height: 25 };
    let rects = layout.compute_rects_with_borders(area);

    assert_eq!(rects.len(), 2);
    assert_eq!(rects[0].1.height + rects[1].1.height + 1, 25);
    assert_eq!(rects[1].1.y, rects[0].1.y + rects[0].1.height + 1);
}

#[test]
fn test_border_segments_vertical_split() {
    let a = uuid::Uuid::new_v4();
    let b = uuid::Uuid::new_v4();

    let pane_rects = vec![
        (a, Rect { x: 0, y: 0, width: 39, height: 24 }),
        (b, Rect { x: 40, y: 0, width: 40, height: 24 }),
    ];

    let segments = compute_borders(
        &pane_rects, a,
        Rect { x: 0, y: 0, width: 80, height: 24 },
        BorderStyle::Rounded,
    );

    // Should have vertical border segments at x=39
    assert!(!segments.is_empty());
    assert!(segments.iter().all(|s| s.x == 39));
    assert_eq!(segments.len(), 24); // One per row
}

#[test]
fn test_no_borders_when_style_none() {
    let a = uuid::Uuid::new_v4();
    let pane_rects = vec![(a, Rect { x: 0, y: 0, width: 80, height: 24 })];

    let segments = compute_borders(
        &pane_rects, a,
        Rect { x: 0, y: 0, width: 80, height: 24 },
        BorderStyle::None,
    );

    assert!(segments.is_empty());
}

#[test]
fn test_four_pane_grid_has_cross_intersection() {
    let a = uuid::Uuid::new_v4();
    let b = uuid::Uuid::new_v4();
    let c = uuid::Uuid::new_v4();
    let d = uuid::Uuid::new_v4();

    // 2x2 grid layout
    let pane_rects = vec![
        (a, Rect { x: 0, y: 0, width: 39, height: 11 }),
        (b, Rect { x: 40, y: 0, width: 40, height: 11 }),
        (c, Rect { x: 0, y: 12, width: 39, height: 12 }),
        (d, Rect { x: 40, y: 12, width: 40, height: 12 }),
    ];

    let segments = compute_borders(
        &pane_rects, a,
        Rect { x: 0, y: 0, width: 80, height: 24 },
        BorderStyle::Thin,
    );

    // Should have a cross intersection at the center
    let cross = segments.iter().find(|s| s.char == '\u{253c}'); // ┼
    assert!(cross.is_some(), "Expected a cross intersection in 4-pane grid");
}

#[test]
fn test_render_metrics_track_changed_cells() {
    // Test that diff-based rendering only updates changed cells
    // (Detailed rendering test would require mock VT grids)
}
```

---

## Verification

### Functional

```bash
# Start shux and create a multi-pane layout
shux new -s test
# (In the TUI)
# Alt+Enter to split
# Alt+Enter again for another split
# Verify: borders appear between panes
# Verify: focused pane border is accent colored
# Verify: unfocused pane borders are dim

# Test zoom
# Alt+z: active pane should fill the screen
# Alt+z: previous layout should be restored

# Test resize
# Resize the host terminal
# Verify: all panes resize proportionally
# Verify: no rendering artifacts

# Test border styles (via config)
# Set pane_border_style = "none" → no borders
# Set pane_border_style = "thin" → thin box-drawing chars
# Set pane_border_style = "double" → double-line chars
```

### Tests

```bash
# Layout rect computation tests
cargo nextest run -p shux-core --lib -- layout

# Border computation tests
cargo nextest run -p shux-ui --test compositor_tests

# All tests
cargo nextest run --workspace

# Clippy
cargo clippy --workspace --all-targets -- -D warnings
```

### Performance

```bash
# Benchmark render time with multiple panes
# (After benchmark harness is available)
# Target: p50 <= 8ms with 10 panes on an 80x24 terminal

# Manual timing: render should feel instant with no perceptible flicker
```

### L4 Visual Regression — iterm2-driver (PRD §16.2)

Create `.claude/automations/test_splits_visual.py` to verify multi-pane rendering:

```python
# /// script
# requires-python = ">=3.14"
# dependencies = ["iterm2", "pyobjc", "pyobjc-framework-Quartz"]
# ///
"""
shux Multi-Pane Rendering Visual Test (iterm2-driver)

Tests:
1. Launch shux, create session
2. Split vertical (Ctrl+Space |) — verify 2 panes with border
3. Split horizontal (Ctrl+Space -) — verify 3 panes
4. Verify border characters connect (box-drawing connectivity)
5. Verify focused pane has accent-colored border
6. Alt+z zoom — verify single pane fills screen
7. Alt+z unzoom — verify layout restored
8. Resize terminal — verify panes recompute
9. Take screenshots at each step

Verification Strategy:
- Read screen content, check for border characters (│, ─, ┼)
- Verify content appears within correct screen regions
- Check box-drawing corner connectivity

Usage:
    uv run .claude/automations/test_splits_visual.py
"""
```

Run: `uv run .claude/automations/test_splits_visual.py`

---

## Completion Criteria

- [ ] Compositor renders multiple panes using LayoutTree rect computation
- [ ] Border-aware layout computation reserves 1 cell for borders between adjacent panes
- [ ] Border styles implemented: thin, thick, double, rounded, none
- [ ] Focused pane border uses accent color; unfocused uses dim color
- [ ] Zoom mode renders only the zoomed pane at full window size
- [ ] Diff-based rendering: only changed cells are written to the terminal
- [ ] Terminal resize triggers recompute of all pane rects and PTY TIOCSWINSZ notifications
- [ ] Cursor is positioned correctly within the focused pane
- [ ] Synchronized output (Mode 2026) wraps render cycles when supported
- [ ] Pane minimum size enforced: 2 columns x 1 row
- [ ] Performance: p50 <= 8ms render target maintained with 10+ panes
- [ ] Layout rect tests pass: single pane, vertical split, horizontal split, borders
- [ ] Border segment tests pass: vertical/horizontal borders, cross intersections, no-border mode
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] `cargo nextest run --workspace` passes

---

## Commit Message

```
feat: extend render compositor for multi-pane layout rendering

- Compute per-pane screen rects from LayoutTree with border reservation
- Render each pane's VT grid content into its assigned rect
- Draw borders between panes with 5 styles: thin, thick, double, rounded, none
- Focused pane border uses accent color, unfocused uses dim color
- Diff-based rendering only updates changed cells across all panes
- Zoom mode renders single pane at full window size
- Handle terminal resize: recompute rects, resize VT grids, notify PTYs
- Synchronized output (Mode 2026) for tear-free rendering
- Tests for layout computation, border segments, and rendering metrics
```

---

## Session Protocol

1. **Before starting:** Verify tasks 015 and 009 are complete. Pane operations with LayoutTree must work. The single-pane compositor from M0 must render correctly. Read `CLAUDE.md`.
2. **During:** Start with border styles (pure data, easy to test). Then implement border-aware layout computation (extend existing code). Then the pane renderer (maps VT grid to screen). Then extend the compositor. Test at each stage.
3. **Key patterns:**
   - The frame buffer uses a 2D Vec of Cell structs. This is simple and correct; optimize to a flat array later if needed.
   - Diff-based rendering compares curr_frame with prev_frame cell by cell. This avoids full redraws and dramatically reduces I/O for typical updates (only the cursor line changes).
   - Border computation operates on the gap between pane rects, not by drawing around each pane. This prevents double-thick borders.
   - Synchronized output must be negotiated via ClientCaps (task 028). For now, always use it and let the terminal ignore it if unsupported.
4. **After:** Run full verification. Manually test in a real terminal with multiple panes. Verify that resize works cleanly. Update `docs/PROGRESS.md`.
