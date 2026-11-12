//! Random bursts, partial reads, stdout close, and kill against the plugin I/O split.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    shux_plugin::io_fuzz::run_interleave(data);
});
