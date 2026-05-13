#!/usr/bin/env bash
# shux-watcher — pane-exit notifier (~60 lines, jq + bash).
#
# Demonstrates TWO v0.16+/v0.18+ plugin primitives:
#
#   event.publish      → re-emit pane.exited as
#                        plugin.watcher.command_exit so other plugins
#                        can subscribe cleanly.
#   plugin.state.*     → persist a cumulative emit counter across
#                        hot reload + daemon restart.
#
# Try it:
#   shux plugin install ./examples/plugins/watcher/plugin.sh
#   shux events watch --filter plugin.watcher.    # one terminal
#   shux session create demo -d -- bash -lc 'echo hi && exit 7'
#     # → watcher emits plugin.watcher.command_exit with seq=1
#   # Edit this file (save) — hot reload triggers. The next emit
#   # carries seq=2 because the counter survived in state.json.
#
# Optional filter: only re-emit when exit_status matches a regex.
# Set EXIT_RE='[1-9]' to fan out only on non-zero exits.
#
# Zero deps beyond bash + jq.

set -u

EXIT_RE="${EXIT_RE:-}"

IFS= read -r _ || exit 1
printf '%s\n' '{"jsonrpc":"2.0","id":"init","result":{"name":"watcher","version":"0.2.0","subscribes":["pane.exited"],"provides":[],"capabilities":[]}}'

# Restore persisted counter (survives hot reload). The plugin host
# replies on stdin; one read pulls the response back.
printf '{"jsonrpc":"2.0","method":"plugin.state.get","params":{},"id":1}\n'
IFS= read -r state_line
emit_count=$(printf '%s' "$state_line" | jq -r '.result.value.emit_count // 0' 2>/dev/null)
emit_count=${emit_count:-0}

rpc_id=1000
while IFS= read -r line; do
  case "$line" in
    *'"plugin.shutdown"'*)
      exit 0
      ;;
    *'"pane.exited"'*)
      payload=$(printf '%s' "$line" | jq -c '{
        session_id:  .params.data.data.session_id,
        pane_id:     .params.data.data.pane_id,
        exit_status: .params.data.data.exit_status,
        command:     .params.data.data.command
      }' 2>/dev/null) || continue
      [ -n "$payload" ] || continue

      if [ -n "$EXIT_RE" ]; then
        es=$(printf '%s' "$payload" | jq -r '.exit_status // "null"')
        if ! printf '%s' "$es" | grep -qE "$EXIT_RE"; then continue; fi
      fi

      emit_count=$((emit_count + 1))
      enriched=$(printf '%s' "$payload" | jq -c --argjson n "$emit_count" '. + {emit_count: $n}')

      rpc_id=$((rpc_id + 1))
      printf '{"jsonrpc":"2.0","method":"event.publish","params":{"event_type":"command_exit","data":%s},"id":%d}\n' \
        "$enriched" "$rpc_id"

      # Persist the new counter so a hot reload picks up where we left
      # off. Best-effort — we don't wait for the response.
      rpc_id=$((rpc_id + 1))
      printf '{"jsonrpc":"2.0","method":"plugin.state.set","params":{"value":{"emit_count":%d}},"id":%d}\n' \
        "$emit_count" "$rpc_id"
      ;;
  esac
done
