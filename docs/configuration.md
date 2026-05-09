# Configuration

Single TOML file at `$XDG_CONFIG_HOME/shux/config.toml` or
`$HOME/.config/shux/config.toml`. Hot-reloaded — edits land in attached
sessions on the next render frame.

```bash
shux config init      # scaffold a starter
shux config path      # print the effective path
shux config show      # print the canonical defaults
```

If the file is missing, shux uses defaults. If it parses with errors,
shux falls back to defaults and logs a warning. Either way, the daemon
never crashes on bad config.

## Schema

```toml
[appearance]
border_style = "rounded"   # thin | thick | double | rounded | ascii | none

[keys]
prefix = "ctrl-space"      # any "<mod>-<key>" combo crossterm understands

[shell]
# By default: $SHELL -l -i. Override with explicit argv (rare).
command = []
# Extra env vars injected into every spawned pane.
env = {}

# Status-bar customization — see below
```

## Status bar

shux's status bar is data-driven: zero or more `[[statusbar.segment]]`
entries, each running a command on an interval. Captured stdout is parsed
through shux's VT (full ANSI color preservation) and rendered into the
declared zone (`left` / `center` / `right`).

### Minimal example

```toml
[[statusbar.segment]]
zone = "right"
command = ["bash", "-c", "date +%H:%M:%S"]
interval_ms = 1000
```

### Full schema

| Field | Type | Default | Notes |
|---|---|---|---|
| `zone` | string | `"left"` | `"left"`, `"center"`, `"right"` |
| `command` | array of strings | (required) | argv to spawn |
| `env` | table of strings | `{}` | extra env vars for the spawn |
| `interval_ms` | integer | `2000` | refresh interval (clamped to ≥100ms) |
| `fallback` | string | `""` | text shown when command is missing or fails |
| `starship_config` | string | none | inline starship TOML — see below |

### Starship integration

Starship doesn't expose a Rust library API. Instead, shux runs the
`starship` binary periodically and parses its ANSI stdout. To configure
starship JUST for the bar (without touching your PS1), embed a starship
config inline:

```toml
[[statusbar.segment]]
zone = "right"
command = ["starship", "prompt"]
interval_ms = 1000
fallback = " (starship not installed) "
starship_config = '''
add_newline = false
command_timeout = 2000
format = "${custom.load}${custom.ip}$time"

[time]
disabled = false
format = "[  $time ](bold #f5a97f)"
time_format = "%H:%M:%S"

[custom.load]
when = true
command = "sysctl -n vm.loadavg | awk '{printf \"%.2f\", $2}'"
format = "[ load $output ](bold #ed8796) "

[custom.ip]
when = true
command = "ipconfig getifaddr en0 || echo offline"
format = "[ ip $output ](bold #8aadf4) "
'''
```

The runner materializes `starship_config` to `$TMPDIR/shux-segment-<idx>.toml`
and exports `STARSHIP_CONFIG=<that file>` for the spawn. Your shell's PS1
config (`~/.config/starship.toml`) is unaffected.

### TOML quoting gotcha

The outer `starship_config` field MUST be a TOML **literal multi-line**
string (`'''...'''`), NOT a basic multi-line string (`"""..."""`).
Triple-double decodes `\"` → `"` mid-parse and corrupts the inner TOML.
Triple-single passes everything through verbatim — copy-paste any starship
example unmodified.

### Starship custom modules — non-obvious bits

- `when = true` (boolean), not `when = "true"` (string).
- Format reference is `${custom.<name>}`, not `$custom_<name>`.
- Default `command_timeout` is 500ms — set higher for slower commands.

## Avoiding the "two starships" duplication

If you also use starship for your shell PS1, you don't want both rendering
inside shux. Guard your shell init on the `SHUX` env var that shux injects:

```bash
# ~/.bashrc
if command -v starship >/dev/null 2>&1; then
  if [[ -n $SHUX ]]; then
    PS1='\[\e[36m\]❯\[\e[0m\] '   # bare cyan chevron inside shux
  else
    eval "$(starship init bash)"  # full PS1 outside
  fi
fi
```

The result: full starship outside shux, bare chevron inside, the rich info
only in shux's status bar.

## Hot reload semantics

The daemon watches the parent directory of the config file (because editors
typically write to a temp file and atomic-rename — file-watch alone misses
that). On change:

1. Re-parse the TOML; on parse error, keep the previous valid config and
   log a warning.
2. Atomically swap the live snapshot.
3. Notify all consumers — render loop redraws with the new appearance,
   status-bar runner tears down the old segments and respawns with the
   new list.

Edits land in <250ms. No restart, no re-attach.

## Debugging config issues

```bash
shux config show > expected.toml             # see what the parser expects
diff <(shux config show) ~/.config/shux/config.toml
```

`shux config validate` is on the M1 follow-up list — it'll cross-check the
outer config plus each inline starship config and report line numbers on
error.
