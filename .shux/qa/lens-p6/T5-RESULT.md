T5: PASS

# T5 — unaided-agent demo result (task 077, PRD §13 T5 / §15 P6 m4)

**Judged by:** the task-077 orchestrator, against the ground truth in
[T5-DEMO-BRIEF.md](T5-DEMO-BRIEF.md) (which the demo agent never saw).
**Demo agent setup:** a FRESH agent in its own isolated worktree, given ONLY
the rewritten `skills/shux` skill and the brief's prompt — zero coaching,
zero prior task-077 context, no access to any implementation session.
**Date:** 2026-07-10. **Verdict: all 7 checklist items PASS.**

This is the claim the whole phase existed to prove, demonstrated: an agent
with no context beyond the skill completed the lens loop
(run → settle → glance → drive/fix → diff/prove) and found a bug that text
capture cannot see, from the pixels alone.

## The 7-point checklist (each judged against the brief)

1. **Hidden spawn via lens, wide enough to manifest the bug — PASS.**
   `shux lens run --size 120x30 -- lens-demo-app` (hidden scratch session;
   120 cols > the 80-col trigger). Did not attach interactively; did not
   read the source first.
2. **Event-driven wait, no sleeps — PASS.**
   `shux pane wait-settled <PANE> --quiet 300ms --timeout 10s` before every
   glance. No bare `sleep` anywhere in the transcript.
3. **Bug discovered via the PNG, not the source — PASS (the critical
   item).** `shux pane glance <PANE> --checkpoint --png before.png`, then
   actual pixel inspection: the agent FOUND THE GAP IN THE PIXELS and
   localized it to column 80 via an ImageMagick bounding-box measurement
   (`9x2+720+8` — x=720px / 9px-per-cell = col 80) BEFORE ever opening
   `main.rs`. The discovery order (pixels → localization → source) is
   exactly what this test exists to prove; a source-first find would have
   failed this item even with the right fix.
4. **Correct identification — PASS.** A one-cell gap in the top border at
   column 80, visible only when the pane is wider than 80 columns.
5. **Clean minimal fix — PASS.** Removed the seeded-bug block AND the
   then-orphaned `BREAK_COL` const (see diff below); the border renders
   continuously; no collateral changes to layout or the rest of the border.
6. **Re-run + before/after proof — PASS.** Rebuilt, re-ran the loop
   (lens run → wait-settled → glance), produced before/after full-frame
   PNGs, before/after zoom crops of the break region, and pixel-diff
   images. The before-zoom shows the gap plainly; the after-zoom shows the
   continuous border.
7. **Clean teardown, proven with command outputs — PASS.**
   `shux session kill <SID>` after each run; final
   `session list --include-scratch` returned `{"sessions": []}`; `pgrep`
   returned rc=1 (no matches) for BOTH binaries (`lens-demo-app` and
   `shux`); the agent also noticed and explicitly killed the auto-started
   daemon by pid (see "Honest finding" below).

**Unprompted bonus:** the agent followed branch-first git discipline for
its fix without being told (created a fix branch in its worktree before
editing) — the repo's git rules were absorbed from context, not coached.

## Command transcript (as judged — the loop, both passes)

```text
# Discovery pass (buggy build)
shux lens run --size 120x30 -- <path>/lens-demo-app     # → {session_id, pane_id}
shux pane wait-settled <PANE> --quiet 300ms --timeout 10s
shux pane glance <PANE> --checkpoint --png before.png
# → pixel inspection; ImageMagick bbox 9x2+720+8 → col 80, top border row
shux session kill <SID>

# (fix applied to demo-app source; rebuilt)

# Proof pass (fixed build)
shux lens run --size 120x30 -- <path>/lens-demo-app
shux pane wait-settled <PANE> --quiet 300ms --timeout 10s
shux pane glance <PANE> --checkpoint --png after.png
shux session kill <SID>

# Teardown proof
shux --format json session list --include-scratch        # → {"sessions": []}
pgrep -x lens-demo-app                                   # → rc=1 (none)
kill <daemon-pid>                                        # explicit daemon reap (see note)
pgrep -x shux                                            # → rc=1 (none)
```

## The fix the agent applied

Reconstructed minimal diff, matching the judged summary (seeded block +
orphaned const removed) against
`.shux/fixtures/lens/t/demo-app/src/main.rs`; the demo agent's exact
hunk context may differ trivially:

```diff
--- a/.shux/fixtures/lens/t/demo-app/src/main.rs
+++ b/.shux/fixtures/lens/t/demo-app/src/main.rs
@@
-/// Column at which the top border is (buggily) broken.
-const BREAK_COL: usize = 80;
-
 fn top_border(width: usize) -> String {
     if width < 2 {
         return "─".repeat(width);
     }
     // A correct top border: ┌────…────┐
-    let mut cells: Vec<char> = std::iter::once('┌')
+    let cells: Vec<char> = std::iter::once('┌')
         .chain(std::iter::repeat_n('─', width - 2))
         .chain(std::iter::once('┐'))
         .collect();
-
-    // ── SEEDED BUG ────────────────────────────────────────────────────────
-    // Punch a one-cell gap in the top border at column 80. The fix is to
-    // delete this block so the border stays continuous.
-    if width > BREAK_COL {
-        cells[BREAK_COL] = ' ';
-    }
-    // ──────────────────────────────────────────────────────────────────────
-
     cells.into_iter().collect()
 }
```

The fix lives only in the demo agent's isolated worktree. The seeded bug
stays committed in this repo BY DESIGN — it is the durable fixture for
future T5-style demo runs (see the brief).

## Evidence (scratch, NOT committed — repo screenshot policy)

`.shux/out/lens-p6-t5-demo/` (gitignored):
`before.png`, `after.png` (full 120×30 frames), `before-zoom.png`,
`after-zoom.png` (break-region crops — the gap at col 80 is plainly visible
before and gone after; verified visually by the orchestrator and the P6
implementer), `diff.png`, `diff-fuzz.png` (pixel diffs). Attach the
review-worthy subset to the PR as comments per repo policy.

## Honest finding surfaced by the demo (logged, not a gate failure)

The demo agent noted that `shux lens run` auto-starts a daemon when none is
running, and `shux session kill` reaps only the scratch session — NOT the
daemon that was auto-started to host it. It killed the daemon pid
explicitly to reach a zero-process end state. This matches designed
behavior (the daemon is a shared long-lived host, not a per-run child),
but it is a real agent-workflow footgun for "leave no processes behind"
cleanup expectations; a future polish item could be a `--no-daemon-spawn`
guard or a documented `shux daemon stop` teardown recipe in the skill.

## Verdict chain closed by this result

PRD §15 P6 gate: K1 ✓, E1 ✓, T1–T4 ✓ (37/0 + 4/4 at ratified HEAD),
12 goldens RATIFIED (18f4b5d), shux-tui-qa substantive checks passed +
independent verifier VERIFIED, dootsabha convergence CLOSED (codex ×2 +
claude ×1), **T5 demo PASS (this report)**. The m4 requirement — "fresh
agent + skill only completes the E1 loop" — is proven.
