#!/usr/bin/env bash
#
# plugin_permissions_shoot.sh — render the plugin permission model as
# one PNG. Demonstrates the full deny→grant→allow flow plus the audit
# log entry.
#
# Output: pages/screenshots/plugin-permissions-demo.png  (committed, gallery)
#         .shux/out/plugin-permissions-demo.png           (debug copy)

set -euo pipefail

SHUX="${SHUX_BIN:-shux}"
SESSION="perm-shoot"
PLUGIN_NAME="hello"
PLUGIN_SRC="examples/plugins/hello/plugin.sh"
OUT=".shux/out/plugin-permissions-demo.png"
FINAL="pages/screenshots/plugin-permissions-demo.png"

mkdir -p "$(dirname "$OUT")"

cleanup () {
    "$SHUX" plugin kill "$PLUGIN_NAME" >/dev/null 2>&1 || true
    "$SHUX" session kill "$SESSION" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM HUP

# Pre-clean
cleanup

# Install plugin so we have a target to grant + audit against.
"$SHUX" plugin install "./$PLUGIN_SRC" >/dev/null
sleep 0.5

# Build the snapshot session: one shell pane that runs the demo script.
"$SHUX" session create "$SESSION" -d -- bash --noprofile --norc -i >/dev/null
sleep 0.4

# Size the pane so the snapshot fits the full demo without scrollback.
SNAP_COLS=110
SNAP_ROWS=42
PANE_COLS=$((SNAP_COLS - 2))
PANE_ROWS=$((SNAP_ROWS - 3))
PID=$("$SHUX" --format json pane list -s "$SESSION" | jq -r '.[0].id')
"$SHUX" rpc call pane.set_size \
    --params "{\"pane_id\":\"$PID\",\"cols\":$PANE_COLS,\"rows\":$PANE_ROWS}" >/dev/null
sleep 0.3

# Single-line prompt; clear scrollback.
"$SHUX" pane send-keys -s "$SESSION" --text $'export PS1=\'$ \'\nclear\n' >/dev/null
sleep 0.3

DEMO_FILE="/tmp/perm-demo.sh"
cat >"$DEMO_FILE" <<EOF
#!/usr/bin/env bash
set -u
echo "── 1. default-deny ──"
echo "\$ shux plugin grants $PLUGIN_NAME"
$SHUX plugin grants $PLUGIN_NAME
echo
echo "── 2. grant a sensitive RPC ──"
echo "\$ shux plugin grant $PLUGIN_NAME pane.snapshot"
$SHUX plugin grant $PLUGIN_NAME pane.snapshot
echo "\$ shux plugin grant $PLUGIN_NAME pane.send_keys --target abc-123"
$SHUX plugin grant $PLUGIN_NAME pane.send_keys --target abc-123
echo
echo "── 3. show the resulting allow-set ──"
echo "\$ shux plugin grants $PLUGIN_NAME"
$SHUX plugin grants $PLUGIN_NAME
echo
echo "── 4. audit log (every plugin RPC frame) ──"
echo "\$ shux plugin audit $PLUGIN_NAME --tail 5"
$SHUX plugin audit $PLUGIN_NAME --tail 5
EOF
chmod +x "$DEMO_FILE"

SNAP_WINDOW=$("$SHUX" --format json window list -s "$SESSION" | jq -r '.[0].id')
"$SHUX" rpc call window.rename \
    --params "{\"id\":\"$SNAP_WINDOW\",\"name\":\"permissions\"}" >/dev/null

"$SHUX" pane send-keys -s "$SESSION" --text "bash $DEMO_FILE
" >/dev/null

sleep 4

"$SHUX" window snapshot -s "$SESSION" \
    --cols "$SNAP_COLS" --rows "$SNAP_ROWS" -o "$OUT" >/dev/null
cp "$OUT" "$FINAL"
"$SHUX" pane capture -s "$SESSION" --lines 80 > "${OUT%.png}.txt" 2>&1 || true

echo "→ $FINAL"
file "$FINAL"
