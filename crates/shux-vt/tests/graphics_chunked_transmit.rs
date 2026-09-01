//! A chunked transmission is placed on the terms its OPENING chunk set.
//!
//! The protocol says a continuation carries only `m=` (and `q=`), so a
//! spec-following client states `a=T` once. Real `kitten icat` repeats it on
//! every chunk, which is why this went unnoticed: every fixture and every
//! recorded client until terminal-browser looked like icat.

use shux_vt::VirtualTerminal;

/// `a=T` on the first chunk only, bare `m=` continuations after it — the shape
/// `terminal-browser` emits, and the one the protocol specifies.
fn chunked(w: u32, h: u32, chunk: usize, repeat_action: bool) -> Vec<u8> {
    chunked_id(w, h, chunk, repeat_action, 0)
}

/// Same, under an explicit image id. `id == 0` omits `i=` entirely.
fn identified(w: u32, h: u32, id: u32) -> Vec<u8> {
    chunked_id(w, h, 4096, false, id)
}

fn chunked_id(w: u32, h: u32, chunk: usize, repeat_action: bool, id: u32) -> Vec<u8> {
    use base64::Engine as _;
    let px: Vec<u8> = std::iter::repeat_n([200u8, 40, 40], (w * h) as usize)
        .flatten()
        .collect();
    let b64 = base64::engine::general_purpose::STANDARD.encode(&px);
    let bytes = b64.as_bytes();
    let total = bytes.len().div_ceil(chunk).max(1);
    let mut out = Vec::new();
    for (i, part) in bytes.chunks(chunk).enumerate() {
        let more = u8::from(i + 1 < total);
        let ident = if id == 0 {
            String::new()
        } else {
            format!(",i={id}")
        };
        let head = if i == 0 || repeat_action {
            format!("a=T,f=24,s={w},v={h},t=d{ident},m={more}")
        } else {
            format!("m={more}")
        };
        out.extend_from_slice(
            format!("\x1b_G{head};{}\x1b\\", std::str::from_utf8(part).unwrap()).as_bytes(),
        );
    }
    out
}

#[test]
fn an_image_whose_continuations_omit_the_action_is_still_placed() {
    let mut vt = VirtualTerminal::new(20, 40);
    vt.process(&chunked(90, 190, 4096, false));
    assert_eq!(
        vt.grid().placements().len(),
        1,
        "a transfer opened with a=T was assembled and then dropped"
    );
}

#[test]
fn repeating_the_action_on_every_chunk_still_works() {
    // `kitten icat` does this, and it must keep working.
    let mut vt = VirtualTerminal::new(20, 40);
    vt.process(&chunked(90, 190, 4096, true));
    assert_eq!(vt.grid().placements().len(), 1);
}

#[test]
fn a_transfer_opened_with_a_plain_transmit_is_not_placed() {
    // `a=t` stores without displaying. The opening chunk decides, so this must
    // stay unplaced however many continuations follow.
    let mut vt = VirtualTerminal::new(20, 40);
    let bytes = String::from_utf8(chunked(90, 190, 4096, false))
        .unwrap()
        .replacen("a=T", "a=t", 1)
        .into_bytes();
    vt.process(&bytes);
    assert!(vt.grid().placements().is_empty());
}

/// The protocol: re-transmitting an image id deletes the existing image and all
/// its placements. An app that redraws under one id -- `terminal-browser` sends
/// `i=1` on every frame -- otherwise accumulates a placement per frame until the
/// pane's cap refuses new ones and snapshots freeze on the oldest.
#[test]
fn redrawing_under_one_image_id_replaces_rather_than_accumulates() {
    let mut vt = VirtualTerminal::new(20, 40);
    for _ in 0..8 {
        vt.process(b"\x1b[H");
        vt.process(&identified(90, 190, 1));
    }
    assert_eq!(
        vt.grid().placements().len(),
        1,
        "each redraw under i=1 added a placement instead of replacing one"
    );
}

#[test]
fn distinct_image_ids_coexist() {
    let mut vt = VirtualTerminal::new(20, 40);
    vt.process(&identified(90, 190, 1));
    vt.process(&identified(90, 190, 2));
    assert_eq!(vt.grid().placements().len(), 2);
}

/// An image with no `i=` has no identity to replace, so it accumulates. That is
/// the protocol's behaviour, not a defect: `kitten icat` sends no `i=`.
#[test]
fn an_unidentified_image_still_accumulates() {
    let mut vt = VirtualTerminal::new(20, 40);
    vt.process(&identified(90, 190, 0));
    vt.process(&identified(90, 190, 0));
    assert_eq!(vt.grid().placements().len(), 2);
}
