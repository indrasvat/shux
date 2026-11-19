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

/// The raw bytes a command wrote, between its start and its end.
fn output_of(vt: &VirtualTerminal, c: &CommandRecord) -> (Vec<u8>, bool) {
    let out = vt
        .command_output(c.started)
        .unwrap_or_else(|| panic!("no output kept for the command started at {}", c.started));
    (out.bytes.to_vec(), out.truncated)
}

/// Only what arrives between C and the end is the command's: not the typed
/// line before C, not the prompt after.
#[test]
fn a_command_keeps_what_it_wrote_and_nothing_around_it() {
    for bytewise in [false, true] {
        let mut vt = vt();
        let floor = shux_vt::program_status_as_of();
        let stream = [
            b"$ echo hi\r\n" as &[u8],
            C,
            b"hi\r\n\x1b[31mred\x1b[0m\r\n",
            &d(";0"),
            A,
            b"$ ",
        ]
        .concat();
        feed(&mut vt, &stream, bytewise);
        let ended = ended_after(&vt, floor);
        let (bytes, truncated) = output_of(&vt, &ended[0]);
        let text = String::from_utf8(bytes).unwrap();
        assert!(
            text == "hi\r\n\x1b[31mred\x1b[0m\r\n",
            "bytewise={bytewise}: {text:?}"
        );
        assert!(!text.contains("echo") && !text.contains("$ "), "{text:?}");
        assert!(!truncated);
    }
}

/// Output longer than the screen, and lines wider than it, are kept as
/// written: the capture is the stream, not the grid.
#[test]
fn output_past_the_screen_and_wider_than_it_is_kept_whole() {
    let mut vt = VirtualTerminal::new(5, 20);
    let floor = shux_vt::program_status_as_of();
    let body: String = (1..=200)
        .map(|n| format!("line {n} {}\r\n", "w".repeat(50)))
        .collect();
    vt.process_with_responses(&[C, body.as_bytes(), &d(";0")].concat());
    let ended = ended_after(&vt, floor);
    let (bytes, _) = output_of(&vt, &ended[0]);
    assert_eq!(String::from_utf8(bytes).unwrap(), body);
}

#[test]
fn each_command_keeps_its_own_output() {
    let mut vt = vt();
    let floor = shux_vt::program_status_as_of();
    vt.process_with_responses(&[C, b"one\r\n", &d(";0"), A, C, b"two\r\n", &d(";1"), A].concat());
    let ended = ended_after(&vt, floor);
    let texts: Vec<String> = ended
        .iter()
        .map(|c| String::from_utf8(output_of(&vt, c).0).unwrap())
        .collect();
    assert_eq!(texts, ["one\r\n", "two\r\n"]);
}

#[test]
fn a_command_past_the_capture_limit_is_truncated_and_says_so() {
    let mut vt = vt();
    let floor = shux_vt::program_status_as_of();
    let big = vec![b'x'; shux_vt::COMMAND_OUTPUT_LIMIT + 10];
    vt.process_with_responses(&[C, &big, &d(";0")].concat());
    let ended = ended_after(&vt, floor);
    let (bytes, truncated) = output_of(&vt, &ended[0]);
    assert_eq!(bytes.len(), shux_vt::COMMAND_OUTPUT_LIMIT);
    assert!(truncated);
}

/// Old outputs go first once the kept ones pass the budget; the newest
/// command's output is always kept.
#[test]
fn the_oldest_outputs_are_let_go_past_the_budget() {
    let mut vt = vt();
    let floor = shux_vt::program_status_as_of();
    let chunk = vec![b'y'; shux_vt::COMMAND_OUTPUT_LIMIT];
    let n = shux_vt::COMMAND_OUTPUT_BUDGET / shux_vt::COMMAND_OUTPUT_LIMIT + 2;
    for _ in 0..n {
        vt.process_with_responses(&[C, &chunk, &d(";0")].concat());
    }
    let ended = ended_after(&vt, floor);
    assert_eq!(ended.len(), n);
    assert!(
        vt.command_output(ended[0].started).is_none(),
        "the oldest output was kept past the budget"
    );
    assert!(vt.command_output(ended[n - 1].started).is_some());
}

/// While a command runs, what it has written so far is readable, but only for
/// a command that started above the floor.
#[test]
fn a_running_command_shows_its_output_so_far() {
    let mut vt = vt();
    let before = shux_vt::program_status_as_of();
    vt.process_with_responses(&[C, b"partial"].concat());
    let after = shux_vt::program_status_as_of();
    let so_far = vt
        .running_command_output(before)
        .expect("the running command's output");
    assert_eq!(so_far.bytes, b"partial");
    assert!(vt.running_command_output(after).is_none());
}

/// RIS between C and D (`reset` as the command) keeps the output so far.
#[test]
fn a_reset_inside_a_command_keeps_its_output() {
    let mut vt = vt();
    let floor = shux_vt::program_status_as_of();
    vt.process_with_responses(&[C, b"before", b"\x1bc", b"after", &d(";0")].concat());
    let ended = ended_after(&vt, floor);
    let text = String::from_utf8(output_of(&vt, &ended[0]).0).unwrap();
    assert!(
        text.contains("before") && text.contains("after"),
        "{text:?}"
    );
}

/// The shell's state follows its boundaries: the transition table, one row
/// per (state, mark).
#[test]
fn the_shell_state_follows_each_boundary() {
    use shux_vt::ShellState::*;
    let rows: [(&[&[u8]], shux_vt::ShellState); 9] = [
        (&[], Unknown),
        (&[b"\x1b]133;D;0\x07"], Unknown), // a stray end says nothing
        (&[A], AtPrompt),
        (&[A, b"\x1b]133;D;0\x07"], AtPrompt), // Enter at an empty prompt
        (&[A, C], Running),
        (&[C], Running),
        (&[A, C, b"\x1b]133;D;1\x07"], Between),
        (&[A, C, b"\x1b]133;D;1\x07", A], AtPrompt),
        (&[A, C, A], AtPrompt), // `exec bash`: a prompt ends the command
    ];
    for (marks, want) in rows {
        let mut vt = vt();
        vt.process_with_responses(&marks.concat());
        assert_eq!(vt.shell_state(), want, "{marks:?}");
        assert_eq!(vt.commands_seen(), want != Unknown, "{marks:?}");
    }
}

#[derive(Debug, Clone)]
enum Ev {
    Prompt,
    Start,
    End(Option<i32>),
    Out(Vec<u8>),
    /// A line submitted to the pane (input, not output).
    Submit,
}

/// The reference: what each event does, written out plainly.
#[derive(Default)]
struct Model {
    state: Option<&'static str>,
    prompts: u64,
    /// "at_prompt" or "before_prompt": a submitted line not yet started.
    line: Option<&'static str>,
    open: Option<Vec<u8>>,
    ended: Vec<(Option<i32>, &'static str, Vec<u8>)>,
}

impl Model {
    fn close(&mut self, code: Option<i32>, by: &'static str) {
        if let Some(out) = self.open.take() {
            self.ended.push((code, by, out));
        }
    }
    fn apply(&mut self, ev: &Ev, bytes: &[u8]) {
        if let (Some(open), Ev::Out(_)) = (self.open.as_mut(), ev) {
            open.extend_from_slice(bytes);
        }
        match ev {
            Ev::Prompt => {
                self.close(None, "prompt");
                self.state = Some("at_prompt");
                self.prompts += 1;
                self.line = (self.line == Some("before_prompt")).then_some("at_prompt");
            }
            Ev::Start => {
                self.close(None, "next_command");
                self.open = Some(Vec::new());
                self.state = Some("running");
                self.line = None;
            }
            Ev::Submit => match self.state {
                Some("at_prompt") => self.line = Some("at_prompt"),
                Some("between") => self.line = Some("before_prompt"),
                _ => {}
            },
            Ev::End(code) => {
                if self.open.is_some() {
                    self.close(*code, "end");
                    self.state = Some("between");
                }
            }
            Ev::Out(_) => {}
        }
    }
}

fn render(ev: &Ev) -> Vec<u8> {
    match ev {
        Ev::Prompt => A.to_vec(),
        Ev::Start => C.to_vec(),
        Ev::End(code) => d(&code.map_or(String::new(), |c| format!(";{c}"))),
        Ev::Out(b) => b.clone(),
        Ev::Submit => Vec::new(),
    }
}

fn ev_strategy() -> impl Strategy<Value = Ev> {
    prop_oneof![
        Just(Ev::Prompt),
        Just(Ev::Start),
        Just(Ev::Submit),
        prop::option::of(-2i32..300).prop_map(Ev::End),
        // Output never holds ESC: an escape could open a string that swallows
        // the next boundary, which the chunking proptest covers separately.
        prop::collection::vec(
            prop_oneof![0x20u8..0x7f, Just(b'\r'), Just(b'\n'), 0x80u8..0xff],
            0..40
        )
        .prop_map(Ev::Out),
    ]
}

proptest! {
    /// The terminal agrees with the reference on every observable, however
    /// the stream is cut into reads.
    #[test]
    fn boundaries_and_output_match_the_reference(
        events in prop::collection::vec(ev_strategy(), 0..40),
        cuts in prop::collection::vec(1usize..17, 1..30),
    ) {
        let mut model = Model::default();
        let mut vt = vt();
        let floor = shux_vt::program_status_as_of();
        // Output between two submissions is cut into reads; a submission is
        // input, so it lands between reads, never inside one.
        let mut cut = cuts.iter().cycle();
        let mut out = Vec::new();
        let flush = |vt: &mut VirtualTerminal, out: &mut Vec<u8>, cut: &mut dyn Iterator<Item = &usize>| {
            let mut at = 0;
            while at < out.len() {
                let end = (at + cut.next().unwrap()).min(out.len());
                vt.process_with_responses(&out[at..end]);
                at = end;
            }
            out.clear();
        };
        for ev in &events {
            let bytes = render(ev);
            model.apply(ev, &bytes);
            if let Ev::Submit = ev {
                flush(&mut vt, &mut out, &mut cut);
                vt.note_line_submitted();
            } else {
                out.extend_from_slice(&bytes);
            }
        }
        flush(&mut vt, &mut out, &mut cut);
        prop_assert_eq!(vt.shell_state().as_str(), model.state.unwrap_or("unknown"));
        prop_assert_eq!(vt.prompts_seen(), model.prompts);
        prop_assert_eq!(
            vt.line_pending(),
            model.line == Some("at_prompt") && model.state == Some("at_prompt")
        );
        let ended = ended_after(&vt, floor);
        let got: Vec<_> = ended
            .iter()
            .map(|c| (c.exit_code, c.ended_by.as_str(), output_of(&vt, c).0))
            .collect();
        prop_assert_eq!(got, model.ended.clone());
        let running = vt.running_command_output(floor).map(|o| o.bytes.to_vec());
        prop_assert_eq!(running, model.open.clone());
    }
}

/// The sequence that ends a command is not its output, however it was split
/// across reads: exactly the limit is not truncated.
#[test]
fn the_ending_sequence_is_never_output_or_counted() {
    for ending in [d(";0"), A.to_vec(), C.to_vec()] {
        let body = vec![b'z'; shux_vt::COMMAND_OUTPUT_LIMIT];
        let stream = [C, &body, &ending].concat();
        for split in [None, Some(stream.len() - 3)] {
            let mut vt = vt();
            let floor = shux_vt::program_status_as_of();
            match split {
                None => {
                    vt.process_with_responses(&stream);
                }
                Some(at) => {
                    vt.process_with_responses(&stream[..at]);
                    vt.process_with_responses(&stream[at..]);
                }
            }
            let ended = ended_after(&vt, floor);
            let (bytes, truncated) = output_of(&vt, &ended[0]);
            assert_eq!(bytes, body, "ending={ending:?} split={split:?}");
            assert!(!truncated, "ending={ending:?} split={split:?}");
        }
    }
}
