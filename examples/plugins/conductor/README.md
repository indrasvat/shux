# shux-conductor v0.1

VT-poll watchdog for coding-agent panes. The first phase of the
conductor design (`docs/designs/conductor/`): one bash plugin that
subscribes to pane lifecycle events, identifies known agent commands,
polls the visible terminal text every 2 seconds, classifies the
agent's state, and surfaces it in the pane border title.

## What it does

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

The pane's border title is set to `<agent> · <emoji>` and updated
only when the state changes.

## Install

```bash
shux plugin install ./examples/plugins/conductor/plugin.sh

# v0.20+ default-deny permission model — conductor doesn't own
# the user's agent panes, so each method needs an explicit grant.
shux plugin grant conductor pane.capture
shux plugin grant conductor pane.set_title
shux plugin grant conductor pane.send_keys
```

If you forget to grant, conductor's first denied call writes a
one-shot hint to stderr (visible in the daemon log) listing the
exact commands needed.

## Configure

Environment variables read at handshake time:

| Variable | Default | Purpose |
|---|---|---|
| `SHUX_CONDUCTOR_POLL_MS`        | `2000` | poll cadence (ms) |
| `SHUX_CONDUCTOR_SETTLE_MS`      | `5000` | grid-still duration before promoting `ready` → `idle` |
| `SHUX_CONDUCTOR_AUTO_DISMISS`   | `1`    | send Enter on `stuck` (set `0` to disable) |
| `SHUX_CONDUCTOR_CAPTURE_LINES`  | `40`   | how many trailing pane lines to capture each poll |

Pass them at install time via `shux plugin install` env inheritance,
or wrap conductor in a tiny shell script that sets them before exec.

## Try it

```bash
# Spawn a claude pane and watch the title flip.
shux session create demo -- claude
# Switch to the demo session and observe the border title:
#   demo · 1   claude · ⚡   →   claude · ✓
shux attach -s demo
```

## Out of scope for v0.1

- **Settle-snapshot archive** (PNG per idle transition) — landing in
  v0.2.
- **Multi-pane window aggregation** — track every pane in every
  session. Single-pane view ships in v0.1; cross-pane is v0.3.
- **Worktree-per-pane** — depends on the v0.20 permission model
  (now landed) but lives in v0.4.
- **ACP fast-path** — replaces the regex polling with structured
  agent events for opencode/gemini. v0.5+.

See [`docs/designs/conductor/README.md`](../../../docs/designs/conductor/README.md)
for the full roadmap.

## Requirements

- bash 4+ (associative arrays). On stock macOS run
  `brew install bash` and re-install conductor with the new shell.
- jq, shasum (or sha256sum), GNU `date` or `python3` for ms epoch.

All four are present on every supported shux platform.
