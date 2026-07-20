# Task 085: lens gate — skill + landing site

**Status:** In Progress
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

- [ ] `SKILL.md` + `references/gate.md` + a new example document the gate accurately.
- [ ] The sleep-based CI examples are replaced with `shux lens gate` + a 1:1 migration.
- [ ] The landing site has a gate section in the existing theme/voice; outcome-first, pixel-perfect framing, no LoC marketing.
- [ ] Hi-res desktop + mobile screenshots captured through shux and attached to the PR.
- [ ] No doc/site drift from the shipped surface.

## Definition of Done

- [ ] DootSabha design review (copy/layout) incorporated.
- [ ] L1/L2/L3 checks pass; migration example runs green.
- [ ] `make check` passes; site builds.
- [ ] `shux-tui-qa` `VERDICT: PASS`; screenshots attached to the PR as comments (not committed unless approved assets).
- [ ] Post-merge `curl | sh` smoke of the public binary (per memory) confirms `shux lens gate` works from the released build.
- [ ] Implementation-diff DootSabha convergence review clean or addressed.
- [ ] `docs/PROGRESS.md` + this task updated; learnings appended.
