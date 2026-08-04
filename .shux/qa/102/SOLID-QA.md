VERDICT: PASS

# SOLID VT QA — issue #102, bound attacker-controlled pane input in `shux-vt`

Gate: `shux-vt-solid-qa`. Audit-only; no product source was edited by this gate.

## 1. What was audited

| | |
| --- | --- |
| Branch | `fix/vt-input-bounds` |
| Baseline for every A/B | `b521f2c` (v0.46.0), built in a separate worktree |
| Spec enforced | GitHub issue #102 `## Acceptance`; `.local/102-vt-input-bounds-plan.md` `## Evidence matrix` |
| Host | darwin 25.5.0, aarch64 (M2 Pro), release builds |

There is no `docs/tasks/NNN-*.md` for this work, so the issue's Acceptance
section and the plan's Evidence matrix were used as the Definition of Done.

### The state this verdict applies to

**Confirmed against the commit.** All five blobs committed at `113b66c` hash
exactly to the pin below, so the condition attached to this verdict is satisfied:

```
git show 113b66c:crates/shux-vt/src/parser.rs        | shasum -a256 -> 3079b8f9...68e9ef85  MATCH
git show 113b66c:crates/shux-vt/src/cell.rs          | shasum -a256 -> e112c9bd...b52ed343  MATCH
git show 113b66c:crates/shux-vt/src/lib.rs           | shasum -a256 -> dd826131...eed59887  MATCH
git show 113b66c:crates/shux-vt/tests/input_bounds.rs| shasum -a256 -> 37bf9bb1...656b0d20  MATCH
git show 113b66c:Cargo.toml                          | shasum -a256 -> 1dcd27c2...5b5184cd  MATCH
```

Clean serial re-gate on `113b66c`: `make test` **1535 passed / 0 failed / 1
ignored across 30 binaries** (exit 0); `make lint` clean; **all 9** VT golden
gates pass; corpus goldens 0 changed pixels; 29 bounds tests.


`crates/shux-vt` moved four times during the audit as findings landed. Each move
was re-gated from scratch. **This PASS applies to exactly these blobs:**

```
3079b8f9597db78cb093d3d65054de293c4634247dd59e1e05d6e6a268e9ef85  crates/shux-vt/src/parser.rs
e112c9bd14810b982c3b88ab0466f440f349ecd23e3e77c5ee0f40a2b52ed343  crates/shux-vt/src/cell.rs
dd8261319e116495ff503c8109ac2f1e217eec6df2e0813b5f003aaeed59887b  crates/shux-vt/src/lib.rs
37bf9bb1e9fb33f5dce6f1417328655ac20ccd8bc7fa0a08f52c740656b0d20d  crates/shux-vt/tests/input_bounds.rs
1dcd27c227ca2baa05d111e97ddc3297a4c749cda9f404d376c5c687b5184cd6  Cargo.toml
```

At the time of writing, `parser.rs` and `input_bounds.rs` are **uncommitted** on
top of `7638470`. **If the committed blobs do not hash to the values above, this
verdict does not apply and the gate must be re-run.** Verify with:

```
git show <sha>:crates/shux-vt/src/parser.rs | shasum -a256
```

Audit trail of the states, all re-gated: `6ed9bc3` (as requested) → `78192c6`
(OSC 4 fix) → `7638470` (DCS latch + evidence refresh) → this state (council
findings closed).

## 2. Verdict

**PASS.** Every acceptance criterion in issue #102 is met, on evidence I
regenerated rather than inherited. Both required DootSabha councils exist and are
substantive. Every finding raised by this gate and by the implementation council
has been fixed, or documented and pinned by a test. Zero leaked processes.

Two findings I raised were significant and are recorded below because they
happened on this branch and shaped the outcome — one of them a defect inside
verification machinery. Both are closed.

One finding I described wrongly at first, and one I nearly over-rated. Both
corrections are recorded in §7, because a gate that quietly launders its own
mistakes is not a gate.

## 3. Issue #102 Acceptance matrix

| # | Acceptance criterion | Status | Evidence |
| --- | --- | --- | --- |
| A1 | CSI counts above the region height perform no more work than the region height | **PASS** | `ESC[65535S/T/L/M` ×100 → **2400** grid mutations vs **6 553 500** pre-fix (= 100 × 24 rows, exactly the clamp). §4.1 |
| A2 | Retained OSC/DCS state never exceeds its cap regardless of chunking | **PASS** | 200 MiB streamed in 1 MiB chunks into an open OSC → **+1 MB** RSS (pre-fix +203 MB); into an open DCS → **+0 MB** (pre-fix +202 MB). Cross-chunk down to `chunk=1`. §4.2, §6.1 |
| A3 | A valid sequence following an overflow still parses | **PASS** | `valid_dcs_after_overflow_still_parses`, `valid_osc_after_overflow_still_parses` green; both green pre-fix too, so they are true negative controls. |
| A4 | Grapheme payload never exceeds its cap; time linear in input up to the cap | **PASS** | 5 000→80 000 combining marks: **63 bytes flat**, 80.8 µs → 1.234 ms (16× input → 15× time). Pre-fix 10 001→160 001 bytes, 1.23 ms → 398 ms (16× input → 324× time). §4.3 |
| A5 | Caps sit well above anything a real TUI emits | **PASS** | Corpus scan of 10 recorded PTY streams: max OSC parameters **3**, max OSC payload **141 bytes**, against caps of 16 params / 4096 bytes. Reply budget re-sized to 512 after measurement showed a full 256-colour palette probe sat exactly on the old 256. §4.4 |
| A6 | Rich-TUI rendering is unchanged | **PASS** | `pane snapshot` **0 changed pixels** pre-fix vs post-fix at 80×24, 120×40, 200×60 at zero tolerance. Committed corpus goldens **0 changed pixels**. 90/90 identical replay grids. §5, §6.1 |

## 4. Independent measurements (my harness, not the implementer's)

Probe crates built against a `b521f2c` worktree and against the audited state,
driving `shux-vt` directly.

### 4.1 CPU amplification — 24×80 grid, 800-byte input (100 repeats)

| Sequence | pre-fix | audited state | pre mutations | post mutations |
| --- | --- | --- | --- | --- |
| `ESC[65535S` SU | 825.3 ms | **0.612 ms** | 6 553 500 | **2 400** |
| `ESC[65535T` SD | 804.1 ms | **0.310 ms** | 6 553 500 | **2 400** |
| `ESC[65535L` IL | 809.1 ms | **0.318 ms** | 6 553 500 | **2 400** |
| `ESC[65535M` DL | 818.9 ms | **0.252 ms** | 6 553 500 | **2 400** |
| REP, wide-CJK source | 97.5 ms | **3.18 ms** | 6 881 255 | 201 681 |
| REP, ZWJ-cluster source | 526.7 ms | **11.82 ms** | 13 435 155 | 394 081 |
| ICH / DCH / ECH (control) | 16–23 µs | 16–23 µs | 100 | 100 |

Sustained hostile rate needed to saturate one core rises from **~970 B/s to
~1.3 MB/s** for the CSI family.

### 4.2 Memory retention — 200 MiB streamed in 1 MiB chunks

| Case | pre-fix | audited state |
| --- | --- | --- |
| OSC opened, never terminated | +203 MB | **+1 MB** |
| DCS opened, never terminated | +202 MB | **+0 MB** |
| control (no control string open) | +9 MB | +11 MB |

DCS overflow throughput **445.9 MB/s** after the latch fix (was 284 MB/s when the
buffer cleared and re-grew).

### 4.3 Grapheme accumulation, one cell

| marks | pre-fix bytes / time | audited state |
| --- | --- | --- |
| 5 000 | 10 001 / 1.23 ms | **63 / 80.8 µs** |
| 20 000 | 40 001 / 12.5 ms | **63 / 303.9 µs** |
| 40 000 | 80 001 / 69.9 ms | **63 / 608.1 µs** |
| 80 000 | 160 001 / 398.1 ms | **63 / 1.234 ms** |

### 4.4 Reply budget vs legitimate bulk replies

Budget is **512 per batch**. Measured against the largest legitimate bursts:

| Workload | replies wanted | got |
| --- | --- | --- |
| 300 DSR cursor-position queries in one read | 300 | **300** |
| 40 OSC 4 sequences × 7 pairs | 280 | **280** |
| Full 256-colour palette probe | 256 | **256** |
| Full palette probe + DA/DA2/DSR/XTVERSION handshake | 260 | **260** |
| 5 000-query flood | 5 000 | 512 (cut by an order of magnitude) |

At the earlier 256 the palette probe sat *exactly* on the budget, so any
additional startup query would have clipped a valid probe. 512 clears it.

### 4.5 Semantic boundaries

| Probe | pre-fix | audited state |
| --- | --- | --- |
| OSC 8 URI 4094 bytes | intact | **intact** |
| OSC 8 URI 4095 bytes | intact | **DROPPED** |
| OSC 8 URI 8187 bytes | intact (unbounded) | **DROPPED** |
| OSC 8 URI, 0–12 semicolons | intact | **intact** |
| OSC 8 URI, 13+ semicolons | intact | **DROPPED** — see §7 F3 |
| OSC 8 semicolon flood / >16 named params | **stored as junk links** | **DROPPED** |
| Title 257 / 4096 / 65536 chars | 257 / 4096 / 65536 | **256 / 256 / 256** |
| IL/DL, cursor above scroll region | **65 535 mutations** | **0** (matches xterm) |
| IL/DL, cursor below scroll region | **131 070 mutations** | **0** |
| Huge-count vs region-height SU/SD/IL/DL | identical grids | identical grids |
| OSC 4, 1/4/7/8/9/16 pairs | 1/4/7/7/7/7 replies | **identical** |
| Plain-text throughput, best-of-7 | 88.0 MB/s | 90.6 MB/s |

**No silent-truncation window on OSC 8**: swept byte by byte from 4085 to 4100,
the transition is directly `intact(4094)` → `DROPPED(4095)`. That is the property
the whole OSC design hinges on.

### 4.6 Negative controls — the caps must not fire on real input

ZWJ family (7 scalars), flag, VS16 heart, Hebrew with marks: all preserved
byte-for-byte. Skin-tone and Devanagari normalise identically pre- and post-fix.
XTGETTCAP 1 reply, DECRQSS 1 reply, DA/DSR 5/5, OSC 10/11/12 3/3, normal title
stored, normal OSC 8 hyperlink intact, 3.8 KB legitimate link intact.
**Identical on both builds.**

## 5. Pixel-level verification

Produced by this audit with `.claude/automations/pixel_verify.py`, zero tolerance
(`--max-pixel-diff-ratio 0 --max-mean-channel-delta 0`).

### 5.1 Pre-fix vs post-fix, fixed session name (deterministic colour/Unicode/SGR/DEC/OSC-8 probe)

| Render path | 80×24 | 120×40 | 200×60 |
| --- | --- | --- | --- |
| `pane snapshot` | **0 changed px** (720×456) | **0 changed px** (1080×760) | **0 changed px** (1800×1140) |
| `window snapshot` | **0 changed px** (1080×684) | 0 changed px — not load-bearing | 0 changed px — not load-bearing |

Metrics: `.shux/qa/102/pixel-fixed-probe_*.json`.

**Caveat I am raising against my own evidence.** At 120×40 and 200×60 the
`window snapshot` images are near-empty on *both* builds, because of the
pre-existing compositor gap in §7 F5. Comparing two near-empty images proves
nothing, so only the **80×24 window cell is load-bearing** — it carries the full
probe and matches exactly. All three `pane snapshot` cells are load-bearing.

### 5.2 Committed goldens (baselines predate this change — not self-minted)

`make test-vt-corpus` replays committed `.shux/fixtures/vt-corpus/` streams
against committed `.shux/goldens/073-vt-corpus/` PNGs: every case
`changed_pixels: 0`, `pixel_diff_ratio: 0.0`, `mean_rgba_channel_delta: 0.0`,
including the real-TUI replays (btop, lazygit, nvim, vicaya, vivecaka) and the
synthetic wide-CJK and grapheme-storage cases. Report regenerated by this audit
and byte-identical to the committed one.

### 5.3 Rich-TUI visual inspection (PNGs opened as images, not merely asserted)

Captured at 80×24, 120×40, 200×60 through `pane snapshot` + `window snapshot`:
btop, htop, lazygit, nvim, vim, gitui, yazi, bat, vicaya-tui, vivecaka, and a
colour/Unicode/DEC/OSC-8/SGR probe — 66 PNGs in `.shux/out/102-qa/liveA/`.
Inspected at native resolution: btop 120×40, htop 200×60, lazygit 120×40, nvim
200×60, yazi 120×40, vivecaka 80×24, probe at all three sizes, isolation window.

No clipping, no colour bleed after SGR reset, no ghost cells, no bad wrapping, no
cursor artifacts; borders, titles and status bar intact; truecolor, 256-indexed
and basic colour all present; DEC line-drawing correct; braille sparklines and
syntax highlighting correct. CJK and regional-indicator glyphs render as
replacement boxes — pre-existing rasterizer font coverage, pinned by the
committed `synthetic-wide-cjk-expected.png` golden which passes at 0 changed
pixels.

## 6. Testing matrix

| Layer | Result |
| --- | --- |
| Unit + integration | **PASS** — 1535 passed, 0 failed, 1 ignored across 30 test binaries (`make test`, workspace, `--test-threads=1`) |
| Lint | **PASS** — clippy `-D warnings` + rustfmt |
| Red/Green TDD proof | **PASS** — regenerated, not inherited; §6.2 |
| Raw-byte / replay fixtures | **PASS** — 90/90 identical grids; §6.1 |
| Comparator sensitivity | **PASS** — provably able to fail; §6.1 |
| shux automation (daemon-backed, colour-probed) | **PASS** — 3 breakpoints × 11 real targets |
| Live daemon isolation A/B | **PASS** — §6.3 |
| Visual inspection | **PASS** — §5.3 |
| Pixel comparison | **PASS**, with the §5.1 caveat stated |
| Committed VT golden gates | **PASS** — 9 gates, serial |
| `make check-progress` / `make check-vt-qa` | **PASS** (exit 0) |
| DootSabha design council | **PASS** — `.local/102-council.json` |
| DootSabha implementation-diff council | **PASS** — `.local/102-impl-council.json`; §6.4 |

### 6.1 Replay equivalence, regenerated independently

10 raw PTY streams (5 committed `.shux/fixtures/vt-corpus/rich-tui/` + 5
`.shux/out/102/recordings/`) × 3 geometries × 3 chunk sizes (1, 4096, 65536 —
`chunk=1` is the worst case for cross-chunk parser state) = **90 cases, 90
identical**. Fingerprint covers every visible cell's char, fg, bg, flags,
grapheme payload and hyperlink, plus title, cursor and reply count.

Sensitivity check — the comparator must be able to fail:

| Hostile fixture | Result |
| --- | --- |
| `grapheme_long`, `il_outside_region`, `osc8_long`, `rep_huge`, `title_long` | **DIFFERENT** |
| `su_huge` | IDENTICAL — the designed property: clamping is visually invisible |
| `benign_control` | IDENTICAL |

### 6.2 Red/Green — regenerated

Copying the final `input_bounds.rs` onto pre-fix `b521f2c`: **16 failed** — every
bound-asserting test fails, every negative control passes. I also RED-verified
each later test against the exact commit it fixes, e.g. the OSC 4 regression test
against `6ed9bc3`:

```
OSC 4 with 8 pairs did not set palette_overridden;
lens gate would judge a non-portable capture portable
```

### 6.3 Live daemon isolation A/B — the issue's actual claim

Three panes in one daemon: a colour probe, a victim printing a tick every 50 ms,
and an attacker streaming 400×(`ESC[65535S`+`T`+`L`+`M`+`b`) then 20 MB of
unterminated OSC then 20 MB of unterminated DCS. Same script, same payload, same
10-second window, once per binary.

| | pre-fix `b521f2c` | audited state |
| --- | --- | --- |
| Victim ticks in 10 s | 14 → 87 = **73** (7.3/s) | 20 → 196 = **176** (17.6/s) |
| Fraction of unattacked rate | ~42 % | **~100 %** |
| Daemon RPC latency during attack | 6.0–9.9 ms | 6.4–8.5 ms |

Probe pane after the attack: colours, wide CJK, ZWJ, VS16, combining marks, flag,
OSC 8 link and DEC line-drawing all intact.
Honest caveat: `session list` does not take the `PaneIoState` lock, so RPC
latency was never the symptom and does not move much either way. The victim-pane
throughput is the real measurement, and it is a clean 2.4×.

### 6.4 DootSabha councils

**Design** — `.local/102-council.json`: chair claude, reviewers codex/agy/grok,
57 KB, 20 min, $0.72. Verdicts visibly folded into the plan.

**Implementation diff** (`b521f2c..78192c6`) — `.local/102-impl-council.json`:
chair claude, all three providers `ok`, 44 KB, 25 min, $0.87. It raised three
open findings. I adjudicated each by measurement rather than accepting or
dismissing them; all three are now closed (§7 F3, F4, and the reply budget in
§4.4). It also explicitly rejected several non-findings (stale `dcs_state` after
RIS, the grapheme rewrite, the IL/DL/SU/SD clamp math), which matches what my own
probes found.

## 7. Findings — all closed

### F1 — CLOSED — OSC 4 palette batches were silently voided (defect in verification machinery)

Commits `e059171`..`6ed9bc3` applied the OSC truncation-drop to **every** selector.
OSC 4 is variadic by spec (`OSC 4 ; c ; spec ; …`) and its handler loops over
`params[1..].as_chunks::<2>()`, so any batch of ≥8 pairs hit vte's 16-parameter
cap and was dropped whole. Measured on `6ed9bc3`:

| batched OSC 4 | `b521f2c` | `6ed9bc3` | now |
| --- | --- | --- | --- |
| 8 / 15 / 16 / 32 queries | 7 replies | **0 replies** | 7 replies |
| 8 batched sets → `palette_overridden` | `true` | **`false`** | `true` |

`palette_overridden` feeds `has_indexed_colors` in `gate_compare.rs`, the
non-portability signal for `shux lens gate`, so a genuinely non-portable capture
could have been judged portable.

Fixed in `78192c6` by scoping the drop to the OSC 8 arm, with a regression test
proved RED against `6ed9bc3`. Found independently by this gate and by an
adversarial agent.

### F2 — CLOSED — DCS overflow cleared and re-grew instead of latching

`put()` set `overflowed`, then cleared and shrank the buffer, so the next byte
started refilling — roughly 51 000 allocate/free cycles per 200 MiB. Retention
was always correctly bounded, so this was cost, not corruption. Fixed in
`7638470` with the latched early return. Measured improvement: **284 → 446 MB/s**
on unterminated-DCS throughput.

### F3 — CLOSED as a documented, unavoidable trade-off — 16-param OSC 8 false positive

A legitimate 109-byte OSC 8 URI containing 13 semicolons produces exactly 16
parameters with nothing lost, and is dropped. The implementation council rated
this its highest-priority open issue, and I was ready to rate it a blocking
regression.

**I verified the counter-argument against vte 0.15 directly before believing it,
and it holds.** A raw `vte::Perform` impl shows a complete 14-segment URI and a
truncated 30-segment one arrive as byte-identical dispatches:

```
A complete-14-segment : params=16 bytes=33 values=["8","","s0",…,"s13"]
B truncated-30-segment: params=16 bytes=33 values=["8","","s0",…,"s13"]
param_count identical: true   byte_total identical: true   values identical: true
```

Nothing at the `Perform` boundary can tell them apart, so the false positive is
genuinely unavoidable and the only choice is which error to make. Dropping
degrades a rare valid link to plain text; storing would put a wrong destination
under a user's cursor. Dropping is correct.

Now documented in `parser.rs` and pinned by
`osc8_semicolon_boundary_is_pinned_including_the_false_positive`, which asserts
intact round-trips up to 12 semicolons and the deliberate drop at 13. Boundary
measured: intact at 0–12 semicolons, dropped from 13.

### F4 — CLOSED — the vte param-cap drift guard could not detect the drift it claimed to guard

The original `vte_osc_param_cap_is_still_sixteen` probed OSC 8 at 15 and 16
parameters. Both assertions are satisfied by shux's *local* constant regardless
of vte's actual cap:

- vte cap raised to 32 → a complete 16-param sequence is still dropped locally →
  assertion passes → shux silently over-drops valid 16–32-param sequences.
- vte cap lowered to 8 → a 15-param request delivers 8 params, shux stores a
  **silently truncated** URI → `is_some()` passes → the exact failure mode the
  whole design exists to prevent is reintroduced undetected.

The test's doc comment claimed it pinned "both sides of the boundary". It did
not. This is a defect in verification machinery — the class CLAUDE.md singles out
("a gate that can be talked into passing is worth less than nothing") and the one
`verify your verifier` exists for.

Closed by `vte_osc_param_cap_observed_directly_via_osc4`, which sends 20 OSC 4
pairs through a selector with no parameter guard of its own and asserts exactly 7
replies. The arithmetic detects **both** directions: a cap of 32 yields 15
replies, a cap of 8 yields 3. Independently corroborated — my raw vte probe
reports `MAX_OSC_PARAMS observed = 16`.

### F5 — PRE-EXISTING, out of scope, not counted — `window snapshot` silently clips an oversized pane

**Correcting my own first description.** I originally attributed this to
`pane split`. That was wrong, and it is why it did not reproduce for the
implementer. A split alone renders correctly. The trigger is `pane set-size`
making the pane's grid **taller than the window's layout rect**; `window
snapshot` then clips from the TOP and keeps only the bottom rows, silently.

| pane size, then split | `pane capture` | `window snapshot` |
| --- | --- | --- |
| 100×24 (fits) | 8/8 markers | **all 8 markers render** |
| 120×40 (overflows) | 8/8 markers | only the last 2 lines |

Single pane, **no split**, 8 numbered coloured markers, 2 s settle, second
snapshot 3 s later:

| pane size | `pane capture` | `pane snapshot` | `window snapshot` | 3 s retry |
| --- | --- | --- | --- | --- |
| 80×24, 100×24 | 8/8 | full | **full** | identical |
| 118×38, 118×39, 120×40 | 8/8 | full | partial, worsening with rows | identical |
| 200×60 | 8/8 | full | almost nothing | identical |

Monotonic in pane rows and **byte-identical on the 3 s retry at every size**, so
this is a genuine render-path disagreement, **not** a settle/ordering race.
`pane snapshot` is correct at every size. Reproduces byte-identically on
`b521f2c`, so it is not caused by #102 and is **not counted against this gate**.

Worth its own issue: `pane set-size --help` advertises exactly this path ("when
you need the pane wider/taller than the daemon default"), so the documented
workflow for oversized panes silently loses content in `window snapshot`.

Repro scripts, both deterministic: `.shux/out/102-qa/qa102_window_size_sweep.sh`,
`.shux/out/102-qa/qa102_split_vs_size.sh`.

### F6 — CLOSED — committed evidence understated the work

`v1_RED_test_results.md` recorded 13 RED against a suite that was 16 RED, and
`v2_baseline_measurements.md` recorded REP only for a narrow source char (the
wide-CJK and ZWJ-cluster cases are 2.5× and 7× more expensive). Both refreshed in
`7638470`; the refreshed figures match my independent measurements.

## 8. Residual risk

1. **REP with a grapheme-cluster source** is the most expensive surviving path
   (~44 KB/s of hostile output to saturate a core, per the refreshed baseline).
   Bounded to one screenful and no worse than printing a screenful of emoji, but
   it is the number to watch.
2. **`VTE_MAX_OSC_PARAMS = 16` is mirrored, not imported.** Now guarded
   behaviourally in both directions (F4), but a vte upgrade still needs a
   deliberate look.
3. **`default-features = false` on vte is workspace-wide.** Verified at parity on
   aarch64 (memchr's NEON path is baseline there). x86_64 throughput parity is
   asserted, not measured — CI should confirm.
4. **Blank scrollback padding on huge `SU` is gone** — a deliberate,
   visible-screen-neutral behaviour change. Called out in the plan; make sure it
   reaches the PR description.
5. **A 13-semicolon OSC 8 URI silently loses its link** (F3). Unavoidable and
   correctly chosen, but it is a real behaviour difference from pre-fix and
   belongs in the PR description, not only in a code comment.
6. **F5** is a live `window.snapshot` correctness gap on `main` today.

## 9. Cleanup status

- Repo-scoped shux daemons after the audit: **none**.
- Repo-scoped orphan automation processes: **none**.
- Isolated runtime dirs `/tmp/sq102*`: **none left**.
- The user's two long-lived daemons (PIDs 7051, 97440, started Jul 31) were never
  touched. `pkill` was not used at any point.
- `make test-shux-leak-guard` passes — the guard itself was verified, not assumed.
- Two leak-guard trips during the audit, both investigated rather than waved off:
  one `target/debug/shux __daemon` caused by **my own** error (a background
  `make test` racing a daemon-backed gate), and one orphan `sleep 30` from a
  concurrent agent. Neither reproduced on a serial re-run; the serial re-runs
  exited 0 with zero new processes.
- Two files under `.shux/qa/074-*` were rewritten by re-running task 074's
  harness (absolute paths and timing only, `changed_pixels: 0` unchanged) and
  restored to their committed content via `git show`.

## 10. Condition on this verdict — SATISFIED

The audited blobs are committed unchanged at `113b66c` and hash-verified (§1).
The condition is met and this PASS stands for `113b66c`.

## 11. P0 on the BRANCH, outside the scope of this gate — do not push as-is

Discovered while re-gating, after the audited code was verified. It does not
affect the verdict on `113b66c`, but it must be fixed before this branch moves.

The branch tip `fix/vt-input-bounds` is **`9ff4242 "add golden"`** — author
`t <t@t>`, 2026-08-03 15:48:15 -0700 — **1101 files changed, 1 insertion,
236 294 deletions**. It deletes 1100 files (all of `crates/`, `.claude/`,
`docs/`, `spikes/`, …) and adds only `goldens/frame.capture.json`.

```
9ff4242 HEAD@{0}: commit: add golden                      <-- branch tip
4fbb5ad HEAD@{1}: commit: docs(102): record SOLID QA PASS
113b66c HEAD@{2}: fix(vt): widen reply budget, pin OSC 8 semicolon boundary
```

**Nothing is lost.** Every file is present on disk, the five audited blobs still
hash correctly, `113b66c` and `4fbb5ad` both exist, and nothing was pushed (no
`origin/fix/vt-input-bounds`). `4fbb5ad` is the real tip.

The `t <t@t>` identity points at a harness that creates a scratch git repo and
runs `git add -A && git commit`, executing against this worktree instead of its
temp dir. It landed at 15:48, the same minute `test-vt-tab-stops` reported a
spurious Error 1 and `check-progress`/`check-vt-qa` began exiting 2 — all
symptoms of the broken HEAD, not real failures (tab-stops passes on re-run).

A test harness that can commit to the working repository is the same class of
problem as a gate that can be talked into passing, and deserves the same
treatment. Recovery is the branch owner's to perform; this gate touched no git
state at any point.

### F7 resolution, verified against the real attack shape

Fixed by `f740f7c` (`git_command()` strips eight repository-location variables).
Branch recovered: tip `f740f7c`, 1100 files tracked, the wipe commit not
reachable, no `t@t` author reachable, and all five pinned VT blobs still hash
exactly to §1.

The root cause is better than my hypothesis and belongs on the record: it was not
a stray shell script but `crates/shux/src/gate/bless.rs` itself. `GIT_DIR`,
`GIT_WORK_TREE` and `GIT_INDEX_FILE` **override** `git -C <path>`, and git sets
them in hook subprocesses — so when the project's own `pre-push` hook ran
`make test`, a temp-repo test ran `git add -A && git commit -m "add golden"`
against the real repository. The same leak was in production `git_tree_is_dirty`,
the bless dirty-tree safety guard: under a hook it assessed whichever repository
the caller named rather than the golden dir's.

**I verified the fix by reproducing the attack against a disposable decoy repo,
never the real one.** Two of my own first attempts were inadequate and I discarded
them: the first ran zero tests (`-p shux --lib` — `shux` is a bin crate), so a
clean decoy proved nothing; the second set `GIT_DIR` + `GIT_WORK_TREE` +
`GIT_INDEX_FILE` all at the decoy, which is the wrong shape — pre-fix merely
failed a test rather than committing, so it would not have caught the real
signature.

The real incident shape is `GIT_DIR` alone pointing at the victim while the work
tree resolves to the test's temp dir. Decoy repo with 4 tracked files, 15 bless
tests:

| | tests | decoy HEAD | `add golden` | author `t@t` | tracked |
| --- | --- | --- | --- | --- | --- |
| pre-fix `e56e989` | 11 passed, **3 failed** | **MOVED** | **1** | **1** | **4 → 1** |
| post-fix `f740f7c` | **15 passed, 0 failed** | unchanged | 0 | 0 | 4 → 4 |

The pre-fix row reproduces the incident signature exactly — everything deleted,
one golden file added. So the probe is proven able to fail, and the fix
demonstrably closes it. Pre-existing on `b521f2c` and outside the pinned VT file
set, so the PASS on `113b66c` is unaffected.

### Note on a transient test failure during re-gating

The first `make test` on `113b66c` failed `pane_kill_reaps_only_that_pane_child`
with `pane.split -> not_found`. Cause: a **second `make test` running
concurrently** (PID 3087). Re-run serially 5×: 5/5 pass. Daemon-backed shux tests
must not be parallelised, per CLAUDE.md; this is what it looks like when they
are. The clean serial run reported above was taken after the concurrent run
drained.
