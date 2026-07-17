# Task 080: lens gate — capture emission + golden compare (3 tiers)

**Status:** Not Started
**Priority:** High
**Milestone:** M2
**Depends On:** 078, 079
**Quality Gate:** shux-vt-solid-qa
**Touches:** `crates/shux/src/main.rs` (`pane.glance` cells field), `crates/shux/src/cli.rs`, gate compare module (CLI-side), `.claude/automations/pixel_verify.py` (logic productized), `.shux/fixtures/lens-gate/`, benchmarks

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
   in the frozen set from 078, exit 1; the compare is refused, not silently trusted,
   until the golden is re-blessed). `stale_golden` is defined HERE and consumed by
   082's verdict/exit model.
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

## Testing Matrix

| Layer | Required Evidence |
|---|---|
| L1 capture | `pane.glance --cells` on a fixture grid produces the exact canonical `FrameEnvelope`; round-trips. |
| L1 tiers | `cell`/`pixel`/`exact` each pass on a matching golden and fail on a seeded mismatch; `missing_golden` on absent baseline. |
| L1 sidecar | Fingerprint written + validated; a font/shux-version bump yields `stale_golden` (exit 1), not a silent pass or a false `fail`. |
| L1 mask/redact absence | A masked timestamp region and a redacted token never appear in the serialized golden or the diff. |
| L1 mask/redact invariance (council #3) | A change *inside* a masked/redacted region does NOT alter `capture_sha256`, the pixel diff/heat regions, the report artifacts, or the `--update` changed-golden manifest — geometry and hashes are stable across masked content. |
| L2 perf | 10/100/1000-frame capture benchmarks recorded; max-artifact-size regression test passes; RPC payload within budget. |
| L3 dogfood | Compare a real colored shux pane (80x24 and 120x40) against freshly-blessed `cell` goldens; leaves no daemons. |
| L3 QA | `shux-vt-solid-qa` full-res PNG + `pixel_verify.py` metric JSON evidence for the `pixel` tier. |

## Acceptance Criteria

- [ ] `pane.glance --cells` emits the canonical captured frame.
- [ ] All three tolerance tiers behave per spec, with platform-partitioned pixel/exact baselines.
- [ ] Missing/stale goldens produce distinct, non-silent outcomes.
- [ ] Masks + redaction are applied before serialization/hash/compare.
- [ ] `cell` tier commits JSON only; PNGs are ephemeral unless a pixel/exact baseline is opted in.
- [ ] Performance benchmarks + artifact-size regression exist and pass the documented budget.

## Definition of Done

- [ ] DootSabha design review incorporated before coding.
- [ ] Red tests captured before implementation.
- [ ] L1/L2/L3 tests pass; benchmarks recorded.
- [ ] `make check` passes (+ any new `make bench-lens-gate`/`test-lens-gate` target).
- [ ] `shux-vt-solid-qa` `VERDICT: PASS`; evidence under `.shux/qa/080-*/` (manifest + full-res PNG + pixel JSON).
- [ ] Implementation-diff DootSabha convergence review clean or addressed.
- [ ] `docs/PROGRESS.md` + this task updated; learnings appended.
