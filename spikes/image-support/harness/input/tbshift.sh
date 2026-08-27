#!/bin/bash
set -uo pipefail
B=/tmp/shux-shift; OUT=/tmp/tbshiftframes; mkdir -p "$OUT"; rm -f "$OUT"/*.png
export PATH="$HOME/.local/bin:$PATH"
unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy
export XDG_RUNTIME_DIR=$(mktemp -d /tmp/ts.XXXXXX)
$B session create --detached inner   >/dev/null 2>&1
$B session create --detached harness >/dev/null 2>&1
pid_of(){ $B pane list --session "$1" --format json | python3 -c 'import sys,json
d=json.load(sys.stdin); ps=d if isinstance(d,list) else d["panes"]; print(ps[0]["id"])'; }
wid_of(){ $B window list --session "$1" --format json | python3 -c 'import sys,json
d=json.load(sys.stdin); ws=d if isinstance(d,list) else d["windows"]; print(ws[0]["id"])'; }
IP=$(pid_of inner); OP=$(pid_of harness); IW=$(wid_of inner)
$B pane set-size --session harness --pane "$OP" --cols 112 --rows 36 >/dev/null
i=0
snap(){ i=$((i+1)); printf -v n "%04d" $i
  $B pane snapshot --session harness --pane "$OP" --output "$OUT/o$n.png" >/dev/null 2>&1
  $B pane snapshot --session inner   --pane "$IP" --output "$OUT/i$n.png" >/dev/null 2>&1; }
hold(){ for _ in $(seq 1 $1); do snap; done; }
phase(){ $B window rename --session inner --window "$IW" --name "$1" >/dev/null 2>&1; sleep 0.6; }

phase "1-loading-hacker-news"
$B pane send-keys --session inner --pane "$IP" -t 'clear; terminal-browser open https://news.ycombinator.com/news
' >/dev/null
sleep 32
# real attach client, typed on screen first
$B pane send-keys --session harness --pane "$OP" -t "$B session attach inner" >/dev/null
sleep 1.2; hold 5
$B pane send-keys --session harness --pane "$OP" -t '
' >/dev/null
sleep 5; hold 6

click(){ # $1 SGR button (0=left, 4=left+Shift), $2 col, $3 row
  $B pane send-keys --session harness --pane "$OP" -t "$(printf "\033[<$1;$2;$3M")" >/dev/null 2>&1; sleep 0.3
  $B pane send-keys --session harness --pane "$OP" -t "$(printf "\033[<$1;$2;$3m")" >/dev/null 2>&1
}
# HN's header nav "comments" is at a fixed spot: inner cell (34,3); +1 for the
# attach border => outer (35,4). Same-origin, so it is always reachable.
phase "2-SHIFT+CLICK-on-the-comments-link"; hold 5
click 4 35 4
sleep 9; hold 10
phase "3-still-on-HN-shux-kept-the-click"; hold 12
phase "4-PLAIN-CLICK-same-link"; hold 5
click 0 35 4
sleep 11; hold 12
phase "5-NAVIGATED-app-got-the-click"; hold 14
$B daemon stop >/dev/null 2>&1 || true
sleep 0.5
echo "frames: $(ls $OUT/o*.png | wc -l) pairs"
