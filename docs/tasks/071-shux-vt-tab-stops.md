# Task 071: shux-vt Real Tab-Stop State

**Status:** Done
**Priority:** Medium
**Milestone:** VT Quality Track
**Depends On:** 005, 073
**Touches:** `crates/shux-vt/src/parser.rs`, `crates/shux-vt/src/lib.rs`, `.shux/qa/071-shux-vt-tab-stops/`

---

## Problem

`shux-vt` currently assumes fixed 8-column tab stops and ignores mutable tab
stop state. Real terminals support setting and clearing tab stops with HTS/TBC.
Some applications depend on this for table alignment.

## Scope

Implement tab-stop state:

- default tab stops every 8 columns,
- HTS (`ESC H`) sets a tab stop at the current column,
- TBC (`CSI 0 g`) clears the current tab stop,
- TBC (`CSI 3 g`) clears all tab stops,
- resize preserves valid tab stops and restores defaults only when appropriate.

## Mandatory Process

- Run DootSabha design council before coding.
- Run DootSabha implementation-diff council before marking done.
- Invoke `shux-vt-solid-qa`.
- Save auditable task artifacts under `.shux/qa/071-shux-vt-tab-stops/`.

## Testing Matrix

| Layer | Required Evidence |
|---|---|
| Unit | Default tabs move to 8-column boundaries. |
| Unit | HTS creates a custom stop and HT lands there. |
| Unit | TBC clears current/all stops. |
| Unit | Resize clamps/removes stops beyond width without corrupting remaining stops. |
| Integration | Table fixture using custom tabs aligns in `capture_text()`. |
| Shux automation | Render tab-alignment fixture at 80x24, 120x40, and after resize. |
| Visual | Inspect columns for drift and verify no wrap artifacts. |
| Pixel | Tab fixture PNG exactly matches committed `.shux/goldens/071-tab-stops/` baselines with `--max-pixel-diff-ratio 0.0` and `--max-mean-channel-delta 0.0`. The baseline glyph content is approved by `.shux/qa/071-shux-vt-tab-stops/dootsabha-design.json`; generated expected PNG files must be committed before SOLID QA treats them as evidence. |
| QA | `shux-vt-solid-qa` returns `VERDICT: PASS` in `.shux/qa/071-shux-vt-tab-stops/SOLID-QA.md`. |

## Acceptance Criteria

- [x] Mutable tab stops behave like xterm for common HTS/TBC cases.
- [x] Default behavior remains unchanged when no custom stops are set.
- [x] Capture and snapshot agree on aligned columns.

## Definition of Done

- [x] DootSabha design and implementation-diff reviews are saved.
- [x] Unit, integration, shux automation, visual, and pixel checks pass.
- [x] Full-resolution PNGs, pixel metric JSON, and `evidence-manifest.json` are committed under `.shux/qa/071-shux-vt-tab-stops/`.
- [x] `shux-vt-solid-qa` hard-gate report is `VERDICT: PASS` saved to `.shux/qa/071-shux-vt-tab-stops/SOLID-QA.md`.
- [x] `make check` passes.
- [x] Progress and learnings are updated.
