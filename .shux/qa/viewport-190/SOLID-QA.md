VERDICT: PASS

commit_under_audit: 4e2d942b68fedb03bbc08a963dad13cd542104c8
The full audit ran at 417e070. A delta re-audit of 417e070..4e2d942 follows
in the last section.
merge_base: f1478e4b1e5dcc163aaeb635069dec0b901790d4
branch: feat/190-viewport. Issue #190, PR B1 "viewport core". Full tier.
Charters: shux-vt-solid-qa and shux-tui-qa, run once by one agent.

## Change under audit

Each attach client lays out panes at its own size. A per-pane `PaneViewport`
crops or fills the presented grid. It covers the hidden-cursor latch, cursor
mapping on both axes, wide-glyph edge clipping, surplus fill, size labels, the
viewer role in the status bar, two-axis mouse and selection mapping, driver
takeover by click, and snapshot sizes that default to the last driver's size.

## Scripted half (cited, not re-run)

- `make vt-neutrality BASE=f1478e4 SCOPE=viewport-190`: `neutrality.json`
  head 417e070, clean tree. Every layer passed (colour-probe 9, corpus-ab 19,
  corpus-goldens 19, fuzz 1, leaks 2, live-ab 15), with 54 metrics at 0/0.
- `make check`: 3296/3296 at 417e070 (the implementer's run).
- `make test-gui-terminal`, two-kitty-window check:
  `.shux/out/viewport-190/green/both.png` inspected. It shows a 160-wide driver
  and a smaller viewer over one split. The viewer's borders carry
  `shux-gui-rig ⤡ 54×33` and `env ⤡ 53×33`, and its status bar says
  `◉ viewing`. The red run's screenshots sit beside it.

## Live attack: two real clients, independent of the implementer

- Rig: an inner daemon of the branch build, `/tmp/b1-bin/shux` (417e070),
  runtime `/tmp/qb1a`, session `t190`. Window 1 splits a colour probe from
  `vim -u NONE`. Window 2 runs htop, window 3 btop, window 4 lazygit. Every
  program used a private `XDG_CONFIG_HOME`. Two real `shux session attach`
  clients ran as panes of an outer pinned v0.58.0 daemon on `/tmp/qb1o`, at
  160x48 (o1) and 90x28 (o2), plus a third client for the `--plain` check.
- Scratch evidence: `.shux/out/viewport-190/qa/`. Every PNG below was opened at
  full resolution.

| # | Area | Result | Evidence |
|---|---|---|---|
| 1 | Crop labels and viewer role (o1 drives) | PASS. The viewer shows `probe ⤡ 158×22` and `vim ⤡ 158×22`. Its status bar reads `◉ viewing · type to drive`, and the driver's has no role segment. | a01-o2-initial.png, a01-o1-initial.txt |
| 2 | vim cursor-follow on both axes | PASS. With `3G$` and `5G0150|`, the slice follows the column and the cursor shows on the last visible cell. | a02-o2-vim-eol.png, a11-o2-vim-coloff.png |
| 3 | Wide CJK and emoji at the crop edges | PASS. A leading orphan half is drawn as a space and a trailing cut glyph is not drawn, in text and in pixels. The CJK tofu in the PNGs comes from the outer v0.58 rasterizer's font, not from this diff: text captures show 中. | a02/a11 .txt + .png |
| 4 | htop and btop latch at top-left | PASS. Both header rows are visible at offset (0,0), and btop's box is drawn from its top-left. | a03-o2-htop.png, a05-o2-btop.png |
| 5 | Takeover by typing, surplus fill | PASS. When o2 types, o1 shows `⤡ 88×12` with themed surplus, and its role flips to viewing. | a06-o1-after-takeover.png |
| 6 | App click in a cropped pane with takeover | PASS. A click on the screen cell of `E` in `END1` (o2, col 42, row 16) put vim's cursor on `<E> 69` (`ga`), with no visual-mode drag. The click made o2 the driver. | outer pane captures in this transcript; a09-o2-cropped-before-click.txt |
| 7 | Drag-select and yank with a column offset | PASS. The OSC 52 payload `中中中中中END2` matches the shown cells, cols 70..84 (`a12-o2-yank.raw` decoded). The highlight sits on those cells. | a12-o2-drag-held.png, a12-o2-yank.raw |
| 8 | Theme hot-reload while cropped | PASS. `status_bg` `#6a0dad` and `border_unfocused` `#ff8800` reached the status bar, the borders, the labels and the surplus fill without a restart. | a13-o2-theme.png, a14-o1-surplus-theme.png |
| 9 | Size label in the status bar when zoomed | PASS. The viewer's status bar shows `⤡ 90×27`, and the driver shows no label. | a15-o1-zoom.txt |
| 10 | Status-bar width ladder | PASS. 70 `◉ viewing · type to drive` → 56 `◉ viewing` → 48 `◉`. `--plain` gives `* viewing · type to drive` → `* viewing` → `*`. | a07, a08 |
| 11 | lazygit at both sizes | PASS. Driven at 90x28, o1 shows surplus with `⤡ 88×25`. Driven at 160x48, o2 is latched at top-left with `⤡ 158×45`. | a16-o1-lazygit.png, a17-o2-lazygit-cropped.txt |
| 12 | Default size of window and session snapshots | PASS. The default equals the explicit `--cols 90 --rows 28` of the driver: 810x532, 0/0 for both commands. A size-mismatch control fails with 15390 px changed, which shows the comparator can see a difference. | pixel-window-…json, pixel-session-…json |

Colour probes: every client capture carries the truecolor, 256-colour and basic
SGR line from `probe.sh`, plus vim, htop, btop and lazygit in colour.

## Findings

No P0, P1 or P2 findings.

- P3: at 40 columns a viewer's role segment disappears entirely, while at 32
  and 26 columns it shows again. The window label keeps priority at 40 and is
  shed below it, so the ladder is not monotonic across widths. This is as
  documented ("then the role"), and it costs no correctness. Reproduce: attach
  a viewer, then `pane set-size --cols 40` on its host and read the last line.
- P3, a note rather than a defect: a client attached with `NO_COLOR=1` still
  draws `◉`. Only `--plain` gives `*`, which is what the code and design say
  ("a plain client"). Under `--plain`, `◉ N attached` (#287) still uses `◉`.

The implementer's P2 list in `/tmp/b1-fnf.md` (eight items from the closure
round) was not re-litigated here.

## Not driven live (covered by named tests in the diff)

- Wheel in a cropped pane:
  `the_wheel_finds_no_history_above_a_tall_grid_shown_from_its_top`.
- vim's brief hides:
  `the_latch_moves_the_slice_only_after_the_cursor_stays_hidden` and
  `a_hidden_cursor_is_stamped_by_the_batch_that_hid_it`. Live, vim's slice
  followed its cursor across every redraw with no jump.
- Cursor hidden outside the slice:
  `the_cursor_is_mapped_through_both_offsets_and_hidden_outside`.
- Snapshot size after the last voter detaches:
  `a_window_snapshot_defaults_to_the_last_driver_size`.

## Cleanup

Sessions `o1`, `o2`, `o3` and `t190` were killed. `daemon stop` stopped PIDs
49448 (`/tmp/qb1a`) and 50441 (`/tmp/qb1o`), and `kill -0` confirms both are
dead. Both pidfiles are gone, and `ps` shows no process whose argv names
`qb1cfg`, `qb1a` or `qb1o`. The real `~/.config/htop/htoprc` is untouched
(mtime Oct 31).

## Delta re-audit: 417e070..4e2d942

The delta is one product change, 4e2d942 "fix(attach): a departed client takes
its mouse-release waiters with it". Attach teardown now drops the departing
client's `(pane, client)` entries from `PaneIoState.mouse_release_written`.
The commit adds the test `a_departed_client_leaves_no_release_waiter`.

- **The fix closes the leak.** Waiters are inserted only in
  `forward_bytes_to_pane`, and only with `Some(client)`. After the input loop
  ends, nothing can insert one for this client. Teardown's
  `release_app_gesture(…, None)` runs after the `retain` but passes `None`, so
  it adds no waiter. There is no early return between registration and
  teardown. The per-pane `retain` on pane exit and respawn is unchanged.
- **Red without the fix.** I removed only the teardown `retain` in a throwaway
  worktree of 4e2d942, keeping the test, and ran `make test-focus`. It fails
  with "a departed client left a release waiter behind". The worktree has been
  removed.
- **Green.** `make test-focus PKG=shux` on the departed-client, takeover,
  driver-leaves, detach, release and gesture tests: 55 run, 55 passed. Log:
  /tmp/qb1cfg/delta-green.log.
- **Neutrality at 4e2d942** (`neutrality.json`, rewritten by the implementer's
  fresh run): head 4e2d942, clean tree, every layer passed, 54 metrics at 0/0.
  No metric file changed.
- **Live, rebuilt binary at 4e2d942.** One pane runs a mouse probe that logs
  each SGR report with the terminal size at read time. Two real clients attach,
  at 160x48 and 90x28.
  - Takeover click from the cropped viewer: press `<0;81;29M` and release
    `<0;81;29m` land on the same cell, both at 158x45, with offsets (70,20).
    After that the viewer drives.
  - Driver leaves mid-drag: press `<0;19;7M`, then drag `<32;24;7M`, then the
    driver's client is killed. The app receives release `<0;24;7m` at the last
    cell, and the remaining client takes the panes at 158x45.
  - A third client attaches after that departure and takes over by click,
    pressing and releasing on one cell.
  - A viewer's click within 1 s of the driver's own input does not take over.
    It is reproduced with an immediate click (held) and a 1.5 s gap (taken).
    This is the documented `SETTLE` hold in `clients.rs`, not a defect.
  - One takeover's release was read after the app's SIGWINCH (88x25), on the
    same cell as its press. This is the known P2 in the PR's list, not new.
- **Cleanup.** Sessions are killed, and `daemon stop` ended PIDs 41973 and
  42025. Both pidfiles are gone and no `qb1` processes remain.

No new findings. The P3 notes above are unchanged.
