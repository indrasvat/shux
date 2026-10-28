VERDICT: PASS

# SOLID VT QA: OSC 7501 status chrome, P2a (scope `osc7501-status-chrome`), final run at a4530be

## 1. Change under audit

- **Audited SHA.** `a4530be` on `feat/osc7501-status-chrome`. The diff is `origin/main...a4530be`, and `origin/main` was
  merged in at `71697bd`. There is no PR yet.
- **What the branch does (rendering).**
  - `shux-ui/src/chrome.rs` (new): `PaneChrome`, `title_overlay` and `text_cells`.
  - `composed.rs` and `compositor.rs` both draw titles and badges from that one overlay.
  - `statusbar.rs` measures and paints segments as VT cells.
  - Window and session snapshots draw borders in theme colours.
  - The daemon's `pane_chrome.rs` builds the badges, plus the "N waiting" segment and the `[program_status]`
    `border` / `status_bar` toggles.
- **Gate history. This is the fourth run.**

| Run | Result | Finding | Fix |
|---|---|---|---|
| `9ee66ee` | FAIL | F1: titles overwrote border junctions. F4: `chrome.rs` was missing from the guard's path list. | `bf22e26` |
| `bf22e26` | FAIL | F1b: a junction on the title's first column was still overwritten. | `a6280cc` (`s.x >= x0`) |
| `a6280cc` | FAIL | F7: attach and snapshot disagreed where status-bar zones overlap a wide glyph. | `a4530be`: `paint_segments` erases the other half of a wide glyph it paints over, as a terminal does |

- **Delta `a6280cc..a4530be`.**
  - `statusbar.rs` carries the fix and a unit test. `chrome_cross_path.rs` adds an attach-bytes-vs-compose test.
  - The merge of `origin/main` (#193 daemon/RPC/client, 0.52.2) touches **no** file under `shux-vt`, `shux-raster`,
    `shux-pty`, `composed.rs`, `compositor.rs`, `chrome.rs` or `borders.rs`.
- **Scope of this run, as the coordinator narrowed it:**
  - the status-bar repro cases, extended into a sweep;
  - the narrow live runs (30x12, 24x10, unicode and nerd, plus a resize sequence) for attach vs window vs session;
  - the junction edge probe.
- **Carried layers.** Other layers carry over from the earlier runs, because their code paths are untouched since then
  (§6).

## 2. Stated DoD matrix

Sources: PLAN §6.1 (P2a re-plan and council outcome) and the coordinator's four gate-focus items.

| # | Criterion | Status | Evidence (scratch root `.shux/out/osc7501-status-chrome/`) |
|---|---|---|---|
| G1 | Live attach == window.snapshot == session.snapshot for chrome and status bar, including CJK/combining/ZWJ/flag text and wide status segments, in both font sets | **PASS** | **Sweep** (`probe/src/bin/bar.rs`): 3803 status bars (5 left × 5 centre × 4 right zones with wide graphemes, widths 3–40). Live attach bytes replayed into a VT vs `compose()`: **0** differences (`bar-sweep-a4530be-r2.txt`). The same sweep at `a6280cc` gives **370** differences (`bar-sweep-a6280cc-red.txt`), so it can fail. The coordinator's 3 cases are in it: `ab日`/`ab👩‍💻` + `XY` at 6 columns now give `a|b| |X|Y| ` on both paths. **Live, narrow:** `edge.sh` gives attach/session vs window 0/0 ×8 (30x12 and 24x10 × unicode/nerd). `edge-seq.sh` resizes attach 30→24→30→24→80→30 and gives 0/0 ×24 (12 steps × attach and session). The stray `1 1` and the dropped `本`/`👩‍💻` are gone. Committed metrics: 8 `pixel-*.json`, all 0/0 pass. **Live, wide (`bf22e26`, carried):** 0/0 at 80x24, 120x40 and 200x60 × 2 fonts × 2 layouts. The status-bar change since then only affects overlapping zones, and the 80x24 steps above re-confirm 0/0 at `a4530be`. |
| G2 | pane.snapshot, pane.glance and committed goldens unchanged; pane geometry does not move | **PASS (carried)** | At `9ee66ee`: `make test-vt-corpus` gave 5/5 rich-TUI actuals sha256 == `.shux/goldens/073-vt-corpus`, and `make test-lens` golden tests were green. At `bf22e26`: lens glance/loop 5/5, and nvim `pane.snapshot` 0/0 ×6 against `9ee66ee`. Nothing since touches a pane render path (§1). The junction probe confirms chrome writes only top-border cells. |
| G3 | nvim, lazygit and btop/htop render unchanged with status on | **PASS (carried)** | At `9ee66ee`, window snapshots of lazygit, htop and btop with ◆/✓/✕ badges were inspected at 200x60, and nvim was 0/0 across toggles. The pane content path is untouched since. |
| G4 | Borders, corners and junctions never overwritten (narrow, zero-height, zoomed, every style) | **PASS** | `probe-run6-a4530be-edges.txt`: 4032 cases (6 + 8 edge-ratio layouts × 6 styles × 8 sizes × zoom × 3 chrome sets) give **0** overwrites and 0 differences between attach and snapshot. It was red at `9ee66ee` (100) and at `bf22e26` (60). Live, in both fonts at 30x12 and 24x10, the 1-column pane's separator joins the border below. The wide pane draws no title, as the narrow rule says. At `bf22e26`, L and grid layouts at 3 sizes kept every junction. |
| D1–D7 | Single builder and overlay; VT-identical layout; theme border colours; glyphs in the primary font; toggles and hot reload; malformed config rejected; output identical to `main` with the feature off | PASS (carried) | Verified at `9ee66ee`, including head vs `origin/main` window.snapshot 0/0 ×6 (`ab/`). |
| F4 | `check-vt-qa` covers `chrome.rs` | PASS | `bf22e26`: the self-test reports "✓ pane chrome layout owes evidence (exit 2)". |
| L8 | DootSabha design and implementation | PASS (see `council-record.md`) | The design council is in PLAN §6.1. A codex `dootsabha consult` reviewed `1e0fcf3`, and its findings were fixed in `37fae50`. I did not see the raw output. This gate's runs stand in for the later fix commits. |

## 3. Testing matrix

| Layer | Result | Evidence |
|---|---|---|
| L0 build | PASS | `make build` at `a4530be` (`make-build-a4530be.log`) |
| L1 unit | PASS | The new `a_zone_painted_over_half_a_wide_glyph_blanks_the_other_half` (coordinator: red on the unfixed painter). Earlier: shux-ui 264 and then 265 tests, all passing. |
| L2 integration | PASS (carried + new) | `make test` 2434/2434 at `9ee66ee`. The new cross-path test in `chrome_cross_path.rs` (coordinator: red first). My sweep covers the same seam with 3803 cases. |
| L3 raw replay | PASS (carried) | Corpus 5/5 byte-identical at `9ee66ee`. The harness depends only on `shux-vt`, which this branch does not touch. |
| L4 shux automation | PASS | `run-a4530be-live.sh` ran 4 passes (`edge` and `edge-seq` × unicode/nerd), each under `.shux/scripts/no_leak_guard.sh` with exit 0. Isolated `XDG_RUNTIME_DIR` `/tmp/vtq2a` (inner) and `/tmp/vtq2b` (outer attach). Every pane prints truecolor `38;2;255;120;0`, indexed `38;5;45`, basic `31` and `30;42`. |
| L5 visual | PASS | §4 |
| L6 pixel | PASS | 32 live comparisons at 0/0 this run. 8 of them are committed as `pixel-*.json`. |
| L7 verdict | this report | |
| L8 DootSabha | PASS | `council-record.md` |

## 4. Screenshot matrix

`edge-a4530be-*` paths are under `.shux/out/osc7501-status-chrome/live/`.

| Viewport | Path × config | Screenshot | Baseline | Metric | Status |
|---|---|---|---|---|---|
| 30x12, 24x10 | attach and session vs window; unicode (rounded, NF off, wide script segment `日本 é 👩‍💻 🇮🇳 中`) | `edge-a4530be-unicode/v1_*` | window.snapshot of the same session | `metrics/*.json` 0/0 ×4 | PASS |
| 30x12, 24x10 | same; nerd (thick, NF on, theme `#ff6600`/`#3366ff`/`#ffee00`) | `edge-a4530be-nerd/v1_*` | window | 0/0 ×4 | PASS |
| 30/24/30/24/80/30 | resize sequence, unicode and nerd | `edge-a4530be-{unicode,nerd}-seq/v1_*_<i>.png` | window per step | 0/0 ×24; committed: `pixel-attach-vs-window-{30x12,24x10,80x24}-{unicode,nerd}.json` and `pixel-session-vs-window-30x12-{unicode,nerd}.json` | PASS |

- **Opened at native resolution.**
  - Unicode attach at 24x10: the junction is joined and the status bar reads `◆ edge 日 ▶ #1/1 1`.
  - The status-row crop at 30x12 (`edge-a4530be-unicode/crop-status-30x12.png`) shows attach above window, identical:
    `◆ edge 日 本 ´e ▶ #1/1 1`.
- **What the frames show.**
  - Wide glyphs that a zone overlapped are blanked on both paths, with no half-glyph and no shifted tail.
  - The colour probes render.
  - There is no colour bleed, ghost cell or cursor artifact.
- **Known renderer-v1 limits (not this diff).** CJK draws as tofu and `é` as `´e` in the raster, the same as in pane
  content.

## 5. Findings

- **There is no open P0, P1 or P2.**
- **Resolved:**
  - F1, titles over junctions (`bf22e26`);
  - F1b, a junction on the first column (`a6280cc`);
  - F4, the guard's path list (`bf22e26`);
  - F7, status-bar wide-cell overlap (`a4530be`).
  - Each fix was proven red, then green, by this gate's probes as well as by the implementer's tests.
- **F2 (process, recorded).** The raw implementation-council output was not provided to this gate. The disposition is
  the `37fae50` commit message. See `council-record.md`.
- **F3 (P3, not this diff).** `lens_fixtures_smoke::f7_winsize_…` fails on `origin/main` too. The coordinator is
  filing an issue.
- **F6 (P3, not this diff).** `borders.rs` draws `┼` where only `┴` meets. The coordinator is filing an issue.
- **F5 (harness noise).** The 19 `.shux/qa/073-…/*-diff.png` files rewritten by `make test-vt-corpus` earlier are
  still modified in this worktree. They must stay out of any commit.
- **Observation.** After `shux daemon stop`, `shux.lock` and `shux.start.lock` remain in the runtime dir. That is the
  merged #193 daemon's lock files, and no process is behind them. It is not this diff.

## 6. Residual risk

- These were not re-run at `a4530be`, by instruction: `make test`, `make test-lens` and `make test-vt-corpus`. The
  coordinator reports that `make check`'s leak-guard self-test fails on `origin/main` since #193 (#206).
- They are carried because no pane-render, VT, raster or PTY file changed after the run that verified them (§1).
- `make test-gui-terminal` (the outer-terminal width of the ambiguous-width badge glyphs, PLAN §6.1) was not run by
  this gate.
- Uptime differs between attach and snapshots, so every comparison masks the trailing status cells (`mask_uptime.py`).
  The 80x24 steps keep "◆ 1 waiting" inside the compared area.

## 7. Cleanup

- `leakcheck.sh`: `/tmp/vtq2{a..e}` all report "daemon not running".
- No `debug/shux` processes remain, and no `sleep 900` children survive.
- All four live runs exited their leak guard with 0. Sessions `edge` and `outer` were killed each run, and both daemons
  were stopped with `shux daemon stop`.
