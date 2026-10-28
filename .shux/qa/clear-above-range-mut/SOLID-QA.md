VERDICT: PASS

# SOLID VT QA — `clear-above-range-mut`

## 1. Change under audit

- Issue #196 (beta clippy `needless_range_loop`), PR #205, branch
  `claude/project-thread-ejlz65`, head `33600e9` (417a8a3 merged with
  origin/main `16ce207`). Base = merge-base `16ce207`.
- VT surface: `Grid::clear_above` in `crates/shux-vt/src/grid.rs` iterates
  `self.raw.range_mut(sb..=sb + end_row)` instead of indexing `sb..=(sb + end_row)`;
  new unit test `test_clear_above_leaves_scrollback_alone`. Only production caller:
  ED 1 (`ESC[1J`) in `crates/shux-vt/src/parser.rs`.
- Non-VT (out of this gate's scope, audited by Agent B, see
  `council-substitution.md`): `make clippy-beta` + CI step, four unused deps
  removed, a shellcheck-only rewrite in `.shux/scripts/reap_daemons.sh`.

## 2. Stated DoD matrix

No Testing Matrix/DoD is stated for the VT change beyond "behaviour-preserving
refactor"; derived from the diff:

| Criterion | Status | Evidence |
|---|---|---|
| shux-vt tests green | PASS | 683/683, 0 skipped (lib + 15 integration binaries) |
| New test catches a broken scrollback offset | PASS | implementation mutant `range_mut(0..=end_row)` → test red, `grid.rs:2029 left ' ' right 'X'` |
| ED 1 renders identically base vs head | PASS | `pixel-ab-ed1-{80x24,120x40,200x60}.json`, exact 0/0 |
| Text capture identical base vs head | PASS | byte-identical at all 3 breakpoints |
| No corpus regression | PASS | 19/19 fixtures exact vs `.shux/goldens/073-vt-corpus` |
| Zero leaked daemons | PASS | 0 |

## 3. Testing matrix

| Layer | Result |
|---|---|
| Unit | PASS — `make test-focus PKG=shux-vt`: 683 run, 683 passed; new test passes on head, red on the mutant for the right reason (scrollback row 0 erased). Pre-existing `test_clear_above` stays green under the mutant, so the new test is the only guard on the offset. |
| Integration | PASS — all 15 shux-vt integration binaries in the same run. |
| Raw replay | PASS — `make test-vt-corpus`: replay unit 3/3, harness 19/19 pixel-exact (5 real-TUI recordings + 14 synthetic). No fixture contains `ESC[1J`, so this is no-regression only. |
| Shux automation | PASS — release daemons for base and head, real pane, scene below, `pane capture` + `pane snapshot`. |
| Visual inspection | PASS — all six A/B PNGs and the vim PNG opened at native resolution. |
| Pixel comparison | PASS — exact 0/0 at 80x24, 120x40, 200x60. |
| DootSabha design / diff | SUBSTITUTED — not installed; parallel adversarial agents per CLAUDE.md *Tooling fallbacks*, run by the implementing session (`council-substitution.md`). |

**Scene.** Pane waits for its geometry via `stty size`, prints rows+30 history lines
coloured with truecolor `38;2;120;220;180`, indexed `38;5;208`, basic `3x` and an
indexed background `48;5;236` (pushes ≥30 lines to scrollback), then `ESC[1J` at
(2/3 rows, cols/2) under `48;5;52`, writes a text band, then `ESC[1J` at
(rows/3, cols/3) under `48;2;40;40;90`. The completion marker is printed by the
outer shell only after the workload exits and is assembled from printf arguments,
so no echoed or source line contains it.

**Negative control.** A release binary built with the same mutant, run through the
same scene against base: text differs and `pixel_verify` FAILs at every breakpoint
(diff ratio 0.575 / 0.592 / 0.629). The scene and comparator can fail.

## 4. Screenshot matrix

| Viewport | Workload | Actual (head) | Expected (base) | Diff | Status |
|---|---|---|---|---|---|
| 80x24 (720x456) | ED 1 scene | `.shux/out/clear-above-range-mut/ab/head-80x24.png` | `…/ab/base-80x24.png` | `…/ab/ab-ed1-80x24-diff.png` | pass, 0 px |
| 120x40 (1080x760) | ED 1 scene | `…/ab/head-120x40.png` | `…/ab/base-120x40.png` | `…/ab/ab-ed1-120x40-diff.png` | pass, 0 px |
| 200x60 (1800x1140) | ED 1 scene | `…/ab/head-200x60.png` | `…/ab/base-200x60.png` | `…/ab/ab-ed1-200x60-diff.png` | pass, 0 px |
| 120x40 | vim, syntax + termguicolors | `…/vim/vim-120x40.png` | — (smoke) | — | inspected |

Pixel probe on every A/B PNG: truecolor-cleared rows carry rgb(40,40,90), the
indexed band rgb(95,0,0), untouched cells (16,16,24); `png_not_blank.py` ok
(1536–1559 colours, 66–68% ink). No screenshot is committed: the base side is
the audit's own capture, not a tracked baseline.

## 5. Findings

- **P3 (from Agent A, re-checked by reading the caller):** for `end_row >= rows`
  the new loop panics before writing; the old one panicked part-way. Unreachable:
  ED 1 passes `cursor.row - 1` only when `cursor.row > 0`.
- **P3 (audit observation):** the CLI exposes no scrollback capture, so live
  scrollback preservation is shown indirectly (a broken offset changes the visible
  screen, as the negative control shows) and directly only by the unit test.
- No P0/P1/P2.

## 6. Residual risk

Low. vim is the only installed rich TUI (nvim, btop, htop, lazygit missing); the
committed real-TUI recordings cover that layer in the corpus run.

## 7. Cleanup

Every pane ran under `.shux/scripts/no_leak_guard.sh` in its own `/tmp/sx?.XXXXXX`
runtime and was stopped through its pidfile. Afterwards there were 0 `shux*`
processes machine-wide, no leftover runtime dirs, and no vim or sleep children.
The tracked files that `make test-vt-corpus` rewrote were restored.
