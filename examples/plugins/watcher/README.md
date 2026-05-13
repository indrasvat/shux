# shux-watcher — `event.publish` reference plugin

Tiny but real. ~50 lines of bash, demonstrates the v0.16+ plugin
primitive `event.publish` end-to-end:

1. Subscribe to `pane.exited` from the daemon's event bus.
2. Optionally filter by exit-status regex (`EXIT_RE='[1-9]'` → only
   non-zero exits).
3. Re-emit each as a `plugin.watcher.command_exit` event with the
   useful subset — `session_id`, `pane_id`, `exit_status`, `command`.

The daemon namespaces every published event under
`plugin.<plugin_id>.<type>`, so subscribers can target this exact
plugin's stream via `events watch --filter plugin.watcher.`.

## Try it

```sh
shux plugin install ./examples/plugins/watcher/plugin.sh

# In one terminal: watch for derived events
shux events watch --filter plugin.watcher.

# In another: trigger one
shux session create demo -d -- bash -lc 'echo hi && exit 7'
# → watcher emits plugin.watcher.command_exit with exit_status=7.

# Or just check history
shux events history --filter plugin.watcher. --count 20
```

## Why this is useful beyond the demo

- **CI ping.** Pair `plugin.watcher.command_exit` with a notifier
  plugin (osascript / Slack webhook / Telegram bridge) to get a
  ping the moment a non-zero exit lands.
- **Multi-plugin composition.** Another plugin (e.g. an aggregator)
  can subscribe to `plugin.watcher.command_exit` and accumulate
  stats without re-implementing the detection logic.
- **Per-project routing.** Set `EXIT_RE='[1-9]'` to only fan out
  failures; pair with `pane.send_keys` to bounce them into a
  designated reporting pane.

## Protocol notes

- The plugin sends `event.publish` requests on its stdout; the
  daemon replies with `{"result": {"seq": <u64>}}` on success.
- `event_type` must not contain dots — the daemon owns the
  `plugin.<id>.` namespace, so a plugin can't synthesise an event
  under a sibling's prefix.
- The published `data` field can be any JSON value (object, array,
  string). Subscribers see the namespaced filterable string in
  `meta.event_type` and the raw payload under `params.data.data`.
- This plugin subscribes via the standard `subscribes:` field in its
  manifest; the daemon delivers matching events on stdin without
  any extra wiring.
