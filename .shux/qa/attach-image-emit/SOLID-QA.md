VERDICT: PASS

# SOLID VT QA — attach-image-emit (audit 7, confirmation pass)

## 1. Change under audit

| | |
|---|---|
| Branch | `claude/shux-image-support-s0u4uy` |
| Frozen HEAD | `2195884` — *fix(ui): stop the graphics override losing to a capital letter* |
| Base | `origin/main` @ `ec62be0` |
| Scope | `.shux/qa/attach-image-emit/` |
| Scratch (gitignored) | `.shux/out/attach-image-emit-r7/` |
| Prior round | audit 6 FAILed `89013c2` on one P1: `SHUX_GRAPHICS` matched exact lowercase bytes, so `OFF` fell through and produced the corruption the valve exists to prevent |

Round 7 re-ran the whole claim set. Every result below was produced during THIS
audit; nothing was carried forward from round 6.

## 2. Stated DoD matrix

| # | Required item | Verdict | Evidence |
|---|---|---|---|
| 1 | `make check` green | PASS | 2280 run / 2280 passed / 2 skipped — `release-check.log`. The parent's 2279 was the pre-`2195884` count; the new override test is +1 |
| 2 | `make shellcheck` clean | PASS | 109 tracked scripts clean — `shellcheck.log` |
| 3 | `make test-gui-terminal` (selftest + plain + image-pane) | PASS | exit 0 — `gui-terminal.log` |
| 4 | `image-overflow` explicitly | PASS (fails by design) | exit 1, 4584 image px outside the pane — `gui-overflow.log`, `overflow/f_inject_01.png` |
| 5 | P1 fixed: A5 clean end to end through client → daemon → emitter → outer VT | PASS | `matrix-fixed.txt`, `matrix-fixed2.txt` |
| 6 | The matrix still measures (a control can still corrupt) | PASS | `A2`/`A2u`/`A3`/`A12`/`B10` still CORRUPT on the fixed binary |
| 7 | Ask (2): the case-insensitivity test seen failing first, verified independently | PASS | `mutant-redfirst.log` |
| 8 | Ask (3): stderr line fires when it should, silent when it should, cannot corrupt | PASS | §5 |
| 9 | Images reach real kitty, clipped by source rect, zero px outside the pane | PASS | `.shux/out/gui-terminal/image-pane/f_pane_01.png` vs `overflow/f_inject_01.png` |
| 10 | `C=1` / `q=2` / `z=-1` | PASS | real attach byte stream, §6 |
| 11 | Unchanged picture emits nothing | PASS | one `a=T` in 7 s of frames, §6 |
| 12 | Fresh id then retire; id replacement | PASS | `swap.bytes`, §6 |
| 13 | Opening-chunk terms | PASS | measured in pixels, §7 |
| 14 | Overlays legible; cursor correct | PASS | `z=-1` on the wire; cursor restored after emit; frames inspected |
| 15 | Rich TUIs unregressed | PASS | replay corpus + 4 live TUIs × 3 breakpoints, §8 |
| 16 | Snapshot paths agree | PASS | pane / window / session all carry exactly 19200 image px; window vs session 0/0, §9 |
| 17 | Zero leaked processes | PASS | §11 |
| 18 | DootSabha design + implementation-diff council evidence | PASS (substituted) | `council-substitution.md` |

## 3. Testing matrix

| Layer | Result | Evidence |
|---|---|---|
| Unit | PASS | `shux-ui` lib tests incl. all six `attach::tests` override cases |
| Integration | PASS | `attach_image_emit.rs` (14), `attach_image_retransmit.rs` (2), `graphics_chunked_transmit.rs` (6), `window_snapshot_images.rs` (5) |
| Raw byte / replay | PASS | `make test-vt-corpus` — committed `rich-tui` + `synthetic` fixtures, `vt-corpus.log` |
| shux automation | PASS | tmux 3.4 truth table (18 + 7 cases), live TUI pass, A/B snapshots at three breakpoints |
| Visual inspection | PASS | full-resolution PNGs opened as images: `image-pane/f_pane_01.png`, `overflow/f_inject_01.png`, `tui/btop-200x60.png`, `tui/lazygit-120x40.png`, `ab/head-img-80x24.png`, `paths/window.png` |
| Pixel comparison | PASS | 4 committed 0/0 metrics + a demonstrated failing arm |
| DootSabha design | PASS (substituted) | `council-substitution.md` §"Step 1" |
| DootSabha diff review | PASS (substituted, with a documented gap) | `council-substitution.md` §"Step 7" and §"Step 7, second pass" |

## 4. The P1, re-measured

`terminal_can_draw_images` now trims and lower-cases before matching, as
`crates/shux/src/gate/driver.rs::is_ci` does.

**Red-first, independently.** `/tmp/shux-a7-mut` = a worktree at `2195884` with
exactly one edit — `.trim().to_ascii_lowercase()` removed, nothing else. The
UNCHANGED test goes red for the right reason:

```
FAIL shux-ui attach::tests::the_override_is_case_insensitive_and_trimmed
  panicked at crates/shux-ui/src/attach.rs:764:17: "ON" did not force images on
PASS attach::tests::the_override_wins_in_both_directions
PASS attach::tests::an_unrecognised_override_leaves_the_decision_automatic
```

The three pre-existing override tests still pass on the mutant, so the new test
is the one that catches THIS defect and not merely "the code does what it does".

**Truth table, tmux 3.4, real pane, real daemon, real emitter.** 18 cases on the
fixed binary; every verdict matched its expectation. Then the same reduced set
against a release build of the mutant, same harness, same workload:

| case | mutant (untrimmed) | fixed `2195884` |
|---|---|---|
| `sudo … SHUX_GRAPHICS=off` | CLEAN | CLEAN |
| `sudo … SHUX_GRAPHICS=OFF` | **CORRUPT** | CLEAN |
| `sudo … SHUX_GRAPHICS=Off` | **CORRUPT** | CLEAN |
| `sudo … SHUX_GRAPHICS='  off  '` | **CORRUPT** | CLEAN |
| `sudo … SHUX_GRAPHICS=No` | **CORRUPT** | CLEAN |
| `SHUX_GRAPHICS='   '` in tmux | CLEAN | CLEAN |
| `SHUX_GRAPHICS=ON` in tmux | CLEAN (hatch missed) | **CORRUPT** (hatch works) |

CORRUPT = the tmux pane title became `Gq=2,m=0;AP+HAP+H…`. Also covered clean on
the fixed binary: `FALSE`, `False`, `0`, and the empty value.

**The control still works.** `SHUX_GRAPHICS=on`/`ON` inside tmux, and the bare
`sudo` gap with no override, still corrupt the title on the fixed binary. The
matrix has not stopped measuring.

## 5. The stderr line

- **Fires when it should.** `SHUX_GRAPHICS=disable` → one line, once per attach,
  in the raw pipe-pane bytes: `shux: ignoring SHUX_GRAPHICS="disable" (expected on/off)`.
- **Silent when it should be.** Unset (A1/A3/A17), empty (A13), whitespace-only
  (B9): zero occurrences.
- **Cannot corrupt the display.** It is written at `attach.rs:111`, before
  `TerminalGuard::enter()` at `attach.rs:155` — before raw mode and before the
  alternate screen. Single call site, so it cannot repeat per frame.
- **Cannot be used to inject.** `{raw:?}` is `Debug`, so control bytes are
  escaped. With `SHUX_GRAPHICS=$'x\e]2;PWNED\ay'` the terminal received
  `shux: ignoring SHUX_GRAPHICS="x\u{1b}]2…`; a grep of the raw bytes for
  `ESC ] 2 ; PWNED` found 0, and the tmux pane title stayed `vm`.
- **Closed stderr does not kill the attach.** `exec 2>&-` plus an unrecognised
  value: the attach rendered normally. Measured `/proc/<pid>/fd/2 -> /dev/null`,
  not a reused socket, so `eprintln!` had somewhere legal to write.

## 6. What the wire actually carried

From a real attach inside tmux with the daemon emitting (`fixed-case-A2-on-tmux.bytes`,
104 432 bytes, ~7 s of frames):

```
a=T,f=24,t=d,i=2000000000,s=160,v=120,p=1,C=1,q=2,z=-1,y=0,w=160,h=120,c=18,r=7,m=1
… 18 continuation headers, each exactly `q=2,m=`
```

One opening transmit for the whole run — the unchanged picture cost nothing on
every later frame, measured on the wire rather than asserted in a unit test.
`C=1`, `q=2`, `z=-1`, `p=1` all present. Source rect `y=0,w=160,h=120` with
`c=18,r=7` cells matches `DECLARED_CELL_PIXELS`.

Fresh-id-then-retire and id replacement, from a second run where the pane
redraws under one `i=7` and then issues `a=d,d=A` (`swap.bytes`):

```
@27299  a=T,…,i=2000000000,…      first picture, fresh host id
@56677  a=T,…,i=2000000001,…      replacement transmitted BEFORE any delete
@86037  a=d,d=I,i=2000000000,q=2  only then is the old id retired
@86072  a=d,d=I,i=2000000001,q=2  and retired again when the placement goes
```

The pane is never blanked for the duration of a transfer, and both retirements
use `d=I`, so pixels are freed and not just the placement.

## 7. Pixel verification

Committed metrics, all exact `0`/`0`, produced by
`.claude/automations/pixel_verify.py` during this audit:

| Metric | Compares | Result |
|---|---|---|
| `pixel-base-vs-head-colour-80x24.json` | base `ec62be0` vs head `2195884`, `pane.snapshot`, colour + wide + combining + DEC workload | 0 changed px of 328 320 |
| `pixel-base-vs-head-colour-120x40.json` | same at 120×40 | 0 of 820 800 |
| `pixel-base-vs-head-colour-200x60.json` | same at 200×60 | 0 of 2 052 000 |
| `pixel-crosspath-window-vs-session.json` | `window.snapshot` vs `session.snapshot` of a pane holding a picture | 0 of 738 720 |

**The comparator can fail.** The same base-vs-head A/B on a CHUNKED image
workload — one whose continuation chunks carry only `m=`, the way real
`kitten icat` does not and the protocol says they may — differs by exactly
19 200 px = 160×120 = the whole picture, at both 80×24 and 120×40. Base never
places such a transfer; head does. That is the opening-chunk claim measured in
pixels, and it is why the three 0/0 results above are not vacuous.

No PNG is committed: nothing here compares against a baseline this repo tracks,
so a committed `*-actual.png` would have nothing to be diffed against.

## 8. Rich TUIs

- Replay: `make test-vt-corpus` PASS over the committed `rich-tui` fixtures
  (btop, lazygit, nvim, vicaya, vivecaka) and the synthetic set.
- Live, through shux, at 80×24 / 120×40 / 200×60, each capture and PNG inspected:
  **btop**, **lazygit**, **nvim**, **vicaya-tui** — box drawing, braille graphs,
  truecolor/indexed/basic, underlined links, dialog overlays, all correct; no
  tofu, no ghost cells, no clipping.
- Unavailable, recorded rather than substituted: **htop** (not installed),
  **vivecaka** (requires `gh`, not installed). Both are covered by committed
  replay fixtures.

## 9. Screenshot matrix

| Viewport | App / path | Screenshot | Baseline | Diff | Status |
|---|---|---|---|---|---|
| kitty 1400×900 | shux + pane-emitted 320×240 image | `.shux/out/gui-terminal/image-pane/f_pane_01.png` | — | — | PASS — picture clipped inside the pane; border, status bar and probes untouched |
| kitty 1400×900 | same bytes straight to the emulator | `.shux/out/attach-image-emit-r7/overflow/f_inject_01.png` | — | — | FAILS BY DESIGN — 4584 px over the border, status bar and desktop; the control |
| 80×24 | chunked image, head | `ab/head-img-80x24.png` | `ab/base-img-80x24.png` | `ab/diff-img-80x24.png` | differs by 19 200 px (intended) |
| 80/120/200 | colour workload | `ab/head-colour-*.png` | `ab/base-colour-*.png` | `ab/diff-colour-*.png` | 0/0 |
| 120×40 | window / session snapshot of an image pane | `paths/window.png`, `paths/session.png` | each other | `paths/diff-window-session.png` | 0/0 |
| 200×60 | btop | `tui/btop-200x60.png` | — | — | PASS |
| 120×40 | lazygit | `tui/lazygit-120x40.png` | — | — | PASS |
| 80/120/200 | nvim, vicaya-tui | `tui/nvim-*.png`, `tui/vicaya-*.png` | — | — | PASS |

## 10. Findings

No P0. No P1.

**P2 — the emit path and the raster path disagree about z-order, and nothing
says which is right.** The emitter asks for `z=-1` (verified on the wire, §6),
which kitty draws UNDER text; that was a deliberate fix so copy mode, the help
sheet and the welcome toast stay legible over a picture. `shux-raster`
composites placements AFTER every cell, so a snapshot draws the picture OVER the
pane's own glyphs — measured here, not inferred: in `paths/window.png` and
`ab/head-img-80x24.png` the pane printed `AB-READY` at a row the picture covers
and only `AB-` survives. Same pane, two render paths, opposite answers.

Not a regression on either side — the raster behaviour is unchanged from
`ec62be0`, and before this branch attach drew no images at all, so the
divergence exists only because attach gained a capability. The trigger is narrow
(cells under a picture must actually hold glyphs; the common `icat` case leaves
them blank). It does not block: no stated criterion covers attach-vs-raster
z-order, and neither path corrupts anything. But it is undocumented, no test
asserts either contract, and six prior rounds passed `z=-1` without noticing.
Worth a line in the PR and a follow-up issue.

**P3 — the docs still do not say the thing that caused the P1.**
`docs/configuration.md` lists the accepted spellings but never says the match is
case-insensitive and trimmed, and does not mention the new stderr diagnostic.
The user in the failure story is precisely one who read that section and
capitalised.

**P3 — the stderr diagnostic has no test.** Verified by hand this round (§5); it
can regress silently. A one-line assertion on the message would cost little.

**P3 — the step-7 record for `2195884` documents an omission rather than a
review.** `council-substitution.md` honestly records that `2546cbc` and
`89013c2` shipped without step 7, and that "the gate caught it" is not the same
as "the step ran". It does not record a step-7 review of `2195884` itself. The
adversarial pass in this audit is a partial substitute, and the honesty of the
note is worth more than the gap costs.

**P3 — pre-existing, out of scope, reproduced at the base.** `make test-vt-corpus`
rewrites 19 TRACKED PNGs under `.shux/qa/073-shux-vt-corpus-regression-harness/`
on every run. The rewritten files are RGB-identical to the committed ones and
differ only in alpha (mean channel delta exactly 63.75 = 255/4), so the
committed artifacts are encoder-stale, not a regression. Confirmed identical
behaviour on a clean `ec62be0` worktree, so this branch did not cause it. Both
working trees were restored. A gate that dirties its own tracked evidence is a
defect in verification machinery and deserves its own issue.

## 11. Passed evidence

`make check` (2280/2280) · `make shellcheck` (109) · `make check-test-groups`
(three groups, exclusively claimed) · `make test-gui-terminal` (selftest + plain
+ image-pane) · `image-overflow` failing by design · `make test-vt-corpus` ·
18-case + 7-case tmux truth tables against both the fixed and a mutant binary ·
red-first proof on an unchanged test · real attach byte streams for transmit,
re-place, replace and retire · 4 committed 0/0 pixel metrics plus a demonstrated
failing arm · four live rich TUIs at three breakpoints · pane/window/session
snapshot agreement.

## 12. Residual risk

- Attach-vs-raster z-order (P2) is untested in both directions.
- `htop` and `vivecaka` could not be driven live on this host; both are covered
  only by committed replay fixtures.
- The `sudo`/`ssh`/`env -i` gap the override exists for remains a heuristic by
  design: the check reads the client's environment while the corruption is a
  property of the byte stream. That is documented in the source and in
  `docs/configuration.md`, and the valve now works in both directions and in any
  casing.

## 13. Cleanup

Zero leaked processes. Daemons stopped in every runtime dir used
(`/tmp/a7rt`, `/tmp/abbase`, `/tmp/abhead`, `/tmp/a7tui`, `/tmp/a6rt`); `tmux -L a7`
and `-L a6` servers gone; no `kitty` or `Xvfb` survivors; no `target/release/shux`
process left. Both git worktrees restored clean after the corpus harness dirtied
them.
