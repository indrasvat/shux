# Which tests a change runs: design note (#323)

## TL;DR

- **Selection is static and per test function**, worked out from the diff with
  ast-grep (`scripts/flake-gate.sh`, since #324). No coverage map, no stored
  state, no LLM.
- **The flake gate starts the slowest tests first** in every run after the
  first (`scripts/test-stress.sh`). On #321's 399 tests, 20 runs under load
  went from 1069 s to 887 s. The tests and their count are unchanged.
- **Measured and not adopted:** a per-test coverage probe, overlapping the 20
  runs, and an LLM layer. The numbers are below.
- **What is left is mostly not waste.** On #321, a PR's own new tests and the
  tests that run its changed harness code are about three quarters of the
  time. Cutting further means changing what the gate promises. That is a
  policy decision, listed at the end.

## Using it

| You want | Run |
|---|---|
| The gate on your branch | `make flake-gate BASE=origin/main` |
| Proof it selects less per kind of change | `make flake-gate-proof MODE=count` (or `time`) |
| Both gates over the last N merged PRs | `make flake-gate-replay FLAKE_REPLAY_COUNT=30` |
| Every flake the gate has caught is still selected | `make flake-gate-misses` |

`docs/testing.md#the-flake-gate` says what counts as a changed test.

## The questions #323 asked

**What gets mapped, and how often?** Nothing is mapped ahead of time. Each run
reads the diff and the two versions of each changed file, and maps changed
lines to test functions, `cfg(test)` code and modules. A helper change falls
back to its module or binary.

**Where is the map stored?** Nowhere. Selection depends only on the diff, so
it cannot go stale.

**What does the LLM see, and what may it add?** Nothing; there is no LLM
step. An LLM could only add tests: the rule was never to let one remove any.
The deterministic selection already keeps every changed test in the replayed
PRs and every flake the gate has caught (below), so an LLM would add only
cost.

**What happens on each fallback?**

| Case | Selected |
|---|---|
| A changed line in test code outside a test function | that file's module, or the whole integration binary |
| A term nextest lists nothing for | one test, then its module, then its package's unit-test binaries |
| A file whose module path is not its file path (`#[path]`, a bin's own `mod`) | its package's unit-test binaries |
| `ast-grep` missing, or failing on a file | the gate fails; it never guesses |

**How is a miss detected and recorded?** Two ways, both scripts:

- `make flake-gate-replay` checks, for each merged PR, that the new selection
  holds every test the PR added or edited. It finds them without the gate's own
  code: added `fn` lines, and hunks inside a fn's braces.
- `make flake-gate-misses` replays each flake the gate caught in CI
  (`.shux/flake-gate-catches.txt`) at the commit where it failed, and fails if
  that test is no longer selected. Add a line there whenever the gate catches
  a new one.

## The spike: numbers

### Selection, last 30 merged PRs

`make flake-gate-replay` over #264–#321, each PR against its parent, the
per-module gate (`8ef34f8`) against the per-test one:

- **10088 → 4630 tests selected (54% fewer), and none of the 30 drops a test
  it added or edited.**
- 25 PRs changed test code. The median PR selects 40% of what it did before,
  and 10 of them select a quarter or less (#320: 84 → 6, #312: 13 → 1,
  #282: 107 → 3).
- 2 select the same: #302 changed a `shux-vt` test helper, and #265 changed
  one test.
- #284 selects 30 tests the per-module gate skipped. It edited
  `cfg_attr(not(test), …)` attributes, and the gate counts any `cfg` that
  names `test` as test code, on purpose.
- `make flake-gate-misses`: all 12 flakes the gate has caught in CI are still
  selected at the commit they failed on.

### Where #321's time goes

#321 is the costliest gate on record: 399 tests, 1101 s for 20 runs. One run of
those tests, unloaded, takes 243 s of test time:

| Part | Tests | Test time | Avoidable? |
|---|---|---|---|
| Tests #321 adds or edits | 48 | 86 s | no: the gate exists to run these |
| Unit tests built on `rpc::test_harness`, which #321 changed | 216 | 82 s | no: every one of them runs the changed harness |
| Three integration binaries with a copy of production's `graph_error_to_rpc`, which a new enum variant forced to change | 120 | 61 s | yes: no test enters that function |
| The rest | 15 | 14 s | partly |

A run lasts as long as its slowest stretch: at most 12 daemon-backed tests run
at once (the `daemon-pty` group in `.config/nextest.toml`), and #321's slowest
new test alone takes 24 s.

### Ideas measured

| Idea | Measured on #321 | Verdict |
|---|---|---|
| Start the slowest tests first in runs 2–20 | 1069 s → 887 s (−17%), 0 failures | **shipped** |
| Run the 20 runs as 4 overlapping lanes, the daemon cap split between them | 881 s, against 1069 s in order (one run, and an unrelated build shared the machine for two minutes of it) | not adopted: no better than ordering, and copies of one test run at once, which no test was written for |
| Coverage probe: one instrumented run, then keep only the tests that entered a changed helper fn | 399 → about 280 tests; costs 110 s to build and 37 s to run, before the 20 runs | not adopted: saves about 10% net, and an unloaded run cannot show which paths a test takes under load, which is the one thing the gate is for |
| A coverage map built on `main` and stored | not built | same objection, and it goes stale |
| Cheaper runs (`nextest archive`) | 0.55 s a run today; archive 2.5 s | not adopted: slower |

The coverage probe's per-function data is real. On #321, m0_integration's 67
tests drop to 12, and cli_integration's and pane_io_integration's 53 to none:
their copy of `graph_error_to_rpc` runs only on error paths, and no test
reaches one unloaded. The council that reviewed this design found the hole:
under load, an RPC can fail during teardown and reach exactly that function.
A gate that drops a test because it did not fail on an idle machine has
stopped gating that test.

### Tools #323 listed

- `cargo-affected`, `cargo-difftests`: per-test coverage, so the objection
  above applies, plus keeping a map current.
- `cargo-impact`: static, like this gate, but at the crate and item level.
  The gate already selects single test functions.
- `cargo-test-changed`: whole crates, coarser than today.
- Predictive selection (Meta, Develocity, Launchable): trades a known miss
  rate for speed, using failure history. shux has 12 recorded catches, far too
  few to train on.

## What would cut more, and what each costs

These change what the gate promises, so they are a decision, not a fix:

1. **Run tests pulled in only by a helper change fewer times**, say 5 runs
   instead of 20, and keep 20 for the tests a PR adds or edits. On #321 that is
   about 157 s of the 243 s per run. Cost: a flake in an unchanged test that a
   helper change introduces becomes less likely to be caught.
2. **The coverage probe, enforcing.** About 10% on #321, for the risk above.
3. **Use production's `graph_error_to_rpc` in the three integration binaries**
   instead of their copies. They test a stale fork, and a new `GraphError`
   variant then no longer pulls in 120 tests. It happened in 1 of the last 20
   PRs.
