Repo: `/Users/indrasvat/code/github.com/indrasvat-shux`
Branch: `feat/vt-dec-special-graphics`
Task: `docs/tasks/070-shux-vt-dec-special-graphics.md`

Please review the current worktree diff for task 070. The text diff is saved at:
`.shux/qa/070-dec-special-graphics/implementation-diff.txt`

Key files to inspect:
- `crates/shux-vt/src/charset.rs`
- `crates/shux-vt/src/cursor.rs`
- `crates/shux-vt/src/parser.rs`
- `crates/shux-vt/src/lib.rs`
- `crates/shux/tests/pane_io_integration.rs`
- `.shux/scripts/dec_special_graphics_check.sh`
- `.shux/fixtures/vt-corpus/synthetic/manifest.json`

Implementation summary:
- Persistent `TerminalCharsets` lives on `VirtualTerminal`.
- `VtHandler` translates printable chars only in `print()`.
- SO/SI switch active G0/G1 in `execute()`.
- `ESC ( 0` / `ESC ) 0` designate DEC Special Graphics; `ESC ( B` /
  `ESC ) B` designate ASCII; unsupported G0/G1 designations fall back to ASCII.
- `SavedCursor` now carries a `TerminalCharsets` snapshot, so alternate-screen
  nested save/restore cannot clobber primary-screen charset restore.
- RIS resets active charset state.
- Added unit tests for full mapping, cross-chunk persistence, G1 SO/SI,
  dynamic re-designation, invalid designation fallback, REP, RIS, DECSC/DECRC,
  1049 nested save/restore, and wide-cell non-regression.
- Added pane.capture integration test.
- Added shux visual/pixel automation at 80x24, 120x40, 200x60.
- Renamed the corpus fixture from old `dec-special-graphics-current` to
  post-task `dec-special-graphics` and regenerated DootSabha-approved goldens.

Verification already passed after the latest fix:
- `make test-vt FILTER=dec_special_graphics`
- `SHUX_TEST_BINARY_TIMEOUT_SECONDS=120 make test-pane-io FILTER=dec_special_graphics`
- `make test-vt-dec-special-graphics`
- `make test-vt-corpus`
- `make test-vt-wide-invariants`

Review questions:
1. Any P1/P2 correctness issue in charset persistence, SO/SI, designation,
   DECSC/DECRC, 1049 alternate-screen restore, RIS, or REP?
2. Any missed task DoD/test matrix item before SOLID QA?
3. Any concern with the corpus rename/baseline governance?

Return severity-ranked findings with concrete file/line references. If no
material issue remains, say APPROVE.
