#!/bin/bash
# $1 = shux binary, $2 = out dir, $3 = label
set -uo pipefail
B="$1"; OUT="$2"; LBL="$3"
mkdir -p "$OUT"
export PATH="$HOME/.local/bin:$PATH"
unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy
export XDG_RUNTIME_DIR=$(mktemp -d /tmp/tbc.XXXXXX)

$B session create --detached inner   >/dev/null 2>&1
$B session create --detached harness >/dev/null 2>&1
pid_of(){ $B pane list --session "$1" --format json | python3 -c 'import sys,json
d=json.load(sys.stdin); ps=d if isinstance(d,list) else d["panes"]; print(ps[0]["id"])'; }
IP=$(pid_of inner); OP=$(pid_of harness)

# outer pane 112x36 -> attach chrome leaves the inner pane at 110x34
$B pane set-size --session harness --pane "$OP" --cols 112 --rows 36 >/dev/null
$B pane send-keys --session inner --pane "$IP" -t 'clear; terminal-browser open https://news.ycombinator.com/news
' >/dev/null
sleep 32
$B pane snapshot --session inner --pane "$IP" --output "$OUT/${LBL}_1_inner_before.png" >/dev/null 2>&1

# a real attach client, in a real pane
$B pane send-keys --session harness --pane "$OP" -t "$B session attach inner
" >/dev/null
sleep 6
$B pane snapshot --session harness --pane "$OP" --output "$OUT/${LBL}_2_attached.png" >/dev/null 2>&1

# THE CLICK: story 1's title sits at inner cell (23,5); +1 for the attach border
for seq in '\033[<0;24;6M' '\033[<0;24;6m'; do
  $B pane send-keys --session harness --pane "$OP" -t "$(printf "$seq")" >/dev/null 2>&1
  sleep 0.4
done
sleep 10
$B pane snapshot --session inner   --pane "$IP" --output "$OUT/${LBL}_3_inner_after.png" >/dev/null 2>&1
$B pane snapshot --session harness --pane "$OP" --output "$OUT/${LBL}_4_outer_after.png" >/dev/null 2>&1
python3 -c "
import hashlib
a=hashlib.md5(open('$OUT/${LBL}_1_inner_before.png','rb').read()).hexdigest()[:8]
b=hashlib.md5(open('$OUT/${LBL}_3_inner_after.png','rb').read()).hexdigest()[:8]
print('$LBL inner before',a,'| after',b,'|', 'CHANGED' if a!=b else 'IDENTICAL')"
$B daemon stop >/dev/null 2>&1 || true
sleep 0.5
