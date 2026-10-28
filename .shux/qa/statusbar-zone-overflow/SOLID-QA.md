VERDICT: PASS

# SOLID VT QA: status-bar zone overflow (issue #202), scope `statusbar-zone-overflow`

## 1. Change under audit

- **Audited SHA:** `1db5659` on `claude/project-thread-4txjje` (commits `6596550`, `3d83b99`, `1db5659` on `origin/main` `0ea4881`).
- **Diff:** `crates/shux-ui/src/statusbar.rs` (`StatusBar::render_row`, `paint_zone`) and shux-ui tests. `make check-vt-qa`
  treats `statusbar.rs` as a VT path because its cells go straight into composed snapshot grids.
- **Gate history:**

| Run | Result | Finding | Fix |
|---|---|---|---|
| `3d83b99` | FAIL | P1: `make check` red. `check-test-inventory` found 2 vanished test names. P3: the "no bare `…`" rule only treated U+0020 as a space. | `1db5659`: both old names restored beside their successors; padding and the text before `…` go by `char::is_whitespace` |
| `1db5659` | PASS | Greptile on PR #212 (P2): a cut right zone ended short of the edge (`Lab…  `; default bar at 14 cols `… ^Sp… `). P2: trailing padding was measured on every frame, even for zones that fit. | Delta commit: a cut right zone is shifted flush to the edge; padding is measured only for a zone that does not fit. Re-verified by the implementer, not a fresh gate run: see §3 row "Delta re-verification". |

## 2. Stated DoD (issue #202)

| # | Criterion | Status | Evidence |
|---|---|---|---|
| A1 | Zones get a defined priority when they do not fit | PASS | Left, then right, then centre (doc on `render_row`). Probe: 0 zone overlap/order failures in 7988 cases. |
| A2 | The centre is truncated or dropped rather than overlapping | PASS | Live 24/30/40/80: left zone whole. Base `0ea4881` overwrites it at the same widths (`◆ a-v ▶ #1/1 1 e RSCRIPT 日本` at 30). |
| A3 | A test at an overflowing width asserts no zone overwrites another | PASS | `issue_202_centre_no_longer_starts_inside_the_left_zone` and `zones_never_overlap_at_any_width`. On base `statusbar.rs` they fail with `left zone overwritten: "a-very-long-se ▶ #1/1 1 shell…"`; on the fix they pass. |

## 3. Testing matrix

| Layer | Result | Evidence (scratch root `.shux/out/statusbar-zone-overflow/`) |
|---|---|---|
| L1/L2 `make check` | PASS | Coordinator log at `1db5659`: "All checks passed", 2488 tests. Re-verified `make check-test-inventory` myself: "every test at 0ea48815f510 still runs" (`test-inventory-1db5659.log`). At `3d83b99` it was red (`make-check.log`). |
| A/B of committed tests | PASS | Base: 4 of 6 overflow tests red for the right reason. Fix: all green. |
| Randomised probe (auditor-written, scratch only) | PASS | 400 random bars mixing wide, ZWJ, combining, flag, VS16, skin-tone, ZWSP and U+3000 text, 1–3 segments per zone, truecolor/indexed/basic fg, segment and bar bg, every width 0..total+3: 7988 cases. At `1db5659`: 0 panics, 0 orphan wide or stray continuation cells, 0 zone overlap/split/order, 0 cross-path head-cell differences (attach bytes replayed through a VT vs `compose()`: glyph, width, style). Base: 11094 failures, so the probe can fail. |
| L3 raw replay | PASS (run at `3d83b99`) | `make test-vt-corpus` passed. Not re-run at `1db5659`, because it rewrites tracked files (#204); the `1db5659` delta touches only status-bar painting. |
| L4 shux automation | PASS | `live.sh`, an outer shux pane running `session attach` under `no_leak_guard.sh`. Isolated `XDG_RUNTIME_DIR`. A right-zone script segment (truecolor, indexed, `日本`). Pane probes: truecolor `38;2;255;120;0`, indexed `38;5;45`, basic `31`, `30;42`. |
| L5 visual | PASS | §4 |
| L6 pixel | PASS | 24 committed `pixel-*.json`, all at exact 0/0. |
| Delta re-verification (bot-review commit) | PASS | Sweep now asserts the right zone ends at the edge whenever painted; red on `1db5659` (`right zone off the edge`, `Lab…  `), green after. All 24 `pixel-*.json` regenerated from the delta build (`live-r2/`), 0/0 ×24; live attach 30x8 opened: `RSCRI…` flush right, left whole. `no_leak_guard.sh` exit 0 both runs; `make reap` clean. |
| L8 DootSabha | PASS (fallback) | `council-record.md`: `dootsabha` not installed; parallel cold-context reviewers substituted, per CLAUDE.md. |

## 4. Screenshot matrix

| Viewport | Path × config | Baseline | Metric | Status |
|---|---|---|---|---|
| 24/30/40/50/60/80 x8 | live attach vs window.snapshot, unicode | window.snapshot of the same session | `pixel-attach-vs-window-*-unicode.json` 0/0 ×6 | PASS |
| same | session.snapshot vs window.snapshot, unicode | same | `pixel-session-vs-window-*-unicode.json` 0/0 ×6 | PASS |
| same | attach and session vs window, nerd fonts | same | `pixel-*-nerd.json` 0/0 ×12 | PASS |

- **Fix vs base** (scratch `live/ab/`):
  - At 50 and 60 columns, where the bar fits, the two are identical (0/0 ×8). Bars that fit are laid out as before.
  - At 24 and 80 columns they differ only in pixel rows 135–149, which is the status row.
- **Opened at native resolution:** attach 30 (unicode), window 80 (nerd), and others.
  - The left zone is whole.
  - A cut zone ends in `…` (`RSCRI…`, `▶ #1/1 1…`).
  - A zone with no room is dropped.
  - The colour probes render.
  - There is no colour bleed, ghost cell or half-glyph.
- **Pre-existing (same on base):** the rasterizer draws CJK and emoji as tofu boxes.

## 5. Rich-TUI non-regression

- **vim** ran in a pane beside a colour-probe pane, with the status bar present, at 80x24, 40x12 and 30x12.
  - At 80x24 and 40x12 the fix is identical to base (0/0).
  - At 30x12 the only difference is pixel rows 211–225, the status row, where base overwrites the left zone.
- **Not installed:** `nvim`, `htop`, `btop`, `lazygit`, `vicaya`, `vivecaka`.

## 6. Findings

- **P1 (`3d83b99`): fixed in `1db5659`.** Test names had vanished; both are restored, and the inventory check passes.
- **P3: fixed.** U+3000 before `…`: `char::is_whitespace` is now used.
- **P3: declined by the implementer as cosmetic.** A ZWSP inside trailing padding keeps one padding cell. The probe's only 2 hits are both this case.
- **Pre-existing, not this diff:**
  - Live attach and compose give the hidden half of a wide glyph different styles (4163 cases). It is not visible in pixels, because the rasterizer never draws that cell.
  - On the first large shrink of an attached client (80x24 to 24x8), the outer terminal occasionally keeps leftovers of the old frame. Base: 1 of 8 runs; fix: 0 of 8 (`race/`). It needs its own issue. No committed metric used an affected frame.

## 7. Residual risk

- Only vim is installed, so no other live rich TUI was checked.
- No committed baseline exists for live chrome frames, so the pixel metrics are cross-path, not against a golden.

## 8. Cleanup

- Every daemon run went through `no_leak_guard.sh` and exited 0.
- `make reap` reports "no shux daemons from /home/user/shux are running", and `ps` shows no shux processes.
