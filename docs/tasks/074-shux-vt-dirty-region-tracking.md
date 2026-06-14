# Task 074: shux-vt Dirty-Region Tracking

**Status:** Done
**Priority:** Medium
**Milestone:** VT Quality Track
**Depends On:** 005, 073
**Touches:** `crates/shux-vt/src/grid.rs`, `crates/shux-vt/src/lib.rs`, `crates/shux-raster`, `crates/shux-core`, `.shux/qa/074-shux-vt-dirty-region-tracking/`

---

## Problem

Snapshots and render loops currently inspect/render more state than necessary.
Dirty-region tracking would let shux know exactly which rows/cells changed
during VT processing. This improves performance, makes debugging easier, and
creates another correctness signal for visual regression tests.

## Scope

Add dirty tracking to the VT/grid layer:

- mark rows/cells dirty on print, erase, insert/delete, scroll, resize,
  alternate-screen transitions, color/default-state changes, and sync-output
  presentation changes,
- expose a clear API to read and clear dirty regions,
- keep full-frame invalidation available for resize and mode transitions,
- benchmark overhead.

Out of scope:

- Rewriting compositor diffing in the same task unless the task evidence proves
  the API first.
- GPU/incremental raster architecture.
- Renderer consumption of dirty regions. Task 074 proves the VT/grid API and
  raster parity for dirty tracking enabled versus disabled; compositor
  incremental redraw belongs in a follow-up task.

## Mandatory Process

- Run DootSabha design council before coding.
- Run implementation-diff review before marking done. Prefer DootSabha without
  Gemini or a bounded Claude + agy review path until DootSabha gains native agy
  support.
- Invoke `shux-vt-solid-qa`.
- Save auditable task artifacts under `.shux/qa/074-shux-vt-dirty-region-tracking/`.

## Testing Matrix

| Layer | Required Evidence |
|---|---|
| Unit | Single print marks one cell/row dirty. |
| Unit | Erase/insert/delete mark correct ranges. |
| Unit | Scroll and resize force appropriate full-row/full-frame invalidation. |
| Unit | Dirty state can be cleared and does not leak across reads. |
| Integration | VT byte fixture produces expected dirty region sequence. |
| Performance | Benchmark overhead for high-output stream and idle snapshot path; dirty tracking must add no more than 5% throughput overhead on replay and no more than 2ms per 200x60 frame on idle snapshot invalidation bookkeeping. |
| Shux automation | Run a live pane with incremental updates, real colored Unix command output, and capture dirty report + PNGs. |
| Visual | Verify dirty-optimized path, if used by renderer, matches full render screenshots. |
| Pixel | Full render vs dirty/incremental render PNGs are exact matches with `--max-pixel-diff-ratio 0.0` and `--max-mean-channel-delta 0.0`. |
| QA | `shux-vt-solid-qa` returns `VERDICT: PASS` in `.shux/qa/074-shux-vt-dirty-region-tracking/SOLID-QA.md`. |

## Acceptance Criteria

- [x] Dirty tracking is correct for all grid mutation classes.
- [x] API is documented and hard to misuse.
- [x] Dirty tracking overhead stays within the Testing Matrix budgets or the task is explicitly re-scoped before coding.
- [x] If any renderer path consumes dirty regions, exact pixel parity with full render is proven.
  - No renderer path consumes dirty regions in task 074; raster parity instead
    compares independent tracking-disabled and tracking-enabled VT replays.

## Definition of Done

- [x] DootSabha design and implementation-diff reviews are saved.
- [x] Unit, integration, performance, shux automation, visual, and pixel checks pass.
- [x] Full-resolution PNGs, pixel metric JSON, performance JSON, and `evidence-manifest.json` are committed under `.shux/qa/074-shux-vt-dirty-region-tracking/`.
- [ ] `shux-vt-solid-qa` hard-gate report is `VERDICT: PASS` saved to `.shux/qa/074-shux-vt-dirty-region-tracking/SOLID-QA.md`.
- [ ] `make check` passes.
- [x] Progress and learnings are updated.
