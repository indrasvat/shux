#!/bin/bash
set -uo pipefail
B=/tmp/shux-shift; OUT=/tmp/tbs2; mkdir -p "$OUT"; rm -f "$OUT"/*.png
export PATH="$HOME/.local/bin:$PATH"
unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy
export XDG_RUNTIME_DIR=$(mktemp -d /tmp/t2.XXXXXX)
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
phase(){ $B window rename --session inner --window "$IW" --name "$1" >/dev/null 2>&1; sleep 0.5; }
send(){ $B pane send-keys --session harness --pane "$OP" -t "$(printf "$1")" >/dev/null 2>&1; }

phase "0-loading-hacker-news"
$B pane send-keys --session inner --pane "$IP" -t 'clear; terminal-browser open https://news.ycombinator.com/news
' >/dev/null
sleep 32
$B pane send-keys --session harness --pane "$OP" -t "$B session attach inner" >/dev/null
sleep 1.2; hold 5
$B pane send-keys --session harness --pane "$OP" -t '
' >/dev/null
sleep 5; hold 5

# ---- PHASE 1: SHIFT+DRAG -> shux paints ITS OWN selection; browser must not move
phase "1-SHIFT+DRAG--watch-shux-paint-a-selection"; hold 5
send '\033[<4;12;10M';  sleep 0.35; hold 2
send '\033[<36;30;10M'; sleep 0.35; hold 2
send '\033[<36;55;10M'; sleep 0.35; hold 3
send '\033[<36;80;10M'; sleep 0.35; hold 4
send '\033[<4;80;10m';  sleep 1.0;  hold 6
phase "2-shux-got-it--browser-did-NOT-move"; hold 12

# ---- PHASE 2: PLAIN CLICK on HN's header "comments" link -> browser must navigate
phase "3-PLAIN-CLICK-on-the-comments-link"; hold 6
send '\033[<0;35;4M'; sleep 0.3
send '\033[<0;35;4m'; sleep 11; hold 12
phase "4-app-got-it--browser-NAVIGATED"; hold 14
$B daemon stop >/dev/null 2>&1 || true
sleep 0.5
echo "frames: $(ls $OUT/o*.png | wc -l) pairs"
