# Task 072: shux-vt Origin Mode and Scroll-Region Semantics

**Status:** Not Started
**Priority:** Medium
**Milestone:** VT Quality Track
**Depends On:** 005, 029, 073
**Touches:** `crates/shux-vt/src/parser.rs`, `crates/shux-vt/src/lib.rs`, `.shux/qa/072-origin-mode-scroll-region/`

---

## Problem

DECOM origin mode changes cursor addressing to be relative to the active scroll
region. Subtle mistakes here cause TUIs to draw into the wrong rows, especially
inside split panes, alternate screen, and scroll-margin layouts.

## Scope

Audit and correct origin-mode behavior for:

- CUP/HVP/VPA and related absolute movement,
- scroll-region set/reset,
- cursor save/restore carrying origin-mode state,
- DECRQM mode reports,
- alternate screen transitions,
- synchronized-output presentation snapshots.

## Mandatory Process

- Run DootSabha design council before coding.
- Run DootSabha implementation-diff council before marking done.
- Invoke `shux-vt-solid-qa`.
- Save auditable task artifacts under `.shux/qa/072-origin-mode-scroll-region/`.

## Testing Matrix

| Layer | Required Evidence |
|---|---|
| Unit | CUP in origin mode addresses relative to scroll-region top. |
| Unit | CUP outside origin mode remains absolute. |
| Unit | Save/restore restores origin-mode state and cursor position. |
| Unit | Scroll-region reset clamps cursor consistently. |
| Integration | VT fixture draws a fixed header/footer and scrollable body without row bleed. |
| Integration | `process_with_responses()` reports DECRQM origin-mode state correctly. |
| Shux automation | Render scroll-margin fixture and resize it across 80x24/120x40/200x60. |
| Visual | Inspect fixed headers/footers, scroll body, cursor, and alternate-screen transitions. |
| Pixel | Deterministic scroll-region PNGs exactly match committed `.shux/goldens/` or DootSabha-approved `.shux/qa/072-origin-mode-scroll-region/` baselines with `--max-pixel-diff-ratio 0.0` and `--max-mean-channel-delta 0.0`. |
| QA | `shux-vt-solid-qa` returns `VERDICT: PASS` in `.shux/qa/072-origin-mode-scroll-region/SOLID-QA.md`. |

## Acceptance Criteria

- [ ] Origin-mode cursor movement matches xterm common behavior.
- [ ] Scroll margins isolate scrolling to the intended body.
- [ ] No header/footer row bleed appears in capture or PNG.
- [ ] Terminal response behavior remains correct.

## Definition of Done

- [ ] DootSabha design and implementation-diff reviews are saved.
- [ ] Unit, integration, shux automation, visual, and pixel checks pass.
- [ ] Full-resolution PNGs, pixel metric JSON, and `evidence-manifest.json` are committed under `.shux/qa/072-origin-mode-scroll-region/`.
- [ ] `shux-vt-solid-qa` hard-gate report is `VERDICT: PASS` saved to `.shux/qa/072-origin-mode-scroll-region/SOLID-QA.md`.
- [ ] `make check` passes.
- [ ] Progress and learnings are updated.
