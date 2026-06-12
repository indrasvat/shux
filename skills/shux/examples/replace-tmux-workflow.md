# Example: common tmux patterns → shux equivalents

Quick translation table for moving a tmux-based workflow over.
Every shux verb mirrors an RPC method — RPC dots become CLI spaces.

## `tmux new-session -d -s name 'cmd'`

```bash
shux session create name -d -- cmd
```

## `tmux send-keys -t name 'hello' Enter`

```bash
# Text first, then Enter as a base64-encoded control byte.
shux pane send-keys -s name --text 'hello'
shux pane send-keys -s name --data 'DQ=='     # \r
```

Or one shot:

```bash
# JSON-encoded \n in --text
shux pane send-keys -s name --text $'hello\n'
```

## `tmux capture-pane -t name -p`

```bash
shux --format json pane capture -s name --lines 50 | jq -r .text
```

For a PNG of the same pane instead:

```bash
shux --format json pane snapshot -s name | jq -r .png_base64 | base64 -d > pane.png
```

## `tmux kill-session -t name`

```bash
shux session kill name
```

## `tmux split-window -t name -h 'cmd'`

```bash
shux pane split -s name --direction vertical -- cmd
```

Direction in shux uses *axis* names (`vertical` = left/right split,
`horizontal` = top/bottom split). This matches the split-line
orientation, not the resulting pane positions.

## `tmux resize-pane -t name -x 200 -y 60`

```bash
shux pane set-size -s name --cols 200 --rows 60
```

Synchronous — the next `pane snapshot` is guaranteed to see the new
dims (no race).

## `tmux list-sessions`

```bash
shux --format json session list | jq '.sessions'
```

## `~/.tmux.conf` declarative workspace → `shux state apply` template

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
shux state apply my-workspace.toml
shux session attach review          # human attach (interactive multiplexer)
```

## tmux `prefix + arrow` to navigate panes

shux's attached client uses Alt+h/j/k/l for directional pane focus
by default. From outside the attached client, drive programmatically:

```bash
shux pane focus -s name --direction right
```

## tmux `prefix + z` to zoom

```bash
shux pane zoom -s name        # toggles
```

## tmux mouse mode

shux has mouse support in the attached client. From outside,
programmatic mouse events aren't part of the public RPC surface yet
— drive via keystrokes instead, which is more deterministic for
tests anyway.

## tmux `if-shell` / hooks

shux doesn't have a tmux-style hook system. Instead, write a
process plugin (see [`references/plugins.md`](../references/plugins.md))
or subscribe to control-plane `events.watch` / `events.history` and react in
your driver script. Use `pane.output.watch` only for sampled live pane bytes;
use `pane record` for byte-exact transcripts. Cleaner contract, no embedded
shell evaluation.

## tmux plugins

shux has its own plugin system — process plugins that speak
line-delimited JSON-RPC on stdin/stdout. Install with
`shux plugin install <path>`. Hot reload on save is the default;
see [`references/plugins.md`](../references/plugins.md).

## Raw RPC fallthrough

When a CLI verb doesn't exist for a method you want to call, or
you'd rather write the params as JSON, use `shux rpc call`:

```bash
shux rpc call session.create --params "{\"name\":\"demo\",\"cwd\":\"$PWD\"}"
shux rpc call pane.send_keys --params @keys.json
echo '{"pane_id":"...","text":"j"}' | shux rpc call pane.send_keys --params -
```
