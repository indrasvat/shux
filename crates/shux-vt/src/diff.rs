//! Lens-gate cell comparator (task 079).
//!
//! ONE diff definition, [`diff_frames`], usable over both a live [`Grid`] and a
//! deserialized [`FrameEnvelope`] golden — via the [`CellGridView`] trait — with
//! no fabricated `Grid`. Lifted verbatim from the daemon's former
//! `compute_lens_diff` (lens PRD §7.2, LENS-R-034..038b); the daemon's
//! `pane.diff_since` is now a thin adapter over this (output byte-identical).
//!
//! Design rulings folded from the task-079 DootSabha design review:
//!
//! - **Owned `CellRef`** (council #3): `cell()` returns a by-value snapshot so a
//!   golden view never has to hand out a borrow into a decode buffer. Equality is
//!   value-exact to `Cell: Eq` (its `extended: Arc<_>` compares inner values).
//! - **Geometry is first-class** (design-r1): a general golden-vs-live comparator
//!   must not silently min-crop, so [`FrameDiff::geometry_changed`] flags any
//!   dimension mismatch. The overlap diff is still computed over the `min` dims
//!   for diagnostics, but a consumer (task 080's gate) treats `geometry_changed`
//!   as decisive. `pane.diff_since` never diffs unequal dims (resize/alt-screen
//!   invalidate the checkpoint first), so it stays false there and unserialized.
//! - **Palette is a portability diagnostic, not "changed"** (design-r2):
//!   [`FrameDiff::palette_overridden_differs`] reports that the sticky OSC-4
//!   *history* bit (task-078 R1) differs between the two sides. It is deliberately
//!   NOT folded into `cells_changed`. Task 080 escalates it to `palette_unportable`
//!   only when indexed-colour cells are also present.

use serde::{Deserialize, Serialize};

use crate::capture::{CaptureError, FrameEnvelope};
use crate::cell::{Cell, Color, TerminalDefaultColors};
use crate::grid::Grid;

/// An owned, value-comparable snapshot of one grid cell (council #3: by-value so a
/// golden view never borrows an RLE-decode temporary). Equality is exactly
/// `Cell: Eq`, so [`diff_frames`] counts a cell changed on the same terms the old
/// `&Cell != &Cell` comparison did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellRef(Cell);

impl CellRef {
    /// Wrap an owned cell.
    pub fn new(cell: Cell) -> Self {
        CellRef(cell)
    }

    /// Whether this is a wide-character head (width 2).
    pub fn is_wide(&self) -> bool {
        self.0.is_wide()
    }

    /// Whether this is a wide-character continuation placeholder (width 0).
    pub fn is_wide_continuation(&self) -> bool {
        self.0.is_wide_continuation()
    }

    /// Foreground colour (before OSC-default resolution).
    pub fn fg(&self) -> Color {
        self.0.style.fg
    }

    /// Background colour (before OSC-default resolution).
    pub fn bg(&self) -> Color {
        self.0.style.bg
    }

    /// The wrapped cell (escape hatch for callers that need the full value).
    pub fn cell(&self) -> &Cell {
        &self.0
    }
}

/// The cursor facts the comparator observes: position + visibility. **No shape** —
/// the former `cursor_moved` compared `(row,col,visible)` only, and carrying shape
/// would break `pane.diff_since` parity. A cursor-shape-only change is therefore a
/// documented cell-tier blind spot that task 080's pixel tier covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorState {
    pub row: usize,
    pub col: usize,
    pub visible: bool,
}

/// A rectangular grid of cells plus the ambient terminal state the comparator
/// needs. Implemented by a live [`GridFrame`] and a golden [`FrameView`] alike, so
/// [`diff_frames`] has one code path over both. Object-safe (every method returns
/// owned data), so `&dyn CellGridView` works.
///
/// Dimension contract: an impl MUST report `rows()`/`cols()` whose product fits in
/// `usize` (`diff_frames` allocates a `rows × cols` mask), and — because the diff's
/// per-row spans and bounding box are `u16` (the `pane.diff_since` wire schema) —
/// dimensions above `u16::MAX` truncate. Both crate impls satisfy this: `GridFrame`
/// wraps a real terminal `Grid` and `FrameView` a `u16`-sized envelope; only a
/// hand-rolled view with absurd dims can violate it (task-079 adversarial B-F2/F3).
pub trait CellGridView {
    fn rows(&self) -> usize;
    fn cols(&self) -> usize;
    /// The cell at `(row, col)`; an out-of-range coordinate yields [`Cell::EMPTY`].
    fn cell(&self, row: usize, col: usize) -> CellRef;
    /// This frame's OSC 10/11/12 dynamic default colours.
    fn defaults(&self) -> TerminalDefaultColors;
    fn cursor(&self) -> CursorState;
    /// The sticky OSC-4 palette-override *history* bit (task-078 R1).
    fn palette_overridden(&self) -> bool;
    /// Whether this frame is the ALTERNATE screen (task-080). Defaulted to `false` so the
    /// frozen [`diff_frames`] path and the live [`GridFrame`] (daemon `pane.diff_since`,
    /// which never diffs across an alt-switch) are byte-unchanged; only [`FrameView`]
    /// overrides it, so the lens gate's [`compare_cell`](crate::compare_cell) can treat an
    /// alt/primary flip between a golden and a live capture as a difference.
    ///
    /// [`compare_cell`]: crate::compare_cell
    fn alt_screen(&self) -> bool {
        false
    }
}

/// One per-row changed-column span (LENS-R-035): 0-based half-open
/// `[col_start, col_end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LensRowSpan {
    pub row: u16,
    pub col_start: u16,
    pub col_end: u16,
}

/// The structured delta between two frames (lens PRD §7.2, LENS-R-034..038b). Moved
/// out of the daemon (council #2) so `shux-vt` owns the diff shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameDiff {
    pub cells_changed: u32,
    pub regions: Vec<LensRowSpan>,
    pub regions_truncated: bool,
    /// `(row_start, col_start, row_end, col_end)` — 0-based HALF-OPEN in both axes;
    /// all zeros when nothing changed.
    pub bounding_box: (u16, u16, u16, u16),
    pub cursor_moved: bool,
    /// The two sides' sticky OSC-4 override *history* bits differ (design-r2). A
    /// portability DIAGNOSTIC, never folded into `cells_changed`. **Not sufficient
    /// for a portability verdict** (council-v2 pin 2): both sides can be `true`
    /// (so this is `false`) while the frame pair is still unportable. Task 080's
    /// `palette_unportable` is a PER-FRAME check —
    /// `(overridden_a && has_indexed_a) || (overridden_b && has_indexed_b)` — read
    /// from each view's `palette_overridden()`, not from this field.
    pub palette_overridden_differs: bool,
    /// The two frames' dimensions differ (design-r1). Decisive for the gate; the
    /// overlap diff below is computed over the `min` dims for diagnostics only.
    pub geometry_changed: bool,
    /// Rows (grid index) with ≥1 changed cell, ascending.
    pub changed_rows: Vec<usize>,
    /// Flat `rows × cols` changed mask for the heat overlay (LENS-R-037).
    pub changed_mask: Vec<bool>,
    /// The compared (OVERLAP / `min`) dimensions the `changed_mask` is sized to —
    /// NOT the original frame sizes (council-v2 pin 1). When `geometry_changed`,
    /// task 080 reports the two original sizes from the envelopes/views; here
    /// `rows`/`cols` are only the compared window.
    pub rows: usize,
    pub cols: usize,
}

/// Compute the structured diff of `b` (current / live) against `a` (checkpoint /
/// golden). A cell counts as changed iff its [`Cell`] value differs, EXCEPT
/// `Color::Default` is resolved against each side's OSC 10/11 defaults: when a
/// side's fg (or bg) default differs, a cell that is `Default` in that channel on
/// BOTH sides counts as changed (LENS-R-038b) — a default-only repaint presents
/// every default-coloured cell differently. Wide head+spacer pair (LENS-R-034).
/// Runs merge into per-row half-open spans; past 256 spans `regions_truncated` is
/// set and only `bounding_box` is meaningful (LENS-R-035). `cursor_moved`,
/// `palette_overridden_differs`, and `geometry_changed` are reported separately and
/// never enter `cells_changed`.
///
/// Faithfully preserved over-report (lifted from the old `compute_lens_diff`,
/// task-079 adversarial B-F1): a COLOURED wide glyph stores a concrete bg on its
/// head but `Default` bg on its spacer (a `wide_continuation` cell). Under an
/// OSC-default-bg change the default clause flips the spacer, then wide-pairing
/// propagates the flip to the concrete-bg head — so a concrete wide head DOES count
/// on a default-bg repaint. This is over-report only (a false FAIL for a gate, never
/// a false PASS), and is pinned by `default_bg_change_flips_colored_wide_head`.
pub fn diff_frames(a: &dyn CellGridView, b: &dyn CellGridView) -> FrameDiff {
    let geometry_changed = a.rows() != b.rows() || a.cols() != b.cols();
    // Overlap dims. For `pane.diff_since` the two sides are always equal (resize
    // invalidates checkpoints); `min` is the general comparator's defensive crop,
    // made non-silent by `geometry_changed`.
    let rows = a.rows().min(b.rows());
    let cols = a.cols().min(b.cols());

    let a_def = a.defaults();
    let b_def = b.defaults();
    let fg_default_changed = a_def.fg != b_def.fg;
    let bg_default_changed = a_def.bg != b_def.bg;

    let mut changed = vec![false; rows * cols];
    // `a.is_wide() || b.is_wide()` per cell, gathered in the same pass so each cell
    // is fetched (and cloned) exactly once.
    let mut wide = vec![false; rows * cols];
    for r in 0..rows {
        for c in 0..cols {
            let ca = a.cell(r, c);
            let cb = b.cell(r, c);
            let idx = r * cols + c;
            wide[idx] = ca.is_wide() || cb.is_wide();
            let differ = ca != cb
                || (fg_default_changed && ca.fg() == Color::Default && cb.fg() == Color::Default)
                || (bg_default_changed && ca.bg() == Color::Default && cb.bg() == Color::Default);
            if differ {
                changed[idx] = true;
            }
        }
    }

    // Wide-glyph pairing (LENS-R-034): a wide head and its spacer are one visual
    // unit — if either half changed, both count.
    for r in 0..rows {
        for c in 0..cols.saturating_sub(1) {
            let i = r * cols + c;
            if wide[i] && (changed[i] || changed[i + 1]) {
                changed[i] = true;
                changed[i + 1] = true;
            }
        }
    }

    // Build spans (per row, contiguous runs), count, bbox, changed rows.
    const MAX_SPANS: usize = 256;
    let mut regions: Vec<LensRowSpan> = Vec::new();
    let mut changed_rows: Vec<usize> = Vec::new();
    let mut cells_changed: u32 = 0;
    let (mut min_row, mut min_col, mut max_row, mut max_col) =
        (usize::MAX, usize::MAX, 0usize, 0usize);

    for r in 0..rows {
        let mut row_had_change = false;
        let mut c = 0;
        while c < cols {
            if changed[r * cols + c] {
                let start = c;
                while c < cols && changed[r * cols + c] {
                    cells_changed += 1;
                    c += 1;
                }
                // `c` is one past the run — half-open [start, c).
                regions.push(LensRowSpan {
                    row: r as u16,
                    col_start: start as u16,
                    col_end: c as u16,
                });
                row_had_change = true;
                min_row = min_row.min(r);
                max_row = max_row.max(r);
                min_col = min_col.min(start);
                max_col = max_col.max(c - 1);
            } else {
                c += 1;
            }
        }
        if row_had_change {
            changed_rows.push(r);
        }
    }

    let regions_truncated = regions.len() > MAX_SPANS;
    if regions_truncated {
        regions.clear();
    }

    let bounding_box = if cells_changed == 0 {
        (0, 0, 0, 0)
    } else {
        (
            min_row as u16,
            min_col as u16,
            (max_row + 1) as u16,
            (max_col + 1) as u16,
        )
    };

    FrameDiff {
        cells_changed,
        regions,
        regions_truncated,
        bounding_box,
        cursor_moved: a.cursor() != b.cursor(),
        palette_overridden_differs: a.palette_overridden() != b.palette_overridden(),
        geometry_changed,
        changed_rows,
        changed_mask: changed,
        rows,
        cols,
    }
}

// ── Live view: borrow a Grid + its ambient VT state ─────────────────────────────

/// A live [`Grid`] plus the ambient terminal state a bare `Grid` does not carry
/// (OSC defaults, cursor, palette bit live on the `VirtualTerminal`). The design
/// review confirmed this wrapper is mandatory: `impl CellGridView for &Grid` cannot
/// satisfy `defaults()`/`cursor()`/`palette_overridden()`.
pub struct GridFrame<'a> {
    grid: &'a Grid,
    defaults: TerminalDefaultColors,
    cursor: CursorState,
    palette_overridden: bool,
}

impl<'a> GridFrame<'a> {
    pub fn new(
        grid: &'a Grid,
        defaults: TerminalDefaultColors,
        cursor: CursorState,
        palette_overridden: bool,
    ) -> Self {
        GridFrame {
            grid,
            defaults,
            cursor,
            palette_overridden,
        }
    }
}

impl CellGridView for GridFrame<'_> {
    fn rows(&self) -> usize {
        self.grid.rows()
    }
    fn cols(&self) -> usize {
        self.grid.cols()
    }
    fn cell(&self, row: usize, col: usize) -> CellRef {
        // Map the visible row to its absolute index and use the Option-returning
        // `row()` so an out-of-range coordinate yields `Cell::EMPTY` per the trait
        // contract, rather than `visible_row()` panicking on an OOB row (impl-review
        // MINOR). For in-range rows this equals `visible_row(row)`.
        CellRef(
            self.grid
                .row(self.grid.scrollback_len() + row)
                .and_then(|r| r.get(col))
                .cloned()
                .unwrap_or(Cell::EMPTY),
        )
    }
    fn defaults(&self) -> TerminalDefaultColors {
        self.defaults
    }
    fn cursor(&self) -> CursorState {
        self.cursor
    }
    fn palette_overridden(&self) -> bool {
        self.palette_overridden
    }
}

// ── Golden view: a validated, decoded FrameEnvelope ─────────────────────────────

/// A golden [`FrameEnvelope`] decoded into comparable cells. Built only via
/// [`FrameEnvelope::try_view`], which VALIDATES first (design-r4) so a malformed /
/// non-canonical golden fails loudly instead of being silently normalized by
/// `to_cells`. The internal `Vec<Vec<Cell>>` is an impl detail behind the owned
/// `cell()` contract, not the trait-level "materialized cache" council #3 rejected.
pub struct FrameView {
    cells: Vec<Vec<Cell>>,
    rows: usize,
    cols: usize,
    defaults: TerminalDefaultColors,
    cursor: CursorState,
    palette_overridden: bool,
    alt_screen: bool,
}

impl FrameEnvelope {
    /// Decode this golden into a [`FrameView`] for comparison, validating
    /// canonicality first (design-r4). Never trust `to_cells` on an unvalidated
    /// envelope — a malformed golden must fail, not normalize.
    pub fn try_view(&self) -> Result<FrameView, CaptureError> {
        self.validate()?;
        // Reject an absurd size BEFORE `to_cells` eagerly allocates
        // `rows × cols` cells (task-079 adversarial: a schema-valid 65535×65535
        // golden would allocate ~95 GB and the allocator would ABORT — uncatchable,
        // violating R9's "typed error, never a panic"). A real golden is orders of
        // magnitude smaller (the lens pixel budget caps renders at 16M PIXELS and a
        // cell spans many pixels), so this never rejects a legitimate frame.
        const MAX_VIEW_CELLS: usize = 16_000_000;
        let (rows, cols) = (self.size.rows as usize, self.size.cols as usize);
        if rows.saturating_mul(cols) > MAX_VIEW_CELLS {
            return Err(CaptureError::NonCanonical {
                row: 0,
                detail: format!(
                    "grid {rows}x{cols} exceeds the comparator's {MAX_VIEW_CELLS}-cell limit"
                ),
            });
        }
        let defaults = TerminalDefaultColors {
            fg: self.defaults.fg,
            bg: self.defaults.bg,
            cursor: self.defaults.cursor,
        };
        let cursor = CursorState {
            row: self.cursor.row as usize,
            col: self.cursor.col as usize,
            visible: self.cursor.visible,
        };
        Ok(FrameView {
            cells: self.to_cells(),
            rows: self.size.rows as usize,
            cols: self.size.cols as usize,
            defaults,
            cursor,
            palette_overridden: self.palette_overridden,
            alt_screen: self.alt_screen,
        })
    }
}

impl CellGridView for FrameView {
    fn rows(&self) -> usize {
        self.rows
    }
    fn cols(&self) -> usize {
        self.cols
    }
    fn cell(&self, row: usize, col: usize) -> CellRef {
        CellRef(
            self.cells
                .get(row)
                .and_then(|r| r.get(col))
                .cloned()
                .unwrap_or(Cell::EMPTY),
        )
    }
    fn defaults(&self) -> TerminalDefaultColors {
        self.defaults
    }
    fn cursor(&self) -> CursorState {
        self.cursor
    }
    fn palette_overridden(&self) -> bool {
        self.palette_overridden
    }
    fn alt_screen(&self) -> bool {
        self.alt_screen
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VirtualTerminal;
    use crate::capture::{FrameEnvelope, MaskSet};

    fn capture(vt: &VirtualTerminal) -> FrameEnvelope {
        FrameEnvelope::from_terminal(vt, &MaskSet::new())
    }

    fn cursor_state(vt: &VirtualTerminal) -> CursorState {
        let c = vt.cursor();
        CursorState {
            row: c.row,
            col: c.col,
            visible: c.visible,
        }
    }

    /// Holds a VT's ambient state + a borrowed grid so a test can mint a
    /// `GridFrame` inline without lifetime churn.
    struct GridFrameOwned<'a> {
        defaults: TerminalDefaultColors,
        cursor: CursorState,
        palette_overridden: bool,
        grid: &'a Grid,
    }
    impl<'a> GridFrameOwned<'a> {
        fn view(&self) -> GridFrame<'a> {
            GridFrame::new(
                self.grid,
                self.defaults,
                self.cursor,
                self.palette_overridden,
            )
        }
    }
    fn grid_frame<'a>(vt: &VirtualTerminal, grid: &'a Grid) -> GridFrameOwned<'a> {
        GridFrameOwned {
            defaults: vt.default_colors(),
            cursor: cursor_state(vt),
            palette_overridden: vt.palette_overridden(),
            grid,
        }
    }

    // ── L1 ownership: CellRef is owned; a cell() result outlives the view ────────
    #[test]
    fn cellref_outlives_its_view() {
        let mut vt = VirtualTerminal::new(2, 6);
        vt.process(b"hi");
        let env = capture(&vt);
        // The view is dropped at the end of the inner scope; the CellRef survives.
        // This only compiles because `cell()` returns an OWNED value, never a
        // borrow into the FrameView's decode buffer (council #3).
        let escaped: CellRef = {
            let view = env.try_view().expect("canonical");
            view.cell(0, 0)
        };
        assert_eq!(escaped.cell().ch, 'h');
    }

    // ── L1 view: golden FrameView == live GridFrame for the same source ─────────
    #[test]
    fn frameview_equals_gridframe_for_same_source() {
        // A frame mixing colours, styles, a wide glyph, a grapheme, and blanks —
        // both views must yield the identical FrameDiff against another such frame.
        let programs: &[&[u8]] = &[
            b"\x1b[1;1Hplain",
            "\x1b[1;1H\x1b[38;2;10;20;30mA\x1b[48;5;28mB\x1b[0m\x1b[2;1H\u{7d42}xy".as_bytes(),
            "e\u{0301}\x1b[1;4H\u{1F1FA}\u{1F1F8}Z".as_bytes(),
        ];
        for a_prog in programs {
            for b_prog in programs {
                let mut va = VirtualTerminal::new(3, 12);
                va.process(a_prog);
                let mut vb = VirtualTerminal::new(3, 12);
                vb.process(b_prog);

                let ga = va.grid().clone_visible();
                let gb = vb.grid().clone_visible();
                let gfa = grid_frame(&va, &ga);
                let gfb = grid_frame(&vb, &gb);
                let via_grid = diff_frames(&gfa.view(), &gfb.view());

                let ea = capture(&va).try_view().expect("A canonical");
                let eb = capture(&vb).try_view().expect("B canonical");
                let via_view = diff_frames(&ea, &eb);

                assert_eq!(
                    via_grid, via_view,
                    "golden view diff must equal live grid diff for {a_prog:?} vs {b_prog:?}"
                );
            }
        }
    }

    // ── L1 defaults: theme mismatch is detected (the false-pass BLOCKER) ────────
    #[test]
    fn theme_mismatch_same_cells_reports_change() {
        // Identical cells, but the current side's OSC 11 default bg differs: every
        // Default-bg cell must count. This is the council BLOCKER that must not
        // recur (a default-colour repaint is invisible to raw Cell equality).
        let mut vt = VirtualTerminal::new(2, 5);
        vt.process(b"\x1b[1;1HAB");
        let grid = vt.grid().clone_visible();
        let base = TerminalDefaultColors::default();
        let themed = TerminalDefaultColors {
            bg: Some([32, 64, 96]),
            ..base
        };
        let cur = cursor_state(&vt);
        let a = GridFrame::new(&grid, base, cur, false);
        let b = GridFrame::new(&grid, themed, cur, false);
        let diff = diff_frames(&a, &b);
        assert!(diff.cells_changed > 0, "theme mismatch must be detected");
        assert_eq!(
            diff.cells_changed, 10,
            "all 2×5 default-bg cells count (A,B have default bg)"
        );
        // The reverse (same defaults) is a no-op — pins byte-identity to raw eq.
        let same = diff_frames(
            &GridFrame::new(&grid, base, cur, false),
            &GridFrame::new(&grid, base, cur, false),
        );
        assert_eq!(same.cells_changed, 0);
    }

    // ── L1 wide: wide + grapheme cells pair across both views ───────────────────
    #[test]
    fn wide_glyph_pairs_across_views() {
        let mut before = VirtualTerminal::new(2, 10);
        before.process(b"\x1b[1;1H  ");
        let mut after = VirtualTerminal::new(2, 10);
        after.process("\x1b[1;1H\u{7d42}".as_bytes()); // 終 width 2 at cols 0-1

        let gb = before.grid().clone_visible();
        let ga = after.grid().clone_visible();
        let via_grid = diff_frames(
            &GridFrame::new(&gb, before.default_colors(), cursor_state(&before), false),
            &GridFrame::new(&ga, after.default_colors(), cursor_state(&after), false),
        );
        assert_eq!(via_grid.cells_changed, 2, "wide head + spacer both count");
        assert!(via_grid.changed_mask[0] && via_grid.changed_mask[1]);
        assert_eq!(via_grid.regions.len(), 1);
        assert_eq!(
            (
                via_grid.regions[0].row,
                via_grid.regions[0].col_start,
                via_grid.regions[0].col_end
            ),
            (0, 0, 2)
        );

        // Same via golden views.
        let ev_b = capture(&before).try_view().unwrap();
        let ev_a = capture(&after).try_view().unwrap();
        assert_eq!(diff_frames(&ev_b, &ev_a), via_grid);
    }

    // ── design-r1: geometry mismatch is first-class ─────────────────────────────
    #[test]
    fn geometry_mismatch_flagged() {
        let mut small = VirtualTerminal::new(2, 5);
        small.process(b"hi");
        let mut big = VirtualTerminal::new(3, 5);
        big.process(b"hi");
        let gs = small.grid().clone_visible();
        let gb = big.grid().clone_visible();
        let diff = diff_frames(
            &GridFrame::new(&gs, small.default_colors(), cursor_state(&small), false),
            &GridFrame::new(&gb, big.default_colors(), cursor_state(&big), false),
        );
        assert!(
            diff.geometry_changed,
            "row-count mismatch must flag geometry"
        );
        assert_eq!(diff.rows, 2, "overlap dims are the min");
        // Equal dims never flag it.
        let same = diff_frames(
            &GridFrame::new(&gs, small.default_colors(), cursor_state(&small), false),
            &GridFrame::new(&gs, small.default_colors(), cursor_state(&small), false),
        );
        assert!(!same.geometry_changed);
    }

    // ── design-r2: palette diagnostic — honest, not folded into cells_changed ────
    #[test]
    fn palette_override_differs_is_diagnostic_only() {
        let mut vt = VirtualTerminal::new(1, 4);
        vt.process(b"\x1b[31mAB\x1b[0m"); // indexed-1 fg cells present
        let grid = vt.grid().clone_visible();
        let cur = cursor_state(&vt);
        let d = vt.default_colors();
        // Same cells, one side's sticky OSC-4 bit set.
        let diff = diff_frames(
            &GridFrame::new(&grid, d, cur, false),
            &GridFrame::new(&grid, d, cur, true),
        );
        assert!(
            diff.palette_overridden_differs,
            "sticky-bit mismatch reported"
        );
        assert_eq!(
            diff.cells_changed, 0,
            "palette history is NOT folded into cells_changed"
        );
        assert!(!diff.cursor_moved && !diff.geometry_changed);
        // Both false → no diagnostic.
        let none = diff_frames(
            &GridFrame::new(&grid, d, cur, false),
            &GridFrame::new(&grid, d, cur, false),
        );
        assert!(!none.palette_overridden_differs);
    }

    // ── cursor: position/visibility move; shape-only is a documented blind spot ──
    #[test]
    fn cursor_moves_and_shape_blind_spot() {
        let mut vt = VirtualTerminal::new(2, 6);
        vt.process(b"ab");
        let grid = vt.grid().clone_visible();
        let d = vt.default_colors();
        let at_00 = CursorState {
            row: 0,
            col: 0,
            visible: true,
        };
        let at_01 = CursorState {
            row: 0,
            col: 1,
            visible: true,
        };
        let hidden = CursorState {
            row: 0,
            col: 0,
            visible: false,
        };

        let moved = diff_frames(
            &GridFrame::new(&grid, d, at_00, false),
            &GridFrame::new(&grid, d, at_01, false),
        );
        assert!(moved.cursor_moved && moved.cells_changed == 0);

        let vis = diff_frames(
            &GridFrame::new(&grid, d, at_00, false),
            &GridFrame::new(&grid, d, hidden, false),
        );
        assert!(vis.cursor_moved, "visibility flip is a cursor move");

        // Shape-only: CursorState has no shape, so two envelopes differing ONLY in
        // cursor.shape produce cursor_moved=false — the documented cell-tier blind
        // spot task 080's pixel tier covers.
        let mut block = VirtualTerminal::new(2, 6);
        block.process(b"\x1b[2 qab"); // steady block
        let mut bar = VirtualTerminal::new(2, 6);
        bar.process(b"\x1b[6 qab"); // steady bar
        let vb = capture(&block).try_view().unwrap();
        let vr = capture(&bar).try_view().unwrap();
        let shape = diff_frames(&vb, &vr);
        assert!(
            !shape.cursor_moved && shape.cells_changed == 0,
            "cursor-shape-only is invisible to the cell tier (blind spot)"
        );
    }

    // ── ported: default-colour change marks default cells; concrete stay ────────
    #[test]
    fn default_color_change_marks_default_cells() {
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"\x1b[1;1HAB\x1b[2;3H\x1b[38;2;1;2;3m\x1b[48;2;4;5;6mX\x1b[0m");
        let grid = vt.grid().clone_visible();
        let cur = cursor_state(&vt);
        let base = TerminalDefaultColors::default();
        let bg_changed = TerminalDefaultColors {
            bg: Some([32, 64, 96]),
            ..base
        };
        let diff = diff_frames(
            &GridFrame::new(&grid, base, cur, false),
            &GridFrame::new(&grid, bg_changed, cur, false),
        );
        assert_eq!(
            diff.cells_changed, 29,
            "3×10 minus the one concrete-bg cell"
        );
        assert!(!diff.changed_mask[10 + 2], "concrete-bg cell NOT marked");
        let spans: Vec<(u16, u16, u16)> = diff
            .regions
            .iter()
            .map(|s| (s.row, s.col_start, s.col_end))
            .collect();
        assert_eq!(spans, vec![(0, 0, 10), (1, 0, 2), (1, 3, 10), (2, 0, 10)]);
        assert_eq!(diff.bounding_box, (0, 0, 3, 10));
    }

    // ── ported: unchanged defaults == raw Cell equality ─────────────────────────
    #[test]
    fn unchanged_defaults_matches_raw() {
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"\x1b[1;1Hhello");
        let cp = vt.grid().clone_visible();
        vt.process(b"\x1b[2;4H\x1b[48;5;28mZW\x1b[0m");
        let cur = vt.grid().clone_visible();
        let cs = cursor_state(&vt);
        let none = TerminalDefaultColors::default();
        let osc = TerminalDefaultColors {
            fg: Some([250, 250, 250]),
            bg: Some([32, 64, 96]),
            cursor: Some([255, 128, 0]),
        };
        let raw = diff_frames(
            &GridFrame::new(&cp, none, cs, false),
            &GridFrame::new(&cur, none, cs, false),
        );
        let same_osc = diff_frames(
            &GridFrame::new(&cp, osc, cs, false),
            &GridFrame::new(&cur, osc, cs, false),
        );
        assert_eq!(raw.cells_changed, 2, "exactly the ZW cells");
        assert_eq!(same_osc.cells_changed, raw.cells_changed);
        assert_eq!(same_osc.changed_mask, raw.changed_mask);
        assert_eq!(same_osc.bounding_box, raw.bounding_box);
    }

    // ── C-BLOCKER (adversarial): golden path == live path UNDER SCROLLBACK ───────
    #[test]
    fn frameview_equals_gridframe_with_scrollback() {
        // A scrolled VT: capture must snapshot the VISIBLE viewport, not the oldest
        // scrollback. Before the capture.rs fix this diffed 10 cells (fixed-point
        // break); it must be 0 — same source frame, both views (task-079 C-BLOCKER).
        let mut vt = VirtualTerminal::new(4, 10);
        for i in 0..20u8 {
            vt.process(format!("line{i:02}\r\n").as_bytes());
        }
        assert!(
            vt.grid().scrollback_len() > 0,
            "precondition: the VT has scrolled"
        );
        let g = vt.grid().clone_visible();
        let live = GridFrame::new(
            &g,
            vt.default_colors(),
            cursor_state(&vt),
            vt.palette_overridden(),
        );
        let gold = capture(&vt).try_view().expect("canonical");
        assert_eq!(
            diff_frames(&live, &gold).cells_changed,
            0,
            "capture must snapshot the visible viewport, not scrollback"
        );
    }

    // ── A-F2 (adversarial): try_view VALIDATES before decoding ──────────────────
    #[test]
    fn try_view_rejects_malformed_instead_of_decoding() {
        // Deleting `self.validate()?` from try_view would let to_cells silently
        // normalize these (truncate the over-width run) into a plausible-but-wrong
        // view — a silent gate false-pass with no other test failing (task-079 A-F2).
        let past = FrameEnvelope::from_canonical_json(
            r#"{"schema":1,"size":{"rows":1,"cols":3},"alt_screen":false,"defaults":{},"cursor":{"row":0,"col":0,"visible":true,"shape":"block"},"palette_overridden":false,"rows":[{"row":0,"runs":[[0,"abcdef"]]}]}"#,
        )
        .expect("parses");
        assert!(
            matches!(
                past.try_view(),
                Err(crate::capture::CaptureError::NonCanonical { .. })
            ),
            "a run past the grid width must be rejected, not decoded truncated"
        );
        let cur = FrameEnvelope::from_canonical_json(
            r#"{"schema":1,"size":{"rows":1,"cols":5},"alt_screen":false,"defaults":{},"cursor":{"row":9,"col":0,"visible":true,"shape":"block"},"palette_overridden":false,"rows":[{"row":0,"runs":[]}]}"#,
        )
        .expect("parses");
        assert!(
            cur.try_view().is_err(),
            "an out-of-bounds cursor must be rejected"
        );
    }

    // ── A-F1 (adversarial): try_view caps absurd sizes before allocating ────────
    #[test]
    fn try_view_rejects_absurd_grid_size() {
        // Under the cap succeeds; over it is a typed Err, never an allocator abort.
        let ok = FrameEnvelope::from_canonical_json(
            r#"{"schema":1,"size":{"rows":1,"cols":65535},"alt_screen":false,"defaults":{},"cursor":{"row":0,"col":0,"visible":true,"shape":"block"},"palette_overridden":false,"rows":[{"row":0,"runs":[]}]}"#,
        )
        .expect("parses");
        assert!(
            ok.try_view().is_ok(),
            "1×65535 (< 16M cells) is under the cap"
        );

        let mut rows = String::from("[");
        for r in 0..300u16 {
            if r > 0 {
                rows.push(',');
            }
            rows.push_str(&format!(r#"{{"row":{r},"runs":[]}}"#));
        }
        rows.push(']');
        let big = format!(
            r#"{{"schema":1,"size":{{"rows":300,"cols":65535}},"alt_screen":false,"defaults":{{}},"cursor":{{"row":0,"col":0,"visible":true,"shape":"block"}},"palette_overridden":false,"rows":{rows}}}"#
        );
        let env = FrameEnvelope::from_canonical_json(&big).expect("parses");
        assert!(
            matches!(
                env.try_view(),
                Err(crate::capture::CaptureError::NonCanonical { .. })
            ),
            "300×65535 (~19.6M cells) must be rejected before to_cells allocates"
        );
    }

    // ── B-F1 (adversarial): coloured wide-head over-report is intentional ───────
    #[test]
    fn default_bg_change_flips_colored_wide_head() {
        // A coloured wide glyph's spacer is bg=Default, so an OSC-11 bg-default
        // change flips the spacer and wide-pairing propagates to the concrete-bg
        // HEAD. Over-report only (false FAIL, never false PASS); lifted verbatim
        // from the old compute_lens_diff and pinned as intentional (task-079 B-F1).
        let mut vt = VirtualTerminal::new(1, 6);
        vt.process("\x1b[48;5;28m\u{7d42}\x1b[0m".as_bytes()); // coloured 終 at cols 0-1
        let head = vt.grid().visible_row(0).get(0).unwrap().clone();
        let spacer = vt.grid().visible_row(0).get(1).unwrap().clone();
        assert!(
            head.is_wide() && matches!(head.style.bg, Color::Indexed(28)),
            "precondition: concrete-bg wide head"
        );
        assert!(
            spacer.is_wide_continuation() && matches!(spacer.style.bg, Color::Default),
            "precondition: default-bg spacer"
        );
        let grid = vt.grid().clone_visible();
        let cur = cursor_state(&vt);
        let base = TerminalDefaultColors::default();
        let themed = TerminalDefaultColors {
            bg: Some([10, 20, 30]),
            ..base
        };
        let diff = diff_frames(
            &GridFrame::new(&grid, base, cur, false),
            &GridFrame::new(&grid, themed, cur, false),
        );
        assert!(
            diff.changed_mask[0],
            "concrete-bg wide HEAD flips via its default-bg spacer (documented over-report)"
        );
        assert!(diff.changed_mask[1], "the default-bg spacer flips");
    }

    // ── impl-review MINOR: out-of-range cell() yields EMPTY, never panics ───────
    #[test]
    fn out_of_range_cell_is_empty_for_both_views() {
        let mut vt = VirtualTerminal::new(2, 4);
        vt.process(b"hi");
        let grid = vt.grid().clone_visible();
        let gf = GridFrame::new(&grid, vt.default_colors(), cursor_state(&vt), false);
        let fv = capture(&vt).try_view().expect("canonical");
        for (label, view) in [
            ("GridFrame", &gf as &dyn CellGridView),
            ("FrameView", &fv as &dyn CellGridView),
        ] {
            // In range still works.
            assert_eq!(view.cell(0, 0).cell().ch, 'h', "{label}: in-range cell");
            // Row and column past the edge both yield EMPTY without panicking
            // (the trait contract at CellGridView::cell).
            assert_eq!(
                view.cell(99, 0),
                CellRef::new(Cell::EMPTY),
                "{label}: row OOB"
            );
            assert_eq!(
                view.cell(0, 99),
                CellRef::new(Cell::EMPTY),
                "{label}: col OOB"
            );
        }
    }
}
