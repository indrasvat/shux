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
cargo nextest list --workspace | grep -c lens_gate    # want: 0
cargo nextest list --workspace | grep -c 'capture::\|gate::'   # want: >0 (L1 in CI)
```

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

## 10. QA gates   ⏳
- `shux-vt-solid-qa`: `.shux/qa/078-.../SOLID-QA.md` (VERDICT: PASS).
- Implementation-diff dootsabha convergence review: clean.

---

## Adversarial review

Parallel adversarial agents attacked the schema (round-trip/canonicalization
holes), the gate contract (exit-map/rollup), and the freeze guard (bypass
vectors); a grok-build research agent mined xAI's solved harness for gotchas.
Findings + resolutions recorded below.

_(results appended as they land)_
