//! `a=p`: placing an image transmitted earlier, by image and placement id, and
//! the `a=d` targets that remove them (issue #234).

use shux_vt::{PixelRect, VirtualTerminal};

/// `a=t` (store only) of a `w` x `h` RGB image under `i=id`.
fn transmit(id: u32, w: u32, h: u32) -> Vec<u8> {
    use base64::Engine as _;
    let px = vec![90u8; (w * h * 3) as usize];
    let b64 = base64::engine::general_purpose::STANDARD.encode(&px);
    format!("\x1b_Ga=t,f=24,s={w},v={h},i={id};{b64}\x1b\\").into_bytes()
}

fn apc(control: &str) -> Vec<u8> {
    format!("\x1b_G{control}\x1b\\").into_bytes()
}

fn vt_with_image(id: u32) -> VirtualTerminal {
    let mut vt = VirtualTerminal::new(20, 40);
    vt.process(&transmit(id, 30, 60));
    assert!(vt.grid().placements().is_empty(), "a=t must not place");
    vt
}

#[test]
fn a_transmitted_image_is_placed_by_a_put() {
    let mut vt = vt_with_image(7);
    vt.process(b"\x1b[3;5H");
    vt.process(&apc("a=p,i=7"));
    let placed = vt.grid().placements();
    assert_eq!(placed.len(), 1, "a=p placed nothing");
    assert_eq!((placed[0].image_id, placed[0].col), (7, 4));
}

/// "If you send two placements with the same image id and placement id the
/// second one will replace the first." Without a placement id, each adds one.
#[test]
fn a_put_with_a_placement_id_moves_it_and_one_without_adds() {
    let mut vt = vt_with_image(7);
    vt.process(&apc("a=p,i=7,p=1,C=1"));
    vt.process(b"\x1b[10;20H");
    vt.process(&apc("a=p,i=7,p=1,C=1"));
    let placed = vt.grid().placements();
    assert_eq!(
        placed.len(),
        1,
        "a repeated (i, p) added a second placement"
    );
    assert_eq!(placed[0].col, 19, "the placement did not move");

    vt.process(&apc("a=p,i=7,C=1"));
    vt.process(&apc("a=p,i=7,p=0,C=1"));
    assert_eq!(vt.grid().placements().len(), 3);
}

#[test]
fn a_put_of_an_unknown_image_places_nothing() {
    let mut vt = vt_with_image(7);
    let refused = vt.graphics_refusals();
    vt.process(&apc("a=p,i=8"));
    vt.process(&apc("a=p,I=7"));
    assert!(vt.grid().placements().is_empty());
    assert_eq!(vt.graphics_refusals(), refused + 2);
}

/// "When re-transmitting image data for a specific id, the existing image and
/// all its placements must be deleted", even by an `a=t` that places nothing.
#[test]
fn re_transmitting_an_id_drops_its_placements() {
    let mut vt = vt_with_image(7);
    vt.process(&apc("a=p,i=7,C=1"));
    vt.process(&transmit(8, 30, 60));
    vt.process(&apc("a=p,i=8,C=1"));
    vt.process(&transmit(7, 30, 60));
    let ids: Vec<u32> = vt.grid().placements().iter().map(|p| p.image_id).collect();
    assert_eq!(ids, [8]);
}

/// The attach emitter retires one image with `a=d,d=I,i=ID`; that must not
/// take every other picture with it.
#[test]
fn a_delete_by_id_removes_only_that_image() {
    let mut vt = vt_with_image(7);
    vt.process(&transmit(8, 30, 60));
    vt.process(&apc("a=p,i=7,p=1,C=1"));
    vt.process(&apc("a=p,i=7,p=2,C=1"));
    vt.process(&apc("a=p,i=8,C=1"));

    vt.process(&apc("a=d,d=i,i=7,p=2"));
    let left: Vec<(u32, u32)> = vt
        .grid()
        .placements()
        .iter()
        .map(|p| (p.image_id, p.placement_id))
        .collect();
    assert_eq!(left, [(7, 1), (8, 0)], "d=i with p= removes one placement");

    vt.process(&apc("a=d,d=I,i=7"));
    let ids: Vec<u32> = vt.grid().placements().iter().map(|p| p.image_id).collect();
    assert_eq!(ids, [8]);
    vt.process(&apc("a=p,i=7"));
    assert_eq!(vt.grid().placements().len(), 1, "d=I freed the image data");
    vt.process(&apc("a=p,i=8,C=1"));
    assert_eq!(vt.grid().placements().len(), 2, "d=I freed another image");
}

/// `d=a` targets "all placements visible on screen": one scrolled wholly into
/// history stays.
#[test]
fn a_delete_of_visible_placements_keeps_those_in_scrollback() {
    let mut vt = vt_with_image(7);
    vt.process(&apc("a=p,i=7,p=1,C=1"));
    vt.process(&"\r\n".repeat(30).into_bytes());
    vt.process(&apc("a=p,i=7,p=2,C=1"));
    vt.process(&apc("a=d,d=a"));
    let left: Vec<u32> = vt
        .grid()
        .placements()
        .iter()
        .map(|p| p.placement_id)
        .collect();
    assert_eq!(left, [1]);
}

#[test]
fn a_put_records_its_crop_and_display_size() {
    let mut vt = vt_with_image(7);
    vt.process(&apc("a=p,i=7,x=5,y=10,w=100,h=20,c=4,C=1"));
    let g = vt.grid().placements()[0].geometry;
    // `w=` runs past the image's 30 px edge, so the crop stops there.
    assert_eq!(
        g.crop,
        PixelRect {
            x: 5,
            y: 10,
            w: 25,
            h: 20
        }
    );
    // Four columns of a 25x20 crop keep its aspect: ceil(4*10*20/(25*20)) rows.
    assert_eq!(g.display, Some((4, 2)));
    vt.process(&apc("a=p,i=7,x=30,C=1"));
    assert_eq!(vt.grid().placements().len(), 1, "a crop outside the image");
    vt.process(&apc("a=p,i=7,c=4000000000,r=4000000000,C=1"));
    assert_eq!(
        vt.grid().placements()[1].geometry.display,
        Some((40, 20)),
        "the display box is capped at the pane"
    );
}

#[test]
fn a_full_reset_clears_placements_and_stored_images() {
    let mut vt = vt_with_image(7);
    vt.process(&apc("a=p,i=7,C=1"));
    vt.process(b"\x1bc");
    assert!(vt.grid().placements().is_empty());
    vt.process(&apc("a=p,i=7"));
    assert!(vt.grid().placements().is_empty(), "RIS kept the image data");
}

/// A put inside a synchronized-output window changes the presented frame only
/// when the window closes.
#[test]
fn a_put_inside_a_sync_window_waits_for_its_release() {
    let mut vt = vt_with_image(7);
    vt.process(b"\x1b[?2026h");
    vt.process(&apc("a=p,i=7,C=1"));
    assert!(
        vt.grid().placements().is_empty(),
        "the put tore the held frame"
    );
    vt.process(b"\x1b[?2026l");
    assert_eq!(vt.grid().placements().len(), 1);
}

/// Hostile sizes cannot overflow the aspect arithmetic.
#[test]
fn an_extreme_display_size_with_one_side_missing_is_capped() {
    let mut vt = VirtualTerminal::new(20, 40);
    vt.process(&transmit(7, 1, 60));
    vt.process(&apc("a=p,i=7,c=4294967295,C=1"));
    vt.process(&apc("a=p,i=7,r=4294967295,C=1"));
    let boxes: Vec<_> = vt
        .grid()
        .placements()
        .iter()
        .map(|p| p.geometry.display)
        .collect();
    assert_eq!(boxes.len(), 2);
    assert!(
        boxes
            .iter()
            .all(|b| b.is_some_and(|(c, r)| c <= 40 && r <= 20)),
        "{boxes:?}"
    );
}
