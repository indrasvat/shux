# T5 — unaided-agent demo brief (task 077, PRD §13 T5 / §15 P6 m4)

**Purpose:** prove the rewritten `skills/shux` skill is sufficient, on its
own, for a fresh coding agent with zero prior context on this task to find
and fix a seeded visual bug using only the `lens` loop — no hints, no
access to this implementation session, no reading of the fixture's own
source comments before it has already found the bug visually.

**This document is for the orchestrator spawning the demo agent, not for
the demo agent itself.** Do not paste this file's contents (especially the
"Ground truth" section) into the demo agent's context.

## What to give the fresh agent

1. A clean checkout of this branch (`feat/lens-p6-skill-polish`), or main
   once merged — anything containing `skills/shux/**` and
   `.shux/fixtures/lens/t/demo-app/`.
2. Access to the `skills/shux` skill (`SKILL.md` + `references/` +
   `examples/`) — this is the ONLY guidance it should be given about *how*
   to use shux. Do not summarize the lens loop for it yourself; let it read
   the skill.
3. A built `shux` binary on PATH (`make build`, or the published
   `curl -fsSL https://shux.pages.dev/install.sh | sh` binary if you want
   the demo to also double as a public-binary smoke test).
4. The prompt below, verbatim or close to it. Do NOT mention "border",
   "column 80", "top", or any other detail that leaks the bug's location or
   nature.

### Prompt for the fresh agent

> There's a tiny ratatui TUI app at `.shux/fixtures/lens/t/demo-app/`
> (`cargo build` inside that directory; binary name `lens-demo-app`). A
> user reported "something looks visually wrong with it" but couldn't say
> what. You have the `shux` skill available. Use it to find the bug, fix
> it, and prove the fix worked. Run the app at least 120 columns wide —
> narrower panes may not show the issue. Attach before/after evidence of
> what you found and fixed.

## What "success" looks like (orchestrator judges against this)

- [ ] The agent used `shux lens run` (or equivalent RPC) to spawn the app
      hidden, at a width > 80 columns, rather than attaching interactively
      or reading the source first to guess the bug.
- [ ] The agent used `pane wait-settled` (or `pane wait-for`) before
      glancing — not a bare `sleep`.
- [ ] The agent used `pane glance` (not just `pane capture`) and actually
      inspected the PNG — the bug is pixel-only; text capture cannot see
      it. A transcript that jumps straight from "ran the app" to "found the
      bug" without a glance/PNG step in between is a FAIL even if it
      landed on the right fix — that means it read the source, not the
      pixels.
- [ ] The agent correctly identified: a one-cell gap in the top border,
      specifically at column 80 (the border reads as a space instead of
      `─`, visible only when the pane is wider than 80 columns).
- [ ] The fix removes the seeded-bug block in `top_border()` (see "Ground
      truth" below) — or otherwise makes the top border continuous — without
      breaking the rest of the border/layout.
- [ ] The agent re-ran, re-glanced, and attached (or otherwise produced) a
      before.png and after.png (or equivalent) showing the border intact.
- [ ] Clean teardown — no leftover scratch sessions, no orphan
      `lens-demo-app` processes (`shux session list --include-scratch`
      empty of demo-app entries after the agent finishes;
      `pgrep -x lens-demo-app` empty).

A partial credit case worth noting rather than failing outright: if the
agent finds the bug via `pane glance` but describes it slightly differently
(e.g. "the border has a broken segment" rather than naming the exact
column) — that's still a pixel-verified find, just less precise language.
The hard requirement is that the bug was discovered via a PNG, not by
reading `main.rs`.

## Evidence to collect from the demo run

- Full transcript of the agent's shux CLI/RPC calls (the orchestrator's own
  session log, or ask the agent to summarize the exact commands it ran).
- The before/after PNGs the agent produced.
- The code diff the agent applied.
- Confirmation of clean teardown (command output, not just the agent's
  claim).

Package these as the T5 evidence in whatever the orchestrator's QA report
format is (this task used `.shux/qa/lens-p6/` for P6 evidence — scratch
`.shux/out/lens-p6-t5-demo/` is also fine for the raw transcript/PNGs per
this repo's "screenshots are scratch by default" rule).

## Ground truth (orchestrator-only — do not leak to the demo agent)

`.shux/fixtures/lens/t/demo-app/src/main.rs`, function `top_border()`:

```rust
const BREAK_COL: usize = 80;
...
// ── SEEDED BUG ────────────────────────────────────────────────────────
// Punch a one-cell gap in the top border at column 80. The fix is to
// delete this block so the border stays continuous.
if width > BREAK_COL {
    cells[BREAK_COL] = ' ';
}
// ──────────────────────────────────────────────────────────────────────
```

The bug only manifests when the pane is wider than 80 columns (`width >
BREAK_COL`) — the prompt above hints "at least 120 columns" specifically so
the agent doesn't accidentally dodge the bug by running at the CLI's
default 80×24. Deleting the `if width > BREAK_COL { cells[BREAK_COL] = '
'; }` block (and the seeded-bug comment around it, for cleanliness) is the
complete, minimal fix — `top_border()`'s only other job is drawing a
correct `┌─…─┐` border, which the surrounding code already does.

## Why this demo matters (context for the orchestrator, not the agent)

This is PRD §13 T5 / §15 P6's m4 requirement: "skill tested in a clean
agent env: fresh agent + skill only completes the E1 loop." Everything
upstream of this (K1/E1 goldens, the CLI UUID-resolution fix, the skill
rewrite itself) is necessary but not sufficient — the actual claim being
tested is "an agent with NO other context can pick up this skill cold and
successfully run the run→settle→glance→drive→diff loop to find something
text capture cannot see." A demo agent that solves this by reading
`main.rs` first (skipping the pixel step) would technically "fix the bug"
but would falsify the claim this test exists to prove.
