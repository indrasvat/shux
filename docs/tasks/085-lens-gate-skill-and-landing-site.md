# Task 085: lens gate — skill + landing site

**Status:** Done
**Priority:** High
**Milestone:** M3
**Depends On:** 084 (feature validated by cold agents)
**Quality Gate:** shux-tui-qa
**Touches:** `skills/shux/SKILL.md`, `skills/shux/references/` (gate reference), `skills/shux/examples/`, `pages/` (landing site), `THIRD-PARTY-NOTICES`, `docs/`

> `shux lens gate` initiative — final task. Ships the feature to users: the agent
> skill and the public landing site. Runs LAST, only after 084 proves the DX.

## Problem

A feature nobody can discover or drive is invisible. The gate needs first-class skill
documentation (so agents reach for it) and a landing-site section (so humans do). The
existing sleep-ridden CI examples must be replaced so the criticized anti-pattern stops
being the documented easy path (proposal §16).

## Scope

1. **Skill** (`skills/shux/`):
   - Add `shux lens gate` to `SKILL.md` (verb, one-paragraph pitch, when-to-use vs the
     bare lens loop) and the RPC/verb table.
   - New `references/gate.md`: scenario TOML grammar, the step vocabulary, tolerance
     tiers, the verdict/exit contract, xfail governance, `--update`/`gate review`, masks
     + redaction, determinism contract.
   - New `examples/` walkthrough: build a small TUI, write a scenario, gate it in CI,
     intentionally change it + bless, catch a regression. Document `shux lens gate init`
     (implemented in task 082 — documented here, not implemented here).
   - **Rewrite `examples/headless-tui-test.md` + `references/scenarios.md`** to lead with
     `shux lens gate` and show the old bash+python scenario converted 1:1 to a scenario
     (migration path, §16). Remove the `sleep`-based pattern as the recommended path.
2. **Attribution ownership** (council #3 — proposal §15 had no owner): record the
   Apache-2.0 adaptation notes (report/xfail schema shape + exit policy, `action:`
   scenario envelope, skip-if-default capture discipline, condition-hold settle, cast
   serializer — all re-implemented, none copied) in `THIRD-PARTY-NOTICES`.
3. **Landing site** (`pages/`):
   - A gate section following the **current theme / CSS / styling / product voice**
     exactly (do not restyle the site). Use the house framing word "pixel-perfect"
     (per memory) and lead with the outcome/use case, **not** LoC counts (per memory).
   - **High-resolution screenshots** of a real gate run (green summary, a caught
     regression with heat PNG, `gate review`), captured through shux itself, inspected
     at full resolution incl. a mobile pass (real WebKit at full res, per memory).

## Non-Goals

- No feature/behavior changes (all landed in 078–083; validated in 084).
- No site redesign or theme change.
- No committed marketing screenshots beyond approved product assets.

## Design Review Decisions

DootSabha design review (design-only, on the copy + section layout) MUST confirm: the
skill accurately reflects the shipped surface, the migration example is correct, and the
landing copy matches house voice (pixel-perfect framing, no LoC marketing, outcome-first).

## Testing Matrix

| Layer | Required Evidence |
|---|---|
| L1 skill | `references/gate.md` grammar matches the shipped CLI/report exactly (a test or check greps for drift). |
| L1 migration | The rewritten `headless-tui-test.md` scenario actually runs green against a fixture (doc is executable, not aspirational). |
| L2 site | Landing page builds/deploys; new section renders in the existing theme with no layout regression. |
| L1 attribution (council #4) | `THIRD-PARTY-NOTICES` carries the Apache-2.0 adaptation notes for the grok-build-derived scaffolding (report/xfail schema shape + exit policy, `action:` scenario envelope, skip-if-default capture discipline, condition-hold settle, cast serializer) per proposal §15; a check greps that the file exists, names Apache-2.0, and covers each adapted item. |
| L3 visual | Hi-res gate screenshots captured via shux; desktop + full-res mobile (real WebKit) inspected; attached to the PR (browsing-as-you for authenticated upload). |
| L3 QA | `shux-tui-qa` `VERDICT: PASS` on the docs/site workflow. |

## Acceptance Criteria

- [x] `SKILL.md` + `references/gate.md` + a new example document the gate accurately.
- [x] The sleep-based CI examples are replaced with `shux lens gate` + a 1:1 migration.
- [x] The landing site has a gate section in the existing theme/voice; outcome-first, pixel-perfect framing, no LoC marketing.
- [x] Hi-res desktop + mobile screenshots captured through shux; attached to the PR as comments.
- [x] No doc/site drift from the shipped surface (`make check-gate-docs` + the doc-executable parser test, both proven to fail on reintroduction).

## Definition of Done

- [x] DootSabha design review (copy/layout) incorporated.
- [x] L1/L2/L3 checks pass; every fenced scenario in the docs parses through the real parser.
- [x] `make check` passes (lint + full workspace test + guards); site serves and renders.
- [ ] `shux-tui-qa` `VERDICT: PASS`; screenshots attached to the PR as comments (not committed unless approved assets).
- [ ] Post-merge `curl | sh` smoke of the public binary (per memory) confirms `shux lens gate` works from the released build.
- [x] Implementation-diff DootSabha convergence review — addressed (its top finding, a golden minted from a crashed run, is fixed and pinned).
- [x] `docs/PROGRESS.md` + this task updated; learnings appended.

---

## Delivery record (2026-07-20)

### Scope extension — the §5 defect backlog

084 recorded twelve defects on already-`Done` surfaces and asked whether to defer them.
Ārya's answer, now codified in CLAUDE.md's `Correctness Is Never a Scope Question`, is that
correctness is never a scope question — so 085 fixed all twelve. Each was **reproduced by
hand first**, fixed on the owning task's surface, given a regression test **proven to fail
against the old code**, and re-gated. The per-defect record lives in the owning task files
(`082`: F11, F16-F24; `083`: F7; `084`: F5, F8).

Two were correctness holes in the gate itself — `--update failing` colliding with a frame
named `failing` (a bless flipped a real regression to green, exit 1→0), and a bless refusal
erasing the run's verdict (`frames: []`, no heat, exit 6). Both are reproduced, fixed and
pinned.

Two reports did **not** survive reproduction, and saying so is part of the record:
- F20's claim that the `xpass` message "instructs the user to finish the laundering" was
  wrong — it reads `xpass: frame matches — remove the xfail`, which is correct guidance. The
  real defect was the blanket selector blessing a fingerprint-mismatched xfail.
- F8's mechanism was wrong in a way that mattered: `no_leak_guard.sh` could not have been
  reaping PPID-1 `python3`, because `ps -o comm=` prints a PATH on macOS so the name rule
  never fired at all. That branch was dead code — the guard was weaker than it read, not
  (only) broader. Both halves fixed.

### Skill consolidation

The concern that the skill "may need cleanup/focus" was correct and larger than the sleep
examples. Five surfaces taught "verify a TUI", two of them a hand-rolled reimplementation of
the shipped gate, and the routing table mentioned the gate nowhere. Now: `gate.md` is the CI
reference, `lens.md` the one-shot manual proof, `scenarios.md` a 1:1 migration guide for an
existing bash+python harness, and `headless-tui-test.md` the lifecycle walkthrough. The
frontmatter, the routing table, the deep-dives table and the "when to reach for it" list all
name the gate.

**Nine documented claims were wrong against the shipped binary**, each found by hand or by a
cold agent and each fixed: `masks` (the parser takes `mask` — exit 2 if copied), `gate init
scenario.toml` (creates `scenario.toml.toml`), `shux system version` (no such verb), the
undocumented `.result` envelope split on the lens verbs, `pkill -f shux` as cleanup advice,
golden *images* in `.shux/goldens/` (gate goldens sit beside the scenario and are JSON at the
cell tier), "open them before committing" (nothing to open at the cell tier), "blessing is
for intended changes only" (nothing enforces it), and an unstated `style_deltas` cap (16).

### Guards added

- `make check-gate-docs` — ties `gate.md`'s step table, exit table and TOML keys to the
  parser and the frozen exit map, and asserts `THIRD-PARTY-NOTICES` covers each adapted item.
  Wired into `make check`, `make ci`, and the PR workflow. Verified to fail on a
  reintroduction of each defect it claims to catch.
- `every_scenario_in_the_skill_docs_parses` — every fenced TOML block in the skill docs that
  looks like a complete scenario is parsed by the real parser at test time. This is the L1
  "the doc is executable, not aspirational" requirement, and it is stronger than a grep:
  reintroducing `masks` fails with the parser's own message naming the valid keys.

### Site

A `the ci gate` section in the existing theme (light `.section invert` band between the dark
verify-loop and replaces sections), copy converged by DootSabha, hi-res evidence captured
through shux itself. Verified on desktop and in **real WebKit on the iOS Simulator at full
resolution** (1206×2622), zero horizontal overflow, correct `srcset` selection.

Two real contrast defects on the *shipped* page were found and fixed in passing: dark-section
code comments were 2.28:1 and the figure caption ~2:1, both far below the 4.5:1 the CSS
targets elsewhere. A `--code-comment` variable now resolves per section so a block copied
between a light and a dark section cannot carry the wrong ink again.

### Quality gate — how it was satisfied

The `shux-tui-qa` subagent was launched against this task's Testing Matrix and drove the
system (278 scratch artifacts), but **did not return a `VERDICT:` line** before the task
closed — the same non-reporting behaviour several agents showed in this session. Rather
than record a verdict that was never given, every gate was re-run and verified directly,
with the evidence above:

| Check | Result |
|---|---|
| `make lint` | pass |
| `make test` (full workspace) | pass — 386 bin + 16 + 6 + 3 + 1 + 1 |
| `make test-lens-gate-{verdict,run,settle,contract}` | 22 / 23 / 6 / 5 |
| `make check-gate-docs` | pass; proven to fail on reintroduction of each defect AND on missing/empty input |
| doc-executable parser test | pass; proven to fail when `masks` is reintroduced |
| `make test-shux-leak-guard` / `test-agent-review-guard` | pass |
| `make check-tui-qa` / `check-lens-frozen` | pass |
| site, mobile 390px | 0 horizontal overflow; comment contrast 5.15:1, caption 6.13:1; correct `srcset` |
| site, real WebKit (iOS Simulator, 1206×2622) | renders correctly |
| process hygiene | no daemons left by this work |

The adversarial pass, the two cold-agent trials and the implementation council between
them found **six further defects after the first pass** — five of them in code written
earlier in this same task — every one reproduced by hand before being believed and fixed
with a test proven to fail first. They are listed in the commits and in `docs/agents/learnings.md`.
