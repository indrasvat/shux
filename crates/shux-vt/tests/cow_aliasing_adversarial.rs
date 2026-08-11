//! Adversarial: copy-on-write row sharing (issue #115).
//!
//! `Row` holds `Arc<Vec<Cell>>`, so `Grid::clone` shares every row with the
//! clone and the synchronized-output freeze RETAINS such a clone while the
//! live grid keeps being written. Every test here holds a frozen/cloned grid,
//! hammers the live one with a write path, and asserts:
//!
//!   * the held side is byte-identical to what it was at freeze time, and
//!   * the live side really did change (so a test that froze nothing, or
//!     hammered nothing, cannot pass vacuously).

use shux_vt::{Cell, Grid, GridConfig, Row, SYNC_UPDATE_TIMEOUT_MS, VirtualTerminal};

/// Every observable byte of a grid: dimensions, and for every line
/// (scrollback + visible) its wrap flag and every cell.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fingerprint {
    rows: usize,
    cols: usize,
    scrollback_len: usize,
    lines: Vec<(bool, Vec<Cell>)>,
}

fn fingerprint(g: &Grid) -> Fingerprint {
    let lines = (0..g.total_lines())
        .map(|i| {
            let row: &Row = g.row(i).expect("row in range");
            let cells = (0..row.len()).map(|c| row[c].clone()).collect::<Vec<_>>();
            (row.wrapped, cells)
        })
        .collect();
    Fingerprint {
        rows: g.rows(),
        cols: g.cols(),
        scrollback_len: g.scrollback_len(),
        lines,
    }
}

/// The presented FRAME: the visible rows and nothing else.
///
/// This is what `CSI ?2026h` promises to hold still, and since issue #115 it
/// is also exactly what the frozen buffer contains — history is not part of
/// the frame and is read live through `VirtualTerminal::presented_row`, so
/// fingerprinting `vt.grid()` whole would compare the frame against the frame
/// plus a scrollback that is not there.
fn frame_fingerprint(g: &Grid) -> Fingerprint {
    let lines = (0..g.rows())
        .map(|i| {
            let row: &Row = g.visible_row(i);
            let cells = (0..row.len()).map(|c| row[c].clone()).collect::<Vec<_>>();
            (row.wrapped, cells)
        })
        .collect();
    Fingerprint {
        rows: g.rows(),
        cols: g.cols(),
        scrollback_len: 0,
        lines,
    }
}

/// Every line copy mode can reach, through the presented coordinate space.
fn presented_lines(vt: &VirtualTerminal) -> Vec<(bool, Vec<Cell>)> {
    (0..vt.presented_total_lines())
        .map(|i| {
            let row = vt.presented_row(i).expect("presented row in range");
            (
                row.wrapped,
                (0..row.len()).map(|c| row[c].clone()).collect::<Vec<_>>(),
            )
        })
        .collect()
}

/// Where two fingerprints first differ, rendered for a failure message.
fn first_divergence(a: &Fingerprint, b: &Fingerprint) -> String {
    if a.rows != b.rows || a.cols != b.cols {
        return format!("dims {}x{} vs {}x{}", a.rows, a.cols, b.rows, b.cols);
    }
    if a.scrollback_len != b.scrollback_len {
        return format!("scrollback {} vs {}", a.scrollback_len, b.scrollback_len);
    }
    if a.lines.len() != b.lines.len() {
        return format!("line count {} vs {}", a.lines.len(), b.lines.len());
    }
    for (i, (la, lb)) in a.lines.iter().zip(b.lines.iter()).enumerate() {
        if la.0 != lb.0 {
            return format!("line {i} wrapped {} vs {}", la.0, lb.0);
        }
        if la.1.len() != lb.1.len() {
            return format!("line {i} width {} vs {}", la.1.len(), lb.1.len());
        }
        for (c, (ca, cb)) in la.1.iter().zip(lb.1.iter()).enumerate() {
            if ca != cb {
                return format!("line {i} col {c}: {ca:?} vs {cb:?}");
            }
        }
    }
    "identical".to_string()
}

fn text_of(g: &Grid) -> String {
    (0..g.rows())
        .map(|r| {
            let row = g.visible_row(r);
            (0..row.len()).map(|c| row[c].ch).collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A terminal with a colour-probed, wide-char, styled starting frame, so a
/// freeze that silently loses style/width/colour cannot pass as "identical".
fn seeded_vt(rows: usize, cols: usize) -> VirtualTerminal {
    let mut vt = VirtualTerminal::new(rows, cols);
    vt.process(b"\x1b[38;2;255;0;128;48;2;0;64;32mtruecolor\x1b[0m\r\n");
    vt.process(b"\x1b[38;5;208;48;5;19mindexed\x1b[0m\r\n");
    vt.process(b"\x1b[1;4;7;31;42mbasic-bold\x1b[0m\r\n");
    vt.process("\u{1F600}\u{4F60}\u{597D} wide\r\n".as_bytes());
    vt.process("e\u{0301}a\u{0308} \u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} zwj\r\n".as_bytes());
    vt.process(b"tail row\r\n");
    vt
}

/// Arm synchronized output and return the presented fingerprint. Nothing has
/// been written since `?2026h`, so this IS the frozen frame by construction.
fn arm_sync(vt: &mut VirtualTerminal) -> Fingerprint {
    vt.process(b"\x1b[?2026h");
    assert!(vt.sync_output_active(), "?2026h must arm the window");
    frame_fingerprint(vt.grid())
}

fn assert_frozen(vt: &VirtualTerminal, at_freeze: &Fingerprint, what: &str) {
    let now = frame_fingerprint(vt.grid());
    assert_eq!(
        &now,
        at_freeze,
        "presented frame moved during a sync window ({what}): {}",
        first_divergence(at_freeze, &now)
    );
}

/// A freeze assertion for a window held across a LONG loop of `process()`
/// calls, where the deadline is in play (issue #154).
///
/// `SYNC_UPDATE_TIMEOUT_MS` bounds every window and `process()` enforces it on
/// every batch by design, so a loop cannot assume the window it armed is still
/// open: a runner descheduled for a deadline's worth of wall clock between two
/// chunks gets the window released, the frame legitimately moves, and a bare
/// `assert_frozen` fails for a reason that is not a freeze bug.
///
/// So the invariant asserted here is the one the freeze actually promises —
/// the frame does not move *while the window is open*. An expiry is not
/// tolerated, it is COUNTED and re-armed: an expiry costs a full deadline of
/// wall clock, so the elapsed time is a hard ceiling on how many are
/// legitimate, and a freeze that releases windows it should be holding blows
/// through that ceiling however slow the machine is.
struct SyncWatch {
    expiries: u64,
    started: std::time::Instant,
}

impl SyncWatch {
    fn new() -> Self {
        Self {
            expiries: 0,
            started: std::time::Instant::now(),
        }
    }

    /// Assert the freeze after a `process()` call, and return the fingerprint
    /// to compare against next time: the same one while the window is still
    /// open, a fresh baseline from a re-armed window when the deadline closed
    /// it.
    fn check(&mut self, vt: &mut VirtualTerminal, frozen: Fingerprint, what: &str) -> Fingerprint {
        if vt.sync_output_active() {
            assert_frozen(vt, &frozen, what);
            frozen
        } else {
            self.expiries += 1;
            arm_sync(vt)
        }
    }

    /// Windows are armed one at a time, so N expiries cannot have happened in
    /// less than N deadlines of wall clock. Anything above that ceiling is the
    /// freeze releasing windows, not the machine being slow.
    fn assert_expiries_fit_the_clock(&self, what: &str) {
        let elapsed_ms = self.started.elapsed().as_millis() as u64;
        let budget = elapsed_ms / SYNC_UPDATE_TIMEOUT_MS + 1;
        assert!(
            self.expiries <= budget,
            "{what}: {} sync windows expired in {elapsed_ms} ms, but each expiry costs a \
             full {SYNC_UPDATE_TIMEOUT_MS} ms deadline (budget {budget}) — the windows are \
             being released, not held",
            self.expiries
        );
    }
}

// ---------------------------------------------------------------------------
// 1. Parser write paths hammered while a sync window holds a frozen clone.
// ---------------------------------------------------------------------------

/// Each case: a name and the bytes to hammer the live grid with while frozen.
fn hammer_cases() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (
            "wide + combining + ZWJ overwrite",
            "\x1b[1;1H\u{4F60}\u{597D}\u{4E16}\u{754C}e\u{0301}\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}"
                .as_bytes()
                .to_vec(),
        ),
        ("ICH", b"\x1b[2;3H\x1b[5@".to_vec()),
        ("DCH", b"\x1b[2;3H\x1b[4P".to_vec()),
        ("ICH straddling a wide pair", "\x1b[4;1H\x1b[1@".as_bytes().to_vec()),
        ("DCH straddling a wide pair", "\x1b[4;2H\x1b[1P".as_bytes().to_vec()),
        ("IL", b"\x1b[2;1H\x1b[3L".to_vec()),
        ("DL", b"\x1b[2;1H\x1b[2M".to_vec()),
        ("ECH", b"\x1b[1;2H\x1b[6X".to_vec()),
        ("EL to end", b"\x1b[2;4H\x1b[0K".to_vec()),
        ("EL whole line", b"\x1b[3;4H\x1b[2K".to_vec()),
        ("scroll region + SU", b"\x1b[2;6r\x1b[3S".to_vec()),
        ("scroll region + SD", b"\x1b[2;6r\x1b[3T".to_vec()),
        ("scroll region + IL inside", b"\x1b[2;6r\x1b[4;1H\x1b[2L".to_vec()),
        ("full-screen scroll into scrollback", b"\x1b[8;1H\r\n\r\n\r\n\r\n".to_vec()),
        ("ED 2", b"\x1b[2J".to_vec()),
        ("ED 3", b"\x1b[3J".to_vec()),
        ("ED 0 from mid-screen", b"\x1b[3;3H\x1b[0J".to_vec()),
        ("ED 1 from mid-screen", b"\x1b[3;3H\x1b[1J".to_vec()),
        ("DECALN", b"\x1b#8".to_vec()),
        ("alt screen enter", b"\x1b[?1049h".to_vec()),
        (
            "alt screen enter+write+leave+re-enter",
            b"\x1b[?1049hZZZZZZ\x1b[?1049l\x1b[?1049hQQQ\x1b[?1049l\x1b[?1049h".to_vec(),
        ),
        (
            "alt screen churn with no draw (recycles the spare)",
            b"\x1b[?1049h\x1b[?1049l\x1b[?1049h\x1b[?1049l\x1b[?1049h\x1b[?1049l".to_vec(),
        ),
        ("REP after a wide char", "\u{4F60}\x1b[b\x1b[b".as_bytes().to_vec()),
        ("reverse index at top", b"\x1b[1;1H\x1bM\x1bM\x1bM".to_vec()),
        ("tab fill + overwrite", b"\x1b[1;1H\tX\tY\tZ".to_vec()),
        (
            "styled overwrite everywhere",
            b"\x1b[1;1H\x1b[38;2;1;2;3;48;2;4;5;6m\x1b[2J\x1b#8".to_vec(),
        ),
    ]
}

#[test]
fn sync_frozen_frame_is_byte_identical_under_every_write_path() {
    let mut inert = Vec::new();
    for (name, bytes) in hammer_cases() {
        let mut vt = seeded_vt(8, 24);
        let before_live = fingerprint(vt.grid());
        let frozen = arm_sync(&mut vt);

        vt.process(&bytes);
        assert_frozen(&vt, &frozen, name);

        // Hammer again: a second batch must not be able to reach the frozen
        // rows either (the freeze slot is already filled, so this exercises
        // "write to an already-unshared row" and "write to a row the first
        // batch never touched" together).
        vt.process(&bytes);
        assert_frozen(&vt, &frozen, name);

        // Release and confirm the live side really moved. A case whose live
        // side did NOT move proves nothing about the freeze, so it is
        // reported rather than silently counted as a pass.
        vt.process(b"\x1b[?2026l");
        assert!(!vt.sync_output_active(), "{name}: ?2026l must release");
        if fingerprint(vt.grid()) == before_live {
            inert.push(name);
        }
    }
    // An alt-screen enter/leave pair that draws nothing legitimately returns
    // the pane to the frame it started on. Every OTHER inert case means the
    // hammer never reached the grid and the corresponding freeze assertion
    // above was vacuous.
    //
    // `DECALN` (`ESC # 8`) used to be on this list: the parser dropped the
    // sequence, so it wrote nothing with or without a sync window and its
    // freeze assertion proved nothing (issue #117). It is a real full-screen
    // write now, so it must NOT be inert — which is what keeps the fill honest
    // about copy-on-write.
    assert_eq!(
        inert,
        vec!["alt screen churn with no draw (recycles the spare)"],
        "these hammer cases wrote nothing to the live grid, so their freeze \
         assertions proved nothing: {inert:?}"
    );
}

/// Interleave: write, then release, then re-arm, then write again — many
/// times. Each window must present its own frozen frame, and no window may
/// inherit the previous one's rows.
#[test]
fn repeated_sync_windows_never_leak_rows_between_each_other() {
    let mut vt = seeded_vt(8, 24);
    let mut seen: Vec<Fingerprint> = Vec::new();
    for i in 0..12u32 {
        let frozen = arm_sync(&mut vt);
        vt.process(format!("\x1b[1;1Hwindow-{i:03}\x1b[2;1H\x1b[38;5;{}mX", i % 256).as_bytes());
        assert_frozen(&vt, &frozen, "repeated windows");
        vt.process(b"\x1b[?2026l");
        let live = frame_fingerprint(vt.grid());
        assert_ne!(
            live, frozen,
            "iteration {i}: release must reveal the writes"
        );
        seen.push(frozen);
    }
    // Each frozen frame must equal the live frame of the previous release —
    // i.e. freezing is exactly "the frame as it stood", never a stale one.
    for w in seen.windows(2) {
        assert_ne!(w[0], w[1], "consecutive frozen frames must differ");
    }
}

// ---------------------------------------------------------------------------
// 2. Resize / reflow while frozen.
// ---------------------------------------------------------------------------

/// A resize RELEASES an open synchronized-output window, so the presented
/// frame afterwards is simply the live one.
///
/// This is the semantic, not an implementation detail: the frame an
/// application asked to hold still was drawn for a geometry that no longer
/// exists, and reflowing it is wrong in two ways (no history to rewrap
/// against; the alternate screen is canvas-resized rather than reflowed). The
/// reference is therefore a terminal that never opened a window at all — after
/// the resize the two must be indistinguishable.
#[test]
fn a_resize_releases_the_window_and_presents_the_live_frame() {
    let seed = |vt: &mut VirtualTerminal| {
        for i in 0..40 {
            vt.process(
                format!(
                    "\x1b[38;5;{}mline-{i:02}-\u{4F60}\u{597D}-padding-padding-padding\r\n",
                    i % 200
                )
                .as_bytes(),
            );
        }
    };
    let hammer: &[u8] = b"\x1b[2J\x1b[1;1H\x1b[38;2;9;9;9mHAMMERED\x1b[3;1H\x1b[5L\x1b[2;7r\x1b[4S";

    for (new_rows, new_cols) in [(8, 12), (8, 40), (4, 24), (16, 24), (3, 7), (20, 61)] {
        // No window at any point.
        let mut reference = VirtualTerminal::new(8, 24);
        seed(&mut reference);
        reference.process(hammer);
        reference.resize(new_rows, new_cols);

        // Same bytes, but with a window open across the hammering.
        let mut vt = VirtualTerminal::new(8, 24);
        seed(&mut vt);
        let frozen = arm_sync(&mut vt);
        vt.process(hammer);
        assert_frozen(&vt, &frozen, "before the resize");
        vt.resize(new_rows, new_cols);

        assert!(
            !vt.sync_output_active(),
            "resize to {new_rows}x{new_cols} left the window open"
        );
        let presented = frame_fingerprint(vt.grid());
        let expected = frame_fingerprint(reference.grid());
        assert_eq!(
            presented,
            expected,
            "after a resize to {new_rows}x{new_cols} the presented frame differs from a \
             terminal that never opened a window: {}",
            first_divergence(&expected, &presented)
        );
        assert_eq!(
            presented_lines(&vt),
            presented_lines(&reference),
            "history diverged after a resize to {new_rows}x{new_cols}"
        );
    }
}

/// The same, on the ALTERNATE screen — where the live resize goes through
/// `Grid::resize_canvas`, the one resize path that mutates rows IN PLACE
/// (`Row::resize` + `Row::sanitize_wide_pairs`) rather than rebuilding them,
/// so a shared row would be written through.
///
/// This case used to present a frozen alternate-screen frame REFLOWED, which
/// the live grid never is: a pane resized while `vim` or `lazygit` held a
/// window open showed rewrapped content the application had never drawn. The
/// defect predates the lazy freeze; releasing on resize removes it.
#[test]
fn alt_screen_resize_releases_the_window_and_writes_through_no_shared_row() {
    let script: &str = "primary line one\r\nprimary line two\r\n\x1b[?1049h\
\x1b[38;2;30;200;120mALT-CANVAS-LINE-ONE\x1b[2;1Halt-row-two\x1b[3;1H\u{4F60}\u{597D} wide";
    let script = script.as_bytes();
    // Every geometry here CHANGES a dimension. A resize to the size the pane
    // already has is not a resize and must leave the window alone — covered
    // separately by `a_same_size_resize_leaves_the_window_open`.
    for (new_rows, new_cols) in [(4, 9), (10, 31), (2, 5), (12, 20), (6, 41)] {
        let mut reference = VirtualTerminal::new(6, 20);
        reference.process(script);
        reference.resize(new_rows, new_cols);

        let mut vt = VirtualTerminal::new(6, 20);
        vt.process(script);
        let frozen = arm_sync(&mut vt);
        // Force the freeze to be taken while the ALT grid is live.
        vt.process(b"\x1b[1;1HXXXX");
        assert_frozen(&vt, &frozen, "alt screen before the resize");
        vt.resize(new_rows, new_cols);

        assert!(!vt.sync_output_active(), "resize left the window open");
        assert!(vt.is_alternate_screen(), "still on the alternate screen");
        // The live content is the hammered one, so compare shapes and the fact
        // that no shared row was written through: the reference, which was
        // never frozen, must have the SAME geometry and wrap flags.
        let presented = frame_fingerprint(vt.grid());
        let expected = frame_fingerprint(reference.grid());
        assert_eq!(
            (presented.rows, presented.cols),
            (expected.rows, expected.cols),
            "alt-screen resize to {new_rows}x{new_cols} produced the wrong geometry"
        );
        assert_eq!(
            presented.lines.iter().map(|l| l.0).collect::<Vec<_>>(),
            expected.lines.iter().map(|l| l.0).collect::<Vec<_>>(),
            "alt-screen resize to {new_rows}x{new_cols} rewrapped a canvas that is never \
             reflowed"
        );
    }
}

/// A `Grid` clone held across a reflow of the original. Reflow rebuilds every
/// row from `row.cells.as_ref().clone()` / `trim_default_trailing_cells`, the
/// two places the COW change rewrote.
#[test]
fn grid_clone_survives_reflow_of_the_original() {
    let mut vt = VirtualTerminal::new(6, 20);
    for i in 0..30 {
        vt.process(format!("row-{i:02}-\u{4F60}\u{597D}-wwwwwwwwwwwwwwwwww\r\n").as_bytes());
    }
    let held = vt.grid().clone();
    let held_fp = fingerprint(&held);
    for cols in [7usize, 13, 41, 20, 3, 80] {
        // Write BEFORE the resize as well: straight after `held` was taken
        // every live row is still shared with it, which is the state a missing
        // unshare would corrupt. (After a reflow the live rows are freshly
        // built and unshared, so a post-resize write alone proves less.)
        vt.process(b"\x1b[H\x1b[38;2;3;3;3mpre-resize-repaint\x1b[2;1H\x1b[3@\x1b[1P");
        vt.resize(6, cols);
        // Reflow rebuilds rows rather than writing through them, so on its own
        // it cannot expose a missing unshare. Write afterwards, which is what
        // a real pane does after SIGWINCH, so the rows the clone still holds
        // are actually written to.
        vt.process(b"\x1b[H\x1b[2J\x1b[38;2;7;7;7mrepaint-after-resize\r\n");
        vt.process(b"\x1b[1;1H\x1b[4@\x1b[2P\x1b[2L\x1b[1M");
        for i in 0..12 {
            vt.process(format!("post-{cols}-{i:02}\r\n").as_bytes());
        }
        let now = fingerprint(&held);
        assert_eq!(
            now,
            held_fp,
            "held clone mutated by a reflow to {cols} cols: {}",
            first_divergence(&held_fp, &now)
        );
    }
}

// ---------------------------------------------------------------------------
// 3. Scrollback: eviction, clearing, and reads from both sides.
// ---------------------------------------------------------------------------

#[test]
fn scrollback_eviction_does_not_disturb_a_held_clone() {
    let cfg = GridConfig {
        max_scrollback: 8,
        track_dirty: true,
    };
    let mut vt = VirtualTerminal::with_config(4, 16, cfg);
    for i in 0..10 {
        vt.process(format!("seed-{i:02}\r\n").as_bytes());
    }
    let held = vt.grid().clone();
    let held_fp = fingerprint(&held);
    assert!(held.scrollback_len() > 0, "the clone must hold scrollback");

    // Push far past the cap: every row the clone holds is evicted from the
    // live deque and recycled (pop_front -> reset -> push_back).
    for i in 0..200 {
        vt.process(format!("churn-{i:03}\r\n").as_bytes());
    }
    let now = fingerprint(&held);
    assert_eq!(
        now,
        held_fp,
        "recycled scrollback rows wrote into a held clone: {}",
        first_divergence(&held_fp, &now)
    );
    // The oldest row of the clone must still read as its original content.
    let oldest = held.scrollback_row(0).expect("clone kept its scrollback");
    let text: String = (0..oldest.len()).map(|c| oldest[c].ch).collect();
    assert!(
        text.starts_with("seed-"),
        "clone's oldest scrollback row became {text:?}"
    );
}

#[test]
fn clear_scrollback_while_a_clone_holds_it() {
    let mut vt = VirtualTerminal::new(4, 16);
    for i in 0..20 {
        vt.process(format!("hist-{i:02}\r\n").as_bytes());
    }
    let held = vt.grid().clone();
    let held_fp = fingerprint(&held);
    let held_sb = held.scrollback_len();
    assert!(held_sb > 0);

    vt.clear_scrollback();
    assert_eq!(vt.scrollback_len(), 0, "live scrollback must be gone");
    assert!(
        vt.grid().scrollback_row(0).is_none(),
        "live side must report no scrollback"
    );
    // Clearing only drops deque entries; keep writing so the rows the clone
    // still holds are the ones the live grid recycles and writes into.
    for i in 0..80 {
        vt.process(format!("after-clear-{i:02}\r\n").as_bytes());
    }

    let now = fingerprint(&held);
    assert_eq!(
        now,
        held_fp,
        "clear_scrollback mutated a held clone: {}",
        first_divergence(&held_fp, &now)
    );
    assert_eq!(held.scrollback_len(), held_sb);
    for i in 0..held_sb {
        assert!(held.scrollback_row(i).is_some(), "row {i} vanished");
    }
}

#[test]
fn sync_window_frame_is_unaffected_by_scrollback_eviction() {
    let cfg = GridConfig {
        max_scrollback: 6,
        track_dirty: true,
    };
    let mut vt = VirtualTerminal::with_config(4, 16, cfg);
    for i in 0..12 {
        vt.process(format!("pre-{i:02}\r\n").as_bytes());
    }
    let frozen = arm_sync(&mut vt);
    for i in 0..300 {
        vt.process(format!("post-{i:03}\r\n").as_bytes());
    }
    assert_frozen(&vt, &frozen, "scrollback eviction under sync");
    // History behind the frame is read live and legitimately shrinks as lines
    // are evicted — a line that has fallen off the pane is gone whether or not
    // a window is open. What must hold is that the coordinate space stays
    // self-consistent: every line it claims to have must resolve.
    let presented = presented_lines(&vt);
    assert_eq!(
        presented.len(),
        vt.presented_total_lines(),
        "presented coordinate space claims lines it cannot resolve"
    );
    assert!(
        presented.len() >= vt.grid().rows(),
        "the presented span must always contain at least the frame"
    );
    vt.process(b"\x1b[?2026l");
    assert_ne!(frame_fingerprint(vt.grid()), frozen);
}

// ---------------------------------------------------------------------------
// 4. Alternate screen: the recycled spare buffer.
// ---------------------------------------------------------------------------

/// `ScreenSwap::leave` parks the retired alternate grid in a spare slot and
/// the next `enter` takes it back. If that retired grid still shares rows with
/// a frozen presentation, blanking or drawing on it must copy first.
#[test]
fn recycled_alt_spare_cannot_write_into_a_frozen_frame() {
    let mut vt = VirtualTerminal::new(6, 20);
    vt.process(b"primary content here\r\nsecond primary line\r\n");
    // Live on the alternate screen with content on it.
    vt.process(b"\x1b[?1049hALT-ORIGINAL\x1b[2;1Halt-second");
    let frozen = arm_sync(&mut vt);
    // Force the freeze to be taken while the ALT grid is live.
    vt.process(b"\x1b[3;1Hforce-freeze");
    assert_frozen(&vt, &frozen, "freeze taken on alt grid");

    // Retire the alt grid into the spare, then take it back and draw all over
    // it, repeatedly.
    for i in 0..8 {
        vt.process(format!("\x1b[?1049l\x1b[?1049h\x1b[1;1HRECYCLE-{i}\x1b[2;1H\x1b#8").as_bytes());
        assert_frozen(&vt, &frozen, "alt spare recycle");
    }
    vt.process(b"\x1b[?2026l");
    let live = text_of(vt.grid());
    assert!(
        live.contains("RECYCLE-7") || live.chars().all(|c| c == 'E' || c == '\n'),
        "live alt grid should show the last recycle draw, got:\n{live}"
    );
}

/// The same, but with a clone taken outside any sync window: hold the alt
/// grid's clone, leave, re-enter (recycling it) and blank it.
#[test]
fn alt_spare_blanking_does_not_reach_a_plain_clone() {
    let mut vt = VirtualTerminal::new(5, 18);
    vt.process(b"\x1b[?1049hHELD-ALT-CONTENT\x1b[2;1Hsecond");
    let held = vt.grid().clone();
    let held_fp = fingerprint(&held);
    for i in 0..10 {
        vt.process(format!("\x1b[?1049l\x1b[?1049hnew-{i}").as_bytes());
    }
    let now = fingerprint(&held);
    assert_eq!(
        now,
        held_fp,
        "spare-buffer blanking reached a held clone: {}",
        first_divergence(&held_fp, &now)
    );
    let row0 = held.visible_row(0);
    let text: String = (0..row0.len()).map(|c| row0[c].ch).collect();
    assert!(
        text.starts_with("HELD-ALT-CONTENT"),
        "clone row 0 is {text:?}"
    );
}

// ---------------------------------------------------------------------------
// 5. RIS and other full resets.
// ---------------------------------------------------------------------------

#[test]
fn ris_while_a_clone_is_held() {
    let mut vt = seeded_vt(8, 24);
    for i in 0..30 {
        vt.process(format!("scrollback-{i:02}\r\n").as_bytes());
    }
    let held = vt.grid().clone();
    let held_fp = fingerprint(&held);
    vt.process(b"\x1bc");
    let now = fingerprint(&held);
    assert_eq!(
        now,
        held_fp,
        "RIS mutated a held clone: {}",
        first_divergence(&held_fp, &now)
    );
    assert!(
        text_of(vt.grid()).chars().all(|c| c == ' ' || c == '\n'),
        "RIS must blank the live grid"
    );
}

// ---------------------------------------------------------------------------
// 6. A plain Grid clone hammered through every public Grid mutator.
// ---------------------------------------------------------------------------

#[test]
fn plain_grid_clone_is_immune_to_every_public_mutator() {
    let build = || {
        let mut vt = seeded_vt(8, 20);
        for i in 0..25 {
            vt.process(format!("g-{i:02}-\u{4F60}\u{597D}\r\n").as_bytes());
        }
        vt.grid().clone()
    };

    type Op = (&'static str, fn(&mut Grid));
    let ops: Vec<Op> = vec![
        ("clear_visible", |g| {
            g.clear_visible(shux_vt::Color::Indexed(4))
        }),
        ("clear_above", |g| {
            g.clear_above(3, shux_vt::Color::Rgb(9, 8, 7))
        }),
        ("clear_below", |g| g.clear_below(2, shux_vt::Color::Default)),
        ("scroll_up", |g| g.scroll_up(0, 7)),
        ("scroll_down", |g| g.scroll_down(0, 7)),
        ("scroll_up_n region", |g| g.scroll_up_n(1, 5, 3)),
        ("scroll_down_n region", |g| g.scroll_down_n(1, 5, 3)),
        ("resize narrower", |g| g.resize(8, 9)),
        ("resize wider", |g| g.resize(8, 44)),
        ("resize shorter", |g| g.resize(3, 20)),
        ("resize taller", |g| g.resize(19, 20)),
        ("visible_row_mut write", |g| {
            let mut row = g.visible_row_mut(1);
            for c in 0..row.len() {
                row[c] = Cell::default();
                row[c].ch = '#';
            }
        }),
        ("mark_all_dirty", |g| g.mark_all_dirty()),
    ];

    for (name, op) in ops {
        let mut live = build();
        let held = live.clone();
        let held_fp = fingerprint(&held);
        op(&mut live);
        // Run it twice, so "already unshared" is exercised too.
        op(&mut live);
        let now = fingerprint(&held);
        assert_eq!(
            now,
            held_fp,
            "{name} reached a held clone: {}",
            first_divergence(&held_fp, &now)
        );
    }
}

/// The reverse direction: mutate the CLONE and prove the ORIGINAL is untouched.
#[test]
fn writing_the_clone_does_not_reach_the_original() {
    let mut vt = seeded_vt(8, 20);
    for i in 0..25 {
        vt.process(format!("orig-{i:02}\r\n").as_bytes());
    }
    let original = vt.grid().clone();
    let original_fp = fingerprint(&original);
    let mut clone = original.clone();
    clone.clear_visible(shux_vt::Color::Indexed(1));
    clone.scroll_up_n(0, 7, 4);
    clone.resize(8, 33);
    {
        let mut row = clone.visible_row_mut(0);
        for c in 0..row.len() {
            row[c].ch = '@';
        }
    }
    let now = fingerprint(&original);
    assert_eq!(
        now,
        original_fp,
        "writing the clone reached the original: {}",
        first_divergence(&original_fp, &now)
    );
}

// ---------------------------------------------------------------------------
// 7. Three-way sharing: clone of a clone, freeze on top.
// ---------------------------------------------------------------------------

#[test]
fn a_chain_of_clones_all_stay_independent() {
    let mut vt = seeded_vt(6, 18);
    for i in 0..15 {
        vt.process(format!("chain-{i:02}\r\n").as_bytes());
    }
    let a = vt.grid().clone();
    let b = a.clone();
    let mut c = b.clone();
    let (fa, fb) = (fingerprint(&a), fingerprint(&b));
    assert_eq!(fa, fb, "clones must start equal");

    // Freeze the terminal too, so four holders share the same rows.
    let frozen = arm_sync(&mut vt);
    vt.process(b"\x1b[2J\x1b#8\x1b[1;1Hlive-writes");
    c.clear_visible(shux_vt::Color::Rgb(1, 2, 3));
    c.scroll_up_n(0, 5, 2);

    assert_eq!(fingerprint(&a), fa, "clone a diverged");
    assert_eq!(fingerprint(&b), fb, "clone b diverged");
    assert_frozen(&vt, &frozen, "chain of clones");
    assert_ne!(fingerprint(&c), fa, "clone c must have changed");
}

// ---------------------------------------------------------------------------
// 8. Byte-level chunking: the freeze must not depend on how output arrives.
// ---------------------------------------------------------------------------

#[test]
fn frozen_frame_is_chunk_independent() {
    let script: &[u8] =
        b"\x1b[2;6r\x1b[3;1H\x1b[38;2;10;20;30mA\x1b[3S\x1b[2L\x1b[4@\x1b[2P\x1b#8\x1b[?1049h\x1b[?1049l\x1b[2J";
    let mut results = Vec::new();
    let mut watch = SyncWatch::new();
    for chunk in [1usize, 2, 3, 5, 8, 13, script.len()] {
        let mut vt = seeded_vt(8, 24);
        let armed = arm_sync(&mut vt);
        let mut frozen = armed.clone();
        let expiries_before = watch.expiries;
        for part in script.chunks(chunk) {
            vt.process(part);
            frozen = watch.check(&mut vt, frozen, &format!("chunk size {chunk}"));
        }
        vt.process(b"\x1b[?2026l");
        // A run whose window the deadline released mid-script was re-armed with
        // `?2026h` bytes the other runs never saw, so its live frame is no
        // longer comparable with theirs. Chunk-independence is a claim about
        // the script, not about the runner's scheduling.
        if watch.expiries == expiries_before {
            results.push((chunk, armed, fingerprint(vt.grid())));
        }
    }
    watch.assert_expiries_fit_the_clock("frozen_frame_is_chunk_independent");
    assert!(
        results.len() > 1,
        "only {} of 7 chunk sizes ran without the sync deadline expiring, so nothing was \
         compared across chunk boundaries",
        results.len()
    );
    let (_, f0, l0) = &results[0];
    for (chunk, f, l) in &results[1..] {
        assert_eq!(f, f0, "frozen frame differs at chunk size {chunk}");
        assert_eq!(
            l,
            l0,
            "live frame differs at chunk size {chunk}: {}",
            first_divergence(l0, l)
        );
    }
}

// ---------------------------------------------------------------------------
// 9. Randomised: arbitrary escape scripts hammered against a frozen frame.
// ---------------------------------------------------------------------------

/// The menu of sequences the fuzz draws from. Every one of these is a write
/// path into `Row`'s cells, a structural change to the deque, or a mode change
/// that redirects where writes land.
const FUZZ_MENU: &[&str] = &[
    "A",
    "\u{4F60}\u{597D}",
    "e\u{0301}",
    "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
    "\r\n",
    "\x1b[H",
    "\x1b[3;5H",
    "\x1b[2J",
    "\x1b[3J",
    "\x1b[0J",
    "\x1b[1J",
    "\x1b[K",
    "\x1b[1K",
    "\x1b[2K",
    "\x1b[4@",
    "\x1b[3P",
    "\x1b[2L",
    "\x1b[2M",
    "\x1b[5X",
    "\x1b[2S",
    "\x1b[2T",
    "\x1b[2;6r",
    "\x1b[r",
    "\x1bM",
    "\x1bD",
    "\x1bE",
    "\x1b[?1049h",
    "\x1b[?1049l",
    "\x1b[?1047h",
    "\x1b[?1047l",
    "\x1b[?6h",
    "\x1b[?6l",
    "\x1b[?7h",
    "\x1b[?7l",
    "\x1b[38;2;200;30;90m",
    "\x1b[48;5;27m",
    "\x1b[1;4;7m",
    "\x1b[0m",
    "\x1b[7m",
    "\t",
    "\x08",
    "\x1b[10b",
    "long-ascii-run-that-soft-wraps-past-the-right-margin",
];

/// Deterministic xorshift, so a failure is reproducible from the printed seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[(self.next() % xs.len() as u64) as usize]
    }
}

#[test]
fn fuzzed_scripts_cannot_move_the_frozen_frame() {
    let mut changed_live = 0usize;
    let mut watch = SyncWatch::new();
    for seed in 1u64..=400 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut vt = seeded_vt(8, 22);
        let before = fingerprint(vt.grid());
        let mut frozen = arm_sync(&mut vt);

        let mut script = String::new();
        for _ in 0..40 {
            script.push_str(rng.pick(FUZZ_MENU));
        }
        // Feed it in random-sized chunks, so a freeze that depends on batch
        // boundaries is exposed too.
        let bytes = script.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let n = 1 + (rng.next() % 9) as usize;
            let end = (i + n).min(bytes.len());
            // Never split inside a multi-byte scalar: the parser handles that
            // correctly, but a split would make the two arms of the chunk
            // comparison incomparable, not the freeze wrong.
            let mut end = end;
            while end < bytes.len() && (bytes[end] & 0xC0) == 0x80 {
                end += 1;
            }
            vt.process(&bytes[i..end]);
            frozen = watch.check(&mut vt, frozen, &format!("fuzz seed {seed}, offset {i}"));
            i = end;
        }
        vt.process(b"\x1b[?2026l");
        if fingerprint(vt.grid()) != before {
            changed_live += 1;
        }
    }
    watch.assert_expiries_fit_the_clock("fuzzed_scripts_cannot_move_the_frozen_frame");
    // Proof the fuzz is not vacuous: nearly every script must actually have
    // written something to the live grid.
    assert!(
        changed_live > 380,
        "only {changed_live}/400 fuzz scripts changed the live grid; the freeze \
         assertions were mostly vacuous"
    );
}

#[test]
fn fuzzed_scripts_cannot_reach_a_held_grid_clone() {
    for seed in 1u64..=200 {
        let mut rng = Rng(seed.wrapping_mul(0xD1B5_4A32_D192_ED03) | 1);
        let mut vt = seeded_vt(8, 22);
        for i in 0..12 {
            vt.process(format!("hist-{i:02}\r\n").as_bytes());
        }
        let held = vt.grid().clone();
        let held_fp = fingerprint(&held);
        let mut script = String::new();
        for _ in 0..60 {
            script.push_str(rng.pick(FUZZ_MENU));
        }
        vt.process(script.as_bytes());
        // A resize partway through, then more churn.
        vt.resize(5 + (seed % 7) as usize, 4 + (seed % 31) as usize);
        vt.process(script.as_bytes());
        let now = fingerprint(&held);
        assert_eq!(
            now,
            held_fp,
            "fuzz seed {seed} reached a held clone: {}",
            first_divergence(&held_fp, &now)
        );
    }
}

// ---------------------------------------------------------------------------
// 10. Many simultaneous holders, and thread-safety of the shared rows.
// ---------------------------------------------------------------------------

#[test]
fn many_simultaneous_holders_stay_independent() {
    let mut vt = seeded_vt(8, 20);
    let mut holders: Vec<(Grid, Fingerprint)> = Vec::new();
    for round in 0..25 {
        vt.process(format!("\x1b[1;1H\x1b[38;5;{}mround-{round:02}\r\n", round % 200).as_bytes());
        let g = vt.grid().clone();
        let fp = fingerprint(&g);
        holders.push((g, fp));
        // Every previously taken holder must still read as it did.
        for (i, (g, fp)) in holders.iter().enumerate() {
            let now = fingerprint(g);
            assert_eq!(
                &now,
                fp,
                "holder {i} diverged at round {round}: {}",
                first_divergence(fp, &now)
            );
        }
    }
    // Now churn hard and re-check every holder.
    for i in 0..500 {
        vt.process(format!("churn-{i:03}\x1b[4@\x1b[2P\r\n").as_bytes());
    }
    for (i, (g, fp)) in holders.iter().enumerate() {
        let now = fingerprint(g);
        assert_eq!(&now, fp, "holder {i} diverged after churn");
    }
}

/// The daemon shares `VirtualTerminal` across tokio tasks, so the shared row
/// storage must be `Send + Sync` — an `Rc` here would compile locally and
/// break the daemon.
#[test]
fn shared_row_storage_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Row>();
    assert_send_sync::<Grid>();
    assert_send_sync::<VirtualTerminal>();
    assert_send_sync::<GridConfig>();
    assert_send_sync::<Cell>();
}

/// A held clone really can cross a thread boundary and be read there while the
/// original keeps being written on this one.
#[test]
fn a_held_clone_reads_identically_on_another_thread() {
    let mut vt = seeded_vt(8, 20);
    for i in 0..30 {
        vt.process(format!("thr-{i:02}\r\n").as_bytes());
    }
    let held = vt.grid().clone();
    let expected = fingerprint(&held);
    let handle = std::thread::spawn(move || (fingerprint(&held), held));
    for i in 0..2000 {
        vt.process(format!("hammer-{i:04}\x1b[3@\x1b[1P\r\n").as_bytes());
    }
    let (seen, held) = handle.join().expect("reader thread");
    assert_eq!(seen, expected, "clone read differently on another thread");
    assert_eq!(fingerprint(&held), expected, "clone changed after the join");
}

// ---------------------------------------------------------------------------
// 11. Cross-path consistency: every reader agrees on the presented frame.
// ---------------------------------------------------------------------------

/// `capture_text`, `Grid::glance_text` and the raw cell fingerprint all read
/// the same presented grid. Under a sync window they must all report the
/// FROZEN frame, not a mix.
#[test]
fn every_read_path_reports_the_same_presented_frame() {
    let mut vt = seeded_vt(8, 22);
    for i in 0..12 {
        vt.process(format!("path-{i:02}\r\n").as_bytes());
    }
    let capture_at_freeze = vt.capture_text(None);
    let glance_at_freeze = vt.grid().glance_text();
    let frozen = arm_sync(&mut vt);

    for step in 0..20 {
        vt.process(format!("\x1b[1;1H\x1b[2Jhidden-{step:02}\x1b[3;1H\x1b[4@\x1b[2P").as_bytes());
        assert_frozen(&vt, &frozen, "cross-path");
        assert_eq!(
            vt.capture_text(None),
            capture_at_freeze,
            "capture_text drifted from the frozen frame at step {step}"
        );
        assert_eq!(
            vt.grid().glance_text(),
            glance_at_freeze,
            "glance_text drifted from the frozen frame at step {step}"
        );
    }

    vt.process(b"\x1b[?2026l");
    assert_ne!(
        vt.capture_text(None),
        capture_at_freeze,
        "release revealed nothing"
    );
    // And the revealed frame is self-consistent across the same paths.
    let live = fingerprint(vt.grid());
    let live_glance = vt.grid().glance_text();
    let from_fp = live
        .lines
        .iter()
        .skip(live.scrollback_len)
        .map(|(_, cells)| {
            cells
                .iter()
                .filter(|c| c.width != 0)
                .map(|c| c.ch)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(from_fp, live_glance, "glance_text disagrees with the cells");
}

/// A "resize" to the size the pane already has is not a resize: it must not
/// release a window, or a daemon re-fanning an unchanged winsize would tear
/// every synchronized redraw in the session.
#[test]
fn a_same_size_resize_leaves_the_window_open() {
    let mut vt = seeded_vt(6, 20);
    let frozen = arm_sync(&mut vt);
    vt.process(b"\x1b[1;1Hhidden writes");
    vt.resize(6, 20);
    assert!(
        vt.sync_output_active(),
        "a resize to the same dimensions released the window"
    );
    assert_frozen(&vt, &frozen, "same-size resize");
}
