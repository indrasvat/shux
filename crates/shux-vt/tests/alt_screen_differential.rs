//! Recycling a retired screen buffer must be UNOBSERVABLE (issue #106).
//!
//! The optimisation is only legitimate if a terminal that reuses its retired
//! alternate-screen buffer behaves identically to one that builds a fresh
//! buffer every time. That is a property over all inputs, not a handful of
//! cases, so it is tested as one: random escape-sequence programs are driven
//! through two terminals — recycling on, recycling off — and every observable
//! is compared after every step.
//!
//! The operation alphabet is deliberately weighted towards the sequences that
//! interact with the swap (alternate-screen entry and exit under both mode
//! numbers, RIS, synchronized output, resize, scrolling writes) rather than
//! uniform random bytes, which would spend almost all of its budget on
//! sequences that never reach this code.

use proptest::prelude::*;
use sha2::{Digest, Sha256};
use shux_vt::{FrameEnvelope, MaskSet, VirtualTerminal};

/// Canonical-JSON SHA-256 of a captured frame.
///
/// This used to be `shux_vt::capture_sha256`, which was never a virtual-terminal
/// concept — it was lens-gate vocabulary parked in this crate because a binary-only
/// `shux` could not export anything its integration tests could import (#150/#151).
/// It lives with the gate now. The digest is reproduced here rather than reached for
/// across a crate boundary: this test needs a compact, exact identity for a frame, and
/// nothing about that requirement belongs to the gate. `frame_stability_hash` is the
/// crate's own frame identity but is a 64-bit transient, and a differential test should
/// not trade collision resistance for brevity.
fn frame_digest(env: &FrameEnvelope) -> String {
    let mut hasher = Sha256::new();
    hasher.update(env.to_canonical_json().as_bytes());
    let mut out = String::with_capacity(64);
    for b in hasher.finalize() {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// One step of a generated program.
#[derive(Debug, Clone)]
enum Op {
    /// Raw bytes fed to the parser.
    Feed(&'static [u8]),
    /// Printable text, which advances the cursor and can scroll.
    Write(String),
    /// A daemon-driven resize. Panes cannot trigger these themselves, but the
    /// swap has to survive one landing at any point.
    Resize(usize, usize),
    /// Drain dirty regions, as a renderer would between frames.
    DrainDirty,
}

/// Everything a consumer of `VirtualTerminal` can see.
#[derive(Debug, PartialEq, Eq)]
struct Observed {
    /// Canonical hash of the PRESENTED frame: cells, styles, cursor, alt-screen
    /// flag, default colours, palette-override flag.
    frame: String,
    /// Scrollback is outside the presented frame but inside copy mode.
    scrollback: Vec<String>,
    scrollback_len: usize,
    total_lines: usize,
    rows: usize,
    cols: usize,
    /// The lens content revision, which the daemon publishes to watchers.
    content_revision: u64,
    title: Option<String>,
    alternate_screen: bool,
    /// Dirty regions drive partial redraw; a difference here is a rendering
    /// bug even when every cell agrees.
    dirty: Vec<(usize, usize, usize)>,
    /// Cursor presentation is drawn as an overlay, outside the frame hash's
    /// cell data, so it is compared explicitly too.
    cursor: (usize, usize, bool),
    /// The absolute index rows are numbered from. Everything above can agree
    /// while a recycled buffer counts from the grid it replaced, which is how
    /// a stale anchor resolves onto a live row of the next session.
    eviction_base: u64,
}

fn observe(vt: &mut VirtualTerminal) -> Observed {
    let env = FrameEnvelope::from_terminal(vt, &MaskSet::new());
    let grid = vt.grid();
    let scrollback = (0..grid.scrollback_len())
        .filter_map(|r| grid.scrollback_row(r))
        .map(|row| (0..row.len()).map(|c| row[c].ch).collect::<String>())
        .collect();
    Observed {
        frame: frame_digest(&env),
        scrollback,
        scrollback_len: grid.scrollback_len(),
        total_lines: grid.total_lines(),
        rows: grid.rows(),
        cols: grid.cols(),
        content_revision: vt.content_revision(),
        title: vt.title().map(str::to_string),
        alternate_screen: vt.is_alternate_screen(),
        cursor: (vt.cursor().row, vt.cursor().col, vt.cursor().visible),
        dirty: Vec::new(),
        eviction_base: vt.eviction_base(),
    }
}

fn drain_dirty(vt: &mut VirtualTerminal) -> Vec<(usize, usize, usize)> {
    vt.take_dirty_regions()
        .into_iter()
        .map(|d| (d.row, d.cols.start, d.cols.end))
        .collect()
}

fn apply(vt: &mut VirtualTerminal, op: &Op) -> Vec<(usize, usize, usize)> {
    match op {
        Op::Feed(bytes) => {
            vt.process(bytes);
            Vec::new()
        }
        Op::Write(text) => {
            vt.process(text.as_bytes());
            Vec::new()
        }
        Op::Resize(rows, cols) => {
            vt.resize(*rows, *cols);
            Vec::new()
        }
        Op::DrainDirty => drain_dirty(vt),
    }
}

/// Sequences that reach the alternate-screen swap, plus the neighbours that
/// have historically interacted with it badly.
const SEQUENCES: &[&[u8]] = &[
    b"\x1b[?1049h",
    b"\x1b[?1049l",
    b"\x1b[?1047h",
    b"\x1b[?1047l",
    b"\x1b[?1048h",
    b"\x1b[?1048l",
    b"\x1bc",       // RIS
    b"\x1b[?2026h", // synchronized output on
    b"\x1b[?2026l", // synchronized output off
    b"\x1b7",       // DECSC
    b"\x1b8",       // DECRC
    b"\x1b[2J",
    b"\x1b[3J",
    b"\x1b[H",
    b"\x1b[5;7H",
    b"\x1b[2;6r", // DECSTBM
    b"\x1b[?6h",  // origin mode
    b"\x1b[?6l",
    b"\x1b[?25l",
    b"\x1b[?25h",
    b"\x1b[1;38;2;200;40;40;48;5;236m", // truecolor fg + indexed bg
    b"\x1b[0m",
    b"\x1b]0;alt-screen differential\x07", // OSC title
    b"\x1b]11;#112233\x07",                // OSC dynamic default background
    b"\x1b[10S",
    b"\x1b[10T",
    b"\x1b[3L",
    b"\x1b[3M",
    b"\x1b#8", // DECALN
];

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        // Weighted towards escape sequences: they are what the swap sees.
        8 => (0..SEQUENCES.len()).prop_map(|i| Op::Feed(SEQUENCES[i])),
        3 => "[a-z ]{0,40}(\r\n)?".prop_map(Op::Write),
        1 => (1usize..12, 1usize..30).prop_map(|(r, c)| Op::Resize(r, c)),
        1 => Just(Op::DrainDirty),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]

    /// The property: recycling changes nothing an observer can see.
    #[test]
    fn recycling_a_retired_buffer_is_unobservable(ops in prop::collection::vec(op_strategy(), 1..60)) {
        let mut recycling = VirtualTerminal::new(8, 24);
        let mut reference = VirtualTerminal::new(8, 24);
        reference.set_retired_grid_reuse(false);

        for (step, op) in ops.iter().enumerate() {
            let dirty_recycling = apply(&mut recycling, op);
            let dirty_reference = apply(&mut reference, op);
            prop_assert_eq!(
                &dirty_recycling,
                &dirty_reference,
                "dirty regions diverged at step {} after {:?}",
                step,
                op
            );

            let mut a = observe(&mut recycling);
            let mut b = observe(&mut reference);
            a.dirty = dirty_recycling;
            b.dirty = dirty_reference;
            prop_assert_eq!(a, b, "state diverged at step {} after {:?}", step, op);
        }
    }
}

// ---------------------------------------------------------------------------
// Guard: the two arms of the differential must really run different code
// ---------------------------------------------------------------------------

/// A differential test whose two sides take the same code path proves nothing.
/// This one measures the difference directly: with recycling off, repeated
/// alternate-screen entries must keep asking the allocator for a grid; with it
/// on, they must not.
#[test]
fn the_two_arms_take_different_allocation_paths() {
    const ROWS: usize = 40;
    const COLS: usize = 160;
    const TOGGLES: usize = 200;

    fn toggle_bytes(reuse: bool) -> u64 {
        let mut vt = VirtualTerminal::new(ROWS, COLS);
        vt.set_retired_grid_reuse(reuse);
        vt.process(b"\x1b[?1049h\x1b[?1049l");
        alloc_probe::arm();
        for _ in 0..TOGGLES {
            vt.process(b"\x1b[?1049h\x1b[?1049l");
        }
        alloc_probe::disarm()
    }

    let rebuilding = toggle_bytes(false);
    let recycling = toggle_bytes(true);
    let one_grid = (ROWS * COLS * std::mem::size_of::<shux_vt::Cell>()) as u64;

    assert!(
        rebuilding >= one_grid * TOGGLES as u64 / 2,
        "the non-recycling reference allocated only {rebuilding} bytes over {TOGGLES} \
         toggles; it is not rebuilding, so the differential compares nothing"
    );
    assert!(
        recycling * 20 < rebuilding,
        "recycling allocated {recycling} bytes against the reference's {rebuilding}; \
         the recycling path is not actually recycling"
    );
}

/// Thread-local allocation probe. Thread-local, not global, because
/// `cargo test` runs test functions concurrently in one process and a global
/// counter would silently tally other tests' allocations into this one.
mod alloc_probe {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        static ARMED: Cell<bool> = const { Cell::new(false) };
        static BYTES: Cell<u64> = const { Cell::new(0) };
    }

    pub fn arm() {
        BYTES.with(|b| b.set(0));
        ARMED.with(|a| a.set(true));
    }

    pub fn disarm() -> u64 {
        ARMED.with(|a| a.set(false));
        BYTES.with(Cell::get)
    }

    fn record(size: usize) {
        if ARMED.try_with(Cell::get).unwrap_or(false) {
            let _ = BYTES.try_with(|b| b.set(b.get() + size as u64));
        }
    }

    pub struct Counting;

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
}

#[global_allocator]
static ALLOCATOR: alloc_probe::Counting = alloc_probe::Counting;
