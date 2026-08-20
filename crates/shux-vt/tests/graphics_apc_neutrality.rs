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
/// The multibyte arms are load-bearing, not decoration. The only split-sensitive
/// machinery inside `vte::Parser::advance` is its partial-UTF-8 buffer and the
/// 3-byte lookahead that fills it, so a cut that disturbed those is precisely
/// the bug this file exists to catch. An ASCII-only alphabet cannot reach that
/// code at all -- an earlier version of this generator was ASCII-only and called
/// itself "hostile".
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

    /// Cutting `advance` at APC boundaries must change nothing.
    ///
    /// Both terminals get the SAME bytes in the SAME chunks; only one of them
    /// slices. That is what isolates this code: comparing two *chunkings*
    /// instead would measure vte, which is chunk-sensitive in ways that predate
    /// this branch (see `c1_controls_are_chunk_sensitive_in_vte`) -- an earlier
    /// version of this test did exactly that and failed on base-commit
    /// behaviour, which is a property shux does not have and this code never
    /// claimed.
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

/// A `RIS` earlier in the same read must not swallow an APC that starts after it.
///
/// The scanner consumes a whole PTY read before vte sees any of it, so by the
/// time `ESC c` executes mid-chunk the scanner's state already reflects the END
/// of that chunk. Resetting it there could only ever discard state belonging to
/// bytes *after* the reset -- never anything before it, because an APC cannot
/// span a `RIS`: the ESC that introduces `ESC c` terminates the string sequence
/// first. So the reset was pure loss, and this pins it.
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
/// Pinned rather than fixed here. Fixing it means changing how shux drives vte's
/// UTF-8 decoding, on the hottest path in the multiplexer, which does not belong
/// in a change about APC sequences -- but it must not be lost, and if a vte
/// upgrade ever changes this, this test is what will say so.
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
