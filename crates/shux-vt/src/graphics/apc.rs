//! APC extraction for the kitty graphics protocol.
//!
//! `vte` 0.15 has no APC callback -- `State::SosPmApcString` dispatches only
//! through `anywhere` -- so kitty's `ESC _ G <control> ; <payload> ESC \` is
//! invisible to a `Perform` impl.
//!
//! This scanner reports only *where* an APC sits; it never removes bytes.
//! Stripping is not fixably correct: vte leaves a string state on `ESC <any>`,
//! `CAN` and `SUB`, not just `ESC \`, so a stripping splitter both loses output
//! (`ESC _ G broken` then `ESC [ 31 m RED` swallows everything) and invents it
//! (`ESC [ 3 ESC _ G x ESC \ HELLO` synthesizes `CSI 3 H`). The caller instead
//! feeds vte every byte and cuts only its `advance` calls.
//!
//! That is neutral, but NOT "by construction" -- vte's output does depend on
//! `advance` call boundaries, which `c1_controls_are_chunk_sensitive_in_vte`
//! pins. It is neutral for a narrower reason: every cut lands one byte past an
//! `ESC \\` that vte consumed in `State::Escape`, and vte only reaches the
//! chunk-length-sensitive `advance_ground` from `Ground`. So no `advance_ground`
//! call ever has its length truncated by a cut, and neither its partial-UTF-8
//! stash nor its `processed < num_bytes` branch can decide differently. The
//! dependency is on vte's `advance` loop; a `vte` bump is where to re-check it.
//!
//! Out of scope: 8-bit C1 forms. `0x9F` is a legal UTF-8 continuation byte --
//! six sit inside `vt-corpus/rich-tui/vivecaka.raw` -- so scanning for it would
//! eat glyphs. `0x9C` is not treated as ST either, and vte agrees -- see
//! `only_the_7bit_st_terminates_an_apc_and_vte_agrees`.

/// Cap on a single APC body held in flight. Kitty chunks direct transmissions
/// at 4096 base64 bytes, so this is ample headroom while bounding a hostile
/// pane. An overrun drops the body and keeps scanning to resynchronise.
pub(crate) const MAX_APC_BODY_BYTES: usize = 64 * 1024;

/// One completed APC found in the current chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApcCut {
    /// One past the terminator. vte is fed everything up to here first, so the
    /// command observes the cursor and mode state the sequence really saw.
    pub(crate) end: usize,
    /// Body between `ESC _` and the terminator; `G<control>;<payload>` for
    /// kitty. May have started in an earlier chunk.
    pub(crate) body: Vec<u8>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum ScanState {
    /// Outside any APC.
    #[default]
    Ground,
    /// Outside an APC, previous byte was ESC. Carried across chunk boundaries.
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
    /// Set when `body` hit [`MAX_APC_BODY_BYTES`]. Still tracked to its
    /// terminator so scanning resynchronises, but no cut is emitted.
    overflowed: bool,
}

impl ApcScanner {
    /// Report every APC that *completes* within `chunk`, in stream order.
    ///
    /// The fast path guards on `no ESC at all`, not on `no literal "ESC _"`:
    /// vte stays in Escape across C0 bytes, DEL and 8-bit bytes, so `ESC LF _`
    /// opens an APC while containing no `ESC _` pair.
    pub(crate) fn scan(&mut self, chunk: &[u8]) -> Vec<ApcCut> {
        if self.state == ScanState::Ground && memchr::memchr(0x1b, chunk).is_none() {
            // No ESC means the state machine cannot leave Ground, so no APC can
            // begin and there is nothing to carry to the next read.
            return Vec::new();
        }
        self.scan_slow(chunk)
    }

    /// Both scanning states skip ahead with `memchr`. A pane that emits a lone
    /// unterminated `ESC _` parks the scanner in `Apc` for its whole life, and
    /// the fast path is gated on `Ground` -- so the parked cost must stay one
    /// SIMD pass rather than a byte-at-a-time walk.
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
                        // vte stays in Escape across these (vte-0.15
                        // src/lib.rs:340-390), so a later `_` still opens an
                        // APC. Only CAN/SUB return to Ground, via the arm below.
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

    /// Append payload, dropping the whole body past the cap rather than
    /// truncating -- a partial image is not worth decoding.
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
    fn every_cut_ends_exactly_one_byte_past_the_st() {
        // Pinned directly: the sliced-vs-unsliced proptest is only ~0.17%
        // likely to catch an off-by-one. One byte short leaves vte mid-ST, one
        // byte long steals a byte of the following text.
        let stream: &[u8] = b"xx\x1b_Ga=q,i=7;QQ\x1b\\yyyy";
        let mut scanner = ApcScanner::default();
        let cuts = scanner.scan(stream);
        assert_eq!(cuts.len(), 1);
        let end = cuts[0].end;
        assert_eq!(
            &stream[end - 2..end],
            b"\x1b\\",
            "a cut must land immediately after the ST it terminated on"
        );
        assert_eq!(
            stream[end], b'y',
            "the next byte is untouched following text"
        );
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

    // The next three tests each put a well-formed terminator LATER in the
    // stream. "Nothing emitted" on a never-terminating stream passes on both
    // the correct and the broken scanner; with a trailing `ESC \\` a scanner
    // that treats the abort byte as payload goes red. Verified by mutation.

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
