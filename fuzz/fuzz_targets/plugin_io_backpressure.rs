//! Reply backlog: a flood written before any read, and a plugin that never reads.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    shux_plugin::io_fuzz::run_backpressure(data);
});
