# Task 070: shux-vt DEC Special Graphics Charset

**Status:** Done
**Priority:** Medium
**Milestone:** VT Quality Track
**Depends On:** 005, 068, 073
**Touches:** `crates/shux-vt/src/parser.rs`, `crates/shux-vt/src/cell.rs`, `.shux/qa/070-shux-vt-dec-special-graphics/`

---

## Problem

Some ncurses/classic terminal apps use DEC special graphics via charset
designation and SO/SI shifts instead of direct Unicode box drawing. The parser
currently ignores SO/SI character-set shifts, so those apps can render line
drawing as ordinary letters.

## Scope

Implement the common VT100 DEC special graphics path:

- `ESC ( 0` / `ESC ) 0` designation for G0/G1,
- `ESC ( B` / `ESC ) B` return to ASCII,
- SO/SI shift between G0/G1,
- mapping of DEC graphics codepoints to Unicode box drawing and symbols.
- DECSC/DECRC save and restore charset state alongside cursor state.

Out of scope:

- Full ISO-2022 charset support beyond the common DEC graphics set.
- G2/G3 designation (`ESC *`, `ESC +`), LS2/LS3, SS2/SS3 (`ESC N`, `ESC O`).
- GR / 96-charset / 8-bit designation (`ESC -`, `ESC .`, `ESC /`).
- National replacement, UK, and DEC technical charsets; unsupported G0/G1
  designations fall back to ASCII rendering.
- DECSTR soft reset handling; when DECSTR is implemented, it should reset
  charset state too.
- Locale-specific legacy charsets.

## Mandatory Process

- Run DootSabha design council before coding.
- Run DootSabha implementation-diff council before marking done.
- Invoke `shux-vt-solid-qa`.
- Save auditable task artifacts under `.shux/qa/070-shux-vt-dec-special-graphics/`.

## Testing Matrix

| Layer | Required Evidence |
|---|---|
| Unit | DEC graphics map produces expected Unicode line drawing for `lqkxmj...` fixtures. |
| Unit | Mapping includes the standard `_`, `` ` ``, `a`-`i`, `j`-`x`, `y`-`~` DEC graphics glyphs, including control-picture symbols. |
| Unit | SO/SI switches G0/G1 correctly, persists across separate `process()` chunks, and resets safely on RIS. |
| Unit | ASCII mode remains unchanged after `ESC ( B` / `ESC ) B`. |
| Unit | Dynamic re-designation while a charset is active takes effect immediately; invalid G0/G1 designations fall back to ASCII safely. |
| Unit | REP repeats already translated glyphs without double translation; wide Unicode emitted while DEC graphics is active remains width-correct. |
| Integration | A curses-style box fixture renders borders correctly in `pane.capture`. |
| Shux automation | Render DEC graphics stress screen and snapshot at 80x24, 120x40, and 200x60. |
| Visual | Inspect corners, horizontal/vertical joins, mixed text, and color boundaries. |
| Pixel | Deterministic DEC graphics PNG exactly matches committed `.shux/goldens/070-dec-special-graphics/` baselines with `--max-pixel-diff-ratio 0.0` and `--max-mean-channel-delta 0.0`. The baseline glyph content is approved by `.shux/qa/070-shux-vt-dec-special-graphics/dootsabha-design.json`; generated expected PNG files must be committed before SOLID QA treats them as evidence. |
| QA | `shux-vt-solid-qa` returns `VERDICT: PASS` in `.shux/qa/070-shux-vt-dec-special-graphics/SOLID-QA.md`. |

## Acceptance Criteria

- [x] Classic DEC graphics boxes render as box drawing, not letters.
- [x] Charset shifts do not leak into subsequent ASCII text.
- [x] Existing Unicode box-drawing paths are not regressed.

## Definition of Done

- [x] DootSabha design and implementation-diff reviews are saved.
- [x] Unit, integration, shux automation, visual, and pixel checks pass.
- [x] Full-resolution PNGs, pixel metric JSON, and `evidence-manifest.json` are committed under `.shux/qa/070-shux-vt-dec-special-graphics/`.
- [x] `shux-vt-solid-qa` hard-gate report is `VERDICT: PASS` saved to `.shux/qa/070-shux-vt-dec-special-graphics/SOLID-QA.md`.
- [x] `make check` passes.
- [x] Progress and learnings are updated.
