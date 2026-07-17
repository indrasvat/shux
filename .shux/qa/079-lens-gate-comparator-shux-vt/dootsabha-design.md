# Task 079 — DootSabha design review (raw: dootsabha-raw/design-v1.json, design-v2.json)

**Round 1 — VERDICT: REVISE** (codex thorough; agy timed out). Findings folded:
- BLOCKER: geometry mismatch must be first-class (no silent min-crop) → `FrameDiff.geometry_changed`.
- BLOCKER: `palette_changed` overclaims (sticky history bit) → renamed `palette_overridden_differs`, a diagnostic; 080 does the per-frame `overridden && has_indexed` check.
- MAJOR: parity oracle from the OLD live fn, not `to_cells`; expanded cases.
- MAJOR: `FrameEnvelope::try_view()` validates before decode.
- MAJOR: divergence table corrected — blink is a cell-tier signal but shux's static raster does not render it; font/emoji pixel divergence is 080's.
- MINORs: `GridFrame`/`FrameView` wrappers confirmed; daemon palette=false confirmed; "value-exact to `Cell: Eq`".

**Round 2 (v2) — VERDICT: CONVERGED** (codex + agy). Two spec pins baked in:
1. `FrameDiff.rows/cols` under `geometry_changed` = overlap/min dims (not original sizes).
2. `palette_unportable` (080) is a per-frame OR, NOT keyed on `palette_overridden_differs`.
