# Task 076: Sightline TUI QA Plugin

**Status:** Done
**Priority:** High
**Milestone:** M2
**Depends On:** 075
**Touches:** `plugins/sightline/`, `.codex/agents/shux-tui-qa.toml`, `.claude/agents/shux-tui-qa.md`, `Makefile`, `.shux/scripts/`

---

## Problem

Shux is increasingly used by agents to verify terminal UI work, but repeated
session-history evidence shows the same gaps: missing color probes, skipped
keyboard navigation, weak visual inspection, unverified alignment/wrapping, and
cleanup checks done late or not at all.

The plugin system now has a local package and scaffold foundation. The next
step is to dogfood it with a first-party plugin that makes rigorous TUI QA easy
for agents to run and hard to hand-wave.

## Scope

Implement a narrow first Sightline package:

- add a first-party local plugin package at `plugins/sightline/`,
- include a valid `shux-plugin.toml` process-plugin manifest,
- make `plugins/sightline/bin/sightline` the v1 product: a direct executable
  one-shot QA runner that verifies a Shux session, window, or pane using the
  public Shux CLI/API,
- keep process-plugin handshake support only as package/lifecycle smoke so the
  package can be installed with `shux plugin install plugins/sightline`,
- generate structured reports under gitignored scratch storage
  `.shux/out/sightline/`,
- include deterministic checks for capture text, PNG validity, PNG dimensions,
  non-blank pixels, rendered color content, viewport dimensions, stable markers,
  keyboard state-delta behavior when the target supports it, and cleanup
  instructions,
- verify three color classes: truecolor, 256-indexed color, and basic SGR. Use
  byte-exact recording or equivalent raw PTY evidence to prove SGR emission and
  PNG pixel sampling to prove Shux rendered color,
- update general TUI QA agent instructions so scratch evidence + PR-comment
  screenshots are the default, while committed `.shux/qa` artifacts remain an
  explicit exception,
- add focused Makefile/script targets for local validation.

## Current Plugin Host Constraint

Task 075 validates manifest `commands` metadata but does not yet route custom
plugin commands through the Shux CLI. Sightline v1 must not pretend that
`shux sightline ...` or `shux plugin run sightline ...` exists unless this task
also implements that host surface.

The pragmatic v1 shape is therefore:

- `plugins/sightline/bin/sightline ...` provides the direct one-shot QA runner
  and is the actual Sightline v1 product,
- `shux plugin install plugins/sightline` proves package install/lifecycle only,
- a later host task can promote package commands into first-class `shux plugin
  run <plugin> <command>` UX if needed.

The package entrypoint must use an explicit plugin-host mode, such as
`entry.args = ["--plugin-host"]`, so direct CLI runs and daemon plugin handshakes
cannot be confused.

## Non-Goals

- No remote plugin registry or package download flow.
- No AI vision engine or LLM-based visual judgment.
- No durable committed screenshots for routine TUI QA.
- No new VT/raster baseline format.
- No general plugin command dispatcher unless design review shows it is a small,
  clean prerequisite.
- No marketplace packaging, signing, or binary release automation.
- No claim that `shux plugin install plugins/sightline` proves Sightline's QA
  effectiveness. It proves package resolution and process-plugin lifecycle only.

## Design Review Decisions

DootSabha design review returned `proceed with changes`:

- defer host command dispatch to a separate council-first task,
- treat `plugins/sightline/bin/sightline` as the product and plugin install as
  lifecycle smoke,
- drop or clearly forward-declare manifest `[commands]` metadata because Shux
  does not dispatch it yet,
- upgrade snapshot checks from presence to content assertions,
- cover truecolor, 256-indexed color, and basic SGR,
- make keyboard checks prove an observable before/after state delta or keep them
  advisory,
- keep `scripts/check-tui-qa.sh` unchanged for now; fix the stale agent prose so
  committed `.shux/qa` artifacts are an explicit exception rather than the
  default path,
- require an independent `shux-tui-qa` PASS so Sightline does not certify itself.

## Laghudarshi Effectiveness Gauntlet

Add a real-use effectiveness check named `लघुदर्शी` / `Laghudarshi` after the
deterministic runner works.

The gauntlet must create a scratch, single-script, `uv`-managed Python Textual
TUI with deliberately seeded UI/UX issues, then hand a cold-context junior-dev
subagent only the scenario and the instruction to use Shux + Sightline. The goal
is to see whether Sightline actually helps an inexperienced agent catch and fix
misses that appear repeatedly in local session history.

Seeded defects should include several of:

- missing or weak color contrast,
- color bleed after reset,
- clipped or overflowing text at 80x24,
- alignment drift between 80x24 and 120x40,
- keyboard navigation that appears present but fails to change focused state,
- missing visible focus indicator,
- stale loading/status text after an interaction.

The gauntlet passes only if the subagent uses Sightline evidence, fixes the
seeded issues, re-runs verification, and leaves no Shux daemons or orphan
automation processes behind.

## Testing Matrix

| Layer | Required Evidence |
|---|---|
| Level 1 unit | Sightline report/checklist rendering is deterministic and fails closed when required inputs are missing. |
| Level 1 unit | Plugin manifest and entrypoint remain valid under `resolve_plugin_package`; plugin-host mode is explicit. |
| Level 1 unit | PNG helper rejects invalid PNGs, all-black/blank PNGs, wrong dimensions, and missing expected color samples. |
| Level 1 unit | General TUI QA agent docs no longer require committed screenshots for ordinary scratch evidence. |
| Level 2 CLI | `plugins/sightline/bin/sightline --help` and invalid-target paths return actionable output. |
| Level 2 plugin | `shux plugin install plugins/sightline`, `plugin list`, and `plugin stop sightline` work with the release-like binary. |
| Level 2 script | Makefile target runs the Sightline focused checks. |
| Level 3 dogfood | Sightline runs against a real colored Shux pane at 80x24 and 120x40, writes a report, text capture, raw SGR evidence, pixel summary, and PNG snapshot under `.shux/out/sightline/`, then leaves no new Shux daemons or orphan automation processes. |
| Level 3 dogfood | Sightline runs against at least one real rich TUI available on the machine, such as `vim`/`nvim`, `lazygit`, `btop`/`htop`, `vicaya`, or `vivecaka`. |
| Level 3 effectiveness | `Laghudarshi` cold-context junior-dev gauntlet catches and fixes seeded defects in a scratch `uv` Textual TUI using Shux + Sightline evidence. |
| Level 3 QA | Independent general TUI QA gate verifies the Sightline workflow; review-worthy screenshot evidence is kept out of git and attached to the PR as comments. |

## Acceptance Criteria

- [x] Sightline has a clear first-party package shape under `plugins/sightline/`.
- [x] The package installs and stops through existing plugin lifecycle commands,
  explicitly as lifecycle smoke rather than proof of QA effectiveness.
- [x] The one-shot runner produces a useful PASS/FAIL report with evidence paths.
- [x] The runner exercises at least capture text, PNG content assertions, three
  color classes, keyboard state-delta checks or advisory reporting,
  viewport/dimension reporting, and cleanup guidance.
- [x] Failure output is actionable enough for an agent to fix the missed QA step.
- [x] Stale general TUI QA agent instructions no longer force ordinary
  screenshots into the repository.
- [x] `Laghudarshi` cold-context gauntlet proves Sightline is effective on a
  deliberately flawed realistic Textual TUI.
- [x] No screenshots are committed for this task unless DootSabha explicitly
  approves them as durable baselines.
- [x] The implementation does not add a broad plugin registry or marketplace
  surface.

## Definition of Done

- [x] DootSabha design review findings are incorporated before coding.
- [x] Red tests or failing smoke checks are captured before implementation.
- [x] Level 1, Level 2, and Level 3 tests pass.
- [x] `make test-sightline` passes.
- [x] `make check` passes.
- [x] General TUI QA gate reports `VERDICT: PASS`.
- [x] Implementation-diff DootSabha review is clean or all findings are
  addressed.
- [x] PR includes screenshot evidence as comments, not committed binary cruft.
- [x] `docs/PROGRESS.md` and this task are updated.
- [x] Relevant learnings are appended to `docs/agents/learnings.md`.
