#!/bin/bash
# $1 = binary, $2 = out dir label
set -uo pipefail
B="$1"; D="/tmp/sync2/$2"; mkdir -p "$D"; rm -f "$D"/*.png
export PATH="$HOME/.local/bin:$PATH"
unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy
export XDG_RUNTIME_DIR=$(mktemp -d /tmp/sy.XXXXXX)
$B session create --detached inner   >/dev/null 2>&1
$B session create --detached harness >/dev/null 2>&1
p1(){ $B pane list --session "$1" --format json | python3 -c 'import sys,json
d=json.load(sys.stdin); ps=d if isinstance(d,list) else d["panes"]; print(ps[0]["id"])'; }
IP=$(p1 inner); OP=$(p1 harness)
IW=$($B window list --session inner --format json | python3 -c 'import sys,json
d=json.load(sys.stdin); ws=d if isinstance(d,list) else d["windows"]; print(ws[0]["id"])')
$B pane set-size --session harness --pane "$OP" --cols 116 --rows 38 >/dev/null 2>&1
$B pane send-keys --session inner --pane "$IP" -t 'clear; terminal-browser open http://127.0.0.1:8731/
' >/dev/null 2>&1
sleep 26
$B pane send-keys --session harness --pane "$OP" -t "$B session attach inner
" >/dev/null 2>&1
sleep 10
i=0
phase(){ $B window rename --session inner --window "$IW" --name "$1" >/dev/null 2>&1; }
pair(){ i=$((i+1)); printf -v n "%04d" $i
  $B pane snapshot --session inner   --pane "$IP" --output "$D/t$n.png" >/dev/null 2>&1
  $B pane snapshot --session harness --pane "$OP" --output "$D/a$n.png" >/dev/null 2>&1
}
# A. same geometry, content ticking -- THE bug
phase "A-same-size-repaint-geometry-unchanged"
for _ in $(seq 1 7); do pair; sleep 1.1; done
# B. grow
phase "B-TERMINAL-resized-BIGGER"
$B pane set-size --session harness --pane "$OP" --cols 132 --rows 42 >/dev/null 2>&1
for _ in $(seq 1 6); do pair; sleep 1.1; done
# C. shrink
phase "C-TERMINAL-resized-SMALLER"
$B pane set-size --session harness --pane "$OP" --cols 92 --rows 28 >/dev/null 2>&1
for _ in $(seq 1 6); do pair; sleep 1.1; done
# D. restore
phase "D-TERMINAL-resized-BACK"
$B pane set-size --session harness --pane "$OP" --cols 116 --rows 38 >/dev/null 2>&1
for _ in $(seq 1 6); do pair; sleep 1.1; done
$B daemon stop >/dev/null 2>&1 || true
sleep 0.5
echo "$2: $(ls $D/t*.png | wc -l) pairs"
