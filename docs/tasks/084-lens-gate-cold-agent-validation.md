# Task 084: lens gate — cold-agent DX validation (Laghudarshi gauntlet)

**Status:** In Progress
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

- [ ] A realistic `uv`+py3.14+`rich` mock with a committed golden + scenario exists.
- [ ] CR-A and CR-B are defined and representative (intended change; regression trap).
- [ ] All three cold agents independently ship CR-A (with a bless) and catch+fix CR-B.
- [ ] The friction log is complete and every item is resolved; feature changes looped back + re-verified.
- [ ] No leaked daemons or orphan processes.

## Definition of Done

- [ ] DootSabha design review incorporated before running the gauntlet.
- [ ] All three cold-agent runs pass both change requests; transcripts + friction log in `.local/`.
- [ ] Any feature-affecting friction fix merged into the owning task and re-gated.
- [ ] `shux-tui-qa` `VERDICT: PASS` on the overall workflow.
- [ ] Implementation/validation DootSabha convergence review clean or addressed.
- [ ] `docs/PROGRESS.md` + this task updated; learnings appended.
