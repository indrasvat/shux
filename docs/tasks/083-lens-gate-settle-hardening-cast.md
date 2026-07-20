# Task 083: lens gate — settle hardening + optional cast

**Status:** Done
**Priority:** Medium
**Milestone:** M3
**Depends On:** 080, 082
**Quality Gate:** shux-vt-solid-qa
**Touches:** `crates/shux/src/main.rs` (`pane.wait_settled`), `crates/shux-vt/src/lib.rs` (settle substrate), `crates/shux/src/cli.rs`, cast serializer, `.shux/fixtures/lens-gate/`

> `shux lens gate` initiative (idea 01). Resequenced AFTER capture (078–080) because
> `--stable-frames` hashes the `CapturedFrame`, which must exist first (council #1).

## Problem

Settle is event-driven and sync-output (`?2026`) aware but has only quiet-period mode:
a fast spinner never goes quiet (times out), a slow one settles between frames. Animated
TUIs need stronger stability contracts, and CI needs retry for loaded runners. A failed
gate frame also lacks a "how did we get here" artifact.

## Scope

1. **`--hold-ms N`** (condition-hold, grok-adapted): the settled condition must hold N ms
   while output keeps pumping; any bump resets the hold clock.
2. **`--stable-frames K`** (novel — grok lacks it): hash the `CapturedFrame`; require K
   consecutive identical revisions. For animated TUIs with no semantic "idle" text. A
   frame that never reaches K within budget is `settle_never_stable` = **failure** (082),
   never infra.
3. **Retry budget** on `expect_golden` (`retries = N`): re-settle + re-capture before
   declaring `fail`, with a fingerprint so a retry cannot mask a *different* regression.
   **Ownership (council #4):** 081 parses `expect_golden.retries`; 082 plumbs `--retries N`
   and exposes it in `report.json`; **083 owns the retry behavior and the anti-masking
   fingerprint rule** — the only task that may assert retry semantics.
4. **Optional `--cast`** (asciinema v2, grok-adapted): attach a replayable `.cast` of the
   run beside the report so a reviewer can scrub how the TUI reached a failing frame.
   **Fix grok's gap**: emit resize (`"r"`) events on `resize` steps so replay geometry is
   honest. UTF-8-boundary-safe; strict range validation; kept out of committed goldens
   (ephemeral in `.shux/out/`).

## Non-Goals

- No change to the default quiet-mode semantics (backward compatible).
- No verdict/report changes (082) beyond consuming `settle_never_stable`.

## Design Review Decisions

DootSabha design review MUST confirm: the hold vs stable-frames vs quiet composition,
the retry+fingerprint anti-masking rule, and the cast format + resize-event handling.

## Testing Matrix

| Layer | Required Evidence |
|---|---|
| L1 hold | `--hold-ms` resets on a bump; succeeds only after the condition holds the full window. |
| L1 stable-frames | A fixture spinner settles under `--stable-frames K` where quiet-mode would time out; a perpetual-animation fixture → `settle_never_stable` (failure), not infra. |
| L1 retry | A jittery fixture passes within the retry budget; a real regression still fails after retries (fingerprint mismatch not masked). |
| L1 cast | `.cast` is valid asciinema v2, UTF-8-safe, includes resize events; replay geometry honest. |
| L2 compat | Existing `pane wait-settled --quiet` behavior unchanged (`make test-lens` green). |
| L3 dogfood | Gate a real animated TUI (e.g. `btop`/`htop` or a rich spinner) with `--stable-frames`; leaves no daemons. |
| L3 QA | `shux-vt-solid-qa` `VERDICT: PASS` (settle touches VT substrate). |

## Acceptance Criteria

- [x] `--hold-ms` and `--stable-frames` implemented; compose with quiet mode; backward compatible.
- [x] Never-stable is a failure, not infra.
- [x] Retry budget absorbs jitter without masking real regressions.
- [x] `--cast` produces valid, honest asciinema v2 with resize events; ephemeral only.

## Definition of Done

- [x] DootSabha design review incorporated before coding.
- [x] Red tests captured before implementation.
- [x] L1/L2/L3 tests pass; `make test-lens` green.
- [x] `make check` passes (0 test failures / no shux leaks; a leak-guard false-positive on the
  local `peon-ping` notification hook is not a shux process).
- [x] `shux-vt-solid-qa` `VERDICT: PASS`; evidence under `.shux/qa/083-*/`.
- [x] Implementation-diff DootSabha convergence review clean or addressed (2 daemon settle-loop
  bugs — seed race + busy-spin — found + fixed + pinned).
- [x] Real-target dogfood (Feature Protocol step 10): cast on real `htop` (valid asciinema v2 +
  alt-screen setup + honest resize event captured); `--hold-ms` settles real `vim` in ~400ms;
  `--help` truthful; the `--cast`-into-golden and out-of-range errors are actionable (fixed the
  wait-settled error to surface the range `detail`, pinned). Findings reproduced firsthand.
- [x] `docs/PROGRESS.md` + this task updated; learnings appended.

## Adversarial follow-up (out of 083 scope — feed forward)

Surfaced by the 083 adversarial review (end-to-end daemon-driver), reproduced, and deferred as
NOT part of 083's settle/cast scope:

- **`--on-missing create` masks a non-zero child death** (082 bless/create domain). A child that
  exits non-zero mid-settle and blesses 0 goldens still exits 0 (create mode), whereas COMPARE
  mode correctly surfaces it (child_error/exit-non-0). The `child_error` contract is bypassed on
  the create path. Local/non-CI only. → a 082-verdict or 084 fix.
- **Secret-scanner entropy false-positive on long path-like strings** (082 `gate/secrets.rs`,
  24-char floor) — a temp-dir UUID segment tripped the note redactor. Already tracked from the 082
  dogfood; still open. → 084 / secrets tuning.
- **No `shux daemon stop` verb; a per-XDG daemon persists after each `lens gate` run** (the scratch
  SESSIONS self-clean; the daemon itself does not). Trips leak-guards that expect one-shot spawn.
  Already noted from the 082 dogfood ("one-shot daemon spawn+teardown for CI"). → daemon-lifecycle
  work.

- **Sandbox `PATH` excludes Homebrew** (`gate/env_plan.rs` `DEFAULT_PATH` = `/usr/local/bin:
  /usr/bin:/bin`) — the 083 dogfood confirmed a bare `htop`/`btop`/`vim`/`bat` (all under
  `/opt/homebrew/bin`) → `infra_error` "No such file or directory"; today the scenario must use an
  ABSOLUTE path. Already tracked from the 082 dogfood ("PATH passthrough"). → 084 cold-agent DX.

Addressed IN 083 (adversarial + dogfood fixes, pinned): cast invalid-UTF-8-lead bytes emit
immediately (`shux-vt/src/cast.rs`); a `--cast` target inside `--golden-dir` is refused (exit 2);
the retry audit dedupes fingerprints; the `stable_frames` idle-pane trade-off is documented (steer
to `--hold-ms`) and pinned; the daemon seed race + busy-spin (impl-review) are fixed; and
`pane wait-settled` errors now surface the actionable range `detail` (e.g. "hold_ms 5 out of range
[10, 60000]") instead of a bare "invalid_params" (dogfood; pinned).

## Defect fixed by task 085 (2026-07-20)

**F7 — retry vocabulary described a stable regression as a flake** (`gate/runner.rs`).
`expect_golden{retries=N}` reported `FAIL after N retries (exhausted fps ...)`. Retries exist
to absorb jitter, so that phrasing read as "this frame is flaky" — exactly backwards for the
common case, where every attempt produced the SAME frame and the run is a stable, reproduced
regression. The distinct-fingerprint count was already computed at the message site, so the
two situations are now named: `FAIL - the same diff on all N attempts (a stable regression,
not a flake)` versus `N attempts produced M different frames (output is non-deterministic;
fix the scenario's determinism before trusting any verdict)`. Re-gated with
`make test-lens-gate-settle` (6/6).
