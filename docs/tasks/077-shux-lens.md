# Task 077: shux lens — give every agent eyes

**Status:** In Progress — P5 scratch sessions + `lens.run` IMPLEMENTED on `feat/lens-p5-scratch-run` (branched from `origin/main` at the v0.41.0 release commit, includes P4 #91 "pane.checkpoint + pane.diff_since"); `make test-lens` **35 passed / 2 failed** — **R1–R8 all green (8/8)**; K1/E1 both red at the identical root cause (`golden not found`, not a functional failure — see P5 notes below for the full E1 trace and the pending golden-mint decision). P0–P4 all Done and merged: P0 red suite (ff688a8, PR #86), P1 ContentRevision substrate (542101f, PR #87, v0.38.0), P2 pane.glance (0e6d611, PR #89, v0.39.0), P3 pane.wait_settled (3914dbb, PR #90, v0.40.0), P4 pane.checkpoint + pane.diff_since (80ffce8, PR #91, v0.41.0; gate 27/10 — D1–D5+A1 green, R1–R8/K1/E1 untouched). Full per-phase detail preserved in the P0–P4 sections below (P0 council rounds, P2 G1/F3 sync-wrap + OSC reclassification, P3 cancellable-RPC + PeerDeathMonitor, P4 invalidation-gating + LENS-R-038b). Awaiting: independent verifier + dootsabha convergence review + E1-golden adjudication before this phase can ship.
**Priority:** High
**Milestone:** M3
**Depends On:** 016, 017, 060, 064, 074
**Touches:** `.shux/fixtures/lens/`, `crates/shux/tests/lens_*`, `crates/shux/src/` (P1+), `scripts/check-lens-frozen.sh`, `Makefile`, `lefthook.yml`

> **Normative PRD (gitignored, outside the worktree):**
> `/Users/indrasvat/orca/workspaces/indrasvat-shux/spiderfish/.local/20260704-2326-shux-lens-PRD.md`
> Doc ID `LENS-PRD-20260704`. This task file mirrors that PRD; the PRD wins on
> any conflict (§0 precedence: §3 Decisions > SPEC > TEST > prose).

---

## Problem

Coding agents build and drive terminal apps blind: text capture cannot see
color, alignment, focus, or glyph width. shux's deterministic embedded
rasterizer is the only engine that can close the loop. `lens` exposes it as an
agent loop: **run** (hidden self-cleaning pane) → **settle** → **glance**
(pixels + text of one frame) → fix → **diff** (prove what changed), with PNG
proof.

Five NEW RPC methods: `pane.glance`, `pane.wait_settled`, `pane.checkpoint`,
`pane.diff_since`, `lens.run`; plus two FIELD extensions (`session.create`
scratch params — superseded by DEC-21: scratch is created only by `lens.run`;
and `session.snapshot` pane entries gain `content_revision`). CLI mirrors RPC
1:1. Nothing else (§2.1b verb map).

## Testing Matrix (mirrors PRD §15 — one PR per phase, strict order)

| Phase | Scope | Green gate | Extra DoD |
|---|---|---|---|
| **P0** | Fixtures + entire red suite + stubs (this task, current) | ALL §12 tests fail `method_not_found` / missing field (red receipt); fixture smoke tests green | PRD council convergence; cross-arch PNG spike (RESOLVED: shared goldens, §17); red receipt embedded; this task file |
| **P1** _(Done)_ | ContentRevision substrate (§4) | G3, G4 via `session.snapshot` + unit mutation-class table | no render-path behavior change (existing goldens byte-stable) |
| **P2** _(Implemented; gate 15/22 green — G1/G2/G2w all pass)_ | `pane.glance` (§5) | G1, G2, G2w + determinism micro-test | SOLID VT QA (glance); goldens approved §16.3 (PROVISIONAL, pending BASELINE-APPROVAL) |
| **P3** _(Implemented; gate 21/16 green — S1–S5, V1 all pass)_ | `pane.wait_settled` (§6) | S1–S5, V1 (incl. 100× S2) | — |
| **P4** _(Done)_ | checkpoints + `pane.diff_since` (§7) | D1–D5, A1 + attached-client concurrency | SOLID VT QA (heat) |
| **P5** _(In Progress)_ | scratch + `lens.run` (§8, §9) | R1–R8 | audit entries asserted; serial-only |
| **P6** | skill rewrite + CLI polish + T-tier + demo (§10, §13) | K1, E1, T1–T4; T5 demo evidence | shux-tui-qa PASS; clean-env skill test |

Dependencies are strict: P2–P4 require P1. Do not parallelize phases; do not
implement during P0.

## Acceptance Criteria (per-phase green gates)

- **P0:** every `crates/shux/tests/lens_*` synthetic test FAILS rooted in a
  missing RPC method (`-32601`) or a missing result field; `lens_fixtures_smoke`
  is GREEN; frozen-path guard + Makefile lane wired; `make check` clean (the red
  suite lives in a `test = false` lane, so `make test` does not run it).
- **P1:** G3, G4 green (revisions read via `session.snapshot`, LENS-R-006).
- **P2:** G1, G2, G2w green (CLI + RPC).
- **P3:** S1–S5, V1 green (S2 is the 100× flake gate).
- **P4:** D1–D5, A1 green.
- **P5:** R1–R8 green under `no_leak_guard.sh`, serial.
- **P6:** K1, E1, T1–T4 green; T5 unaided-agent demo evidence.

## Definition of Done (per PRD phase DoDs)

Every phase implicitly includes: `make check` clean · leak-guard serial run
clean · no frozen red-suite file modified without the `LENS-TEST-CHANGE:`
trailer (§16.2) · `docs/PROGRESS.md` + this Status updated · **converging
dootsabha review per §2.4 (REQUIRED)**. Phase-specific DoDs are the §4–§10 DoD
checklists in the PRD.

## P0 deliverables (this phase)

- Fixtures `.shux/fixtures/lens/f1..f10` (§11): POSIX sh + printf, token-handshake
  paced (no sleeps), shellcheck-clean, truecolor + 256 + basic content each.
- Fixture smoke tests (`lens_fixtures_smoke.rs`) — GREEN, existing machinery only.
- Red suite `crates/shux/tests/lens_*.rs` — 27 synthetic tests (G1,G2,G2w,G3,G4 ·
  S1–S5,V1 · D1–D5 · A1 · R1–R8 · K1 · E1) + RPC twins where marked ⇄.
- T-tier scaffolding (§13): `t/make_nidhi_repo.sh`, `t/demo-app/` (seeded border
  break at col 80), tests T1–T4 (loud-skip when `nidhi`/`vivecaka` absent).
- `scripts/check-lens-frozen.sh` (§16.2) + lefthook `commit-msg` wiring +
  Makefile `check-lens-frozen` / `test-lens` / `test-lens-t`.
- Red receipt: `make test-lens` output captured to `.shux/out/lens-p0/`.

## Mid-flight deltas applied (PRD convergence council)

1. F4 `s`-before-`a` documented NO-OP. 2. F7 `while :; do read -r _ || :; done`
loop (SIGWINCH-interrupt-proof). 3. D4 resequenced `a → settle → checkpoint → s
→ settle → diff`. 4. Explicit repo-relative fixture paths only. 5. glance
`evicted_revision`; zero-delta diff `bounding_box`=0 / `regions_truncated`=false;
`SPAWN_FAILED (-32014)`; FIFO eviction. 6. Scratch is created only by `lens.run`.

## P0 phase-diff council round 1 (2026-07-05) — hardening applied

1 blocker + 9 majors + 4 minors adjudicated (PRD §A1). Applied: S3 per-check
pump lifetimes (no false-green window) · harness NO_COLOR removed, color cases
assert non-grayscale · CLI twins completed (G1 50/50 split, G2/G2w full-field +
--png file, D1/D2 successful-path diff + --heat file, D3/R5 json error
envelopes, R1 CLI-scratch reap, R3 CLI size path) · D2 byte-exact full-width
rows · G4 session+pane structural versions · NEW tests D5/V1/R8 (count 24→27) ·
frozen guard uses interpret-trailers --parse, HEAD fallback, first-parent merge
diffs · make_nidhi_repo pins commit.gpgsign=false · F2 drains post-READY ·
classify_frame validates exact RGB · G1 single-decode · D-tests assert
from/to_revision. Hardening exposed a real fixture bug: PTY echo of token
newlines corrupted token-paced frames — all token-paced fixtures now set
`stty -echo` (like F4).

## P0 phase-diff council round 2 (2026-07-05) — hardening applied

3 majors adopted (PRD §A1 round-2 entry): (1) EOF busy-spin — the PRD's own
`while :; do read || :; done` prescription spun at 100% CPU on EOF; F2 (and the
F1/F5 blockers) now drain via `cat >/dev/null`, F7 uses the signal-safe
`while read -r _ || [ $? -gt 128 ]; do :; done` (WINCH-interrupt continues, EOF
exits), F4's dd loop breaks on empty read; F2/F7 smoke tests prove
signal-survival and EOF-exit with zero residual processes. (2) G1 pump loops on
a shared done-flag set after all glance threads join (outlives the slowest
glance); 10k-token cap + 120s deadline are panic bounds only; joins collected
non-panicking so the flag is always stored. (3) R8 CLI twin repeats the RPC
twin's daemon-state assertions (zero residual scratch + health).

## P0 phase-diff council round 3 (2026-07-05) — micro-fixes applied

Codex CONVERGED (1 minor) + 1 live-found robustness bug: (1) count_procs
substring match false-matched co-tenant processes whose argv merely mentioned a
fixture filename (proven A/B under a parallel dootsabha run: 8/29 vs 10/27) —
fixture spawns now use the absolute repo-root-anchored path and
count_fixture_procs matches argv anchored at start (`sh <abs>/…/<script>`).
(2) F4's empty-read-as-EOF conflation made explicit: normative input contract
(a/s/Tab only; LF/NUL never sent) added to the fixture header and smoke test.

## P2 implementation notes (`pane.glance`, this branch: `feat/lens-p2-glance`)

**Delivered:** `pane.glance` RPC (`crates/shux/src/main.rs`, registered in
`register_pane_io_methods`, next to `pane.snapshot`) + `shux pane glance
<pane> [--png PATH] [--text-only] [--no-cursor] [--checkpoint]` CLI
(`crates/shux/src/cli.rs` + `crates/shux/src/style.rs::print_pane_glance`).
LENS-R-010..016 implemented: one atomic clone (grid/cursor/size/alt_screen/
default_colors/content_revision) under a single `PaneIoState` lock, render +
text extraction from that frozen clone outside the lock. New `Grid::glance_text()`
(`crates/shux-vt/src/grid.rs`) extracts full-width, untrimmed, `\n`-joined
viewport rows (LENS-R-012) — deliberately distinct from `capture_text()`,
which trims trailing blank rows/whitespace for its own UX contract. New
`ErrorCode::PayloadTooLarge` (-32013, `crates/shux-rpc/src/error.rs`) for the
8 MiB decoded-PNG cap. Checkpoint storage (§7 LENS-R-030/031, P2-scoped: FIFO
cap 4, unique-per-revision no-op, `evicted_revision`) lives in a new
`PaneIoState.checkpoints: HashMap<PaneId, VecDeque<PaneCheckpoint>>` +
`PaneIoState::store_checkpoint`; `pane.checkpoint`/`pane.diff_since` (P4) are
NOT added, per the phase boundary. Determinism micro-test added to
`crates/shux-raster/src/lib.rs` (`glance_clone_renders_byte_identical_twice`,
NOT in the frozen `lens_*` paths).

**Gate result:** `make test-lens` → 14 passed / 23 failed (target per the
phase brief was 15/22 — see the G1 finding below for the one-test delta).
G2/G2w green (CLI+RPC, incl. `--png` file-write parity) against freshly
minted, PROVISIONAL goldens (`.shux/goldens/lens/{g2_f1_80x24,g2w_f5_100x30}.{png,txt}`
+ `evidence-manifest.json` + `contact-sheet.png` + `BASELINE-APPROVAL.md`,
all marked provisional pending human/SOLID-QA sign-off per §16.3). All other
red-suite roots are unchanged (`-32601 method_not_found` on the still-missing
P3/P4/P5 methods). `make test-vt-corpus` byte-exact, `make test` (all
workspace lanes) green, `make lint` clean, every daemon-backed run wrapped in
`.shux/scripts/no_leak_guard.sh` with zero leaked processes.

**G1 finding (NOT a `pane.glance` bug — spec/fixture gap, needs a decision):**
G1 (`crates/shux/tests/lens_glance.rs::g1_glance_atomicity_under_concurrent_flips`)
fails reproducibly under its 100-way concurrent load. Root-caused empirically:
F3 (`f3_flip.sh`) draws each full-screen flip as 24 independent `printf`
writes (one per row, absolute cursor positioning), with NO DEC 2026
synchronized-output wrapping (`CSI ?2026h`/`?2026l`). Under sustained
concurrent load, a PTY `read()`/`process()` batch can land mid-repaint,
producing a VT state that is legitimately a mix of the old and new frame —
and per §4.2, that batch still gets exactly one Class-A `ContentRevision`
bump (revision has no concept of "clean frame boundary"). Verified this is
NOT a `pane.glance` atomicity bug: three independent glances that happened to
land on the SAME revision during a manual repro returned byte-identical text
AND byte-identical PNG (proving the clone is atomic and the render/encode
path is deterministic) — the underlying VT content at that revision is
itself torn. A `dootsabha council` consult (`.shux/scripts/agent_review_guard.sh
lens-p2-g1-dispute`) independently reached the same conclusion and recommends
NOT patching `pane.glance` (no retry/quiet-wait/PTY-drain inside the RPC —
would violate LENS-R-010/011/015 and make the API fixture-aware) and NOT
touching the shared PTY read loop (out of P2 scope, gated by the repo's Rich
TUI Compatibility Guardrail, no hard guarantee anyway). The council's
recommended fix is a `LENS-TEST-CHANGE` to `f3_flip.sh` wrapping each
`draw_frame` call in `\033[?2026h`/`\033[?2026l` — `shux-vt`'s sync-mode
support (already shipped in P1) freezes the presented grid during sync and
releases it as one atomic batch on `?2026l`, which is exactly what G1's
"never observe a torn frame" claim needs from its producer. Per §16.4, this
requires explicit user approval before `f3_flip.sh` (frozen since P0) can be
touched — left for the phase orchestrator to decide; not applied in this PR.

**OSC 10/11/12 dynamic-color finding (§4.2 mandated re-examination):**
`crates/shux-raster` is a pure function of `RasterOptions` and has no OSC
awareness itself (`lib.rs::render`/`resolve_color`). But the CALLER already
wires OSC-derived colors in: `crates/shux-vt/src/parser.rs` (OSC 10/11/12
handler) mutates `VirtualTerminal`'s `default_colors: TerminalDefaultColors`,
`default_colors()` exposes it, and `pane.glance` (mirroring `pane.snapshot`
exactly) feeds `default_colors.{fg,bg,cursor}` straight into
`RasterOptions.{fg_default,bg_default,cursor_color}`. So YES, `pane.glance`'s
rendered pixels DO change on an OSC 10/11/12-only repaint — and since that's
Class B (§4.2, no `content_revision` bump), a caller polling `revision` to
decide whether to re-glance can miss a color-only frame change. This is a
real, live gap, not hypothetical (confirmed against the actual wiring, not
just spec prose). Not redesigned in P2 per the phase brief's explicit
instruction; flagged here for adjudication.

**Not done in this PR (explicitly out of P2 scope):** `pane.checkpoint`,
`pane.diff_since` RPCs (P4); SOLID VT QA subagent run + BASELINE-APPROVAL
sign-off (orchestrator-run per the phase brief).

## P2 adjudication round (2026-07-09 — all three P2 findings ruled; applied)

The orchestrator adjudicated the three P2 findings (PRD updated: §4.2 OSC
row, §11 F3 row, §17 new font-risk row). Applied on this branch:

1. **G1/F3 — APPROVED as a LENS-TEST-CHANGE.** `f3_flip.sh` now wraps each
   `draw_frame` in DEC 2026 synchronized output (`\033[?2026h` …
   `\033[?2026l`): the 24 row writes present as ONE atomic Class-A batch at
   release, exactly per the dispute council's recommendation. The F3 smoke
   test gained a sync-wrap contract assertion: one token → the flip presents
   as exactly ONE revision step (`content_revision` +1), chunking-independent.
   Result: **G1 green, 3/3 consecutive runs** (was 0/3 before the wrap);
   suite now exactly 15 passed / 22 failed with all remaining roots unchanged
   (`-32601` on P3/P4/P5 methods). Fixture stays shellcheck-clean.
2. **OSC 10/11/12 — RE-ADJUDICATED to Class A** (revision tracks the
   PRESENTED frame; the P2 evidence that dynamic-default-color changes alter
   glance pixels without a bump made the P1 Class-B ruling untenable).
   Implementation: `VirtualTerminal::process_with_responses` snapshots
   `default_colors` before the parser batch and includes
   `self.default_colors != before_colors` in the Class-A disjunction — the
   parser's existing change-guards make a same-value OSC set a net-zero
   batch (no bump), and the existing sync-deferral handles `?2026h` frames
   (color change while frozen defers to ONE bump at `?2026l`). Both
   directions covered: sets (OSC 10/11/12) and resets (OSC 110/111/112)
   bump when they change the presented colors; a reset with nothing set is
   a no-op. Unit tests flipped/renamed (NOT frozen paths):
   `osc_10_11_12_bumps`, `osc_110_111_112_bumps_when_set`, new
   `osc_dynamic_color_defers_under_sync`. **OSC 4 palette redefinition
   remains Class B** (adjudicated known limitation — noted in the test and
   in `Grid::mark_all_dirty`'s doc). The glance-handler comment now
   references the new ruling. shux-vt lane: 251 pass / 0 fail.
3. **Goldens — approved as-is pending the QA gate.** NOT regenerated:
   verified F1/F5 emit zero OSC 10/11/12 sequences (SGR colors only), so
   the reclassification cannot change their rendering; tofu limitation now
   documented in PRD §17.

## P2 codex review round (2026-07-09 — 1 blocker + 2 majors fixed, 1 minor disputed)

Verifier: VERIFIED-WITH-NOTES (goldens byte-matched a live drive). Codex
review NOT CONVERGED: 1 BLOCKER + 2 MAJORS + 1 MINOR, all presented-frame-
doctrine descendants. Fixes applied on the branch:

1. **BLOCKER — torn `alt_screen` under sync (fixed):** glance cloned
   grid/cursor/colors from the frozen sync presentation but `alt_screen`
   read the LIVE mode flag — an alt toggle inside `?2026h` paired old
   pixels with a future flag. Fix: `SyncPresentation` gains
   `alternate_screen` (captured at freeze); `is_alternate_screen()` is now
   presented-aware (same source as `grid()`/`cursor()`/`default_colors()`;
   live state remains available via `modes()`). Pinned by
   `sync_alt_toggle_glance_consistency` (live flag flips immediately,
   presented flag + pixels stay the frozen primary frame until release).
2. **MAJOR — OSC net-zero false bump under sync (fixed):** the batch
   compare used the raw live `default_colors` field, so a hidden
   set-then-restore inside sync flagged each batch and false-bumped at
   release. Fix: compare PRESENTED colors (`default_colors()` accessor) on
   both sides — under sync the pair is frozen==frozen (hidden churn never
   flags) and the release batch is frozen-vs-live (net change bumps exactly
   once, net-zero never). Pinned by `osc_color_net_zero_under_sync_no_bump`
   (net-zero → no bump; control: net change → +1 at release).
3. **MAJOR — checkpoint resurrection (fixed):** `store_checkpoint`'s
   `entry().or_default()` could recreate checkpoint state for a pane torn
   down between glance's clone and its second lock acquisition. Fix: refuse
   when `vts` has no live entry; returns `(stored, evicted)` and the
   handler reports `checkpointed: false` honestly in the race. Pinned by
   `checkpoint_store_refuses_resurrection_after_teardown` (no entry
   creation pre-VT, store + same-revision no-op with live VT, no
   resurrection post-teardown).
4. **MINOR — CLI `--format json` envelope (DISPUTED with evidence):** codex
   asked for bare-result emission per §10's prose. The FROZEN harness
   (`lens_common::cli_envelope`, whose own doc comment reads §10 as "the
   raw RPC `{result|error}` envelope") parses `.get("result")/.get("error")`
   from every lens CLI verb's json output. Verified empirically: emitting
   the bare result panics G2's CLI twin at `lens_common/mod.rs:59`
   ("envelope had neither result nor error") and would break G1's 50 CLI
   glances + G2w — gate 15/22 → 12/25, violating the round's own
   no-other-flips tripwire. The envelope also gives byte-parity with
   `shux rpc call` (M9). Changing the shape requires a LENS-TEST-CHANGE to
   the frozen harness first; code comment at the emission site records the
   dispute. Escalated for the claude convergence round to adjudicate.

Gates after the round: `make test-lens` 15/22 (identical fail set, goldens
byte-stable — F1/F5 rendering untouched) · shux-vt 253/0 · shux bin lane
183/0 · vt-corpus byte-exact · full lanes 0 failed · lint clean ·
leak-guard clean.

## P2 ship round (2026-07-09 — claude CONVERGED; minors applied; baselines approved)

Claude full review: CONVERGED — zero new blockers/majors, 3 minors. The P2
chain is complete (verifier ✓ · codex round fixed ✓ · claude converged ✓ ·
SOLID VT QA PASS ✓ at `.shux/qa/lens-p2/SOLID-QA.md`, commit 1a578b4).

1. **Minor (a), REAL — sync-enter color lag (fixed):** a color change in
   the SAME batch that opens `?2026h` is frozen INTO the presentation (the
   presented frame visibly changed) but the bump was deferred to release.
   Fix: the presented-colors compare bumps immediately even when sync is
   active at batch end — under sync it can only fire if the presentation
   itself changed this batch; the other Class-A signals (live-state
   compares that cannot split pre-/post-freeze within a batch) keep the
   defer path. Test: `osc_color_set_then_sync_enter_same_batch_bumps_immediately`.
2. **Minor (b) — FIFO eviction unit coverage (added):** frozen D5 stays
   red until P4, so `checkpoint_fifo_evicts_lowest_creation_revision` pins
   the LENS-R-031 contract at unit level (5 ascending stores → rev 1
   evicted + reported).
3. **Minor (c) — revision-ordered FIFO (fixed):** `store_checkpoint` now
   inserts sorted by revision, so eviction always takes the LOWEST creation
   revision even when racing glances store out of arrival order (the same
   unit test covers the out-of-order insert: arrivals [10,5,20,30]+40 evict
   5, not 10).

Baselines: BASELINE-APPROVAL.md → **APPROVED** (QA PASS + orchestrator
sign-off under the user's standing ship authorization; tofu limitation
acknowledged per PRD §17); `evidence-manifest.json` `provisional: false`;
golden bytes verified unchanged (sha256 re-check). Council verdicts
committed at `.shux/qa/lens-p2/council/`.

## PR #89 bot round + golden re-mint (2026-07-09 — user-ordered "fix everything")

**Bot fixes (all four threads):**
1. **P1 — pre-render pixel budget:** glance rasterized the full pane before
   any size check; a max-size (1000×1000) pane forced hundreds of MB of
   RGBA allocation + encode before the post-encode 8 MiB cap could fire.
   The 16M-pixel budget (pane.snapshot's cap) now runs inside the lock,
   BEFORE clone/render, mapped to `PAYLOAD_TOO_LARGE (-32013)` with
   `{pixels, max_pixels, hint}`. Text-only glances skip it (no PNG payload
   exists). Test: `production_glance_rejects_over_budget_panes_before_render`
   (full production router: guard fires at 1000×1000; `include_png=false`
   on the same pane still succeeds).
2. **P2 — glance_text comment:** rewritten; blank cells PAD rows to full
   display width, trailing whitespace preserved, no trimming mechanism.
3. **P2 — CLI conflict:** `--png` now carries clap `conflicts_with =
   "text_only"` (parse-time rejection, exit 2, zero RPC round-trips) plus
   a defensive bail in `handle_pane_glance` for programmatic callers.
4. **P2 — needless clone:** clone routing is a `(include_png,
   want_checkpoint)` move-matrix — only PNG+checkpoint pays one clone;
   text-only+checkpoint MOVES the grid into the checkpoint.

**Golden re-mint with real fixture fonts (user adjudication, PRD §17):**
- `.shux/fixtures/fonts/`: `NotoSansDevanagari-Regular.ttf` (OFL, notofonts
  hinted static TTF) + `NotoSansJP-shuxlens-subset.ttf` (OFL, pyftsubset of
  google/fonts NotoSansJP wght=400 instance to exactly the 9 CJK codepoints
  the fixtures use — ステト実界真端終面 — ~4 KB; commands in README.md).
  OFL texts committed alongside; sha256 + provenance in the evidence
  manifest.
- `lens_common::Harness::new` (LENS-TEST-CHANGE p2-fonts) writes the
  isolated daemon config: `appearance.font_fallbacks = [builtin:nerd-font,
  <devanagari>, <cjk subset>, builtin:math, builtin:symbols,
  builtin:symbols-legacy, builtin:emoji]`. Primary stays bundled JBM →
  cell metrics identical; DEFAULT chain + vt-corpus goldens untouched
  (verified byte-exact).
- Goldens re-minted (PNGs + contact sheet; TXT goldens unchanged —
  font-independent). Devanagari/CJK tofu GONE. KNOWN + ACCEPTABLE:
  per-codepoint rendering, no OpenType shaping (conjuncts/matras
  decomposed) — stated in BASELINE-APPROVAL.md, which is now
  "RE-MINTED — pending QA re-inspection" with the prior approval preserved
  as history. A focused QA re-pass runs next.

Gates: test-lens 15/22 (G1/G2/G2w green vs NEW goldens, smoke lane 10/10
under the font config) · vt-corpus byte-exact · full lanes 18/18 · lint ·
shellcheck · leak-guard all clean.

## P4 implementation notes (`pane.checkpoint` + `pane.diff_since`, branch `feat/lens-p4-checkpoint-diff`)

**Delivered (§7 SPEC-D, LENS-R-030..038):**
- `pane.checkpoint` RPC (`crates/shux/src/main.rs`, registered after
  `pane.wait_settled`) + `shux pane checkpoint <pane>` CLI. Reuses the P2
  `store_checkpoint` FIFO (cap 4, same-revision no-op, `evicted_revision`).
- `pane.diff_since` RPC + `shux pane diff <pane> --since REV [--heat PATH]
  [--no-row-text]` CLI. Existence-first lookup (`diff_lookup_checkpoint`):
  PANE_NOT_FOUND (-32004) before any checkpoint lookup; then LENS-R-033 —
  exact stored checkpoint → diff; else `since ≤ last_invalidation` →
  RESIZE_INVALIDATED (-32011); else STALE_REVISION (-32010) with
  `{requested, available:[u64]}`.
- Invalidation markers (LENS-R-032/DEC-4): new `PaneIoState.invalidations:
  HashMap<PaneId,u64>` + `invalidate_checkpoints` (frees the deque, records
  the POST-mutation revision, monotonic). Two hook points in the PTY loop:
  the resize branch (after `vt.resize`) and the process branch (PRESENTED
  alt-flag compared before/after each batch — a net-zero enter+leave never
  invalidates, matching §4.2). Cleared on pane teardown.
- Structured diff (`compute_lens_diff`, LENS-R-034..036): value-equality of
  underlying `Cell` data (no cursor overlay — clones carry none), wide-glyph
  head+spacer pairing, per-row merged half-open spans (cap 256 →
  `regions_truncated`), half-open `bounding_box` (all-zero on zero delta),
  `changed_row_text` via `glance_row_text` (byte-parity with `glance_text`),
  `cursor_moved` reported separately (cursor excluded from cell count/regions
  by construction).
- Heat PNG (LENS-R-037, `render_lens_heat_png`): current clone through the
  P2-approved rasterizer (no cursor), then changed cells alpha-blended with
  `rgba(163,38,56,128)` and unchanged cells desaturated 50% (Rec.601 luma).
  Integer-only → deterministic. shux-raster/shux-vt source UNCHANGED (overlay
  is post-processing in main.rs).
- New error codes `crates/shux-rpc/src/error.rs`: `StaleRevision` (-32010),
  `ResizeInvalidated` (-32011) + `stale_revision`/`resize_invalidated`
  constructors + tests.
- CLI exit codes (§10): checkpoint 0/2/3/4; diff 0 on any delta, 5 on
  STALE/INVALIDATED/PAYLOAD_TOO_LARGE, 2 INVALID_PARAMS, 4 PERMISSION_DENIED,
  3 otherwise. `crates/shux/src/style.rs`: `print_pane_checkpoint`,
  `print_pane_diff`.

**Gate result:** `make test-lens` **27 passed / 10 failed** (was 21/16):
D1–D5 + A1 all green (CLI + RPC twins); the 10 reds are R1–R8 (lens.run
-32601, P5) + K1 (missing P6 golden `k1_pos1.png`) + E1 (lens.run, P5) — all
untouched. K1 root-cause confirmed clean: its checkpoint/glance/diff RPCs all
succeed, it stops only at the P6 golden. Other lanes: `make lint` clean ·
`make test-rpc` 43/43 (+2 error-code tests) · `make test-vt-corpus`
byte-exact (no raster/default-chain regression) · new
`make test-lens-diff-concurrency` green · all daemon-backed runs under
`no_leak_guard.sh`, zero leaked daemons/processes.

**P4 DoD concurrency (council D2):** two-part proof — (a) in-process unit
test `compute_lens_diff_independent_of_dirtystate_drains` drains the VT's
DirtyState between the checkpoint clone and the diff and asserts the delta is
unchanged (diff reads cell VALUES via `clone_visible`, never render state);
(b) black-box `crates/shux/tests/diff_concurrent_readers.rs` hammers the pane
with concurrent `pane.snapshot`/`pane.glance` render reads while
checkpoint→drive→settle→diff reports the exact 10-cell F4 delta. Plus unit
tests: `diff_lookup_existence_first_and_invalidation_marker`,
`invalidation_marker_is_monotonic`, `compute_lens_diff_wide_glyph_pairs_spacer`,
`heat_png_is_deterministic`.

**Goldens (PROVISIONAL, §16.3 — pending SOLID QA + orchestrator sign-off):**
`d2_heat.png` (NEW rendering — heat overlay, full SOLID gate),
`a1_alt.png` + `a1_normal.png` (raster-UNTOUCHED glance path — §14 lighter
ratification). All minted from the actual implementation output, sha256 +
provenance in `evidence-manifest.json`, visual inspection recorded in
`BASELINE-APPROVAL.md` P4 addendum.

## P4 convergence round 1 (2026-07-10 — 2 blockers + 1 major + 1 minor, all fixed)

Verifier: VERIFIED-WITH-NOTES, goldens RATIFIED (3d70a31). Codex NOT
CONVERGED (1B+1M), claude NOT CONVERGED (1B+1m). Both P4 design calls
(cursor-less heat base, Rec.601 luma) ruled ACCEPTABLE by both reviewers —
goldens and SOLID QA PASS stand; neither blocker touches rendering.

1. **BLOCKER (codex) — checkpoint resurrection across an invalidation
   (fixed):** `pane.glance{checkpoint:true}` clones revision R under lock #1
   and stores under lock #2; a concurrent resize/alt-switch invalidating at
   R+1 between the two got its invalidation silently undone —
   `store_checkpoint` never consulted the marker, so a later `diff_since(R)`
   silently diffed stale-dimension frames instead of -32011. Fix:
   `store_checkpoint` refuses any revision STRICTLY BELOW the pane's
   invalidation marker, enforced under the store lock, as the same honest
   no-op as the teardown race (`(false, None)` → `checkpointed: false`).
   Strictly-less-than, NOT ≤, per LENS-R-033's own text ("a checkpoint
   created AFTER the invalidation (revision ≥ marker) is found by rule
   (1)"): revision+clone are read in one io-lock critical section and
   invalidating events bump the revision inside that same lock, so an
   ==marker clone depicts the POST-mutation frame — refusing it would orphan
   the immediately-post-resize `pane.checkpoint` and make
   `diff_since(marker)` wrongly -32011. Deterministic unit test (direct
   store path, manually-set marker, no timing):
   `checkpoint_store_refuses_pre_invalidation_revisions` — refused store
   materializes nothing, `diff_lookup_checkpoint(R)` → -32011, ==marker and
   >marker stores stay storable/diffable.

2. **BLOCKER (claude) — ungated resize invalidation (fixed):** the PTY
   resize branch invalidated unconditionally, but `vt.resize` only changes
   the frame when dims actually change — so the attach render loop's
   `apply_resize_to_window` re-fan (fires on attach, client resize, window
   switch, zoom at an unchanged client size) and same-size `pane.set_size`
   destroyed every checkpoint on unchanged panes. Fix: capture (rows, cols)
   across `vt.resize` and invalidate only on an actual dimension change —
   mirroring the process branch's `alt_switched` gating. RED-FIRST receipts
   (`.shux/out/lens-p4/red-receipt-gating.txt`, both -32011 pre-fix; note
   `invalidated_at == requested` on the same-size repro — the no-op resize
   invalidated at an UNCHANGED revision):
   `crates/shux/tests/checkpoint_invalidation_gating.rs` —
   (i) `same_size_set_size_preserves_checkpoints` (synchronous ack flush,
   zero-delta then exact 10-cell delta), (ii)
   `real_attach_window_switch_preserves_checkpoints_and_diff_exact`.

3. **MAJOR (codex, ADJUDICATED — PRD §7.3 amended):** the -32011 payload
   keeps `{requested, invalidated_at, hint}`. Wire-shape-pinning tests
   added at both levels: shux-rpc unit (`data_keys` exact-set asserts in
   `test_stale_revision_error` / `test_resize_invalidated_error`) and
   black-box (`error_wire_shapes_pinned`: -32010 EXACTLY
   `{requested, available}`; -32011 EXACTLY
   `{requested, invalidated_at, hint}`, `invalidated_at` > checkpoint).

4. **MINOR (claude) — real-attach coverage (folded into 2(ii)):** the
   window-switch test drives a REAL attach client — the daemon-side
   AttachHello/AttachReady handshake + streaming frames over `attach.sock`
   (thinnest headless client: dedicated thread, framed codec, Action frames
   for SwitchToWindow, Input frames for Tab/`a`, continuous drain of render
   frames). While attached: checkpoint survives the away/back re-fans, Tab
   delta is EXACTLY 2 cells `[{8,5,6},{8,25,26}]`, and the accumulated
   Tab+`a` delta is EXACTLY 12 cells with exact regions. Determinism: the
   attach loop handles client frames in order and awaits
   `apply_resize_to_window` inline, so the observed Tab marker proves the
   switch-back re-fans were queued; a synchronous same-size `pane.set_size`
   then flushes the pane's resize channel (its ack fires only after
   everything queued before it) — the diff is deterministic in both the red
   and green directions.

## PR #91 bot round (2026-07-10 — codex P1+P2, greptile 3×P2, all fixed)

1. **codex P1 (memory) — heat pre-render pixel budget:** shared
   `lens_pixel_budget_check` (the exact 16M-pixel cap from
   glance/snapshot; glance refactored onto it, error byte-identical) now
   gates `pane.diff_since{heat_png:true}` inside the lock BEFORE any RGBA
   allocation, positioned AFTER the LENS-R-033 lookup so
   stale/invalidated wins over the payload error. Heat-less diffs never
   hit it. Unit: `lens_pixel_budget_check_guard_predicate`. Black-box:
   `crates/shux/tests/diff_heat_budget.rs` (1000×1000 F1 pane → -32013
   `{pixels, max_pixels, hint(heat_png=false)}`; CLI `--heat` exit 5, no
   file written; heat-less diff on the same oversized pane succeeds).

2. **codex P2 (semantic, ADJUDICATED → LENS-R-038b):** `PaneCheckpoint`
   gains `default_colors` (captured in the same critical section as the
   grid clone in both `pane.checkpoint` and `pane.glance{checkpoint}`);
   `diff_lookup_checkpoint` returns it; `compute_lens_diff` resolves
   `Color::Default` against each side's respective defaults. Rule: a
   changed fg/bg default marks every cell that is `Default` in that
   channel on BOTH sides (asymmetric pairs already raw-differ);
   concrete-colored cells stay unmarked; OSC 12 cursor default never
   marks (DEC-11); unchanged defaults → byte-identical to raw equality.
   Internal-only storage change; no RPC surface change beyond diff
   behavior. Verified non-regressive: D-tier 6/6 with `d2_heat.png`
   byte-identical (F4 touches no defaults); lens gate 27/10 identical.
   Tests: (a) unit marks/regions/bbox + black-box
   `crates/shux/tests/diff_default_colors.rs` — REAL OSC 11 `#204060`
   via a token-paced `exec sh -c` script pane (40×10): exactly 395/400
   cells, full-width regions split around the 5 truecolor cells; (b)
   `compute_lens_diff_unchanged_defaults_matches_raw`; (c) heat base
   uses CURRENT defaults — unit integer-math probes + black-box pixel
   probes (changed blank = (97,50,75); unchanged truecolor = (58,103,73)).

3. **greptile P2s:** `lens_checkpoint_exit_code` doc enumerates the real
   error surface incl. INVALID_PARAMS→2 (their suggested text); `pane
   diff` CLI help enumerates PAYLOAD_TOO_LARGE among exit-5 causes;
   `diff.changed_mask` MOVED into the heat closure (needless clone
   dropped).

   Implementation note: the first draft of the OSC black-box test used a
   prompt-bearing `sh` pane — the long default prompt wrapped at 40 cols
   and SCROLLED the pane (shifting every row; observed 400/400 cells).
   Rewritten as an inline `exec`'d token-paced script (the fixtures'
   promptless pattern; EOF-safe `while read` loop).

## P5 implementation notes (scratch sessions + `lens.run`, branch `feat/lens-p5-scratch-run`)

**Delivered (§8 SPEC-E, LENS-R-040..046):**

- New module `crates/shux/src/lens_scratch.rs`: `ScratchRegistry`
  (in-memory `Arc<Mutex<HashMap<SessionId, ScratchState>>>` +
  `$XDG_RUNTIME_DIR/shux/scratch-registry.json`, rewritten whole-file on
  every insert/remove — LENS-R-044's exact schema
  `{session_id, pgid, created_at, max_runtime_deadline}`), the `lens.run`
  RPC handler, a per-scratch `scratch_reaper` task, and a sha256-chained
  NDJSON audit log at `$XDG_STATE_HOME/shux/lens-audit.ndjson`
  (LENS-R-052; mirrors `shux-plugin/src/audit.rs`'s synchronous-write
  posture, extended with a genuine hash chain — each entry's `hash` covers
  `prev_hash` + the entry body, genesis is 64 zeros).
- `spawn_pane_pty` (main.rs) gained two params — `size: PtySize`,
  `extra_env: Vec<(String,String)>` — and now returns the raw
  `shux_pty::PtyError` instead of pre-mapping to `RpcError::internal`, so
  `lens.run` can map spawn failure to `SPAWN_FAILED (-32014)` while every
  other call site (session.create/ensure, window.create, `state.apply`,
  attach's split/new-window paths) keeps its existing `internal()` mapping.
  All 6 pre-existing call sites now pass `PtySize::default()` / `Vec::new()`
  — byte-for-byte the same behavior as before (VT construction now reads
  `size.rows/cols` instead of a hardcoded 24×80, which are the same values
  for every non-lens caller). New `PaneIoState.pty_pids: HashMap<PaneId,
  u32>` lets `lens.run` read back the spawned child's pid (== pgid; PTY
  children are session leaders) without needing its own `PtyHandle`.
- `lens.run` allocates via the SAME `GraphHandle::create_session_with_command`
  entrypoint `session.create`/`session.ensure` use (DEC-21: no public
  scratch param exists; this is an internal-only call path with a
  `__scratch-<uuid>` session name), then execs `argv` directly (non-empty
  argv guarantees `PtyConfig::with_command`'s no-shell path — `resolve_command`
  only substitutes a shell when `command.is_empty()`). Spawn failure rolls
  back via `graph.destroy_session` before returning `SPAWN_FAILED`; zero
  residue by construction (nothing is registered until spawn succeeds).
  `lens.run`'s response is exactly the §8.1 schema — `{session_id, pane_id,
  revision}` (+`exit_code` when `wait:true`) — it does NOT call
  glance/wait_settled/diff_since internally; those stay separate RPCs an
  agent chains itself (proven end-to-end by E1's own call sequence).
- Exit detection is event-driven (§16.1 guardrail 3 — no polling): both the
  reaper and the `wait:true` branch subscribe to the daemon's existing
  `pane.exited` bus event (already fired by `run_pane_pty_task` via
  `graph.set_pane_exit_status`) — both subscriptions are created BEFORE the
  PTY is spawned, so a fast-exiting command (F6) can't race ahead of the
  listener. The reaper (`scratch_reaper`) is a single `tokio::select!` over
  three deadline-bounded arms: explicit-kill cancellation, `max_runtime_ms`
  deadline, and (after the exit event fires) a second race between
  `post_exit_ttl_ms`, `max_runtime_ms`, and explicit-kill. Reap itself calls
  the SAME `state.teardown_panes()` + `graph.destroy_session()` pair
  `session.kill` already uses — LENS-R-042's "killpg(SIGTERM)…" reap
  contract is satisfied by reusing `run_pane_pty_task`'s existing
  SIGHUP→500ms-grace→SIGKILL escalation (triggered by cancelling the pane's
  shutdown token) rather than re-implementing process-group signalling
  end-to-end; only the true STARTUP-orphan path (`ScratchRegistry::startup_reap`,
  no live PTY task to escalate through) does its own `killpg`
  SIGTERM→500ms→SIGKILL directly.
- `session.list` gained `include_scratch: bool` (default false): scratch
  ids come from `ScratchRegistry::ids()` (a point-in-time snapshot,
  building the JSON outside the registry lock); default listing filters
  scratch out entirely, `include_scratch:true` includes them flagged
  `"scratch": true` (LENS-R-041 — visibility only, never a substitute for
  direct-id resolution: `session.kill`/`pane.glance`/etc. never consult
  this filter). `session.kill` calls `lens_scratch::on_session_killed`
  after its existing teardown — a no-op for ordinary sessions (registry
  miss), immediate reap(reason=explicit) audit + reaper-task cancellation
  for scratch (LENS-R-042c).
- Daemon startup (`run_daemon`) calls `ScratchRegistry::startup_reap`
  right after `ensure_runtime_dir()`, before the RPC server binds — reads
  a leftover registry file, `killpg`s every registered pgid, audits
  `reap(reason=registry)` per row, deletes the file (DEC-7: scratch never
  survives a restart).
- CLI: `shux lens run [--size CxR] [--ttl D] [--max-runtime D] [--env K=V]...
  [--cwd PATH] [--wait] -- <argv...>` (new `Command::Lens{LensCommand::Run}`,
  `cli::handle_lens_run`, `style::print_lens_run`); `shux session list
  --include-scratch`. `shux lens` / `shux lens --help` print the five-verb
  loop recipe (§10 discoverability requirement) via `after_help` on the
  `LensCommand` subcommand group. `--size`/`--env` get dedicated value
  parsers (`parse_size_cxr`, `parse_env_kv`) that validate SHAPE only —
  range bounds stay server-side INVALID_PARAMS, matching the existing
  `--quiet`/`--timeout` convention (`parse_duration_ms`) rather than
  duplicating the bounds check client-side. Exit codes follow §10 exactly
  (`lens_run_exit_code`: INVALID_PARAMS→2, PERMISSION_DENIED→4,
  RESOURCE_EXHAUSTED/SPAWN_FAILED→5, else→3); `--wait`'s success path
  exits with the CHILD's code per the documented precedence rule.
- New RPC error codes (`crates/shux-rpc/src/error.rs`):
  `ResourceExhausted` (-32012) and `SpawnFailed` (-32014), with
  constructors + wire-shape-pinning unit tests (mirroring the P4
  `stale_revision`/`resize_invalidated` pattern).
- `lens.run`'s `Policy`: `Sensitivity::Grantable` (never default-allow for
  plugins, same tier as `state.apply`) — a judgment call, since LENS-R-050's
  `scratch:create` scope doesn't exist as a distinct tier in the current
  4-tier permission model; P2–P4 made the same call mapping their
  `pane:observe` intent onto the existing `ContentRead` tier rather than
  inventing new scope strings.

**Known limitation (documented, not test-gated):** `ScratchRegistry::startup_reap`
probes pgid liveness via a signal-0 `killpg`, not a full process-start-time
comparison against the registry's `created_at` field (LENS-R-044 says "if
still alive and its start time matches"). A PID that wrapped around to an
unrelated process in the exact same narrow window would be killed too. Given
`killpg` only ever targets pgids this daemon created as scratch
process-group leaders, the blast radius is bounded to "processes sharing a
recycled pgid" — the same class of risk the PRD already accepts for
double-forked escapees (§17 M14). Not covered by R7 (which only requires a
genuinely-live scratch to be reaped after a real daemon restart, and does
not construct a PID-reuse adversarial case).

**Gate result:** `make test-lens` **35 passed / 2 failed** (was 27/10):
**R1–R8 all green** (8/8, CLI + RPC twins, incl. R6's 16-slot quota +
17th-rejected + retry-after-kill and R7's SIGKILL-daemon +
restart + zero-orphan + registry-file-removed + audit(reason=registry)
proof). The 2 reds are K1 and E1 — BOTH fail at the identical root cause,
`golden not found` (`k1_pos1.png` / `e1_glance.png`), not a functional or
logic failure. Precisely (verifier-corrected): E1 panics at its FIRST
`assert_png_golden` call (`lens_loop.rs:99`, `e1_glance.png`) — i.e. after
`lens.run` → `wait_for` → settle → `glance --checkpoint` succeed but
BEFORE the drive→diff tail, so the frozen suite itself never reaches the
`cells_changed:10` assertion. The tail's correctness was proven by the P5
verifier's own independent live drive (send `a` → settle → diff on the E1
scratch: exact 10-cell delta + exact regions), not by the frozen test.
`assert_png_golden` mints no golden if missing (§16.3), and per this
task's brief ("if you find you must … mint a golden: STOP and message the
orchestrator first"), no goldens were minted — K1/E1 stay red pending an
explicit decision on whether E1's goldens (raster-UNTOUCHED —
`pane.glance`/`pane.diff_since`'s rendering code is P2/P4-approved and
unmodified in this phase) get pulled into P5 via the same "golden
ratification for raster-untouched phases" path P3 used for `s1_ready.png`,
or deferred to P6 (where the PRD's own phase table places E1's green
gate). Other lanes: `make lint` clean (clippy -D warnings + fmt-check) ·
`make test` **1163/1163 across 23 suites** (the "210" previously reported
here was the shux bin-lane count alone — verifier-corrected) ·
`make test-rpc` 45/45 (+2 new error-code tests) · `make test-vt-corpus`
byte-exact (no raster/VT source touched) · every daemon-backed run under
`.shux/scripts/no_leak_guard.sh -j 1`, zero leaked `shux` processes or
orphan fixture procs (confirmed both by the leak guard's own pass-through
exit code and a separate manual smoke test: `lens run` with real
truecolor+256-color+basic-ANSI content, `session list --include-scratch`,
glance text readback, registry-file schema, audit-chain readback, explicit
`session kill`, and a daemon-restart registry-reap cycle — every artifact
matched expectations, no residue).

**Known CLI-polish note (P6):** `lens run --wait` on a child killed by
`max_runtime` exits 255 — the child dies by signal, so `exit_status` is
`None`, the handler reports `exit_code: -1`, and `std::process::exit(-1)`
becomes 255. §10's precedence rule ("once the child has started, the CLI's
exit code is the child's") has no defined mapping for signal deaths;
documenting a shell-conventional `128+signo` mapping (or keeping 255) is
P6 CLI-polish material.

## P5 convergence round 1 (2026-07-10 — 3 blockers + 4 majors + 6 minors, all fixed)

Verifier VERIFIED-WITH-NOTES (and REFUTED the E1-tail receipt — corrected
above). Codex NOT CONVERGED (3B+4M+1m), claude NOT CONVERGED (2M ≡ codex's
+ 6 minors). All fixed on-branch:

1. **B1 (codex ≡ claude M1) — quota TOCTOU:** `registry.len()` at the top
   of `handle_lens_run` vs the insert after spawn let 2+ concurrent
   `lens.run` calls at 15/16 both pass → 17 scratch. Fix: atomic
   check-and-reserve in ONE critical section (`ScratchRegistry::try_reserve`
   → `ScratchReservation`, a quota slot counted as `rows + reserved` under
   one `std::sync::Mutex`); the reservation releases on EVERY failure path
   by `Drop` and converts into the committed row via `commit()` (release +
   insert + persist in one critical section). Tests: unit
   (`reserve_admits_exactly_one_at_the_last_slot`,
   `dropped_reservation_releases_its_slot`, `committed_reservation_…`) +
   PRODUCTION-router concurrency
   (`production_lens_run_quota_is_atomic_under_concurrent_calls`: occupy
   15, `tokio::join!` two real lens.run dispatches → exactly one wins,
   loser -32012, kill winner → retry succeeds; zero sleeps) and
   `production_lens_run_failed_spawn_releases_its_reservation` (SPAWN_FAILED
   at 15/16 → slot reusable).

2. **B2 (codex ≡ claude N2) — registry not crash-safe:** `persist()` used
   `std::fs::write` (truncate-in-place) — a crash mid-write left partial
   JSON that `startup_reap` then discarded AND deleted, killing nothing
   (DEC-7 violation). Fix: temp-file + `rename` atomic persist; on parse
   failure the file is PRESERVED as `scratch-registry.json.corrupt`
   (renamed, `tracing::error!`, never silently deleted, nothing killed —
   the evidence is what an operator needs). Tests:
   `persist_is_atomic_rename_and_leaves_no_temp_file`,
   `startup_reap_preserves_corrupt_registry_as_evidence` (truncated JSON →
   0 killed, `.corrupt` holds the original bytes).

3. **B3 (codex) + SIGHUP-vs-SIGTERM major, fixed together:** the reap
   delegated to `teardown_panes` (which only SPAWNS waiters; the actual
   kill ran later in the PTY task via `handle.terminate()` = SIGHUP) while
   the registry row was removed immediately — daemon death in the gap
   orphaned the group with no row, and the signal violated LENS-R-042's
   "killpg(SIGTERM), 500 ms grace, killpg(SIGKILL)". Fix: the scratch reap
   path now performs its OWN synchronous LENS-R-042 sequence
   (`kill_pgid_lens_sequence`: probe → SIGTERM → 500 ms bounded grace →
   SIGKILL → bounded death confirmation) → close PTY (teardown) → remove
   session → audit, and `registry.remove` runs ONLY after the group is
   confirmed dead. Applied to both the timer reaper and
   `on_session_killed`. Black-box proof
   (`crates/shux/tests/scratch_reap_order.rs`, `make
   test-lens-scratch-reap`): a TERM-trapping workload writes a marker
   (TERM delivered first), survives into the grace window (registry row
   still present right after the marker), then dies anyway (only SIGKILL
   ends a TERM-ignoring loop), and the row disappears only after death;
   audit reap(reason=max_runtime) asserted.

4. **M3 (codex) — validate BEFORE casting:** cols/rows went through `as
   u16` and ttl/runtime through `as u32` before the range check —
   `{"cols": 66000}` wrapped to a legal 464, `{"post_exit_ttl_ms":
   4294967297}` to 1. Fix: `ranged_u64_param` range-checks the FULL u64
   (strict-typed: present-but-mistyped → INVALID_PARAMS, the P3 rule) and
   only then casts (bounds provably fit). Tests: unit wrapping/type/bounds
   quartet + production-router raw shapes
   (`production_lens_run_rejects_wrapping_params_before_cast`).

5. **M2 (codex ≡ claude M2/N1) — audit per LENS-R-052:** (a)
   COMPLETENESS — glance/checkpoint/diff handlers now append entries with
   the spec's fields (ts, caller, method, pane_id, revision(s),
   bytes_returned = decoded payload); plugin permission DENIALS of lens
   methods mirror into the daemon log via a new generic
   `PluginManager::set_denial_hook` (shux-plugin stays lens-ignorant; the
   per-plugin audit already recorded every denial — this adds the
   daemon-level view with `caller: plugin:<uuid>`). (b) CONCURRENCY —
   `LensAuditLog` serializes appends behind a mutex with the chain head
   CACHED in memory (read once at open): no forked chains under concurrent
   writers, no O(n²) whole-file re-read per append
   (`audit_concurrent_appends_never_fork_the_chain`, 24 concurrent
   appends → chain verifies, count exact). (c) ROTATION — 1 MiB cap,
   keep-5, mirroring the plugin audit log; each rotated file carries its
   own genesis-rooted chain (documented contract;
   `audit_rotates_at_cap_and_restarts_the_chain`). (d) VERIFICATION —
   `verify_chain()` recomputes every link;
   `audit_chain_verifies_and_detects_tampering` proves a single-byte edit
   is detected.

6. **M4 (codex ≡ claude item-8, ADJUDICATED IMPLEMENT) — LENS-R-044
   start-time match:** registry rows now carry the group leader's
   OS-reported start token captured at registration (macOS:
   `libc::proc_pidinfo(PROC_PIDTBSDINFO)` → `pbi_start_tvsec/usec`; Linux:
   `/proc/<pid>/stat` field 22; cfg-gated, other platforms store 0 →
   liveness-only fallback, logged). `startup_reap` kills only if alive AND
   the token matches — a recycled PID is spared (row still cleared).
   Tests with a REAL spawned group leader:
   `startup_reap_spares_mismatched_start_token_but_clears_the_row`,
   `startup_reap_kills_on_matching_start_token`. New direct dep: `libc`
   (already in-tree transitively via nix; nix has no proc_pidinfo wrapper).

7. **Minors:** (1) `session list` text mode renders a visible `[scratch]`
   tag on the name and plain mode appends a 5th `scratch` column on
   scratch rows only (ordinary rows keep the stable 4-column shape). (2)
   Attach guard: bare `shux`/`shux attach` target choice extracted into
   pure `choose_attach_session` — filters `scratch: true` AND the
   `__scratch-` name prefix (defense in depth over the default-list
   omission); unit-tested (`choose_attach_session_never_picks_scratch`).
   PR-description note: `session.snapshot`/`events.watch` can still name a
   scratch session by id — spec-conformant (LENS-R-041: "visibility ≠
   authorization"). (3) pgid==0 rejected in `kill_pgid_lens_sequence`
   (killpg(0) signals the daemon's own group) AND never persisted —
   `handle_lens_run` rolls back if the spawned pane lost its pid
   (`kill_sequence_refuses_pgid_zero`). (4) Reap dedup: `biased` selects
   put the explicit-kill arm first, and a `claimed` flag on the registry
   row makes reap ownership exactly-once (`claim()` — timer reaper and
   explicit kill can never double-audit; `claim_is_exactly_once`); the
   stale `remove` doc-comment rewritten. (5) Audit `caller` identity —
   ADJUDICATED IMPLEMENT (task-local proposal accepted): new
   `shux_rpc::caller` module (`tokio::task_local!` `RPC_CALLER` +
   `current_caller()` defaulting to `"uds"` + `with_caller()` scope
   wrapper); shux-plugin's `dispatch_plugin_frame` wraps each router
   dispatch in `with_caller("plugin:<uuid>", …)` (the dispatch already
   runs in its own spawned task, so the scope is naturally
   request-bounded and does NOT propagate to tasks the handler spawns —
   reap timers correctly revert to the daemon default); all seven
   production audit sites read `current_caller()`. Zero handler-signature
   changes. Tests at three levels: shux-rpc unit trio (default / scope /
   no-spawn-propagation), shux-plugin
   `dispatch_plugin_frame_scopes_caller_identity` (probe handler observes
   `plugin:<uuid>` through the real dispatch path; direct dispatch reads
   `uds`), and production-router
   `production_lens_audit_caller_identity` (two real lens.run calls — one
   plain, one wrapped in the exact plugin scope — audit `caller: uds` vs
   `caller: plugin:test-uuid-1234`, chain still verifies). Denial entries
   carry `plugin:<uuid>` via the denial hook as before. (6) Docs
   corrected per the verifier: E1 failure point, 1163/1163 count,
   `--wait` 255 note (all above).

**Policy-tier caveat for the PR description (both reviewers accepted
Grantable):** the grant name is `lens.run`, not `scratch:create`;
a Grantable-granted plugin inherits scratch-spawn authority — a
pre-existing limit of the 4-tier permission model, not new surface.

## P5 convergence round 2 (2026-07-10 — codex 1B+2M+2m; claude round-1 all-FIXED + 1M ≡ codex N1; all fixed)

Codex round 2 on the round-1 delta: NOT CONVERGED — 1 new BLOCKER + 2 new
MAJORS in the hard corners, 2 residual minors. Claude round 2 ruled ALL
EIGHT round-1 findings FIXED with clean probes; its single MAJOR is
identical to codex N1 (and it proved the window pre-exists in 27efecc).
All fixed on-branch:

1. **N1 (codex B ≡ claude M) — cancellable lens.run leaked unregistered
   sessions/PTYs:** the shux-rpc server drops in-flight handler futures on
   client disconnect (the P3 contract); a drop between graph-session
   creation and registry commit leaked a phantom `__scratch-*` session —
   visible in `session.list` as an ORDINARY session, uncounted by quota,
   invisible to the restart registry (claude confirmed the PTY task's
   shutdown binds to the ROOT daemon token, never the request, so
   handler-drop can also never cascade teardown). Fix (the recommended
   spawn-shield shape): the non-idempotent core (reserve → create session
   → spawn PTY → commit + arm reaper → audit) now runs in its OWN spawned
   task (`spawn_scratch_core`); the handler awaits its JoinHandle —
   dropping a JoinHandle does not abort the task, so a disconnected client
   simply never reads the response while the composite completes and the
   ttl/max_runtime reaper owns the scratch from commit onward. The
   `--wait` tail stays freely cancellable per P3 semantics. The task-local
   caller identity is re-scoped around the spawned core (spawn does not
   propagate task-locals), keeping scratch.create attribution truthful —
   pinned by the existing `production_lens_audit_caller_identity`. Test
   (`production_lens_run_dropped_mid_core_leaves_no_orphan`): two
   deterministic interior pause points (`test_hooks::PAUSE_AFTER_CREATE`,
   `PAUSE_BEFORE_COMMIT` — armed Notify pairs; the core signals reached
   and blocks until released), the dispatch future is aborted mid-window
   (the exact disconnect shape), the released core must COMMIT; asserts
   no phantom (every `__scratch` graph session ∈ `registry.ids()`),
   quota exact, full cleanup on kill, zero stray processes.

2. **N2 (codex M) — startup reap orphaned same-group descendants:**
   `process_start_token(pgid) == None` (leader gone) used to skip the
   kill — but `sh -c 'sleep 999 & exit'` leaves the sleep IN the group
   after the leader exits, and a pgid stays allocated (unrecyclable as a
   new PID) while ANY member lives, so a live group with a dead leader is
   OURS. Fix: leader-gone rows now go to the kill sequence, whose
   `killpg(pgid, 0)` probe decides liveness. Residual edge (whole group
   died AND the pgid was recycled by an unrelated NEW group inside the
   restart window) is the same class as the §17 M14 double-fork
   tolerance — noted here as the PRD-adjacent record. Test
   (`startup_reap_kills_orphaned_descendants_when_leader_is_gone`): real
   `sh -c 'sleep 300 & exit 0'` group, leader reaped, token unreadable,
   group alive via the descendant → startup reap kills it (killpg probe
   errs after); the recycled-PID mismatch test stays green.

3. **N3 (codex M) — "kill confirmed" was not a real condition:**
   `kill_pgid_lens_sequence` returned `true` even when the post-SIGKILL
   confirmation loop timed out with the group still signalable, and
   callers then removed the registry row — resurrecting the B3 orphan
   window for stubborn/unreaped groups. Fix: honest `KillOutcome`
   tri-state (AlreadyDead / Died / Unconfirmed); `kill_confirmed` retries
   the full sequence once (500 ms backoff) and reports honestly;
   `reap_scratch` returns `false` on unconfirmed (no teardown, no destroy,
   no reap audit — ERROR log) and BOTH callers (`scratch_reaper`,
   `on_session_killed`) leave the registry row for the next daemon's
   startup reap. Injectable confirmation via
   `TEST_FORCE_UNCONFIRMED_KILL` (short-circuits before any signal).
   Tests: `forced_unconfirmed_kill_is_reported_honestly` (unit) +
   `production_unconfirmed_kill_preserves_registry_row` (production
   session.kill with the forced flag → row survives, no reap audited).

4. **Minors:** (a) corrupt-registry evidence is timestamped
   (`.json.corrupt.<unix_ms>`) so repeated corrupt startups never
   overwrite earlier evidence (second-corruption case tested). (b) The
   audit hash chain now CARRIES ACROSS rotated files: rotation writes an
   `audit.rotate` header entry chained off the rotated-out file's final
   hash (naming its predecessor as a historical label); new
   `verify_chain_set` walks `.5→…→.1→live` as ONE chain, so deleting or
   reordering an interior rotated file is detectable
   (`audit_rotation_carries_the_chain_across_files`: two real rotations,
   set verifies, swap `.1`/`.2` → fail, restore → pass, delete interior
   `.1` → fail). Documented residual: the oldest file's predecessor is
   legitimately discarded (keep-5), so deleting the ENTIRE rotated set
   remains undetectable by construction. (c) claude nit: `bytes_returned`
   now uses the exact padded-base64 decoded length (`b64_decoded_len`).

**Accepted-as-documented (claude round 2):** explicit `session.kill` on a
scratch now blocks up to ~2.5 s awaiting group-death confirmation
(LENS-R-042 sequence + bounded retry) — intended behavior, no action.

## P5 convergence round 3 (2026-07-10 — codex: N1/N2 FIXED; N3-at-startup + 1 new minor; both fixed)

Codex round 3 on the round-2 delta: N1 FIXED (shield + JoinError mapping +
cfg-gated hooks verified), N2 FIXED (killpg(0) probe semantics confirmed;
M14-class residual accepted). Two remaining items, both fixed on-branch:

1. **N3 NOT FIXED at the STARTUP path (now fixed):** round 2 preserved
   registry rows on Unconfirmed in the timer reaper and
   `on_session_killed`, but `startup_reap` still treated Unconfirmed as a
   mere `killed=false`, wrote a `scratch.reap` audit row anyway, and then
   UNCONDITIONALLY deleted `scratch-registry.json` — a stubborn group
   surviving the startup reap became invisible to every future restart
   reap (the B3-class hole relocated to startup). Fix: rows now resolve
   INDIVIDUALLY — Died/AlreadyDead rows are audited and dropped;
   Unconfirmed rows get an ERROR log, NO reap audit, and are RE-PERSISTED
   via the shared `persist_rows_atomic` helper (temp+rename, the same B2
   atomicity as the runtime persist); the file is deleted only when every
   row resolved (an empty survivor list and "no file" are the same state).
   Test (`startup_reap_repersists_unconfirmed_rows_for_the_next_restart`):
   real live group + forced-unconfirmed flag → after the first startup the
   row is still in the persisted file (valid JSON, same pgid), zero
   `scratch.reap` audit entries; a second startup with the flag cleared
   reaps it for real (group dead, file removed, reap audited).

2. **Minor (new) — self-declared trust anchor (fixed):** `verify_chain`
   adopted the FIRST line's `prev_hash` as the trust anchor, so deleting a
   PREFIX of lines inside a single file passed verification. Fix: anchors
   must be externally justified, never self-declared — new `ChainAnchor`
   {`Exact(hash)` | `TrustedStart`}: `verify_chain` requires the first
   entry to be genesis-rooted (strict `Exact(GENESIS)` verify) or an
   `audit.rotate` continuation header (which delegates to the full
   `verify_chain_set` walk — only a verified predecessor justifies the
   anchor); anything else is rejected with "unjustified chain anchor".
   `verify_chain_set`'s oldest-present file applies the same structural
   rule (genesis or rotate header), so a prefix deletion inside the oldest
   rotated file is caught too. Test (`audit_prefix_deletion_is_detected`):
   delete the first 2 lines of a live log → both `verify_chain` and
   `verify_chain_set` fail naming the unjustified anchor; intact log and
   the existing rotation/tamper tests stay green.
