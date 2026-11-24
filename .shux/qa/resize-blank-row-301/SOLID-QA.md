VERDICT: PASS

commit_under_audit: 7758370 (code at 2e153c4; 7758370 refreshes neutrality only)
merge-base: 974f3b2e4837dab61486b28b6f1fb6415937867e
scope: resize-blank-row-301 (issue #301, PR #302, branch fix/301-resize-drops-blank-row), tier full
audit type: delta re-audit of 02572d5..7758370, on top of the full audit of 974f3b2..efb3dc8 (kept below)

## Delta re-audit: 02572d5..7758370

2e153c4 fixes a bot P1. An erase that a wide glyph widens to cover the whole first visible
row now cuts history's link. EL/ED and ECH both go through `after_erase`, which judges the
range that was actually cleared. Before, they judged the range that was asked for. 7758370
refreshes neutrality: head 2e153c4, status pass (cited). `make check-vt-qa-pregate` at
HEAD passes: "3 VT path(s) touched; evidence conforming in resize-blank-row-301".

Real binary: fix = 2e153c4 release build. Each case starts at 2x5, is widened to 10x2,
then grown to 10x4 so history shows.

| Input | fix 10x4 | v0.59.1 10x4 |
|---|---|---|
| bot: `ABCDE界\r\n ESC[1;2H ESC[K ESC[H XYZ` (EL 0 on the wide tail) | `ABCDE` / `XYZ` | `ABCDEXYZ` |
| EL 1 on a wide head at the row end: `ABCDEabc界\r\n ESC[1;4H ESC[1K ESC[H XYZ` | `ABCDE` / `XYZ` | `ABCDEXYZ` |
| ECH from the wide tail to the edge: `ABCDE界\r\n ESC[1;2H ESC[4X ESC[H XYZ` | `ABCDE` / `XYZ` | `ABCDEXYZ` |
| control, a partial ECH that widens over the glyph only: `ABCDE界ZZ\r\n ESC[1;2H ESC[1X ESC[H XY` | `ABCDEXYZZ` (link kept) | `ABCDEXYZZ` |

- **Issue repro, re-run** (80x24 `bash --norc`, shrink to 40x10, grow to 120x40): all
  three fills survive. The frames are pixel-identical to the 81b3c8d audit frames
  (`pixel-302-prev-repro-*.json`, 3/3 at 0/0).
- **CJK-heavy coloured `cat`** (Japanese, Chinese, Korean; truecolor fg, indexed bg, basic
  fg) across 80x24, 37x24, 120x24 and back to 80x24:
  - Text captures are identical to v0.59.1 at every size.
  - Pixels are identical too (`pixel-302-v0591-cjk-*.json`, 4/4 at 0/0).
  - Wide glyphs wrap at the boundary and rejoin at 120 columns.
  - The raster draws CJK as tofu on both builds. That is the existing font-chain limit, so
    CJK is judged from the text captures.
- **vim** across 5 sizes against base 974f3b2: 5/5 at 0/0 (`pixel-302-base-vim-*.json`).
- **Readline across a wrap, re-run:** 6/6 frames at 0/0 against v0.59.1
  (`pixel-302-v0591-rl-*.json`). The issue repro's pre-resize frames also match v0.59.1,
  2/2 at 0/0.
- **Metrics:** the full audit's metric files were produced by the 81b3c8d build, so they are
  replaced by the `pixel-302-*` set (20 files) produced by 2e153c4.
- **Cleanup:** runtime dirs `/tmp/qn302`, `/tmp/qo0591b`, `/tmp/qn302v` and `/tmp/qb301v`
  have no daemon running and no leaked processes.

Findings for the delta: none.

## Full audit: 974f3b2..efb3dc8

audit commit: efb3dc8 (code at 81b3c8d)
binaries: fix = 81b3c8d release build; v0.59.1 = /tmp/smoke-0591 (git_sha 6bc9d94);
base = 974f3b2 (vt-neutrality cache)

## Stated DoD (issue #301)

| DoD row | Status | Evidence |
|---|---|---|
| every row of coloured blanks in the repro survives the shrink and the grow | PASS | `repro.sh` below: all three fills at 40x10 and at 120x40 on the fix; v0.59.1 loses the indexed row at both |
| unit test with these exact bytes, seen failing on 6bc9d94 | PASS (cited) | `smoke_301` in `crates/shux-vt/src/lib.rs`; red run is the implementer's, not re-run |
| `make vt-neutrality` 0/0 apart from the intended change | PASS (cited) | `neutrality.json`: base 974f3b2, head 81b3c8d, status pass |
| VT QA gate passes | PASS | this report; `make check-vt-qa-pregate` passed against a temporary index with the manifest staged: "3 VT path(s) touched; evidence conforming" |

## Testing matrix

| Layer | Status | Evidence |
|---|---|---|
| unit / integration | PASS (cited) | `make check`, 3348 tests, at 81b3c8d |
| raw replay corpus, live rich-TUI A/B, colour probe, fuzz, leaks | PASS (cited) | `neutrality.json`, `neutrality/*.json` |
| shux automation: the issue's repro in a real `bash --norc` | PASS | `.shux/out/resize-blank-row-301/repro.sh` |
| shux automation: closure inputs and erase variants | PASS | `.shux/out/resize-blank-bg/delta/{work,drive}.sh`, cases ed1, el1, ech, el0, keep |
| readline editing across a wrap | PASS | `.shux/out/resize-blank-row-301/rl.sh` |
| EL around colour at the wrap edge (GNU grep shape) | PASS | cases grepedge, grepmid |
| real TUIs across resizes | PASS | `delta/tui.sh`: vim, htop, lazygit with private config copies |
| visual inspection | PASS | frames listed below, read at native resolution |
| pixel verification | PASS | 13 `pixel-*.json` here + 16 neutrality metrics, all 0/0; negative control below |
| DootSabha design + implementation | PASS | `council-design.md`, `council-implementation.json` (one council, codex + agy, on a7ef8fa, after the fix was written; the record says so). Closure review of 916bdcd: 2 findings, fixed in 81b3c8d and re-driven below |

## The issue's repro

The pane is 80x24 running `env PS1='R301> ' bash --norc`. The script types
`clear; printf '\033[48;2;30;60;160m%40s\033[0m\n\033[48;5;130m%40s\033[0m\n\033[42m%40s\033[0m\n' '' '' ''`
(it soft-wraps as it echoes) and presses Enter. The pane then shrinks to 40x10 and grows to
120x40.

| Size | fix (81b3c8d) | v0.59.1 |
|---|---|---|
| 80x24 after the command | 3 fills | 3 fills (pixel-identical: `pixel-v0591-repro-0-80x24.json`) |
| 40x10 | truecolor, indexed, green | truecolor, green: indexed row lost |
| 120x40 | truecolor, indexed, green | truecolor, green: not restored |

Negative control: fix vs v0.59.1 at 40x10 fails with 14092 changed pixels, so the
comparator can see the change.

## Closure inputs (2x5, widen to 10, then grow to 10x4 to show history)

| Input | fix | v0.59.1 |
|---|---|---|
| closure 2: `ABCDEF\r\n ESC[1;5H ESC[1J ESC[H XYZ` | `ABCDE` in history, `XYZ` on row 0 | `ABCDEXYZ` joined |
| same via EL 1 at the last column | same as above | joined |
| same via ECH 5 over the row | same as above | joined |
| same via EL 0 from column 1 | same as above | joined |
| control, no erase: `ABCDEF\r\n ESC[H X` | `ABCDEX` (link kept) | `ABCDEX` |

Closure 5, a 1-row screen, can't be driven: `pane set-size` needs at least 2 rows. It is
covered by the unit test the fix added.

## Readline across a wrap (40x10, `bash --norc`)

The script types a 42-character `echo` (it wraps), sends 25 backspaces back across the
wrap, retypes 25 characters, then resizes to 80x10, 30x10 and 100x10. At 80 and 100 columns
the edited line rejoins as one line. Every frame is pixel-identical to v0.59.1 (6
`pixel-v0591-rl-*.json`). The doubled prompt at 30x10 is bash's own SIGWINCH redraw, and
it is identical on both builds.

## EL around colour at the wrap edge

GNU grep emits `SGR + ESC[K` around each match. In `grepedge`, a match ends exactly at
column 40 and is followed by `ESC[m ESC[K` and more text.
- Both builds erase the `H` under the pending-wrap cursor.
- v0.59.1 then wraps, and after a widen leaves a blank cell where `H` was.
- The fix cancels the deferred wrap, so the next character lands on column 40 and the line
  rejoins with no gap. This follows xterm's ClearRight, which clears the row's wrapped flag
  and resets the pending wrap.
- No character other than the erased `H` is lost on either build.

`grepmid`, with matches mid-row on both physical rows, is identical on both builds.

## Real TUIs

vim (`hi Normal ctermbg=17`, indexed), htop (basic + 256 colours) and lazygit, each across
80x24, 120x40, 200x60, 50x15 and back to 80x24:
- vim against base 974f3b2: 5/5 at 0/0 (`pixel-base-vim-*.json`).
- htop and lazygit: inspected, no artifacts.

The truecolor probe is the repro's own 24-bit fill, plus the neutrality colour-probe metrics.

## Screenshots (under `.shux/out/resize-blank-row-301/`)

| Viewport | Workload | fix | v0.59.1 |
|---|---|---|---|
| 40x10 | repro after shrink | `new/repro-1-40x10.png` | `v0591/repro-1-40x10.png` |
| 120x40 | repro after grow | `new/repro-2-120x40.png` | `v0591/repro-2-120x40.png` |
| 40x10 | readline edit | `new/rl-2-retyped.png` | `v0591/rl-2-retyped.png` (0/0) |

TUI frames: `.shux/out/resize-blank-bg/delta/n301/tui/`.

## Findings

P0 / P1 / P2: none.

P3 (process): one council served as both the design and the implementation review, held
after the fix was written. Its record says so, and it reached a design-level verdict.

## Residual risk

- The 1-row-screen closure case is covered only by the unit test; the daemon can't size a
  pane below 2 rows.
- `make check`, the corpus and fuzz are cited from `neutrality.json` and the parent's run.

## Cleanup

Runtime dirs `/tmp/q3n`, `/tmp/q3o`, `/tmp/qn301`, `/tmp/qo0591` and `/tmp/qb301`: no
daemon running and no leaked processes. TUIs used private config copies under
`/tmp/svtq/cfg`, and the real htoprc is unchanged.
