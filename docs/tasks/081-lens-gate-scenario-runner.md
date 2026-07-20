# Task 081: lens gate — scenario runner + `shux lens gate`

**Status:** Done
**Priority:** High
**Milestone:** M3
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
   never-stabilized. Each emits a **distinct raw runner signal**; a frame that never
   stabilizes is emitted as a *failure-class* signal, never an infra-class one (082
   maps it to the frozen `settle_never_stable` status). Child-kill + scratch cleanup on
   any timeout.
6. **Child-exit handling** (council #2 residual): an **unexpected child exit
   short-circuits before any visual compare** and emits the raw `child_exit{code}`
   signal (so a crash that happens to match a "crash screen" golden cannot false-pass);
   an optional `expect_exit{code}` step makes an intended exit explicit.
7. **Scratch quota + session isolation** made explicit (council #2/#3): a scenario uses
   one scratch session; the quota is the lens constant **16 concurrent scratch sessions
   per daemon** (the 17th emits a raw quota-exceeded signal); **parallel scenario
   execution is explicitly deferred** (documented, not silently unsupported).
8. **Retries — parse only** (council #4): 081 parses and validates
   `expect_golden.retries`; the retry *behavior* and its anti-masking semantics are
   owned by **083**, CLI plumbing/report exposure by **082**.
9. **`stable_frames` is parse-only here** (council #3): the step parses and validates but
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

### Deferred behaviors — explicit non-support (081 shipped contract)

Non-support is REJECTED, never silently ignored (design D10). Each is tracked for a
later task:

- **`mouse` / `focus` / `bracketed_paste` steps** → the parser REJECTS them with
  `parse_error` ("… not supported in this runner … tracked in docs/tasks/081"). Pinned by
  `deferred_mouse_step_is_rejected_not_ignored` (`lens_gate_run.rs`). A future step-plugin
  task adds them.
- **`respond_to_queries = false` (query suppression)** → RESERVED (design D9). The shux
  terminal answers OSC 11 / DA / XTVERSION deterministically + byte-exact and CANNOT be
  silenced in 081 (the field is parsed and its suppression semantics documented as
  reserved; `false` = "this scenario does not rely on query responses", NOT rejected).
  Byte-exactness pinned by `gate::queries` unit tests.
- **`retries` (on `expect_golden`) + `stable_frames`** → PARSE-ONLY (design D6): validated
  but behaviorally a documented `--quiet` settle placeholder until task 083. No
  half-working retry/stability mode ships.
- **`xfail`** → parsed OPAQUELY into the 082 `XfailMeta` shape and RESERVED; 081 takes no
  xfail action (governance is task 082).
- **Parallel scenario execution** → deferred; the runner runs one scenario / one scratch
  session at a time (quota constant 16 shared with the lens scratch pool).

## Design Review Decisions

DootSabha design review MUST confirm: the TOML step schema, the deterministic-env
default set + allow/deny model, the child-exit short-circuit semantics + `expect_exit`,
and the timeout-class taxonomy.

**Incorporated (council: codex + agy; chair synthesis claude — verdict REVISE →
architecture APPROVED after the revisions below; evidence
`.shux/qa/081-lens-gate-scenario-runner/dootsabha-design.json`):**

- **D1 — ownership boundary (confirmed).** 081 owns runner MECHANICS + RAW SIGNALS.
  082 owns status names, `report.json`, the stdout summary/report contract, xfail
  governance, bless/`--update`, and the exact exit-code map. The frozen
  `lens_gate_contract.rs` lane stays RED until 082 (it is `test = false`).
- **D2 — the raw trace vocabulary is runner signals, NEVER `GateStatus`.** The signal
  set: compare → `frame_match` / `frame_mismatch` / `golden_absent` / `golden_untrusted`
  (082 maps → pass/fail/missing_golden/stale_golden); child → `child_exit{code}` /
  `expected_child_exit{code}`; four DISTINCT timeouts → `step_timeout` /
  `frame_settle_timeout` / `scenario_timeout` / `never_stabilized`; ops → `quota_exceeded`
  / `parse_error` / `no_visual_check`; asserts → `assert_passed` / `assert_failed`. The
  internal compare still runs 080's `evaluate_tier` (returns `GateStatus`); an adapter
  maps it to the runner signal for the trace, so 081 never emits a frozen status name.
- **D3 — trace channel + privacy.** The NDJSON raw-signal trace is emitted ONLY behind
  `--trace <path|->`; default `shux lens gate` stdout does NOT emit unconditional NDJSON
  (082 owns the stdout summary/report). 081's exit code is provisional (documented as
  non-final; 082 installs the frozen map). The trace carries hashes
  (`cmd_env_hash`/`scenario_hash`), never raw env/argv; assert failures carry
  bounded/redacted excerpts + a hash, never a full screen dump (codex privacy catch).
- **D4 — `env_clear` (deny-by-default) is implemented.** New opt-in `env_clear` on
  `lens.run` + `PtyConfig` (default `false` = byte-identical existing spawn). The gate
  runner sets it; `PtyHandle::spawn` `env_clear()`s before the PTY defaults + plan.
  Deterministic defaults (always): `LC_ALL=C.UTF-8`, `LANG=C.UTF-8`, `TZ=UTC`,
  `TERM=xterm-256color`, `COLORTERM=truecolor`, `SOURCE_DATE_EPOCH="0"` (string), a
  deterministic `PATH`, and sandbox `TMPDIR`/`HOME`/`XDG_*`/`XDG_RUNTIME_DIR`/`SHUX_SOCKET`
  (no host-temp leak — agy's `TMPDIR` catch; no host daemon reach). `NO_COLOR` default
  UNSET; scenario opts in with `NO_COLOR="1"`. `[env] KEY="v"` SETS (incl. empty string —
  never overloaded as unset; unset = absence under `env_clear`). `[env] allow=["PATH",…]`
  = opt-in host passthrough. Rich-TUI guardrail satisfied (opt-in, default-off).
- **D5 — `cmd_env_hash` is release-stable; `scenario_hash` is structure-only.**
  `cmd_env_hash` = SHA-256 over {argv, rows, cols, respond_to_queries, the resolved PLAN
  env map (sorted)} EXCLUDING daemon-injected infra vars (`SHUX`, `TERM_PROGRAM`) and ALL
  version-bearing values (`TERM_PROGRAM_VERSION`), so a shux release never churns it
  (080 D5; agy's version-churn catch). `scenario_hash` = SHA-256 over the canonicalized
  scenario STRUCTURE (name/description/terminal/steps/command), env VALUES excluded. Both
  populate the `Fingerprint` placeholders (provenance; excluded from `is_stale_vs`).
- **D6 — step schema.** `expect_golden` uses `tier = "cell"|"pixel"|"exact"` (default
  cell), NOT a runtime tolerance — tolerance comes ONLY from the blessed sidecar (080).
  `retries` parse-only (083). `stable_frames` parse-only placeholder → wired to the
  existing `--quiet` settle until 083 (a test pins the placeholder — no half-working
  mode). `xfail` parsed OPAQUELY into the 082 `XfailMeta` shape and RESERVED (081 takes
  no xfail action). Masks are `ROW,COL,WIDTH` row-spans (aligned with `MaskRect` — NOT
  `[row,col,width,height]`). Unknown action / unknown field → fail closed (`parse_error`).
  A scenario with NO `expect_golden` emits `no_visual_check` (text asserts are smoke, not
  visual proof). Top-level `deadline_ms` = whole-scenario budget. `hello.toml` is refined
  (`tol`→`tier`) under a `GATE-TEST-CHANGE:` trailer.
- **D7 — child-exit + `expect_exit` (pre-spawn cursor, strict consume).** The runner
  captures the event head seq BEFORE `lens.run` (pre-spawn cursor) and monitors
  `pane.exited` from that seq, so a fast-exiting child cannot publish its exit before the
  runner listens (codex's race). An UNEXPECTED child exit during ANY step (wait / settle /
  compare) → `child_exit{code}` + short-circuit BEFORE any visual compare + kill/reap/
  cleanup. Only an explicit `expect_exit{code?}` step consumes a buffered/awaited exit →
  `expected_child_exit{code}`; a mismatched code, or no exit within the step deadline, is
  the failure. "The next step is `expect_exit`" does NOT bless an exit mid-wait.
- **D8 — timeout taxonomy: 4 DISTINCT raw signals.** `step_timeout`,
  `frame_settle_timeout`, `scenario_timeout`, `never_stabilized` — 081 never collapses
  them (082 may map the two settle-class causes → `settle_never_stable`). Every timeout →
  kill/reap/cleanup.
- **D9 — `respond_to_queries` reserved-honest.** Parsed (bool). The shux terminal answers
  OSC 11 / DA / XTVERSION deterministically + byte-exact UNCONDITIONALLY (pinned by a
  `process_with_responses` fixture test). 081 does NOT plumb suppression (reserved, not a
  half-working mode); `false` = "this scenario does not rely on query responses" (harmless,
  NOT rejected — `hello.toml` keeps `false`). Suppression is documented as reserved.
- **D10 — quota + isolation + deferrals.** One scenario = one scratch session; reuse
  `SCRATCH_QUOTA=16`; the 17th → `quota_exceeded{limit:16}`; the scenario leaves no
  scratch behind. Parallel scenarios explicitly deferred (documented). mouse / focus /
  bracketed-paste steps → REJECTED at parse with a clear "not supported (tracked)" error
  (non-support explicit, never silently ignored) + a fixture placeholder.

## Testing Matrix

| Layer | Required Evidence |
|---|---|
| L1 parse | Valid scenario parses; malformed scenario → a raw parse-error signal with an actionable message; unknown step fails closed. (Status/exit mapping asserted in 082.) |
| L1 env | Child sees sanitized `LC_ALL`/`TZ`/`TERM`/`COLORTERM`/`NO_COLOR`/`SOURCE_DATE_EPOCH`, isolated `HOME`/`XDG` with sandboxed socket/runtime-dir fallback; allow/deny honored; all recorded in `cmd_env_hash`. |
| L1 queries | OSC 11 / DA / XTVERSION answered with byte-exact deterministic responses under `respond_to_queries` (protocol fixtures). |
| L1 child-exit (mechanic) | A TUI that crashes (exit 139 / exit 1) short-circuits **before** compare and surfaces the raw `child_exit`; `expect_exit` records the intended-exit signal. (Mapping to `child_error` + exit code is asserted in 082.) |
| L1 timeouts (mechanic) | Per-step/per-frame/whole-scenario/never-settle produce their distinct raw signals + child-kill + scratch cleanup. (Verdict mapping asserted in 082.) |
| L1 stable-frames placeholder | The `stable_frames` step parses/validates and behaves as its documented `--quiet` placeholder until 083; no half-working mode is exposed. |
| L2 CLI | `shux lens gate --help` + bad-path invocations return actionable output. |
| L2 quota | 17th concurrent scratch (quota constant 16) → raw quota-exceeded signal; scenario leaves no scratch session behind. (Status/exit mapping asserted in 082.) |
| L3 dogfood | Run a real scenario end-to-end against a fixture TUI at 80x24 and 120x40 under `no_leak_guard.sh`, serial; zero leaked daemons. |
| L3 edge | Cursor, alt-screen, resize, and query-response fixtures exercised; mouse/focus/bracketed-paste explicitly deferred with a placeholder + a tracked issue link + defined non-support behavior (rejected, not silently ignored). |

## Acceptance Criteria

- [x] `shux lens gate <scenario.toml>` drives a TUI via the agnostic step core, no sleeps.
- [x] Child environment is deterministically sanitized and isolated (`env_clear` deny-by-default; `child_env_is_sanitized_and_denies_host`).
- [x] Unexpected child exit short-circuits before compare; `expect_exit` supported (incl. signal-death — adv BLOCKER fixed).
- [x] All four timeout classes behave per spec; never-stable is a failure.
- [x] Masks + redaction apply through the before-serialize path (`pane.glance --mask`).
- [x] Scratch quota + isolation explicit; parallelism explicitly deferred.

## Definition of Done

- [x] DootSabha design review incorporated before coding (codex+agy, chair claude; D1–D10 folded; evidence `.shux/qa/081-*/dootsabha-design.json`).
- [x] Red tests captured before implementation (TDD; the 4-agent `adversarial-review` pass — parser · env/keys · compare/signal · runner/daemon driving the REAL system — found **1 BLOCKER + 1 BLOCKER + 5 MAJOR + several MINOR** each fixed with a regression test).
- [x] L1/L2/L3 tests pass under the serial leak guard; zero leaked daemons proven (`lens_gate_run` 21/21; 59 gate unit tests; existing `test-lens` 37/37, `lens_gate_glance_cells`/`lens_gate_capture` 7/7 — no regression).
- [x] `make check` + new `make test-lens-gate-run` pass.
- [x] `shux-tui-qa` gate `VERDICT: PASS`; scratch evidence under `.shux/out/`, review shots on the PR (audit: every matrix cell PASS, 0 daemon leaks, live signal-kill + short-circuit-before-compare confirmed).
- [x] Implementation-diff DootSabha convergence review clean or addressed (focused impl-review caught + fixed the signal-kill `-1`-vs-`code:None` drift).
- [x] `docs/PROGRESS.md` + this task updated; learnings appended.

## Scope extension — landed by task 084 (2026-07-19)

The 084 cold-agent gauntlet drove this runner against a real `uv`+`rich` project and hit
two gaps in 081's own surface. Both were fixed on this task's files and re-gated with
`make test-lens-gate-run` (21/21 green).

**1. An ENOENT spawn named neither the program, the PATH, nor a remedy** (`gate/runner.rs`,
`program_not_found_message`). The sandbox `DEFAULT_PATH` is `/usr/local/bin:/usr/bin:/bin`,
so no Homebrew / `~/.local/bin` tool is reachable — the same wall the 082 (`bat`) and 083
(`htop`/`vim`) dogfoods hit — and the failure surfaced as a bare "No such file or
directory". The message is now REPLACED (not appended: the note is capped at 240 chars and
the errno preamble ate the budget) with one naming the program, the resolved sandbox PATH
and the three remedies, in ASCII because the summary sanitizes non-ASCII to `?` at the
output boundary. Tests: `enoent_spawn_names_the_program_the_path_and_the_remedies`,
`an_absolute_command_path_and_other_errors_get_no_path_hint`.

**2. A scenario could not point at a program sitting beside it** (`gate/scenario.rs`,
`gate/runner.rs`, `gate/env_plan.rs`). The child's `cwd` was hard-wired to the sandbox HOME
with no knob, and the only workaround — an absolute host path in `command` — lands in
`cmd_env_hash`, which is part of the staleness fingerprint, so the committed golden became
`untrusted` on every other machine. The gate could therefore gate a self-contained shell
one-liner but not a project in a real repo. Scenarios now take an optional **`cwd`,
resolved relative to the scenario file's own directory**, with absolute paths and any `..`
component refused at parse time and a clear `infra` error when the directory is missing.
`cwd` enters `scenario_hash` under `skip_serializing_if = "Option::is_none"`, so every
pre-084 scenario hashes exactly as before. Tests:
`cwd_defaults_to_absent_so_the_child_keeps_the_sandbox_home`,
`a_relative_cwd_parses_and_is_kept_verbatim`, `an_absolute_cwd_is_refused`,
`a_cwd_escaping_the_scenario_dir_is_refused` (the containment test was proven to FAIL with
the guard disabled).

Together these are what let the gate run a real installed tool at all — verified
end-to-end with Homebrew `bat` and with the 084 `uv`+`rich` mock.

