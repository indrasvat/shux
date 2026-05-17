# shux for AI agents

shux is built so agents can drive it without the brittleness of wrapping
tmux. Every CLI command is a thin JSON-RPC call over a Unix domain socket.
There's no string parsing, no screen scraping, no waiting on tmux to flush.

This guide is for agent authors writing scripts, MCP tools, or harness code
that drives shux programmatically.

## The contract

| Property | What it means |
|---|---|
| **CLI == API** | Every subcommand maps 1:1 to a JSON-RPC method. The CLI is just a thin wrapper. |
| **Idempotent `--ensure`** | `shux session create X --ensure` is safe to retry. Creates only if missing. |
| **Deterministic state** | `shux session list --format json` returns the full session graph. Always consistent, never stale. |
| **Typed events** | `events.watch` (planned, M2) yields a sequenced stream of typed events for reacting to pane output, exit codes, focus changes. |
| **Same errors everywhere** | RPC error codes are stable: `-32601` method-not-found, `-32007` name-conflict, etc. CLI surfaces them as exit code 1 + structured stderr. |

## Connect

Two ways. Pick whichever fits your harness.

### Spawn the CLI

Easiest. Stable interface, JSON output via `--format json`:

```python
import json, subprocess

def shux_api(method, params=None):
    args = ["shux", "--format", "json", "rpc", "call", method]
    if params is not None:
        args.extend(["--params", json.dumps(params)])
    result = subprocess.run(args, capture_output=True, text=True, check=True)
    return json.loads(result.stdout)

state = shux_api("session.list")
```

### Talk JSON-RPC directly

Lower latency. Connect to `$XDG_RUNTIME_DIR/shux/shux.sock` (or
`$TMPDIR/shux-$UID/shux.sock`). Frame each message as 4-byte big-endian
length + JSON-RPC 2.0 payload.

```python
import socket, struct, json

def rpc(sock, method, params=None, id=1):
    req = json.dumps({"jsonrpc":"2.0","id":id,"method":method,"params":params or {}}).encode()
    sock.sendall(struct.pack(">I", len(req)) + req)
    n = struct.unpack(">I", sock.recv(4))[0]
    return json.loads(sock.recv(n))

s = socket.socket(socket.AF_UNIX)
s.connect("/tmp/shux-501/shux.sock")
print(rpc(s, "system.version"))
```

## The methods you'll use most

| Method | What it does | CLI mirror |
|---|---|---|
| `system.version` | Daemon version + git SHA | `shux version` |
| `system.health` | Daemon health probe | — |
| `session.list` | All sessions, sorted by created_at | `shux session list` |
| `session.create` | Create session, optionally with a command and initial pane title. CLI mirror sends caller cwd by default. | `shux session create X --title X` |
| `session.ensure` | Create-if-missing. CLI mirror sends caller cwd when creating. | `shux session create X --ensure --title X` |
| `session.kill` | Destroy session, reap PTYs | `shux session kill X` |
| `window.list` / `.create` / `.kill` / `.focus` / `.rename` / `.reorder` / `.ensure` | Window CRUD | `shux window <verb>` |
| `pane.list` / `.split` / `.focus` / `.focus_dir` / `.resize` / `.zoom` / `.swap` / `.kill` / `.ensure` | Pane CRUD + layout ops | `shux pane <verb>` |
| `pane.send_keys` | Forward keystrokes to a pane | `shux pane send-keys -t "..."` |
| `pane.run` | Run a command, await exit | `shux pane run -- ...` |
| `pane.capture` | Read VT-rendered text from a pane | `shux pane capture` |

## Identifying things

Every resource accepts EITHER a UUID or a name:

```bash
shux session kill work                                             # by name
shux session kill 8b1a3c5e                                         # by ID prefix (8 chars)
shux rpc call session.kill --params '{"id":"8b1a3c5e-..."}'        # by full UUID
```

## Idempotent patterns

```bash
# Create-if-missing — safe to run from a flaky retry loop:
shux session create build --ensure

# Pin the initial pane's border label when app OSC titles are noisy:
shux session create agent --title agent -- codex --yolo

# Run a command, get exit code:
shux pane run -s build -- cargo test
echo $?   # 0 on success, exit code of cargo test on failure

# Capture the last N lines of pane output:
shux pane capture -s build --lines 50 --format json
```

## Driving an attached session

If you need a real interactive PTY (e.g. running `vim` in a pane and sending
keystrokes), there are three flavors of input:

1. **`pane.send_keys` with text** — typed verbatim into the focused pane:
   ```bash
   shux pane send-keys -s work -t "git status\n"
   ```
2. **`pane.send_keys` with bytes (base64)** — raw bytes including escape
   sequences for arrow keys, Ctrl combos, etc:
   ```bash
   shux pane send-keys -s work --data $(echo -n $'\x1b[A' | base64)  # up arrow
   ```
3. **`pane.run`** — fire-and-forget; returns when the command exits:
   ```bash
   shux pane run -s work -- bash -c 'echo done'
   ```

To read what's currently on the pane's VT (think: a stable, ANSI-stripped
text view of what the user would see), use `pane.capture`.

## Error model

RPC errors are JSON-RPC 2.0 error objects:

```json
{ "jsonrpc": "2.0", "id": 1,
  "error": { "code": -32601, "message": "method_not_found",
             "data": { "method": "session.frob" } } }
```

Stable codes:

| Code | Symbol | Meaning |
|---|---|---|
| `-32600` | invalid_request | Malformed JSON-RPC envelope |
| `-32601` | method_not_found | Method doesn't exist |
| `-32602` | invalid_params | Wrong shape or missing required field |
| `-32603` | internal_error | Server-side bug |
| `-32700` | parse_error | JSON parse failure |
| `-32001` | frame_too_large | Single frame > 16 MB |
| `-32003` | auth_required | TCP transport, no auth token sent |
| `-32004` | not_found | Resource (session/window/pane) doesn't exist |
| `-32007` | name_conflict | Name already taken |
| `-32008` | version_conflict | Optimistic concurrency mismatch |

CLI translates these to exit code 1 with a human-readable message;
`--format json` returns the full error object so you can switch on `code`.

## Events (planned, M2)

`events.watch` will return a streamed JSON-Lines feed of typed events:

```jsonl
{"seq":1,"type":"pane_output","pane":"...","bytes":"..."}
{"seq":2,"type":"pane_exited","pane":"...","exit_code":0}
{"seq":3,"type":"window_focused","window":"..."}
```

Sequenced for gap detection. Until the M2 plugin host lands, the workaround
is polling `pane.capture` and `session.list` at a low rate.

## MCP integration (planned)

Once the M2 plugin system is in, a bundled `shux-mcp` plugin will expose
shux operations as MCP tools — drop into Claude Desktop or any MCP client
and you have a multiplexer your model can drive natively. Tracked as part
of M2 in [`roadmap.md`](roadmap.md).
