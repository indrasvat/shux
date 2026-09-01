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

## Step 7 — council on the implementation diff (SUBSTITUTED, evidence present)

Re-audited at `76fc6e8`. The step is now evidenced and the `3d46d74` finding
P2-1 is closed.

Two adversarial reviews of the implementation diff completed after `3d46d74`, on
disjoint surfaces, and both landed measured findings that `76fc6e8` applies:

- **the production wiring surface** — `snapshot.rs`'s single
  `composite_composed` call is what makes `window.snapshot` and
  `session.snapshot` draw anything, and no test reached it; deleting the line
  left the whole `shux` crate green at 896 tests. This is the same defect the
  `3d46d74` gate raised as P1-1, found independently. Applied as
  `crates/shux/tests/window_snapshot_image_rpc.rs`, a daemon-backed black-box
  test on the real RPCs. **Re-verified by this gate**: the mutation is now caught
  with `window.snapshot returned a frame with no picture (0 px); pane.snapshot
  has 10260`, while the three in-process tests stay green.
- **the decode-cost surface** — `blit` decodes and rescales a whole bitmap
  before the clip narrows it, and the 64 MiB ceiling is per IMAGE while a
  composed frame gathers every placement of every pane. Measured end to end on
  the real binary: four panes each printing six 4096×4096 PNGs took 3.344 /
  3.310 / 3.356 s per `window snapshot`, against 0.667 / 0.645 / 0.637 s with a
  per-render budget. The commit records two earlier attempts at that measurement
  that measured nothing, and why — an honest negative result, which is what a
  real review round looks like.

Applied as `MAX_RENDER_DECODE_BYTES`.

**This gate's judgement on the second finding is that the review was right about
the problem and the fix is wrong about its scope.** The budget is reset per
render, so the composed path spends one 256 MiB across every pane while the
single-pane path gives each pane its own — which lets one pane delete a
neighbour's picture from `window.snapshot` while `pane.snapshot` still draws it.
Reproduced, dose-responded and A/B'd against `3d46d74` in `SOLID-QA.md` §6 as
P1-1. Neither review caught that, and no test can: mutation M16b shows the
budget's accumulation branch is reached by zero tests (P2-1).

So: the step is done, and its output is recorded here rather than asserted. What
it did not catch is a finding of this audit, not a gap in the substitution.

## Step 7 note on the substitution itself

`dootsabha` remains uninstalled (`command -v dootsabha` → not found). Per
CLAUDE.md *Tooling fallbacks*, parallel adversarial agents on disjoint surfaces
serve the step, and the substitution is named in the PR rather than skipped. The
two reviews above are that substitution; their findings were reproduced by this
gate before being believed, per *Reproduce before believing — including your own
findings*.

## Addendum — delta after the PASS at `76fc6e8`

The gate's PASS names `76fc6e8`. Two of its three P2 findings were then closed,
and both are visible in this diff:

* **P2-1** — `MAX_PANE_DECODE_BYTES`'s doc led with a stall figure the per-pane
  scoping does not deliver. It now states what the gate measured (2950 ms
  unmitigated / 629 ms frame-wide / 2325 ms per-pane) and why the weaker brake
  is still the right one.
* **P2-2** — mutation M19, a budget that charges but never refuses, survived 926
  tests: the starvation test asserts a picture SURVIVES, which an inert budget
  satisfies. `the_decode_budget_refuses_past_its_ceiling_identically_on_both_paths`
  uses the gate's own §4.1 scene — five 4096x4096 placements in distinct
  colours, ceiling 256 MiB, so the 4th is drawn and the 5th refused, on both
  render paths. Verified failing against M19 and against M17.

No production behaviour changed: one comment and one test. The gate's twelve
mutations and twenty-five metrics are unaffected.
