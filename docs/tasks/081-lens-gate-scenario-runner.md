# Task 081: lens gate — scenario runner + `shux lens gate`

**Status:** Not Started
**Priority:** High
**Milestone:** M2
**Depends On:** 080
**Quality Gate:** shux-tui-qa
**Touches:** `crates/shux/src/cli.rs` (`lens gate` verb), scenario parser + runner (CLI-side), `crates/shux/tests/lens_gate_*`, `.shux/fixtures/lens-gate/`, `Makefile`

> `shux lens gate` initiative. The declarative scenario runner that drives a TUI
> deterministically and calls `expect_golden` — replacing the shipped sleep-ridden
> bash+python pattern.

## Problem

There is no declarative, settle-driven scenario runner. shux's own CI-regression
guidance uses `sleep 3` + a bespoke comparator (the anti-pattern the lens abolished).
The runner must drive a TUI with known wait points, capture named frames, apply masks
and redaction, sanitize the child environment, and handle child death cleanly.

## Scope

1. **`shux lens gate <scenario.toml> [-- <argv>]`** CLI verb (CLI==API). Parse a **TOML**
   scenario — **DECIDED** (Ārya, 2026-07-17): TOML, matching the gh-hound ecosystem
   (`~/.config/gh-hound/config.toml`, `.codex/agents/*.toml`) and shux's own
   `.shux/templates/*.toml`. YAML appears in gh-hound only for the GitHub Actions files
   it *consumes*, never its own authoring surface — the same split holds here. Envelope:
   `name`, `description`, `[terminal]{rows,cols,respond_to_queries}`, `[env]`, `[[steps]]`.
2. **Agnostic step core** (~10; plugin seam for domain asserts): `wait_for_text`,
   `wait`, `settle`/`hold_settle`/`stable_frames`, `type_text`, `keys` (vim notation),
   `paste`, `resize`, `expect_golden{name,tol,retries,ignore_region?,xfail?}`,
   `assert_contains`/`assert_not_contains` (cheap smoke, not the verdict). Deliberately
   NOT importing grok's ~40 pager-specific steps.
3. **Deterministic env injection** (council #1/#2/#3): default `LC_ALL=C.UTF-8`,
   `TZ=UTC`, `TERM=xterm-256color`, `COLORTERM=truecolor`, **`SOURCE_DATE_EPOCH`**
   (scenario-set, forwarded), explicit `NO_COLOR` handling, **isolated `HOME`/`XDG_*`
   with a sandboxed CI socket/runtime-dir fallback** (so a shared daemon socket path
   does not leak host state), **shell-startup avoidance** (lens `run` spawns argv
   directly, no shell), and an env allow/deny model. All recorded in `cmd_env_hash`.
   The terminal answers **exact, deterministic query responses** — OSC 11 (bg), DA,
   XTVERSION — under `terminal.respond_to_queries` (byte-exact protocol fixtures).
4. **Dynamic-region masks + redaction** as steps/scenario config, applied via task 080's
   before-serialize sentinel path.
5. **Timeout classes**: per-step, per-frame (settle), whole-scenario, and
   `settle_never_stable` (a frame that never stabilizes = **failure**, exit 1, not
   infra). Child-kill + scratch cleanup on any timeout.
6. **Child-exit handling** (council #2 residual): an **unexpected child exit
   short-circuits before any visual compare** and yields `child_error{child_exit}`
   (so a crash that happens to match a "crash screen" golden cannot false-pass); an
   optional `expect_exit{code}` step makes an intended exit explicit.
7. **Scratch quota + session isolation** made explicit (council #2/#3): a scenario uses
   one scratch session; the quota is the lens constant **16 concurrent scratch sessions
   per daemon** (the 17th → `infra_error`); **parallel scenario execution is explicitly
   deferred** (documented, not silently unsupported).
8. **`stable_frames` is parse-only here** (council #3): the step parses and validates but
   its stability behavior is a documented placeholder wired to the existing `--quiet`
   settle until task 083 implements it — with a test asserting the placeholder behavior,
   so 081 exposes no half-working mode.

**Ownership boundary (council #3):** 081 owns runner *mechanics* — TOML parse, step
execution, deterministic env, the *inputs* to child-exit/timeout classification, mask
application. The final **verdict/status names, report.json, and exact exit-code
assertions live in task 082**, not here. 081 tests that the runner *produces the raw
signals* (child died with code C; a frame never stabilized; a step timed out); 082 tests
that those map to `child_error`/`settle_never_stable`/exit codes.

## Non-Goals

- No verdict rollup, `report.json`, xfail governance, or bless — task 082.
- No settle-mode *implementation* internals beyond wiring the steps (the modes land in
  task 083; here they are invoked and may be stubbed to the existing `--quiet` until 083).
- No parallel scenarios; no mouse/focus/bracketed-paste steps (declared deferred with a
  fixture placeholder).

## Design Review Decisions

DootSabha design review MUST confirm: the TOML step schema, the deterministic-env
default set + allow/deny model, the child-exit short-circuit semantics + `expect_exit`,
and the timeout-class taxonomy.

## Testing Matrix

| Layer | Required Evidence |
|---|---|
| L1 parse | Valid scenario parses; malformed scenario → parse error (082 maps to `scenario_error`/exit 2) with an actionable message; unknown step fails closed. |
| L1 env | Child sees sanitized `LC_ALL`/`TZ`/`TERM`/`COLORTERM`/`NO_COLOR`/`SOURCE_DATE_EPOCH`, isolated `HOME`/`XDG` with sandboxed socket/runtime-dir fallback; allow/deny honored; all recorded in `cmd_env_hash`. |
| L1 queries | OSC 11 / DA / XTVERSION answered with byte-exact deterministic responses under `respond_to_queries` (protocol fixtures). |
| L1 child-exit (mechanic) | A TUI that crashes (exit 139 / exit 1) short-circuits **before** compare and surfaces the raw `child_exit`; `expect_exit` records the intended-exit signal. (Mapping to `child_error` + exit code is asserted in 082.) |
| L1 timeouts (mechanic) | Per-step/per-frame/whole-scenario/never-settle produce their distinct raw signals + child-kill + scratch cleanup. (Verdict mapping asserted in 082.) |
| L1 stable-frames placeholder | The `stable_frames` step parses/validates and behaves as its documented `--quiet` placeholder until 083; no half-working mode is exposed. |
| L2 CLI | `shux lens gate --help` + bad-path invocations return actionable output. |
| L2 quota | 17th concurrent scratch (quota constant 16) → `infra_error`; scenario leaves no scratch session behind. |
| L3 dogfood | Run a real scenario end-to-end against a fixture TUI at 80x24 and 120x40 under `no_leak_guard.sh`, serial; zero leaked daemons. |
| L3 edge | Cursor, alt-screen, resize, and query-response fixtures exercised; mouse/focus/bracketed-paste explicitly deferred with a placeholder + a tracked issue link + defined non-support behavior (rejected, not silently ignored). |

## Acceptance Criteria

- [ ] `shux lens gate <scenario.toml>` drives a TUI via the agnostic step core, no sleeps.
- [ ] Child environment is deterministically sanitized and isolated.
- [ ] Unexpected child exit short-circuits before compare; `expect_exit` supported.
- [ ] All four timeout classes behave per spec; never-stable is a failure.
- [ ] Masks + redaction apply through the before-serialize path.
- [ ] Scratch quota + isolation explicit; parallelism explicitly deferred.

## Definition of Done

- [ ] DootSabha design review incorporated before coding.
- [ ] Red tests captured before implementation.
- [ ] L1/L2/L3 tests pass under the serial leak guard; zero leaked daemons proven.
- [ ] `make check` + new `make test-lens-gate` pass.
- [ ] `shux-tui-qa` gate `VERDICT: PASS`; scratch evidence under `.shux/out/`, review shots on the PR.
- [ ] Implementation-diff DootSabha convergence review clean or addressed.
- [ ] `docs/PROGRESS.md` + this task updated; learnings appended.
