#!/bin/bash
# $1 = shux binary, $2 = output dir, $3 = label
set -uo pipefail
B="$1"; OUT="$2"; LBL="$3"
mkdir -p "$OUT"
export XDG_RUNTIME_DIR=$(mktemp -d /tmp/cp.XXXXXX)
pid_before=$(ls "$XDG_RUNTIME_DIR" 2>/dev/null | wc -l)

$B session create --detached inner  >/dev/null 2>&1
$B session create --detached harness >/dev/null 2>&1
pid_of(){ $B pane list --session "$1" --format json | python3 -c 'import sys,json
d=json.load(sys.stdin); ps=d if isinstance(d,list) else d["panes"]; print(ps[0]["id"])'; }
IP=$(pid_of inner); OP=$(pid_of harness)

$B pane set-size --session harness --pane "$OP" --cols 100 --rows 30 >/dev/null
# inner app: asks for mouse reports and echoes any it receives
$B pane send-keys --session inner --pane "$IP" -t 'python3 /tmp/mouse_echo.py
' >/dev/null
$B pane wait-for --session inner --pane "$IP" -t 'MOUSE_ECHO READY' --timeout-ms 10000 >/dev/null 2>&1 \
  || { echo "$LBL: echo app never started"; exit 1; }

# outer pane runs a REAL attach client against the inner session
$B pane send-keys --session harness --pane "$OP" -t "$B session attach inner
" >/dev/null
sleep 4
$B pane snapshot --session harness --pane "$OP" --output "$OUT/${LBL}_outer_attached.png" >/dev/null 2>&1

# a real user clicks: press then release, SGR, at outer cell (20, 8)
for seq in '\033[<0;20;8M' '\033[<0;20;8m' '\033[<0;25;9M' '\033[<0;25;9m'; do
  $B pane send-keys --session harness --pane "$OP" -t "$(printf "$seq")" >/dev/null 2>&1
  sleep 0.3
done
sleep 1.5
$B pane snapshot --session inner --pane "$IP" --output "$OUT/${LBL}_inner_afterclick.png" >/dev/null 2>&1
VIA_ATTACH=$($B pane capture --session inner --pane "$IP" 2>/dev/null | grep -c "GOT_REPORT")
# POSITIVE CONTROL: inject a report straight into the inner pane, bypassing attach.
# If this shows up but the attach clicks did not, the harness works and the gap is real.
$B pane send-keys --session inner --pane "$IP" -t "$(printf '\033[<0;7;3M')" >/dev/null 2>&1
sleep 1
DIRECT=$($B pane capture --session inner --pane "$IP" 2>/dev/null | grep -c "GOT_REPORT")
echo "via attach client : $VIA_ATTACH"
echo "direct injection  : $((DIRECT - VIA_ATTACH)) (positive control, must be >0)"
$B pane snapshot --session inner   --pane "$IP" --output "$OUT/${LBL}_inner.png" >/dev/null 2>&1
$B pane snapshot --session harness --pane "$OP" --output "$OUT/${LBL}_outer.png" >/dev/null 2>&1
TXT=$($B pane capture --session inner --pane "$IP" 2>/dev/null || $B pane glance --session inner --pane "$IP" 2>/dev/null)
echo "=== $LBL: inner pane text ==="
echo "$TXT" | grep -c "GOT_REPORT" | xargs -I{} echo "reports received: {}"
echo "$TXT" | grep "GOT_REPORT" | head -5
$B daemon stop >/dev/null 2>&1 || true
sleep 0.5
