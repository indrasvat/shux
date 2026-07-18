# Task 080: lens gate — capture emission + golden compare (3 tiers)

**Status:** Done
**Priority:** High
**Milestone:** M3
**Depends On:** 078, 079
**Quality Gate:** shux-vt-solid-qa
**Touches:** `crates/shux/src/main.rs` (`pane.glance` cells field), `crates/shux/src/cli.rs`, `crates/shux-vt/src/gate_compare.rs` (cell tier + `Fingerprint` — testable by the frozen contract oracles; D1), `crates/shux-raster/src/gate_pixel.rs` (pixel/exact + font fingerprint; D1), `.claude/automations/pixel_verify.py` (logic productized into `gate_pixel.rs`, no shelling), `.shux/fixtures/lens-gate/`, benchmarks

> `shux lens gate` initiative. Turns the schema (078) + comparator (079) into a
> real capture + frame-vs-golden compare with tolerance tiers and committable
> provenance.

## Problem

The comparator exists but nothing emits a `CapturedFrame` or compares it to a
committed golden. `pane.glance` exposes only text + PNG; golden compare exists only
as test-harness Python (`pixel_verify.py`), not in the shipped binary. And pixel-only
compare flakes on AA (shux's own docs warn).

## Scope

1. **Emit `CapturedFrame`**: add `cells` to `pane.glance` (RPC + `--cells`), producing
   the canonical `FrameEnvelope` from task 078 for the current viewport.
2. **Three tolerance tiers** (CLI-side compare module):
   - `cell` (default, portable) — `diff_frames` semantic equality; **JSON golden only**.
   - `pixel` — RGBA `{max_channel_delta, max_changed_frac}` (productize `pixel_verify.py`
     logic into the binary; no shelling); committed PNG baseline **partitioned by
     `<os>-<arch>`**.
   - `exact` — byte-identical PNG (single render key).
   Requesting `pixel`/`exact` with no matching-platform baseline is an explicit
   `missing_golden`, never a silent pass.
3. **Fingerprint sidecar** (council #1 MAJOR — no ephemeral revision): per golden
   `{schema, shux_version, raster_font_fingerprint, unicode_width_ver, scenario_hash,
   cmd_env_hash, capture_sha256, png_sha256?, tol, tol_params}`. A fingerprint
   mismatch yields the **`stale_golden`** status (council #3 — a first-class verdict
   in the frozen set from 078; the compare is refused, not silently trusted, until the
   golden is re-blessed). The **semantics** of `stale_golden` are defined HERE; its
   **exit-code mapping is owned by 082** (council #4 — 080 must not assert exits).
4. **Masks + redaction applied before serialize/hash/compare/diff** (council #2): the
   sentinel from 078 is written into the capture prior to any hashing or comparison,
   so masked geometry is stable and secrets never enter a golden.
5. **PNG bloat policy** (council #1 MAJOR): `cell` tier writes **no committed PNG**;
   PNGs are ephemeral failure artifacts in gitignored `.shux/out/<scn>/` (heat PNG via
   `render_lens_heat_png`). Committed PNG baselines are opt-in (`pixel`/`exact`) with
   git-LFS guidance.
6. **Performance**: benchmarks at 10 / 100 / 1000 captured frames; a max-artifact-size
   regression test; establish that base64-PNG-over-RPC + full cell JSON stays within a
   documented budget (make PNG capture selective / file-backed as needed).

## Non-Goals

- No scenario runner or CLI `gate` verb yet (task 081) — compare is exercised via a
  test/bench harness and a minimal `pane glance --cells` + compare helper.
- No verdict rollup / report.json (task 082).
- No `--update`/bless flow (task 082).

## Design Review Decisions

DootSabha design review MUST confirm: the platform-partition layout for
`pixel`/`exact`, the sidecar fingerprint set, the mask-before-hash ordering, and the
performance budget + selective-PNG policy.

**Incorporated (council: codex + agy + claude; synthesis round timed out but the three
independent reviews CONVERGED on the split with refinements — captured evidence
`.shux/qa/080-*/dootsabha-design.md`):**

- **D1 — the compare SPLITS across crates, not "CLI-side" as the Touches line
  understated.** Cell-tier compare + the `Fingerprint` struct + tier vocabulary live in
  `shux-vt` (new `gate_compare.rs`) because the frozen contract tests import
  `shux_vt::{diff_frames, FrameEnvelope, GateStatus}` and cannot reach binary internals.
  Pixel/exact PNG compare + envelope render + font fingerprint live in `shux-raster`
  (new `gate_pixel.rs`). The binary composes filesystem layout + env hashing + CLI. No
  new crate (a future `shux-lens` crate is the cleaner long-term home, deferred).
- **D2 — pixel/exact tiers are CONJUNCTIVE with the cell tier (codex #3, load-bearing).**
  `pixel`/`exact` ≡ valid fingerprint AND cell compare passes AND the PNG condition
  passes. A matching PNG must NEVER override a semantic cell fail. The cell tier
  (`compare_cell`) gates visible cells, cursor position/visibility, geometry, palette
  portability, AND the `alt_screen` flag (it is in the schema + `capture_sha256`, so the
  cell verdict must gate it too — impl-review; done via a `CellGridView::alt_screen`
  default so the frozen `diff_frames`/daemon path is byte-unchanged). The pixel/exact PNG
  check is ADDITIONAL — it catches what the cell tier is still blind to (cursor SHAPE,
  font-fallback pixels: same cells, different glyphs).
- **D3 — self-rendered tempdir baselines are PLUMBING proof only (codex #2 / agy #2 /
  CLAUDE.md:197).** They prove path resolution, `missing_golden`, seeded-mutation-fails,
  and determinism — NOT renderer correctness (same impl mints expected+actual in one
  pass). The pixel tier's real proof is the divergence set, which compares GENUINELY
  DIFFERENT inputs (font-chain-with vs -without emoji; block vs bar cursor; OSC-11 bg
  change) — never self-referential. No platform-specific PNG is committed into the CI
  test path (cross-platform CI-flake trap); durable dev-host pixel evidence lives under
  `.shux/qa/080-*/`.
- **D4 — mask-before-hash covers EVERY emitted artifact (codex #7 / agy #3).** The gate's
  pixel render and the ephemeral heat PNG render from the MASKED `FrameEnvelope`
  (`env.to_grid()`), never the raw live `Grid` (a raw-grid heat leaks the secret behind
  the mask). `pane.glance` with non-empty `masks` masks the returned `text` and PNG too
  (derived from the masked envelope), so `cells`/`text`/`png` are all safe; default
  (no masks) is byte-unchanged → frozen glance tests stay green. AA bleed from adjacent
  cells is symmetric (both sides render their masked envelope) and pinned by the
  mask-invariance test.
- **D5 — fingerprint: `shux_version` is INFORMATIONAL, not a stale trigger (codex fp /
  agy #4).** Keying stale on the exact app version would churn every golden each release.
  Stale triggers are the fields that actually change output: `schema` (capture format),
  `fp_schema` (sidecar format), `renderer_format_version`, `raster_font_fingerprint`
  (ordered bundled font-asset SHA + size), `unicode_width_ver` (Unicode table version,
  = `UNICODE_VERSION`), `tol`/`tol_params`, `mask_hash`, and the content pin. Content pin
  uses `capture_sha256` (canonical JSON) for cell and `rgba_sha256` (raw uncompressed
  RGBA — encoder-stable) for pixel; `png_sha256` is used ONLY by the exact tier.
  `scenario_hash`/`cmd_env_hash` are placeholders 081/082 populate (round-tripped now so
  no schema bump later).
- **D6 — strict stale vs missing vs fail (codex #4).** Absent tier/platform golden →
  `missing_golden`; present golden whose sidecar mismatches a stale-trigger field →
  `stale_golden` (compare REFUSED); a valid golden that the live capture differs from →
  `fail`. No exact→pixel→cell downshift. `080 asserts STATUSES only` — the exit-code map
  (already frozen in `gate.rs`, `StaleGolden → 1`) is 082's; 080 never asserts a process
  exit (codex #5).
- **D7 — font-chain divergence proof is bundled-only + deterministic (codex #6).** The
  glyph-fallback pixel divergence renders `❤️X` through the FULL bundled chain vs a chain
  WITHOUT the emoji fallback (both from bundled assets — no host-local `.local/fonts`),
  proving same cells → different pixels while `diff_frames == 0`. Belt-and-suspenders:
  a font-resolution assertion (emoji resolves to a fallback via `glyph_pixel_count`) +
  a `raster_font_fingerprint` mutation → `stale_golden`.
- **D8 — `palette_unportable` is a per-frame escalation (079 D2, confirmed).** A cell
  golden that is `(overridden && has_indexed)` on either side cannot certify a portable
  match → the cell verdict is `fail` with `reason: "palette_unportable"` even when
  `cells_changed == 0` (else a silent false pass; the `palette-with-indexed` divergence
  fixture). `palette-no-indexed` (overridden, no indexed cells) is portable → passes.

## Testing Matrix

| Layer | Required Evidence |
|---|---|
| L1 capture | `pane.glance --cells` on a fixture grid produces the exact canonical `FrameEnvelope`; round-trips. |
| L1 tiers | `cell`/`pixel`/`exact` each pass on a matching golden and fail on a seeded mismatch; `missing_golden` on absent baseline. |
| L1 sidecar | Fingerprint written + validated; a font/shux-version bump yields the `stale_golden` **verdict/status**, not a silent pass or a false `fail`. (CLI exit mapping is asserted in 082.) |
| L1 mask/redact absence | A masked timestamp region and a redacted token never appear in the serialized golden or the diff. |
| L1 mask/redact invariance (council #3, scoped to 080-owned artifacts) | A change *inside* a masked/redacted region does NOT alter `capture_sha256`, the compare outcome, or the pixel diff/heat regions — geometry and hashes are stable across masked content. |
| L1 mask invariance — downstream (RED, retired by 082) | Frozen red cases asserting mask invariance of the **report artifacts** and the `--update` changed-golden manifest. 082 owns both; these stay red in the quarantined lane until 082 turns them green. |
| L2 perf | 10/100/1000-frame capture benchmarks recorded; max-artifact-size regression test passes; RPC payload within budget. |
| L3 dogfood | Compare a real colored shux pane (80x24 and 120x40) against freshly-blessed `cell` goldens; leaves no daemons. |
| L3 QA | `shux-vt-solid-qa` full-res PNG + `pixel_verify.py` metric JSON evidence for the `pixel` tier. |

## Acceptance Criteria

- [x] `pane.glance --cells` emits the canonical captured frame.
- [x] All three tolerance tiers behave per spec, with platform-partitioned pixel/exact baselines.
- [x] Missing/stale goldens produce distinct, non-silent outcomes.
- [x] Masks + redaction are applied before serialization/hash/compare.
- [x] `cell` tier commits JSON only; PNGs are ephemeral unless a pixel/exact baseline is opted in.
- [x] Performance benchmarks + artifact-size regression exist and pass the documented budget.

## Definition of Done

- [x] DootSabha design review incorporated before coding (codex+agy+claude; D1–D8 folded).
- [x] Red tests captured before implementation (TDD; adversarial pass surfaced a BLOCKER + 3 MAJOR + 5 MINOR, each fixed with a pinning test; impl-review found 2 BLOCKER + 2 MAJOR, all fixed).
- [x] L1/L2/L3 tests pass; benchmarks recorded (`make bench-lens-gate`).
- [x] `make check` passes (new `make bench-lens-gate` / `test-lens-gate-compare` / `test-lens-gate-glance-cells` targets); clippy + fmt clean.
- [x] `shux-vt-solid-qa` `VERDICT: PASS`; evidence under `.shux/qa/080-lens-gate-capture-golden-compare/` (manifest + full-res PNG + pixel JSON, 0/0 thresholds; 0 daemon leaks).
- [x] Implementation-diff DootSabha convergence review clean (round-2 codex 👍 converged, no findings).
- [x] `docs/PROGRESS.md` + this task updated; learnings appended.
