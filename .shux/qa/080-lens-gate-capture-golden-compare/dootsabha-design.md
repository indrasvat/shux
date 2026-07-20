# Task 080 DootSabha design review — captured reviews (synthesis round timed out)

The council chair's final synthesis round hit the per-agent deadline, but all three
reviewers produced complete independent reviews (captured verbatim from the review
guard's process listing). They CONVERGE on the split design with refinements. No
BLOCKER. Verdict read as: **PROCEED with the split, fold the refinements below.**

## codex (gpt-5.5) — 7 findings + fingerprint changes
1. Keep the SPLIT; do NOT consolidate into shux-raster. shux-vt already owns canonical
   frame semantics, masks, CellGridView, diff_frames, gate vocabulary. shux-raster =
   pure raster: render sanitized frames, exact PNG byte compare, RGBA tolerance
   compare, ordered font-chain fingerprint. Binary composes fs layout + scenario/env
   hashing + CLI. (A future `shux-lens` crate would beat stuffing non-raster state into
   shux-raster.)
2. Self-rendered tempdir PNG baselines are NOT golden proof — only determinism/plumbing
   tests (same input→same bytes, seeded mutation fails, missing→missing_golden). Same
   impl minted expected+actual in one pass (CLAUDE.md:197 rule).
3. **Pixel/exact MUST be CONJUNCTIVE with cell compare.** blink/alt_screen/cursor/
   default/palette metadata can produce identical PNGs while cells differ. pixel/exact
   ≡ valid fingerprint + masked cell compare passes + PNG condition passes. A PNG pass
   must NEVER override a semantic cell fail.
4. Stale vs missing STRICT: missing tier/platform/render key → missing_golden; existing
   golden/sidecar with mismatched schema/tier/tol/scenario-env-hash/render-font-key/
   sidecar-hash → stale_golden; current capture DIFFERING from a valid golden → fail
   (not stale). No exact→pixel→cell downshift.
5. Exit-code ownership: gate.rs already freezes GateStatus::exit_code (StaleGolden→1).
   080 asserts STATUSES, not CLI process exits — word it that way.
6. Font-chain variation faithful only if deterministic; host-local `.local/fonts` is
   weak CI proof. Prefer bundled-only resolution tests (char→font_index / pixel_count
   assertions) + separately mutate raster_font_fingerprint in the sidecar for stale.
7. Mask-before-hash must cover EVERY emitted artifact incl. the heat PNG: render from a
   MASKED FrameEnvelope/sanitized view, never the raw Grid (raw-grid heat leaks
   secrets). Also `pane.glance` mask semantics: if masks supplied, mask text/png too or
   suppress them — else `cells` are safe while text/PNG leak.

Fingerprint changes: distinct `fingerprint_schema` vs frame `schema`; explicit tier +
platform_key/triple + compare_algo_version; renderer_key (raster compat ver,
fontdue/image versions, font size, cell metrics, ordered font asset SHA256s, render
opts); mask_hash + redaction_policy_version; `rgba_sha256` in addition to png_sha256
(png only for exact); scenario_semantic_hash/settle_policy_hash/viewport/env-allowlist;
make producer shux_version INFORMATIONAL (or capture_compat_version) — exact app version
as a stale trigger churns every release. Avoid hashing full env / abs paths / timestamps
/ dirty git SHAs / host-local font paths / raw secrets.

## agy (Gemini) — 4 hazards
1. Split vs consolidated (leaned consolidate, but codex/claude override — keep split).
2. Self-rendered tempdir baselines are self-referential → plumbing-only; force
   missing/stale failures in CI; deterministic bundled-font render profile.
3. Mask bleed-through: adjacent cells' AA edges bleed into the masked pixel region.
   (Mitigated here: both sides render from their MASKED envs, so bleed is symmetric and
   the mask-invariance test proves no false mismatch.)
4. Fingerprint volatility: png_sha256 fluctuates by encoder → track raw RGBA sha;
   shux_version forces golden rebuilds every commit → renderer_format_version.

## claude — 2 sharpened points
- Q5: glance's in-lock clone captures revision/cursor(+shape)/alt_screen/size/
  default_colors/clone_visible but NOT palette_overridden, and never returns
  default_colors in JSON. Emitting a correct FrameEnvelope from `--cells` REQUIRES
  adding `vt.palette_overridden()` to that exact critical section.
- Q1: the split is what keeps the tiers inside the frozen-contract test boundary
  (lens_gate_parity/divergence import `shux_vt::{diff_frames,FrameEnvelope,GateStatus}`
  and can't reach binary internals) — cell compare + fingerprint STRUCT must sit in
  shux-vt; pixel/exact in shux-raster; binary composes.

## FOLDED DECISIONS (D1–D8) — see task file Design Review Decisions.
