//! Any bytes a daemon could find as its saved workspace (issue #329): decoding
//! never panics, and an accepted workspace rebuilds whole (see
//! `shux_core::persist::fuzz_one`).
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| shux_core::persist::fuzz_one(data));
