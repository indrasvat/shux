//! `shux.view.v1`: what the gateway and its browser viewer say to each other.
//!
//! Server to browser: one JSON `ready`, then the composed screen as ordered
//! binary messages, each an 8-byte big-endian offset (the stream position of
//! its first byte) and at most [`CHUNK`] bytes of ANSI. Text messages
//! between them keep their place: `resize` marks the grid the bytes after it
//! are drawn for, `host` reports the host's layout size, and `ended` says why
//! the stream is closing. Browser to server: text only, and only `hello`
//! (first), `resize` and `ack`, each a few dozen bytes.
//!
//! The browser acknowledges the offset its terminal has parsed. No more than
//! [`WINDOW`] bytes are ever sent and unacknowledged; a viewer that holds
//! back more than [`BACKLOG`] or acknowledges nothing for [`ACK_DEADLINE`]
//! is dropped, and reconnects to a full redraw.

use std::collections::VecDeque;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const PROTOCOL: &str = "shux.view.v1";
pub const CHUNK: usize = 32 * 1024;
pub const WINDOW: u64 = 256 * 1024;
/// Composed output queued for a viewer that has no credit for it.
pub const BACKLOG: usize = 4 * 1024 * 1024;
pub const ACK_DEADLINE: Duration = Duration::from_secs(10);
pub const MAX_COLS: u16 = 240;
pub const MAX_ROWS: u16 = 100;
const MIN_COLS: u16 = 2;
const MIN_ROWS: u16 = 2;
/// The largest message a viewer may send.
pub const MAX_INBOUND: usize = 256;

pub fn clamp(cols: u16, rows: u16) -> (u16, u16) {
    (
        cols.clamp(MIN_COLS, MAX_COLS),
        rows.clamp(MIN_ROWS, MAX_ROWS),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case", deny_unknown_fields)]
pub enum Inbound {
    Hello { cols: u16, rows: u16 },
    Resize { cols: u16, rows: u16 },
    Ack { offset: u64 },
}

impl Inbound {
    /// The socket refuses a message over [`MAX_INBOUND`] before this sees it.
    pub fn parse(text: &str) -> Option<Self> {
        serde_json::from_str(text).ok()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
}

/// Colours the page draws its chrome and terminal with.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Palette {
    pub background: String,
    pub foreground: String,
    pub bar: String,
    pub bar_text: String,
    pub muted: String,
    pub shared: String,
}

impl Palette {
    pub fn of(theme: &shux_core::theme::Theme) -> Self {
        let hex = |c: shux_core::theme::Rgb| format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b);
        Self {
            // The panes' own default colours are the terminal's; these are
            // Macchiato's, shux's default palette.
            background: "#24273a".into(),
            foreground: "#cad3f5".into(),
            bar: hex(theme.status_bg),
            bar_text: hex(theme.status_fg),
            muted: hex(theme.status_muted),
            shared: hex(theme.status_shared),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "t", rename_all = "snake_case")]
#[expect(
    clippy::large_enum_variant,
    reason = "each is built, serialized and dropped at once"
)]
pub enum Outbound {
    Ready {
        v: &'static str,
        session: String,
        size: Size,
        host: Option<Size>,
        palette: Palette,
        /// RFC 3339, UTC.
        expires_at: String,
        /// The page counts down from this, whatever its own clock says.
        expires_in_ms: u64,
        max: Size,
    },
    /// The bytes from `at` on are drawn for this grid.
    Resize {
        cols: u16,
        rows: u16,
        at: u64,
    },
    Host {
        cols: u16,
        rows: u16,
    },
    Ended {
        reason: &'static str,
    },
}

impl Outbound {
    pub fn text(&self) -> String {
        // A closed enum of plain fields always serializes.
        serde_json::to_string(self).expect("view message serializes")
    }
}

/// One message to send, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Queued {
    Bytes(Vec<u8>),
    Text(String),
}

/// What a viewer has been sent and has acknowledged, and what waits.
#[derive(Debug, Default)]
pub struct Credit {
    queue: VecDeque<Queued>,
    /// Where each sent, unacknowledged chunk ends.
    chunk_ends: VecDeque<u64>,
    queued_bytes: usize,
    sent: u64,
    acked: u64,
}

/// What an acknowledgement did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ack {
    /// Nothing new.
    Duplicate,
    /// Moved, within a chunk.
    Partial,
    /// Completed at least one sent chunk: the one ack per chunk a browser
    /// owes, which no rate limit counts.
    Chunk,
}

impl Credit {
    pub fn in_flight(&self) -> u64 {
        self.sent - self.acked
    }

    /// Queues composed output; false once more than [`BACKLOG`] waits.
    pub fn push_bytes(&mut self, bytes: &[u8]) -> bool {
        if bytes.is_empty() {
            return true;
        }
        self.queued_bytes += bytes.len();
        if self.queued_bytes > BACKLOG {
            return false;
        }
        match self.queue.back_mut() {
            Some(Queued::Bytes(tail)) if tail.len() < CHUNK => tail.extend_from_slice(bytes),
            _ => self.queue.push_back(Queued::Bytes(bytes.to_vec())),
        }
        true
    }

    pub fn push_text(&mut self, message: &Outbound) {
        self.queue.push_back(Queued::Text(message.text()));
    }

    /// The stream position the next queued byte will have.
    pub fn position(&self) -> u64 {
        self.sent + self.queued_bytes as u64
    }

    /// The next message the window allows: text always, bytes as an
    /// offset-prefixed chunk of at most [`CHUNK`] within the credit left.
    pub fn pop(&mut self) -> Option<Queued> {
        let credit = WINDOW.saturating_sub(self.in_flight());
        match self.queue.front_mut()? {
            Queued::Text(_) => self.queue.pop_front(),
            Queued::Bytes(bytes) => {
                let take = bytes
                    .len()
                    .min(CHUNK)
                    .min(usize::try_from(credit).unwrap_or(CHUNK));
                if take == 0 {
                    return None;
                }
                let rest = bytes.split_off(take);
                let chunk = std::mem::replace(bytes, rest);
                if bytes.is_empty() {
                    self.queue.pop_front();
                }
                let mut out = Vec::with_capacity(8 + chunk.len());
                out.extend_from_slice(&self.sent.to_be_bytes());
                out.extend_from_slice(&chunk);
                self.sent += chunk.len() as u64;
                self.chunk_ends.push_back(self.sent);
                self.queued_bytes -= chunk.len();
                Some(Queued::Bytes(out))
            }
        }
    }

    /// Records an acknowledgement, or `None` for one past what was sent or
    /// before the last.
    pub fn ack(&mut self, offset: u64) -> Option<Ack> {
        if offset > self.sent || offset < self.acked {
            return None;
        }
        if offset == self.acked {
            return Some(Ack::Duplicate);
        }
        self.acked = offset;
        let mut ack = Ack::Partial;
        while self.chunk_ends.front().is_some_and(|&end| end <= offset) {
            self.chunk_ends.pop_front();
            ack = Ack::Chunk;
        }
        Some(ack)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn drain(credit: &mut Credit) -> Vec<Queued> {
        std::iter::from_fn(|| credit.pop()).collect()
    }

    fn offset_of(chunk: &[u8]) -> u64 {
        u64::from_be_bytes(chunk[..8].try_into().unwrap())
    }

    #[test]
    fn inbound_is_three_small_shapes_and_nothing_else() {
        assert_eq!(
            Inbound::parse(r#"{"t":"hello","cols":80,"rows":24}"#),
            Some(Inbound::Hello { cols: 80, rows: 24 })
        );
        assert_eq!(
            Inbound::parse(r#"{"t":"ack","offset":7}"#),
            Some(Inbound::Ack { offset: 7 })
        );
        for refused in [
            r#"{"t":"input","data":"ls\r"}"#,
            r#"{"t":"ack","offset":7,"data":"x"}"#,
            r#"{"t":"resize","cols":70000,"rows":1}"#,
            "not json",
        ] {
            assert_eq!(Inbound::parse(refused), None, "{refused}");
        }
    }

    #[test]
    fn sizes_are_held_to_the_browser_grid() {
        assert_eq!(clamp(1000, 1000), (MAX_COLS, MAX_ROWS));
        assert_eq!(clamp(0, 0), (2, 2));
        assert_eq!(clamp(80, 24), (80, 24));
    }

    #[test]
    fn no_more_than_the_window_is_ever_in_flight() {
        let mut credit = Credit::default();
        assert!(credit.push_bytes(&vec![b'x'; 3 * WINDOW as usize]));
        let sent = drain(&mut credit);
        assert_eq!(credit.in_flight(), WINDOW);
        assert_eq!(sent.len(), WINDOW as usize / CHUNK);
        for q in &sent {
            let Queued::Bytes(b) = q else { panic!("text") };
            assert!(b.len() - 8 <= CHUNK);
        }
        assert!(credit.pop().is_none(), "sent past the window");
        assert_eq!(credit.ack(CHUNK as u64), Some(Ack::Chunk));
        let Some(Queued::Bytes(next)) = credit.pop() else {
            panic!("an ack opened no credit");
        };
        assert_eq!(offset_of(&next), WINDOW);
        assert_eq!(credit.in_flight(), WINDOW);
    }

    #[test]
    fn a_resize_keeps_its_place_between_the_bytes() {
        let mut credit = Credit::default();
        assert!(credit.push_bytes(b"old"));
        credit.push_text(&Outbound::Resize {
            cols: 10,
            rows: 5,
            at: credit.position(),
        });
        assert!(credit.push_bytes(b"new"));
        let got = drain(&mut credit);
        assert!(matches!(&got[0], Queued::Bytes(b) if &b[8..] == b"old"));
        assert!(matches!(&got[1], Queued::Text(t) if t.contains(r#""at":3"#)));
        assert!(matches!(&got[2], Queued::Bytes(b) if offset_of(b) == 3 && &b[8..] == b"new"));
    }

    #[test]
    fn a_lagging_viewer_and_a_lying_ack_are_caught() {
        let mut credit = Credit::default();
        assert!(credit.push_bytes(&vec![0; BACKLOG]));
        assert!(!credit.push_bytes(b"x"));
        let mut credit = Credit::default();
        assert!(credit.push_bytes(b"abc"));
        drain(&mut credit);
        assert_eq!(credit.ack(4), None);
        assert_eq!(credit.ack(2), Some(Ack::Partial));
        assert_eq!(credit.ack(1), None);
        assert_eq!(credit.ack(2), Some(Ack::Duplicate));
        assert_eq!(credit.ack(3), Some(Ack::Chunk));
    }

    proptest! {
        // Each case moves megabytes; fewer cases keep it to seconds.
        #![proptest_config(ProptestConfig::with_cases(32))]
        /// Whatever the pushes and acks, the chunks reassemble to exactly
        /// the bytes pushed, at contiguous offsets, never past the window.
        /// UTF-8 and escapes split across chunks are the parser's to join.
        #[test]
        fn chunks_reassemble_the_stream(
            pushes in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..400_000), 1..8),
            // Past the window's 8 chunks, the window is what stops a drain.
            ack_every in 1usize..20,
        ) {
            let mut credit = Credit::default();
            let mut want = Vec::new();
            let mut got = Vec::new();
            let mut n = 0;
            for p in &pushes {
                want.extend_from_slice(p);
                prop_assert!(credit.push_bytes(p));
                while let Some(q) = credit.pop() {
                    let Queued::Bytes(b) = q else { unreachable!() };
                    prop_assert_eq!(offset_of(&b), got.len() as u64);
                    prop_assert!(b.len() - 8 <= CHUNK);
                    got.extend_from_slice(&b[8..]);
                    prop_assert!(credit.in_flight() <= WINDOW);
                    n += 1;
                    if n % ack_every == 0 {
                        prop_assert_eq!(credit.ack(got.len() as u64).map(|_| ()), Some(()));
                    }
                }
                prop_assert_eq!(credit.ack(got.len() as u64).map(|_| ()), Some(()));
            }
            while let Some(Queued::Bytes(b)) = credit.pop() {
                got.extend_from_slice(&b[8..]);
                prop_assert_eq!(credit.ack(got.len() as u64).map(|_| ()), Some(()));
            }
            prop_assert_eq!(got, want);
        }
    }
}
