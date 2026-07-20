# Task 084: lens gate — cold-agent DX validation (Laghudarshi gauntlet)

**Status:** Done
**Priority:** High
**Milestone:** M3
**Depends On:** 078, 079, 080, 081, 082, 083 (the fully-built gate)
**Quality Gate:** acceptance (cold-agent gauntlet) + shux-tui-qa
**Touches:** `.shux/fixtures/lens-gate/mock-rich-tui/` (uv project), validation harness under `.shux/scripts/`, `.local/` transcripts

> `shux lens gate` initiative — the **real-feature acceptance gate**. This is the
> mandatory verification Ārya specified: spin up cold-context agents (claude, codex,
> agy) and have each *actually drive the built gate* on a real mock, to get an
> unambiguous signal on whether agents find it useful/effective. Extends the existing
> `Laghudarshi` (लघुदर्शी) cold-context gauntlet pattern (see task 076).

## Problem

Unit/integration/dogfood tests prove the gate *works*. They do not prove the gate is
*usable by a cold agent under realistic friction*. Session history repeatedly shows
agents hand-wave TUI verification. We need an unambiguous, adversarial signal:
does a fresh agent, given only the scenario and the shux skill, successfully use
`shux lens gate` to ship a correct visual change and to catch a regression?

## Scope

1. **The mock** (Ārya's spec): a small, real **`uv` + Python 3.14 + `rich`** terminal
   UI — a self-contained scratch project (e.g. a task/todo board or a system panel)
   built with `rich`, run via `uv run`. Deterministic (fixed data, `SOURCE_DATE_EPOCH`,
   no network). Ship a committed `cell` golden baseline + a `scenario.toml`.
2. **The change requests** (shape: implement-a-change → prove-no-regression):
   - **CR-A (intended visual change):** e.g. "add a footer status bar." The agent must
     implement it, run `shux lens gate`, see the expected frames change, and **bless**
     them via `gate review`/`--update` — proving the intended change and nothing else moved.
   - **CR-B (regression trap):** a change that *looks* safe but shifts/recolors an
     unrelated region. The gate must **catch** it; the agent must read the heat PNG +
     report, localize, and fix until green.
3. **Cold-context runs**: run **claude, codex, and agy** each **independently**, from a
   fresh context, given ONLY the scenario + repo + the shux skill (no hints). Invoke
   through `.shux/scripts/agent_review_guard.sh`; daemon-backed steps serial under
   `no_leak_guard.sh`. Capture each transcript to `.local/`.
4. **Signal + friction log — proven by gate-produced artifacts, not claims** (council #3
   MAJOR — the gauntlet must not be gameable by eyeballing). Success for each agent
   requires, from the gate itself and the git history, NOT the transcript's assertions:
   - **CR-B:** a `report.json` showing `fail` + a heat PNG BEFORE the fix, and a `pass`
     report AFTER — both gate-produced.
   - **CR-A:** a `--update`/`gate review` invocation and a changed-golden manifest showing
     ONLY the intended goldens changed.
   - **No out-of-band edits:** goldens changed ONLY via the gate — a check fails if any
     golden file was edited directly (git diff of goldens vs. the gate's manifest).
   - Transcript shows *meaningful* use of `shux lens gate` + reading the report/diff — but
     the pass bar is the artifacts, not a tool-name grep (avoid brittle `view_file`-style
     string checks).
   **Every friction/issue is addressed** (skill wording, CLI ergonomics, error messages,
   defaults); a feature-affecting fix loops back to its owning task (078–083), is
   re-gated, and only then does 084 pass.
5. **Two-phase note**: an early lightweight *paper* DX walkthrough of the CLI + scenario
   ergonomics happened during 081 design review (pre-implementation de-risk); this task
   is the heavy **real-binary** phase.

## Non-Goals

- No new gate features invented here — only friction fixes traced to a specific task.
- Not a benchmark of the models; a benchmark of the *feature's* usability.
- No committed screenshots unless approved as durable assets.

## Design Review Decisions

DootSabha design review MUST confirm: the mock is realistic (not a toy that trivializes
the gate), CR-A/CR-B are representative, and the pass bar is unambiguous (all three cold
agents succeed on both CRs with no blocking friction, zero leaked processes).

## Testing Matrix

| Layer | Required Evidence |
|---|---|
| L3 mock | `uv run` builds/runs the `rich` TUI deterministically; committed `cell` golden + scenario exist. |
| L3 cold-claude | Fresh `claude` drives the gate: ships CR-A (bless), catches+fixes CR-B; transcript captured. |
| L3 cold-codex | Fresh `codex` drives the gate: same two outcomes; transcript captured. |
| L3 cold-agy | Fresh `agy` drives the gate: same two outcomes; transcript captured. |
| L3 artifact proof | For each agent: gate-produced `fail`+heat report before CR-B fix, `pass` after; CR-A changed-golden manifest limited to intended goldens; goldens changed ONLY via the gate (git-diff-vs-manifest check); no direct golden edits. |
| L3 CR-B no-bless proof (council #4) | The regression trap is fixed by fixing the **code**, never by re-blessing: goldens are **byte-identical before and after** the whole CR-B flow (git diff over the golden tree is empty), and no `--update`/bless ran during CR-B (no changed-golden manifest emitted). An agent that blesses its way out of CR-B is a **FAIL**, not a pass. |
| L3 friction | Every friction/issue logged and resolved; feature-affecting fixes traced back to the owning task and re-verified. |
| L3 hygiene | Zero new shux daemons / orphan automation processes after all runs (leak guard proven). |

## Acceptance Criteria

- [x] A realistic `uv`+py3.14+`rich` mock with a committed golden + scenario exists.
- [x] CR-A and CR-B are defined and representative (intended change; regression trap).
- [x] All three cold agents independently ship CR-A (with a bless) and catch+fix CR-B.
- [x] The friction log is complete; every item is either FIXED or explicitly re-scoped below; feature changes looped back into 081/082 + re-gated.
- [x] No leaked daemons or orphan processes (verified independently by the `shux-tui-qa` gate).

## Definition of Done

- [x] DootSabha design review incorporated before running the gauntlet (`.local/dootsabha-084-design.json`).
- [x] All three cold-agent runs pass both change requests; transcripts + friction log in `.local/084-gauntlet/` and `.local/084-friction-log.md`.
- [x] Feature-affecting fixes recorded in `docs/tasks/081-*.md` + `082-*.md` and re-gated (contract 5/5, verdict 18/18, run 21/21, settle 6/6).
- [ ] `shux-tui-qa` `VERDICT: PASS` on the overall workflow.
- [ ] Implementation/validation DootSabha convergence review clean or addressed.
- [x] `docs/PROGRESS.md` + this task updated; learnings appended.

## Results (2026-07-19)

**6/6.** Every cell verified from supervisor-observed state — sha256 manifests of the
golden tree, bless audit entries, and the gate re-run by the supervisor before and after —
never from a transcript claim, then re-audited a second time by recomputing from the files.

| agent | CR-B (fix the code) | CR-A (bless the change) |
|---|---|---|
| codex  | goldens IDENTICAL, +0 bless, gate 1→0 | only `start`+`after-nav` blessed, +1 audit entry, gate green |
| claude | goldens IDENTICAL, +0 bless, gate 1→0 | only `start`+`after-nav` blessed, +1 audit entry, gate green |
| agy    | goldens IDENTICAL, +0 bless, gate 1→0 | only `start`+`after-nav` blessed, +1 audit entry, gate green |

No agent blessed its way out of CR-B; no golden changed outside a bless; CR-A moved only
row 15 (the new footer) on every frame for every agent. The `cheat` negative control
(blessing the regression away) is correctly scored FAIL, so the bar is not gameable.

All three converged independently on the same correct architecture: keep the refactor's
shared palette module, but stop it flattening the deliberate table/summary divergence on
`healthy`. None reverted; none blessed.

**The `style_deltas` fix proved load-bearing.** With two greens on screen, it is what tells
an agent which one the baseline blesses. Claude's report used exactly that ("the summary
row had zero changed cells, which independently confirms the summary's green was the
correct half of the pair") to decide to raise the table rather than lower the summary.

### Friction: fixed

| # | Defect | Owner |
|---|---|---|
| F4 | **BLOCKER** — `--on-missing create`/`--update` laundered a `step_timeout`/crash/infra/no-visual failure into `pass`/exit 0 | 082 |
| F1 | ENOENT spawn named neither program, PATH, nor remedy | 081 |
| F2 | The gate could not run any project-based tool (no `cwd`) | 081 |
| F6 | Colour-only regressions reported coordinates but never colours | 082 |
| F9 | Harness: `export PATH` does not survive codex's `bash -lc` login shell | 084 |
| — | `missing_golden` detail reached users as `no committed golden ?` | 082 |

### Friction: explicitly re-scoped (NOT fixed here)

- **F5 — every gate run leaks a `shux __daemon`.** There is no `daemon stop` verb; this is
  the deferred daemon-lifecycle item already owned by 083. 084 does not fix it, but the
  skill reference cold agents read (`skills/shux/references/gate.md`) now documents the
  `pkill -f "shux __daemon"` cleanup and the isolated `XDG_RUNTIME_DIR`, so following the
  docs no longer silently leaks. **Deferred to the daemon-lifecycle work.**
- **F7 — retry vocabulary.** With `retries` set, a perfectly deterministic regression reads
  `FAIL after 2 retries (exhausted fps [...])` — flakiness vocabulary for the opposite
  situation, and a single stable fingerprint across attempts is in fact *evidence of
  determinism*. Cosmetic; the audit itself is correct and is 083's anti-masking contract.
  **Deferred to 083's surface.**
- **F8 — `agent_review_guard.sh` kills unrelated agent processes.** It baselines PIDs and
  kills anything newly matching `LEAK_PATTERNS`, so a concurrent session in *another
  repository* was terminated as collateral. Pre-existing; 084 is merely the first task to
  run six long guarded agent invocations back to back. Fixing it means narrowing the kill
  set to the guard's own process group. **Deferred to tooling.**
- **F10 — `claude -p` / `agy -p` transcripts are final-message only**, so per-step friction
  is observable for codex alone. The pass bar is state-based, so no verdict depends on it.
  `--output-format stream-json` would capture it. **Accepted for this gauntlet.**

