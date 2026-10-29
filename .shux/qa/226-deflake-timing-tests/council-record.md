# Council record: 226-deflake-timing-tests (#226)

`dootsabha` is not installed in this environment, so neither council ran as
`dootsabha council`. Per CLAUDE.md *Tooling fallbacks*, each was replaced by
parallel adversarial agents on disjoint surfaces that drove the real system.
Every finding was reproduced before it was fixed, and each fix carries a test
seen failing first.

## Design (step 1)

No council ran before coding. The design (shux-testkit condition waits, a
clippy ban, an advisory PostToolUse hook plus a CI guard, docs/testing.md
behind a short CLAUDE.md rule) was reviewed late, by a design-review agent
on the finished branch. Its findings, all addressed:

- Guard false positives on annotated fns, `select!` arms and closures: the
  `expect` now covers the whole item it sits on (4c90343).
- Cargo.toml `warn` levels were flagged though `-D warnings` keeps the ban:
  only `allow`/`expect` levels are flagged now (4c90343).
- Short `timeout(1-2 s)` bounds left in attach and uds test modules: moved to
  `BUDGET`.
- lens_glance's token cap could stop the pump before the last glance: removed;
  the deadline is a panic bound only.
- The paused-clock auto-advance trap got a row in docs/testing.md.

## Implementation diff (step 7)

Three agents on disjoint surfaces, plus the adversarial test-waits agent and
the simplification review.

| Surface | Findings fixed |
|---|---|
| Enforcement and CI | Dotted and inline Cargo.toml `clippy` keys, `allow(warnings)`, spaced `clippy :: …` paths and an `expect` on a module/impl/trait all bypassed the guard; `nix`/`libc`/`sleep_ms` sleeps were not banned; `clippy-beta` did not pin `CLIPPY_CONF_DIR`. |
| Production seams | Corrupt-registry evidence that was linked but whose original could not be removed was reported lost (8c681c7). |
| Test waits (adversarial) | Three tests could pass with their defect present: the successor-files stop race, the garbage-manifest descendant fixture, the kill-mid-frame exit match (801fe9c). `ps` scans that failed counted 0 processes. |
| Simplification | Guard waivers mis-scoped, `terminate` blind to zombies (72d2f08), shared teardown/gate/serve helpers (a511e08). |
| VT gate F1 | Restamping a window on every batch went uncaught: `output_inside_a_window_does_not_extend_its_deadline` (083e122), seen failing against that mutant. |

Declined or out of scope, stated in the PR: asserting a time bound on attach's
signal restore (that would be a timing test again), and the pre-existing
findings reported to the coordinator rather than filed.
