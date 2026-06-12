# Task 073: shux-vt Corpus Regression Harness

**Status:** Done
**Priority:** High
**Milestone:** VT Quality Track
**Depends On:** 066
**Touches:** `Makefile`, `.shux/scripts/`, `.shux/goldens/`, `.shux/fixtures/vt-corpus/`, `.shux/qa/073-shux-vt-corpus-regression-harness/`, `crates/shux-vt/tests/`

---

## Problem

VT improvements need a durable corpus. One-off screenshots from spikes are
useful for exploration, but they do not prevent regressions. shux already has
lossless `pane.record`; we should use it to build a repeatable VT quality
harness for synthetic fixtures and real TUI byte streams.

## Scope

Create a Makefile-driven corpus harness that:

- runs deterministic synthetic VT byte fixtures,
- replays committed raw PTY streams from rich TUIs,
- records optional refreshed raw PTY streams into `.shux/out/073-vt-corpus/`,
- captures text and PNG output,
- compares pixel output against baselines,
- emits a concise machine-readable report.

The harness has two baseline layers:

- **Synthetic fixtures are correctness anchors.** Their expected text/grid
  output is hand-authored from VT behavior, and their PNG baselines are allowed
  to be pixel-exact gates because the cell/text oracle is independent of the
  implementation output.
- **Rich-TUI replays are regression/drift gates.** Their PNG baselines are
  frozen shux-vt output, blessed by DootSabha design review for regression
  detection only. They are not an independent correctness oracle.

The artifact flow is strict:

- `.shux/out/073-vt-corpus/` — scratch render output and refreshed recordings.
- `.shux/goldens/073-vt-corpus/` — committed baseline text/JSON/PNG artifacts.
- `.shux/qa/073-shux-vt-corpus-regression-harness/` — committed review evidence: DootSabha outputs,
  full-resolution PNGs, pixel metric JSON, report, manifest, and SOLID verdict.

Synthetic fixtures use a typed action-script schema, not just raw byte blobs,
so resize and terminal request/response behavior can be tested:

```json
{
  "name": "resize-reflow-basic",
  "init": {"rows": 24, "cols": 80},
  "steps": [
    {"process": {"text": "long wrapped text..."}},
    {"resize": {"rows": 24, "cols": 40}},
    {"expect_text": "expected visible text"},
    {"render_png": true}
  ]
}
```

Rich-TUI replay manifests must freeze render-determinism inputs: rows, cols,
font size, bundled font identity, default foreground/background colors, and
cursor policy. Rich-TUI replay PNGs render with cursor disabled; cursor
presentation belongs in dedicated synthetic fixtures.

## Determinism and CI Stance

Exact PNG comparison is intended to be canonical on local macOS and Linux CI
for this corpus. The render path uses `fontdue` with embedded font bytes, fixed
font size, fixed default colors, fixed rows/cols, and cursor-disabled rich-TUI
replays, so it avoids host terminal/fontconfig/subpixel state. The CI `vt-qa`
job runs `make test-vt-corpus` with exact thresholds before enforcing the QA
evidence contract. If a future runner proves exact PNGs unstable, this task
must be reopened with measured pixel metrics before thresholds are weakened.

Out of scope:

- Requiring every optional TUI to be installed on every machine.
- Making flaky live network tools mandatory.

## Mandatory Process

- Run DootSabha design council before coding.
- Run DootSabha implementation-diff council before marking done.
- Invoke `shux-vt-solid-qa`.
- Save auditable task artifacts under `.shux/qa/073-shux-vt-corpus-regression-harness/`.

## Testing Matrix

| Layer | Required Evidence |
|---|---|
| Unit | Fixture parser/replayer handles partial chunks and invalid bytes deterministically. |
| Integration | Harness runs synthetic fixtures through `VirtualTerminal` and compares expected text/cells. |
| Integration | Harness replays committed raw `.raw` PTY files from `.shux/fixtures/vt-corpus/rich-tui/`. |
| Makefile | `make test-vt-corpus` or equivalent target exists and is documented in `make help`. |
| Shux automation | `make record-vt-corpus` captures installed TUI streams into `.shux/out/073-vt-corpus/recordings/`. |
| Visual | Harness emits individual full-resolution PNGs and optional contact sheets; individual PNGs remain the review source. |
| Pixel | Harness calls `.claude/automations/pixel_verify.py` with task-stated thresholds, defaulting to exact `--max-pixel-diff-ratio 0.0` and `--max-mean-channel-delta 0.0`. |
| Determinism | Document whether exact PNG comparisons are canonical on all local/CI platforms or Linux-CI only; do not weaken thresholds without measured evidence. |
| QA | `shux-vt-solid-qa` returns `VERDICT: PASS` in `.shux/qa/073-shux-vt-corpus-regression-harness/SOLID-QA.md`. |

## Acceptance Criteria

- [x] Corpus includes fixtures for resize reflow, wide cells, graphemes, DEC graphics, tab stops, origin mode, OSC defaults, alternate screen, scroll regions, and sync output.
- [x] Committed real-TUI fixtures exist for `btop`, `lazygit`, `nvim`, `vicaya-tui`, and `vivecaka`; optional live refresh reports missing tools without skipping committed replay.
- [x] Generated reports include pass/fail, screenshot paths, baseline paths, diff paths, and metric values.
- [x] Harness is deterministic enough for CI on committed fixtures.
- [x] Rich-TUI baselines are treated as regression-only goldens and never self-minted as proof in the same implementation pass.
- [x] Synthetic fixtures include action-script coverage for resize, request/response, default colors, alternate screen, scroll regions, and sync output.

## Definition of Done

- [x] DootSabha design and implementation-diff reviews are saved.
- [x] Make targets are added and documented.
- [x] Unit, integration, shux automation, visual, and pixel checks pass.
- [x] Full-resolution PNGs, pixel metric JSON, and `evidence-manifest.json` are committed under `.shux/qa/073-shux-vt-corpus-regression-harness/`.
- [x] `shux-vt-solid-qa` hard-gate report is `VERDICT: PASS` saved to `.shux/qa/073-shux-vt-corpus-regression-harness/SOLID-QA.md`.
- [x] `make check` passes.
- [x] Progress and learnings are updated.
