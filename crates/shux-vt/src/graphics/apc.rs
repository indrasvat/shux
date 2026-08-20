//! APC (Application Program Command) extraction for the kitty graphics protocol.
//!
//! # Why this exists
//!
//! `vte` 0.15 has no APC callback: `State::SosPmApcString` consumes the bytes
//! and dispatches only through `anywhere`, so a `Perform` implementation never
//! sees them (vte-0.15 `src/lib.rs:182`). Kitty graphics commands arrive as
//! `ESC _ G <control> ; <payload> ESC \`, which means shux cannot see a single
//! byte of them through the parser alone.
//!
//! # Why this scanner does NOT remove bytes
//!
//! The obvious design -- strip the APC out of the stream and hand the rest to
//! vte -- is wrong, and not fixably so. Deleting bytes from a stream feeding a
//! state machine changes that machine's state. vte leaves a string state on
//! `ESC <any>`, `CAN` (0x18) and `SUB` (0x1A), not only on `ESC \`, so a
//! stripping splitter diverges from a correct terminal in ways that lose or
//! invent user-visible output. Measured against stock vte:
//!
//! | input | stock vte | strip-first |
//! |---|---|---|
//! | `ESC [ 3 ESC _ G x ESC \ HELLO` | CSI aborted, prints `HELLO` | **invents `CSI 3 H`** |
//! | `ESC _ G broken` then `ESC [ 31 m RED` | full coloured output | **everything swallowed** |
//! | `ESC ] 0 ; t ESC _ G a=q ; ESC \ HI` | title set, prints `HI` | title AND text lost |
//!
//! So this scanner only reports *where* an APC sits. The caller feeds vte every
//! byte unchanged and merely cuts its `advance` calls at those boundaries, which
//! makes the text path bit-identical to the pre-graphics build **by
//! construction** rather than by test. A scanner false positive therefore costs
//! at most one spurious image -- never a lost glyph, never a synthesized CSI.
//!
//! Deliberately out of scope: 8-bit C1 forms (`0x9F` APC / `0x9C` ST). vte only
//! speaks 7-bit codes, and `0x9F` is a legal UTF-8 continuation byte -- six of
//! them sit inside `.shux/fixtures/vt-corpus/rich-tui/vivecaka.raw` as parts of
//! `U+27A0`/`U+27D0`/`U+27C1`/`U+27E1`. A byte-level scan for `0x9F` would eat
//! those glyphs.

/// Cap on a single APC body held in flight.
///
/// The kitty protocol requires clients to chunk direct transmissions at 4096
/// base64 bytes per escape code, so this is ample headroom (terminal-browser's
/// largest observed APC is 4152 bytes) while keeping a hostile pane's in-flight
/// buffer bounded. An overrun is dropped, not truncated-and-decoded: a partial
/// image is not worth rendering, and the scanner keeps running so the stream
/// resynchronises at the real terminator.
pub(crate) const MAX_APC_BODY_BYTES: usize = 64 * 1024;

/// One completed APC found in the current chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApcCut {
    /// Offset in the current chunk one past the APC's terminator. The caller
    /// feeds vte everything up to here before acting on `body`, so the cursor
    /// and mode state the command observes are the ones the sequence really saw.
    pub(crate) end: usize,
    /// Body between `ESC _` and the terminator. For a kitty graphics command
    /// this is `G<control>;<payload>`. May have started in an earlier chunk.
    pub(crate) body: Vec<u8>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum ScanState {
    /// Outside any APC.
    #[default]
    Ground,
    /// Outside an APC, previous byte was ESC. Carried across chunk boundaries:
    /// 48% of terminal-browser's APCs straddle an 8192-byte PTY read.
    GroundEsc,
    /// Inside an APC body.
    Apc,
    /// Inside an APC body, previous byte was ESC.
    ApcEsc,
}

/// Locates APC sequences across arbitrarily-chunked PTY reads.
#[derive(Debug, Default)]
pub(crate) struct ApcScanner {
    state: ScanState,
    body: Vec<u8>,
    /// Set when `body` hit [`MAX_APC_BODY_BYTES`]; the sequence is still tracked
    /// to its terminator so scanning resynchronises, but no cut is emitted.
    overflowed: bool,
}

impl ApcScanner {
    /// Report every APC that *completes* within `chunk`, in stream order.
    ///
    /// Escape-free input short-circuits on one SIMD pass.
    ///
    /// The guard is `no ESC at all`, not `no literal "ESC _"`. The tighter
    /// probe looks equivalent and is not: vte stays in the Escape state across
    /// C0 bytes, DEL and 8-bit bytes, so `ESC LF _` opens an APC while
    /// containing no `ESC _` pair. Searching for the pair skipped those
    /// sequences entirely and then read their bodies as ground text.
    ///
    /// The cost of the wider guard is small because the slow path is itself
    /// `memchr`-driven: escape-heavy TUI output pays roughly 1.5x a `memmem`
    /// probe rather than the ~8x a byte-at-a-time loop would cost, and plain
    /// text -- build logs, `cat` -- still short-circuits.
    pub(crate) fn scan(&mut self, chunk: &[u8]) -> Vec<ApcCut> {
        if self.state == ScanState::Ground && memchr::memchr(0x1b, chunk).is_none() {
            // No ESC means the state machine cannot leave Ground, so no APC can
            // begin and there is nothing to carry to the next read.
            return Vec::new();
        }
        self.scan_slow(chunk)
    }

    /// The two "scanning for a byte" states skip ahead with `memchr` rather than
    /// stepping one byte at a time.
    ///
    /// This is not only about speed in the APC case. A pane that emits a lone
    /// `ESC _` and never terminates it -- a truncated writer, or `cat` on a
    /// binary that happens to contain those two bytes -- leaves the scanner in
    /// `Apc` forever. `scan`'s `memmem` fast path is gated on `Ground`, so every
    /// subsequent read for the life of that pane would take the slow path. With
    /// a byte-at-a-time loop that is a permanent ~8x scan-cost penalty bought
    /// with two bytes; with `memchr` the parked state costs one SIMD pass.
    fn scan_slow(&mut self, chunk: &[u8]) -> Vec<ApcCut> {
        let mut cuts = Vec::new();
        let mut i = 0;
        while i < chunk.len() {
            match self.state {
                ScanState::Ground => {
                    // Only ESC can leave Ground; everything before it is text.
                    match memchr::memchr(0x1b, &chunk[i..]) {
                        Some(off) => {
                            self.state = ScanState::GroundEsc;
                            i += off + 1;
                        }
                        None => break,
                    }
                }
                ScanState::Apc => {
                    // Only ESC, CAN or SUB can leave an APC body; everything
                    // before them is payload.
                    match memchr::memchr3(0x1b, 0x18, 0x1a, &chunk[i..]) {
                        Some(off) => {
                            self.push_span(&chunk[i..i + off]);
                            let b = chunk[i + off];
                            i += off + 1;
                            if b == 0x1b {
                                self.state = ScanState::ApcEsc;
                            } else {
                                // CAN / SUB abort a string sequence outright.
                                self.abort();
                            }
                        }
                        None => {
                            self.push_span(&chunk[i..]);
                            break;
                        }
                    }
                }
                ScanState::GroundEsc => {
                    let b = chunk[i];
                    i += 1;
                    match b {
                        b'_' => self.begin(),
                        // vte stays in Escape for all of these -- it executes C0
                        // bytes and ignores DEL and 8-bit bytes without leaving
                        // the state (vte-0.15 src/lib.rs:340-390) -- so a `_`
                        // after one of them still opens an APC. Only CAN (0x18)
                        // and SUB (0x1A) really return to Ground, and they fall
                        // through to the default arm below.
                        0x00..=0x17 | 0x19 | 0x1b..=0x1f | 0x7f..=0xff => {}
                        _ => self.state = ScanState::Ground,
                    }
                }
                ScanState::ApcEsc => {
                    let b = chunk[i];
                    i += 1;
                    match b {
                        // ST: the only well-formed terminator.
                        b'\\' => {
                            if !self.overflowed {
                                cuts.push(ApcCut {
                                    end: i,
                                    body: std::mem::take(&mut self.body),
                                });
                            }
                            self.abort();
                        }
                        // `ESC _` starts a fresh APC; the unterminated one is junk.
                        b'_' => self.begin(),
                        // Same "vte is still in Escape" set as GroundEsc above.
                        0x00..=0x17 | 0x19 | 0x1b..=0x1f | 0x7f..=0xff => {}
                        // Any other ESC-introduced sequence ends this APC malformed.
                        _ => self.abort(),
                    }
                }
            }
        }
        cuts
    }

    fn begin(&mut self) {
        self.state = ScanState::Apc;
        self.body.clear();
        self.overflowed = false;
    }

    fn abort(&mut self) {
        self.state = ScanState::Ground;
        self.body.clear();
        self.overflowed = false;
    }

    /// Append a run of payload bytes, dropping the whole body once it exceeds
    /// the cap. Dropped rather than truncated: a partial image is not worth
    /// decoding, and the scanner keeps running so the stream resynchronises at
    /// the real terminator.
    fn push_span(&mut self, span: &[u8]) {
        if self.overflowed {
            return;
        }
        if self.body.len() + span.len() > MAX_APC_BODY_BYTES {
            self.overflowed = true;
            self.body.clear();
            return;
        }
        self.body.extend_from_slice(span);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bodies(chunks: &[&[u8]]) -> Vec<Vec<u8>> {
        let mut s = ApcScanner::default();
        let mut out = Vec::new();
        for c in chunks {
            out.extend(s.scan(c).into_iter().map(|cut| cut.body));
        }
        out
    }

    #[test]
    fn finds_a_well_formed_apc() {
        assert_eq!(
            bodies(&[b"abc\x1b_Ga=T;AAAA\x1b\\def"]),
            vec![b"Ga=T;AAAA".to_vec()]
        );
    }

    #[test]
    fn reports_the_offset_past_the_terminator() {
        let mut s = ApcScanner::default();
        let cuts = s.scan(b"ab\x1b_Gx\x1b\\zz");
        assert_eq!(cuts.len(), 1);
        // `ab` = 2, `ESC _ G x` = 4, `ESC \` = 2  ->  8
        assert_eq!(cuts[0].end, 8);
    }

    #[test]
    fn survives_every_chunk_boundary() {
        let stream: &[u8] = b"xx\x1b_Ga=q,i=1;AAAA\x1b\\yy\x1b_Gm=0;BB\x1b\\zz";
        let whole = bodies(&[stream]);
        assert_eq!(whole.len(), 2);
        for split in 1..stream.len() {
            let (a, b) = stream.split_at(split);
            assert_eq!(bodies(&[a, b]), whole, "split at {split}");
        }
    }

    #[test]
    fn survives_byte_at_a_time_delivery() {
        let stream: &[u8] = b"\x1b_Ga=T;QQ\x1b\\tail";
        let chunks: Vec<&[u8]> = stream.chunks(1).collect();
        assert_eq!(bodies(&chunks), vec![b"Ga=T;QQ".to_vec()]);
    }

    // NOTE on the shape of the next three tests: each puts a *well-formed
    // terminator later in the stream*. Asserting "nothing was emitted" on a
    // stream that simply never terminates passes for both the correct and the
    // broken scanner, so it proves nothing. With a trailing `ESC \\`, a scanner
    // that wrongly treats the abort byte as payload emits a body here, and
    // these go red -- verified by mutation.

    #[test]
    fn can_aborts_the_sequence() {
        // CAN kills the string; the later ST must not resurrect it.
        assert!(bodies(&[b"\x1b_GA\x18junk\x1b\\"]).is_empty());
    }

    #[test]
    fn sub_aborts_the_sequence() {
        assert!(bodies(&[b"\x1b_GA\x1ajunk\x1b\\"]).is_empty());
    }

    #[test]
    fn esc_without_st_aborts_the_sequence() {
        // vte leaves a string state on ESC-anything, not just on `ESC \\`. An APC
        // interrupted by `ESC [` is malformed: the bytes after it are ordinary
        // output, and the later ST must not close a body that already died.
        assert!(bodies(&[b"\x1b_GA\x1b[0m\x1b\\"]).is_empty());
    }

    #[test]
    fn unterminated_apc_emits_nothing() {
        assert!(bodies(&[b"\x1b_Gbroken-forever"]).is_empty());
    }

    #[test]
    fn a_c0_byte_after_esc_does_not_cancel_the_escape() {
        // vte's `advance_esc` EXECUTES C0 bytes (and ignores DEL and 8-bit
        // bytes) while STAYING in the Escape state, so `ESC LF _` still opens an
        // APC (vte-0.15 src/lib.rs:340-390). A scanner that dropped to Ground on
        // the C0 would miss the sequence entirely and then read the body as
        // ground text.
        for interruption in [
            &b"\x00"[..], // NUL
            &b"\x07"[..], // BEL
            &b"\x0a"[..], // LF
            &b"\x1f"[..], // US
            &b"\x7f"[..], // DEL
            &b"\xff"[..], // 8-bit
        ] {
            let mut stream = Vec::from(&b"\x1b"[..]);
            stream.extend_from_slice(interruption);
            stream.extend_from_slice(b"_Ga=T;P\x1b\\");
            assert_eq!(
                bodies(&[&stream]),
                vec![b"Ga=T;P".to_vec()],
                "ESC {interruption:?} _ should still open an APC"
            );
        }
    }

    #[test]
    fn can_and_sub_after_esc_do_cancel_it() {
        // The two that really do return vte to Ground.
        for cancel in [&b"\x18"[..], &b"\x1a"[..]] {
            let mut stream = Vec::from(&b"\x1b"[..]);
            stream.extend_from_slice(cancel);
            stream.extend_from_slice(b"_Ga=T;P\x1b\\");
            assert!(
                bodies(&[&stream]).is_empty(),
                "ESC {cancel:?} returns to Ground, so `_` opens nothing"
            );
        }
    }

    #[test]
    fn esc_underscore_restarts_a_pending_apc() {
        assert_eq!(
            bodies(&[b"\x1b_Gfirst\x1b_Gsecond\x1b\\"]),
            vec![b"Gsecond".to_vec()]
        );
    }

    #[test]
    fn esc_esc_st_still_terminates() {
        assert_eq!(bodies(&[b"\x1b_Gx\x1b\x1b\\"]), vec![b"Gx".to_vec()]);
    }

    #[test]
    fn oversized_body_is_dropped_but_stream_resynchronises() {
        let mut huge = Vec::from(&b"\x1b_G"[..]);
        huge.extend(std::iter::repeat_n(b'A', MAX_APC_BODY_BYTES + 10));
        huge.extend_from_slice(b"\x1b\\");
        huge.extend_from_slice(b"\x1b_Gsmall\x1b\\");
        // The overrun is dropped; the APC after it is still found.
        assert_eq!(bodies(&[&huge]), vec![b"Gsmall".to_vec()]);
    }

    #[test]
    fn fast_path_carries_a_trailing_esc() {
        // `ESC` ends one read, `_` opens the next: the fast path must not lose it.
        assert_eq!(bodies(&[b"plain\x1b", b"_Gx\x1b\\"]), vec![b"Gx".to_vec()]);
    }

    #[test]
    fn ignores_streams_with_no_apc() {
        assert!(bodies(&[b"\x1b[31mRED\x1b[0m\r\n$ \x1b]0;title\x07"]).is_empty());
    }
}
