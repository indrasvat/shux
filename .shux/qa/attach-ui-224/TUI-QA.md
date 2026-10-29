VERDICT: PASS

# shux-tui-qa: attach UI fixes for issue #224

- **Audited SHA:** `373dd97` on branch `claude/project-thread-58qznu`. Diff scope is `37ece0a..373dd97`.
- **Surfaces covered:**
  1. Failed-action notice.
  2. Help-overlay and hint prefix, with client-side `[keys]`/`[keybindings]` hot-reload and the `AttachClientFrame::Prefix` frame.
  3. Copy-mode cursor glyph.
  4. The `session create` terminal check.
  5. Terminal-cursor ownership under overlays.
- **Not covered here:** snapshot sizing, layout and raster changes. The `shux-vt-solid-qa` gate audits those.
- **Method:** an adversarial QA gate that drives the real binary. The attach client runs inside a host shux pane. It is driven with `pane send-keys` and read with `pane capture` (text plus JSON cursor state) and `pane snapshot` PNGs. Every daemon gets its own `XDG_RUNTIME_DIR`, `XDG_CONFIG_HOME` and `XDG_STATE_HOME`.
- **Evidence:** everything is under `.shux/out/attach-ui-224/` (gitignored scratch). Harness: `h.sh`, `run.sh`.

## Rounds

| Round | SHA | Verdict | Findings |
|---|---|---|---|
| 1 | 1c17968 | FAIL | F1 MAJOR, F2 MINOR, F3 MINOR (pre-existing), F4 MAJOR (machinery) |
| 2 | 21de4cf | FAIL | F1–F4 fixed; F5 MAJOR (regression from the F3 fix) |
| 3 | 84307b6 | FAIL | F5 fixed; F6 MINOR (pre-existing) |
| 4 | 373dd97 | PASS | F6 fixed; F5 cells still hold; notice text unchanged |

## Findings and their resolution

Each fix shipped with a test that the authors report seeing red on the unfixed tree. For each one I reproduced the defect before the fix and saw it gone after.

| ID | Rank | Defect (as found) | Fix | Verified fixed (how) |
|---|---|---|---|---|
| F1 | MAJOR | The attach client's new config watcher logged to stderr, which is the terminal it draws on. With `RUST_LOG=warn` or `-v`, a bad TOML save during attach smeared a multi-line WARN across the screen (`rustlog_warn.png`). Base `37ece0a` was clean. | 7ab874a: `client_log.rs` holds the client's logs while attached and writes them after the terminal is restored. Startup `rebind()` closes the snapshot-to-listener race. | Round 2. With `RUST_LOG=warn` and a bad TOML save, the screen stayed clean (`f1_badtoml_attached.png`). The WARN printed after `[detached …]`. Same result for `shux -v`. SIGTERM flushed the held logs, exited 143 and left `icanon` set. Losing the daemon connection flushed them after `[connection to daemon lost]`. |
| F2 | MINOR | At 40 and 80 columns, a long `[shell].command` path pushed the reason ("No such file or directory") off the bar (`long_40.png`). | 7968889: the notice now reads `<verb> failed: <cause> ([shell].command \`<argv0>\`)`. | Round 2: the reason shows at 100, 80 and 40 columns (`f2_long_{100,80,40}.png`). Control characters are still stripped. Round 4: the live text is unchanged. |
| F3 | MINOR (pre-existing) | The copy cursor was invisible on the last pane row, under the hint bar. `G` lands there. | a57b4af: the cursor is drawn after the hint. | Round 2: the cursor is visible on the hint row with the pane glyph, wide characters from either half included. |
| F4 | MAJOR (machinery) | `proc_scope.sh`'s pattern `"$REPO_ROOT"/*/shux` claimed nested worktrees' daemons. A guard or reap in the main checkout then killed other agents' daemons, which caused one spurious 10-failure run during this audit. | 21de4cf: `path_in_checkout` excludes nested checkouts. Adds `proc_scope_selftest.sh`. | Round 2: `scope_repro2.sh` reports "not claimed". The selftest passes at HEAD and FAILs (2 lines) against 1c17968's lib, so it can fail. |
| F5 | MAJOR (regression from a57b4af) | Drawing the cursor last parked the visible terminal cursor in the next cell. Every copy-mode frame showed two adjacent blocks (`f3_ab_row0.png`, host `cursor.visible:true` at col+1). | e890a72: `set_cursor_hidden` while the modal copy overlay or help is up. | Rounds 3 and 4: host `cursor.visible:false` at row 0, a middle row, the last row, and a wide character on the last row, with one block only (pixel-checked). Help hides the cursor and leaves no stray block at the box edge. |
| F6 | MINOR (pre-existing, same at `37ece0a`) | After a mouse selection, the right-click menu, or the welcome toast, the terminal cursor showed at the overlay's last write. Example: (12,5) when it should be at (98,37) (`r3_drag_crop.png`). | 49a3bdf: `restore_cursor()` runs after any overlay bytes. | Round 4: host cursor at the pane cursor (98,37), visible, during the fresh-state welcome toast, mid-drag, after release, and with the menu open. No stray block (`r4_drag.png`, `r4_menu_crop.png`, `r4_toast.png`). |

## DoD matrix (the change's own promises)

| Criterion | Status | Evidence |
|---|---|---|
| Failed split or new window shows `<verb> failed: …` in the error colour, centred, until the next action or keystroke | PASS | `split_fail_100.png`: text pixels are exactly `status_error` (237,135,150). Cleared by a keystroke (also in copy mode) and by a successful split. The failed window was rolled back. The error stays shown while zoomed and in copy mode. With help open the action is swallowed and no notice appears. Round 4 live text: `split failed: No such file or directory ([shell].command \`/nonexistent/prog\`)` and the same for new window. |
| Help title and narrow hint name the client's live prefix; reload works; bad reloads keep working bindings | PASS | ctrl-a→ctrl-b live with the overlay open, and the new prefix closes it. A bad prefix `ctrl-zz`, bad TOML, or an unknown `[keybindings]` action keeps ^B, with no crash or blank screen (`badprefix_help.png`). Deleting the config falls back to ^Sp; recreating it restores ^A. Atomic rename, vim's backup-rename and 30 rapid writes all settle on the final prefix. Narrow hint: `^A + ?  for help`. |
| Daemon-drawn hints use the client prefix | PASS | Welcome toast and status-bar hint show ^A, then ^B, then M-W across reloads, and stay put after `ctrl-zz`. Two clients with different config dirs each see their own prefix. |
| Copy cursor draws the glyph under it in cursor colours; wide characters covered whole from either half | PASS | A/B against base at (0,0): base ` RUECOLOR`, head `TRUECOLOR`. `pixel_verify.py` gives 34 changed pixels, all inside the cursor cell. Also checked: wide character at the last column, 😀, ❤️ (VS16), 🇮🇳, combining é, bold/underline/reverse, a blank cell, after PgUp, over vim with syntax highlighting, and on the last row. |
| `session create` without `-d` refuses before creating anything unless stdout is a terminal and a key source exists | PASS | Allowed and attaches: stdin `</dev/null` with a terminal on stdout; `setsid` with terminal stdio; `--ensure` on an existing session. Refused with exit 1 and no session created: stdout piped, stdout to a file, no terminal at all, `setsid` with stdin `/dev/null`. |
| Version skew | PASS (by design) | A binary mismatch restarts the daemon (`client.rs:249`), seen live. An old daemon skips the unknown `Prefix` frame (`attach.rs:981-986`). |
| The overlays own no terminal cursor | PASS | Copy mode and help hide it. Selection, menu and toast put it back. After `q`, closing help, detach and reattach in copy mode, switching windows in copy mode, and over a zoomed pane, the cursor returns to the shell's cell, visible. vim's insert-mode bar shape is the same before and after copy mode (`r3_vim_shape.png`). |

## Test matrix

| Layer | Status | Evidence |
|---|---|---|
| Unit | PASS | round 4: shux-ui `copy_mode compositor help_overlay attach` 80/80; shux `says_failed_once`, `pane_spawn`, `statusbar_build`, `client_log` inside the focused run below |
| Integration / e2e | PASS | round 4: `make test-focus PKG=shux` (`attach_ui_e2e`, `shell_config_e2e` and the unit filters): 70/70 (`test-r4.log`), including `a_mouse_selection_leaves_the_terminal_cursor_on_the_pane_cursor`, `copy_mode_and_the_help_overlay_hide_the_terminal_cursor`, `the_attached_client_logs_after_the_terminal_is_restored`, `a_failed_attach_split_or_new_window_tells_the_user_why`. Earlier rounds: 87/87 and 61/61 (round 2), e2e 4/4 (round 3). |
| Real shux automation | PASS | isolated roots q1–qc, r1–r2, s1–s2, t1 |
| Real workloads | PASS | bash, sh, vim (syntax highlighting, insert-mode bar cursor), setsid, real config edits (rename, delete, backup-rename) |
| Not available | — | htop, btop, lazygit and nvim are not installed; less is installed but was not run |
| Colour probe | PASS | truecolor + 256-indexed + basic in every target |
| Text capture | PASS | `pane capture` text plus JSON `.cursor` |
| PNG at full resolution | PASS | every PNG named here was opened |
| Pixel verification | PASS | A/B diff against base for the cursor fix; colour sampling for the notice, the cursor and stray blocks. No committed baseline exists or is required for scratch evidence. |
| `make check-tui-qa` | PASS | "No general TUI QA manifests found" |
| Process cleanup | PASS | see the cleanup section |
| Council and simplify evidence | outside this gate | the PR owner's responsibility |

## Screenshot matrix (all in `.shux/out/attach-ui-224/`)

| Viewport | Scenario | Files | Status |
|---|---|---|---|
| 100x30 | failed split notice | `split_fail_100.png` | PASS |
| 100 / 80 / 40 cols | reason-first notice | `f2_long_100.png`, `f2_long_80.png`, `f2_long_40.png` | PASS |
| 100 cols | control characters stripped | `f2_inject_100.png` | PASS |
| 80x24 | zoomed / copy-mode notice | `zoom_fail.png`, `copy_fail.png` | PASS |
| 100x40 | help overlay after bad prefix; help with hidden cursor | `badprefix_help.png`, `r3_help.png`, `r4_help.png` | PASS |
| 100x30 | onboarding toast | `onboard_A.png`, `r4_toast.png` | PASS |
| 98x37 pane | copy-cursor edge cases | `cur_*.png`, `emoji_strip.png`, `misc_strip.png`, `cur_scrolled_crop.png` | PASS |
| 98x37 pane | copy cursor rows, one block | `r3_f5_strip.png`, `r4_row0.png`, `r4_mid.png`, `r4_last.png`, `r4_lastw.png` | PASS |
| 100x30 | A/B copy cursor against base | `ab_base_copy00.png`, `ab_head_copy00.png`, `ab_copy00_diff.png` | PASS (change confined to the cursor cell) |
| 100x40 | vim | `vim_copy.png`, `r3_vim_shape.png` | PASS |
| 100x40 | mouse selection and menu | `r4_drag.png`, `r4_menu_crop.png` | PASS |
| 100x30 | F1 before and after fix | `rustlog_warn.png`, `f1_badtoml_attached.png` | PASS at HEAD |

## Residual risk
- The action error is not cleared by a mouse click or scroll. The stated contract is "action or keystroke", so this was accepted.
- Under `-u NONE`, vim shows the em-dash as `�~@~T`. That is the pane locale, which the VT gate covers.
- `shux daemon stop` returns before the daemon exits. Callers must wait on the pidfile, and the test `Env` does.
- When stderr is redirected to a file but stdout is a terminal, CLI errors are still coloured. This is pre-existing and outside this change.
- `session create` exits 0 on "connection to daemon lost". This is pre-existing.

## Cleanup
- Every root I used has its daemon stopped and verified by pidfile: q1–q9 and qa–qc, r1–r2, s1–s2, t1.
- A process scan for my binaries, the base binary, the roots and pane children (sleep, vim, bash) is empty after every round.
- Every `make test-focus` run went through `no_leak_guard` and exited 0.
- `git status --porcelain` is empty; no tracked files were changed.
- Scratch is in `.shux/out/attach-ui-224/` and `/tmp/qbase224` (base build).
