#!/usr/bin/env bash
#
# conductor_v0.1_shoot.sh — render the conductor v0.1 watchdog story
# as one PNG. Spawns a fake "claude" pane that goes through the
# thinking → idle states and snapshots the moment the title flips
# to ✓.
#
# Output: pages/screenshots/conductor-v0.1-demo.png  (committed, gallery)
#         .shux/out/conductor-v0.1-demo.png           (debug copy)

set -euo pipefail

SHUX="${SHUX_BIN:-shux}"
SESSION="conductor-shoot"
PLUGIN_NAME="conductor"
PLUGIN_SRC="examples/plugins/conductor/plugin.sh"
OUT=".shux/out/conductor-v0.1-demo.png"
FINAL="pages/screenshots/conductor-v0.1-demo.png"

mkdir -p "$(dirname "$OUT")"

# A fake `claude` binary: prints a splash, shows a "Working" spinner
# for ~3s, then settles to a prompt. Conductor's regex catches
# "Working" → thinking, then idle after SETTLE_MS of no change.
FAKE_AGENT_DIR="/tmp/conductor-shoot-bin"
mkdir -p "$FAKE_AGENT_DIR"
cat >"$FAKE_AGENT_DIR/claude" <<'EOF'
#!/usr/bin/env bash
echo "Claude Code v1.0.0"
echo
echo "Working ⠋"
sleep 3
clear
echo "claude > "
exec bash --noprofile --norc -i
EOF
chmod +x "$FAKE_AGENT_DIR/claude"

cleanup() {
    "$SHUX" plugin kill "$PLUGIN_NAME" >/dev/null 2>&1 || true
    "$SHUX" session kill "$SESSION" >/dev/null 2>&1 || true
    rm -rf "$FAKE_AGENT_DIR"
}
trap cleanup EXIT INT TERM HUP

# Pre-clean (also removes FAKE_AGENT_DIR — recreate below).
cleanup
sleep 0.3
mkdir -p "$FAKE_AGENT_DIR"
cat >"$FAKE_AGENT_DIR/claude" <<'EOF'
#!/usr/bin/env bash
echo "Claude Code v1.0.0"
echo
echo "Working ⠋"
sleep 3
clear
echo "claude > "
exec bash --noprofile --norc -i
EOF
chmod +x "$FAKE_AGENT_DIR/claude"

# Install conductor + grant the three sensitive RPCs it needs.
# The daemon inherits the conductor's env at spawn — but the daemon
# itself was likely started before this shoot, so a plain `export`
# here wouldn't reach conductor. Drop a wrapper that sets the env
# inline and install that instead. SETTLE_MS=1500 makes the
# ready→idle transition visible within the 14s snapshot window.
WRAP="$FAKE_AGENT_DIR/conductor-with-fast-settle.sh"
cat > "$WRAP" <<EOF
#!/usr/bin/env bash
exec env SHUX_CONDUCTOR_SETTLE_MS=1500 bash $(pwd)/$PLUGIN_SRC
EOF
chmod +x "$WRAP"
"$SHUX" plugin install "$WRAP" >/dev/null
sleep 0.5
"$SHUX" plugin grant "$PLUGIN_NAME" pane.capture   >/dev/null
"$SHUX" plugin grant "$PLUGIN_NAME" pane.set_title >/dev/null
"$SHUX" plugin grant "$PLUGIN_NAME" pane.send_keys >/dev/null

# Spawn a session running the fake claude binary by absolute path.
# Path-prepending PATH doesn't help — the daemon's spawn uses the
# daemon's environment, not the CLI's. Absolute path bypasses lookup
# entirely; conductor's agent_for_command does basename($command[0])
# so "/tmp/.../claude" still matches the "claude" agent.
"$SHUX" session create "$SESSION" -d -- "$FAKE_AGENT_DIR/claude" >/dev/null
sleep 0.4

# Force a pane split so the compositor renders pane borders (single-
# pane sessions suppress borders, hiding the title overlay we want
# to capture). The split spawns the user's default shell; we then
# send a label via pane.send_keys so the right side looks intentional.
PID=$("$SHUX" --format json pane list -s "$SESSION" | jq -r '.[0].id')
SPLIT_OUT=$("$SHUX" --format json pane split -s "$SESSION" -d horizontal -p "$PID")
RIGHT_PID=$(echo "$SPLIT_OUT" | jq -r '.pane.id')
sleep 0.4
"$SHUX" pane send-keys -s "$SESSION" -p "$RIGHT_PID" --text $'clear; echo "(showcase pane)"; echo "border title comes from conductor on the left"\n' >/dev/null
sleep 0.4

# Wait for: Working spinner (3s) + clear (~0) + SETTLE_MS (5s) +
# one more poll tick (2s) + buffer for event delivery + poll cadence.
sleep 14

# Snapshot the WINDOW (renders both panes + borders + title overlays).
SNAP_COLS=110
SNAP_ROWS=34
"$SHUX" window snapshot -s "$SESSION" \
    --cols "$SNAP_COLS" --rows "$SNAP_ROWS" -o "$OUT" >/dev/null
cp "$OUT" "$FINAL"

# Capture the audit log too — proves the polls actually fired and
# went through the permission model.
"$SHUX" plugin audit "$PLUGIN_NAME" --tail 8 > "${OUT%.png}-audit.txt" 2>&1 || true

echo "→ $FINAL"
file "$FINAL"
