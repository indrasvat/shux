VERDICT: PASS

# SOLID VT + TUI QA: viewer-output-288 (issue #288)

- Change: fix/288-viewer-output, d36b738..c9657f2 (c9657f219b4262ffd865a53be87a397adde2db1c).
- Both gate charters (shux-vt-solid-qa and shux-tui-qa) ran as one agent, one run, one report.
- The first pass audited 04527bc and was BLOCKED on one row: the outside terminal. A delta re-audit at
  c9657f2 closed that row.
- The 04527bc..c9657f2 delta is doc-only: the `--plain` clap doc comment in `crates/shux/src/cli/args.rs`,
  which fixes this gate's P3. The rebuilt binary's `--help` shows the new text.
- `make vt-neutrality` was re-run at c9657f2. It is 0/0 on all six layers (`neutrality.json`, head c9657f2).

## DoD matrix (the issue's "Done when" list)

| # | Row | Status | Evidence |
|---|---|---|---|
| 1 | A link survives a colour reset; the VT gate passes | PASS | See "Links" below. |
| 2 | A plain viewer gets text and colour but no effects; a normal viewer is unchanged | PASS | See "Plain viewers" below. |
| 3 | The leaner redraw draws the same screen in a real outside terminal | PASS (WezTerm); kitty not run | See "Outside terminal" below. |
| 4 | btop, vim, lazygit and htop render through attach | PASS | See "Rich TUIs" below. |
| 5 | Before/after evidence on the PR | n/a at gate time | The PR step owns it. |
| — | Plain attach exits non-zero when refused or cut off | PASS | See "Exit codes" below. |

### Links (row 1)

- **Method:** `pane.glance` cell runs from a live pane, HEAD vs v0.57.3.
- **SGR reset:** on HEAD, a link stays on its text across SGR 22, 39 and 0. It ends only at OSC 8 with an
  empty URI. v0.57.3 dropped it at SGR 0.
- **Screen switches:**
  - On HEAD, text written after `?47h`, `?1047h` or `?1049h` is not linked.
  - v0.57.3 carried the link onto the alternate screen for `?47h` and `?1047h`.
  - After `l`, HEAD's text is not linked.
- **No over-correction:** SGR 0 inside a link still resets underline style and colour.

### Plain viewers (row 2)

- **Method:** a real `shux session attach` runs as the program of a pane in a second isolated daemon. That
  daemon's `pane.record` captures the exact bytes sent to the viewer.
- **Plain viewer:** 0 OSC 8, 0 OSC 12/112, 0 DECSCUSR, 0 kitty image sequences and 0 bidi or zero-width
  characters.
- **Colour kept:** truecolor, indexed and basic SGRs are all present.
- **Normal viewer:** HEAD sends the same effect counts as v0.57.3.
- **Images:** a normal viewer gets 2 kitty sequences; a plain viewer gets 0.
- **Copy mode:** a plain viewer in copy mode, with a keyboard selection over the bidi line, got 111 KB with
  none of the above.
- **Pixels:** the normal viewer at 100x30 matches v0.57.3 at 0/0.

### Outside terminal (row 3)

- **Unit level:** the 2,000-random-frame unit test is cited from `make check`.
- **WezTerm method:** the harness `/tmp/s288-wez.sh` + `/tmp/s288-wez.py` runs a real `shux session attach`
  inside a `wezterm-mux-server` pane. The pane prints CJK, emoji, ZWJ, VS16, a Devanagari conjunct,
  combining marks and tabs, then partial rewrites, so later frames are diffs. Rows 0–11 of `wezterm cli
  get-text` are compared with shux's `pane capture`.
- **Independent re-run:** this gate re-ran the harness (`wezterm-ab.json`).
  - HEAD and v0.57.3 draw identical text on every row.
  - Both differ from shux's own capture on rows 1 and 9 in the same way: ❤️ and क्षि, which v0.57.3 already
    does.
  - A sabotaged trim (no move after any cell) draws row 1 differently, so the check can fail.
- **Coverage limit:** the comparison is text only.
- **kitty rig:** NOT run. `colima start` fails with "disk shrinking is not supported" (instance disk 100GiB,
  profile 40GiB). The config was left alone. The PR must say so.

### Rich TUIs (row 4)

- **Replays:** the committed rich-TUI recordings (btop, nvim, lazygit, vicaya, vivecaka) were replayed through
  v0.57.3 and HEAD viewers at 122x39.
  - Result: 0/0.
  - Mask: the status-bar uptime counter only (1000,720,1098,741). Unmasked, the only difference was "up 3s"
    against "up 4s".
- **Live:** htop (private HTOPRC) and vim (`-u NONE`) were inspected live. vim's plain and normal viewers
  match at 0/0.

### Exit codes

- A refused attach (`session_resolve`) exits 1; v0.57.3 exits 0.
- A lost connection (target daemon SIGKILLed) exits 1 with "connection to daemon lost".

## Pixel metrics

- **Runs:** all six use `pixel_verify.py` via `uv run --script`, at exact 0/0:
  - `pixel-ab-normal-viewer-100x30.json`
  - `pixel-viewer-replay-{btop,nvim,lazygit,vicaya,vivecaka}-122x39.json`
- **Controls:** a double snapshot of the same daemon compares 0/0.
- **The comparator can fail:**
  - Plain vs normal fails at a ratio of 0.00055.
  - btop vs nvim, under the same mask, fails with 738,720 px changed.
- **Screenshots:** none are committed, because no baseline is tracked. PNGs are in
  `.shux/out/viewer-output-288/`.
- **Rasterizer coverage:** `neutrality.json` does not cover the attach redraw path. The viewer A/Bs above do.

## Findings

- **P3, fixed in c9657f2:** the `--plain` help text left out the invisible-control stripping.
- **P3, behaviour this diff did not introduce:**
  - What happens: HEAD `--plain` against a v0.57.3 daemon never reaches the new "older daemon" refusal. The
    client restarts a daemon whose build differs first, and that kills its sessions.
  - Coverage: the unit test only.
- **P3, harness:**
  - `s288-wez.sh` finds its mux with `pgrep -nx` rather than a pidfile.
  - It compares text only, not colour.
- No P0, P1 or P2.

## Residual risk

- kitty was not photographed; WezTerm is the only foreign terminal checked.
- The WezTerm check covers rows 0–11 and text only.

## Cleanup

- Every daemon this audit started was stopped with `shux daemon stop`; no live pidfile remains.
- The intentionally killed daemon (q288k, pid 97800) is dead.
- The first attempt's WezTerm mux (7130) and its children were killed by pid.
- The re-run harness stopped its own mux and daemons.
- No orphaned probe, attach, `bash --norc` or sleep processes remain.

## Delta: rebase onto main after #289 merged

The branch was rebased from d36b738 onto f61f664 (#289). commit_under_audit
moves to ebbafab, the rebased copy of c9657f2. `git diff c9657f2 ebbafab`
over every VT path differs only in crates/shux-ui/src/statusbar.rs, where
#289's `StatusSegment.group` field arrived from main (trivial tier there,
neutrality 0/0); no VT code of this change moved. After it, 7bff949 adds
`plain: false` to #289's test hellos (tests only). `make vt-neutrality
BASE=f61f664` re-run at 7bff949: 0/0 on every layer. `make check` green at
7bff949. The pixel metrics and WezTerm A/B above were measured before the
rebase; #289 changes nothing a single viewer draws ("◉ N attached" appears
only with two or more).

Rebased again onto 91ab639 (the v0.57.4 release commit, version bumps only); commit_under_audit is now 8595c2b, its rebased copy. `make vt-neutrality BASE=91ab639` re-run at the new head: 0/0 on every layer.
