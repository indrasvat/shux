# shux process plugins — protocol reference (v0)

A process plugin is any executable that speaks shux's line-delimited
JSON-RPC dialect on stdin/stdout. The daemon spawns it on
`shux plugin install`, performs a one-line handshake, then multiplexes
three streams on the same pair of pipes:

1. **Daemon → plugin** events (bus notifications) and RPC responses on
   the plugin's stdin.
2. **Plugin → daemon** RPC requests on the plugin's stdout.
3. **Plugin diagnostics** on stderr — relayed to daemon `debug!()`
   logs, tagged with the plugin name. No separate log file.

Framing: **one JSON value per line**, terminated with `\n`. Not the
length-prefixed framing the public daemon RPC uses on UDS/TCP — the
plugin pipe is line-delimited because pipes don't need length prefixes
to find frame boundaries.

## CLI surface

```bash
shux plugin install <path> [--args ...] [--cwd <dir>] [--no-watch]
shux plugin list                  # alias: ls — shows `watching` column
shux plugin reload <name>         # manual kill + respawn
shux plugin kill <name>

# permission management (default-deny model — see Permissions section)
shux plugin grant <name> <method>[--target <id>] [--subscribe]
shux plugin revoke <name> <method> [--target <id>] [--subscribe]
shux plugin grants <name>         # show method + subscribe allow-set
shux plugin audit <name> [--tail N]   # tail NDJSON audit log
```

`name` is what the plugin reports in its manifest, not the script
filename. Two plugins reporting the same `name` is a `NameConflict`
on install — the second loses, the first stays running.

### Hot reload (default on)

`shux plugin install` watches the plugin's source file. On every
save the daemon kills the running child and reinstalls from the
same source — debounced ~250ms so a burst of saves coalesces into
one respawn. The new manifest takes effect immediately; subscribed
events resume with the new code in <500ms.

```bash
shux plugin install ./my-plugin.py   # watching=true
$EDITOR ./my-plugin.py               # edit, save
# → daemon log: "watcher: file changed, reloading"
# → next event hits the new code

shux plugin install ./my-plugin.py --no-watch   # opt out for CI / prod
shux plugin reload my-plugin                    # manual tick when --no-watch
```

The watcher uses `notify` (FSEvents on macOS, inotify on Linux).
It watches the parent directory so editors that atomic-rename on
save (vim, neovim with `backupcopy=auto`) still trigger.

## Phase 1: handshake

Daemon writes one line on stdin within 5 seconds of spawn:

```json
{"jsonrpc":"2.0","method":"plugin.init","id":"init","params":{}}
```

Plugin must reply with one line on stdout within the 5 s budget:

```json
{"jsonrpc":"2.0","id":"init","result":{
  "name":"my-plugin",
  "version":"0.1.0",
  "subscribes":["session.created","window.created"],
  "provides":[],
  "capabilities":[]
}}
```

| Field | Type | Notes |
|---|---|---|
| `name` | string, required | Must be non-empty and unique across installed plugins. |
| `version` | string, required | Free-form (semver suggested). |
| `subscribes` | array of strings | Event-type prefixes. `"session."` matches `session.created`, `session.killed`, etc. `[]` means "no events" (plugin receives only RPC responses). |
| `provides` | array of strings | Reserved for phase 1+; currently informational. |
| `capabilities` | array of strings | Reserved for phase 1+; currently informational. |

If the manifest doesn't arrive in 5 s, or if `name` is empty, or if
the line isn't valid JSON, the daemon kills the child and returns
`HandshakeFailed` to the install caller. Long plugin init should
happen *after* sending the manifest, not before.

## Phase 2: event delivery (daemon → plugin)

For every bus event whose `type` matches one of the plugin's
`subscribes` prefixes, the daemon writes one line to the plugin's
stdin:

```json
{
  "jsonrpc": "2.0",
  "method":  "event",
  "params": {
    "type":      "session.created",
    "seq":       42,
    "timestamp": 1715553600000,
    "data": {
      "type": "SessionCreated",
      "data": {
        "session_id": "f0a1...UUID",
        "name":       "alpha"
      }
    }
  }
}
```

Selected payload fields by event type (the part nested under
`params.data.data`):

| Event type | Payload |
|---|---|
| `session.created` | `session_id`, `name` |
| `session.killed` | `session_id`, `name` |
| `session.renamed` | `session_id`, `old_name`, `new_name` |
| `window.created` | `window_id`, `session_id`, `title`, `index` |
| `window.renamed` | `window_id`, `old_title`, `new_title` |
| `window.killed` | `window_id`, `session_id` |
| `window.activated` | `window_id`, `session_id`, `previous_window_id` |
| `pane.created` | `pane_id`, `window_id`, `session_id`, `command` |
| `pane.exited` | `pane_id`, `window_id`, `session_id`, `command`, `exit_status` *(numeric when the child self-exits; null when destroyed via `pane.kill` / `window.kill` / `session.kill`)* |
| `pane.focused` | `pane_id`, `window_id`, `session_id`, `previous_pane_id` |
| `pane.title_changed` | `pane_id`, `window_id`, `session_id`, `old_title`, `new_title` |

- Filter on `params.type` (top-level, from `EventMetadata.event_type`).
  This is the canonical event-type string — `session.created`,
  `pane.exited`, `window.renamed`, etc.
- Payload fields live at `params.data.data.*`. Read
  `params.data.data.session_id`, not `params.session_id`. The
  outer `params.data.type` is the Rust enum variant name
  (`SessionCreated`) and is NOT part of the stable contract — use
  `params.type` for routing.
- The `params.data.data.*` re-wrap is an inherited ergonomics wart
  from the bus envelope and is documented as a future breaking-change
  flatten. Don't depend on it forever, but until then, navigate it.

## Phase 3: plugin → daemon RPC

The plugin can call any registered shux RPC method (`window.rename`,
`pane.send_keys`, `state.apply`, `session.create`, …) by writing a
request line to **stdout**:

```json
{
  "jsonrpc": "2.0",
  "method":  "window.rename",
  "params":  {"id": "<window UUID from event>", "name": "🚀 alpha"},
  "id":      1001
}
```

The daemon writes the response back to **stdin** as another line,
with the same `id`:

```json
{"jsonrpc":"2.0","id":1001,"result":{"window_id":"...","new_title":"🚀 alpha"}}
```

or an error:

```json
{"jsonrpc":"2.0","id":1001,"error":{"code":-32004,"message":"window not found","data":{"resource":"window","id":"..."}}}
```

### Identifiers are UUIDs, not human names

`pane.send_keys`, `window.rename`, `window.focus`, `session.kill`,
etc. all expect the UUID fields (`session_id` / `window_id` /
`pane_id`) directly. The CLI's name → UUID resolution lives in the
CLI, not the RPC layer, so a plugin that passes `{"session":"alpha"}`
gets `invalid_params` back. The good news: event payloads carry the
UUIDs, so use those directly.

### RPC method discovery

The CLI is the most up-to-date schema reference for every RPC
method:

```bash
shux window rename --help      # describes the window.rename params
shux pane send-keys --help     # describes the pane.send_keys params
```

`shux rpc call <method> [--params <PARAMS>]` is the raw fallthrough
from outside the daemon — useful for prototyping a plugin call by
hand before wiring it into the plugin loop. `--params` accepts
inline JSON, `@<file>`, or `-` (stdin).

### Publish your own events: `event.publish`

A plugin can emit events onto the daemon's bus so other plugins (or
external `events watch` consumers) can subscribe to them.

```json
{
  "jsonrpc": "2.0",
  "method":  "event.publish",
  "params":  {"event_type": "branch_changed", "data": {"branch": "main"}},
  "id":      2001
}
```

Response (success):

```json
{"jsonrpc":"2.0","id":2001,"result":{"seq":<u64>}}
```

The daemon namespaces every plugin-emitted event under
**`plugin.<plugin_id>.<event_type>`** so the plugin's identity is
locked in and subscribers can target a specific plugin's stream:

```bash
shux events watch --filter plugin.git-status.            # only this plugin's events
shux events watch --filter plugin.git-status.branch_     # one type
shux events watch --filter plugin.                       # ALL plugin events
```

Rules:

- `event_type` is required, non-empty, and **must not contain `.`**
  (the daemon owns the `plugin.<id>.` namespace; embedded dots would
  let a plugin synthesise events under a sibling's prefix).
- `data` is any JSON value (object / array / string / number / null).
- `plugin_id` is taken from the calling plugin's manifest name — it
  cannot be spoofed via params.

See the reference plugin at
[`examples/plugins/watcher/`](https://github.com/indrasvat/shux/blob/main/examples/plugins/watcher/plugin.sh)
for a tiny working example.

### Persist state across hot reload: `plugin.state.*`

Hot reload kills the running plugin process and respawns it from the
(possibly updated) source — all in-memory state is lost. To keep
counters, last-seen markers, per-project preferences, etc., a plugin
persists them to a small daemon-managed store.

```json
// read (returns null if nothing's been written yet)
{"jsonrpc":"2.0","method":"plugin.state.get","params":{},"id":3001}
// → {"jsonrpc":"2.0","id":3001,"result":{"value": null}}

// write (atomic — tempfile + rename)
{"jsonrpc":"2.0","method":"plugin.state.set",
 "params":{"value":{"hits":42,"branch":"main"}},"id":3002}
// → {"jsonrpc":"2.0","id":3002,"result":{"bytes_written":34}}

// delete (returns whether a file actually existed)
{"jsonrpc":"2.0","method":"plugin.state.delete","params":{},"id":3003}
// → {"jsonrpc":"2.0","id":3003,"result":{"deleted":true}}
```

On-disk layout (per project, gitignored by default via
`.shux/.gitignore`):

```
<project-root>/.shux/plugins/<plugin_name>/state.json
```

Where **project root** is resolved by the CLI at install time:

1. Start from the user's `cwd` when `shux plugin install` was run.
2. Walk up looking for an existing `.shux/` directory — that's the
   project root.
3. If none found, anchor at the cwd itself.

This means a daemon shared across multiple project checkouts keeps
each project's plugin state isolated. The daemon's own cwd is **not**
used for path resolution.

Rules:

- `value` is any JSON value (object / array / string / number /
  null). Plain `null` deletes nothing — use `plugin.state.delete`.
- Total serialized state is capped at **256 KiB**. Larger blobs
  should go to your own path under `<state_root>/<plugin_name>/`
  directly.
- The store is **per-plugin**: a plugin reads/writes ONLY its own
  state, never another's. Identity is taken from the spawn context.
- Survives `plugin reload`, `plugin kill` + re-install, daemon
  restart. Wiped only by `plugin.state.delete` or by removing the
  file on disk.

A plugin that wants to restore its in-memory state after a hot
reload typically does this first, right after the handshake:

```bash
printf '{"jsonrpc":"2.0","method":"plugin.state.get","params":{},"id":1}\n'
# read one line back from stdin — that's the response; parse .result.value
```

### CLI ↔ RPC namespace mapping

The CLI doesn't mirror the RPC namespace exactly. **Session ops
live at the top level** because they're what a human runs most;
window and pane ops nest under their nouns. The plugin always
calls the RPC name (left column).

**The mapping is mechanical: RPC dots become CLI spaces.** Every
noun is namespaced — no top-level shortcut verbs. Aliases (e.g.
`session ls` for `session list`) are listed in parens.

| RPC method | CLI command |
|---|---|
| `session.create` | `shux session create <NAME>` (or `-s <NAME>`) |
| `session.list` | `shux session list` (alias `session ls`) |
| `session.rename` | `shux session rename -s <OLD> -n <NEW>` |
| `session.kill` | `shux session kill <NAME>` (or `-s <NAME>`) |
| `session.ensure` | `shux session create --ensure <NAME>` |
| `(attach, client-side)` | `shux session attach <NAME>` |
| `window.create` | `shux window create -s <SESSION> -n <NAME>` |
| `window.list` | `shux window list -s <SESSION>` |
| `window.rename` | `shux window rename -s <SESSION> -w <CURRENT> -n <NEW>` |
| `window.focus` | `shux window focus -s <SESSION> -w <NAME>` |
| `window.kill` | `shux window kill -s <SESSION> -w <NAME>` |
| `window.snapshot` | `shux window snapshot -s <SESSION>` |
| `pane.send_keys` | `shux pane send-keys -s <SESSION> --text "..."` |
| `pane.list` | `shux pane list -s <SESSION>` |
| `pane.split` | `shux pane split -s <SESSION>` |
| `pane.kill` | `shux pane kill -s <SESSION>` |
| `pane.snapshot` | `shux pane snapshot -s <SESSION>` |
| `pane.capture` | `shux pane capture -s <SESSION>` |
| `pane.wait_for` | `shux pane wait-for -s <SESSION> --text "..."` |
| `state.apply` | `shux state apply <template.toml>` |
| `events.history` | `shux events history` |
| `events.watch` | `shux events watch` |
| `plugin.install` | `shux plugin install <path>` |
| `plugin.list` | `shux plugin list` |
| `plugin.reload` | `shux plugin reload <name>` |
| `plugin.kill` | `shux plugin kill <name>` |
| _(any method)_ | `shux rpc call <method> --params <JSON\|@FILE\|->` |

When in doubt: `shux --help` lists every namespace, then each
subcommand has its own `--help` with every accepted flag. The
`session` namespace also accepts `ses` / `sess` aliases.

### Common plugin RPC calls — params + result shapes

These are the methods plugins reach for most often. Every example
uses the UUID fields the event payload already carries. All
mutating methods also accept `expected_version: u64` for optimistic
concurrency (omit on first call). Shapes verified against the live
daemon — if you find a drift, that is a bug, file it.

| Method | Params | Result shape |
|---|---|---|
| `window.rename` | `{"id": "<window UUID>", "name": "<new title>"}` | Returns the full updated window object: `{id, title, index, pane_count, is_active, active_pane_id, session_id, version}`. `name` is required and must be a non-null string — pass `null` and you get `invalid_params: "missing 'name' parameter"`. (To clear a manual override use `pane.set_title --clear` on a pane; windows have no manual-override-clear path.) |
| `window.focus` | `{"id": "<window UUID>"}` | `{...window, previous_window_id: <UUID or null>}`. |
| `window.list` | `{"session_id": "<session UUID>"}` | **Bare array** `[{id, title, index, pane_count, is_active, active_pane_id, session_id, version}, ...]` — NOT wrapped in `{windows: [...]}`. The window UUID field is `id`, not `window_id`. |
| `pane.send_keys` | `{"pane_id": "<UUID>", "text": "..."}` — or `data: "<base64>"` for control bytes. Accepts `session_id` / `window_id` instead of `pane_id` to target the resolved active pane (see chain below). | `{bytes_written, pane_id}`. |
| `pane.set_size` | `{"pane_id": "<UUID>", "cols": 200, "rows": 60}` | `{pane_id, cols, rows}`. Synchronous — the next snapshot reflects the new dims. |
| `pane.snapshot` | `{"pane_id": "<UUID>"}` | `{pane_id, png_base64, format, width, height, cols, rows, cell_width, cell_height}`. Capped at 16M pixels. |
| `pane.capture` | `{"pane_id": "<UUID>"}` | `{pane_id, cols, rows, cursor, text, lines}` — `text` is ANSI-stripped, `lines` is per-row. |
| `pane.split` | `{"pane_id": "<UUID>", "direction": "horizontal" \| "vertical"}` | `{pane: {id, command, cwd, exit_status, ...}}` — the new pane's UUID lives at `result.pane.id`. |
| `pane.kill` | `{"pane_id": "<UUID>"}` | `{killed: "<UUID>"}`. **Returns `invalid_params` with `"cannot remove last pane from window"` if it's the only pane.** Use `window.kill` or `session.kill` for those cases. |
| `session.create` | `{"name": "<unique>", "cwd": "/path"}`. Optional `command: ["bash","-l"]`. CLI wrappers fill `cwd` from the caller's current directory. | `{id, name, window_id, pane_id, windows, window_count, active_window_id, created_at}`. |
| `session.list` | `{}` | `{sessions: [{id, name, active_window_id, windows, window_count, ...}, ...]}` — the `sessions` wrapper is present here. |
| `session.kill` | Either `{"id": "<session UUID>"}` OR `{"name": "<session name>"}`. | `{killed: "<name>"}`. Reaps all windows and panes. |
| `state.apply` | `{"ops": [<Op>, ...]}` | `{batch_id, correlation_id, spawn_results}`. Atomic — all graph ops commit or none. See `references/templates.md` for the `Op` shape. |
| `events.history` | `{"count": 50, "filter": ["session."]}` | `{current_seq, events: [...]}` — the events array is **inside the result object**, not the result itself. |

**Identifier resolution chain.** When you only have a session id but
need a pane, `pane.send_keys` (and other pane methods) accept
`session_id` and resolve to the session's active window's active
pane. Similarly, `window_id` → active pane of that window. Use the
longest-known identifier to be explicit; rely on the chain when you
can't.

**Discovering an unlisted method.** Run `shux <subcommand> --help` —
the CLI handler is the canonical source of truth for which JSON
fields each method accepts. If a method is callable via the CLI it
is callable from a plugin with the same params.

**Verifying a response shape on the fly.**
`shux rpc call <method> [--params <JSON|@FILE|->]` prints `{result: ...}`
(or `{error: ...}`) to stdout. Pipe through `jq '.result | keys'`
to see exactly which fields come back.

### Common gotchas plugins hit

- **Single-pane sessions auto-collapse.** When the only pane in a
  session exits, the session is destroyed (tmux parity). A plugin
  that reacts to `pane.exited` and tries to `window.rename` the
  source session will hit "session not found". Either subscribe to
  a longer-lived session (one with ≥ 2 panes / ≥ 2 windows) or
  pick a different target window in the same agent's workspace.
- **`pane.send_keys` to an exited pane returns "pane VT not found".**
  Once a pane exits, its VT is torn down even though the graph still
  carries the `Pane` entry briefly. If you need to bell or signal on
  exit, target a sibling pane in the same session, not the one that
  just died.
- **`pane.kill` rejects the last pane in a window.** The daemon
  refuses to leave a window paneless. Either split first or kill
  the window directly.
- **`pane.exited.exit_status` is `null` only for API destruction.**
  When a pane is destroyed via `pane.kill` / `window.kill` /
  `session.kill`, the event fires with `exit_status: null`. When
  the child command exits on its own (the PTY hits EOF), the
  daemon reaps it and populates the actual exit code — even for
  signal-killed children, `.code()` returns the integer when the
  shell wraps it (e.g. `sh -c 'exit 7'` → `exit_status: 7`).
  Filter on `null` for "destroyed via API"; filter on numeric for
  "command exited on its own with status N".
- **Events flow through a double envelope.** `params.type` is the
  flat event type (route on this). The payload fields live at
  `params.data.data.*` — see [Phase 2](#phase-2-event-delivery-daemon--plugin)
  above. A future PR will flatten this; until then, navigate the
  double-nest.

## Phase 4: shutdown

`shux plugin kill <name>` causes the daemon to:

1. Write `{"jsonrpc":"2.0","method":"plugin.shutdown","params":{}}`
   to the plugin's stdin.
2. Wait up to 2 s for the child to exit on its own.
3. SIGKILL if it's still running.

A well-behaved plugin loops reading lines from stdin and exits 0 on
seeing `plugin.shutdown`. If it ignores the frame, the SIGKILL still
reaps it — `kill_on_drop(true)` is set on the child handle.

## Minimum-correct skeleton (bash)

```bash
#!/usr/bin/env bash
set -u
# 1. Handshake.
IFS= read -r _ || exit 1
printf '%s\n' '{"jsonrpc":"2.0","id":"init","result":{
  "name":"hello","version":"0.1.0",
  "subscribes":["session.created"],"provides":[],"capabilities":[]
}}'

# 2. Event + response loop.
rpc_id=1000
while IFS= read -r line; do
  case "$line" in
    *'"plugin.shutdown"'*) exit 0 ;;
    *'"session.created"'*)
      sid=$(printf '%s' "$line" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p' | head -1)
      [ -n "$sid" ] || continue
      rpc_id=$((rpc_id + 1))
      printf '{"jsonrpc":"2.0","method":"pane.send_keys","params":{"session_id":"%s","text":"echo from plugin\n"},"id":%d}\n' \
        "$sid" "$rpc_id"
      ;;
  esac
done
```

That's the full shape. See
[`examples/plugins/hello/plugin.sh`](https://github.com/indrasvat/shux/blob/main/examples/plugins/hello/plugin.sh)
for the same plugin with comments.

## Permissions (default-deny)

From v0.19 every plugin RPC frame passes through a permission check
before it reaches the router. The full design lives at
[`docs/designs/permissions/README.md`](https://github.com/indrasvat/shux/blob/main/docs/designs/permissions/README.md);
the short version:

- **Identity is a per-install UUID**, not the plugin name. Reinstalling
  with the same name does NOT inherit the predecessor's grants or
  entity ownership. Look up the UUID with `shux plugin list --format json`.
- **Sensitivity tiers**:
  - `Public` — `session.list`, `window.list`, `pane.list`, `plugin.list`,
    `system.version`, `system.health`. Always allowed.
  - `ContentRead` — `pane.capture`, `pane.snapshot`, `pane.output.watch`,
    `pane.command_status`, `pane.wait_for`, `session.snapshot`,
    `window.snapshot`, `events.history`, `events.watch` (when not
    self-scoped). Default-deny; auto-allowed for entities the plugin
    created; explicit grant otherwise.
  - `OwnedMutation` — `pane.send_keys`, `pane.kill`, `pane.split`,
    `pane.resize`, `pane.set_title`, `window.create`, `session.create`,
    every other mutation. Same ownership-or-grant rule.
  - `Grantable` — `state.apply` only. No ownership shortcut; needs
    `shux plugin grant <name> state.apply` (blanket) to call at all.
  - `PluginsForbidden` — `plugin.install`, `plugin.kill`, `plugin.reload`,
    `plugin.grant`, `plugin.revoke`, `plugin.grants`, `plugin.audit`.
    Flat-deny to plugins (only the user / CLI may call). No grant path.
- **`events.watch` is param-aware**: a filter starting with
  `plugin.<self>.` is treated as `Public` (a plugin can always watch its
  own events). Broader filters are `ContentRead`.
- **Manifest `subscribes:` is locked after first install.** Hot reload
  fails handshake if the new manifest adds a filter that the user
  hasn't allowed via `shux plugin grant <name> <filter> --subscribe`.
- **Audit log is unconditional.** Every parsed plugin RPC frame
  appends one NDJSON line to `.shux/plugins/by-id/<uuid>/audit.log`,
  rotated at 1 MiB (keeps `.log.{1..5}`).
- **Files are atomic + symlink-rejecting.** Grants and audit writes
  use temp + `rename(2)`; both refuse to follow a symlink at the target
  path.

### Granting + revoking

```bash
shux plugin grant conductor pane.snapshot                       # blanket
shux plugin grant conductor pane.send_keys --target <pane-uuid> # scoped
shux plugin grant conductor state.apply                         # Grantable
shux plugin grant conductor pane.input.keystroke --subscribe    # widen
                                                                # manifest

shux plugin revoke conductor pane.snapshot
shux plugin revoke conductor pane.send_keys --target <pane-uuid>

shux plugin grants conductor    # human view
shux plugin grants conductor --format json
```

A denied call returns JSON-RPC error `-32004` with
`data.reason` ∈ {`plugins_forbidden`, `no_grant_and_not_owned`,
`no_grant`}. Plugins should surface this to the user — the council
review explicitly rejected a "prompt-on-first-use" channel for v0,
so errors are the only signal.

### Reference: enforcement chokepoint

`dispatch_plugin_frame` in `crates/shux-plugin/src/lib.rs` runs
`permissions::check` + `audit::record` on every plugin RPC frame
BEFORE forwarding to the router. The plugin-only intercepts
(`event.publish`, `plugin.state.*`) are audited with
`reason: "plugin_self_namespace"` and skip the check (they only
touch the plugin's own namespace).

## Out of scope for v0

- **Sandboxing.** Plugins run with the same uid and filesystem access
  as the `shux` daemon. Same trust model as a shell function — the
  permission model gates the RPC surface, NOT direct filesystem or
  network access.
- **WASM plugins.** Process plugins ship first; the sandboxed-
  distribution layer (WASM + WASI Preview 2) is queued for a later
  milestone.
- **`plugin.audit` RPC for plugins themselves.** Only the user/CLI
  can read another plugin's audit log; plugins inspecting their own
  log can `tail` the file directly. Tracked as a v0.next gap (the
  CLI == API principle wants every CLI verb to have a corresponding
  RPC — currently `plugin.audit` exists, but plugins are forbidden
  to call it).
- **Prompt-on-first-use** UX (browser-style permission prompts). The
  enforcement model can grow this later without changing the on-disk
  grant format.

## Where to learn more

- The task design doc: [`docs/tasks/044a-process-plugins-v0.md`](https://github.com/indrasvat/shux/blob/main/docs/tasks/044a-process-plugins-v0.md).
- Permission model design + council review:
  [`docs/designs/permissions/README.md`](https://github.com/indrasvat/shux/blob/main/docs/designs/permissions/README.md).
- The host source: [`crates/shux-plugin/src/lib.rs`](https://github.com/indrasvat/shux/blob/main/crates/shux-plugin/src/lib.rs).
- The integration tests (which exercise every flow in this doc):
  [`crates/shux-plugin/tests/plugin_lifecycle.rs`](https://github.com/indrasvat/shux/blob/main/crates/shux-plugin/tests/plugin_lifecycle.rs)
  + [`tests/permissions.rs`](https://github.com/indrasvat/shux/blob/main/crates/shux-plugin/tests/permissions.rs).
