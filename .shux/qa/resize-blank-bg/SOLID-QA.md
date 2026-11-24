VERDICT: PASS

commit_under_audit: de65edccee1119afe1f323df9f8d1e2ca976db14
merge-base: 444b6ba50035efe0191904814149d84c39a67b11 (origin/main, 0.59.0, includes #294 / #190 B1)
scope: resize-blank-bg (issue #295, PR #300, branch fix/resize-keeps-blank-bg), tier full
audit type: delta re-audit #3, the rebase onto 444b6ba (cursor.rs conflict resolution and
c35cdb1), on top of re-audits #1 and #2. Those are kept below under their pre-rebase SHAs.

## Pre-rebase to rebased SHAs

The fix commits' changed lines are identical before and after the rebase (compared `+`/`-`
lines; only context differs where #294 touched cursor.rs).

| Pre-rebase | Rebased | Commit |
|---|---|---|
| 7cff347 | bebbfad | mark the wide-glyph wrap gap on its cell |
| f25c203 | 9aaa0b9 | cursor intent, reflowed history trim, `Cell::meta` |
| 7ae17af | 26125f5 | closure-review P0/P1 fixes |
| 72f5044 | e517ab6 | VT gate PASS (re-audit #1) |
| 8ed9a6c | 5d41e7a | `after_print` flag, carried by DECSC |
| fcee6d0 | 8a009a7 | neutrality refresh |
| new | c35cdb1 | build fix: `meta` in `shux-ui/src/viewport.rs`, `after_print` made `pub` |
| new | de65edc | neutrality refresh: base 444b6ba, head c35cdb1, 6/6 layers pass (cited) |

## Re-audit #3: the rebase

`make check-vt-qa-pregate` at de65edc passes: "9 VT path(s) touched; evidence conforming in resize-blank-bg".

Review of the rebase:
- **cursor.rs:** `Cursor` keeps both #294's `hidden_since_ns` (latch stamp, `hidden_for_ns`)
  and `after_print`. `Cursor::new()` initialises both. `save` and `restore` carry
  `after_print` and leave the hidden stamp alone, as on main. Resize writes cursor fields in
  place, so the stamp survives a reflow.
- **c35cdb1:** `blank_like` in viewport.rs (the B1 crop blank) sets `meta: Default::default()`.
  That is right: a crop blank is display-only, and a gap mark there would mean nothing.
  `after_print` became `pub` only so that viewport.rs tests compile `..Cursor::new()` across
  crates (private fields block functional record update). That widens the public API, which
  is P3: `hidden_since_ns` is already `pub` on the same struct.

Real binary: h3 = c35cdb1 release build (`git_sha` c35cdb1; de65edc changes no code),
b2 = 444b6ba (vt-neutrality cache).

| Input | h3 | Expected | Status |
|---|---|---|---|
| bot 1: `ABCDE ESC[1;6H`, 10x4 -> 5x4, X | `ABCDX` | `ABCDX` | PASS |
| bot 2: `ABCDE ?1049h`, shrink to 5 in alt, `?1049l F` | `ABCDE` / `F` | E kept, F on next row | PASS |
| `ABCDE ESC7 ESC[3;1H zz ESC8`, shrink, X | `ABCDE` / `X` / `zz` | DECRC restores intent | PASS |
| closure inputs: CUP one past the edge, ICH widen, `ABCDE ` intent, history at the cap, 4-col DCH gap rows | `abc F`, `c   界`, `ABCDE`/`F`, `TOP`/`mid`, no panic | unchanged | PASS |
| B1: pane pinned 120x40, 80x24 attach client, indexed-bg ED fill, truecolor `ABCDE`, `?25l` | client: crop with "pinned 120×40" label, no cursor drawn | | PASS |
| then pin narrows to 5x40, X typed | pane and client: `ABCDE` / `X`, cursor (1,1) hidden; 5-col blue column kept; surplus shaded | hidden cursor does not end print intent | PASS |
| then pin back to 120x40 | `ABCDEX` rejoined, cursor (0,6) still hidden, client label "pinned 120×40" | | PASS |

Pixel: coloured blanks on the main and alt screens over 6 sizes, h3 against the pre-rebase
build (8ed9a6c): 12/12 at 0/0 (`pixel-r3-rebase-*.json`). The rebase moved nothing on the
change's own surface. vim across 5 sizes, h3 against base 444b6ba: 5/5 at 0/0
(`pixel-r3-base-vim-*.json`). Re-audit #1/#2 metrics, whose arms were pre-rebase builds,
have been removed from the manifest and the tree.

Frames: `.shux/out/resize-blank-bg/delta/h3/b1/{0-pin120x40,1-pin5x40,2-typed,3-pin120x40}-{inner,attach}.png`,
read at full resolution.

Findings for re-audit #3: no P0, P1 or P2. P3: `after_print` and `SavedCursor::after_print`
are now `pub` for a test's sake. The P2 alt-screen height-shrink finding from re-audit #1
stands, unchanged: it was already there before this branch.

Cleanup: daemons in `/tmp/qh3`, `/tmp/qh3i`, `/tmp/qh3o` and `/tmp/qb2` report not running,
and no processes leaked.

## Re-audit #2: 72f5044..fcee6d0 (pre-rebase SHAs; see the map above)

8ed9a6c fixes two P1s from the PR bots. A new `Cursor::after_print` flag is set by printing
and cleared by every cursor command. DECSC/DECRC (and SCOSC/SCORC) save it, and reflow
carries it. fcee6d0 refreshes `neutrality.json`: head 8ed9a6c, 6/6 layers pass (cited).
`make check-vt-qa-pregate` at HEAD passes ("8 VT path(s) touched; evidence conforming").

Binaries: h2 = 8ed9a6c release build (`git_sha` 8ed9a6c; fcee6d0 changes no code),
p1 = 7ae17af. Same scripts as re-audit #1 (`.shux/out/resize-blank-bg/delta/`), output
under `h2/` and `p1/`. Each case starts at 10x4, shrinks to 5x4, then types X:

| Input | h2 (HEAD) | p1 (7ae17af) | Expected | Status |
|---|---|---|---|---|
| bot 1: `ABCDE ESC[1;6H` | `ABCDX` | `ABCDE` / `X` | `ABCDX` | PASS (red on p1) |
| `ABCDE ESC[C ESC[D` | `ABCDX` | `ABCDE` / `X` | addressed, so overwrite | PASS |
| `ABCDE CR ESC[5C` | `ABCDX` | `ABCDE` / `X` | addressed | PASS |
| bot 2: `ABCDE ?1049h`, shrink in alt, `?1049l F` | `ABCDE` / `F` | `ABCDF` | F on next row, E kept | PASS (red on p1) |
| `ABCDE ESC[31m` (SGR, no move) | `ABCDE` / `X` | same | intent kept | PASS |
| `ABCDE ESC[K` (EL, no move) | `ABCDE` / `X` | same | intent kept | PASS |
| `ABCDE ESC7 ESC[3;1H zz ESC8` | `ABCDE` / `X` / `zz` | `ABCDX` / `` / `zz` | DECRC restores intent | PASS (red on p1) |
| `ABCDE CSI s ESC[3;1H zz CSI u` | `ABCDE` / `X` / `zz` | `ABCDX` / `` / `zz` | SCORC restores intent | PASS (red on p1) |

Regressions from re-audit #1, all unchanged on h2: CUP one past the edge (`abc F`), ICH widen
(`c   界`), `ABCDE ` intent (`ABCDE` / `F`), history at the 5000-line cap (`TOP` / `mid`).
The 4-column DCH gap-only rows run through 8x2, 4x2, 12x3 and 5x2 with no panic, and Q
prints. Coloured blanks on the main and alt screens over 6 sizes: h2 against p1, 12/12 at
0/0 (`pixel-r2-delta-*.json`). vim across 80x24, 120x40, 200x60, 50x15 and 80x24: h2 against
p1, 5/5 at 0/0 (`pixel-r2-vim-*.json`). htop across the same sizes, inspected at full
resolution (`h2/tui/htop-*.png`): no artifacts. The TUIs used private config copies, and the
real htoprc is unchanged. Daemons in `/tmp/qh2` and `/tmp/qp1` report not running, and no
processes leaked.

Findings for re-audit #2: no P0/P1. The P2 alt-screen height-shrink finding from re-audit #1
stands unchanged: it was already there before this branch, and is out of this delta.

## Re-audit #1: 7cff347..7ae17af (pre-rebase SHAs; see the map above)

commit audited: 7ae17af1cf0d5a3a4231d436a97da0a862c5fb3e
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
