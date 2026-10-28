//! Every program-status badge glyph shux draws (`shux::pane_chrome`) is in
//! the bundled primary font itself, so a snapshot never falls back to another
//! font's metrics for it.

use shux_raster::{BUILTIN_NERD_FONT, Rasterizer, builtin_font_bytes};

const BADGES: &[(&str, char)] = &[
    ("blocked", '◆'),
    ("error", '✕'),
    ("done", '✓'),
    ("working", '◔'),
    ("idle", '○'),
    ("blocked nf", '\u{f071}'),
    ("error nf", '\u{f00d}'),
    ("done nf", '\u{f00c}'),
    ("working nf", '\u{f110}'),
];

#[test]
fn every_badge_glyph_is_in_the_bundled_primary_font() {
    let primary = builtin_font_bytes(BUILTIN_NERD_FONT).expect("bundled font");
    let only_primary = Rasterizer::with_fonts(14.0, [primary]).expect("rasterizer");
    for (name, ch) in BADGES {
        assert!(
            only_primary.has_glyph(*ch),
            "{name} U+{:04X} is not in the primary font",
            *ch as u32
        );
        assert!(
            only_primary.glyph_pixel_count(*ch) >= 8,
            "{name} U+{:04X} renders blank",
            *ch as u32
        );
    }
}
