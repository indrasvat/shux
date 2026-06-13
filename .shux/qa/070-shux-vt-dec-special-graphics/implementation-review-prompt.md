Review the implementation diff for shux task 070: DEC Special Graphics charset.

Task/design:
- Task file: `docs/tasks/070-shux-vt-dec-special-graphics.md`
- Design: `.shux/qa/070-dec-special-graphics/DESIGN.md`
- Design council: `.shux/qa/070-dec-special-graphics/dootsabha-design.json`
- Implementation diff: `.shux/qa/070-dec-special-graphics/implementation-diff.txt`

What changed:
- Added persistent `TerminalCharsets` state to `VirtualTerminal`, borrowed into
  `VtHandler` so charset designation survives separate PTY chunks.
- Added G0/G1 designation for `ESC ( 0`, `ESC ) 0`, `ESC ( B`, `ESC ) B`.
- Added SO/SI active-slot switching in `execute()`.
- Translates printable characters in `print()` only; REP reuses already
  translated cells.
- RIS resets charset state.
- Cursor save/restore helper saves and restores charset state too.
- Added focused VT unit tests for G0/G1 mapping, full map, cross-chunk
  persistence, SO/SI, dynamic re-designation, invalid designation fallback,
  REP, RIS reset, DECSC/DECRC restore, and wide-cell non-regression.
- Added pane I/O integration proving real `pane.capture` sees Unicode box
  drawing from DEC bytes.
- Renamed the synthetic DEC corpus fixture from old `*-current` behavior to
  post-070 `dec-special-graphics`, promoted DootSabha-approved expected
  goldens, and updated corpus QA evidence.
- Added `make test-vt-dec-special-graphics` and shux automation with exact PNG
  comparison at 80x24, 120x40, and 200x60.

Verification already run:
- `make test-vt FILTER=dec_special_graphics`
- `SHUX_TEST_BINARY_TIMEOUT_SECONDS=120 make test-pane-io FILTER=dec_special_graphics`
- `make test-vt-dec-special-graphics` after an approved promotion pass
- visual inspection of the three DEC PNGs
- `make test-vt-corpus`
- `make test-vt-wide-invariants`

Please review for:
1. Any correctness bug in charset state persistence, G0/G1 designation,
   SO/SI switching, unknown-designation fallback, RIS reset, or DECSC/DECRC.
2. Any risk that translation in `print()` only misses valid DEC bytes or
   corrupts Unicode/grapheme/wide-cell behavior.
3. Any missed render path or test evidence from the task Testing Matrix / DoD.
4. Any concern with the corpus fixture rename and DootSabha-approved baseline
   promotion.
5. Any P1/P2 issue that must be fixed before PR.

Return severity-ranked findings with concrete file/line references if possible.
If no material issue remains, say APPROVE.
