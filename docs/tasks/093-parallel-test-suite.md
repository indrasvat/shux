# 093 — `make test` serialized the whole workspace, and diverged from CI

**Status:** In Progress
**Priority:** High (developer feedback loop; local/CI gate divergence)
**Milestone:** M3 polish
**Depends On:** —
**Quality Gate:** shux-tui-qa
**Touches:** `.config/nextest.toml`, `Makefile`, `Cargo.toml`, `.cargo/config.toml`,
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

## What makes the parallelism safe

nextest runs **each test in its own process**. That is the whole foundation: process-
global state — env vars, cwd, signal disposition — stops being shared, which is what
made `--test-threads=1` load-bearing for the `shux::bin/shux` unit tests that call
`set_var`. Those are now safe by construction rather than by serialization.

What remains genuinely global is bounded explicitly, one group per **resource**
rather than one per "looks scary" (`.config/nextest.toml`):

| Group | Bounds | Why |
|---|---|---|
| `process-table` | 1 | Tests that decide their verdict by counting processes machine-wide |
| `pty-pool` | 4 | Real `openpty` allocations from a fixed kernel table; macOS is much tighter than Linux |
| `wall-clock` | 1 + `threads-required = "num-test-threads"` | One test times two workloads and compares the ratio; a ratio cancels machine speed but not contention |
| `daemon-backed` | 8 | Real daemon + real panes + real deadlines. Not CPU-hungry, but starvable |

The other ~1,750 tests — every in-memory VT, parser, layout, raster, rpc and core
test — run unbounded.

### Calibrating the two numbers that are not obvious

**`daemon-backed = 8`, deliberately not `num-cpus`.** These tests are asleep almost
the whole time; what they contend for is daemon startup, PTYs and memory, so the
right ceiling is a resource count, not a core count.

| daemon cap | wall | note |
|---|---|---|
| 4 (`num-cpus`) | 35.5 s | cap is the critical path; cores sit idle |
| 8 | **22.0 s** | chosen |
| none | 23.8 s | but starves into failure at 6× oversubscription |
| `threads-required = 2` instead | 42.9 s | booking two slots per daemon test starves the in-memory bulk |

Verified under `taskset -c 0,1` (a simulated 2-core machine, `-j 8`): 43.7 s, all
1933 passing. So 8 is not merely tuned to the box it was measured on.

**`-j = 4 × cores`.** The suite waits far more than it computes, so one thread per
core leaves most of the machine idle: 69.5 s at 1×, 22.3 s at 4×, same 1933 passes.
Beyond that it falls over — at 6× a `decaln_pane_e2e` case spent 62.8 s inside a 60 s
wait budget and failed, having done nothing wrong. The `daemon-backed` cap is what
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

## Results

| | Before | After | |
|---|---|---|---|
| `make test`, 4-core box, warm | **461.3 s** | **22.3 s** | **20.7×** |
| Tests run | 1933 | 1933 | |
| Leaked daemons / orphans | 0 | 0 | |

## Testing Matrix

- [x] Baseline reproduced (`make test`, serial) — 461.3 s
- [x] Naive parallel reproduced — 119.0 s, and it FAILS the leak guard
- [x] Each of the three defects reproduced in isolation before being fixed
- [x] Plugin process-tree kill: regression test seen failing first
- [x] Group guard proven to fail on dead / overbroad / missing / invalid config
- [x] Concurrency sweep: 1×, 2×, 3×, 4×, 6×, 8× cores
- [x] Simulated 2-core machine (`taskset -c 0,1`)
- [ ] Repeated stability trials
- [ ] hyperfine A/B with mean ± σ
- [ ] Cross-platform (macOS) — CI is the check
