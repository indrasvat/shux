# Council substitution — `dootsabha` unavailable

`dootsabha` is not installed in this environment. Per CLAUDE.md *Tooling
fallbacks*, the design and implementation-diff council steps were not skipped:
the implementing session (the parent of this QA audit, not the QA gate itself)
ran two parallel adversarial agents on disjoint surfaces. Their results are
recorded here as reported by that session; the QA gate independently
re-verified the VT-relevant claims (see `SOLID-QA.md`).

This file is the record `evidence-manifest.json` names for both
`dootsabha_design` and `dootsabha_implementation`.

## Agent A — `Grid::clear_above` loop (shux-vt)

- Ran the old index loop and the new `range_mut` loop side by side on 1080
  grids: rows 1–5, cols 0/1/4, scrollback 0/1/3/10, scrollback limit 0/2/5
  (with eviction), dirty tracking on/off, `end_row` 0..rows+2, and
  copy-on-write clones.
- Identical full grid state for every `end_row < rows`.
- Four range mutants of the new loop: all red.
- The only production caller (ED 1, `ESC[1J`, `parser.rs`) always passes
  `end_row = cursor.row - 1 < rows`.
- **P3:** for `end_row >= rows` (unreachable from the caller) the new loop
  panics before writing anything, where the old loop panicked part-way through.
  Not a behaviour change on any reachable input.

## Agent B — Makefile / CI / dependency removal

- `make clippy-beta` matches the CI command under CI's environment, fails
  loudly when the beta toolchain is missing, and is listed by `make help`; no
  guard needed updating.
- The four removed dependencies are unused (whole-word grep across the
  crates), doc tests pass, beta clippy is clean with no manifest warnings, and
  `cargo deny` bans/licenses/sources pass.
- **P3:** the target's echo line named `cargo` rather than `make` — fixed in
  the branch since.

## Disposition

No P0/P1/P2 findings. Both P3s are dispositioned: Agent A's is unreachable and
fails earlier (safer) than before; Agent B's was fixed.
