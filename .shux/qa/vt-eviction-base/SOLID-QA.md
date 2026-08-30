VERDICT: PASS

# SOLID VT QA — `vt-eviction-base` (re-audit)

| | |
|---|---|
| Branch | `claude/shux-image-support-s0u4uy` |
| Frozen HEAD | `e114e4c` |
| Base | `9793961` (`git merge-base origin/main HEAD`) |
| Diff identity | `sha256(git diff 9793961..e114e4c)` = `987ff44fdf455ef0c486c28eb0e9e0bd4ed324274ee64e4e57b5167b2710ed27` |
| Range | `f01c954` · `6172c24` · `15fd835` · `fd3e1c8` · `e114e4c` |
| Council tool | `dootsabha` NOT installed; substitution recorded in `council-substitution.md` |

The diff hash was taken before the first measurement and re-checked after the
last; it did not move, and `git status --porcelain` was empty at both points.
Nothing in `crates/` was edited by this audit. Every mutation experiment ran in
throwaway `git worktree`s outside the checkout.

## 1. Acceptance criteria, from the commits themselves

| # | Criterion (source) | Verdict | Evidence |
|---|---|---|---|
| A1 | `f01c954`: a viewport-only clone advances the base by the history it drops | PASS | `grid::tests::a_viewport_clone_advances_the_eviction_count_by_the_history_it_drops` compiled and in the 2238-test run |
| A2 | `6172c24`: doc-only; the repaint-state block moves onto the function it describes | PASS | diff contains no non-comment `+`/`-` line; block is accurate on `clone_presented_viewport` (`impl Clone` really does reset `dirty`, the viewport clone really does keep it) |
| A3 | `15fd835`: `reset_blank` returns the base to a fresh grid's | PASS | `reset_blank_returns_the_eviction_base_to_a_fresh_grids`, `a_recycled_alternate_screen_buffer_does_not_alias_the_previous_session` |
| A4 | `15fd835`: `is_blank_canvas` may be left unchanged — no grid with a moved base reaches it | PASS (conclusion) / see F3 (stated reason) | 1,737,924-step counterexample search, zero hits |
| A5 | `fd3e1c8`: the recycling oracle can now see the base, and bites | PASS | reverting `15fd835`'s single line fails the oracle at step 4 on `?1049h` |
| A6 | Whole range is a visual no-op on every render path | PASS | 27/27 byte-identical A/B artifacts, 20 pixel metrics at exact `0`/`0` |
| A7 | Council evidence exists for design and implementation diff | PASS | `council-substitution.md`, audited against the diff below |

## 2. Testing matrix

| Layer | Command / harness | Result |
|---|---|---|
| Unit + integration | `make test` | 2238 run, **2238 passed**, 2 skipped (`.shux/out/vt-eviction-base/make-test.log`) |
| Lint | `make lint` | clean (`.shux/out/vt-eviction-base/lint.log`) |
| New tests really in the suite | `cargo nextest list -p shux-vt` | all four new `grid::tests::*` plus both `alt_screen_differential` tests enumerated |
| Raw byte / replay | `make test-vt-corpus` | 3 replay tests + harness passed; 5 real-TUI raw PTY recordings (`btop`, `lazygit`, `nvim`, `vicaya`, `vivecaka`) replayed |
| Oracle mutation | proptest against a tree with `15fd835` reverted | **FAILS** at step 4, `Feed(\x1b[?1049h)`, identical frame digest `295380c2…`, `eviction_base: 1` vs `0` |
| Counterexample search | instrumented `ScreenSwap` + direct `alt_spare` inspection | 0 hits in 1,737,924 steps |
| shux automation | 3 binaries × 3 breakpoints × 5 scenes, real daemon | 27/27 + 33 artifacts compared |
| Live rich TUIs | `nvim`, `btop --utf-force`, `lazygit`, `vicaya-tui`, `vim` through real panes | all render correctly; `vivecaka` unavailable (see F4) |
| Visual inspection | full-resolution PNGs opened | no clipping, tofu, ghost cells, colour bleed, cursor artifacts |
| Pixel verification | `.claude/automations/pixel_verify.py` | 20 metrics, all `pass` at `0`/`0` |
| Leak proof | `.shux/scripts/no_leak_guard.sh` + pidfile | zero daemons; `ps` needle deliberately not used (only this agent's own argv matches `shux`) |

## 3. The three claims that needed real work

### 3.1 `fd3e1c8`'s oracle bites (reproduced, not accepted)

In a `git worktree` at `e114e4c` with `15fd835`'s single line
(`self.evicted = 0;` in `reset_blank`) removed and **nothing else changed**,
`recycling_a_retired_buffer_is_unobservable` fails:

```
left:  Observed { frame: "295380c2a3954ca95519b208ffec2e198f4880c06836fdfc6aa7eb93751720fc", …, eviction_base: 1 }
right: Observed { frame: "295380c2a3954ca95519b208ffec2e198f4880c06836fdfc6aa7eb93751720fc", …, eviction_base: 0 }
state diverged at step 4 after Feed([27, 91, 63, 49, 48, 52, 57, 104])
```

`[27,91,63,49,48,52,57,104]` is `ESC [ ? 1 0 4 9 h`. Frame digest, scrollback,
`total_lines`, revision, title, alt flag, cursor and dirty regions all agree —
the base is the only difference, which is exactly what `fd3e1c8` was added to
see. The guard test `the_two_arms_take_different_allocation_paths` passed in the
same run, so the two arms really do take different code paths.

### 3.2 No grid with a moved base reaches `is_blank_canvas`

A `panic!` was wired into the real recycling arm of `ScreenSwap::enter`, and the
real `VirtualTerminal::alt_spare` slot was inspected after **every** operation
for `is_blank_canvas(rows, cols, alt_grid_config()) && evicted() != 0`.

| search | programs | steps | steps with a spare parked | steps with a spare whose base ≠ 0 | real reuses | counterexamples |
|---|---|---|---|---|---|---|
| exhaustive, depth 4, 21-op alphabet | 194,481 | 777,924 | 20,610 | 555 | 804 | **0** |
| randomised, depth 24, LCG-seeded | 40,000 | 960,000 | 159,951 | 29,969 | 5,563 | **0** |

The alphabet includes `?1049h/l`, `?1047h/l`, `?1048h/l`, RIS, DECALN, `2J`,
`3J`, `?2026h/l`, DECSTBM, scrolling writes, `10S`/`10T`, four resizes (row-only
and column-reflowing) and `clear_scrollback`. The 30,524 steps with a
non-zero-base spare are the load-bearing number: spares with moved bases really
do occur, they are simply always rejected by the write tally.

Static confirmation: `evicted` moves in exactly four places
(`grid.rs:983,995` scroll eviction — both bump `mutations`; `1258`
reflow-drop; `1297` `clear_scrollback`). The grid that becomes the spare is
always the live ALTERNATE grid, and `VirtualTerminal::resize` gives that grid
`resize_canvas`, never `resize_reflowing_columns` — so `1258` is unreachable for
it. `1297` needs `scrollback_len() > 0`, which on an `max_scrollback: 0` alt
grid needs non-blank rows retained by a shrink, which needs writes. The two new
base-advancing clone paths produce grids that never enter the slot: `alt_spare`
is written in exactly one place (`screen.rs::leave`), and the frozen
`clone_presented_viewport` grid is only ever dropped, never installed.

### 3.3 The vim/alt-screen evidence cannot see `15fd835`

A third binary was built from `e114e4c` with `15fd835`'s line reverted and run
through the identical scene set. **All 27 artifacts are byte-identical to the
fixed binary**, including four real `vim` enter/exit rounds per breakpoint.

The council record says this itself, and it is right. `evicted` has exactly
three readers (`sync.rs:99`, `lib.rs:707`, and `fd3e1c8`'s own accessor at
`lib.rs:307`). The only consumer path, `presented_history_len`, multiplies the
base delta by `frozen.history_len`, and an alternate-screen grid is built with
`max_scrollback: 0`, so that term is always `0`. `15fd835` is therefore a
**latent-invariant fix**: through the production API it is unobservable today,
and the differential's `eviction_base` is the only instrument that sees it. The
vim evidence is a no-regression result, not proof the fix works — cited that way
here.

## 4. Screenshot matrix

Base leg = `9793961` binary, head leg = `e114e4c` binary, unfixed leg =
`e114e4c` with `15fd835` reverted. Scenes are deterministic by construction (no
clocks, no timestamps, fixed file paths).

| Scene | 80x24 | 120x40 | 200x60 | What it shows |
|---|---|---|---|---|
| `history` | identical | identical | identical | 6000 colour-probed lines into a 5000-line scrollback |
| `altlive` | identical | identical | identical | 5th alternate-screen entry, buffer recycled 4 times |
| `altback` | identical | identical | identical | primary screen after 5 alt enter/leave cycles |
| `vim4` | identical | identical | identical | real `vim -u NONE`, 4th entry, text typed into the recycled buffer |
| `copymode-top` | identical | identical | clock-only delta | attach client copy mode at the top of retained history |

`copymode-top-200x60` differs between legs by exactly one character —
`up 11s` vs `up 10s` in the attach status bar. That arm is deliberately kept in
scratch and carries no committed metric, per `.shux/qa/README.md`.

Baseline-backed pixel metrics (`expected` is a tracked golden):

| Case | Actual | Baseline | Result |
|---|---|---|---|
| `rich-tui-nvim` | `corpus-rich-tui-nvim-actual.png` | `.shux/goldens/073-vt-corpus/rich-tui-nvim-expected.png` | 0 changed px of 738,720 |
| `rich-tui-btop` | `corpus-rich-tui-btop-actual.png` | `.shux/goldens/073-vt-corpus/rich-tui-btop-expected.png` | 0 changed px of 738,720 |
| `synthetic-alternate-screen` | `corpus-synthetic-alternate-screen-actual.png` | `.shux/goldens/073-vt-corpus/synthetic-alternate-screen-expected.png` | 0 changed px of 54,720 |
| `rich-tui-lazygit` / `vicaya` / `vivecaka` | scratch | tracked goldens | 0 changed px each |

## 5. Scrollback was genuinely exercised

`f01c954` is a no-op at `scrollback_len() == 0`, so the scenes push 6000 lines
through a 5000-line scrollback. `pane capture` is viewport-only
(`clone_visible`), so retained history was read the way a user reads it — the
attach client's copy mode, `prefix [` then `gg`:

| breakpoint | oldest line still reachable | lines evicted |
|---|---|---|
| 80x24 | `history-line-0982` | ~981 |
| 120x40 | `history-line-0983` | ~982 |
| 200x60 | `history-line-0991` | ~990 |

Identical on both legs. Colours survive into scrollback (truecolor `RGB`,
basic `GREEN` both rendered in the copy-mode frame).

## 6. Colour probes

Every daemon-backed capture carries truecolor (`38;2;…`), 256-indexed
foreground (`38;5;208`), basic ANSI (`34`/`32`/`36`), bold+truecolor, and an
indexed BACKGROUND run (`48;5;236`). Verified present in the captured text and
visible in the opened PNGs, so a monochrome or `NO_COLOR` regression could not
have passed.

## 7. Findings

### P3 — `15fd835`'s stated reason for leaving `is_blank_canvas` alone is inexact

The commit message says a resize "does not [bump the tally], but
`VirtualTerminal` drops the spare outright when dimensions change, so a resized
grid never reaches the check at all"; the same claim sits in the doc comment on
`a_grid_that_looks_blank_to_the_recycling_check_counts_from_zero`. A resized
grid **does** reach the check. Measured against the real `VirtualTerminal`:

```
enter alt (?1049h) → resize(3,10) → resize(6,10) → leave alt (?1049l)
  spare.is_blank_canvas(6,10,alt_cfg) = true      spare.evicted() = 0
```

`dims_changed` clears the *parked* spare; it does not stop the *live* alt grid
from being resized and retired afterwards. The conclusion survives for a
different reason — the live alt grid is only ever `resize_canvas`'d, and that
function never touches `evicted` — but the argument as written is false, and it
is the second time this rationale has been corrected (the council record already
fixed the analogous `clear_scrollback` sentence as its own P3). Not a defect:
the pinning test asserts the *implication*, not the argument, which is why the
wrong reasoning cannot make the test wrong.

### P3 — `eviction_base()`'s doc does not match what it returns

`lib.rs:307` documents "Absolute index the **live** grid is numbering its rows
from" but returns `self.grid().evicted()`, and `grid()` prefers the FROZEN grid
while a synchronized-output window is open. After `f01c954` the frozen grid's
base is `live_evicted + scrollback_len_at_freeze`, so under `?2026h` — which the
differential's alphabet emits — the accessor returns a different number from the
one its rustdoc promises. The oracle stays sound (both arms read the same thing,
and it demonstrably still bites), but this is verification machinery and the
contract on it should be exact.

### P3 — the council record's numbers are unreproducible from the repo

`council-substitution.md` cites 24,000 differential steps, 921 alt-screen
entries, a 712/712/0 divergence table, 52/52 artifact pairs and 24/24 PNGs at
`0.000000`. None of those harnesses are committed, so none can be re-run. Its
falsifiable claims all held when I tested them independently — the oracle
reproduction (§3.1) matches its description exactly, the "vim cannot see this
fix" caveat is correct (§3.3), and its caller enumeration is right (`evicted()`
has exactly the readers it names, at `lib.rs:694` + the 13 lines `fd3e1c8` later
inserted above it = `lib.rs:707`) — so this is a provenance weakness, not a
falsehood. It also names `1ff6e72` as the fix commit; that object exists only as
a dangling pre-rebase blob and is in no ref, the shipped commit being `15fd835`.

### P3 — `make test-vt-corpus` dirties tracked files

Running the committed corpus gate rewrites 19 tracked PNGs under
`.shux/qa/073-shux-vt-corpus-regression-harness/` with re-encoded (2228-byte vs
2944-byte) all-zero diff images, so a clean checkout goes dirty just from running
a gate. Pre-existing and outside this diff; restored with `git checkout --` and
the frozen tree re-verified clean. Recorded because a QA gate that mutates
tracked evidence is a trap for the next audit.

### P3 (pre-existing, NOT caused by this diff) — a styled trailing space is intermittently lost

The first A/B run showed a 171-pixel (exactly one cell) difference on
`history-120x40`: the trailing space of a `\033[48;5;236m BGIDX \033[0m` run
sometimes lands with default background instead of indexed 236. Repeated 31 times
per binary, it appears on **both** legs — 4/31 on `9793961`, 3/31 on `e114e4c` —
and is stable within a run (three consecutive glances of the same quiescent pane
always agree), so it is fixed at parse time rather than being a read race. Not
attributable to this diff, and mechanically impossible for it to be: `evicted` is
`pub(crate)` and unreachable from `shux-raster`. The committed scenes drop the
trailing styled space so the evidence is deterministic; the raw observation is in
`.shux/out/vt-eviction-base/repeat3/`. Worth a separate issue.

### P3 — my own first harness silently skipped settling

Recorded because it nearly cost a wrong verdict: `shux pane wait-settled` takes a
positional pane id, not `-s`, so every call exited 2 and a `|| true` swallowed it
— the exact failure shape CLAUDE.md warns about. Instrumenting the exit status is
what surfaced it. The final harness checks the status and the flake rate
attribution above was re-measured afterwards.

Nothing at P0/P1/P2.

## 8. Passed evidence

- `make test` 2238/2238; `make lint` clean; `make test-vt-corpus` clean.
- Oracle proven able to fail against the unfixed tree, for the right reason.
- 1,737,924-step counterexample search against the real `ScreenSwap`, zero hits.
- 27/27 byte-identical daemon artifacts across `9793961`, `e114e4c` and the
  unfixed variant, at 80x24 / 120x40 / 200x60.
- 20 pixel metrics at exact `0`/`0`, six of them against tracked goldens.
- Real `vim`, `nvim`, `btop --utf-force`, `lazygit`, `vicaya-tui` driven live
  through real panes and inspected as images.
- Scrollback eviction demonstrated at ~981 lines through copy mode.

## 9. Residual risk

- `15fd835` is unobservable through today's production API; its value is the
  invariant it restores for the absolute-anchor consumer that
  `docs/designs/inline-images.md` is about. If that consumer never lands, the
  fix is protecting nothing user-visible — but it is still correct.
- The council record is prose over uncommitted harnesses (P3 above).
- The trailing-styled-space flake is unexplained and pre-existing.

## 10. Cleanup

Every daemon-backed run was wrapped in `.shux/scripts/no_leak_guard.sh` and all
exited `0`. Each harness asserted an empty pidfile at
`$XDG_RUNTIME_DIR/shux/shux.pid` on exit and removed its isolated short runtime
dir. Post-audit `shux_daemon_pids` is empty. `ps | grep shux` matches only this
agent's own argv, which is why the pidfile is the authority here. Scratch
worktrees `wt-base`, `wt-probe` are outside the checkout; the audited tree is
clean and `HEAD` is still `e114e4c`.
