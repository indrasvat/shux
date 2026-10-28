VERDICT: PASS

# shux-vt-solid-qa — border junctions (issue #203)

## Change under audit

| | |
|---|---|
| Issue | #203: `compute_borders` drew `┼` where only three border lines meet (two panes on top, one wide pane below) |
| Branch | `claude/project-thread-hv5sei` |
| Commits | `408c063` fix + `d13db01` review follow-up; audited head `d13db01` |
| Base | `0ea4881` (`origin/main`) |
| Diff | `crates/shux-ui/src/borders.rs` (+57/-101), `crates/shux-ui/tests/border_junctions.rs` (new, 472 lines) |
| Why this gate | `scripts/check-vt-qa.sh` lists `crates/shux-ui/src/borders.rs` as a VT rendering path: its glyphs reach `shux-raster` via `composed.rs` |

The diff replaces "vertical? horizontal? on the viewport edge?" flags with per-cell
arms (up/down/left/right) recorded while drawing each pane edge and the outline as
clipped lines, and picks the glyph from the arms. No other file changes.

Binaries were built for this audit from their own worktrees and target dirs and
pinned by hash: fix `d13db01` sha256 `070c0a1d5133d672…`, base `0ea4881` sha256
`543246a1defad55c…`.

## Stated DoD matrix

Issue #203 states one criterion: where three lines meet, draw the tee whose arms
match the lines. Derived from what the diff touches: four-way junctions still draw
`┼`; nothing else on screen moves; focus colouring unchanged; live attach and
`window snapshot` agree.

| Criterion | Status | Evidence |
|---|---|---|
| Two-over-one junction is `┴` (each style) | PASS | text + pixels, 4 drawn styles x 3 sizes; `pixel-fixbase-masked-*-w1.json` |
| `├` for left \| (top/bottom) | PASS | 4 styles x 3 sizes; `pixel-fixbase-masked-thin-120x40-tee.json` |
| Aligned 2x2 keeps `┼`, pixel-identical to base | PASS | `pixel-fixbase-unmasked-thin-120x40-grid.json`; 16/16 grid cases unmasked-identical |
| Nothing outside junction cells changes | PASS | 52/52 masked fix-vs-base exact; 20k-layout A/B |
| Junction keeps its colour (focus) | PASS | brightest-pixel ink fix == base in all 49 cases that have a junction (the 3 `none` cases have none); A/B: focus flags identical in all 20 000 layouts |
| attach == window snapshot | PASS | 52/52 fix and 52/52 base, exact; `pixel-xpath-*.json` |
| ascii / none styles untouched | PASS | `pixel-fixbase-unmasked-{ascii,none}-120x40-w1.json`; 6/6 cases unmasked-identical |

## Testing matrix

| Layer | Status | Evidence |
|---|---|---|
| Unit | PASS | `make test` on the fix: 2486 passed, 2 skipped, exit 0, under `no_leak_guard`. The existing `borders.rs` unit tests are in that run. `make lint` exit 0 |
| Integration, red-first | PASS | `tests/border_junctions.rs` copied onto the base worktree: 0/5 pass, and every failure is `┼` where a tee is due (e.g. `left: Some('┼') right: Some('┴')`; `'┼' at (20,6) draws a line down that nothing continues`; `attach, Thin: left '┼' right '┴'`). 5/5 pass on the fix |
| Old vs new A/B, randomized | PASS | I re-ran the earlier adversarial probe on `d13db01` (the first run was on `408c063`, before the refactor): 20 000 random layouts at random offsets and sizes, in 5 styles with random focus. Segment positions and focus were identical in every case. Glyphs changed only at cells the old code drew as a cross, apart from 2734 cells (569 of them ascii), all in degenerate layouts with a 0-width or 0-height pane. Layouts with an arm that no line continues: 10 943 before, 0 after, and 0 got worse |
| Raw byte / replay | PASS | The `attach_and_snapshot_draw_the_tee` integration test feeds the compositor's emitted bytes through a `VirtualTerminal`. The committed rich-TUI corpus (`nvim`, `lazygit`, `btop`, `vicaya`, `vivecaka` `.raw`) was replayed into panes of both junction layouts on fix and base, thin + double (4 cases), and every pixel and text check passed |
| shux automation | PASS | 32 runs, each a pair of real daemons: vim `syntax on` plus colour-probe panes (truecolor 38;2, indexed 38;5, basic 31, bg 42) in three layouts. Config states: thin/thick/double/rounded/ascii/none/default/malformed (`"bogus"`, which falls back to rounded) at 120x40, and thin/thick/double/rounded at 80x24 and 200x60. All 32 passed on the first attempt |
| Independent terminal | PASS | tmux draws the attach stream with `┴`/`╩` at the junction (fix); `tw1_tmux.txt` |
| Visual inspection | PASS | Full-frame PNGs and 4–5x junction zooms opened and inspected for thin/thick/double/rounded/ascii, all three layouts, and the TUI replay. Base shows a dangling arm and the fix shows the tee. No clipping, colour bleed, tofu or drift |
| Pixel comparison | PASS | 17 committed metrics, all exact 0/0 `pass`; 208 scratch metrics in `.shux/out/border-junctions/metrics/` |
| Design council | PASS (substituted) | `council-record.md`: dootsabha is unavailable here, so three parallel agents ran on disjoint surfaces |
| Diff council | PASS (substituted) | `council-record.md`; this gate re-ran the A/B on the final SHA |

## Screenshot matrix

There is no committed baseline for border chrome, so no PNG is committed and every
metric stands alone. Scratch lives under `.shux/out/border-junctions/`.

| Viewport | Workload | Comparison | Metric | Status |
|---|---|---|---|---|
| 120x40 | vim + probes, issue layout | attach vs window snapshot: thin, thick, double, rounded, ascii | `pixel-xpath-<style>-120x40-w1.json` | pass 0/0 |
| 80x24, 200x60 | same | attach vs window snapshot, thin | `pixel-xpath-thin-{80x24,200x60}-w1.json` | pass 0/0 |
| 120x40 | nvim/lazygit/btop replay | attach vs window snapshot | `pixel-xpath-tui-thin-120x40-tw1.json` | pass 0/0 |
| 120x40 | issue layout | fix vs base, junction cell + 1px halo masked: thin, thick, double, rounded | `pixel-fixbase-masked-<style>-120x40-w1.json` | pass 0/0 |
| 120x40 | left \| (top/bottom) | fix vs base, masked | `pixel-fixbase-masked-thin-120x40-tee.json` | pass 0/0 |
| 120x40 | TUI replay, issue layout | fix vs base, masked | `pixel-fixbase-masked-tui-thin-120x40-tw1.json` | pass 0/0 |
| 120x40 | aligned 2x2 / ascii / none | fix vs base, UNMASKED | `pixel-fixbase-unmasked-*.json` | pass 0/0 |

The junction cells are located from geometry alone: separator columns come from the
outline's top/bottom tees and separator rows from its left/right tees, read off the
base text and intersected. Each masked cell is also checked as text: fix `┴`/`├`
(and the thick, double and rounded equivalents), base `┼`/`╋`/`╬`.

### Proof the comparators can fail

- The 28 changed cases compared UNMASKED fail, as required. The ┴/┼ difference is 18 px.
- Shifting the mask one cell right fails.
- Fix attach against base snapshot fails.
- A blank frame fails at ratio 0.07.
- A missing file fails with `actual_missing`, and mismatched sizes fail with `size_mismatch`.

## Findings

No P0, P1 or P2 findings in the change. Two P3 items, plus fixes to my own analyzer:

- **P3 (observation, pre-existing, not this diff):** box glyphs ink one pixel past
  their cell. Base `┼`'s left arm lights the last pixel column of the cell to its
  left. Removing that arm clears 2–5 px there, so a mask confined to the cell
  reports a "diff outside the junction" on tee layouts. Reproduce: diff the
  masked `thin_120x40` tee pair with no halo; the diff is at px x=539 (cell 59,
  column 8 of 9). This is a consequence of the removed arm, not of the diff
  touching anything else. The mask carries a 1px halo, and that is documented.
- **P3 (behaviour in degenerate layouts):** where a pane has 0 width or height,
  glyphs also change at cells the old code did not draw as a cross (2734 cells over
  20k layouts, ascii included). In every such case the new glyphs join (0
  dangling arms) and the old ones did not, so this is an improvement and not a
  regression.
- I fixed three defects in my own analyzer before relying on it:
  - `pane capture` defaults to 50 lines, which truncated the 200x60 text. I now pass `--lines $ROWS`.
  - My "most common pixel" junction-colour measure depended on glyph shape. I replaced it with the brightest pixel.
  - The masked tee pairs failed until the mask got its 1px halo (the P3 above).

  The whole matrix was re-captured with the corrected scripts. Every number above
  comes from that second run.

## Residual risk

- kitty and an X GUI terminal are not installed, so `make test-gui-terminal` could
  not run. This diff changes which character is emitted and not where, so cursor
  positioning and framing are untouched. tmux, as an independent terminal, renders
  the attach stream with the tee.
- The rich-TUI replays were recorded at 120x36 and play into smaller panes, so their
  content is chopped. That is identical in base and fix (the masked compare is
  exact), and the replays exercise composition only, not app fidelity.
- Hot-reload of `border_style` was not exercised. The diff does not touch config.

## Cleanup

Each run used two daemons on unique short `XDG_RUNTIME_DIR`s (`/tmp/vi.*`, `/tmp/vo.*`,
`/tmp/ti.*`, `/tmp/to.*`) under `.shux/scripts/no_leak_guard.sh`. Each run stopped
its daemons and asserted zero survivors with a per-run unique needle (its runtime
dirs and a unique `sleep` duration). There were 0 `LEAK` markers across the 36
runs. The final `ps` scan found no process referencing the pinned binaries or
runtime dirs, and no runtime dirs and no tmux server remain.
