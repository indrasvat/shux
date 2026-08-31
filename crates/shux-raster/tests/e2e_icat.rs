//! Drive the bytes REAL `kitten icat` emits through the VT and assert the
//! picture reaches the PNG. Captured from kitten 0.32.2 under a PTY at 80x24
//! with 9x19 px cells -- shux's declared geometry.
use shux_raster::{RasterOptions, Rasterizer};
use shux_vt::VirtualTerminal;

#[test]
fn a_real_icat_image_reaches_the_rendered_png() {
    let bytes = include_bytes!("fixtures/icat-32x32-png.bin");
    let mut vt = VirtualTerminal::new(24, 80);
    vt.process(bytes);

    assert_eq!(
        vt.grid().placements().len(),
        1,
        "icat placed exactly one image"
    );

    let r = Rasterizer::new(14.0).expect("rasterizer");
    let img = r.render(vt.grid(), &RasterOptions::default());

    let (mut magenta, mut green) = (0u32, 0u32);
    for p in img.pixels() {
        let [rr, gg, bb, _] = p.0;
        if rr > 200 && gg < 80 && bb > 200 {
            magenta += 1;
        }
        if rr < 80 && gg > 150 && bb < 140 {
            green += 1;
        }
    }
    assert!(
        magenta > 100,
        "the image's magenta quadrants are missing ({magenta} px)"
    );
    assert!(
        green > 100,
        "the image's green quadrants are missing ({green} px)"
    );
}

/// A CHUNKED transfer. Real `kitten icat` repeats `a=T` on every continuation
/// chunk, so a rule that restarts assembly whenever `a=` is present keeps only
/// the first chunk and the image silently never appears.
#[test]
fn a_chunked_icat_transfer_is_assembled_whole() {
    let bytes = include_bytes!("fixtures/icat-chunked-rgb-zlib.bin");
    let mut vt = VirtualTerminal::new(24, 80);
    vt.process(bytes);

    let placements = vt.grid().placements();
    assert_eq!(placements.len(), 1, "icat placed exactly one image");
    let img = &placements[0].image;
    assert_eq!(
        (img.width, img.height),
        (720, 540),
        "the declared size came from the FIRST chunk's control block"
    );

    let r = Rasterizer::new(14.0).expect("rasterizer");
    let png = r.render(vt.grid(), &RasterOptions::default());
    let lit = png
        .pixels()
        .filter(|p| p.0[0] as u32 + p.0[1] as u32 + p.0[2] as u32 > 90)
        .count();
    assert!(
        lit > 10_000,
        "the reassembled image did not reach the canvas ({lit} lit px)"
    );
}
