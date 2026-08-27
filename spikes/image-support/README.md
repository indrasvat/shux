# Image support — throwaway spikes

Everything built while proving that kitty graphics support is feasible in shux, kept
in one place so the implementation does not have to rediscover it.

**Do not merge this branch.** It is an archive. The code here is spike quality: it
compiles, it is not formatted, not linted, has no QA evidence, and carries deliberate
dead ends. The work it justifies is filed as issues and lands as fresh commits.

| | |
|---|---|
| Design plan | https://claude.ai/code/artifact/3e89a37e-fb48-40c4-9bc6-ca3c8493566e |
| Issue — pane input contract | https://github.com/indrasvat/shux/issues/174 |
| Issue — GUI-terminal rig | https://github.com/indrasvat/shux/issues/175 |
| Base | `claude/terminal-browser-shux-g9ig0n` @ `4835fd1` |

Video proof of every claim is embedded in the design plan. Nothing binary is committed
here — no MP4s, no captured frames — per the repo's evidence rules.

## What is in the diff

`git diff 4835fd1..HEAD -- crates/` is the whole spike. It maps to the plan's six work
items:

| Work item | Files | Added |
|---|---|---|
| #174 A — winsize pixel geometry | `shux-pty/src/handle.rs` | +12 |
| #174 B — mouse forwarding + Shift | `shux/src/attach.rs`, `shux-rpc/src/attach.rs`, `shux-ui/src/attach.rs` | +167 |
| 3 — parse + store | `shux-vt/src/graphics/spike_kitty.rs` (new), `lib.rs`, `grid.rs`, `parser.rs`, `screen.rs` | +419 |
| 4 — raster compositing | `shux-raster/src/lib.rs` | +35 |
| 5 — multi-pane composers | `shux-ui/src/composed.rs` | +14 |
| 6 — attach re-transmit | `shux-ui/src/compositor.rs` | +267 |

Tests worth keeping, both written cold-context and both proven able to fail:

- `crates/shux-ui/tests/spike_image_repro.rs` — a same-geometry repaint must reach the
  outer terminal; a full-size image must survive chunked emit byte-exact (889 chunks).
- `crates/shux-ui/tests/spike_fix_attack.rs` — eight attacks on the emit path.

Examples used as instruments, not deliverables: `shux-raster/examples/replay_frames.rs`
and `reemit.rs` replay recorded PTY bytes through two builds and compare grids, which is
the only honest way to A/B an animated TUI; `shux-vt/examples/m1016.rs` is the DECSET
1016 probe that established the mode breaks clicking.

## Known defects in this code

Do not port these forward.

- **The cell size is hardcoded twice** — `shux-pty/src/handle.rs:14` and
  `shux-ui/src/compositor.rs:121,132`, both `9×19`. #174 replaces both with one declared
  value pinned by test to `Rasterizer::new(14.0).cell_size()`.
- **`shux-raster` blits images 1:1**, ignoring the destination cell box, so a rasterizer
  at a different font size draws them at the wrong scale. Invisible in every recorded
  proof, which all ran at 14.0. Work item 4 fixes it.
- **Dead DECSET 1016 remnants** in `shux/src/attach.rs` — `SPIKE_CELL`,
  `encode_mouse_wheel_px`'s unused `pixel_cell`, `sgr_px`. `cargo check` warns about
  them. 1016 is rejected (plan D12); strip them.
- **No `§5` bounds.** Transmit is unbounded here. Every hostile-input defence in the
  plan is unwritten, and work item 3 must land it with the parser rather than after.

## harness/

The scripts that produced the proofs. They are **not runnable as-is**: they hardcode
`/tmp` paths and binary names like `/tmp/shux-shift`, and `compare/paircmp.py` reads
`compare/verdict.py` from an absolute path. There is deliberately no `make` target — a
target that cannot work is worse than none. Issue #175 is where a real rig gets built,
and these are its starting point, not its answer.

They also use `set -uo pipefail` without `-e`, which the repo's guard rules forbid. Fix
that when adapting rather than copying it.

- `kitty/` — the rig that found the pane-overflow bug: kitty 0.32.2 under Xvfb with Mesa
  software GL, `import -window root` to capture, `xdotool windowsize` to drive resize.
  `kproto*.sh` drive kitty with hand-built protocol payloads and are how the `a=t`/`a=p`
  transmit/place split was shown not to render at 428 chunks.
- `capture/` — paired capture of a pane's own render and an attached client's view at the
  same instant, which is what turns "it looks right" into a frame-by-frame count.
- `input/` — click, Shift+drag and mouse-report probes against a real terminal-browser.
  `mouse_echo.py` is a pane-side listener that prints what actually arrived.
- `compare/` — PNG decode and comparators. `verdict.py` detects the demo page's counter
  green rather than counting lit pixels, because a lit-pixel threshold counted shux's own
  border chrome as "image present" and produced a false pass.
- `demopage/` — the page used in the videos: a counter painted inside the image plus the
  canvas's own size printed on it, so a stale frame is visible rather than inferred.
