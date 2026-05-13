#!/usr/bin/env bash
# shux-watcher — pane-exit notifier (~50 lines, jq + bash).
#
# Demonstrates the v0.16+ `event.publish` plugin primitive:
#   - subscribes to pane.exited events from the bus
#   - re-emits each as a derived `plugin.watcher.command_exit`
#     event with the useful subset (session/pane/command/exit_status)
#   - the daemon namespaces every published event under
#     `plugin.<id>.<type>`, so other plugins (or `events watch
#     --filter plugin.watcher.`) can target this stream exactly.
#
# Try it:
#   shux plugin install ./examples/plugins/watcher/plugin.sh
#   shux events history --filter plugin.watcher. --count 20
#   # in another terminal — fires a pane.exited event:
#   shux session create demo -d -- bash -lc 'echo hi && exit 7'
#   # → watcher's derived event appears in events.history.
#
# Optional filter: only re-emit when exit_status matches a regex.
# Set EXIT_RE='[1-9]' to fan out only on non-zero exits.
#
# Zero deps beyond bash + jq.

set -u

EXIT_RE="${EXIT_RE:-}"

IFS= read -r _ || exit 1
printf '%s\n' '{"jsonrpc":"2.0","id":"init","result":{"name":"watcher","version":"0.1.0","subscribes":["pane.exited"],"provides":[],"capabilities":[]}}'

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

      rpc_id=$((rpc_id + 1))
      printf '{"jsonrpc":"2.0","method":"event.publish","params":{"event_type":"command_exit","data":%s},"id":%d}\n' \
        "$payload" "$rpc_id"
      ;;
  esac
done
