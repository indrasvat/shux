# Spike 067: libghostty-vt VT/Renderer Boundary Evaluation

**Status:** Replacement spike complete; follow-up implementation not started
**Branch:** `spike/libghostty-vt-eval`
**Question:** Can `libghostty-vt` become an optional high-fidelity VT-state backend for shux, and how much of the screenshot gap would still remain in shux's cell model and rasterizer?

## Framing

The spike must not conflate three layers:

| Layer | Current shux owner | What `libghostty-vt` can prove |
|---|---|---|
| VT state | `shux-vt` | Yes: parser/grid/reflow/style/cursor behavior can be compared directly. |
| Cell model | `shux-vt::Cell` and `Grid` | Only indirectly: shux currently stores one `char`, so an adapter can still lose grapheme clusters. |
| Pixels | `shux-raster` | No: Ghostty-identical PNGs require font fallback, shaping, emoji/color glyphs, image protocols, and raster metrics beyond a VT-state backend. |

The fair verdict is therefore two-axis:

1. **VT-state fit:** can `libghostty-vt` expose better retrievable terminal state than `shux-vt` for real TUI byte streams?
2. **Pixel-gap remainder:** after better state extraction, how much of screenshot mismatch remains blocked by the shux cell model and rasterizer?

## Decision Criteria

| Criterion | Why it matters | Pass signal |
|---|---|---|
| Build isolation | Normal shux builds must not require unstable Ghostty/Zig plumbing. | `make ci` remains unaffected; spike runs only through explicit `make spike-libghostty-*` targets. |
| Build viability | A backend that cannot build reproducibly is not a shippable optional path. | Native spike builds; crates.io/current-git behavior, Zig version, source fetch, and link mode are recorded. |
| Grapheme retrieval | shux currently stores one `char` per cell, which breaks ZWJ emoji, VS16, flags, skin tones, and combining marks. | Render-state cells expose full grapheme strings, not only storage-side internals. |
| Width semantics | PNG rendering must know when a cell is narrow, wide, or a spacer. | Render-state cells expose narrow/wide/tail metadata for CJK, emoji, and mixed-width rows. |
| Style/color extraction | Snapshot parity needs SGR attributes and RGB colors, not only plain text. | Tests can read bold/italic/underline and truecolor fg/bg from cells. |
| Cursor/default color state | Bubble Tea/Charm TUIs rely on cursor shape/color and OSC color state. | Tests can read cursor shape and cursor color after escape sequences. |
| Resize/reflow behavior | shux panes resize constantly. Wrapped text and wide chars at boundaries are high-risk. | Tests cover resize with wrapped content and document observed reflow. |
| Adapter seam | A perfect backend is useless if shux collapses it back to `Cell { ch: char }`. | Spike defines a normalized snapshot cell that can carry grapheme strings and width/style metadata. |
| Threading model | `libghostty-vt` is `!Send + !Sync`; shux's daemon is async/multi-task. | Spike either prototypes or precisely sketches a single-owner actor/channel boundary. |
| Extraction cost | shux snapshots need a full visible grid quickly. | Benchmark target can measure full-grid extraction allocation/latency. |
| Pixel promise clarity | `libghostty-vt` is not the Ghostty app renderer. | Report distinguishes VT-state parity from Ghostty-identical pixels and avoids a 90-95% claim without pixel diffs. |

## Non-Goals

- Do not replace `shux-vt`.
- Do not replace `shux-raster`.
- Do not add `libghostty-vt` to the main workspace dependency graph.
- Do not claim Ghostty-app-identical pixels unless the pixel-rendering path is actually exercised.
- Do not make Zig, Ghostty source fetches, or libghostty headers part of normal `make ci`.

## Spike Harness

The spike lives in `spikes/libghostty-vt-eval`, a standalone Cargo package.

```bash
make spike-libghostty-build
make spike-libghostty-test
make spike-libghostty-fmt
```

The harness is intentionally outside the workspace so normal CI and release
builds do not inherit pre-1.0 libghostty or Zig requirements.

## Initial Findings

- `libghostty-vt` exposes `Terminal`, `RenderState`, row/cell iterators, cell grapheme APIs, style APIs, color APIs, cursor APIs, and resize APIs.
- The crate is `!Send + !Sync` by design; shux integration would need a single-thread owner task and channel boundary.
- crates.io `libghostty-vt 0.1.1` requires Zig 0.15.2 through its vendored sys build. Local Homebrew Zig 0.16.0 fails that version gate.
- A Zig 0.15.2 tarball downloaded directly from ziglang.org gets past the version gate, but fails on this macOS host while linking Zig's native build runner, with unresolved Darwin/libSystem symbols such as `__availability_version_check`, `_dispatch_queue_create`, `_clock_gettime`, and `_malloc_size`.
- Homebrew `zig@0.15` is also Zig 0.15.2, but its bottled build works on this host. The Makefile now uses `/opt/homebrew/opt/zig@0.15/bin` for the default spike build/test targets.
- Upstream `libghostty-rs` at `20edad15d7984c727acc4f4facdadf045609f543` has a materially newer sys build script: static/dynamic link modes, optional `pkg-config`, `GHOSTTY_ZIG_SYSTEM_DIR`, `-Dapp-runtime=none`, and `-Demit-xcframework=false`. The spike manifest pins this revision for the primary evaluation.
- The pinned upstream wrapper still requires Zig 0.15.2 because its pinned Ghostty source enforces that version. Homebrew Zig 0.16.0 therefore fails before compilation.
- The pinned upstream wrapper with Homebrew `zig@0.15` builds and tests successfully in the isolated spike crate.
- `pkg-config` is not currently an escape hatch on this machine: no `libghostty-vt` or Ghostty package is installed under `/opt/homebrew` and `pkg-config --list-all` does not expose `libghostty-vt`.
- Upstream docs state `libghostty-vt` is usable today but its API signatures are still in flux.
- Existing shux visual goldens are available under `.shux/goldens/` and should seed any later pixel-diff phase instead of inventing a new corpus from scratch.
- API finding: `libghostty-vt` preserves `e + combining acute` as one retrievable grapheme string, but currently exposes the rainbow-flag ZWJ sequence and thumbs-up skin-tone sequence as separate render cells. It improves some grapheme cases but does not make extended emoji clustering a solved problem.
- shux contrast finding: the current `shux-vt::Cell { ch: char }` model loses combining-mark context for `e + combining acute` in the tested path, confirming the adapter/cell-model seam is real.

## Required Test Classes

| Class | Purpose | Current status |
|---|---|---|
| Build tests | Prove native build viability for the pinned wrapper/source/toolchain. | Passing with Homebrew `zig@0.15`. |
| Grapheme retrieval | Prove multi-codepoint symbols are readable as one renderable string. | Partial: combining marks pass; extended emoji clusters split. |
| Current shux contrast | Show today's `Cell.ch: char` model loses grapheme-cluster context. | Passing. |
| Width metadata | Prove wide heads and spacer tails are exposed. | Passing. |
| Style/color extraction | Prove truecolor and SGR attributes survive extraction. | Passing. |
| Cursor/default colors | Prove cursor shape/color state can be read. | Passing. |
| Resize/reflow | Probe wrapped-content behavior after resize. | Passing for basic wrapped text; needs harder wide-boundary cases. |
| Differential grid corpus | Replay deterministic VT fixtures through both `shux-vt` and `libghostty-vt`. | Passing; 12 synthetic cases generated as visual contact sheets. |
| Real TUI corpus | Replay raw PTY recordings from popular and local TUIs through both backends. | Passing; btop, lazygit, nvim, vicaya, and vivecaka recorded and compared. |
| Actor prototype | Validate a `!Send + !Sync` backend behind a thread/channel boundary. | Pending. |
| Full-grid extraction benchmark | Measure visible-grid extraction overhead for snapshots. | Pending. |
| Pixel-diff phase | Compare same bytes through same `shux-raster` with only the VT backend swapped. | Passing for spike scope; not a Ghostty-app pixel oracle. |

## Replacement Test Plan

The replacement question needs a stricter plan than the initial API probe:

| Stage | Test input | What it answers | Done signal |
|---|---|---|---|
| Unit API probe | Small escape sequences | Can the wrapper expose text, style, width, cursor, and resize state? | 7 Rust tests pass. |
| Synthetic VT A/B | Hand-built byte fixtures for colors, alternate screen, scroll regions, sync output, graphemes, wide cells, resize | Does `libghostty-vt` normalize common terminal semantics the same way or better than `shux-vt`? | PNG contact sheets plus cell/pixel/default-color metrics. |
| Real TUI A/B | Raw PTY streams recorded by shux from installed apps | Does the backend survive real TUI output, not just toy fixtures? | btop, lazygit, nvim, vicaya, vivecaka contact sheets. |
| Failure classification | Visual inspection of outliers | Are mismatches acceptable, better behavior, or replacement blockers? | Each high-diff case has a cause and decision. |
| Integration readiness | Actor/threading, responses, perf, Linux/headless CI | Can this actually replace `shux-vt` inside daemon/snapshot flows? | Still pending; this is the main remaining work. |

The comparison harness intentionally uses the same `shux-raster` for both paths.
That isolates the VT backend. It does **not** prove Ghostty-identical pixels, and
the current adapter still collapses libghostty render cells into `shux_vt::Cell`
for raster reuse, so grapheme improvements can be underreported.

## Replacement A/B Results

Artifacts are generated under `.shux/out/libghostty-vt-replacement/`.

| Case group | Cases | Result | Replacement meaning |
|---|---|---|---|
| Basic text/style/color | `plain`, `sgr-truecolor`, `ansi-palette`, `cursor-color-shape`, `cjk-wide` | 0 cell diff, 0 pixel diff. | Good. No regression for common visible state in this harness. |
| Screen mechanics | `alternate-screen`, `scroll-region`, `sync-output` | 0 cell diff, 0 pixel diff. | Good for the covered mechanics. Still needs terminal-response tests. |
| Grapheme edge cases | `combining-mark`, `extended-emoji` | 1 cell diff each, 0 pixel diff after lossy adapter. | Mixed. libghostty preserves combining marks better, but extended emoji are still split; current adapter hides much of this. |
| Resize/reflow | `resize-reflow` | 31.25% cell diff, 11.50% pixel diff. | Positive libghostty signal: it preserves/reflows wrapped content that current `shux-vt` loses. |
| Popular TUIs | `btop`, `lazygit`, `nvim` | nvim exact; btop/lazygit 0 cell diff and 0.02% pixel diff. | Strong compatibility signal for normal rich TUI output. |
| Local TUIs | `vicaya`, `vivecaka` | vicaya exact; vivecaka 0 cell diff but 96.45% pixel diff. | `vivecaka` exposes a blocker: OSC 11 default background differs. |
| OSC default colors | `osc-default-bg`, `vivecaka` | 99.99% and 96.45% pixel diff, default-color state differs. | Replacement blocker until libghostty wrapper state extraction or adapter mapping handles OSC 11 `#RRGGBB`. |

Representative report:

```text
plain                 cell 0.00%   pixel 0.00%
resize-reflow         cell 31.25%  pixel 11.50%
btop                  cell 0.00%   pixel 0.02%
lazygit               cell 0.00%   pixel 0.02%
nvim                  cell 0.00%   pixel 0.00%
vicaya                cell 0.00%   pixel 0.00%
vivecaka              cell 0.00%   pixel 96.45%   default colors differ
osc-default-bg        cell 0.00%   pixel 99.99%   default colors differ
```

The most important visual finding is resize/reflow: current `shux-vt` keeps
`abcdef g` and later `qr stuvw`; libghostty preserves the intervening wrapped
content:

```text
--- shux-vt
abcdef g
qr stuvw
--- libghostty-vt
abcdef g
hijkl mn
opqr stu
vwx yz
```

The most important blocker is default background color. `vivecaka` emits OSC 11
with `#1E1E2E`; current shux applies it, while the libghostty path exposed by
the Rust wrapper did not carry that default background through the current
snapshot/raster adapter.

## Commands Run

| Command | Result | Evidence |
|---|---|---|
| `make spike-libghostty-fmt` | Pass | Spike crate formats. |
| `make help` | Pass | Spike targets are listed under `Spikes`. |
| `brew install zig@0.15` | Pass | Installs Homebrew Zig 0.15.2 as a keg-only formula. |
| `make spike-libghostty-build` | Pass | Uses Homebrew `zig@0.15` and builds the pinned upstream wrapper. |
| `make spike-libghostty-test` | Pass | 7 tests passed. |
| `make spike-libghostty-build-zig015` | Pass | Alias-style explicit Homebrew Zig 0.15 path also builds. |
| `make release` | Pass | Release binary built for recording real TUI byte streams. |
| `make spike-libghostty-record-tuis` | Pass | Raw PTY fixtures recorded for btop, lazygit, nvim, vicaya, and vivecaka. |
| `make spike-libghostty-compare-tuis` | Pass | 17 A/B PNG contact sheets and report generated under `.shux/out/libghostty-vt-replacement/`. |
| Direct tarball Zig 0.15.2 path | Fail | Fails linking the native Zig build runner with unresolved Darwin/libSystem symbols. |
| `make spike-libghostty-build-zig015-macos-target` | Fail | Explicit `-Dtarget=aarch64-macos-none` does not alter the native build-runner link failure. |

## Replacement Verdict

Do **not** replace `shux-vt` wholesale yet.

`libghostty-vt` is a credible future backend. It already matches many common
cases, handles some state more correctly, and appears materially better for
wrapped-content resize/reflow. For screenshots of real TUIs, that is a real
user benefit: fewer lost lines after pane resizing, better width metadata, and
a path toward richer cell state.

But a full replacement is not ready because:

1. shux currently has a `Cell { ch: char }` data model that cannot carry a full grapheme cluster end-to-end.
2. `shux-raster` still owns actual PNG pixels and remains responsible for shaping, font fallback, emoji/color glyph rendering, underline metrics, and image protocols.
3. `libghostty-vt` objects are `!Send + !Sync`, so integration likely requires a dedicated single-owner actor/thread boundary.
4. `libghostty-vt` did not expose tested extended emoji sequences as single render cells, so "grapheme fidelity" is partial, not complete.
5. The current wrapper/adapter path missed OSC 11 `#RRGGBB` default background state, which breaks real TUIs like `vivecaka`.
6. Terminal response behavior, chunked writes, scrollback behavior, dirty tracking, Linux/headless CI, and sustained extraction performance are not proven.
7. The build story still depends on a pinned upstream git revision plus Zig 0.15.2; Homebrew works locally, but this is not yet a clean cross-platform dependency story.

The right product shape is:

1. Keep `shux-vt` as the default backend.
2. Continue `libghostty-vt` behind an explicit experimental feature/backend flag.
3. Add a real `TerminalSnapshot`/`SnapshotCell` model that can carry grapheme strings, width/spacer flags, style, cursor/default color state, and backend metadata without collapsing back to one `char`.
4. Build a single-owner actor around `libghostty-vt` before touching daemon code.
5. Promote it only after it passes OSC default-color parity, terminal responses, Linux/headless CI, performance ceilings, and a broader corpus.

In short: `libghostty-vt` should not replace the custom rasterizer, and it
should not yet replace the current VT backend. It is worth keeping as an
experimental high-fidelity VT backend because it shows concrete wins, especially
for reflow, but the spike found real blockers that would affect end users today.

## Unblock Options

| Option | What it would prove | Cost/risk |
|---|---|---|
| Keep using Homebrew `zig@0.15` for the spike path | Reproducible local build/test loop. | Low; keg-only formula must remain installed. |
| Patch or upstream-report the Zig 0.15.2 macOS native build-runner failure | Whether the vendored build can be made reliable on current macOS. | Medium; depends on Zig/Ghostty/libghostty-rs ownership. |
| Use a prebuilt/installed `libghostty-vt` through upstream's `pkg-config` feature | Separates Rust API evaluation from vendored Ghostty source builds. | Medium; needs a package/install path that exposes headers, libs, and `.pc` files. |
| Wait for or help move the wrapper/Ghostty pin to Zig 0.16-compatible sources | Removes the stale Zig 0.15.2 requirement. | Medium/high; upstream timing. |
| Continue shux-side design now that libghostty builds | Defines `SnapshotCell`, actor boundary, corpus diff tooling, and extraction benchmarks. | Next useful phase. |

## Open Verdict

Build/API viability is plausible with Homebrew `zig@0.15`, but the fidelity verdict is mixed: strong state extraction basics, partial grapheme improvement, and no pixel-perfect claim until adapter/raster/golden-diff phases run.
