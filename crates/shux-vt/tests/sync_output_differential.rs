//! Deferring the synchronized-output freeze must be UNOBSERVABLE (issue #115).
//!
//! `CSI ?2026h` promises that the frame on screen will not change until
//! `CSI ?2026l`. shux used to keep that promise by copying the whole grid the
//! instant the mode opened. It now copies nothing until something would
//! actually change the presented frame, which is cheaper only if it is also
//! identical — and "identical" is a property over all inputs, not over a
//! handful of cases.
//!
//! So it is tested as one. Random escape-sequence programs are driven through
//! two terminals — one that freezes lazily, one that freezes at `?2026h` —
//! and every observable is compared after every step. The operation alphabet
//! is weighted towards what interacts with the freeze: synchronized-output
//! windows, and the things that historically go wrong inside one (alternate
//! screen switches, RIS, resizes, scrolling writes, title and default-colour
//! changes, dirty-region drains).
//!
//! This file is the semantic half of the fix. The cost half is
//! `sync_output_bounds.rs`; neither is a substitute for the other, and a
//! differential test in particular cannot see a bug both arms share.

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
    /// freeze has to survive one landing at any point — including inside an
    /// open window, where the frozen frame and the live one reflow along
    /// different content.
    Resize(usize, usize),
    /// Drain dirty regions, as a renderer would between frames.
    DrainDirty,
    /// Feed one sequence split across two `process` calls at an arbitrary
    /// point. A PTY read boundary can fall anywhere, including the middle of
    /// `ESC[?2026h`.
    FeedSplit(&'static [u8], usize),
}

/// Everything a consumer of `VirtualTerminal` can see.
#[derive(Debug, PartialEq, Eq)]
struct Observed {
    /// Canonical hash of the PRESENTED frame: cells, styles, cursor, alt-screen
    /// flag, default colours, palette-override flag.
    frame: String,
    /// History is outside the presented FRAME but inside copy mode's
    /// coordinate space, and it is read through the terminal rather than off
    /// the frozen grid — that indirection is the fix, so it is exactly what
    /// has to be compared.
    history: Vec<String>,
    presented_total_lines: usize,
    rows: usize,
    cols: usize,
    /// The lens content revision, which the daemon publishes to watchers.
    content_revision: u64,
    title: Option<String>,
    alternate_screen: bool,
    /// The live mode flag, as `DECRQM` would report it. Distinct from whether
    /// a frozen copy exists, which is exactly what this change alters.
    sync_mode: bool,
    /// Dirty regions drive partial redraw; a difference here is a rendering
    /// bug even when every cell agrees.
    dirty: Vec<(usize, usize, usize)>,
    /// Cursor presentation is drawn as an overlay, outside the frame hash's
    /// cell data, so it is compared explicitly too.
    cursor: (usize, usize, bool),
    /// What the pane would render as text, which is what `pane capture` and
    /// every agent workflow reads.
    text: String,
}

fn observe(vt: &mut VirtualTerminal) -> Observed {
    let env = FrameEnvelope::from_terminal(vt, &MaskSet::new());
    let text = vt.capture_text(None);
    let presented_total_lines = vt.presented_total_lines();
    // Every line copy mode can reach, in copy mode's own coordinates.
    let history = (0..presented_total_lines)
        .map(|r| {
            vt.presented_row(r)
                .map(|row| (0..row.len()).map(|c| row[c].ch).collect::<String>())
                .unwrap_or_else(|| "<missing>".to_string())
        })
        .collect();
    let grid = vt.grid();
    Observed {
        frame: frame_digest(&env),
        history,
        presented_total_lines,
        rows: grid.rows(),
        cols: grid.cols(),
        content_revision: vt.content_revision(),
        title: vt.title().map(str::to_string),
        alternate_screen: vt.is_alternate_screen(),
        sync_mode: vt.modes().synchronized_output,
        cursor: (vt.cursor().row, vt.cursor().col, vt.cursor().visible),
        dirty: Vec::new(),
        text,
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
        Op::FeedSplit(bytes, at) => {
            let at = (*at).min(bytes.len());
            vt.process(&bytes[..at]);
            vt.process(&bytes[at..]);
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

/// Sequences that reach the synchronized-output freeze, plus the neighbours
/// that have historically interacted with a frozen presentation badly.
const SEQUENCES: &[&[u8]] = &[
    b"\x1b[?2026h",
    b"\x1b[?2026l",
    // Combined private modes: the freeze must arm from a parameter list, not
    // from an exact byte match on the whole sequence.
    b"\x1b[?1049;2026h",
    b"\x1b[?2026;1049h",
    b"\x1b[?2026;1l",
    b"\x1b[?25;2026h",
    b"\x1b[?1049h",
    b"\x1b[?1049l",
    b"\x1b[?1047h",
    b"\x1b[?1047l",
    b"\x1b[?1048h",
    b"\x1b[?1048l",
    b"\x1bc", // RIS
    b"\x1b7", // DECSC
    b"\x1b8", // DECRC
    b"\x1b[2J",
    b"\x1b[3J",
    b"\x1b[H",
    b"\x1b[5;7H",
    b"\x1b[2;6r", // DECSTBM
    b"\x1b[?6h",  // origin mode
    b"\x1b[?6l",
    b"\x1b[?25l",
    b"\x1b[?25h",
    b"\x1b[6n",                         // DSR: parses, changes nothing presented
    b"\x1b[?9999h",                     // unhandled private mode: same
    b"\x1b[1;38;2;200;40;40;48;5;236m", // truecolor fg + indexed bg
    b"\x1b[0m",
    b"\x1b]0;sync differential\x07", // OSC title
    b"\x1b]2;\x07",                  // OSC title cleared
    b"\x1b]11;#112233\x07",          // OSC dynamic default background
    b"\x1b]10;#ddeeff\x07",          // OSC dynamic default foreground
    b"\x1b[10S",
    b"\x1b[10T",
    b"\x1b[3L",
    b"\x1b[3M",
    b"\x1b[5@", // ICH
    b"\x1b[5P", // DCH
    b"\x1b#8",  // DECALN
];

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        // Weighted towards escape sequences: they are what the freeze sees.
        8 => (0..SEQUENCES.len()).prop_map(|i| Op::Feed(SEQUENCES[i])),
        3 => "[a-z ]{0,40}(\r\n)?".prop_map(Op::Write),
        1 => (1usize..12, 1usize..30).prop_map(|(r, c)| Op::Resize(r, c)),
        1 => Just(Op::DrainDirty),
        2 => (0..SEQUENCES.len(), 0usize..10).prop_map(|(i, at)| Op::FeedSplit(SEQUENCES[i], at)),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]

    /// The property: deferring the freeze changes nothing an observer can see.
    #[test]
    fn deferring_the_sync_freeze_is_unobservable(ops in prop::collection::vec(op_strategy(), 1..60)) {
        let mut lazy = VirtualTerminal::new(8, 24);
        let mut eager = VirtualTerminal::new(8, 24);
        eager.set_eager_sync_freeze(true);

        for (step, op) in ops.iter().enumerate() {
            let dirty_lazy = apply(&mut lazy, op);
            let dirty_eager = apply(&mut eager, op);
            prop_assert_eq!(
                &dirty_lazy,
                &dirty_eager,
                "dirty regions diverged at step {} after {:?}",
                step,
                op
            );

            let mut a = observe(&mut lazy);
            let mut b = observe(&mut eager);
            a.dirty = dirty_lazy;
            b.dirty = dirty_eager;
            prop_assert_eq!(a, b, "state diverged at step {} after {:?}", step, op);
        }
    }

    /// The same property on a pane that has real scrollback to lose. The bug
    /// was scrollback-shaped, and the fix shares scrollback rows between the
    /// live grid and the frozen one, so history is where an aliasing mistake
    /// would show up: a write to the live grid leaking into the frozen frame,
    /// or a frozen row still visible after the window closed.
    #[test]
    fn deferring_the_sync_freeze_is_unobservable_with_scrollback(
        ops in prop::collection::vec(op_strategy(), 1..40)
    ) {
        let mut lazy = VirtualTerminal::new(6, 20);
        let mut eager = VirtualTerminal::new(6, 20);
        eager.set_eager_sync_freeze(true);
        for i in 0..80 {
            let line = format!("history line {i} xyz\r\n");
            lazy.process(line.as_bytes());
            eager.process(line.as_bytes());
        }

        for (step, op) in ops.iter().enumerate() {
            let dirty_lazy = apply(&mut lazy, op);
            let dirty_eager = apply(&mut eager, op);
            prop_assert_eq!(&dirty_lazy, &dirty_eager, "dirty diverged at step {}", step);
            let mut a = observe(&mut lazy);
            let mut b = observe(&mut eager);
            a.dirty = dirty_lazy;
            b.dirty = dirty_eager;
            prop_assert_eq!(a, b, "state diverged at step {} after {:?}", step, op);
        }
    }
}

// ---------------------------------------------------------------------------
// Guard: the two arms of the differential must really run different code
// ---------------------------------------------------------------------------

/// A differential test whose two sides take the same code path proves nothing.
/// This one measures the difference directly: with the freeze eager, opening
/// and closing a window must keep asking the allocator for a grid; with it
/// lazy, it must not.
#[test]
fn the_two_arms_take_different_allocation_paths() {
    const ROWS: usize = 40;
    const COLS: usize = 160;
    const TOGGLES: usize = 200;

    fn toggle_bytes(eager: bool, seq: &[u8]) -> u64 {
        let mut vt = VirtualTerminal::new(ROWS, COLS);
        vt.set_eager_sync_freeze(eager);
        // Real history, so an eager freeze has something to copy.
        for _ in 0..500 {
            vt.process(b"scrollback\r\n");
        }
        vt.process(seq);
        alloc_probe::arm();
        for _ in 0..TOGGLES {
            vt.process(seq);
        }
        alloc_probe::disarm()
    }

    const SYNC: &[u8] = b"\x1b[?2026h\x1b[?2026l";
    /// A mode pair with the same parse shape that touches no buffer at all.
    /// Parsing costs what it costs in both arms, so it is the floor both are
    /// measured against rather than a number either has to beat.
    const INERT: &[u8] = b"\x1b[?1000h\x1b[?1000l";

    let parse_floor = toggle_bytes(false, INERT);
    let eager = toggle_bytes(true, SYNC);
    let lazy = toggle_bytes(false, SYNC);
    // The frozen frame is the VIEWPORT, so a real copy costs one pointer per
    // visible row — not per retained line. That is the point of the change:
    // the floor no longer moves when the pane's history does.
    let one_frame_walk = (ROWS * std::mem::size_of::<usize>()) as u64;

    assert!(
        eager >= parse_floor + one_frame_walk * TOGGLES as u64,
        "the eager reference allocated {eager} bytes over {TOGGLES} windows against a \
         parse floor of {parse_floor}, under the {} a per-line walk of the frame costs; \
         it is not copying the frame, so the differential compares nothing",
        one_frame_walk * TOGGLES as u64,
    );
    assert!(
        lazy <= parse_floor,
        "the lazy arm allocated {lazy} bytes where parsing the same number of inert mode \
         changes costs {parse_floor}; the freeze is not actually being deferred"
    );
    assert!(
        eager > lazy * 5,
        "the eager arm allocated {eager} bytes and the lazy one {lazy}; the two sides of \
         this differential are taking the same path and it proves nothing"
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
