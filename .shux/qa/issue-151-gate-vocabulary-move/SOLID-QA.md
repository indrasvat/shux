VERDICT: PASS

# VT SOLID QA — gate vocabulary moved out of `shux-vt` / `shux-raster` (#150 + #151)

Scope: PR #164, branch `claude/issue-151-refactor-9jvtkw`, base `6c18a16`.

## What this audit had to decide

The change claims **zero rendering difference**. That claim is why the gate applies
at all — every VT-crate edit in the diff is a removal or a comment:

| inside `crates/shux-vt/src` + `crates/shux-raster/src` | |
|---|---|
| deleted | 2,065 lines (the three moved-out modules) |
| removed from the two `lib.rs` | 18 lines of `mod` + `pub use` |
| doc comments rewritten | 3 lines |
| **inserted, excluding comments and blanks** | **4 lines, all `Cargo.toml` dependency entries** |

No rendering code was modified. The audit's job was to prove that claim against
the running binary rather than accept it from the diff.

## Method

A/B against a worktree of the base commit (`/tmp/shux-base`, binary
`/tmp/shux-base-target/debug/shux`) versus HEAD. Every comparison drives the real
daemon and a real PTY. No fixture stands in for a workload.

## Pixel evidence — 19 cases, all 0 changed pixels at exact 0/0 thresholds

Produced by `.claude/automations/pixel_verify.py` via `uv run --script`.

| case | what it covers | size |
|---|---|---|
| `f1-80x24` | truecolor + 256 + basic probes, Devanagari/CJK/emoji | 720×456 |
| `f3-80x24` | alternate-screen flip | 720×456 |
| `f5-80x24`, `f5-200x60` | wide cells / CJK at two widths | 720×456, 1800×1140 |
| `f7-resize`, `rz-pre`, `rz-post` | SIGWINCH-aware child across a live resize | 1188×817, 720×456, 1080×760 |
| `f8-80x24` | repaint without geometry change | 720×456 |
| `f10alt-120x40`, `f10ret-120x40` | alt-screen enter and return | 1080×760 |
| `vim-120x40`, `vim-200x60` | real `vim`, pinned view, two widths | 1080×760, 1800×1140 |
| `replay-{btop,lazygit,nvim,vicaya,vivecaka}` | recorded PTY bytes replayed through both builds, grids compared | 1080×684 |
| `probe-120x40`, `probe2-120x40` | colour probes | 1080×760 |

The five `replay-*` cases use recorded PTY bytes rather than live capture,
because screenshot-diffing an animated TUI measures capture timing, not
rendering. An earlier live-capture attempt at `vim` produced a 12% pixel delta
that turned out to be two different scroll positions, not a rendering change —
recorded here because it is exactly the trap this method exists to avoid.

**Comparator proven non-vacuous**: two genuinely different renders → `status:
fail`, exit 1, 22.7% changed. Missing input → exit 2. It is not passing
everything handed to it.

**No screenshots committed.** The baseline is a build of the base commit, not a
file this repo tracks, so per `.shux/qa/README.md` the pixel-metric JSON stands
alone. There is nothing committed to diff a PNG against.

## Behavioural evidence beyond pixels

- `--help` byte-identical across all 85 subcommand blocks (114,862 bytes each side).
- Gate suites identical base vs head: `lens_gate_run` 23/23, `lens_gate_verdict` 22/22.
- Direct CLI matrix over four distinct verdicts and four distinct exit codes —
  `pass`/0, `missing_golden`/1, `scenario_error`/2, `child_error`/5 — byte-identical,
  including the golden bless → compare round-trip that exercises the moved
  fingerprint and cell comparator.
- 108 serde values round-tripped through probe binaries built against both sides:
  byte-identical. This covers `report.json`, the `Fingerprint` sidecar, `GateStatus`
  snake_case names and `Tier`.
- `GateStatus::exit_code()` — every arm unchanged.
- All 50 moved inline tests (20 `vocab`, 17 `cell_compare`, 13 `pixel`) compile,
  list under their new module paths, and keep their leaf names. Verified by
  diffing the name set against `6c18a16`.
- Frozen parity corpus reproduces bit-for-bit.
- 2132 tests pass; `make check` green.
- Four config states identical: default, `config init`, malformed, populated.

## Public surface

`shux-vt` and `shux-raster` shrank by **exactly** the gate re-export blocks — 30
items and 15 items respectively. Nothing else removed, nothing added. Diffed the
`pub use` lists directly.

## Dependencies

Four edges removed, none added. `shux-vt` did not need `sha2`
(`frame_stability_hash` uses a `DefaultHasher`); `shux-raster` needed none of
`serde`, `serde_json`, `sha2`. All demoted to dev-dependencies, where the example
harnesses still use them. `cargo build --workspace --all-targets` clean, so no
normal build path lost anything.

## Process hygiene

Zero leaked daemons or child processes. Identified by pidfile and by `ps` with a
per-run unique needle; never `pgrep -f`/`pkill -f` on a substring. Every
daemon-backed run used an isolated short `XDG_RUNTIME_DIR`, torn down on exit.

## Findings raised during the audit, and their disposition

The first audit pass returned FAIL on the evidence contract — the metrics were
not yet committed. Every technical claim it checked held. Everything it named is
now in this directory.

Three defects were found in guards this branch itself introduced, all fixed and
re-proven (commits `c18ea1f`, `cd66fbc`):

1. The cross-crate MOVE amnesty in `check-test-inventory.sh` matched leaf names
   workspace-wide, so deleting real assertions and adding an empty same-named
   function in the *same* package printed "moved to another crate" and exited 0.
   Now cross-package only.
2. `check-no-bin-mods.sh` missed `pub(crate) mod foo;` and nine other forms.
   Rewritten as a normalising scan.
3. A `char` literal containing a double quote could hide a `mod` declaration from
   that scan. Character literals are now consumed before strings; lifetimes handled.

## Residual risk — stated, not hidden

- A same-named stub within one package still hides a deletion from
  `check-test-inventory.sh`. Pre-existing; the MOVE amnesty did not introduce it.
- 112 tests live in `test = false` binaries and are outside that guard's view.
- One `TEST-MOVE:` trailer authorises every move in its commit range.
- `bash` defers the WINCH trap while blocked in `read`, so `f7_winsize.sh` does not
  reprint under bash. Unchanged by this branch — verified identical before and after.

## Verdict

The rendering claim holds. 19 pixel cases at zero delta across colour, wide
cells, alt screen, resize and five real TUIs; the public surface moved by exactly
the intended set; serialization and exit codes are byte-identical. Evidence is
committed and reproducible from the commands above.

VERDICT: PASS
