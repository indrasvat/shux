# shux-hello — reference process plugin

Task 044a, phase 0 reference. A ~50-line POSIX shell plugin that
proves the protocol end-to-end with zero runtime dependencies.

## What it does

1. Performs the JSON-RPC handshake.
2. Subscribes to `session.created`.
3. For every new session, sends a `pane.send_keys` RPC that echoes
   `👋 from shux-hello` into the new session's active pane.

## Try it

```sh
shux plugin install ./examples/plugins/hello/plugin.sh
shux plugin list                    # → hello v0.1.0 running
shux new -s demo -d                  # → triggers session.created
shux pane capture -s demo            # → output contains the greeting
shux plugin kill hello
```

## Protocol notes

- One JSON object per line on stdin / stdout. No nested newlines.
- The first stdin line is `plugin.init` from the daemon. The plugin
  responds with its manifest on stdout.
- After handshake, the daemon writes:
  - Event notifications: `{ "method": "event", "params": <event> }`
  - Shutdown notifications: `{ "method": "plugin.shutdown" }`
  - Responses to plugin-issued RPC requests (matched by `id`).
- Plugin → daemon RPC requests are written to stdout with a unique
  `id`; the daemon dispatches through its router and writes a
  response back with the matching `id`.

See `docs/tasks/044a-process-plugins-v0.md` for the full spec.
