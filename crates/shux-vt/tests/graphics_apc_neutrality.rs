//! The APC scanner must be observationally neutral.
//!
//! `process_with_responses` cuts its `advance` calls at APC boundaries so a
//! graphics command lands at its true stream position. That is only sound if
//! the result does not depend on where the cuts fall or how the PTY chunked
//! the read.
//!
//! The generated properties compare slicing on against slicing off on the
//! SAME chunks. That comparison is only meaningful while the graphics path is
//! inert, because slicing off never dispatches at all -- so `G` is absent from
//! the generated alphabet and every generated APC is a non-kitty one, which
//! exercises the cutting identically. A kitty command that really places is
//! chunk-tested deterministically instead, by
//! `a_chunked_command_places_the_same_however_it_is_delivered`.
//!
//! The alphabet is weighted toward bytes that make a naive splitter diverge:
//! `ESC`, `CAN`, `SUB`, the string introducers `_ X ^ P ]`, and ST's `\`.
//!

use proptest::prelude::*;
use shux_vt::{FrameEnvelope, MaskSet, VirtualTerminal};

/// Everything a pane can observe about a terminal after feeding it bytes.
///
/// `frame` is the load-bearing field: it carries each cell's foreground,
/// background, flags and extended attributes. Comparing characters alone lets
/// an attribute-only divergence through, which is what CLAUDE.md's colour-probe
/// rule exists to stop. The rest are the other channels a future
/// `dispatch_graphics` could disturb the presented frame through.
#[derive(Debug, PartialEq, Eq)]
struct Observable {
    frame: String,
    scrollback: String,
    cursor: String,
    title: Option<String>,
    scroll_region: String,
    content_revision: u64,
    responses: Vec<Vec<u8>>,
}

fn drive(chunks: &[&[u8]]) -> Observable {
    drive_with(chunks, true)
}

fn drive_with(chunks: &[&[u8]], slicing: bool) -> Observable {
    let mut vt = VirtualTerminal::new(24, 80);
    vt.set_apc_cut_slicing(slicing);
    let mut responses = Vec::new();
    for chunk in chunks {
        responses.extend(vt.process_with_responses(chunk));
    }
    Observable {
        frame: FrameEnvelope::from_terminal(&vt, &MaskSet::new()).to_canonical_json(),
        scrollback: vt.capture_text(None),
        cursor: format!("{:?}", vt.cursor()),
        title: vt.title().map(str::to_owned),
        scroll_region: format!("{:?}", vt.scroll_region()),
        content_revision: vt.content_revision(),
        responses,
    }
}

/// Bytes chosen to land on the seams: string introducers, aborts, terminators.
///
/// The multibyte arms are load-bearing: the only split-sensitive machinery in
/// `vte::Parser::advance` is its partial-UTF-8 buffer and the 3-byte lookahead
/// filling it, which an ASCII-only alphabet cannot reach at all.
fn hostile_byte() -> impl Strategy<Value = u8> {
    prop_oneof![
        6 => prop::num::u8::ANY.prop_map(|b| 0x20 + (b % 0x5f)), // printable ASCII
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
        1 => Just(b'\n'),
        1 => Just(0x07),                                        // BEL, the OSC terminator
        1 => Just(0x08),                                        // BS
        1 => Just(0x09),                                        // TAB
        1 => Just(0x0d),                                        // CR
        3 => (0xc2u8..=0xf4).boxed(),                           // UTF-8 lead bytes
        3 => (0x80u8..=0xbf).boxed(),                           // UTF-8 continuations
        1 => Just(0x9b),                                        // C1 CSI
        1 => Just(0x9c),                                        // C1 ST
        1 => Just(0x9f),                                        // C1 APC
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// Cutting `advance` at APC boundaries must change nothing. Both terminals
    /// get the SAME bytes in the SAME chunks; only one slices. Comparing two
    /// *chunkings* instead would measure vte, which is chunk-sensitive already
    /// (see `c1_controls_are_chunk_sensitive_in_vte`).
    #[test]
    fn slicing_at_apc_boundaries_is_invisible(
        stream in prop::collection::vec(hostile_byte(), 0..400),
        split_a in 0usize..400,
        split_b in 0usize..400,
    ) {
        let (mut i, mut j) = (split_a.min(stream.len()), split_b.min(stream.len()));
        if i > j {
            std::mem::swap(&mut i, &mut j);
        }
        let chunks: [&[u8]; 3] = [&stream[..i], &stream[i..j], &stream[j..]];
        prop_assert_eq!(drive_with(&chunks, true), drive_with(&chunks, false));
    }

    /// Byte-at-a-time delivery is the worst case for the scanner's carried state.
    #[test]
    fn slicing_is_invisible_byte_at_a_time(
        stream in prop::collection::vec(hostile_byte(), 0..200),
    ) {
        let singles: Vec<&[u8]> = stream.chunks(1).collect();
        prop_assert_eq!(drive_with(&singles, true), drive_with(&singles, false));
    }
}

/// An APC's payload is not screen content, however printable it looks. A
/// mis-cut letting vte resume in `Ground` mid-body would paint control data
/// over the user's text.
#[test]
fn apc_payload_never_reaches_the_grid() {
    let observed = drive(&[
        b"visible-before\r\n\x1b_Ga=T,f=32,s=1,v=1,i=1,q=2;SECRETPAYLOAD\x1b\\visible-after",
    ]);
    assert!(observed.scrollback.contains("visible-before"));
    assert!(observed.scrollback.contains("visible-after"));
    assert!(
        !observed.scrollback.contains("SECRETPAYLOAD"),
        "APC payload leaked onto the screen: {:?}",
        observed.scrollback
    );
    assert!(
        !observed.scrollback.contains("a=T"),
        "APC control data leaked onto the screen: {:?}",
        observed.scrollback
    );
}

/// The scanner must not disturb an unterminated string sequence around it: vte
/// leaves a string state on `ESC`-anything, and a splitter consuming that ESC
/// would park vte inside the OSC forever and mute the pane.
#[test]
fn text_after_an_apc_interrupted_osc_still_renders() {
    let observed = drive(&[b"\x1b]0;title\x1b_Ga=q;\x1b\\AFTER-OSC"]);
    assert!(
        observed.scrollback.contains("AFTER-OSC"),
        "text was swallowed: {:?}",
        observed.scrollback
    );
}

/// The screen-visible half of the RIS case; the unit test of the same name in
/// `lib.rs` covers delivery, and carries the reasoning.
#[test]
fn a_ris_does_not_swallow_an_apc_that_starts_after_it() {
    let mut vt = VirtualTerminal::new(24, 80);
    // RIS, then the first half of a graphics command, in one read.
    vt.process_with_responses(b"\x1bc\x1b_Ghalf");
    // The rest of it, in the next read.
    vt.process_with_responses(b"more\x1b\\visible");
    let text = vt.capture_text(None);
    assert!(
        text.contains("visible"),
        "text after the APC was lost: {text:?}"
    );
    assert!(
        !text.contains("half") && !text.contains("more"),
        "APC body leaked onto the screen, so the sequence was not tracked \
         across the RIS: {text:?}"
    );
}

/// A C1 control written as two-byte UTF-8 renders differently depending on how
/// the PTY happened to chunk the read. **This is a pre-existing shux defect and
/// is NOT caused by the APC scanner.**
///
/// Reproduced against a build of the base commit `f071c89`, which has no
/// graphics code at all: `" " C2 80 " "` delivered whole leaves the cursor at
/// column 2 having printed nothing, and delivered byte-at-a-time prints U+0080
/// and leaves it at column 3. vte executes the character on one decode path and
/// prints it on the other.
///
/// The class is exactly U+0080..=U+009F: NBSP, e-acute, CJK, astral emoji, bare
/// continuation bytes and truncated sequences were all measured chunk-invariant.
///
/// Pinned, not fixed: the fix means changing how shux drives vte's UTF-8
/// decoding on the hottest path, which does not belong in a change about APC
/// sequences. If a vte upgrade changes this, this test says so.
#[test]
fn c1_controls_are_chunk_sensitive_in_vte() {
    let stream: &[u8] = b"\x20\xc2\x80\x20";
    let whole = drive(&[stream]);
    let singles: Vec<&[u8]> = stream.chunks(1).collect();
    let split = drive(&singles);
    assert_ne!(
        whole, split,
        "vte's C1 chunk-sensitivity appears to be fixed -- delete this test, drop \
         `encodes_a_c1_control`, and let the chunking properties cover the range"
    );
    assert!(
        !whole.scrollback.contains('\u{80}'),
        "whole-buffer decode executed it"
    );
    assert!(
        split.scrollback.contains('\u{80}'),
        "byte-at-a-time decode printed it"
    );
}

/// The witness the rest of this file lacks.
///
/// Every other property here compares a sliced terminal against an unsliced
/// one, and both arms stay equal when the scanner does nothing at all -- a
/// build with `scan` gutted to `return Vec::new()` passes all of them. Nothing
/// in an integration test can see `dispatched_graphics`, which is
/// `#[cfg(test)]` and so belongs to the unit-test build, not this one.
///
/// The refusal counter is the one thing the shipping library exposes that only
/// moves when an APC was actually located, parsed, and declined.
#[test]
fn a_located_command_reaches_the_parser_in_the_shipping_build() {
    let mut vt = VirtualTerminal::new(24, 80);
    assert_eq!(vt.graphics_refusals(), 0);

    vt.process_with_responses(b"\x1b_Ga=T,t=f,i=77;L2V0Yy9wYXNzd2Q=\x1b\\");
    assert_eq!(
        vt.graphics_refusals(),
        1,
        "the file transport was not located, parsed and refused"
    );

    // A well-formed direct transmission is not a refusal, so the counter is
    // measuring the verdict rather than merely counting APCs.
    vt.process_with_responses(b"\x1b_Ga=T,f=32,s=1,v=1,i=1;AAAA\x1b\\");
    assert_eq!(vt.graphics_refusals(), 1);
}

/// Nothing is answered, ever.
///
/// The protocol treats ANY response -- an error included -- as an
/// advertisement that the terminal does graphics, after which an application
/// abandons its text fallback and transmits into a pane shux cannot draw. With
/// no reply path in the library this is structurally true, not a setting.
#[test]
fn only_a_direct_capability_probe_is_answered() {
    let mut vt = VirtualTerminal::new(24, 80);
    for command in [
        b"\x1b_Ga=T,t=f,i=77;L2V0Yy9wYXNzd2Q=\x1b\\".as_slice(),
        b"\x1b_Gaq,i=31,t=f,s=1,v=1,f=24;AAAA\x1b\\",
        b"\x1b_Ga=q,i=31,s=1,v=1,t=t,f=24;AAAA\x1b\\",
        b"\x1b_Ga=q,i=31,s=1,v=1,t=s,f=24;AAAA\x1b\\",
        b"\x1b_Ga=T,f=32,s=1,v=1,i=1;AAAA\x1b\\",
        b"\x1b_Ga=T,i=1,I=2;AAAA\x1b\\",
        b"\x1b_Ga=Z,i=1;AAAA\x1b\\",
    ] {
        assert!(
            vt.process_with_responses(command).is_empty(),
            "shux answered a graphics command it cannot honour: {command:?}"
        );
    }

    // The support-detection idiom: a graphics query and a device-attributes
    // request in ONE read. Measured against real `kitten icat`, which sends
    // exactly this and treats the DA1 answer as the sentinel: with no `OK`
    // before it, it declares the terminal unsupported and transmits nothing.
    // Asserted as an exact equality rather than "contains an OK", which would
    // admit any other unexpected reply.
    let combined = vt.process_with_responses(b"\x1b_Ga=q,i=31,s=1,v=1,t=d,f=24;AAAA\x1b\\\x1b[c");
    assert_eq!(
        combined.concat(),
        b"\x1b_Gi=31;OK\x1b\\\x1b[?62;1;2;6;9;15;22c".to_vec(),
        "the probe answer and the DA1 answer, in that order, and nothing else"
    );
}

/// A NON-PLACING APC must not disturb the pen, and this must be checked on a
/// stream that definitely contains one. `a=t` transmits without placing, so
/// everything the frame can see is unchanged; a placing command moves the
/// cursor on purpose and is covered by `a_placement_advances_the_cursor`.
///
/// The generated properties above cannot be relied on for this: their alphabet
/// opens APCs readily but terminates them rarely, so the dispatch seam is
/// almost never reached in a generated case. Colour in `Observable` is
/// necessary and not sufficient -- catching a pen change at the seam needs a
/// deterministic stream that definitely carries a complete
/// `ESC _ G ... ESC \` between two attributed spans.
#[test]
fn a_complete_apc_disturbs_neither_the_pen_nor_the_frame() {
    // Underline on, text, a complete graphics command, then more text under
    // the same attribute. Colour probes per CLAUDE.md: truecolor, indexed and
    // basic all cross the seam.
    let stream: &[u8] = b"\x1b[4m\x1b[38;2;10;200;30mUNDER\
\x1b_Ga=t,f=32,s=1,v=1,i=1;AAAA\x1b\\AFTER\
\x1b[38;5;93mINDEXED\x1b[31mBASIC";

    let sliced = drive(&[stream]);
    let unsliced = drive_with(&[stream], false);
    assert_eq!(
        sliced, unsliced,
        "a complete APC changed the presented frame"
    );

    // The comparison is only worth anything if the attributes it must preserve
    // are in the frame at all.
    assert!(
        sliced.frame.contains("200"),
        "the compared frame carries no truecolor component; it cannot see a \
         pen change: {}",
        &sliced.frame[..sliced.frame.len().min(200)]
    );
}

/// A placement moves the cursor: right by its cell width, down by its height
/// less one. The `- 1` is what kitty and Zellij do against a spec that says
/// otherwise, and `kitten icat`'s single trailing newline depends on it.
#[test]
fn a_placement_advances_the_cursor() {
    // 18x38 px against the declared 9x19 cell box: 2 cells wide, 2 tall.
    let payload = base64(&[0u8; 18 * 38 * 4]);
    let mut vt = VirtualTerminal::new(24, 80);
    vt.process(format!("\x1b_Ga=T,f=32,s=18,v=38;{payload}\x1b\\").as_bytes());

    assert_eq!(vt.grid().placements().len(), 1, "the image was not placed");
    let c = vt.cursor();
    assert_eq!((c.row, c.col), (1, 2), "cursor advance");
}

/// The same placing command, delivered whole and one byte at a time, must
/// place identically. This is the chunking property for a command the
/// generated alphabet cannot reach, since it has to be a well-formed kitty
/// transfer to place at all.
#[test]
fn a_chunked_command_places_the_same_however_it_is_delivered() {
    let payload = base64(&[0u8; 9 * 19 * 4]);
    let stream = format!("\x1b_Ga=T,f=32,s=9,v=19;{payload}\x1b\\").into_bytes();

    let whole = drive(&[&stream]);
    let singles: Vec<&[u8]> = stream.chunks(1).collect();
    assert_eq!(whole, drive(&singles), "chunking changed the placement");
}

fn base64(bytes: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        for i in 0..4 {
            out.push(if i <= c.len() {
                A[(n >> (18 - 6 * i)) as usize & 63] as char
            } else {
                '='
            });
        }
    }
    out
}

/// A placement is a write. `Screen::enter` recycles a retired alternate-screen
/// buffer whenever `is_blank_canvas` says nothing was drawn on it, and that
/// verdict reads the write tally -- so a placement the tally never counted puts
/// one application's picture on the next application's screen.
#[test]
fn an_alternate_screen_placement_does_not_reach_the_next_application() {
    let payload = base64(&[0u8; 9 * 19 * 4]);
    let place = format!("\x1b_Ga=T,f=32,s=9,v=19;{payload}\x1b\\");
    let mut vt = VirtualTerminal::new(24, 80);

    vt.process(b"\x1b[?1049h");
    vt.process(place.as_bytes());
    assert_eq!(vt.grid().placements().len(), 1, "nothing was placed in alt");
    vt.process(b"\x1b[?1049l");
    assert_eq!(
        vt.grid().placements().len(),
        0,
        "the primary screen inherited the alternate screen's picture"
    );

    // The retired alt buffer goes back in the spare slot; the next application
    // that opens an alternate screen gets it.
    vt.process(b"\x1b[?1049h");
    assert_eq!(
        vt.grid().placements().len(),
        0,
        "a recycled alternate-screen buffer carried a dead application's picture"
    );
}

/// `a=d` changes the presented frame, so inside a `CSI ?2026h` window it must
/// take the freeze exactly as a placement does. Without it the picture vanishes
/// from a frame the terminal promised to hold still (#115).
#[test]
fn a_delete_inside_synchronized_output_does_not_disturb_the_held_frame() {
    let payload = base64(&[0u8; 9 * 19 * 4]);
    let place = format!("\x1b_Ga=T,f=32,s=9,v=19;{payload}\x1b\\");
    let mut vt = VirtualTerminal::new(24, 80);

    vt.process(place.as_bytes());
    assert_eq!(vt.grid().placements().len(), 1, "nothing was placed");

    vt.process(b"\x1b[?2026h");
    vt.process(b"\x1b_Ga=d,d=A\x1b\\");
    assert_eq!(
        vt.grid().placements().len(),
        1,
        "the delete reached the frame the terminal is holding still"
    );

    vt.process(b"\x1b[?2026l");
    assert_eq!(
        vt.grid().placements().len(),
        0,
        "the delete was lost when the window closed"
    );
}
