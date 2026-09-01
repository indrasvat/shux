VERDICT: PASS

# SOLID VT QA — multipane-image-clip

Scoped re-audit of `c836d8a..76fc6e8`. Third audit of this branch.

| | |
|---|---|
| Branch | `claude/shux-image-support-s0u4uy` |
| HEAD audited | `76fc6e8` — *fix(raster): scope the decode budget per PANE, and prove it* |
| Previously audited | `c836d8a` (FAIL, P1 starvation) · `3d46d74` (FAIL, unpinned call site) |
| Merge base | `0e97d24` |
| Scope | the `c836d8a..76fc6e8` delta only: `crates/shux-raster/src/lib.rs`, `crates/shux/tests/window_snapshot_images.rs`, `crates/shux/Cargo.toml`, `Cargo.lock` |
| Working tree | clean at `git status --porcelain` before the first command and after the last |

`dootsabha` is not installed (`command -v dootsabha` → not found). The substitution
is recorded in `council-substitution.md`; see §3 row 8 and finding P2-3.

---

## 1. What the delta claims, and what this audit found

The delta answers the P1 this gate raised at `c836d8a`: `MAX_RENDER_DECODE_BYTES`
was reset per RENDER, so `composite_placements` gave each pane 256 MiB while
`composite_composed` spent one 256 MiB across the whole window, first come first
served — a greedy pane deleted a neighbour's picture from `window.snapshot` while
`pane.snapshot` still drew it, order-dependently.

`76fc6e8` renames it `MAX_PANE_DECODE_BYTES` and keeps one budget per clip rect
inside `composite_composed`, looked up rather than reset on change.

Every claim the commit makes for itself was re-derived from scratch here. All five
hold. Three P2s remain — one introduced by this delta, one carried, one process.

---

## 2. Stated DoD matrix

| # | Claim made by `76fc6e8` | Result | Evidence |
|---|---|---|---|
| 1 | The budget is scoped per PANE, not per render | **PASS** | §4, §5 |
| 2 | Both paths spend the same budget on the same pane, so the drawn subset is identical by construction | **PASS** | §5 — 10 metrics, 0/0 exact, incl. a scene where the budget genuinely truncates |
| 3 | `a_greedy_pane_does_not_starve_its_neighbours_picture` pins it in both pane orders, and fails when the scoping is reverted | **PASS** | §7 mutation M17 — caught, exit 100, exact stated message, other 3 tests green |
| 4 | The constant's justification is corrected: 9.27 MiB per real `kitten icat`, ~27 pictures | **PASS** | §8 — arithmetic re-derived, backed by a real `kitten icat` APC header |
| 5 | Bound is now panes × 256 MiB | **PASS (measured, and see P2-1)** | §6 — quantified at 4 panes |

---

## 3. Testing matrix

| Layer | Result | Evidence |
|---|---|---|
| Unit | PASS | `make test` — 2253 run, 2253 passed, 2 skipped · `.shux/out/multipane-image-clip/r3-make-test.log` |
| Integration | PASS | `shux` + `shux-raster` suites, 926 tests, incl. the daemon-backed `window_snapshot_image_rpc` |
| Raw byte / replay | PASS | kitty APC `f=100` PNG fixtures driven through real PTYs; generator `budget/gen_r3.py`; real `kitten icat` capture `/tmp/sxatk/icat.raw`, 2349 APC chunks |
| shux automation | PASS | 22 daemon-backed sessions across 6 drivers, isolated `XDG_RUNTIME_DIR` each, run serially |
| Visual inspection | PASS | §9 — 5 full-resolution frames opened as images |
| Pixel comparison | PASS | §5 — 10 new metrics at exact 0/0, plus 9 committed metrics re-derived at HEAD |
| Comparator falsifiability | PASS | §5.3 negative control — same scene, same crop, pre-fix binary → `status: fail`, 17100 changed px, exit 1 |
| DootSabha design | PASS (substituted) | `council-substitution.md` §1 |
| DootSabha diff review | **P2-3** | `council-substitution.md` §7 records the branch, but names `c836d8a`, not `76fc6e8` |
| Lint | PASS | `make lint` — clippy `-D warnings` + fmt · `r3-lint.log` |
| Process hygiene | PASS | §10 — 22 daemons started, 22 confirmed gone from `/proc` |

---

## 4. The dose-response attack, re-run at `76fc6e8`

Driver `.shux/out/multipane-image-clip/budget/attack_r3.sh`. Unlike the previous
`attack.sh` it **exits 90** if a daemon survives, so a leak cannot be silently
absorbed by a teardown `|| true`.

Scene: two panes at 200x60. Attacker holds four 4096x4096 kitty PNG placements —
75 KB each on the wire, 64 MiB each charged from the declared `s=`/`v=`, so 4 ×
64 MiB is exactly the budget. Victim holds one ordinary 180x95 magenta picture.
Every pane also prints a truecolor + indexed + basic colour probe.

| Arrangement | `pane.snapshot` victim | `window.snapshot` | `session.snapshot` |
|---|---|---|---|
| `c836d8a`, victim in the pane that composes second | 17100 px | **0** | **0** |
| `76fc6e8`, victim second (`dose4`, `trunc5`) | 17100 px | **17100 px** | **17100 px** |
| `76fc6e8`, victim first (`swap5`) | 17100 px | **17100 px** | **17100 px** |

The P1 is closed in both orders and on all three snapshot paths.

### 4.1 A scene that proves the budget still fires

Survival alone would also be explained by a budget that never refuses anything.
`budget/gen_r3.py` therefore builds five 4096x4096 placements in **distinct
colours**, drawn in order, so the visible colour names how many were drawn:

- 256 MiB ÷ 64 MiB = 4, so #4 (yellow) must be the last drawn and #5 (cyan) refused.
- `pane.snapshot(attacker)` → yellow, 328320 px. Cyan absent.
- `window.snapshot` attacker region → yellow, 328320 px. Cyan absent.
- Same truncation point on both paths, and the victim's 17100 px intact.

---

## 5. Pixel verification

All metrics from `.claude/automations/pixel_verify.py` at
`--max-pixel-diff-ratio 0 --max-mean-channel-delta 0`.

### 5.1 Screenshot / metric matrix

| Viewport | Scene | Actual (crop of composed frame) | Compared against | Metric | Status |
|---|---|---|---|---|---|
| 200x60 | budget truncates in attacker pane, victim second | `trunc5-cropL.png` | `trunc5-paneL.png` (`pane.snapshot`) | `pixel-r3-crosspath-attacker.json` | pass 0/328320 |
| 200x60 | same frame, victim pane | `trunc5-cropR.png` | `trunc5-paneR.png` | `pixel-r3-crosspath-victim.json` | pass 0/328320 |
| 200x60 | `session.snapshot`, victim pane | `trunc5-scropR.png` | `trunc5-paneR.png` | `pixel-r3-session-victim.json` | pass 0/328320 |
| 200x60 | panes swapped, victim first | `swap5-cropL.png` | `swap5-paneL.png` | `pixel-r3-swap-victim.json` | pass 0/328320 |
| 200x60 | panes swapped, attacker pane | `swap5-cropR.png` | `swap5-paneR.png` | `pixel-r3-swap-attacker.json` | pass 0/328320 |
| 200x60 | panes swapped, `session.snapshot` | `swap5-scropL.png` | `swap5-paneL.png` | `pixel-r3-swap-session-victim.json` | pass 0/328320 |
| 200x60 | attacker ZOOMED (rects collapse to one clip) | `scope-B-crop.png` | `scope-A-paneL.png` | `pixel-r3-zoom-attacker.json` | pass 0/328320 |
| 200x60 | victim ZOOMED | `scope-C-crop.png` | `scope-A-paneR.png` | `pixel-r3-zoom-victim.json` | pass 0/328320 |
| 200x60 | panes RESIZED to 60x20 (clip `min()`s with grid dims) | `scope-E-cropL.png` | `scope-E-paneL.png` | `pixel-r3-resize-attacker.json` | pass 0/205200 |
| 200x60 | same, victim pane | `scope-E-cropR.png` | `scope-E-paneR.png` | `pixel-r3-resize-victim.json` | pass 0/205200 |

No baseline PNG is tracked for these scenes, so per CLAUDE.md's evidence-storage
rule the metric JSONs stand alone and no screenshot is committed. Frames and diffs
live in `.shux/out/multipane-image-clip/budget/r3/`.

### 5.2 Committed metrics re-derived at HEAD

`runall.sh` + `metric_case.sh` re-run with the `76fc6e8` release binary across
80x24 / 120x40 / 200x60 × small/big/none. Nine of the 25 committed metrics were
re-derived from scratch and compared field by field on `status`,
`changed_pixels`, `pixel_diff_ratio`, `mean_rgba_channel_delta`, `size`,
`total_pixels` and both thresholds: **9/9 numerically identical**. The delta moves
no pixel in any scene where the budget does not fire.

### 5.3 The comparator is proven able to fail

Same `trunc5` scene, same 720x456 crop at the same origin, run against a
`c836d8a` binary **built by this audit** in its own worktree (`/tmp/shux-c836`,
`git worktree add`), not against an artifact handed to it:

```
window magenta = 0        pane magenta = 17100
pixel_verify exit = 1     status = fail     changed_pixels = 17100
mean_rgba_channel_delta = 6.23   pixel_diff_ratio = 0.0521
```

`NEGCTL-r3-prefix-crosspath.json`, kept in scratch because `.shux/qa/` metrics
must be exact 0/0 passes. The same comparator that reports 0 on ten scenes at
`76fc6e8` reports 17100 on the pre-fix build — it can both pass and fail, and the
defect is genuinely present at `c836d8a` and genuinely gone at `76fc6e8`.

---

## 6. Does the mitigation still hold?

Idle machine, 7 reps, minimum reported. `budget/timing_r3.sh`, `budget/quad_r3.sh`.

| Scene | `3d46d74` (no budget) | `c836d8a` (shared 256 MiB) | `76fc6e8` (per-pane) |
|---|---|---|---|
| 2 panes, 24 placements in one | 1132 ms | 526 ms | **537 ms** |
| 4 panes, ALL hostile (5 placements each) | 2950 ms | 629 ms | **2325 ms** |

The single-greedy-pane mitigation is intact: 1132 → 537 ms, and the ~2 % against
`c836d8a` is one extra decode (`76fc6e8` still draws the victim's picture, which
`c836d8a` had starved), not lookup overhead.

The four-pane number is finding **P2-1**. Cost is exactly linear in decoded bytes
— 20 / 4 / 16 decodes of 4096² respectively, and 2950/2325 = 1.27 ≈ 20/16 — so
per-pane scoping recovers only 21 % of the four-way-split stall that the
constant's own doc comment still cites as its motivation.

---

## 7. Mutation testing

Worktree `/tmp/shux-mut3`, detached at `76fc6e8`, own `CARGO_TARGET_DIR`. No file
under `/home/user/shux/crates` was modified by this audit.

| Mutation | Result | Detail |
|---|---|---|
| **M17** revert the per-clip lookup to one shared budget | **CAUGHT** | exit 100 — `victim second: the neighbouring pane's picture was starved by a greedy pane`. The other 3 tests in the file stayed green: the new test is the sole pin, and it fails for the right reason and on the order-dependent arm. |
| **M8** delete the `composite_composed` call in `snapshot.rs` | **CAUGHT** | `window.snapshot returned a frame with no picture (0 px); pane.snapshot has 10260`. The `3d46d74` P1 stays closed. |
| **M19** budget charges but never refuses (`None => *budget = 0`) | **SURVIVED** | 926 tests green across `shux` + `shux-raster`. Finding P2-2. |

Skipped, with reason: M0–M12 (clip geometry, `3d46d74`) and M16/M16b
(`c836d8a`) — the delta does not touch `blit`'s clip arithmetic, its visibility
tests or its charge point, and their scenes were re-verified unchanged by the 9
re-derived metrics in §5.2. M16b is superseded by M19, which is the same question
asked of the shipping code.

One M17 run was lost to `ENOSPC` on the build filesystem, which surfaced as a
`rustc` exit 101. That run was **retried after freeing space, not counted** — a
build that could not run is not a mutation that survived.

---

## 8. The constant's corrected justification

Re-derived independently, not taken from the comment:

```
1800 × 1350 × 4 = 9 720 000 B = 9.27 MiB
256 MiB ÷ 9.27 MiB = 27.6  →  "about 27"
4096 × 4096 × 4 = 64 MiB   →  4 placements per pane
```

`1800×1350` is the real declared size, from a real `kitten icat` of a 4000x3000
photo into a 200x55 pane: APC header `a=T,q=2,f=24,o=z,m=1,s=1800,v=1350` over
2349 chunks (`budget/icat-declared-size.json`). The "4 placements" figure is
confirmed empirically by §4.1 — yellow (#4) drawn, cyan (#5) refused. The doc and
the binary agree.

---

## 9. Visual inspection

Frames opened as images at native resolution, not asserted on by size.

| Frame | What was checked |
|---|---|
| `trunc5-window.png` 1800x1140 | Attacker's yellow stops exactly at the separator; victim's magenta inside the right pane; probe row renders `TRUECOLOR` green, `INDEXED` orange, `BASIC` blue — three distinct colour classes; borders and status bar unpainted; no tofu, ghost cells, bleed or clipping artefacts. |
| `c836trunc5-window.png` | The defect, visible: the victim's magenta is gone while its own shell output (`RIGHTMARK`, prompt, probe row) is still on screen — so the loss is the budget, not a capture race. |
| `swap5-window.png` | Mirror image. Victim's magenta on the left survives; attacker's yellow ends at x=1629 = 909+720, inside its pane. |
| `scope-D-80x24.png` 720x456 | Smallest breakpoint. Both pictures clipped to their panes at the separator and the bottom border; probes legible; status bar intact. |
| `scope-C-zoomR.png` | Victim zoomed: borders correctly hidden, status bar reads `Z … 1 pane`, attacker's picture correctly absent. |

---

## 10. Findings

No P0. No P1.

### P2-1 (new in this delta) — the constant's doc cites a harm it no longer bounds

`MAX_PANE_DECODE_BYTES`'s doc comment, rewritten by this commit, still leads with
*"a four-way split whose panes printed 4096x4096 PNGs took 3.3 s for one
`window snapshot` against 10 ms with no images — work a pane chooses for a caller
that did not."* Measured at `76fc6e8` that scene now costs 2325 ms against
2950 ms unmitigated: 21 %. The shared budget gave 629 ms.

This is not a correctness defect and the trade is disclosed in the commit message
("Bound is now panes × 256 MiB"). It is also **forced**: any frame-level cap
reintroduces exactly the cross-path disagreement this commit exists to remove,
because `pane.snapshot` renders one pane with a full budget and could not agree
with a window that rationed it. The invariant the code now actually delivers is a
good one — *composing N panes costs no more than snapshotting them individually*,
each capped at 256 MiB, killing the 16 GiB single-pane worst case.

The defect is that the comment carries a contract the code no longer honours, and
CLAUDE.md is explicit that comments carry contract. It should state what it bounds
(one pane, 64× down from 256 placements × 64 MiB) rather than cite a four-pane
measurement as the harm it prevents. One comment edit; no code change.

### P2-2 (carried from `c836d8a`, still open) — nothing pins the refusal

M19 makes the budget charge and never refuse. 926 tests pass. The starvation test
added by this commit asserts *survival*, which a budget that never fires satisfies
trivially, so it does not cover this. §4.1 shows the refusal working on the real
binary — but only this audit shows it. A future refactor can delete the bound and
every gate stays green. The §4.1 scene is directly usable as that test: five
distinct-colour 4096² placements, assert #4 drawn and #5 absent, on both paths.

### P2-3 (process) — the step-7 record does not name `76fc6e8`

`council-substitution.md` §7 documents the implementation-diff review for the
branch but was written at `c836d8a`. `dootsabha` is still not installed, so
CLAUDE.md's fallback (parallel adversarial agents on disjoint surfaces) applies to
this delta too, and no such record exists for it. Mitigating: the delta is a
minimal remedy to a defect this gate specified, and this gate independently
attacked it on four disjoint surfaces — scoping semantics, cross-path identity,
DoS timing, mutation coverage — reproducing every claim before believing it. A
gate is not a substitute for the pre-push review, so the substitution note must
name `76fc6e8` before push.

### P3-1 — `composite_composed` is `pub` and its budget key is caller-controlled

`compose` emits exactly one `CellRect` per pane rect, so `budgets` holds at most
one entry per pane and a pane cannot mint extra budgets. An external caller
passing many distinct clips for one logical pane would get one budget each. Not
reachable from any RPC; noted because the signature permits it and the commit's
own reasoning is that `pub` plus an unstated invariant is how the last defect
happened.

### P3-2 — two panes cannot collide on a clip key, and a degenerate rect cannot borrow one

`clip = (rect.x, rect.y, min(rect.w, grid.cols), min(rect.h, grid.rows))`. Layout
rects tile without overlap, so distinct panes have distinct origins and cannot
share a budget. A zero-size rect would collide on origin, but `blit` returns on
`ox >= x1` **before** the charge, so it spends nothing. Verified by reading
`composed.rs` and `blit`, and consistent with §5's zoom and resize metrics.

### P3-3 — the `Vec` lookup is not a measurable cost

`budgets.len()` ≤ pane count; the scan is a handful of 4-`usize` comparisons per
placement against a 64 MiB decode. At 24 placements `76fc6e8` runs 537 ms against
`c836d8a`'s 526 ms while doing one more decode. Unmeasurable.

### P3-4 — the per-clip lookup is untestable against its weaker sibling

Resetting the budget when the clip changes behaves identically under `compose`'s
grouping, so no test distinguishes it from the lookup. The stronger form is a
defensive choice, correctly argued in the comment; recorded so it is not mistaken
for a tested property.

---

## 11. Passed evidence

- `make test` 2253/2253, 2 skipped; `make lint` clippy + fmt clean.
- P1 closed: victim survives at 4 hostile placements in both pane orders, on
  `pane.snapshot`, `window.snapshot` and `session.snapshot`.
- Cross-path identity exact (0 changed px, 0 mean channel delta) in 10 scenes
  including budget truncation, both pane orders, zoom in both directions, pane
  resize, and the session path.
- The budget still refuses: #5 of five distinct-colour maximal placements is
  dropped, at the same point on both paths.
- Comparator falsifiable: 17100 changed px, `status: fail`, exit 1 on a pre-fix
  binary this audit built itself.
- M17 and M8 caught; the new test fails with its stated message when reverted.
- 9 committed metrics re-derived at HEAD, numerically identical.
- Colour probes (truecolor + indexed + basic) present in every capture and
  legible in every inspected frame.
- Constant's 9.27 MiB / ~27 arithmetic re-derived and empirically confirmed.

## 12. Residual risk

- P2-2 is the live one: the decode bound has no regression test, so it can be
  removed without any gate noticing.
- The `panes × 256 MiB` bound grows with pane count and there is no pane-count
  cap (`grep MAX_PANES` → none). Panes are user-created, so this is a
  self-inflicted cost, not an external attack surface — but a 20-pane window of
  image-heavy panes will make `window.snapshot` slow in a way no test measures.
- Timing figures are wall-clock on one machine. Ratios are stable and the linear
  bytes-decoded model predicts them exactly; absolute numbers are not portable.
- The five distinct-colour placements are a synthetic kitty stream. Real-TUI
  coverage for this branch is the committed `pixel-richtui-ab-*` metrics from the
  `3d46d74` audit (btop, lazygit, nvim, vicaya, vivecaka); this delta moves no
  pixel in those scenes, which §5.2 re-verifies for the breakpoint arms.

## 13. Cleanup and tree status

**The product source stayed frozen. The repository did not.**

`git status --porcelain` was empty before the first command of this audit and
showed no change under `crates/`, `Makefile`, `scripts/`, `Cargo.toml`,
`Cargo.lock` or `.config/` at any point. `git diff 76fc6e8 70b3883 -- crates/ …`
is empty. Every measurement in this report was taken from a binary built at
`76fc6e8` or from a detached worktree, so nothing here is contaminated. The
`c836d8a` regression — product source patched mid-audit — did **not** recur.

What did happen: at 19:05:28, while this gate was still writing its manifest, a
commit `70b3883` *"test(qa): VT solid-QA evidence for multi-pane inline images"*
landed on the branch containing this report and ten of its metric files —
authored by something other than this gate, and one file short of the evidence
set the gate was assembling. It touched **only** `.shux/qa/multipane-image-clip/`
paths and its content is byte-identical to what this gate had written, so it
changes no finding. It is recorded because the audit contract is that the gate
commits its own terminal verdict: a PASS artifact reaching the branch before the
gate has finished writing the manifest that `make check-vt-qa` validates is the
same shape of mistake as the last round, one directory over. The manifest and
this correction are committed by the gate.

- 22 daemon-backed sessions across 6 drivers. Every driver reads the pid from
  `$XDG_RUNTIME_DIR/shux/shux.pid` **before** `daemon stop` and polls `/proc/<pid>`
  after; `attack_r3.sh`, `timing_r3.sh` and `quad_r3.sh` exit 90 on a survivor.
- 22/22 daemons confirmed gone (`budget/r3/hygiene.txt`, `daemon-hygiene.txt`).
- No leftover runtime dirs (`/tmp/sxa.* /tmp/sxt.* /tmp/sxq.* /tmp/sxs.*` → none).
- `ps -eo pid,args | grep '[s]hux'` → no processes.
- Worktrees `/tmp/shux-c836` and `/tmp/shux-mut3` are read-only scratch, both
  restored to a clean `git status` before teardown.
- Audit-only: no file under `crates/` was modified. The only tracked files this
  audit writes are in `.shux/qa/multipane-image-clip/`.
