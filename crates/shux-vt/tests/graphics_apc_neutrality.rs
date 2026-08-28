//! The APC scanner must be observationally neutral.
//!
//! `process_with_responses` cuts its `advance` calls at APC boundaries so a
//! graphics command lands at its true stream position. That is only sound if it
//! is invisible: grid, cursor, title and replies must not depend on where the
//! cuts fall or how the PTY chunked the read.
//!
//! The property currently holds structurally -- `dispatch_graphics` has no body
//! -- which is why it needs pinning now: the first line added there that touches
//! the grid, cursor or `responses` would break it while every other test stayed
//! green.
//!
//! The alphabet is weighted toward bytes that make a naive splitter diverge:
//! `ESC`, `CAN`, `SUB`, the string introducers `_ X ^ P ]`, and ST's `\`.
//!
//! Those bytes are not exotic. Counted over the five committed rich-TUI
//! fixtures: `btop` carries 1,902 `ESC`, `lazygit` 1,010 (and 10 `\`), `nvim`
//! 760, `vicaya` 747 (and 19 `_`), `vivecaka` 87. The fast path needs
//! `Ground` AND no `ESC` in the chunk, and btop emits one every ~20 bytes, so
//! every real rich-TUI read takes `scan_slow`. The generated alphabet is
//! therefore representative of the hot path, not a synthetic worst case.

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
    drive_with(chunks, true)
}

fn drive_with(chunks: &[&[u8]], slicing: bool) -> Observable {
    let mut vt = VirtualTerminal::new(24, 80);
    vt.set_apc_cut_slicing(slicing);
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
        1 => Just(b'G'),                                        // kitty graphics
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

/// No graphics command is answered yet, deliberately. Replying `OK` before shux
/// can draw turns an honest "cannot show images" into a silent blank screen: the
/// client stops probing and starts transmitting. The answer ships with the
/// renderer.
#[test]
fn graphics_queries_are_not_answered_yet() {
    let observed = drive(&[b"\x1b_Gi=4207,a=q,t=d,f=24,s=1,v=1;AAAA\x1b\\\x1b[c"]);
    // The DA1 that follows it is still answered, in order, unchanged.
    assert_eq!(
        observed.responses,
        vec![b"\x1b[?62;1;2;6;9;15;22c".to_vec()]
    );
}

/// The scanner must not disturb an unterminated string sequence around it: vte
/// leaves a string state on `ESC`-anything, and a splitter consuming that ESC
/// would park vte inside the OSC forever and mute the pane.
#[test]
fn text_after_an_apc_interrupted_osc_still_renders() {
    let observed = drive(&[b"\x1b]0;title\x1b_Ga=q;\x1b\\AFTER-OSC"]);
    assert!(
        observed.text.contains("AFTER-OSC"),
        "text was swallowed: {:?}",
        observed.text
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
        !whole.text.contains('\u{80}'),
        "whole-buffer decode executed it"
    );
    assert!(
        split.text.contains('\u{80}'),
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
/// So: turn the refusals on and observe one on the wire. This is the only
/// assertion in the file that fails if the scanner stops locating APCs, and it
/// exercises the shipping library rather than a test-only field.
#[test]
fn a_located_command_reaches_the_parser_in_the_shipping_build() {
    let mut vt = shux_vt::VirtualTerminal::new(24, 80);
    vt.set_graphics_replies(true);

    let responses = vt.process_with_responses(b"\x1b_Ga=T,t=f,i=77;L2V0Yy9wYXNzd2Q=\x1b\\");
    let joined: Vec<u8> = responses.concat();
    let reply = String::from_utf8_lossy(&joined);

    assert!(
        reply.contains("\x1b_Gi=77;") && reply.contains("ENOTSUPPORTED"),
        "the file transport was not refused on the wire; got {reply:?}"
    );
}

/// ...and by default it stays off, so a pane sees exactly what it sees today.
///
/// This is the half that protects users: any reply at all tells an application
/// that shux supports the graphics protocol, so it abandons its text fallback.
#[test]
fn nothing_is_answered_by_default() {
    let mut vt = shux_vt::VirtualTerminal::new(24, 80);
    for command in [
        b"\x1b_Ga=T,t=f,i=77;L2V0Yy9wYXNzd2Q=\x1b\\".as_slice(),
        b"\x1b_Ga=q,i=31,s=1,v=1,t=d,f=24;AAAA\x1b\\",
        b"\x1b_Ga=T,f=32,s=1,v=1,i=1;AAAA\x1b\\",
    ] {
        assert!(
            vt.process_with_responses(command).is_empty(),
            "shux answered a graphics command it cannot yet honour: {command:?}"
        );
    }
}
