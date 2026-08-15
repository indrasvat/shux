# Implementation-diff review — substituted for `dootsabha council`

`dootsabha` is not installed in this environment (`command -v dootsabha` → not
found). Per CLAUDE.md *Tooling fallbacks*, the council step is not skipped: it is
replaced by **parallel adversarial agents on disjoint surfaces that drive the real
system**, every finding reproduced before it is believed. Two ran against this
diff, plus the `shux-vt-solid-qa` hard gate as a third, independent pass.

| agent | surface | outcome |
|---|---|---|
| A | the verification machinery this branch added or changed | 7 reproduced defects |
| B | whether the code move is behaviour-preserving | no behaviour-changing defect found |
| VT gate | rendering, VT surface, rich-TUI compatibility | rendering claim verified; FAIL on the evidence contract, since cleared |

The surfaces are genuinely disjoint: A never judged the Rust move, B never judged
the guards, and the gate judged neither — it re-derived the rendering claim from
scratch.

## Agent A — guard machinery. Findings and disposition

Two of these were defects **I introduced on this branch**. Both are fixed here,
and the fix is proven against the agent's own reproduction.

### Fixed: the MOVE amnesty laundered a same-package deletion (P1)

The first cut of the cross-crate move rule matched a removed leaf name against
*every* addition in the workspace. The agent deleted 815 bytes of real assertions
from `crates/shux/tests/lens_gate_compare.rs`, added an empty function of the same
name to `crates/shux/tests/lib_target.rs` — same package, different test binary —
and the guard reported "moved to another crate", claimed the test still ran, and
exited 0. Both claims were false.

Matching is now **cross-package only**: a removed `(package, leaf)` pairs only
with an addition of that leaf under a *different* package. The agent's exact
reproduction now fails, correctly:

```
error: these tests no longer run anywhere in the workspace:
    - cell_tier_palette_unportable_is_fail_not_silent_pass  (was in shux)
exit 1
```

What is still not caught, and is documented in the script rather than claimed
away: deleting a test in crate A while adding a same-named stub in crate B, with
a trailer. Name-based matching cannot see that, and names are all this guard ever
has. It is bounded by printing every move in full and requiring a deliberate
trailer.

### Fixed: `check-no-bin-mods.sh` missed the most idiomatic form it exists to ban (P2)

`pub(crate) mod foo;` evaded the regex — `pub` followed by `(` is not `pub`
followed by whitespace. So did `pub(super)`, `pub(in crate::x)`, a same-line
attribute, `mod r#gen;`, an uppercase name, and a declaration wrapped across two
lines. The agent then built a minimal two-target crate proving the hazard is real
and not theoretical: guard green, `cargo build` failing with the exact
same-name-different-type error the guard exists to prevent.

Rewritten from a per-line regex to a scan that blanks comments and string
literals, then matches across newlines. All 11 evasions are caught; inline
`mod tests { }`, prose mentioning `mod foo;`, and string literals containing it
still pass. The guard also now reads `[[bin]] path` from `Cargo.toml` instead of
hardcoding `src/main.rs` — the agent showed that repointing the binary left the
guard inspecting a file the binary does not build.

### Fixed: `--write-baseline` poisoned its own cache from a dirty tree (P2)

The list comes from the working tree; the cache key names the *committed*
`crates/` tree. The agent deleted one test in the working tree only, published,
restored, and left a cached "baseline" permanently short a test that tree
contains — licensing its future deletion for free. It now refuses to publish from
a dirty `crates/` tree.

### Accepted, pre-existing, reported not fixed

- **A same-named stub anywhere in the same package hides a deletion (P1).** This
  is the module-path-dropping design the guard has always had, and it predates
  this branch: before #150 those unit tests already shared one binary id. Fixing
  it means pinning module paths, which would fail on every move the guard exists
  to permit. Not this PR's to change.
- **112 tests across 16 `test = false` binaries are outside the guard's view
  (P2).** `cargo nextest list` cannot enumerate them, so neither can the guard —
  including the six suites this branch edits. Architectural; worth its own issue.
- **One `TEST-MOVE:` trailer authorises every removal in the range, and can
  arrive from a sibling branch merged in (P3).** Same range-scan model
  `check-lens-frozen.sh` uses. Noted; tightening it means naming the leaf in the
  trailer.
- **A pure rename hard-fails with no escape hatch (P3).** Genuinely
  indistinguishable from delete-plus-add by name alone.
- **Sort order would break at ≥10 duplicate `(binary, leaf)` pairs (P4).** Max in
  this workspace is 2; unreachable today.

### Attacks that failed to break the guards

Ten forgeries of the `TEST-MOVE:` trailer — prose mid-body, trailing paragraph,
fenced code block, indented, quoted, subject-line, lowercase, `#`-prefixed, empty
and whitespace-only values — **all rejected**; only a real trailer parses.
Deleting tests with no compensating addition always hard-fails. `check-gate-docs.sh`
genuinely reads the exit-code table at its new path (emptied file, renamed
function and deleted file all fail correctly). The cache disc-suffix prevents
cross-version format comparison. The nextest group change was verified by
*listing*: `binary_id(=shux::bin/shux)` matches **0** tests, confirming the old id
was dead. CI-environment parity holds under `CARGO_TERM_COLOR=always CI=1`.

## Agent B — refactor equivalence. What it proved

No behaviour-changing defect. The checks that matter:

- **Line accounting.** Comments stripped, `vocab.rs` differs from its origin by
  **one** line, `cell_compare.rs` by four, `pixel.rs` by three — every one an
  import path. No assertion, string literal, serde attribute, JSON key, error
  message or constant changed.
- **Serde contract, actually serialized.** Two standalone probe binaries — one
  against base's `shux_vt`/`shux_raster`, one against HEAD's `shux::gate::*` —
  emitted **108 values**; `diff` clean. Includes the full 144-pair `worst()`
  matrix, the 15-field `is_stale_vs` staleness matrix, the 9-cell `evaluate_tier`
  matrix, `deny_unknown_fields` still rejecting on all five structs, and
  `to_string_pretty` of the `report.json` array SHA-pinned.
- **`exit_code()` verified arm by arm** from live values, not by reading.
- **The locally-computed digest is not a weakening.** The new `frame_digest` was
  run against base's `capture_sha256` over **24 real frames** (8 programs × 3
  geometries, alt-screen, truecolor, wide CJK, scrollback): 0 mismatches, with a
  negative control proving the comparison can fail.
- **Golden wire compatibility.** Goldens blessed by the base binary are
  byte-identical to HEAD's, and each binary accepts the other's, cross-blessed
  both directions.
- **Public API** shrank by exactly 30 items in `shux-vt` and 15 in `shux-raster`,
  zero additions, zero modifications.

Its one finding — a broken intra-doc link in `lib.rs` (`gate::cells` for a module
named `cell_compare`) — is fixed and committed here.

## What this substitution does not give you

A council converges opinions; parallel adversaries do not converge, they
accumulate. Nothing here weighed whether the *destination* module names are the
right ones, or whether the guard hardenings are the design a maintainer would
have chosen — only whether the code does what it claims and whether the guards
catch what they claim. That judgement is the reviewer's, and the PR says so.
