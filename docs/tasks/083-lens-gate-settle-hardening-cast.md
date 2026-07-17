# Task 083: lens gate — settle hardening + optional cast

**Status:** Not Started
**Priority:** Medium
**Milestone:** M2
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

- [ ] `--hold-ms` and `--stable-frames` implemented; compose with quiet mode; backward compatible.
- [ ] Never-stable is a failure, not infra.
- [ ] Retry budget absorbs jitter without masking real regressions.
- [ ] `--cast` produces valid, honest asciinema v2 with resize events; ephemeral only.

## Definition of Done

- [ ] DootSabha design review incorporated before coding.
- [ ] Red tests captured before implementation.
- [ ] L1/L2/L3 tests pass; `make test-lens` green.
- [ ] `make check` passes.
- [ ] `shux-vt-solid-qa` `VERDICT: PASS`; evidence under `.shux/qa/083-*/`.
- [ ] Implementation-diff DootSabha convergence review clean or addressed.
- [ ] `docs/PROGRESS.md` + this task updated; learnings appended.
