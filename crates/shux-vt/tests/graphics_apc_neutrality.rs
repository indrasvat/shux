//! The APC scanner must be observationally neutral.
//!
//! `VirtualTerminal::process_with_responses` cuts its `advance` calls at APC
//! boundaries so a graphics command can be acted on at its true position in the
//! stream. That slicing is only sound if it is invisible: vte receives every
//! byte either way, so the grid, cursor, title and reply stream must not depend
//! on where the cuts fall or on how the PTY happened to chunk the read.
//!
//! Nothing else in the suite pins this. The property is currently upheld
//! structurally -- `dispatch_graphics` has no body yet -- which is exactly why
//! it needs a test now rather than later: the first line added there that
//! touches the grid, the cursor or `responses` would break the invariant
//! silently, and every other test would stay green.
//!
//! The alphabet below is deliberately hostile. It is weighted toward the bytes
//! that make a naive splitter diverge from vte: `ESC`, `CAN` (0x18), `SUB`
//! (0x1A), the string introducers `_ X ^ P ]`, and the `\` of a String
//! Terminator. Plain-text-only inputs would exercise only the `memmem` fast
//! path -- as, in fact, all five committed rich-TUI corpus fixtures do, since
//! not one of them contains a single `ESC _`, CAN or SUB byte.

use proptest::prelude::*;
use shux_vt::VirtualTerminal;

/// Everything a pane can observe about a terminal after feeding it bytes.
#[derive(Debug, PartialEq, Eq)]
struct Observable {
    text: String,
    cursor: (usize, usize, bool),
    title: Option<String>,
    alt_screen: bool,
    responses: Vec<Vec<u8>>,
}

fn drive(chunks: &[&[u8]]) -> Observable {
    let mut vt = VirtualTerminal::new(24, 80);
    let mut responses = Vec::new();
    for chunk in chunks {
        responses.extend(vt.process_with_responses(chunk));
    }
    let cursor = vt.cursor();
    Observable {
        text: vt.capture_text(None),
        cursor: (cursor.row, cursor.col, cursor.visible),
        title: vt.title().map(str::to_owned),
        alt_screen: vt.is_alternate_screen(),
        responses,
    }
}

/// Bytes chosen to land on the seams: string introducers, aborts, terminators.
fn hostile_byte() -> impl Strategy<Value = u8> {
    prop_oneof![
        6 => prop::num::u8::ANY.prop_map(|b| 0x20 + (b % 0x5f)), // printable
        3 => Just(0x1b),                                        // ESC
        1 => Just(0x18),                                        // CAN
        1 => Just(0x1a),                                        // SUB
        2 => Just(b'_'),                                        // APC introducer
        1 => Just(b'X'),                                        // SOS
        1 => Just(b'^'),                                        // PM
        1 => Just(b'P'),                                        // DCS
        1 => Just(b']'),                                        // OSC
        1 => Just(b'['),                                        // CSI
        2 => Just(b'\\'),                                       // ST tail
        1 => Just(b'G'),                                        // kitty graphics
        1 => Just(b'\n'),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// Delivering the same bytes as one write or as many must be identical.
    ///
    /// This is the property that protects the slicing: a cut at an APC boundary
    /// is one particular re-chunking, so if arbitrary re-chunking is invisible
    /// then so is the cut.
    ///
    /// Note what this deliberately does NOT cover. Neutrality is invariant to
    /// *where* the cuts land -- that is the entire safety argument -- so no test
    /// here can detect a scanner that finds the wrong APCs. Verified by
    /// mutation: replacing the carried scanner with a fresh one per read leaves
    /// this file green. Detection correctness is a different property and is
    /// pinned a layer down, by the unit tests in `graphics::apc` (themselves
    /// mutation-proved).
    #[test]
    fn chunking_is_invisible(
        stream in prop::collection::vec(hostile_byte(), 0..400),
        split_a in 0usize..400,
        split_b in 0usize..400,
    ) {
        let whole = drive(&[&stream]);

        let (mut i, mut j) = (split_a.min(stream.len()), split_b.min(stream.len()));
        if i > j {
            std::mem::swap(&mut i, &mut j);
        }
        let split = drive(&[&stream[..i], &stream[i..j], &stream[j..]]);
        prop_assert_eq!(whole, split);
    }

    /// Byte-at-a-time delivery is the worst case for carried state.
    #[test]
    fn byte_at_a_time_is_invisible(
        stream in prop::collection::vec(hostile_byte(), 0..200),
    ) {
        let whole = drive(&[&stream]);
        let singles: Vec<&[u8]> = stream.chunks(1).collect();
        prop_assert_eq!(whole, drive(&singles));
    }
}

/// An APC's payload is not screen content, however printable it looks.
///
/// A `dispatch_graphics` that ever wrote its command through to the grid would
/// paint control data over the user's text; so would a mis-cut that let vte
/// resume in `Ground` partway through a body.
#[test]
fn apc_payload_never_reaches_the_grid() {
    let observed = drive(&[
        b"visible-before\r\n\x1b_Ga=T,f=32,s=1,v=1,i=1,q=2;SECRETPAYLOAD\x1b\\visible-after",
    ]);
    assert!(observed.text.contains("visible-before"));
    assert!(observed.text.contains("visible-after"));
    assert!(
        !observed.text.contains("SECRETPAYLOAD"),
        "APC payload leaked onto the screen: {:?}",
        observed.text
    );
    assert!(
        !observed.text.contains("a=T"),
        "APC control data leaked onto the screen: {:?}",
        observed.text
    );
}

/// No graphics command is answered yet, and that is deliberate.
///
/// Replying `OK` to a query before shux can actually draw would turn an honest
/// "this terminal cannot show images" into a silent blank screen: a client that
/// believes the terminal is capable stops probing and starts transmitting.
/// The query answer must land in the same change as the renderer.
#[test]
fn graphics_queries_are_not_answered_yet() {
    let observed = drive(&[b"\x1b_Gi=4207,a=q,t=d,f=24,s=1,v=1;AAAA\x1b\\\x1b[c"]);
    // The DA1 that follows it is still answered, in order, unchanged.
    assert_eq!(
        observed.responses,
        vec![b"\x1b[?62;1;2;6;9;15;22c".to_vec()]
    );
}

/// The scanner must not disturb an unterminated string sequence around it.
///
/// vte leaves a string state on `ESC`-anything; a splitter that consumed that
/// ESC would park vte inside the OSC forever and mute the pane.
#[test]
fn text_after_an_apc_interrupted_osc_still_renders() {
    let observed = drive(&[b"\x1b]0;title\x1b_Ga=q;\x1b\\AFTER-OSC"]);
    assert!(
        observed.text.contains("AFTER-OSC"),
        "text was swallowed: {:?}",
        observed.text
    );
}
