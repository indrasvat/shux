//! OSC 7501 program status inside the VT: raw-byte framing, the per-terminal
//! record table, and the generation stamped on every change.
//!
//! Framing is decided here from raw bytes, not from vte callbacks. vte ends
//! an OSC on any ESC and moves on with no callback for `ESC [`, `ESC ]`,
//! `ESC P`, `ESC X`, `ESC ^` and `ESC _`, so `Perform` cannot tell `ESC \`
//! from an aborted string; it also drops C0 bytes inside an OSC without
//! counting them, so its buffer length is not the wire length. The scanner
//! mirrors vte's string grammar, and its cuts are fed through the same
//! slicing as APC cuts (see `graphics::apc` for why slicing is neutral).

use std::sync::atomic::{AtomicU64, Ordering};

use shux_program_status::limits::MAX_SEQUENCE_BYTES;
use shux_program_status::wire::{self, Message};
use shux_program_status::{RecordView, StatusSummary, StatusTable};

/// Process-global, so a publication from a replaced terminal or from before
/// a disable is always older than anything that follows it.
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn next_generation() -> u64 {
    GENERATION.fetch_add(1, Ordering::Relaxed) + 1
}

/// A fresh generation for a terminal whose program status is off.
pub(crate) fn disabled_generation() -> u64 {
    next_generation()
}

/// What the daemon forwards to the graph. Carries no program-authored text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusPublication {
    pub generation: u64,
    pub summary: Option<StatusSummary>,
    /// The terminal has received at least one report since the last RIS.
    pub seen: bool,
    /// Program status is on for this terminal. Lets a reader tell "disabled"
    /// from "enabled, nothing reported".
    pub enabled: bool,
}

/// Records plus the generation they were read at, for `pane.status`.
#[derive(Debug, Clone)]
pub struct StatusRecords {
    pub generation: u64,
    pub seen: bool,
    pub records: Vec<RecordView>,
}

/// Status state for one enabled terminal.
#[derive(Debug, Default)]
pub(crate) struct VtProgramStatus {
    table: StatusTable,
    seen: bool,
    generation: u64,
}

impl VtProgramStatus {
    pub(crate) fn new() -> Self {
        Self {
            generation: next_generation(),
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn publication(&self) -> StatusPublication {
        StatusPublication {
            generation: self.generation,
            summary: self.table.summary(),
            seen: self.seen,
            enabled: true,
        }
    }

    pub(crate) fn records(&self) -> StatusRecords {
        StatusRecords {
            generation: self.generation,
            seen: self.seen,
            records: self.table.records(),
        }
    }

    /// Runs `f` against the table and moves the generation if anything
    /// observable changed.
    fn change(&mut self, f: impl FnOnce(&mut StatusTable, &mut bool)) {
        let (rev, seen) = (self.table.revision(), self.seen);
        f(&mut self.table, &mut self.seen);
        if self.table.revision() != rev || self.seen != seen {
            self.generation = next_generation();
        }
    }

    /// A finished OSC 7501 payload. Returns the reply to queue, if any.
    pub(crate) fn on_report(&mut self, payload: &[u8], terminator: &[u8]) -> Option<Vec<u8>> {
        match wire::decode(payload) {
            Ok(Message::Query) => Some(wire::query_reply(terminator)),
            Ok(Message::Report(report)) => {
                self.change(|t, seen| {
                    t.apply(report);
                    *seen = true;
                });
                None
            }
            Err(reject) => {
                tracing::trace!(?reject, "OSC 7501 report discarded");
                None
            }
        }
    }

    pub(crate) fn on_prompt_start(&mut self) {
        self.change(|t, _| t.on_prompt_start());
    }

    pub(crate) fn on_process_exit(&mut self) {
        self.change(|t, _| t.on_process_exit());
    }

    pub(crate) fn on_full_reset(&mut self) {
        self.change(|t, seen| {
            t.on_full_reset();
            *seen = false;
        });
    }
}

/// A finished OSC the status code acts on, located at `end` (one past its
/// terminator) within the scanned chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusCut {
    pub(crate) end: usize,
    pub(crate) action: CutAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CutAction {
    /// Payload after `7501;`, and the terminator to mirror in a reply.
    Report { payload: Vec<u8>, bel: bool },
    /// OSC 133 `A`: a new shell prompt.
    PromptStart,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum ScanState {
    #[default]
    Ground,
    /// vte is in Escape: the previous byte was ESC, possibly followed by
    /// bytes vte executes or ignores without leaving Escape.
    Esc,
    Osc,
    /// Inside an OSC, previous byte was ESC.
    OscEsc,
}

/// Locates OSC 7501 and OSC 133;A sequences across arbitrarily-chunked
/// reads, following vte 0.15's string grammar exactly.
#[derive(Debug, Default)]
pub(crate) struct OscStatusScanner {
    state: ScanState,
    /// The OSC body as vte would retain it: ignored C0 bytes are not stored.
    body: Vec<u8>,
    /// Wire length of the current sequence, from `ESC ]` on.
    raw_len: usize,
    /// Past [`MAX_SEQUENCE_BYTES`]: tracked to its end, never cut.
    poisoned: bool,
}

/// Bytes vte executes or ignores while staying in Escape
/// (vte-0.15 `advance_esc`), shared with `graphics::apc`.
fn stays_in_escape(b: u8) -> bool {
    matches!(b, 0x00..=0x17 | 0x19 | 0x1b..=0x1f | 0x7f..=0xff)
}

impl OscStatusScanner {
    /// Every relevant OSC that *completes* within `chunk`, in stream order.
    pub(crate) fn scan(&mut self, chunk: &[u8]) -> Vec<StatusCut> {
        if self.state == ScanState::Ground && memchr::memchr(0x1b, chunk).is_none() {
            return Vec::new();
        }
        let mut cuts = Vec::new();
        let mut i = 0;
        while i < chunk.len() {
            match self.state {
                ScanState::Ground => match memchr::memchr(0x1b, &chunk[i..]) {
                    Some(off) => {
                        self.state = ScanState::Esc;
                        i += off + 1;
                    }
                    None => break,
                },
                ScanState::Esc => {
                    let b = chunk[i];
                    i += 1;
                    match b {
                        b']' => self.begin(),
                        b if stays_in_escape(b) => {}
                        _ => self.state = ScanState::Ground,
                    }
                }
                ScanState::Osc => {
                    // Only BEL, CAN, SUB and ESC leave an OSC string. CAN/SUB
                    // are sought only before the first BEL/ESC, so each byte
                    // is examined at most twice per chunk.
                    let rest = &chunk[i..];
                    let stop = memchr::memchr2(0x07, 0x1b, rest);
                    let window = &rest[..stop.unwrap_or(rest.len())];
                    match memchr::memchr2(0x18, 0x1a, window).or(stop) {
                        Some(off) => {
                            self.take_body(&chunk[i..i + off]);
                            let b = chunk[i + off];
                            i += off + 1;
                            self.raw_len = self.raw_len.saturating_add(1);
                            match b {
                                0x07 => self.finish(i, true, &mut cuts),
                                0x1b => self.state = ScanState::OscEsc,
                                // CAN / SUB abort the string.
                                _ => self.reset(ScanState::Ground),
                            }
                        }
                        None => {
                            self.take_body(&chunk[i..]);
                            break;
                        }
                    }
                }
                ScanState::OscEsc => {
                    if chunk[i] == b'\\' {
                        i += 1;
                        self.raw_len = self.raw_len.saturating_add(1);
                        self.finish(i, false, &mut cuts);
                    } else {
                        // Not ST: the OSC ended malformed and vte is now in
                        // Escape with this byte still to come.
                        self.reset(ScanState::Esc);
                    }
                }
            }
        }
        cuts
    }

    fn begin(&mut self) {
        self.reset(ScanState::Osc);
        self.raw_len = 2;
    }

    fn reset(&mut self, state: ScanState) {
        self.state = state;
        self.body.clear();
        self.raw_len = 0;
        self.poisoned = false;
    }

    /// Payload bytes between terminators. vte drops these C0 bytes without
    /// storing them (`advance_osc_string`); they still count on the wire.
    fn take_body(&mut self, span: &[u8]) {
        self.raw_len = self.raw_len.saturating_add(span.len());
        if self.poisoned || self.raw_len > MAX_SEQUENCE_BYTES {
            self.poisoned = true;
            self.body.clear();
            return;
        }
        self.body.extend(
            span.iter()
                .copied()
                .filter(|&b| !matches!(b, 0x00..=0x06 | 0x08..=0x17 | 0x19 | 0x1c..=0x1f)),
        );
    }

    fn finish(&mut self, end: usize, bel: bool, cuts: &mut Vec<StatusCut>) {
        if !self.poisoned
            && self.raw_len <= MAX_SEQUENCE_BYTES
            && let Some(action) = classify(&self.body, bel)
        {
            cuts.push(StatusCut { end, action });
        }
        self.reset(ScanState::Ground);
    }
}

fn classify(body: &[u8], bel: bool) -> Option<CutAction> {
    if let Some(payload) = body
        .strip_prefix(wire::OSC_NUMBER)
        .and_then(|r| r.strip_prefix(b";"))
    {
        return Some(CutAction::Report {
            payload: payload.to_vec(),
            bel,
        });
    }
    let rest = body.strip_prefix(b"133;A")?;
    (rest.is_empty() || rest.starts_with(b";")).then_some(CutAction::PromptStart)
}

/// The scanner and its vte reference, for the differential tests and the
/// libFuzzer targets (`fuzz/`). Not part of the API.
#[cfg(any(test, fuzzing))]
#[doc(hidden)]
pub mod fuzzing {
    use super::*;

    /// Finished OSCs the scanner cuts from `chunks`, as debug strings.
    pub fn scan(chunks: &[&[u8]]) -> Vec<String> {
        let mut s = OscStatusScanner::default();
        chunks
            .iter()
            .flat_map(|c| s.scan(c))
            .map(|c| format!("{:?}", c.action))
            .collect()
    }

    /// The same, decided by vte itself fed one byte at a time.
    pub fn reference_strings(bytes: &[u8]) -> Vec<String> {
        reference(bytes)
            .into_iter()
            .map(|a| format!("{a:?}"))
            .collect()
    }

    // The byte that ended each OSC is known exactly: BEL, or ESC whose very
    // next byte is `\` (seen as `esc_dispatch('\')` with no callback in
    // between). Anything else ended the string malformed.
    #[derive(Default)]
    struct Recorder {
        finished: Vec<CutAction>,
        /// An OSC vte ended on ESC, waiting to see whether ST follows.
        pending: Option<Vec<u8>>,
        /// Set by any callback during the current byte.
        other: bool,
        dispatched: Option<(Vec<u8>, bool)>,
        st: bool,
    }

    impl vte::Perform for Recorder {
        fn print(&mut self, _: char) {
            self.other = true;
        }
        fn execute(&mut self, _: u8) {
            self.other = true;
        }
        fn hook(&mut self, _: &vte::Params, _: &[u8], _: bool, _: char) {
            self.other = true;
        }
        fn put(&mut self, _: u8) {
            self.other = true;
        }
        fn unhook(&mut self) {
            self.other = true;
        }
        fn osc_dispatch(&mut self, params: &[&[u8]], bell: bool) {
            self.dispatched = Some((params.join(&b';'), bell));
        }
        fn csi_dispatch(&mut self, _: &vte::Params, _: &[u8], _: bool, _: char) {
            self.other = true;
        }
        fn esc_dispatch(&mut self, intermediates: &[u8], _: bool, byte: u8) {
            if byte == b'\\' && intermediates.is_empty() {
                self.st = true;
            } else {
                self.other = true;
            }
        }
    }

    pub(crate) fn reference(bytes: &[u8]) -> Vec<CutAction> {
        let mut parser = crate::parser::VtParser::new_with_size();
        let mut r = Recorder::default();
        for &b in bytes {
            r.other = false;
            r.st = false;
            r.dispatched = None;
            parser.advance(&mut r, &[b]);
            if let Some(body) = r.pending.take()
                && r.st
                && !r.other
                && let Some(a) = classify(&body, false)
            {
                r.finished.push(a);
            }
            if let Some((body, bell)) = r.dispatched.take() {
                if bell {
                    if let Some(a) = classify(&body, true) {
                        r.finished.push(a);
                    }
                } else if b == 0x1b {
                    r.pending = Some(body);
                }
            }
        }
        r.finished
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cuts(chunks: &[&[u8]]) -> Vec<CutAction> {
        let mut s = OscStatusScanner::default();
        chunks
            .iter()
            .flat_map(|c| s.scan(c))
            .map(|c| c.action)
            .collect()
    }

    fn report(p: &[u8], bel: bool) -> CutAction {
        CutAction::Report {
            payload: p.to_vec(),
            bel,
        }
    }

    #[test]
    fn st_and_bel_both_finish_and_are_remembered() {
        assert_eq!(
            cuts(&[b"\x1b]7501;state=idle\x1b\\"]),
            [report(b"state=idle", false)]
        );
        assert_eq!(cuts(&[b"\x1b]7501;?\x07"]), [report(b"?", true)]);
    }

    #[test]
    fn cut_lands_one_past_the_terminator() {
        let mut s = OscStatusScanner::default();
        let bytes = b"ab\x1b]7501;?\x1b\\cd";
        assert_eq!(s.scan(bytes)[0].end, bytes.len() - 2);
    }

    #[test]
    fn every_interruption_aborts() {
        for tail in [
            &b"\x18"[..],
            b"\x1a",
            b"\x1b[",
            b"\x1b_Gx",
            b"\x1bPq",
            b"\x1bX",
            b"\x1b]0;t",
            b"\x1b\x1b",
            b"\x1b\n",
            b"\x1b[\x1b",
        ] {
            let mut stream = b"\x1b]7501;state=working".to_vec();
            stream.extend_from_slice(tail);
            stream.extend_from_slice(b"\x1b\\");
            assert_eq!(cuts(&[&stream]), [], "{:?}", String::from_utf8_lossy(tail));
        }
    }

    #[test]
    fn an_aborting_osc_still_opens_its_own_sequence() {
        // `ESC ]` both ends the 7501 and starts the next OSC.
        assert_eq!(
            cuts(&[b"\x1b]7501;state=done\x1b]7501;state=idle\x07"]),
            [report(b"state=idle", true)]
        );
    }

    #[test]
    fn framing_survives_any_chunking() {
        let stream: &[u8] = b"x\x1b[31m\x1b]7501;?\x1b\\\x1b]133;A\x07\x1b]7501;state=done\x1b\\y";
        let whole = cuts(&[stream]);
        assert_eq!(whole.len(), 3);
        let bytewise: Vec<&[u8]> = stream.chunks(1).collect();
        assert_eq!(cuts(&bytewise), whole);
        for split in 0..stream.len() {
            assert_eq!(
                cuts(&[&stream[..split], &stream[split..]]),
                whole,
                "split {split}"
            );
        }
    }

    #[test]
    fn ignored_c0_bytes_count_toward_the_wire_limit_but_not_the_payload() {
        assert_eq!(
            cuts(&[b"\x1b]7501;sta\x00te=idle\x07"]),
            [report(b"state=idle", true)]
        );
        let mut big = b"\x1b]7501;state=idle".to_vec();
        big.extend(std::iter::repeat_n(0u8, MAX_SEQUENCE_BYTES));
        big.push(0x07);
        assert_eq!(cuts(&[&big]), [], "over the limit in NULs alone");
    }

    #[test]
    fn del_and_8_bit_bytes_are_stored_like_vte_stores_them() {
        assert_eq!(
            cuts(&[b"\x1b]7501;a\x7fb\xc3\xa9\x07"]),
            [report(b"a\x7fb\xc3\xa9", true)]
        );
    }

    #[test]
    fn the_limit_is_the_whole_sequence_including_framing() {
        let fit = |extra: usize| {
            // ESC ] (2) + "7501;" (5) + body + ESC \ (2)
            let mut s = b"\x1b]7501;".to_vec();
            s.extend(std::iter::repeat_n(b'a', MAX_SEQUENCE_BYTES - 9 + extra));
            s.extend_from_slice(b"\x1b\\");
            cuts(&[&s]).len()
        };
        assert_eq!(fit(0), 1);
        assert_eq!(fit(1), 0);
    }

    #[test]
    fn scanning_resynchronises_after_a_poisoned_sequence() {
        let mut s = b"\x1b]7501;".to_vec();
        s.extend(std::iter::repeat_n(b'a', MAX_SEQUENCE_BYTES * 2));
        s.extend_from_slice(b"\x07\x1b]7501;state=done\x07");
        assert_eq!(cuts(&[&s]), [report(b"state=done", true)]);
    }

    #[test]
    fn only_133_a_and_7501_are_cut() {
        assert_eq!(
            cuts(&[b"\x1b]133;A\x07\x1b]133;A;aid=1\x1b\\\x1b]133;B\x07\x1b]133;AB\x07"]),
            [CutAction::PromptStart, CutAction::PromptStart]
        );
        assert_eq!(
            cuts(&[b"\x1b]0;7501;x\x07\x1b]75010;state=idle\x07\x1b]7501\x07"]),
            []
        );
    }

    #[test]
    fn escape_intermediate_does_not_open_an_osc() {
        // `ESC SP ]` is an escape sequence with an intermediate, not OSC.
        assert_eq!(cuts(&[b"\x1b ]7501;state=idle\x07"]), []);
        // vte stays in Escape across C0, DEL and 8-bit bytes.
        assert_eq!(cuts(&[b"\x1b\n]7501;?\x07"]), [report(b"?", true)]);
        assert_eq!(cuts(&[b"\x1b\x7f]7501;?\x07"]), [report(b"?", true)]);
        assert_eq!(cuts(&[b"\x1b\x1b]7501;?\x07"]), [report(b"?", true)]);
        assert_eq!(cuts(&[b"\x1b\x80]7501;?\x07"]), [report(b"?", true)]);
    }

    #[test]
    fn generation_moves_only_on_observable_change() {
        let mut ps = VtProgramStatus::new();
        let g0 = ps.generation();
        assert!(ps.on_report(b"?", b"\x07").is_some());
        assert_eq!(ps.generation(), g0, "a query changes nothing");
        ps.on_report(b"state=working", b"\x07");
        let g1 = ps.generation();
        assert!(g1 > g0);
        ps.on_report(b"state=working", b"\x07");
        assert_eq!(ps.generation(), g1, "identical report");
        ps.on_report(b"state=bogus", b"\x07");
        assert_eq!(ps.generation(), g1, "discarded report");
        ps.on_full_reset();
        assert!(ps.generation() > g1);
        assert!(!ps.publication().seen);
        assert!(
            VtProgramStatus::new().generation() > ps.generation(),
            "global and monotonic"
        );
    }

    mod differential {
        //! The scanner against vte itself. vte is fed one byte at a time so
        //! the byte that ended each OSC is known exactly: BEL, or ESC whose
        //! very next byte is `\` (seen as `esc_dispatch('\')` with no
        //! callback in between). Anything else ended the string malformed.

        use super::super::*;
        use crate::program_status::fuzzing::reference;
        use proptest::prelude::*;

        fn scanned(chunks: &[&[u8]]) -> Vec<CutAction> {
            let mut s = OscStatusScanner::default();
            chunks
                .iter()
                .flat_map(|c| s.scan(c))
                .map(|c| c.action)
                .collect()
        }

        /// Fragments that steer both parsers through every state.
        fn fragment() -> impl Strategy<Value = Vec<u8>> {
            prop::sample::select(vec![
                &b"\x1b]7501;"[..],
                b"\x1b]133;A",
                b"\x1b]133;B",
                b"\x1b]0;t",
                b"\x1b]",
                b"\x1b\\",
                b"\x07",
                b"\x18",
                b"\x1a",
                b"\x1b",
                b"\x1b[",
                b"\x1b[31m",
                b"\x1bP",
                b"\x1bPq",
                b"\x1b_G",
                b"\x1bX",
                b"\x1b^",
                b"\x1b ",
                b"\x1b(",
                b"\x00",
                b"\n",
                b"\x7f",
                b"\xc3\xa9",
                b"\x9c",
                b"\x9d",
                b"]",
                b"]7501;",
                b"]133;A",
                b"\\",
                b"?",
                b"state=idle",
                b";",
                b":",
                b"A",
                b"text",
            ])
            .prop_map(<[u8]>::to_vec)
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(8192))]

            #[test]
            fn scanner_frames_exactly_what_vte_frames(
                frags in prop::collection::vec(fragment(), 0..40),
                cut in any::<prop::sample::Index>(),
            ) {
                let bytes: Vec<u8> = frags.concat();
                // Keep vte's own caps out of play: params and buffer.
                prop_assume!(bytes.iter().filter(|&&b| b == b';').count() < 15);
                let expected = reference(&bytes);
                prop_assert_eq!(&scanned(&[&bytes]), &expected, "{:?}", String::from_utf8_lossy(&bytes));
                let at = cut.index(bytes.len() + 1);
                prop_assert_eq!(&scanned(&[&bytes[..at], &bytes[at..]]), &expected);
                let bytewise: Vec<&[u8]> = bytes.chunks(1).collect();
                prop_assert_eq!(&scanned(&bytewise), &expected);
            }
        }

        #[test]
        fn the_reference_itself_rejects_aborted_strings() {
            assert_eq!(
                reference(b"\x1b]7501;?\x1b\\"),
                [CutAction::Report {
                    payload: b"?".to_vec(),
                    bel: false
                }]
            );
            assert_eq!(reference(b"\x1b]7501;?\x1b[\x1b\\"), []);
            assert_eq!(reference(b"\x1b]7501;?\x1b_G\x1b\\"), []);
            assert_eq!(reference(b"\x1b]7501;?\x18"), []);
        }
    }
}
