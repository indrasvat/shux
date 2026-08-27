#!/bin/bash
set -uo pipefail
B="$1"; LBL="$2"; D=/tmp/kittyproof; mkdir -p "$D"
export PATH="$HOME/.local/bin:$PATH"
unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy
export XDG_RUNTIME_DIR=$(mktemp -d /tmp/kp.XXXXXX)
export DISPLAY=:99
Xvfb :99 -screen 0 1500x950x24 -nolisten tcp >/tmp/xvfb.log 2>&1 &
XV=$!
sleep 2
# inner shux session running the real browser
$B session create --detached inner >/dev/null 2>&1
IP=$($B pane list --session inner --format json | python3 -c 'import sys,json
d=json.load(sys.stdin); ps=d if isinstance(d,list) else d["panes"]; print(ps[0]["id"])')
$B pane send-keys --session inner --pane "$IP" -t 'clear; terminal-browser open https://news.ycombinator.com/news
' >/dev/null 2>&1
sleep 30
# a REAL GUI terminal - kitty, the reference implementation - attaches to it
LIBGL_ALWAYS_SOFTWARE=1 kitty --config NONE \
  -o font_family="DejaVu Sans Mono" -o font_size=11 -o remember_window_size=no \
  -o initial_window_width=1480 -o initial_window_height=920 \
  -e "$B" session attach inner >"$D/${LBL}_kitty.log" 2>&1 &
KP=$!
sleep 18
import -window root -display :99 "$D/${LBL}_screen.png" 2>/dev/null
echo "screenshot: $(stat -c%s "$D/${LBL}_screen.png" 2>/dev/null || echo MISSING) bytes"
kill $KP 2>/dev/null; sleep 1; kill -9 $KP 2>/dev/null
$B daemon stop >/dev/null 2>&1 || true
kill $XV 2>/dev/null; sleep 1; kill -9 $XV 2>/dev/null
echo done
