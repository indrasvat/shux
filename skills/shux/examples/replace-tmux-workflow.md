# Example: common tmux patterns → shux equivalents

Quick translation table for moving a tmux-based workflow over.

## `tmux new-session -d -s name 'cmd'`

```bash
shux api session.create '{"name":"name","command":["cmd"]}'
```

## `tmux send-keys -t name 'hello' Enter`

```bash
# Text first, then Enter as a base64-encoded control byte.
shux api pane.send_keys '{"pane_id":"$PID","text":"hello"}'
shux api pane.send_keys '{"pane_id":"$PID","data":"DQ=="}'        # \r
```

Or one shot:

```bash
shux api pane.send_keys '{"pane_id":"$PID","text":"hello\n"}'      # JSON-encoded \n
```

## `tmux capture-pane -t name -p`

```bash
shux api pane.capture '{"pane_id":"$PID","lines":50}' | jq -r .result.text
```

For a PNG of the same pane instead:

```bash
shux api pane.snapshot '{"pane_id":"$PID"}' \
  | jq -r .result.png_base64 | base64 -d > pane.png
```

## `tmux kill-session -t name`

```bash
shux kill -s name
```

## `tmux split-window -t name -h 'cmd'`

```bash
shux api pane.split '{"pane_id":"$PID","direction":"vertical","ratio":0.5,"command":["cmd"]}'
```

Direction in shux uses *axis* names (`vertical` = left/right split,
`horizontal` = top/bottom split). This matches the split-line orientation,
not the resulting pane positions.

## `tmux resize-pane -t name -x 200 -y 60`

```bash
shux api pane.set_size '{"pane_id":"$PID","cols":200,"rows":60}'
```

Synchronous — the next `pane.snapshot` is guaranteed to see the new
dims (no race).

## `tmux list-sessions`

```bash
shux api session.list '{}' | jq '.result.sessions'
```

## `~/.tmux.conf` declarative workspace → shux apply template

A tmux session you'd otherwise script with `new-session ; send-keys ;
split-window ; ...` becomes a TOML the daemon commits atomically:

```toml
# my-workspace.toml
[session]
name = "review"
cwd  = "~/code/myproject"

[[windows]]
title = "editor"
[[windows.panes]]
command = ["nvim"]
[[windows.panes]]
direction = "vertical"
ratio = 0.3
command = ["bash"]

[[windows]]
title = "claude"
[[windows.panes]]
command = ["claude", "--repo", "indrasvat/shux"]
```

```bash
shux apply my-workspace.toml
shux attach review                  # human attach (interactive multiplexer)
```

## tmux `prefix + arrow` to navigate panes

shux's attached client uses Alt+h/j/k/l for directional pane focus by
default. From outside the attached client, drive it programmatically:

```bash
shux api pane.focus_direction '{"session_id":"name","direction":"right"}'
```

## tmux `prefix + z` to zoom

```bash
shux api pane.zoom '{"pane_id":"$PID"}'        # toggles
```

## tmux mouse mode

shux has mouse support in the attached client. From outside, programmatic
mouse events aren't part of the public RPC surface yet — drive via
keystrokes instead, which is more deterministic for tests anyway.

## tmux `if-shell` / hooks

shux doesn't have a tmux-style hook system. Instead, subscribe to
`events.history` (sequenced events) or `pane.output.watch` (sampled PTY
chunks) and react in your driver script. Cleaner contract, no embedded
shell evaluation.

## tmux plugins

shux has its own plugin system (Wasm-based, `shux-plugin` crate). Not a
tmux-plugin equivalent — closer in spirit to VS Code extensions.
Different design space, document separately.
