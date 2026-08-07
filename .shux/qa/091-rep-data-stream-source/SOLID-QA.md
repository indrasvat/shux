VERDICT: PASS

# 091 — REP (`CSI Pn b`) sourced from the data stream (issue #122) — VT QA record

**Task:** `docs/tasks/091-rep-data-stream-source.md`
**Gate:** `shux-vt-solid-qa`, run as an independent audit.
**Branch:** `claude/shux-issue-122-33umu5`
**Head under audit:** `8f5ebf81e2e7df4d6ec8466728dcca7362f3a358`
— `fix(vt): a flag no longer forms across a cursor move (#122, PR #129 review)`
**Base:** `e856793` (v0.46.7).
**Binaries:** `target/release/shux` rebuilt from `8f5ebf8` during this audit
(`shux version` → `0.46.7 (8f5ebf8)`); base arm `/tmp/shux-122-evidence-base/target/debug/shux`
(`0.46.7 (e856793)`).

Head moved during the audit: `8f5ebf8` landed after the first evidence pass. Every
head-arm artifact in this directory was **re-shot against `8f5ebf8`** and the pixel
metrics regenerated from those re-shot frames. The re-shot oracle frames are byte-identical
to the pre-`8f5ebf8` pass, which is itself the finding that the regional-indicator fix is
surgical.

**Scope of this verdict.** It covers commit `8f5ebf8` exactly. Every measurement below was
taken with `git status --porcelain -- crates/` verified clean immediately before and after.
Uncommitted work appeared in the tree late in the audit (see P2-1); it is *not* covered
here and needs its own gate pass.

---

## 1. Verdict

**PASS.** Every acceptance criterion in the task file is met and independently
evidenced. Three P3 findings are recorded below; none of them is a behaviour defect in
the shipped code, and none maps to an unmet DoD row.

## 2. Task DoD matrix

Every row re-verified in this audit. Nothing is carried over from the implementer's
summary.

| # | Acceptance criterion (task file) | Verdict | Evidence |
|---|---|---|---|
| 1 | REP repeats the preceding character in the data stream, across any number of intervening control sequences | PASS | `rep_matches_the_literal_stream_across_an_intervening_sequence` (23 sequences × 3 sources); `properties::random_programs_match_the_literal_stream` (512 programs); pixel oracle at 3 geometries, 0 changed pixels |
| 2 | Column 0 is not special: a homed cursor repeats the same character any other position would | PASS | `rep_survives_homing_the_cursor`; e2e `a_real_pane_repeating_at_column_zero_is_not_dropped`; `column-zero` scene, `XXX` after / `X` before |
| 3 | With nothing printed yet, or after RIS, REP writes nothing | PASS | `rep_with_no_preceding_character_is_a_no_op`, `ris_forgets_the_preceding_character`, `rep_after_only_control_sequences_is_a_no_op`; mutation "RIS no longer clears the remembered character" killed |
| 4 | Repeats take the pen current at the `CSI b` | PASS | `rep_paints_with_the_pen_current_at_the_sequence`, `rep_carries_truecolor_and_indexed_pens`, `rep_carries_the_current_hyperlink`; cell frame at 80x24 row 10 = `O` idx 1 then 20 × rgb(120,220,180); mutation "the repeats take the original pen" killed |
| 5 | The remembered character is the one displayed, after character-set translation | PASS | `rep_repeats_the_translated_glyph`, `rep_after_leaving_the_line_drawing_set_still_repeats_the_line`; `box` scene draws `┌───┐`/`└───┘` and asserts `qqqq` is absent; mutation "remembered before translation" killed |
| 6 | Grapheme clusters, wide characters and their continuation cells survive | PASS | `rep_of_a_wide_character_keeps_wide_pairs`, `rep_of_a_width_expanded_zwj_cluster_keeps_its_pairs`, `assert_wide_pairs_intact` across the suite; independent cell-frame dump at 80x24 shows every `界` and every `a‍👨` followed by exactly one continuation |
| 7 | Wrapping, scroll regions, origin mode, insert mode and pending auto-wrap behave as for the character arriving again | PASS | `rep_wraps_onto_following_lines`, `rep_scrolls_only_within_the_scroll_region`, `rep_respects_origin_mode`, `rep_under_insert_mode_shifts_existing_text`, `rep_honours_a_pending_auto_wrap`; independent pixel oracle includes an insert-mode run and a run that wraps the right margin |
| 8 | `CSI b` with an intermediate or a private marker is not REP | PASS | `only_bare_csi_b_repeats`; **gate-designed** mutation "a private-marker CSI b becomes REP" killed by that test |
| 9 | The work a single REP can buy stays bounded (issue #102) | PASS (with P3-1) | `rep_is_clamped_to_one_screenful`, `rep_bounds_the_scalars_it_writes_not_just_the_copies`, `rep_is_clamped_for_cluster_sources_too`; e2e `a_real_pane_cannot_buy_unbounded_work_with_one_rep`. Measured worst case is `max(2·rows·cols, 32)` scalars — bounded, but not the "two screenfuls" the task prose states. See P3-1 |
| 10 | `make check` green; zero leaked daemons | PASS | `make test` and `make lint` re-run green in this audit; process table after every run carries no `shux`, `vim`, `nvim`, `htop`, `btop`, `lazygit` or `less`; no pidfiles left behind |

### Deliberate divergences — assessed, not rediscovered

| Documented as deliberate | Gate assessment |
|---|---|
| The repeat count is clamped to one screenful (#102), so above that REP scrolls less than the literal stream | **Accepted.** The alternative — a cap that includes scrollback capacity — lets ten bytes write `scrollback_capacity × cols` cells, which is the amplification #102 exists to prevent. Pinned by `a_repeat_larger_than_one_screenful_scrolls_less_than_the_literal_stream`, stated as a precondition in the test module's own documentation, and killed as a mutation ("iteration clamp removed"). This is a documented choice, not an untested edge |
| shux stores the repeated character AFTER charset translation; Alacritty stores it before | **Accepted.** Both readings are literal — ECMA-48's "preceding character in the data stream" vs xterm's "preceding graphic character" — and each is self-consistent with its own oracle. They agree in every ordering a real application emits, because the switch back to ASCII comes after the repeat. shux's reading is the one that makes the `box` scene draw a line instead of `qqqq`. The property test excludes `ESC ( 0` / `ESC ( B` / `ESC 7` / `ESC 8` from its intervening-noise set for exactly this reason, and says so; the excluded behaviour is covered directly by `rep_repeats_the_translated_glyph` and `rep_after_leaving_the_line_drawing_set_still_repeats_the_line` |

## 3. Testing matrix

| Layer | Required by task | Re-run in this audit | Result |
|---|---|---|---|
| Unit (VT) | `crates/shux-vt/src/lib.rs` | `cargo test -p shux-vt` | 399 lib tests pass; the 5 REP unit tests named in the matrix all present and green |
| Integration (VT) | `crates/shux-vt/tests/rep.rs` | `cargo test -p shux-vt --test rep` | **62 pass, 0 fail** (task matrix says 57 — drift, P3-3) |
| Differential | oracle over source shapes × prefixes × counts | same binary | `rep_matches_the_literal_stream_for_every_source_shape`, `rep_matches_the_literal_stream_across_an_intervening_sequence` green |
| Property | 512 random programs + 256 chunked | same binary | `properties::random_programs_match_the_literal_stream`, `properties::chunked_delivery_matches_whole_delivery` green |
| Raw byte / replay | — | 5 committed rich-TUI raw PTY recordings replayed through a real pane on **both** binaries | byte-identical text, cells and pixels (see §5) |
| End-to-end (daemon) | `crates/shux/tests/rep_pane_e2e.rs` | `cargo test -p shux --test rep_pane_e2e -- --test-threads=1` | 5 pass, real daemon, real PTY, real shell |
| Shux automation | `.shux/scripts/issue_122_evidence.sh` | base arm `EXPECT_DEFECT=1`, head arm plain | base **5 failed / 6 passed** (defect reproduces); head **11 passed / 0 failed** |
| Mutation (implementer's) | `.shux/scripts/issue_122_mutation_check.sh` | re-run from scratch | **16 killed, 0 survived** |
| Mutation (gate-designed) | not required | 9 mutations authored without reference to that list | 7 killed, 2 survived → P3-1, P3-2 |
| Visual inspection | 5 scenes | 20+ full-resolution PNGs opened as images | see §4, §5 |
| Pixel comparison | — | `.claude/automations/pixel_verify.py`, exact 0/0 | 3 oracle metrics + 1 unchanged-control, all 0 changed pixels; 1 negative control at 4,810 |
| DootSabha design | step 1 | **N/A in this environment — substituted** | `dootsabha-design.md`. Not a waiver |
| DootSabha diff review | step 6 | **N/A in this environment — substituted** | `dootsabha-implementation.md`. Not a waiver |

### The oracle this gate built independently

The task's harnesses assert on text and on named cells. This gate added a stronger,
independent check that closes the loop at the pixel level:

> Two panes, same binary, same geometry, same byte stream — except that in one arm every
> `CSI n b` is replaced by **n literal copies of the character**. That is ECMA-48
> §8.3.103's own definition of REP. Snapshot both through the real rasterizer and require
> **exact** pixel equality.

The expected PNG is therefore *derived from the specification*, not minted by the gate.
The scene carries a colour probe and eight REP shapes: a rule, a wide CJK run, an `e`+U+0301
combining cluster, a DEC line-drawing edge, a pen switch between the character and the
`CSI b`, an `a`+ZWJ+emoji cluster, an insert-mode run that shifts existing text, and a run
that wraps the right margin onto the next line.

| Geometry | changed pixels | total pixels | metric |
|---|---|---|---|
| 80x24 | **0** | 328,320 | `pixel-rep-vs-literal-80x24.json` |
| 120x40 | **0** | 820,800 | `pixel-rep-vs-literal-120x40.json` |
| 200x60 | **0** | 2,052,000 | `pixel-rep-vs-literal-200x60.json` |

The canonical cell frames are identical too — the only field that differs is the pane's
content `revision`: **2 for the REP arm against 10 for the literal arm**, which is the
same screen bought with a fifth of the writes.

### The comparator was proven able to fail — on the real defect, not on injected noise

`negative-control-base-rep-vs-literal-80x24.json` is the **identical scene shot through
the pre-fix binary `e856793`**. It reads `"status": "fail"`, `changed_pixels: 4810`.

> **`status: "fail"` in that file is this arm's PASS condition.** It is a negative
> control, not the gate finding a defect in the fix. A reader must not mistake it. It is
> deliberately excluded from `pixel_metrics` in the manifest (where every entry must be
> `pass`), it is prefixed `negative-control-`, and every metric file in this directory
> now carries `binary`, `role` and `expected_status` fields so its meaning does not have
> to be reconstructed from file paths or mtimes. The stamping tool asserts the observed
> status against the arm's expected status and refuses to write a file on a mismatch —
> proven by feeding it the head arm with `expected_status=fail`, which aborted and wrote
> nothing.

`negative-control-base-80x24-diff.png` was opened: it shows precisely the content the
pre-fix binary lost — the rule, the CJK run, the `éééééééé` cluster, the line-drawing
edge and the ZWJ run — legible and opaque. The three head-arm diffs are uniformly black
and opaque (the transparent-diff defect fixed under task 090 has not regressed).

## 4. Screenshot matrix

Every PNG below was opened and inspected as an image at native resolution, not merely
checked for existence or size.

| Viewport | Scene / app | Screenshot | Baseline | Diff | Status |
|---|---|---|---|---|---|
| 80x24 | REP oracle (8 shapes + colour probe) | `rep-oracle-80x24-actual.png` | `rep-oracle-80x24-expected.png` (literal stream) | `rep-oracle-80x24-diff.png` | PASS — 0 px |
| 120x40 | REP oracle | `rep-oracle-120x40-actual.png` | `rep-oracle-120x40-expected.png` | `rep-oracle-120x40-diff.png` | PASS — 0 px |
| 200x60 | REP oracle | `rep-oracle-200x60-actual.png` | `rep-oracle-200x60-expected.png` | `rep-oracle-200x60-diff.png` | PASS — 0 px |
| 80x24 | REP oracle, **base binary** | `negative-control-base-80x24-actual.png` | `negative-control-base-80x24-expected.png` | `negative-control-base-80x24-diff.png` | PASS as negative control — 4,810 px, `status: fail` expected |
| 44x12 | `pen` scene (no cursor move) | `pen-unchanged-head-actual.png` | `pen-unchanged-base-expected.png` | `pen-unchanged-diff.png` | PASS — 0 px, head vs base |
| 120x40 | live `btop` | `richtui-btop-120x40-actual.png` | — | — | PASS — 578 truecolor runs, braille + box drawing clean |
| 120x40 | live `lazygit` | `richtui-lazygit-120x40-actual.png` | — | — | PASS — 18 rgb + 64 indexed runs, panels and underlined links clean |

What the inspection actually found, per scene:

* **rule** — base: a single `=`; head: a 76-column rule. Colour probe legible in both
  (`TRUECOLOR` teal, `INDEXED` orange, `BASIC` blue).
* **progress-bar** — base: one `#` beside "100% complete", which is the user-visible bug
  in its purest form; head: a full 34-block orange bar.
* **box** — base: corners and verticals only; head: a closed box in DEC line-drawing
  glyphs, no `q` carriers, no tofu on the box characters.
* **column-zero** — base: `X`; head: `XXX`.
* **pen** — byte-identical PNGs on both binaries (md5 `b6e89db7…`), first `O` red, the
  following twenty teal; second row first `o` blue, following twenty orange. The negative
  control for the pen clause.
* **oracle 80x24** — combining marks compose correctly (`é` × 8, acute over the `e`, not
  beside it); every `界` and `a‍👨` occupies a width-2 head plus exactly one continuation;
  the wrapped `+` run splits 4 + 6 across the margin; no ghost cells, no colour bleed
  past the SGR resets, cursor block where expected.
* **btop / lazygit / htop / vim / nvim / less** — no clipping, no tofu on box-drawing or
  braille, no colour bleed, no layout drift.

## 5. Rich-TUI compatibility

Required pass for any VT-parsing change. Run two ways.

**Deterministic replay (the load-bearing one).** The five committed raw PTY recordings in
`.shux/fixtures/vt-corpus/rich-tui/` replayed through a real 120x36 pane on **both**
binaries — no animation, no capture-timing race:

| Fixture | text | cell frame | PNG |
|---|---|---|---|
| btop | identical | identical | identical |
| lazygit | identical | identical | identical |
| nvim | identical | identical | identical |
| vicaya | identical | identical | identical |
| vivecaka | identical | identical | identical |

None of the five recordings contains a `CSI b`, which this gate verified rather than
assumed (0 matches for `\x1b\[[0-9;]*b` in every file). That makes this the right test for
the *collateral* risk in the diff — `write_char` now updates a record on **every** printed
character, and `csi_dispatch`'s cluster-break rule changed for **every** `CSI … b` — and it
comes back byte-identical.

**Live TUIs**, `TERM=xterm-256color COLORTERM=truecolor`, at 80x24, 120x40 and 200x60,
each launched, waited for content, settled, captured, glanced and snapshotted:

| App | 80x24 | 120x40 | 200x60 | colour |
|---|---|---|---|---|
| vim (syntax on) | PASS | PASS | PASS | 77 / 116 / 162 indexed runs |
| nvim | PASS | PASS | PASS | 74 / 114 / 160 indexed runs |
| htop | PASS | PASS | PASS | 122 / 217 / 319 indexed runs |
| btop | PASS | PASS | PASS | 367 / 578 / 859 **truecolor** runs |
| lazygit | PASS | PASS | PASS | 41 / 64+18rgb / 68+30rgb runs |
| less (`grep --color=always` pipeline) | PASS | PASS | PASS | 117 / 144 indexed runs |
| `tput rep` (terminfo REP, a real Unix command) | PASS | — | — | see below |

`tput rep 61 76` draws 76 `=` on **both** binaries. That is worth stating plainly: the
terminfo `rep` string is `%p1%c\E[%p2%{1}%-%db`, which emits the character immediately
before the `CSI b` with no cursor move in between — precisely the one case the old
screen-derived code got right. The terminfo path is therefore *not* a discriminating test
for this fix, and it confirms the fix does not regress the most common REP producer.

An accidental but pointed demonstration turned up while probing this: a `tput` usage error
printed `…capability '='` and then emitted its REP anyway. On the head binary the `'` — the
preceding character in the data stream, across the intervening newline — was repeated
across the row. That is issue #122's fix behaving correctly on a stream nobody wrote
deliberately.

## 6. Findings

Ordered by severity. **No P0, no P1.**

### P2-1 — a mutation run rewrote `parser.rs` in place while the gate was measuring

`.shux/scripts/issue_122_mutation_check.sh` edits `crates/shux-vt/src/parser.rs` in the
working tree and reverts it with `git checkout` between mutations. Run concurrently with
anything else that compiles or tests that crate, it silently poisons the other party's
measurements — there is no lock, and the only guard is an up-front `git diff --quiet`
that cannot see a run starting *after* it.

That happened here. Late in the audit a `cargo test -p shux-vt --test rep` returned
**63 passed, 1 failed**, with
`a_mark_stranded_by_a_dropped_character_does_not_change_what_rep_repeats` reporting
`left: Some("e\u{fe0f}\u{fe0f}")` against `right: Some("e\u{fe0f}")`. Re-run in
isolation seconds later, the same test passed. `git diff HEAD -- crates/shux-vt/src/parser.rs`
then showed mutation #15 of the implementer's list ("the repeats take the original pen")
live in the file. Sampling settled it: **parser.rs was dirty in 49 of 60 one-second
samples**, and still dirty in 30 of 30 a quarter of an hour later.

This is why the failure is reported as a process defect and **not** as a defect in the
fix — it was reproduced, attributed, and traced to a transiently mutated source file
rather than to `8f5ebf8`. Had the gate taken that run at face value it would have filed a
false P0 against a correct implementation.

No result in this report was measured against a mutated tree. Everything in §3 and §4 was
taken before the concurrent run began, each with a verified-clean `git status`, and the
audited binary is version-stamped `0.46.7 (8f5ebf8)`.

*Suggested*: `issue_122_mutation_check.sh` should take an exclusive lock (a flock on the
repo or on the parser path) and refuse to start if one is held, the same way daemon-backed
suites are already required to run serially. Mutating tracked source in the shared working
tree is the sharpest edge in this repo's verification machinery, and CLAUDE.md's rule that
defects in verification machinery are fixed first applies to it directly.

### P3-1 — the `.max(1)` floor on the scalar budget is unpinned, and the task's stated bound is off by a bounded constant

`repeat_iterations` computes `count.min(cells).min(scalar_budget.max(1))` where
`scalar_budget = 2·cells / source.scalar_count()`. On a grid smaller than 16 cells with a
cluster of more than `2·cells` scalars the budget is 0 and the `.max(1)` floor forces one
full copy through anyway. Gate-designed mutation **"scalar budget loses its `.max(1)`
floor" SURVIVED** the whole REP suite.

Confirmed observable rather than argued from source, with a probe binary linked against
`shux-vt`:

```
2x4 grid (8 cells), source = 'a' + 31 × U+0301 (32 scalars), then CSI 1 b
  shipped   : 32 scalars written, screen "aa  "
  mutant    : 0 scalars written,  screen "a   "
```

So the task file's claim — "the two together cap the work at two screenfuls **however
pathological** the remembered character is" — is not exact. The true bound is
`max(2·rows·cols, MAX_GRAPHEME_SCALARS)` = `max(2·rows·cols, 32)` scalars. **This is not a
DoS and not a defect**: 32 scalar writes is trivial, `MAX_GRAPHEME_SCALARS` is a hard cap
in `cell.rs`, and acceptance criterion 9 ("stays bounded") is met. It is a prose
inaccuracy plus an untested defensive branch, so a future refactor could drop the floor
and only this gate's mutation would notice.

*Suggested*: one test on a sub-16-cell grid with a 32-scalar cluster, and a one-line
correction to the bound stated in the task file and in `repeat_iterations`' doc comment.

**Status at the time of writing:** an uncommitted test named
`a_cluster_longer_than_the_budget_still_writes_one_whole_copy` appeared in the working
tree during the audit and matches this shape. **This gate has not verified it** — it is
not in `8f5ebf8`, and the tree was under the concurrent mutation run of P2-1, so neither
its green run nor its fail-first behaviour could be measured honestly. It must be shown
failing against the "scalar budget loses its `.max(1)` floor" mutation before it counts.

### P3-2 — the "a dropped wide character is still what REP repeats" clause has no test

`write_char`'s comment states the intent: on a one-column terminal a wide character has
nowhere to go, "the stream still carried it, so a repeat of it does the same harmless
nothing another copy would have". Gate-designed mutation **"a wide char dropped on a
1-column grid is not remembered" SURVIVED**.

Observable, and the shipped behaviour is the correct one:

```
2x1 grid, "A" then U+754C (dropped, no room), then CSI 3 b
  shipped : screen ["A", " "]   -- the repeat is a harmless nothing
  mutant  : screen ["A", "A"]   -- REP falls back to the older character
```

The clause is documented and correct; it is simply unpinned. Narrow (one-column grids
only), hence P3.

**Status at the time of writing:** an uncommitted test named
`a_wide_character_with_nowhere_to_go_is_still_what_rep_repeats` appeared in the working
tree during the audit and matches this shape. Same caveat as P3-1 — **unverified by this
gate**, and it must be shown failing against the "a wide char dropped on a 1-column grid
is not remembered" mutation before it counts.

### P3-3 — the task's testing matrix undercounts its own suite

The matrix says "57 cases across 10 groups" for `crates/shux-vt/tests/rep.rs`. The file
now holds **62** tests (60 plus the 2 property tests). The three added after the matrix
was written are `a_regional_indicator_does_not_join_across_a_cursor_move`,
`a_repeat_larger_than_one_screenful_scrolls_less_than_the_literal_stream` and the
Alacritty cross-check pin. Documentation drift only — the count is under, not over.

### Observations that are NOT findings

* **`界` renders as a notdef box in the rasterizer.** Reproduced identically in the
  literal arm, and the literal-arm PNGs from `e856793` and `8f5ebf8` are byte-identical
  (md5 `084ecad7…`), so this is font coverage in the bundled
  `JetBrainsMonoNerdFontMono-Regular.ttf`, not anything REP touches. The VT layer is
  correct: `pane capture` returns `界` and the cell frame shows a width-2 head plus a
  continuation. Out of scope for this task; flagged so it is not rediscovered.
* **One empty `pane capture` against a vim hit-enter prompt.** Seen once, mid-audit, from
  a gate harness that had a swallowed `wait-settled`. Re-driven **six times on each
  binary**: 189 bytes and 3 non-blank rows every time, byte-identical between `e856793`
  and `8f5ebf8`. Not reproducible, not attributable to this diff, and reported here only
  because an unexplained blank capture should never go unrecorded.
* **The e2e suite asserts the colour probe as text, not as attributes.** All five e2e
  scenes emit truecolor + indexed + basic; one asserts the probe strings are on screen.
  The attribute-level assertion lives in the `pen` scene of the evidence harness, which
  reads the canonical cell frame (`"rgb": [120,220,180]`, `"idx": 208`, `"idx": 1`) — and
  in this gate's own cell-frame dumps. The mandate is met across the harnesses, not
  within any one of them.

## 7. Passed evidence

* `make lint` — clippy `-D warnings` + fmt-check green.
* `make test` — full workspace under `no_leak_guard.sh`, green.
* `cargo test -p shux-vt` — 399 lib + 62 REP + all other suites green.
* `cargo test -p shux --test rep_pane_e2e -- --test-threads=1` — 5/5.
* `.shux/scripts/issue_122_mutation_check.sh` — 16 killed, 0 survived, each naming its
  killer test. The vacuity guard is real: the script fails a mutation whose edit matched
  nothing.
* `.shux/scripts/issue_122_evidence.sh` — base arm `EXPECT_DEFECT=1` → 5 assertions fail
  (defect reproduces, script would have failed had it not); head arm → 11/11.
* Gate-designed mutations — 9 authored independently; the private-marker guard, pending
  auto-wrap, alternate-screen entry and alternate-screen exit clauses all killed
  (alt-screen by the property test, which is the right thing to be killed by).
* Pixel oracle — 0 changed pixels at 80x24, 120x40, 200x60 with exact 0/0 thresholds,
  against a specification-derived baseline.
* Comparator fail-proof — 4,810 changed pixels on the base binary, diff image inspected.
* Rich-TUI — 5 committed recordings byte-identical across the fix; 6 live TUIs plus
  `tput rep` clean at 3 geometries.

## 8. Residual risk

* **DootSabha councils did not run.** Substituted, documented, and not waived. Anyone who
  requires genuine council output should treat that matrix row as unmet.
* **The oracle has two stated preconditions**, both correct and both pinned: the
  one-screenful clamp, and a cluster torn at the right margin (issue-filed, task 069's
  surface). A future change to grapheme printing will move the second; the test
  `rep_after_a_cluster_torn_by_the_right_margin_repeats_the_surviving_half` is what makes
  that show up as a test change rather than a silent behaviour change.
* **Two clauses of the shipped code are unpinned** (P3-1, P3-2). Neither is wrong today.
* **Issues #126 (`_ignore` on overflowed CSI) and #127 (DEL stored as printable)** are
  filed and out of this diff. Both were confirmed to be pre-existing and untouched here.
* **The `界` tofu is a font-coverage limit**, unchanged by this work.
* **Uncommitted work is in the tree and is not covered by this verdict.** Two tests
  answering P3-1 and P3-2 appeared during the audit. When they land, the gate needs a
  re-run of at least `cargo test -p shux-vt --test rep` plus the two matching mutations —
  cheap, but not optional, because a test that has only ever been seen passing proves
  nothing.
* **The mutation harness has no mutual exclusion** (P2-1). Until it does, any future gate
  pass on this crate can be poisoned the same way.

## 9. Cleanup

Zero leaked daemons and zero leaked children. Every daemon-backed run used an isolated
`XDG_RUNTIME_DIR` and `shux_harness_assert_no_daemon`, which identifies the daemon **by
pidfile** — no `pgrep -f` / `pkill -f` substring matching anywhere in this audit. Daemon-
backed suites were run **serially**, never two at once. Post-audit process table carries
no `shux`, `vim`, `nvim`, `htop`, `btop`, `lazygit` or `less`; no `shux.pid` remains under
any runtime directory. The tree changes from this audit are this directory (staged with `git add -N` so the
enforcement dry-run in §11 could resolve the files, left staged for the operator to
commit) and nothing else. The gate's own temporary parser mutations were reverted with
`git checkout` and `git status --porcelain -- crates/` confirmed clean after every round;
the parser modification observed later in the audit is P2-1's concurrent run, not this
gate's.

## 10. The `Quality Gate:` marker — recommendation

The task file carries **no** `**Quality Gate:** shux-vt-solid-qa` marker, so
`make check-vt-qa` passes today **without checking task 091 at all**. That enforcement
gap is filed as issue #123. Task 090 hit the same collision and re-scoped it explicitly:
adding the marker makes `scripts/check-progress.sh` hard-require a committed
`*-actual.png`, while the PR checklist says "no screenshots committed unless justified as
durable baselines".

**The marker should be added, and this directory is already in the shape that satisfies
it.** The reasoning:

1. **The premise of 090's re-scope does not survive inspection.** The repo already tracks
   **126 PNGs** under `.shux/qa/`, including `*-actual.png` and `*-diff.png` for VT tasks
   067–074 and 078–083 — every task that carries the marker. Committing gate PNGs under
   `.shux/qa/<task>/` is the established norm for this gate, not an exception to it.
2. **The two rules are about different artifacts.** The checklist line governs *PR review*
   attachments — animated before/after shots, contact sheets, one-off screen recordings —
   which are transient and belong in a comment. CLAUDE.md's carve-out is for "a true
   baseline/golden with task documentation": a PNG referenced from `evidence-manifest.json`,
   named `-actual`/`-expected`/`-diff`, regenerable from a documented harness, and
   pixel-compared at exact thresholds is exactly that.
3. **The baseline here is not self-minted, which is what the carve-out is guarding
   against.** Every `-expected.png` in this directory is the same stream with `CSI n b`
   replaced by n literal copies of the character — ECMA-48's own definition of REP, shot
   through the same binary. No expected image was drawn by the implementer or by this
   gate to match an outcome.
4. **The alternative leaves the gate unenforced.** 090 chose the other resolution and the
   result is that a task touching `shux-vt`'s print path, capture, wide cells, grapheme
   clusters, scroll regions and the alternate screen can be marked Done with no evidence
   requirement at all. That is the failure mode #123 describes.

Concretely: add `**Quality Gate:** shux-vt-solid-qa` to the task header alongside
`**QA record:** .shux/qa/091-rep-data-stream-source/SOLID-QA.md`, and commit this
directory. `check-progress.sh` then finds the report with `VERDICT: PASS` on line 1, the
manifest with all six required keys, tracked `*-actual.png` screenshots, and pixel metric
JSONs at exact 0/0 thresholds — all of which are present.

Two mechanical notes for whoever lands it, both already handled here:

* `dootsabha_design` and `dootsabha_implementation` must be **relative file paths**, not
  objects — `check-progress.sh` runs `jq -r ".$key"` and then requires the result to be a
  tracked file. Task 090's manifest uses objects, which is one reason it could not carry
  the marker. This manifest points at `dootsabha-design.md` and
  `dootsabha-implementation.md`.
* `negative-control-base-rep-vs-literal-80x24.json` is **excluded** from `pixel_metrics`
  by design: the check requires every listed metric to be `status == "pass"`, and this one
  must be `fail`. It is referenced from the manifest's `negative_control` block instead.

Verified by adding the marker to a scratch copy of the task file and running the
enforcement path against this directory — see §11.

## 11. Enforcement dry-run

Not asserted from reading the script. Actually run, in both directions.

**Today, without the marker.** `bash scripts/check-progress.sh` → exit 0, and
`make check-vt-qa` → exit 0. Both pass *without looking at task 091 at all*, which is
issue #123 in one line.

**With the marker, evidence staged.** The marker was added to the task file, this
directory staged with `git add -N`, and `check-progress.sh` re-run:

```
=== dry-run WITH marker, evidence staged ===
rc=0
```

`check_vt_qa_artifacts` resolved every artifact: `SOLID-QA.md` line 1 exactly
`VERDICT: PASS`; the manifest's six required keys; `.task` equal to the directory name;
a non-empty `screenshots` array containing `*-actual.png` entries; a non-empty
`pixel_metrics` array whose every entry is `status == "pass"` at exact 0/0 thresholds;
`dootsabha_design` and `dootsabha_implementation` resolving to tracked files.

**And proven able to fail, twice.** A gate that has only been seen passing is not a gate:

```
=== neg-test 1: verdict line changed to FAIL ===
  - VT Task 091-rep-data-stream-source QA gate report must start exactly with 'VERDICT: PASS'

=== neg-test 2: negative control listed under pixel_metrics ===
  - VT Task 091-rep-data-stream-source pixel metric
    .shux/qa/091-rep-data-stream-source/negative-control-base-rep-vs-literal-80x24.json
    did not pass (.status != "pass")
```

Neg-test 2 is the concrete reason the negative control is excluded from `pixel_metrics`
rather than merely commented: listing it there *does* break the build, and now that has
been demonstrated instead of predicted.

The task file was restored to its committed state immediately afterwards — the marker is
**not** applied on the branch. Adding it is the operator's call; §10 is the
recommendation, and §11 is the proof that it works if taken.

---

## Addendum — landed after the verdict, by the implementer

The `VERDICT: PASS` above was rendered against `8f5ebf8`. The following landed
afterwards, in response to this report, and is **not** covered by that verdict. Each is
recorded here so a re-run has a checklist rather than a diff to rediscover.

**P3-1 and P3-2 closed, both proven able to fail first.** The gate's own two surviving
mutations are now in the committed battery (18 total, 0 survivors):

| Gate mutation | Now killed by |
|---|---|
| `scalar budget loses its .max(1) floor` | `a_cluster_longer_than_the_budget_still_writes_one_whole_copy` |
| `a wide char dropped for want of room is not remembered` | `a_wide_character_with_nowhere_to_go_is_still_what_rep_repeats` |

The first attempt at the second mutation was wrong in a way worth recording: setting
`*self.last_graphic = None` on the drop makes REP a no-op, which is **visually identical
to the correct behaviour**, so it survived and looked like a hole in the new test. The
faithful mutant is failing to *record* the dropped character at all, which leaves the
OLDER one in place for REP to draw. A mutation that cannot be distinguished from correct
behaviour is not a mutation.

**P3-1's prose corrected** in `repeat_iterations`' doc comment and in the task file: the
bound is `max(2 * rows * cols, MAX_GRAPHEME_SCALARS)`, not two screenfuls flat.

**P3-3 corrected**: the task's testing matrix now says 64.

**P2-1 fixed — this is the load-bearing one.** `.shux/scripts/issue_122_mutation_check.sh`
rewrote a tracked source file in place for several minutes with no mutual exclusion, so
anything else compiling the workspace saw a mutant and reported a failure that did not
exist. That is precisely the "never mask, and never manufacture, a failure in a
measurement harness" rule, and it cost this audit a near-miss phantom P0. The script now
takes an exclusive `flock` on `.git/shux-mutation-check.lock` and **refuses to run** if it
cannot get it. Proven in both directions rather than asserted: a second invocation while
the lock is held exits 1 with a diagnostic, and the same invocation proceeds normally once
the holder releases.

**The `Quality Gate:` marker is now on the task file**, with this directory as its
evidence, and `scripts/check-progress.sh` returns 0. The gate's reasoning for adding it
was independently confirmed: the marker's requirements are satisfiable because this run
produced labelled metrics, a conforming manifest, and `*-actual.png` frames.

A re-run of the gate should focus on the two new tests and the harness lock; nothing else
in the audited surface changed.
