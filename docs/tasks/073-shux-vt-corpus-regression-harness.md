# Task 073: shux-vt Corpus Regression Harness

**Status:** Not Started
**Priority:** High
**Milestone:** VT Quality Track
**Depends On:** 066
**Touches:** `Makefile`, `.shux/scripts/`, `.shux/goldens/`, `.shux/fixtures/vt-corpus/`, `.shux/qa/073-vt-corpus/`, `crates/shux-vt/tests/`

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

Out of scope:

- Requiring every optional TUI to be installed on every machine.
- Making flaky live network tools mandatory.

## Mandatory Process

- Run DootSabha design council before coding.
- Run DootSabha implementation-diff council before marking done.
- Invoke `shux-vt-solid-qa`.
- Save auditable task artifacts under `.shux/qa/073-vt-corpus/`.

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
| QA | `shux-vt-solid-qa` returns `VERDICT: PASS` in `.shux/qa/073-vt-corpus/SOLID-QA.md`. |

## Acceptance Criteria

- [ ] Corpus includes fixtures for resize reflow, wide cells, graphemes, DEC graphics, tab stops, origin mode, OSC defaults, alternate screen, scroll regions, and sync output.
- [ ] Committed real-TUI fixtures exist for `btop`, `lazygit`, `nvim`, `vicaya-tui`, and `vivecaka`; optional live refresh reports missing tools without skipping committed replay.
- [ ] Generated reports include pass/fail, screenshot paths, baseline paths, diff paths, and metric values.
- [ ] Harness is deterministic enough for CI on committed fixtures.

## Definition of Done

- [ ] DootSabha design and implementation-diff reviews are saved.
- [ ] Make targets are added and documented.
- [ ] Unit, integration, shux automation, visual, and pixel checks pass.
- [ ] Full-resolution PNGs, pixel metric JSON, and `evidence-manifest.json` are committed under `.shux/qa/073-vt-corpus/`.
- [ ] `shux-vt-solid-qa` hard-gate report is `VERDICT: PASS` saved to `.shux/qa/073-vt-corpus/SOLID-QA.md`.
- [ ] `make check` passes.
- [ ] Progress and learnings are updated.
