//! A put (`a=p`) draws only its crop, fitted to the cells it asked for
//! (issue #234).
use shux_raster::{RasterOptions, Rasterizer};
use shux_vt::VirtualTerminal;

/// One declared cell wide and two tall: red over blue, one cell each.
fn two_tone() -> Vec<u8> {
    use base64::Engine as _;
    let (w, h) = (9u32, 38u32);
    let px: Vec<u8> = (0..w * h)
        .flat_map(|i| {
            if i / w < 19 {
                [220, 30, 30]
            } else {
                [30, 30, 220]
            }
        })
        .collect();
    let b64 = base64::engine::general_purpose::STANDARD.encode(&px);
    format!("\x1b_Ga=t,f=24,s={w},v={h},i=1;{b64}\x1b\\").into_bytes()
}

/// (red, blue) pixels drawn after `put`.
fn drawn(put: &str) -> (usize, usize) {
    let mut vt = VirtualTerminal::new(10, 20);
    vt.process(&two_tone());
    vt.process(format!("\x1b_G{put}\x1b\\").as_bytes());
    let r = Rasterizer::new(14.0).expect("rasterizer");
    let img = r.render(vt.grid(), &RasterOptions::default());
    let count = |want: fn(&[u8; 4]) -> bool| img.pixels().filter(|p| want(&p.0)).count();
    (
        count(|p| p[0] > 180 && p[2] < 80),
        count(|p| p[2] > 180 && p[0] < 80),
    )
}

#[test]
fn a_put_draws_the_whole_image_by_default() {
    let (red, blue) = drawn("a=p,i=1");
    assert!(red > 50 && blue > 50, "red {red}, blue {blue}");
}

#[test]
fn a_put_draws_only_its_crop() {
    let (red, blue) = drawn("a=p,i=1,y=19,h=19");
    assert_eq!(red, 0, "the cropped-away half was drawn");
    assert!(blue > 50, "the crop itself is missing ({blue} px)");
}

/// Both `c` and `r` letterbox the crop into the box: a 1x2-cell image in a
/// 4x4-cell box is scaled by the box's height, twice its own, so it covers
/// four times its area and keeps red over blue.
#[test]
fn a_put_fits_its_crop_into_the_cells_asked_for() {
    let (red, blue) = drawn("a=p,i=1");
    let (big_red, big_blue) = drawn("a=p,i=1,c=4,r=4");
    let (natural, scaled) = (red + blue, big_red + big_blue);
    assert!(
        scaled > natural * 3 && scaled < natural * 5,
        "fitted into 4x4 cells it drew {scaled} px against {natural} at its own size"
    );
    assert!(
        big_red > big_blue / 2 && big_blue > big_red / 2,
        "aspect lost"
    );
}
