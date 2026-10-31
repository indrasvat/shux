VERDICT: PASS

# SOLID VT QA — mouse-view-offset (#192)

| | |
|---|---|
| Issue | #192 — forwarded mouse and wheel events ignore the pane view offset |
| Branch | `fix/mouse-view-offset` |
| HEAD audited | `8016fde` — *fix(attach): never swallow an event of a gesture underway* |
| Base | `6b21101` |
| VT-gated files in the diff | `crates/shux-ui/src/compositor.rs`, `crates/shux-ui/src/viewport.rs` |
| Scope | rendering identity: the compositor now draws each pane through a `PaneView` (`PaneView::of`, same `pane_view_row_offset` as before) and records `pane_views()` per frame for input mapping. Mouse encoding itself belongs to the `shux-tui-qa` gate and is not judged here. |

## 1. Claim under test

Every cell, cursor position, image placement and pixel of live attach output,
`pane.snapshot`, `window.snapshot` and `session.snapshot` is identical at `8016fde`
and `6b21101`, for fitting, oversized, undersized, split and zoomed panes, rich
TUIs and colour probes. `pane_views()` records the view that was actually drawn.

The claim holds. Every committed comparison is exact (0 changed pixels), the
comparator is shown to fail on two reintroduced defects, and a head binary built
independently by this audit reproduces every result.

## 2. Stated DoD matrix (VT-relevant rows)

| # | Requirement | Result | Evidence |
|---|---|---|---|
| 1 | Attach output identical to base: fitting, oversized, undersized, split, zoomed | **PASS** | §4 — 31 `attachbody` metrics, 0/0 |
| 2 | Snapshot paths identical to base | **PASS** | §4 — window/session/pane metrics, 0/0; 142/142 PNGs and 76/76 text captures per batch |
| 3 | Image placement unchanged in an oversized pane | **PASS** | `img-bottom` at 80x24, 120x40, 200x60, every path, 0/0, three runs |
| 4 | Rich TUIs render correctly (vim/nvim, lazygit, btop/htop) | **PASS** | committed `rich-tui` replays A/B (btop, lazygit, nvim, vicaya, vivecaka); live vim, nvim, btop, htop (private `HTOPRC`), lazygit inspected (§5) |
| 5 | Colour probes: truecolor + 256 + basic | **PASS** | every probe scene prints all three; visible in every probe frame |
| 6 | `pane_views()` names the view that was drawn | **PASS** | mutation M1 (§3) caught by `pane_grid_cell_names_the_cell_the_compositor_drew` |
| 7 | Zero leaked daemons | **PASS** | §8 |

DoD rows 1–6 and 9 of `.shux/out/192/DoD.md` (mouse routing, gestures, zoom
focus, two clients) are input behaviour, outside this gate.

## 3. Testing matrix

| Layer | Result | Evidence |
|---|---|---|
| Unit | PASS | `make test` in a clean detached worktree at `8016fde`: 2703 run, 2703 passed (2 leaky), 2 skipped. Includes the 8 `viewport::tests` and `compositor::tests::test_multi_pane_oversized_pane_renders_top_content` |
| Integration | PASS | same run, all crates including daemon-backed `shux` suites |
| Mutation M1 — `compose_pane` draws at offset 0 while the view records the real one | caught | 287/288; fails `pane_grid_cell_names_the_cell_the_compositor_drew` with "(0,0) pinned to a cell not shown" |
| Mutation M2 — image placements resolved at offset 0 | **not caught by `make test`** (2703/2703) | caught only by this audit's pixel A/B: 3/3 `img-bottom` attach frames fail, 2432 px each — finding P3-1 |
| Raw byte / replay | PASS | `.shux/fixtures/vt-corpus/rich-tui/{btop,lazygit,nvim,vicaya,vivecaka}.raw` replayed at 120x36 into 120x40 (grid wider than rect) and 200x60 (narrower) windows, every path, base vs head |
| shux automation | PASS | each arm: an isolated daemon on the binary under test, and a real `shux session attach` client running in a pane of a second isolated daemon (the host); colour-probed shell, vim, nvim, kitty images |
| Visual inspection | PASS | §5 |
| Pixel comparison | PASS | 61 committed metrics, all `status: pass`, exact 0/0 |
| Comparator falsifiability | PASS | M1 binary: `over`/`splitv` 120x40 attach frames fail, 16886 px each; M2 binary: as above |
| Independence | PASS | the whole matrix re-run against a head binary this audit built itself (sha256 `87f997d9cd6c`; the lead's is `70250a9b6836`): 154/154 PNGs identical to base |
| DootSabha design | PASS | `council-design.md` — round 1 FIX-FIRST on D1 produced the per-frame `PaneView` record this audit covers. See P3-3 on timing |
| DootSabha implementation | PASS (exists, covers the whole diff) | `council-implementation.md` — rounds 2–5. Round 5 is **not** a clean SHIP; see P2-1 |

## 4. Screenshot / pixel matrix

Each cell is one scene run twice — base and head binaries, fresh daemons — then
compared with `.claude/automations/pixel_verify.py --max-pixel-diff-ratio 0
--max-mean-channel-delta 0`. "attach" is the attach client's full screen less its
status-bar row: that row's uptime segment ticks each second, so two runs can
never match there, and the status bar is outside this diff.

| Scene | 80x24 | 120x40 | 200x60 | Paths compared |
|---|---|---|---|---|
| fit | 0 px | 0 px | 0 px | attach, pane, window, session |
| over (grid 2x rect, cursor bottom) | 0 | 0 | 0 | all four |
| under (grid half the rect) | 0 | 0 | 0 | all four |
| splitv (left pane oversized) | 0 | 0 | 0 | all four, both panes |
| splith (top pane oversized) | 0 | 0 | 0 | all four, both panes |
| zoom (oversized pane zoomed) | 0 | 0 | 0 | all four, both panes |
| img-bottom (kitty image, oversized) | 0 | 0 | 0 | all four |
| vim-over (`+100`, syntax on) | 0 | 0 | — | all four |
| nvim-split | — | 0 | — | all four, both panes |
| replay btop / lazygit / nvim / vicaya / vivecaka | — | 0 | 0 | all four |

Totals: `ab` 142/142 PNG + 76/76 text identical; `ab-own` 142/142 + 76/76;
`imgb`, `imgb-own`, `imgb-rep` 12/12 + 6/6 each. Committed: every attach metric
(31), window/session/pane for six 120x40 scenes (18), and every `img-bottom`
metric (12). Frames: `.shux/out/192/vtqa/{ab,ab-own,imgb,imgb-own,imgb-rep}/`;
diff images under each `diff/`.

## 5. Visual inspection

Opened at full resolution: attach frames for fit, over, under, splitv, splith,
zoom at several sizes; vim-over; nvim-split; img-bottom attach and window;
replay btop and lazygit; live btop, btop oversized, htop, lazygit.

- Oversized panes show the cursor-following slice: last probe lines, `END-VQ`,
  prompt and cursor on the last shown row. Never a blank top or a stray cursor.
- CJK wide cells, the emoji and box drawing render with no tofu (the probe config
  adds a CJK fallback face); SGR resets leave no colour bleed; reverse video is
  bounded.
- Zoom draws only the zoomed pane; splits draw both panes with the focused border
  coloured.
- Live htop, btop and lazygit render complete layouts. htop ran with a private
  copy of the rc file through `HTOPRC`; the copy was byte-identical afterwards.

## 6. Findings

No P0 or P1.

**P2-1 — the closing council did not converge (input contract, not rendering).**
Round 5's codex reviewer returned FIX-FIRST: with a legacy (X10) host and the
shown slice entirely past row 223, a secondary press inside a gesture underway
is pinned to row 223, a cell the client is not shown. The chair calls it a
contract decision. It changes no cell or pixel, so it does not bear on this
verdict, but it should be settled in `shux-tui-qa` and the PR description
instead of being summarised as "no defect".

**P2-2 — shux as a terminal ignores kitty `a=p` (pre-existing, not this diff).**
`crates/shux-vt/src/lib.rs:691` discards `Action::Put`. Two consequences:
(a) inside a pane, an app that transmits once and later re-places an image with
`a=p` sees nothing move; (b) this audit cannot check attach re-placements
through a shux host. When an image appears while the oversized view is still
moving, the first `a=T` lands at that frame's offset and the attach emitter's
later `a=p` correction is dropped by the host, so the picture looks misplaced.
Both binaries do this: base 4/4 runs, head 3/4 (one head run happened to
transmit after the view settled). Those scenes (`img-over`, `img-last`) were
kept out of committed evidence and replaced by `img-bottom`, where the view
never moves. `make test-gui-terminal` (kitty under Xvfb) is the right check for
this emit path.

**P3-1 — no test pins the image offset at a line this diff touched.**
`resolve_placements(*rect, vt.grid(), view.row_offset)` can be changed to `0`
with `make test` still fully green (M2). Base has the same gap; only this audit's
pixel A/B catches it. A compositor unit test placing an image in an oversized
pane and asserting the emitted CUP row would close it.

**P3-2 — a CLI `pane split` while attached leaves pane PTYs at their old sizes
(pre-existing).** In `nvim-split`, the original pane stays 118x37 and the new one
80x24 inside rects about 58 columns wide, so nvim's ruler is clipped. Identical
at base; belongs with #190.

**P3-3 — the "design" council ran on the first implementation commits.** Round 1
started within a minute of `95346b5`. The earlier design source is the Sep 2026
attach-resize research verdict. Process only.

**P3-4 — `make test` reported 2 leaky tests** (nextest `LEAK`) in the
audited-head run and none in the M2 run, so the leak is intermittent. The
process-leak guard passed both times.

## 7. Residual risk

- The attach emit path for moving image placements (P2-2) is unverified by any
  shux-rendered check, at base and head alike.
- The attach status-bar row is excluded from pixel identity (timing-dependent
  uptime). The status bar is not in this diff.
- Live animated TUIs are inspected, not pixel-compared; their A/B identity rests
  on the committed raw replays.

## 8. Cleanup

- Every scene ran in fresh runtime dirs (`/tmp/vqi*`, `/tmp/vqh*`), each daemon
  stopped by its runner's EXIT trap; `daemon status` reports "not running" for
  all of them.
- One exploratory command run without `XDG_RUNTIME_DIR` started a daemon on the
  default socket, with zero sessions. It was found by the final process sweep and
  stopped with `daemon stop`. Afterwards, `ps` shows no process from the audit's
  binaries.
- No shux sessions remain. The human follow-along runtime dirs `/tmp/shux192` and
  `/tmp/s192i` were never touched.
- Harness and logs: `.shux/out/192/vtqa/harness/`.
