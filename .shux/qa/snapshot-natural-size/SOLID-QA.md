VERDICT: PASS

# shux-vt-solid-qa — snapshot-natural-size

## Change under audit
- PR #230, branch claude/project-thread-58qznu. Latest audited SHA: **6f1ddab**.
- Round 4 audits **cef1182** only: `fix(raster): clip a glyph's ink to its pane or bar, not to its cell row`.
  - `Rasterizer::render` is unclipped again, as on main.
  - New `Rasterizer::render_regions(grid, opts, regions)` keeps a glyph's ink inside the region that owns its cell. A region is a pane rect or the status-bar rect. Cells in no region (borders, gap columns) share one region.
  - `shux_ui::compose` returns `ComposedFrame.regions`.
  - `snapshot_window` (`session.snapshot` and `window.snapshot`) calls `render_regions`.
- Rounds 1-3 audited the natural-size snapshot commits: 40878bf, 0edab07, 9ba0b65, 84307b6 and 373dd97.
- The attach and client-log commits after 373dd97 are out of scope; the TUI gate covers them.

| Round | Commit audited | Verdict | Findings |
|---|---|---|---|
| 1 | 40878bf (at HEAD 1c17968) | FAIL | F1-F5: dead bands and clipping, a 1000-col clamp, unvalidated dims, no cap hint |
| 2 | 21de4cf | FAIL | F6: a one-dim snapshot cropped the other axis. F7: status-bar glyph ink bled into a borderless or zoomed pane's last row |
| 3 | 84307b6, then 373dd97 | PASS | none. 84307b6's per-row clip later broke 5 VT corpus goldens in CI (box-drawing joins) |
| 4 | cef1182 at 6f1ddab | PASS | No P0 or P1. P2 and P3 items are listed below |

Every round used its own `XDG_RUNTIME_DIR` and `XDG_CONFIG_HOME`.

## Round 4 DoD matrix (6f1ddab, real binaries from `make build`)
"Exact" means `pane.snapshot` equals the pane's box cropped from `session.snapshot` under `pixel_verify.py` at 0/0, including the pane's last pixel row.

How the crop is placed:
- The crop offset is searched on cell-aligned positions.
- Only the pane's rows **above** its last cell row are matched, so the row under test does not decide where the crop goes.
- Exactly one match is required.

Every pane runs a colour probe:
- truecolor background on row 0;
- indexed-256 backgrounds on the body rows;
- basic-SGR background on the last row;
- bright-white edge columns;
- a per-pane label;
- box, block and descender glyphs.

The session cwd is this git worktree, so the bar draws U+E0A0 under pane A. Every session snapshot was opened and the glyph was seen.

| # | Promise | Result | Metric(s) |
|---|---|---|---|
| 1 | `make test-vt-corpus` | PASS. 19/19 cases have 0 changed px, including rich-tui btop, lazygit, nvim, vicaya and vivecaka. 3/3 replay unit tests pass | corpus harness `pixel-report.json` (scratch) |
| 2a | F7: `border_style = "none"`, single pane, U+E0A0 drawn | Exact | pixel-r4-f7-none-single-A |
| 2b | F7: none, 2-pane split, pane A (and B) | Exact | pixel-r4-f7-none-split2-{A,B} |
| 2c | F7: none, 3-way chain A/B/C | Exact. Offsets are cells 0, 81 and 162 | pixel-r4-f7-none-chain3-{A,B,C} |
| 2d | F7: zoomed 90x30 (rounded config, split, A set to 90x29 and zoomed) | 90x30 frame, exact | pixel-r4-f7-zoom-90x30-A |
| 2e | **Negative control**: scratch build where `snapshot.rs` calls `render` (not committed, reverted) | **2a, 2b(A), 2c(A) and 2d FAIL**, 3 px each, all in the pane's last pixel row (y 455, or y 550 in the zoom cell), x 54-56: the top of U+E0A0 in bar column 6. Panes B and C and all bordered cells stay exact, as expected | scratch only |
| 3 | Cross-path with borders: single pane (rounded), 3-way chain (rounded) | Exact. Offsets are cells (1,1), (82,1) and (163,1) | pixel-r4-rounded-single-A, pixel-r4-rounded-chain3-{A,B,C} |
| 3b | `window.snapshot` equals `session.snapshot`, all 6 cells | 0 px | pixel-r4-*-window-vs-session |
| 4a | Raster A/B, main (466ffa0) vs 6f1ddab, `pane.snapshot` from the same bytes: vim with syntax colours on a Rust source, 100x30 | 0 px | pixel-r4-raster-ab-vim-syntax-pane-main-vs-6f1ddab |
| 4b | Same, dense fixture at 100x30 | 0 px | pixel-r4-raster-ab-fixture-pane-main-vs-6f1ddab |
| 4c | Same, vim on the fixture's plain-glyph twin | 0 px | pixel-r4-raster-ab-vim-glyphs-pane-main-vs-6f1ddab |
| 4d | Same, both panes of a rounded split running the dense fixture | 0 px | pixel-r4-raster-ab-split-pane-{L,R}-main-vs-6f1ddab |
| 4e | Session snapshot of that split | See *Raster A/B* below. 3 px differ, all in a border cell at the bar edge, none inside a pane | scratch only (non-zero by design) |
| 5 | `make check-vt-qa` | PASS on the committed tree at 6f1ddab. PASS again with this folder, its files marked intent-to-add so the tracked-file check could see them; that marking was then undone | — |
| 6 | Zero leaked daemons | Every daemon was stopped and confirmed gone by its pidfile pid. `ps` shows no audit binary, probe, vim or `sleep` child | — |

The dense fixture contains:
- light, heavy, double and rounded box grids, with vertical lines stacked over 6 rows;
- block and quadrant elements;
- powerline and nerd glyphs U+E0A0, E0B0, E0B2, E0B4, E0B6, F015, F07C, E7A8 and F113 on coloured backgrounds;
- tall glyphs (∫ ⌠ ⌡ ∑ √ ÅÉÍ) between red rows;
- bold, italic overhang, underline, curly coloured underline, strike, reverse, dim and overline;
- CJK, emoji (skin tone, flag, VS16, ZWJ) and combining marks.

## Raster A/B (region clip)
- **`render` is byte-for-byte main's path.**
  - With `clip = None`, `blit_glyph` has the same bounds test as main.
  - Every pane-only render (`pane.snapshot`, lens, gate, corpus harness) still calls `render`.
  - `pane.snapshot` from the same bytes is 0 px from main for vim, the dense fixture and both split panes (4a-4d).
  - All 19 committed corpus goldens pass at 0 px.
- **`render_regions` changes only what crosses a region edge.**
  - Split session, head vs main:
    - main's default frame is 120x36, so head was snapshotted at explicit `--cols 120 --rows 36`;
    - a scratch build with the clip removed, at that size, equals main at 0 px, so the frame logic matches and only the clip differs;
    - 3 px differ, all in border cell (row 34, col 6) at pixel row 18 of that cell: the top of the bar's U+E0A0, which main inks into the bottom border row;
    - 0 px differ inside a pane rect.
  - Head vs the clip-removed build at natural size gives the same 3 px, at the same place.
- **Line continuity.**
  - In a 4x magnified crop of pane L in the head session snapshot, every vertical stroke of the light, heavy, double and rounded grids is continuous across rows.
  - The pane crop equals its `pane.snapshot`, which is main's render.
  - The row-clip gaps from 84307b6 are gone.
- **Visible trade-off.**
  - In every `session.snapshot` and `window.snapshot`, the bar's U+E0A0 now loses its top 3 px at the bar's top edge, bordered or not.
  - Main painted those pixels over the pane's last row or over the border.
  - That is the price of cross-path equality (P3 below).
- **Where the clip applies.** `compose`'s only raster consumer is `snapshot.rs`; the `attach.rs` call is a test. Zoom passes a single region, the zoomed pane's rect. Pane rects never overlap.
  - Underline, strikethrough and cursor drawing stay inside their own cell.
  - Images are composited separately and are not affected.

## Earlier rounds (373dd97, kept for history)
- These cells were exact at 373dd97 and their metrics are still in this folder:
  - F1 chain, set-size and resize;
  - F2 nested slack (re-scoped as design: slack sits inside the pane's rect and is background only);
  - F3 1000x80;
  - F6 one-dim fits;
  - vim split.
- F4 and F5 were transcript checks.
- Round 4 re-ran the single, split, chain and zoom shapes. The other shapes have no region edge that round 4 changed.

## Testing matrix (round 4)
| Layer | Result |
|---|---|
| Unit | shux-raster 32/32, including `a_glyph_inks_only_its_own_region` and `box_drawing_ink_joins_the_row_above_without_regions`. shux-ui compose 9/9 |
| Integration | `make test-focus PKG=shux FILTER=snapshot` 25/25. `a_borderless_window_snapshot_draws_the_pane_as_its_pane_snapshot_does` passes, and **fails by 11 px when its window image is rendered with `render`** (reproduced with a scratch edit, then reverted) |
| Raw byte / replay | `make test-vt-corpus` (committed goldens, rich-TUI replays). Same-bytes A/B against main |
| shux automation | Real daemons and panes, colour probes, vim, `session`/`window`/`pane` snapshot |
| Visual inspection | Every session snapshot opened. Magnified crops of box joins and the bar edge, head and clip-removed |
| Pixel metrics | 45 committed metrics, all `pass` at 0/0 with 0 changed px (22 new in round 4) |
| Negative controls | Region clip removed: 4 F7 cells fail by 3 px. Borderless unit test: 11 px |
| DootSabha | Unavailable. See council-record.md and finding P2-b |

## Screenshot matrix
- No PNG is committed. Every comparison is cross-path (pane vs crop, window vs session) or main vs head, so there is no baseline to keep.
- The PNGs were generated in this audit, inspected, and deleted with the scratch output.

| Cell | Frame (cells) | Status |
|---|---|---|
| none-single | 80x25 | exact |
| none-split2 | 161x25 | exact |
| none-chain3 | 242x25 | exact |
| zoom-90x30 | 90x30 | exact |
| rounded-single | 82x27 | exact |
| rounded-chain3 | 244x27 | exact |
| A/B panes (vim x2, fixture, split L/R) | 100x30 / 80x24 | 0 px vs main |

## Findings
- **P2-a: the PR #230 description is stale.** It still says "a glyph's ink is clipped to its own cell row" and "ink outside a glyph's row is dropped". It should describe cef1182's region clip and the U+E0A0 trade-off.
  - To reproduce: `gh api repos/indrasvat/shux/pulls/230 --jq .body | grep -n Raster`.
- **P2-b: no implementation-diff review is recorded for cef1182.**
  - council-record.md covers rounds up to 373dd97.
  - `shux-simplify-architect` last reviewed 84307b6, and 195 lines in `crates/*.rs` have been added by cef1182 alone since then.
  - This gate's audit is not a substitute for either step. The implementing session must run them, or re-scope them in the PR.
  - Resolved after this audit: a `shux-simplify-architect` pass over 729b185..6f1ddab covered cef1182, with no P1. Its P2 coverage gap was fixed in 958981f (test only). The PR description was updated for P2-a.
- **P3: the bar's branch glyph U+E0A0 is 3 px shorter in session and window snapshots than in live attach and on main** (the top is clipped at the bar edge). This follows from the design. Cross-path equality requires it.
- Not a defect: `make check-vt-qa` passed at 6f1ddab while this report still described the 373dd97 row clip. The guard checks the evidence's shape, not whether it is stale.

## Residual risk
- No committed pixel baseline exists for this feature, and none was minted.
- Only vim is installed. nvim, btop, lazygit, vicaya and vivecaka are covered by the committed corpus replays only.
- No cell put a box-drawing glyph on a pane's first row beside a top border. The rule is the same as at the bar edge (ink is clipped at the region edge), and the classifier would count such pixels as border.
- `make lint` was not re-run here: disk was short.

## Cleanup
- Daemons: 8 cell daemons (head n/r/ab/sp, clip-removed n/r/sp, main ab), plus a trial run and two aborted head runs. Each was stopped with `shux daemon stop` and its pidfile pid was confirmed dead.
- No audit process remains in `ps`.
- The runtime dirs were removed.
- The scratch `snapshot.rs` and test edits were reverted. `git status` shows only this folder.
