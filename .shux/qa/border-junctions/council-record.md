# Council record: border junction glyphs (#203)

`dootsabha` is not installed in the cloud environment this change was built in. Per CLAUDE.md
*Tooling fallbacks*, the council steps were replaced by parallel agents on disjoint surfaces that drove
the real system. Pointers are to commits on `claude/project-thread-hv5sei`.

## Design (`dootsabha_design`)

The design was taken from the issue itself: the junction glyph should depend on which of the four
directions actually have a border line. `408c063` implements that directly (per-cell up/down/left/right
arms, lines clipped to the viewport, glyph read off the arm set). The design was not put to a separate
council before coding; the implementation review below covered it.

## Implementation diff (`dootsabha_implementation`)

Three agents reviewed `408c063` in parallel, before any push, on disjoint surfaces:

- **Adversarial A/B.** A verbatim copy of the old `compute_borders` against the new one over 20,000
  random layouts (depth up to 5, ratios 0.1 to 0.9, 3x3 to 152x52, offset viewports, all five styles,
  inset and non-inset pane rects). Segment positions and `focused` flags were identical in every case;
  in layouts without zero-size panes every glyph change was at a cell the old code drew as a cross.
  Mutations (never cross, swapped tees, armed endpoints, corner drawn as tee) were each caught by the
  new tests. Finding: focus on horizontal pane edges was untested. Fixed in `d13db01`, seen red with the
  focus flag dropped from horizontal edges.
- **Simplification review** (`shux-simplify-architect`). No correctness finding over 3,000 tiny random
  layouts. Proposed one clipped `line` helper in place of the vertical closure and queued horizontal
  pass, and a ring-coverage check in the random sweep. Both adopted in `d13db01`; the ring check was
  seen red with right-hand pane edges not drawn.
- **`shux-tui-qa` gate** on `d13db01`: `VERDICT: PASS`. Live attach and `window snapshot`, fix vs base,
  five styles, three sizes, aligned and tee layouts, config states, rich TUIs (vim, top, less).

One review round, as the change's owner asked; no finding required a second.

The `shux-vt-solid-qa` gate (`SOLID-QA.md` in this folder) then audited `d13db01` independently,
including a re-run of the 20,000-layout old-vs-new comparison on the refactored code.
