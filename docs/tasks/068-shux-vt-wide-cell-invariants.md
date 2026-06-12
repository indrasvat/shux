# Task 068: shux-vt Wide-Cell Invariants

**Status:** Not Started
**Priority:** High
**Milestone:** VT Quality Track
**Depends On:** 005, 067, 073
**Touches:** `crates/shux-vt/src/cell.rs`, `crates/shux-vt/src/grid.rs`, `crates/shux-vt/src/parser.rs`, `.shux/qa/068-wide-cell-invariants/`

---

## Problem

Wide characters occupy a head cell plus a continuation cell. Terminal editing
operations can corrupt this invariant if they overwrite only one side, delete
one half, insert into the middle, erase only a continuation, or wrap a wide
cell at the edge.

Corruption shows up as ghost cells, bad spacing, broken borders, stale CJK
tails, and misleading `pane.capture` text.

## Scope

Harden all cell-mutating operations around width-2 cells:

- print/overwrite,
- insert mode and ICH,
- DCH,
- ECH/EL/ED,
- wrapping at right edge,
- scroll-region movement,
- resize interactions from task 067.

## Mandatory Process

- Run DootSabha design council before coding.
- Run DootSabha implementation-diff council before marking done.
- Invoke `shux-vt-solid-qa` for an independent hard-gate review.
- Save auditable task artifacts under `.shux/qa/068-wide-cell-invariants/`.

## Testing Matrix

| Layer | Required Evidence |
|---|---|
| Unit | Overwriting a wide head clears its continuation. |
| Unit | Overwriting a wide continuation clears the head and continuation safely. |
| Unit | DCH/ICH/ECH/EL/ED never leave orphan `width == 0` cells. |
| Unit | Wide char at final column wraps or degrades according to documented terminal behavior. |
| Unit | Property-style invariant check: every visible row has no orphan continuation and no wide head missing a continuation. |
| Integration | VT byte fixtures mix CJK, box drawing, ANSI colors, and edit operations. |
| Shux automation | Render a wide-glyph stress grid at 80x24, 120x40, and 200x60 and capture PNGs. |
| Visual | Inspect CJK rows, right borders, selected rows, and mixed-width tables for drift. |
| Pixel | Compare deterministic stress-grid PNGs against committed `.shux/goldens/` or DootSabha-approved `.shux/qa/068-wide-cell-invariants/` expected PNGs with `--max-pixel-diff-ratio 0.0` and `--max-mean-channel-delta 0.0`. Self-minted implementation baselines are not allowed. |
| QA | `shux-vt-solid-qa` returns `VERDICT: PASS` in `.shux/qa/068-wide-cell-invariants/SOLID-QA.md`. |

## Acceptance Criteria

- [ ] A helper exists to validate row/grid wide-cell invariants in tests.
- [ ] All mutation paths either preserve or intentionally clear wide pairs.
- [ ] `pane.capture` skips continuation cells and never emits duplicate wide characters.
- [ ] PNG snapshots show no ghost cells after insert/delete/erase operations.

## Definition of Done

- [ ] DootSabha design council evidence saved under `.shux/qa/068-wide-cell-invariants/`.
- [ ] Implementation-diff DootSabha review saved and clean or addressed.
- [ ] Unit, integration, shux automation, visual, and pixel checks pass.
- [ ] Full-resolution PNGs, pixel metric JSON, and `evidence-manifest.json` are committed under `.shux/qa/068-wide-cell-invariants/`.
- [ ] `shux-vt-solid-qa` hard-gate report is `VERDICT: PASS` saved to `.shux/qa/068-wide-cell-invariants/SOLID-QA.md`.
- [ ] `make check` passes.
- [ ] Progress and learnings are updated.
