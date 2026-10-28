# Council record: status-bar zone overflow (issue #202)

`dootsabha` is not installed in the cloud session that built this change, so per CLAUDE.md
*Tooling fallbacks* the council steps were replaced by parallel cold-context agents on disjoint
surfaces. The owner's standing rule for this run allows one review pass before the push; these
agents were that pass.

## Design (`dootsabha_design`)

Design taken from the issue itself: the zones get a priority when they do not fit, and the centre
is truncated or dropped rather than overlapping. Priority chosen: left, then right, then centre.
Left and right may touch, as they could before; the centre keeps the one-column gap either side
that `append_waiting` (crates/shux/src/statusbar_build.rs) budgets for.

## Implementation (`dootsabha_implementation`)

| Reviewer | Surface | Findings | Disposition |
|---|---|---|---|
| `shux-simplify-architect` (cold, read-only) | the diff at 6596550's parent tree | no P1; P2 left/right zones that only touched lost text to a new mandatory gap; P3 a zone one column short lost a glyph to its trailing space; tidiness: subsumed cross-path test, retarget wide-glyph unit test, doc line said "as it always was" | all fixed in 6596550: left/right may touch, trailing padding goes first, old cross-path test removed, unit test now hits a cut, doc line reworded |
| `shux-tui-qa` (cold, drove the real binary, live attach in an outer shux pane, right-zone script segment, unicode and nerd fonts, 16 to 120 cols) | 6596550 | fix confirmed against base (base overlaps from 80 cols down with a script segment); FAIL only on missing VT evidence; P3 a centre could shrink to ` …`; nit: a right zone that only lost padding hugged the left zone | ` …` and the padding nit fixed in 3d83b99 with tests seen red; the width sweep then caught a dropped zone leaving a painted space, fixed in the same commit; VT evidence is this directory |
| `shux-vt-solid-qa` (cold, randomised probe of 7988 bars, live attach vs window and session snapshots, vim) | 3d83b99, then 1db5659 | FAIL at 3d83b99: two test names vanished (`check-test-inventory`); P3 an ideographic space could stand alone before the `…`; declined cosmetic: a zero-width space in trailing padding keeps one cell | fixed in 1db5659; PASS at 1db5659, report in `SOLID-QA.md` |
