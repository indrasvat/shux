# Task 069: shux-vt Grapheme-Aware Cell Storage

**Status:** Done
**Priority:** Medium/High
**Milestone:** VT Quality Track
**Depends On:** 005, 068, 073
**Touches:** `crates/shux-vt/src/cell.rs`, `crates/shux-vt/src/parser.rs`, `crates/shux-vt/src/lib.rs`, `crates/shux-raster/src/lib.rs`, `.shux/qa/069-shux-vt-grapheme-cell-storage/`

---

## Problem

`Cell` stores one `char`. That loses combining marks, variation selectors,
ZWJ sequences, skin tones, and regional-indicator flag pairs before the
rasterizer can make a good decision. The libghostty spike confirmed that this
is a real adapter/model gap.

The goal is not full shaping or color emoji. The goal is to stop destroying
multi-codepoint terminal cell content inside `shux-vt`.

## Scope

Add an optional grapheme payload to `Cell` while preserving the fast ASCII path.

Required shape:

- `Cell` keeps simple scalar `ch` for common cells.
- Extended storage can carry a display string/grapheme for rare complex cells.
- `capture_text()` emits the full grapheme string.
- Rasterizer consumes the grapheme payload where it can, and degrades
  intentionally where font/shaping support is still absent.
- Memory impact is measured on large scrollback.

Out of scope:

- Full HarfBuzz/shaping integration.
- Color emoji rendering.
- Bidi/RTL layout.

## Mandatory Process

- Run DootSabha design council before coding, with explicit memory/performance critique.
- Run DootSabha implementation-diff council before marking done.
- Invoke `shux-vt-solid-qa`.
- Save auditable task artifacts under `.shux/qa/069-shux-vt-grapheme-cell-storage/`.

## Testing Matrix

| Layer | Required Evidence |
|---|---|
| Unit | Combining mark sequence `e + U+0301` is stored and captured as one display string. |
| Unit | VS16, skin-tone modifier, ZWJ emoji, and flag-pair fixtures are preserved in cell data even if raster output degrades. |
| Unit | ASCII memory/common path remains compact and unchanged in behavior. |
| Unit | Wide-cell invariant helper from task 068 still passes with grapheme cells. |
| Integration | `pane.capture` returns full grapheme content for deterministic fixtures. |
| Raster | PNG rendering remains stable for existing simple glyphs and documents unsupported composed rendering. |
| Performance | Memory and extraction cost measured against pre-change baseline for 80x24 with 5K scrollback; RSS must increase by no more than 15% and ASCII `capture_text()` throughput must slow by no more than 10% unless the task is explicitly re-scoped before implementation. |
| Shux automation | Capture a Unicode stress pane across 80x24, 120x40, and 200x60. |
| Visual | Inspect combining marks, emoji fallback, CJK adjacency, and tofu behavior. |
| Pixel | Existing non-grapheme golden/stress PNGs remain exact with `--max-pixel-diff-ratio 0.0`; grapheme expected PNGs must be committed and DootSabha-approved before implementation output is compared. |
| QA | `shux-vt-solid-qa` returns `VERDICT: PASS` in `.shux/qa/069-shux-vt-grapheme-cell-storage/SOLID-QA.md`. |

## Acceptance Criteria

- [x] Complex cell content is not irreversibly lost by `shux-vt`.
- [x] Existing ASCII/simple Unicode behavior remains compatible.
- [x] Capture and snapshot paths both account for grapheme payloads.
- [x] Remaining renderer limitations are documented in task evidence.
- [x] Memory and extraction overhead stay within the Testing Matrix budgets or the task is explicitly re-scoped before coding.

## Definition of Done

- [x] DootSabha design and implementation-diff reviews are saved.
- [x] Unit, integration, performance, shux automation, visual, and pixel checks pass.
- [x] Full-resolution PNGs, pixel metric JSON, performance JSON, and `evidence-manifest.json` are committed under `.shux/qa/069-shux-vt-grapheme-cell-storage/`.
- [x] `shux-vt-solid-qa` hard-gate report is `VERDICT: PASS` saved to `.shux/qa/069-shux-vt-grapheme-cell-storage/SOLID-QA.md`.
- [x] `make check` passes.
- [x] Progress and learnings are updated.
