VERDICT: PASS

# SOLID VT QA — shux lens Phase P2 (`pane.glance`)

**Active task:** `docs/tasks/077-shux-lens.md` (Phase P2 sections)
**Normative spec:** LENS-PRD-20260704 §5 (LENS-R-010..016), §14 (P2 SOLID scope), §16.3 (golden discipline), §17 (font/tofu risk)
**Branch:** `feat/lens-p2-glance` — 5 commits atop `origin/main` (e049060)
**Audited commit:** `d3cd1b9` (HEAD) — *fix(lens): presented-frame consistency under sync + checkpoint guard*
**Auditor:** `shux-vt-solid-qa` subagent, independent regeneration (release binary, isolated XDG). Implementer claims were not reused as evidence.

PASS is scoped to the P2 SOLID mandate (§14): the glance PNG raster path vs the F1/F5 goldens, glance-vs-snapshot parity, and determinism. Two governance closures (baseline-approval flip, P2 council JSON + ledger) remain the orchestrator's phase DoD; see Residual Risk. They are P2 (paperwork), not raster defects, and do not gate this raster verdict — but they DO gate overall P2 phase completion.

---

## 1. Task DoD Matrix (PRD §5.3 P2 + §14 SOLID scope)

| DoD item | Status | Evidence |
|---|---|---|
| LENS-R-010..016 exercised; G1, G2, G2w green via CLI **and** RPC | PASS | `make test-lens` → 15 passed / 22 failed; G1/G2/G2w all PASS (`.shux/out/lens-p2-qa/test-lens.txt`). All 22 failures are `-32601 method_not_found` on unimplemented P3/P4/P5 methods — zero golden/pixel/byte-mismatch failures. |
| G2 revision == `session.snapshot` content_revision (substrate cross-check) | PASS | G2/G2w bodies assert equality and pass; independent RPC drive confirmed glance `revision` present + consistent. |
| Determinism micro-test: same clone rendered twice → byte-identical PNG | PASS | `shux-raster tests::glance_clone_renders_byte_identical_twice` PASS; plus live double-render (F1 rev 359, F5 rev 216) byte-identical (`pixel_metrics/f{1,5}_determinism_g1_vs_g2.json`). |
| Glance PNG path == existing snapshot render path (LENS-R-013, no fork) | PASS | glance vs `pane.snapshot` at same frame: byte-identical, 0 changed pixels (`pixel_metrics/f{1,5}_glance_vs_snapshot.json`). |
| Goldens byte-exact under `pixel_verify` 0.0 semantics | PASS | Independent re-render (release binary, isolated XDG) of F1 & F5 == committed goldens, 0 changed pixels, `cmp` identical (`pixel_metrics/f{1,5}_glance_vs_golden.json`). |
| Goldens visually inspected at full resolution (§16.3) | PASS | Both PNGs + contact sheet inspected as images; see Screenshot Matrix. Tofu confirmed width-correct; cursor hidden; no corruption/bleed. |
| Golden provenance committed (evidence-manifest + BASELINE-APPROVAL) | PASS (data) / RESIDUAL (approval flip) | Manifest sha256 (4 files) + 5 font hashes match; BASELINE-APPROVAL present but still marked PROVISIONAL (R1). |
| `make check` / corpus byte-stable (no render-path regression) | PASS | `make test-vt-corpus` byte-exact (3 replay tests + full verify); existing goldens untouched by P2 shux-vt changes. |
| Converging dootsabha review of the phase diff (§2.4) | RESIDUAL (R2) | Codex impl-diff round ran + fixes landed in-diff (83d92f9, d3cd1b9); claude convergence council in-flight during this audit; JSON not yet committed, §A2 P2 ledger empty. Orchestrator-owned. |
| Frozen-path changes carry `LENS-TEST-CHANGE:` trailer (§16.2) | PASS | `f3_flip.sh` + `lens_fixtures_smoke.rs` changed only in commit `0396cc1`, which carries `LENS-TEST-CHANGE: p2-g1 F3 sync-wrap`. |
| Process hygiene: zero new shux daemons | PASS | Anchored `ps`/`pgrep` after every run: no `target/release/shux` daemon survivors; isolated XDG removed. |

---

## 2. Testing Matrix

| Layer | Result | Evidence |
|---|---|---|
| Unit (raster determinism) | PASS | `shux-raster tests::glance_clone_renders_byte_identical_twice` 1/1 pass |
| Integration (red suite, CLI + RPC twins) | PASS for P2 gate | `make test-lens` 15/22; G1/G2/G2w green; remaining 22 = `-32601` only |
| Raw byte / replay (VT corpus) | PASS | `make test-vt-corpus` byte-exact; 3 replay tests pass (invalid-bytes, chunk-invariance, response determinism) |
| shux automation (daemon-backed live drive) | PASS | Independent release-binary drive of F1 (80×24) + F5 (100×30) with truecolor + 256 + basic color content; `.shux/out/lens-p2-qa/drive-summary.txt` |
| Visual inspection (full-res PNGs) | PASS | Both goldens + contact sheet read as images; no clipping/bleed/tofu-width/ghost-cell/cursor defects (tofu is the documented, width-correct font limitation) |
| Pixel comparison (`pixel_verify.py` exact) | PASS | 6/6 comparisons 0 changed pixels; `.shux/qa/lens-p2/pixel-metrics.json` |
| DootSabha design | EXISTS / RESIDUAL | PRD converged (4 rounds, §A1/§A2); raw JSON in scratchpad outside worktree |
| DootSabha diff review | EXISTS / RESIDUAL (R2) | Codex round output landed in-diff; claude convergence in-flight; JSON + ledger not yet committed |

Color-probe requirement met: F1 and F5 both carry explicit truecolor (`38;2`/`48;2`), 256-color (`38;5`/`48;5`), and basic ANSI blocks — a monochrome/NO_COLOR regression could not pass these goldens.

---

## 3. Screenshot Matrix

| Viewport | Command / fixture | Screenshot (this audit) | Pixel baseline | Diff | Status |
|---|---|---|---|---|---|
| 80×24 | `pane.glance` on `f1_static.sh` | `.shux/qa/lens-p2/evidence-f1-glance-80x24.png` | `.shux/goldens/lens/g2_f1_80x24.png` | `.shux/out/lens-p2-qa/metrics/f1_glance_vs_golden.diff.png` (all-zero) | PASS (0 px) |
| 100×30 | `pane.glance` on `f5_wide.sh` | `.shux/qa/lens-p2/evidence-f5-glance-100x30.png` | `.shux/goldens/lens/g2w_f5_100x30.png` | `.shux/out/lens-p2-qa/metrics/f5_glance_vs_golden.diff.png` (all-zero) | PASS (0 px) |
| 80×24 | glance vs `pane.snapshot` | `.shux/out/lens-p2-qa/artifacts/f1_snapshot.png` | glance F1 | (all-zero) | PASS (0 px) |
| 100×30 | glance vs `pane.snapshot` | `.shux/out/lens-p2-qa/artifacts/f5_snapshot.png` | glance F5 | (all-zero) | PASS (0 px) |
| both | committed contact sheet | `.shux/goldens/lens/contact-sheet.png` | — | — | PASS (readable, labelled) |

Visual findings (native resolution):
- **F1 (80×24):** rounded/heavy/light box border with centered `LENS-F1-STATIC`; truecolor gradient bar; 256-color strip; 16-color blocks; Devanagari row + CJK row render as **width-correct tofu** (bundled fonts lack those scripts); emoji `✓ ✗ ⚠` render as real colored glyphs; cursor hidden. No color bleed after SGR resets, no ghost/stale cells, no cursor artifact.
- **F5 (100×30):** `LENS-F5-WIDE` title; heavy box-drawing cross-joins; CJK fullwidth as width-correct tofu (2-cell advances, grid alignment intact); VS16 emoji row as real glyphs; combining-Devanagari as tofu with real `·` separators; gradient/256/basic strips grid-aligned. No wide-cell head/tail corruption.
- Tofu width-correctness cross-checked against the `.txt` goldens (correct fullwidth column counts) and confirmed by intact box-drawing/gradient alignment in the same frames.

---

## 4. Findings (by severity)

- **P0:** none.
- **P1:** none.
- **P2 — R1 (governance):** `BASELINE-APPROVAL.md` still reads `PROVISIONAL — NOT YET APPROVED` and `evidence-manifest.json` has `"provisional": true`. This QA PASS is the approval trigger the file's own step 2 anticipates; the flip is orchestrator-owned (I am scoped to `.shux/qa/lens-p2/` only and did not modify the goldens dir).
- **P2 — R2 (governance):** P2 phase-diff converging dootsabha review (§2.4) JSON is not committed on-branch and §A2 P2 ledger row is empty. Actual council output exists and shaped the diff (codex round → fixes in 83d92f9 + d3cd1b9; claude convergence in-flight), so this is a record-keeping gap, not an absent review. Must be committed before the P2 PR is marked done.
- **P3 — note:** `§16.3` (implementation PR may not mint goldens in the same PR that changes rendering code) is technically stretched: P2 both changes `shux-vt`/`shux-raster` and mints the goldens. This is disclosed and mitigated by (a) PROVISIONAL status, (b) this independent QA gate re-rendering the goldens byte-for-byte from committed code, and (c) the in-flight design-review council. No raster consequence.

No P0/P1 findings ⇒ no forced FAIL/BLOCKED.

---

## 5. Passed Evidence (summary)

1. `make test-lens` 15/22 — G1/G2/G2w green; 22 failures all `-32601` (expected phase state).
2. Independent re-render (release binary, isolated XDG) byte-identical to both committed goldens — 0 changed pixels, `cmp` identical.
3. Determinism: raster unit test PASS + live double-render byte-identical at fixed revisions (F1 359, F5 216).
4. Glance-vs-`pane.snapshot` parity byte-identical (LENS-R-013 upheld).
5. `make test-vt-corpus` byte-exact — no render-path regression.
6. Golden integrity: 4 golden sha256 + 5 font sha256 match `evidence-manifest.json`.
7. Visual inspection of both full-res goldens + contact sheet — correct, width-correct tofu, cursor hidden, no corruption/bleed.
8. Frozen-path change carries the required `LENS-TEST-CHANGE:` trailer.
9. Zero leaked shux daemons.

---

## 6. Residual Risk

- **R1 / R2 (P2, orchestrator-owned):** flip BASELINE-APPROVAL + `provisional:false`, and commit the P2 dootsabha convergence JSON + fill §A2 ledger, before the P2 PR is completed. This QA report is the citation for R1.
- **Font/tofu limitation (accepted, PRD §17):** CJK/Devanagari render as tofu because the bundled OFL font chain lacks those scripts. Goldens remain valid for what they assert (width/layout/color determinism, byte-exact). Any future font-chain expansion re-baselines every golden via `LENS-TEST-CHANGE` + re-approval.
- **Scope:** this gate covers the P2 raster/glance path only. P3–P6 methods (`wait_settled`, `checkpoint`, `diff_since`, `lens.run`) are intentionally unimplemented (their red tests fail `-32601`) and out of P2 scope.

---

## 7. Cleanup Status

- `make test-lens` ran serially under `.shux/scripts/no_leak_guard.sh` (`-j 1`).
- Independent QA drive used an isolated `XDG_RUNTIME_DIR` under `/tmp/lp2q-<pid>`, killed only daemons it spawned (baseline-diffed), and removed the runtime dir.
- Post-run anchored `ps`/`pgrep`: **0** `target/release/shux` daemon survivors.
- One concurrent `dootsabha consult` (`agent_review_guard.sh lens-p2-claude-full`) was observed — this is the orchestrator's in-flight P2 convergence council, **not** a QA leak; left untouched.

*Scratch artifacts (all PNGs, diffs, per-comparison JSON, run logs, drive script): `.shux/out/lens-p2-qa/` (gitignored).*
