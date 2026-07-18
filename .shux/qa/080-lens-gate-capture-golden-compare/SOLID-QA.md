VERDICT: PASS

# SOLID VT QA — Task 080: lens gate — capture emission + golden compare (3 tiers)

- **Task file:** `docs/tasks/080-lens-gate-capture-golden-compare.md`
- **Branch:** `feat/lens-ci-gate` (working tree uncommitted at audit time)
- **Audited HEAD:** `2adde0c80d47280a091236796528ff9dbc2116da`
- **Audit date:** 2026-07-17
- **Gate:** `shux-vt-solid-qa` (audit-only; no source mutated, no git state mutated)

## 1. Verdict

`VERDICT: PASS`. Every required Testing-Matrix row, Acceptance Criterion, and
Definition-of-Done item that this gate owns is satisfied with fresh,
independently-regenerated evidence. All test layers were re-run by this audit
(not reused from the implementer's claims). The pixel hard gate was produced by
this audit: a real colored shux pane rendered through the actual
`pane.glance --png` gate path, glanced twice → byte-identical PNGs → 0 changed
pixels via `pixel_verify.py`. No daemon or orphan-process leaks.

## 2. Task DoD Matrix

| DoD item | Status | Evidence |
|---|---|---|
| DootSabha design review incorporated before coding | PASS | `dootsabha-design.md` (codex+agy+claude converged on D1–D8 split); folded into task §"Design Review Decisions" |
| Red tests captured before implementation | PASS | Frozen RED contract lane (`test-lens-gate-contract`, task 078) + divergence fixtures under `.shux/fixtures/lens-gate/`; impl-review confirms red-before-green |
| L1/L2/L3 tests pass; benchmarks recorded | PASS | §4 below — all suites re-run green; bench numbers §4 L2 |
| `make check` passes (+ new `bench-lens-gate`/`test-lens-gate-*` targets) | PASS | `make lint` clean (clippy ✓, fmt ✓); targeted test suites all green; Makefile targets present |
| `shux-vt-solid-qa VERDICT: PASS`; evidence under `.shux/qa/080-*/` | PASS | this file + `evidence-manifest.json` + full-res PNG + pixel JSON |
| Implementation-diff DootSabha convergence review clean/addressed | PASS | `dootsabha-implementation.md` — 2 BLOCKERs + 2 MAJORs found and fixed + regression-tested |
| `docs/PROGRESS.md` + task updated; learnings appended | EXPECTED-PENDING | Task 080 still `In Progress` — correct: the implementer flips to Done *after* this gate PASSes. Not a QA failure; `make check-progress` will pass once flipped. |

## 3. Testing Matrix (task §Testing Matrix)

| Layer | Required evidence | Status | This-audit evidence |
|---|---|---|---|
| L1 capture | `pane.glance --cells` produces exact canonical `FrameEnvelope`; round-trips | PASS | daemon suite `glance_cells_emits_canonical_envelope_rpc_and_cli` (80×24 + 120×40); `capture::tests::arbitrary_frames_round_trip`; live envelope validated (schema 1, size/cursor/alt_screen/defaults/palette_overridden/rows keys) |
| L1 tiers | cell/pixel/exact pass on match, fail on seeded mismatch; `missing_golden` on absent | PASS | `cell_tier_pass_fail_missing`, `pixel_tier_pass_fail_missing_platform`, `exact_tier_pass_and_pixel_only_fail`, `gate_pixel::pixel_tier_needs_baseline_and_passes_on_match` |
| L1 sidecar | fingerprint written+validated; font/version bump → `stale_golden` (not silent pass / false fail) | PASS | `stale_golden_on_font_bump_not_a_pass_or_false_fail`, `stale_golden_on_tampered_golden_file`, `fingerprint_stale_on_font_or_width_or_schema_or_tol_or_mask`, `fingerprint_round_trips_and_denies_unknown_fields` |
| L1 mask absence | masked timestamp + redacted token never in golden or diff | PASS | `mask_absence_secret_never_in_golden_or_diff`; daemon `glance_cells_masks_redact_emitted_content` |
| L1 mask invariance (080-owned) | change inside masked region does not alter `capture_sha256`, outcome, or pixel diff/heat | PASS | `mask_invariance_across_capture_hash_compare_and_pixels`, `mask_does_not_leak_secret_length_via_cursor` (adv F1.1 cursor-length-leak fix) |
| L2 perf | 10/100/1000-frame benchmarks recorded; max-artifact-size regression passes; RPC within budget | PASS | `make bench-lens-gate` numbers §4; `artifact_sizes_stay_within_budget` |
| L3 dogfood | real colored pane at 80×24 + 120×40 vs `cell` goldens; no daemon leak | PASS | daemon `glance_cells_*` at both viewports; live colored pane driven by this audit; 0 leaks §5 |
| L3 QA | full-res PNG + `pixel_verify.py` metric JSON for pixel tier | PASS | `lens-gate-frame-80x24.png` (720×456) + `pixel-metrics-80x24.json` (status pass, all deltas 0) |

Divergence set (proves the pixel tier "catches what cell misses" — D3, not
self-referential; genuinely different inputs): `divergence_cursor_shape_is_pixel_only`,
`divergence_glyph_fallback_is_pixel_only_under_a_different_font_stack`,
`divergence_blink_is_cell_signal_but_not_pixel`, `divergence_default_color_repaints_the_field`,
`divergence_palette_indexed_escalates_no_indexed_does_not`,
`divergence_cursor_position_and_visibility_diverge_at_pixel`,
`divergence_size_mismatch_is_hard_pixel_fail` — all PASS.

## 4. Test execution (re-run by this audit)

**Unit (touched crates):**
- `cargo nextest run -p shux-vt` → 342 passed, 0 skipped.
- `cargo nextest run -p shux-raster` → 38 passed, 0 skipped.
- `gate_compare` (shux-vt) → 17/17 incl. adv fixes: `palette_unportable_flags_indexed_underline_color` (F4.1 BLOCKER), `compare_cell_gates_alt_screen_flip`, `fingerprint_nan_tolerance_*`, `tol_params_validate_rejects_non_finite_and_out_of_range`.
- `gate_pixel` (shux-raster) → 13/13 incl. `exact_tier_corrupt_baseline_is_decode_error_not_content_fail` (F3.2), `pixel_gate_is_max_channel_not_mean` (F4.2), `pixel_tier_never_overrides_a_cell_fail` (D2), `render_is_byte_deterministic_for_same_envelope`.

**Integration (pure compare):** `make test-lens-gate-compare` → 18/18 passed, incl. `pixel_and_exact_baselines_are_pin_enforced_against_tamper` (impl-review BLOCKER #2).

**Daemon / shux automation:** `make test-lens-gate-glance-cells` (leak-guarded, `-j 1`) → 2/2 passed. Truecolor + indexed color probes at 80×24 and 120×40. Live colored pane driven by this audit (isolated `XDG_RUNTIME_DIR`).

**L2 perf (`make bench-lens-gate`):**
```
n=  10: capture=  11.99ms ( 834/s)  cell-compare= 3.68ms (2714/s)  render= 396.80ms (25/s)  [changed=19200  px_bytes=16896000]
n= 100: capture= 112.93ms ( 885/s)  cell-compare=35.16ms (2844/s)  render=   3.97s (25/s)  [changed=192000 px_bytes=168960000]
n=1000: capture=   1.15s ( 871/s)  cell-compare=350.59ms(2852/s)  render=  39.51s (25/s)  [changed=1920000 px_bytes=1689600000]
```
Capture ~870/s and cell-compare ~2850/s are flat/linear across 10→1000 (no super-linear blowup); render is the cost driver (~25 frames/s) — consistent with the task's selective/file-backed PNG policy.

**Lint:** `make lint` → clippy ✓, fmt ✓ (residual per-crate warnings are the pre-existing MSRV config note, not new).

## 5. Screenshot Matrix

| Viewport | Command/app | Screenshot | Pixel baseline | Diff | Status |
|---|---|---|---|---|---|
| 80×24 | real colored bash pane (truecolor + indexed + basic SGR) via `shux pane glance --png` | `lens-gate-frame-80x24.png` (720×456 RGBA) | independent re-render (2nd glance, byte-identical) | `diff-80x24.png` | PASS |

**Visual inspection (native res):** TRUECOLOR-ORANGE renders orange (fg 255,120,0);
INDEXED-GREEN renders on green bg (idx 28) white fg; BOLD-BLUE bold blue; colored
powerline prompt with git branch + version pills; cursor block visible. No tofu,
no color bleed after SGR reset, no wide-cell corruption, no clipping.

**Pixel metric (`pixel-metrics-80x24.json`):** `status: "pass"`, `changed_pixels: 0`,
`max_pixel_diff_ratio: 0.0`, `max_mean_channel_delta: 0.0`, `mean_rgba_channel_delta: 0.0`,
`size: [720, 456]`, `total_pixels: 328320`. Two independent glances of the same frame
are byte-identical (sha256 `1995dac2…`) — proves the pixel/exact tier's render
determinism (the property those tiers depend on). Per D3 this self-rendered frame is
determinism/plumbing proof; renderer-correctness "catches-what-cell-misses" is proven
by the non-self-referential divergence set in §3.

## 6. Findings

No P0/P1/P2 findings. One P3 (informational):

- **P3 (informational):** Task 080 is still `**Status:** In Progress` and `make check-progress`
  fails on it. This is the expected ordering — the SOLID gate is the precondition for
  marking Done. The implementer must flip task 080 + `docs/PROGRESS.md` row to **Done**
  and append the learnings entry (final open DoD row) after committing this PASS bundle.
  Not a gate failure.

## 7. Passed evidence

- 342 shux-vt + 38 shux-raster unit tests; 18 pure compare; 2 daemon glance; 1 bench — all green.
- Adversarial BLOCKER/MAJOR fixes (F4.1 indexed-underline palette leak, F1.1 cursor-length
  leak, F2.1 NaN-tolerance false-stale, F4.2 max-channel gate doc, F3.2 corrupt-exact-baseline)
  each have a passing regression test confirmed by name in this audit.
- Impl-review BLOCKER/MAJOR fixes (masked-glance cursor leak in daemon PNG+response,
  pixel/exact pin enforcement, `parse_glance_masks` width==0 reject, alt_screen cell-gating)
  covered by `pixel_and_exact_baselines_are_pin_enforced_against_tamper`,
  `compare_cell_gates_alt_screen_flip`, and the daemon mask-redaction test.
- Pixel hard gate satisfied by this audit's own render (0/328320 changed px).

## 8. Residual risk

- Self-rendered pixel baseline is determinism/plumbing proof only (D3, by design); renderer
  correctness rests on the divergence set + visual inspection — both satisfied here.
- Committed pixel/exact PNG baselines are `<os>-<arch>` partitioned and intentionally kept out
  of the CI test path (D3 cross-platform flake trap); the durable dev-host PNG here is
  aarch64-darwin. Cross-platform baseline provenance is a 081/082 concern, out of 080 scope.
- Exit-code mapping for `stale_golden`/`missing_golden` is owned by 082 (D6); 080 asserts
  statuses only — confirmed, no process-exit assertions in 080 tests.

## 9. Cleanup status

- Baseline `shux` daemons before audit: `70983 86746 92234 97431` (4, pre-existing, not mine).
- Isolated audit daemon `84226` spawned in a `mktemp`-created `XDG_RUNTIME_DIR` → terminated (SIGTERM→SIGKILL).
- Leak-guarded daemon suite exited 0 (guard reported no new leaks).
- Post-audit `pgrep -x shux`: `70983 86746 92234 97431` — exactly the 4 baseline, zero new daemons, zero orphans.
