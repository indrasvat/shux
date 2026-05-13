# shux-conductor v0.3

Coding-agent watchdog for shux. One bash plugin (~440 lines) that
turns shux into a live agent dashboard:

1. **VT-poll watchdog** (v0.1) — subscribes to `pane.created` /
   `pane.exited`, identifies known agent commands by basename,
   polls visible pane text every 2 s, classifies state, sets a
   per-pane border title.
2. **Settle-snapshot archive ⭐** (v0.2) — on every `ready→idle`
   transition, calls `pane.snapshot` and saves a PNG of the
   settled state to `.shux/conductor/snapshots/`. Maintains a
   rolling `INDEX.tsv` row per snapshot. **No other agent watchdog
   in the cmux/dmux/amux space can do this — they don't own a
   rasterizer.** This is the conductor's headline feature.
3. **Window aggregation + OS notifications** (v0.3) — tracks per-
   window in-flight counts. Fires ONE `osascript display
   notification` (macOS) or `notify-send` (Linux) the moment a
   window's last in-flight agent pane goes idle. Re-arms on the
   next state change.

Currently recognises four agent commands by basename: `claude`,
`codex`, `opencode`, `gemini`. For each pane spawned with one of
these as `command[0]`, conductor:

| State | Marker | Meaning |
|---|---|---|
| ready    | `○` | agent prompt visible, waiting for input |
| thinking | `●` | "Working" / "Thinking" / "Generating" / spinner glyph visible |
| idle     | `✓` | grid unchanged for `SETTLE_MS` (5 s default) |
| stuck    | `!` | trust prompt / `[y/n]` visible — auto-dismissed via Enter |
| empty    | `·` | pane has no content yet |

Markers are picked from JetBrains Mono Regular's geometric-shapes
block so they render cleanly in `pane.snapshot` PNGs.

The pane's border title is set to `<agent> · <marker>` and updated
only when the state changes.

## Install

```bash
shux plugin install ./examples/plugins/conductor/plugin.sh

# v0.20+ default-deny permission model — conductor doesn't own
# the user's agent panes, so each method needs an explicit grant.
shux plugin grant conductor pane.capture     # poll the visible text
shux plugin grant conductor pane.set_title   # update the border
shux plugin grant conductor pane.send_keys   # auto-dismiss stuck prompts
shux plugin grant conductor pane.snapshot    # v0.2: settle-archive PNGs
```

If you forget to grant, conductor's first denied call writes a
one-shot hint to stderr (visible in the daemon log) listing the
exact commands needed.

## Configure

Environment variables read at handshake time:

| Variable | Default | Purpose |
|---|---|---|
| `SHUX_CONDUCTOR_POLL_MS`        | `2000`              | poll cadence (ms) |
| `SHUX_CONDUCTOR_SETTLE_MS`      | `5000`              | grid-still duration before promoting `ready` → `idle` |
| `SHUX_CONDUCTOR_AUTO_DISMISS`   | `1`                 | send Enter on `stuck` (set `0` to disable) |
| `SHUX_CONDUCTOR_CAPTURE_LINES`  | `40`                | how many trailing pane lines to capture each poll |
| `SHUX_CONDUCTOR_SNAPSHOTS`      | `1`                 | enable v0.2 settle-snapshot archive (`0` to disable) |
| `SHUX_CONDUCTOR_SNAPSHOT_DIR`   | `.shux/conductor/snapshots` | where archived PNGs + INDEX.tsv land |
| `SHUX_CONDUCTOR_SNAPSHOT_COLS`  | `100`               | (advisory; daemon currently renders the VT's actual dimensions) |
| `SHUX_CONDUCTOR_SNAPSHOT_ROWS`  | `32`                | (advisory) |
| `SHUX_CONDUCTOR_NOTIFY`         | `system`            | `system` / `stdout` / `off` — backend for v0.3 notifications |
| `SHUX_CONDUCTOR_NOTIFY_TITLE`   | `shux-conductor`    | notification title shown by `osascript` / `notify-send` |

Pass them at install time via `shux plugin install` env inheritance,
or wrap conductor in a tiny shell script that sets them before exec
(see the v0.2/v0.3 shoot scripts under `.shux/scripts/` for examples).

## Try it

```bash
# Spawn a claude pane and watch the title flip.
shux session create demo -- claude
# Switch to the demo session and observe the border title:
#   claude · ○   →   claude · ●   →   claude · ✓
shux attach -s demo

# Browse the settle-snapshot archive after the agent has been
# idle a few times.
ls .shux/conductor/snapshots/
column -t -s "$(printf '\t')" .shux/conductor/snapshots/INDEX.tsv

# Multi-pane window: snapshot a single window with three agents,
# get one OS notification when they all settle.
shux session create dash -d -- claude
shux rpc call state.apply --params '{"ops":[
  {"op":"split_pane","target":"<pane-uuid>","direction":"vertical",  "ratio":0.5,"command":["codex"]},
  {"op":"split_pane","target":"<pane-uuid>","direction":"horizontal","ratio":0.5,"command":["opencode"]}
]}'
shux attach -s dash
# → 3 borders: claude · ✓ | codex · ✓ | opencode · ✓
# → 1 OS notification: "window <uuid>: all agent panes idle"
```

> **Subtle:** `pane.split` RPC accepts a `command` field but does NOT
> persist it on `Pane.command` — conductor would see an empty command
> and refuse to track. `state.apply` with `SplitPane` ops DOES persist
> the command (apply-batch was fixed for this on PR 4). Use
> `state.apply` (or `session.create` / `window.create` with `--cmd`)
> when you want a non-default-shell pane that conductor can see.

## Visual proofs

- v0.1 — single-pane watchdog: `pages/screenshots/conductor-v0.1-demo.png`
- v0.2 — settle-snapshot archive: `pages/screenshots/conductor-v0.2-settle-archive.png`
- v0.3 — multi-pane + notification: `pages/screenshots/conductor-v0.3-notifications.png`

Reproducer scripts: `.shux/scripts/conductor_v0.1_shoot.sh`,
`.shux/scripts/conductor_v0.2_shoot.sh`,
`.shux/scripts/conductor_v0.3_shoot.sh`.

## Out of scope for v0.1–v0.3

- **Worktree-per-pane** (v0.4) — needs design work on "cd a running
  pane". A subprocess that's already exec'd into the agent can't
  have its CWD changed externally; needs either an interception
  hook in the spawn path or a CLI helper that wraps the spawn. Not
  a plain plugin concern.
- **ACP fast-path** (v0.5+) — replaces the regex polling with
  structured agent events for opencode / gemini (native ACP
  support) and claude / codex (npm adapter shims). Different
  architecture: spawn agent's ACP server as a child of conductor,
  speak ACP, re-emit `session/update` events on shux's bus.
- **Transcripts** (v0.6) — depends on v0.5.
- **Cross-agent notes / task board** (v0.7) — `flock`-coordinated
  shared state files; standalone follow-up.
- **Tool-call routing** (v0.8) — depends on v0.5; requires the
  permission model audit log to be queryable per-call (tracked as
  the v0.next gap on the permissions design doc).

See [`docs/designs/conductor/README.md`](../../../docs/designs/conductor/README.md)
for the full roadmap and prior-art notes.

## Requirements

- bash 4+ (associative arrays). On stock macOS, run `brew install bash`
  and re-install conductor with the new shell.
- `jq`, `shasum` (or `sha256sum`), `base64` (BSD or GNU).
- For v0.3 system notifications: `osascript` (macOS) or `notify-send`
  (Linux). Falls back to a stderr `conductor[notify]: ...` line if
  neither is available.

All present on every supported shux platform.
