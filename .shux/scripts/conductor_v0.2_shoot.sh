#!/usr/bin/env bash
#
# conductor_v0.2_shoot.sh — settle-snapshot archive demo.
# Spawns three fake "claude" panes side-by-side, lets them settle,
# captures one combined snapshot of (1) the live multi-pane window
# showing all three borders flipped to `claude · ✓`, and (2) the
# resulting INDEX.tsv contents rendered into the bottom pane.
#
# Output:
#   pages/screenshots/conductor-v0.2-settle-archive.png
#   .shux/out/conductor-v0.2-settle-archive.png
#   plus one .shux/conductor/snapshots/<agent>-<short>-<ts>.png per
#   agent pane (committed-as-is to demonstrate the artifact format).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
source "${REPO_ROOT}/.shux/scripts/lib/shux_harness.sh"
SHUX="${SHUX_BIN:-shux}"
SESSION="conductor-v0.2-shoot"
PLUGIN_NAME="conductor"
PLUGIN_SRC="examples/plugins/conductor/plugin.sh"
OUT=".shux/out/conductor-v0.2-settle-archive.png"
FINAL="pages/screenshots/conductor-v0.2-settle-archive.png"
SNAPSHOT_DIR_OUT=".shux/out/conductor-v0.2-snapshots"
RUNTIME_DIR="${SHUX_RUNTIME_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/shux-conductor-v02.XXXXXX")}"
export XDG_RUNTIME_DIR="${RUNTIME_DIR}"

mkdir -p "$(dirname "$OUT")" "$SNAPSHOT_DIR_OUT"

FAKE_AGENT_DIR="/tmp/conductor-v0.2-shoot"
cleanup() {
    shux_harness_kill_plugin "$RUNTIME_DIR" "$SHUX" "$PLUGIN_NAME"
    shux_harness_cleanup_runtime "$RUNTIME_DIR" "$SHUX" "${SESSION:-}" "${SESSION_C:-}" "${SESSION_X:-}" "${SESSION_O:-}"
    rm -f "$FAKE_AGENT_DIR"/* "/tmp/conductor-v0.2-wrap.sh" 2>/dev/null || true
    rmdir "$FAKE_AGENT_DIR" 2>/dev/null || true
}
trap cleanup EXIT INT TERM HUP

cleanup
sleep 0.3
mkdir -p "$FAKE_AGENT_DIR"

# Three near-identical fake agents — different greetings so their
# settled snapshots are visually distinct.
for name in claude codex opencode; do
    cat > "$FAKE_AGENT_DIR/$name" <<EOF
#!/usr/bin/env bash
echo "$name v1.0.0"
echo
echo "ready · awaiting prompt"
echo "$name > "
sleep 60
EOF
    chmod +x "$FAKE_AGENT_DIR/$name"
done

# Wrap conductor with a short SETTLE_MS + dedicated snapshot dir.
cat > /tmp/conductor-v0.2-wrap.sh <<EOF
#!/usr/bin/env bash
exec env SHUX_CONDUCTOR_SETTLE_MS=1500 \
    SHUX_CONDUCTOR_SNAPSHOT_DIR=$SNAPSHOT_DIR_OUT \
    SHUX_CONDUCTOR_NOTIFY=stdout \
    bash $(pwd)/$PLUGIN_SRC
EOF
chmod +x /tmp/conductor-v0.2-wrap.sh

"$SHUX" plugin install /tmp/conductor-v0.2-wrap.sh >/dev/null
sleep 0.5
"$SHUX" plugin grant "$PLUGIN_NAME" pane.capture   >/dev/null
"$SHUX" plugin grant "$PLUGIN_NAME" pane.set_title >/dev/null
"$SHUX" plugin grant "$PLUGIN_NAME" pane.send_keys >/dev/null
"$SHUX" plugin grant "$PLUGIN_NAME" pane.snapshot  >/dev/null

# Spawn three SEPARATE sessions, each with a known-agent command as
# command[0]. Conductor only tracks panes whose initial command
# matches a known agent prefix, so spawning via `pane send-keys
# exec ...` from a shell wouldn't get them tracked (the daemon
# records `bash`, not the post-exec command).
SESSION_C="$SESSION-claude"
SESSION_X="$SESSION-codex"
SESSION_O="$SESSION-opencode"
"$SHUX" session create "$SESSION_C" -d -- "$FAKE_AGENT_DIR/claude"   >/dev/null
"$SHUX" session create "$SESSION_X" -d -- "$FAKE_AGENT_DIR/codex"    >/dev/null
"$SHUX" session create "$SESSION_O" -d -- "$FAKE_AGENT_DIR/opencode" >/dev/null
sleep 0.5

# A FOURTH session for the snapshot canvas: split into 4 panes so we
# can render (top) one of the live agent panes via repeated capture
# AND (bottom) tail the INDEX.tsv so the artifact shape is visible
# in the proof PNG.
"$SHUX" session create "$SESSION" -d -- bash --noprofile --norc -i >/dev/null
sleep 0.4
SHELL_PID=$("$SHUX" --format json pane list -s "$SESSION" | jq -r '.[0].id')
SPLIT=$("$SHUX" --format json pane split -s "$SESSION" -d horizontal -p "$SHELL_PID")
INDEX_PID=$(echo "$SPLIT" | jq -r '.pane.id')

# Top pane: live capture of one of the agent panes — make it visible
# that the agent's view is being archived.
"$SHUX" pane send-keys -s "$SESSION" -p "$SHELL_PID" --text "
clear
echo '── shux-conductor v0.2 — settle-snapshot archive ──'
echo
echo 'three known-agent sessions tracked:'
$SHUX session list 2>&1 | grep -E 'conductor-v0.2-shoot-' | sed 's/^/  /'
echo
sleep 7
echo
echo 'ls -1 $SNAPSHOT_DIR_OUT (after settle):'
ls -1 $SNAPSHOT_DIR_OUT/ | sed 's/^/  /'
" >/dev/null

# Bottom pane: tail INDEX.tsv with column alignment so the row shape
# is legible in the snapshot.
"$SHUX" pane send-keys -s "$SESSION" -p "$INDEX_PID" --text "
clear
echo '── INDEX.tsv (one row per settle) ──'
sleep 8
column -t -s '\t' $SNAPSHOT_DIR_OUT/INDEX.tsv 2>/dev/null \
    || cat $SNAPSHOT_DIR_OUT/INDEX.tsv
" >/dev/null

# Wait for all three agent panes to settle + snapshot + the bottom
# pane's `cat INDEX.tsv` to fire after the sleeps complete.
sleep 14

SNAP_COLS=140
SNAP_ROWS=42
"$SHUX" window snapshot -s "$SESSION" \
    --cols "$SNAP_COLS" --rows "$SNAP_ROWS" -o "$OUT" >/dev/null
cp "$OUT" "$FINAL"

cp "$SNAPSHOT_DIR_OUT/INDEX.tsv" ".shux/out/conductor-v0.2-INDEX.tsv" 2>/dev/null || true

# Make sure the agent sessions are torn down too.
shux_harness_kill_session "$RUNTIME_DIR" "$SHUX" "$SESSION_C"
shux_harness_kill_session "$RUNTIME_DIR" "$SHUX" "$SESSION_X"
shux_harness_kill_session "$RUNTIME_DIR" "$SHUX" "$SESSION_O"

echo "→ $FINAL"
echo "→ snapshots in $SNAPSHOT_DIR_OUT"
ls -1 "$SNAPSHOT_DIR_OUT"
file "$FINAL"
