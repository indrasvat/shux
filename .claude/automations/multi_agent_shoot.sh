#!/usr/bin/env bash
# Capture three real `shux snapshot` PNGs of agent splash screens —
# claude, codex, opencode — side-by-side ammo for the landing-page
# "multi-agent driving shux" gallery tab.
#
# Output: $OUT/m-claude.png, $OUT/m-codex.png, $OUT/m-opencode.png
# Default OUT=/tmp. Default cell grid is 90x33 → ~720x528 PNGs.

set -euo pipefail

OUT=${OUT:-/tmp}
SHUX=${SHUX:-shux}
COLS=${COLS:-90}
ROWS=${ROWS:-33}

# Wait wrappers — agent CLIs take a beat to draw their splash; sleep
# values were tuned against the existing in-repo screenshots.
SPLASH_WAIT=${SPLASH_WAIT:-3.5}
PROMPT_WAIT=${PROMPT_WAIT:-0.4}

mkdir -p "$OUT"

# ------------------------------------------------------------------
# helpers
# ------------------------------------------------------------------

# Kill a session if it exists (idempotent, error-tolerant).
kill_if_exists () {
  local name=$1
  ( $SHUX kill -s "$name" >/dev/null 2>&1 || true )
}

# Run a shoot for one agent.
#   $1 = friendly slug (claude|codex|opencode)
#   $2 = bash -lc command body (env-injection happens here so the
#        spawned PTY inherits IS_DEMO, etc — daemon-spawned PTYs
#        inherit the daemon's env, not the CLI invoker's)
#   $3 = demo prompt text to leave in the agent's input box
#   $4 = optional pre-prompt key sequence (base64). Used by codex to
#        press CR on the trust dialog before the splash settles.
#   $5 = optional override for the splash wait (seconds). Codex on
#        first-run takes longer to initialize its TUI keyboard
#        reader, and CR sent too early lands in the raw tty buffer
#        before the trust prompt is listening.
shoot () {
  local slug=$1 cmd_body=$2 prompt=$3 pre_data=${4:-} splash_wait=${5:-$SPLASH_WAIT}
  local sess="ag-${slug}"
  local dir; dir=$(mktemp -d -t "shux-ma-${slug}.XXXXXX")

  kill_if_exists "$sess"

  # Spawn via raw RPC: cwd pins to the ephemeral dir; command goes as
  # an array so we can wrap in `bash -lc` to evaluate env-prefixed
  # syntax (e.g. `IS_DEMO=1 exec claude`). String form would have
  # been whitespace-split and exec'd directly, dropping the env.
  local cmd_json
  cmd_json=$(jq -nc \
    --arg name "$sess" \
    --arg cwd "$dir" \
    --arg body "$cmd_body" \
    '{name:$name, cwd:$cwd, command:["bash","-lc",$body]}')
  $SHUX api session.create "$cmd_json" >/dev/null

  sleep "$splash_wait"

  # Trust-prompt dismissal (codex). DQo= is base64("\r\n") — send
  # both CR and LF so we cover whatever the agent's keymap expects.
  # Twice, with a settle in between, in case the first press lands
  # before the trust dialog's keyboard reader is wired up.
  if [[ -n "$pre_data" ]]; then
    $SHUX pane send-keys -s "$sess" --data "$pre_data" >/dev/null
    sleep 0.8
    $SHUX pane send-keys -s "$sess" --data "$pre_data" >/dev/null
    sleep 1.8
  fi

  # Type the demo prompt into the agent's input box (no Enter — we
  # want it visible in the screenshot, not submitted).
  $SHUX pane send-keys -s "$sess" --text "$prompt" >/dev/null
  sleep "$PROMPT_WAIT"

  $SHUX snapshot -s "$sess" --cols "$COLS" --rows "$ROWS" -o "$OUT/m-${slug}.png" >/dev/null

  kill_if_exists "$sess"
  rm -rf "$dir"

  echo "→ $OUT/m-${slug}.png"
}

# ------------------------------------------------------------------
# shoot all three
# ------------------------------------------------------------------

# IS_DEMO=1 strips email + org from claude's splash. Confirmed by
# last session's PII audit. Inject into the shell body itself so the
# daemon-spawned PTY actually sees it.
shoot claude "IS_DEMO=1 exec claude" "Refactor this Rust Fibonacci impl for clarity"

# Codex prints a trust prompt on first run in a fresh cwd. Send CR
# (DQ==) to accept option 1 ("Yes, continue") before the splash
# settles.
# Send both CR+LF (DQo=) and bump splash wait to 7s — codex's first
# launch in a fresh cwd has slow keychain/config init before its TUI
# starts reading stdin.
shoot codex "exec codex" "Find the memory leak in this Node.js worker" "DQo=" 7

shoot opencode "exec opencode" "Audit this Python for thread-safety risks"

echo "done."
