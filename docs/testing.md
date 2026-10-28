# Testing without timing

A test waits for a **condition**, never for an **amount of time**. A deadline
bounds failure only; it never decides success. A test that sleeps "long enough"
passes on an idle laptop and fails on a loaded CI runner, nextest at 12
concurrent daemon tests, or a macOS hypervisor that delivers latency in bursts.

## The helpers: `shux-testkit`

Add `shux-testkit = { path = "../shux-testkit" }` to the crate's
`[dev-dependencies]`.

| Helper | Use |
|---|---|
| `wait_until(what, \|\| cond)` | Wait for a condition; panics naming `what` after `BUDGET` (30s). |
| `wait_for(what, \|\| Option<T>)` | Same, returning the value the probe found. |
| `wait_for_within(budget, what, probe)` | When the test is about a specific bound. |
| `poll(budget, probe)` / `poll_until(budget, cond)` | Returns `None`/`false` on timeout, for a caller that must clean up first. |
| `*_async` variants | In async tests. They yield between probes, so they are safe on a current-thread runtime whose other tasks make the condition true. Never block an async test with a sync helper. |

Probes start at a 1 ms interval and back off to 25 ms, so a fast machine
finishes as soon as the condition holds.

## Which wait to use

| You are waiting for | Do this | Not this |
|---|---|---|
| A condition you can query (pidfile exists, socket accepts, RPC state, process gone) | `wait_until("daemon pidfile", \|\| pidfile.exists())` | a hand-rolled `loop { check; sleep(50ms) }` |
| A program to be ready before you act | Wait for a marker it prints **after** the state holds, spelled so the shell's echo of the typed command cannot contain it (`printf 'RE%sDY\n' ''`), and not one that lands mid-output | `sleep(500ms)` "head start" |
| Something **not** to happen | Fence it: after the action, do a later action ordered behind it (a sentinel through the same pipe, an RPC serialised after it), wait for the sentinel, then assert absence | `sleep(300ms); assert!(nothing_arrived)` |
| A timeout/deadline in the code under test | Drive the clock: `#[tokio::test(start_paused = true)]` + `tokio::time::advance` for tokio time, or a `cfg(test)` clock seam (see `test_clock` in `shux-vt`) | sleeping past the deadline |
| A process to exit | Poll `kill(pid, None)` / `try_wait()` with `wait_until` | `sleep(1s)` then check |

## When a sleep is the subject

Some code legitimately sleeps (a backoff, a pacing interval, a grace period),
and a few tests pace *input* to a pane on purpose. Those say why, on the
statement:

```rust
#[expect(clippy::disallowed_methods, reason = "paces input; never synchronises on output")]
std::thread::sleep(Duration::from_millis(100));
```

`expect`, not `allow`: it fails the build once the sleep is gone, so stale
exemptions cannot accumulate. A sleep that must stay **shorter** than some
deadline for the test to pass is never acceptable; only sleeping **past** a
deadline is ordered by the monotonic clock.

## Enforcement

- `clippy.toml` disallows `std::thread::sleep`, `tokio::time::sleep` and
  `tokio::time::sleep_until` everywhere. `make clippy` (CI's Clippy step) also
  lints the `test = false` lens lane, which `--all-targets` skips.
- `.claude/settings.json` runs `scripts/check-test-sleeps.sh` after every edit
  of a file under `crates/*/tests/`, so an agent hears about a raw sleep at
  write time. `make check-test-sleeps` runs its self-test and scans the tree.

Neither catches `recv_timeout(SHORT)`, `timeout(SHORT, ..)` used to assert
absence, or a fixture script's `sleep 1; echo ready`. Those are the same defect;
review for them.

## Proving a de-flake

A timing fix has no "unfixed behaviour" to fail on, so prove the old test
depended on the scheduler and the new one does not: run both under load.

```bash
make test-stress STRESS_FILTER='binary(<binary>)' STRESS_COUNT=30 STRESS_LOAD=16
```

`STRESS_LOAD` starts that many `stress-ng` CPU hogs for the run; without
`stress-ng` installed it refuses rather than running unloaded.

Where you replace a wait, also point it at a never-true condition once and read
the timeout message, then revert: a wait that cannot fail proves nothing.
