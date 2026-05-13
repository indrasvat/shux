#!/usr/bin/env bash
# shux-hello — reference process plugin (~50 lines, zero deps).
#
# Demonstrates BOTH plugin reaction patterns:
#   1. PTY output     — on session.created, type a tiny tour into the
#                       new session's first pane via pane.send_keys.
#   2. State mutation — on every new window, rename it to `demo·N`
#                       so the plugin's footprint shows up in the
#                       window list / status bar.
#
# Protocol: docs/tasks/044a-process-plugins-v0.md
# Hot-reload safe: edit + save, daemon respawns within 500ms.

set -u

# Phase 1 — handshake. Daemon writes plugin.init within 5s; we MUST
# reply with the manifest within the same budget. Long init goes
# AFTER the manifest, never before.
IFS= read -r _ || exit 1
printf '%s\n' '{"jsonrpc":"2.0","id":"init","result":{"name":"hello","version":"0.2.0","subscribes":["session.created","window.created"],"provides":[],"capabilities":[]}}'

# Phase 2 — main loop. One JSON frame per stdin line. Plugin→daemon
# RPC requests are written to stdout with a unique id.
rpc_id=1000
window_seq=0

while IFS= read -r line; do
  case "$line" in
    *'"plugin.shutdown"'*)
      exit 0
      ;;
    *'"session.created"'*)
      # session.created carries `session_id` in its payload (UUIDs,
      # not human names — that's what the RPC layer expects).
      sid=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p' | head -1)
      [ -n "$sid" ] || continue
      rpc_id=$((rpc_id + 1))
      sleep 0.5  # let the initial pane's PTY finish attaching
      printf '{"jsonrpc":"2.0","method":"pane.send_keys","params":{"session_id":"%s","text":"echo \\"\\xf0\\x9f\\x91\\x8b shux: Ctrl+Space c (window) | %% (vsplit) | shux plugin list (here)\\"\n"},"id":%d}\n' "$sid" "$rpc_id"
      ;;
    *'"window.created"'*)
      # window.created → rename via window.rename (graph mutation).
      wid=$(printf '%s' "$line" | sed -n 's/.*"window_id":"\([^"]*\)".*/\1/p' | head -1)
      [ -n "$wid" ] || continue
      window_seq=$((window_seq + 1))
      rpc_id=$((rpc_id + 1))
      printf '{"jsonrpc":"2.0","method":"window.rename","params":{"id":"%s","name":"demo·%d"},"id":%d}\n' \
        "$wid" "$window_seq" "$rpc_id"
      ;;
  esac
done
