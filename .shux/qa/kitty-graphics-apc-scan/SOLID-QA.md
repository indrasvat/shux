VERDICT: PASS

# SOLID VT QA — round 6 — `kitty-graphics-apc-scan`

## 1. Change under audit

| | |
|---|---|
| Branch | `claude/terminal-browser-shux-g9ig0n` |
| Base | `f071c89b99ca5604252756c529d29c5ae0e5736b` |
| HEAD at audit start | `a81cddd` (`refactor: trim comment density to the repo norm`) |
| HEAD at audit end | `04e0ed1` (`fix(qa): make the determinism evidence agree with itself`) |
| Round-6 delta | `f7692fb..a81cddd`, claimed comment-only |
| Scope | `.shux/qa/kitty-graphics-apc-scan/` |

The change locates kitty-graphics APC sequences without altering the byte stream,
scrubs outer-terminal identity vars from pane children, and fixes `daemon stop`
for a renamed binary. Round 5 passed at `f7692fb`. Round 6 re-establishes that
verdict at `04e0ed1`.

**Mid-audit commit.** `04e0ed1` landed while this audit was running. It carries
exactly the two working-tree edits under review in §6 and touches nothing else.
Its content was verified; all pixel and behavioural evidence below was
regenerated against binaries built from `04e0ed1`. CLAUDE.md's "batch changes
while a gate is auditing" was not followed, but no evidence was invalidated.

## 2. Is `a81cddd` really comment-only? Yes — proven mechanically

Two independent checks, both green:

1. **Raw-line scan.** Every added and removed line in `git diff f7692fb..a81cddd`,
   with leading whitespace stripped, is either empty or begins with `//`. Zero
   lines are code, and zero are code-with-trailing-comment.
2. **Comment-stripped source comparison.** A Rust-aware stripper (handles `//`,
   `///`, `//!`, nested `/* */`, `"…"`, `b"…"`, `r#"…"#`, char literals vs
   lifetimes; then normalises all whitespace) was run over each of the 6 touched
   files at both revisions. All 6 are byte-identical after stripping:

```
IDENTICAL-AFTER-STRIP: crates/shux-pty/src/handle.rs
IDENTICAL-AFTER-STRIP: crates/shux-pty/tests/integration.rs
IDENTICAL-AFTER-STRIP: crates/shux-vt/src/graphics/apc.rs
IDENTICAL-AFTER-STRIP: crates/shux-vt/src/lib.rs
IDENTICAL-AFTER-STRIP: crates/shux-vt/tests/graphics_apc_neutrality.rs
IDENTICAL-AFTER-STRIP: crates/shux/src/daemon_boot.rs
```

**The stripper was proved able to fail before being trusted** — four negative
controls, all correctly reported DIFFERENT: dropping a `#[derive(...)]` attribute
line, deleting a code line, mutating a string literal on a code line
(`"tmux-256color"` → `"ZZtmux-256color"`), and empty input. It correctly ignores
comment-lookalikes inside strings, raw strings and char literals, and correctly
ignores a mutation confined to a doc comment. No attribute, no `#[cfg]`, no
`#[doc(hidden)]`, no string literal changed.

**Explicit statement: no non-comment line changed in `a81cddd`.** Several blocks
were rewrapped; that is the whole of it.

## 3. Testing matrix

| Layer | Result | Evidence |
|---|---|---|
| Unit tests | PASS | `make check` → 2158 passed, 2 skipped |
| Integration tests | PASS | incl. `graphics_apc_neutrality` (2 proptests + 6 cases), `pane_child_does_not_inherit_outer_terminal_identity`, `daemon_stop_reaps_a_daemon_started_by_a_renamed_binary` |
| Raw byte / replay | PASS | all 5 committed `vt-corpus/rich-tui` fixtures replayed through real panes on both arms; all 5 match manifest `sha256` + `bytes` |
| Differential oracle | PASS | 400,000 hostile streams, 0 mismatches |
| Shux automation | PASS | 11 A/B pane arms at 80x24, 120x40, 120x36, 200x60; truecolor + indexed + basic probes in every colour arm |
| Visual inspection | PASS | 7 full-resolution PNGs opened and read as images |
| Pixel comparison | PASS | 12 metrics, exact 0/0, all `status: pass` |
| Cross-render-path | PASS | `pane`/`window`/`session` snapshot all exact-0 A/B at 200x60 |
| DootSabha design | PASS (substituted) | `adversarial-review-apc.md`, `adversarial-review-protocol.md` |
| DootSabha diff review | PASS (substituted) | `adversarial-review-implementation.md` — reviews code, not design |
| `make shellcheck` | PASS | 99 tracked scripts clean |
| `make check-test-groups` | PASS | `daemon-pty` 301, `process-table` 5, `wall-clock` 2, exclusively claimed |

### Differential oracle — rebuilt and mutation-proved

A standalone crate links **both** terminals: `git archive f071c89 crates/shux-vt`
and `git archive a81cddd crates/shux-vt`, as `shux-vt-base` and `shux-vt-head`.
The head copy was diffed against `git show a81cddd:crates/shux-vt/src/lib.rs`
after mutation testing to confirm byte-identical restoration.

HEAD runs the **production** path — `apc_cut_slicing` left at its default `true`.
`set_apc_cut_slicing(false)` is not used, because a real base crate is a stronger
oracle than the switch.

Observable: 10 fields — full `capture_text(None)`; `Debug` of every `Row` across
`total_lines()` (so every cell's char, fg, bg and attributes); cursor; modes;
title; default colours; scroll region; alt-screen + scrollback len +
palette-overridden + presented-total-lines + sync-active; and the ordered
reply-byte stream.

Corpus: partial UTF-8 (lead bytes and orphan continuations), ZWJ/VS16/flag/
skin-tone sequences, C1 bytes including bare `0x9B`/`0x9C`/`0x9F`/`0x84`/`0x85`/
`0x88`/`0x8D`, `CAN`/`SUB`, non-`G` APCs, `ESC _` with no terminator, `ESC _`
re-opened inside an APC, SOS/PM/DCS/OSC/CSI introducers, sixel and DECRQSS
payloads, DEC line drawing, origin mode, scroll regions, alt-screen enter/exit,
synchronized output, OSC 10/11/112, HTS/TBC, DA1/DA2/DSR queries, real kitty
fragments (`a=T`, `a=q`, `a=p`, `a=d`, chunked `m=1`/`m=0`), and 6% raw random
bytes. Chunked three ways — whole, byte-at-a-time, random 1–7 — with the
identical chunk sequence fed to both terminals; grids at 24x80 / 40x120 / 60x200
with a post-stream resize on a third of runs.

```
iters=400000 seed=2830318416 bytes=28737352 streams_containing_ESC_underscore=328806 mismatches=0
ORACLE: PASS
```

**Proved able to fail** (both at n=5000, same seed as the green run):

| Mutation | Result |
|---|---|
| `dispatch_graphics` emits a reply (`ESC _ Gi=1;OK ESC \`) | **3585 / 5000 mismatches**, exit 1 |
| scanner strips APC bytes instead of cutting around them | **1031 / 5000 mismatches**, exit 1 |

## 4. Screenshot matrix

All arms: actual = HEAD build (`04e0ed1`), baseline = base build (`f071c89`),
regenerated by this audit. Thresholds `--max-pixel-diff-ratio 0
--max-mean-channel-delta 0`.

| Viewport | Workload | Size | Changed px | Status |
|---|---|---|---|---|
| 80x24 | colour probe | 720x456 | 0 | pass |
| 120x40 | colour probe | 1080x760 | 0 | pass |
| 80x24 | APC stream | 720x456 | 0 | pass |
| 120x40 | APC stream | 1080x760 | 0 | pass |
| 120x36 | btop replay | 1080x684 | 0 | pass |
| 120x36 | lazygit replay | 1080x684 | 0 | pass |
| 120x36 | nvim replay | 1080x684 | 0 | pass |
| 120x36 | vicaya replay | 1080x684 | 0 | pass |
| 120x36 | vivecaka replay | 1080x684 | 0 | pass |
| 200x60 | APC + colour + origin mode, `pane.snapshot` | 1800x1140 | 0 | pass |
| 200x60 | same, `window.snapshot` | 1080x684 | 0 | pass |
| 200x60 | same, `session.snapshot` | 1080x684 | 0 | pass |

All 12 metrics report `"status": "pass"`, `changed_pixels: 0`,
`pixel_diff_ratio: 0.0`, `mean_rgba_channel_delta: 0.0`, equal sizes. Text
captures byte-identical on all 10 pane-capture arms (`cmp` clean, 62–7029 bytes
each).

**Not blank.** Every PNG was measured for content: distinct-colour counts
427–4557, dominant-colour share 0.737–0.997, identical per-arm across base and
head.

**Opened and inspected** at native resolution: `colour-80x24`, `colour-120x40`,
`apc-80x24`, `tui-btop`, `tui-lazygit`, `tui-nvim`, `tui-vicaya`,
`tui-vivecaka`, `pane-200x60`. Findings: truecolor fg/bg, indexed 208/27, basic
red/green, bold/italic/underline all render distinctly; DEC line-drawing renders
as box glyphs, not letters; no colour bleed past `SGR 0`; no ghost cells, no
clipping, no bad wrapping; cursor block present and correctly placed;
btop/lazygit/vicaya/nvim/vivecaka all fully legible. **The APC arms show no
graphics payload text anywhere in the grid** (`before-apc` /
`after-apc-truecolor` / `abort-esc-then-red` / `tail-yellow`, each in its correct
colour), which is the load-bearing visual claim of this change.

**Comparator proved able to fail** at 0/0:

| Control | rc | Result |
|---|---|---|
| different content, same size | 1 | `fail`, 12625 changed px |
| mismatched sizes | 2 | `fail`, `reason: size_mismatch` |
| **one channel of one pixel** perturbed by +1 | 1 | `fail`, `changed_pixels: 1` |

## 5. Non-VT behaviours re-verified (their files were edited)

**Pane env scrub.** Measured on the pane child's real `/proc/<pid>/environ`, not
on a capture (an early capture-based attempt truncated and would have
under-reported). Child identified by a per-run unique needle in its argv.

- Base leaks **all 31** identity vars plus `WINDOW`: `ALACRITTY_WINDOW_ID`,
  `CMUX_X`, `CONTOUR_PROFILE`, `GHOSTTY_RESOURCES_DIR`, `HERDR_X`,
  `ITERM_PROFILE`, `ITERM_SESSION_ID`, `KITTY_PID`, `KITTY_WINDOW_ID`,
  `KONSOLE_VERSION`, `LC_TERMINAL`, `LC_TERMINAL_VERSION`, `STY`, `SUPACODE_X`,
  `TERMINATOR_DBUS_NAME`, `TERMINATOR_DBUS_PATH`, `TERMINATOR_UUID`,
  `TERMINOLOGY`, `TERM_SESSION_ID`, `TILIX_ID`, `TMUX`, `TMUX_PANE`, `TTY7_PANE`,
  `VSCODE_INJECTION`, `VSCODE_SHELL_INTEGRATION`, `VTE_VERSION`, `WARP_X`,
  `WEZTERM_PANE`, `WINDOW`, `WT_SESSION`, `ZELLIJ`, `ZELLIJ_SESSION_NAME`.
- HEAD leaks **zero**.
- **Deny-list, not `env_clear`:** base child 174 env entries, head 142 — a delta
  of exactly 32. The full `diff` of the two environs contains nothing beyond
  those 32 plus `XDG_RUNTIME_DIR` and `_`, which differ by construction. All six
  non-identity controls (`MY_KEEPER`, `HOME_STAY`, `NOT_IDENTITY_TERMLIKE`,
  `WINDOWMANAGER`, `PATH`, `HOME`) survive, as do `TERM=tmux-256color`,
  `COLORTERM=truecolor`, `TERM_PROGRAM=shux`.
- The `WINDOW` conditional is real: with `STY` unset, HEAD keeps `WINDOW=42`.
  Confirmed on a separate run.

**`daemon stop` from a renamed binary.** Both builds copied to `shux-BASE-AB` /
`shux-HEAD-AB`, each with its own `XDG_RUNTIME_DIR`, daemon pid read from
`$XDG_RUNTIME_DIR/shux/shux.pid` and confirmed via `/proc/<pid>/exe`:

```
BASE: daemon pid=28522  daemon stop rc=0 out="no daemon running"
      RESULT: daemon 28522 STILL ALIVE -> stop failed (leak)   [reaped by the audit]
HEAD: daemon pid=28553  daemon stop rc=0 out="daemon stopped pid 28553"
      RESULT: daemon 28553 GONE -> stop worked
```

This is the defect seen failing on the unfixed tree and fixed on this one,
reproduced independently in round 6.

## 6. Round-5 P2-1 — fixed, correctly and completely

The fix landed as `04e0ed1`.

- `evidence-manifest.json` no longer states "35 runs -> 3 distinct / 37 runs ->
  1". It now quotes the committed logs verbatim: `echo-determinism-on.txt`
  `runs=20 distinct=2` (19/1 split), `echo-determinism-off.txt` `runs=20
  distinct=1`. Both committed logs were read; the prose matches them exactly. It
  additionally records round 5's independent reproduction and explains why an
  n=20 clean run is consistent with a 19/1 split.
- `echo-determinism.sh` no longer restates any run count. Its header now points
  at `harness_corrections` in the manifest as the single location, which makes
  `ab-render.sh`'s existing "recorded in ONE place … do not restate the run
  counts here" claim true for the first time.
- No third copy exists: no run counts remain in `ab-render.sh`, the script, or
  elsewhere in the scope directory.

Both halves of the finding are addressed. Closed.

## 7. Findings

**P0 — none. P1 — none.**

**P2-1 (round 5) — CLOSED.** Determinism prose contradicted the committed logs;
both the manifest and the script are fixed, and the "one place" invariant now
holds.

**P3-1 — stale self-certification in provenance notes.** All nine committed
`pixel-*.json` carried a `note` arguing that the evidence commit touches no
`crates/`, so `actual_commit` was exactly the code the pixels came from. Since
`a81cddd` landed, that shortcut no longer closes. The *conclusion* is unharmed —
§2 proves `a81cddd` compiles to identical code, and §4 regenerates all pixels at
HEAD with exact 0/0 — but the record invited a reader to a check that now fails.
Corrected in the same commit as this report.

**P3-2 — `make check-vt-qa` exits 2 until this file is committed.** The guard
correctly refuses a diff touching `crates/shux-vt/src/` with no `SOLID-QA.md`.
Its self-test passed first (trigger fires on pty-capture and src+tests diffs,
stays quiet on `tests/`, `benches/` and unrelated crates), so the guard is
working, not broken. This PASS is conditional on this report being committed
verbatim alongside the already-tracked `evidence-manifest.json`.

## 8. Passed evidence

- `a81cddd` is comment-only under two independent checks, each proved able to fail.
- 400,000 hostile streams, 28.7 MB, 328,806 APC-bearing, 10-field observable,
  **0 mismatches** against a real base-commit terminal on the production code
  path; two mutations confirmed to break it.
- 12 pixel metrics at exact 0/0 across 80x24 / 120x40 / 120x36 / 200x60 and three
  render paths; comparator fails on a single channel of a single pixel.
- 7 PNGs opened and read; content asserted numerically as well as visually.
- 10 pane captures byte-identical between base and HEAD.
- 5/5 rich-TUI corpus fixtures integrity-checked against their manifest hashes
  and replayed live.
- `make check` 2158/2158 pass; `make shellcheck` 99 scripts clean;
  `make check-test-groups` clean.
- Pane env scrub and renamed-binary `daemon stop` both re-verified by A/B against
  real base and HEAD binaries, with the base defect reproduced.
- DootSabha substitution documented and correctly labelled: two design reviews,
  one implementation-diff review, `dootsabha_implementation` pointing at the code
  review.
- Manifest carries all five required top-level keys; no committed baseline PNG,
  consistent with the metrics-stand-alone rule.
- This audit edited no tracked file.

## 9. Residual risk

- 8-bit C1 APC (`0x9F`) is deliberately unhandled. Documented in `graphics::apc`
  and in `known_limitations`; the oracle feeds bare `0x9F` and finds no
  divergence, so it is a known gap, not a regression.
- CJK renders as tofu (bundled raster font has no CJK) and the combining-acute
  draws in the preceding cell. Both base-identical, both recorded in
  `known_limitations`; those arms prove wide-cell and combining-mark
  *accounting*, not glyph rendering.
- `dispatch_graphics` has no body. Neutrality holds partly by construction today;
  `graphics_apc_neutrality.rs` and the oracle exist to catch the first line added
  there that touches grid, cursor or `responses`.
- `is_live_shux_daemon` parses `ps` output and can be truncated by BSD/macOS `ps`
  without `-ww`. Pre-existing exposure on the basename branch; this platform is
  Linux and untested elsewhere.
- Live `btop`/`lazygit`/`nvim`/`vicaya`/`vivecaka` binaries were not exercised;
  committed raw replay fixtures were used, which is the deterministic method
  CLAUDE.md prescribes for before/after proof.
- The one-in-twenty echo flake is mitigated by `stty -echo`, not eliminated at
  the source; the A/B run in this audit was clean on both arms.

## 10. Cleanup status

Zero leaked daemons. Machine-wide sweep by `/proc/<pid>/exe` found **0**
processes whose executable is any `shux`, `shux-BASE-AB` or `shux-HEAD-AB`. Every
runtime dir this audit created was checked for a live pid via its
`shux/shux.pid`: **0** live. All 12 scratch runtime dirs removed. The one base
daemon that leaked — deliberately, from the base binary under a renamed path,
which is the defect being demonstrated — was reaped by the audit. No `pgrep -f` /
`pkill -f` on a substring was used anywhere. The temporary `git worktree` this
audit created was removed.
