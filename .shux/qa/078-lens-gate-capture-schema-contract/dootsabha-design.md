# DootSabha design review — task 078 (capture schema + frozen contract)

**Status: CONVERGED.** Design review (codex + agy) plus a tie-break council on the two split
blockers. Records live under `.local/`:

- `.local/078-design-codex.md`, `.local/078-design-agy.md` — the two design reviews.
- `.local/078-tiebreak-codex.md`, `.local/078-tiebreak-agy.md` — tie-break on Q2/Q6.
- `.local/dootsabha-078-design-review.json`, `.local/dootsabha-078-tiebreak.json` — raw council JSON.
- `.local/078-q6-decisive-finding.md` — the continuation-style grounding finding.

## Rulings → frozen decisions

Both reviewers produced rulings Q1–Q8 (with BLOCKER/MAJOR/MINOR severities) that map 1:1 to the
frozen R1–R10 recorded in `docs/tasks/078-lens-gate-capture-schema-contract.md`:

| Council question | Ruling | Frozen as |
|---|---|---|
| Q1 OSC 4 palette | Sticky `palette_overridden` flag (option b); no `content_revision` bump | R1 |
| Q2 run content | Hybrid: string iff all-simple, else per-column array (tie-break, unanimous) | R2 |
| Q2/Q6 wide continuation | Explicit `""` entry, escapes run style, decodes to `Cell::wide_continuation()` | R3 |
| Q3 cursor | Drop `blinking`; no duplicate `color` (OSC 12 lives in `defaults.cursor`) | R4 |
| Q4 unicode-width | Record resolved version in 080 fingerprint; don't hard-pin | R5 |
| Q5 style flags | Named booleans, skip-if-false | R6 |
| Q8 extended attrs | `hyperlink`, `underline_color`, `underline_style` in run style | R7 |
| Q6 mask | Structural sentinel `[col, null, {"mask":true,"cells":n}]`, no glyph text | R8 |
| Q7 schema evolution | `schema:1` + `deny_unknown_fields`, typed unsupported-schema path | R9 |
| CellRef ownership | By-value/owned at the view boundary | R10 |

The `synthesis` auto-field in the design-review JSON is `null`, but the individual agent reviews
and the tie-break council carry the substantive, decision-shaping content — verified by reading
`.local/078-design-{codex,agy}.md` and `.local/078-tiebreak-{codex,agy}.md`.
