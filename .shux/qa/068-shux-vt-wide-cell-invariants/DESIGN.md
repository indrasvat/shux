# Task 068 Design Brief - shux-vt Wide-Cell Invariants

## Goal

Make width-2 terminal cells impossible to corrupt through normal VT mutation
paths. A valid row must never contain:

- a continuation cell (`width == 0`) at column 0,
- a continuation whose left neighbor is not a width-2 head,
- a width-2 head without a continuation cell to its right,
- a stale continuation left behind after overwriting, erasing, inserting,
  deleting, scrolling, or resizing.

## Current Code Shape

- `Cell::wide_continuation()` represents the second column of a wide glyph as
  `ch = ' '` and `width = 0`.
- `VtHandler::write_char()` writes a head and then a continuation, but does not
  clear an old pair when writing over either half.
- `Grid::erase_chars()`, `insert_chars()`, and `delete_chars()` mutate raw row
  cells without repairing adjacent wide pairs.
- `Grid::scroll_up()` / `scroll_down()` move rows wholesale, which should keep
  intra-row invariants but must be covered by tests.
- Task 067 resize reflow already filters continuations and recreates wide
  continuations when wrapping logical lines; task 068 should add stronger
  invariant tests around that behavior.

## Proposed Design

1. Add row/grid invariant helpers for tests:
   - `assert_row_wide_invariants(row)`
   - `assert_grid_wide_invariants(grid)`
   These should produce row/column-specific failure messages.

2. Add small mutation primitives on `Row`:
   - `clear_wide_pair_around(col, bg)` clears the head+tail pair if `col`
     points at either side of a wide glyph.
   - `sanitize_wide_pairs(bg)` repairs an entire row after bulk shifts by
     clearing orphan continuations and heads missing tails.
   - Optional: `put_cell(col, cell, bg)` to centralize overwrite repair.

3. Update write behavior:
   - Before writing any char at the cursor, clear the wide pair around the
     cursor.
   - Before writing a width-2 char, also clear the wide pair around `col + 1`,
     because the new continuation may overwrite the head of the next old wide
     pair.
   - If writing width-1 over a wide head, clear the old tail.
   - If writing over a continuation, clear the old head and tail first.
   - If writing width-2 in the last column:
     - auto-wrap on: blank the trailing cell, mark the row wrapped, advance to
       the next line or scroll, then write the wide pair at column 0;
     - auto-wrap off: write a single attributed space at the final column and
       keep the cursor there. This is a deliberate shux choice, not a claim of
       uniform terminal-standard behavior.
   - Keep the repair in `write_char()` so REP and insert mode inherit it.

4. Update edit/erase behavior:
   - `erase_chars()` expands the affected range to include any intersecting
     wide pair, including the head just left of the start boundary when the
     erase starts on a continuation.
   - `insert_chars()` clears any pair intersecting the insertion point before
     the exact shift, performs the shift by the requested count, clears
     inserted cells, then sanitizes the row.
   - `delete_chars()` shifts left by exactly the requested count, blanks the
     right edge, then sanitizes the row. It must not expand the deleted range,
     because that changes terminal column geometry.
   - `EL`/`ED` coverage comes through `erase_chars()` and row resets.

5. Update resize behavior:
   - `resize_reflowing_columns()` already filters continuations and recreates
     wide tails; add invariant coverage around shrink/grow.
   - `resize_canvas()` must sanitize every row after any column resize because
     alternate-screen/fixed-canvas shrink can truncate one side of a wide pair.

6. Keep capture behavior:
   - `VirtualTerminal::capture_text()` must continue skipping continuation
     cells so wide characters appear once.
   - Copy/selection extraction must handle boundaries that start or end on a
     continuation cell without duplicating or dropping unrelated blanks.

7. Visual and pixel evidence:
   - Add a deterministic shux automation script for a wide-glyph stress grid at
     80x24, 120x40, and 200x60.
   - Capture text and full-resolution PNGs under `.shux/qa/068-shux-vt-wide-cell-invariants/`.
   - Pixel baseline resolution: this task will generate new wide-glyph stress
     expected PNGs as explicit task deliverables, record DootSabha design
     approval in `dootsabha-design.json`, and compare actual PNGs against those
     tracked expected PNGs with `.claude/automations/pixel_verify.py` at exact
     thresholds. The implementation cannot mint hidden baselines after the fact.
   - Include a colored CJK-on-background case. The rasterizer already skips
     continuation cells and paints a width-2 head across two cells, but this
     evidence protects against visual gaps.

## Test Plan

- Unit tests for overwrite head, overwrite continuation, wide-over-wide
  overwrite at adjacent columns, width-1 over width-2, ICH, DCH, ECH, EL, ED,
  REP, cursor on continuation, wrap at final column with auto-wrap on/off,
  resize reflow, resize canvas, and scroll-region row movement.
- A sequence-level `proptest` fuzzer over print, CUP, ICH, DCH, ECH, EL/ED,
  resize, and scroll-like operations, asserting whole-grid invariants after
  every step.
- Integration byte fixtures with CJK + ANSI colors + edit operations.
- Existing `make test-vt` and `make test-vt-corpus`.
- New focused Make target for wide-cell stress automation.
- `make check`, `make check-vt-qa`, implementation DootSabha review, and
  SOLID QA hard gate before PR.

## Council Decisions Incorporated

- Shift operations preserve exact terminal geometry; repair happens after the
  shift. Pure erase operations may expand to clear the other half of a glyph.
- `sanitize_wide_pairs()` uses these canonical rules:
  - continuation at column 0 becomes a normal blank;
  - continuation whose left neighbor is not a width-2 head becomes a blank;
  - width-2 head at the final column becomes a blank;
  - width-2 head whose right neighbor is not a continuation becomes a blank.
- The sanitizer must re-evaluate against mutated cells left-to-right so
  head-to-blank conversion also clears the former tail.
- Invariant helpers must inspect `scrollback + visible` rows, not visible rows
  only, and must assert continuation cells have `ch == ' '`.

## Council Questions

1. Is the repair model too aggressive, especially for delete/insert expanding
   ranges around wide cells?
2. What terminal behavior should shux choose for a width-2 character printed
   at the final column?
3. Are there mutation paths missing from this design?
4. What specific tests would catch false confidence or visual-only gaps?
