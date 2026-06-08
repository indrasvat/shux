//! shux-raster — turn a `shux-vt` Grid<Cell> into a PNG without any terminal emulator.
//!
//! - One bundled monospace font: **JetBrains Mono Nerd Font Mono Regular**
//!   (Nerd Fonts patched, OFL). The full NF-patched build replaces the
//!   prior 270 KB JBM Regular + 5 KB curated symbols subset combo. Every
//!   Nerd Font codepoint (rust, node, python, helm, branch, …) resolves
//!   out of the box — no subset-regen ritual, no tofu when a user's
//!   `[[statusbar.segment]]` script (starship, kubectl, …) emits NF
//!   glyphs we didn't anticipate.
//! - **Text-symbol fallbacks**: bundled Noto Sans Math, Noto Sans
//!   Symbols 2, and Noto Sans Symbols cover common TUI text glyphs
//!   such as rerun arrows, braille spinners, key legends, and status
//!   symbols that programming fonts usually omit.
//! - **Monochrome emoji fallback**: bundled Noto Emoji Regular
//!   (~860 KB OFL) as the final entry in the font chain. Standalone
//!   emoji (🍺 🧩 🦀 🚀 ⚡ …) resolve to legible monochrome glyphs in
//!   PNG snapshots; the live `attach` path is unaffected (your
//!   terminal's font stack handles it there).
//! - Per-cell glyph rendering via `fontdue` (pure Rust, no system deps).
//! - 16-color ANSI + 256-indexed + truecolor RGB palette.
//! - Bold (synthetic offset), dim, underline, strikethrough, inverse.
//! - Block/underline/bar cursor, including OSC 12 cursor color when present.
//! - PNG output via the `image` crate.
//!
//! Out of scope (v1): **colour** emoji and **composed** emoji. Colour
//! requires a COLRv1/CBDT-aware rasterizer (fontdue is grayscale-only).
//! Composed emoji (ZWJ sequences like `👨‍💻`, skin-tone modifiers,
//! regional-indicator flag pairs, VS16 like `🛠️`) are gated on
//! grapheme-cluster storage in `shux-vt`, which today keys cells on a
//! single `char` — even a swap to swash would not reconstruct what the
//! parser split apart. Tracked as future work. Also deferred: ligatures
//! via shaping, italics with a real italic face, OSC 8 hyperlink
//! styling, RTL text, GPU acceleration.

use fontdue::{Font, FontSettings};
use image::{ImageBuffer, Rgba, RgbaImage};
use shux_vt::{Cell, CellFlags, Color, CursorShape, Grid, UnderlineStyle};

/// Embedded text font. JetBrains Mono Nerd Font Mono Regular, the
/// upstream Nerd Fonts patched build (2.4 MB) under SIL Open Font
/// License (see `assets/OFL.txt`). Bundles the full NF glyph set so
/// every codepoint a user's script-driven status segment can emit
/// renders correctly OOTB. To update: pull the latest from
/// <https://github.com/ryanoasis/nerd-fonts/releases/latest/>.
const FONT_BYTES: &[u8] = include_bytes!("../assets/JetBrainsMonoNerdFontMono-Regular.ttf");

/// Embedded mathematical / arrow symbol fallback. Noto Sans Math
/// covers common text-presentation arrows such as U+21BB (↻) that
/// terminal UIs use for rerun / refresh affordances.
const MATH_FONT_BYTES: &[u8] = include_bytes!("../assets/NotoSansMath-Regular.ttf");

/// Embedded text-symbol fallback. Noto Sans Symbols 2 covers braille
/// spinner glyphs (U+2800..U+28FF) and many status / UI symbols.
const SYMBOLS_FONT_BYTES: &[u8] = include_bytes!("../assets/NotoSansSymbols2-Regular.ttf");

/// Embedded text-symbol fallback. Noto Sans Symbols covers
/// Miscellaneous Technical glyphs that Symbols 2 deliberately does
/// not, including U+2387 (⎇) ALTERNATIVE KEY SYMBOL.
const SYMBOLS_1_FONT_BYTES: &[u8] = include_bytes!("../assets/NotoSansSymbols-Regular.ttf");

/// Embedded monochrome emoji fallback. Noto Emoji Regular (~860 KB
/// under the same SIL OFL v1.1; license text in `assets/OFL.txt`).
/// Appended to every rasterizer's font chain so standalone emoji
/// codepoints resolve to legible glyphs in PNG snapshots instead of
/// rendering as tofu. Composed emoji (ZWJ / VS16 / regional-indicator
/// flag pairs) are out of scope until `shux-vt` gains grapheme-cluster
/// storage. See `assets/NOTICE.md` for re-fetch instructions.
const EMOJI_FONT_BYTES: &[u8] = include_bytes!("../assets/NotoEmoji-Regular.ttf");

/// Builtin font token for the bundled JetBrains Mono Nerd Font.
pub const BUILTIN_NERD_FONT: &str = "builtin:nerd-font";
/// Builtin font token for the bundled Noto Sans Math fallback.
pub const BUILTIN_MATH: &str = "builtin:math";
/// Builtin font token for the bundled Noto Sans Symbols 2 fallback.
pub const BUILTIN_SYMBOLS: &str = "builtin:symbols";
/// Builtin font token for the bundled Noto Sans Symbols fallback.
pub const BUILTIN_SYMBOLS_LEGACY: &str = "builtin:symbols-legacy";
/// Builtin font token for the bundled monochrome Noto Emoji fallback.
pub const BUILTIN_EMOJI: &str = "builtin:emoji";

/// Default fallback chain appended after the primary font.
pub const DEFAULT_FALLBACK_FONT_SPECS: &[&str] = &[
    BUILTIN_NERD_FONT,
    BUILTIN_MATH,
    BUILTIN_SYMBOLS,
    BUILTIN_SYMBOLS_LEGACY,
    BUILTIN_EMOJI,
];

/// Resolve a builtin font token to embedded bytes.
pub fn builtin_font_bytes(spec: &str) -> Option<&'static [u8]> {
    match spec {
        BUILTIN_NERD_FONT => Some(FONT_BYTES),
        BUILTIN_MATH => Some(MATH_FONT_BYTES),
        BUILTIN_SYMBOLS => Some(SYMBOLS_FONT_BYTES),
        BUILTIN_SYMBOLS_LEGACY => Some(SYMBOLS_1_FONT_BYTES),
        BUILTIN_EMOJI => Some(EMOJI_FONT_BYTES),
        _ => None,
    }
}

/// Rasterizer errors.
#[derive(Debug, thiserror::Error)]
pub enum RasterError {
    #[error("font load failed: {0}")]
    Font(String),
    #[error("font has no horizontal line metrics at size {0}")]
    NoMetrics(f32),
}

/// Pixel color (sRGB, no alpha — caller supplies opaque pixels).
pub type Rgb = [u8; 3];

/// Visual options for a render pass.
#[derive(Debug, Clone)]
pub struct RasterOptions {
    /// Color used when a cell asks for `Color::Default` foreground.
    pub fg_default: Rgb,
    /// Color used when a cell asks for `Color::Default` background.
    pub bg_default: Rgb,
    /// If set, the cursor cell `(row, col)` is rendered as an inverse block.
    pub cursor: Option<(usize, usize)>,
    /// Cursor shape used when `cursor` is set.
    pub cursor_shape: CursorShape,
    /// Cursor color used when `cursor` is set. `None` preserves the legacy
    /// inverse-block behavior for block cursors and uses `fg_default` for
    /// underline/bar cursors.
    pub cursor_color: Option<Rgb>,
}

impl Default for RasterOptions {
    fn default() -> Self {
        Self {
            fg_default: [220, 220, 220],
            bg_default: [16, 16, 24],
            cursor: None,
            cursor_shape: CursorShape::Block,
            cursor_color: None,
        }
    }
}

/// Owning rasterizer. Holds an ordered font fallback chain plus
/// derived cell metrics. The first font that has a glyph for the
/// requested character wins. The chain always ends with the bundled
/// NF JetBrains Mono, text-symbol fallbacks, and the bundled
/// monochrome Noto Emoji so PNG snapshots never tofu on common glyphs;
/// user-supplied `appearance.font` slots in front of those builtins.
pub struct Rasterizer {
    /// Fallback chain: try fonts[0] first, then [1], etc. Metrics
    /// (cell_w / cell_h / ascent) are derived from fonts[0] only —
    /// the primary text font dominates the grid geometry. Fallback
    /// glyphs from later entries are size-fitted and centered within
    /// each cell's bounding box (see `draw_cell`) so emoji rendered
    /// from Noto Emoji don't spill into adjacent columns.
    fonts: Vec<Font>,
    font_size: f32,
    cell_w: u32,
    cell_h: u32,
    ascent: f32,
}

impl Rasterizer {
    /// Construct a rasterizer at the given font size (in pixels)
    /// using the bundled NF-patched JetBrains Mono as primary, with
    /// text-symbol fallbacks and bundled monochrome Noto Emoji. The
    /// resulting chain is `[JBM_NF, NotoSansMath, NotoSansSymbols2,
    /// NotoSansSymbols, NotoEmoji]`.
    pub fn new(font_size: f32) -> Result<Self, RasterError> {
        Self::with_fonts(
            font_size,
            [
                FONT_BYTES,
                MATH_FONT_BYTES,
                SYMBOLS_FONT_BYTES,
                SYMBOLS_1_FONT_BYTES,
                EMOJI_FONT_BYTES,
            ],
        )
    }

    /// Construct a rasterizer with a user-supplied primary font, plus
    /// the default builtin fallback chain. Lets users override the
    /// typeface via `appearance.font` while still getting NF icons,
    /// text-symbol coverage, and non-tofu emoji in PNG snapshots
    /// regardless of their chosen primary's coverage.
    pub fn with_primary_font(font_size: f32, primary: &[u8]) -> Result<Self, RasterError> {
        Self::with_primary_and_fallback_fonts(
            font_size,
            Some(primary),
            [
                FONT_BYTES,
                MATH_FONT_BYTES,
                SYMBOLS_FONT_BYTES,
                SYMBOLS_1_FONT_BYTES,
                EMOJI_FONT_BYTES,
            ],
        )
    }

    /// Construct a rasterizer from an optional primary font and an
    /// explicit fallback chain. When `primary` is `None`, the first
    /// fallback font becomes the primary metrics font.
    pub fn with_primary_and_fallback_fonts<'a, I>(
        font_size: f32,
        primary: Option<&'a [u8]>,
        fallbacks: I,
    ) -> Result<Self, RasterError>
    where
        I: IntoIterator<Item = &'a [u8]>,
    {
        let fonts = primary.into_iter().chain(fallbacks);
        Self::with_fonts(font_size, fonts)
    }

    /// Construct a rasterizer from an explicit fallback chain.
    /// `fonts[0]` is the primary text font; later entries are
    /// consulted for codepoints not present in earlier ones.
    pub fn with_fonts<'a, I>(font_size: f32, fonts: I) -> Result<Self, RasterError>
    where
        I: IntoIterator<Item = &'a [u8]>,
    {
        let settings = FontSettings {
            scale: font_size,
            ..FontSettings::default()
        };
        let mut parsed: Vec<Font> = Vec::new();
        for bytes in fonts {
            let f = Font::from_bytes(bytes, settings).map_err(|e| RasterError::Font(e.into()))?;
            parsed.push(f);
        }
        if parsed.is_empty() {
            return Err(RasterError::Font("no fonts provided".into()));
        }
        let primary = &parsed[0];
        let line = primary
            .horizontal_line_metrics(font_size)
            .ok_or(RasterError::NoMetrics(font_size))?;
        let m = primary.metrics('M', font_size);
        let cell_w = m.advance_width.ceil().max(1.0) as u32;
        let cell_h = line.new_line_size.ceil().max(1.0) as u32;
        Ok(Self {
            fonts: parsed,
            font_size,
            cell_w,
            cell_h,
            ascent: line.ascent,
        })
    }

    /// Pick which font in the fallback chain has a glyph for `ch`.
    /// Returns the primary font if no fallback has the glyph (so the
    /// caller still gets fontdue's "missing glyph" rendering — better
    /// than panicking or skipping the cell).
    fn font_for(&self, ch: char) -> &Font {
        &self.fonts[self.font_idx_for(ch)]
    }

    /// Index of the font in the fallback chain that has a glyph for
    /// `ch`. Returns 0 (primary) if no entry has the glyph. The
    /// renderer uses this to distinguish "primary glyph, position by
    /// baseline metrics" from "fallback glyph, size-fit + center
    /// inside the cell box" — fallback fonts (especially Noto Emoji)
    /// have native advance widths that don't match the primary text
    /// font's cell metric, so blitting them at the primary's geometry
    /// would spill across cell boundaries.
    fn font_idx_for(&self, ch: char) -> usize {
        for (i, f) in self.fonts.iter().enumerate() {
            if f.lookup_glyph_index(ch) != 0 {
                return i;
            }
        }
        0
    }

    /// Number of fonts in the fallback chain. Exposed for tests + the
    /// glyph-coverage assertion suite.
    pub fn font_count(&self) -> usize {
        self.fonts.len()
    }

    /// Does the fallback chain resolve `ch` to a real glyph?
    /// `false` means every font would render the missing-glyph box.
    /// Exposed for deterministic coverage tests so we can assert
    /// "the bundled font has a glyph for every codepoint the shux
    /// statusbar might emit" without inspecting pixels.
    pub fn has_glyph(&self, ch: char) -> bool {
        self.fonts.iter().any(|f| f.lookup_glyph_index(ch) != 0)
    }

    /// Diagnostic helper: count non-empty (coverage > 0) pixels
    /// fontdue would emit when rasterizing `ch` at the rasterizer's
    /// current font size, using the first font in the fallback chain
    /// that has the glyph.
    ///
    /// `has_glyph` only confirms the font's `cmap` has an entry for
    /// the codepoint — but some fonts ship "stub" glyphs whose outline
    /// is empty (the box would render blank pixels, indistinguishable
    /// from tofu). This pixel-coverage check catches those: an outline
    /// that should render a visible glyph has at least a handful of
    /// non-zero coverage samples.
    ///
    /// **Not a hot-path API.** Allocates a fresh bitmap on every call.
    /// Intended for the deterministic tofu-free assertion suite and
    /// future debug commands like `shux raster probe <codepoint>`.
    /// Production `render()` also calls `font.rasterize()` per cell
    /// (no glyph cache today; one is a documented future
    /// optimisation), so don't lean on this for production rendering
    /// either — it's diagnostic-only.
    pub fn glyph_pixel_count(&self, ch: char) -> usize {
        let font = self.font_for(ch);
        let (_metrics, bitmap) = font.rasterize(ch, self.font_size);
        bitmap.iter().filter(|&&px| px > 0).count()
    }

    /// Cell dimensions in pixels.
    pub fn cell_size(&self) -> (u32, u32) {
        (self.cell_w, self.cell_h)
    }

    /// Render the visible grid to an RGBA image.
    pub fn render(&self, grid: &Grid, opts: &RasterOptions) -> RgbaImage {
        let cols = grid.cols() as u32;
        let rows = grid.rows() as u32;
        let w = cols * self.cell_w;
        let h = rows * self.cell_h;
        let mut img: RgbaImage = ImageBuffer::from_pixel(
            w,
            h,
            Rgba([
                opts.bg_default[0],
                opts.bg_default[1],
                opts.bg_default[2],
                255,
            ]),
        );

        let cursor_block_cell = match (opts.cursor, opts.cursor_shape, opts.cursor_color) {
            (Some((row, col)), CursorShape::Block, Some(color))
                if row < grid.rows()
                    && grid
                        .visible_row(row)
                        .get(col)
                        .is_some_and(|cell| !cell.is_wide_continuation()) =>
            {
                Some((row, col, color))
            }
            _ => None,
        };

        for r in 0..grid.rows() {
            let row = grid.visible_row(r);
            for c in 0..grid.cols() {
                if c >= row.len() {
                    break;
                }
                let cell = &row[c];
                if cell.is_wide_continuation() {
                    continue;
                }
                let cursor_color = cursor_block_cell
                    .filter(|(cr, cc, _)| *cr == r && *cc == c)
                    .map(|(_, _, color)| color);
                self.draw_cell(&mut img, r, c, cell, opts, cursor_color);
            }
        }

        if let Some((cr, cc)) = opts.cursor
            && cursor_block_cell.is_none()
        {
            self.draw_cursor(&mut img, grid, cr, cc, opts);
        }

        img
    }

    fn draw_cell(
        &self,
        img: &mut RgbaImage,
        row: usize,
        col: usize,
        cell: &Cell,
        opts: &RasterOptions,
        cursor_block_color: Option<Rgb>,
    ) {
        let x = col as u32 * self.cell_w;
        let y = row as u32 * self.cell_h;
        let style = cell.style;
        let mut fg = resolve_color(style.fg, opts.fg_default);
        let mut bg = resolve_color(style.bg, opts.bg_default);
        if style.flags.contains(CellFlags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if style.flags.contains(CellFlags::DIM) {
            // 50% blend toward bg
            for i in 0..3 {
                fg[i] = ((fg[i] as u16 + bg[i] as u16) / 2) as u8;
            }
        }
        if style.flags.contains(CellFlags::HIDDEN) {
            fg = bg;
        }
        if let Some(cursor_bg) = cursor_block_color {
            fg = bg;
            bg = cursor_bg;
        }
        if style.flags.contains(CellFlags::HIDDEN) {
            fg = bg;
        }

        let cell_pixels_w = if cell.is_wide() {
            self.cell_w * 2
        } else {
            self.cell_w
        };
        fill_rect(img, x, y, cell_pixels_w, self.cell_h, bg);

        let ch = cell.ch;
        if ch != ' ' && ch != '\0' {
            let idx = self.font_idx_for(ch);
            let font = &self.fonts[idx];
            let (metrics, bitmap, glyph_x, glyph_y) = if idx == 0 {
                // Primary font: cell metrics were derived from it, so
                // baseline positioning aligns by construction.
                let (m, bmp) = font.rasterize(ch, self.font_size);
                let baseline_y = y as i32 + self.ascent.round() as i32;
                let gx = x as i32 + m.xmin;
                let gy = baseline_y - m.height as i32 - m.ymin;
                (m, bmp, gx, gy)
            } else {
                // Fallback font (e.g. Noto Emoji): its native advance
                // and ascent don't match the primary's, so naively
                // blitting at primary baseline would spill into the
                // next cell or float far above the row. Re-rasterize
                // at a font size that fits inside the cell box, then
                // center both axes within `cell_pixels_w * cell_h`.
                let (m, bmp) =
                    fit_and_rasterize(font, ch, self.font_size, cell_pixels_w, self.cell_h);
                let gx = x as i32 + (cell_pixels_w as i32 - m.width as i32).max(0) / 2;
                let gy = y as i32 + (self.cell_h as i32 - m.height as i32).max(0) / 2;
                (m, bmp, gx, gy)
            };
            blit_glyph(
                img,
                glyph_x,
                glyph_y,
                metrics.width as u32,
                metrics.height as u32,
                &bitmap,
                fg,
            );
            // Synthetic bold: render again 1px to the right and blend.
            if style.flags.contains(CellFlags::BOLD) {
                blit_glyph(
                    img,
                    glyph_x + 1,
                    glyph_y,
                    metrics.width as u32,
                    metrics.height as u32,
                    &bitmap,
                    fg,
                );
            }
        }

        let ext = cell.extended.as_ref();
        let underline_style = ext
            .map(|attrs| attrs.underline_style)
            .unwrap_or(UnderlineStyle::None);
        if style.flags.contains(CellFlags::UNDERLINE) || underline_style != UnderlineStyle::None {
            let effective_style = if underline_style == UnderlineStyle::None {
                UnderlineStyle::Single
            } else {
                underline_style
            };
            let underline_color = ext
                .and_then(|attrs| attrs.underline_color)
                .map(|color| resolve_color(color, opts.fg_default))
                .unwrap_or(fg);
            self.draw_underline(img, x, y, cell_pixels_w, effective_style, underline_color);
        }
        if style.flags.contains(CellFlags::STRIKETHROUGH) {
            let sy = y + (self.cell_h / 2);
            fill_rect(img, x, sy, cell_pixels_w, 1, fg);
        }
    }

    fn draw_underline(
        &self,
        img: &mut RgbaImage,
        x: u32,
        y: u32,
        width: u32,
        style: UnderlineStyle,
        color: Rgb,
    ) {
        let base = y + self.cell_h.saturating_sub(2);
        match style {
            UnderlineStyle::None => {}
            UnderlineStyle::Single => fill_rect(img, x, base, width, 1, color),
            UnderlineStyle::Double => {
                fill_rect(img, x, base.saturating_sub(2), width, 1, color);
                fill_rect(img, x, base, width, 1, color);
            }
            UnderlineStyle::Curly => {
                for dx in 0..width {
                    let yy = match dx % 4 {
                        0 | 2 => base,
                        1 => (base + 1).min(y + self.cell_h.saturating_sub(1)),
                        _ => base.saturating_sub(1),
                    };
                    set_pixel_rgb(img, x + dx, yy, color);
                }
            }
            UnderlineStyle::Dotted => {
                for dx in (0..width).step_by(2) {
                    set_pixel_rgb(img, x + dx, base, color);
                }
            }
            UnderlineStyle::Dashed => {
                for dx in 0..width {
                    if (dx / 3) % 2 == 0 {
                        set_pixel_rgb(img, x + dx, base, color);
                    }
                }
            }
        }
    }

    fn draw_cursor(
        &self,
        img: &mut RgbaImage,
        grid: &Grid,
        row: usize,
        col: usize,
        opts: &RasterOptions,
    ) {
        let x = col as u32 * self.cell_w;
        let y = row as u32 * self.cell_h;
        let max_y = (y + self.cell_h).min(img.height());
        let cursor_w = if row < grid.rows() {
            grid.visible_row(row)
                .get(col)
                .filter(|cell| cell.is_wide())
                .map_or(self.cell_w, |_| self.cell_w * 2)
        } else {
            self.cell_w
        };
        let max_x = (x + cursor_w).min(img.width());
        let cursor_color = opts.cursor_color.unwrap_or(opts.fg_default);
        match opts.cursor_shape {
            CursorShape::Block => {
                if let Some(color) = opts.cursor_color {
                    fill_rect(img, x, y, cursor_w, self.cell_h, color);
                    return;
                }
                for yy in y..max_y {
                    for xx in x..max_x {
                        let p = img.get_pixel_mut(xx, yy);
                        p[0] = 255 - p[0];
                        p[1] = 255 - p[1];
                        p[2] = 255 - p[2];
                    }
                }
            }
            CursorShape::Underline => {
                let h = (self.cell_h / 8).max(1);
                let yy = max_y.saturating_sub(h);
                fill_rect(img, x, yy, cursor_w, h, cursor_color);
            }
            CursorShape::Bar => {
                let w = (self.cell_w / 5).max(1);
                fill_rect(img, x, y, w, self.cell_h, cursor_color);
            }
        }
    }
}

/// Re-rasterize a glyph at a font size that fits inside a target box
/// (`box_w × box_h`). Used for fallback-font glyphs whose native
/// advance / height don't match the primary font's cell metric.
///
/// Strategy: probe at the primary font size first. If the result
/// already fits, use it. Otherwise scale the font size down by the
/// tighter of the two dimensions (`box_w / probe_w` vs `box_h / probe_h`)
/// and re-rasterize once. Never enlarges — a small emoji glyph stays
/// small, so the user gets the proportionally-correct visual weight
/// instead of a smeared upscale. Floors at 6pt so even an absurdly
/// small cell still gets something legible.
fn fit_and_rasterize(
    font: &Font,
    ch: char,
    primary_size: f32,
    box_w: u32,
    box_h: u32,
) -> (fontdue::Metrics, Vec<u8>) {
    let (probe_metrics, probe_bitmap) = font.rasterize(ch, primary_size);
    if probe_metrics.width <= box_w as usize && probe_metrics.height <= box_h as usize {
        return (probe_metrics, probe_bitmap);
    }
    let scale_w = box_w as f32 / probe_metrics.width.max(1) as f32;
    let scale_h = box_h as f32 / probe_metrics.height.max(1) as f32;
    let scale = scale_w.min(scale_h).min(1.0);
    let fit_size = (primary_size * scale).max(6.0);
    font.rasterize(ch, fit_size)
}

fn fill_rect(img: &mut RgbaImage, x: u32, y: u32, w: u32, h: u32, rgb: Rgb) {
    let x_end = (x + w).min(img.width());
    let y_end = (y + h).min(img.height());
    for yy in y..y_end {
        for xx in x..x_end {
            *img.get_pixel_mut(xx, yy) = Rgba([rgb[0], rgb[1], rgb[2], 255]);
        }
    }
}

fn set_pixel_rgb(img: &mut RgbaImage, x: u32, y: u32, rgb: Rgb) {
    if x < img.width() && y < img.height() {
        *img.get_pixel_mut(x, y) = Rgba([rgb[0], rgb[1], rgb[2], 255]);
    }
}

fn blit_glyph(img: &mut RgbaImage, x: i32, y: i32, w: u32, h: u32, alpha: &[u8], fg: Rgb) {
    for j in 0..h {
        for i in 0..w {
            let idx = (j * w + i) as usize;
            if idx >= alpha.len() {
                continue;
            }
            let a = alpha[idx] as u32;
            if a == 0 {
                continue;
            }
            let px = x + i as i32;
            let py = y + j as i32;
            if px < 0 || py < 0 {
                continue;
            }
            let (px, py) = (px as u32, py as u32);
            if px >= img.width() || py >= img.height() {
                continue;
            }
            let dst = img.get_pixel_mut(px, py);
            let inv = 255 - a;
            for k in 0..3 {
                dst[k] = ((fg[k] as u32 * a + dst[k] as u32 * inv) / 255) as u8;
            }
            dst[3] = 255;
        }
    }
}

fn resolve_color(c: Color, fallback: Rgb) -> Rgb {
    match c {
        Color::Default => fallback,
        Color::Rgb(r, g, b) => [r, g, b],
        Color::Indexed(i) => indexed_to_rgb(i),
    }
}

/// Standard xterm 256-color palette.
fn indexed_to_rgb(i: u8) -> Rgb {
    match i {
        0 => [0, 0, 0],
        1 => [205, 49, 49],
        2 => [13, 188, 121],
        3 => [229, 229, 16],
        4 => [36, 114, 200],
        5 => [188, 63, 188],
        6 => [17, 168, 205],
        7 => [229, 229, 229],
        8 => [102, 102, 102],
        9 => [241, 76, 76],
        10 => [35, 209, 139],
        11 => [245, 245, 67],
        12 => [59, 142, 234],
        13 => [214, 112, 214],
        14 => [41, 184, 219],
        15 => [255, 255, 255],
        16..=231 => {
            let v = i - 16;
            let r = v / 36;
            let g = (v % 36) / 6;
            let b = v % 6;
            let to = |n: u8| if n == 0 { 0u8 } else { n * 40 + 55 };
            [to(r), to(g), to(b)]
        }
        232..=255 => {
            let g = (i - 232) * 10 + 8;
            [g, g, g]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shux_vt::VirtualTerminal;

    #[test]
    fn rasterizer_constructs_with_reasonable_metrics() {
        let r = Rasterizer::new(14.0).unwrap();
        let (w, h) = r.cell_size();
        assert!((6..=20).contains(&w), "cell width out of range: {w}");
        assert!((12..=32).contains(&h), "cell height out of range: {h}");
    }

    #[test]
    fn renders_empty_grid_to_solid_bg() {
        let r = Rasterizer::new(14.0).unwrap();
        let vt = VirtualTerminal::new(4, 10);
        let img = r.render(vt.grid(), &RasterOptions::default());
        let (cw, ch) = r.cell_size();
        assert_eq!(img.width(), 10 * cw);
        assert_eq!(img.height(), 4 * ch);
        // Sample center pixel — should match bg_default.
        let p = img.get_pixel(img.width() / 2, img.height() / 2);
        assert_eq!([p[0], p[1], p[2]], [16, 16, 24]);
    }

    #[test]
    fn renders_text_writes_non_bg_pixels() {
        let r = Rasterizer::new(14.0).unwrap();
        let mut vt = VirtualTerminal::new(4, 20);
        vt.process(b"Hello!");
        let img = r.render(vt.grid(), &RasterOptions::default());
        let bg = [16u8, 16, 24];
        let mut found = false;
        for y in 0..img.height() {
            for x in 0..img.width() {
                let p = img.get_pixel(x, y);
                if [p[0], p[1], p[2]] != bg {
                    found = true;
                    break;
                }
            }
            if found {
                break;
            }
        }
        assert!(
            found,
            "no non-background pixels found — rasterizer drew nothing"
        );
    }

    #[test]
    fn indexed_palette_red_is_red() {
        let rgb = indexed_to_rgb(1);
        assert!(rgb[0] > rgb[1] + 50);
        assert!(rgb[0] > rgb[2] + 50);
    }

    #[test]
    fn cube_palette_pure_green() {
        // 16 + 0*36 + 5*6 + 0 = 46 → pure green corner of the 6x6x6 cube
        let rgb = indexed_to_rgb(46);
        assert_eq!(rgb[0], 0);
        assert!(rgb[1] > 200);
        assert_eq!(rgb[2], 0);
    }

    #[test]
    fn renders_cursor_shape_and_color() {
        let r = Rasterizer::new(14.0).unwrap();
        let vt = VirtualTerminal::new(2, 4);
        let (cw, ch) = r.cell_size();
        let opts = RasterOptions {
            fg_default: [255, 255, 255],
            bg_default: [0, 0, 0],
            cursor: Some((0, 0)),
            cursor_shape: CursorShape::Bar,
            cursor_color: Some([0, 255, 128]),
        };

        let img = r.render(vt.grid(), &opts);

        let bar = img.get_pixel(0, ch / 2);
        assert_eq!([bar[0], bar[1], bar[2]], [0, 255, 128]);
        let body = img.get_pixel(cw.saturating_sub(1), ch / 2);
        assert_eq!([body[0], body[1], body[2]], [0, 0, 0]);
    }

    #[test]
    fn colored_block_cursor_preserves_underlying_glyph() {
        let r = Rasterizer::new(14.0).unwrap();
        let mut vt = VirtualTerminal::new(1, 2);
        vt.process(b"A");
        let (cw, ch) = r.cell_size();
        let opts = RasterOptions {
            fg_default: [255, 255, 255],
            bg_default: [0, 0, 0],
            cursor: Some((0, 0)),
            cursor_shape: CursorShape::Block,
            cursor_color: Some([0, 255, 128]),
        };

        let img = r.render(vt.grid(), &opts);
        let mut cursor_bg_pixels = 0usize;
        let mut glyph_pixels = 0usize;
        for y in 0..ch {
            for x in 0..cw {
                let p = img.get_pixel(x, y);
                let rgb = [p[0], p[1], p[2]];
                if rgb == [0, 255, 128] {
                    cursor_bg_pixels += 1;
                } else {
                    glyph_pixels += 1;
                }
            }
        }

        assert!(cursor_bg_pixels > 0, "cursor background was not drawn");
        assert!(glyph_pixels > 0, "cursor block erased the underlying glyph");
    }

    #[test]
    fn colored_block_cursor_keeps_hidden_text_concealed() {
        let r = Rasterizer::new(14.0).unwrap();
        let mut vt = VirtualTerminal::new(1, 2);
        vt.process(b"\x1b[8mA\r");
        let (cw, ch) = r.cell_size();
        let cursor_color = [0, 255, 128];
        let opts = RasterOptions {
            fg_default: [255, 255, 255],
            bg_default: [0, 0, 0],
            cursor: Some((0, 0)),
            cursor_shape: CursorShape::Block,
            cursor_color: Some(cursor_color),
        };

        let img = r.render(vt.grid(), &opts);

        for y in 0..ch {
            for x in 0..cw {
                let p = img.get_pixel(x, y);
                assert_eq!(
                    [p[0], p[1], p[2]],
                    cursor_color,
                    "hidden glyph leaked at ({x},{y})"
                );
            }
        }
    }

    #[test]
    fn wide_lead_cell_cursor_spans_full_character_width() {
        let r = Rasterizer::new(14.0).unwrap();
        let mut vt = VirtualTerminal::new(1, 3);
        vt.process("界\r".as_bytes());
        let (cw, ch) = r.cell_size();
        let cursor_color = [0, 255, 128];
        let opts = RasterOptions {
            fg_default: [255, 255, 255],
            bg_default: [0, 0, 0],
            cursor: Some((0, 0)),
            cursor_shape: CursorShape::Underline,
            cursor_color: Some(cursor_color),
        };

        let img = r.render(vt.grid(), &opts);
        let underline_y = ch - 1;
        let left = img.get_pixel(cw / 2, underline_y);
        let right = img.get_pixel(cw + cw / 2, underline_y);
        assert_eq!([left[0], left[1], left[2]], cursor_color);
        assert_eq!([right[0], right[1], right[2]], cursor_color);
    }

    #[test]
    fn colored_block_cursor_on_wide_continuation_is_visible() {
        let r = Rasterizer::new(14.0).unwrap();
        let mut vt = VirtualTerminal::new(1, 3);
        vt.process("界\x1b[1;2H".as_bytes());
        let (cw, ch) = r.cell_size();
        let cursor_color = [0, 255, 128];
        let opts = RasterOptions {
            fg_default: [255, 255, 255],
            bg_default: [0, 0, 0],
            cursor: Some((0, 1)),
            cursor_shape: CursorShape::Block,
            cursor_color: Some(cursor_color),
        };

        let img = r.render(vt.grid(), &opts);
        let mut continuation_cursor_pixels = 0usize;
        for y in 0..ch {
            for x in cw..(cw * 2) {
                let p = img.get_pixel(x, y);
                if [p[0], p[1], p[2]] == cursor_color {
                    continuation_cursor_pixels += 1;
                }
            }
        }

        assert!(
            continuation_cursor_pixels > 0,
            "colored cursor disappeared on wide continuation cell"
        );
    }

    #[test]
    fn renders_advanced_underline_color_in_snapshots() {
        let r = Rasterizer::new(14.0).unwrap();
        let mut vt = VirtualTerminal::new(1, 4);
        vt.process(b"\x1b[4:4;58:2::255:0:0mA");
        let opts = RasterOptions {
            fg_default: [255, 255, 255],
            bg_default: [0, 0, 0],
            ..Default::default()
        };

        let img = r.render(vt.grid(), &opts);
        let red_pixels = img
            .pixels()
            .filter(|p| [p[0], p[1], p[2]] == [255, 0, 0])
            .count();

        assert!(red_pixels > 0, "underline color did not rasterize");
    }

    // ── font fallback ──────────────────────────────────────────────

    #[test]
    fn primary_font_renders_ascii() {
        let r = Rasterizer::new(14.0).expect("rasterizer");
        // 'A' is in JetBrains Mono → primary font wins.
        let f = r.font_for('A');
        assert!(f.lookup_glyph_index('A') != 0);
    }

    #[test]
    fn bundled_font_covers_ascii() {
        let r = Rasterizer::new(14.0).expect("rasterizer");
        for ch in 0x21u32..=0x7eu32 {
            let c = char::from_u32(ch).unwrap();
            assert!(
                r.has_glyph(c),
                "ASCII {c:?} ({ch:#x}) missing from bundled font"
            );
        }
    }

    /// Deterministic codepoint-coverage assertion — the bundled NF
    /// JetBrains Mono MUST resolve every codepoint shux's own status
    /// bar emits AND every commonly-seen NF codepoint from external
    /// status-segment scripts (starship language modules, kubectl
    /// helm, etc.). If a future asset update drops one of these
    /// glyphs we want a hard test failure, not silent tofu in PNGs.
    /// Alt fonts (loaded via `with_primary_font`) satisfy the same
    /// contract via the chain — the bundled NF JBM stays as fallback
    /// so any glyph the alt font lacks resolves through it. See
    /// `alt_nf_fonts_load_and_resolve_important_glyphs_when_staged`.
    #[test]
    fn bundled_font_covers_important_nf_and_unicode_glyphs() {
        let r = Rasterizer::new(14.0).expect("rasterizer");
        let missing: Vec<String> = important_glyphs_for_bundled_font()
            .iter()
            .filter(|(_, ch)| !r.has_glyph(*ch))
            .map(|(label, ch)| format!("  - {label} ({:#x})", *ch as u32))
            .collect();
        assert!(
            missing.is_empty(),
            "bundled NF JBM missing glyphs — tofu regression:\n{}",
            missing.join("\n")
        );
    }

    /// Stronger than `has_glyph`: confirm each important codepoint
    /// rasterizes to a non-empty bitmap. Catches the case where a
    /// font's `cmap` has an entry but the outline is blank (would
    /// render as visual tofu even though `glyph_id != 0`). 8 non-zero
    /// pixels is well below the smallest legitimate icon and well
    /// above the empty-outline baseline.
    #[test]
    fn bundled_font_renders_important_glyphs_as_non_empty_bitmaps() {
        let r = Rasterizer::new(14.0).expect("rasterizer");
        let mut empties: Vec<String> = Vec::new();
        for (label, ch) in important_glyphs_for_bundled_font() {
            let n = r.glyph_pixel_count(*ch);
            if n < 8 {
                empties.push(format!(
                    "  - {label} ({:#x}) rendered only {n} non-zero pixels",
                    *ch as u32
                ));
            }
        }
        assert!(
            empties.is_empty(),
            "bundled NF JBM rasterizes these glyphs as empty / near-empty bitmaps \
             (would visually tofu):\n{}",
            empties.join("\n")
        );
    }

    #[test]
    fn unknown_codepoint_returns_a_font_not_panic() {
        let r = Rasterizer::new(14.0).expect("rasterizer");
        // U+10000 (Linear B) is in neither bundled font — the fallback
        // returns the primary so callers get fontdue's notdef
        // rendering instead of a panic.
        let _f = r.font_for('\u{10000}');
        assert!(!r.has_glyph('\u{10000}'));
    }

    #[test]
    fn with_primary_font_keeps_bundled_fallback() {
        // Pass the same bundled NF JBM bytes as "primary" — every
        // glyph the chain ever needs is here. The chain length must
        // be 6 so a real "plain non-patched font" used as primary
        // still gets NF coverage from the JBM fallback, text-symbol
        // coverage from Noto, and emoji coverage from the Noto Emoji
        // fallback.
        let r = Rasterizer::with_primary_font(14.0, FONT_BYTES).expect("rasterizer");
        assert_eq!(
            r.font_count(),
            6,
            "user-primary + JBM-NF + math + symbols2 + symbols + emoji"
        );
        // Sanity: ASCII, NF private-use, and an emoji codepoint all resolve.
        assert!(r.has_glyph('A'));
        assert!(r.has_glyph('\u{e0a0}')); // git branch
        assert!(r.has_glyph('\u{e7a8}')); // rust logo
        assert!(r.has_glyph('\u{21bb}')); // ↻ rerun / refresh
        assert!(r.has_glyph('\u{2839}')); // ⠹ braille spinner
        assert!(r.has_glyph('\u{1F37A}')); // 🍺 beer mug — resolves via Noto Emoji
    }

    /// Default chain (no user-supplied primary): just NF JBM + emoji.
    /// Verifies `Rasterizer::new()` picks up the emoji fallback so
    /// snapshots produced by daemons that never see `appearance.font`
    /// still render emoji legibly.
    #[test]
    fn default_chain_has_emoji_fallback() {
        let r = Rasterizer::new(14.0).expect("rasterizer");
        assert_eq!(
            r.font_count(),
            5,
            "JBM-NF + math + symbols2 + symbols + emoji"
        );
        assert!(r.has_glyph('\u{1F37A}')); // 🍺
        assert!(r.has_glyph('\u{1F9E9}')); // 🧩
        assert!(r.has_glyph('\u{1F680}')); // 🚀
        assert_eq!(r.font_idx_for('A'), 0, "ASCII resolves at primary (JBM)");
        assert_eq!(
            r.font_idx_for('\u{1F37A}'),
            4,
            "emoji resolves at the emoji fallback"
        );
    }

    #[test]
    fn builtin_font_tokens_resolve() {
        for spec in DEFAULT_FALLBACK_FONT_SPECS {
            assert!(
                builtin_font_bytes(spec).is_some(),
                "builtin font token {spec:?} should resolve"
            );
        }
        assert!(builtin_font_bytes("builtin:nope").is_none());
    }

    /// Tofu-free assertion for common text-presentation glyphs used
    /// by TUIs. These are not Nerd Font private-use icons and not
    /// emoji; real terminals usually resolve them through their host
    /// font fallback stack, so shux's headless PNG renderer must carry
    /// equivalent fallback coverage.
    #[test]
    fn bundled_text_symbol_fallback_covers_important_tui_glyphs() {
        let r = Rasterizer::new(14.0).expect("rasterizer");
        let mut problems: Vec<String> = Vec::new();
        for (label, ch) in important_tui_text_glyphs() {
            let has = r.has_glyph(*ch);
            let n = r.glyph_pixel_count(*ch);
            if !has || n < 8 {
                problems.push(format!(
                    "  - {label} ({:#x}) has_glyph={has} pixels={n}",
                    *ch as u32
                ));
            }
        }
        assert!(
            problems.is_empty(),
            "bundled text-symbol fallbacks would tofu these codepoints:\n{}",
            problems.join("\n")
        );
    }

    /// Tofu-free assertion for the curated emoji set. Mirrors the NF
    /// glyph contract but lives in a SEPARATE list — if a future emoji
    /// font swap drops one of these the failure is targeted at the
    /// emoji asset, not the text font. Adding entries is fine;
    /// removing one means "shux silently accepts tofu for this emoji
    /// codepoint" — review carefully.
    #[test]
    fn bundled_emoji_font_covers_important_emoji_glyphs() {
        let r = Rasterizer::new(14.0).expect("rasterizer");
        let mut problems: Vec<String> = Vec::new();
        for (label, ch) in important_emoji_glyphs() {
            let has = r.has_glyph(*ch);
            let n = r.glyph_pixel_count(*ch);
            if !has || n < 8 {
                problems.push(format!(
                    "  - {label} ({:#x}) has_glyph={has} pixels={n}",
                    *ch as u32
                ));
            }
        }
        assert!(
            problems.is_empty(),
            "bundled emoji fallback would tofu these codepoints:\n{}",
            problems.join("\n")
        );
    }

    /// Fallback-font glyphs are size-fitted and centered within the
    /// cell box. Drive the VT parser with a wide emoji and assert the
    /// rendered glyph straddles both halves of the 2-column wide cell —
    /// the council-flagged "uncentered, spilling into next cell"
    /// failure mode would put all pixels in one half (or worse, past
    /// the right edge into a phantom 3rd cell).
    #[test]
    fn fallback_emoji_glyph_stays_inside_wide_cell_bounds() {
        let r = Rasterizer::new(14.0).expect("rasterizer");
        let (cw, _ch) = r.cell_size();

        // 1×3 grid: emoji at col 0..1 (wide), space at col 2. The
        // space column gives us an empty bg-only region to confirm
        // the emoji isn't spilling past the wide-cell box.
        let mut vt = VirtualTerminal::new(1, 3);
        vt.process("🍺 ".as_bytes()); // emoji + trailing space

        let opts = RasterOptions {
            fg_default: [255, 255, 255],
            bg_default: [0, 0, 0],
            cursor: None,
            ..Default::default()
        };
        let img = r.render(vt.grid(), &opts);
        assert_eq!(img.width(), 3 * cw);

        let mut left_pixels = 0u32;
        let mut right_pixels = 0u32;
        let mut spillover_pixels = 0u32;
        for y in 0..img.height() {
            for x in 0..img.width() {
                let p = img.get_pixel(x, y);
                if (p[0], p[1], p[2]) == (0, 0, 0) {
                    continue;
                }
                if x < cw {
                    left_pixels += 1;
                } else if x < 2 * cw {
                    right_pixels += 1;
                } else {
                    spillover_pixels += 1;
                }
            }
        }
        assert!(
            left_pixels > 0 && right_pixels > 0,
            "wide emoji should straddle both columns of its 2-cell box: \
             left={left_pixels} right={right_pixels}"
        );
        assert_eq!(
            spillover_pixels, 0,
            "wide emoji glyph spilled past the 2-cell box into col 3 \
             ({spillover_pixels} px) — centering / clipping regression"
        );
    }

    /// Local-only test: when alternative NF fonts staged under
    /// `.local/fonts/` are present, verify each loads via
    /// `with_primary_font`, the rasterizer ends up with a 2-font
    /// chain, and the SAME important-glyph set resolves AND
    /// rasterizes to a non-empty bitmap (not the silent
    /// has-cmap-entry-but-blank-outline tofu mode).
    ///
    /// CI doesn't stage these fonts (they're 2.6 MB binaries not in
    /// git), so the test must remain CI-safe. To make the skip
    /// visible rather than silent, the test PRINTS the exercise count
    /// to stdout (nextest captures stdout) and the absent-paths list
    /// is included in the panic message if any pass fails.
    /// To force a hard failure when nothing exercised, set
    /// `SHUX_RASTER_REQUIRE_ALT_FONTS=1` — dev workflow can opt in.
    #[test]
    fn alt_nf_fonts_load_and_resolve_important_glyphs_when_staged() {
        let mut exercised: Vec<std::path::PathBuf> = Vec::new();
        let mut absent: Vec<std::path::PathBuf> = Vec::new();
        for alt in alt_font_paths() {
            let Ok(bytes) = std::fs::read(&alt) else {
                absent.push(alt);
                continue;
            };
            let r = Rasterizer::with_primary_font(14.0, &bytes)
                .unwrap_or_else(|e| panic!("alt font {} failed to load: {e}", alt.display()));
            assert_eq!(
                r.font_count(),
                6,
                "alt-primary + JBM-NF + math + symbols2 + symbols + emoji"
            );
            let mut tofus: Vec<String> = Vec::new();
            for (label, ch) in important_glyphs_for_bundled_font() {
                let c = *ch;
                let has = r.has_glyph(c);
                let pixels = r.glyph_pixel_count(c);
                if !has || pixels < 8 {
                    tofus.push(format!(
                        "  - {label} ({:#x}) has_glyph={has} pixels={pixels}",
                        c as u32
                    ));
                }
            }
            assert!(
                tofus.is_empty(),
                "alt font chain ({}) would tofu these glyphs:\n{}",
                alt.display(),
                tofus.join("\n")
            );
            exercised.push(alt);
        }
        println!(
            "alt_nf_fonts: exercised {} font(s); absent: {}",
            exercised.len(),
            if absent.is_empty() {
                "none".to_string()
            } else {
                absent
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        );
        if std::env::var_os("SHUX_RASTER_REQUIRE_ALT_FONTS").is_some() {
            assert!(
                !exercised.is_empty(),
                "SHUX_RASTER_REQUIRE_ALT_FONTS=1 but no alt fonts staged under .local/fonts/"
            );
        }
    }

    /// The full contract: every codepoint listed here MUST resolve via
    /// the bundled NF JBM. Keep sorted by category. Adding entries is
    /// fine; removing one means "shux silently accepts tofu for this
    /// codepoint" — review carefully.
    ///
    /// Deliberately excluded (don't add unless you also add a font
    /// that has them):
    ///   - Color emoji (e.g. 🦀 U+1F980) — requires a color emoji
    ///     font and a glyph-rendering pipeline that handles SVG/CBDT.
    ///     `shux config init`'s starship template sets `[rust]
    ///     symbol = ""` so the NF rust logo is used instead.
    fn important_glyphs_for_bundled_font() -> &'static [(&'static str, char)] {
        &[
            // ── shux's own statusbar chrome (statusbar_build.rs) ──
            ("nf-cod-terminal U+F489", '\u{f489}'),
            ("nf-pl-branch U+E0A0", '\u{e0a0}'),
            ("nf-fa-home U+F015", '\u{f015}'),
            // ── ssh-host indicator (often used in cwd / session left zone) ──
            ("nf-fa-server U+F233", '\u{f233}'),
            // ── starship language module defaults (NF codepoints) ──
            ("nf-dev-rust U+E7A8", '\u{e7a8}'),
            ("nf-dev-nodejs U+E718", '\u{e718}'),
            ("nf-dev-python U+E73C", '\u{e73c}'),
            ("nf-dev-go U+E626", '\u{e626}'),
            ("nf-dev-ruby U+E739", '\u{e739}'),
            // ── kubectl / cluster ops — NF kubernetes (not BMP helm) ──
            ("nf-md-kubernetes U+F10FE", '\u{f10fe}'),
            ("nf-md-ship_wheel U+F124A", '\u{f124a}'),
            ("nf-md-docker U+F308", '\u{f308}'),
            // ── shux unicode fallback set (when nerd_fonts=false) ──
            ("diamond U+25C6", '\u{25c6}'),
            ("right-triangle U+25B6", '\u{25b6}'),
            ("plus-minus U+00B1", '\u{00b1}'),
            ("middle-dot U+00B7", '\u{00b7}'),
        ]
    }

    /// Curated emoji codepoint set the bundled Noto Emoji fallback
    /// MUST cover. Kept separate from `important_glyphs_for_bundled_font`
    /// because the contract is different: emoji codepoints land in
    /// standard Unicode blocks (Misc Symbols & Pictographs, Supplemental
    /// Symbols & Pictographs, …), not Nerd Fonts' private-use area.
    ///
    /// Scope: only **standalone** scalar codepoints. Variation selectors
    /// (VS16 / U+FE0F) and ZWJ sequences are deliberately excluded —
    /// `shux-vt` stores one `char` per cell so the parser splits them
    /// before the rasterizer sees them, and even a colour-emoji
    /// rasterizer wouldn't be able to reconstruct the cluster. That's a
    /// VT-layer change tracked as future work.
    fn important_emoji_glyphs() -> &'static [(&'static str, char)] {
        &[
            ("beer_mug U+1F37A", '\u{1F37A}'),      // 🍺
            ("jigsaw U+1F9E9", '\u{1F9E9}'),        // 🧩
            ("hammer_wrench U+1F6E0", '\u{1F6E0}'), // 🛠 (no VS16)
            ("rocket U+1F680", '\u{1F680}'),        // 🚀
            ("crab U+1F980", '\u{1F980}'),          // 🦀
            ("package U+1F4E6", '\u{1F4E6}'),       // 📦
            ("party_popper U+1F389", '\u{1F389}'),  // 🎉
            ("lock U+1F512", '\u{1F512}'),          // 🔒
            ("fire U+1F525", '\u{1F525}'),          // 🔥
            ("magnifier U+1F50D", '\u{1F50D}'),     // 🔍
            ("thumbsup U+1F44D", '\u{1F44D}'),      // 👍
            ("high_voltage U+26A1", '\u{26A1}'),    // ⚡
            ("warning_sign U+26A0", '\u{26A0}'),    // ⚠
            ("heart U+2764", '\u{2764}'),           // ❤
            ("star_medium U+2B50", '\u{2B50}'),     // ⭐
        ]
    }

    fn important_tui_text_glyphs() -> &'static [(&'static str, char)] {
        &[
            // ── arrows / key legends ──
            ("rerun_clockwise_open_circle_arrow U+21BB", '\u{21bb}'),
            ("rerun_anticlockwise_open_circle_arrow U+21BA", '\u{21ba}'),
            ("clockwise_gapped_circle_arrow U+27F3", '\u{27f3}'),
            ("anticlockwise_gapped_circle_arrow U+27F2", '\u{27f2}'),
            ("left_arrow U+2190", '\u{2190}'),
            ("right_arrow U+2192", '\u{2192}'),
            ("up_arrow U+2191", '\u{2191}'),
            ("down_arrow U+2193", '\u{2193}'),
            ("downwards_arrow_with_corner_leftwards U+21B5", '\u{21b5}'),
            ("return_symbol U+23CE", '\u{23ce}'),
            ("place_of_interest U+2318", '\u{2318}'),
            ("option_key U+2325", '\u{2325}'),
            ("up_arrowhead U+2303", '\u{2303}'),
            ("alternative_key_symbol U+2387", '\u{2387}'),
            ("helm_symbol U+2388", '\u{2388}'),
            ("erase_to_the_left U+232B", '\u{232b}'),
            ("erase_to_the_right U+2326", '\u{2326}'),
            ("rightwards_arrow_to_bar U+21E5", '\u{21e5}'),
            ("leftwards_arrow_to_bar U+21E4", '\u{21e4}'),
            ("upwards_white_arrow U+21E7", '\u{21e7}'),
            ("escape_symbol U+238B", '\u{238b}'),
            // ── common braille spinner frames ──
            ("braille_spinner U+280B", '\u{280b}'),
            ("braille_spinner U+2819", '\u{2819}'),
            ("braille_spinner U+2839", '\u{2839}'),
            ("braille_spinner U+2838", '\u{2838}'),
            ("braille_spinner U+283C", '\u{283c}'),
            ("braille_spinner U+2834", '\u{2834}'),
            ("braille_spinner U+2826", '\u{2826}'),
            ("braille_spinner U+2827", '\u{2827}'),
            ("braille_spinner U+2807", '\u{2807}'),
            ("braille_spinner U+280F", '\u{280f}'),
            // ── status / diagnostics ──
            ("check_mark U+2713", '\u{2713}'),
            ("success_heavy_check U+2714", '\u{2714}'),
            ("failure_ballot_x U+2717", '\u{2717}'),
            ("heavy_ballot_x U+2718", '\u{2718}'),
            ("heavy_multiplication_x U+2716", '\u{2716}'),
            ("multiplication_sign U+00D7", '\u{00d7}'),
            ("bullet U+2022", '\u{2022}'),
            ("horizontal_ellipsis U+2026", '\u{2026}'),
            ("dotted_circle U+25CC", '\u{25cc}'),
            ("circled_division_slash U+2298", '\u{2298}'),
            ("circled_dash U+229D", '\u{229d}'),
            ("warning_sign U+26A0", '\u{26a0}'),
            ("high_voltage U+26A1", '\u{26a1}'),
            ("hourglass_flowing_sand U+23F3", '\u{23f3}'),
            ("alarm_clock U+23F0", '\u{23f0}'),
            ("timer_clock U+23F2", '\u{23f2}'),
            ("stopwatch U+23F1", '\u{23f1}'),
            ("timer_clock U+29D7", '\u{29d7}'),
            ("hourglass U+231B", '\u{231b}'),
            ("watch U+231A", '\u{231a}'),
            ("black_star U+2605", '\u{2605}'),
            ("white_star U+2606", '\u{2606}'),
            ("black_four_pointed_star U+2726", '\u{2726}'),
            ("white_four_pointed_star U+2727", '\u{2727}'),
            ("circled_white_star U+272A", '\u{272a}'),
            ("outlined_black_star U+272D", '\u{272d}'),
            ("heavy_asterisk U+2731", '\u{2731}'),
            ("open_center_asterisk U+2732", '\u{2732}'),
            ("eight_spoked_asterisk U+2733", '\u{2733}'),
            ("eight_pointed_black_star U+2734", '\u{2734}'),
            ("twelve_pointed_black_star U+2739", '\u{2739}'),
            ("ballot_box_with_check U+2611", '\u{2611}'),
            ("ballot_box_with_x U+2612", '\u{2612}'),
            ("ballot_box U+2610", '\u{2610}'),
            ("trigram_for_heaven U+2630", '\u{2630}'),
            ("trigram_for_lake U+2631", '\u{2631}'),
            ("trigram_for_fire U+2632", '\u{2632}'),
            ("trigram_for_thunder U+2633", '\u{2633}'),
            ("trigram_for_wind U+2634", '\u{2634}'),
            ("trigram_for_water U+2635", '\u{2635}'),
            ("trigram_for_mountain U+2636", '\u{2636}'),
            ("trigram_for_earth U+2637", '\u{2637}'),
            // ── block / progress glyphs ──
            ("full_block U+2588", '\u{2588}'),
            ("left_seven_eighths_block U+2589", '\u{2589}'),
            ("left_three_quarters_block U+258A", '\u{258a}'),
            ("left_five_eighths_block U+258B", '\u{258b}'),
            ("left_half_block U+258C", '\u{258c}'),
            ("left_three_eighths_block U+258D", '\u{258d}'),
            ("left_one_quarter_block U+258E", '\u{258e}'),
            ("left_one_eighth_block U+258F", '\u{258f}'),
            ("lower_one_eighth_block U+2581", '\u{2581}'),
            ("lower_one_quarter_block U+2582", '\u{2582}'),
            ("lower_three_eighths_block U+2583", '\u{2583}'),
            ("lower_half_block U+2584", '\u{2584}'),
            ("lower_five_eighths_block U+2585", '\u{2585}'),
            ("lower_three_quarters_block U+2586", '\u{2586}'),
            ("lower_seven_eighths_block U+2587", '\u{2587}'),
            ("light_shade U+2591", '\u{2591}'),
            ("medium_shade U+2592", '\u{2592}'),
            ("dark_shade U+2593", '\u{2593}'),
            // ── box drawing / borders ──
            ("box_light_horizontal U+2500", '\u{2500}'),
            ("box_light_vertical U+2502", '\u{2502}'),
            ("box_light_down_right U+250C", '\u{250c}'),
            ("box_light_down_left U+2510", '\u{2510}'),
            ("box_light_up_right U+2514", '\u{2514}'),
            ("box_light_up_left U+2518", '\u{2518}'),
            ("box_light_vertical_right U+251C", '\u{251c}'),
            ("box_light_vertical_left U+2524", '\u{2524}'),
            ("box_light_down_horizontal U+252C", '\u{252c}'),
            ("box_light_up_horizontal U+2534", '\u{2534}'),
            ("box_light_cross U+253C", '\u{253c}'),
            ("box_rounded_down_right U+256D", '\u{256d}'),
            ("box_rounded_down_left U+256E", '\u{256e}'),
            ("box_rounded_up_right U+2570", '\u{2570}'),
            ("box_rounded_up_left U+256F", '\u{256f}'),
            ("box_double_horizontal U+2550", '\u{2550}'),
            ("box_double_vertical U+2551", '\u{2551}'),
            ("box_double_down_right U+2554", '\u{2554}'),
            ("box_double_down_left U+2557", '\u{2557}'),
            ("box_double_up_right U+255A", '\u{255a}'),
            ("box_double_up_left U+255D", '\u{255d}'),
            ("box_double_vertical_right U+2560", '\u{2560}'),
            ("box_double_vertical_left U+2563", '\u{2563}'),
            ("box_double_down_horizontal U+2566", '\u{2566}'),
            ("box_double_up_horizontal U+2569", '\u{2569}'),
            ("box_double_cross U+256C", '\u{256c}'),
            // ── geometric UI markers ──
            ("black_right_small_triangle U+25B8", '\u{25b8}'),
            ("black_down_small_triangle U+25BE", '\u{25be}'),
            ("black_left_small_triangle U+25C2", '\u{25c2}'),
            ("white_left_small_triangle U+25C3", '\u{25c3}'),
            ("right-triangle U+25B6", '\u{25b6}'),
            ("black_down_triangle U+25BC", '\u{25bc}'),
            ("black_up_triangle U+25B2", '\u{25b2}'),
            ("diamond U+25C6", '\u{25c6}'),
            ("white_diamond U+25C7", '\u{25c7}'),
            ("white_circle U+25CB", '\u{25cb}'),
            ("black_circle U+25CF", '\u{25cf}'),
            ("circle_left_half_black U+25D0", '\u{25d0}'),
            ("circle_right_half_black U+25D1", '\u{25d1}'),
            ("circle_lower_half_black U+25D2", '\u{25d2}'),
            ("circle_upper_half_black U+25D3", '\u{25d3}'),
            ("circle_upper_right_quadrant_black U+25D4", '\u{25d4}'),
            (
                "circle_all_but_upper_left_quadrant_black U+25D5",
                '\u{25d5}',
            ),
            ("left_half_black_circle U+25D6", '\u{25d6}'),
            ("right_half_black_circle U+25D7", '\u{25d7}'),
            ("upper_left_quadrant_circular_arc U+25DC", '\u{25dc}'),
            ("upper_right_quadrant_circular_arc U+25DD", '\u{25dd}'),
            ("lower_right_quadrant_circular_arc U+25DE", '\u{25de}'),
            ("lower_left_quadrant_circular_arc U+25DF", '\u{25df}'),
            ("white_bullet U+25E6", '\u{25e6}'),
            ("black_small_square U+25AA", '\u{25aa}'),
            ("white_small_square U+25AB", '\u{25ab}'),
            ("white_square U+25A1", '\u{25a1}'),
            ("black_square U+25A0", '\u{25a0}'),
            (
                "white_square_containing_black_small_square U+25A3",
                '\u{25a3}',
            ),
            ("square_horizontal_fill U+25A4", '\u{25a4}'),
            ("square_vertical_fill U+25A5", '\u{25a5}'),
            ("square_orthogonal_crosshatch_fill U+25A6", '\u{25a6}'),
            ("square_upper_left_to_lower_right_fill U+25A7", '\u{25a7}'),
            ("square_upper_right_to_lower_left_fill U+25A8", '\u{25a8}'),
            ("square_diagonal_crosshatch_fill U+25A9", '\u{25a9}'),
            ("square_left_half_black U+25E7", '\u{25e7}'),
            ("square_right_half_black U+25E8", '\u{25e8}'),
            ("square_upper_left_half_black U+25E9", '\u{25e9}'),
            ("square_lower_right_half_black U+25EA", '\u{25ea}'),
            ("downwards_arrow_with_tip_rightwards U+21B3", '\u{21b3}'),
            ("upwards_arrow_with_tip_leftwards U+21B0", '\u{21b0}'),
            ("upwards_arrow_with_tip_rightwards U+21B1", '\u{21b1}'),
            ("downwards_arrow_with_tip_leftwards U+21B2", '\u{21b2}'),
            ("rightwards_arrow_from_bar U+21A6", '\u{21a6}'),
            ("leftwards_arrow_from_bar U+21A4", '\u{21a4}'),
            ("rightwards_dashed_arrow U+21E2", '\u{21e2}'),
            ("leftwards_dashed_arrow U+21E0", '\u{21e0}'),
            ("upwards_dashed_arrow U+21E1", '\u{21e1}'),
            ("downwards_dashed_arrow U+21E3", '\u{21e3}'),
            ("rightwards_arrow_over_leftwards_arrow U+21C4", '\u{21c4}'),
            (
                "upwards_arrow_leftwards_of_downwards_arrow U+21C5",
                '\u{21c5}',
            ),
            ("left_right_arrow U+2194", '\u{2194}'),
            ("up_down_arrow U+2195", '\u{2195}'),
            ("rightwards_double_arrow U+21D2", '\u{21d2}'),
            ("leftwards_double_arrow U+21D0", '\u{21d0}'),
        ]
    }

    fn alt_font_paths() -> Vec<std::path::PathBuf> {
        // `.local/fonts/<…>.ttf` is gitignored and only present on
        // dev machines that ran the override-test fetch step.
        // Walk up from the crate root to find the workspace root.
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir
            .parent()
            .and_then(|p| p.parent())
            .unwrap_or(manifest_dir);
        let fonts_dir = workspace_root.join(".local").join("fonts");
        [
            "FiraCodeNerdFontMono-Regular.ttf",
            "HackNerdFontMono-Regular.ttf",
        ]
        .iter()
        .map(|name| fonts_dir.join(name))
        .collect()
    }
}
