VERDICT: PASS

# shux-tui-qa: attach viewers (#287)

- Change: issue indrasvat/shux#287, branch `fix/287-attach-viewers`, diff `d36b738..fcbf780`
  (commit_under_audit `fcbf780c65fd4259935ecf47534f45014091287d`). No PR exists yet.
- Binary: `/tmp/s287-bin/shux` = `shux 0.57.3 (fcbf780)`, release build of HEAD, not rebuilt.
- VT side: trivial tier, cited and not re-run: `.shux/qa/attach-viewers-287/SOLID-QA.md` and
  `neutrality.json` (base d36b738 → head 237778b, 0/0 on every layer; the only VT path
  touched is `crates/shux-ui/src/statusbar.rs`, a `group` field).
- `make check` green at HEAD per the author; not re-run (gate charter).
- Rig: daemon A `/tmp/q287a` holds the watched sessions; daemon B `/tmp/q287b` runs every
  viewer as a real `shux session attach` inside its own pane, so each viewer is snapshotted
  as it is drawn. All under `env -u SHUX -u SHUX_PANE_ID -u SHUX_SOCKET`, `TERM=xterm-256color`,
  `COLORTERM=truecolor`.

## DoD matrix (issue #287 "Done when")

| # | Criterion | Status | Evidence |
|---|---|---|---|
| 1 | Stuck viewer dropped ~10 s after last progress; 1 KiB/s viewer kept >60 s; paused-clock tests | PASS | Live: SIGSTOPped viewer released 20.9 s after SIGSTOP (socket buffer fill plus 10 s stall), `client.disconnected reason=connection_lost`; a viewer duty-cycled 4 s stopped / 0.3 s running stayed attached for all 44 s. Paused-clock tests `write_progress::a_slow_reader_gets_the_whole_frame` (64 KiB at 1 KiB/s = 64 s) and `a_reader_that_stops_is_dropped`, plus `attach::a_client_that_stops_reading_is_released`. The red-first run is the author's claim (commit b4b655d) and was not re-run here |
| 2 | `shux client list` / `client.list` show every viewer | PASS | text (TTY), plain and json all list 3, then 4 viewers with the right sizes. Read-only marking waits on #191, which has not landed |
| 3 | `client detach` removes exactly one, tells it why, others keep rendering; unknown/ambiguous exit 1 with a clear message | PASS | 8-char prefix and full id: the removed viewer prints `[removed from session 'work' by \`shux client detach\`]`, its terminal is restored (`icanon echo` before and after, `tput cols` 60), and the other viewers drop to "3 attached" then "2 attached". Unknown id, a re-used id, a 2-hex prefix and a non-hex id all exit 1 with clear messages. The ambiguous case cannot be set up live (it needs two random UUIDs sharing 4 hex digits) and is covered only by `rpc::client::tests::list_names_each_client_and_detach_takes_a_unique_prefix` |
| 4 | "◉ N attached" for N≥2, under a full bar; nothing for one viewer | PASS | N=2,3,4 on every viewer at 120/100/90/80/60/40 cols. A full bar sheds its script segments (starship, clock) and keeps the count. A 57-char session name on a 40-col bar is shortened to `an-extremely-long-sess…` and the count stays. A lone viewer shows no count. A passing notice (`[zoom toggled]`) takes the 40-col bar whole, then the count comes back |
| 5 | `events` connect/disconnect with reasons; `session list` attached | PASS | `events.history`: `client.connected` ×5 and `client.disconnected` with `removed` ×2 and `connection_lost` ×1. `session list` text shows `1 window · 4 attached`; json shows `"attached": 2` |
| 6 | Real-binary e2e with three viewers; before/after evidence attached to PR | PASS for the e2e run (this audit); PR attachment PENDING | No PR exists yet. Attaching the evidence is the author's job when the PR is opened |

## Test matrix

| Layer | Status | Evidence |
|---|---|---|
| Unit / paused-clock | cited | test names above; `make check` green per the author |
| Integration / CLI | PASS | `client list` in all 3 formats; `client detach` with full id, prefix, unknown, short and garbage ids |
| shux automation, isolated runtime | PASS | `/tmp/q287a` and `/tmp/q287b`, both stopped with `shux daemon stop` |
| Real workloads | PASS | bash with colour probes, `ls -G`, htop (`HTOPRC` private), btop (`XDG_CONFIG_HOME` private), `vim -u NONE` with syntax, all while viewers attach and leave |
| Colour probes | PASS | truecolor orange, indexed 45 cyan, basic red, green background in s1_*.png; htop, btop and vim syntax colours intact |
| pane.capture text | PASS | `*.txt` beside each PNG |
| Snapshots, inspected at full resolution | PASS | s1_v1/v2/v3, s2_v4_removed, s2_cli, s4_v1_htop, s4_v1_btop, s4_v6_btop, s4_v6_vim, s4_n1_longname, s4_n1_notice, s5_v7_btop |
| Pixel comparison | N/A | No committed baseline exists for attach chrome, and VT neutrality covers the rendering path at 0/0. No expected PNG was minted |
| Process cleanup | PASS | see Cleanup |
| External review | per the author | review frozen; not part of this gate |
| `make check-tui-qa` | PASS | `✓ No general TUI QA manifests found` |

## Screenshot matrix (`.shux/out/attach-viewers-287/`, gitignored)

| Viewport | Workload | PNG | Status |
|---|---|---|---|
| 120x40 solo | bash colour probe | s1_v1_solo_120x40.png | no count, PASS |
| 120x40 / 80x24 / 40x20 | 3 viewers | s1_v1_3att_120x40.png, s1_v2_3att_80x24.png, s1_v3_3att_40x20.png | "◉ 3 attached", PASS |
| 60x20 | removed viewer's shell | s2_v4_removed_60x20.png | reason text, terminal restored, PASS |
| 130x40 | CLI list/detach/errors | s2_cli_list_detach_130x40.png | coloured text format, PASS |
| 120x40, 40x20 | after removals | s2_v1_2att_120x40.png, s2_v3_2att_40x20.png | "◉ 2 attached", PASS |
| 40x12 / 80x24 | long session name | s4_n1_longname_40x12.png, s4_n2_longname_80x24.png | name shortened with "…", count kept, PASS |
| 40x12 | passing notice | s4_n1_notice_40x12.png, s4_n1_after_notice_40x12.png | notice whole, then the count returns, PASS |
| 120x40 / 100x30 | htop, 2 viewers | s4_v1_htop_2att_120x40.png, s4_v5_htop_2att_100x30.png | PASS |
| 120x40 / 90x30 / 100x32 | btop, 2 viewers | s4_v*_btop_*.png, s5_v7_btop_2att_100x32.png | PASS; see the resize note under Residual risk |
| 120x40 / 90x30 | vim, 2 viewers then solo | s4_v1_vim_2att_120x40.png, s4_v6_vim_2att_90x30.png, s4_v1_vim_solo_120x40.png | PASS |
| 98x30, no viewer | btop resize control | s6_ctl_btop_noviewer_98x30.png | control, see Residual risk |

## Findings

- P0, P1: none.
- **P2: a released stalled viewer gets a framing error, not a lost-connection notice.**
  When a SIGSTOPped viewer resumes after being released, it prints
  `[attach error: framing error: bytes remaining on stream]` and exits 0
  (`s3_v3_after_stall.txt`). The daemon cuts the connection mid-frame, so the client
  hits EOF inside a length-delimited frame and reports `ExitReason::Error` instead of
  `ConnectionLost` (`crates/shux/src/dispatch.rs` ~L118–123). This is the normal ending
  for the new release path (a laptop lid reopened), and the message names a codec
  detail rather than the cause. Under the review budget a P2 belongs in the PR's
  "Found, not fixed" list unless the author chooses to fix it.
- P3: `--format plain session list` prints no attached column, while text and json do.
  This predates the diff (the plain columns are unchanged).

## Passed evidence

The text above, plus `client-list.json` (3 viewers, distinct sizes and session names) and
the event stream quoted in DoD #5.

## Residual risk

- **btop leaves stale frame pieces after a resize.** btop redraws only part of its frame,
  so pieces of the old layout stay on screen (a doubled `mem`/`disks`/`proc` header, a
  stale `02:4` clock). A control with no viewer, where the pane is only resized by
  `pane set-size` (s6), shows the same thing, so this diff did not cause it.
- **Pane size follows the most recent resize, not the viewers present.** A 120-col viewer
  draws a pane sized by a smaller viewer. This rule predates the diff, and #190 owns it.
- The ambiguous-prefix error was verified by unit test only (see DoD #3).
- Red-first for the stall tests is cited from the author's commit, not re-observed.
- The worktree has an uncommitted `fuzz/Cargo.lock` change that this gate did not make.
  It probably comes from the neutrality fuzz build. Check it before committing.

## Cleanup

- Daemon A pid 4109 and daemon B pid 4186, from their pidfiles: `shux daemon stop` rc=0 for each.
- All 8 descendants recorded before the stop (shells, attach viewers, TUIs) are gone.
  No `/tmp/s287-bin/shux` or `q287` process remains, and no orphaned htop, btop or vim
  (checked by pid; no `pkill`/`pgrep -f`).
- htop and btop wrote only their private configs under `/tmp/q287b/cfg`.
- One stray `tail` from my own mistyped harness line (pid 18776, reading stdin) was killed.

## Delta after this verdict

The P2 above (a released viewer that resumes printed `[attach error: framing
error: bytes remaining on stream]`) is fixed in the next commit: a read error
after the handshake is a lost connection unless the data is malformed
(`InvalidData`). Pinned by `a_connection_cut_mid_frame_is_reported_as_lost`
(tests/attach_signal_restore.rs), seen failing with exactly that message first.
