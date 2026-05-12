# Templates — declarative workspace specs

`shux state apply foo.toml` ships an atomic batch of typed Ops to `state.apply`.
The template file describes what the workspace should look like; the
lowerer (`crates/shux/src/template.rs`) turns it into back-ref'd ops.

## Minimal — single pane

```toml
[session]
name = "demo"

[[windows]]
title = "main"

[[windows.panes]]
command = ["htop"]
```

Lowers to **one** op (`CreateSession` with `initial_command=["htop"]`,
`initial_window_title="main"`). No phantom window.

## Multi-pane window — splits

```toml
[session]
name = "editor"

[[windows]]
title = "code"
cwd = "~/code/myproject"

# First pane = the window's initial pane.
[[windows.panes]]
command = ["nvim"]

# Second pane = vertical split off the first, 40% width.
[[windows.panes]]
direction = "vertical"
ratio = 0.4
command = ["bash"]
```

## Multi-window workspace — agents in parallel

```toml
[session]
name = "swarm"
cwd = "~/code/x"

[[windows]]
title = "editor"
[[windows.panes]]
command = ["nvim"]

[[windows]]
title = "claude"
[[windows.panes]]
command = ["claude", "--repo", "indrasvat/shux"]

[[windows]]
title = "codex"
[[windows.panes]]
command = ["codex", "--repo", "indrasvat/shux"]
```

`windows[0]` folds into `CreateSession`. `windows[1..]` lower to
`CreateWindow` ops with proper back-refs.

## Schema

```toml
[session]
name = "..."          # optional — daemon auto-generates if absent
cwd  = "~/path"       # optional — defaults to daemon cwd; ~ expanded

[[windows]]
title = "required"
cwd   = "~/optional"  # per-window cwd; inherits session if absent

[[windows.panes]]
command   = ["argv", ...]  # empty → default shell at PTY spawn
cwd       = "~/optional"   # per-pane cwd; inherits window/session
direction = "vertical"     # split direction relative to prior pane (ignored on first pane of a window)
                           # "vertical" → splits left/right; "horizontal" → splits top/bottom
ratio     = 0.5            # split ratio in (0.0, 1.0); ignored on first pane
```

## Atomicity contract

The lowered ops execute in a single `state.apply` batch. Graph mutations
land all-or-nothing. PTY spawn outcomes are reported per-op in the
response's `spawn_results` and do not roll back the graph — e.g. if pane 3
fails to spawn its command, panes 0–2 still exist.

## Validation

```bash
shux state apply spec.toml --dry-run            # prints the lowered ops, no commit
```

The dry-run output is the exact `{ops: [...]}` payload sent to
`state.apply`. Inspect when you suspect lowering surprises.

## Things templates can't currently do

- Env vars per pane (deliberate — explicit env modes only; PR-tracked).
- Per-pane size in cells (templates set layout *direction* + *ratio*; absolute sizes are post-create via `pane.set_size`).
- Conditional / templated values (no interpolation — the file is read as-is).

Set these post-apply via `pane.set_size`, `pane.set_title`, etc.
