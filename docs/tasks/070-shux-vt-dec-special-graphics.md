# Task 070: shux-vt DEC Special Graphics Charset

**Status:** Not Started
**Priority:** Medium
**Milestone:** VT Quality Track
**Depends On:** 005, 068
**Touches:** `crates/shux-vt/src/parser.rs`, `crates/shux-vt/src/cell.rs`, `.shux/out/070-dec-special-graphics/`

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

Out of scope:

- Full ISO-2022 charset support beyond the common DEC graphics set.
- Locale-specific legacy charsets.

## Mandatory Process

- Run DootSabha design council before coding.
- Run DootSabha implementation-diff council before marking done.
- Invoke `shux-vt-solid-qa`.
- Save artifacts under `.shux/out/070-dec-special-graphics/`.

## Testing Matrix

| Layer | Required Evidence |
|---|---|
| Unit | DEC graphics map produces expected Unicode line drawing for `lqkxmj...` fixtures. |
| Unit | SO/SI switches G0/G1 correctly and resets safely. |
| Unit | ASCII mode remains unchanged after `ESC ( B` / `ESC ) B`. |
| Integration | A curses-style box fixture renders borders correctly in `pane.capture`. |
| Shux automation | Render DEC graphics stress screen and snapshot at 80x24, 120x40, and 200x60. |
| Visual | Inspect corners, horizontal/vertical joins, mixed text, and color boundaries. |
| Pixel | Deterministic DEC graphics PNG exactly matches baseline. |
| QA | `shux-vt-solid-qa` returns `VERDICT: PASS`. |

## Acceptance Criteria

- [ ] Classic DEC graphics boxes render as box drawing, not letters.
- [ ] Charset shifts do not leak into subsequent ASCII text.
- [ ] Existing Unicode box-drawing paths are not regressed.

## Definition of Done

- [ ] DootSabha design and implementation-diff reviews are saved.
- [ ] Unit, integration, shux automation, visual, and pixel checks pass.
- [ ] `shux-vt-solid-qa` hard-gate report is `VERDICT: PASS`.
- [ ] `make check` passes.
- [ ] Progress and learnings are updated.
