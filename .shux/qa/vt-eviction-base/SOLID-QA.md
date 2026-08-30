VERDICT: PASS

# SOLID VT QA — `vt-eviction-base` (re-audit at `32f192a`)

| | |
|---|---|
| PR | [#182](https://github.com/indrasvat/shux/pull/182) |
| Branch | `claude/shux-image-support-s0u4uy` |
| Frozen HEAD | `32f192a` |
| Base | `9793961` (`git merge-base origin/main HEAD`) |
| Diff identity (crates only) | `sha256(git diff 9793961..32f192a -- crates/)` = `21222b61771c530713f5e12a4c2bec4b8f596a1165e923b77ab36f315e2a4b14` |
| Diff identity (full) | `sha256(git diff 9793961..32f192a)` = `e694146fb416793bc1a397f2cbdb8cda48b2ad7d2b36776d7ccf6a166e11665f` |
| `crates/` tree | `8131f363c630a92add7b0838d587d57d2c40807a` |
| Range | `f01c954` · `6172c24` · `15fd835` · `fd3e1c8` · `e114e4c` · `028754f` · `c64efb1` · `32f192a` |
| Council tool | `dootsabha` NOT installed; substitution recorded in `council-substitution.md` |

**Lag disclosure, stated so the next reader is not misled the way Greptile was.**
Committing this evidence necessarily moves `HEAD`. The only commit after `32f192a`
on this branch is the one that adds this folder. Nothing under `crates/` moves in
it; `git diff 32f192a..HEAD -- crates/` is empty. That is the house pattern (see
`.shux/qa/kitty-graphics-control-parse/`).

This record supersedes the PASS at `c64efb1` (commit `8369838`), which superseded
the PASS committed by `028754f`. §0 explains why the first one did not hold.

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

## 0.5 Delta since the last verdict — `c64efb1..32f192a`

`32f192a` acts on the two P3s the `c64efb1` run raised. The implementer's claim
was that it touches only the `#[cfg(test)]` module of `crates/shux-vt/src/grid.rs`,
with no production code and no rustdoc on any public or crate-visible item.
**Verified mechanically, not accepted** — the last comment-only claim was true for
`crates/` and wrong about the commit (§0).

**Scope.** Exactly one code file changes: `crates/shux-vt/src/grid.rs`, +32/-24.
Everything else in the range is the previous evidence commit `8369838`.

**Containment, by construction rather than by reading hunk headers.** `grid.rs`
contains exactly **one** `#[cfg(test)]`, at line 1471. Strip lines 1471..EOF from
both revisions and sha256 what remains:

```
c64efb1 non-test prefix: caa940d7d18ef59fd0d1e1f9d3d119f2b8fcad7a76c6ff12e6acc10324ddef48
32f192a non-test prefix: caa940d7d18ef59fd0d1e1f9d3d119f2b8fcad7a76c6ff12e6acc10324ddef48
```

Byte-identical, and that prefix is brace-balanced. The only column-0 item starts
after the boundary are `#[cfg(test)]` and the private `mod tests {` it gates, so
everything from 1471 to EOF is nested inside that module. The rustdoc that changed
sits on `a_grid_that_looks_blank_to_the_recycling_check_counts_from_zero`, a private
`#[test]` fn. **Claim confirmed: zero production code, no public or crate-visible
rustdoc.**

**The shipped binary is provably unaffected — and the naive check said otherwise.**
Building `make release` at each commit gives *different* binaries, 33,514,664 vs
33,514,656 bytes. That is not the diff. `crates/shux/build.rs` and
`crates/shux-rpc/build.rs` shell out to `git rev-parse --short HEAD` and embed the
result as `SHUX_GIT_SHA`; the two binaries self-report `shux 0.48.0 (c64efb1)` and
`shux 0.48.0 (32f192a)`. The 8-byte delta is that string. Controlling for it — HEAD
pinned at `32f192a` so the embedded SHA is constant, rebuilding twice while swapping
only the content of `grid.rs`:

```
grid.rs @ 32f192a content -> sha256 0331befef4be690d2b92aeb3
grid.rs @ c64efb1 content -> sha256 0331befef4be690d2b92aeb3   BYTE-IDENTICAL
```

**Consequence, stated as a decision rather than an omission.** The pixel matrix,
the 27 daemon artifacts and the 19-fixture corpus sweep in §5 were measured at
`c64efb1` against an executable that is byte-for-byte the one `32f192a` produces.
Re-running them would re-measure the same binary. They are carried forward on
identity, not on argument, and were deliberately **not** repeated. Had the delta
touched one line of production code this paragraph would not be available and the
full matrix would have been re-run.

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
| A8 | `c64efb1`: the recycling test's non-vacuity guard is honest | PASS / **F3**, fixed in `32f192a` — see A12 | census: 3 of 4 candidates admitted, the load-bearing one among them (§4.3) |
| A9 | The whole range is a visual no-op on every render path | PASS | 27/27 byte-identical daemon artifacts; 18 committed pixel metrics at exact `0`/`0` |
| A10 | Council evidence exists for the change | PASS | `council-substitution.md` for `f01c954..fd3e1c8`; `028754f` and `c64efb1` carry theirs in their commit messages, which is where CLAUDE.md puts the durable record |
| A11 | `32f192a`: the delta is test-module-only | PASS | §0.5 — non-test prefix byte-identical; release binary byte-identical with the embedded SHA held constant |
| A12 | `32f192a`: the restructured test still discriminates | PASS | §4.4 — unchanged test (sha `6c5928c22abe8a6a`) reds on the unfixed tree at the loop assertion, naming `reset` |

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

### 4.4 The restructured test still bites, and on the right assertion

`32f192a` replaces the count-based non-vacuity guard with a by-name assertion that
the `reset` candidate is admitted. The hazard in that shape is that the *guard*
starts failing instead of the property, which would still be red but would no
longer be a regression test for `15fd835`. Measured, not reasoned.

Line 653 (`self.evicted = 0` in `reset_blank`) deleted at `32f192a`, nothing else
changed. The target test was left byte-identical — sha256 of its extracted body is
`6c5928c22abe8a6a` on both the fixed and the unfixed tree, so this is the unchanged
test run against the unfixed code, not a sabotaged assertion. Three lib tests red:

```
grid::tests::a_grid_that_looks_blank_to_the_recycling_check_counts_from_zero  FAILED
grid::tests::a_recycled_alternate_screen_buffer_does_not_alias_the_previous_session  FAILED
grid::tests::reset_blank_is_indistinguishable_from_a_fresh_grid  FAILED

panicked at crates/shux-vt/src/grid.rs:2163:17:
assertion `left == right` failed: reset: a grid the recycling branch would reuse
untouched is still counting from a discarded grid's base
  left: 9
 right: 0
```

**It fails at the loop, not at the guard.** A census probe (added as a separate
test in a throwaway worktree; the target test untouched) confirms the reset
candidate is *still admitted* on the unfixed tree — `admitted=true, evicted=9` —
so the guard passes on both trees and it is the property assertion that bites. The
new per-candidate name in the message points straight at the offender.

**Candidate census on the fixed tree**, against the real `is_blank_canvas`
(`mutations == 0 && rows && cols && raw.len() && config`):

| candidate | admitted | evicted | mutations | rows | rejected on |
|---|---|---|---|---|---|
| fresh | yes | 0 | 0 | 4 | — |
| scrolled | no | 9 | 9 | 4 | tally |
| **reset** | **yes** | 0 | 0 | 4 | — |
| resized | yes | 0 | 0 | 4 | — |
| restructured | no | 2 | 4 | 2 | tally, rows, rawlen |

**`reset` is admitted for the right reason, not by coincidence of geometry.** Its
parent `scrolled` has identical geometry (4x8, rawlen 4) and is rejected; the only
thing that changes between them is `reset_blank` zeroing the write tally. Admission
turns on the actual recycling path.

F1 is resolved by deletion rather than by a fifth rewording, which is what the
previous run recommended: the mover enumeration is gone and the doc now states the
property the test pins. That also disposes of `reset_blank`, the fourth in-place
writer the enumeration had omitted.

## 5. Screenshot and pixel matrix

**Provenance.** Measured at `c64efb1` by this audit's previous pass, and carried
forward to `32f192a` on the binary-identity proof in §0.5 rather than re-run — the
two commits compile to the same executable byte-for-byte, so repeating the matrix
would re-measure the same binary. Stated as a decision; see the residual risk in §8.

Two binaries built from source: base `9793961`, head `c64efb1` (`shux version`
self-reports the SHA on each leg, checked in the logs). Real daemon, isolated short
`XDG_RUNTIME_DIR`, pidfile asserted empty on exit.

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

Nothing at P0, P1 or P2. **F1 and F3 from the `c64efb1` run are fixed in `32f192a`
and re-verified here**; they are kept below with their resolution so the record of
what was found stays legible.

### F1 (P3) — RESOLVED in `32f192a` — the `is_blank_canvas` rationale's second mover was wrong

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

**Resolution.** `32f192a` took the recommendation rather than rewording a fifth
time: the enumeration is deleted and the doc is five lines stating the property the
test pins. Re-read at `32f192a` — no argument remains, so there is nothing left to
falsify, and the omitted fourth mover (`reset_blank`) stops mattering.

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

### F3 (P3) — RESOLVED in `32f192a` — the non-vacuity guard was satisfiable without the candidate that matters

`assert!(admitted >= 2)` is met today with `admitted == 3` (§4.3), but `fresh` and
`resized` alone satisfy it. If a future change caused the `reset` candidate to be
rejected — the one that makes this test a regression test for `15fd835` — the guard
would stay green while the assertion went vacuous. Asserting that `reset` in
particular is admitted would close it.

**Resolution.** `32f192a` asserts exactly that, by name. Verified in §4.4 that the
guard passes on both the fixed and unfixed trees while the property assertion is
what reds — i.e. the fix closed the hole without moving the failure onto the guard.

### F4 (P3, pre-existing) — `make test-vt-corpus` dirties tracked files

Running the committed corpus gate rewrites tracked PNGs under
`.shux/qa/073-shux-vt-corpus-regression-harness/`. Run in a worktree here, as the
task requires. Unchanged from the previous run's finding; outside this diff.

### F5 (P3, environmental) — the audit host ran out of disk mid-run

A `make test` invocation exited 2 with `No space left on device` — not a test
failure. Freed and re-run to completion in the shared checkout; result in §7.
Recorded because an unread `exit 2` here would have looked like a red suite.

### F6 (P3, new) — the `restructured` candidate can never reach the admitted set

`32f192a` adds a fifth candidate built from the previous run's F1 counterexample.
The census (§4.4) shows it is rejected on **three** grounds at once — tally,
`rows` and `raw.len()` — because `resize_canvas(2, 8)` leaves it 2 rows tall while
the check is asked for 4. It therefore contributes nothing to the property being
asserted over the candidate set; a reader scanning the list would reasonably assume
otherwise.

Its real value is the line above it, `assert!(restructured.evicted() > 0, "the base
really moved")`, which pins the F1 counterexample as executable documentation —
scrollback arriving with zero scroll calls. That is worth keeping. One clause
saying the candidate is there to document a base move, not to be admitted, would
stop the list reading as five live candidates when it is three.

Worth stating positively, because it is the reason the test is sound: **no grid
with a moved base can ever be admitted except via `reset_blank`**, since admission
requires `mutations == 0` and every other base-mover leaves the tally raised. That
is precisely why `reset` is the discriminator and why F3's fix is the right one.

## 7. Passed evidence

- `make test` @ `32f192a`: **2237 run, 2237 passed, 2 skipped** — unchanged from
  `c64efb1`, as a test-restructuring with no net test count change should be. Both
  are one fewer than the 2238 at `028754f`, exactly the test deleted there.
- `make lint` @ `32f192a`: clippy clean, formatting OK.
- `cargo test -p shux-vt --lib` @ `32f192a`: 397 passed, same as `c64efb1`.
- `make test` / `make lint` @ `c64efb1`: green (previous run, superseded).
- `cargo test -p shux-vt` @ `c64efb1`: lib 397 passed; `alt_screen_differential` 2
  passed; every integration binary green.
- `make test-vt-corpus` @ `c64efb1` (worktree): passed.
- `make check-vt-qa` including `scripts/check-vt-qa-selftest.sh`: passed @ `32f192a`.
  Additionally proven able to reject THIS evidence set: flipping a committed metric's
  threshold to `0.01`, its `status` to `fail`, and the report's first line to
  `VERDICT: FAIL` were each rejected with the specific message, then restored.
- Every fix seen RED on an unfixed tree, for the right reason (§4.1, §4.4), with the
  target test proven byte-identical between the fixed and unfixed trees.
- Release binary proven byte-identical across `c64efb1..32f192a` once the embedded
  `SHUX_GIT_SHA` is held constant (§0.5).
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
- The `is_blank_canvas` prose no longer carries an argument (F1 resolved), so the
  four-falsification cycle is closed. The residual is F6: the candidate list reads
  as five live candidates when three can be admitted.
- The pixel and daemon layers were measured at `c64efb1`, not re-run at `32f192a`.
  That rests on the binary-identity experiment in §0.5. If that experiment is wrong
  — e.g. the build were not reproducible for a reason unrelated to `SHUX_GIT_SHA` —
  the visual evidence would be one commit stale. Two builds of the same source in
  the same target dir produced identical bytes, which is the check available here.

## 9. Cleanup

Every daemon-backed run asserted an empty pidfile at
`$XDG_RUNTIME_DIR/shux/shux.pid` on exit and removed its isolated short runtime
dir; all three legs printed `pidfile-clean: ''`. Processes were identified by
pidfile only — no `pgrep -f`/`pkill -f` on a substring, per CLAUDE.md. Post-audit
daemon count: **0**. Scratch worktrees were created outside the checkout and
removed. `crates/` was never modified: `git status --porcelain -- crates/` is empty
at `32f192a`, and every mutation experiment ran in a throwaway worktree that was
removed afterwards. The one experiment performed in the shared checkout — the
binary-identity build — touched a single tracked file via `git checkout`, restored
it immediately, and `git status --porcelain` was confirmed empty afterwards.
