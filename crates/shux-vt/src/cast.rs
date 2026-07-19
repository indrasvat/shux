//! asciinema v2 `.cast` serialization (task 083) — pure, no daemon deps.
//!
//! The daemon's per-pane recorder (`spawn_pane_recorder`) feeds this the raw PTY output chunks
//! (with arrival instants) and resize events; it emits asciinema v2 lines. Kept in `shux-vt` (not
//! the binary) so the UTF-8-boundary carry — the subtle part — is unit-testable and adversarial-
//! fuzzable in isolation, like the settle and gate-compare cores. Design: `.local/083-design.md`.
//!
//! **UTF-8 safety.** Raw PTY reads split anywhere, including mid-multibyte-sequence. asciinema
//! `data` is a JSON string, so it must be valid UTF-8. Rather than grok's lossy per-chunk decode
//! (which turns a split glyph into two replacement chars), this carries the trailing incomplete
//! sequence into the next chunk so it replays intact; only genuine interior garbage — or a
//! truncated tail at EOF — becomes U+FFFD.

use std::time::Instant;

/// Split `buf` at the start of any TRAILING incomplete UTF-8 sequence (at most 3 bytes). Returns
/// `(emit_len, decoded_prefix)`: bytes `[0, emit_len)` are safe to emit now (lossy-decoded, so
/// interior garbage becomes U+FFFD); `[emit_len, len)` is a partial lead+continuation run to keep
/// buffered until the completing bytes arrive.
pub fn cast_complete_prefix(buf: &[u8]) -> (usize, String) {
    let mut split = buf.len();
    for back in 1..=3.min(buf.len()) {
        let b = buf[buf.len() - back];
        if b < 0x80 {
            break; // ASCII boundary — everything after is complete
        }
        if b >= 0xC0 {
            // A lead byte: 2-, 3-, or 4-byte sequence. Only carry it when it can STILL become a
            // valid sequence — a structurally-invalid lead (0xC0/0xC1 overlong, 0xF5..=0xFF out of
            // range) can never be completed, so emit it now (lossy → U+FFFD) rather than lag it to
            // the next chunk / EOF (adv-083 Agent B MINOR).
            let valid_lead = (0xC2..=0xF4).contains(&b);
            let need = if b >= 0xF0 {
                4
            } else if b >= 0xE0 {
                3
            } else {
                2
            };
            if valid_lead && back < need {
                split = buf.len() - back; // incomplete but recoverable — buffer from this lead
            }
            break;
        }
        // Continuation byte (0x80..0xC0): keep scanning back toward its lead.
    }
    (split, String::from_utf8_lossy(&buf[..split]).into_owned())
}

/// A UTF-8-boundary-safe asciinema v2 event serializer. Accumulates raw PTY bytes and, per chunk,
/// emits the longest prefix that ends on a COMPLETE UTF-8 sequence; a trailing incomplete
/// multibyte sequence is carried into the next chunk so a glyph split across two PTY reads replays
/// intact. Timestamps are monotonic-non-decreasing relative seconds from the record epoch.
#[derive(Debug)]
pub struct CastWriter {
    epoch: Instant,
    carry: Vec<u8>,
    last_t: f64,
}

impl CastWriter {
    /// Start a writer whose relative timestamps are measured from `epoch` (the record-arm instant).
    pub fn new(epoch: Instant) -> Self {
        Self {
            epoch,
            carry: Vec::new(),
            last_t: 0.0,
        }
    }

    /// Relative seconds since the epoch, clamped monotonic non-decreasing (asciinema requires
    /// non-decreasing offsets; a late or backward `Instant` never rewinds the cast clock).
    fn rel(&mut self, at: Instant) -> f64 {
        let t = at.saturating_duration_since(self.epoch).as_secs_f64();
        let t = t.max(self.last_t);
        self.last_t = t;
        t
    }

    /// Fold an output chunk → an asciinema `[t,"o",data]` line, or `None` when only an incomplete
    /// trailing sequence remains buffered.
    pub fn output_line(&mut self, data: &[u8], at: Instant) -> Option<String> {
        self.carry.extend_from_slice(data);
        let (emit_len, out) = cast_complete_prefix(&self.carry);
        if emit_len == 0 {
            return None;
        }
        self.carry.drain(..emit_len);
        let t = self.rel(at);
        Some(serde_json::json!([t, "o", out]).to_string())
    }

    /// An asciinema `[t,"r","COLSxROWS"]` resize event (grok's honesty gap). Does NOT flush the
    /// output carry first: a pending INCOMPLETE multibyte sequence must wait for its completing
    /// bytes (which arrive after the resize), so it is serialized after this event with its own
    /// later timestamp. Timestamps stay monotonic; a half-glyph can't render across a resize
    /// anyway, so this ordering is honest, not a loss (adv-083 Agent B MINOR).
    pub fn resize_line(&mut self, cols: u16, rows: u16, at: Instant) -> String {
        let t = self.rel(at);
        serde_json::json!([t, "r", format!("{cols}x{rows}")]).to_string()
    }

    /// Flush a genuinely-truncated trailing sequence at EOF (lossy — no more bytes are coming).
    pub fn flush_line(&mut self) -> Option<String> {
        if self.carry.is_empty() {
            return None;
        }
        let out = String::from_utf8_lossy(&self.carry).into_owned();
        self.carry.clear();
        let t = self.last_t;
        Some(serde_json::json!([t, "o", out]).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(line: &str) -> String {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        v[2].as_str().unwrap().to_string()
    }

    #[test]
    fn complete_prefix_buffers_trailing_incomplete_multibyte() {
        // "a" + the first 2 bytes of the 3-byte '⠋' → emit "a", buffer the 2 partial bytes.
        let g = "⠋".as_bytes();
        let mut buf = vec![b'a'];
        buf.extend_from_slice(&g[..2]);
        let (n, out) = cast_complete_prefix(&buf);
        assert_eq!(n, 1);
        assert_eq!(out, "a");
    }

    #[test]
    fn complete_prefix_emits_all_when_boundary_is_clean() {
        let buf = "hi⠋".as_bytes().to_vec();
        let (n, out) = cast_complete_prefix(&buf);
        assert_eq!(n, buf.len());
        assert_eq!(out, "hi⠋");
    }

    #[test]
    fn output_line_carries_a_glyph_split_across_two_chunks() {
        // A multibyte glyph split across two PTY reads must replay INTACT (not two tofu boxes).
        let mut w = CastWriter::new(Instant::now());
        let g = "⠋".as_bytes(); // E2 A0 8B
        let mut c1 = vec![b'a'];
        c1.extend_from_slice(&g[..2]); // "a" + first 2 bytes
        assert_eq!(data(&w.output_line(&c1, Instant::now()).unwrap()), "a");
        let mut c2 = vec![g[2]]; // last byte completes the glyph
        c2.push(b'b');
        assert_eq!(data(&w.output_line(&c2, Instant::now()).unwrap()), "⠋b");
    }

    #[test]
    fn resize_line_is_an_asciinema_r_event() {
        let mut w = CastWriter::new(Instant::now());
        let v: serde_json::Value =
            serde_json::from_str(&w.resize_line(100, 40, Instant::now())).unwrap();
        assert_eq!(v[1], "r");
        assert_eq!(v[2], "100x40");
    }

    #[test]
    fn timestamps_are_non_negative_and_non_decreasing() {
        let mut w = CastWriter::new(Instant::now());
        let l1 = w.output_line(b"a", Instant::now()).unwrap();
        let l2 = w.output_line(b"b", Instant::now()).unwrap();
        let t = |l: &str| {
            serde_json::from_str::<serde_json::Value>(l).unwrap()[0]
                .as_f64()
                .unwrap()
        };
        assert!(t(&l1) >= 0.0);
        assert!(t(&l2) >= t(&l1));
    }

    #[test]
    fn structurally_invalid_lead_bytes_emit_immediately_not_buffered() {
        // adv-083 Agent B (MINOR): a byte that can NEVER begin a valid UTF-8 sequence
        // (0xC0/0xC1 overlong, 0xF5..=0xFF out of range) must NOT be held in carry as if an
        // incomplete multibyte lead — it can never be completed, so emit it (lossy) now rather
        // than lag it to the next chunk / EOF.
        for b in [0xC0u8, 0xC1, 0xF5, 0xF8, 0xFF] {
            let (n, out) = cast_complete_prefix(&[b]);
            assert_eq!(n, 1, "invalid lead {b:#x} must be emitted, not buffered");
            assert_eq!(out, "\u{FFFD}", "invalid lead {b:#x} decodes to U+FFFD");
        }
        // A VALID incomplete lead is still buffered (unchanged).
        let g = "😀".as_bytes(); // F0 9F 98 80 — 0xF0 is a valid 4-byte lead
        let (n, _) = cast_complete_prefix(&g[..1]);
        assert_eq!(n, 0, "a valid incomplete lead is still carried");
    }

    #[test]
    fn flush_emits_a_genuinely_truncated_tail_lossily() {
        let mut w = CastWriter::new(Instant::now());
        let g = "⠋".as_bytes();
        assert!(
            w.output_line(&g[..2], Instant::now()).is_none(),
            "incomplete → buffered"
        );
        let out = data(&w.flush_line().expect("flush emits the truncated tail"));
        assert!(
            out.contains('\u{FFFD}'),
            "a truncated tail decodes lossily at EOF"
        );
    }
}
