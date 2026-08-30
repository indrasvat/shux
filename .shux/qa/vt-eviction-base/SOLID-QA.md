VERDICT: PASS

# SOLID VT QA — `vt-eviction-base` (re-audit at `c64efb1`)

| | |
|---|---|
| PR | [#182](https://github.com/indrasvat/shux/pull/182) |
| Branch | `claude/shux-image-support-s0u4uy` |
| Frozen HEAD | `c64efb1` |
| Base | `9793961` (`git merge-base origin/main HEAD`) |
| Diff identity (crates only) | `sha256(git diff 9793961..c64efb1 -- crates/)` = `31eb6fc2da6bc56c786ab62b304764c56ba1fd29675ecf17c8232ef5b72ab173` |
| Diff identity (full) | `sha256(git diff 9793961..c64efb1)` = `c18ad6d8cc6a80cf05113adf479247bc93f65d46dfc2bee090f8877775c0c2dd` |
| `crates/` tree | `65646bf46c8452aa4a0180940ec0b2665e040cf0` |
| Range | `f01c954` · `6172c24` · `15fd835` · `fd3e1c8` · `e114e4c` · `028754f` · `c64efb1` |
| Council tool | `dootsabha` NOT installed; substitution recorded in `council-substitution.md` |

**Lag disclosure, stated so the next reader is not misled the way Greptile was.**
Committing this evidence necessarily moves `HEAD`. The only commit after `c64efb1`
on this branch is the one that adds this folder. Nothing under `crates/` moves in
it; `git diff c64efb1..HEAD -- crates/` is empty. That is the house pattern (see
`.shux/qa/kitty-graphics-control-parse/`).

## 0. Why this re-audit exists

A previous run of this gate returned PASS frozen at `e114e4c`. Greptile raised a
P2 on PR #182: the committed records named `e114e4c` while the PR ended at
`028754f`, so nobody could establish that the PASS covered the final contents.
Greptile was right, and **understated**: `.shux/qa/vt-eviction-base/SOLID-QA.md`
and `evidence-manifest.json` do not exist at `e114e4c` at all. They were
introduced *by* `028754f` — the same commit that changed the rustdoc they claim
to have audited. `git log --follow` on both files returns exactly one commit,
`028754f`. So the previous PASS was not merely stale by one commit; it was
committed alongside code changes it had never seen.

Two further commits then landed. This record replaces the previous one and names
the SHA it actually measured.

## 1. The comment-only claim, verified mechanically

The implementer's claim was that `e114e4c..028754f` changes no non-comment line.
Not taken on assertion. Two independent constructions:

1. **Diff-line classification.** `git diff --unified=0 e114e4c..028754f -- crates/`
   yields 24 changed lines; **0** survive `grep -vE '^[+-][[:space:]]*(///|//)'`.
2. **Whole-file reconstruction** (a different construction, not a re-run of the
   first). Strip every `^\s*///` line from both revisions of `grid.rs` and
   `lib.rs` and diff the results: **byte-identical** for both files.

Neither doc block contains a code fence, so no doctest changed, and the diff adds
no `#[doc]` attribute. **The claim holds for `crates/`.** It does not hold for the
commit: `028754f` also adds 25 evidence files (§0).

For the range this audit actually covers, `028754f..c64efb1`, the only non-test,
non-comment line in `crates/` is the body of `eviction_base()`:

```
-        self.grid().evicted()
+        self.grid.evicted()
```

`eviction_base` is `#[doc(hidden)]` and has exactly one caller in the repository,
`crates/shux-vt/tests/alt_screen_differential.rs:102`. Everything else in that
range is inside `mod tests` or is a comment. **No production render, capture or
raster code differs between `e114e4c`, `028754f` and `c64efb1`.** Pixel evidence
was regenerated at `c64efb1` anyway (§5) rather than re-stamped.

## 2. Acceptance criteria

| # | Criterion | Verdict | Evidence |
|---|---|---|---|
| A1 | `f01c954`: a viewport-only clone advances the base by the history it drops | PASS | `a_viewport_clone_advances_the_eviction_count_by_the_history_it_drops`; reverting the two clone lines reds it |
| A2 | `15fd835`: `reset_blank` returns the base to a fresh grid's | PASS | reverting `self.evicted = 0` reds 3 lib tests **and** the differential (§4.1) |
| A3 | `15fd835`: `is_blank_canvas` may be left unchanged | PASS (conclusion) / **F1** (stated reason) | 10,000-sequence search, 1,220 admitted, 0 violations (§4.2) |
| A4 | `fd3e1c8`: the recycling oracle can see the base, and bites | PASS | diverges at step 4 on `ESC[?1049h`, `eviction_base` 1 vs 0, every other field equal |
| A5 | `028754f`: `eviction_base()`'s rustdoc is true | SUPERSEDED by `c64efb1` | the "presented" wording was replaced; see A6 |
| A6 | `c64efb1`: `eviction_base()` returns the LIVE base, and that is the right choice | PASS | §3 |
| A7 | `c64efb1`: `GridSnapshot` gains `evicted`; the pre-existing test discriminates | PASS | §4.1 — `reset_blank_is_indistinguishable_from_a_fresh_grid` reds on the unfixed tree |
| A8 | `c64efb1`: the recycling test's non-vacuity guard is honest | PASS / **F3** | census: 3 of 4 candidates admitted, the load-bearing one among them (§4.3) |
| A9 | The whole range is a visual no-op on every render path | PASS | 27/27 byte-identical daemon artifacts; 18 committed pixel metrics at exact `0`/`0` |
| A10 | Council evidence exists for the change | PASS | `council-substitution.md` for `f01c954..fd3e1c8`; `028754f` and `c64efb1` carry theirs in their commit messages, which is where CLAUDE.md puts the durable record |

## 3. `eviction_base()` reads the LIVE grid — attacked, and correct

`c64efb1` reverses the doc half of the previous run's P3. The previous run said the
doc claimed "live" while the function returned `grid()`; `028754f` fixed the doc to
say "presented"; `c64efb1` decided that was the wrong half and made the function
live. All four claims in the new rustdoc were checked against the source:

- **"Absolute index the LIVE grid is numbering its rows from."** `self.grid.evicted()`
  reads the field directly, bypassing `grid()`'s frozen preference. TRUE.
- **"Exists so `tests/alt_screen_differential.rs` can compare a `pub(crate)` field."**
  Repo-wide grep: that file is the only caller. TRUE.
- **"a frozen viewport clone counts from its own base, which is not the base
  `presented_row` resolves against."** TRUE, and this is the load-bearing one.
  `sync.rs` documents `FrozenScreen::evicted` as "the live grid's eviction counter
  at freeze time" and sets it from `self.evicted()`, while `FrozenScreen::grid` is
  `clone_presented_viewport()`, whose base `f01c954` sets to `live + sb`.
  `presented_history_len()` resolves against `frozen.evicted`, **not** against
  `frozen.grid.evicted()`. So the viewport clone's own base is a coordinate space
  no consumer reads.
- **"the recycling defect this observes lands on the live grid at alternate-screen
  entry."** TRUE, demonstrated in §4.1.

A probe run against the *previous* (`grid()`-based) implementation confirmed the
old behaviour too: with a `?2026h` window open the accessor pinned at 9 while the
live grid ran on to 33. Both implementations are sound for the oracle — both arms
read the same accessor — but the live read is the one whose coordinate space
something else uses, and it is strictly more sensitive: a presented read masks a
live-base divergence behind the frozen frame for the duration of a window.

## 4. The claims that needed real work

### 4.1 The fixes are covered, and the coverage was seen red

`c64efb1` deletes `reset_blank_returns_the_eviction_base_to_a_fresh_grids`, the
unit test `15fd835` shipped. Deleting a regression test is exactly the shape that
silently drops coverage, so it was measured, not argued. With
`reset_blank`'s `self.evicted = 0` removed at `c64efb1` and nothing else changed,
`cargo test -p shux-vt` reds **four** tests:

```
grid::tests::a_grid_that_looks_blank_to_the_recycling_check_counts_from_zero  FAILED
grid::tests::a_recycled_alternate_screen_buffer_does_not_alias_the_previous_session  FAILED
grid::tests::reset_blank_is_indistinguishable_from_a_fresh_grid  FAILED
alt_screen_differential::recycling_a_retired_buffer_is_unobservable  FAILED
```

and the differential fails for the right reason — not merely "something broke":

```
left:  Observed { frame: "295380c2…", scrollback: [], scrollback_len: 0, total_lines: 1,
                  rows: 1, cols: 1, content_revision: 6, title: None, alternate_screen: true,
                  dirty: [], cursor: (0, 0, true), eviction_base: 1 }
right: … identical …                                                    eviction_base: 0
state diverged at step 4 after Feed([27, 91, 63, 49, 48, 52, 57, 104])
```

`[27,91,63,49,48,52,57,104]` is `ESC [ ? 1 0 4 9 h`. Frame digest, scrollback,
line count, revision, title, alt flag, cursor and dirty regions all agree; the base
is the only difference. **The deletion consolidated coverage rather than losing
it**, and it confirms the `GridSnapshot.evicted` addition (A7) does discriminate.

### 4.2 No grid with a moved base reaches `is_blank_canvas`

Argument replaced with search. 10,000 depth-4 programs over
`{scroll_up, clear_scrollback, shrink rows, grow rows, narrow cols, widen cols,
fill, clear_visible, reset_blank, column reflow}` on a `max_scrollback: 3` grid:

| | |
|---|---|
| sequences | 10,000 |
| admitted by `is_blank_canvas` | 1,220 |
| admitted with `evicted() != 0` | **0** |

Proven able to fail before being trusted: the same probe, unchanged, against the
tree with `15fd835` reverted returns **48 violations**, every one of the shape
`[…, ResetBlank] evicted != 0 mutations == 0`. That is the defect, not a
manufactured red.

Static confirmation of the four in-place writers of `evicted`: `grid.rs` scroll
eviction (two sites, both after `scroll_up_n` has already added `n` to the write
tally), reflow-drop, `clear_scrollback`, and `reset_blank` (→ 0). The grid that
becomes the spare is always the live alternate grid, and `VirtualTerminal::resize`
gives that grid `resize_canvas` — which touches neither the tally nor the base —
never `resize_reflowing_columns`. `alt_spare` is cleared on `dims_changed` before
any resize runs.

### 4.3 The non-vacuity guard is honest today, and under-specified

`c64efb1` adds `assert!(admitted >= 2)` to the recycling test. Census of the four
candidates against the real check:

| candidate | admitted | `evicted` | `mutations` |
|---|---|---|---|
| fresh | yes | 0 | 0 |
| scrolled | **no** (rejected on tally) | 9 | 9 |
| reset | **yes** | 0 | 0 |
| resized | yes | 0 | 0 |

`admitted == 3`, and the load-bearing `reset` candidate is among them — confirmed
independently by the sabotage in §4.1, which reds this exact test. See F3.

## 5. Screenshot and pixel matrix

Two binaries built from source for this audit: base `9793961`, head `c64efb1`
(`shux version` self-reports the SHA on each leg, checked in the logs). Real
daemon, isolated short `XDG_RUNTIME_DIR`, pidfile asserted empty on exit.

| scene | what it exercises | 80x24 | 120x40 | 200x60 |
|---|---|---|---|---|
| `history` | 6,000 lines into scrollback; eviction | 0/0 | 0/0 | 0/0 |
| `altlive` | 5 alt cycles, final frame on a RECYCLED buffer | 0/0 | 0/0 | 0/0 |
| `altback` | 5 alt round-trips, back on primary | 0/0 | 0/0 | 0/0 |
| `vim4` | real `vim`, 4 alt cycles, typed into the recycled buffer | 0/0 | 0/0 | 0/0 |
| `copymode-top` | oldest retained line through copy mode | text-equal (see F2) | text-equal | text-equal |

Corpus replay at `c64efb1` against tracked goldens in `.shux/goldens/073-vt-corpus/`:
**19/19 fixtures exact 0/0**, including the five real-TUI raw PTY recordings
(`btop`, `lazygit`, `nvim`, `vicaya`, `vivecaka`) and the synthetic
alternate-screen, synchronized-output, origin/scroll-region, tab-stop, wide-CJK,
DEC-special-graphics and OSC-default-colour fixtures. 18 metrics are committed
here; the 13 remaining synthetic ones are in scratch.

**27/27 daemon artifacts byte-identical** between the two binaries.

**The comparator was proven able to fail before its zeros were believed:**
two different frames of the same size → FAIL (61,355 px); size mismatch → FAIL;
missing actual → FAIL; a **one-pixel, one-channel** perturbation of a real frame →
FAIL (`changed_pixels: 1`).

**Visual inspection** — PNGs opened as images at native resolution, not counted as
files. `history` shows lines 5980–6000 with the colour probe intact; `altlive`
shows the recycled alt buffer with no ghost cells in the large empty region below
the content, which is exactly where an unblanked reuse would show; `altback` shows
primary history 380–400 undisturbed with no alt bleed; `vim4` shows the file drawn
into the recycled grid with the typed marker and correct `~` filler; `btop` renders
box drawing, gradient bars and truecolor with no tofu or misalignment. No clipping,
colour bleed, cursor artifact or wrapping defect at any breakpoint.

**Colour probes** are asserted as rendered PIXELS, not as escape bytes —
`shux pane capture` emits plain text by design, so grepping it for `38;2;` proves
nothing. Extracted from the snapshots: truecolor `(120,220,180)`, indexed-208
`(255,135,0)`, bold truecolor `(255,170,60)`, indexed background 236 `(48,48,48)`
and basic blue all present. A monochrome or `NO_COLOR` regression could not pass.

## 6. Findings

Nothing at P0, P1 or P2.

### F1 (P3) — the `is_blank_canvas` rationale's second mover is still wrong

The doc on `a_grid_that_looks_blank_to_the_recycling_check_counts_from_zero` says:

> `clear_scrollback` bumps nothing, but cannot move a base FIRST: a non-empty
> scrollback implies prior scrolling, which does bump.

**A non-empty scrollback does not imply prior scrolling.** Reproduced:

```
Grid::new(4,8) → fill_alignment_pattern() → resize_canvas(2,8)
  raw=4 rows=2 sb=2 mutations=1 evicted=0     ← zero scroll calls
clear_scrollback()  →  evicted=2
```

`resize_canvas`'s row-shrink pops only *blank* trailing rows and retains non-blank
ones, so it manufactures scrollback and bumps no tally. The conclusion survives by
a different route — a retained row is non-blank, and non-blank means prior cell
writes, which *do* bump — but the stated premise is false.

This is the **fourth** correction this one rationale has needed: the council record
fixed a `clear_scrollback` sentence, the previous gate run falsified the resize
sentence, `028754f` rewrote all three movers, and the replacement's second mover is
wrong again. `c64efb1` also deleted the sentence that made the block robust to
exactly this:

> This pins the implication rather than the argument, because the day any of that
> stops holding the recycling branch starts carrying one session's indices into the
> next.

Removing that left pure argument with no acknowledgement that the test pins the
implication. Recommendation: stop enumerating movers in prose. The test asserts
`is_blank_canvas ⇒ evicted == 0` over a candidate set; say that, and restore the
deleted sentence. Not a defect — §4.2 shows the wrong reasoning has never made the
test wrong.

Related, minor: "Three things move a base" is a literal enumeration that omits
`reset_blank`, which is also an in-place writer of `evicted` (to `0`, so benign).

### F2 (P3, corrected here) — two committed pixel metrics were not deterministic

The previous record committed `pixel-ab-copymode-top-80x24.json` and
`-120x40.json` as exact-`0` metrics. The copy-mode scene is **not**
pixel-deterministic: it renders the attach client's status bar, which carries a
live uptime clock, and a copy-mode cursor whose row depends on when the capture
lands. At 200x60 this fired — 413 changed pixels — and an **A/A run of the head
binary against itself reproduced the identical 413-pixel delta on the identical
three text rows** (one 9×19 cell on rows 1 and 2, the clock on row 59), so it is
capture timing, not rendering, and not attributable to this diff. The 80x24 and
120x40 arms read `0/0` by luck. Both metrics are withdrawn. Copy-mode evidence is
kept as the deterministic part: the oldest retained line is identical on both
binaries at every breakpoint (981 / 982 / 990 lines evicted).

### F3 (P3) — the new non-vacuity guard is satisfiable without the candidate that matters

`assert!(admitted >= 2)` is met today with `admitted == 3` (§4.3), but `fresh` and
`resized` alone satisfy it. If a future change caused the `reset` candidate to be
rejected — the one that makes this test a regression test for `15fd835` — the guard
would stay green while the assertion went vacuous. Asserting that `reset` in
particular is admitted would close it.

### F4 (P3, pre-existing) — `make test-vt-corpus` dirties tracked files

Running the committed corpus gate rewrites tracked PNGs under
`.shux/qa/073-shux-vt-corpus-regression-harness/`. Run in a worktree here, as the
task requires. Unchanged from the previous run's finding; outside this diff.

### F5 (P3, environmental) — the audit host ran out of disk mid-run

A `make test` invocation exited 2 with `No space left on device` — not a test
failure. Freed and re-run to completion in the shared checkout; result in §7.
Recorded because an unread `exit 2` here would have looked like a red suite.

## 7. Passed evidence

- `make test` @ `c64efb1`: **2237 run, 2237 passed, 2 skipped**. One fewer than the
  2238 at `028754f`, which is exactly the deleted test — the counts corroborate.
- `make lint` @ `c64efb1`: clippy clean, formatting OK.
- `cargo test -p shux-vt` @ `c64efb1`: lib 397 passed; `alt_screen_differential` 2
  passed; every integration binary green.
- `make test-vt-corpus` @ `c64efb1` (worktree): passed.
- `make check-vt-qa` including `scripts/check-vt-qa-selftest.sh`: passed.
- Every fix seen RED on an unfixed tree, for the right reason (§4.1).
- 10,000-sequence counterexample search, 0 hits; 48 hits against the unfixed tree.
- 27/27 byte-identical daemon artifacts; 18 committed pixel metrics at exact `0`/`0`;
  19/19 corpus fixtures at exact `0`/`0` against tracked goldens.
- Pixel comparator proven sensitive to a single-pixel, single-channel change.
- Real `vim` driven through real panes at three breakpoints and inspected as images.

## 8. Residual risk

- `15fd835` remains unobservable through today's production API; its value is the
  invariant it restores for the absolute-anchor consumer in
  `docs/designs/inline-images.md`. If that consumer never lands, the fix protects
  nothing user-visible — it is still correct.
- `council-substitution.md` documents uncommitted harnesses, so its numbers cannot
  be re-run (carried forward from the previous run's P3). Its falsifiable claims
  held under independent test.
- The `is_blank_canvas` prose (F1) will need a fifth correction unless it is
  replaced with the implication the test actually pins.

## 9. Cleanup

Every daemon-backed run asserted an empty pidfile at
`$XDG_RUNTIME_DIR/shux/shux.pid` on exit and removed its isolated short runtime
dir; all three legs printed `pidfile-clean: ''`. Processes were identified by
pidfile only — no `pgrep -f`/`pkill -f` on a substring, per CLAUDE.md. Post-audit
daemon count: **0**. Scratch worktrees were created outside the checkout and
removed. `crates/` was never modified: `git status --porcelain -- crates/` is empty
at `c64efb1`, and every mutation experiment ran in a throwaway worktree.
