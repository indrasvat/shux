# Task 079: lens gate — one comparator in `shux-vt` (`CellGridView`)

**Status:** Not Started
**Priority:** High
**Milestone:** M3
**Depends On:** 078
**Quality Gate:** shux-vt-solid-qa
**Touches:** `crates/shux-vt/src/` (comparator + trait + result types), `crates/shux/src/main.rs` (diff adapter), `crates/shux/tests/lens_gate_*`, `.shux/fixtures/lens-gate/`

> `shux lens gate` initiative. Replaces council #1's rejected "rehydrate a `Grid`
> and reuse `compute_lens_diff`" plan with one comparator over a view trait.

## Problem

The gate must diff a live captured frame against a committed golden. `compute_lens_diff`
(currently in the 446 KB `crates/shux/src/main.rs`) is exactly the right semantics —
cell-exact, OSC-default-resolved, wide-glyph-paired — but it takes concrete
`shux_vt::Grid` values. Rehydrating a `Grid` from a golden fabricates VT invariants
(council #1 BLOCKER). We need **one** diff definition usable over both a live `Grid`
and a deserialized `CapturedFrame`, with **no fake Grid**.

## Scope

1. **`CellGridView` trait in `shux-vt`**: `rows/cols`, `cell(r,c) -> CellRef`,
   `defaults() -> TerminalDefaultColors`, `palette() -> Option<&Palette>`,
   `cursor() -> CursorState`. **`CellRef` is by-value / owned** (council #3 — no
   "materialized cache" alternative); it never borrows an RLE-decode temporary. If a
   by-value `cell()` proves impossible without changing the trait, that is a design
   escalation, not a silent fallback.
2. **Lift `compute_lens_diff` → `diff_frames(a: &dyn CellGridView, b: &dyn CellGridView) -> FrameDiff`**
   into `shux-vt`, preserving semantics exactly: `Color::Default` resolved against
   each side's OSC 10/11/12 defaults, OSC 4 palette per task 078's decision,
   wide-glyph head+spacer pairing, region merge + 256-span cap + bounding box,
   `cursor_moved`. **Move `FrameDiff`/`LensRowSpan` result types into `shux-vt`**
   (council #2) so the daemon does not own the diff shape.
3. **`impl CellGridView for &Grid`** (live) and **`for &CapturedFrame`** (golden).
4. **Adapt the daemon**: `pane.diff_since` becomes a thin adapter calling
   `diff_frames(&checkpoint_grid, &cur_grid)`. Its observable output is byte-identical
   to today (existing lens diff tests + goldens must stay green — this is a pure refactor
   on the daemon side).
5. **Divergence fixtures** (council #1 MAJOR — replaces cell/pixel parity): committed
   cases proving the tiers' boundaries — cell-pass/pixel-fail, pixel-only-fail,
   cursor-only-change, palette-only-change (OSC 4), emoji/font-fallback change,
   blink-only. Each asserts the `cell` verdict *and* documents whether `pixel` would
   diverge.
6. **Pre-refactor parity corpus** (council #3 — parity must not be self-referential):
   BEFORE moving `compute_lens_diff`, capture a frozen corpus of its outputs on the
   existing 077 lens goldens/fixtures (`.shux/fixtures/lens-gate/parity/`), committed
   under the freeze guard. Post-refactor `diff_frames` is asserted bit-for-bit against
   that captured corpus — not merely against the live (already-moved) function.

## Non-Goals

- No golden-file I/O, no tolerance tiers beyond the cell comparator (task 080).
- No capture emission, runner, or verdict rollup.
- No behavioral change to `pane.diff_since` output.

## Design Review Decisions

DootSabha design review MUST confirm: the trait shape (by-value `CellRef`), that no
dependency cycle is introduced (`shux` depends on `shux-vt`, not the reverse —
verified in council #2), and the divergence-fixture set is complete for the tiers.

## Testing Matrix

| Layer | Required Evidence |
|---|---|
| L1 parity | `diff_frames` equals the **pre-move frozen output corpus** (`.shux/fixtures/lens-gate/parity/`) captured before extraction — bit-for-bit; not self-referential against the live moved fn. |
| L1 ownership | `CellRef` is owned/by-value (a `cell()` result outlives any decode buffer). |
| L1 view | `CellGridView` over `CapturedFrame` yields identical cells/defaults/palette/cursor to the source grid it was captured from. |
| L1 defaults | Theme-mismatch case: same cells, different OSC 11 default bg → `diff_frames` reports a change (the false-pass council BLOCKER cannot recur). |
| L1 divergence | Each divergence fixture asserts the correct `cell` result; cursor-only/palette-only/blink-only are caught by `cell`. |
| L1 ownership | `CellRef` does not borrow a decoded temporary (compile + miri/ownership test where applicable). |
| L2 no-regress | Full existing lens suite (`make test-lens`) stays green after the daemon adapter refactor. |
| L1 wide | Wide-glyph + grapheme cells pair correctly across both views. |

## Acceptance Criteria

- [ ] `CellGridView` + `diff_frames` live in `shux-vt`; `FrameDiff`/`LensRowSpan` moved there.
- [ ] Both `&Grid` and `&CapturedFrame` implement the trait; `CellRef` is ownership-safe.
- [ ] `pane.diff_since` is a thin adapter with byte-identical output to today.
- [ ] Theme/default-color mismatch is detected by the comparator.
- [ ] Divergence fixtures committed and asserted.
- [ ] No new crate introduced; no dependency cycle.

## Definition of Done

- [ ] DootSabha design review incorporated before coding.
- [ ] Red tests captured before implementation.
- [ ] L1/L2 tests pass; `make test-lens` green (no regression).
- [ ] `make check` passes.
- [ ] `shux-vt-solid-qa` gate `VERDICT: PASS`; evidence under `.shux/qa/079-*/`.
- [ ] Implementation-diff DootSabha convergence review clean or addressed.
- [ ] `docs/PROGRESS.md` + this task updated; learnings appended.
