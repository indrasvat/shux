# DootSabha implementation-diff review — task 078

**Status: CONVERGED (v2 retry).** The v1 dispatch timed out (both agents errored); the v2 retry
produced real reviews. Records:

- `.local/078-impl-v2-codex.md` — **Verdict: CLEAN** (no BLOCKER/MAJOR/MINOR).
- `.local/078-impl-v2-agy.md` — **Verdict: ISSUES** (3 findings, all addressed below).

DoD item satisfied: *"Implementation-diff DootSabha convergence review is clean or all findings
addressed"* — codex is CLEAN, and every agy finding is resolved. Verified independently by the
`shux-vt-solid-qa` gate against committed HEAD `8f7c324`.

## agy findings → resolution

| # | agy severity | Finding | Resolution | Proof (verified this audit) |
|---|---|---|---|---|
| 1 | BLOCKER | `s.chars().count() as u16` in the string-run arm wraps for ≥65536-char runs, so a huge out-of-bounds run could pass `col+span<=cols` | Explicit guard rejects `n > u16::MAX` before the cast (real bug) | Fixed in `910d8c3`; `capture.rs` string-run arm returns `NonCanonical` for over-width runs; shux-vt 310/310 |
| 2 | BLOCKER | `MaskTrue` deser might fail to round-trip `{"mask":true}` or accept `{"mask":false}` | **Not a real bug** — speculative (agy: *"if MaskTrue is a unit/ignored-field deser…"*). The actual impl (`capture.rs:291–299`) deserializes a `bool` and returns `Err("mask marker must be true")` on `false`. codex reached the same conclusion. Pinned by a test | `mask_true_round_trips_and_rejects_false` PASS: round-trips `{"mask":true}`, rejects `{"mask":false}` |
| 3 | MAJOR | Regional-indicator flag emoji (`🇺🇸`) captured as `["🇺🇸",""]` widths its first scalar (=1), so the validator falsely rejects the valid wide capture | Width logic reconciled: single-scalar entries use base-scalar width; multi-scalar graphemes (VS16 width-1, flags/ZWJ width-2) trust the self-describing `""` structure (R3), not `UnicodeWidthChar` of one scalar (real bug) | Fixed in `910d8c3`; `flag_emoji_validates` + `vs16_emoji_presentation_validates` PASS |

**Net:** 2 real bugs fixed (`910d8c3`: `fix(078): dootsabha impl-review round — flag-emoji width +
span-overflow guard`, `crates/shux-vt/src/capture.rs` +94/-27), 1 speculative BLOCKER shown to be
a non-issue and pinned by a regression test. codex CLEAN. Convergence achieved.
