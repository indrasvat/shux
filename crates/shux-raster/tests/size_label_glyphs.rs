//! The glyphs of a pane's size label (`⤡ 118×37`, `shux_ui::viewport::size_label`)
//! each draw inside the one cell the border gives them.

use shux_raster::{RasterOptions, Rasterizer};
use shux_vt::VirtualTerminal;

#[test]
fn each_size_label_glyph_inks_its_own_cell_and_no_other() {
    let r = Rasterizer::new(14.0).expect("rasterizer");
    let (cw, _) = r.cell_size();
    for ch in ['⤡', '×'] {
        let mut vt = VirtualTerminal::new(1, 3);
        vt.process(format!(" {ch} ").as_bytes());
        let opts = RasterOptions {
            fg_default: [255, 255, 255],
            bg_default: [0, 0, 0],
            cursor: None,
            ..Default::default()
        };
        let img = r.render(vt.grid(), &opts);
        let mut ink = [0u32; 3];
        for (x, _, p) in img.enumerate_pixels() {
            if (p[0], p[1], p[2]) != (0, 0, 0) {
                ink[(x / cw) as usize] += 1;
            }
        }
        assert!(ink[1] >= 8, "U+{:04X} renders blank: {ink:?}", ch as u32);
        assert_eq!(
            (ink[0], ink[2]),
            (0, 0),
            "U+{:04X} spills out of its cell: {ink:?}",
            ch as u32
        );
    }
}
