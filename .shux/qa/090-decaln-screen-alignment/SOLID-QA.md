VERDICT: PASS

# 090 — DECALN screen-alignment pattern (issue #117) — VT QA record

**Task:** `docs/tasks/090-decaln-screen-alignment.md`
**Commit:** the `fix(vt): DECALN (ESC # 8) was parsed and dropped (#117)` commit on
`claude/shux-issue-117-fix-phh1vs`. **Base:** `6866fec` (v0.46.7).
**Gate:** `shux-vt-solid-qa`, run as a review step. See "Scope of the committed
subset" below for what is and is not in this directory, and why.

## Outcome

The gate's first pass returned **FAIL**. It did not dispute the DECALN change —
it found that the task shipped with no tracked QA record and that two pieces of
shared verification machinery were themselves defective. Everything actionable
it raised is fixed in the same branch:

| Gate finding | Disposition |
|---|---|
| P1-3 `pixel_verify.py` wrote fully transparent diff PNGs | **Fixed.** Reproduced (alpha 0/0 across the whole image while RGB carried 34,778 changed pixels), fixed by diffing on RGB so the saved PNG is opaque, and proven both directions: a real difference now renders 34,778 visible pixels, an identical pair renders 0. |
| P1-1 no tracked QA evidence for the task | **Fixed** — this directory. |
| P1-2 the task file omits `**Quality Gate:** shux-vt-solid-qa`, so `check-progress` skips the evidence check | **Recorded, not silently adopted.** See "Scope of the committed subset". |
| P2-4 no pixel verification had been run for the task | **Fixed** — two exact-threshold metrics in this directory. |
| P2-5 "pane grid clamps at 50 rows while the PTY reports 60" | **Not reproduced — the diagnosis was wrong.** See below. |
| P3-7 tab stops and window title had no regression test | **Fixed** — `decaln_leaves_tab_stops_alone`, `decaln_leaves_the_window_title_alone`, each proven able to fail. |
| P3-8 harness assertions only counted when `LABEL=after` | **Fixed** — assertions always count; recording a known-broken baseline is now `EXPECT_DEFECT=1`, which fails if the defect does *not* reproduce. |
| P3-6 no DootSabha council artefacts | **N/A in this environment** — substituted, see manifest. |

## P2-5 did not reproduce; it exposed a different, real defect

The gate reported that a pane asked for 60 rows kept a 50-row grid and silently
lost the top rows. Re-driven against the same binary:

```
requested rows=24 40 50 55 60  ->  pane glance returns 24 40 50 55 60 rows
stty size inside a 60-row pane ->  "60 60"
ESC[60;1H writes                ->  land on grid row 60 (verified by content)
```

The grid is the size it was asked for; nothing is lost. What the gate measured
was `pane capture`, whose CLI default is `--lines 50` — documented in
`pane capture --help`. On any pane taller than 50 rows the default returns the
last 50 rows, which reads exactly like a grid that dropped its top.

That is still a real defect **in this task's evidence harness**, which used the
defaulted capture: `.shux/scripts/issue_117_evidence.sh` now passes
`--lines "${rows}"`. The 200x60 breakpoint the gate reported as unusable runs
clean: 10/10 assertions, captured line count 60 = pane height.

## Testing matrix

| Layer | Result |
|---|---|
| Unit (grid) | `-p shux-vt --lib fill_alignment_pattern` — 8 passed |
| Integration (VT) | `-p shux-vt --test decaln` — 42 passed (41 cases + proptest) |
| Property | 256 random programs over a 30-sequence alphabet, chunked at random read boundaries — passed |
| Adversarial (existing) | `cow_aliasing_adversarial` — 22 passed; DECALN removed from that file's vacuity list, so its freeze assertion is no longer vacuous |
| End-to-end | `-p shux --test decaln_pane_e2e` — 4 passed, real daemon + real PTY, colour-probed |
| Visual A/B | 5 scenes x {before, after}, shot through shux's own rasterizer, every PNG opened and inspected |
| Geometry | 48x14 and 200x60 — 10/10 assertions each |
| Pixel | 2 exact-threshold (0/0) metrics, both `"status": "pass"` — see below |
| Mutation | **12 mutations of the shipped fix, 12 killed**, each by a named test |
| `make check` | green |
| Leaks | zero; daemons identified by pidfile, never by command-line match |

### Mutations, and the test that killed each

| # | Mutation | Killed by |
|---|---|---|
| 1 | drop `bump_mutations` | `decaln_advances_the_grid_write_tally` + both alternate-screen recycle tests |
| 2 | drop `wrapped = false` | `decaln_clears_soft_wrap_flags_so_reflow_does_not_join_rows` |
| 3 | drop the margin reset | `decaln_resets_the_margins_to_the_whole_page`, `scrolling_after_decaln_uses_the_full_page` |
| 4 | drop the cursor home | 5 cursor tests |
| 5 | `cell.ch = 'E'` instead of a whole cell | wide-continuation + extended-attribute tests |
| 6 | fill scrollback too | `decaln_does_not_touch_scrollback` |
| 7 | clip the fill to the scroll region | 3 region tests |
| 8 | bypass the synchronized-output freeze | `decaln_inside_a_sync_window_does_not_disturb_the_frozen_frame` |
| 9 | drop the dirty mark | `decaln_marks_the_whole_viewport_dirty` |
| 10 | fill with the current SGR pen | `decaln_ignores_the_current_sgr_state` |
| 11 | also reset tab stops | `decaln_leaves_tab_stops_alone` |
| 12 | also clear the window title | `decaln_leaves_the_window_title_alone` |

Mutations 5 and 10 initially survived: the tests that should have caught them
were asserting against a screen that was already blank and unstyled. Both were
strengthened to draw first, then re-checked against the mutation.

## Pixel metrics (exact, 0/0 thresholds)

| Metric | File | Result |
|---|---|---|
| `pane.snapshot` determinism — two consecutive snapshots of a still pane | `pixel-snapshot-determinism.json` | pass, 0 changed pixels of 349,920 |
| Cross-path — `pane.glance` PNG vs `pane.snapshot` PNG of the same frame | `pixel-glance-vs-snapshot.json` | pass, 0 changed pixels of 349,920 |

Text cross-path was checked in the same run: `pane capture --lines 24` is
byte-identical to `pane glance --text-only` after trailing-space normalisation.

Scene under both metrics: an 80x24 pane that emits a colour probe, sets a scroll
region rows 4-9, selects bright red on blue, runs DECALN, then writes `HOME`.
The rendered frame shows `HOME` in the application's red-on-blue at the top-left
over a page of default-attribute `E` — the pattern is not clipped by the region,
not painted in the pen, and the cursor is home.

Colour probe sampled from the rendered pane, so a monochrome regression could
not pass as clean:

```
TRUECOLOR glyph fg (120, 220, 180)   exact SGR 38;2;120;220;180
INDEXED   glyph fg (255, 135, 0)     exact xterm-208
BASIC     glyph fg (36, 114, 200)    theme blue (SGR 34)
plain E   glyph fg (220, 220, 220)   default fg — the pen was not applied
```

## Scope of the committed subset

`.shux/qa/README.md` describes a committed subset that includes baseline PNGs and
a `**Quality Gate:** shux-vt-solid-qa` marker in the task file. Neither is
included here, deliberately and on the record:

* **No PNGs.** CLAUDE.md permits committing a PNG "only as a true baseline/golden
  with task documentation + DootSabha approval". DootSabha is unavailable in this
  environment, so that approval cannot exist, and the repo's own PR checklist
  says "no screenshots committed unless justified as durable baselines". These
  shots are A/B evidence for one fix, not durable baselines. They live in
  gitignored `.shux/out/issue-117/` and are published for review as a Claude
  Artifact instead.
* **No `Quality Gate:` marker.** `scripts/check-progress.sh` keys its evidence
  check off that marker and then hard-requires a committed `*-actual.png`.
  Adding the marker without the PNG would fail the pre-push hook; adding both
  would commit a screenshot this task is not entitled to commit. The established
  precedent for M3 issue-fix tasks is to run the gate as a review step without
  the committed baseline subset — tasks 087, 088 and 089 (the closest analogue,
  a shux-vt parser + grid fix) carry no `.shux/qa/` directory at all.

**This is a real hole and it is worth its own task**, independent of #117: an M3
task can touch `shux-vt`, capture, cursor, alt screen and scroll regions — every
row of CLAUDE.md's VT gate table — and `make check-progress` will not ask for any
evidence, because enforcement keys off a marker no M3 task carries. The gate
found it; recording it here is not the same as fixing it.

## Residual risk

* `vim` and `less` are the only rich TUIs installed on this host. `nvim`, `btop`,
  `htop`, `lazygit`, `vicaya` and `vivecaka` are **not** installed — stated
  rather than skipped silently. The task DoD names vim, which passed at 48x14,
  80x24 and 120x40.
* Roughly 50 diff PNGs committed under `.shux/qa/` by tasks 067-086 were produced
  by the defective `pixel_verify.py`. Every one whose `-actual`/`-expected` pair
  is also committed (19 of them) was regenerated with the fixed tool and its
  metrics reproduced exactly: **all 19 are genuine zero-difference cases**, so
  those artefacts were blank because the truth is blank, not because the tool ate
  the signal. They are left as-is rather than rewritten inside a DECALN change;
  the remaining 53 compare against goldens that are not committed and belong to
  their own tasks.
