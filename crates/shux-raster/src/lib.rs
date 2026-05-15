//! shux-raster — turn a `shux-vt` Grid<Cell> into a PNG without any terminal emulator.
//!
//! - One bundled monospace font: **JetBrains Mono Nerd Font Mono Regular**
//!   (Nerd Fonts patched, OFL). The full NF-patched build replaces the
//!   prior 270 KB JBM Regular + 5 KB curated symbols subset combo. Every
//!   Nerd Font codepoint (rust, node, python, helm, branch, …) resolves
//!   out of the box — no subset-regen ritual, no tofu when a user's
//!   `[[statusbar.segment]]` script (starship, kubectl, …) emits NF
//!   glyphs we didn't anticipate.
//! - Per-cell glyph rendering via `fontdue` (pure Rust, no system deps).
//! - 16-color ANSI + 256-indexed + truecolor RGB palette.
//! - Bold (synthetic offset), dim, underline, strikethrough, inverse.
//! - Block cursor (inverse cell).
//! - PNG output via the `image` crate.
//!
//! Out of scope: color emoji (e.g. `🦀` U+1F980 from starship's default
//! rust prompt). Set `[rust] symbol = ""` in starship config to use the
//! NF rust logo instead — see `shux config init`'s emitted template.
//! Other deferred: ligatures via shaping, italics with a real italic
//! face, OSC 8 hyperlink styling, RTL text, GPU acceleration.

use fontdue::{Font, FontSettings};
use image::{ImageBuffer, Rgba, RgbaImage};
use shux_vt::{Cell, CellFlags, Color, Grid};

/// Embedded text font. JetBrains Mono Nerd Font Mono Regular, the
/// upstream Nerd Fonts patched build (2.4 MB) under SIL Open Font
/// License (see `assets/OFL.txt`). Bundles the full NF glyph set so
/// every codepoint a user's script-driven status segment can emit
/// renders correctly OOTB. To update: pull the latest from
/// <https://github.com/ryanoasis/nerd-fonts/releases/latest/>.
const FONT_BYTES: &[u8] = include_bytes!("../assets/JetBrainsMonoNerdFontMono-Regular.ttf");

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

/// Owning rasterizer. Holds an ordered font fallback chain plus
/// derived cell metrics. The first font that has a glyph for the
/// requested character wins. With the full NF-patched bundled font
/// the chain typically has exactly one entry; user-supplied
/// `appearance.font` puts the user's font first and the bundled NF
/// font as a fallback so any glyph the user's font lacks (e.g. NF
/// icons in a plain non-patched typeface) still resolves.
pub struct Rasterizer {
    /// Fallback chain: try fonts[0] first, then [1], etc. Metrics
    /// (cell_w / cell_h / ascent) are derived from fonts[0] only —
    /// the primary text font dominates the grid geometry.
    fonts: Vec<Font>,
    font_size: f32,
    cell_w: u32,
    cell_h: u32,
    ascent: f32,
}

impl Rasterizer {
    /// Construct a rasterizer at the given font size (in pixels)
    /// using the bundled NF-patched JetBrains Mono. Single-font chain
    /// — full NF coverage included.
    pub fn new(font_size: f32) -> Result<Self, RasterError> {
        Self::with_fonts(font_size, [FONT_BYTES])
    }

    /// Construct a rasterizer with a user-supplied primary font, plus
    /// the bundled NF-patched JBM as a fallback for codepoints the
    /// user's font lacks. Lets users override the typeface via
    /// `appearance.font` in shux config while still getting Nerd-Font
    /// icons OOTB even if their chosen font is plain (non-patched).
    pub fn with_primary_font(font_size: f32, primary: &[u8]) -> Result<Self, RasterError> {
        Self::with_fonts(font_size, [primary, FONT_BYTES])
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
        for f in &self.fonts {
            if f.lookup_glyph_index(ch) != 0 {
                return f;
            }
        }
        &self.fonts[0]
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
            let (metrics, bitmap) = self.font_for(ch).rasterize(ch, self.font_size);
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
        // Use a font that is monochrome-emoji-only (synthetic test:
        // pass the same bundled NF JBM bytes as "primary" — every
        // glyph the chain ever needs is here). The fallback chain
        // length must be 2 so a real "plain non-patched font" used
        // as primary still gets NF coverage from the bundled fallback.
        let r = Rasterizer::with_primary_font(14.0, FONT_BYTES).expect("rasterizer");
        assert_eq!(r.font_count(), 2, "user-primary + bundled-fallback");
        // Sanity: both ASCII and NF resolve.
        assert!(r.has_glyph('A'));
        assert!(r.has_glyph('\u{e0a0}')); // git branch
        assert!(r.has_glyph('\u{e7a8}')); // rust logo
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
            assert_eq!(r.font_count(), 2);
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
    ///   - `⎈` U+2388 (kubectl helm), `⎇` U+2387 (alt-branch) — these
    ///     are obscure Miscellaneous-Technical-block BMP glyphs that
    ///     JetBrains Mono does NOT include, and Nerd Fonts patching
    ///     adds private-use-area glyphs only. The upstream
    ///     `SymbolsNerdFontMono-Regular.ttf` also lacks them. The
    ///     remedy is to use NF equivalents (`nf-md-kubernetes` U+F10FE
    ///     for the helm wheel, `nf-pl-branch` U+E0A0 for git-style
    ///     branch). `shux config init` emits a kubectl segment that
    ///     uses U+F10FE for exactly this reason.
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
