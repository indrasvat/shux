//! shux-raster — turn a `shux-vt` Grid<Cell> into a PNG without any terminal emulator.
//!
//! This is the P1 (spike) rasterizer:
//! - One bundled monospace font (JetBrains Mono Regular, OFL).
//! - Per-cell glyph rendering via `fontdue` (pure Rust, no system deps).
//! - 16-color ANSI + 256-indexed + truecolor RGB palette.
//! - Bold (synthetic offset), dim, underline, strikethrough, inverse.
//! - Block cursor (inverse cell).
//! - PNG output via the `image` crate.
//!
//! Out of scope for the spike (deferred to P2/P3): emoji color glyphs,
//! ligatures via shaping, italics with a real italic face, OSC 8 hyperlink
//! styling, RTL text, GPU acceleration.

use fontdue::{Font, FontSettings};
use image::{ImageBuffer, Rgba, RgbaImage};
use shux_vt::{Cell, CellFlags, Color, Grid};

/// Embedded font asset. JetBrains Mono Regular is shipped under the SIL Open
/// Font License (see `assets/OFL.txt`).
const FONT_BYTES: &[u8] = include_bytes!("../assets/JetBrainsMono-Regular.ttf");

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
}

impl Default for RasterOptions {
    fn default() -> Self {
        Self {
            fg_default: [220, 220, 220],
            bg_default: [16, 16, 24],
            cursor: None,
        }
    }
}

/// Owning rasterizer. Holds the parsed font + derived cell metrics.
pub struct Rasterizer {
    font: Font,
    font_size: f32,
    cell_w: u32,
    cell_h: u32,
    ascent: f32,
}

impl Rasterizer {
    /// Construct a rasterizer at the given font size (in pixels).
    pub fn new(font_size: f32) -> Result<Self, RasterError> {
        let settings = FontSettings {
            scale: font_size,
            ..FontSettings::default()
        };
        let font =
            Font::from_bytes(FONT_BYTES, settings).map_err(|e| RasterError::Font(e.into()))?;
        let line = font
            .horizontal_line_metrics(font_size)
            .ok_or(RasterError::NoMetrics(font_size))?;
        let m = font.metrics('M', font_size);
        let cell_w = m.advance_width.ceil().max(1.0) as u32;
        let cell_h = line.new_line_size.ceil().max(1.0) as u32;
        Ok(Self {
            font,
            font_size,
            cell_w,
            cell_h,
            ascent: line.ascent,
        })
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
                self.draw_cell(&mut img, r, c, cell, opts);
            }
        }

        if let Some((cr, cc)) = opts.cursor {
            self.draw_cursor(&mut img, cr, cc);
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

        let cell_pixels_w = if cell.is_wide() {
            self.cell_w * 2
        } else {
            self.cell_w
        };
        fill_rect(img, x, y, cell_pixels_w, self.cell_h, bg);

        let ch = cell.ch;
        if ch != ' ' && ch != '\0' {
            let (metrics, bitmap) = self.font.rasterize(ch, self.font_size);
            let baseline_y = y as i32 + self.ascent.round() as i32;
            let glyph_x = x as i32 + metrics.xmin;
            let glyph_y = baseline_y - metrics.height as i32 - metrics.ymin;
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

        if style.flags.contains(CellFlags::UNDERLINE) {
            let uy = y + self.cell_h.saturating_sub(2);
            fill_rect(img, x, uy, cell_pixels_w, 1, fg);
        }
        if style.flags.contains(CellFlags::STRIKETHROUGH) {
            let sy = y + (self.cell_h / 2);
            fill_rect(img, x, sy, cell_pixels_w, 1, fg);
        }
    }

    fn draw_cursor(&self, img: &mut RgbaImage, row: usize, col: usize) {
        let x = col as u32 * self.cell_w;
        let y = row as u32 * self.cell_h;
        let max_y = (y + self.cell_h).min(img.height());
        let max_x = (x + self.cell_w).min(img.width());
        for yy in y..max_y {
            for xx in x..max_x {
                let p = img.get_pixel_mut(xx, yy);
                p[0] = 255 - p[0];
                p[1] = 255 - p[1];
                p[2] = 255 - p[2];
            }
        }
    }
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
}
