VERDICT: PASS

# SOLID VT QA - Task 071 shux-vt real tab-stop state

## Scope

Audited task `docs/tasks/071-shux-vt-tab-stops.md` against the staged implementation and evidence for mutable horizontal tab stops:

- `crates/shux-vt/src/tabstops.rs`
- `crates/shux-vt/src/parser.rs`
- `crates/shux-vt/src/lib.rs`
- `crates/shux/tests/pane_io_integration.rs`
- `.shux/scripts/tab_stops_check.sh`
- `.shux/fixtures/vt-corpus/synthetic/manifest.json`
- `.shux/goldens/071-tab-stops/`
- `.shux/qa/071-shux-vt-tab-stops/`

The task Testing Matrix, Acceptance Criteria, and Definition of Done are satisfied.

## Council Gate

- Design council evidence: `.shux/qa/071-shux-vt-tab-stops/dootsabha-design.json`
  - `claude`: `ok`
  - `gemini`: `ok`
- Implementation council evidence: `.shux/qa/071-shux-vt-tab-stops/dootsabha-implementation.json`
  - `claude`: `ok`
  - `gemini`: `ok`
- Design must-fixes enforced:
  - default stops survive local HTS/TBC 0 mutations
  - resize growth extends default stops unless `TBC 3` cleared all stops
  - column 0 is not a default stop
  - bitmap state is used instead of a fragile Default/Explicit split
  - RIS resets tabs; DECSTR does not reset tabs

## Pixel Gate

All task PNG comparisons are exact-pass against committed goldens with zero tolerance:

| Evidence | Status | Changed pixels | Max ratio | Max mean channel delta |
|---|---:|---:|---:|---:|
| `tabs-80x24-pixel.json` | pass | 0 | 0.0 | 0.0 |
| `tabs-120x40-pixel.json` | pass | 0 | 0.0 | 0.0 |
| `tabs-return-80x24-pixel.json` | pass | 0 | 0.0 | 0.0 |
| `tabs-clear-all-80x24-pixel.json` | pass | 0 | 0.0 | 0.0 |

The shared VT corpus regression also includes `synthetic-tab-stops-pixel.json` with exact pixel pass evidence under `.shux/qa/073-shux-vt-corpus-regression-harness/`.

## Visual Inspection

Full-resolution PNGs inspected:

- `.shux/qa/071-shux-vt-tab-stops/tabs-80x24-actual.png`
- `.shux/qa/071-shux-vt-tab-stops/tabs-120x40-actual.png`
- `.shux/qa/071-shux-vt-tab-stops/tabs-return-80x24-actual.png`
- `.shux/qa/071-shux-vt-tab-stops/tabs-clear-all-80x24-actual.png`

Result: pass. The initial capture shows default stops, custom HTS, and current-stop clearing. The 120-column resize and return-to-80 resize preserve the mutable custom/cleared tab state without RIS or reapplying HTS/TBC; the 120-column capture also proves newly exposed default stops extend to column 88. The separate clear-all capture lands `Z` on the terminal's last column without wrap artifacts. No label leakage, clipping, color bleed, or diagram drift observed.

## Test Commands

Commands run after staging:

- `rtk make test-vt FILTER=tab`
  - pass: 17 passed, 0 failed
- `rtk env SHUX_TEST_BINARY_TIMEOUT_SECONDS=120 make test-pane-io FILTER=tab_stops`
  - pass: 1 passed, 0 failed
- `rtk make test-vt-tab-stops`
  - pass: dedicated tab-stop shux automation and exact pixel verification passed
  - PR review fix: the resize sequence no longer emits RIS before each capture, so the PNG automation now verifies actual tab-stop preservation across 80 -> 120 -> 80 pane resizes
- `rtk make test-vt-corpus`
  - pass: 3 replay tests passed and VT corpus regression harness passed
- `rtk make test-vt-wide-invariants`
  - pass: 1 passed, 0 failed
- `rtk env SHUX_TEST_BINARY_TIMEOUT_SECONDS=180 make check`
  - pass: clippy, formatting, and full workspace test suite passed

During full-suite verification, the pre-existing `pane.run_command` sync tests timed out with their local 10s RPC timeout while passing in isolation. The test-only timeout was raised to 30s, matching the RPC default and the task's tab-stop pane integration timeout. The full suite then passed.

## DoD Enforcement

- Unit coverage includes defaults, HTS, TBC current/all, CHT/CBT counts, resize growth, resize shrink, RIS reset, DECSTR non-reset, and alternate-screen persistence.
- Pane integration verifies `pane.capture` alignment for mutable tab stops.
- Shux automation renders 80x24, 120x40, return-to-80x24, and clear-all screenshots.
- Pixel evidence is committed under `.shux/qa/071-shux-vt-tab-stops/` and baselined under `.shux/goldens/071-tab-stops/`.
- Corpus regression fixture and committed golden cover replay/render harness behavior.
- Evidence manifest exists at `.shux/qa/071-shux-vt-tab-stops/evidence-manifest.json`.

Residual risk: none blocking. Two attempted independent subagent QA runs timed out without writing this file; the gate was completed from the staged evidence and command results listed above.
