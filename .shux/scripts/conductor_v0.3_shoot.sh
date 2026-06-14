#!/usr/bin/env bash
#
# conductor_v0.3_shoot.sh — multi-pane window aggregation +
# notification firing demo. Spawns three fake agent panes, captures
# the conductor's stderr (via NOTIFY=stdout), and renders both the
# multi-pane snapshot AND the notification line into one PNG via a
# bottom shell pane.
#
# Output:
#   pages/screenshots/conductor-v0.3-notifications.png

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
source "${REPO_ROOT}/.shux/scripts/lib/shux_harness.sh"
SHUX="${SHUX_BIN:-shux}"
SESSION="conductor-v0.3-shoot"
PLUGIN_NAME="conductor"
PLUGIN_SRC="examples/plugins/conductor/plugin.sh"
OUT=".shux/out/conductor-v0.3-notifications.png"
FINAL="pages/screenshots/conductor-v0.3-notifications.png"
RUNTIME_DIR="${SHUX_RUNTIME_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/shux-conductor-v03.XXXXXX")}"
export XDG_RUNTIME_DIR="${RUNTIME_DIR}"

mkdir -p "$(dirname "$OUT")"

FAKE_AGENT_DIR="/tmp/conductor-v0.3-shoot"
NOTIFY_LOG="/tmp/conductor-v0.3-notify.log"

cleanup() {
    shux_harness_kill_plugin "$RUNTIME_DIR" "$SHUX" "$PLUGIN_NAME"
    shux_harness_cleanup_runtime "$RUNTIME_DIR" "$SHUX" "$SESSION"
    rm -f "$FAKE_AGENT_DIR"/* "/tmp/conductor-v0.3-wrap.sh" "$NOTIFY_LOG" 2>/dev/null || true
    rmdir "$FAKE_AGENT_DIR" 2>/dev/null || true
}
trap cleanup EXIT INT TERM HUP

cleanup
sleep 0.3
mkdir -p "$FAKE_AGENT_DIR"

for name in claude codex opencode; do
    cat > "$FAKE_AGENT_DIR/$name" <<EOF
#!/usr/bin/env bash
echo "$name v1.0.0"
echo "$name > "
sleep 60
EOF
    chmod +x "$FAKE_AGENT_DIR/$name"
done

cat > /tmp/conductor-v0.3-wrap.sh <<EOF
#!/usr/bin/env bash
exec env SHUX_CONDUCTOR_SETTLE_MS=1500 \
    SHUX_CONDUCTOR_SNAPSHOTS=0 \
    SHUX_CONDUCTOR_NOTIFY=stdout \
    bash $(pwd)/$PLUGIN_SRC 2>$NOTIFY_LOG
EOF
chmod +x /tmp/conductor-v0.3-wrap.sh

"$SHUX" plugin install /tmp/conductor-v0.3-wrap.sh >/dev/null
sleep 0.5
"$SHUX" plugin grant "$PLUGIN_NAME" pane.capture   >/dev/null
"$SHUX" plugin grant "$PLUGIN_NAME" pane.set_title >/dev/null
"$SHUX" plugin grant "$PLUGIN_NAME" pane.send_keys >/dev/null

# Three side-by-side agent panes in a SINGLE window so we can prove
# conductor's "all panes in window settled → ONE notification"
# semantic. We go through `state.apply` (atomic batch) because
# `pane.split` RPC accepts a `command` field but doesn't persist
# it to `Pane.command` — conductor would see an empty command and
# refuse to track. Apply-batch was fixed for this on PR 4.
"$SHUX" session create "$SESSION" -d -- "$FAKE_AGENT_DIR/claude" >/dev/null
sleep 0.4
P1=$("$SHUX" --format json pane list -s "$SESSION" | jq -r '.[0].id')

# Atomic batch: split twice off the original pane to add codex +
# opencode + a fourth shell pane for the notification log tail.
# PaneRef is `untagged` — a bare UUID string OR a back-ref object —
# NOT `{pane_id: ...}` like the pane.split RPC takes.
APPLY_RESULT=$("$SHUX" --format json rpc call state.apply --params "$(jq -cn \
    --arg p "$P1" \
    --arg cx "$FAKE_AGENT_DIR/codex" \
    '{ops: [
        {op: "split_pane", target: $p, direction: "vertical",   ratio: 0.5, command: [$cx]},
        {op: "split_pane", target: $p, direction: "horizontal", ratio: 0.5, command: ["/bin/bash"]}
    ]}')")
P_CODEX=$(echo "$APPLY_RESULT" | jq -r '.result.outputs[0].pane_id')
P_BOTTOM=$(echo "$APPLY_RESULT" | jq -r '.result.outputs[1].pane_id')

APPLY_RESULT=$("$SHUX" --format json rpc call state.apply --params "$(jq -cn \
    --arg p "$P_CODEX" --arg op "$FAKE_AGENT_DIR/opencode" \
    '{ops: [
        {op: "split_pane", target: $p, direction: "vertical", ratio: 0.5, command: [$op]}
    ]}')")
sleep 0.4

# Bottom shell pane: tail the notification log so the snapshot
# captures the conductor[notify] line firing in real time.
"$SHUX" pane send-keys -s "$SESSION" -p "$P_BOTTOM" --text "clear; echo '── conductor notification log ──'; tail -F $NOTIFY_LOG | grep --line-buffered -E 'notify|tracking'
" >/dev/null

# Wait for settle + notification.
sleep 8

SNAP_COLS=130
SNAP_ROWS=44
"$SHUX" window snapshot -s "$SESSION" \
    --cols "$SNAP_COLS" --rows "$SNAP_ROWS" -o "$OUT" >/dev/null
cp "$OUT" "$FINAL"

echo "→ $FINAL"
file "$FINAL"
echo "--- conductor stderr (notify lines) ---"
grep -E "tracking|notify" "$NOTIFY_LOG" || true
