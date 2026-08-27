//! RenderCompositor: orchestrates compose -> diff -> render pipeline.
//!
//! The compositor ties everything together: it takes a grid cell accessor
//! (decoupled from VirtualTerminal via a closure), maps cells into the
//! FrameBuffer, diffs against the previous frame, and renders only
//! changed cells to the terminal via RenderBackend.
//!
//! In this task (009) we support single-pane rendering only. Task 017
//! extends this to multi-pane with borders and layout-aware composition.

use std::collections::HashMap;
use std::io::{self, Write};
use std::time::Instant;

use shux_core::layout::{LayoutNode, Rect, ZoomState};
use shux_core::model::PaneId;

use crate::borders::{BorderColors, BorderStyle, compute_borders};
use crate::buffer::{FrameBuffer, RenderAttrs, RenderCell};
use crate::render::RenderBackend;
use crate::statusbar::StatusBar;

/// Statistics from the last render pass, used for performance monitoring
/// against the PRD section 14.1 budget (p50 <= 8ms).
#[derive(Debug, Clone)]
pub struct RenderStats {
    /// How many cells were dirty (differed from previous frame).
    pub dirty_cells: usize,
    /// Total number of cells in the frame.
    pub total_cells: usize,
    /// Time spent composing the VirtualTerminal grid into the FrameBuffer.
    pub compose_time_us: u64,
    /// Time spent diffing current vs previous frame.
    pub diff_time_us: u64,
    /// Time spent rendering dirty cells to the terminal.
    pub render_time_us: u64,
    /// Total frame time (compose + diff + render).
    pub total_time_us: u64,
}

/// Configuration for the RenderCompositor.
#[derive(Debug, Clone)]
pub struct CompositorConfig {
    /// Whether to show a simple border around the single pane.
    /// In single-pane mode this is typically false (the pane fills the
    /// entire terminal). Set to true for testing or when a status bar
    /// reserves space.
    pub show_border: bool,

    /// Number of rows reserved at the bottom for a status bar.
    /// In M0 this is 0 (no status bar). Task 026 will set this to 1.
    pub status_bar_height: u16,

    /// Border style for multi-pane mode. Default `Rounded`.
    pub border_style: BorderStyle,

    /// Border colors for focused/unfocused panes.
    pub border_colors: BorderColors,
}

impl Default for CompositorConfig {
    fn default() -> Self {
        Self {
            show_border: false,
            status_bar_height: 0,
            border_style: BorderStyle::Rounded,
            border_colors: BorderColors::default(),
        }
    }
}

/// Inputs for one multi-pane render cycle.
///
/// The compositor doesn't borrow the SessionGraph directly — that lives in
/// the daemon. The caller assembles a snapshot of layout + per-pane VTs
/// and hands it over.
pub struct MultiPaneFrame<'a> {
    /// Layout tree for the active window.
    pub layout: &'a LayoutNode,
    /// Optional zoom state. If `Some`, the zoomed pane fills the content area.
    pub zoom: Option<&'a ZoomState>,
    /// Pane id currently focused (its border is drawn with the accent color
    /// and the cursor lives inside its rect).
    pub focused: PaneId,
    /// Per-pane virtual terminals keyed by pane id.
    pub vts: &'a HashMap<PaneId, &'a shux_vt::VirtualTerminal>,
    /// Per-pane titles (PR 4 / task 027). When `show_pane_titles` is
    /// enabled and a pane has a non-empty title, the title is overlaid
    /// onto the pane's top border. Caller passes whatever the
    /// graph snapshot reports as `Pane.title` (priority-resolved from
    /// manual > osc > auto-derived). Missing keys = no title overlay,
    /// not a panic.
    pub titles: Option<&'a HashMap<PaneId, String>>,
    /// Optional status bar (renders into the rows reserved by
    /// `CompositorConfig::status_bar_height`).
    pub status_bar: Option<&'a StatusBar>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CursorVisual {
    shape: shux_vt::CursorShape,
    color: Option<shux_vt::Rgb>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CursorTarget {
    x: u16,
    y: u16,
    visual: CursorVisual,
}

/// The RenderCompositor is responsible for:
/// 1. Reading cells from a VirtualTerminal grid (via a closure)
/// 2. Writing them into a FrameBuffer
/// 3. Diffing against the previous frame
/// 4. Rendering only changed cells to the terminal via RenderBackend
///
/// In this task (009) we support single-pane rendering only. Task 017
/// extends this to multi-pane with borders and layout-aware composition.
/// SPIKE: cell height in pixels (matches the winsize shux reports to panes).
const SPIKE_CELL_H: u16 = 19;

fn spike_wire(msg: &str) {
    if let Ok(path) = std::env::var("SHUX_SPIKE_WIRE_LOG") {
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{msg}");
        }
    }
}
/// SPIKE: cell width in pixels, matching the winsize shux reports to panes.
const SPIKE_CELL_W: u16 = 9;

/// SPIKE: crop an image to the pixels that actually fit inside its pane.
///
/// The rasterizer clips while compositing, but the emit path hands the whole
/// bitmap to the host terminal, which draws all of it -- straight over the
/// pane border, the status bar and any neighbouring pane. Zellij transmits a
/// sub-rectangle for exactly this reason (`source_px_*` on its image chunks).
fn spike_clip_to_pane(
    img: &shux_vt::graphics::spike_kitty::SpikeImage,
    row: u16,
    col: u16,
    rect: Rect,
) -> Option<shux_vt::graphics::spike_kitty::SpikeImage> {
    let avail_w = u32::from(rect.x.saturating_add(rect.width).saturating_sub(col))
        * u32::from(SPIKE_CELL_W);
    let avail_h = u32::from(rect.y.saturating_add(rect.height).saturating_sub(row))
        * u32::from(SPIKE_CELL_H);
    let w = img.width.min(avail_w);
    let h = img.height.min(avail_h);
    if w == 0 || h == 0 {
        return None;
    }
    if w == img.width && h == img.height {
        return Some(img.clone());
    }
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let start = ((y * img.width) * 4) as usize;
        let end = start + (w * 4) as usize;
        match img.rgba.get(start..end) {
            Some(rowpx) => rgba.extend_from_slice(rowpx),
            None => return None,
        }
    }
    let mut out = img.clone();
    out.width = w;
    out.height = h;
    out.rgba = rgba;
    Some(out)
}


pub struct RenderCompositor<W: Write> {
    buffer: FrameBuffer,
    backend: RenderBackend<W>,
    config: CompositorConfig,
    last_stats: Option<RenderStats>,
    /// Set to true after resize or other events that require a full redraw.
    force_full_redraw: bool,
    /// Best-known terminal cursor position after the previous render.
    ///
    /// `None` means hidden. The first render starts unknown, so the
    /// compositor still emits an initial hide/show pair as needed.
    terminal_cursor: Option<(u16, u16)>,
    /// Best-known host-terminal cursor shape/color. Tracked separately from
    /// position so cursor-only movement can still emit just `MoveTo`.
    terminal_cursor_visual: Option<CursorVisual>,
    cursor_state_known: bool,
    /// SPIKE: signature of the images last re-transmitted to the outer
    /// terminal, so an unchanged frame costs nothing.
    spike_last_images: Vec<(u16, u16, u32, u32, u32)>,
    /// SPIKE FIX: per-pane image epoch behind those images. Geometry alone
    /// cannot tell two repaints apart -- terminal-browser re-transmits i=1 at
    /// the SAME s=/v= on every frame, so a content-blind signature pins the
    /// first frame after a resize forever.
    ///
    /// Kept per pane and keyed BY pane: a single summed counter cannot tell
    /// "pane A repainted" from "pane A repainted while pane B's epoch went
    /// backwards", and an unkeyed list cannot tell two panes apart at all.
    spike_last_epochs: Vec<(PaneId, u64)>,
    /// SPIKE: per-placement key, so only changed pictures are re-sent.
    spike_last_keys: Vec<(u16, u16, u32, u32, u32, PaneId, u64, (u16, u16))>,
    /// SPIKE: bitmaps already sent to the host, keyed by content identity.
    spike_transmitted: std::collections::HashMap<(PaneId, u32, u64), u32>,
    spike_next_host_image: u32,
    /// SPIKE: host image id currently placed in each slot, so a slot that
    /// switches to a different bitmap can retire the old one.
    spike_last_host: Vec<u32>,
}

impl<W: Write> RenderCompositor<W> {
    pub fn new(width: u16, height: u16, out: W, config: CompositorConfig) -> Self {
        Self {
            buffer: FrameBuffer::new(width, height),
            backend: RenderBackend::new(out),
            config,
            last_stats: None,
            force_full_redraw: true, // First render is always full
            terminal_cursor: None,
            terminal_cursor_visual: None,
            cursor_state_known: false,
            spike_last_images: Vec::new(),
            spike_last_epochs: Vec::new(),
            spike_last_keys: Vec::new(),
            spike_transmitted: std::collections::HashMap::new(),
            spike_next_host_image: 1_000_000,
            spike_last_host: Vec::new(),
        }
    }

    /// Handle a terminal resize event. Resizes the internal buffer and
    /// forces a full redraw on the next render pass.
    pub fn resize(&mut self, width: u16, height: u16) {
        self.buffer.resize(width, height);
        self.force_full_redraw = true;
        self.cursor_state_known = false;
        self.terminal_cursor_visual = None;
    }

    /// Get the usable area for the pane content (excluding borders and
    /// status bar).
    ///
    /// Returns `(x, y, width, height)` describing the content rectangle.
    pub fn content_area(&self) -> (u16, u16, u16, u16) {
        let border_offset = if self.config.show_border { 1 } else { 0 };
        let x = border_offset;
        let y = border_offset;
        let w = self.buffer.width().saturating_sub(border_offset * 2);
        let h = self
            .buffer
            .height()
            .saturating_sub(border_offset * 2)
            .saturating_sub(self.config.status_bar_height);
        (x, y, w, h)
    }

    /// Compose a single pane's grid into the frame buffer and render.
    ///
    /// `grid_cell_fn` is a callback that returns the RenderCell at a given
    /// (col, row) position in the virtual terminal grid. This decouples
    /// the compositor from the VirtualTerminal type (which lives in
    /// shux-vt).
    ///
    /// `cursor_pos` is the cursor position within the pane (col, row),
    /// or None if the cursor should be hidden.
    pub fn render_frame<F>(
        &mut self,
        grid_cell_fn: F,
        grid_width: u16,
        grid_height: u16,
        cursor_pos: Option<(u16, u16)>,
    ) -> io::Result<RenderStats>
    where
        F: Fn(u16, u16) -> RenderCell,
    {
        let frame_start = Instant::now();

        // --- Phase 1: Compose ---
        let compose_start = Instant::now();

        self.buffer.clear_current();

        // Draw border if configured
        if self.config.show_border {
            self.compose_border();
        }

        // Map VT grid cells into the frame buffer
        let (content_x, content_y, content_w, content_h) = self.content_area();

        for row in 0..content_h.min(grid_height) {
            for col in 0..content_w.min(grid_width) {
                let cell = grid_cell_fn(col, row);
                self.buffer.set_cell(content_x + col, content_y + row, cell);
            }
        }

        let compose_time = compose_start.elapsed();

        // --- Phase 2: Diff ---
        let diff_start = Instant::now();

        let dirty = if self.force_full_redraw {
            self.buffer.invalidate();
            self.force_full_redraw = false;
            self.buffer.diff()
        } else {
            self.buffer.diff()
        };

        let dirty_count = dirty.len();
        let diff_time = diff_start.elapsed();

        // --- Phase 3: Render ---
        let render_start = Instant::now();

        let target_cursor = cursor_pos.and_then(|(cx, cy)| {
            let screen_x = content_x + cx;
            let screen_y = content_y + cy;
            (screen_x < self.buffer.width() && screen_y < self.buffer.height())
                .then_some((screen_x, screen_y))
        });
        let target_cursor = target_cursor.map(|(x, y)| CursorTarget {
            x,
            y,
            visual: CursorVisual {
                shape: shux_vt::CursorShape::Block,
                color: None,
            },
        });
        self.render_dirty_and_cursor(&dirty, target_cursor)?;

        let render_time = render_start.elapsed();

        // --- Swap buffers ---
        self.buffer.swap();

        let total_time = frame_start.elapsed();

        let stats = RenderStats {
            dirty_cells: dirty_count,
            total_cells: (self.buffer.width() as usize) * (self.buffer.height() as usize),
            compose_time_us: compose_time.as_micros() as u64,
            diff_time_us: diff_time.as_micros() as u64,
            render_time_us: render_time.as_micros() as u64,
            total_time_us: total_time.as_micros() as u64,
        };

        self.last_stats = Some(stats.clone());
        Ok(stats)
    }

    /// Draw a simple Unicode box border around the content area.
    /// Uses single-line box-drawing characters: top-left, top-right,
    /// bottom-left, bottom-right, horizontal, vertical.
    fn compose_border(&mut self) {
        let w = self.buffer.width();
        let h = self
            .buffer
            .height()
            .saturating_sub(self.config.status_bar_height);

        if w < 2 || h < 2 {
            return;
        }

        let border_cell = |ch: char| RenderCell {
            ch,
            fg: None, // Use default terminal color; task 024 adds theme tokens
            bg: None,
            attrs: RenderAttrs::default(),
            extended: None,
            wide_continuation: false,
        };

        // Corners
        self.buffer.set_cell(0, 0, border_cell('\u{250C}')); // box light down and right
        self.buffer.set_cell(w - 1, 0, border_cell('\u{2510}')); // box light down and left
        self.buffer.set_cell(0, h - 1, border_cell('\u{2514}')); // box light up and right
        self.buffer.set_cell(w - 1, h - 1, border_cell('\u{2518}')); // box light up and left

        // Top and bottom borders
        for col in 1..(w - 1) {
            self.buffer.set_cell(col, 0, border_cell('\u{2500}')); // horizontal
            self.buffer.set_cell(col, h - 1, border_cell('\u{2500}'));
        }

        // Left and right borders
        for row in 1..(h - 1) {
            self.buffer.set_cell(0, row, border_cell('\u{2502}')); // vertical
            self.buffer.set_cell(w - 1, row, border_cell('\u{2502}'));
        }
    }

    /// Force a full redraw on the next render pass. Call this after
    /// events that may have corrupted the terminal state (e.g., a
    /// child process writing directly to the terminal).
    pub fn force_redraw(&mut self) {
        self.force_full_redraw = true;
    }

    fn render_dirty_and_cursor(
        &mut self,
        dirty: &[crate::buffer::DirtyCell],
        target_cursor: Option<CursorTarget>,
    ) -> io::Result<()> {
        let cursor_unknown = !self.cursor_state_known;
        let cursor_visible = self.terminal_cursor.is_some();
        let must_hide = cursor_unknown
            || (!dirty.is_empty() && cursor_visible)
            || (target_cursor.is_none() && cursor_visible);

        if must_hide {
            self.backend.hide_cursor()?;
            self.terminal_cursor = None;
            if cursor_unknown {
                self.terminal_cursor_visual = None;
            }
            self.cursor_state_known = true;
        }

        self.backend.render_diff(dirty)?;

        if let Some(target) = target_cursor {
            if self.terminal_cursor_visual != Some(target.visual) {
                self.backend.set_cursor_shape(target.visual.shape)?;
                self.backend.set_cursor_color(target.visual.color)?;
                self.terminal_cursor_visual = Some(target.visual);
            }
            if self.terminal_cursor != Some((target.x, target.y)) {
                let was_hidden = self.terminal_cursor.is_none();
                self.backend.set_cursor(target.x, target.y)?;
                if was_hidden {
                    self.backend.show_cursor()?;
                }
                self.terminal_cursor = Some((target.x, target.y));
                self.cursor_state_known = true;
            }
        }

        Ok(())
    }

    /// Live-swap the border style. Used by the daemon's attach session
    /// when the user's config.toml changes on disk: the next frame uses
    /// the new style without a restart.
    pub fn set_border_style(&mut self, style: BorderStyle) {
        if self.config.border_style != style {
            self.config.border_style = style;
            self.force_full_redraw = true;
        }
    }

    /// Live-swap the border colors. Mirrors `set_border_style` for the
    /// theme engine (`[theme] border_focused / border_unfocused`).
    pub fn set_border_colors(&mut self, colors: BorderColors) {
        // BorderColors is Copy + uses crossterm's Color (PartialEq).
        if (
            self.config.border_colors.focused,
            self.config.border_colors.unfocused,
        ) != (colors.focused, colors.unfocused)
        {
            self.config.border_colors = colors;
            self.force_full_redraw = true;
        }
    }

    /// Borrow the underlying writer. Useful in tests where we capture
    /// bytes into a `Cursor<Vec<u8>>` and want to assert on them.
    pub fn inner(&self) -> &W {
        self.backend.inner()
    }

    /// Mutably borrow the underlying writer. The daemon's attach loop
    /// uses this with `Vec<u8>` to drain the rendered ANSI bytes after
    /// each frame.
    pub fn inner_mut(&mut self) -> &mut W {
        self.backend.inner_mut()
    }

    /// Compute the content rect (everything above the status bar).
    fn content_rect(&self) -> Rect {
        let h = self
            .buffer
            .height()
            .saturating_sub(self.config.status_bar_height);
        Rect::new(0, 0, self.buffer.width(), h)
    }

    /// Render a full multi-pane frame. Replaces the single-pane path of
    /// `render_frame` for any window with more than one pane (or one pane
    /// inside a layout). Honors zoom state and the configured border style.
    /// SPIKE: cell height in pixels, matching the pty winsize shux reports.
    const _SPIKE_CELL_H_DOC: () = ();

    /// SPIKE: re-transmit pane images to the OUTER terminal.
    ///
    /// The compositor writes cells. Images are not cells, so an attached
    /// client renders every pane's text and none of its pictures. This
    /// forwards each visible image as a kitty graphics command positioned
    /// at its pane-relative anchor, offset into screen coordinates.
    ///
    /// Only re-emits when the visible image set changes -- re-transmitting
    /// megabytes every frame would make the attach unusable.
    fn spike_emit_images(
        &mut self,
        placed: &[(
            u16,
            u16,
            shux_vt::graphics::spike_kitty::SpikeImage,
            PaneId,
            u64,
            (u16, u16),
        )],
        repainted_rows: &std::collections::HashSet<u16>,
        epochs: &[(PaneId, u64)],
    ) -> io::Result<()> {
        use base64::Engine as _;
        let sig: Vec<(u16, u16, u32, u32, u32)> = placed
            .iter()
            .map(|(r, c, i, _, _, _)| (*r, *c, i.image_id, i.width, i.height))
            .collect();
        if let Ok(path) = std::env::var("SHUX_SPIKE_EMIT_LOG") {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(f, "emit_called placed={:?} last={:?}", sig, self.spike_last_images);
            }
        }
        // Zellij's rule (`changed_kitty_chunks_in_viewport`): a placement is
        // re-emitted whenever the cells underneath it are repainted, because
        // the cell layer paints straight over the picture. Its full-redraw
        // variant just passes the whole viewport as one changed rect.
        //
        // Without this, a resize wipes the image with cells and an unchanged
        // signature refuses to put it back -- which looked like "images do not
        // re-fit on resize" when the daemon had the right image all along.
        let overpainted = placed.iter().any(|(row, _, img, _, _, _)| {
            let span = img.height.div_ceil(u32::from(SPIKE_CELL_H)).max(1) as u16;
            (*row..row.saturating_add(span)).any(|r| repainted_rows.contains(&r))
        });
        // Per-image keys. Delete-all-then-replace-all meant ANY change
        // anywhere re-sent every picture in the window -- one pane's 1-pixel
        // repaint re-transmitting another pane's 400 KB image, every frame.
        // Key each placement on its own identity + its pane's epoch, and touch
        // only the ones that actually changed.
        // PaneId is part of the key. Without it, pane A being replaced by
        // pane B at the same rect -- same app, same image id, same geometry,
        // same epoch -- compares equal, and B inherits A's picture.
        let keys: Vec<(u16, u16, u32, u32, u32, PaneId, u64, (u16, u16))> = placed
            .iter()
            .map(|(r, c, i, pid, ep, d)| {
                (*r, *c, i.image_id, i.width, i.height, *pid, *ep, *d)
            })
            .collect();
        // DOUBLE-BUFFERED host image ids.
        //
        // kitty deletes an existing image the instant you re-transmit its id
        // ("the existing image and all its placements are deleted, the new data
        // replaces the old"). Re-sending a ~3.6 MB bitmap under the SAME id
        // therefore empties the pane for the whole ~900-chunk transfer -- a
        // visible flicker on every repaint, measured as the image being absent
        // on 5 of 25 sampled frames in real kitty.
        //
        // So each slot alternates between two ids. The new bitmap is
        // transmitted and placed under the id the slot is NOT currently using;
        // the old image stays on screen for the entire transfer and is deleted
        // only once its replacement is up.
        let mut resend: Vec<usize> = (0..placed.len())
            .filter(|&i| self.spike_last_keys.get(i) != keys.get(i) || overpainted)
            .collect();
        let gone: Vec<u32> = (placed.len()..self.spike_last_host.len())
            .filter_map(|i| self.spike_last_host.get(i).copied())
            .collect();
        self.spike_last_images = sig;
        self.spike_last_epochs = epochs.to_vec();
        self.spike_last_keys = keys;
        if resend.is_empty() && gone.is_empty() {
            return Ok(());
        }
        resend.sort_unstable();

        let mut next_host = self.spike_last_host.clone();
        next_host.resize(placed.len(), 0);
        for &idx in &resend {
            let (row, col, img, _, _, dest) = &placed[idx];
            let slot_a: u32 = 1_000_000 + (idx as u32) * 2;
            let slot_b: u32 = slot_a + 1;
            let old = self.spike_last_host.get(idx).copied().unwrap_or(0);
            let new_id = if old == slot_a { slot_b } else { slot_a };
            {
                let out = self.backend.inner_mut();
                write!(out, "\x1b[{};{}H", row + 1, col + 1)?;
                let b64 = base64::engine::general_purpose::STANDARD.encode(&img.rgba);
                let bytes = b64.as_bytes();
                let total = bytes.len().div_ceil(4096).max(1);
                for (ci, chunk) in bytes.chunks(4096).enumerate() {
                    let more = u8::from(ci + 1 < total);
                    let payload = std::str::from_utf8(chunk).unwrap_or("");
                    if ci == 0 {
                        // c=/r= give the destination box in CELLS, so the host's
                        // own cell pixel size stops mattering.
                        write!(
                            out,
                            "\x1b_Ga=T,f=32,t=d,s={},v={},i={},c={},r={},q=2,m={};{}\x1b\\",
                            img.width, img.height, new_id, dest.0, dest.1, more, payload
                        )?;
                    } else {
                        if more == 0 {
                            write!(out, "\x1b[{};{}H", row + 1, col + 1)?;
                        }
                        write!(out, "\x1b_Gq=2,m={};{}\x1b\\", more, payload)?;
                    }
                }
                // The replacement is up; only now retire the previous one.
                if old != 0 {
                    write!(out, "\x1b_Ga=d,q=2,d=I,i={old}\x1b\\")?;
                }
            }
            next_host[idx] = new_id;
        }
        {
            let out = self.backend.inner_mut();
            for old in &gone {
                write!(out, "\x1b_Ga=d,q=2,d=I,i={old}\x1b\\")?;
            }
        }
        self.spike_last_host = next_host;
        let out = self.backend.inner_mut();
        out.flush()?;
        Ok(())
    }

    pub fn render_multi_pane(&mut self, frame: MultiPaneFrame<'_>) -> io::Result<RenderStats> {
        let frame_start = Instant::now();
        let compose_start = Instant::now();

        let content = self.content_rect();
        self.buffer.clear_current();

        // The outer 1-cell ring is reserved for the border outline when
        // borders are enabled. Pane content lives strictly inside this
        // ring so the outline never overdraws the first/last column or
        // first/last row of any pane.
        let zoomed = frame.zoom.is_some();
        // Borders need at least a 3x3 content area (1 cell of inset on
        // each side leaves a 1-cell pane). Below that, suppress borders
        // entirely — drawing the outline would overwrite the pane's
        // only column/row.
        let borders_on = !zoomed
            && self.config.border_style != BorderStyle::None
            && content.width >= 3
            && content.height >= 3;
        let pane_viewport = if borders_on {
            Rect::new(
                content.x + 1,
                content.y + 1,
                content.width - 2,
                content.height - 2,
            )
        } else {
            content
        };

        // 1. Compute pane rects.
        // When zoomed: only the zoomed pane is shown filling the content
        // area; we deliberately bypass borders so the pane really fills the
        // window (matches tmux behavior).
        let pane_rects: Vec<(PaneId, Rect)> = if let Some(zoom) = frame.zoom {
            vec![(zoom.zoomed_pane, content)]
        } else {
            frame.layout.compute_rects(pane_viewport)
        };

        // 2. Render each pane's VT cells into the framebuffer.
        for (pid, rect) in &pane_rects {
            if let Some(vt) = frame.vts.get(pid) {
                self.compose_pane(*rect, vt);
            } else {
                // Missing VT: render an "(no output)" placeholder so the
                // pane is visible.
                self.compose_placeholder(*rect, "(no output)");
            }
        }

        // 2b. SPIKE: collect this frame's visible images in screen coords.
        let mut spike_placed = Vec::new();
        let mut spike_epochs: Vec<(PaneId, u64)> = Vec::new();
        for (pid, rect) in &pane_rects {
            if let Some(vt) = frame.vts.get(pid) {
                let before = spike_placed.len();
                for img in &vt.grid().clone_visible().spike_images {
                    let row = img.row as u16 + rect.y;
                    let col = img.col as u16 + rect.x;
                    if row < rect.y + rect.height && col < rect.x + rect.width {
                        if let Some(clipped) = spike_clip_to_pane(img, row, col, *rect) {
                            // Destination box in CELLS. The host terminal's cell
                            // pixel size is its own business -- ours is 9x19 and
                            // kitty's is not -- so hand it a cell rectangle and
                            // let it scale, instead of assuming pixels match.
                            let cols = (clipped.width.div_ceil(u32::from(SPIKE_CELL_W)).max(1)
                                as u16)
                                .min(rect.x + rect.width - col);
                            let rows = (clipped.height.div_ceil(u32::from(SPIKE_CELL_H)).max(1)
                                as u16)
                                .min(rect.y + rect.height - row);
                            spike_placed.push((
                                row,
                                col,
                                clipped,
                                *pid,
                                vt.spike_image_epoch(),
                                (cols, rows),
                            ));
                        }
                    }
                }
                // Only panes that actually placed something get an epoch: a
                // pane with no pictures must not make an idle client chatter,
                // and a pane that LOSES its last picture already shows up in
                // the placement signature.
                if spike_placed.len() > before {
                    spike_epochs.push((*pid, vt.spike_image_epoch()));
                }
            }
        }

        // 3. Draw borders unless we're zoomed or borders are disabled.
        // We pass the OUTER content area (not the inset pane viewport) so
        // compute_borders can render the outline ring around all panes
        // while still drawing inter-pane separators in the gaps reserved
        // by `compute_rects`.
        if borders_on {
            let segments = compute_borders(
                &pane_rects,
                frame.focused,
                content,
                self.config.border_style,
            );
            for seg in &segments {
                let cell = RenderCell {
                    ch: seg.ch,
                    fg: Some(if seg.focused {
                        self.config.border_colors.focused
                    } else {
                        self.config.border_colors.unfocused
                    }),
                    bg: None,
                    attrs: RenderAttrs::default(),
                    extended: None,
                    wide_continuation: false,
                };
                self.buffer.set_cell(seg.x, seg.y, cell);
            }

            // PR 4 / task 027 — overlay pane titles on the top border.
            //
            // For each pane with a non-empty title, write " <title> "
            // (space-padded so the corners stay visible) starting at
            // x = rect.x + 2 on the pane's top border row. Skip if the
            // pane's outer width is < 6: there's no room for the
            // smallest meaningful overlay (` X ` plus two corner cells).
            if let Some(titles) = frame.titles {
                for (pid, rect) in &pane_rects {
                    let title = titles.get(pid).map(|s| s.as_str()).unwrap_or("");
                    if title.is_empty() || rect.width < 6 {
                        continue;
                    }
                    let is_focused = *pid == frame.focused;
                    let fg = if is_focused {
                        self.config.border_colors.focused
                    } else {
                        self.config.border_colors.unfocused
                    };
                    // Available glyph cells between the two corners,
                    // minus 4 chars of breathing room (corner + space
                    // on each side).
                    let max_chars = rect.width.saturating_sub(4) as usize;
                    let truncated: String = title.chars().take(max_chars).collect();
                    let label = format!(" {truncated} ");
                    let mut x = rect.x.saturating_add(1);
                    // Overlay on the pane's top border row, NOT its first content row.
                    let y = rect.y.saturating_sub(1);
                    for ch in label.chars() {
                        if x >= rect.x.saturating_add(rect.width).saturating_sub(1) {
                            break;
                        }
                        let cell = RenderCell {
                            ch,
                            fg: Some(fg),
                            bg: None,
                            attrs: RenderAttrs::default(),
                            extended: None,
                            wide_continuation: false,
                        };
                        self.buffer.set_cell(x, y, cell);
                        x = x.saturating_add(1);
                    }
                }
            }
        }

        // 4. Status bar (bottom rows). The bar is rendered in row 0 of
        // the reserved area; any extra rows above it (when
        // status_bar_height > 1) are blanked so they don't show stale
        // pane content. Multi-row bars are a future task.
        if let Some(bar) = frame.status_bar {
            let bar_top = self
                .buffer
                .height()
                .saturating_sub(self.config.status_bar_height);
            for row_offset in 0..self.config.status_bar_height {
                let row = bar_top + row_offset;
                if row_offset + 1 == self.config.status_bar_height {
                    let cells = bar.render_row(self.buffer.width());
                    for (col, cell) in cells.into_iter().enumerate() {
                        self.buffer.set_cell(col as u16, row, cell);
                    }
                } else {
                    let blank = RenderCell::default();
                    for col in 0..self.buffer.width() {
                        self.buffer.set_cell(col, row, blank.clone());
                    }
                }
            }
        }

        let compose_time = compose_start.elapsed();

        // 5. Diff + render.
        let diff_start = Instant::now();
        let dirty = if self.force_full_redraw {
            self.buffer.invalidate();
            self.force_full_redraw = false;
            self.buffer.diff()
        } else {
            self.buffer.diff()
        };
        let dirty_count = dirty.len();
        let diff_time = diff_start.elapsed();

        let render_start = Instant::now();

        // Position the cursor inside the focused pane.
        // Cursor at the *exact* right edge (col == width) is valid —
        // that's a "wrap pending" terminal state. Use ≤ on the upper
        // bound so we don't hide it.
        let target_cursor =
            if let Some((_, rect)) = pane_rects.iter().find(|(id, _)| *id == frame.focused) {
                if let Some(vt) = frame.vts.get(&frame.focused) {
                    let cur = vt.cursor();
                    let defaults = vt.default_colors();
                    // Same cursor-following viewport the pane content used, so
                    // the cursor lands on the row that actually shows it (#108).
                    let row_offset = crate::viewport::pane_view_row_offset(
                        vt.grid().rows(),
                        rect.height as usize,
                        cur.row,
                    );
                    let vy = (cur.row.saturating_sub(row_offset) as u16)
                        .min(rect.height.saturating_sub(1));
                    let sx = rect
                        .x
                        .saturating_add((cur.col as u16).min(rect.width.saturating_sub(1)));
                    let sy = rect.y.saturating_add(vy);
                    if cur.visible
                        && sx < rect.x.saturating_add(rect.width)
                        && sy < rect.y.saturating_add(rect.height)
                        && sx < self.buffer.width()
                        && sy < self.buffer.height()
                    {
                        Some(CursorTarget {
                            x: sx,
                            y: sy,
                            visual: CursorVisual {
                                shape: cur.shape,
                                color: defaults.cursor,
                            },
                        })
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };
        self.render_dirty_and_cursor(&dirty, target_cursor)?;
        // SPIKE: pictures go out after the cells, so text never overdraws them.
        let repainted_rows: std::collections::HashSet<u16> =
            dirty.iter().map(|c| c.row).collect();
        self.spike_emit_images(&spike_placed, &repainted_rows, &spike_epochs)?;

        let render_time = render_start.elapsed();
        self.buffer.swap();
        let total_time = frame_start.elapsed();

        let stats = RenderStats {
            dirty_cells: dirty_count,
            total_cells: (self.buffer.width() as usize) * (self.buffer.height() as usize),
            compose_time_us: compose_time.as_micros() as u64,
            diff_time_us: diff_time.as_micros() as u64,
            render_time_us: render_time.as_micros() as u64,
            total_time_us: total_time.as_micros() as u64,
        };
        self.last_stats = Some(stats.clone());
        Ok(stats)
    }

    /// Compose a single pane's VT grid into the framebuffer at `rect`.
    fn compose_pane(&mut self, rect: Rect, vt: &shux_vt::VirtualTerminal) {
        let grid = vt.grid();
        let defaults = vt.default_colors();
        let total_rows = grid.rows();
        let total_cols = grid.cols();
        let visible_rows = rect.height as usize;
        let visible_cols = rect.width as usize;

        // The VT grid can be taller than the pane rect (an oversized
        // `pane set-size`, a split that shrank the rect, or a transient
        // resize-down). Use a cursor-following viewport so content and the
        // cursor mapping below agree — a plain bottom anchor rendered
        // top-of-grid content blank while the cursor still painted (#108).
        let row_offset =
            crate::viewport::pane_view_row_offset(total_rows, visible_rows, vt.cursor().row);

        for r in 0..visible_rows {
            let grid_row = row_offset + r;
            if grid_row >= total_rows {
                continue;
            }
            let row = grid.visible_row(grid_row);
            for c in 0..visible_cols {
                if c >= total_cols {
                    break;
                }
                let cell = &row[c];
                let rcell = RenderCell::from_vt_cell_with_defaults(cell, defaults);
                self.buffer
                    .set_cell(rect.x + c as u16, rect.y + r as u16, rcell);
            }
        }
    }

    /// Render a placeholder string centered in the rect. Used when a pane
    /// has no VT (shouldn't happen in practice, but fail-soft is nicer
    /// than panicking during a render cycle).
    fn compose_placeholder(&mut self, rect: Rect, text: &str) {
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        let text_chars: Vec<char> = text.chars().collect();
        let col = rect
            .x
            .saturating_add(((rect.width as usize).saturating_sub(text_chars.len())) as u16 / 2);
        let row = rect.y + rect.height / 2;
        for (i, ch) in text_chars.iter().enumerate() {
            self.buffer.set_cell(
                col + i as u16,
                row,
                RenderCell {
                    ch: *ch,
                    fg: None,
                    bg: None,
                    attrs: RenderAttrs {
                        dim: true,
                        ..Default::default()
                    },
                    extended: None,
                    wide_continuation: false,
                },
            );
        }
    }

    /// Get statistics from the last render pass.
    pub fn last_stats(&self) -> Option<&RenderStats> {
        self.last_stats.as_ref()
    }

    /// Clear the screen and reset internal state. Call during
    /// initialization or when restoring the terminal.
    pub fn clear(&mut self) -> io::Result<()> {
        self.backend.clear_screen()?;
        self.force_full_redraw = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::Cursor;

    use super::*;
    use crate::buffer::RenderCell;
    use shux_vt::VirtualTerminal;

    /// Helper: create a compositor backed by a `Cursor<Vec<u8>>` sink.
    /// The Cursor wrapper is owned by the compositor, so there are no
    /// borrow conflicts when calling `render_frame` multiple times.
    fn make_compositor(
        width: u16,
        height: u16,
        config: CompositorConfig,
    ) -> RenderCompositor<Cursor<Vec<u8>>> {
        RenderCompositor::new(width, height, Cursor::new(Vec::new()), config)
    }

    #[test]
    fn test_compositor_single_pane_render() {
        let mut output = Vec::new();
        let config = CompositorConfig::default();
        let mut compositor = RenderCompositor::new(80, 24, &mut output, config);

        // Render a simple grid with "Hello" in the first row
        let stats = compositor
            .render_frame(
                |col, _row| {
                    let chars = ['H', 'e', 'l', 'l', 'o'];
                    if (col as usize) < chars.len() {
                        RenderCell::text(chars[col as usize])
                    } else {
                        RenderCell::default()
                    }
                },
                80,
                24,
                Some((5, 0)),
            )
            .unwrap();

        // First render should touch at least some cells
        assert!(stats.dirty_cells > 0);

        // Output should contain the characters
        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains('H'));
        assert!(output_str.contains('e'));
        assert!(output_str.contains('o'));
    }

    #[test]
    fn test_compositor_incremental_render() {
        let mut compositor = make_compositor(10, 5, CompositorConfig::default());

        // First render: "Hello" on row 0 only
        compositor
            .render_frame(
                |col, row| {
                    if row == 0 {
                        let chars = ['H', 'e', 'l', 'l', 'o'];
                        if (col as usize) < chars.len() {
                            RenderCell::text(chars[col as usize])
                        } else {
                            RenderCell::default()
                        }
                    } else {
                        RenderCell::default()
                    }
                },
                10,
                5,
                None,
            )
            .unwrap();

        // Second render: "Hxllo" on row 0 only (only col 1 changed)
        let stats = compositor
            .render_frame(
                |col, row| {
                    if row == 0 {
                        let chars = ['H', 'x', 'l', 'l', 'o'];
                        if (col as usize) < chars.len() {
                            RenderCell::text(chars[col as usize])
                        } else {
                            RenderCell::default()
                        }
                    } else {
                        RenderCell::default()
                    }
                },
                10,
                5,
                None,
            )
            .unwrap();

        // Only 1 cell should be dirty (the 'x' replacing 'e' at row 0, col 1)
        assert_eq!(stats.dirty_cells, 1);
    }

    #[test]
    fn test_compositor_resize() {
        let mut compositor = make_compositor(10, 5, CompositorConfig::default());

        // Initial render
        compositor
            .render_frame(|_, _| RenderCell::text('A'), 10, 5, None)
            .unwrap();

        // Resize
        compositor.resize(20, 10);

        // Render after resize should be a full redraw
        let stats = compositor
            .render_frame(|_, _| RenderCell::text('B'), 20, 10, None)
            .unwrap();

        // All cells should be dirty (full redraw after resize)
        assert_eq!(stats.dirty_cells, 20 * 10);
    }

    #[test]
    fn test_compositor_with_border() {
        let config = CompositorConfig {
            show_border: true,
            status_bar_height: 0,
            ..Default::default()
        };
        let compositor = make_compositor(10, 5, config);

        let (x, y, w, h) = compositor.content_area();
        // With border: content starts at (1,1), width and height reduced by 2
        assert_eq!(x, 1);
        assert_eq!(y, 1);
        assert_eq!(w, 8);
        assert_eq!(h, 3);
    }

    #[test]
    fn test_compositor_with_status_bar() {
        let config = CompositorConfig {
            show_border: false,
            status_bar_height: 1,
            ..Default::default()
        };
        let compositor = make_compositor(80, 24, config);

        let (x, y, w, h) = compositor.content_area();
        assert_eq!(x, 0);
        assert_eq!(y, 0);
        assert_eq!(w, 80);
        assert_eq!(h, 23); // 24 - 1 for status bar
    }

    #[test]
    fn test_compositor_with_border_and_status_bar() {
        let config = CompositorConfig {
            show_border: true,
            status_bar_height: 1,
            ..Default::default()
        };
        let compositor = make_compositor(80, 24, config);

        let (x, y, w, h) = compositor.content_area();
        assert_eq!(x, 1);
        assert_eq!(y, 1);
        assert_eq!(w, 78); // 80 - 2 for borders
        assert_eq!(h, 21); // 24 - 2 borders - 1 status bar
    }

    #[test]
    fn test_render_stats_reported() {
        let mut compositor = make_compositor(10, 5, CompositorConfig::default());

        assert!(compositor.last_stats().is_none());

        compositor
            .render_frame(|_, _| RenderCell::default(), 10, 5, None)
            .unwrap();

        let stats = compositor.last_stats().unwrap();
        assert_eq!(stats.total_cells, 50);
    }

    #[test]
    fn test_force_redraw() {
        let mut compositor = make_compositor(10, 5, CompositorConfig::default());

        // Initial render (full redraw forced automatically)
        compositor
            .render_frame(|_, _| RenderCell::text('A'), 10, 5, None)
            .unwrap();

        // Second render -- identical content, should have 0 dirty cells
        let stats = compositor
            .render_frame(|_, _| RenderCell::text('A'), 10, 5, None)
            .unwrap();
        assert_eq!(stats.dirty_cells, 0);

        // Force redraw
        compositor.force_redraw();

        // Third render -- same content but force redraw means all dirty
        let stats = compositor
            .render_frame(|_, _| RenderCell::text('A'), 10, 5, None)
            .unwrap();
        assert_eq!(stats.dirty_cells, 10 * 5);
    }

    #[test]
    fn test_compositor_clear() {
        let mut output = Vec::new();
        let config = CompositorConfig::default();
        let mut compositor = RenderCompositor::new(10, 5, &mut output, config);

        compositor.clear().unwrap();
        assert!(!output.is_empty());
    }

    #[test]
    fn test_compositor_border_rendering() {
        let mut output = Vec::new();
        let config = CompositorConfig {
            show_border: true,
            status_bar_height: 0,
            ..Default::default()
        };
        let mut compositor = RenderCompositor::new(10, 5, &mut output, config);

        let stats = compositor
            .render_frame(|_, _| RenderCell::text('X'), 8, 3, None)
            .unwrap();

        // All cells should be dirty on first render
        assert_eq!(stats.dirty_cells, 10 * 5);

        // Output should contain box-drawing characters
        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains('\u{250C}')); // top-left corner
        assert!(output_str.contains('\u{2510}')); // top-right corner
        assert!(output_str.contains('\u{2514}')); // bottom-left corner
        assert!(output_str.contains('\u{2518}')); // bottom-right corner
        assert!(output_str.contains('\u{2500}')); // horizontal
        assert!(output_str.contains('\u{2502}')); // vertical
        // Content character should also be present
        assert!(output_str.contains('X'));
    }

    #[test]
    fn test_compositor_cursor_hidden_when_none() {
        let mut output = Vec::new();
        let config = CompositorConfig::default();
        let mut compositor = RenderCompositor::new(10, 5, &mut output, config);

        // Render with no cursor
        compositor
            .render_frame(|_, _| RenderCell::default(), 10, 5, None)
            .unwrap();

        // The output should contain Hide cursor but not Show cursor
        // (since cursor_pos is None)
        let output_str = String::from_utf8_lossy(&output);
        // crossterm Hide = CSI ?25l, Show = CSI ?25h
        assert!(output_str.contains("\x1b[?25l")); // Hide
        assert!(!output_str.contains("\x1b[?25h")); // No Show
    }

    #[test]
    fn test_compositor_cursor_shown_when_some() {
        let mut output = Vec::new();
        let config = CompositorConfig::default();
        let mut compositor = RenderCompositor::new(10, 5, &mut output, config);

        // Render with cursor at (3, 2)
        compositor
            .render_frame(|_, _| RenderCell::default(), 10, 5, Some((3, 2)))
            .unwrap();

        let output_str = String::from_utf8_lossy(&output);
        // Should contain both Hide and Show
        assert!(output_str.contains("\x1b[?25l")); // Hide (during render)
        assert!(output_str.contains("\x1b[?25h")); // Show (after render)
    }

    #[test]
    fn test_compositor_does_not_churn_cursor_when_idle() {
        let mut compositor = RenderCompositor::new(10, 5, Vec::new(), CompositorConfig::default());

        compositor
            .render_frame(|_, _| RenderCell::default(), 10, 5, Some((3, 2)))
            .unwrap();
        compositor.inner_mut().clear();

        let stats = compositor
            .render_frame(|_, _| RenderCell::default(), 10, 5, Some((3, 2)))
            .unwrap();

        assert_eq!(stats.dirty_cells, 0);
        assert!(
            compositor.inner().is_empty(),
            "idle render should not emit cursor hide/show churn: {:?}",
            String::from_utf8_lossy(compositor.inner())
        );
    }

    #[test]
    fn test_compositor_moves_cursor_without_hide_show_when_only_cursor_changes() {
        let mut compositor = RenderCompositor::new(10, 5, Vec::new(), CompositorConfig::default());

        compositor
            .render_frame(|_, _| RenderCell::default(), 10, 5, Some((3, 2)))
            .unwrap();
        compositor.inner_mut().clear();

        let stats = compositor
            .render_frame(|_, _| RenderCell::default(), 10, 5, Some((4, 2)))
            .unwrap();
        let output = String::from_utf8_lossy(compositor.inner());

        assert_eq!(stats.dirty_cells, 0);
        assert!(
            output.contains("\x1b[3;5H"),
            "missing cursor move: {output:?}"
        );
        assert!(!output.contains("\x1b[?25l"), "unexpected hide: {output:?}");
        assert!(!output.contains("\x1b[?25h"), "unexpected show: {output:?}");
    }

    #[test]
    fn test_multi_pane_cursor_shape_and_color_are_emitted() {
        let pid = PaneId::new();
        let layout = LayoutNode::Leaf { pane: pid };
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"\x1b[3 q\x1b]12;#00ff80\x1b\\A");
        let mut vts = HashMap::new();
        vts.insert(pid, &vt);
        let frame = MultiPaneFrame {
            layout: &layout,
            zoom: None,
            focused: pid,
            vts: &vts,
            titles: None,
            status_bar: None,
        };
        let mut compositor = RenderCompositor::new(10, 3, Vec::new(), CompositorConfig::default());

        compositor.render_multi_pane(frame).unwrap();

        let output = String::from_utf8_lossy(compositor.inner());
        assert!(
            output.contains("\x1b[4 q"),
            "missing underline cursor shape: {output:?}"
        );
        assert!(
            output.contains("\x1b]12;#00ff80\x1b\\"),
            "missing cursor color: {output:?}"
        );
    }

    #[test]
    fn test_multi_pane_attach_preserves_vt_cell_colors_when_no_color_is_set() {
        crossterm::style::Colored::set_ansi_color_disabled(true);

        let pid = PaneId::new();
        let layout = LayoutNode::Leaf { pane: pid };
        let mut vt = VirtualTerminal::new(3, 12);
        vt.process(
            b"\x1b[38;2;10;20;30;48;2;40;50;60mT\x1b[0m\
              \x1b[38;5;196mI\x1b[0m\
              \x1b[31mB",
        );
        let mut vts = HashMap::new();
        vts.insert(pid, &vt);
        let frame = MultiPaneFrame {
            layout: &layout,
            zoom: None,
            focused: pid,
            vts: &vts,
            titles: None,
            status_bar: None,
        };
        let mut compositor = RenderCompositor::new(
            12,
            3,
            Vec::new(),
            CompositorConfig {
                border_style: BorderStyle::None,
                ..CompositorConfig::default()
            },
        );

        compositor.render_multi_pane(frame).unwrap();
        crossterm::style::Colored::set_ansi_color_disabled(false);

        let output = String::from_utf8_lossy(compositor.inner());
        assert!(
            output.contains("\x1b[38;2;10;20;30m"),
            "attach output stripped truecolor fg: {output:?}"
        );
        assert!(
            output.contains("\x1b[48;2;40;50;60m"),
            "attach output stripped truecolor bg: {output:?}"
        );
        assert!(
            output.contains("\x1b[38;5;196m"),
            "attach output stripped 256-color fg: {output:?}"
        );
        assert!(
            output.contains("\x1b[38;5;1m"),
            "attach output stripped indexed basic fg: {output:?}"
        );
    }

    #[test]
    fn test_multi_pane_hidden_cursor_is_not_shown() {
        let pid = PaneId::new();
        let layout = LayoutNode::Leaf { pane: pid };
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"\x1b[?25lA");
        let mut vts = HashMap::new();
        vts.insert(pid, &vt);
        let frame = MultiPaneFrame {
            layout: &layout,
            zoom: None,
            focused: pid,
            vts: &vts,
            titles: None,
            status_bar: None,
        };
        let mut compositor = RenderCompositor::new(10, 3, Vec::new(), CompositorConfig::default());

        compositor.render_multi_pane(frame).unwrap();

        let output = String::from_utf8_lossy(compositor.inner());
        assert!(
            output.contains("\x1b[?25l"),
            "initial render should hide host cursor: {output:?}"
        );
        assert!(
            !output.contains("\x1b[?25h"),
            "hidden VT cursor must not be shown by attach: {output:?}"
        );
    }

    #[test]
    fn test_compositor_grid_smaller_than_content_area() {
        // When the grid is smaller than the content area, only the
        // grid portion should be filled; the rest stays blank.
        let mut compositor = make_compositor(20, 10, CompositorConfig::default());

        // Grid is only 5x3
        let stats = compositor
            .render_frame(|_, _| RenderCell::text('Z'), 5, 3, None)
            .unwrap();

        // First frame is always full redraw, so all 200 cells are dirty
        assert_eq!(stats.dirty_cells, 200);

        // Second render: grid content unchanged, blank area unchanged
        let stats = compositor
            .render_frame(|_, _| RenderCell::text('Z'), 5, 3, None)
            .unwrap();
        assert_eq!(stats.dirty_cells, 0);
    }

    /// issue #108 (attach path): a pane whose grid is taller than its layout
    /// rect must render its top content in a live `render_multi_pane` frame,
    /// not just the bottom-anchored (blank) tail. Red before the fix.
    #[test]
    fn test_multi_pane_oversized_pane_renders_top_content() {
        let pid = PaneId::new();
        let layout = LayoutNode::Leaf { pane: pid };
        // 60-row grid, top content, composited into a 36-row buffer.
        let mut vt = VirtualTerminal::new(60, 200);
        vt.process(b"OVERSIZE-MARKER\r\nrow2\r\n");
        let mut vts = HashMap::new();
        vts.insert(pid, &vt);
        let frame = MultiPaneFrame {
            layout: &layout,
            zoom: None,
            focused: pid,
            vts: &vts,
            titles: None,
            status_bar: None,
        };
        // No border / status bar so the pane rect is the whole 200x36 buffer:
        // isolates compose_pane's vertical clipping.
        let mut compositor = RenderCompositor::new(
            200,
            36,
            Vec::new(),
            CompositorConfig {
                border_style: BorderStyle::None,
                ..CompositorConfig::default()
            },
        );
        compositor.render_multi_pane(frame).unwrap();

        // The diff renderer emits each cell preceded by a cursor-move escape,
        // so glyphs aren't a contiguous substring — strip CSI/OSC and read the
        // painted characters back.
        let painted = strip_ansi(&String::from_utf8_lossy(compositor.inner()));
        assert!(
            painted.contains("OVERSIZE-MARKER"),
            "attach dropped the oversized pane's top content: painted={painted:?}"
        );
        assert!(
            painted.contains("row2"),
            "attach dropped the oversized pane's second row: painted={painted:?}"
        );
    }

    /// Strip ANSI escape sequences (CSI `\x1b[...<final>` and OSC
    /// `\x1b]...\x07`/`\x1b\\`) so a test can read the glyphs a diff render
    /// actually painted.
    fn strip_ansi(s: &str) -> String {
        let bytes: Vec<char> = s.chars().collect();
        let mut out = String::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == '\u{1b}' {
                i += 1;
                match bytes.get(i) {
                    Some('[') => {
                        // CSI: params until a final byte in 0x40..=0x7E.
                        i += 1;
                        while i < bytes.len() && !('\u{40}'..='\u{7e}').contains(&bytes[i]) {
                            i += 1;
                        }
                        i += 1; // consume the final byte
                    }
                    Some(']') => {
                        // OSC: until BEL or ST (ESC \).
                        i += 1;
                        while i < bytes.len() && bytes[i] != '\u{07}' {
                            if bytes[i] == '\u{1b}' && bytes.get(i + 1) == Some(&'\\') {
                                i += 1;
                                break;
                            }
                            i += 1;
                        }
                        i += 1;
                    }
                    _ => i += 1,
                }
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        out
    }

    #[test]
    fn test_performance_80x24_under_budget() {
        // A catastrophic-regression guard, not the PRD's 8ms frame budget: this
        // runs on an unoptimised test build (opt-level 0 is ~4.2x slower here),
        // so it measures codegen and runner load. Nothing asserts the 8ms budget
        // now — that needs a release-profile bench, which does not exist yet.
        let mut compositor = make_compositor(80, 24, CompositorConfig::default());

        let stats = compositor
            .render_frame(
                |col, row| {
                    let ch = (b'A' + ((col + row) % 26) as u8) as char;
                    RenderCell::text(ch)
                },
                80,
                24,
                Some((0, 0)),
            )
            .unwrap();

        assert!(
            stats.total_time_us < 100_000,
            "Full 80x24 render took {}us — an order of magnitude over any sane cost",
            stats.total_time_us
        );
    }
}
