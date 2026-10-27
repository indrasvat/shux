//! Any payload: decoding never panics, and anything accepted re-encodes to
//! a payload that decodes to the same report.
#![no_main]
use libfuzzer_sys::fuzz_target;
use shux_program_status::wire::{Message, decode, encode_payload};

fuzz_target!(|data: &[u8]| {
    if let Ok(Message::Report(report)) = decode(data) {
        let again = decode(&encode_payload(&report));
        assert_eq!(again, Ok(Message::Report(report)));
    }
});
