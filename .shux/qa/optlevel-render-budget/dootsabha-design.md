# DootSabha design council — `[profile.test] opt-level = 0`

Council on the DESIGN decision, before judging the implementation.

Raw `dootsabha council --json` output, run by the QA gate (not by the implementer),
from an isolated worktree of the audited commit. providers={'agy': 'error', 'codex': 'ok'} duration_ms=220230 tokens_in=837906

## Synthesis (chair)

Ārya, unified position: **approve lowering test builds to `opt-level = 0`, but do not treat that as performance evidence.** Correctness and performance need separate proof.

**Decision**

`[profile.test] opt-level = 0` is semantically sound for defined deterministic Rust code. It should affect compile/runtime speed and code shape, not safe Rust semantics, integer behavior, debug assertions, overflow checks, iterator order, or float reassociation rules.

The caveat is narrow but real: differences can surface through UB, unsafe dependency code, timing-sensitive tests, `HashMap` iteration order, float rounding at layout boundaries, SIMD/rasterization behavior, nondeterministic PNG metadata, or tests depending on allocation/inlining/destructor timing.

For the compositor path, opt0 vs opt1 should produce identical VT bytes for a fixed fixture because output appears driven by deterministic layout traversal and row-major buffer diffing, not map iteration. Still, this must be proven.

**Performance Ceiling**

`100_000us` is not the right default ceiling if the stated rule is “10x observed max.” With an observed opt0 max of `10_365us`, 10x is `103_650us`, so `100_000us` is slightly below the claimed headroom.

If a catastrophic wall-clock guard remains in default tests, use **`125_000us`**. It is loose enough for noisy CI and still catches major regressions. But it must not be described as proving the PRD budget.

**Better Design**

Default tests should verify rendering correctness:

- ANSI byte snapshots / exact VT output
- dirty-cell counts
- deterministic layout traversal
- decoded RGBA equality for raster/PNG paths

Performance should move to a release-profile benchmark lane:

- Criterion/Divan or equivalent bench
- explicit `make bench-render` or CI perf job
- release-profile timing distribution
- PRD budget validation there, not in unit/integration tests

An env-gated wall-clock assertion is acceptable for a perf lane, e.g. `SHUX_ASSERT_RENDER_WALL_CLOCK=1`, but default tests should not claim `p50 <= 8ms` or `p99 <= 25ms`.

**Evidence Required Before Approval**

Require:

- cold CI A/B showing opt0 actually reduces wall time
- `make test` passing under opt0
- opt0/opt1 byte-identical ANSI output for deterministic compositor fixtures
- opt0/opt1 decoded RGBA-identical raster output for representative fixtures
- repeated CI timing distribution for any retained wall-clock guard
- removal or correction of any default-test comment/assertion claiming PRD 8ms compliance

**Reject Conditions**

Reject the change if:

- opt0 vs opt1 produces unexplained ANSI or RGBA mismatches
- `100_000us` is presented as PRD compliance
- default tests still claim to enforce the 8ms frame budget
- wall-clock assertions remain in default tests without isolation or explicit rationale

Bottom line: **merge the profile change only with correctness proof; move real frame-budget enforcement into release-profile benchmarks.**

## Dispatch

### codex

Ārya, council position: **approve the direction, not as a performance proof**. Moving test builds to `opt-level = 0` is sound if rendering correctness is separately proven byte-for-byte, and if the 8ms PRD budget stops living in default unit tests.

**Q1. `opt-level = 0` Soundness**

Position: **Sound, with a narrow semantic caveat.**

Changing only `[profile.test] opt-level` from `1` to `0` should change **speed and code shape**, not defined Rust semantics. It does **not** inherently change:

- `debug_assertions`: controlled by `debug-assertions`, still enabled for test profile unless separately changed.
- `overflow-checks`: controlled separately, still enabled for test profile unless separately changed.
- integer results: safe Rust integer behavior remains the same, including checked overflow in test builds.
- float semantics: Rust does not enable `-ffast-math` equivalents by changing `opt-level`; no reassociation contract should appear just from `0` vs `1`.
- iterator order for deterministic iterators: `Vec`, row/column loops, and tree traversal stay deterministic.
- rendered bytes, for defined deterministic code.

Where a real behavioral difference could hide:

- latent UB or unsafe code in dependencies, especially font/raster code;
- timing-dependent tests or runtime paths using `Instant`, sleeps, settle windows, deadlines;
- output depending on `HashMap` iteration order;
- float thresholds around layout split rounding, especially `f32 * ratio).round() as u16` in layout computation;
- uninitialized memory, target-feature-sensitive SIMD, or panic/backtrace/assertion text differences;
- tests that accidentally rely on inlining, allocation layout, or destructor timing.

For the compositor path I read, the output path is mostly deterministic: layout traversal, row-major buffer diffing, and crossterm byte emission. The current profile is already `opt-level = 0` in [Cargo.toml](/private/tmp/qopt-wt/Cargo.toml:90).

**Q2. Can Opt Level Change VT Grid Or PNG Content?**

Position: **For VT/compositor bytes, no mechanism in defined code. For PNGs, still verify.**

The compositor builds cells into a `FrameBuffer`, diffs row-major, and emits ANSI commands from that dirty-cell vector. The `HashMap` in `MultiPaneFrame` is used for keyed lookup by pane id, not as the output ordering source. Pane order comes from layout traversal. Border output is generated from a deterministic flat vector grid. So for a fixed VT state, config, terminal size, and layout, opt0 vs opt1 should produce identical ANSI bytes.

That belief is wrong if any of these are true:

- UB or uninitialized memory exists locally or in dependencies;
- output order starts depending on `HashMap` iteration;
- timing affects selected content, viewport, cursor state, or settle state;
- float rounding crosses a layout boundary;
- randomized IDs, timestamps, or environment state reach visible output;
- rasterization uses unsafe/SIMD/platform floating behavior that changes pixels;
- PNG encoding includes nondeterministic metadata.

For PNGs, I would compare decoded RGBA bytes, not just PNG file bytes, because encoders can change metadata/chunk details while pixels remain identical.

**Q3. Is `100_000us` The Right Ceiling?**

Position: **No. If kept in default tests, I would use `125_000us`; better still, move real perf to a bench.**

Observed opt0 max is `10_365us`. A strict 10x headroom rule gives `103_650us`, so `100_000us` is just under the stated 10x rule. It is probably fine most days, but it is not materially safer than the edge it claims to avoid.

My number if this remains a default test assertion: **`125_000us`**.

Rationale:

- about 12x over the observed max;
- still catches catastrophic regressions;
- leaves room for CI descheduling and noisy shared runners;
- does not pretend to validate the PRD’s p50 <= 8ms / p99 <= 25ms target.

But a wall-clock assertion inside a unit/integration test is a bad instrument for the PRD budget. The repo’s own nextest config says wall-clock tests need special scheduling isolation, yet the compositor wall-clock tests are not in that group. The wall-clock group currently enumerates only two tests in [.config/nextest.toml](/private/tmp/qopt-wt/.config/nextest.toml:188).

**Q4. Strictly Better Design**

Position: **Yes. Default tests should verify correctness; release-profile benches should verify frame budget.**

Best design:

- Default tests assert rendering correctness: dirty-cell counts, ANSI byte snapshots, cross-path logical equality, decoded RGBA equality.
- Remove default PRD-budget assertions from unit/integration tests.
- Add a release-profile benchmark for compositor render latency and gate it in a perf lane or explicit `make bench-render`.
- Keep a loose catastrophic guard only if it is opt-in or isolated as a wall-clock test.

Specific options:

- `cfg!(debug_assertions)`: **not useful** here. Both opt0 and opt1 test profiles normally have debug assertions.
- env-gated assertion: acceptable for local/CI perf lanes, e.g. only assert when `SHUX_ASSERT_RENDER_WALL_CLOCK=1`.
- delete assertion and move to Criterion/Divan bench: best long-term design.
- keep opt-level 1 only for timing crates: not my choice. It preserves a special test semantic/codegen island and keeps timing assertions in the wrong layer.

Current repo also has an implementation smell: [Makefile](/private/tmp/qopt-wt/Makefile:394) says there are no real cargo bench targets and no Criterion dependency, while the compositor comment says the real budget belongs to a release-profile bench in [compositor.rs](/private/tmp/qopt-wt/crates/shux-ui/src/compositor.rs:1283).

**Q5. Evidence Required**

I would demand:

- cold CI A/B showing opt0 saves the claimed wall clock, with same cache pruning behavior;
- `make test` under opt0;
- byte-identical compositor output between opt0 and opt1 for deterministic fixtures;
- decoded RGBA-identical raster output between opt0 and opt1 for representative fixtures;
- timing distribution for the retained catastrophic guard across repeated CI runs;
- no remaining default test that claims to enforce the PRD 8ms frame budget.

Cheapest falsifier for “rendering output is byte-identical between opt-level 0 and 1”:

Build and run the same deterministic render fixture twice, once with `CARGO_PROFILE_TEST_OPT_LEVEL=0` and once with `CARGO_PROFILE_TEST_OPT_LEVEL=1`, dump the captured `Cursor<Vec<u8>>` ANSI stream and decoded RGBA buffer hashes, then `cmp`/hash compare them. One differing byte falsifies the claim.

Reject conditions:

- Any opt0/opt1 byte or RGBA mismatch without a clear nondeterministic reason.
- Keeping the default test comment/assertion that says it beats the PRD 8ms budget. There is still one at [compositor_tests.rs](/private/tmp/qopt-wt/crates/shux-ui/tests/compositor_tests.rs:57).
- Calling `100_000us` evidence of PRD compliance.
- Keeping wall-clock assertions in default tests without either nextest isolation or an explicit rationale.

### agy

{
  "provider": "agy",
  "model": "",
  "content": "",
  "duration_ms": 0,
  "cost_usd": 0,
  "tokens_in": 0,
  "tokens_out": 0,
  "error": "invoke agy: agy: Error: timeout waiting for response"
}
