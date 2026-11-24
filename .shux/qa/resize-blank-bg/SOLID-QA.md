VERDICT: PASS

commit_under_audit: 7ae17af1cf0d5a3a4231d436a97da0a862c5fb3e
merge-base: f1478e4b1e5dcc163aaeb635069dec0b901790d4
scope: resize-blank-bg (issue #295, branch fix/resize-keeps-blank-bg), tier full
audit type: delta re-audit of 7cff347..7ae17af (f25c203, 7ae17af) plus the previous gate's findings
binaries: new = 7ae17af release build, old = 7cff347 (previous gate), base = f1478e4 (vt-neutrality cache)

## Previous finding

The gate on 7cff347 failed only because no DootSabha design-council record existed.
`council-design.md` now records a design council (codex + agy) held on 7cff347, after
implementation, and says so. Its three merge blockers (cursor intent, trim floor, wide-gap
marker) were fixed in f25c203. `council-implementation.md` records the implementation
council on 73e9de4. The closure review of f25c203 found four P0/P1s, fixed in 7ae17af.
Closed.

## Stated DoD (issue #295)

| DoD row | Status | Evidence |
|---|---|---|
| resize, shrink or grow, either axis, main or alt screen, keeps surviving blank background | PASS | bgmain/bgalt below; new vs base at 200x60 keeps the surviving blue area that base wipes |
| unit test seen failing first | PASS (cited) | previous gate: 16 shux-vt tests red on f1478e4; delta tests named in 7ae17af exist in `crates/shux-vt/src/lib.rs` |
| `make vt-neutrality` 0/0 apart from the intended change | PASS (cited) | `neutrality.json`: head 7ae17af, base f1478e4, 6/6 layers pass |
| design + implementation councils recorded | PASS | `council-design.md`, `council-implementation.md` |

## Testing matrix

| Layer | Status | Evidence |
|---|---|---|
| unit / integration | PASS (cited, not re-run per gate time-box) | parent `make check` at 7ae17af, 3294 tests |
| raw replay corpus A/B + goldens, live rich-TUI A/B, colour probe, fuzz, leaks | PASS (cited) | `neutrality.json`, `neutrality/*.json` |
| pregate | PASS | `make check-vt-qa-pregate` against a temporary index: "8 VT path(s) touched; evidence conforming" |
| shux automation, closure-review inputs | PASS | `.shux/out/resize-blank-bg/delta/{work,drive}.sh` |
| shux automation, coloured blanks main + alt, both axes | PASS | same scripts, 80x24 -> 40x24 -> 40x10 -> 120x40 -> 200x60 -> 80x24 |
| real TUIs across resizes | PASS | `delta/tui.sh`, `delta/btop2.sh`: vim, nvim, htop, btop, lazygit with private config copies |
| visual inspection | PASS | PNGs read at native resolution (list below) |
| pixel verification | PASS | 22 `pixel-*.json` here + 16 neutrality metrics, all 0/0 |
| DootSabha design | PASS | `council-design.md` (held after implementation; stated in the record) |
| DootSabha implementation | PASS | `council-implementation.md`; closure review fixed in 7ae17af |

## Closure-review inputs, driven on the real binary

| Input | new (7ae17af) | old (7cff347) | Result |
|---|---|---|---|
| 4x10 `abc ESC[1;6H`, shrink to 5, type F | `abc F`, cursor row 0 col 4 before typing | same | PASS: no deferred wrap from CUP |
| 4x4 `abc界 ESC[1;1H ESC[2P ESC[1;4H Z ESC[1;2H ESC[@`, widen to 8 | `c   界` | same | PASS: no stale gap bits |
| `ABCDE ` at 10 cols, shrink to 5, type F | `ABCDE` / `F` | `ABCDF` | PASS: design-council cursor-intent P1 fixed |
| history at the daemon's 5000-line cap, 3x10 `HHHHHHHHHH TOP mid ESC[2;4H`, narrow to 5 | `TOP`, `mid` | `HHHHH`, `TOP`, `mid` | PASS: the real-binary form of the `max_scrollback=1` case |
| `ab界 ESC[T ESC[2;1H ESC[3P` at 2x3 | not drivable: `pane set-size` needs at least 4 columns | | covered by `a_line_left_with_only_gap_cells_survives_a_resize` |
| 4-column form: `abc界 ESC[T ESC[2;1H ESC[4P` and `ESC[3P` at 4x2, then 8x2, 4x2, 12x3, 5x2, type Q | no panic, daemon alive, Q prints | same | PASS |

## Screenshot matrix (all under `.shux/out/resize-blank-bg/delta/`)

| Viewport | Workload | Screenshot | Compared with | Status |
|---|---|---|---|---|
| 80x24 .. 200x60 (6 sizes) | coloured blanks, primary (truecolor ED, indexed + basic EL, fg probes) | `new/bgmain-*.png` | `old/` 0/0 (`pixel-delta-bgmain-*.json`) | PASS |
| 80x24 .. 200x60 (6 sizes) | same on `?1049` alt screen | `new/bgalt-*.png` | `old/` 0/0 (`pixel-delta-bgalt-*.json`) | PASS |
| 200x60 | coloured blanks, primary | `new/bgmain-4-200x60.png` | `base/bgmain-4-200x60.png`: base wipes the blue fill, new keeps it | intended change |
| 80x24 .. 200x60, 50x15 | vim, indexed bg 17 | `new/tui/vim-*.png` | `base/tui/` 0/0 (`pixel-base-vim-*.json`) | PASS |
| 80x24 .. 200x60, 50x15 | nvim, truecolor bg | `new/tui/nvim-*.png` | `base/tui/` 0/0 (`pixel-base-nvim-*.json`) | PASS |
| 80x24 .. 200x60, 50x15 | htop, lazygit | `new/tui/{htop,lazygit}-*.png` | inspected | PASS |
| 120x40 | btop, two clock ticks after the resize | `new/tui/btop2-120x40-t2.png` | `base/tui/btop2-120x40-t2.png`, same layout | PASS |

The negative control fails as it should: `bgmain-0` against `bgalt-0` gives `status: fail`
with exit 1.

## Findings

P0 / P1: none.

P2, present before this branch, not caused by it (identical on f1478e4 and 7cff347):
alternate-screen height shrink with a background-filled screen. 40x24 -> 40x10 on `?1049`,
with text on rows 0-3 and the cursor on row 4 over a blue ED fill. `resize_canvas` trims
with `RowTrim::Blank`, so bg-filled rows count as content and are not popped from the bottom.
The top 14 rows leave the view while the cursor stays on row 4: the text disappears, and the
cursor now sits on what was row 18. Growing again restores the rows. A fullscreen app
repaints after SIGWINCH, so this only shows until the next repaint. Repro:
`delta/drive.sh new <bin> <rt> bgalt 80x24 40x24 40x10`, frame `new/bgalt-2-40x10.png`.
This is the alt-screen form of the primary-screen eviction that the implementation council
blocked on. The branch fixed it for the primary screen only.

P3: the btop frame taken right after a resize (`new/tui/btop-2-120x40.png`) shows the old
layout partly overdrawn. This was capture timing. Once the clock had ticked twice, new and
base show the same layout.

## Residual risk

- The exact 2x3 DCH input can't be reached through `pane set-size` (min 4 columns). It is
  covered by the unit test and by the 4-column form above.
- `make check`, the corpus and the councils' compliance are cited, not re-run (gate time-box).

## Cleanup

Every drive ran in its own runtime dir (`/tmp/qnew`, `/tmp/qold`, `/tmp/qbase`), and its
session was killed on exit. `daemon status` reports "not running" for all three. No orphan
btop, htop, lazygit, vim or nvim processes. TUIs used private config copies under
`/tmp/svtq/cfg`, and the real htoprc is unchanged.
