//! OSC 133 command boundaries, driven through the terminal's byte stream.

use proptest::prelude::*;
use shux_vt::{CommandLookup, CommandRecord, EndedBy, VirtualTerminal};

const C: &[u8] = b"\x1b]133;C\x07";
const A: &[u8] = b"\x1b]133;A\x1b\\";

fn d(exit: &str) -> Vec<u8> {
    format!("\x1b]133;D{exit}\x07").into_bytes()
}

fn vt() -> VirtualTerminal {
    VirtualTerminal::new(24, 80)
}

/// Every ended command after `floor`, in order, by chaining.
fn ended_after(vt: &VirtualTerminal, mut floor: u64) -> Vec<CommandRecord> {
    let mut out = Vec::new();
    while let CommandLookup::Found(c) = vt.command_after(floor) {
        floor = c.started;
        out.push(c);
    }
    out
}

/// The whole stream in one read, or a byte per read.
fn feed(vt: &mut VirtualTerminal, stream: &[u8], bytewise: bool) {
    if bytewise {
        for b in stream {
            vt.process_with_responses(std::slice::from_ref(b));
        }
    } else {
        vt.process_with_responses(stream);
    }
}

fn exits(records: &[CommandRecord]) -> Vec<Option<i32>> {
    records.iter().map(|c| c.exit_code).collect()
}

#[test]
fn a_command_ends_with_its_exit_code_whatever_the_terminator() {
    let mut vt = vt();
    let floor = shux_vt::program_status_as_of();
    let stream = [
        C,
        b"output\r\n",
        &d(";0"),
        C,
        &d(";1;aid=7"),
        C,
        &d(""),
        C,
        &d(";"),
        C,
        &d(";not-a-number"),
        b"\x1b]133;C;cmdline=make\x1b\\",
        b"\x1b]133;D;130\x1b\\",
    ]
    .concat();
    vt.process_with_responses(&stream);
    assert!(vt.commands_seen());
    assert_eq!(
        exits(&ended_after(&vt, floor)),
        [Some(0), Some(1), None, None, None, Some(130)],
        "a missing or malformed exit code must read as unknown, never 0"
    );
}

#[test]
fn boundaries_count_with_program_status_off_and_on() {
    for enabled in [false, true] {
        let mut vt = vt();
        vt.set_program_status_enabled(enabled);
        let floor = shux_vt::program_status_as_of();
        vt.process_with_responses(&[C, &d(";4")].concat());
        assert_eq!(
            exits(&ended_after(&vt, floor)),
            [Some(4)],
            "enabled={enabled}"
        );
    }
}

/// `reset` run as a command writes RIS between its own C and D.
#[test]
fn a_reset_inside_a_command_does_not_lose_it() {
    let mut vt = vt();
    vt.set_program_status_enabled(true);
    let floor = shux_vt::program_status_as_of();
    vt.process_with_responses(&[C, b"\x1bc", &d(";0")].concat());
    assert_eq!(exits(&ended_after(&vt, floor)), [Some(0)]);
    assert!(
        vt.commands_seen(),
        "RIS forgot that the shell sends boundaries"
    );
}

#[test]
fn a_prompt_ends_a_command_that_never_reported_and_a_stray_end_is_ignored() {
    let mut vt = vt();
    let floor = shux_vt::program_status_as_of();
    vt.process_with_responses(&[&d(";0")[..], C, A, A].concat());
    let ended = ended_after(&vt, floor);
    assert_eq!(ended.len(), 1);
    assert_eq!(
        (ended[0].exit_code, ended[0].ended_by),
        (None, EndedBy::Prompt)
    );
    assert!(!vt.command_running());
}

/// kitty and fish attach the command line to C; past the sequence limit the
/// rest is dropped, but the start still counts.
#[test]
fn a_start_with_a_long_command_line_still_starts_a_command() {
    let mut vt = vt();
    let floor = shux_vt::program_status_as_of();
    let long = format!("\x1b]133;C;cmdline_url={}\x07", "x".repeat(6000));
    vt.process_with_responses(&[long.as_bytes(), &d(";2")].concat());
    assert_eq!(exits(&ended_after(&vt, floor)), [Some(2)]);
}

#[test]
fn near_misses_are_not_boundaries() {
    let mut vt = vt();
    let floor = shux_vt::program_status_as_of();
    vt.process_with_responses(b"\x1b]133;CD\x07\x1b]133;Dx;1\x07\x1b]133;B\x07\x1b]1330;C\x07");
    assert!(!vt.commands_seen());
    assert_eq!(vt.command_after(floor), CommandLookup::Pending);
}

proptest! {
    /// The same stream gives the same commands however it is cut into reads.
    #[test]
    fn boundaries_do_not_depend_on_chunking(
        codes in prop::collection::vec(prop::option::of(-3i32..300), 1..6),
        cuts in prop::collection::vec(1usize..9, 1..40),
    ) {
        let mut stream = Vec::new();
        for code in &codes {
            stream.extend_from_slice(C);
            stream.extend_from_slice(b"\x1b[31mout\x1b[0m\r\n");
            stream.extend_from_slice(&d(&code.map_or(String::new(), |c| format!(";{c}"))));
            stream.extend_from_slice(A);
        }
        let mut vt = vt();
        let floor = shux_vt::program_status_as_of();
        let mut at = 0;
        for n in cuts.iter().cycle() {
            if at >= stream.len() {
                break;
            }
            let end = (at + n).min(stream.len());
            vt.process_with_responses(&stream[at..end]);
            at = end;
        }
        prop_assert_eq!(exits(&ended_after(&vt, floor)), codes);
    }
}

/// Past the sequence limit only a short prefix is kept; whether it is kept
/// must not depend on where the reads were cut.
#[test]
fn a_long_sequence_is_classified_the_same_whole_or_bytewise() {
    let pad = vec![0u8; 5000]; // C0 NUL: on the wire, never in the body
    let malformed = [b"\x1b]133;C" as &[u8], &pad, b"X\x07", &d(";7")].concat();
    let late_mark = [b"\x1b]133;" as &[u8], &pad, b"C\x07", &d(";7")].concat();
    for (name, stream, want) in [
        ("133;C…X is not a start", malformed, vec![]),
        ("133;…C is a start", late_mark, vec![Some(7)]),
    ] {
        for bytewise in [false, true] {
            let mut vt = vt();
            let floor = shux_vt::program_status_as_of();
            feed(&mut vt, &stream, bytewise);
            assert_eq!(
                exits(&ended_after(&vt, floor)),
                want,
                "{name}, bytewise={bytewise}"
            );
        }
    }
}

/// An end with long parameters still carries its own exit code.
#[test]
fn a_long_end_keeps_its_own_exit_code() {
    let long_d = format!("\x1b]133;D;7;err={}\x07", "y".repeat(5000));
    for bytewise in [false, true] {
        let mut vt = vt();
        let floor = shux_vt::program_status_as_of();
        let stream = [C, long_d.as_bytes()].concat();
        feed(&mut vt, &stream, bytewise);
        let ended = ended_after(&vt, floor);
        assert_eq!(exits(&ended), [Some(7)], "bytewise={bytewise}");
        assert_eq!(ended[0].ended_by, EndedBy::End, "bytewise={bytewise}");
    }
}

/// Ignored C0 bytes before the mark: vte stores none of them, so the start
/// counts, read whole or a byte at a time.
#[test]
fn a_start_after_ignored_control_bytes_counts_however_it_is_read() {
    let stream = [
        b"\x1b]" as &[u8],
        &[0u8; 5000],
        b"133;C;cmdline=ls\x07",
        &d(";1"),
    ]
    .concat();
    for bytewise in [false, true] {
        let mut vt = vt();
        let floor = shux_vt::program_status_as_of();
        feed(&mut vt, &stream, bytewise);
        assert_eq!(
            exits(&ended_after(&vt, floor)),
            [Some(1)],
            "bytewise={bytewise}"
        );
    }
}

/// Ignored C0 bytes inside an end: vte stores none of them, so the exit
/// code it carries is read, whole or a byte at a time.
#[test]
fn an_end_padded_with_ignored_control_bytes_keeps_its_exit_code() {
    let stream = [C, b"\x1b]133;D;7", &[0u8; 5000], b"\x07"].concat();
    for bytewise in [false, true] {
        let mut vt = vt();
        let floor = shux_vt::program_status_as_of();
        feed(&mut vt, &stream, bytewise);
        assert_eq!(
            exits(&ended_after(&vt, floor)),
            [Some(7)],
            "bytewise={bytewise}"
        );
    }
}

/// An exit code is a decimal number, optionally negative: nothing else.
#[test]
fn only_a_plain_decimal_is_an_exit_code() {
    let mut vt = vt();
    let floor = shux_vt::program_status_as_of();
    for exit in [";+5", ";-5", ";05", "; 5", ";0x5", ";-"] {
        vt.process_with_responses(&[C, &d(exit)].concat());
    }
    assert_eq!(
        exits(&ended_after(&vt, floor)),
        [None, Some(-5), Some(5), None, None, None]
    );
}
