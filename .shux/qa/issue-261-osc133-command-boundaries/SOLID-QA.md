VERDICT: PASS

# SOLID VT QA: issue #261, OSC 133 command boundaries

## 1. Change under audit

- Issue #261: `pane wait-command` waits for a typed command's end, and its exit code, from OSC 133.
- Branch `feat/osc133-command-end`. The audited SHA is `e9aafe0`, on base `origin/main` = merge-base = `8410f2f`.
- VT-path scope (`git diff 8410f2f..e9aafe0 -- crates/shux-vt crates/shux-pty/src/command.rs`):
  - The raw-byte OSC scanner (`crates/shux-vt/src/program_status.rs`) now also cuts `133;C` and `133;D[;exit]`, with strict classification and a 24-byte prefix kept past 4096 bytes.
  - `crates/shux-vt/src/commands.rs` tracks command boundaries.
  - Boundary cuts are dispatched even when program status is off.
  - `pane run`'s framed line hands back the pane's `$?` with `(exit "$__shux_ec") && :` and `eval "unset __shux_ec; (exit $__shux_ec)" && :`.
- Claim under test: none of this changes what the VT draws or replies, and `pane run` output and exit codes are unchanged.
- Out of this gate's scope: the RPC, CLI, shell-integration and docs parts. They count here only where they expose VT state (`pane status` `commands`) or feed it bytes.

How the build relates to the audited SHA:
- The release binaries and the replay tool were built in a scratch worktree at `db271d1`.
- `git diff db271d1..e9aafe0` touches only `crates/shux/src/program_status.rs`, the two shell-integration scripts and an e2e test. It touches no `VT_PATHS` file and nothing under `crates/shux-vt/src`, `crates/shux-pty/src`, `crates/shux-raster` or `crates/shux-ui`, so the VT code that was built is the VT code under audit.
- The one layer the delta can reach is `pane run` under the hooks. That layer and the ERR-trap probe were rerun with `e9aafe0`'s scripts.

Earlier SHAs:
- The same harness ran on `f5a003c`, `93a2cc9` and `db271d1`. On every one, the VT outputs are byte-identical to base.
- On `f5a003c`, the `pane run` layer failed (P1, §6). It has been fixed since.

Every artifact was produced by this audit.

## 2. Stated DoD matrix

| # | Requirement (from the gate request) | Status | Evidence |
|---|---|---|---|
| 1 | Real panes emit interleaved 133 A/C/D, 7501 and colour output (truecolor, indexed, basic), split across reads, with program status on and off. Pixel-verified against the same byte streams on `8410f2f`. | PASS | Workloads w1c, w1, w5, w6, w2 and w4×5 ran in live panes, split into seeded 1..17-byte writes, on isolated daemons (db271d1 and base, status on and off). 94 head/base pairs were compared: 40 PNGs (pane and window at 80x24, 120x40, 200x60 and 120x36), 28 captures and 28 reply-byte files. 0 differ. 18 live metrics are committed (`pixel-ab-live-{on,off}-*.json`, 0/0). Head's `pane status` shows `commands {seen:true, running:false}` in both modes, so the 133 path was live and the A/B is not vacuous. |
| 2 | Grid, cursor, colours, alt screen and OSC/DA replies are unchanged by 133 C/D traffic, with status on or off. | PASS | Replay of 10 crafted streams compares every cell's Debug value, cursor, modes, title, default colours, scrollback length, every reply byte, and text. w6 covers `?1049h/l`, `?2026h/l`, OSC 10/11/12 set and query, OSC 110-112, DA1, DSR 6n and 7501 queries, all around C/D. db271d1 == base for 120/120 files over {on, off} × {whole, two seeded 1..32-byte chunkings}. Head with 133 == base with every 133 stripped (w1c, w5, w6, w2). 400 random streams (5751 OSC 133 sequences, including poisoned 4000-4200-byte C/D, partial ESC/OSC, C1 ST, CAN/SUB, DCS, APC and split UTF-8) × {on, off} × 3 chunkings: 4800/4800 db271d1 == base. |
| 3 | The 7501 query reply bytes are unchanged. | PASS | Live w2 (4 queries, ST, BEL, ST, BEL, around C/D and mid-SGR): identical to base, 322 bytes. Each terminator is mirrored in order, then the DSR barrier. With status off there are no 7501 replies on either build. Live w6's OSC 11, DA1, DSR and OSC 10 replies are identical between builds and between modes, and 7501 replies appear only with status on. |
| 4 | Rich TUIs are unregressed with 133 traffic around them (vim/nvim, htop/btop, lazygit), each with a private rc copy. | PASS | (a) The committed recordings of btop, lazygit, nvim, vicaya and vivecaka, with 380/201/151/149/17 marks injected, match the committed goldens at 0/0, whole and chunked, with status on and off: 20 metrics, in scratch at `.shux/out/issue-261/replay-golden/`. Their full VT state equals base fed the uninjected recording. (b) Live nvim (`--clean -i NONE -n`) and lazygit (private `CONFIG_DIR`, `LG_CONFIG_FILE`, empty `GIT_CONFIG_GLOBAL`) had 133 traffic written to their tty, including a 5000-byte `C;cmdline_url`: pane, window and pre-inject frames match base at 0/0, on and off. (c) Live htop (private `HTOPRC`) was inspected; see §5. The real `~/.config/htop/htoprc` mtime is unchanged (Oct 31). |
| 5 | `pane run` output and exit codes are unchanged in a real bash and zsh pane. | PASS | bash 5.3 and zsh 5.9 interactive panes, with `e9aafe0`'s integration scripts sourced on both builds. 24 rows each, all identical to base in exit code, state and stdout: colour printf (stripped), `false`=1, `(exit 7)`=7, `ls` missing=1 with stderr, `(exit 3)`=3 with output, `set -u`, `set -e` with `false && true`=1 (shell lives), `set -eu` with `[ -f /nonexistent ] && echo never`=1 (shell lives), and an ERR trap with `false` / `(exit 2)`, which fires once inside the capture. The one exception is `echo prev=$?`: 3 on head, 0 on base. That is the intended change. Pane `exit_status` is null on both builds. |
| 6 | Zero leaked daemons, checked by pidfile, with a short isolated `XDG_RUNTIME_DIR`. | PASS | Every run used `/tmp/q261<build><mode>`. After each one: pid from `daemon status`, then `daemon stop`, then `kill -0` fails. Every runtime dir holds only `shux.lock` and `shux.start.lock`. `ps` finds nothing matching the per-run needles `qa261` and `q261`. |

## 3. Testing matrix

| Layer | Status | Evidence |
|---|---|---|
| Unit | PASS | In the `db271d1` worktree: `make test-focus PKG=shux-vt` 750/750, `PKG=shux-pty` 99/99. |
| Integration | PASS | All 12 tests in `crates/shux-vt/tests/commands.rs`, including `an_end_padded_with_ignored_control_bytes_keeps_its_exit_code`. `pane_wait_command_e2e`: 13/13. shux `run_command`, `program_status`, `wait_command`, `shell_integration`: 43/43. |
| Raw byte / replay | PASS | `make test-vt-corpus` passes, including the harness verify. Crafted and random head-vs-base replays, and the committed recordings vs goldens, are in DoD rows 2 and 4. |
| Fuzz | PASS | `make fuzz-program-status FUZZ_SECONDS=30`, run on `f5a003c`, with no crash artifacts: `program_status_decode` 5,238,569 runs, `program_status_table` 1,493,969, `scanner_vs_vte` 1,625,724, `vt_program_status_stream` 394,433. The scanner has changed once since: `db271d1`'s 3-line `exit_field` change alters only the exit value carried by a poisoned `D`. It does not change whether or where a cut lands, and the replay layer covers it. |
| Shux automation | PASS | `session create`, `pane set-size`, `wait-for`, `pane capture`, `pane snapshot`, `window snapshot`, `pane status`, `wait-settled`, `pane run` and `send-keys`, at 80x24, 120x40, 200x60 and 120x36. |
| Visual inspection | PASS | Opened at native resolution: live w1c, w5 and w6 windows, the w1 pane at 200x60, live lazygit and htop windows, the btop replay, the zsh `pane run` pane, the lazygit pre/post diff, and the leak control's diff. See §5. |
| Pixel comparison | PASS | 22 committed metrics, all `pass` at 0/0. 20 more against the committed goldens, in scratch, all 0/0. Negative controls on the `db271d1` build fail as they should: one injected mark with its ESC removed gives 349 px, a wrong live baseline gives 44153 px, and an empty PNG exits 1. |
| DootSabha design | PASS | `council-design.json`: codex ok, agy out of quota, ACCEPT-WITH-CHANGES. The VT owns boundaries independently of 7501 (DoD 1 and 2). C allocates the identity, RIS preserves tracking, and malformed exits read as unknown (unit tests). |
| DootSabha implementation diff | PASS for the VT path | `council-impl-r1.json` (`7518576`): poisoned-prefix chunk dependence. Fixed, and the replay layer confirms it. `council-impl-r2.json` (`f5a003c`): BLOCK on `pane run` errexit, the same P1 as §6, now fixed. `council-impl-r3.json` (`1fccf5b`): a bash.sh nounset blocker, outside the VT path. `council-impl-r4.json` (`37281ea`): a `wait_command.rs` kill race, outside the VT path; it is addressed in later commits that this gate does not judge. Every VT-path finding from the councils is closed. |

## 4. Screenshot matrix

No PNG is committed: this scope holds JSON only. Every PNG is under `.shux/out/issue-261/`.

| Viewport | Command / app | Screenshot (scratch) | Baseline | Metric | Status |
|---|---|---|---|---|---|
| 120x36 | 5 rich-TUI recordings + 133 marks, status on/off, whole/chunked | `replay-golden/replay-133-status-*-actual.png` | committed `.shux/goldens/073-vt-corpus/rich-tui-*-expected.png` | `replay-golden/pixel-*.json` 0/0 ×20 | PASS |
| 80x24 / 120x40 / 200x60 | w1c colour probes + 133/7501 at token boundaries, pane | `live/db271d1-{on,off}-w1c_pane-snapshot_*.png` | `live/8410f2f-*` | `pixel-ab-live-{on,off}-w1c-pane-snapshot-*.json` ×6 | PASS |
| 120x40 | w1c window (border badge, status bar) | `live/db271d1-*-w1c_window-snapshot_120x40.png` | `8410f2f` | `pixel-ab-live-{on,off}-w1c-window-snapshot-120x40.json` | PASS |
| 120x40 | w1: marks inside CSI/UTF-8 (aborts them on purpose), pane | `live/` | `8410f2f` | `pixel-ab-live-{on,off}-w1-pane-snapshot-120x40.json` | PASS |
| 120x40 | w5: long and poisoned 133, window | `live/` | `8410f2f` | `pixel-ab-live-{on,off}-w5-window-snapshot-120x40.json` | PASS |
| 120x40 | w6: alt, sync, OSC 10-12, DA1, DSR around C/D, window | `live/` | `8410f2f` | `pixel-ab-live-{on,off}-w6-window-snapshot-120x40.json` | PASS |
| 80x24 | w2: 7501 queries around C/D | `live/` | `8410f2f` | `pixel-ab-live-{on,off}-w2-pane-snapshot-80x24.json` | PASS |
| 120x36 | lazygit recording + marks, live pane | `live/` | `8410f2f` | `pixel-ab-live-{on,off}-w4-lazygit-pane-snapshot-120x36.json` | PASS |
| 120x40 | live nvim and lazygit with tty-injected 133, window | `tui/db271d1-*-{nvim,lazygit}_window-snapshot_120x40.png` | `tui/8410f2f-*` | `pixel-ab-live-tui-{on,off}-*.json` ×4 | PASS |
| 120x40 | live htop with tty-injected 133 | `tui/db271d1-{on,off}-htop_window-snapshot_120x40.png` | none (live data) | inspected | PASS |

## 5. Visual inspection notes

- No OSC 133 or 7501 text reaches any screen in the clean workloads.
- w1c renders every probe as colour:
  - a truecolor background gradient;
  - 38;5 indexed `#` runs;
  - basic red;
  - green-on-blue;
  - bold yellow;
  - reverse;
  - underline.
- DEC `lqqk` renders as `┌──┐`.
- w5 shows each `after-long-N` line in its colour, with no residue of the 5000-byte payloads.
- In w6, the main screen is restored after `?1049l` and the dynamic colours return to default after OSC 110-112.
- In w1, marks inserted mid-CSI print the leftover parameters as text. That is the expected vte abort, and it is identical on base.
- nvim shows its syntax colours. lazygit shows its panels, the diff colours and both fixture commits; its pre/post difference is 62 px in its own bottom-right region, identical on base.
- The live TUIs with status on show the border badge `▲` and the status bar's `1 waiting`.
- htop, injected in 5-byte writes: on `f5a003c` those writes interleaved with htop's own redraws, and the long OSC's `x` payload printed as text. Base did the same, so it is an artifact of the injection.
- htop, injected in one atomic short write: no `x` runs and no `133;` text in any arm. Meters, the process table and the F-key bar are intact.
- The zsh `pane run` pane on the final scripts shows the colour prompt and the BEGIN/MARKER framing. The ERR trap prints once per failure. There are 2 on-screen `ERR-TRAP` lines for the two failing rows, on both this build and base.
- Two things look wrong but are identical on base, so they are not caused by this diff: CJK `日本語` renders as tofu (the font has no CJK coverage), and 👍🏽 renders as a thumb plus a swatch.

## 6. Findings

All findings are closed on the audited SHA.

- **P1, closed:** at `f5a003c`, the trailing `eval "unset __shux_ec; (exit $__shux_ec)"` ended the pane's shell under `set -e` after an errexit-spared non-zero command such as `false && true`. Seen live in bash and zsh (pane `exit_status: 1`), with base surviving.
  - Fixed in `1fccf5b` with `&& :`.
  - Verified closed on `1fccf5b`, `93a2cc9`, `db271d1` and with `e9aafe0`'s scripts, by DoD row 5.
- **P2, closed, outside the VT path:** at `93a2cc9`, with the integration sourced, a user's ERR trap fired twice for every failing command typed at the prompt, in bash and zsh. It fired once without the integration.
  - Fixed in `e9aafe0`. A typed `false` with the integration sourced now fires once in both shells, the same as without it.
- **P3, closed:** at `f5a003c`, the trailing `(exit N)` also re-fired the trap for a failing `pane run` (3 fires against base's 1 in `bash -c`). `&& :` removed that.
- **P3, open, test strength:** `fuzz/fuzz_targets/vt_program_status_stream.rs` compares status on with off. Both modes now cut at every 133 mark, so no committed test compares a cut stream with an uncut one any more. This audit's head-vs-base replays (4920 files) and stripped-stream comparisons cover the gap now. A committed oracle with the 133 cuts disabled would keep it covered.
- **P3, not reproduced:** one shux-vt test was reported `leaky` (a handle held past exit) in one run on `93a2cc9`. Two reruns with `NEXTEST_STATUS=leak` and the `db271d1` run show none, and no stray process is left.

## 7. Passed evidence (summary)

- The VT source under audit produces byte-identical full state and pixels to base for every crafted, random and rich-TUI stream. That holds with status on and off, however the stream is chunked, and includes the poisoned-prefix path.
- 133 marks are invisible: head with marks equals base without them.
- Live panes and windows match base, and the reply bytes are identical.
- Live nvim and lazygit match base at 0/0 in both modes. htop is clean.
- `pane run` exit codes and output match base in bash and zsh, apart from the intended `$?` hand-back, and errexit shells survive.

## 8. Residual risk

- The committed metrics compare live head with live base and have no committed baseline, so they stand alone. The comparisons against committed goldens are reproducible but live only in scratch, because this scope commits no PNG.
- The fuzz targets were not rerun after `db271d1`'s 3-line scanner change. The replay layer exercises that path: the random corpus feeds poisoned C/D bodies with NUL padding, and the outputs are byte-identical to base.
- The `wait_command` race and the other non-VT fixes after `37281ea` are not judged by this gate.
- Known limit, outside the VT path, accepted by the implementer and not reproduced by this gate: re-sourcing the bash integration in a shell that already has it loses that line's `D`. The command then reads `ended_by: prompt` with exit null, which is an unknown exit, never a wrong one.

## 9. Cleanup

- All daemons this audit started were stopped and their pids proven gone.
- No shux sessions are left. `ps` shows no `qa261` or `q261` processes.
- The scratch worktrees `qa261-head`, `qa261-base`, `qa261-new` and `qa261-final` (the last now at `e9aafe0`) remain in the session scratchpad. They are detached and not shared.
