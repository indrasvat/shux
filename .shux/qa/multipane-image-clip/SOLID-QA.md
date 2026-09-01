VERDICT: FAIL

# SOLID VT QA — multipane-image-clip (re-audit at `c836d8a`)

## 1. Change under audit

| | |
|---|---|
| Branch | `claude/shux-image-support-s0u4uy` |
| HEAD | `c836d8a` |
| Previously audited HEAD | `3d46d74` (verdict FAIL, one P1) |
| Base | `origin/main` = `0e97d24` (v0.49.0) |
| Delta re-audited | `crates/shux-raster/src/lib.rs` (+40/-2), `crates/shux/tests/window_snapshot_image_rpc.rs` (new, 97), `Makefile` (+6), and this evidence directory |
| Scratch | `.shux/out/multipane-image-clip/` (gitignored) |

Scoped re-audit. The behaviour findings at `3d46d74` are carried forward, not
re-derived; §4 marks each row `carried` or `re-run`.

## 2. Verdict

FAIL, on one new P1 introduced by `c836d8a` itself.

The old P1 is closed — verified independently, not accepted on report. What
replaces it is a regression the same commit introduced: the new per-render
decode budget lets **one pane silently delete a different pane's picture from
`window.snapshot` and `session.snapshot` while that pane's own `pane.snapshot`
still draws it.** That is exactly the cross-path identity this branch exists to
establish and that 18 of the 25 committed metrics assert. It is reproducible for
about 405 KB of ordinary pane output, it is pane-order dependent, it is silent,
and it does not happen on `3d46d74`.

## 3. Delta DoD matrix

| # | Claim made for `c836d8a` | Result | Evidence |
|---|---|---|---|
| D1 | The new RPC test pins the production call site: removing `composite_composed` from `snapshot.rs` fails it while the three in-process tests stay green | **PASS** | Re-ran the mutation myself in `/tmp/shux-mut` @ `c836d8a`. Exit 100; 3 passed, 1 failed; `window.snapshot returned a frame with no picture (0 px); pane.snapshot has 10260`. Log `reaudit-mut-M8.log`. Baseline `make test-window-snapshot-images` → 4/4 (`reaudit-make-target.log`). |
| D2 | The budget cannot fire on a plausible real frame | **PASS, with a corrected number** | Measured, not assumed: a real `kitten icat` of a 4000×3000 photo into a 200×55 pane transmits `a=T,q=2,f=24,o=z,s=1800,v=1350` — the client downscales to the pane. Charged cost = 1800·1350·4 = **9.27 MiB**, so ~27 full-pane photos fit in 256 MiB. Conclusion holds; the "~1 MB" in the source comment and commit message is ~10× optimistic (P2-2). Raw capture `/tmp/sxatk/icat.raw` (2349 APC chunks). |
| D3 | The budget does not perturb the 25 recorded metrics | **PASS** | 9 of 25 re-derived from scratch with the `c836d8a` binary (6 cross-path + 3 outside-pane, 3 viewports): **numerically identical** to the committed JSONs on every field. Independently, mutation M16b proves the accumulation branch is unreachable at these scene sizes. |
| D4 | `window.snapshot` and `pane.snapshot` cannot disagree by straddling the budget | **FAIL — P1-1** | They can, cheaply and deterministically. Dose-response + A/B against `3d46d74` + a `0/0` pixel metric that fails at exactly 17100 px. §6. |
| D5 | The budget closes a measured pane-controlled stall | **PASS** | Independently measured: 24 declared-4096×4096 placements in one pane, one `window snapshot` — `3d46d74` **802 ms**, `c836d8a` **368 ms**. The mitigation is real. |
| D6 | `make test-window-snapshot-images` exists and runs both files | **PASS** | 4 tests across 2 binaries, 2.883 s. |
| D7 | Implementation-diff council step (protocol 7) evidenced | **PASS (substituted)** | `council-substitution.md` §7. Closes P2-1 from the `3d46d74` audit. |
| — | Budget ships with a test seen failing first | **FAIL — P2-1** | Zero tests reach it. §6. |

## 4. Testing matrix

| Layer | Result | Evidence |
|---|---|---|
| Unit (touched crates) | **re-run** PASS | `cargo nextest run -p shux-raster -p shux-vt -p shux-ui` on the pristine tree → **911 passed**. `reaudit-unit-touched.log`. |
| Integration (new file) | **re-run** PASS | `make test-window-snapshot-images` → 4/4. `reaudit-make-target.log`. |
| Integration (workspace) | carried from `3d46d74` | `make test` → 2251 passed. Not re-run; the delta is confined to `shux-raster` + one new test file, and both were re-run directly. |
| Mutation / can-it-fail | **re-run, MIXED** | M8 now **CAUGHT** (was the sole survivor). M16 / M16b are new and expose P2-1. §5. |
| Raw byte / replay | carried | 5 committed `.shux/fixtures/vt-corpus/rich-tui/*.raw` replays, `pixel-richtui-ab-*.json`. Unperturbed: M16b proves the budget never engages on them. |
| Shux automation | **re-run** PASS | 9 `runall.sh` cases + 8 attack/A-B cases + 1 `kitten icat` probe = 18 fresh daemon-backed runs at `c836d8a`, isolated `XDG_RUNTIME_DIR` each. |
| Colour probes | **re-run** PASS | truecolor `38;2;0;200;90`, indexed `38;5;208`, basic `34` in every pane of every fresh run, including both attack panes. Read back from the window PNG: `right_pane_probes [151,109,35]` in all 9 metric cases; visible in `dose4-window.png`. |
| Visual inspection | **re-run** PASS/FAIL-as-found | Full-resolution PNGs opened as images: `dose4-window.png` (victim pane has probes + `RIGHTMARK` and **no picture**), `dose4-paneR.png` (same pane, same instant, picture present), `budget/dose4-diff.png` (one white rectangle, nothing else). No tofu, bleed, ghost cells or layout drift. |
| Pixel comparison | **re-run** PASS on controls, FAIL on the defect | 9 re-derived metrics `0/0 pass`; `budget/pixel-budget-dose3.json` and `-prebudget4.json` `0/0 pass`; `budget/pixel-budget-dose4.json` **fail, 17100 changed px**. |
| Comparator falsifiability | **re-run** PASS | The same comparator, same thresholds, same crop geometry passes `0/0` on dose3 and on the `3d46d74` binary and fails on dose4. That pair is the negative control. |
| DootSabha design | carried, PASS (substituted) | `council-substitution.md` §1. |
| DootSabha implementation diff | **re-run** PASS (substituted) | `council-substitution.md` §7 — now evidenced by two adversarial reviews with measured findings, both applied in `c836d8a`. |
| Process hygiene | **re-run** PASS | 0 daemons, 0 leftover runtime dirs. §9. |

## 5. Mutation matrix for the delta

Run in a detached worktree `/tmp/shux-mut` at `c836d8a` with a shared
`CARGO_TARGET_DIR`; no file in `/home/user/shux` was modified.

| Mutation | Result | Meaning |
|---|---|---|
| `M8_snapshot_no_call` — `composite_composed(...)` → `let _ = &composed.placements;` | **CAUGHT** (exit 100) | The `3d46d74` P1 is closed. The three in-process tests stayed green, so `window_snapshot_image_rpc.rs` is the sole pin and it fails for the right reason. |
| `M16_panic_any_budget` — `None => panic!(...)` | CAUGHT by 1 test | Only `shux-raster::png_bomb::a_png_that_decodes_to_gigabytes_is_refused_before_it_allocates`, with `cost=18446744073709551615` — the `declared_rgba_bytes → None` branch, i.e. the pre-existing oversize refusal that `decode_placement` also performs. Behaviour-preserving. |
| `M16b_panic_accumulation_only` — panic only when `cost != u64::MAX` | **SURVIVED** — 911 + 4 tests green | **The accumulation branch, which is the entire point of the new budget, is reached by zero tests.** |

Two consequences follow from M16b without further builds, and are stated as
derivations rather than runs: (a) the prior audit's M0–M12 are unperturbed,
because the budget never engages on any tested scene; (b) deleting the budget,
or moving the charge *before* the visibility tests so off-screen placements are
billed, must also survive — there is nothing to catch either.

## 6. Findings

### P1-1 — one pane silently deletes another pane's picture from `window.snapshot`, while `pane.snapshot` still draws it (NEW in `c836d8a`, regression vs `3d46d74`)

`MAX_RENDER_DECODE_BYTES` is a **per-render** budget, and the two render paths
scope it differently:

- `composite_placements` (the `pane.snapshot` path) resets 256 MiB **per pane**;
- `composite_composed` (the `window`/`session.snapshot` path) resets 256 MiB
  **once for the whole window**, then spends it first-come-first-served across
  every pane's placements.

So a pane that spends the budget starves whichever pane composes after it.

**Cost of the attack.** The charge is `declared_rgba_bytes` — width × height × 4
from the client's own `s=`/`v=` — not the payload. A solid 4096×4096 PNG is
75 KB on the wire and charges the full 64 MiB. Four of them, 405 KB total, spend
the entire per-render budget. `MAX_PLACEMENTS` is 256 and `MAX_IMAGE_BYTES` is
32 MiB of payload, so a pane may charge up to 16 GiB against a 256 MiB budget.
`C=1` keeps them all on screen, so none is discounted as off-screen.

**Measured, on the real release binary, real daemon, real APC bytes.** Two panes,
200×60 window; left pane = 4 hostile placements, right pane = one legitimate
180×95 picture:

| | `pane.snapshot` (victim) | `window.snapshot` | `session.snapshot` |
|---|---|---|---|
| `c836d8a` | 17100 magenta px | **0** | **0** |
| `3d46d74`, identical scene | 17100 | 17100 | 17100 |

**Dose-response at `c836d8a`,** one variable, monotone:

| hostile placements | charged | `window.snapshot` magenta |
|---|---|---|
| 1 | 64 MiB | 17100 |
| 2 | 128 MiB | 17100 |
| 3 | 192 MiB | 17100 |
| 4 | 256 MiB = the budget | **0** |

**Pixel metric,** identical crop geometry and identical exact thresholds for all
three arms — the comparator is demonstrably able to pass and to fail:

| arm | metric | changed px | ratio | status |
|---|---|---|---|---|
| dose3, `c836d8a` (control) | `budget/pixel-budget-dose3.json` | 0 | 0.0 | pass |
| dose4 scene, `3d46d74` (A/B) | `budget/pixel-budget-prebudget4.json` | 0 | 0.0 | pass |
| dose4, `c836d8a` | `budget/pixel-budget-dose4.json` | **17100** | 0.0521 | **fail** |

`budget/dose4-diff.png` shows a single white rectangle — the missing picture.
Text, colour probes, borders and status bar are byte-identical, so the clip
logic this branch added is still correct; only the picture is gone.

**Three properties make this a defect rather than a stated trade:**

1. **It breaks the branch's own headline acceptance criterion.** Claim 1 of the
   `3d46d74` audit — *"a picture in one pane of a split appears in
   `window`/`session snapshot`, same slice as `pane snapshot`"* — is exactly what
   now fails. Eighteen committed metrics assert it.
2. **It is order-dependent.** Swapping the two panes reverses the outcome: victim
   in the left (composed first) pane survives with 17100 px; victim in the right
   pane vanishes. The same two panes, same content, different composed frame.
3. **It is silent.** No error, no warning, no truncation flag on the RPC
   response. A consumer — `lens gate`, a plugin, an agent — cannot distinguish
   "this pane has no picture" from "this pane's picture was dropped".

The same starvation also applies within one pane: 4 hostile + 1 legitimate
placement in a single pane loses the legitimate one from `pane.snapshot` itself
(`samepane-paneL.png`, magenta 0). So `pane.snapshot`, `lens glance` and
`pane diff --heat` are affected too, not only the composed path.

The commit message discloses *a* trade ("past the budget a picture is not
drawn"), but as a within-one-scene pixel loss. It does not identify the
cross-path divergence, the cross-pane starvation, or the order dependence.

**The mitigation itself is sound and worth keeping** — I reproduced it: 24
hostile placements, one `window snapshot`, 802 ms on `3d46d74` against 368 ms on
`c836d8a`. The defect is in the budget's *scope*, not its existence. Budgeting
**per pane** inside `composite_composed` — the same 256 MiB each pane already
gets from `composite_placements`, spent in the same placement order — bounds the
frame at `panes × 256 MiB` while making the drawn subset identical on both
paths by construction, which restores claim 1 exactly. A pane could then only
starve itself, and would starve itself identically in both verbs.

### P2-1 — the budget ships with no test; nothing in the repository can observe it working or stop it being removed

`grep -rn MAX_RENDER_DECODE_BYTES crates/` returns three hits, all in
`crates/shux-raster/src/lib.rs`. Mutation M16b — panic on genuine accumulation
exhaustion — **survived** 911 unit tests and all 4 image tests. The accumulation
branch is dead code as far as the suite is concerned.

CLAUDE.md: *"Every fix ships with a test seen failing first — failing for the
RIGHT reason."* This fix has none. The commit's own numbers (3.34 s → 0.64 s)
were measured by hand and are not defended by anything that runs in CI, and the
same commit's `window_snapshot_image_rpc.rs` is precisely the argument for why
that matters. A regression test here is cheap: the scene in P1-1 is 405 KB of
deterministic bytes and takes ~2 s.

### P2-2 — the stated safety margin is ~10× optimistic

`crates/shux-raster/src/lib.rs` (and the commit message): *"a `kitten icat` photo
decodes to ~1 MB."* Measured this audit with a real `kitten icat` of a 4000×3000
photo into a 200×55 pane: the client downscales to the pane and transmits
`s=1800,v=1350`, charging **9.27 MiB** — 9× the stated figure, and it scales with
pane pixel area, so a large pane on a high-resolution terminal charges more. The
*conclusion* survives (≈27 such pictures fit in 256 MiB, and normal `icat` usage
keeps one or two on screen), but the margin quoted in the source is not the
margin that exists. Worth correcting where it sits, since it is the only thing
justifying the constant's value.

### P3-1 — `shux daemon stop` can return 0 with the daemon still running (PRE-EXISTING, carried forward)

`crates/shux/src/daemon_boot.rs:308-338` polls `kill(pid,0)` 40×50 ms and on
timeout warns but exits 0. Reproduces on `origin/main`. Every harness in this
audit reads the pid from the pidfile before `daemon stop` and polls `/proc/<pid>`
to disappearance rather than trusting the exit status.

### P3-2 — live attach still draws no pictures (SCOPED OUT, carried forward)

`crates/shux-ui/src/compositor.rs` has no placement handling, so `shux attach`
shows a split with text and no images while all three snapshot verbs now render
them. Unchanged by `c836d8a`.

### P3-3 — two rich-TUI replay arms are only conditionally deterministic (carried forward)

`nvim.raw` and `lazygit.raw` carry terminal queries; `stty -echo` before replay
makes both exactly reproducible, and the committed metrics are from those runs.

### CLOSED — P1-1 of the `3d46d74` audit (unpinned production call site)

Verified independently, not accepted on report. Mutation re-run, caught, correct
failure message, in-process tests unaffected. `window_snapshot_image_rpc.rs` also
guards itself against a vacuous pass by asserting `pane.snapshot` drew the
picture before comparing the composed verbs, and it carries the three mandated
colour probes.

### CLOSED — P2-1 of the `3d46d74` audit (implementation-diff council)

`council-substitution.md` §7.

## 7. Passed evidence

- Old P1 closed with a re-run mutation, not a claim.
- 9 of 25 metrics re-derived at `c836d8a` and numerically identical to the
  committed JSONs; magenta extents reproduce exactly (17100 / 140049 / 373293 /
  540000) at 9×19 cells across three viewports.
- The image clip itself is intact under the attack: in `dose4-window.png` the
  attacker's 328320 blue px are exactly equal in `pane.snapshot` and
  `window.snapshot`, and nothing paints a border, a neighbour or the status bar.
- The stall mitigation is real and independently measured (802 ms → 368 ms).
- `make test-window-snapshot-images` exists, runs both files, 4/4.
- 911 unit tests green on the pristine tree.
- Zero leaked daemons across 18 fresh daemon-backed runs.

## 8. Residual risk

- 16 of the 25 metrics (config states, hot reload, rich-TUI A/B, zoom,
  session-vs-window) were not re-derived. They are covered by the M16b
  derivation — the budget cannot engage at their scene sizes — not by a fresh
  run.
- `make test` (2251) was not re-run at `c836d8a`; the delta's crates were.
- The exact threshold behaviour at `cost == budget` (draw) versus
  `cost == budget + 1` (skip) was probed only at 64 MiB granularity.
- Emit-path blindness is unchanged: everything here was drawn by `shux-raster`.
  `make test-gui-terminal` was not run; the delta emits nothing new to an outer
  terminal.

## 9. Cleanup

Zero leaked daemons and zero leftover runtime dirs at close:
`ps -eo pid,args | grep -c "[s]hux __daemon"` → **0**; no `/tmp/sxq.*`,
`/tmp/sxa.*`, `/tmp/sxi.*` remain. Every one of the 18 runs read its pid from
`$XDG_RUNTIME_DIR/shux/shux.pid` **before** `daemon stop` and polled
`/proc/<pid>` to disappearance, failing loud past 20 s; per-run log in
`.shux/out/multipane-image-clip/daemon-hygiene.txt` and `/tmp/sxatk/out/hygiene.txt`.
No `pgrep -f` / `pkill -f`. Mutations ran in a detached worktree
`/tmp/shux-mut`, restored clean and left at `c836d8a`; no file under
`/home/user/shux/crates/` was modified by this audit.

Disk note: `target/debug/incremental` (regenerable cache) and
`target/aarch64-apple-darwin` (a Linux-unusable cross-compile tree) were deleted
to make room for the mutation worktree. No source, no tracked file and no
non-regenerable artifact was touched. `target/release/shux` was rebuilt from
`c836d8a` after the `3d46d74` A/B build and byte-compared to the stashed copy.

## 10. Audit-integrity note — the tree did not stay frozen

The tree was declared frozen for this audit. It was not.

Two uncommitted edits appeared in the shared checkout `/home/user/shux` while
this report was being written — `crates/shux-raster/src/lib.rs` at **07:09:44**
and `crates/shux/tests/window_snapshot_images.rs` at **07:10:41**. The first renames `MAX_RENDER_DECODE_BYTES` to
`MAX_PANE_DECODE_BYTES` and resets the budget per pane inside
`composite_composed` — an attempted fix for P1-1 above, quoting this audit's own
measured numbers ("17100 px to 0", "9.27 MiB", "200x55 pane"). It is not part of
`c836d8a` and it is not committed. The second adds ~54 lines to the image test
file, presumably the regression test P2-1 asks for. Neither was audited here.

**No evidence in this report is contaminated.** Every measurement predates it:

| artifact | mtime |
|---|---|
| `make test-window-snapshot-images` | 06:51:17 |
| `3d46d74` A/B binary built | 06:58:38 |
| `c836d8a` release binary rebuilt, `cmp`-verified, all attack runs | 06:59:29 |
| `runall.sh` metric re-derivation finished | 07:03:36 |
| 911 unit tests finished | 07:05:56 |
| **`crates/shux-raster/src/lib.rs` modified** | **07:09:44** |
| **`crates/shux/tests/window_snapshot_images.rs` modified** | **07:10:41** |

Every binary used here was built from a clean tree — the attack and A/B binaries
from a detached worktree at `c836d8a` and `3d46d74`, and the one used by
`runall.sh` was `cmp`-verified byte-identical to it immediately before the run.

Two things follow.

1. **The verdict is on `c836d8a` and stands.** The uncommitted edits were not
   audited, were not run by this gate, and cannot be evidence for anything.
   P1-1 and P2-1 are findings against `c836d8a`; whether these edits close them
   is a question for the next audit, on the commit that carries them. This gate
   did not revert them; that is not this role's job.
2. **Writing to the shared checkout is itself the violation.** CLAUDE.md: *"An
   agent that rewrites tracked files runs in its own git worktree. Never point
   one at the shared checkout: a `git add -A` during its run commits its
   scratch."* Had this gate been committing evidence with a broad `git add`, those
   edits would have ridden along inside a QA-evidence commit.

Observation on the uncommitted patch, offered so it is not lost, not as a
finding: the per-pane reset keys on *the clip changing since the previous
placement*, so it bounds correctly only while placements are contiguous by pane.
They are today — `crates/shux-ui/src/composed.rs:82-102` extends all of one
pane's placements before moving to the next, with `clip` constant inside the
loop — but `composite_composed` is `pub` and nothing states or asserts that
precondition, which is the same unstated-precondition risk this gate recorded
against it at `3d46d74`. If a future producer ever interleaves panes, the budget
resets on every alternation and bounds nothing. Re-audit it on its own commit,
with the regression test P2-1 asks for.
