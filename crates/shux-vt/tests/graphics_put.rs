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

/// Re-transmitting an id drops its placements even when the new placement
/// itself places nothing (a crop outside the image).
#[test]
fn a_re_transmit_that_places_nothing_still_drops_the_old_placements() {
    use base64::Engine as _;
    let mut vt = VirtualTerminal::new(20, 40);
    let px = base64::engine::general_purpose::STANDARD.encode(vec![90u8; 30 * 60 * 3]);
    vt.process(format!("\x1b_Ga=T,f=24,s=30,v=60,i=7,C=1;{px}\x1b\\").as_bytes());
    assert_eq!(vt.grid().placements().len(), 1);
    vt.process(format!("\x1b_Ga=T,f=24,s=30,v=60,i=7,x=999,C=1;{px}\x1b\\").as_bytes());
    assert!(
        vt.grid().placements().is_empty(),
        "the old pixels of i=7 are still on screen"
    );
}

/// `d=I` frees the data of the image it names, not of every unplaced one.
#[test]
fn an_uppercase_delete_frees_only_the_image_it_names() {
    let mut vt = vt_with_image(7);
    vt.process(&transmit(9, 30, 60));
    vt.process(&apc("a=p,i=7,C=1"));
    vt.process(&apc("a=d,d=I,i=7"));
    vt.process(&apc("a=p,i=9,C=1"));
    let ids: Vec<u32> = vt.grid().placements().iter().map(|p| p.image_id).collect();
    assert_eq!(ids, [9], "image 9 was freed by a delete naming 7");
}

/// A delete-all from the alternate screen keeps the data of an image the
/// primary screen still shows.
#[test]
fn a_delete_all_on_the_alternate_screen_keeps_what_the_primary_shows() {
    let mut vt = vt_with_image(7);
    vt.process(&apc("a=p,i=7,C=1"));
    vt.process(b"\x1b[?1049h");
    vt.process(&apc("a=d,d=A"));
    vt.process(b"\x1b[?1049l");
    assert_eq!(vt.grid().placements().len(), 1);
    vt.process(&apc("a=p,i=7,C=1"));
    assert_eq!(
        vt.grid().placements().len(),
        2,
        "the primary's image was freed"
    );
}

/// A placement id means nothing without an image id: two such pictures are
/// two placements, not one replacing the other.
#[test]
fn a_placement_id_without_an_image_id_replaces_nothing() {
    use base64::Engine as _;
    let mut vt = VirtualTerminal::new(20, 40);
    let px = base64::engine::general_purpose::STANDARD.encode(vec![90u8; 30 * 60 * 3]);
    for _ in 0..2 {
        vt.process(format!("\x1b_Ga=T,f=24,s=30,v=60,p=1,C=1;{px}\x1b\\").as_bytes());
    }
    assert_eq!(vt.grid().placements().len(), 2);
}

/// Two 2x2-cell pictures side by side: 7 at the top left, 8 at columns 6-7.
fn two_pictures() -> VirtualTerminal {
    let mut vt = VirtualTerminal::new(20, 40);
    vt.process(&transmit(7, 18, 38));
    vt.process(&transmit(8, 18, 38));
    vt.process(&apc("a=p,i=7,C=1"));
    vt.process(b"\x1b[1;6H");
    vt.process(&apc("a=p,i=8,C=1"));
    vt
}

fn placed(vt: &VirtualTerminal) -> Vec<u32> {
    vt.grid().placements().iter().map(|p| p.image_id).collect()
}

/// A delete aimed at a cell, column, row, z-index or id range removes what it
/// hits and nothing else.
#[test]
fn a_targeted_delete_removes_only_what_it_hits() {
    for (target, left) in [
        ("d=p,x=1,y=1", vec![8]),
        ("d=p,x=7,y=2", vec![7]),
        ("d=p,x=4,y=1", vec![7, 8]),
        ("d=x,x=6", vec![7]),
        ("d=y,y=3", vec![7, 8]),
        ("d=y,y=2", vec![]),
        ("d=z,z=5", vec![7, 8]),
        ("d=z,z=0", vec![]),
        ("d=q,x=1,y=1,z=0", vec![8]),
        ("d=r,x=8,y=9", vec![7]),
    ] {
        let mut vt = two_pictures();
        vt.process(b"\x1b[10;10H");
        vt.process(&apc(&format!("a=d,{target}")));
        assert_eq!(placed(&vt), left, "a=d,{target}");
    }
    let mut vt = two_pictures();
    vt.process(b"\x1b[1;7H");
    vt.process(&apc("a=d,d=c"));
    assert_eq!(placed(&vt), [7], "d=c deletes at the cursor");
}

/// An uppercase delete frees only the images whose placements it removed; a
/// delete it cannot resolve removes nothing.
#[test]
fn an_uppercase_targeted_delete_frees_only_what_it_removed() {
    let mut vt = two_pictures();
    vt.process(&apc("a=d,d=P,x=1,y=1"));
    vt.process(&apc("a=p,i=8,C=1"));
    vt.process(&apc("a=p,i=7,C=1"));
    assert_eq!(placed(&vt), [8, 8], "7 was freed, 8 kept");

    let mut vt = two_pictures();
    let refused = vt.graphics_refusals();
    vt.process(&apc("a=d,d=N,I=3"));
    vt.process(&apc("a=d,d=f,i=7"));
    assert_eq!(placed(&vt), [7, 8]);
    assert_eq!(vt.graphics_refusals(), refused + 2);
}

/// Each screen has its own images: an id re-used on the alternate screen does
/// not change what the primary screen's puts draw.
#[test]
fn the_alternate_screen_has_its_own_images() {
    use base64::Engine as _;
    let enc = |byte: u8| base64::engine::general_purpose::STANDARD.encode([byte; 3]);
    let mut vt = VirtualTerminal::new(20, 40);
    vt.process(format!("\x1b_Ga=T,f=24,s=1,v=1,i=7,C=1;{}\x1b\\", enc(0)).as_bytes());
    vt.process(b"\x1b[?1049h");
    vt.process(format!("\x1b_Ga=T,f=24,s=1,v=1,i=7,C=1;{}\x1b\\", enc(255)).as_bytes());
    vt.process(b"\x1b[?1049l");
    vt.process(&apc("a=p,i=7,p=2,C=1"));
    let payloads: Vec<u8> = vt
        .grid()
        .placements()
        .iter()
        .map(|p| p.image.payload[0])
        .collect();
    assert_eq!(
        payloads,
        [0, 0],
        "the primary's put drew the alternate screen's image"
    );

    // And the alternate screen starts each visit with none.
    vt.process(b"\x1b[?1049h");
    vt.process(&apc("a=p,i=7,C=1"));
    assert!(vt.grid().placements().is_empty());
}

/// kitty drops an id's old image when a re-transmit of it begins, so one
/// that then fails (a payload of the wrong size) leaves nothing behind.
#[test]
fn a_failed_re_transmit_still_drops_the_old_image() {
    let mut vt = vt_with_image(7);
    vt.process(&apc("a=p,i=7,C=1"));
    vt.process(b"\x1b_Ga=T,f=24,s=30,v=60,i=7;AAAA\x1b\\");
    assert!(
        vt.grid().placements().is_empty(),
        "the old picture stayed up"
    );
    vt.process(&apc("a=p,i=7"));
    assert!(
        vt.grid().placements().is_empty(),
        "the old data is still stored"
    );
}

/// A z-index delete matches the z-index each picture was put at.
#[test]
fn a_z_index_delete_matches_the_z_each_picture_was_put_at() {
    let mut vt = vt_with_image(7);
    vt.process(&apc("a=p,i=7,z=-1,C=1"));
    vt.process(&apc("a=d,d=z,z=0"));
    assert_eq!(
        vt.grid().placements().len(),
        1,
        "z=0 removed a z=-1 picture"
    );
    vt.process(&apc("a=d,d=q,x=1,y=1,z=-1"));
    assert!(vt.grid().placements().is_empty(), "z=-1 missed its picture");
}

/// Uppercase deletes by id or range free the images they name, placed or
/// not; one that names a placement frees nothing it did not remove.
#[test]
fn an_uppercase_delete_by_id_or_range_frees_what_it_names() {
    let mut vt = vt_with_image(7);
    vt.process(&apc("a=d,d=R,x=0,y=7"));
    vt.process(&apc("a=p,i=7,C=1"));
    assert!(
        vt.grid().placements().is_empty(),
        "d=R kept unplaced image 7"
    );

    let mut vt = vt_with_image(7);
    vt.process(&apc("a=p,i=7,C=1"));
    vt.process(&apc("a=d,d=r,x=0,y=7"));
    assert!(
        vt.grid().placements().is_empty(),
        "a range from 0 was refused"
    );

    let mut vt = vt_with_image(7);
    vt.process(&apc("a=d,d=I,i=7,p=99"));
    vt.process(&apc("a=p,i=7,C=1"));
    assert_eq!(
        vt.grid().placements().len(),
        1,
        "a missing placement freed 7"
    );
}

/// A transfer the alternate screen started and the primary screen finished
/// belongs to neither: it must not overwrite the primary's image under the
/// same id.
#[test]
fn a_transfer_finished_on_the_other_screen_stores_nothing() {
    use base64::Engine as _;
    let enc = |byte: u8| base64::engine::general_purpose::STANDARD.encode([byte; 3]);
    let mut vt = VirtualTerminal::new(20, 40);
    vt.process(format!("\x1b_Ga=t,f=24,s=1,v=1,i=7;{}\x1b\\", enc(0)).as_bytes());
    vt.process(b"\x1b[?1049h");
    vt.process(format!("\x1b_Ga=t,f=24,s=1,v=1,i=7,m=1;{}\x1b\\", enc(255)).as_bytes());
    vt.process(b"\x1b[?1049l");
    vt.process(b"\x1b_Gm=0;\x1b\\");
    vt.process(&apc("a=p,i=7,C=1"));
    let drawn: Vec<u8> = vt
        .grid()
        .placements()
        .iter()
        .map(|p| p.image.payload[0])
        .collect();
    assert_eq!(
        drawn,
        [0],
        "the alternate screen's transfer replaced the primary's image"
    );
}
