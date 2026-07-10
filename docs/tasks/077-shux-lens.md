# Task 077: shux lens — give every agent eyes

**Status:** Done (through P4) — P4 `pane.checkpoint` + `pane.diff_since` IMPLEMENTED on `feat/lens-p4-checkpoint-diff` (branched from origin/main d3b6282, includes P3 #90 + v0.40.0); `make test-lens` **27 passed / 10 failed** (D1–D5 + A1 flipped green; the 10 reds are R1–R8 lens.run + K1/E1 P6-golden/loop, all untouched). SOLID VT QA (heat scope) **VERDICT: PASS** (08ff46b); verifier VERIFIED-WITH-NOTES, goldens **RATIFIED** (3d70a31). Convergence round 1 (codex 1B+1M, claude 1B+1m) all fixed on-branch: dims-gated resize invalidation (RED-FIRST), store-marker guard, -32011/-32010 wire-shape pins, REAL-attach survival test. Convergence round 2: codex CONVERGED + claude CONVERGED (zero new defects; `<`-boundary adjudication upheld by both). Shipping as PR. (P0, P1, P2, P3 complete; P3 `pane.wait_settled` implemented + verifier VERIFIED — S1–S5, V1 green, `make test-lens` 21 passed / 16 failed (remaining reds all -32601 on P4/P5 methods); `s1_ready.png` golden RATIFIED (verifier re-render byte-identical, ddebb43); P3 review round fixed on-branch: codex B1 deadline precedence + claude TOCTOU guard (bf975bc), codex B2 shux-rpc cancellable request execution — client disconnect drops in-flight waiters (04032a2), codex M1 pane-killed-mid-wait → NOT_FOUND (7ee229a), codex M2 strict param typing (1a7981b), round-2 B2 completion — unbounded frame queue + MAX_PENDING_FRAMES=256 pipelining cap so EOF detection is never starved (3dff905, mechanism pre-reviewed by claude as correct), round-3 deterministic frame-error responses — decode errors no longer race conn_closed, InvalidData-keyed split pinned in codec tests (9e21103); awaiting the lean codex pass on the frame-error commit; P4–P6 pending)
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
| **P4** | checkpoints + `pane.diff_since` (§7) | D1–D5, A1 + attached-client concurrency | SOLID VT QA (heat) |
| **P5** | scratch + `lens.run` (§8, §9) | R1–R8 | audit entries asserted; serial-only |
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
