VERDICT: FAIL

# SOLID VT QA — issue #102, bound attacker-controlled pane input in `shux-vt`

Gate: `shux-vt-solid-qa`. Audit-only; no product source was edited by this gate.

## 1. What was audited

| | |
| --- | --- |
| Branch | `fix/vt-input-bounds` |
| Commits named by the requester | `e059171`, `49b75ff`, `61075c6`, `6ed9bc3` |
| HEAD at audit start | `6ed9bc3` (clean tree) |
| HEAD at audit end | `78192c6` — `fix(vt): scope the OSC truncation-drop to OSC 8 (#102)` |
| **State verified** | **`78192c6`** — committed mid-audit; its blobs hash-match the tree I tested |
| Baseline for every A/B | `b521f2c` (v0.46.0), built in a separate worktree |
| Spec enforced | GitHub issue #102 `## Acceptance`; `.local/102-vt-input-bounds-plan.md` `## Evidence matrix` |
| Host | darwin 25.5.0, aarch64 (M2 Pro), release builds |

There is no `docs/tasks/NNN-*.md` for this work, so the issue's Acceptance section
and the plan's Evidence matrix were used as the Definition of Done.

### State A pin

The working tree changed **during** this audit: the OSC truncation-drop was
narrowed from all OSC selectors to OSC 8 only, and a new test was added. Every
result below was re-derived against this exact state, which was then committed as
`78192c6` while the audit was finishing:

```
59e0306f9b70b74169217515532ce88358f84c4eb55857061283748a6016e303  crates/shux-vt/src/parser.rs
e112c9bd14810b982c3b88ab0466f440f349ecd23e3e77c5ee0f40a2b52ed343  crates/shux-vt/src/cell.rs
dd8261319e116495ff503c8109ac2f1e217eec6df2e0813b5f003aaeed59887b  crates/shux-vt/src/lib.rs
f70dcbde1779f54b74277328dda746ceef4c24e16cdd374b8dd7fc8de06383b9  crates/shux-vt/tests/input_bounds.rs
1dcd27c227ca2baa05d111e97ddc3297a4c749cda9f404d376c5c687b5184cd6  Cargo.toml
```

Re-confirmed unchanged at the end of the audit, and confirmed against the
committed blobs:

```
git show 78192c6:crates/shux-vt/src/parser.rs         | shasum -a256 -> 59e0306f...6016e303  MATCH
git show 78192c6:crates/shux-vt/tests/input_bounds.rs | shasum -a256 -> f70dcbde...06383b9  MATCH
```

So everything below applies to committed `78192c6`. Any re-gate must show these
same hashes, or be re-run.

## 2. Verdict

**FAIL** — on one gate item, not on the engineering.

The bounds work is strong and **every technical acceptance criterion in issue #102
is met**, with evidence I regenerated rather than inherited (§4, §5, §6). The
single blocker is:

- **P1-B** — **no DootSabha implementation-diff council exists for #102.** The
  *design* council does (`.local/102-council.json`, 57 KB, substantive, chaired by
  claude over codex/agy/grok) and its verdicts are visibly reflected in the plan.
  The implementation-diff council is required by CLAUDE.md Feature Protocol step 6
  ("BEFORE pushing") and by this gate's mandatory evidence layer 8. It is not a
  formality here: it is precisely the review that should have caught P1-A at
  `61075c6` instead of leaving it to an adversarial pass.

**P1-A cleared during the audit.** The commits originally named
(`e059171`..`6ed9bc3`) carried a defect in verification machinery that I
reproduced independently; `78192c6` fixes it correctly and adds a regression test
I proved RED against `6ed9bc3`. Recorded below for the branch history, no longer
blocking.

One council run clears this report. Nothing else does.

## 3. Issue #102 Acceptance matrix

| # | Acceptance criterion (issue #102) | Status | Evidence |
| --- | --- | --- | --- |
| A1 | CSI counts above the region height perform no more work than the region height | **PASS** | `ESC[65535S/T/L/M` ×100 → **2400** grid mutations post-fix vs **6,553,500** pre-fix (= 100 × 24 rows, exactly the clamp). §4.1 |
| A2 | Retained OSC/DCS state never exceeds its cap regardless of chunking | **PASS** | 200 MiB streamed in 1 MiB chunks into an open OSC → **+1 MB** RSS (pre-fix +203 MB); into an open DCS → **+0 MB** (pre-fix +202 MB). Cross-chunk down to `chunk=1` in §6.1. |
| A3 | A valid sequence following an overflow still parses | **PASS** | `valid_dcs_after_overflow_still_parses`, `valid_osc_after_overflow_still_parses` green; both were already green pre-fix, so they are true negative controls. |
| A4 | Grapheme payload never exceeds its cap; time linear in input up to the cap | **PASS** | 5 000→80 000 combining marks: retained **63 bytes flat**, time **80.8 µs → 1.206 ms** (16× input → 14.9× time). Pre-fix: 10 001→160 001 bytes, 1.23 ms → 398 ms (16× input → 324× time). §4.3 |
| A5 | Caps sit well above anything a real TUI emits | **PASS** | Corpus scan of 10 recorded PTY streams (btop, htop, lazygit, nvim, vim, vicaya, vivecaka): max OSC parameters **3**, max OSC payload **141 bytes** — against caps of 16 params / 4096 bytes. |
| A6 | Rich-TUI rendering is unchanged | **PASS** | Bit-exact: `pane snapshot` **0 changed pixels** pre-fix vs post-fix at 80×24, 120×40, 200×60 (zero tolerance). Committed VT corpus goldens **0 changed pixels**. 90/90 identical replay grids. §5, §6 |

## 4. Independent measurements (my harness, not the implementer's)

Two probe crates built against the pre-fix worktree and against state A, driving
`shux-vt` directly. Sources: `.../scratchpad/probe_pre`, `.../scratchpad/probe_post`.

### 4.1 CPU amplification — 24×80 grid, 800-byte input (100 repeats)

| Sequence | pre-fix time | post-fix time | pre-fix mutations | post-fix mutations | B/s to saturate a core (pre → post) |
| --- | --- | --- | --- | --- | --- |
| `ESC[65535S` SU | 825.3 ms | **0.578 ms** | 6 553 500 | **2 400** | 969 → 1 384 282 |
| `ESC[65535T` SD | 804.1 ms | **0.747 ms** | 6 553 500 | **2 400** | 995 → 1 071 607 |
| `ESC[65535L` IL | 809.1 ms | **0.428 ms** | 6 553 500 | **2 400** | 989 → 1 870 251 |
| `ESC[65535M` DL | 818.9 ms | **0.288 ms** | 6 553 500 | **2 400** | 977 → 2 773 367 |
| REP after a wide char | 97.5 ms | **3.18 ms** | 6 881 255 | 201 681 | 11 282 → 346 507 |
| REP after a ZWJ cluster | 526.7 ms | **11.82 ms** | 13 435 155 | 394 081 | 4 936 → 219 964 |
| ICH / DCH / ECH (control) | 16–23 µs | 16–23 µs | 100 | 100 | unchanged, as expected |

Note the **ZWJ-source REP** row: it is the true worst case and it is **not**
recorded in `.shux/out/102/v2_baseline_measurements.md`, which reports only the
narrow-char REP at 1.9 ms. Post-fix it is bounded (one screenful), but at
~220 KB/s of hostile pane output to saturate a core it remains the most
expensive surviving amplifier. Bounded and defensible — an attacker gets the same
cost by printing a screenful of emoji — but it should be the number on record.

### 4.2 Memory retention — 200 MiB streamed in 1 MiB chunks

| Case | pre-fix | post-fix |
| --- | --- | --- |
| OSC opened, never terminated | +203 MB | **+1 MB** |
| DCS opened, never terminated | +202 MB | **+0 MB** |
| control (no control string open) | +9 MB | +11 MB |

DCS-overflow throughput 282 MB/s — the clear-and-regrow overflow path is not a
CPU sink (see P3-A).

### 4.3 Grapheme accumulation, one cell

| marks | pre-fix bytes / time | post-fix bytes / time |
| --- | --- | --- |
| 5 000 | 10 001 / 1.23 ms | **63 / 80.8 µs** |
| 10 000 | 20 001 / 4.02 ms | **63 / 164.7 µs** |
| 20 000 | 40 001 / 12.5 ms | **63 / 303.9 µs** |
| 40 000 | 80 001 / 69.9 ms | **63 / 606.5 µs** |
| 80 000 | 160 001 / 398.1 ms | **63 / 1.206 ms** |

### 4.4 Response amplification

8 KiB read of `ESC[6n` (2048 queries): **2048 replies / 12 288 reply bytes** pre-fix
→ **256 replies / 1 536 reply bytes** post-fix. Verified the cap is **per batch,
not cumulative**: the next `process_with_responses` call still answers
(`responses` is a fresh `Vec` per call, `lib.rs:225`).

### 4.5 Semantic boundaries (state A)

| Probe | pre-fix | post-fix (state A) |
| --- | --- | --- |
| OSC 8 URI 4094 bytes | stored intact | **stored intact** |
| OSC 8 URI 4095 bytes | stored intact | **DROPPED** |
| OSC 8 URI 8187 bytes | stored intact (unbounded) | **DROPPED** |
| Window title 257 / 4096 / 65536 chars | 257 / 4096 / 65536 stored | **256 / 256 / 256** |
| IL/DL, cursor above scroll region | **65 535 mutations** | **0** (matches xterm) |
| IL/DL, cursor below scroll region | **131 070 mutations** | **0** |
| `SU/SD/IL/DL` huge count vs region-height count | identical grids | identical grids |
| Plain-text throughput, best-of-7 | 88.0 MB/s | 90.6 MB/s |

There is **no silent-truncation window** on OSC 8: the transition is directly
`intact(4094)` → `DROPPED(4095)`, swept byte by byte from 4085 to 4100. That is
the property the whole OSC design hinges on, and it holds exactly.

### 4.6 Negative controls — the caps must not fire on real input

`zwj family` (7 scalars), `flag`, `vs16 heart`, `hebrew+marks` all preserved
byte-for-byte; `skin tone` and `devanagari` normalise identically pre- and
post-fix (pre-existing behaviour, unchanged). `XTGETTCAP` 1 reply, `DECRQSS`
1 reply, `DA/DSR` 5/5 replies, `OSC 10/11/12` 3/3 replies, normal title stored,
normal OSC 8 hyperlink stored intact. **Identical on both builds.**

## 5. Pixel-level verification

All produced by this audit with `.claude/automations/pixel_verify.py`, zero
tolerance (`--max-pixel-diff-ratio 0 --max-mean-channel-delta 0`).

### 5.1 Pre-fix vs post-fix, fixed session name (deterministic colour/Unicode/SGR probe)

| Render path | 80×24 | 120×40 | 200×60 |
| --- | --- | --- | --- |
| `pane snapshot` | **0 changed px** (720×456) | **0 changed px** (1080×760) | **0 changed px** (1800×1140) |
| `window snapshot` | **0 changed px** (1080×684) | 0 changed px — *see caveat* | 0 changed px — *see caveat* |

Metrics: `.shux/qa/102/pixel-fixed-probe_*.json`.
Captures: `.shux/out/102-qa/fixed-pre/`, `.shux/out/102-qa/fixed-post/`.

**Caveat I am flagging against my own evidence.** At 120×40 and 200×60 the
`window snapshot` images are near-empty on *both* builds — the window compositor
does not render a pane whose grid exceeds the window's layout rect (pre-existing;
see P3-C). Comparing two near-empty images proves nothing, so only the **80×24
window cell is load-bearing**; it carries the full probe and matches exactly. The
`pane snapshot` cells are load-bearing at all three sizes.

### 5.2 Committed goldens (baselines predate this change — not self-minted)

`make test-vt-corpus` replays committed `.shux/fixtures/vt-corpus/` streams
against committed `.shux/goldens/073-vt-corpus/` PNGs: every case
`changed_pixels: 0`, `pixel_diff_ratio: 0.0`, `mean_rgba_channel_delta: 0.0`,
including the real-TUI replays (btop, lazygit, nvim, vicaya, vivecaka) and the
synthetic wide-CJK / grapheme-storage cases. Report regenerated by this audit and
byte-identical to the committed one (`git status` clean afterwards).

### 5.3 Rich-TUI visual inspection (opened as images, not just asserted)

Captured at 80×24, 120×40, 200×60 through shux `pane snapshot` + `window snapshot`:
btop, htop, lazygit, nvim, vim, gitui, yazi, bat, vicaya-tui, vivecaka, and a
colour/Unicode/DEC/OSC-8 probe — 66 PNGs, `.shux/out/102-qa/liveA/`.
Inspected: btop 120×40, htop 200×60, lazygit 120×40, nvim 200×60, yazi 120×40,
vivecaka 80×24, probe 80×24/120×40/200×60, isolation window.
No clipping, no colour bleed after SGR reset, no ghost cells, no bad wrapping, no
cursor artifacts, borders and status bar intact, truecolor/256-indexed/basic all
present. CJK and regional-indicator glyphs render as replacement boxes — that is
pre-existing rasterizer font coverage, pinned by the committed
`synthetic-wide-cjk-expected.png` golden which passes at 0 changed pixels.

## 6. Testing matrix

| Layer | Result | Evidence |
| --- | --- | --- |
| Unit + integration | **PASS** — 397 tests, 0 failures (`make test`) | re-run on state A |
| Lint | **PASS** — clippy `-D warnings` + rustfmt | `make lint` on state A |
| Red/Green TDD proof | **PASS, and stronger than documented** — see §6.2 | regenerated, not inherited |
| Raw-byte / replay fixtures | **PASS** — 90/90 identical grids | §6.1 |
| Comparator sensitivity | **PASS** — comparator provably able to fail | §6.1 |
| shux automation (daemon-backed, colour-probed) | **PASS** — 3 breakpoints × 11 targets | `.shux/out/102-qa/liveA/` |
| Live daemon isolation A/B | **PASS** | §6.3 |
| Visual inspection | **PASS** | §5.3 |
| Pixel comparison | **PASS** (with the §5.1 caveat stated) | §5.1, §5.2 |
| Committed VT golden gates | **PASS** — 9 gates | §6.4 |
| `make check-progress` / `make check-vt-qa` | **PASS** (exit 0) | run on state A |
| DootSabha design council | **PASS** | `.local/102-council.json` |
| DootSabha implementation-diff council | **FAIL — absent** | P1-B — the only blocker |

### 6.1 Replay equivalence, regenerated independently

10 raw PTY streams (5 committed `.shux/fixtures/vt-corpus/rich-tui/` + 5
`.shux/out/102/recordings/`) × 3 geometries (80×24, 120×40, 200×60) × 3 chunk
sizes (1, 4096, 65536 — `chunk=1` is the worst case for cross-chunk parser state)
= **90 cases, 90 identical**. Fingerprint covers every visible cell's char, fg,
bg, flags, grapheme payload and hyperlink, plus title, cursor and reply count.

Sensitivity check — the comparator must be able to fail:

| Hostile fixture | Result |
| --- | --- |
| `grapheme_long` (100 combining marks) | **DIFFERENT** |
| `il_outside_region` | **DIFFERENT** |
| `osc8_long` (6000-byte URI) | **DIFFERENT** |
| `rep_huge` | **DIFFERENT** |
| `title_long` (5000 chars) | **DIFFERENT** |
| `su_huge` | IDENTICAL — the designed property: clamping is visually invisible |
| `benign_control` | IDENTICAL |

### 6.2 Red/Green — regenerated, and the record is stale

Copying the **final** `input_bounds.rs` onto pre-fix `b521f2c` and running it:
**16 failed, 9 passed** — every bound-asserting test fails, every negative control
passes. `.shux/out/102/v1_RED_test_results.md` records **13 failed, 8 passed**; it
was written at `e059171` and never refreshed after `49b75ff` / `61075c6` / state A
added four more tests. The committed record therefore *understates* the proof.

I also RED-verified the newest test on its own target: copying state A's
`input_bounds.rs` onto committed `6ed9bc3` fails with

```
OSC 4 with 8 pairs did not set palette_overridden;
lens gate would judge a non-portable capture portable
```

— 25 passed, 1 failed. The test is proven to catch the defect it fixes.

### 6.3 Live daemon isolation A/B — the issue's actual claim

Three panes in one daemon: a colour probe, a victim printing a tick every 50 ms,
and an attacker streaming 400×(`ESC[65535S`+`T`+`L`+`M`+`b`) then 20 MB of
unterminated OSC then 20 MB of unterminated DCS. Same script, same payload, same
10-second measurement window, once per binary.

| | pre-fix `b521f2c` | post-fix state A |
| --- | --- | --- |
| Victim ticks in 10 s | 14 → 87 = **73** (7.3/s) | 20 → 196 = **176** (17.6/s) |
| Fraction of unattacked rate | ~42 % | **~100 %** |
| Daemon RPC latency during attack | 6.0–9.9 ms | 6.4–8.5 ms |

Probe pane after the attack: colours, wide CJK, ZWJ, VS16, combining marks, flag,
OSC 8 link and DEC line-drawing all intact.
Honest caveat: `session list` does not take the `PaneIoState` lock, so RPC latency
was never the symptom and does not move much either way. The victim-pane
throughput is the real measurement, and it is a clean 2.4×.

### 6.4 Committed VT golden gates, run serially on state A

`test-vt-corpus`, `test-vt-corpus-unit`, `test-vt-wide-invariants`,
`test-vt-wide-visual`, `test-vt-grapheme`, `test-vt-dec-special-graphics`,
`test-vt-tab-stops`, `test-vt-origin-mode`, `test-vt-dirty-regions`,
`test-vt-resize-reflow` — **all pass**, `git status` clean afterwards (regenerated
evidence byte-identical to committed).

## 7. Findings

### P1-A — CLEARED by `78192c6`; recorded because it happened on this branch

On committed `6ed9bc3`, `osc_payload_was_truncated` was applied to **every** OSC
selector. OSC 4 is variadic by spec (`OSC 4 ; c ; spec ; c ; spec ; … ST`) and its
handler loops over `params[1..].as_chunks::<2>()`, so any batch of ≥8 pairs hits
vte's 16-parameter cap and was dropped whole. Measured by me on `6ed9bc3`, before
seeing any fix:

| batched OSC 4 pairs | pre-fix `b521f2c` | committed `6ed9bc3` | state A |
| --- | --- | --- | --- |
| 7 queries | 7 replies | 7 replies | 7 replies |
| 8 / 15 / 16 / 32 queries | 7 replies | **0 replies** | 7 replies |
| 8 batched sets → `palette_overridden` | `true` | **`false`** | `true` |

`palette_overridden` feeds `has_indexed_colors` in `gate_compare.rs`, the
non-portability signal for `shux lens gate`. On `6ed9bc3` a genuinely
non-portable capture could be judged portable — a defect *inside* the
verification machinery, which CLAUDE.md singles out as the worst place for one.

`78192c6` fixes it correctly — scope the drop to OSC 8, where truncation changes a
URI's *destination*; everywhere else truncation only loses trailing content — and
adds `osc4_palette_batches_survive_the_truncation_guard`, which I proved RED
against `6ed9bc3`. The committed blobs hash-match the state I measured, so this is
**closed**. Worth a line in the PR description: the branch briefly shipped a
lens-gate signal regression and fixed it before merge.

### P1-B — no DootSabha implementation-diff council for #102

`.local/102-council.json` is a real, substantive **design** council (chair claude,
reviewers codex/agy/grok, 57 KB, 20 min, $0.72) and its verdicts are visibly
reflected in the plan. There is **no implementation-diff council** anywhere in the
repo for this change. CLAUDE.md Feature Protocol step 6 requires one before
pushing, and this gate's evidence layer 8 requires it. It is not a formality here:
an implementation-diff review of the OSC guard is precisely what would have caught
P1-A at `61075c6` instead of after the fact.

**To clear:** run `dootsabha council` on the implementation diff (config from
`~/.config/dootsabha/config.yaml`, no CLI overrides), address or record its
findings, and store the output where the manifest can reference it.

### P2-A — committed evidence in `.shux/out/102/` is stale and understates the work

1. `v1_RED_test_results.md` says 13 RED; the final suite is 16 RED / 9 green
   negative controls at `b521f2c` (§6.2).
2. `v2_baseline_measurements.md` reports REP only for a narrow source char
   (1.9 ms). The wide-source (3.18 ms) and ZWJ-cluster-source (11.82 ms) cases are
   the real worst case and are absent (§4.1).
3. Neither file mentions the OSC 4 behaviour that P1-A concerns.

Not a correctness problem — the underlying claims are true and, where I could
re-derive them, stronger than written. But a reader of the committed record gets a
weaker and now partly wrong picture.

### P2-B — `.shux/qa/102/` was absent before this audit

This report, `evidence-manifest.json` and the pixel-metric JSONs are written by
this gate. They are **untracked** until committed. Note the standing tension
between this gate's artifact contract (which wants a full-resolution PNG committed
under `.shux/qa/<task>/`) and CLAUDE.md (which says PNGs stay scratch and go to PR
comments unless they are approved durable baselines). I followed **CLAUDE.md**:
PNGs stay under `.shux/out/102-qa/`, and the manifest records their paths, sizes
and sha256 so the set is auditable without committing binaries. If a committed PNG
baseline is wanted, that needs explicit DootSabha design-review approval per
CLAUDE.md — it is not this gate's call to mint one.

### P3-A — DCS overflow clears and re-grows instead of latching

`parser.rs` `put()`: on overflow it sets `overflowed`, then `clear()` +
`shrink_to_fit()`. The next byte sees `len() == 0` and starts refilling, so the
buffer cycles 0→4096→0 for the rest of the sequence (~51 000 cycles per 200 MiB).
Retention is still correctly bounded (measured +0 MB) and throughput is fine
(282 MB/s), so this is cosmetic. An early `if dcs.overflowed { return; }` would
make the intent obvious and drop the churn.

### P3-B — a legitimate 16-parameter OSC 8 is dropped

`params.len() >= VTE_MAX_OSC_PARAMS` fires at exactly 16 even when vte truncated
nothing (`osc_num_params` saturates at 16, so "complete 16" and "truncated ≥17"
are indistinguishable). The code comment calls the 16-param case "truncated",
which is not strictly accurate. Failing closed is right and OSC 8 uses three
parameters in practice; only the comment overstates.

### P3-C — PRE-EXISTING, not caused by #102: `window snapshot` drops pane content

Reproduced **byte-identically on `b521f2c` and state A**, so it is out of scope
here and is not counted against this gate. Two triggers:

1. After `pane split`, the split pane renders blank in `window snapshot` while
   `pane snapshot` and `pane capture` on that same pane at the same instant return
   its full content. Evidence: `.shux/out/102-qa/splitprobe/` vs
   `.shux/out/102-qa/splitprobe-prefix/`.
2. When a pane's grid exceeds the window's layout rect (`pane set-size --cols 200
   --rows 60` inside a 120×40 window), `window snapshot` renders it blank; at
   120×40 it renders only the last row. `pane snapshot` is correct at every size.
   Evidence: `.shux/out/102-qa/fixed-post/probe_{120x40,200x60}_window.png` vs the
   corresponding `_pane.png`.

A render-path disagreement between `pane.snapshot` and `window.snapshot` deserves
its own issue.

## 8. Passed evidence (summary)

- 397 workspace tests, clippy + rustfmt clean, on state A.
- 26/26 `input_bounds` tests green on state A; 16 of them proven RED at `b521f2c`;
  the newest proven RED at `6ed9bc3`.
- 6 vectors independently measured pre/post: SU, SD, IL, DL, REP (narrow/wide/ZWJ),
  OSC retention, DCS retention, grapheme accumulation, response amplification.
- 90/90 identical replay grids, with a sensitivity check proving the comparator can fail.
- 9 committed VT golden gates pass serially; corpus PNG comparison exact.
- `pane snapshot` bit-identical pre/post at 80×24, 120×40, 200×60.
- Live 3-pane daemon A/B: neighbouring-pane throughput restored 7.3/s → 17.6/s.
- Plain-text throughput 88.0 → 90.6 MB/s (no regression from no-std vte).

## 9. Residual risk

1. **REP with a grapheme-cluster source** is the most expensive surviving path
   (~220 KB/s of hostile output saturates a core). Bounded to one screenful and no
   worse than printing a screenful of emoji, but it is the number to watch.
2. **`VTE_MAX_OSC_PARAMS = 16` is mirrored, not imported.** A vte upgrade that
   moves it changes the OSC 8 drop rule. `vte_osc_param_cap_is_still_sixteen` is
   the drift guard; keep it.
3. **`default-features = false` on vte is workspace-wide.** Verified at parity on
   aarch64 (memchr's NEON path is baseline there); x86_64 throughput parity is
   still only asserted, not measured. CI should confirm.
4. **Blank scrollback padding on huge `SU` is gone** — a deliberate, visible-screen-
   neutral behaviour change. It is called out in the plan; make sure it reaches the
   PR description.
5. **P3-C** is a live `window.snapshot` correctness gap on `main` today.

## 10. Cleanup status

- Repo-scoped shux daemons after the audit: **none** (`shux_daemon_pids` empty).
- Repo-scoped orphan automation processes: **none** (`orphan_candidate_pids` empty).
- Isolated runtime dirs `/tmp/sq102*`: **none left**.
- The user's two long-lived daemons (PIDs 7051, 97440, started Jul 31) were never
  touched. No `pkill` was used at any point.
- `make test-shux-leak-guard` passes — the guard itself was verified, not assumed.
- One leak-guard trip mid-audit (`target/debug/shux __daemon`, PID 94924) was
  **my** error: I had a background `make test` running alongside a daemon-backed
  gate. Re-run serially, it passed with zero leaks. Recorded because a gate that
  hides its own process mistakes is not a gate.
- `.shux/qa/074-.../{dirty-120x30-pixel,performance}.json` were rewritten by
  re-running task 074's harness (absolute paths + timing only, `changed_pixels: 0`
  unchanged); both restored to their committed content via `git show`.

## 11. What a PASS requires

1. **Run `dootsabha council` on the implementation diff** (`b521f2c..78192c6`),
   using `~/.config/dootsabha/config.yaml` with no CLI agent/chair/model overrides.
   Address or record its findings, and store the output where the manifest can
   reference it. **This is the only blocker.**
2. Refresh `.shux/out/102/v1_RED_test_results.md` (16 RED, not 13) and add the
   wide-source and ZWJ-cluster-source REP numbers to `v2_baseline_measurements.md`.
3. Commit `.shux/qa/102/` so the evidence is tracked.
4. Re-gate. If `crates/shux-vt/**` or `Cargo.toml` change again, the §1 hashes must
   be re-derived and §4–§6 re-run; if they do not, this report stands as-is.

Item 1 is the only one that needs thinking time. Items 2–4 are mechanical.
