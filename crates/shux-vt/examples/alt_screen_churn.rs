//! Measurement harness for issue #106: what does one alternate-screen
//! enter/leave toggle actually cost, and what can a pane buy with it?
//!
//! The acceptance criteria on #106 ask for a benchmark before and after, so
//! this reports allocator traffic (exact, reproducible) alongside wall clock
//! (indicative). Allocation bytes are the number that matters: the toggle
//! runs inside the daemon-wide `PaneIoState` mutex, so every byte of
//! allocate-zero-free work a pane can conjure there is work every *other*
//! pane waits on.
//!
//! Run via `make bench-alt-screen`. Emits one JSON object per scenario.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use shux_vt::{Cell, VirtualTerminal};

/// Counting allocator. Only counts while `ARMED` is set, so setup traffic
/// (building scrollback, warming the parser) stays out of the measurement.
struct Counting;

static ARMED: AtomicBool = AtomicBool::new(false);
static ALLOC_CALLS: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) && new_size > layout.size() {
            ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES.fetch_add((new_size - layout.size()) as u64, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

struct Measured {
    alloc_calls: u64,
    alloc_bytes: u64,
    seconds: f64,
    grid_writes: u64,
}

fn measure(vt: &mut VirtualTerminal, iters: usize, chunk: &[u8]) -> Measured {
    let writes_before = vt.grid().mutations();
    ALLOC_CALLS.store(0, Ordering::Relaxed);
    ALLOC_BYTES.store(0, Ordering::Relaxed);
    ARMED.store(true, Ordering::Relaxed);
    let started = Instant::now();
    for _ in 0..iters {
        vt.process(chunk);
    }
    let seconds = started.elapsed().as_secs_f64();
    ARMED.store(false, Ordering::Relaxed);
    Measured {
        alloc_calls: ALLOC_CALLS.load(Ordering::Relaxed),
        alloc_bytes: ALLOC_BYTES.load(Ordering::Relaxed),
        seconds,
        grid_writes: vt.grid().mutations().saturating_sub(writes_before),
    }
}

/// Fill the primary grid's scrollback so the measurement reflects a pane that
/// has actually been used, not a pristine one.
fn with_scrollback(rows: usize, cols: usize, lines: usize) -> VirtualTerminal {
    let mut vt = VirtualTerminal::new(rows, cols);
    let line = "x".repeat(cols.saturating_sub(1));
    for _ in 0..lines {
        vt.process(line.as_bytes());
        vt.process(b"\r\n");
    }
    vt
}

fn report(scenario: &str, rows: usize, cols: usize, iters: usize, seq: &[u8], m: Measured) {
    let input_bytes = (seq.len() * iters) as f64;
    println!(
        concat!(
            "{{",
            "\"scenario\":\"{scenario}\",",
            "\"rows\":{rows},\"cols\":{cols},\"toggles\":{iters},",
            "\"cell_size_bytes\":{cell},",
            "\"input_bytes\":{input},",
            "\"alloc_calls\":{calls},",
            "\"alloc_bytes\":{bytes},",
            "\"alloc_bytes_per_toggle\":{per_toggle:.1},",
            "\"alloc_bytes_per_input_byte\":{amp:.1},",
            "\"grid_writes\":{writes},",
            "\"seconds\":{secs:.6},",
            "\"toggles_per_second\":{tps:.0}",
            "}}"
        ),
        scenario = scenario,
        rows = rows,
        cols = cols,
        iters = iters,
        cell = std::mem::size_of::<Cell>(),
        input = input_bytes as u64,
        calls = m.alloc_calls,
        bytes = m.alloc_bytes,
        per_toggle = m.alloc_bytes as f64 / iters as f64,
        amp = m.alloc_bytes as f64 / input_bytes.max(1.0),
        writes = m.grid_writes,
        secs = m.seconds,
        tps = iters as f64 / m.seconds.max(f64::EPSILON),
    );
}

fn main() {
    let iters: usize = std::env::var("SHUX_ALT_CHURN_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000);

    // Geometries: a default 80x24 pane, and a full-screen pane on a large
    // display. Both are ordinary; neither is an attacker-chosen extreme,
    // because a pane cannot choose its own size.
    for &(rows, cols) in &[(24usize, 80usize), (64, 240)] {
        for &(scenario, seq) in &[
            ("alt_toggle_1049", &b"\x1b[?1049h\x1b[?1049l"[..]),
            ("alt_toggle_1047", &b"\x1b[?1047h\x1b[?1047l"[..]),
        ] {
            let mut vt = with_scrollback(rows, cols, 5_000);
            // Warm: first toggle pays one-time parser costs.
            vt.process(seq);
            let m = measure(&mut vt, iters, seq);
            report(scenario, rows, cols, iters, seq, m);
        }

        // Same defect class, different sequence: DEC 2026 synchronized output
        // snapshots the presented frame by cloning the WHOLE grid, scrollback
        // included, so its per-toggle cost scales with retained history rather
        // than screen size (issue #115).
        {
            let seq = &b"\x1b[?2026h\x1b[?2026l"[..];
            let mut vt = with_scrollback(rows, cols, 5_000);
            vt.process(seq);
            let m = measure(&mut vt, iters / 20, seq);
            report("sync_toggle_2026", rows, cols, iters / 20, seq, m);
        }

        // The sharp version of the same attack. Deferring the snapshot alone
        // would leave this one untouched: the interleaved character IS a
        // change to the presented frame, so it legitimately takes the copy.
        // What makes it survivable is that the copy itself no longer walks the
        // scrollback's cells. Reported separately so a regression in EITHER
        // half of the fix has its own number.
        {
            let seq = &b"\x1b[?2026ha\x1b[?2026l"[..];
            let mut vt = with_scrollback(rows, cols, 5_000);
            vt.process(seq);
            let m = measure(&mut vt, iters / 20, seq);
            report(
                "sync_toggle_2026_with_write",
                rows,
                cols,
                iters / 20,
                seq,
                m,
            );
        }

        // Adversarial review, issue #115: a window that SCROLLS the whole
        // retained history. Every recycled line is a line the frozen frame
        // still holds, so each one unshares. This is the most expensive thing
        // a pane can do inside one window, and it is what the per-window
        // ceiling has to be judged on — not the empty toggle.
        {
            let mut seq: Vec<u8> = b"\x1b[?2026h".to_vec();
            for _ in 0..80 {
                seq.extend_from_slice(format!("\x1b[{rows}S").as_bytes());
            }
            seq.extend_from_slice(b"\x1b[?2026l");
            let mut vt = with_scrollback(rows, cols, 5_000);
            vt.process(&seq);
            let m = measure(&mut vt, iters / 400, &seq);
            report(
                "sync_scroll_all_history_2026",
                rows,
                cols,
                iters / 400,
                &seq,
                m,
            );
        }

        // The same scroll with NO window open: the floor this costs anyway.
        {
            let mut seq: Vec<u8> = b"\x1b[?1000h".to_vec();
            for _ in 0..80 {
                seq.extend_from_slice(format!("\x1b[{rows}S").as_bytes());
            }
            seq.extend_from_slice(b"\x1b[?1000l");
            let mut vt = with_scrollback(rows, cols, 5_000);
            vt.process(&seq);
            let m = measure(&mut vt, iters / 400, &seq);
            report(
                "scroll_all_history_no_sync",
                rows,
                cols,
                iters / 400,
                &seq,
                m,
            );
        }

        // A full-screen clear inside a window: every visible row unshares.
        {
            let seq = &b"\x1b[?2026h\x1b[2J\x1b[?2026l"[..];
            let mut vt = with_scrollback(rows, cols, 5_000);
            vt.process(seq);
            let m = measure(&mut vt, iters / 20, seq);
            report("sync_clear_screen_2026", rows, cols, iters / 20, seq, m);
        }

        // Sequences that PARSE inside an open synchronized-output window but
        // change nothing presented. A coarse "any callback re-arms the copy"
        // hook would pay a full snapshot for each of these; a precise one pays
        // nothing. Bytes chosen so the window stays open across the whole run.
        {
            let seq = &b"\x1b[?2026h\x1b[6n\x1b[?1000h\x1b[?1000l\x1b[?2026l"[..];
            let mut vt = with_scrollback(rows, cols, 5_000);
            vt.process(seq);
            let m = measure(&mut vt, iters / 20, seq);
            report("sync_inert_traffic_2026", rows, cols, iters / 20, seq, m);
        }

        // Baseline for scale: a pane printing one ordinary character.
        let mut vt = with_scrollback(rows, cols, 5_000);
        let m = measure(&mut vt, iters, b"a");
        report("plain_char_baseline", rows, cols, iters, b"a", m);
    }
}
