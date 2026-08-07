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
`.shux/scripts/issue_117_richtui_check.sh` (new),
`.claude/automations/pixel_verify.py` (defect in shared verification machinery),
`.shux/qa/090-decaln-screen-alignment/` (new)
**QA record:** `.shux/qa/090-decaln-screen-alignment/SOLID-QA.md` — `VERDICT: PASS`


## Follow-up (2026-08-07, during task 091)

`decaln_pane_e2e`'s `wait_for` deadline was raised from 20s to 60s. It timed
out in the `Coverage (llvm-cov)` job on PR #128 while the SAME test on the SAME
commit passed in `Test (ubuntu)` and `macOS (test + smoke)`, and runs in ~3s
locally. The coverage job is the only one that runs the workspace instrumented
(`cargo llvm-cov nextest --workspace`) and in parallel, which is several times
slower than the serial uninstrumented suites everywhere else.

The budget is a deadline for *reporting* failure, not an assertion: every
content check runs immediately after it, so a broken DECALN still fails
loudly and a longer wait cannot mask a defect. Hardening the test is also what
`.config/nextest.toml` says to prefer over widening the retry override.

Verified not attributable to #120's changes before touching this: 60 `pane
capture` calls with a full uuid took 815ms on `e856793` and 838ms on the fix
(~2.8%, within noise), and the failure is on a VT path that branch does not
touch.
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

## Fixed after adversarial review: `ESC[?47h` never entered the alternate screen

Private mode `47` — the ORIGINAL xterm "use alternate screen buffer", still
emitted by anything built against pre-1049 terminfo (the old termcap `ti`/`te`
pair) — was not implemented. `set_private_mode` matched `1047 | 1049` only, so
`?47h` fell through unhandled and a program that asked for the alternate screen
the old way was drawing on the PRIMARY one, with `?47l` restoring nothing.

The gap predates this change: plain text under `?47` corrupted the primary too,
verified with a control arm containing no DECALN. But DECALN is what turns it
from "an application overwrote part of your screen" into "the whole page is
gone and there is nothing to restore" — so it is fixed here.

```
SECRET-LINE / SECOND-LINE   on the primary screen
ESC[?47h                    (before) is_alternate_screen == false
ESC#8                       fills the PRIMARY page
ESC[?47l                    restores nothing
                            -> SECRET-LINE is gone, permanently
```

`47` now joins the `1047 | 1049` arm and behaves as `1047` does — the cursor is
carried across rather than parked, because only `1049` saves and restores one —
and `DECRQM ?47` reports the mode's real state instead of "not recognized".

**This test suite did not catch it, and that is the more useful finding.**
`no_alternate_screen_mode_combination_recycles_the_pattern` drove `?47` in its
matrix but asserted only that the NEXT application got a blank screen — which is
true even when the fill never reached an alternate screen at all, because
entering `?1049` always yields a fresh one. It passed while `?47` destroyed the
primary. The test now also asserts the primary survived the round trip, and
reverting the `47` arm fails four tests including that one.

### A note for whoever screenshots a TUI after a DECALN

`less` legitimately shows `E` residue after repainting over the pattern, and it
is not a shux defect. Adversarial review recorded the raw PTY bytes both ways:
`less` emits a byte-identical repaint stream whether or not the pattern is
there, with no per-line erase and no `ED` — it relies on being at the bottom row
so each newline scrolls a fresh blank line in. DECALN homes the cursor, so
nothing scrolls and the columns past the text keep their `E`s. Any conforming
terminal does the same. shux's erase primitives after a fill were checked
separately and are correct.

## Found in passing, pre-existing, NOT fixed here

Both were surfaced by the adversarial pass and both were reproduced with control
arms that contain no DECALN at all, so neither is in this change's blast radius.
Recorded with reproductions rather than fixed, because each is a behavioural
change to a different sequence and would need its own design and test surface.

### The scroll region is global terminal state, not per-screen

```
ESC[3;6r            primary margins -> rows 3..6
ESC[?1049h          enter the alternate screen
ESC[5;7r            the alternate screen sets its own margins
ESC[?1049l          leave
                    -> primary margins are now 5..7, not 3..6
```

No DECALN anywhere. `?1047`/`?1049` save and restore the CURSOR (that is what
the mode number means) and shux keeps `ScrollRegion` outside that, so a
full-screen application's margins outlive it. DECALN's own margin reset rides
the same channel — a DECALN on the alternate screen leaves the primary at the
extremes of the page — but it is not the cause.

Whether this is a defect depends on a reference terminal, which is not
available in this environment: DECSC/DECRC (which is what `?1049` restores)
carry cursor position, attributes, charsets, origin mode and the wrap flag —
not margins — so "margins are global" may well be correct. It should be settled
against a real xterm before anything is changed. Owner: alternate-screen state,
not the VT fill.

### REP (`CSI b`) derives its source from the screen, not the data stream

```
X                   print a graphic character
ESC[1;1H            home the cursor
ESC[3b              REP 3  ->  no-op
X ESC[3b            (no cursor move)  ->  "XXXX", works
```

`repeat_preceding_char` reads the cell to the LEFT of the cursor, so at column 0
`checked_sub(1)` returns `None` and the repeat is dropped. ECMA-48 defines REP
as repeating "the preceding character in the data stream", which would survive a
cursor move. DECALN only surfaces this by homing the cursor; the divergence is
in REP and predates this change. Fixing it means carrying a `last_graphic` cell
in parser state and deciding what invalidates it — a change to REP's semantics
that does not belong in a DECALN fix.

## Review round (PR #119, Codex bot) — two P2s, both real, both fixed

**Alpha-only differences were still invisible in the diff image.** The P1-3 fix
converted both inputs to RGB before amplifying, which cured the transparent-PNG
bug but discarded the one channel that carries an alpha-only difference — the
RGBA metrics counted those pixels while the picture beside them stayed blank. A
narrower blind spot, but the same class of defect. The mask is now derived from
`diff_arr`, the SAME array the metrics come from, so the picture and the numbers
cannot disagree: any channel differing lights the pixel, and the result is saved
opaque. Verified on three fixtures — identical (0/0), RGB-only (28/28) and
alpha-only (68/68) — and on the real #117 A/B (34,778/34,778).

**Mixing the alternate-screen aliases lost the parked cursor.** `?1049h` fills
the DECSC save slot and then parks the primary cursor for the screen swap; the
park used `mem::take`, which swallowed the save slot whole and left a default
cursor behind, so the slot became unreachable. `?1047l` and `?47l` drop the
stash by design — they have no cursor of their own to hand back — and took the
save slot with it. An application opening the alternate screen with `?1049h` and
closing it with either alias lost its cursor entirely.

Pre-existing: `?1049h` + `?1047l` lost it identically, and `?1047l` shipped long
before `?47`. Adding the alias gave the hazard one more spelling, which is how
review found it. `ScreenSwap::enter` now carries the save slot across the swap,
keeping the two mechanisms independent — the stash restores the screen's cursor,
DECSC restores the terminal's, neither can eat the other. The deliberate
`mode_1047_does_not_restore_primary_cursor_on_leave` behaviour is unchanged.
Reverting the one line fails `mixing_alternate_screen_aliases_keeps_the_parked_cursor`.

## Rich-TUI matrix, completed

CLAUDE.md makes `vim`/`nvim`, `lazygit` and `btop`/`htop` a REQUIRED pass for any
change to VT parsing. The first pass of this task reported four of them as "not
installed on this host" and moved on. That was wrong — they install fine
(`apt-get install neovim htop btop`, and lazygit from its release tarball), and
"the tool is missing" is not a reason to skip a required check when the tool can
be fetched. `.shux/scripts/issue_117_richtui_check.sh` now runs the whole matrix.

Each TUI is started in a pane whose page has just been filled edge to edge with
the alignment pattern, and must repaint over it with no residue. All six pass at
100x30, every screenshot opened and inspected:

| TUI | result |
|---|---|
| `vim` | clean repaint |
| `nvim` | clean repaint |
| `htop` | clean repaint, colour meters intact |
| `btop` | clean repaint, box-drawing + colour meters intact |
| `lazygit` | clean repaint, panels + colour + hyperlinks intact |
| `less` | clean repaint |

Most of these take the ALTERNATE screen, which makes the check sharper than it
looks: the buffer they are handed comes out of the one-slot spare, so a
pattern-filled buffer wrongly recycled as blank would show straight through
their UI. That is the #106 interaction this task's fill had to get right.

**Two harness bugs found on the way, both mine, neither a shux defect.**
`btop` refuses to start without a UTF-8 locale and `lazygit` exits before
drawing; the container sets no `LANG`, and the panes inherited that. Locale is
named in CLAUDE.md's pane-env list alongside `TERM` and `COLORTERM` precisely
because of this, so the harness sets `LANG=C.utf8`. Second, the script built its
exec line with `printf 'exec %s' "$*"`, so passing `sh -c "cd DIR && exec
lazygit"` produced `exec sh -c cd DIR && exec lazygit` — the first `exec`
replaced the shell and lazygit never ran. The harness now emits `cd` as its own
line, and asserts the TUI's marker is on the FINAL screen rather than merely
having flashed past, which is what caught the second bug instead of passing it.
