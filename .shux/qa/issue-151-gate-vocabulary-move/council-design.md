# Design review — substituted for `dootsabha council`

`dootsabha` is not installed in this environment (`command -v dootsabha` → not found).
Per CLAUDE.md *Tooling fallbacks*, the council step is not skipped: it is replaced by
parallel adversarial agents on **disjoint** surfaces that drive the real system, and the
substitution is named here and in the PR.

## The design question, settled before coding

Issue #151 says it "cannot start before #150 lands". #150 had not landed — `crates/shux`
had no `lib.rs`, and `main.rs` still owned all 25 module declarations. The six frozen
contract tests under `crates/shux/tests/lens_gate_*` import the gate vocabulary, and a
`[[bin]]`-only package exports nothing an integration test can reach. So the move was
blocked on a library target, and there were three ways forward:

1. **Report blocked.** Rejected: it delivers nothing, and the prerequisite is small now
   that #149 has distributed `main.rs` into modules.
2. **One commit doing both.** Rejected on #150's own reasoning — *"bundling it here would
   make neither verifiable"*. The public-API diff that proves #151 correct is only legible
   if the target split is a separate step.
3. **Two commits on one branch, each independently reviewable.** Chosen. #150's commit
   stands alone (its own guard, its own demonstration test); #151's commit is then a pure
   move whose diff is import paths and doc comments.

## Placement, decided up front

The three modules land in the existing `crates/shux/src/gate/` tree rather than as new
top-level modules, because that is where every consumer already lives:

| from | to | why this name |
|---|---|---|
| `shux-vt/src/gate.rs` | `gate/vocab.rs` | it *is* the vocabulary: status set, exit map, report schema |
| `shux-vt/src/gate_compare.rs` | `gate/cell_compare.rs` | cell-tier comparator; `gate/compare.rs` already exists and is the runner-level one |
| `shux-raster/src/gate_pixel.rs` | `gate/pixel.rs` | pixel/exact tiers |

`cell_compare` rather than `compare` specifically to avoid colliding with the existing
`gate/compare.rs`, which is a different thing (runner signal adaptation).

## Verification designed before the change, not after

The change claims "zero behavioural difference", so the matrix was chosen to be able to
*falsify* that, and every cell is an A/B against a worktree of the base commit
(`/tmp/shux-base`) rather than against expectations:

- public `pub use` lists — must shrink by exactly the gate blocks, gain nothing
- `--help` for every subcommand — must be byte-identical
- gate verdicts + exit codes — both daemon-backed suites, plus a direct CLI matrix
  covering four distinct verdicts and four distinct exit codes
- rendering — pixel comparison at exact 0/0 thresholds on real colour-probed workloads
- test inventory — the set of test names, which is exactly what a cross-crate move puts
  at risk
- `cargo tree` — no dependency added anywhere

## Risk identified at design time, and what was done about it

Moving tests between crates is precisely the case `check-test-inventory.sh` exists to
catch, so it was expected to fire, and it did — twice, for two different reasons (the
bin→lib target rename, then the genuine cross-crate move). Neither was routed around.
The first is a false positive and was fixed at the root; the second is real and now
requires a `TEST-MOVE:` trailer, matching the house idiom already used by
`check-lens-frozen.sh`. Both amendments were re-proven to fail on a genuine deletion.
