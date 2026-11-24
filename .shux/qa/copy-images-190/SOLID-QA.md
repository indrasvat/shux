VERDICT: PASS

# copy-images-190: VT + TUI gate (one run, both charters)

- Change: #190 PR B2 (copy mode frozen on the shown slice; pictures cropped to the shown cells; SIGTERM on a stalled client terminal)
- Branch `feat/190-b2-copy-images`, base `0764a91` (origin/main after the rebase), commit_under_audit `1ae8b433bf837a26998d43bf9d39c5ebfdc243a6` (code identical to `b46d321`; the three commits after it add only QA evidence)
- Tier: full. Full audit ran at pre-rebase `4c2894b` on base `6bc9d94`; re-stamped after a conflict-free rebase onto `0764a91` (see *Re-stamp after rebase*). Base binary from the `make vt-neutrality` cache at 6bc9d94. Binaries copied outside `target/`.

## Re-stamp after rebase (onto 0764a91, which brings #302's erase and wrap-link changes under copy mode)

- `make check-vt-qa-pregate` at 1ae8b43: pass. `make vt-neutrality` vs 0764a91: 0/0, refreshed in 1ae8b43 (cited).
- Sanity on a binary rebuilt with `make release` at 1ae8b43 (`git_sha` 1ae8b43):
  - **Copy mode:** entered over the 200x60 pinned pane while output streamed, under a 90x28 client. It opened on the slice the client was showing (`L108`), and that view stayed byte-identical after ten more streamed lines.
  - **Search:** `/R100` panned to column 150. `/漢字050` panned back to the match's first cell.
  - **Yank:** the OSC 52 decoded to `😀x040 end`.
  - **Left-cropped picture:** the bytes are unchanged (`CUP 6;2 a=p,…,x=27,w=81,c=9,r=2`, then the restore at `CUP 6;72` with no `x=`).
  - **Pixels:** all 8 committed metrics were regenerated at 1ae8b43 at 90x28 and 120x40, all 0/0. They are byte-identical to the pre-rebase ones. Base vs head crop still FAILs (3078 px).
- Old → new SHAs (same 18 commits, same order): base 6bc9d94→0764a91 · fbd4da5→b79f897 · e83b6a9→e8d991e · 080ab08→e07886c · e3d0c62→30ef9a5 · 68b1f13→6dabcf9 · 9567497→d3ea02b · a330fbb→cb7e2f0 · 91e99bb→77f4734 · 976a100→d8a9e55 · 03de873→60acc31 · 85b2d27→137b3e4 · 4495b3b→2086c32 · cb5626a→7a138e0 · 3e296b1→b4ad528 · a39fbf6→b46d321 · 4c2894b→4a9016c (neutrality) · 62304ff→620c6dc (this PASS) · new 1ae8b43 (neutrality refresh).
- Pre-rebase SHAs below are kept as they were audited; read them through this map.
- Isolation: inner daemons `/tmp/q190h` (head) and `/tmp/q190a` (base); attach clients hosted as pane programs in an outer daemon `/tmp/q190o`. Private `XDG_STATE_HOME`/`XDG_CONFIG_HOME`/`HOME`/`HTOPRC` under `/tmp/q190b`; lazygit ran in a scratch repo.

## Scripted half (cited, not re-run)

| Item | Result | Evidence |
|---|---|---|
| `make vt-neutrality` | 0/0 on all six layers, against 0764a91, refreshed in 1ae8b43 | `neutrality.json`, `neutrality/` |
| `make check` | pass (implementer) | cited |
| `make test-gui-terminal` | pass: real kitty under Xvfb, crop, restore, mid-pixel `X=` at font 20, containment | cited; `docs/agents/visual-testing.md` |
| `make check-vt-qa-pregate` | first run exit 2 (no manifest yet, expected); re-run with this scope staged before commit | this commit |

## Councils and review budget

- Design council: `council-design.md` (copy of the synthesis the implementation followed).
- Implementation council: `council-implementation.json`.
- Closure review (round 4 of 5, read-only, `a330fbb..cb5626a`): items 1, 2, 6-10 CLOSED. Items 3 (stale stamped drag after eviction), 4 (wheel tick swallowed when copy mode stops mapping mid-route) and 5 (wide glyph blanked at a pan edge inside the selection) were NOT CLOSED there and were fixed in `3e296b1` and `a39fbf6`. Item 1's Linux-hang note was answered by making the SIGTERM test macOS-only (`3e296b1`). No further review round; this gate saw those fixes on the real binary only through the flows below.

## Testing matrix

| Layer | Status | Evidence |
|---|---|---|
| Unit / integration | PASS (cited `make check`) | implementer run |
| Raw bytes | PASS | recorded client bytes: kitty `a=p` crop/restore, OSC 52 yanks (below) |
| Shux automation | PASS | real attach clients via PTY at 90x28, 100x30 and 120x40 over panes pinned at 160x50, 200x40, 200x60 |
| Visual inspection | PASS | PNGs opened, list below |
| Pixel | PASS | 8 metrics, all 0/0 |
| DootSabha design / diff | PASS | council records above |

## What was broken on the new surface (head 4c2894b vs base 6bc9d94)

Copy mode, pane pinned 200x60 under a 90x28 client, output streaming at 5 lines/s:
- Entry froze the exact slice the client was shown (`L108..S00010`). Ten more stream lines later the view was byte-identical (`head-copy-1-entered.txt` vs `head-copy-2-held.txt`). Base opened at `S00059` instead and drifted to `S00069`.
- `/R100` panned to column 150 and revealed rows above the entry slice; base found nothing. `/漢字050` panned back left to the match's first cell; base landed on row 109. `?😀x040` (backward, already visible) did not pan.
- `v` + 9×`l` + `y` from the emoji hit: OSC 52 decoded to `😀x040 end`, which is exactly the selected cells. Base emitted no OSC 52.
- Wheel entry (3 ticks) opened 9 rows above the shown top (`L116` to `L107`). 6 ticks back down exited to live.
- Client resize 90x28 to 120x40 while modal ended copy mode cleanly. Zoom of a reflowing split while modal kept copy mode; `/TRUE077` + `v` + 6×`l` + `y` yanked `TRUE077`. Focus change (prefix `k`) ended copy mode in the old pane. After `q`, typed text reached the app (`head-life-4-typed.png`).

Pictures, pane pinned 200x40, a 12-band picture at column 70, cursor walked to the far column and home:
- 90x28 (slice offset 73) and 120x40 (offset 73): the picture is cropped by exactly 3 cells. Bands 4-12 are shown, with nothing on the border. Base deleted it (`a=d`) and re-sent the whole payload on restore.
- Head bytes: `CUP 6;2` `a=p,i=2000000000,…,x=27,y=0,w=81,h=38,c=9,r=2` with no payload; restore `CUP 6;72` `a=p` with the same id, no `x=` and no payload.
- Live attach (the client's bytes drawn by the outer shux) vs inner `window snapshot`: 0/0 at both sizes and in all three phases. Restore vs the first live frame: 0/0. The status row is excluded: its uptime clock differs between the two paths (132 px, bbox in the status bar only). The comparator FAILs base vs head crop (3078 px), so it can fail.

Rich TUIs (vim with syntax, btop, htop, lazygit) at 90x28 and 140x42, each beside a truecolor + indexed + basic + background probe pane (btop alone, since it needs 80x24): all render correctly. btop's cpu box stops at column 80 in an 88-column pane on base and head alike. vim and htop also ran pinned 200x60 under a 90x28 client and showed the cursor-following slice.

## Findings

None at P0-P2.
- P3 (not B2): the snapshot rasterizer draws CJK as tofu in PNGs (known renderer-v2 gap). CJK was judged from text captures.
- P3 (not B2): the centred `[copy mode]` toast outlives copy mode by its toast timeout. The `Y` flag is what tracks the mode, and it does so correctly.

## Not covered here (residual risk)

- Modal drag-release yank and eviction mid-gesture (closure items 3 and 4): covered by unit/integration tests only, not driven live.
- Mid-cell `X=` crop: proven by the GUI kitty suite (cited), not by this rig.

## Cleanup

All sessions killed; `shux daemon stop` on the three runtime dirs; no `shux-head`/`shux-base`, workload or TUI process left (`ps` scan for `/tmp/q190b`).
