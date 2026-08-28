# Visual Testing (L4)

Visual tests use iterm2-driver to automate iTerm2 for screenshot-based regression testing.

```bash
uv run .claude/automations/<test>.py   # Run a visual test script
```

Screenshots are saved to `.claude/screenshots/` (gitignored).
Visual test scripts live in `.claude/automations/` and are added per-task as needed.

## The GUI-terminal rig — what a foreign terminal draws from shux's output

```bash
make test-gui-terminal            # self-test, then the rig
make test-gui-terminal-selftest   # only the proof that the rig can fail
```

Everything else on this page — pixel goldens, `pane snapshot`, `lens gate`, shux
attached to shux — renders through `shux-raster`, and `shux-raster` **clips while
compositing**: it will not draw a pixel outside the frame. So anything shux
*emits* to an outer terminal is never drawn by one, and a whole class of defect is
structurally invisible. On the image spike, shux's attach path emitted an image
that spilled 160–252 px past the pane border and over the status bar; every
shux-based check was green, because the clip that hid it lives in the code doing
the checking. kitty does not clip, and saw it in the first frame.

The rig runs **kitty 0.32.2** — the kitty graphics protocol's reference
implementation — under **Xvfb** with Mesa software GL, points it at a real shux
session, photographs the X root window with ImageMagick's `import`, and drives the
attach resize path with `xdotool windowsize`.

Not in `make check` or `make ci`: it needs an X server and a GUI terminal. Image
work and emit-path changes invoke it deliberately. `make test-gui-terminal` is
96 s measured — three emulator bring-ups for the self-test, one for the rig.

### What it asserts, per frame

Geometry is measured off the photograph rather than derived from font metrics.
The rig pins an isolated config whose border is `#ff0000` and whose status bar is
`#0000ff` — landmarks used nowhere else in the picture, because shux's default
theme paints the status accent in the same sapphire as the border.

| Assertion | What a failure means |
|---|---|
| `chrome` | the pane border is not a rectangle, or the status bar is not below it |
| `clipped` | the window ran off the screen, so the capture is missing the region an overflow lands in |
| `grid` | the size shux told the pane does not match the grid the emulator is drawing, to under one cell of slack in the window |
| `containment:foreign` | something that is not shux's chrome — **any colour** — was painted outside the pane |
| `containment:image` | a known payload was painted outside the pane, under its own name so the self-test can require *that* failure |
| `content:image` / `content:block` | a promised payload is absent, or the workload's block is half-drawn |
| `probe` | the truecolor, indexed or basic colour probe did not render — a colour class is not reaching the emulator |
| `crosspath` | the payload block sits at different cells in kitty than in shux's own `pane capture` |

`containment:foreign` is the one that generalises: a real emitted image is not one
flat colour, so counting a hand-picked payload colour only ever catches the
payload the rig paints for itself. Measured on real captures, a correct frame has
**exactly zero** foreign pixels outside the pane — at every tolerance from 8 to 40
— and an overflowing frame has 4686. It is an exact assertion, not a budget.

### Proving it can fail

`scripts/check-gui-terminal-selftest.sh` drives the real rig, never a copy of its
logic, against the three cases issue #175 requires — a reintroduced overflow,
empty input, a missing tool — plus one the issue does not name: the default
scenario must pass at all four window sizes, since nothing else exercises the
resize path.

The reintroduced defect is an A/B on one thing: the same payload emitted with and
without a destination box in **cells** (`c=`/`r=`), which was the fix. Without it
kitty draws at natural pixel size and it spills; the rig must go red naming
`containment:image` specifically, so a crashed kitty cannot satisfy the case.

### What it is not

The sidecar that emits the payload **stands in for the sender**. It proves the
rig's optics, not shux's emit path — shux has no image code, and its VT parser has
no APC handling, so kitty graphics written into a pane are swallowed and never
reach the outer terminal (measured: the marker arrives in `pane capture`, zero
pixels arrive in kitty). When shux gains an emit path, the payload's source moves
into shux and no assertion changes.

The rig is **single-pane**, and asserts that rather than assuming it. The
rectangle finder takes the outermost rules of the border mask, so with two panes
it would measure the union of their outlines and an image spilling from one pane
into its neighbour would score zero pixels outside. Per-pane rects are the
extension to make when that case matters.

### Tools

`kitty`, `Xvfb`, `xdotool`, ImageMagick's `import`, `python3`, `uv`. The rig
**runs** each one during preflight rather than looking it up on `PATH` — a
non-executable file of the right name is skipped by `command -v`, and a tool that
is installed and broken looks present. A missing or broken tool exits **3**, which
is distinct from an assertion failure (**1**) and a usage error (**2**), so the
self-test can tell them apart. On Debian/Ubuntu: `kitty xdotool xvfb imagemagick`.

Artifacts land in `.shux/out/gui-terminal/<scenario>/` (gitignored): the captured
frames, shux's own `pane snapshot` of the same pane at the same moment, and
`run.json`, which is the geometry every assertion was made against. Re-running the
comparator over an existing `run.json` is the fast way to iterate on an assertion:

```bash
.shux/scripts/lib/kitty_frame_verdict.py --geometry .shux/out/gui-terminal/plain/run.json --verbose
```
