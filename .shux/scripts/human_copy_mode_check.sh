#!/usr/bin/env bash
# Focused copy-mode regression + dogfood capture for human-interactive work.
#
# Outputs:
#   .shux/out/human-copy-mode.png
#   .shux/out/human-copy-mode.txt
#   .shux/out/human-copy-mode-attach.log
#   .shux/out/human-copy-mode-idle-bytes.txt

set -euo pipefail

SHUX="${SHUX_BIN:-target/debug/shux}"
SESSION="${SESSION:-human-copy-mode-$$}"
OUT_DIR="${OUT_DIR:-.shux/out}"
PNG="$OUT_DIR/human-copy-mode.png"
TXT="$OUT_DIR/human-copy-mode.txt"
ATTACH_LOG="$OUT_DIR/human-copy-mode-attach.log"
CLEAN_ATTACH_LOG="$OUT_DIR/human-copy-mode-attach.clean.txt"
IDLE_BYTES="$OUT_DIR/human-copy-mode-idle-bytes.txt"
RUNTIME_DIR="${SHUX_RUNTIME_DIR:-/tmp/shux-human-copy-mode-$$}"
CONFIG_HOME="$RUNTIME_DIR/config"
MAKE_BIN="${MAKE:-make}"

mkdir -p "$OUT_DIR"
mkdir -p "$RUNTIME_DIR"
mkdir -p "$CONFIG_HOME/shux"
export XDG_RUNTIME_DIR="$RUNTIME_DIR"
export XDG_CONFIG_HOME="$CONFIG_HOME"
cat > "$CONFIG_HOME/shux/config.toml" <<'CONFIG'
[keys]
prefix = "ctrl-b"
CONFIG
export SESSION
export ATTACH_LOG
export IDLE_BYTES

if [[ ! -x "$SHUX" ]]; then
    "$MAKE_BIN" build
fi
if [[ "$SHUX" != /* ]]; then
    SHUX="$(cd "$(dirname "$SHUX")" && pwd)/$(basename "$SHUX")"
fi
export SHUX_BIN="$SHUX"
shux_cmd() {
    "$SHUX" "$@"
}

trap 'shux_cmd session kill "$SESSION" >/dev/null 2>&1 || true' EXIT
shux_cmd session kill "$SESSION" >/dev/null 2>&1 || true

echo "==> unit coverage: scrollback copy-mode navigation/search/render"
"$MAKE_BIN" test-copy-mode

echo "==> dogfood: spawn scrollback-heavy pane"
shux_cmd --format json session create "$SESSION" -d --title copy-mode-check -- \
    bash -lc 'for i in $(seq -w 0 500); do printf "copy-line-%s  searchable payload %s\n" "$i" "$i"; done; printf "\033[38;2;116;199;236mcolor-probe\033[0m TERM=%s COLORTERM=%s NO_COLOR=%s\n" "$TERM" "${COLORTERM-unset}" "${NO_COLOR-unset}"; sleep 9000' \
    >/dev/null

shux_cmd pane set-size -s "$SESSION" --cols 96 --rows 18 >/dev/null
shux_cmd pane wait-for -s "$SESSION" -t copy-line-500 --timeout-ms 5000 >/dev/null
shux_cmd pane capture -s "$SESSION" --lines 520 > "$TXT"
grep -q 'copy-line-500' "$TXT"
grep -q 'NO_COLOR=unset' "$TXT"

shux_cmd pane snapshot -s "$SESSION" -o "$PNG" >/dev/null
head -c 8 "$PNG" | od -A n -t x1 | tr -d ' \n' | grep -q '89504e470d0a1a0a'

echo "==> dogfood: attach, enter copy mode, select text, and idle-check repaint volume"
rm -f "$ATTACH_LOG" "$CLEAN_ATTACH_LOG" "$IDLE_BYTES"
expect >/dev/null <<'EXPECT'
proc drain_ms {milliseconds} {
    set deadline [expr {[clock milliseconds] + $milliseconds}]
    while {[clock milliseconds] < $deadline} {
        set timeout 1
        expect {
            -re {(.|\r|\n)+} {}
            timeout { after 25 }
            eof { return }
        }
    }
}

proc sgr_mouse {code x y suffix} {
    send -- "\033\[<${code};${x};${y}${suffix}"
}

log_user 1
log_file -noappend $env(ATTACH_LOG)
spawn env TERM=xterm-256color COLORTERM=truecolor sh -c "unset NO_COLOR; stty rows 24 columns 100; exec $env(SHUX_BIN) session attach $env(SESSION)"
drain_ms 1000
send -- "\x02\["
drain_ms 500
send "/copy-line-49\r"
drain_ms 700
send "vjjj"
drain_ms 500
set before [file size $env(ATTACH_LOG)]
drain_ms 1200
set after [file size $env(ATTACH_LOG)]
set delta [expr {$after - $before}]
set fd [open $env(IDLE_BYTES) w]
puts $fd $delta
close $fd
send "q"
drain_ms 300

# Normal-mode mouse UX: left-drag visible text, release to copy via OSC52,
# right-click the still-visible selection, and click Copy from the inline menu.
sgr_mouse 0 2 2 M
drain_ms 120
sgr_mouse 32 32 2 M
drain_ms 120
sgr_mouse 0 32 2 m
drain_ms 500
sgr_mouse 2 6 2 M
drain_ms 250
sgr_mouse 2 6 2 m
drain_ms 150
sgr_mouse 0 6 2 M
drain_ms 250
sgr_mouse 0 6 2 m
drain_ms 400

send -- "\x02d"
after 300
log_file
catch {close}
catch {wait}
EXPECT

IDLE_DELTA="$(cat "$IDLE_BYTES")"
if [[ "$IDLE_DELTA" -gt 15000 ]]; then
    echo "copy-mode idle repaint volume too high: ${IDLE_DELTA} bytes" >&2
    exit 1
fi
perl -0pe 's/\e\[[0-?]*[ -\/]*[@-~]//g' "$ATTACH_LOG" > "$CLEAN_ATTACH_LOG"
grep -q 'copy-line-49' "$CLEAN_ATTACH_LOG"
grep -q ' Copy' "$CLEAN_ATTACH_LOG"
grep -q ' Clear' "$CLEAN_ATTACH_LOG"
perl -0ne 'exit(/\e\[48;2;116;199;236m/ && /\e\[38;2;30;32;48m/ ? 0 : 1)' "$ATTACH_LOG"
perl -0ne 'exit(/\e\]52;c;/ ? 0 : 1)' "$ATTACH_LOG"

echo "✓ copy-mode dogfood captured: $PNG; idle attach delta ${IDLE_DELTA} bytes"
