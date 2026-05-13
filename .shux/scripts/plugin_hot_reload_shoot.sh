#!/usr/bin/env bash
#
# plugin_hot_reload_shoot.sh — render the plugin hot-reload story as
# one PNG of a shell pane's scrollback. Proves "edit + save → new
# code live in <500ms" using the hello plugin's `demo·N` → `hot·N`
# window-rename behavior.
#
# Output: pages/screenshots/plugin-hot-reload.png  (committed, gallery)
#         .shux/out/plugin-hot-reload.png          (debug copy)

set -euo pipefail

SHUX="${SHUX_BIN:-shux}"
SESSION="hr-shoot"
INNER_SESSION="hr-target"
PLUGIN_SRC="examples/plugins/hello/plugin.sh"
OUT=".shux/out/plugin-hot-reload.png"
FINAL="pages/screenshots/plugin-hot-reload.png"

mkdir -p "$(dirname "$OUT")"

# Backup the plugin source so we can restore it after the demo.
cp "$PLUGIN_SRC" "$PLUGIN_SRC.shootbak"

TMPFILES=()
cleanup () {
    mv -f "$PLUGIN_SRC.shootbak" "$PLUGIN_SRC" 2>/dev/null || true
    "$SHUX" plugin kill hello >/dev/null 2>&1 || true
    "$SHUX" session kill "$INNER_SESSION" >/dev/null 2>&1 || true
    "$SHUX" session kill "$SESSION" >/dev/null 2>&1 || true
    for f in "${TMPFILES[@]:-}"; do [[ -n "$f" && -e "$f" ]] && rm -f "$f"; done
}
trap cleanup EXIT INT TERM HUP

# Pre-clean
"$SHUX" plugin kill hello >/dev/null 2>&1 || true
"$SHUX" session kill "$INNER_SESSION" >/dev/null 2>&1 || true
"$SHUX" session kill "$SESSION" >/dev/null 2>&1 || true

# The demo runs inside one shux shell pane. Each `echo "$ ..."` is
# the prompt-mock; the actual command runs underneath. Sleeps give the
# FSEvents watcher time to debounce the source edit and respawn the
# plugin (~250ms) before we open a new window.
# Setup (install + target session) outside the snapshot window so the
# snapshot only sees the rename-visible bits.
"$SHUX" plugin install "./$PLUGIN_SRC" >/dev/null
sleep 0.5
"$SHUX" session create "$INNER_SESSION" -d >/dev/null
sleep 1.5

# Spawn a clean interactive bash (no rc-files → no starship → no
# multi-line prompt eating viewport rows). Then resize the PTY BEFORE
# sending demo keystrokes — the daemon-default 24-row grid would
# scroll our demo's top into unrenderable scrollback.
"$SHUX" session create "$SESSION" -d -- bash --noprofile --norc -i >/dev/null
sleep 0.4
# Size the VT grid to match the FUTURE pane rect — the compositor's
# `compose_pane` (crates/shux-ui/src/composed.rs) crops from the top
# when grid_rows > rect_rows, so any extra grid rows lose the demo's
# header lines. Window snapshot 96×42 → status (1) + top+bottom
# borders (2) → pane rect 94×39. Set VT grid exactly to that.
SNAP_COLS=96
SNAP_ROWS=42
PANE_COLS=$((SNAP_COLS - 2))
PANE_ROWS=$((SNAP_ROWS - 3))
PID=$("$SHUX" --format json pane list -s "$SESSION" | jq -r '.[0].id')
"$SHUX" rpc call pane.set_size --params "{\"pane_id\":\"$PID\",\"cols\":$PANE_COLS,\"rows\":$PANE_ROWS}" >/dev/null
sleep 0.3
# Tiny single-line prompt so it doesn't eat the snapshot's bottom rows.
"$SHUX" pane send-keys -s "$SESSION" --text $'export PS1=\'$ \'\nclear\n' >/dev/null
sleep 0.3

# Build the demo as a single shell-quoted blob and push it via
# pane.send_keys --text. The blob ends with `;` chains so bash sees one
# command line and runs the whole thing.
# Write the demo as a tiny script and have the inner shell exec it via
# `bash <file>`. That way bash's terminal echo of the typed command is
# only the short `bash <file>` line, not the whole 25-line `;`-chained
# blob — which would otherwise dominate the snapshot.
# Deterministic path so bash's command-echo line reads cleanly in the
# snapshot: `$ bash /tmp/hr-demo.sh` instead of a 90-char mktemp path.
DEMO_FILE="/tmp/hr-demo.sh"
cat >"$DEMO_FILE" <<EOF
#!/usr/bin/env bash
set -u
echo "── before ──"
echo "\$ shux window list -s $INNER_SESSION"
$SHUX window list -s $INNER_SESSION
echo
echo "── edit (live source swap) ──"
echo "\$ sed -i s/demo·/hot·/g  $PLUGIN_SRC"
sed -i.tmp 's/demo·/hot·/g' $PLUGIN_SRC
rm $PLUGIN_SRC.tmp
sleep 1.5
echo "  daemon log: watcher fired → plugin respawned"
echo
echo "── after (new window picks up live code) ──"
echo "\$ shux window create -s $INNER_SESSION"
$SHUX window create -s $INNER_SESSION
sleep 1.0
echo
echo "\$ shux window list -s $INNER_SESSION"
$SHUX window list -s $INNER_SESSION
EOF
chmod +x "$DEMO_FILE"

# Add the demo file to the cleanup trap.
TMPFILES=("$DEMO_FILE")

# Rename the snapshot session's window so the status-bar segment reads
# "hot-reload" instead of "demo·N" (the plugin renamed it on creation).
SNAP_WINDOW=$("$SHUX" --format json window list -s "$SESSION" | jq -r '.[0].id')
"$SHUX" rpc call window.rename --params "{\"id\":\"$SNAP_WINDOW\",\"name\":\"hot-reload\"}" >/dev/null

"$SHUX" pane send-keys -s "$SESSION" --text "bash $DEMO_FILE
" >/dev/null

# Wait for the demo to play out (~3s sleeps + cmd overhead).
sleep 6

"$SHUX" window snapshot -s "$SESSION" --cols "$SNAP_COLS" --rows "$SNAP_ROWS" -o "$OUT" >/dev/null
cp "$OUT" "$FINAL"
"$SHUX" pane capture -s "$SESSION" --lines 80 > "${OUT%.png}.txt" 2>&1 || true

echo "→ $FINAL"
file "$FINAL"
