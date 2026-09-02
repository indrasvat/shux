# Council substitution — `dootsabha` unavailable

`dootsabha` is not installed on this host (`command -v dootsabha` → nothing), so
steps 1 and 7 of the feature protocol ran through the documented fallback:
context-appropriate parallel adversarial agents on disjoint surfaces, each
required to cite primary sources and to reproduce before reporting.

## Step 1 — design, before any code

Three agents, disjoint surfaces:

1. **The kitty protocol as an emitting client.** Read kitty @8df06b30
   (`graphics.c`, `screen.c`, `vt-parser.c`, `parse-graphics-command.h`,
   `kittens/icat/detect.go`) plus the protocol docs, and ghostty
   (`graphics_exec.zig`, `graphics_storage.zig`), wezterm (`kitty.rs`,
   `image.rs`), konsole (`Vt102Emulation.cpp`, `Screen.cpp`) and xterm's
   ctlseqs for portability.
2. **Prior art.** zellij @af38660, full clone, plus tmux HEAD for contrast.
3. **The shux seam.** `compositor.rs`, `composed.rs`, `attach.rs` at ec62be0.

They converged, and overruled the archived spike on two points: placement ids
rather than double-buffered image ids for flicker, and a source rectangle
rather than a cropped bitmap for the pane clip.

## Step 7 — the implementation diff, before the first push

Two agents on disjoint surfaces:

- **Shell and build machinery** — found the `image-pane` arm's synchronisation
  was `wait-settled` on a pane whose last mutation was tens of seconds old, so
  it returned settled before the payload was read; that `require_image` was
  satisfied by a single pixel; that `image-pane` was asserted by nothing; and a
  `nix` dependency left behind by a deleted feature.
- **Rust** — cleared `placed_bytes` drift, `Assembler::opening` and the
  emitter's index pairing by probing each, and found that every fixture in the
  emit suite was an exact 2x2 cells, so nothing exercised an image whose size
  is not a whole number of cells.

## What the substitution did not give

A council converges on a shared judgement; parallel agents produce independent
reports that someone has to reconcile. Where the two disagreed — whether to
drop `c=`/`r=` for cross-path consistency — the reconciliation was mine, and is
recorded in the commit that made the call.

## Step 7, second pass — the capability commits

`2546cbc` and `89013c2` changed how shux decides whether a terminal can be
drawn on, and shipped without a council record. Step 7 is explicitly not scaled
down for small diffs, and `89013c2` is five lines that shipped a P1: the
`SHUX_GRAPHICS` escape hatch matched exact lowercase bytes, so `OFF` fell
through and produced the corruption the variable exists to prevent.

The review that caught it was the QA gate, not a council, and the defect was
one this repository had already paid for and written down — `shux gate`'s
`is_ci` mis-read `CI=True` as "not CI" and let `--update` bless a regression.
A reviewer holding the repo's own history would have matched the shape on
sight. Recorded here because "the gate caught it" is not the same as "the step
ran".
