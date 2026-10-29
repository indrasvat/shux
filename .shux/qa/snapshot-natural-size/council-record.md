# Council record — snapshot-natural-size

`dootsabha` is not installed in this environment, so neither the design council
nor the implementation-diff council ran. Per CLAUDE.md *Tooling fallbacks*, the
substitute is parallel adversarial review on disjoint surfaces that drove the
real system. Each finding was reproduced before it was fixed, and each fix
carries a test seen red first.

| Reviewer | Surface | SHAs reviewed | Outcome |
|---|---|---|---|
| `shux-vt-solid-qa` gate (this record's author) | Snapshot frame geometry, pane placement, dims parsing, pixel cap, raster glyph clip, rich-TUI raster A/B | Round 1: 40878bf (at HEAD 1c17968). Round 2: 21de4cf. Round 3: 84307b6, then every layout/snapshot cell and the raster A/B again on 373dd97. Round 4: cef1182 (region clip) at 6f1ddab | Round 1 FAIL (F1-F5). Round 2 FAIL (F6, F7). Round 3 PASS at 373dd97. Round 4 PASS at 6f1ddab. See SOLID-QA.md |
| `shux-tui-qa` gate | Attach UI, CLI flows, rich-TUI compatibility | Run in parallel by the implementing session; its report lives with its own scope | Not audited here: the implementing session's claim, recorded as such |
| `shux-simplify-architect` (advisory) | Diff simplification, cold context | First pass 1c17968. Second pass 84307b6; its findings were applied in 49a3bdf and 373dd97 | Not audited here: the implementing session's claim, recorded as such. 373dd97's snapshot and layout changes were re-verified by this gate |

Findings from this gate, with the commit that fixed each:

- F1: clipping and dead bands when grids disagree with split ratios. Fixed in 0edab07 (`LayoutNode::fitted`).
- F3: 1000-col clamp on the defaulted frame. Fixed in 0edab07.
- F4: non-integer RPC dims silently ignored. Fixed in 0edab07.
- F5: no hint in the pixel-cap error. Fixed in 0edab07.
- F2: cross-axis slack in mixed nested splits. Re-scoped as design (see SOLID-QA.md); verified that the slack is inside the pane's rect and that nothing is cropped.
- F6: a one-axis-explicit snapshot cropped the defaulted axis. Fixed in 9ba0b65.
- F7: status-bar glyph ink bled into the pane row above. Fixed in 84307b6 (raster row clip).
- F7 re-fix: 84307b6's per-row clip broke box-drawing joins in 5 VT corpus goldens. cef1182 replaced it with a per-region clip (`Rasterizer::render_regions`), verified in round 4.

cef1182, 1838d71 and 6f1ddab were reviewed by a `shux-simplify-architect` pass over 729b185..6f1ddab, which ran in parallel with this gate's round 4. It found no P1. Its P2, that nothing pinned box-drawing joins inside a pane in a window snapshot, was fixed with a test in 958981f, red by 8 px under a per-row clip. No separate adversarial-agent round was run on cef1182; the PR description says so.
