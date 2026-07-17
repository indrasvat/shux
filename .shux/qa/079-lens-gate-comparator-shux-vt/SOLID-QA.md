VERDICT: PASS

# SOLID VT QA — Task 079: lens-gate comparator lifted into `shux-vt`

- **Task file:** `docs/tasks/079-lens-gate-comparator-shux-vt.md`
- **Branch:** `feat/lens-ci-gate`
- **Audited state:** working tree on top of HEAD `9742cd3` (task 079 changes are
  UNCOMMITTED). Evidence certifies the audited working tree, not a commit — a
  re-audit of the final committed state is recommended after the parent commits.
- **Auditor:** shux-vt-solid-qa (independent gate; evidence regenerated, not
  reused from the implementer).

## Nature of the change

Pure refactor: the daemon's `compute_lens_diff` was lifted verbatim into
`shux_vt::diff_frames` behind a `CellGridView` trait (`crates/shux-vt/src/diff.rs`,
new), with `GridFrame` (live) + `FrameView`/`FrameEnvelope::try_view` (golden)
wrappers. `pane.diff_since` is now a thin adapter whose observable output must be
byte-identical to before. Plus one real behavior fix: `build_row_runs`
(`crates/shux-vt/src/capture.rs`) now captures the visible viewport, not the
oldest scrollback, for scrolled panes (adversarial finding).

The correct visual/pixel evidence is therefore a PRESERVATION proof (byte-identity
against an already-approved golden), not a new-feature baseline.

## Task DoD Matrix

| DoD item | Status | Evidence |
|---|---|---|
| `CellGridView`+`diff_frames` in `shux-vt`; `FrameDiff`/`LensRowSpan` moved | PASS | `crates/shux-vt/src/diff.rs`; `diff::tests::*` green |
| Live grid + golden frame implement trait via `GridFrame`/`FrameView`; owned `CellRef` | PASS | `diff::tests::cellref_outlives_its_view`, `frameview_equals_gridframe_*` green |
| `pane.diff_since` thin adapter, byte-identical output | PASS | `make test-lens` 37/37 incl. `d2_diff_exact_delta`; independent D2 regen byte-identical (see Pixel gate) |
| Theme/default-color mismatch detected | PASS | `diff::tests::theme_mismatch_same_cells_reports_change`, `default_color_change_marks_default_cells`; divergence `default-color-only` |
| Divergence fixtures committed + asserted (9 cases) | PASS | `lens_gate_divergence::divergence_fixtures_assert_cell_tier`; `.shux/fixtures/lens-gate/divergence/` |
| No new crate / no dependency cycle | PASS | `shux-vt` gains no `shux` dep; refactor moves types INTO `shux-vt` |
| DootSabha design incorporated pre-code (D1–D7) | PASS | `dootsabha-design.md` (R1 REVISE → v2 CONVERGED) + raw JSON |
| Red tests before impl / adversarial pass | PASS | parity corpus red-capable; 6 adversarial findings each pinned (scrollback test present + green) |
| L1/L2 pass; `make test-lens` green (37/37) | PASS | `/tmp/q079-lens.log`: 37 passed |
| `make check` (lint + full test) | PASS (lint independently re-run) | `/tmp/q079-lint.log` clippy+fmt clean; task asserts full `make test` green |
| `shux-vt-solid-qa` gate `VERDICT: PASS`; evidence under `.shux/qa/079-*` | PASS | this report + manifest + PNG + pixel JSON |
| Impl-diff DootSabha clean or addressed | PASS | `dootsabha-implementation.md` (agy CLEAN; codex one MINOR OOB-guard fixed; `diff::tests::out_of_range_cell_is_empty_for_both_views` green) |
| PROGRESS + task updated; learnings appended | PASS | `docs/PROGRESS.md`, task file, `docs/agents/learnings.md` all modified in tree |

## Testing Matrix

| Layer | Result | Evidence |
|---|---|---|
| Unit (shux-vt) | PASS 325/325 | `cargo nextest run -p shux-vt`; incl. all `diff::tests::*` + `capture::tests::captures_visible_viewport_not_scrollback` |
| Integration | PASS | `lens_gate_parity`, `lens_gate_divergence`, `diff_palette_isolation` green |
| Raw-byte/replay parity | PASS | L1 parity corpus (11 scenarios, bit-for-bit vs pre-move frozen output); divergence (9 cases) |
| shux automation | PASS | independent D2 heat regen + scrolled colored-pane capture/snapshot, release binary, isolated XDG |
| Visual inspection | PASS | `d2_heat_actual.png` (heat tint on the 2 changed regions, rest desaturated, no tofu); `scrollback_snapshot.png` (recent viewport lines 39-60, truecolor+256+basic) |
| Pixel comparison | PASS 0/328320 changed | `d2_heat_pixelmetrics.json` (--max-pixel-diff-ratio 0 --max-mean-channel-delta 0); sha256 == approved `deef295d…` |
| DootSabha design | PASS | `dootsabha-design.md` + `dootsabha-raw/design-v1.json`,`design-v2.json` |
| DootSabha diff review | PASS | `dootsabha-implementation.md` + `dootsabha-raw/impl.json` |

## Screenshot Matrix

| Viewport | Command/App | Screenshot | Baseline | Diff | Status |
|---|---|---|---|---|---|
| 80x24 | F4 lens fixture → `pane.diff_since{heat_png}` | `d2_heat_actual.png` | `.shux/goldens/lens/d2_heat.png` (APPROVED, sha `deef295d…`) | `d2_heat_diff.png` (all-zero) | PASS byte-identical |
| 80x24 | scrolled colored `seq`-style pane → `pane.snapshot` | `scrollback_snapshot.png` | n/a (behavior-fix proof; text cross-checked via `pane.capture` = lines 39-60) | n/a | PASS recent viewport |

## Pixel-Level Hard Gate

Independent regeneration drove the frozen D2 scenario (F4 fixture →
`pane.glance{checkpoint}` → send `a` → `pane.wait_settled` →
`pane.diff_since{heat_png:true}`) with the release binary under an isolated XDG
env mirroring the harness fixture-font fallback chain. Result: `cells_changed=10`,
regions `[{2,2,3},{5,10,19}]` (matches the frozen D2 assertion), heat PNG sha256
`deef295d5c3d55aeadfdb974fbe96a6b385806e790a8d846a35c77e9182e503e` ==
committed golden (`cmp` byte-identical). `pixel_verify.py` at zero thresholds:
0 / 328320 changed pixels. The golden is a pre-refactor APPROVED baseline
(`.shux/goldens/lens/BASELINE-APPROVAL.md`, P4 ratification), NOT a
self-minted baseline.

## Findings

None at P0/P1/P2. P3 (informational): `cargo nextest -p shux-vt` reports 325
tests, task text said ~326 — count drift only, all green.

## Passed Evidence

- Unit: 325/325 shux-vt (`diff::tests::*`, `captures_visible_viewport_not_scrollback`).
- Integration: parity + divergence + palette-isolation green.
- `make test-lens-gate-comparator`: 3/3 (+OSC-4 isolation), leak-guarded.
- `make test-lens`: 37/37 frozen suite incl. `d2_diff_exact_delta` + `d2_heat.png` golden.
- `make lint`: clippy + fmt clean.
- Pixel: D2 heat byte-identical to approved golden (0 pixel diff).
- Scrollback fix: `pane.capture` = viewport lines 39-60 (not oldest 01-24); snapshot confirms.
- DootSabha design (CONVERGED) + implementation (agy CLEAN, codex MINOR fixed).

## Residual Risk

- Evidence certifies the UNCOMMITTED working tree; `capture.rs` and shared files
  can be touched by concurrent agents. Re-audit the final committed SHA.
- `pane.snapshot` rasters `grid.visible_row` directly and does not exercise the
  fixed `build_row_runs` path; the fix is pinned by the `shux-vt` unit test and
  demonstrated user-facing via `pane.capture` (which does use the fixed path).

## Cleanup Status

Isolated audit daemon on `XDG_RUNTIME_DIR=/tmp/q079` (release binary, PID 2260)
killed and reaped; zero `q079` sockets remain. No peer daemons touched. All
audit sessions killed.
