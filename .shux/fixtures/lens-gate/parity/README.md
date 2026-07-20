# Lens-gate parity corpus (task 079) — provenance

Each scenario is three files: `<n>.a.json` + `<n>.b.json` (two `FrameEnvelope`
frames) and `<n>.diff.json` (the expected `shux_vt::FrameDiff`, canonical
sorted-key JSON). `crates/shux/tests/lens_gate_parity.rs` reloads each pair, runs
`shux_vt::diff_frames`, and asserts it reproduces `<n>.diff.json` **bit-for-bit**.

## Why this is an independent oracle (council #3 / design D6)

The `.diff.json` oracles were minted by the **pre-extraction** daemon function
`compute_lens_diff` (in `crates/shux/src/main.rs`), over live `Grid`/cursor/defaults
inputs, by the one-shot generator `tests::gen_lens_gate_parity_corpus` — **before**
that function was deleted and its semantics moved to `shux_vt::diff_frames`. The
generator hand-mapped the old `LensDiff` to the `FrameDiff` JSON shape (NOT via
`FrameDiff::Serialize`), so the frozen oracle does not depend on the new type's
serialization. The post-extraction test therefore checks the moved function against
frozen data produced by the deleted function — it is not self-referential.

## Regenerating (rare)

The generator was removed with `compute_lens_diff` (keeping a dead copy would
reintroduce the two-implementation hazard the extraction removed). To regenerate
faithfully you MUST check out the pre-extraction revision (the commit that still
contains `compute_lens_diff` + `gen_lens_gate_parity_corpus`, on branch
`feat/lens-ci-gate`) and re-run it there — regenerating from the current
`diff_frames` would make the parity lane tautological.

## Freeze

This directory is on the GATE freeze lane: any change requires a
`GATE-TEST-CHANGE:` commit trailer (`scripts/check-lens-frozen.sh`). That guard,
plus this provenance note, is the durable defense for the corpus's independence.
