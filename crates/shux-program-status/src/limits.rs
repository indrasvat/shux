//! Spec §Limits. A report that breaks any of these is discarded whole.

/// Whole sequence, `ESC ]` through `ST`.
pub const MAX_SEQUENCE_BYTES: usize = 4096;
/// `ESC ] 7501 ;` plus the two-byte `ESC \` terminator.
const FRAMING_BYTES: usize = 9;
/// The payload after `7501;` in the longest-terminator case.
pub const MAX_PAYLOAD_BYTES: usize = MAX_SEQUENCE_BYTES - FRAMING_BYTES;

pub const MAX_KEY_BYTES: usize = 16;
pub const MAX_MSG_ENCODED: usize = 2732;
pub const MAX_MSG_DECODED: usize = 2048;
pub const MAX_TITLE_ENCODED: usize = 256;
pub const MAX_TITLE_DECODED: usize = 192;
pub const MAX_APP_BYTES: usize = 32;
pub const MAX_ID_BYTES: usize = 128;
pub const MAX_ID_SEGMENT_BYTES: usize = 32;
pub const MAX_ID_DEPTH: usize = 8;
/// Records one terminal keeps. The spec requires at least 64 and caps at 256.
pub const MAX_RECORDS: usize = 256;
