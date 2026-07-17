VERDICT: PASS

# SOLID VT QA — Task 078: lens gate — capture schema + frozen contract suite

**Re-audit of the FINAL committed state.** Supersedes the first audit (FAIL @ `2714e01`),
which failed on two governance items only and certified every functional/contract
criterion airtight. Both items are now closed and re-certified against HEAD.

- **Task file:** `docs/tasks/078-lens-gate-capture-schema-contract.md`
- **Audited commit:** `8f7c324558796a4540e48925c51cd17eb93a1b48` (branch `feat/lens-ci-gate`)
- **Delta re-certified since `2714e01`:** `910d8c3` (agy impl-review fixes to the FROZEN
  validator `crates/shux-vt/src/capture.rs`, +94/−27) and `8f7c324` (PROOF/learnings docs).
- **Auditor:** shux-vt-solid-qa (audit-only; no product source edited; no git mutated).
- **Scope note:** Task 078 is a serde-schema + frozen-red-contract task in `shux-vt`. Its
  Non-Goals explicitly state **"No PNG/pixel work"** — it changes serialization types and a
  validator, **not** the raster/render path. The pixel gate is therefore satisfied by a
  rasterizer-determinism metric (threshold 0) + the in-repo cross-path semantic↔pixel test,
  not by a committed golden PNG (none is defined by the task).

---

## 1. Commands run (this audit, at HEAD)

| Command | Result |
|---|---|
| `git rev-parse HEAD` | `8f7c324…` (matches expected) |
| `cargo nextest run -p shux-vt` | **310 passed, 0 skipped**, exit 0 |
| `cargo nextest run -p shux-vt -E <4 agy-fix tests>` | **4 passed** (`flag_emoji_validates`, `mask_true_round_trips_and_rejects_false`, `rejects_overlong_string_run`, `vs16_emoji_presentation_validates`) |
| `make test-lens-gate` | **5 passed** (incl. `semantic_capture_agrees_with_rasterized_pixels`); leak-guard exit 0 |
| `make test-lens-gate-contract` | **5 FAILED — RED by design**, exit 2 (retire in 081/082) |
| `make lint` | fmt-check + clippy `-D warnings`, exit 0 |
| `shellcheck scripts/check-lens-frozen.sh` | clean |
| `make check-lens-frozen MSG=<HEAD subject>` | exit 0 |
| `make check` | **All checks passed**, exit 0 |
| `pixel_verify.py` (2 independent HEAD snapshots, threshold 0) | ratio 0.0, mean Δ 0.0, **pass** |

---

## 2. Task DoD Matrix

| DoD item | Status | Evidence |
|---|---|---|
| DootSabha design review incorporated before coding (OSC 4, canonical shape, `CellRef` rule) | **PASS** | R1–R10 in task file map to council rulings; records `.local/078-design-{codex,agy}.md`, `.local/078-tiebreak-{codex,agy}.md`, `.local/078-q6-decisive-finding.md`, `.local/dootsabha-078-{design-review,tiebreak}.json` present at HEAD |
| Red contract suite committed first + demonstrably failing | **PASS** | `make test-lens-gate-contract` → 5/5 red (exit 2); `RED-CONTRACT-TRANSCRIPT.md` committed; each case annotated with its retiring task (081/082) |
| L1/L2 tests pass (except intentionally-red contract) | **PASS** | 310/310 shux-vt; 5/5 green dogfood lane |
| `make check` and `make check-lens-frozen` pass | **PASS** | both exit 0 |
| `shux-vt-solid-qa` reports `VERDICT: PASS`; evidence under `.shux/qa/078-*/` | **PASS** | this report + manifest + PNGs + pixel metric written under `.shux/qa/078-…/` |
| Implementation-diff DootSabha convergence review clean or all findings addressed | **PASS** | v2 converged: **codex CLEAN**; agy flagged 3, each independently verified in `capture.rs` at HEAD (2 real fixes in `910d8c3`, 1 hypothetical proven-correct). Records `.local/078-impl-v2-{codex,agy}.md`, `.local/dootsabha-078-impl-review-v2.json` |
| `docs/PROGRESS.md` + task Status updated; learnings appended | **POST-PASS** (not an open blocker) | Learnings appended (`docs/agents/learnings.md`) and a PROGRESS session-log entry present, **but** task `Status:` is still `In Progress` and the PROGRESS task-table row is not yet `Done`, so `make check-progress` FAILS. This is the acknowledged completion step that can only run **after** a clean QA verdict (the QA PASS is itself a DoD prerequisite for Done). See §8. |

---

## 3. Testing Matrix

| Layer | Verdict | Evidence |
|---|---|---|
| Unit (shux-vt) | **PASS** | 310/310, incl. serde round-trip, byte-stability, encode∘decode fixed point |
| Integration (crate-level) | **PASS** | `make test-lens-gate` 5/5 (real `/bin/sh` capture, fixtures conform to frozen schema) |
| Raw byte / replay | **PASS** | 400-case proptest drives arbitrary ANSI (incl. VS16 ❤️⚠️ + ZWJ 👨‍💻) through a real VT; flag emoji 🇺🇸 covered by the deterministic `flag_emoji_validates` |
| Shux automation | **PASS** | fresh `pane.capture` + `pane.snapshot` at 80×24 & 120×40 via `target/release/shux`, isolated runtime, cleaned up |
| Visual inspection | **PASS** | both PNGs opened + inspected; §5 |
| Pixel comparison | **PASS** | determinism ratio 0.0 @ threshold 0; cross-path semantic↔pixel test green |
| DootSabha design | **PASS** | converged; records present |
| DootSabha impl-diff | **PASS** | v2 converged (codex CLEAN, agy 3 addressed) |

### Frozen-validator (`capture.rs`) fixes independently verified

- **Span-overflow guard (agy #1, REAL):** `capture.rs:1079-1088` checks `n > u16::MAX`
  before the `as u16` cast, and the array path uses `saturating_add`; a wrapped span can no
  longer slip past `col + span <= cols`. Proof: `rejects_overlong_string_run` (70k-char run
  rejected).
- **Flag / regional-indicator width (agy #3 + adv-schema BLOCKER, REAL):** `capture.rs:1105-1122`
  applies the authoritative `UnicodeWidthChar == 2` check **only to single-scalar entries**;
  multi-scalar graphemes (flags/ZWJ/VS16) trust the self-describing `""` structure (R3) rather
  than re-deriving width. Proof: `flag_emoji_validates`, `vs16_emoji_presentation_validates`.
- **MaskTrue deser (agy #2, hypothetical):** the code was already correct — `Deserialize`
  for `MaskTrue` (`capture.rs:291-300`) parses a bool and **rejects `false`** with a custom
  error. `mask_true_round_trips_and_rejects_false` proves round-trip of `true` and rejection
  of `{"mask":false}`.

---

## 4. Screenshot Matrix

| Viewport | Command | Screenshot | Pixel metric | Diff | Status |
|---|---|---|---|---|---|
| 80×24 | color-probe (truecolor/idx-208/red-bold-ul/green-bg/CJK/emoji/combining) | `.shux/qa/078-…/pane-80x24.png` (720×456, `cbc14564…`) | `.shux/qa/078-…/pixel-metrics-determinism.json` ratio 0.0 | `.shux/qa/078-…/diff-80x24-ab.png` | **PASS** |
| 120×40 | same probe | `.shux/qa/078-…/pane-120x40.png` (1080×760, `f30d1fbb…`) | (determinism proven at 80×24) | — | **PASS** |

No committed golden PNG baseline exists (task non-goal "No PNG/pixel work"); the pixel gate is
rasterizer determinism at threshold 0 plus the cross-path Rust test. The fresh-vs-prior-audit
capture differs by 1.48% pixels (mean Δ 0.65/255) solely because this audit's ad-hoc probe used
a truecolor **foreground** on the TRUECOLOR line where the prior probe used a truecolor
background block — a probe-authoring difference between two independent audits, not a render
change (the agy fix touches only the serialization validator, never the raster path).

---

## 5. Visual inspection notes

Both frames opened as images. All four color classes render **non-gray and distinct**:
truecolor cyan `(80,170,255)`, indexed-208 orange, basic red **bold+underline**, and
green-bg/black-fg. No SGR-reset color bleed, no ghost cells, no layout drift between
breakpoints, cursor block clean. The wide CJK (`漢 字`) and combining `é` are preserved
**losslessly in the capture text** (verified via `pane.capture`); they render as tofu / `´e`
in the raster **only** because the default raster font chain lacks CJK/combining glyph
coverage — font-coverage artifact of an ad-hoc probe, independent of the semantic capture this
task delivers, not a task-078 regression.

---

## 6. Findings

- **P0 / P1:** none.
- **P2:** none open.
- **P3 (informational):** the re-audit brief described the proptest generator as including
  "VS16 + ZWJ + flag graphemes"; the generator (`capture.rs:1688-1690`) carries VS16 (❤️ ⚠️)
  and ZWJ (👨‍💻), while flag emojis are covered by the dedicated deterministic
  `flag_emoji_validates` test rather than the random generator. Coverage is complete (arguably
  stronger for that case); this is a wording nuance only.

---

## 7. Passed evidence (regenerated this audit, at HEAD)

- `.shux/qa/078-…/pane-80x24.png`, `.shux/qa/078-…/pane-120x40.png` — full-resolution frames.
- `.shux/qa/078-…/pixel-metrics-determinism.json` — ratio 0.0 @ threshold 0.
- `.shux/qa/078-…/diff-80x24-ab.png` — determinism diff (empty).
- `.shux/qa/078-…/evidence-manifest.json` — all required top-level keys, verdict PASS.
- Test logs: 310/310 shux-vt; 5/5 green lane; 5/5 red lane (by design); `make check` exit 0.

---

## 8. Residual risk & remaining post-PASS step

**Residual risk: low.** All functional, contract, governance, DootSabha (design + impl), and
evidence criteria are satisfied with fresh evidence at HEAD.

**The single remaining action is post-PASS bookkeeping (implementer, not a QA blocker):**

1. Flip `docs/tasks/078-*.md` `Status:` → `Done` and set the `docs/PROGRESS.md` task-table row
   to `Done` (makes `make check-progress` green — it currently FAILS on the stale `In Progress`).
2. `git add` the six now-untracked QA evidence files under `.shux/qa/078-…/`
   (`SOLID-QA.md`, `evidence-manifest.json`, `pane-80x24.png`, `pane-120x40.png`,
   `diff-80x24-ab.png`, `pixel-metrics-determinism.json`) and commit them together with the
   bookkeeping. The auditor cannot commit: the shux checkout is a **shared worktree** across
   concurrent agents and git mutation is forbidden this session. `.shux/qa/` is tracked
   (not gitignored), so these files are in the correct durable location, ready to commit.

---

## 9. shux session cleanup

All audit sessions (`solid-vt-078-80x24-a/-b`, `solid-vt-078-120x40`, `solid-vt-078-dbg`) were
killed and the isolated daemon (`XDG_RUNTIME_DIR=/tmp/q078`) stopped; `pgrep` shows **no stray
daemon in the isolated runtime**. Pre-existing `~/.local/bin/shux __daemon` processes belong to
the 7 other concurrent agents in this session and were left untouched. Leak-guarded lanes
(`make test-lens-gate`, `make check`) returned exit 0 with clean guards.
