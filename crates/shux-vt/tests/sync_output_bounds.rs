//! Resource bounds on synchronized output (issue #115).
//!
//! `CSI ?2026h` / `CSI ?2026l` is sixteen bytes a pane chooses to emit. It
//! asks the terminal to keep showing the frame it is already showing until the
//! pane is finished redrawing, which shux honoured by copying the whole grid —
//! scrollback and all — the instant the mode opened. On a 240x64 pane holding
//! 5000 lines of history that was 29 MB per toggle, taken inside the
//! daemon-wide pane-IO mutex, so the bill went to every other pane in every
//! other session. These tests pin what sixteen bytes may buy.
//!
//! Allocator traffic is the metric, not wall clock: it is exact, and it is the
//! quantity that was unbounded. A counting global allocator makes the
//! assertions deterministic under CI load, which timing never is.
//!
//! Two independent properties are asserted, because the fix has two halves and
//! either could regress alone:
//!
//! 1. **A window that changes nothing copies nothing.** The snapshot is taken
//!    by the first write that would change the presented frame, so a window
//!    that never writes never takes one.
//! 2. **A snapshot that IS taken costs lines, not cells.** Rows are shared
//!    copy-on-write, so copying a grid walks one pointer per line instead of
//!    every cell of every line — and the cost stops depending on how wide the
//!    pane is or how much history it holds.

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
const SCROLLBACK: usize = 5_000;

/// A private-mode pair with the same parse shape as a synchronized-output
/// toggle, that touches no buffer at all. Parsing a CSI sequence costs a few
/// small allocations inside `vte` itself; that floor is not what issue #115 is
/// about, so every bound here is stated RELATIVE to it. Comparing against a
/// control also means the tests keep working if that floor ever changes.
const INERT_TOGGLE: &[u8] = b"\x1b[?1000h\x1b[?1000l";

const SYNC_TOGGLE: &[u8] = b"\x1b[?2026h\x1b[?2026l";

/// A pane that has been used: `history` lines of scrollback, real content on
/// screen. Every cell is written, so nothing here is a blank-page special case.
fn used_pane(rows: usize, cols: usize, history: usize) -> VirtualTerminal {
    let mut vt = VirtualTerminal::new(rows, cols);
    let line = "x".repeat(cols - 1);
    for _ in 0..history {
        vt.process(line.as_bytes());
        vt.process(b"\r\n");
    }
    vt
}

/// Allocator traffic per repetition of `seq`, minus the CSI-parsing floor.
/// `csi_count` is how many CSI sequences `seq` contains, so the one-pair
/// control can be scaled to match.
fn excess_over_inert(rows: usize, cols: usize, seq: &[u8], csi_count: usize) -> (i64, i64) {
    let floor = cost(
        &mut used_pane(rows, cols, SCROLLBACK),
        TOGGLES,
        INERT_TOGGLE,
    );
    let measured = cost(&mut used_pane(rows, cols, SCROLLBACK), TOGGLES, seq);
    let scale = (csi_count / 2).max(1) as i64;
    (
        measured.calls as i64 - floor.calls as i64 * scale,
        measured.bytes as i64 - floor.bytes as i64 * scale,
    )
}

// ---------------------------------------------------------------------------
// 1. A window that changes nothing copies nothing
// ---------------------------------------------------------------------------

/// The whole of issue #115: opening and closing a synchronized-output window
/// without drawing anything must cost no more than parsing the two escape
/// sequences involved. Before the fix it cost a copy of the entire grid — 29 MB
/// and 5,010 allocations for sixteen bytes of pane output, inside the
/// daemon-wide pane-IO mutex.
#[test]
fn sync_toggling_costs_no_more_than_parsing_it() {
    let (calls, bytes) = excess_over_inert(ROWS, COLS, SYNC_TOGGLE, 2);
    assert!(
        calls <= 0,
        "{TOGGLES} synchronized-output toggles allocated {calls} times and {bytes} bytes \
         beyond the cost of parsing the same number of inert mode changes; the presented \
         frame must not be copied by a window that never changes it"
    );
}

/// Repeating `?2026h` while a window is already open, and `?2026l` while none
/// is, are both no-ops a pane can emit as fast as it likes.
#[test]
fn redundant_sync_mode_changes_cost_no_more_than_parsing_them() {
    let (calls, bytes) = excess_over_inert(
        ROWS,
        COLS,
        b"\x1b[?2026h\x1b[?2026h\x1b[?2026l\x1b[?2026l",
        4,
    );
    assert!(
        calls <= 0,
        "repeated sync mode changes allocated {calls} times / {bytes} bytes beyond parsing"
    );
}

/// The precision requirement. Sequences that parse INSIDE an open window but
/// change nothing presented — a cursor-position report, a mouse-mode toggle, an
/// unhandled private mode — must not be usable to re-arm the copy. A hook
/// placed on the parser's callbacks rather than on the presented state itself
/// would take a full snapshot for every one of these.
#[test]
fn inert_traffic_inside_a_sync_window_takes_no_snapshot() {
    let (calls, bytes) = excess_over_inert(
        ROWS,
        COLS,
        b"\x1b[?2026h\x1b[6n\x1b[?1000h\x1b[?1000l\x1b[?9999h\x1b[?2026l",
        12,
    );
    assert!(
        calls <= 0,
        "inert traffic inside a sync window allocated {calls} times / {bytes} bytes beyond \
         parsing; the snapshot hook is firing on sequences that change nothing presented"
    );
}

/// A pane can also open a window and then reset the terminal. RIS throws the
/// frozen presentation away, so freezing on the way through it would be a full
/// grid copy taken only to be dropped.
#[test]
fn sync_then_full_reset_takes_no_snapshot() {
    let mut inert = used_pane(ROWS, COLS, 0);
    let mut synced = used_pane(ROWS, COLS, 0);
    // RIS clears scrollback, so history cannot be part of this comparison;
    // both arms run the identical RIS, and only the `?2026h` differs.
    let floor = cost(&mut inert, TOGGLES, b"\x1b[?1000h\x1bc");
    let measured = cost(&mut synced, TOGGLES, b"\x1b[?2026h\x1bc");
    assert!(
        measured.calls as i64 - floor.calls as i64 <= 0,
        "`ESC[?2026h ESC c` allocated {} times / {} bytes against {} / {} for the same shape \
         with an inert mode; RIS must release the freeze before it touches anything",
        measured.calls,
        measured.bytes,
        floor.calls,
        floor.bytes,
    );
}

/// The property that actually protects the daemon. The bug scaled with
/// retained history: a pane with a full scrollback cost 5,024 lines of copying
/// per toggle and an empty one cost 64. Cost must now be the same.
#[test]
fn sync_toggle_cost_is_independent_of_scrollback_depth() {
    let empty = cost(&mut used_pane(ROWS, COLS, 0), TOGGLES, SYNC_TOGGLE);
    let full = cost(&mut used_pane(ROWS, COLS, SCROLLBACK), TOGGLES, SYNC_TOGGLE);
    assert_eq!(
        (empty.calls, empty.bytes),
        (full.calls, full.bytes),
        "toggling a pane with no scrollback cost {} allocations / {} bytes but one holding \
         {SCROLLBACK} lines cost {} / {}; the freeze is still scaling with retained history",
        empty.calls,
        empty.bytes,
        full.calls,
        full.bytes,
    );
}

/// Same property against screen size rather than history. A full-screen pane on
/// a large display was the worst case; it must now cost what a small one does.
#[test]
fn sync_toggle_cost_is_independent_of_pane_size() {
    let small = cost(&mut used_pane(24, 80, SCROLLBACK), TOGGLES, SYNC_TOGGLE);
    let large = cost(&mut used_pane(ROWS, COLS, SCROLLBACK), TOGGLES, SYNC_TOGGLE);
    assert_eq!(
        (small.calls, small.bytes),
        (large.calls, large.bytes),
        "toggling a 24x80 pane cost {} allocations / {} bytes but a {ROWS}x{COLS} pane cost \
         {} / {}; allocation is still scaling with the grid",
        small.calls,
        small.bytes,
        large.calls,
        large.bytes,
    );
}

// ---------------------------------------------------------------------------
// 2. A snapshot that IS taken costs lines, not cells
// ---------------------------------------------------------------------------

/// A window that draws legitimately takes one snapshot — that is what
/// synchronized output is for. What must NOT still be true is that the
/// snapshot copies every cell of every retained line: with rows shared
/// copy-on-write it copies one pointer per line plus the rows actually
/// written, so widening the pane fourfold must not multiply the cost.
///
/// Stated as a ratio rather than an absolute so it does not have to be
/// re-tuned every time `Cell` changes size. Before the fix this ratio was 3.0,
/// dead in line with the 3x cell count. It cannot be 1.0 either: the one row
/// the window writes is a row of cells, and a row of cells does scale with
/// width. What must NOT scale is the snapshot around it.
#[test]
fn a_written_sync_window_does_not_scale_with_pane_width() {
    let seq = b"\x1b[?2026ha\x1b[?2026l";
    let narrow = cost(&mut used_pane(ROWS, 80, SCROLLBACK), TOGGLES, seq);
    let wide = cost(&mut used_pane(ROWS, COLS, SCROLLBACK), TOGGLES, seq);
    let ratio = wide.bytes as f64 / narrow.bytes.max(1) as f64;
    assert!(
        ratio < 2.5,
        "a written sync window cost {} bytes at 80 columns and {} bytes at {COLS} columns \
         (ratio {ratio:.2}); tripling the width tripled the cost, so the snapshot is \
         copying cells rather than line pointers",
        narrow.bytes,
        wide.bytes,
    );
}

/// The same shape against history, and the sharpest bound in the file:
/// **what one extra retained line adds to one written window.**
///
/// A shared line costs a pointer. A deep-copied line costs all of its cells.
/// Stated per line rather than as a ratio because a ratio hides the answer:
/// ten times the lines costing ten times the bytes looks the same whether each
/// line is 16 bytes or 5,760, and only one of those is a bug.
#[test]
fn a_retained_line_costs_a_pointer_not_its_cells() {
    let seq = b"\x1b[?2026ha\x1b[?2026l";
    let shallow = cost(&mut used_pane(ROWS, COLS, 500), TOGGLES, seq);
    let deep = cost(&mut used_pane(ROWS, COLS, SCROLLBACK), TOGGLES, seq);
    let per_line =
        deep.bytes.saturating_sub(shallow.bytes) as f64 / (TOGGLES * (SCROLLBACK - 500)) as f64;
    // One `Row` is a pointer plus a flag. A row of COLS cells is COLS * 24
    // bytes — over 350 times as much — so any per-cell copying lands miles
    // outside this bound and cannot be confused with allocator rounding.
    assert!(
        per_line < 64.0,
        "each extra retained line added {per_line:.1} bytes to one written sync window \
         ({} bytes over 500 lines vs {} over {SCROLLBACK}); a shared line costs a pointer, \
         so anything near {} means whole rows are still being deep-copied",
        shallow.bytes,
        deep.bytes,
        COLS * 24,
    );
}

/// Copy-on-write must copy on write and not before: a window that writes to
/// one row must not pay for rows it never touched.
#[test]
fn a_written_sync_window_copies_only_the_rows_it_writes() {
    let one_row = cost(
        &mut used_pane(ROWS, COLS, SCROLLBACK),
        TOGGLES,
        b"\x1b[?2026h\x1b[1;1Ha\x1b[?2026l",
    );
    let twenty_rows = {
        let mut seq: Vec<u8> = b"\x1b[?2026h".to_vec();
        for row in 1..=20 {
            seq.extend_from_slice(format!("\x1b[{row};1Ha").as_bytes());
        }
        seq.extend_from_slice(b"\x1b[?2026l");
        cost(&mut used_pane(ROWS, COLS, SCROLLBACK), TOGGLES, &seq)
    };
    assert!(
        twenty_rows.bytes > one_row.bytes,
        "writing 20 rows inside a sync window cost {} bytes and writing 1 cost {}; \
         copy-on-write is not observable at all here, so this bound proves nothing",
        twenty_rows.bytes,
        one_row.bytes,
    );
    // 19 extra rows of COLS cells each, plus allocator rounding. If the whole
    // grid were being re-copied per row, this would be 19 whole grids.
    let extra_per_row = (twenty_rows.bytes - one_row.bytes) / (TOGGLES as u64 * 19);
    assert!(
        extra_per_row < (COLS * 64) as u64,
        "each additional written row inside a sync window cost {extra_per_row} bytes; \
         a single row of {COLS} cells is the bound, so the snapshot is being retaken"
    );
}

/// The sharpest bound the viewport-only freeze buys, and the one that would
/// have caught the whole defect on its own: **taking a snapshot must cost the
/// same whether the pane holds 500 lines of history or 5,000.**
///
/// The other scrollback bound above measures a window that never writes, so it
/// passes trivially once the freeze is deferred. This one forces the snapshot
/// to actually be taken and then varies only the history behind it.
#[test]
fn a_taken_snapshot_does_not_scale_with_retained_history() {
    let seq = b"\x1b[?2026ha\x1b[?2026l";
    let shallow = cost(&mut used_pane(ROWS, COLS, 500), TOGGLES, seq);
    let deep = cost(&mut used_pane(ROWS, COLS, SCROLLBACK), TOGGLES, seq);
    let per_line =
        deep.bytes.saturating_sub(shallow.bytes) as f64 / (TOGGLES * (SCROLLBACK - 500)) as f64;
    assert!(
        per_line < 1.0,
        "each extra retained line added {per_line:.2} bytes to a window that DOES take a \
         snapshot ({} bytes over 500 lines vs {} over {SCROLLBACK}); the frozen frame is \
         holding history again",
        shallow.bytes,
        deep.bytes,
    );
}

/// A pane can scroll its entire history away inside one window. Every line the
/// frozen frame still holds is a line the live grid cannot recycle as it
/// scrolls, so it must allocate a replacement — which is why the frame holds
/// the viewport and nothing else. Bounded against the identical scroll with no
/// window open, because the scrolling itself is work the pane can do anyway
/// and is not what this is about.
#[test]
fn a_window_that_scrolls_all_history_costs_little_more_than_the_scroll_alone() {
    let sweep = |open: &[u8], close: &[u8]| {
        let mut seq: Vec<u8> = open.to_vec();
        for _ in 0..80 {
            seq.extend_from_slice(format!("\x1b[{ROWS}S").as_bytes());
        }
        seq.extend_from_slice(close);
        cost(&mut used_pane(ROWS, COLS, SCROLLBACK), 200, &seq)
    };
    let synced = sweep(b"\x1b[?2026h", b"\x1b[?2026l");
    let plain = sweep(b"\x1b[?1000h", b"\x1b[?1000l");
    // The window can only add what the frame it froze is worth. Every row the
    // frozen frame holds must be replaced as the scroll recycles it — but the
    // frame is the VIEWPORT, so that is one screen, once, however many
    // thousands of lines are scrolled through it. Stated as an absolute
    // difference rather than a ratio: a ratio would pass just as happily if
    // both sides grew, and it is the extra that is the bug.
    let one_screen = (ROWS * COLS * std::mem::size_of::<shux_vt::Cell>()) as u64;
    let extra_per_window = synced.bytes.saturating_sub(plain.bytes) / 200;
    assert!(
        extra_per_window < one_screen * 2,
        "scrolling {} lines inside a synchronized window added {extra_per_window} bytes per \
         window over the identical scroll with no window open, against a screen's \
         {one_screen} ({} vs {} in total); the frozen frame is pinning history, so the live \
         grid is allocating a replacement for every line it scrolls",
        80 * ROWS,
        synced.bytes,
        plain.bytes,
    );
}

/// A full-screen clear inside a window rewrites every visible row, and every
/// visible row is one the frozen frame is holding — so one screen is copied.
/// That is the irreducible cost of freezing a frame and it must stay ONE
/// screen: not one screen per retained line, and not one whole grid.
#[test]
fn a_full_screen_clear_inside_a_window_copies_one_screen_not_one_grid() {
    let seq = b"\x1b[?2026h\x1b[2J\x1b[?2026l";
    let cleared = cost(&mut used_pane(ROWS, COLS, SCROLLBACK), TOGGLES, seq);
    let one_screen = (ROWS * COLS * std::mem::size_of::<shux_vt::Cell>()) as u64;
    let per_window = cleared.bytes / TOGGLES as u64;
    assert!(
        per_window < one_screen * 2,
        "clearing the screen inside a window cost {per_window} bytes against a screen's \
         {one_screen}; the snapshot is bigger than the frame it froze"
    );
}

// ---------------------------------------------------------------------------
// 3. Nothing is retained
// ---------------------------------------------------------------------------

/// The fix must not become a cache. Ten times the windows must cost ten times
/// one window and not one byte of steady-state growth, or a pane could inflate
/// daemon memory just by opening and closing sync windows.
#[test]
fn repeated_sync_windows_do_not_retain() {
    let mut vt = used_pane(ROWS, COLS, SCROLLBACK);
    let seq = b"\x1b[?2026ha\x1b[?2026l";
    let small = cost(&mut vt, 100, seq);
    let large = cost(&mut vt, 1_000, seq);
    let small_per = small.bytes as f64 / 100.0;
    let large_per = large.bytes as f64 / 1_000.0;
    assert!(
        (large_per - small_per).abs() / small_per.max(1.0) < 0.05,
        "bytes per sync window moved from {small_per} at 100 windows to {large_per} at 1000; \
         the presentation buffer is behaving like a growing cache"
    );
}

// ---------------------------------------------------------------------------
// Guards: these bounds must be capable of failing
// ---------------------------------------------------------------------------

/// Every bound above is an upper bound, and an upper bound is vacuous if the
/// thing it measures is invisible. A sync window that writes DOES take a
/// snapshot, and that snapshot must show up in the counters — if it ever stops
/// doing so, the allocator has stopped observing this crate and none of the
/// numbers above mean anything.
#[test]
fn a_written_sync_window_is_visible_to_the_counters() {
    let mut vt = used_pane(ROWS, COLS, SCROLLBACK);
    let quiet = cost(&mut vt, TOGGLES, SYNC_TOGGLE);
    let written = cost(&mut vt, TOGGLES, b"\x1b[?2026ha\x1b[?2026l");
    assert!(
        written.bytes > quiet.bytes * 10,
        "a sync window that writes cost {} bytes and one that does not cost {}; the \
         snapshot is not being measured, so every bound in this file is vacuous",
        written.bytes,
        quiet.bytes,
    );
}

/// The other direction: feeding nothing at all must cost nothing. A harness
/// that reports allocations for an empty input is measuring its own loop.
#[test]
fn an_empty_input_costs_nothing() {
    let mut vt = used_pane(ROWS, COLS, SCROLLBACK);
    let empty = cost(&mut vt, TOGGLES, b"");
    assert_eq!(
        (empty.calls, empty.bytes),
        (0, 0),
        "feeding {TOGGLES} empty chunks allocated {} times / {} bytes; the measurement \
         harness has a cost of its own and every bound here is offset by it",
        empty.calls,
        empty.bytes,
    );
}

/// And the control itself must be non-trivial: if `INERT_TOGGLE` ever became
/// free, every `excess_over_inert` bound would silently harden into `<= 0`
/// against a floor of zero and start failing for the wrong reason — or, worse,
/// stop being a relative bound at all.
#[test]
fn the_inert_control_is_not_free() {
    let mut vt = used_pane(ROWS, COLS, SCROLLBACK);
    let floor = cost(&mut vt, TOGGLES, INERT_TOGGLE);
    assert!(
        floor.calls > 0,
        "parsing {TOGGLES} inert mode toggles allocated nothing; the control these bounds \
         are stated against has vanished"
    );
}
