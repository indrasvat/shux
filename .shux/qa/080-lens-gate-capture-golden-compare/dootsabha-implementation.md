# Task 080 DootSabha implementation-diff review — verdict + resolution

Council (codex chair-synthesis; agy timed out on dispatch). Verdict: **not ready to ship**
— found real gaps the 4-agent adversarial pass missed (the daemon render path + the
orchestrator's pin enforcement), which is exactly why the impl-review step exists. All
findings fixed + regression-tested; the whole suite re-verified green with zero frozen
regression.

## Findings (codex) + resolution
1. **BLOCKER — masked `pane.glance` still leaks cursor-derived secret LENGTH via the PNG
   render + the JSON `cursor.col` response.** The clamp lived only in
   `FrameEnvelope::from_snapshot` (the `cells` field); the daemon's own PNG render
   (`main.rs`) and the top-level cursor response used the RAW `cursor_col`.
   → FIXED: `main.rs` computes `present_cursor_col = masks.cursor_redaction_col(...)` once
   and uses it for BOTH the rendered cursor and the response `cursor.col`; the checkpoint
   keeps the raw cursor (internal state). Daemon regression: mask a window straddling the
   live cursor → the reported col snaps to the mask origin.
2. **BLOCKER — pixel/exact sidecar content pins written but never ENFORCED.** `bless_pixel`
   recorded `rgba_sha256`/`png_sha256`, but `gate_status` only checked `capture_sha256`
   (the cell JSON) before accepting whatever PNG file existed — a swapped/valid baseline
   passed.
   → FIXED: `gate_status` verifies the on-disk baseline's pin before `evaluate_tier`
   (`png_sha256` for exact, decoded `rgba_sha256` for pixel; undecodable ⇒ refused).
   Mismatch ⇒ `stale_golden`. Regression:
   `pixel_and_exact_baselines_are_pin_enforced_against_tamper` swaps in a DIFFERENT valid
   PNG (cell JSON + sidecar untouched) and asserts `StaleGolden` for both tiers.
3. **MAJOR — RPC `parse_glance_masks` silently accepted `width == 0`** (CLI rejects it) →
   `MaskSet::with` drops it → an intended redaction becomes an unmasked glance.
   → FIXED: `parse_glance_masks` rejects `width == 0` with `INVALID_PARAMS` (matching the
   CLI). Daemon regression asserts `-32602`.
4. **MAJOR — `alt_screen` contract drift:** the schema + `capture_sha256` record it, but
   `compare_cell` didn't gate it, so a live frame differing only by `alt_screen` passed.
   → FIXED (the reviewers' preferred option): a `CellGridView::alt_screen()` DEFAULT
   method (returns `false`) so the frozen `diff_frames`/daemon `pane.diff_since` path is
   byte-unchanged; `FrameView` overrides it; `compare_cell` gates it (`Fail` reason
   `alt_screen_changed`). D2 + the module doc updated to name alt_screen as a cell signal.
   Regression `compare_cell_gates_alt_screen_flip` also pins that `diff_frames` stays
   alt-blind (frozen).

"Already-fixed adversarial items look directionally correct" (underline indexed colour,
tolerance comparison, corrupt exact baseline) — codex confirmed. `git diff --check` clean.

## Re-verification after fixes
- shux-vt 342, shux-raster gate_pixel 13, lens_gate_compare/divergence/parity 20, daemon
  lens_gate_glance_cells 2 — all green.
- `make test` 245 passed. Frozen daemon suites green: lens_diff 6 (incl
  `a1_altscreen_semantics`), lens_gate_capture 5, lens_glance 3 — zero regression, 0 leaks.
- clippy clean (bar the pre-existing MSRV config note).

## Convergence re-review (round 2, after the fixes)
codex: **"None. 👍 Converged. No BLOCKER/MAJOR/MINOR in the four fixes."** Independently
verified: cursor redaction covers cells+response+PNG (checkpoint raw; clamp-to-origin is
the right invariant); pixel/exact pins fail closed before tier eval (stale on
mismatch/undecodable; double-decode acceptable); RPC mask parsing rejects zero-width +
invalid numeric shapes; `alt_screen` default trait method is object-safe with FrameView
override + GridFrame default-false correct for the frozen daemon path. codex ran
`make test-lens-gate-compare` (18 passed) + `make test-lens-gate-glance-cells` (2 passed);
`git diff --check` clean. Chair synthesis: "ready to ship; no findings remain."
