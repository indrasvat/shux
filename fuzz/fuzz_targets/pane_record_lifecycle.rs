//! Any sequence of overlapping record starts, output, stops, waits, owner
//! loss, faults and shutdown: the recording registry keeps its safety and
//! liveness properties (see `shux::pane_record_fuzz`).
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // libfuzzer-sys aborts on any panic, even one a task contains; the
    // interpreter injects writer panics the registry must survive. A broken
    // property still unwinds out of `run`, which libfuzzer-sys aborts on.
    static DEFAULT_HOOK: std::sync::Once = std::sync::Once::new();
    DEFAULT_HOOK.call_once(|| drop(std::panic::take_hook()));
    shux::pane_record_fuzz::run(data);
});
