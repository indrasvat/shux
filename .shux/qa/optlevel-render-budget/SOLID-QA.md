VERDICT: PASS

# SOLID VT QA — `optlevel-render-budget`

The load-bearing question — *is rendering output byte-identical between
`[profile.test] opt-level = 0` and `= 1`?* — is **established, with 35 exact
(0/0) pixel metrics and byte-identical text captures** across the committed VT
corpus, five recorded rich-TUI PTY streams, and three viewport breakpoints
driven through a real daemon.

**This report was issued FAIL first.** Round 1 found one P1: the diff re-scoped
two of the three wall-clock render assertions and left the third asserting the
PRD 8 ms release budget on the build it had just made unoptimised. That is now
fixed and re-verified (§11). The verdict below is round 2. The round-1 findings
are kept in §7 rather than rewritten, because a gate that erases what it caught
is a gate nobody can audit.

## 1. Change under audit

| | |
|---|---|
| Branch | `perf/optlevel-probe` |
| HEAD | `25a2771` — *test(ui): stop asserting a release budget on a debug build* |
| Base | `d308a0c` (`git merge-base origin/main HEAD`) |
| Also in range | `880b864` chore(ci): probe opt-level 0 against the fixed handshake deadline |
| Issue | #157 (opt-level probe), #160/#161 (handshake deadline, already fixed on main) |
| Audited worktree | `/tmp/qopt-wt` (detached at `25a2771`); the shared checkout was never moved |

**Characterisation, verified independently** (`git diff d308a0c..HEAD`), not taken
from the request:

| File | What changed | Ships? |
|---|---|---|
| `Cargo.toml` | `[profile.test] opt-level` 1 → 0 (+2 comment lines) | test binaries only; `[profile.release]` untouched |
| `crates/shux-ui/src/compositor.rs` | `8000` → `100_000` us + comment, at lines 1280-1305 | **no** — `#[cfg(test)] mod tests` opens at line 726, file is 1306 lines |
| `crates/shux-ui/tests/compositor_tests.rs` | `8000` → `100_000` us + comment | no |
| `.github/workflows/optlevel-probe.yml` | new, temporary, deleted before merge | no |

No shipping rendering code changed. Confirmed by reading the hunks, by the
`mod tests` line number, and independently by both councils.

The gate is genuinely owed: `scripts/check-vt-qa.sh` lists
`crates/shux-ui/src/compositor.rs` in `VT_PATHS` (the snapshot-composition half
of the pipeline). `crates/shux-ui/tests/compositor_tests.rs` is exempt under the
`*/tests/*` suffix rule, but the `src/` file alone triggers the requirement.

## 2. Stated DoD matrix

The five items the change asked this gate to establish.

| # | Required | Verdict | Evidence |
|---|---|---|---|
| 1 | Rendering byte-identical between opt-level 0 and 1 | **PASS** | §4 rows A-D; 35 × `pixel_verify.py` at exact `0`/`0`; all text captures `cmp`-identical |
| 2 | Real coloured workloads + real TUIs; truecolor + indexed + basic probes | **PASS** | §5 screenshot matrix; 5 real-TUI raw replays; 16-colour fg/bg, 256-indexed fg/bg, truecolor fg/bg all probed |
| 3 | Pixel-verify against committed baselines | **PASS** | all 19 corpus renders byte-identical to `.shux/goldens/073-vt-corpus/` at both opt levels |
| 4 | Zero leaked daemons; isolated short `XDG_RUNTIME_DIR` | **PASS** (with a pre-existing caveat) | §8; audit daemons under `/tmp/qopt/r0`, `/tmp/qopt/r1`, both stopped |
| 5 | Judge the 100 ms ceiling | **Answered — 100 ms stands** | §6; F1 (§7) was the round-1 FAIL and is now fixed — see §11 |

## 3. Testing matrix

| Layer | Status | What was run |
|---|---|---|
| Unit tests | PASS | `make test` × 3 (opt0 ×2, opt1 ×1): **2120 run / 2120 passed / 2 skipped**, identical at both opt levels, zero `FAIL` lines |
| Integration tests | PASS | same run; includes `shux-ui` `compositor_tests`, `shux-vt` `alt_screen_*`, `decaln`, `rep`, `scroll_region_bounds`, `sync_output_*`, `cow_aliasing_adversarial`, `wide_invariants` |
| Raw byte / replay | PASS | committed fixtures `.shux/fixtures/vt-corpus/{synthetic,rich-tui}` — 14 synthetic + 5 recorded rich-TUI PTY streams (btop, lazygit, nvim, vicaya, vivecaka), replayed through both arms |
| shux automation | PASS | two isolated daemons from `--profile test` binaries; `pane set-size`, `pane wait-for`, `pane wait-settled`, `pane capture`, `pane snapshot`, `window snapshot`, `session kill`, `daemon stop` |
| Visual inspection | PASS | full-resolution PNGs opened as images (colour probe 80x24; btop window composite); 6x nearest-neighbour crop of the status-bar divergence |
| Pixel comparison | PASS | 35 metrics, all `status: pass`, `max_pixel_diff_ratio: 0`, `max_mean_channel_delta: 0`, `changed_pixels: 0` |
| DootSabha design | RUN — **conditional approve** | `dootsabha-design.md` (providers: codex ok, agy error; chair synthesised) |
| DootSabha diff review | RUN — **reject as written** | `dootsabha-implementation.md` (providers: codex ok, agy ok) |

Both councils were run by this gate, from the isolated worktree, not reused from
the implementer.

## 4. How byte-identity was established

The variable under audit is `[profile.test] opt-level`. Every arm below is
compiled **under that exact profile**, flipped only by
`CARGO_PROFILE_TEST_OPT_LEVEL`; the build logs prove the flip landed
(`Finished \`test\` profile [unoptimized + debuginfo]` vs `[optimized + debuginfo]`).
Separate `CARGO_TARGET_DIR`s (`/tmp/qopt-t0`, `/tmp/qopt-t1`) so the arms cannot
share artifacts.

**Row A — VT → raster, committed corpus.**
`cargo run --profile test -p shux-raster --example vt_corpus_harness -- --mode verify`
over the whole committed corpus, at each opt level. 19 cases: `plain-crlf`,
`wide-cjk`, `wide-cjk-ansi-dec-edit`, `grapheme-storage-current`,
`dec-special-graphics`, `tabs-current`, `tab-stops`, `origin-response`,
`origin-scroll-region`, `osc-default-colors`, `alternate-screen`,
`scroll-region`, `sync-output`, `resize-smoke`, and the five rich-TUI
recordings. Result: **all 19 `-actual.png` and all 19 `-actual.txt` byte-identical
between arms, and byte-identical to the committed goldens** in
`.shux/goldens/073-vt-corpus/`. The two `corpus-report.json` files are identical
once the output-directory prefix is normalised. 19 pixel metrics at 0/0.

That single row is the answer to the question this PR turns on, and it covers
the exact defect classes the gate hunts: wide-cell head/tail, combining marks,
DEC line drawing, tab stops after TBC, origin mode inside scroll margins, alt
screen entry/exit, synchronized output, OSC default colours, and resize reflow.

**Row B — end-to-end through the daemon, `pane.snapshot`.**
`cargo build --profile test --bin shux` at each opt level, then a full daemon per
arm on its own short `XDG_RUNTIME_DIR`. Colour probe at 80x24 / 120x40 / 200x60,
plus the five rich-TUI raw recordings replayed into a live pane at their native
120x36. **8/8 pane snapshots at 0 changed pixels; 8/8 `pane capture` text
`cmp`-identical.**

**Row C — end-to-end through `shux_ui::compose`, `window.snapshot`.**
Same arms. This is the path the changed file actually belongs to.
**8/8 at 0 changed pixels** once one cell is excluded — see below.

**Row D — self-determinism control.**
Two consecutive `pane.snapshot` calls against the same daemon, at all three
breakpoints, both arms: byte-identical. Without this control a 0/0 result proves
nothing about whether the comparator can see a difference at all.

### Two harness defects found and fixed mid-audit — recorded, not hidden

Both produced non-zero diffs that had nothing to do with codegen. Reporting them
because the first version of this audit would have called them regressions.

1. **Status-bar clock.** Every `window.snapshot` differed by exactly 87 (later
   145) pixels in one cell, row 35, cols 118-119. Cropped at 6x and read as an
   image: `00:19` vs `00:20` — the status bar's elapsed-session counter. The
   arms ran seconds apart. Re-verified with the right-hand 20 px column excluded
   (`0,0,1060,684`): **0 changed pixels, 8/8**. `window.snapshot` is therefore
   pixel-unstable by construction in its right-hand status zone; per the
   evidence contract that arm is narrowed rather than thresholded.
2. **Replay race.** The first TUI-replay arm called `pane set-size` *after*
   `cat`ing the recording had already begun, so a 120x36 stream reflowed through
   an 80x24 grid at a nondeterministic point. `tui-vicaya` came back 605 229
   pixels apart and 24 rows tall instead of 36. Fixed by gating the `cat` on a
   file the driver touches only after the resize acks. Post-fix: 0 changed
   pixels for all five. **This was my harness, not the product** — and it is a
   good illustration of why "screenshot-diffing animated TUIs measures capture
   timing, not rendering".

## 5. Screenshot matrix

Everything below is `.shux/out/optlevel-render-budget/` (gitignored scratch);
the committed evidence is the metric JSON, which is correct here because
**neither side of any comparison is a repo-tracked baseline** — so no
`-actual.png` is owed, and committing one would add a binary nothing can be
diffed against.

| Viewport | Content | Path (pane) | Baseline | Diff | Status |
|---|---|---|---|---|---|
| 80x24 | colour probe (16/256/truecolor fg+bg, SGR, CJK, ZWJ/combining, DEC, tabs) | `shots/probe-80x24-pane-opt{0,1}.png` | opt0 arm | `metrics/probe-80x24-pane-diff.png` | 0/0 pass |
| 120x40 | colour probe | `shots/probe-120x40-pane-opt{0,1}.png` | opt0 arm | ↑ | 0/0 pass |
| 200x60 | colour probe | `shots/probe-200x60-pane-opt{0,1}.png` | opt0 arm | ↑ | 0/0 pass |
| 120x36 | btop (raw PTY replay) | `shots/tui-btop-pane-opt{0,1}.png` | opt0 arm | `metrics/tui-btop-pane-diff.png` | 0/0 pass |
| 120x36 | lazygit / nvim / vicaya / vivecaka (raw PTY replay) | `shots/tui-*-pane-opt{0,1}.png` | opt0 arm | `metrics/tui-*-pane-diff.png` | 0/0 pass ×4 |
| 120x36 window | all of the above through `window.snapshot` | `crops/*-noclock-opt{0,1}.png` | opt0 arm | `metrics/*-noclock-diff.png` | 0/0 pass ×8 |
| corpus (19 sizes) | committed VT corpus, both arms | `corpus-opt{0,1}/*-actual.png` | opt0 arm + committed goldens | `metrics/*-diff.png` | 0/0 pass ×19 |

Opened as images, at native resolution:

- `shots/probe-80x24-pane-opt0.png` — 16 basic fg + 16 basic bg, 16 indexed fg +
  16 indexed bg, 10 truecolor fg + 10 truecolor bg, bold/italic/underline/reverse/
  strike/dim with a clean `after-reset-plain`, DEC special graphics rendered as
  box-drawing (not as `lqqqk` letters), and `TABS:` showing `a` then the
  remaining tabs collapsing to the right margin after `CSI 3 g` — correct. No
  colour bleed past any SGR reset, no ghost cells.
- `shots/tui-btop-window-opt1.png` — full window composite: rounded borders,
  pane title, braille meters, Nerd Font glyphs, per-core colour ramps, status
  bar. No tofu, no clipping, no wrap corruption.
- The two 6x status-bar crops that identified the clock.

Pre-existing font-coverage limits visible in the probe and **identical in both
arms**, so not attributable to this diff: CJK renders as replacement boxes and
combining marks render spacing. Both are the documented renderer-v2 gap
(bundled font chain has no CJK/system-font discovery). Noted as F4, not counted
against this change.

## 6. Is 100 ms defensible? — measured, then judged

Measured on this machine (10-core, `n=30` per cell, `total_time_us` straight off
`RenderStats`, via a throwaway probe test compiled into the isolated worktree at
both opt levels — never into the shared checkout):

| Case | cells | opt1 p50 | opt0 p50 | opt0 max | ratio |
|---|---|---|---|---|---|
| `render_frame` 80x24 | 1920 | 169 us | 715 us | 778 us | 4.2x |
| complex 4-pane 80x24 | 1920 | 198 us | 793 us | 868 us | 4.0x |
| `single_pane` 10x5 | 50 | 6 us | 25 us | 39 us | 4.2x |

Under load (`make test` saturating the box, load avg 5-9) the opt0 numbers moved
to p50 748 / 890 / 26 us — this machine has too much headroom to model a
contended 4-core runner, so the CI figures in the PR remain the operative ones.

**Judgement: 100 000 us stands. I do not ask for it to change.** It is 9.65x the
worst opt0 observation on CI (10 365 us) — essentially the 10x-headroom rule, and
the right shape for something explicitly relabelled a catastrophic-regression
guard. I **disagree with both councils' counter-proposals**: codex asked for
25 000-30 000 us, which is 2.4-2.9x the worst observation — that is the same
marginal shape (1.03x) that made the 8000 us bound flaky in the first place, and
it would buy a re-run of this whole exercise in a month. If you want the 10x rule
to be exactly true rather than approximately, `125_000` is the number; that is a
preference, not a finding.

What the councils are right about, and what F1 below is really about: a
wall-clock `assert!` inside `cargo test` is the wrong instrument for a frame
budget, and the new comment's promise that "the real budget belongs to a
release-profile bench" currently points at nothing — `make bench` reports no
cargo bench targets exist. That is fine as a direction; it is not fine as a
justification for deleting the only 8 ms signal while leaving a third assertion
still claiming to be that signal.

## 7. Findings (as recorded in round 1)

### F1 — P1 — **FIXED, re-verified in §11** — the diff re-scoped 2 of 3 wall-clock render assertions

`crates/shux-ui/tests/compositor_tests.rs:57-58`, untouched by this diff, in one
of the two files this diff edits:

```rust
    // Render time should comfortably beat the PRD 8ms budget.
    assert!(stats.total_time_us < 8000);
```

`grep -rn "total_time_us <" crates/` returns exactly three sites. Two were
re-scoped to `100_000` with a comment explaining that an unoptimised test build
cannot carry the PRD budget. The third still asserts the PRD 8 ms release budget
— and after this diff it does so on an unoptimised build, which is precisely what
the commit subject says it stops doing.

Why P1 and not P2:

- **The diff changed this line's meaning without updating it.** Before the diff
  it was a release-ish measurement; after it, it is a debug measurement wearing a
  release claim. That is caused by this change, not inherited.
- **Headroom is not comfortable, contrary to the surviving comment.** Local opt0
  max is 39 us. Scaling by the PR's own CI factor for the 80x24 case
  (8260-10365 us on CI vs 715 us here ⇒ 12-14x) gives ~500 us, 16x under the
  bound. But the PR also states the 8000 us bound sat at 1.03x observed *at
  opt1* on CI — implying the CI tail reaches ~46x this machine, which puts the
  50-cell case at ~1800 us, i.e. **4.4x headroom in the tail**. Somewhere between
  4x and 16x, on the same runner class that already took a sibling assertion red.
- **Both councils flagged it independently**, before I looked, and each named it
  "the concrete missed inconsistency". The design council listed "default tests
  still claim to enforce the 8 ms frame budget" as an explicit reject condition.
- It is a defect in verification machinery, where CLAUDE.md says the correctness
  rule applies hardest.

**Remedy — and this matters: the remedy is NOT to drop `opt-level = 0`.** The
byte-identity claim behind `opt-level = 0` is proven, at 0/0, across 35
comparisons including five real TUIs and the whole committed corpus. Dropping it
would discard a measured -63 s/job for a defect it has nothing to do with. The
remedy is to bring the third site in line with the two the diff already changed:
raise it to the same ceiling and replace the comment with the same explanation.
Two lines. Then re-run this gate and it flips to PASS.

### F2 — P3 (downgraded from P2 in round 2) — a shipped doc comment mentions the 8 ms budget

`crates/shux-ui/src/compositor.rs:22-23`:

```rust
/// Statistics from the last render pass, used for performance monitoring
/// against the PRD section 14.1 budget (p50 <= 8ms).
```

Still present at the round-2 state, deliberately. **Downgraded to P3 and
explicitly not blocking**, on re-reading: this describes what the struct is
*for*, and `RenderStats` genuinely is the instrument you would monitor that
budget with. It does not claim any test enforces it, which was the actual defect
in F1. Leave it or reword it; either is defensible.

### F3 — P2 — `make test` exits 2 on the leak guard — **pre-existing, not this diff**

All three of my full-suite runs ended:

```
Summary [ 44.155s] 2120 tests run: 2120 passed (1 leaky), 2 skipped
shux leak guard: command left new orphan automation process(es): 43719
43719     1 S    sleep 9…
make: *** [test] Error 1
```

An orphaned `sleep 900` pane child (one of the `cli_contract` /
`pane_list_columns` / `shell_config_e2e` fixtures) survives with `ppid 1`.
**A/B'd rather than assumed:** it reproduces identically with
`CARGO_PROFILE_TEST_OPT_LEVEL=1`, i.e. on main's setting. Not attributable to
this change, and out of this gate's scope — but it means `make test` is red on
this machine for everyone right now, and someone should own it. (The same run
also caught a `~/.claude/hooks/peon-ping/scripts/notify.sh` orphan, so part of
what the guard sees is local environment noise.)

### F4 — P3 — pre-existing font-coverage gaps, identical in both arms

CJK → replacement boxes, combining marks → spacing marks in `pane.snapshot`.
Documented renderer-v2 limitation, byte-identical across opt levels, unrelated
to this diff. Recorded so a future reader of these PNGs does not re-report it.

### F5 — P3 — `.github/workflows/optlevel-probe.yml` must actually be deleted

The PR says it is removed before merge; it is present at `25a2771`. Not a defect
today, but nothing in the tree enforces it and a `push:`-triggered 90-minute
4-cell matrix left behind is expensive. Verify at merge.

## 8. Passed evidence

- 35 pixel metrics, every one `status: pass` with `max_pixel_diff_ratio: 0` and
  `max_mean_channel_delta: 0` and `changed_pixels: 0`, all produced by
  `.claude/automations/pixel_verify.py` during this audit.
- 19/19 corpus renders byte-identical to committed goldens at **both** opt levels.
- 16/16 text captures `cmp`-identical across arms.
- 2120/2120 tests pass at both opt levels, identical counts, zero FAIL lines.
- Comparator liveness proven twice by accident: the harness defects in §4 show
  the pipeline reports 87, 145, 33 538 and 605 229 changed pixels when something
  really differs. A 0/0 here is not a comparator that cannot see.

## 9. Residual risk

- The A/B is single-platform (darwin/arm64, 10 cores). Codegen-dependent
  rendering divergence, if it existed, would most plausibly show on a different
  target; the corpus + goldens run in CI on both ubuntu and macOS and would
  catch it there.
- `opt-level = 0` is a global slowdown of every test binary. The councils list
  timeout-sensitive tests it could pressure (`plugin_lifecycle` deadlines,
  `region_scroll_cost_is_linear_in_pane_height`, lens loop deadlines). All pass
  here, three runs, but a loaded 4-core CI runner is a harsher environment than
  this machine and this gate cannot simulate it. The PR's own 5-rep × 2-OS probe
  is the right instrument for that and should be read before merge.
- `make bench-lens-gate` and `make bench-test-suite` produce test-profile
  timings; their historical numbers stop being comparable after this change.
  Nothing asserts on them, so this is bookkeeping, not breakage.
- This verdict is pinned to `25a2771`. Uncommitted or subsequent edits are not
  covered.

## 10. Cleanup

- Audit daemons: `/tmp/qopt/r0` and `/tmp/qopt/r1`, both `shux daemon stop`ped by
  the arm scripts' `EXIT` trap and re-confirmed after the run.
- Every session created by the audit killed by name; `session list
  --include-scratch` empty in both runtimes before teardown.
- Council agents ran in `/tmp/qopt-wt`, a throwaway worktree of `25a2771`, so
  `--dangerously-skip-permissions` could not reach the shared checkout. Worktree
  removed after the audit; the timing probe test file lived only inside it.
- Scratch target dirs `/tmp/qopt-t0`, `/tmp/qopt-t1` removed.
- The shared checkout's HEAD was never moved and no tracked file outside
  `.shux/qa/optlevel-render-budget/` was written.
- Caveat, stated rather than papered over: this checkout is shared with other
  running agents, and `ps` during the audit showed several concurrent poll loops
  and four pre-existing `target/debug/shux __daemon` processes on temp sockets
  that were not started by this audit. F3's leak-guard finding is A/B'd against
  that noise; nothing else in this report depends on process counts.

## 11. Round 2 — F1 re-verification

Scope of this round, deliberately narrow: **F1 only.** A test-threshold and
comment edit cannot move a pixel, so §4's byte-identity work and the 35 metrics
are not invalidated by it and were not redone. What changed is confined to two
`assert!` bounds and three comments, which the diff below shows in full.

### The fix, read rather than taken on trust

`git diff` against the round-1 state, `crates/` only — two files, `+6 -4`, no
executable statement outside a test:

- `crates/shux-ui/tests/compositor_tests.rs:54-59` — `assert!(stats.total_time_us
  < 8000)` → `< 100_000`, and the comment "Render time should comfortably beat
  the PRD 8ms budget" → "Catastrophic-regression guard, not the PRD's 8ms budget
  — see the note on `test_performance_80x24_under_budget`". This is exactly the
  remedy F1 asked for: the third site now says what the other two say.
- `crates/shux-ui/src/compositor.rs:1281-1289` (inside `#[cfg(test)] mod tests`,
  which opens at line 726) — the comment no longer promises "the real budget
  belongs to a release-profile bench". It now states that nothing asserts the
  8 ms budget any more and that doing so needs a bench which does not exist yet.
  Correct: `make bench` reports no cargo bench targets, so the old wording
  pointed at nothing. It also records the measured ~4.2x opt0/opt1 ratio from §6.

### Consistency, checked mechanically

```
$ grep -rn "total_time_us <" crates/ --include=*.rs
crates/shux-ui/tests/compositor_tests.rs:59:    assert!(stats.total_time_us < 100_000);
crates/shux-ui/tests/compositor_tests.rs:307:        stats.total_time_us < 100_000,
crates/shux-ui/src/compositor.rs:1302:            stats.total_time_us < 100_000,
```

Three sites, three identical ceilings, three comments telling the same story. No
fourth site appeared.

### Green at both opt levels

| Run | Result |
|---|---|
| `make test-ui` (lib, opt-level 0) | 227 run / **227 passed** / 0 skipped |
| `cargo nextest run -p shux-ui`, `CARGO_PROFILE_TEST_OPT_LEVEL=0` | 235 run / **235 passed** / 0 skipped |
| `cargo nextest run -p shux-ui`, `CARGO_PROFILE_TEST_OPT_LEVEL=1` | 235 run / **235 passed** / 0 skipped |
| the three wall-clock tests alone, opt0, **×5 reps** | 3/3 passed every rep |

Identical counts at both opt levels, so nothing stopped being compiled. The five
reps are there because a wall-clock assertion that passes once has demonstrated
very little; §6's measured worst case at opt0 on this machine is 868 us against
a 100 000 us bound, ~115x headroom locally and ~9.65x against the worst CI
observation.

### What this PASS is pinned to

The fix arrived **uncommitted** in the shared checkout. `scripts/check-vt-qa.sh`
deliberately reads the committed range plus the index and ignores the unstaged
worktree, so this verdict is pinned to exact content, not to "whatever is in the
tree later":

```
473452f7d62886deddb1c63b335743d96bd18acb9d1f55f1655ded46ce494c83  crates/shux-ui/src/compositor.rs
cb032b038a87fe977f2dee2930b10fdabd9b46f91ec6642ac3dd869c9f90a9e7  crates/shux-ui/tests/compositor_tests.rs
```

Commit those two blobs unchanged and the verdict holds. Edit either before
committing and it does not — re-run the gate.

### Outstanding after round 2

Nothing blocking. F2 is downgraded to P3 and explained above. F3 (`make test`
exits 2 on an orphaned `sleep 900`) still stands and still reproduces at
opt-level 1, so it remains **pre-existing and out of this change's scope** —
but it is real, it is red on this machine today, and it wants an owner. F5 (the
temporary probe workflow) must actually be deleted before merge; nothing in the
tree enforces that.
