# lens — the pixel-perfect agent verify loop

You built or fixed a TUI. Text capture can't see it: color, alignment,
focus highlight, glyph width, and "is the border actually broken at column
80" are all invisible to `pane.capture`. `lens` is the loop that closes
that gap — five primitives, no new mental model beyond what's in
[SKILL.md](../SKILL.md):

```
run → settle → glance → drive → diff
```

**run** a command in a hidden, self-cleaning pane → **settle** (block until
the screen stops repainting) → **glance** (atomic PNG + text of one frame,
by revision) → **drive** it (`pane.send_keys`, already in your toolbox) →
**diff** (prove exactly which cells changed, with PNG proof) → fix → re-glance
→ done, with pixels.

## The five verbs, exact grammar

Four of the five are `pane` subcommands that take the pane **as a bare
positional UUID** — not `-s/--session` like the pre-lens pane commands.
Only `lens run` is a `lens` subcommand (there is exactly one: `run`; do not
guess at `shux lens glance` or similar, they don't exist):

```bash
shux lens run [--size CxR] [--ttl DUR] [--max-runtime DUR] \
              [--env KEY=VALUE]... [--cwd PATH] [--wait] -- <argv...>

shux pane wait-settled <PANE> [--quiet DUR] [--timeout DUR]
shux pane glance       <PANE> [--png PATH] [--text-only] [--no-cursor] [--checkpoint]
shux pane checkpoint   <PANE>
shux pane diff         <PANE> --since REV [--heat PATH] [--no-row-text]
```

`shux lens` / `shux lens --help` prints this five-verb recipe on its own —
run it if you forget the shape mid-session.

`DUR` is a human duration everywhere in lens: `300ms`, `2s`, `1h`. RPC
fields underneath are always milliseconds; the CLI normalizes for you.

## The canonical loop (this is what E1 looks like end to end)

```bash
# 1. run — spawn hidden, no shell, self-cleaning.
RUN=$(shux --format json lens run --size 120x30 -- ./target/debug/my-tui)
PANE=$(echo "$RUN" | jq -r .result.pane_id)
SID=$(echo "$RUN"  | jq -r .result.session_id)

# 2. settle — block until the first frame stops repainting (NOT a sleep).
shux pane wait-settled "$PANE" --quiet 300ms --timeout 10s

# 3. glance — atomic PNG + text + revision of that exact frame.
BEFORE=$(shux --format json pane glance "$PANE" --checkpoint --png before.png)
REV=$(echo "$BEFORE" | jq -r .result.revision)

# 4. drive — ordinary pane.send_keys, nothing new here.
shux pane send-keys -s "$SID" -p "$PANE" --text 'q'
shux pane wait-settled "$PANE" --quiet 300ms --timeout 10s

# 5. diff — exactly what changed, with a heat-map PNG as proof.
shux pane diff "$PANE" --since "$REV" --heat delta.png

shux session kill "$SID"     # or let --ttl reap it
```

Every step is event-driven — `wait-settled` blocks on a `watch` channel, not
a poll loop, and there is never a bare `sleep` synchronizing on pane output
anywhere in this loop. If you're tempted to add one, use `wait-settled` (for
"stopped repainting") or `pane wait-for` (for "this text appeared") instead.

**"Settled" is not "process finished."** It means "quiet for `--quiet`". A
process that dribbles output with gaps *longer* than `--quiet` will report
settled=true between dribbles. If you need "the whole operation is done",
pair `wait-settled` with `pane wait-for <sentinel>` — wait for the sentinel
first, then settle to get a clean frame.

## `lens run` in depth

Spawns `argv` **directly into the PTY — no shell, ever.** No `.bashrc`, no
profile script, no `sh -c` wrapper. `argv[0]` is resolved via `PATH`.

```bash
shux lens run --size 100x30 --ttl 30s --max-runtime 1h \
  --env NO_COLOR=1 --env MY_FLAG=1 \
  --cwd /path/to/repo \
  -- nidhi -C /path/to/repo --no-animation
```

- `--size CxR` (default `80x24`): cols ∈ [20,500], rows ∈ [5,200].
- `--ttl DUR` (default `30s`, range [0,300s]): how long to keep the scratch
  session around **after the command exits**, before reaping it. Useful for
  a final glance after a short-lived command finishes.
- `--max-runtime DUR` (default `1h`, range [1s,24h]): hard cap on the
  scratch session's total lifetime, regardless of whether the command has
  exited. Applies even to long-running TUIs you forgot to kill.
- `--env KEY=VALUE` (repeatable): additions only, no inherit-suppression in
  v1 — the child gets the daemon's environment plus these.
- Async by default: prints `{session_id, pane_id, revision}` and returns
  immediately. The process keeps running in the background.

### `--wait`

```bash
shux lens run --wait -- sh -c 'exit 42'; echo $?    # → 42
```

Blocks the RPC until the command exits, adds `exit_code` to the printed
result, and the CLI process itself exits with the **child's** code — once
the child has actually started. Setup failures *before* the child starts
(bad argv[0], quota exhausted, invalid size) use the exit-code table below
instead, not the child's code.

**Signal death has no POSIX exit code.** If the process is killed by
`--max-runtime`, an explicit `session kill`, or anything else that never
lets it report its own status, the RPC's `exit_code` field comes back `-1`
— and because a Unix process exit truncates to its low 8 bits, the
shell-visible `$?` after `shux lens run --wait` is **255**, not `-1`. Treat
255 as "never exited on its own", not as a literal exit-code-255 from the
child:

```bash
shux lens run --max-runtime 2s --wait -- sleep 30
echo $?     # → 255 (reaped by --max-runtime, not a real exit code)
```

### Scratch sessions are hidden, not unlisted-and-forgotten

`lens run` is the **only** way a scratch session gets created — there is no
scratch parameter on `session create`. Scratch sessions are excluded from
the default `session list` so they don't clutter a human's session picker,
but hidden ≠ unauthorized: audit records always include them, and

```bash
shux session list --include-scratch     # reveals them, `scratch: true`
```

**Kill a scratch session by the UUID `lens run` gave you, or by name:**

```bash
shux session kill "$SID"          # UUID from lens run's `session_id` — works directly
shux session kill my-scratch-name # name also works, same as any other session
```

(`session kill` and the `-s/--session` flag on every `shux pane …` and
`shux window …` subcommand — including the snapshot commands — accept
either form. UUID-shaped input — hyphenated or 32-hex simple form —
resolves as a session ID first, falling back to a session NAMED that
string if no id matches; when both match, the ID wins and a warning is
printed. `session rename`, `session save`, and `session attach` are the
exceptions: they take the session NAME only. If you're driving a scratch
pane with pre-lens commands like `pane send-keys -s <SID> -p <PANE>`, the
UUID from `lens run`'s `session_id` is exactly what `-s` wants.)

Quota: 16 concurrent scratch sessions per daemon. The 17th `lens run` gets
`RESOURCE_EXHAUSTED` (CLI exit 5) until one is reaped or killed.

## Checkpoints and diff

`pane checkpoint` (or `pane glance --checkpoint`) snapshots the pane's
current visible frame, keyed by its revision. **At most 4 checkpoints per
pane** — a 5th evicts the oldest by creation revision (FIFO; reading a
checkpoint never refreshes its recency). Re-checkpointing the exact current
revision with no intervening change is a no-op, not a new slot.

```bash
shux pane checkpoint <PANE>              # → { revision, evicted_revision }
shux pane diff <PANE> --since <REV>      # → structured delta, exit 0 regardless of size
shux pane diff <PANE> --since <REV> --heat delta.png   # + a heat-map PNG
```

A diff result's `regions` are per-row half-open spans
(`{row, col_start, col_end}`), sorted and merged, capped at 256 — beyond
that only `bounding_box` is populated and `regions_truncated: true`.
`cells_changed` counts every glyph/color/attribute change; cursor position
is excluded from the count but a content change under the cursor still
counts. `diff` is data, not a verdict — a zero-cell delta is still a
successful `exit 0`, and CLI/RPC diff calls never fail just because nothing
changed.

**Resizing the pane or switching alt-screen invalidates every checkpoint on
it** (`RESIZE_INVALIDATED`, CLI exit 5) — re-checkpoint after either. A
`--since` revision with no matching checkpoint and no resize/alt-switch in
between is `STALE_REVISION` (also exit 5); the error payload lists which
revisions are still live.

## Exit codes (CLI-normative — do not guess at others)

| Exit | Meaning |
|--|--|
| 0 | success (including a diff with any delta, or `settled=true`) |
| 1 | `wait-settled` timeout (`settled=false`) · `lens run --wait` when the child's own exit code happens to be 1 |
| 2 | usage error / `INVALID_PARAMS` (bad flags, out-of-range size/quiet/timeout) |
| 3 | any other RPC error, including pane-not-found / daemon unreachable |
| 4 | `PERMISSION_DENIED` (plugin caller lacking the scope) |
| 5 | `STALE_REVISION` / `RESIZE_INVALIDATED` / `RESOURCE_EXHAUSTED` / `PAYLOAD_TOO_LARGE` / `SPAWN_FAILED` |

`lens run --wait`'s exit code is a special case: once the child has
**started**, the CLI exits with the child's own code — even if that code
collides with a row above (a child that legitimately exits `2` looks like
"usage error" to a naive script). Scripts that need certainty should parse
`--format json`, where `exit_code` is present in the result iff the child
actually ran. Setup failures *before* the child starts (bad argv[0], quota
full) use the table above, not this override.

## Output formats

`--format json` on any lens verb prints the **raw RPC `{result}` /
`{error}` envelope** — identical to `shux rpc call <method>` — so scripts
can `jq .result.revision` etc. regardless of whether they went through the
CLI verb or the raw RPC form. `--format text` (default) prints a styled
summary and writes files for `--png`/`--heat`. PNG/heat bytes are never
printed to stdout in text mode — always a file via the flag, or base64
buried in the JSON envelope.

## Secrets — no automated redaction

`pane glance` and `pane diff --heat` capture **exactly what's on screen** —
API keys pasted into a prompt, tokens echoed by a misconfigured tool,
credentials in a `.env` a TUI happens to display. There is no automated
redaction in lens v1. Treat every glance/diff PNG and text blob as
**log-sensitive**, same as you'd treat a raw terminal transcript:

- Don't glance or diff a pane you didn't create, unless the user explicitly
  directs you to.
- Don't paste glance text or attach glance/diff PNGs into a place a human
  didn't ask for (a public PR comment, a shared channel) without checking
  what's actually in frame first.
- The `lens run` loop above is safe by construction for this because it
  only ever targets a pane you just spawned yourself.

## When lens is the wrong tool

- **You just want a screenshot of a long-lived pane a human is using** —
  use `pane snapshot` / `window snapshot` (see [SKILL.md](../SKILL.md)),
  not `lens run` (which is for spawning something *new* in a *hidden*
  pane).
- **You need scrollback, not the viewport** — nothing in lens (or the rest
  of shux) exposes scrollback programmatically; v1 is viewport-only
  everywhere.
- **You're driving a long-lived interactive session a human might reattach
  to** — scratch sessions are quota-bounded, auto-reaped, and hidden from
  `session list` by design; they're for agent-owned throwaway verification,
  not persistent workspaces.
