# shux for humans

Quick reference for using shux interactively. For the full design rationale,
see [`PRD.md`](PRD.md).

## Sessions, windows, panes

A **session** is a named workspace that persists across attaches.
A session contains one or more **windows**. A window contains a layout tree of
**panes**, each running its own shell or command.

```bash
shux                          # attach to last session, or create "default"
shux session create work      # create + attach in the caller's cwd
shux session create work -d   # create without attaching
shux session list             # list sessions
shux session kill work        # destroy a session
shux session rename -s work -n staging
shux session attach work      # attach to existing
```

## Keybindings

The prefix key is `Ctrl+Space` (configurable via `[keys].prefix` in
`~/.config/shux/config.toml`).

### Inside the TUI

| Key | Action |
|---|---|
| `Ctrl+Space \|` or `v` | split vertical |
| `Ctrl+Space -` or `s` | split horizontal |
| `Ctrl+Space space` | smart split (wider→vertical, taller→horizontal) |
| `Ctrl+Space h`/`j`/`k`/`l` | focus left / down / up / right |
| `Ctrl+Space o` | cycle focus to next pane |
| `Ctrl+Space z` | toggle zoom |
| `Ctrl+Space x` | kill focused pane |
| `Ctrl+Space c` | new window |
| `Ctrl+Space n`/`p` | next / previous window |
| `Ctrl+Space ←/→/↑/↓` | resize focused pane |
| `Ctrl+Space r` | force redraw |
| `Ctrl+Space d` | detach |
| `Ctrl+Space Ctrl+Space` | send literal prefix to inner shell |
| `Alt+Enter` | smart split (no prefix needed) |
| **mouse click** on a pane | focus that pane |
| **mouse drag** on a border | resize the split |
| **mouse drag** on pane text | select visible text and copy on release |
| **right-click** selected text | open inline `Copy` / `Clear` menu |

### Copying text

For visible text, use the normal mouse path: drag over text in a pane and
release. shux keeps the selected range highlighted and copies it through
OSC 52 so it can work locally and over SSH when the outer terminal permits
OSC 52 clipboard writes. Right-click the visible selection for the inline
`Copy` / `Clear` menu. Typing into the pane clears the selection and sends
your input to the running program.

For scrollback, search, or keyboard-only selection, use copy mode:
`Ctrl+Space [` enters copy mode, `/` and `?` search, `v` starts selection,
`y` yanks, and `q` / `Esc` exits.

## Running commands directly

shux can launch a pane with a specific command instead of a shell:

```bash
shux session create vim -- vim foo.rs        # pane runs vim
shux session create top -- top                # pane runs top
shux session create srv -- python3 -m http.server 8000
```

The pane lifetime equals the command lifetime: when it exits, the pane EOFs.

## Sending input from outside

Useful for scripts and agents that drive an attached session:

```bash
shux pane send-keys -s work -t "git status\n"   # types "git status<Enter>"
shux pane run -s work cargo test                # runs cargo test, waits for completion
shux pane capture -s work --lines 50            # captures last 50 lines as text
```

## Customization

Generate a starter config:

```bash
shux config init      # writes ~/.config/shux/config.toml
shux config path      # prints the effective path
shux config show      # prints the canonical defaults
```

The daemon hot-reloads the file: edits land in attached sessions on the next
render frame, no restart needed (mirrors `tmux source-file`).

For schema details — appearance, keys, shell overrides, status-bar segments —
see [`configuration.md`](configuration.md).

## Status bar

Out of the box: session name, window indicator, clock, all in Catppuccin
colors. To extend with anything that prints to stdout (CPU, IP, git branch,
weather, kubernetes context, …) drop a `[[statusbar.segment]]` into your
config. starship is the canonical example:

```toml
[[statusbar.segment]]
zone = "right"
command = ["starship", "prompt"]
interval_ms = 1000
fallback = " (starship not installed) "
env = { STARSHIP_SHELL = "cmd", TERM = "xterm-256color" }
starship_config = '''
# any starship config — fully embedded, hot-reloadable
add_newline = false
format = "$git_branch$rust$time"
[time]
disabled = false
time_format = "%H:%M:%S"
'''
```

Full schema in [`configuration.md`](configuration.md#status-bar).

## Loading your shell dotfiles

shux spawns the user's shell as `$SHELL -l -i` (login + interactive — same
as iTerm2's default), so `~/.bashrc` / `~/.zshrc` runs and starship / atuin
/ ble.sh / etc. all initialize. shux also overrides `TERM_PROGRAM=shux` and
exports `SHUX=1`, which lets users guard config:

```bash
# Skip the rich starship PS1 inside shux (the status bar carries it).
if command -v starship >/dev/null 2>&1; then
  if [[ -n $SHUX ]]; then
    PS1='\[\e[36m\]❯\[\e[0m\] '
  else
    eval "$(starship init bash)"
  fi
fi
```

`shux config init` prints this snippet for you to copy into your bashrc.

## When something goes wrong

```bash
shux rpc call system.health      # daemon health check
shux session list --format json  # raw machine-readable state
RUST_LOG=debug shux session list # verbose logs
pkill -f 'shux.*__daemon'        # force-kill the daemon
```

Daemon socket lives at `$XDG_RUNTIME_DIR/shux/shux.sock` or
`$TMPDIR/shux-$UID/shux.sock`. PID file: same dir, `shux.pid`.
