# 091 — DootSabha design council: N/A in this environment, substituted

**Status:** `N/A-substituted`

`dootsabha` is not available on this host, so feature-protocol step 1 (a council on
the design, before coding) could not be run as written. The operator directed that
design and implementation review be carried instead by parallel agents that drive the
shipped binary rather than reason from source.

**What was substituted, and what it produced.** Four adversarial reviewers on disjoint
surfaces — rich-TUI compatibility, grapheme/wide-cell invariants, resource bounds, and
an A/B regression sweep against the pre-fix commit `e856793` — plus a Codex review round
on PR #129. Between them they changed the design three times:

| Finding | Design consequence |
|---|---|
| a stray combining mark redefined what REP repeats | the record is extended only when the scalar joined the cell the record already describes (xterm's rule) |
| 10–24% throughput regression on cluster-heavy text | the record's scalar buffer is reused, not reallocated per scalar |
| a regional-indicator pair formed across a cursor move (`8f5ebf8`) | `try_append_regional_indicator_pair` located its target with `active_grapheme_position()` instead of `preceding_cell_position()` |

The design claim this review round settled — that REP's source is the data stream and
must be replayed through the ordinary print path — was independently cross-checked
against Alacritty `1b2b36a6`, which has sourced REP from `ProcessorState::preceding_char`
and replayed it through `handler.input()` since 2017. See the "Reference cross-check"
section of `docs/tasks/092-rep-data-stream-source.md`.

**This is a substitution, not a waiver.** A reader who requires genuine council output
for this task should treat this row of the evidence matrix as unmet.
