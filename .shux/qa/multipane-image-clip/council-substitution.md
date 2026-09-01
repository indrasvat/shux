# Council substitution — multipane-image-clip

`dootsabha` is not installed on this machine (`command -v dootsabha` → not found),
so CLAUDE.md's *Tooling fallbacks* row applies: the council steps are served by
parallel adversarial agents on disjoint surfaces, and the substitution is named
here rather than silently skipped.

## Step 1 — council on the design (SUBSTITUTED, evidence present)

Commit `56568e4` records two adversarial design reviews run before the code, on
disjoint surfaces, each with a measured result rather than an opinion:

- `abs_row` inflated by the source grid's scrollback, and the two snapshot
  callers differ (`window.snapshot` clones the whole grid, `pane.snapshot` uses
  `clone_visible`) — naive `abs_row + rect.y` wrong in 1170 of 2340 measured
  cases;
- `compose_pane`'s cursor-following `pane_view_row_offset` dropped — wrong in
  672 cases, and the placement moved as the cursor moved;
- a picture taller than its pane scrolls its anchor above the viewport
  (`viewport_row == -2` measured with zero history), which a `u64` anchor can
  only clamp;
- `composite_placements` clipping to the CANVAS — 162567 escaped pixels at real
  `icat` geometry, 138510 of them erasing the pane next door.

Audit note: those four are design findings with numbers attached, and three of
them are independently re-derivable from the mutation matrix in this audit
(M2/M3 reproduce the offset and clamp findings, M1/M5/M6/M11/M12 reproduce the
canvas-clip finding). The design step is satisfied.

## Step 6 — simplification review (advisory, evidence present)

Commit `4da4e13` acts on a `shux-simplify-architect` pass; commit `3d46d74` is a
second pruning pass. Advisory, no verdict owed.

## Step 7 — council on the implementation diff (NOT COMPLETE AT AUDIT TIME)

At the audited HEAD (`3d46d74`) there is no implementation-diff council record
in the repository, and none was available to this audit. The parent agent
reported two adversarial agents running in their own worktrees under
`.claude/worktrees/` while this audit ran; their output did not exist when the
audit closed and was therefore not read, not reproduced, and not judged.

This file does not attest to a completed implementation-diff review. It records
that the step was in flight. See finding P2-1 in `SOLID-QA.md`.
