# 090 — DECALN (`ESC # 8`) is parsed and dropped, so shux reports a blank screen where every other terminal reports a full one

**Status:** Done
**Priority:** High (conformance + a latent cross-pane content leak; issue #117)
**Milestone:** M3 polish
**Depends On:** 106 fix (`daab8a4`, alternate-screen buffer reuse), 089 (`27e5b82`,
synchronized-output lazy freeze) — the fill has to satisfy both
**Touches:** `crates/shux-vt/src/parser.rs`, `crates/shux-vt/src/grid.rs`,
`crates/shux-vt/src/cell.rs`, `crates/shux-vt/tests/decaln.rs` (new),
`crates/shux-vt/tests/cow_aliasing_adversarial.rs`,
`crates/shux/tests/decaln_pane_e2e.rs` (new),
`.shux/scripts/issue_117_evidence.sh` (new), `.shux/scripts/issue_117_shots.py` (new),
`.claude/automations/pixel_verify.py` (defect in shared verification machinery),
`.shux/qa/090-decaln-screen-alignment/` (new)
**QA record:** `.shux/qa/090-decaln-screen-alignment/SOLID-QA.md` — `VERDICT: PASS`

---

## Problem (issue #117)

`ESC # 8` — DECALN, the DEC screen-alignment test — fills the whole page with `E`.
shux parsed it and did nothing with it. `esc_dispatch` had an arm for `(b'8', [])`
(DECRC) and none for `(b'8', [b'#'])`, so the sequence fell through to:

```rust
_ => trace!(byte, intermediates = ?intermediates, "unhandled ESC sequence"),
```

Reproduced at `6866fec` (v0.46.7) through the real binary, not just the parser:

```
$ shux lens run -- sh -c "printf '\033#8'; sleep 60"
$ shux pane capture -s <session> -p <pane>
TRUECOLOR INDEXED BASIC
this pane is about to run the screen-alignment test
```

Every other terminal answers that with a screen of `E`.

### Why it matters

`ESC # 8` is the first thing a terminal conformance suite emits — `vttest` opens with
it — because it is the standard "fill the screen so I can see where the margins fall"
sequence. A terminal that ignores it makes every subsequent test in the run start from
a wrong baseline.

### The second, quieter half

A full-screen fill in shux is not just a loop over cells. Three grid invariants sit
under it, and one of them is a **cross-pane content leak**:

* **The write tally.** `Grid::is_blank_canvas` decides whether a retired
  alternate-screen buffer can be handed to the next application as-is (issue #106) by
  asking whether the buffer has ever been written to. A fill that draws cells without
  advancing that tally leaves a screen full of `E` parked in the spare slot — and the
  next application to enter the alternate screen in that pane inherits it. Different
  program, same pane, previous program's screen.
* **Copy-on-write row sharing.** A row still shared with a synchronized-output freeze
  or a `pane.snapshot` clone (issue #115) must be copied before the fill lands, or the
  fill rewrites a frame someone else is already reading.
* **Cell structure.** Wide-character pairs, extended attributes (hyperlinks, grapheme
  payloads) and the soft-wrap flag survive into capture and into resize reflow. A fill
  that assigns `cell.ch = 'E'` and nothing else leaves orphaned wide-continuation
  cells and rows that reflow back together on the next resize.

### Note from the issue, discharged

`sync_output_differential.rs` and `cow_aliasing_adversarial.rs` both already fed
`\x1b#8` as "a sequence that touches the grid". They were feeding a no-op.
`cow_aliasing_adversarial.rs` even listed `DECALN` in its own vacuity guard — the list
of hammer cases known to write nothing. That entry is removed: DECALN is now a real
full-screen write, and the guard fails if it ever stops being one.

## The fix

One match arm and one grid method.

```rust
// DECALN -- Screen Alignment Pattern (ESC # 8).
(b'8', [b'#']) => self.screen_alignment_pattern(),
```

```rust
fn screen_alignment_pattern(&mut self) {
    self.grid.fill_alignment_pattern();
    self.scroll_region.top = 0;
    self.scroll_region.bottom = self.grid.rows().saturating_sub(1);
    self.cursor.row = 0;
    self.cursor.col = 0;
    self.cursor.auto_wrap_pending = false;
}
```

VT510 §DECALN specifies three things beyond the fill, and each is a separate way to get
it wrong:

1. the pattern covers the **complete page** — the scroll region does not clip it (that
   is the point: the operator is looking at where the margins fall);
2. it **"sets the margins to the extremes of the page"**; and
3. it **"moves the cursor to the home position"**.

Margins are reset *before* the cursor is homed, so home means the top-left of the page
under origin mode too — not the top-left of whatever region the application had set.
xterm (`resetMargins` then `CursorSet(0,0)`) and kitty (`screen_alignment_display`) both
do the same.

The fill writes a whole `Cell::ALIGNMENT` per cell rather than assigning `ch`, which is
what drops extended payloads and wide-pair widths. It goes through `Row::cells_mut`,
which is the only way to mutate a row and therefore where copy-on-write unsharing
happens. `Grid::fill_alignment_pattern` mirrors `Grid::clear_visible` exactly: same row
range (viewport only, history untouched), one `bump_mutations`, one full-viewport dirty
mark.

DECALN draws a fixed test pattern, not text, so the current SGR pen is not applied to
it — and is not reset by it either. The next printable character still uses the pen the
application selected.

## Testing matrix

| Level | Where | What |
|---|---|---|
| Unit (grid) | `crates/shux-vt/src/grid.rs` | every visible cell written; scrollback untouched; wide pairs dissolved; extended attrs dropped; not a blank canvas afterwards; shared rows copied first; viewport dirtied; empty grid inert |
| Integration (VT) | `crates/shux-vt/tests/decaln.rs` | 41 cases across 11 groups — the fill, the scroll region, the cursor, attributes, grid invariants, change notification, alternate screen, synchronized output, the sequence space around `ESC # 8`, RIS/idempotence/resize, consumer-visible capture |
| Property | `crates/shux-vt/tests/decaln.rs::properties` | 256 random programs over a 30-sequence alphabet, chunked at random boundaries: after DECALN the page is the pattern, margins are the extremes, the cursor is home |
| Adversarial (existing) | `cow_aliasing_adversarial.rs` | DECALN promoted out of the vacuity list; the freeze assertion around it is now non-vacuous |
| End-to-end | `crates/shux/tests/decaln_pane_e2e.rs` | real daemon, real PTY, real shell, colour-probed: the fill through `pane capture`/`pane glance`; region + pen + homed cursor in one pane; the alternate-screen recycle; the primary screen across an alternate-screen round trip |
| Visual | `.shux/scripts/issue_117_evidence.sh` | five scenes shot through shux's own rasterizer, before and after, with per-scene assertions |

### Every guard was proven able to fail

Twelve mutations applied to the fix in turn, each re-running the suite:

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

Mutations 5 and 10 initially SURVIVED: the tests that should have caught them were
asserting against a screen that was already blank and unstyled, so the mutated code
produced the same result as the correct code. Both tests were strengthened to draw
first. A test that has only ever been seen passing is not evidence.

## Acceptance criteria

- [x] `ESC # 8` fills the visible page with default-attribute `E`, on either screen.
- [x] The scroll region does not clip the fill; the margins are reset to the page.
- [x] The cursor is homed to the page origin, with any pending auto-wrap cleared.
- [x] The current SGR pen is neither applied to the pattern nor reset by it.
- [x] Scrollback, the DECSC save slot, tab stops, cursor visibility/shape, the window
      title and the dynamic default colours are untouched.
- [x] No orphan wide-continuation cells, no extended payloads, no soft-wrap flags left.
- [x] The fill registers as a content mutation: viewport dirtied, `ContentRevision`
      advanced, write tally advanced (so a retired alternate buffer is never recycled
      as blank).
- [x] A synchronized-output freeze and a held grid clone both survive the fill intact.
- [x] `ESC 8` is still DECRC; no other `ESC #` sequence fills the screen.
- [x] Rich TUIs (vim) repaint over the pattern with no residue.
- [x] `make check` green; zero leaked daemons.

---

## QA gate outcome (`shux-vt-solid-qa`)

The gate's first pass returned **FAIL**. It did not dispute the DECALN change; it
found that the task shipped with no tracked QA record and that two pieces of shared
verification machinery were themselves defective. Full disposition table lives in
`.shux/qa/090-decaln-screen-alignment/SOLID-QA.md`. The load-bearing items:

**Fixed — `pixel_verify.py` wrote fully transparent diff PNGs.** Both inputs are
opaque, so the alpha band of `ImageChops.difference` is 0 everywhere and `point`
mapped it to 0. Every diff image the tool has ever produced rendered blank in any
viewer, so the gate clause "the diff image reveals obvious defects even if the
numeric threshold is permissive" could not be exercised by any task. The numeric
metrics were always correct. Reproduced (alpha 0/0 while RGB carried 34,778 changed
pixels), fixed by diffing on RGB, and proven both directions. This is a defect in
verification machinery, so it is fixed here rather than filed.

Of the ~50 diff PNGs committed under `.shux/qa/` by tasks 067-086, the 19 whose
`-actual`/`-expected` pair is also committed were regenerated with the fixed tool
and their metrics reproduced exactly: **all 19 are genuine zero-difference cases**,
so those artefacts were blank because the truth is blank. They are left as-is rather
than rewritten inside a DECALN change; the other 53 compare against uncommitted
goldens and belong to their own tasks.

**Fixed — the evidence harness truncated its own captures.** `pane capture` defaults
to `--lines 50`; on a pane taller than that it silently returns the last 50 rows.
The harness now passes `--lines "${rows}"`. The gate read this as a 50-row grid clamp
losing content — that diagnosis did **not** reproduce: a 60-row pane keeps a 60-row
grid, `stty size` reports 60, and `ESC[60;1H` lands on row 60. The 200x60 breakpoint
the gate reported as unusable now runs 10/10 with a captured line count equal to the
pane height.

**Fixed — the harness masked its own failures.** Assertions only incremented the
failure counter when `LABEL` was literally `after`, so any other label printed FAIL
lines and still exited 0. Assertions now always count, and recording a known-broken
baseline is an explicit `EXPECT_DEFECT=1` which fails if the defect does *not*
reproduce. All four combinations were exercised and each returned the expected exit
code.

**Fixed — two DoD clauses had no regression test.** Tab stops and the window title
were verified behaviourally but nothing would have caught a future regression;
`decaln_leaves_tab_stops_alone` and `decaln_leaves_the_window_title_alone` now do,
and both were proven able to fail.

## Explicitly re-scoped

**The committed QA subset omits baseline PNGs and the `**Quality Gate:**` marker.**
CLAUDE.md permits committing a PNG "only as a true baseline/golden with task
documentation + DootSabha approval"; DootSabha is unavailable in this environment, so
that approval cannot exist, and the PR checklist requires "no screenshots committed
unless justified as durable baselines". `scripts/check-progress.sh` keys its evidence
check off the `Quality Gate:` marker and then hard-requires a committed `*-actual.png`
— so adding the marker without a PNG fails the pre-push hook, and adding both commits
a screenshot this task is not entitled to commit. Tasks 087, 088 and 089 (the closest
analogue: a shux-vt parser + grid fix for an issue) carry no `.shux/qa/` directory at
all; this task commits the report, manifest and pixel metrics, and publishes the
images as a Claude Artifact.

**Follow-up worth its own task, found by the gate:** an M3 task can touch `shux-vt`,
capture, cursor, alt screen and scroll regions — every row of CLAUDE.md's VT gate
table — and `make check-progress` will not ask for any evidence, because enforcement
keys off a marker no M3 task carries. Enforcement should follow the touched surface,
not a hand-written field.

**DootSabha councils (feature protocol steps 1 and 6) are N/A in this environment**
and were substituted, on the operator's instruction, with five parallel agents that
drive the real binary: four adversarial reviewers on disjoint surfaces plus the
`shux-vt-solid-qa` gate agent.
