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
| `poll(budget, probe)` / `poll_until(budget, cond)` | Returns `None`/`false` on timeout, for a caller that must clean up first. |
| `Deadline::start()` + `deadline.run(fut).await` | Read a stream (attach frames, events) until the item you want: one `BUDGET` bounds every read, and each read ends as soon as its item arrives. Scan each item once, not the whole buffer per item. |
| `*_async` variants | In async tests. They yield between probes, so they are safe on a current-thread runtime whose other tasks make the condition true. Never block an async test with a sync helper. Their poll interval is a tokio sleep, so under a paused clock that is held (auto-advance off) they never wake: wait on the blocking pool with a sync helper instead. |

Probes start at a 1 ms interval and back off to 25 ms, so a fast machine
finishes as soon as the condition holds.

## Which wait to use

| You are waiting for | Do this | Not this |
|---|---|---|
| A condition you can query (pidfile exists, socket accepts, RPC state, process gone) | `wait_until("daemon pidfile", \|\| pidfile.exists())` | a hand-rolled `loop { check; sleep(50ms) }` |
| A program to be ready before you act | Wait for a marker it prints **after** the state holds, spelled so the shell's echo of the typed command cannot contain it (`printf 'RE%sDY\n' ''`), and not one that lands mid-output | `sleep(500ms)` "head start" |
| Something **not** to happen | Fence it: after the action, do a later action ordered behind it (a sentinel through the same pipe, an RPC serialised after it), wait for the sentinel, then assert absence | `sleep(300ms); assert!(nothing_arrived)` |
| A timeout/deadline in the code under test | Drive the clock: `#[tokio::test(start_paused = true)]` + `tokio::time::advance` for tokio time, or a `cfg(test)` clock seam (see `test_clock` in `shux-vt`) | sleeping past the deadline |
| A process to exit | Poll `kill(pid, None)` / `try_wait()` with `wait_until`, or `shux_testkit::terminate` (an exited, unreaped child counts as gone) | `sleep(1s)` then check |
| Real I/O under `start_paused = true` | Keep the clock paused only around the timer you drive; auto-advance jumps to the next pending timer whenever the runtime idles, so a `timeout` around a socket read fires before the peer answers | relying on a paused clock while a real process or socket does the work |

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

- `clippy.toml` disallows `std::thread::sleep`, `tokio::time::sleep`,
  `tokio::time::sleep_until` and the `nix`/`libc` sleeps everywhere. `make clippy` (CI's Clippy step) pins
  `CLIPPY_CONF_DIR` to the root: clippy reads the nearest `clippy.toml` and
  does not merge, so a crate-local one would drop the ban. `make check-darwin`
  runs the same lints over macOS-only code.
- `scripts/check-test-sleeps.sh` flags a raw sleep in `crates/*/tests/`, and
  any attribute or Cargo.toml lint entry that waives `clippy::disallowed_methods`
  or a group holding it (`warnings` included), except an `expect` on a
  statement or fn, anywhere in `crates/` or a `Cargo.toml`. It is textual: a waiver built by a macro, or a
  sleep reached through an alias, gets past it and not past clippy.
  `.claude/settings.json` runs it after every edit, so an agent hears
  about it at write time; `make check-test-sleeps` runs its self-test and scans
  the tree, in `make check` and in CI.

- `scripts/check-test-timing.sh` runs the ast-grep rules in `lint/ast-grep/`
  over test code: `crates/*/tests/`, the test-only files in `src/` (those a
  parent declares under a `cfg` naming `test`, found by
  `scripts/test-only-files.sh`; the rules list them, and the scan fails if
  the list drifts), and every item under `cfg(test)`, `cfg(all(test, …))` or
  `cfg(any(test, …))`. They ban the sleeps in disguise that clippy cannot scope
  to tests, because production code uses the same calls legitimately:

  | Rule | Flags |
  |---|---|
  | `test-wall-clock` | arithmetic, a comparison or a duration on a clock read (`Instant::now() + D`, `now - t0`, `now.checked_add`), directly or through the `let` holding it, and any `.elapsed()` |
  | `test-wall-clock-assert` | an assertion comparing a measured duration (`waited_ms`, `elapsed`, `total_time_us`) or a clock read to a bound |
  | `test-timed-wait` | `recv_timeout`, `park_timeout`, `wait_timeout` whose duration argument is not `BUDGET` |
  | `test-absence-by-timeout` | a short wait's expiry read as the outcome (`is_err`, `unwrap_err`, `!is_ok`, `Err(_)`, an `Ok` arm that panics), directly or through its `let` |
  | `test-sleep-then-act` | a fixture script that sleeps, then runs anything (a `done` after it, or nothing, is a hold) |
  | `test-outside-cfg-test` | a test attribute or `proptest!` in `src/` outside `cfg(test)` code, where the rules would not see it |

  A clock read handed on as data (`record(Instant::now())`, a `let` passed to
  the code under test, `duration_since(UNIX_EPOCH)`) is not flagged; the rare
  test that does arithmetic on one says why in a suppression.

  A test on a paused tokio clock or the `test_clock` seam is exempt by
  structure: a real `#[tokio::test(start_paused = true)]`, or a
  `tokio::time::pause();` / `let _c = test_clock::Frozen::new();` statement
  before the code in its block. Where time really is the subject (a bound
  load can only loosen, a slow cleanup under test), suppress the one line,
  with a reason of three words or more directly above. The script checks
  that in text, since ast-grep also honours forms that would hide it
  (trailing, bare, block or file-level comments); only this one passes:

  ```rust
  // why: a lower bound the monotonic clock orders; load only adds to it
  // ast-grep-ignore: test-wall-clock-assert
  assert!(waited >= TIMEOUT_MS);
  ```

  `.claude/settings.json` runs it after every edit; `make check-test-timing`
  runs its self-test and scans the tree, in `make check` and in CI. It fails,
  rather than passing, when ast-grep is missing or not the pinned version,
  when it loads fewer rules than `lint/ast-grep/rules` defines, when it
  scans no file, and when a Rust path it is given cannot be scanned. A file
  is scanned from the checkout that holds it (the nearest `sgconfig.yml`).
- `[workspace.lints]` in the root `Cargo.toml` is the one lint policy, and
  every crate opts in. A waiver is `#[expect(lint, reason = "...")]`, never
  `#[allow]`: an `expect` fails once the lint it waives stops firing.

Macro arguments are an unparsed token tree to ast-grep, so inside a macro
(`assert!`, `proptest!`, `tokio::select!`, `dbg!`) the rules match text. Process races (job control, `ps` seeing a zombie or a
child between `fork` and `exec`, a pane exiting between two reads) cannot be
linted at all: that is what the flake gate is for. The shell QA scripts under
`.shux/scripts/` follow the same rules and are not scanned.

## Fixing a load-sensitive test, by shape

| Shape | Correct fix | Never |
|---|---|---|
| Wall-clock bound: `elapsed() < X`, `waited_ms` in a range, `total_time_us < N` | Assert the outcome and its order. Where the code under test has a deadline, drive the clock: `#[tokio::test(start_paused = true)]` with `tokio::time::advance`, or `test_clock`. A cost belongs in a bench, or in the `wall-clock` nextest group measured in thread CPU time. | widen the bound |
| Absence by timeout: `timeout(SHORT, rx.recv()).await.is_err()` | Fence it: wait for a later action ordered behind the one under test, then assert absence. A queue filled before the call returned is drained with `try_recv()` until empty. | a longer timeout |
| Load-sensitive settle: a background pump racing a quiet window | Make the churn something the test controls, and emit each frame as one write (DEC 2026 synchronized output in a fixture). The e2e asserts only what load cannot change. | retries |
| Real-process race: job control, `ps` seeing a zombie or a fork | Turn job control off in a test shell (`set +m`), wait on the process's own signal, hold a process open while the test needs it, count from shux's own records or one process per command (`count_command_roots`). | `#[ignore]` forever |
| Scheduling assumption: "still pending on its first poll" | A `cfg(test)` latch the test holds makes the state certain. | hoping |
| A real clock the setup must beat (`max_runtime_ms: 5000`) | Hold the code under test at a `cfg(test)` pause point until the state the test checks is in place. | a longer clock |

## Ignoring a test

`#[ignore]` is for a test that cannot run yet, never for one that fails
sometimes. Its reason names an open issue, `#[ignore = "<why>: see #N"]`, or
starts with `generator:` for a test run by hand to write fixtures.
`make check-test-ignores` reads `#[ignore]` and `ignore` inside a
`#[cfg_attr(…)]` wherever they sit, and fails a bare one, a reason with no
issue, and one naming an issue `gh` reports closed or that is a pull request:
an ignore does not outlive its fix.

## The flake gate

On every pull request, CI's `Flake gate` job runs the tests the branch adds or
changes 20 times, next to twice as many CPU hogs as cores, on ubuntu and
macOS (`make flake-gate BASE=origin/main`). "Changed" is an integration-test
file, a shared helper under `tests/<dir>/` (every binary that declares it, by
`mod` or `#[path]`), a hunk reaching a src file's `cfg(test)` code, a
test-only src file (its module's tests; for a helper with none, the tests of
the modules that name it, and the whole unit-test binary only when none does),
or a fixture a test names. shux-testkit's dependents are
not re-run when it changes. Any failed run fails the
job, and a run that outlasts the job's time limit fails it too. This replaces
"stress it locally if you remember".

## Proving a de-flake

A timing fix has no "unfixed behaviour" to fail on, so prove the old test
depended on the scheduler and the new one does not: run both under load.

```bash
make test-stress STRESS_FILTER='binary(<binary>)' STRESS_COUNT=200 STRESS_LOAD=16
```

`STRESS_LOAD` starts that many `stress-ng` CPU hogs for the run; without
`stress-ng` installed it refuses rather than running unloaded.

Where you replace a wait, also point it at a never-true condition once and read
the timeout message, then revert: a wait that cannot fail proves nothing.
