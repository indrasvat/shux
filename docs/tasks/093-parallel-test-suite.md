# 093 — `make test` serialized the whole workspace, and diverged from CI

**Status:** Done
**Priority:** High (developer feedback loop; local/CI gate divergence)
**Milestone:** M3 polish
**Depends On:** —
**Quality Gate:** shux-tui-qa
**Touches:** `.config/nextest.toml`, `crates/shux-vt/tests/scroll_region_bounds.rs`,
`crates/shux/tests/checkpoint_invalidation_gating.rs`, `.shux/scripts/bench_edit_loop.sh` (new),, `Makefile`, `Cargo.toml`, `.cargo/config.toml`,
`.github/workflows/ci.yml`, `lefthook.yml`, `scripts/check-test-groups.sh` (new),
`scripts/ensure-nextest.sh` (new), `scripts/ensure-hyperfine.sh` (new),
`.shux/scripts/bench_test_suite.sh` (new), `crates/shux-plugin/src/lib.rs`,
`crates/shux-plugin/tests/plugin_lifecycle.rs`, `crates/shux/src/main.rs`,
`crates/shux/tests/id_prefix_resolution.rs`, `crates/shux/tests/cli_integration.rs`

---

## Problem (issue #130)

`make test` — which `make check` and the pre-push hook both go through — ran every
test in the workspace one at a time, and did it twice over:

```make
# Makefile:110 (before)
@.shux/scripts/no_leak_guard.sh bash scripts/run-cargo-test.sh --workspace -- --test-threads=1
```

`scripts/run-cargo-test.sh` ran the test **binaries** strictly sequentially (a
`while read -r binary` loop that `wait`s on each one), and `--test-threads=1` then
serialized the tests **inside** each binary. `--test-threads=1` appeared 23 times in
the Makefile: it was the house pattern, not one target.

CI did not do this. `.github/workflows/ci.yml:105` ran `cargo nextest run --workspace`
at default parallelism, and had been green for a long time.

That divergence was worth as much as the speed. Local and CI ran **different runners
at different concurrency**, so the two gates could disagree in both directions — and
`lefthook.yml:23-25` asserted in a comment that they matched, which was simply untrue.

### Measured — before

Whole workspace, 4-core box, warm build, nothing else running:

| | |
|---|---|
| `make test` (serial) | **461.3 s** |
| `cargo nextest run --workspace` (default parallelism) | **119.0 s** |

The parallel run passed 1932/1932 — and **failed the leak guard**, which is where
this stopped being a scheduling task.

## Three defects the serial suite was hiding

Every one predates this change. Two are in the verification machinery itself.

### 1. Killing a plugin left its children running

`PluginManager::install` kills the plugin on a failed handshake, and
`PluginManager::kill` kills it on teardown — both via `Child::kill`, which signals
exactly one process. A plugin is a process *tree*: plugins are shell scripts as
often as binaries, and a script's children outlive it. Every failed handshake
leaked the plugin's whole subtree to init, with nothing recording that it existed.

Reproduced in isolation, serially, with no parallelism involved:

```
$ .shux/scripts/no_leak_guard.sh cargo nextest run -p shux-plugin --test plugin_lifecycle -j1
     Summary [   5.531s] 15 tests run: 15 passed, 0 skipped
shux leak guard: command left new orphan automation process(es): 8863
 8863     1 S    sleep 30
```

The serial suite hid it for a mundane reason: `shux-plugin` runs early, the stray
`sleep 30` expires on its own, and by the time 461 seconds of other tests had
finished there was nothing left to find. Cutting the suite to 119 s is what made the
guard able to see it.

**Fix:** plugins spawn with `process_group(0)` and are killed with `killpg`, matching
what `shux-pty` already does (`handle.rs:550`) and what `lens_scratch` already does
for scratch sessions. `killpg` is only ever called while the `Child` handle is alive,
so the pid cannot have been recycled.

### 2. Two suites shared one machine-global `ps` needle

`crates/shux/src/main.rs` waited for the string `sleep 30` to disappear from
`ps -axo args=` — a machine-wide view — while `crates/shux-plugin/tests/plugin_lifecycle.rs:86`
spawned exactly `sleep 30`. Serially they never overlapped. In parallel the daemon
test can sit out its entire 5 s budget watching a different crate's process and
conclude nothing about its own.

**Fix:** per-run unique needles, the pattern `production_lens_run_dropped_mid_core_leaves_no_orphan`
was already using two hundred lines above (`sleep_tag`).

### 3. `id_prefix_resolution` compared two captures of a pane that was still writing

The fixture waited for `BRAVO` to appear in pane B and then stopped. Pane B is an
interactive shell; the prompt it redraws afterwards lands whenever the scheduler gets
round to it. The test glances the same pane twice and asserts the two agree — and on
a loaded machine the shell's next write slips between them:

```
left:  "printf 'BRAVO\n'  ..."
right: "printf 'BRAVO\n'\nnvm  ..."
assertion failed: short id resolved to a different pane than the full uuid
```

Nothing about id resolution is wrong. The fixture did the first half of the house
rule — *content, then settle* — and not the second.

**Fix:** `pane.wait_settled` on both panes after the content wait. The verb already
existed; the fixture just never called it.

The first version of this fix called it through `rpc_ok`, which panics on any
error — and a pane's VT is registered slightly after `pane.split` returns and torn
down as soon as its command exits, so `not_found` is a legitimate transient at
both ends of a pane's life. Under load that turned a benign race into three red
tests. The settle helper retries around `not_found` and gives up quietly: the
caller wants the pane QUIET, and a pane with no VT is as quiet as it gets.

## What makes the parallelism safe

nextest runs **each test in its own process**. That is the whole foundation: process-
global state — env vars, cwd, signal disposition — stops being shared, which is what
made `--test-threads=1` load-bearing for the `shux::bin/shux` unit tests that call
`set_var`. Those are now safe by construction rather than by serialization.

What remains genuinely global is bounded explicitly, one group per **resource**
rather than one per "looks scary" (`.config/nextest.toml`):

| Group | Bounds | Members | Why |
|---|---|---|---|
| `process-table` | 1 | 5 | Tests that decide their verdict by counting processes machine-wide |
| `daemon-pty` | 12 | 183 | Real daemon and/or real PTYs, with real deadlines. Not CPU-hungry, but starvable |
| `wall-clock` | 1 + `threads-required = "num-test-threads"` | 1 | Verdict is a timing measurement; a ratio cancels machine speed but not contention |

The other ~1,740 tests — every in-memory VT, parser, layout, raster, rpc and core
test — run unbounded.

`daemon-pty` is one group and not two on purpose. Every daemon-backed suite here
except `statusbar_starship_symlink` also allocates PTYs, because creating a
session creates a pane. Modelled as two overlapping groups, the effective ceiling
was the sum of two caps and neither number described the machine.

`max-threads = 1` on `wall-clock` is *not* what isolates its member. A group's
`max-threads` serializes members against each other and nothing else — with one
member that is a no-op, and the test goes on competing with the other 1,932.
`threads-required = "num-test-threads"` is the knob that reserves the whole run's
budget.

### Calibrating the two numbers that are not obvious

**`daemon-pty = 12`, deliberately not `num-cpus`.** These tests are asleep almost
the whole time; what they contend for is daemon startup, PTYs and memory, so the
right ceiling is a resource count, not a core count.

| daemon cap | wall | note |
|---|---|---|
| 8 | 41.9 s | cap is the critical path; cores sit idle |
| **12** | **29.3 s** | chosen |
| 16 | 31.4 s | past the knee — contention costs more than it buys |
| none | 23.8 s | faster, but starves into failure at 6× oversubscription |
| `threads-required = 2` instead | 42.9 s | booking two slots per daemon test starves the in-memory bulk |

Verified under `taskset -c 0,1` (a simulated 2-core machine): all 1933 passing.

**This number had to be re-derived after the groups were merged, and that is the
trap worth recording.** `pty-pool` and `daemon-pty` began as two groups sharing
four binaries. A test belongs to at most one group and the first match wins, so
the effective ceiling was the *sum* of the two caps. Merging them into one group
at the old number silently halved concurrency and cost 12 seconds a run — with
nothing red, because every test still passed. `make check-test-groups` now
asserts each group's exact member count for exactly this reason.

**`-j = 4 × cores`.** The suite waits far more than it computes, so one thread per
core leaves most of the machine idle: 69.5 s at 1×, 22.3 s at 4×, same 1933 passes.
Beyond that it falls over — at 6× a `decaln_pane_e2e` case spent 62.8 s inside a 60 s
wait budget and failed, having done nothing wrong. The `daemon-pty` cap is what
holds that cliff back, so the two settings are a pair.

## Guarding the guard

A nextest test-group whose filterset matches nothing is **completely silent**: nextest
prints an empty group and runs everything unbounded. Nothing goes red.

This is not hypothetical. The first version of this config used
`test(=shux::bin/shux::tests::production_unconfirmed_kill_preserves_registry_row)` —
which reads exactly like the identifier `cargo nextest list` prints, and matches **all
1934 tests**, because `test()` matches the test name and never the `binary-id name`
pair. Caught by counting, not by reading.

`scripts/check-test-groups.sh` (`make check-test-groups`, wired into `make check`)
asserts every declared group is non-empty and holds no more than 30% of the suite.
Proven to fail on all four modes before being trusted:

| Injected defect | Result |
|---|---|
| Filterset naming a nonexistent binary | ✗ exit 1, quotes nextest's parse error |
| Filterset that matches zero tests but parses | ✗ exit 1, "matched ZERO tests" |
| Filterset widened with `all()` (1928/1934) | ✗ exit 1, ">30%" |
| Group removed from the config entirely | ✗ exit 1, "not declared" |
| Unmodified config | ✓ exit 0 |

## Build profiles

The repo had **no `[profile.*]` sections anywhere**. Everything compiled and ran at
`opt-level = 0` with full DWARF across ~45 test binaries.

Adding them cut test **execution** from 117.5 s to 68.1 s before any scheduling change
— the VT scroll-region bounds tests alone went 13.9 s → 2.2 s each.

## The timing test had to be rebuilt, not re-tuned

`region_scroll_cost_is_linear_in_pane_height` decides whether region scrolling is
linear or quadratic by timing a 1024-row scroll against a 128-row one. Its 12×
threshold was calibrated at `opt-level = 0` — 8.1× fixed, 19.9× broken.

Moving test targets to `opt-level = 1` pushed the *same, still-linear*
implementation to 11–12.8× and it began failing about one run in ten. Nothing had
regressed: 1024 rows of blanking exceeds L2 where 128 rows does not, and removing
the unoptimised interpreter overhead simply stopped masking that. **A ratio
cancels out machine speed; it does not cancel out the memory hierarchy.**

Two fixes were tried and rejected before the third:

| Attempt | Result |
|---|---|
| Minimise each arm separately instead of best-of-N paired ratios | 3/30 failures — better methodology, same drift |
| `[profile.test.package.shux-vt] opt-level = 0` — pin the codegen the calibration was taken under | 0/30 failures, but **2.4× slower** (52 s vs 22 s): it pins the whole crate, so every VT bounds test loses its optimisation |
| **Measure the quadratic reference in the same run** | 0/30 failures, no speed cost |

The test now times the per-line scroll path — which really is O(height²), since
`Grid::scroll_up` rotates the region once per line — alongside the bulk path, and
asserts bulk resembles linear rather than per-line. Both arms feel the same cache
behaviour and the same codegen, so whatever moves one moves the other.

Proven in both directions:

- **Passes** 30/30 solo, bulk 9.4–12.4× against a measured midpoint near 20×.
- **Fails** when the defect is reintroduced (bulk arm switched to per-line
  scrolling): `bulk region scrolling grew 54.0x ... anything under 22.8x is
  linear-shaped`.

It also asserts its own reference is still quadratic, so it cannot silently
degrade into a test that passes against anything.

## Results

| | Before | After | |
|---|---|---|---|
| `make test`, 4-core box, warm | **461.3 s** | **29.1 s** | **15.9×** |
| Tests run | 1933 | 1933 | |
| Leaked daemons / orphans | 0 | 0 | |

An earlier revision of this work measured 22.3 s (20.7×). That number was real but
was taken before the group-overlap fix, which restored a correctness property at
the cost of some concurrency. The slower, correct configuration is the one that
ships.

### The loop you are actually in

A warm re-run measures the tests. It does not measure the thing you spend a
session doing, because every real iteration pays a rebuild first — and a rebuild
is exactly what a profile change can make *worse* while the test-execution number
gets better. `make bench-edit-loop` measures the whole loop:

| After changing | `make test` | vs idle |
|---|---|---|
| nothing | 29.3 s | — |
| a test file | 29.7 s | +0.4 s |
| `shux/src/main.rs` (11k-line CLI) | 33.4 s | +4.1 s |
| `shux-vt/src/lib.rs` (core lib, many dependents) | 37.8 s | +8.5 s |

The same loop before this change was 461 s of tests on top of the same rebuild.
The `opt-level = 1` on test targets buys 49 s of execution and costs ~8 s of
recompile on the worst edit, so it is worth it at every point in that table.

### Consistency

Fifteen consecutive full runs of the shipping configuration, leak guard armed on
every one:

```
run  1: 31.2s   run  6: 29.2s   run 11: 28.6s
run  2: 29.6s   run  7: 31.2s   run 12: 29.5s
run  3: 30.8s   run  8: 28.8s   run 13: 29.4s
run  4: 29.8s   run  9: 29.4s   run 14: 28.8s
run  5: 29.2s   run 10: 29.5s   run 15: 29.1s

15/15 clean · 1933/1933 passing every run · 0 leaks · 28.6-31.2 s
```

Every flake seen along the way was chased to a root cause and fixed. None was
retried away and none was left as "probably fine":

| Flake | Root cause | Fix |
|---|---|---|
| `region_scroll_cost_is_linear_in_pane_height` | Threshold calibrated at a different `opt-level`; a ratio does not cancel out cache behaviour | Measures its own quadratic reference in the same run |
| `id_prefix_resolution` ×3 | `pane.wait_settled` through `rpc_ok`; `not_found` is a legitimate transient at both ends of a pane's life | Settle helper retries and gives up quietly |
| `checkpoint_invalidation_gating` | Settled after sending a keystroke without first requiring it to land — a pane that has not started writing is "quiet" | Require the content, then the stillness |
| `rep_pane_e2e` ×2 | Self-inflicted: tightening the connect backoff cut the total budget from ~11 s to 1.4 s | Change reverted (see below) |

### hyperfine

`make bench-test-suite`, 3 runs per arm, nothing else on the machine:

```
before: serial (run-cargo-test.sh --test-threads=1)   278.89 s ± 8.17
after:  parallel (make test)                           30.11 s ± 0.23
                                                       9.26x ± 0.28
```

Note what this does **not** say. Both arms build with the new profiles, so 9.26×
is the contribution of **scheduling alone**. The end-to-end change a developer
experiences is 461.3 s → 29.1 s, because the profiles were worth another 1.65×
on their own.

## Two optimisations measured and rejected

Recorded because the next person will have the same two ideas, and both look
obviously right until you measure them.

### A faster linker

Standard advice for slow Rust builds, and `.cargo/config.toml` carried a
commented-out mold block waiting for it. Warm cache, three touch-and-relink
cycles of the largest binary: **`rust-lld` 2.50 s / 2.33 s, default linker
2.33 s / 2.41 s.** No difference. `debug = "line-tables-only"` had already
removed most of what a linker spends its time on — fixing the profile made the
linker swap redundant, not the other way round. The dead block was deleted and
these numbers put in its place.

*Measurement trap:* `RUSTFLAGS` is part of cargo's fingerprint, so the first
build after a flag change rebuilds the whole graph. Comparing that against a
warm default build reported the fast linker as **35× slower**.

### Eager daemon-connect probing

`ensure_daemon_running_at` sleeps before its first probe, so the fastest possible
outcome is "the initial backoff" rather than "as soon as the daemon is up".
Tightening it (probe first, 2 ms floor, 1.6× growth) is a genuine win **in
isolation** — a cold `shux session list` autostart went from 0.37 s to 0.25 s,
measured repeatably.

It is a net loss for the suite. Twelve daemon-backed tests start at once by
design, and twelve clients each spinning through `connect()` every few
milliseconds cost more in contention than the eager probing saves:

| | solo autostart | whole suite |
|---|---|---|
| HEAD (sleep-first, 50 ms, 2×) | 0.37 s | **29.3 s** |
| eager (probe-first, 2 ms, 1.6×) | **0.25 s** | 32.6 s |

Restoring the original constants but keeping the restructured loop still measured
32.4 s across two independent A/B runs, so the cost was not the probing rate and
was not something this change could explain. An unexplained 10% regression on the
thing being optimised is not worth a 120 ms improvement somewhere else, so the
client is untouched.

**A real defect was found on the way, and is worth fixing separately.** The retry
budget is an emergent property of the backoff series — 50 ms doubling to a 2 s
cap over ten attempts, about 11 seconds if you sit down and sum it, which nobody
does. Tightening the series silently cut the budget to 1.4 s, and `rep_pane_e2e`
immediately failed with *"failed to connect to daemon after 14 retries"* against
daemons that had simply not finished starting. Anyone touching that backoff will
hit the same trap. It wants an explicit deadline instead of a retry count — a
small change, but one that belongs in its own review rather than inside a
test-speed PR.

## What is still on the table

Measured, not guessed. From a full run's per-test timings:

```
TOTAL test-seconds 372s across 1933 tests   (wall 29.3s)
  daemon-pty group   234s / 183 tests  ->  floor at cap 12 = 19.5s
  everything else    138s / 1750 tests ->  floor at 16 threads = 8.6s
```

The `daemon-pty` group is the critical path, and **most of its 234 seconds is
daemon startup, not test work**. `window_title_escape_injection` spends 53.6 s
across 23 tests that each run one or two CLI commands and check the output is
inert — 2.33 s per test, almost all of it booting and tearing down a daemon that
exists for one assertion. `id_prefix_resolution` is 3.11 s per test for the same
reason plus a heavier fixture.

Raising the cap does not fix this — it is past the knee already (cap 16 measures
*worse*, 31.4 s, because more concurrent daemons slow each other down). The work
itself has to shrink.

Sharing one daemon across a binary's tests is **not** the answer, and is worth
naming explicitly so it does not get proposed again. Every one of these tests
currently gets a daemon that has never seen another test; that isolation is the
reason the suite can run twelve-wide at all, and trading it for wall-clock would
buy speed with exactly the flakiness this change exists to avoid. A test that
genuinely needs to exercise daemon-sharing behaviour should say so and share one
deliberately.

What is left is therefore the daemon's own **startup cost**, ~0.37 s of which
every one of these tests pays. That is a product question — what the daemon does
between `fork` and `listen` — not a test-scheduling one, and making it faster
helps every user's first command as much as it helps the suite. The client-side
half of it was measured and rejected above; the daemon-side half is unexplored.

## Testing Matrix

- [x] Baseline reproduced (`make test`, serial) — 461.3 s
- [x] Naive parallel reproduced — 119.0 s, and it FAILS the leak guard
- [x] Each of the three defects reproduced in isolation before being fixed
- [x] Plugin process-tree kill: regression test seen failing first
- [x] Group guard proven to fail on dead / overbroad / missing / invalid config
- [x] Concurrency sweep: 1×, 2×, 3×, 4×, 6×, 8× cores
- [x] Simulated 2-core machine (`taskset -c 0,1`)
- [x] Repeated stability trials — 12/12 clean
- [x] Timing test proven to FAIL on a reintroduced defect
- [x] Adversarial review by two parallel agents; every finding reproduced first
- [x] hyperfine A/B with mean ± σ
- [x] Edit → test loop measured (`make bench-edit-loop`)
- [ ] Cross-platform (macOS) — CI is the check
