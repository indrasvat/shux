//! OSC 7501 program status as seen through `VirtualTerminal`.
//!
//! The generated properties compare a terminal with program status enabled
//! against one with it disabled, on the SAME chunks. Enabling it slices
//! `advance` at every 7501 / 133;A cut, so this is also the neutrality proof
//! for that slicing: nothing a pane can observe may differ except the 7501
//! probe replies themselves.

use proptest::prelude::*;
use shux_program_status::State;
use shux_vt::{FrameEnvelope, MaskSet, VirtualTerminal};

const DA1: &[u8] = b"\x1b[?62;1;2;6;9;15;22c";
const REPLY_ST: &[u8] =
    b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1b\\";
const REPLY_BEL: &[u8] =
    b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x07";

#[derive(Debug, PartialEq, Eq)]
struct Observable {
    frame: String,
    scrollback: String,
    cursor: String,
    title: Option<String>,
    content_revision: u64,
    responses: Vec<Vec<u8>>,
}

fn drive(chunks: &[&[u8]], enabled: bool) -> (Observable, VirtualTerminal) {
    let mut vt = VirtualTerminal::new(24, 80);
    vt.set_program_status_enabled(enabled);
    let mut responses = Vec::new();
    for chunk in chunks {
        responses.extend(vt.process_with_responses(chunk));
    }
    let obs = Observable {
        frame: FrameEnvelope::from_terminal(&vt, &MaskSet::new()).to_canonical_json(),
        scrollback: vt.capture_text(None),
        cursor: format!("{:?}", vt.cursor()),
        title: vt.title().map(str::to_owned),
        content_revision: vt.content_revision(),
        responses,
    };
    (obs, vt)
}

fn enabled(bytes: &[u8]) -> VirtualTerminal {
    drive(&[bytes], true).1
}

fn top_state(vt: &VirtualTerminal) -> Option<State> {
    vt.program_status_publication().summary.map(|s| s.state)
}

/// Raw bytes and whole protocol fragments, so generated streams reach both
/// the framing seams and the table.
fn piece() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        8 => prop::collection::vec(prop_oneof![
            4 => 0x20u8..0x7f,
            2 => Just(0x1b), 1 => Just(0x18), 1 => Just(0x1a), 1 => Just(0x07),
            1 => Just(b']'), 1 => Just(b'['), 1 => Just(b'_'), 1 => Just(b'P'),
            1 => Just(b'\\'), 1 => Just(b'\n'), 1 => Just(0x00),
            2 => 0xc2u8..=0xf4, 2 => 0x80u8..=0xbf, 1 => Just(0x9c),
        ], 1..12),
        2 => prop::sample::select(vec![
            &b"\x1b]7501;?\x1b\\"[..], b"\x1b]7501;?\x07",
            b"\x1b]7501;state=working:progress=40\x1b\\", b"\x1b]7501;state=blocked:kind=auth\x07",
            b"\x1b]7501;state=done:id=a/b\x1b\\", b"\x1b]7501;state=clear\x07",
            b"\x1b]133;A\x07", b"\x1bc", b"\x1b[!p", b"\x1b[c", b"\x1b[?2026h", b"\x1b[?2026l",
            b"\x1b[?1049h", b"\x1b[?1049l", b"\x1b]0;title\x07", b"\x1b[31mred\x1b[0m",
            b"\x1b]7501;", b"\x1b]133;", b"state=idle", b"\x1b_Gx\x1b\\",
        ]).prop_map(<[u8]>::to_vec),
    ]
}

fn strip_status_replies(responses: &[Vec<u8>]) -> Vec<Vec<u8>> {
    responses
        .iter()
        .filter(|r| r.as_slice() != REPLY_ST && r.as_slice() != REPLY_BEL)
        .cloned()
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(600))]

    /// Enabling program status changes nothing a pane can observe except the
    /// probe replies, however the stream is chunked.
    #[test]
    fn status_is_invisible_to_everything_else(
        pieces in prop::collection::vec(piece(), 0..40),
        split_a in 0usize..600,
        split_b in 0usize..600,
    ) {
        let stream = pieces.concat();
        let (mut i, mut j) = (split_a.min(stream.len()), split_b.min(stream.len()));
        if i > j {
            std::mem::swap(&mut i, &mut j);
        }
        let chunks: [&[u8]; 3] = [&stream[..i], &stream[i..j], &stream[j..]];
        let (mut on, _) = drive(&chunks, true);
        let (off, _) = drive(&chunks, false);
        on.responses = strip_status_replies(&on.responses);
        prop_assert_eq!(on, off);
    }

    /// The table sees the same reports whether the stream arrives whole or a
    /// byte at a time.
    #[test]
    fn records_do_not_depend_on_chunking(pieces in prop::collection::vec(piece(), 0..30)) {
        let stream = pieces.concat();
        let whole = enabled(&stream);
        let singles: Vec<&[u8]> = stream.chunks(1).collect();
        let (_, bytewise) = drive(&singles, true);
        let records = |vt: &VirtualTerminal| vt.program_status_records().map(|r| (r.seen, r.records));
        prop_assert_eq!(records(&whole), records(&bytewise));
    }

    /// Every query is answered at its own stream position, so replies come
    /// back in exactly the order their queries were written, at any split.
    #[test]
    fn replies_keep_stream_order(
        tokens in prop::collection::vec(0u8..4, 1..12),
        split_a in 0usize..300,
        split_b in 0usize..300,
    ) {
        let mut stream = Vec::new();
        let mut expected = Vec::new();
        for t in &tokens {
            match t {
                0 => { stream.extend_from_slice(b"\x1b]7501;?\x1b\\"); expected.push(REPLY_ST.to_vec()); }
                1 => { stream.extend_from_slice(b"\x1b]7501;?\x07"); expected.push(REPLY_BEL.to_vec()); }
                2 => { stream.extend_from_slice(b"\x1b[c"); expected.push(DA1.to_vec()); }
                _ => stream.extend_from_slice(b"text\x1b]7501;state=idle\x07"),
            }
        }
        let (mut i, mut j) = (split_a.min(stream.len()), split_b.min(stream.len()));
        if i > j {
            std::mem::swap(&mut i, &mut j);
        }
        let (obs, _) = drive(&[&stream[..i], &stream[i..j], &stream[j..]], true);
        prop_assert_eq!(obs.responses, expected);
    }
}

#[test]
fn disabled_answers_only_the_da1() {
    let (obs, vt) = drive(
        &[b"\x1b]7501;?\x1b\\\x1b[c\x1b]7501;state=working\x07"],
        false,
    );
    assert_eq!(obs.responses, vec![DA1.to_vec()]);
    assert!(vt.program_status_records().is_none());
    assert_eq!(vt.program_status_publication().summary, None);
}

#[test]
fn reports_update_the_table_without_touching_the_frame() {
    let mut vt = VirtualTerminal::new(24, 80);
    vt.set_program_status_enabled(true);
    let rev = vt.content_revision();
    let g0 = vt.program_status_publication().generation;
    vt.process(b"\x1b]7501;state=blocked:kind=permission:app=claude-code\x1b\\");
    assert_eq!(vt.content_revision(), rev, "status is not frame content");
    let p = vt.program_status_publication();
    assert!(p.generation > g0 && p.seen);
    assert_eq!(p.summary.unwrap().app.as_deref(), Some("claude-code"));
}

#[test]
fn aborted_reports_and_prompts_never_apply() {
    for stream in [
        &b"\x1b]7501;state=working\x1b]0;t\x1b\\"[..],
        b"\x1b]7501;state=working\x1b_Gx\x1b\\",
        b"\x1b]7501;state=working\x1b[\x1b\\",
        b"\x1b]7501;state=working\x18",
    ] {
        assert_eq!(top_state(&enabled(stream)), None, "{stream:?}");
    }
    let vt = enabled(b"\x1b]7501;state=working\x07\x1b]133;A\x1b[\x1b\\");
    assert_eq!(
        top_state(&vt),
        Some(State::Working),
        "an aborted 133;A drops nothing"
    );
}

#[test]
fn prompt_start_drops_live_records_and_keeps_finished_ones() {
    let vt = enabled(
        b"\x1b]7501;state=working:id=w\x07\x1b]7501;state=done:id=d\x07\x1b]7501;state=idle\x07\x1b]133;A\x07",
    );
    let left: Vec<_> = vt
        .program_status_records()
        .unwrap()
        .records
        .into_iter()
        .map(|r| r.state)
        .collect();
    assert_eq!(left, vec![State::Done]);
}

#[test]
fn ris_clears_records_and_seen_but_decstr_does_not() {
    let vt = enabled(b"\x1b]7501;state=done\x07\x1b[!p");
    assert_eq!(top_state(&vt), Some(State::Done), "DECSTR keeps records");
    let vt = enabled(b"\x1b]7501;state=done\x07\x1bc");
    let p = vt.program_status_publication();
    assert_eq!((p.summary, p.seen), (None, false));
}

#[test]
fn status_stays_live_under_synchronized_output_and_the_alt_screen() {
    let mut vt = VirtualTerminal::new(24, 80);
    vt.set_program_status_enabled(true);
    vt.process(b"\x1b[?1049h\x1b[?2026h\x1b]0;frozen\x07\x1b]7501;state=blocked\x07");
    assert_eq!(
        top_state(&vt),
        Some(State::Blocked),
        "not held behind DEC 2026"
    );
    vt.process(b"\x1b[?2026l\x1b[?1049l");
    assert_eq!(
        top_state(&vt),
        Some(State::Blocked),
        "records belong to the terminal"
    );
}

#[test]
fn process_exit_ends_live_records() {
    let mut vt = enabled(b"\x1b]7501;state=working\x07\x1b]7501;state=error:id=e\x07");
    vt.note_process_exit();
    assert_eq!(top_state(&vt), Some(State::Error));
}

#[test]
fn toggling_always_moves_the_generation_forward() {
    let mut vt = enabled(b"\x1b]7501;state=working\x07");
    let g1 = vt.program_status_publication().generation;
    vt.set_program_status_enabled(false);
    let off = vt.program_status_publication();
    assert!(off.generation > g1 && off.summary.is_none());
    vt.set_program_status_enabled(true);
    let on = vt.program_status_publication();
    assert!(
        on.generation > off.generation && on.summary.is_none(),
        "re-enabling starts empty"
    );
    let fresh = VirtualTerminal::new(24, 80).program_status_publication();
    assert!(
        fresh.generation > on.generation,
        "a replacement terminal outranks the old one"
    );
}

/// One `process()` call over output dense with OSC strings (hyperlinks) must
/// cost linear time, status on or off: the scanner runs either way.
#[test]
fn osc_dense_output_scans_in_linear_time() {
    fn cost_ns(repeats: usize) -> u128 {
        let chunk = b"\x1b]8;;f\x07x\x1b]8;;\x07 ".repeat(repeats);
        let mut vt = VirtualTerminal::new(24, 80);
        let start = std::time::Instant::now();
        vt.process(&chunk);
        start.elapsed().as_nanos()
    }
    let (small, large) = (2_000usize, 16_000usize);
    cost_ns(small);
    cost_ns(large);
    let (mut s, mut l) = (u128::MAX, u128::MAX);
    for _ in 0..5 {
        s = s.min(cost_ns(small));
        l = l.min(cost_ns(large));
    }
    let ratio = l as f64 / s as f64;
    let growth = (large / small) as f64;
    eprintln!(
        "OSC-dense scan grows {ratio:.1}x for {growth:.0}x input (linear ~{growth:.0}x, quadratic ~{:.0}x)",
        growth * growth
    );
    assert!(
        ratio < growth * 3.0,
        "scan cost grew {ratio:.1}x for {growth:.0}x input: quadratic, not linear"
    );
}

/// Re-asserting "off" draws a fresh generation, so a terminal made off before
/// an await still outranks anything its predecessor published meanwhile.
#[test]
fn reasserting_off_moves_the_generation_forward() {
    let mut vt = VirtualTerminal::new(4, 20);
    let before = vt.program_status_publication().generation;
    let mut other = VirtualTerminal::new(4, 20);
    other.set_program_status_enabled(true); // the predecessor publishes meanwhile
    let theirs = other.program_status_publication().generation;
    vt.set_program_status_enabled(false);
    let after = vt.program_status_publication().generation;
    assert!(theirs > before);
    assert!(
        after > theirs,
        "an already-off terminal kept its old generation"
    );
}
