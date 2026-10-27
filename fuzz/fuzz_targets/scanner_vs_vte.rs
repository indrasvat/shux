//! The raw-byte OSC 7501 / 133;A scanner frames exactly what vte frames,
//! whole, split once, and byte at a time.
#![no_main]
use libfuzzer_sys::fuzz_target;
use shux_vt::fuzzing::{reference_strings, scan};

fuzz_target!(|input: (u16, &[u8])| {
    let (split, bytes) = input;
    // Keep vte's own caps out of play: at most 16 params, a 4096-byte
    // retained buffer. Under 4000 bytes the scanner's wire limit cannot bind.
    if bytes.len() >= 4000 || bytes.iter().filter(|&&b| b == b';').count() >= 15 {
        return;
    }
    let expected = reference_strings(bytes);
    assert_eq!(scan(&[bytes]), expected, "whole");
    let at = usize::from(split) % (bytes.len() + 1);
    assert_eq!(scan(&[&bytes[..at], &bytes[at..]]), expected, "split at {at}");
    let singles: Vec<&[u8]> = bytes.chunks(1).collect();
    assert_eq!(scan(&singles), expected, "bytewise");
});
