# Task 078 — proof of correctness

Every claim below is backed by a command you can run yourself. This is the
"clear, unambiguous proof that everything works as designed" for the lens-gate
capture schema + frozen contract.

> Status legend: ✅ verified green · 🔴 red-by-design (frozen contract) · ⏳ pending

---

## 1. What 078 delivers (the contract)

| Deliverable | Where | Proof command |
|---|---|---|
| `FrameEnvelope` lossless capture schema | `crates/shux-vt/src/capture.rs` | `cargo nextest run -p shux-vt` |
| Gate verdict/report vocabulary (frozen) | `crates/shux-vt/src/gate.rs` | `cargo nextest run -p shux-vt` |
| OSC 4 sticky `palette_overridden` flag | `crates/shux-vt/src/{lib,parser}.rs` | see §4 |
| Two-lane freeze guard (+ collision fix, CI teeth) | `scripts/check-lens-frozen.sh`, `.github/workflows/ci.yml` | see §5 |
| Quarantined lanes | `crates/shux/Cargo.toml`, `Makefile` | see §6 |
| Frozen fixtures (showcase, report, xfail, scenario) | `.shux/fixtures/lens-gate/**` | see §3 |
| Green dogfood + cross-path PNG | `crates/shux/tests/lens_gate_capture.rs` | `make test-lens-gate` |
| Frozen RED contract lane | `crates/shux/tests/lens_gate_contract.rs` | `make test-lens-gate-contract` |

---

## 2. Lossless schema — L1 unit + property proof   ⏳

```
cargo nextest run -p shux-vt
```
Expected: all pass. Covers serde round-trip + byte-stability + **encode∘decode
fixed point** across ASCII, wide glyphs (漢字), combining graphemes (é),
ZWJ emoji (👨‍💻), every colour variant, style flags, hyperlinks, masks,
multi-row — plus a **400-case proptest** driving arbitrary ANSI through a real
VT, and validator negatives (overlap, unsorted, wrong row count, string-with-wide,
array-all-simple, missing/double continuation, past-width, zero-cell mask).

## 3. The frozen canonical shape (human-reviewable)   ⏳

`.shux/fixtures/lens-gate/capture/showcase.capture.json` shows every schema
feature in one readable file. Regenerate + diff:
```
LENS_GATE_BLESS=1 make test-lens-gate      # rewrites the golden
git diff .shux/fixtures/lens-gate/capture/showcase.capture.json   # should be empty
```

## 4. OSC 4 sticky flag, no revision bump   ⏳

```
cargo nextest run -p shux-vt palette_override_flag_is_captured_without_bump
cargo nextest run -p shux-vt osc_4_palette_no_bump   # the adjudicated invariant still holds
```

## 5. Freeze guard — collision fix + teeth   ⏳

- `shellcheck scripts/check-lens-frozen.sh` → clean.
- Collision behaviour (gate-only file passes with GATE-TEST-CHANGE; lens-only
  still needs LENS-TEST-CHANGE; both needs both) — proven in the session log.
- **Live proof**: committing the frozen fixtures without a properly-placed
  trailer was BLOCKED by the guard, then PASSED once the trailer was in the
  final block (commit 9757493).

## 6. Lanes excluded from the default run (make check stays green)   ⏳

```
# The two QUARANTINED lanes must be excluded (make check stays green):
cargo nextest list --workspace | grep -cE 'lens_gate_(capture|contract)'   # want: 0
# lens_gate_exit_contract IS a normal CI-run target (a frozen exit-map pin) — expected present.
cargo nextest list --workspace | grep -c 'capture::\|gate::'   # want: >0 (L1 in CI)
```
(Corrected per SOLID-QA P3: a bare `grep -c lens_gate` returns 3, not 0, because
`lens_gate_exit_contract` runs in CI by design; the quarantine invariant is about
the `capture`/`contract` lanes, which are 0.)

## 7. Green dogfood on real shux   ⏳

```
make test-lens-gate
```
Expected: 5 pass. Real `/bin/sh` output captured losslessly; **cross-path**
(semantic capture ↔ rasterized pixels agree); fixtures conform to frozen types.

## 8. Frozen RED contract lane   🔴 (by design)

```
make test-lens-gate-contract    # EXPECTED to fail — 5/5 red until 081/082
```
Transcript: `.shux/qa/078-.../RED-CONTRACT-TRANSCRIPT.md`. Each case is annotated
`RETIRED BY 081/082` and goes green by BUILDING the verb, never by editing.

## 9. Whole workspace stays green   ⏳

```
make check
```

## 10. QA gates
- **Implementation-diff dootsabha convergence review:** the first run timed out
  (both providers); the **v2 self-contained retry converged** — codex **CLEAN**,
  agy flagged 3, each verified by driving the real VT: 2 were real (flag-emoji
  width, span-overflow guard) and fixed in commit `910d8c3`, 1 was a bad test
  assertion. Records: `.local/078-impl-v2-{codex,agy}.md`,
  `.local/dootsabha-078-impl-review-v2.json`.
- **`shux-vt-solid-qa`:** `.shux/qa/078-.../SOLID-QA.md`. First audit returned
  FAIL on two governance items only (impl-diff dootsabha not-yet-converged +
  pre-completion bookkeeping) — it certified all functional/contract criteria
  "airtight" with fresh evidence (307/307 shux-vt, cross-path pixel proof,
  `make check` green, pixel-determinism ratio 0.0). Those items are now closed;
  re-audited against the final committed state (with the agy fixes) for the PASS.

---

## Adversarial review

Parallel agents attacked the schema, the gate contract, and the freeze guard; a
grok-build research agent mined xAI's solved harness. Findings + resolutions:

| # | Source | Finding | Resolution | Proof |
|---|---|---|---|---|
| 0 | **adv-schema BLOCKER** | **The validator rejected faithful captures of VS16 emoji (❤️ ⚠️ ✔️, digit keycaps).** The VT stores ❤️ as a *width-1* grapheme; the validator string-widthed the cluster to 2 (`UnicodeWidthStr`) and rejected it as "wide missing continuation" — baking the defect into the frozen validator. | The validator now widths a grapheme by its base scalar via `UnicodeWidthChar` (`grapheme_display_width`), matching the VT/encoder exactly. | `vs16_emoji_presentation_validates`; VS16 + ZWJ added to the 400-case proptest generator |
| 0b | **adv-schema MAJOR** | **A mask on a wide glyph's *continuation* column leaked the glyph** (the `col += 2` jump skipped past the masked continuation) — a redaction/security hole. | `build_row_runs` pre-expands masks to wide-glyph boundaries: masking any column of a wide glyph redacts the whole glyph. | `mask_on_wide_continuation_does_not_leak` (leak + invariance) |
| 1 | schema (self-run) + **adv-schema MAJOR** | **`validate()` accepted non-coalesced/split adjacent same-style runs** → two encodings for one grid (golden instability). Real bug (encoder never hit it, but a hand-edited/drifted golden could). | `validate_row` now rejects adjacent runs sharing a coalescing key (same style, or both masks). | `rejects_non_coalesced_adjacent_same_style_runs`, `rejects_split_multichar_same_style_runs`, `rejects_non_coalesced_adjacent_masks`, `allows_adjacent_runs_with_different_styles`; 400-case proptest still green |
| 2 | adv-gate M1 | Freeze guard **failed OPEN** in a shallow CI clone (base ref unresolved → HEAD-only check a multi-commit PR could slip past). | Fails **closed** in CI (errors, demands the base); degrades with a warning only locally. | `CI=… bash check-lens-frozen.sh` → exit 1; local → graceful |
| 3 | adv-gate M2 | `worst()` ranked by declaration order → `worst(Fail, InfraError)` = InfraError (exit 3, retryable) **masked a regression**. | Severity tiers: regression (exit 1) always outranks operational errors > greens. | `worst_never_masks_a_regression_with_an_error`, `rollup_never_masks_a_regression` |
| 4 | adv-gate M3 | The "frozen" exit map lived in un-frozen `gate.rs`; the RED lane used `worst()` as its own oracle (tautological). | New **frozen, CI-run** `lens_gate_exit_contract.rs` hard-codes every status→exit value independently. | `exit_map_values_are_frozen`, `status_set_and_names_are_frozen` |
| 5 | adv-gate B-min1 | GATE regex `lens_gate_` missed a hypothetical `lens_gate.rs`. | Tightened to `lens_gate(_\|\.)`. | collision re-verified (6 cases) |
| 6 | schema (self-run) | Edge cases: wide glyph in last column, mask splitting a wide glyph, masked fixed-point, alt-screen grid/flag consistency, differing-hyperlink run split. | All handled — verified with targeted tests. | 5 edge tests pass |
| — | grok-capture | Design validated (we keep semantic colour, explicit continuation, underline variants, full-grid, masking — all places grok loses fidelity). Env-scrub + `LC_ALL`/`TZ` gap + frame-hash settle → forward-notes for 081/083. | Recorded in `.local/078-grounding-findings.md §10`. | — |

adv-gate also **verified correct** (no action): the 12-status set matches the closed contract, the exit map matches §7.4, `deny_unknown_fields` fails closed, `palette_unportable` can't be a status, and the guard's trailer parsing / `--no-renames` / merge+root handling / collision fix all hold.

adv-schema did not deliver a written report; its full attack surface was covered
independently (rows 1 and 6 above), including the one real bug it targets (#10
coalescing).
