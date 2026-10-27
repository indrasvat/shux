//! A terminal with program status on is indistinguishable from one with it
//! off, except for the 7501 probe replies, however the stream is chunked.
#![no_main]
use libfuzzer_sys::fuzz_target;
use shux_vt::{FrameEnvelope, MaskSet, VirtualTerminal};

const REPLY_PREFIX: &[u8] = b"\x1b]7501;?:";

fn drive(chunks: &[&[u8]], enabled: bool) -> (String, String, String, Option<String>, u64, Vec<Vec<u8>>) {
    let mut vt = VirtualTerminal::new(12, 40);
    vt.set_program_status_enabled(enabled);
    let mut responses = Vec::new();
    for c in chunks {
        responses.extend(vt.process_with_responses(c));
    }
    responses.retain(|r| !r.starts_with(REPLY_PREFIX));
    (
        FrameEnvelope::from_terminal(&vt, &MaskSet::new()).to_canonical_json(),
        vt.capture_text(None),
        format!("{:?}", vt.cursor()),
        vt.title().map(str::to_owned),
        vt.content_revision(),
        responses,
    )
}

fuzz_target!(|input: (u16, u16, &[u8])| {
    let (a, b, bytes) = input;
    if bytes.len() > 8192 {
        return;
    }
    let (mut i, mut j) = (usize::from(a) % (bytes.len() + 1), usize::from(b) % (bytes.len() + 1));
    if i > j {
        std::mem::swap(&mut i, &mut j);
    }
    let chunks = [&bytes[..i], &bytes[i..j], &bytes[j..]];
    assert_eq!(drive(&chunks, true), drive(&chunks, false));
});
