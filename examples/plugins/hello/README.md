# shux-hello — reference process plugin

Tiny but useful. ~50 lines of POSIX shell, zero runtime deps,
demonstrates BOTH plugin reaction patterns.

## What it does

1. **Handshake** — replies to `plugin.init` with a manifest that
   subscribes to `session.created` and `window.created`.
2. **PTY output** — on every new session, types a one-line tour
   into the first pane via `pane.send_keys` (basic keybinds plus
   how to list active plugins).
3. **State mutation** — on every new window, calls `window.rename`
   to tag it `demo·N` (incrementing). Now visible in the status
   bar, `shux window list`, and the attach-mode tab strip.

Two events, two reaction patterns, ~50 lines. The same shape
scales to anything you'd actually build — context-aware window
titles, command-completion notifiers, project-router plugins.

## Try it

```sh
shux plugin install ./examples/plugins/hello/plugin.sh
shux plugin list                     # → hello v0.2.0 running, watching=true

shux new -s demo -d                  # → triggers session.created
shux pane capture -s demo            # → output contains the tour
shux window list -s demo             # → first window tagged "demo·1"

shux window create -s demo           # → new window tagged "demo·2"
```

## Hot reload — watch the plugin react to its own edits

The daemon watches the source file by default. Edit, save, the
new code is live in <500ms (~250ms FSEvents/inotify debounce +
respawn).

```sh
# Portable across BSD/macOS sed and GNU sed — `-i` semantics differ
# between the two, but `-i.tmp` works on both, so we just delete the
# backup file afterwards.
sed -i.tmp 's/demo/hot/g' examples/plugins/hello/plugin.sh && \
  rm examples/plugins/hello/plugin.sh.tmp
# → daemon log: "watcher: file changed, reloading"

shux window create -s demo           # → new window tagged "hot·3"
```

The reproducer for the gallery's hot-reload tile lives at
`.claude/automations/plugin_hot_reload_shoot.sh`.

## Protocol notes

- One JSON object per line on stdin / stdout. No nested newlines.
- The first stdin line is `plugin.init`. Reply with the manifest
  on stdout within 5 s.
- After handshake, the daemon writes:
  - Event notifications: `{ "method": "event", "params": <event> }`
  - Shutdown: `{ "method": "plugin.shutdown" }` (2 s grace,
    SIGKILL fallback)
  - Responses to plugin-issued RPC requests (matched by `id`).
- Plugin → daemon RPC requests go to stdout with a unique `id`;
  the daemon dispatches through its router and writes a response
  back on stdin.

Full spec: `skills/shux/references/plugins.md`.
