#!/bin/bash
set -uo pipefail
B="$1"; LBL="$2"; D="/tmp/kvid/$LBL"; mkdir -p "$D"; rm -f "$D"/*.png
export PATH="$HOME/.local/bin:$PATH"
unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy
export SHUX_SPIKE_WIRE_LOG=/tmp/wire.log
export XDG_RUNTIME_DIR=$(mktemp -d /tmp/kv.XXXXXX)
export DISPLAY=:99
Xvfb :99 -screen 0 1500x950x24 -nolisten tcp >/dev/null 2>&1 & XV=$!
sleep 2
$B session create --detached inner >/dev/null 2>&1
IP=$($B pane list --session inner --format json | python3 -c 'import sys,json
d=json.load(sys.stdin); ps=d if isinstance(d,list) else d["panes"]; print(ps[0]["id"])')
$B pane send-keys --session inner --pane "$IP" -t 'clear; terminal-browser open http://127.0.0.1:8731/
' >/dev/null 2>&1
sleep 26
LIBGL_ALWAYS_SOFTWARE=1 kitty --config NONE \
  -o font_family="DejaVu Sans Mono" -o font_size=11 -o remember_window_size=no \
  -o initial_window_width=1180 -o initial_window_height=760 \
  -e "$B" session attach inner >/dev/null 2>&1 & KP=$!
sleep 16
WID=$(xdotool search --class kitty | head -1)
echo "kitty window id: ${WID:-NONE}"
i=0
shot(){ i=$((i+1)); printf -v n "%04d" $i; import -window root -display :99 "$D/f$n.png" 2>/dev/null; }
# A: same size, content ticking
for _ in $(seq 1 7); do shot; sleep 1.1; done
# B: grow the kitty WINDOW
[ -n "$WID" ] && xdotool windowsize "$WID" 1420 900
for _ in $(seq 1 6); do shot; sleep 1.1; done
# C: shrink it
[ -n "$WID" ] && xdotool windowsize "$WID" 900 560
for _ in $(seq 1 6); do shot; sleep 1.1; done
# D: back
[ -n "$WID" ] && xdotool windowsize "$WID" 1180 760
for _ in $(seq 1 6); do shot; sleep 1.1; done
kill $KP 2>/dev/null; sleep 1; kill -9 $KP 2>/dev/null
$B daemon stop >/dev/null 2>&1 || true
kill $XV 2>/dev/null; sleep 1; kill -9 $XV 2>/dev/null
echo "$LBL: $(ls $D/*.png | wc -l) frames"
