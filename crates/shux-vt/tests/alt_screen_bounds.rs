//! Resource bounds on alternate-screen switching (issue #106).
//!
//! `ESC[?1049h` / `ESC[?1049l` is eight bytes a pane chooses to emit. The
//! switch runs inside the daemon-wide `PaneIoState` mutex, so whatever those
//! eight bytes cost is paid by every OTHER pane in every OTHER session. These
//! tests pin what one toggle may buy.
//!
//! Allocator traffic is the metric, not wall clock: it is exact, and it is the
//! quantity that was unbounded. A counting global allocator makes the
//! assertions deterministic under CI load, which timing never is.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell as StdCell;

use shux_vt::VirtualTerminal;

// ---------------------------------------------------------------------------
// Counting allocator
// ---------------------------------------------------------------------------

/// Counters are THREAD-LOCAL, not global. `cargo test` runs test functions on
/// several threads in one process, and a global counter would tally every
/// other test's allocations into whichever measurement happened to be armed —
/// which produces exactly the kind of quietly-wrong number a bound like this
/// exists to prevent. Const-initialised `Cell`s so that touching the counters
/// from inside the allocator cannot itself allocate.
struct Counting;

thread_local! {
    static ARMED: StdCell<bool> = const { StdCell::new(false) };
    static CALLS: StdCell<u64> = const { StdCell::new(0) };
    static BYTES: StdCell<u64> = const { StdCell::new(0) };
}

/// Record an allocation of `size` on this thread, if this thread is measuring.
/// `try_with` because TLS is unavailable while a thread is being torn down,
/// and an allocation there must not abort the process.
fn record(size: usize) {
    let armed = ARMED.try_with(StdCell::get).unwrap_or(false);
    if !armed {
        return;
    }
    let _ = CALLS.try_with(|c| c.set(c.get() + 1));
    let _ = BYTES.try_with(|b| b.set(b.get() + size as u64));
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size > layout.size() {
            record(new_size - layout.size());
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[derive(Debug, Clone, Copy)]
struct Cost {
    calls: u64,
    bytes: u64,
}

fn arm() {
    CALLS.with(|c| c.set(0));
    BYTES.with(|b| b.set(0));
    ARMED.with(|a| a.set(true));
}

fn disarm() -> Cost {
    ARMED.with(|a| a.set(false));
    Cost {
        calls: CALLS.with(StdCell::get),
        bytes: BYTES.with(StdCell::get),
    }
}

/// Allocator traffic caused by feeding `chunk` to `vt` `iters` times.
fn cost(vt: &mut VirtualTerminal, iters: usize, chunk: &[u8]) -> Cost {
    // Warm: the first pass through any sequence pays one-time parser costs.
    vt.process(chunk);
    arm();
    for _ in 0..iters {
        vt.process(chunk);
    }
    disarm()
}

const ROWS: usize = 64;
const COLS: usize = 240;
const TOGGLES: usize = 2_000;

/// A private-mode pair with the same parse shape as an alternate-screen
/// toggle, that touches no buffer at all. Parsing a CSI sequence costs a few
/// small allocations inside `vte` itself; that floor is not what issue #106 is
/// about, so every bound here is stated RELATIVE to it. Comparing against a
/// control also means the tests keep working if that floor ever changes.
const INERT_TOGGLE: &[u8] = b"\x1b[?1000h\x1b[?1000l";

/// A pane that has been used: full scrollback, real content on screen.
fn used_pane(rows: usize, cols: usize) -> VirtualTerminal {
    let mut vt = VirtualTerminal::new(rows, cols);
    let line = "x".repeat(cols - 1);
    for _ in 0..5_000 {
        vt.process(line.as_bytes());
        vt.process(b"\r\n");
    }
    vt
}

/// Allocator traffic per repetition of `seq`, minus the CSI-parsing floor.
fn excess_over_inert(rows: usize, cols: usize, seq: &[u8], csi_count: usize) -> (i64, i64) {
    let floor = cost(&mut used_pane(rows, cols), TOGGLES, INERT_TOGGLE);
    let measured = cost(&mut used_pane(rows, cols), TOGGLES, seq);
    // The control is one CSI pair; scale it to the sequence's CSI count.
    let scale = (csi_count / 2).max(1) as i64;
    (
        measured.calls as i64 - floor.calls as i64 * scale,
        measured.bytes as i64 - floor.bytes as i64 * scale,
    )
}

// ---------------------------------------------------------------------------
// The bound
// ---------------------------------------------------------------------------

/// The whole of issue #106: entering and leaving the alternate screen must
/// cost no more than parsing the two escape sequences involved. Before the fix
/// every enter built a fresh `ROWS x COLS` grid — 372 KB and 73 allocations for
/// eight bytes of pane output, inside the daemon-wide pane-IO mutex.
#[test]
fn alt_screen_toggling_costs_no_more_than_parsing_it() {
    for seq in [&b"\x1b[?1049h\x1b[?1049l"[..], &b"\x1b[?1047h\x1b[?1047l"[..]] {
        let (calls, bytes) = excess_over_inert(ROWS, COLS, seq, 2);
        assert!(
            calls <= 0,
            "{TOGGLES} toggles of {seq:?} allocated {calls} times and {bytes} bytes \
             beyond the cost of parsing the same number of inert mode changes; \
             a retired alternate-screen buffer must be reused, not rebuilt"
        );
    }
}

/// Mixing the two alternate-screen mode numbers must not defeat the reuse:
/// 1047 and 1049 differ only in cursor handling, and a pane can interleave
/// them freely.
#[test]
fn mixed_1047_1049_toggling_costs_no_more_than_parsing_it() {
    let (calls, bytes) = excess_over_inert(
        ROWS,
        COLS,
        b"\x1b[?1047h\x1b[?1049l\x1b[?1049h\x1b[?1047l",
        4,
    );
    assert!(
        calls <= 0,
        "mixed 1047/1049 toggling allocated {calls} times / {bytes} bytes beyond parsing"
    );
}

/// A pane that actually DRAWS on the alternate screen still gets its buffer
/// reused: the retired grid is blanked in place rather than freed and
/// re-requested. The cell writes remain — a blank screen has to be blanked —
/// but the allocator traffic does not.
#[test]
fn drawing_on_the_alt_screen_still_reuses_the_buffer() {
    let (calls, bytes) = excess_over_inert(
        ROWS,
        COLS,
        b"\x1b[?1049h\x1b[10;10Hdrawing on the alternate screen\x1b[?1049l",
        4,
    );
    assert!(
        calls <= 0,
        "toggling with drawing allocated {calls} times / {bytes} bytes beyond parsing"
    );
}

/// The cost of a toggle must not depend on how big the pane is. This is the
/// property that actually protects the daemon: a full-screen pane on a large
/// display was the worst case, at 372 KB a toggle, and it must now cost the
/// same as a small one.
#[test]
fn toggle_cost_is_independent_of_pane_size() {
    let seq = &b"\x1b[?1049h\x1b[?1049l"[..];
    let small = cost(&mut used_pane(24, 80), TOGGLES, seq);
    let large = cost(&mut used_pane(ROWS, COLS), TOGGLES, seq);
    assert_eq!(
        (small.calls, small.bytes),
        (large.calls, large.bytes),
        "toggling a 24x80 pane cost {} allocations / {} bytes but a {ROWS}x{COLS} pane \
         cost {} / {}; allocation is still scaling with the grid",
        small.calls,
        small.bytes,
        large.calls,
        large.bytes,
    );
}

/// The reuse must not become an unbounded cache: exactly one retired buffer is
/// kept, so a pane cannot grow daemon memory by toggling. Ten times the
/// toggles, ten times the allocation would mean a leak; the same allocation
/// per toggle means a slot.
#[test]
fn retained_buffers_do_not_grow_with_toggle_count() {
    let mut vt = used_pane(ROWS, COLS);
    let small = cost(&mut vt, 100, b"\x1b[?1049h\x1b[?1049l");
    let large = cost(&mut vt, 1_000, b"\x1b[?1049h\x1b[?1049l");
    let small_per = small.bytes as f64 / 100.0;
    let large_per = large.bytes as f64 / 1_000.0;
    assert!(
        (large_per - small_per).abs() < 1.0,
        "bytes per toggle moved from {small_per} at 100 toggles to {large_per} at 1000; \
         the retired-buffer slot is behaving like a growing cache"
    );
}

// ---------------------------------------------------------------------------
// Guards: these bounds must be capable of failing
// ---------------------------------------------------------------------------

/// Entering the alternate screen for the FIRST time has no buffer to reuse and
/// must allocate a whole grid. If this ever stops holding, the counting
/// allocator has stopped observing this crate and every bound above is vacuous.
#[test]
fn the_first_enter_allocates_a_whole_grid() {
    let mut vt = VirtualTerminal::new(ROWS, COLS);
    arm();
    vt.process(b"\x1b[?1049h");
    let bytes = disarm().bytes;
    let want = (ROWS * COLS * std::mem::size_of::<shux_vt::Cell>()) as u64;
    assert!(
        bytes >= want / 2,
        "the first alternate-screen enter allocated only {bytes} bytes for a \
         {ROWS}x{COLS} grid (expected at least {}); the counting allocator is not \
         observing this crate and every bound in this file is vacuous",
        want / 2
    );
}

/// ...and the control really is a floor, not a coincidence: an inert mode
/// toggle must allocate something, or `excess_over_inert` is subtracting zero
/// and the assertions above would pass on a completely unfixed build.
#[test]
fn the_inert_control_allocates_something() {
    let c = cost(&mut used_pane(24, 80), TOGGLES, INERT_TOGGLE);
    assert!(
        c.calls > 0,
        "the inert control sequence allocated nothing, so the relative bounds \
         above are absolute ones in disguise"
    );
}
