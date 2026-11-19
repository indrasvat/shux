VERDICT: PASS

# SOLID VT QA: issue #275, `pane run` on OSC 133 boundaries

## Change under audit

- Issue: #275 ("pane run types markers the shell no longer needs; wait-command returns no output; the two waits cap differently"). No PR is open yet; the claims audited are the ones the parent handed over and the draft PR body.
- Branch `fix/275-osc133-run-output`, HEAD `64bde9eceab193b73d0912d234e61ffe6b266390`. Base `origin/main` `ffb7cc87d32e` (v0.57.1).
- Diff: 39 files, +4298/-238. VT_PATHS touched: `crates/shux-vt/src/{commands,lib,program_status}.rs`, `crates/shux-pty/src/capture.rs`.
- Binaries: built by this audit from detached worktrees `/tmp/s275-qa/{base,head}` with one `CARGO_TARGET_DIR` per arm. `make check` and `make fuzz-program-status` ran in the branch worktree at the same SHA.
- Not covered: the uncommitted `fuzz/Cargo.lock` change in the worktree (a version bump that `cargo fuzz` writes, see P3-2).

## Verdict basis

PASS at `983ca92`. The first audit, at `64bde9e`, failed one required row (D2, finding P1-1). The re-audit below shows `983ca92` fixes it, and the commits since `64bde9e` touch no VT_PATHS file, so every other layer's evidence still applies. The rows and findings below record the `64bde9e` audit; the re-audit section supersedes D2, L4b, P1-1 and P3-1.

## Stated DoD matrix

| # | Done-when / claim | Status | Evidence |
|---|---|---|---|
| D1 | With the integration, `pane run -c 'echo hi'` shows only `echo hi` and `hi`, and returns `{"stdout":"hi\n","exit_code":0,"boundaries":"osc133"}` | PASS | bash/readline, bash+ble.sh, zsh: `.shux/out/issue-275/live/{bash,ble,zsh}-run-echo.json` and `*-capture-echo.txt` (no `SHUX_`). PNG `v1_pane-snapshot_80_bash-run-colour.png` |
| D2 | Without the integration, `pane run` behaves as today, and the JSON says `markers` | FAIL at 64bde9e, **PASS at 983ca92** (see Re-audit) | A bare `bash -i`: markers, `hi`, exit 0 (PASS). `env QA=1 bash -i` and `bash -c 'exec bash -i'`: head exits 3 with `not_a_shell`, while base exits 0 with `hi`. Evidence: `live/plain-{head,base}-{pd,pe,pc}.json`, script `live/probe-plain.sh`. See P1-1 |
| D3 | `wait-command --output` returns exactly what the command wrote, including output longer than the screen and wrapped lines | PASS | A 300-line generator with 250-column lines (wrapping at 80) and truecolor, indexed and basic SGR. Its stdout is byte-identical to the ANSI-stripped expected text in bash, ble and zsh, through both `wait-command --output` and `pane run` (`live/*-wc-long.txt`, `*-run-long.txt`). `truncated:false` |
| D3b | `truncated` past 1 MiB, `evicted` when let go | PASS | 1.5 MiB gives `truncated:true` and 1 047 553 chars, the first 1 MiB of raw PTY bytes after the CRLF fold (`live/big.json`). Three 0.9 MiB runs, then `wait-command --after` the floor, give `stdout:null, evicted:true` (`live/*-wc-evicted.json`) |
| D4 | `wait-for --timeout-ms 90000` waits 90 s; over 3600000 every pane wait refuses with the same -32602 | PASS (one P3) | It waited 91 s and exited 1 (`live/waitfor-90s.out`). The CLI wait-for, wait-command (`61m`, `3600001ms`) and wait-status, and the RPC `pane.wait_for`/`wait_command`/`wait_status` with `timeout_ms:3600001`, all return the identical -32602 `timeout_ms must be at most 3600000 (1h)` with exit 2. `pane run --timeout 3601|61m` returns -32602 too, but with a different message and data (P3-1). The cap itself is accepted: `wait-for 3600000`, `pane run --timeout 1h` |
| D5 | Skill run recipes updated | PASS | `skills/shux/SKILL.md` and `references/api.md` are in the diff. `make check` runs `check-skill-docs`, which passed |
| C4 | The VT-side changes alter nothing the VT draws, replies or snapshots | PASS | L3a, L3b, L3c, L6 below: 72 VT dumps identical, 50 committed metrics at 0/0, 19 goldens at 0/0, 13 captures identical |
| C5 | Rich TUIs in a pane still render; `pane run` refuses a full-screen program and a password prompt | PASS | vim, nvim, less, htop and lazygit render in an integrated bash pane. Each refusal exits 3 (`other_program`), and the screen after it has no `QA-INJECT`. A `read -s` password prompt is refused `hidden_input` in bash and zsh, and the read then received exactly `abc` (`got-3`). ble+vim is refused too. PNGs `live/v1_pane-snapshot_80_bash-{vim,nvim,less,htop,lazygit,password}.png` |
| C6 | `make fuzz-program-status` (includes `vt_command_capture`) and `make check` pass | PASS | `make check` under `LANG=en_US.UTF-8`: exit 0, "All checks passed", 3162/3162. The first run under C locale failed `check-test-inventory` on a pre-existing locale defect (P3-3). Fuzz: see the L1f row |

## Testing matrix

| Layer | Result | Evidence |
|---|---|---|
| L1 unit | PASS | 31 `shux-vt::commands` tests, the `commands::` and `capture::` unit tests (`capture::tests::application_strings_are_not_text`) and the `run_framing` tests, inside `make check` |
| L1f fuzz | PASS | `make fuzz-program-status` (60 s each): program_status_decode 16.3M runs, program_status_table 972k, scanner_vs_vte 1.77M, vt_program_status_stream 656k, vt_command_capture 578k; 0 crashes (`/tmp/s275-qa/fuzz.log`) |
| L2 integration | PASS | 42 `pane_wait_command_e2e` tests and `pane_run_output_e2e`, in `make check` (log `/tmp/s275-qa/make-check-utf8.log`) |
| L3a raw replay, VT A/B | PASS | Probe `vt_ab_probe.rs`, built per arm. 9 streams (the 5 committed rich-tui recordings, plus OSC 133 streams carrying `shux-editor=zsh|ble|readline|bogus`, a 10 MiB stream past both caps, malformed and truncated marks, APC/PM/SOS, DA1/DA2/CPR/DSR/OSC 10-12/DECRQM/XTWINOPS queries, wide, combining and emoji cells, sync output, alt screen, origin mode) × 6 chunkings (whole, 1, 3, 17, 4096, random). 54 dumps identical, covering every grid and scrollback row (`Debug`), the cursor, modes, title, default colours, every reply byte, and the grid after shrink and after grow. 27 PNGs at 0/0. The negative control (btop vs lazygit) fails, as it must |
| L3b corpus vs committed goldens | PASS | `vt_corpus_harness --mode verify`, 19 cases, exit 0 in both arms. The head output matches `.shux/goldens/073-vt-corpus/*-expected.png` 19/19 at 0/0, and its PNG and text are byte-identical to base |
| L3c real recordings, VT A/B | PASS | Raw PTY recordings of integrated bash, ble.sh and zsh sessions (`pane.record.start`), covering vim, nvim, less, htop, lazygit, a password prompt and 1.5 MiB of output: 18 dumps identical, 9 PNGs at 0/0 |
| L4a shux automation, HEAD | PASS | `live-head.sh`: 52 checks pass. 12 harness failures traced to my own expectation: macOS's tty emits `\r\r\n` after the first 1023-column line, reproduced with `script(1)` outside shux |
| L4b no-integration behaviour | FAIL at 64bde9e, **PASS at 983ca92** | P1-1; see Re-audit |
| L5 visual inspection | PASS | Opened at native resolution: run-colour (truecolor orange, indexed red, basic red), vim, htop, lazygit, password, zsh wc-long (wrapped rows intact), the window snapshot at 200x60, and the probe colour frame. No bleed, ghost cells, cursor artefacts or clipping. CJK shows as boxes, the known renderer-v2 gap, the same on base |
| L6 pixel, live daemon A/B | PASS | `ab-live.sh`: 13 streams replayed with `stty -echo -opost` behind a gate set after the resize, at 80x24, 120x40, 200x60 and 120x36. 13 pane.snapshot and 13 window.snapshot at 0/0, 13 pane.capture byte-identical. The self-determinism control (a second pane.snapshot) was identical every time |
| L7 independent verdict | this report | |
| L8 DootSabha design | PASS | `council-design.json` (codex+agy, 2026-11-18 15:13) |
| L8 DootSabha implementation | PASS | `council-impl.json` (reviewed 9f41a78, 18:00) and `council-closure.json` (18:56). 64bde9e (18:59) fixes the closure's item 1 after that round; it was not reviewed by a council |

## Screenshot / pixel matrix (committed metrics, all `status: pass`, 0/0)

| Viewport | Stream | Paths | Metric |
|---|---|---|---|
| 80x24 | real bash, ble, zsh recordings; plain prompt | pane + window | `pixel-ab-live-rec-*-80x24-{pane,window}.json` |
| 80x24, 120x40, 200x60 | `osc133-colour` | pane + window | `pixel-ab-live-osc133-colour-*-{pane,window}.json` |
| 80x24 | `osc133-mix` | pane + window | `pixel-ab-live-osc133-mix-80x24-*.json` |
| 120x36 | btop, lazygit, nvim, vicaya, vivecaka (committed rich-tui) | pane + window | `pixel-ab-live-rich-*-120x36-*.json` (vivecaka window: status-bar clock masked) |
| per stream, plus grown +10x+30 | 12 streams, VT probe | raster | `pixel-ab-vt-*-{whole,grown}.json` |

Actual and expected PNGs and diffs: `.shux/out/issue-275/{live-ab,vt-ab,diff}/`. Goldens: `.shux/out/issue-275/golden/`.

## Findings

### P1-1 (fixed in 60c9d65, verified at 983ca92): `pane run` refuses a shell without integration that was started through a wrapper (`not_a_shell`), so D2 fails

- Repro (`live/probe-plain.sh`). Start a session with `-- env QA=1 bash --rcfile plain.rc -i`, or with `-- bash -c 'exec bash --rcfile plain.rc -i'`, then run `pane run -c 'echo hi'`.
  - Head: exit 3, `{"error":{"code":-32012,"data":{"reason":"not_a_shell","foreground":"pane_program","shell_state":"unknown"}}}`.
  - Base: exit 0, `{"stdout":"hi\n","exit_code":0}`.
- Cause: `run_framing::choose` sends `(Wanted::Auto, Unknown)` to markers only when `front.pane_is_shell`. That flag is `runs_a_shell(&pane.command)`, a test of the spawn argv alone, so `env …`, `bash -c '… exec bash'`, `ssh host`, `docker exec -it … bash` and `nix develop` all count as "not a shell".
- The PR body's state map says `unknown → markers or no_prompt`. Its "Contract changes" and "Risk" sections do not list this narrowing.
- A fix is to accept the foreground group's process name (`group_is_a_shell`, which already exists) as well, which keeps `env …` and `exec` chains working. Otherwise keep base's markers behaviour for `Unknown` in `auto`. Remote shells (`ssh`) stay ambiguous under either option; whichever is chosen must be stated in the PR, or D2 re-scoped there.
- Ship it with a red-first e2e test that spawns a wrapped plain shell.

### P3-1 (fixed in 60c9d65, verified at 983ca92): `pane run`'s over-cap error is not the same error the waits give

The waits return `message: "timeout_ms must be at most 3600000 (1h)"`, `data:{field:"timeout_ms",max:3600000,hint}`. `pane run --timeout 3601` returns `message:"invalid_params"`, `data:{detail:"timeout must be a whole number of seconds, at most 3600 (1h)",field:"timeout",max:3600}`. Both use -32602 and exit 2. The issue's DoD only binds wait-for against wait-command, which agree. The claim "the same -32602 error" holds for the code only.

### P3-2: `fuzz/Cargo.lock` left modified in the worktree

`cargo fuzz` bumps the workspace crates from 0.55.0 to 0.57.1, because main's lock is stale. Commit it or restore it; it is not part of the audited SHA.

### P3-3 (pre-existing, outside the diff): `check-test-inventory` depends on the shell's locale

`scripts/check-test-inventory.sh` runs `sort` and `comm` without `LC_ALL=C`. A cached base list written under a UTF-8 locale (`_` sorts before digits) makes `comm` under the C locale report three tests "no longer run anywhere" that are present in both trees. Reproduced, and passes under `en_US.UTF-8`.

## Passed evidence (independent of the implementer)

- All VT-visible state is identical to base across 72 dumps, 50 metrics at 0/0, 19 committed goldens at 0/0 and 13 captures. That covers synthetic, committed rich-TUI and freshly recorded real-shell streams, under six chunkings and through a resize.
- Live behaviour of D1, D3, D4 and C5 in readline bash, ble.sh and zsh, including exact long and wrapped output, truncation, eviction, the 90 s wait, the shared ceiling, refusals that type nothing into TUIs or a password prompt, and editor-scoped end-of-line trimming. zsh and ble `printf abc` return `abc`; output that merely looks like a mark is kept (`live/probe-eol.sh`).
- `make check` passes (3162/3162).

## Residual risk

- `ssh` and container shells are still told apart from non-shells only by process name. After 60c9d65, `env …` and `exec` chains are accepted; a pane whose foreground leader is `ssh` or `docker` is still refused `not_a_shell` unless `--boundaries markers` is passed.
- nextest reported 1 and then 2 "leaky" tests across the two runs, without naming them.
- 64bde9e was not reviewed by a council (it arrived after the closure round).

## Cleanup

- shux sessions: every audit session was killed and every daemon stopped (`/tmp/q275h`, `/tmp/q275base`, `/tmp/q275head`, `/tmp/q275e`, `/tmp/q275probe`): no pidfile under any of them, and no process from either audit binary or any pane child (`sleep 900`, `gen.py`, `htop`, `lazygit`) remains, checked with `ps` on the per-audit binary paths.
- Scratch worktrees `/tmp/s275-qa/{base,head}`: kept on purpose, so a re-audit of P1-1 can reuse both arms' binaries; remove with `git worktree remove /tmp/s275-qa/base` and `git worktree remove /tmp/s275-qa/head`.

## Re-audit at 983ca92 (2026-11-18, delta only)

- Commits since the first audit: `60c9d65` (a wrapped shell counts as a shell; the `pane run` cap message), `2b648ca` (`scripts/test-stress.sh`, Makefile), `983ca92` (the timed_out `hint`, send-keys' `own_pane` refusal). `git diff 64bde9e..983ca92` touches no VT_PATHS file, checked against the `VT_PATHS` list in `scripts/check-vt-qa.sh`. So L1-L3, L5, L6 and L8 are not re-run, and their 64bde9e evidence stands for the VT side.
- The binary is a fresh release build of `983ca92` from the detached worktree `/tmp/s275-qa/head`. It contains the new cap message and not the old one.
- Layers re-run:
  - **L4b, no-integration behaviour, PASS.** Script `probe-plain.sh`, run on both arms. Head (983ca92) exits 0 with `{"stdout":"hi\n","exit_code":0,"boundaries":"markers"}` for each of `bash --rcfile plain.rc -i`, `env QA=1 bash … -i` and `bash -c 'exec bash … -i'`. Base (ffb7cc8) exits 0 with `hi` for the same three. Guard rails still hold:
    - an integrated bash still gets `boundaries: osc133`;
    - a `python3 -i` pane is still refused `not_a_shell` (exit 3);
    - a wrapped plain shell running vim is refused `other_program`, with nothing typed (the PNG is opened and shows vim's unchanged buffer).
  - **P3-1 check, PASS.** `pane run --timeout 3601` and `--timeout 61m` exit 2 with -32602 `timeout must be at most 3600 (1h), in whole seconds`, `data:{field:"timeout",max:3600,hint}`. That is now the waits' shape (`timeout_ms must be at most 3600000 (1h)`, `data:{field:"timeout_ms",max:3600000,hint}`), with the unit of each flag. `--timeout 1h` is still accepted.
- Evidence: `.shux/out/issue-275/reaudit-983ca92/` (`plain-{head,base}-{pd,pe,pc}.json`, `run-over-*.out`, `wf-over.out`, `wrapped-vim.txt`, `v1_pane-snapshot_80_wrapped-vim.png`, `probe-plain.sh`, `probe-reaudit.sh`).
- `make check` at 983ca92 is the implementer's run, not re-run here. My own `make check` pass is at 64bde9e.
- Still open: P3-2 (`fuzz/Cargo.lock` is still modified in the worktree).
- Cleanup: sessions killed and daemons stopped in `/tmp/q275r`, `/tmp/q275head` and `/tmp/q275base`. No pidfile and no process from the audit binaries remains.
