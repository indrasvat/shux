#!/usr/bin/env bash
# shux-hello — reference process plugin (task 044a, phase 0).
#
# A ~30-line POSIX shell plugin that:
#   1. Performs the JSON-RPC handshake (reads plugin.init from stdin,
#      writes its manifest to stdout).
#   2. Subscribes to session.created events.
#   3. For every new session, sends an `echo` line via pane.send_keys
#      so you can see the plugin's footprint in the session's pane.
#
# Zero runtime dependencies beyond bash + sed. The protocol is
# documented in docs/tasks/044a-process-plugins-v0.md.

set -u

# Handshake: wait for plugin.init from the daemon, then emit our
# manifest. The id MUST echo "init" back — v0 only sends one init.
IFS= read -r _ || exit 1
printf '%s\n' '{"jsonrpc":"2.0","id":"init","result":{"name":"hello","version":"0.1.0","subscribes":["session.created"],"provides":[],"capabilities":[]}}'

# Main loop: one JSON frame per line on stdin. Plugin→daemon RPC
# requests are written to stdout with a unique id; the daemon writes
# responses back as additional lines on stdin.
rpc_id=1000
while IFS= read -r line; do
  case "$line" in
    *'"plugin.shutdown"'*)
      exit 0
      ;;
    *'"session.created"'*)
      # pane.send_keys requires a UUID identifier (session_id /
      # window_id / pane_id), not a human name. The event payload
      # carries `session_id` — use that directly.
      sid=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p' | head -1)
      [ -n "$sid" ] || continue
      rpc_id=$((rpc_id + 1))
      # Give the daemon a beat to attach the initial pane's PTY
      # before we type into it. A real plugin would subscribe to a
      # pane.spawned event instead.
      sleep 0.5
      printf '{"jsonrpc":"2.0","method":"pane.send_keys","params":{"session_id":"%s","text":"echo from shux-hello\n"},"id":%d}\n' "$sid" "$rpc_id"
      ;;
  esac
done
