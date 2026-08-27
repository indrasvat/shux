#!/bin/bash
# $1=binary  $2=phase dir  $3=window label  $4=mode (single|split)
set -uo pipefail
B="$1"; D="/tmp/reel/$2"; LBL="$3"; MODE="${4:-single}"
mkdir -p "$D"; rm -f "$D"/*.png
export PATH="$HOME/.local/bin:$PATH"
unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy
export XDG_RUNTIME_DIR=$(mktemp -d /tmp/reel.XXXXXX)
$B session create --detached inner   >/dev/null 2>&1
$B session create --detached harness >/dev/null 2>&1
pid1(){ $B pane list --session "$1" --format json | python3 -c 'import sys,json
d=json.load(sys.stdin); ps=d if isinstance(d,list) else d["panes"]; print(ps[0]["id"])'; }
IP=$(pid1 inner); OP=$(pid1 harness)
IW=$($B window list --session inner --format json | python3 -c 'import sys,json
d=json.load(sys.stdin); ws=d if isinstance(d,list) else d["windows"]; print(ws[0]["id"])')
$B window rename --session inner --window "$IW" --name "$LBL" >/dev/null 2>&1
$B pane set-size --session harness --pane "$OP" --cols 116 --rows 38 >/dev/null 2>&1
if [ "$MODE" = "split" ]; then
  $B pane split --session inner --pane "$IP" --direction horizontal >/dev/null 2>&1; sleep 1
  mapfile -t PS < <($B pane list --session inner --format json | python3 -c 'import sys,json
d=json.load(sys.stdin); ps=d if isinstance(d,list) else d["panes"]
[print(x["id"]) for x in ps]')
  $B pane send-keys --session inner --pane "${PS[0]}" -t 'clear; terminal-browser open https://news.ycombinator.com/news
' >/dev/null 2>&1
  $B pane send-keys --session inner --pane "${PS[1]}" -t 'clear; terminal-browser open https://news.ycombinator.com/newest
' >/dev/null 2>&1
else
  $B pane send-keys --session inner --pane "$IP" -t 'clear; terminal-browser open https://news.ycombinator.com/news
' >/dev/null 2>&1
fi
i=0; snap(){ i=$((i+1)); printf -v n "%04d" $i; $B pane snapshot --session harness --pane "$OP" --output "$D/f$n.png" >/dev/null 2>&1; }
# show the attach command being typed, then run it
if [ "$MODE" = "split" ]; then sleep 46; else sleep 26; fi
$B pane send-keys --session harness --pane "$OP" -t "$B session attach inner" >/dev/null 2>&1
sleep 1; for _ in 1 2 3 4 5 6; do snap; done
$B pane send-keys --session harness --pane "$OP" -t '
' >/dev/null 2>&1
sleep 12
for _ in $(seq 1 14); do snap; sleep 0.15; done
$B daemon stop >/dev/null 2>&1 || true
sleep 0.5
echo "$2: $(ls $D/*.png 2>/dev/null | wc -l) frames"
