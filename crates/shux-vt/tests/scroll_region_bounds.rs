//! Bounds and safety on region scrolling driven by attacker-controlled pane
//! input (issue #107, follow-up to #102).
//!
//! #102 clamped *how many times* a scroll runs. It did not bound the cost of
//! one scroll, and it did not make `Grid` safe against a scroll region that no
//! longer fits the grid. Both are reachable from pane bytes:
//!
//!   * a pane that is 0 rows tall (a client can drive one through the zoomed
//!     attach resize path) makes DECSTBM's clamp underflow, so the region can
//!     name rows the grid does not have. `Grid::scroll_up` then removed
//!     nothing and inserted anyway — the deque grew without bound, or the
//!     insert index was past the end and the pane I/O task panicked.
//!   * one region scroll shifted O(rows) deque slots, so scrolling a whole
//!     region cost O(rows^2). That is quadratic work bought with a
//!     fixed-size escape sequence, under the daemon-wide `PaneIoState` lock.
//!
//! Everything here is deterministic except the final scaling test, which
//! asserts a *ratio* between two pane heights rather than an absolute time, so
//! it self-calibrates to the machine it runs on.

use shux_vt::{Grid, GridConfig, VirtualTerminal};

const COLS: usize = 80;

fn vt(rows: usize) -> VirtualTerminal {
    VirtualTerminal::new(rows, COLS)
}

fn vt_rc(rows: usize, cols: usize) -> VirtualTerminal {
    VirtualTerminal::new(rows, cols)
}

/// The structural invariant every grid must hold: the backing deque is exactly
/// the scrollback plus the visible window, and never grows past the visible
/// window plus the configured scrollback cap.
fn assert_grid_invariant(g: &Grid, max_scrollback: usize, what: &str) {
    let rows = g.rows();
    let total = g.total_lines();
    assert!(
        total >= rows,
        "{what}: grid holds {total} lines but claims {rows} visible rows"
    );
    assert_eq!(
        g.scrollback_len(),
        total - rows,
        "{what}: scrollback_len must be total - rows"
    );
    let cap = rows + max_scrollback;
    assert!(
        total <= cap,
        "{what}: grid retains {total} lines on a {rows}-row pane; cap is {cap}"
    );
}

fn assert_vt_invariant(t: &VirtualTerminal, what: &str) {
    assert_grid_invariant(t.grid(), GridConfig::default().max_scrollback, what);
}

// ---------------------------------------------------------------------------
// A degenerate (zero-row) pane must not panic and must not grow the grid.
// ---------------------------------------------------------------------------

const SCROLLERS: &[(&str, &[u8])] = &[
    ("SU", b"\x1b[999999S"),
    ("SD", b"\x1b[999999T"),
    ("IL", b"\x1b[999999L"),
    ("DL", b"\x1b[999999M"),
    ("RI", b"\x1bM"),
    ("IND", b"\x1bD"),
    ("NEL", b"\x1bE"),
    ("LF", b"\n"),
    ("text+LF", b"hello\r\n"),
];

const REGIONS: &[(&str, &[u8])] = &[
    ("none", b""),
    ("stbm 1;9999", b"\x1b[1;9999r"),
    ("stbm 2;9999", b"\x1b[2;9999r"),
    ("stbm 1;65535", b"\x1b[1;65535r"),
    ("stbm 5;3", b"\x1b[5;3r"),
    ("stbm 0;0", b"\x1b[0;0r"),
    ("alt+stbm", b"\x1b[?1049h\x1b[1;9999r"),
    ("stbm+alt", b"\x1b[1;9999r\x1b[?1049h"),
    ("stbm+origin", b"\x1b[2;9999r\x1b[?6h"),
];

#[test]
fn zero_row_pane_survives_every_scroll_sequence() {
    for (rname, region) in REGIONS {
        for (sname, seq) in SCROLLERS {
            let what = format!("rows=0 region={rname} seq={sname}");
            let mut t = vt(24);
            t.resize(0, COLS);
            t.process(region);
            // Repeat: a single pass can look innocent while the grid creeps.
            for _ in 0..8 {
                t.process(seq);
            }
            assert_vt_invariant(&t, &what);
            // A degenerate size is clamped to the smallest real terminal; it
            // never yields a grid with no rows to address.
            assert_eq!(
                t.grid().rows(),
                1,
                "{what}: degenerate pane must clamp to 1 row"
            );
        }
    }
}

#[test]
fn zero_row_pane_does_not_grow_the_grid() {
    let mut t = vt(24);
    t.resize(0, COLS);
    let before = t.grid().total_lines();
    // These 19 bytes bought ~65535 rows of allocation before the fix.
    t.process(b"\x1b[1;65535r\x1b[999999T");
    let after = t.grid().total_lines();
    assert!(
        after <= before,
        "a 0-row pane grew from {before} to {after} retained lines on 19 bytes of input"
    );
}

/// A sustained flood must converge on the scrollback cap rather than climbing
/// past it. Scrolling a one-row screen *does* legitimately push lines into
/// scrollback — the bug was never that the grid grew, but that it grew without
/// a ceiling.
#[test]
fn degenerate_pane_scroll_flood_stays_under_the_scrollback_cap() {
    let mut t = vt(24);
    t.resize(0, COLS);
    t.process(b"\x1b[1;65535r");
    for _ in 0..2000 {
        t.process(b"\x1b[999999S\x1b[999999T\x1b[999999L\x1b[999999M\n\x1bM");
        assert_vt_invariant(&t, "degenerate pane flood");
    }
    // Converged: the cap is the ceiling, and further input does not move it.
    let settled = t.grid().total_lines();
    for _ in 0..500 {
        t.process(b"\x1b[999999S\n");
    }
    assert_eq!(
        t.grid().total_lines(),
        settled,
        "grid kept growing after the scrollback cap was reached"
    );
}

/// A one-row pane is the smallest *legal* pane and exercises the degenerate
/// region (top == bottom) on every path.
#[test]
fn one_row_pane_survives_every_scroll_sequence() {
    for (rname, region) in REGIONS {
        for (sname, seq) in SCROLLERS {
            let what = format!("rows=1 region={rname} seq={sname}");
            let mut t = vt(1);
            t.process(region);
            for _ in 0..8 {
                t.process(seq);
            }
            assert_vt_invariant(&t, &what);
            assert_eq!(t.grid().rows(), 1, "{what}");
        }
    }
}

/// Shrinking to zero rows and back must leave a working terminal, not a
/// corrupted deque.
#[test]
fn resize_through_zero_rows_leaves_a_sane_grid() {
    let mut t = vt(24);
    t.process(b"hello\r\nworld\r\n");
    for rows in [0usize, 1, 0, 2, 0, 40, 0, 24] {
        t.resize(rows, COLS);
        t.process(b"\x1b[1;9999r\x1b[999999S\x1b[999999T\x1b[999999L\x1b[999999M");
        assert_vt_invariant(&t, &format!("resize churn rows={rows}"));
        assert_eq!(t.grid().rows(), rows.max(1));
    }
    t.process(b"\x1b[r\x1b[Hrecovered");
    assert!(
        t.grid().glance_text().contains("recovered"),
        "terminal must still render after resize churn: {:?}",
        t.grid().glance_text()
    );
}

// ---------------------------------------------------------------------------
// Bulk scrolling must be indistinguishable from the same number of one-line
// scrolls, on every region shape.
// ---------------------------------------------------------------------------

/// A grid with identifiable per-row content plus `scrollback` lines behind it.
fn seeded(rows: usize, scrollback: usize) -> Grid {
    let mut g = Grid::new(rows, COLS, GridConfig::default());
    for i in 0..scrollback {
        stamp(&mut g, 0, &format!("sb{i}"));
        g.scroll_up_n(0, rows.saturating_sub(1), 1);
    }
    for r in 0..rows {
        stamp(&mut g, r, &format!("row{r}"));
    }
    g
}

fn stamp(g: &mut Grid, row: usize, text: &str) {
    let mut r = g.visible_row_mut(row);
    for (i, ch) in text.chars().enumerate() {
        r[i].ch = ch;
    }
}

/// Every retained line, trailing blanks trimmed — scrollback included, so a
/// scroll that leaks a row into or out of scrollback is caught.
fn all_lines(g: &Grid) -> Vec<String> {
    (0..g.total_lines())
        .filter_map(|i| g.row(i))
        .map(|r| {
            (0..r.len())
                .filter_map(|c| r.get(c))
                .map(|c| c.ch)
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

#[test]
fn bulk_scroll_up_matches_repeated_single_line_scroll() {
    for rows in [1usize, 2, 3, 8, 24] {
        for scrollback in [0usize, 3, 40] {
            for top in 0..rows {
                for bottom in top..rows {
                    for n in 0..=(rows + 2) {
                        let mut bulk = seeded(rows, scrollback);
                        let mut single = seeded(rows, scrollback);
                        bulk.scroll_up_n(top, bottom, n);
                        for _ in 0..n.min(bottom - top + 1) {
                            single.scroll_up(top, bottom);
                        }
                        assert_eq!(
                            all_lines(&bulk),
                            all_lines(&single),
                            "scroll_up_n(rows={rows} sb={scrollback} top={top} bottom={bottom} n={n})"
                        );
                        assert_grid_invariant(
                            &bulk,
                            GridConfig::default().max_scrollback,
                            "bulk scroll_up_n",
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn bulk_scroll_down_matches_repeated_single_line_scroll() {
    for rows in [1usize, 2, 3, 8, 24] {
        for scrollback in [0usize, 3, 40] {
            for top in 0..rows {
                for bottom in top..rows {
                    for n in 0..=(rows + 2) {
                        let mut bulk = seeded(rows, scrollback);
                        let mut single = seeded(rows, scrollback);
                        bulk.scroll_down_n(top, bottom, n);
                        for _ in 0..n.min(bottom - top + 1) {
                            single.scroll_down(top, bottom);
                        }
                        assert_eq!(
                            all_lines(&bulk),
                            all_lines(&single),
                            "scroll_down_n(rows={rows} sb={scrollback} top={top} bottom={bottom} n={n})"
                        );
                        assert_grid_invariant(
                            &bulk,
                            GridConfig::default().max_scrollback,
                            "bulk scroll_down_n",
                        );
                    }
                }
            }
        }
    }
}

/// A region scroll larger than the region blanks exactly the region.
#[test]
fn scroll_beyond_region_height_blanks_the_region_and_nothing_else() {
    let mut g = seeded(10, 5);
    g.scroll_up_n(3, 6, 999_999);
    let sb = g.scrollback_len();
    let lines = all_lines(&g);
    for (i, line) in lines.iter().enumerate().skip(sb) {
        let visible = i - sb;
        if (3..=6).contains(&visible) {
            assert_eq!(line, "", "row {visible} should be blank");
        } else {
            assert_eq!(
                line,
                &format!("row{visible}"),
                "row {visible} must be untouched"
            );
        }
    }
    assert_grid_invariant(&g, GridConfig::default().max_scrollback, "over-scroll");
}

/// Out-of-range regions are the grid's own responsibility: it is the last
/// thing between a bad index and the deque.
#[test]
fn out_of_range_regions_are_clamped_not_trusted() {
    for rows in [1usize, 4, 24] {
        for (top, bottom, n) in [
            (0usize, usize::MAX, 5usize),
            (usize::MAX, usize::MAX, 5),
            (rows + 10, rows + 20, 5),
            (0, rows + 100, usize::MAX),
            (5, 2, 3),
        ] {
            let what = format!("rows={rows} region={top}..{bottom} n={n}");
            let mut up = seeded(rows, 4);
            let before = up.total_lines();
            up.scroll_up_n(top, bottom, n);
            assert_grid_invariant(&up, GridConfig::default().max_scrollback, &what);
            assert!(
                up.total_lines() <= before + rows,
                "{what}: scroll_up_n must not balloon the grid"
            );

            let mut down = seeded(rows, 4);
            let before = down.total_lines();
            down.scroll_down_n(top, bottom, n);
            assert_grid_invariant(&down, GridConfig::default().max_scrollback, &what);
            assert_eq!(
                down.total_lines(),
                before,
                "{what}: scroll_down_n must never change the retained line count"
            );
        }
    }
}

/// A zero-row grid has no rows to scroll; every region is out of range.
#[test]
fn zero_row_grid_scrolls_are_inert() {
    for (top, bottom, n) in [(0usize, 0usize, 1usize), (0, 65535, 999), (1, 9999, 999)] {
        let mut g = Grid::new(0, COLS, GridConfig::default());
        g.scroll_up_n(top, bottom, n);
        g.scroll_down_n(top, bottom, n);
        assert_eq!(
            g.total_lines(),
            0,
            "0-row grid grew from scroll_up_n/scroll_down_n({top}..{bottom}, {n})"
        );
    }
}

// ---------------------------------------------------------------------------
// Work accounting stays exact: the #102 bound is per scrolled line, and the
// bulk path must not under-report it (content_revision depends on this).
// ---------------------------------------------------------------------------

#[test]
fn bulk_scroll_reports_one_mutation_per_scrolled_line() {
    let mut g = seeded(10, 0);
    let before = g.mutations();
    g.scroll_up_n(2, 6, 3);
    assert_eq!(
        g.mutations() - before,
        3,
        "scrolling 3 lines must count as 3 mutations"
    );

    let before = g.mutations();
    g.scroll_up_n(2, 6, 999);
    assert_eq!(
        g.mutations() - before,
        5,
        "a scroll clamped to a 5-row region must count 5 mutations, not 999"
    );

    let before = g.mutations();
    g.scroll_up_n(2, 6, 0);
    assert_eq!(
        g.mutations() - before,
        0,
        "a zero-line scroll must not count as work"
    );
}

// ---------------------------------------------------------------------------
// Cost: scrolling a region must be linear in the region, not quadratic.
// ---------------------------------------------------------------------------

/// Nanoseconds for `reps` full-region scrolls on a `rows`-tall pane with a
/// full default scrollback behind it.
fn scroll_cost_ns(rows: usize, reps: usize) -> u128 {
    let mut t = vt(rows);
    for i in 0..5000 {
        t.process(format!("sb{i}\r\n").as_bytes());
    }
    t.process(format!("\x1b[2;{}r", rows - 1).as_bytes());
    let payload = b"\x1b[999999S".repeat(reps);
    let start = std::time::Instant::now();
    t.process(&payload);
    start.elapsed().as_nanos().max(1)
}

/// Nanoseconds for the SAME total scrolling done one line at a time.
///
/// This models the defect. `Grid::scroll_up` rotates the region by one line,
/// which is O(region height); doing that once per line is O(height^2) — exactly
/// the cost the bulk API replaced. It is measured live, in this run, on this
/// machine, at this optimisation level, so it can serve as a calibrated upper
/// reference rather than a number recorded once in a comment.
fn per_line_scroll_cost_ns(rows: usize, reps: usize) -> u128 {
    let mut t = vt(rows);
    for i in 0..5000 {
        t.process(format!("sb{i}\r\n").as_bytes());
    }
    t.process(format!("\x1b[2;{}r", rows - 1).as_bytes());
    // One line at a time, region height times, `reps` times over: the same
    // total displacement `\x1b[999999S` achieves in one bulk operation.
    let payload = b"\x1b[1S".repeat(reps * (rows - 2));
    let start = std::time::Instant::now();
    t.process(&payload);
    start.elapsed().as_nanos().max(1)
}

/// Multiplying the pane height by 8 must multiply the cost of scrolling it by
/// roughly 8, not by 64.
///
/// **Both reference points are measured in the same run.** The obvious version
/// of this test compares one ratio against a threshold recorded in a comment —
/// and that threshold is only valid under the machine, cache and codegen it was
/// taken on. It bit exactly that way here: the recorded calibration (8.1x
/// linear, 19.9x quadratic, 12x threshold) was taken at `opt-level = 0`, and
/// when test targets moved to `opt-level = 1` the same, still-linear
/// implementation started reading 11-12.8x and failing about one run in ten.
/// Nothing had regressed. 1024 rows of blanking exceeds L2 where 128 rows does
/// not, and optimising away the interpreter overhead stopped masking it. A
/// ratio cancels out machine SPEED; it does not cancel out the memory
/// hierarchy.
///
/// So the quadratic reference is measured too, from the per-line path that is
/// genuinely O(height^2), and the verdict is which of the two the bulk path
/// resembles. Both arms feel the same cache behaviour and the same codegen, so
/// whatever moves one moves the other and the comparison survives it. The only
/// way this test goes red is if bulk scrolling actually starts scaling like the
/// per-line path — which is the regression it exists to catch.
#[test]
fn region_scroll_cost_is_linear_in_pane_height() {
    let small = 128usize;
    let large = 1024usize;
    let reps = 40;
    const SAMPLES: usize = 5;

    // Warm BOTH sizes: allocator growth and first-touch page faults are paid
    // once per size, and charging them to the large arm's first sample is a
    // bias, not a measurement.
    scroll_cost_ns(small, 4);
    scroll_cost_ns(large, 4);

    // Each arm minimised independently. Taking the best of N *paired* ratios
    // only rejects noise that lands on the small arm; noise on the large arm
    // inflates every pair, and the minimum of inflated pairs is still inflated.
    let (mut bulk_s, mut bulk_l) = (u128::MAX, u128::MAX);
    for _ in 0..SAMPLES {
        bulk_s = bulk_s.min(scroll_cost_ns(small, reps));
        bulk_l = bulk_l.min(scroll_cost_ns(large, reps));
    }
    let bulk_ratio = bulk_l as f64 / bulk_s as f64;

    // The quadratic reference. Fewer reps — it is ~(height) times more work per
    // rep by construction, and the point is its SHAPE, not its absolute cost.
    let q_reps = 1;
    per_line_scroll_cost_ns(small, q_reps);
    let (mut pl_s, mut pl_l) = (u128::MAX, u128::MAX);
    for _ in 0..3 {
        pl_s = pl_s.min(per_line_scroll_cost_ns(small, q_reps));
        pl_l = pl_l.min(per_line_scroll_cost_ns(large, q_reps));
    }
    let per_line_ratio = pl_l as f64 / pl_s as f64;

    let growth = (large / small) as f64;
    eprintln!(
        "region scroll: bulk grows {bulk_ratio:.1}x from {small} to {large} rows; \
         the per-line path grows {per_line_ratio:.1}x. \
         Linear is ~{growth:.0}x, quadratic ~{:.0}x.",
        growth * growth
    );

    // The per-line reference must actually behave quadratically, or it is not a
    // reference and this test would pass against anything. If it ever stops
    // doing so, that is a signal in its own right and deserves a human.
    assert!(
        per_line_ratio > growth * 1.5,
        "the per-line reference grew only {per_line_ratio:.1}x from {small} to {large} rows; \
         it is supposed to be the QUADRATIC arm (~{:.0}x) and can no longer calibrate \
         anything. Did `Grid::scroll_up` stop being O(region height)?",
        growth * growth
    );

    // The verdict: bulk must sit far closer to linear than to the measured
    // quadratic arm. The geometric mean of the two is the neutral midpoint on a
    // scale where both are multiplicative, so it does not favour either.
    let midpoint = (growth * per_line_ratio).sqrt();
    assert!(
        bulk_ratio < midpoint,
        "bulk region scrolling grew {bulk_ratio:.1}x from {small} to {large} rows. \
         Linear is ~{growth:.0}x and the per-line path measured {per_line_ratio:.1}x \
         on this machine, so anything under {midpoint:.1}x is linear-shaped — \
         region scroll is scaling like the per-line path again"
    );
}

// ---------------------------------------------------------------------------
// Defects found by the #107 adversarial review. All three are reachable from
// pane bytes or a client resize, and all three predate this change — they are
// the same class the issue is about, so they are fixed here rather than filed.
// ---------------------------------------------------------------------------

/// A cursor BELOW the scroll region is not equal to `region.bottom`, so the
/// auto-wrap path incremented it straight off the grid. `write_char` clamps
/// the cursor before the wide-character wrap branch and not after, so only a
/// wide glyph at the right edge reached the bad row — the shape any app with a
/// bottom status line outside its scroll region produces when it draws CJK or
/// emoji at the last column.
#[test]
fn wide_char_at_right_edge_below_the_scroll_region_does_not_walk_off_the_grid() {
    for (rows, cols, region_bottom) in [(24usize, 80usize, 23u16), (4, 6, 2), (2, 2, 1)] {
        for glyph in ["中", "🙂", "a"] {
            let what = format!("{rows}x{cols} region 1;{region_bottom} glyph {glyph}");
            let mut t = vt_rc(rows, cols);
            t.process(format!("\x1b[1;{region_bottom}r").as_bytes());
            t.process(format!("\x1b[{rows};{cols}H").as_bytes());
            t.process(glyph.as_bytes());
            // The real assertion is "did not panic"; the invariant catches any
            // corruption that stopped short of a panic.
            assert_vt_invariant(&t, &what);
            assert_eq!(t.grid().rows(), rows, "{what}");
        }
    }
}

/// The realistic shape: an app with a bottom status line outside its scroll
/// region, repainting the region and then drawing a wide glyph at the last
/// column of the status line. The trigger is the glyph landing ON the final
/// column — filling the row and letting it overflow takes a different path and
/// does NOT reproduce, which an earlier version of this test got wrong.
#[test]
fn status_line_below_the_region_survives_a_wide_glyph_at_the_last_column() {
    let mut t = vt_rc(24, 80);
    t.process(b"\x1b[1;23r");
    for row in 1..=23 {
        t.process(format!("\x1b[{row};1H").as_bytes());
        t.process("scrollable content ".repeat(4).as_bytes());
    }
    // Status line, row 24, wide glyph in the last cell.
    t.process(b"\x1b[24;1H status \x1b[24;80H");
    t.process("中".as_bytes());
    assert_vt_invariant(&t, "status line wide glyph at last column");

    t.process(b"\x1b[r\x1b[Hstill alive");
    assert!(
        t.grid().glance_text().contains("still alive"),
        "pane stopped parsing: {:?}",
        t.grid().glance_text()
    );
}

/// DECSET 1047 enters the alternate screen WITHOUT saving a cursor. Resizing
/// while it is active left the stashed primary grid at its old size, so on
/// leaving 1047 the pane reported one geometry and rendered another — and the
/// next erase or insert indexed a row that was no longer there.
#[test]
fn alternate_screen_without_a_saved_cursor_still_resizes_the_primary_grid() {
    for mode in ["1047", "1049", "47"] {
        for (from, to) in [((3usize, 1usize), (9usize, 2usize)), ((2, 80), (24, 80))] {
            let what = format!("mode {mode} {from:?} -> {to:?}");
            let mut t = vt_rc(from.0, from.1);
            t.process(format!("\x1b[?{mode}h").as_bytes());
            t.resize(to.0, to.1);
            t.process(format!("\x1b[?{mode}l").as_bytes());
            assert_eq!(
                (t.grid().rows(), t.grid().cols()),
                to,
                "{what}: primary grid kept its pre-resize size"
            );
            // The desync used to surface as a panic on the next cell write.
            t.process(format!("\x1b[{};1H\x1b[5X\x1b[5@\x1b[5P", to.0).as_bytes());
            assert_vt_invariant(&t, &what);
        }
    }
}

/// `n` is clamped to the region height, and for a full-screen region that is a
/// deliberate divergence from `n` separate one-line scrolls: the loop would
/// push `n` lines into scrollback, all but a screenful of them blank. Pinning
/// it so the clamp cannot be "fixed" into a scrollback flood.
#[test]
fn region_scroll_beyond_screen_height_does_not_flood_scrollback() {
    let cfg = GridConfig {
        max_scrollback: 1000,
        track_dirty: true,
    };
    let mut bulk = Grid::new(4, 4, cfg.clone());
    bulk.scroll_up_n(0, 3, 100);
    assert_eq!(
        (bulk.total_lines(), bulk.scrollback_len()),
        (8, 4),
        "one sequence must not push more than a screenful into scrollback"
    );

    let mut loops = Grid::new(4, 4, cfg);
    for _ in 0..100 {
        loops.scroll_up(0, 3);
    }
    assert_eq!(
        (loops.total_lines(), loops.scrollback_len()),
        (104, 100),
        "the documented divergence changed — update the scroll_up_n docs"
    );
}
