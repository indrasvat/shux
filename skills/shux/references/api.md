# shux RPC reference

All methods accept JSON in, return JSON out. Errors follow JSON-RPC 2.0
shape with shux-specific error codes (`-32601` method-not-found, `-32602`
invalid-params, `-32004` not-found, `-32002` version-conflict). Every
mutating method optionally accepts `expected_version` for optimistic
concurrency.

The shell wrapper:

```bash
shux rpc call <method> [--params <JSON|@FILE|->]   # one-shot RPC.
                                                    # Prints {result:...} or {error:...} on stdout.
                                                    # --params accepts inline JSON, @file, or - (stdin).
                                                    # Defaults to {} for no-arg methods.
```

Or use the noun-namespaced verbs (every RPC method has a 1:1 CLI
form: dots become spaces, underscores become kebabs):

```bash
shux session create demo -- lazygit        # → session.create
shux session list                           # → session.list
shux pane send-keys -s demo --text 'j'      # → pane.send_keys
shux pane set-size -s demo --cols 200 --rows 60  # → pane.set_size
shux pane snapshot -s demo -o frame.png     # → pane.snapshot (one pane, no chrome)
shux window snapshot -s demo -o frame.png   # → window.snapshot (composed)
shux session snapshot -s demo -o frame.png  # → session.snapshot (session's active window)
shux state apply spec.toml                  # → state.apply
shux events watch --filter 'session.'       # → events.watch
shux config validate ./my-config.toml       # → CLI-only: positional path
```

## Session

### `session.create`

Spawn a new session with an initial window + pane.

```json
// Request
{ "name": "demo",                       // optional; auto-generates "session-N" if omitted
  "cwd":  "/path/to/dir",               // optional; defaults to daemon cwd
  "command": ["bash", "-l"] }           // optional; defaults to user shell

// Response
{ "id": "uuid",
  "name": "demo",
  "active_window_id": "uuid",
  "window_id": "uuid",
  "windows": ["uuid"],
  "window_count": 1,
  "pane_id": "uuid",
  "created_at": 1778... }
```

### `session.list`, `session.kill`, `session.rename`, `session.ensure`

```json
// session.list
{} → { "sessions": [{...}, ...] }

// session.kill
{ "id": "name-or-uuid", "expected_version": 5 }   // expected_version optional

// session.rename
{ "id": "name-or-uuid", "new_name": "demo2", "expected_version": 5 }

// session.ensure — create-if-missing
{ "name": "demo", "command": [...] }
```

## Window

```json
// window.create
{ "session_id": "uuid", "title": "editor", "command": [...], "cwd": "..." }
  → { "id": "uuid", "title": "editor", "session_id": "uuid", "pane_id": "uuid" }

// window.list
{ "session_id": "uuid-or-name" } → { "windows": [{...}, ...] }

// window.focus
{ "session_id": "...", "window_id": "uuid-or-index" }

// window.kill / window.rename / window.ensure
{ "session_id": "...", "id": "...", "expected_version": N }
```

## Pane I/O — the core agent surface

### `pane.send_keys`

Send keystrokes to a pane's PTY.

```json
// Request — pick one of text/data
{ "pane_id": "uuid",
  "text": "j" }                         // raw UTF-8 text (JSON-quoted)

{ "pane_id": "uuid",
  "data": "Gw==" }                      // base64-encoded raw bytes (use for Esc/Enter/Tab/Ctrl-X)

// Response
{ "pane_id": "uuid", "bytes_written": 1 }
```

### `pane.set_size`

Synchronous PTY + VT resize. The RPC returns only after `vt.resize` has
been applied, so the next `pane.snapshot` is guaranteed to capture at the
requested dimensions. 2-second internal timeout on the ack.

```json
// Request
{ "pane_id": "uuid", "cols": 200, "rows": 60 }    // cols 4..=1000, rows 2..=1000

// Response
{ "pane_id": "uuid", "cols": 200, "rows": 60 }
```

### `pane.snapshot`

Rasterize the live pane to a PNG and return base64.

```json
// Request
{ "pane_id": "uuid" }

// Response
{ "pane_id": "uuid",
  "png_base64": "iVBORw0KG...",
  "width":  1800,
  "height": 1140,
  "cell_width":  9,
  "cell_height": 19,
  "cols": 200,
  "rows": 60,
  "format": "png" }
```

Implementation notes:

- The live `Grid<Cell>` + cursor are cloned under the pane-IO mutex, then
  the lock is dropped before rasterization + PNG encoding (which run on
  a `spawn_blocking` worker so they don't starve the runtime).
- Only the visible viewport is cloned — not the scrollback. Hard cap of
  16 M output pixels (~4000×4000) — over-cap requests are rejected with
  `-32602` *before* any allocation.
- Cursor is rendered only when `cursor.visible` is true (alt-screen apps
  that hide the cursor produce snapshots without a stray block).

### `pane.capture`

Plain-text capture of up to N most-recent **non-blank** lines, ANSI
stripped. The walk starts from the last row that has content and goes
back N rows — blank rows below the cursor (the typical case for any TUI
that doesn't fill the whole viewport) are skipped. Returns `"\n"` when
the viewport is entirely blank.

```json
{ "pane_id": "uuid", "lines": 50 } → { "text": "...", "lines": 50 }
```

### `window.snapshot` / `session.snapshot`

Rasterize an entire window (all panes + borders + titles + focus highlight)
into a single PNG. `session.snapshot` is the same call against the session's
active window.

```json
// Request — window.snapshot
{ "window_id": "uuid",
  "cols": 200, "rows": 60 }       // optional; defaults 120 × 36 cells

// Request — session.snapshot
{ "session_id": "uuid-or-name",
  "cols": 200, "rows": 60 }       // optional

// Response (same shape for both)
{ "window_id":   "uuid",
  "png_base64":  "iVBORw0KG...",
  "width":  1800, "height": 1140,
  "cell_width":  9, "cell_height": 19,
  "cols":   200,    "rows":   60,
  "format": "png" }
```

Use this for full-window visual regression (single PNG per `shux state apply`
run vs a golden). Use `pane.snapshot` when you only care about one pane
and want to skip border / status-bar composition.

### `pane.list`

Enumerate panes in a window (or across all windows of a session).

```json
{ "window_id": "uuid" }                  // panes of one window
{ "session_id": "uuid-or-name" }         // panes of all windows in a session
  → { "panes": [{ "id": "...", "window_id": "...", "title": "...",
                  "command": [...], "cwd": "...",
                  "exit_status": null|0, "version": N }, ...] }
```

### `pane.output.watch`

Subscribe to the data-plane stream of sampled PTY chunks. Returns a list
of recent chunks (or blocks up to `timeout_ms` waiting for new ones).

```json
{ "pane_id": "uuid", "timeout_ms": 1500, "limit": 10 }
  → { "chunks": [{ "seq": 42, "bytes_b64": "...", "timestamp": ... }, ...],
      "next_seq": 52 }
```

## Pane management

```json
// pane.split — split a pane into two
{ "pane_id": "uuid", "direction": "horizontal|vertical", "ratio": 0.5, "command": [...] }

// pane.focus / pane.focus_direction
{ "pane_id": "uuid" }
{ "session_id": "...", "direction": "up|down|left|right" }

// pane.zoom — toggle zoom on a pane
{ "pane_id": "uuid" }

// pane.swap — swap two panes within a window
{ "pane_id": "a", "target_pane_id": "b", "expected_version": N }

// pane.kill
{ "pane_id": "uuid", "expected_version": N }

// pane.set_title
{ "pane_id": "uuid", "title": "new" }    // omit for auto-derive; pass null to clear
```

## State

### `state.apply` — atomic batch

```json
// Request
{ "ops": [
    { "op": "create_session", "name": "demo",
      "initial_command": ["nvim"], "initial_window_title": "editor" },
    { "op": "create_window", "session": {"ref_op": 0}, "title": "agents",
      "initial_command": ["claude"] },
    { "op": "split_pane", "target": {"ref_op": 1}, "direction": "vertical",
      "ratio": 0.4, "command": ["bash"] }
] }

// Response
{ "outputs":       [{op_index, session_id, window_id, pane_id}, ...],
  "spawn_results": [{op_index, pane_id, spawned: true|false, error: null|"..."}] }
```

Graph mutations are all-or-nothing. PTY spawn outcomes are reported
per-pane and do **not** roll back the graph (rolling back already-launched
subprocesses has its own side effects; honest reporting beats dishonest
atomicity).

### `events.history`

```json
{ "count": 100 } →
  { "events": [{"seq": 1, "type": "session.created", "data": {...}, "ts": ...}, ...] }
```

Events are sequenced and gap-detectable. Excludes `pane.output` (those
flow through the data-plane sealed bus, not the event history).

### `system.version`

```json
{} → { "name": "shux", "version": "0.8.0", "git_sha": "..." }
```

## Error envelope

```json
{ "error": {
    "code": -32602,
    "message": "invalid_params",
    "data": { "detail": "rows/cols out of range (got rows=1)" } } }
```

| Code   | Meaning                                       |
|--      |--                                             |
| -32601 | `method_not_found`                            |
| -32602 | `invalid_params` (data.detail = explanation)  |
| -32603 | `internal_error`                              |
| -32002 | `version_conflict` (data = {resource, id, expected, actual}) |
| -32003 | `name_conflict` (data = {resource, name})     |
| -32004 | `not_found` (data = {resource, id})           |
