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

CLI-created sessions always send the caller's current directory as
`cwd` unless `--cwd <DIR>` is provided. Direct RPC callers should pass
`cwd` explicitly because a daemon is long-lived and its own process
directory is not meaningful project context.

## Session

### `session.create`

Spawn a new session with an initial window + pane.

```json
// Request
{ "name": "demo",                       // optional; auto-generates "session-N" if omitted
  "cwd":  "/path/to/dir",               // recommended; CLI fills caller cwd by default
  "pane_title": "demo",                 // optional; pins initial pane border title
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
// session.list — scratch sessions (from lens.run) are excluded unless
// include_scratch is true; scratch entries carry "scratch": true.
{ "include_scratch": false } → { "sessions": [{..., "attached": 2}, ...] }  // attached = clients on it

// session.kill — `id` is a STRICT UUID; names go via the separate `name`
// field. Pass exactly one of the two. expected_version optional.
{ "id": "<uuid>", "expected_version": 5 }
{ "name": "<name>", "expected_version": 5 }

// session.rename — same strict id-or-name split as session.kill
{ "id": "<uuid>", "new_name": "demo2", "expected_version": 5 }
{ "name": "<name>", "new_name": "demo2", "expected_version": 5 }

// session.ensure — create-if-missing
{ "name": "demo", "cwd": "/path/to/dir", "pane_title": "demo", "command": [...] }
```

`shux session kill` and the `-s/--session` flag on the `pane`/`window`
subcommands (incl. snapshots) additionally accept a UUID-shaped argument
and resolve it client-side: session ID first, falling back to a session
NAMED that string; the ID wins when both match (a warning is printed).
`session rename` / `session save` / `session attach` take NAMEs only. Raw
RPC callers pick the field themselves per the shapes above.

## Attached clients

### `client.list` / `client.detach`

Every terminal attached with `shux session attach` is a client. The status
bar of a session shows `◉ N attached` once two or more are on it.

```json
// client.list (shux client list)
{} → { "clients": [{ "id": "<uuid>", "session_id": "<uuid>", "session": "demo",
                     "cols": 120, "rows": 40, "attached_at": 1778... }] }

// client.detach (shux client detach <id>): ends that one connection; the
// client is told "removed". A unique id prefix works, in any case; an
// ambiguous one is invalid_params with `candidates`, an unknown one
// not_found, an empty or non-string id invalid_params.
{ "id": "3f2a" } → { "detached": "<uuid>" }
```

`attached_at` is Unix seconds. `--format plain` prints one tab-separated
line per client: id, session, `COLSxROWS`, `attached_at`.

A client that takes no bytes at all for 10–20 s (progress is checked every
10 s) is dropped; a slow one that keeps reading stays. Events:
`client.connected` (`terminal_cols`, `terminal_rows`), `client.resized`,
`client.disconnected` (`reason`: `detached`, `closed`, `removed`,
`connection_lost`, `session_destroyed`, `daemon_stopped`), and
`session.attached` / `session.detached` with the session id.

## Window

```json
// window.create — the window's name param is `name` (NOT `title`);
// auto-generated ("0", "1", ...) when omitted. session_id is a strict UUID.
{ "session_id": "<uuid>", "name": "editor", "command": [...], "cwd": "..." }
  → { "id": "uuid", "title": "editor", "session_id": "uuid", "pane_id": "uuid" }

// window.list — session_id is a strict UUID (resolve names client-side).
// Returns a BARE ARRAY, not a wrapper object.
{ "session_id": "<uuid>" } → [ {...}, ... ]

// window.focus — takes the window's own id (strict UUID) only. Position/name
// resolution is CLI-side sugar (`shux window focus -s S -w 2` is window 2,
// counted from 1 like `index` below; window names are never numbers), not RPC.
{ "id": "<window-uuid>", "expected_version": N }

// window.kill — window id only (no session_id param)
{ "id": "<window-uuid>", "expected_version": N }

// window.rename
{ "id": "<window-uuid>", "name": "new-name", "expected_version": N }

// window.ensure — create-if-missing by name within a session
{ "session_id": "<uuid>", "name": "editor", "cwd": "...", "command": [...] }
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

### `pane.wait_command`

A command typed into a pane's shell, ended, as the shell reports it over OSC
133 (`C` start, `D;<exit>` end). The shell must emit the marks:
`eval "$(shux shell-integration bash|zsh)"` in its rc file.

```json
pane.wait_command { "pane_id": "uuid", "after": 41, "timeout_ms": 30000, "output": true }
  → { "outcome": "completed", "exit_code": 1, "started": 57, "ended": 58, "ended_by": "end",
      "stdout": "...", "truncated": false }
  | { "outcome": "timeout", "no_integration": false }
  | { "outcome": "overrun", "kept": 32 }
  | { "outcome": "pane_gone", "reason": "exited" | "killed" | "respawned" | "not_started" }
```

- **`after`** is `as_of` from `pane.status`, read before the command was
  sent, or a previous `started`. The earliest command that *started* above it
  is returned. Omitted, it is now, which misses a command already sent.
- **`exit_code`** is null when the end carried none, or when the end was
  inferred: `ended_by: "prompt"` (for example after `exec bash`) or
  `"next_command"`.
- **A nested shell that also reports boundaries** (`bash`, `ssh`,
  `docker exec -it` into an integrated shell) ends the outer command at its
  first prompt, with `ended_by: "prompt"` and no exit code. Its own commands
  are then reported as they run.
- **`overrun`:** more than 32 commands ended after the floor. Read a new
  `as_of`.
- **`no_integration`:** the pane's shell has never sent a command start, so
  this timeout was never going to be met.
- **Lifecycle:** the wait ends `pane_gone` if the pane exits or is
  respawned.
- **Program status:** boundaries are tracked whether or not program status is
  enabled, and RIS does not reset them.
- **`output`** (only when asked for): what the command wrote between its
  start and its end, stdout and stderr as the terminal received them,
  ANSI-stripped, `\r\n` as `\n`, first 1 MiB (`truncated`), named as
  `pane.run_command` names it: `stdout`. It is the
  stream, not the screen, so long output and wrapped lines come back whole.
  Output is kept passively for every command, within a few MiB per pane; an
  older command's comes back `stdout: null, evicted: true`. zsh's
  partial-line mark (PROMPT_SP), which it prints before reporting the end, is
  not part of it, nor is ble.sh's `prompt_eol_mark`: the shipped integration
  names its line editor on the prompt mark (`133;A;shux-editor=zsh|ble|readline|none`), and
  the mark is trimmed only for the editor named, pinned to the pane's width.
- **`timeout_ms`**: 30 s by default, at most 3600000 (1 h). Past it the wait is
  refused with `-32602`, `data: { field, max, hint }`, exactly as
  `pane.wait_status` and `pane.wait_for` refuse it.
- `pane.status` reports `commands: { seen, running, shell, line_pending, input_present }`;
  `shell` is `unknown` (no boundary yet), `at_prompt`, `running` or `between`
  (a command ended, its next prompt not yet drawn); `line_pending` is true while
  a line submitted at the prompt has not started.
- **The boundaries are in-band.** Output the command itself prints, such as a
  file `cat`'d that contains `ESC ] 133;D;0 BEL`, is read as a boundary too, as
  it is by every terminal's shell integration. Trust `exit_code` only as far as
  you trust what the pane prints.

CLI: `shux pane wait-command <PANE> [--after AS_OF] [--timeout 30s] [--output]`.
- Exit 0: the command ended, whatever its own code (read `exit_code`).
- Exit 1: timeout.
- Exit 2: bad arguments, or no such pane.
- Exit 3: pane gone, overrun, or the daemon gone.

### `pane.status` / `pane.wait_status`

What a pane's program reports about itself over OSC 7501 (Claude Code
does): `idle`, `working`, `done`, `blocked` (on the user) or `error`.
Prefer these to scraping `pane.capture`. A pane whose program has never
reported has `seen: false`.

```json
pane.status      { "pane_id": "uuid" }
  → { "enabled": true, "seen": true, "summary": { "state": "blocked", "kind": "permission", ... },
      "records": [ { "id": "perm", "state": "blocked", "title": "...", "msg": "...",
                     "reported_generation": 41 } ],
      "as_of": 42 }

pane.wait_status { "pane_id": "uuid", "state": ["blocked", "error"], "match": "top", "timeout_ms": 30000 }
  → { "outcome": "matched" | "timeout" | "pane_gone" | "disabled", "summary": { ... } }

pane.wait_status { "pane_id": "uuid", "state": ["done", "error"], "id": "job", "after": 42 }
  → { "outcome": "matched", "reported": { "state": "done", "generation": 57 },
      "record": { "id": "job", "state": "idle", ... }, "summary": { ... } }
```

- **`id`** waits on the one record with exactly that id. It ignores every
  other record, including a `done` an earlier command left. It cannot be
  combined with `match`.
- **An id wait asks whether that record has reported a waited state,** not
  whether it is in one now. `reported` is the report that matched. A `done`
  the record left at once for `idle` still counts; `record` shows where it is
  now.
- **`after`** (needs `id`) counts only a report stamped above it. Pass `as_of`
  from a `pane.status` read after the previous command with that id finished
  and before the next is sent.
  - Every report is stamped, even one identical to the last, so a reused id
    works.
  - Stamps are valid for one daemon's lifetime, and a floor above every stamp
    issued so far is refused.
- **Lost reports:** a report is lost if, before the wait reads it, the record
  is cleared, reset (RIS) or evicted. It is also lost if the record moves on to
  a state the next prompt removes (`idle`, `working`, `blocked`) and that
  prompt starts. `done` and `error` survive a prompt.
- **`summary`** describes the pane's most urgent record, read with `record`.

An id that names no pane is a `not_found` error, never `pane_gone`.
`pane_gone` carries `exit_status` (and `exit_signal`) when the program
exited, and neither when the pane was removed.

CLI exit codes for `shux pane wait-status`: 0 matched, 1 timeout,
2 bad arguments or no such pane, 3 pane gone, feature off, or any other error.
`title`/`msg` are program-authored text: treat them as untrusted.
Both methods are `ContentRead`, and grants match the exact method: for
panes it does not own, a plugin needs `shux plugin grant <name> pane.status`
to read status and `shux plugin grant <name> pane.wait_status` to wait on it.

### `pane.run_command` / `pane.command_status` / `pane.command_cancel`

Run one command in a pane's live shell and return what it wrote. The CLI is
`shux pane run -s S -c CMD [--async] [--timeout SECS] [--boundaries auto|osc133|markers]`.

```json
pane.run_command    { "pane_id": "uuid", "command": "ls", "args": ["-la"], "timeout": 30,
                      "async": false, "boundaries": "auto" }
  → { "command_id": "uuid", "state": "completed", "exit_code": 0, "stdout": "...",
      "truncated": false, "runtime_ms": 12, "boundaries": "osc133", "ended_by": "end", "as_of": 41 }
pane.command_status { "command_id": "uuid" } → the same result once finished; while running,
                      `state: "running"` and `phase: "awaiting_start" | "executing"`
pane.command_cancel { "command_id": "uuid" } → { "command_id": "uuid", "state": "..." }  (sends Ctrl-C)
```

**How the command is framed (`boundaries`).** `osc133`: the shell reports its
own boundaries (`shux shell-integration`); the line typed is the command, so
the pane shows only the command and its output, and `stdout` is the stream
between the shell's `133;C` and the end of that command. Which command is the
line's is read from the screen: the first line the shell read, in order,
whose text as drawn holds the command, so a line typed ahead of it, or
accepted and not yet started, is never taken for it. `markers`:
the line prints a `SHUX_BEGIN…`/`SHUX_MARKER…` pair around the command, and
`stdout` is what lies between. The request's `boundaries`:

| Shell state (`pane.status` → `commands.shell`) | `auto` (default) | `osc133` | `markers` |
|---|---|---|---|
| `unknown`: no boundary ever seen | markers | wait for a prompt, else `no_prompt` | markers |
| `at_prompt` | osc133 | osc133 | markers |
| `between`: a command ended, prompt not drawn | wait for the prompt, then osc133 | same | markers |
| `running` | refused: `command_running` | refused | markers |

Before any framing but `markers`, the pane's terminal is asked who reads its
input (the foreground process group, and termios): another program in front
is refused as `other_program` unless it is a shell (one started from the
pane's, at its own prompt). It is waited out, up to `timeout`, while the marks say the prompt is
being drawn (prompt hooks) or the shell reports no marks (the last run's own
`(exit N)` may still hold the terminal); a line read with echo off is `hidden_input`; the
alternate screen is `full_screen`. At the prompt mark the terminal must be in
raw mode, as readline, zle and ble.sh leave it while they read: in line mode a
hook, or a line the shell accepted, runs, and it waits, then refuses
`prompt_not_ready`. A prompt whose mark says no line editor reads it (the
shipped bash integration under `--noediting`: `shux-editor=none`) is read in
line mode, so that is not taken as a sign. Input the pane was sent counts too: a line
submitted at the prompt (a CR or LF outside a bracketed paste) that has not
started, or input still queued, is `input_pending`; keys sent to the prompt
that submitted nothing are `input_present` (C-c gives a fresh prompt). A pane whose
program is not a shell (`python3`, `bash -c …`) and reports no prompt is
`not_a_shell` under `auto`. A multi-line command is refused
(`-32602`) unless the shell reports nothing (then markers) or `markers` is asked for. A control character in `command` or
`args` is refused under every framing (`-32602`, `data: { field, offset, char }`).

Refusals are `ResourceExhausted` (`-32012`) with
`data: { resource, reason, shell_state, foreground, foreground_pgid,
hidden_input, full_screen, hint }`; nothing was typed.

**How it ended (`state`).**

| `state` | Meaning |
|---|---|
| `completed` | `exit_code` is the command's. `ended_by` (`osc133` only): `end` (its own `D`), or `prompt` / `next_command`, where the shell drew a prompt or started another command without reporting this one's end, and `exit_code` is null |
| `not_run` | `osc133` only: the shell drew its next prompt without starting the line. `stdout` is the shell's reply as the screen showed it. Plain bash and zsh do this for a syntax error or a comment; ble.sh runs such a line instead, so it is `completed` with bash's own status (2 for a syntax error) |
| `timed_out` | the timeout passed first; `stdout` is what it wrote so far. Its program may still run, and the pane stays held until it ends or is cancelled |
| `cancelled` | `pane.command_cancel` |
| `pane_gone` | the pane's program ended or the pane was removed first |
| `overrun` | `osc133` only: more commands ended after the line than the terminal keeps |

- `as_of` (`osc133` only) is the floor read just before the line was typed, so
  the line's command is the first above it: pass it to
  `pane.wait_command` as `after`, with `output: true`, to keep waiting on a
  `timed_out` command until its own end.
- A `timed_out` result carries `phase` (`awaiting_start`: the line never
  started) and, when the terminal was reading with echo off, `hidden_input:
  true`. While running, `pane.command_status`'s `phase` is `awaiting_start`,
  `executing` or `awaiting_input` (echo off now).
- `stdout` is ANSI-stripped, `\r\n` as `\n`, up to 1 MiB (`truncated`).
- One command per pane. A second is refused with `ResourceExhausted`,
  `reason: "pane_held"`, `running_command_id` and `running_command_state`.
- `from_pane`: the caller's own pane (the CLI sends `$SHUX_PANE_ID`, which
  every pane's program has); a target equal to it is refused, `reason:
  "own_pane"`, here and in `pane.send_keys`.
- A `timed_out` result carries `hint`: the exact command to run next
  (`shux pane wait-command <PANE> --after <as_of> --output` for OSC 133,
  `shux pane command-cancel <ID>` for markers).
- CLI: `shux pane command-status <ID>` and `shux pane command-cancel <ID>`
  mirror `pane.command_status` and `pane.command_cancel`.
- `timeout` is whole seconds, 30 by default, at most 3600; anything else is
  refused with `-32602`.
- CLI exit: 0 ended (read `exit_code`), 1 timed out, 2 bad arguments or no
  such pane, 3 anything else; `--async` exits 0 once the line is typed. Under
  `--format json`, an error is `{"error": {code, message, data}}` on stdout.

### `pane.wait_exit`

Block until a pane's program exits and say how it ended; returns at once
for one that already has. OSC 7501 status plays no part: a program that
reports `done` and keeps running has not exited.

```json
pane.wait_exit { "pane_id": "uuid", "timeout_ms": 600000 }   // timeout_ms omitted: no deadline; 0: check once
  → { "outcome": "exited", "pane_id": "uuid", "exit_code": 7 }
  | { "outcome": "exited", "pane_id": "uuid", "signal": 15, "signal_name": "SIGTERM", "by_shux": true }
  | { "outcome": "timeout" | "killed" | "not_started", "pane_id": "uuid" }
  | { "outcome": "unknown", "pane_id": "uuid", "reason": "not_reaped" | "events_lost" }
```

`exited` carries exactly one of `exit_code` and `signal`. `signal_name` is
`null` for a signal with no name; `by_shux` appears when the signal that
ended the program is one shux sent — the pane was torn down, or the program
kept running after its terminal closed. `killed`: the pane was removed
before its program's exit was recorded. `not_started`: the pane's program
failed to spawn and no retry is under way; a pane still waiting for its
first spawn is waited on. `unknown`: the child could not be reaped, or the
events that would say how a removed pane ended are gone. A pane id the
daemon has never seen is a `-32004` not-found error; a pane removed since is
answered from event history when named by its full id. `Public`, like
`pane.list`.

CLI: `shux pane wait-exit <PANE> [--timeout D]` exits with its own code,
not the program's: 0 exited (read `exit_code` / `signal`), 1 timeout,
2 bad arguments or no such pane, 3 daemon gone, `not_started` or
`unknown`, 4 killed. JSON is `{"result": …}` or `{"error": …}`.

`session create`, `window create` and `pane split` take `--wait`
[`--wait-timeout D`]: spawn, wait, and print the wait's payload with the
create response under `created` (text and plain print the create line,
then the outcome), under the same exit codes — a failed create is 2 or 3,
never 1. `session create --wait` does not attach; `--wait` does not combine
with `--ensure`.

### `window.snapshot` / `session.snapshot`

Rasterize an entire window (all panes + borders + titles + focus highlight)
into a single PNG. `session.snapshot` is the same call against the session's
active window.

```json
// Request — window.snapshot
{ "window_id": "uuid",
  "cols": 200, "rows": 60 }       // optional; defaults 120 × 36 cells

// Request — session.snapshot (session_id is a strict UUID)
{ "session_id": "<uuid>",
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

Enumerate the panes of one window. With `session_id` (strict UUID), the
session's ACTIVE window is used — not every window of the session; iterate
`window.list` yourself for that. Returns a BARE ARRAY.

```json
{ "window_id": "<uuid>" }                // panes of that window
{ "session_id": "<uuid>" }               // panes of the session's ACTIVE window
  → [ { "id": "...", "window_id": "...", "title": "...",
        "command": [...], "cwd": "...",
        "exit_status": null|0, "exit_signal": null|15, "version": N }, ... ]
```

`exit_signal` is set when a signal ended the program; `exit_status` is then
`-1`.

### `pane.output.watch`

Subscribe to the data-plane stream of sampled PTY chunks. Returns a list
of recent chunks (or blocks up to `timeout_ms` waiting for new ones).
This is for live observation, not absence-of-bytes assertions.

```json
{ "pane_id": "uuid", "timeout_ms": 1500, "limit": 10 }
  → { "chunks": [{ "seq": 42, "bytes_b64": "...", "sampled": true, "timestamp": ... }, ...],
      "next_seq": 52 }
```

### `pane.record.start` / `pane.record.stop`

Record byte-exact raw PTY bytes to a daemon-side file. Bytes are teed at
the PTY read source before VT processing and before sampled
`pane.output.watch` coalescing.

```json
{ "pane_id": "uuid", "path": "/abs/path/pane.raw",
  "overwrite": false, "duration_ms": 10000 }
  → { "recording_id": "uuid", "pane_id": "uuid", "path": "/abs/path/pane.raw",
      "duration_ms": 10000, "lossless": true, "backpressure": true }

{ "recording_id": "uuid" }
  → { "recording_id": "uuid", "path": "/abs/path/pane.raw",
      "bytes_written": 12345, "status": "complete",
      "lossless": true, "error": null }
```

## Pane management

```json
// pane.split — split a pane into two
{ "pane_id": "uuid", "direction": "horizontal|vertical", "ratio": 0.5, "command": [...] }

// pane.focus / pane.focus_direction
{ "pane_id": "uuid" }
{ "session_id": "...", "direction": "up|down|left|right" }

// pane.zoom — toggle zoom on a pane; zooming in also focuses it
{ "pane_id": "uuid" }

// pane.swap — swap two panes within a window
{ "pane_id": "a", "target_pane_id": "b", "expected_version": N }

// pane.kill
{ "pane_id": "uuid", "expected_version": N }

// pane.set_title
{ "pane_id": "uuid", "title": "new" }    // omit for auto-derive; pass null to clear
```

## Lens — the pixel-perfect agent verify loop

Five methods that turn "I edited a TUI" into "I proved the fix worked, with
PNG evidence": **run** (spawn hidden) → **settle** (wait for stillness) →
**glance** (atomic pixels+text) → drive (`pane.send_keys`, unchanged) →
**diff** (prove exactly what changed). Full workflow guide, CLI grammar,
exit-code table, and the scratch-session lifecycle:
[references/lens.md](lens.md). This section is the RPC contract only.

### `pane.glance`

Atomic `{png, text, revision}` of one pane from **one** grid clone — unlike
`pane.snapshot` + `pane.capture` (two separate clones, can tear under
concurrent writes), glance guarantees the PNG and text agree on the same
frame.

```json
// Request
{ "pane_id": "uuid",
  "include_cursor": true,     // default true
  "include_png": true,        // default true; false = text-only, cheaper
  "checkpoint": false }       // default false; true = also store for pane.diff_since

// Response
{ "revision": 42, "cols": 80, "rows": 24,
  "cursor": { "row": 23, "col": 0, "visible": false },
  "alt_screen": false,
  "text": "...\n...",
  "png_base64": "iVBORw0KG...",     // null if include_png=false
  "checkpointed": false,
  "evicted_revision": null }        // set when checkpoint:true evicted the FIFO-oldest slot
```

### `pane.wait_settled`

Block until a pane's screen has been quiet for `quiet_ms`, or time out.
Event-driven off a `watch` channel — no polling, no sleeps. Settled means
"stopped repainting", **not** "process finished": pair with `pane.wait_for`
(sentinel text) when a slow-dripping process has silent gaps longer than
`quiet_ms`.

```json
// Request
{ "pane_id": "uuid", "quiet_ms": 300, "timeout_ms": 10000 }
// quiet_ms ∈ [10, 60000]; timeout_ms ∈ [quiet_ms, 600000]

// Response — a RESULT, never an RPC error, even on timeout
{ "settled": true, "revision": 42, "waited_ms": 12 }
```

### `pane.checkpoint` / `pane.diff_since`

`pane.checkpoint` stores the pane's current visible frame, keyed by its
revision. At most 4 checkpoints per pane; a 5th evicts the oldest by
creation revision (FIFO — reads never refresh recency). Re-checkpointing
the current revision with no intervening mutation is a no-op.

```json
// pane.checkpoint request/response
{ "pane_id": "uuid" }
  → { "revision": 42, "evicted_revision": null }

// pane.diff_since request
{ "pane_id": "uuid", "since_revision": 42,
  "changed_row_text": true,     // default true
  "heat_png": false }           // default false — rendered PNG with changed
                                 // cells tinted red, unchanged desaturated 50%

// pane.diff_since response
{ "from_revision": 42, "to_revision": 45,
  "cells_changed": 10, "cursor_moved": false,
  "regions": [ { "row": 5, "col_start": 10, "col_end": 19 } ],
  "regions_truncated": false,
  "bounding_box": { "row_start": 2, "col_start": 2, "row_end": 6, "col_end": 19 },
  "changed_row_text": { "5": "  A-PRESSED" },
  "heat_png_base64": null }
```

Resize or an alt-screen switch invalidates ALL checkpoints of that pane
(`RESIZE_INVALIDATED`, `-32011`); diffing a revision with no matching
checkpoint and no invalidation in between is `STALE_REVISION` (`-32010`,
`data.available` lists live revisions). Checkpoints never survive a daemon
restart.

### `lens.run` — the composite "run in a hidden pane" call

Allocates a hidden, quota-bounded (16 concurrent max), self-cleaning
**scratch session** and execs `argv` directly into its PTY — no shell, ever,
so no profile-script surprises. This is the ONLY way to create a scratch
session; there is no scratch parameter on `session.create`.

```json
// Request
{ "argv": ["nidhi", "-C", "/path/to/repo"],
  "cols": 80, "rows": 24,               // default 80x24; cols [20,500], rows [5,200]
  "env": { "NO_COLOR": "1" },           // additions only, no inherit control
  "cwd": null,                          // default: daemon cwd
  "post_exit_ttl_ms": 30000,            // default 30s; range [0, 300000]
  "max_runtime_ms": 3600000,            // default 1h; range [1000, 86400000]
  "wait": false }

// Response (wait=false) — async, returns immediately
{ "session_id": "uuid", "pane_id": "uuid", "revision": 1 }

// Response (wait=true) — blocks until the command exits, adds:
{ "exit_code": 0 }
```

Scratch sessions are excluded from default `session.list`; pass
`include_scratch: true` to reveal them (`"scratch": true` flag). Hidden from
listing is not the same as unauthorized — audit records and `session.kill`
always see them, by `id` (UUID, e.g. the `session_id` this call returns) or
`name`.

### Lens error codes

| Code | Meaning |
|--|--|
| -32010 | `STALE_REVISION` — no checkpoint at that revision; `data.available` lists live ones |
| -32011 | `RESIZE_INVALIDATED` — a resize/alt-switch invalidated checkpoints since; `data.hint` suggests re-checkpointing |
| -32012 | `RESOURCE_EXHAUSTED` — 16 concurrent scratch sessions already running |
| -32013 | `PAYLOAD_TOO_LARGE` — PNG/heat-PNG would exceed the 8 MiB cap; shrink the pane or use `include_png:false` |
| -32014 | `SPAWN_FAILED` — `argv[0]` not found / bad `cwd` / exec error; the scratch allocation is rolled back, nothing leaks |

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

### `plugin.state.{get,set,delete}` — plugin-only

```json
// get
{} → { "value": <JSON or null> }

// set
{ "value": <JSON> } → { "bytes_written": N }

// delete
{} → { "deleted": <bool> }
```

Per-plugin persisted state, survives hot reload + daemon restart.
On disk at `<daemon-cwd>/.shux/plugins/<plugin_name>/state.json`
(atomic writes via tempfile+rename). Cap: 256 KiB serialized.

Callable only from inside a process plugin — the daemon takes the
plugin's identity from the spawn context, so a plugin reads/writes
only its own state.

Full plugin-side documentation in [plugins.md](plugins.md).

### `event.publish` — plugin-only

```json
{ "event_type": "branch_changed", "data": {"branch": "main"} } →
  { "seq": 1042 }
```

Callable only from inside a process plugin (the daemon takes the
plugin's identity from the spawn context, not from params).
Published events land on the bus as
`plugin.<plugin_id>.<event_type>` and are filterable like any other
event (`shux events watch --filter plugin.git-status.`). External
RPC clients calling this method receive `method_not_found`.

Rules:

- `event_type` is required, non-empty, and **must not contain `.`**.
- `data` is any JSON value.

Full plugin-side documentation lives in [plugins.md](plugins.md).

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
