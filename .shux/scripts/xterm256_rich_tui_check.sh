#!/usr/bin/env bash
# Rich TUI visual proof for xterm-256color response support.
#
# Outputs:
#   .shux/out/xterm256-rich-tui-<timestamp>/*.png
#   .shux/out/xterm256-rich-tui-<timestamp>/*.txt
#   .shux/out/xterm256-rich-tui-<timestamp>/contact-sheet.png
#
# Set EXPECT_VIVECAKA_PRS=1 when this branch has an open PR; the script then
# fails if `vivecaka --repo=indrasvat/shux` renders the empty PR list.

set -euo pipefail

SHUX="${SHUX_BIN:-target/release/shux}"
MAKE_BIN="${MAKE:-make}"
STAMP="$(date +%Y%m%d-%H%M%S)"
OUT_DIR="${OUT_DIR:-.shux/out/xterm256-rich-tui-$STAMP}"
SESSION_PREFIX="xterm256-rich-$$"
RUNTIME_DIR="${SHUX_RUNTIME_DIR:-/tmp/shux-xterm256-rich-$$}"
CONFIG_HOME="$RUNTIME_DIR/config"
EXPECT_VIVECAKA_PRS="${EXPECT_VIVECAKA_PRS:-0}"
ORIG_XDG_CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"

mkdir -p "$OUT_DIR" "$RUNTIME_DIR" "$CONFIG_HOME/shux"
export XDG_RUNTIME_DIR="$RUNTIME_DIR"
export XDG_CONFIG_HOME="$CONFIG_HOME"
# Keep GitHub-backed TUIs authenticated while still isolating shux's config.
export GH_CONFIG_DIR="${GH_CONFIG_DIR:-$ORIG_XDG_CONFIG_HOME/gh}"

if [[ ! -x "$SHUX" ]]; then
    "$MAKE_BIN" release
fi
if [[ "$SHUX" != /* ]]; then
    SHUX="$(cd "$(dirname "$SHUX")" && pwd)/$(basename "$SHUX")"
fi

shux_cmd() {
    "$SHUX" "$@"
}

SESSIONS=()
cleanup() {
    for session in "${SESSIONS[@]:-}"; do
        shux_cmd session kill "$session" >/dev/null 2>&1 || true
    done
}
trap cleanup EXIT INT TERM HUP

run_tui() {
    local name="$1"
    local wait_s="$2"
    shift 2

    local session="$SESSION_PREFIX-$name"
    SESSIONS+=("$session")

    echo "==> $name: $*"
    shux_cmd session kill "$session" >/dev/null 2>&1 || true
    shux_cmd --format json session create "$session" -d --title "$name" -- \
        env TERM=xterm-256color COLORTERM=truecolor "$@" \
        >"$OUT_DIR/$name.create.json"
    shux_cmd pane set-size -s "$session" --cols 160 --rows 48 >/dev/null
    sleep "$wait_s"
    shux_cmd pane capture -s "$session" --lines 80 >"$OUT_DIR/$name.txt"
    shux_cmd pane snapshot -s "$session" -o "$OUT_DIR/$name-pane.png" >/dev/null
    shux_cmd window snapshot -s "$session" --cols 164 --rows 52 -o "$OUT_DIR/$name-window.png" >/dev/null
}

run_if_installed() {
    local name="$1"
    local binary="$2"
    local wait_s="$3"
    shift 3

    if command -v "$binary" >/dev/null 2>&1; then
        run_tui "$name" "$wait_s" "$binary" "$@"
    else
        echo "SKIP $name: $binary not found" | tee "$OUT_DIR/$name.skip.txt"
    fi
}

run_if_installed "lazygit" "lazygit" 4
run_if_installed "btop" "btop" 4
run_if_installed "nvim" "nvim" 2 -u NONE -n +"set termguicolors laststatus=2 ruler" +"syntax on" +"set statusline=shux-xterm-256color\\ %f\\ %y\\ %m%=\\ %l,%c" crates/shux-vt/src/parser.rs
run_if_installed "vicaya-tui" "vicaya-tui" 5
run_if_installed "vivecaka" "vivecaka" 8 --repo=indrasvat/shux

SYNC_PROBE="$OUT_DIR/sync-output-probe.py"
cat >"$SYNC_PROBE" <<'PY'
import sys
import time

sys.stdout.write("old-frame")
sys.stdout.flush()
sys.stdout.write("\x1b[?2026h\x1b[1;1Hpending-frame")
sys.stdout.flush()
time.sleep(1.0)
sys.stdout.write("\x1b[?2026l\x1b[2;1Hsync-output committed")
sys.stdout.flush()
time.sleep(60)
PY
run_tui "sync-output-probe" 3 python3 "$SYNC_PROBE"
grep -q 'pending-frame' "$OUT_DIR/sync-output-probe.txt"
grep -q 'sync-output committed' "$OUT_DIR/sync-output-probe.txt"

if [[ -f "$OUT_DIR/vivecaka.txt" ]]; then
    grep -q 'indrasvat/shux' "$OUT_DIR/vivecaka.txt"
    if [[ "$EXPECT_VIVECAKA_PRS" == "1" ]]; then
        if grep -q 'No pull requests found' "$OUT_DIR/vivecaka.txt"; then
            echo "vivecaka rendered an empty PR list for indrasvat/shux" >&2
            exit 1
        fi
    fi
fi

if command -v magick >/dev/null 2>&1; then
    mapfile -t windows < <(find "$OUT_DIR" -maxdepth 1 -name '*-window.png' | sort)
    if [[ "${#windows[@]}" -gt 0 ]]; then
        FONT_OPT=()
        if [[ -f /System/Library/Fonts/Supplemental/Arial.ttf ]]; then
            FONT_OPT=(-font /System/Library/Fonts/Supplemental/Arial.ttf)
        fi
        magick montage "${windows[@]}" -tile 2x -geometry +18+18 \
            "${FONT_OPT[@]}" -background '#111318' "$OUT_DIR/contact-sheet.png"
    fi
fi

echo "✓ xterm-256color rich TUI proof: $OUT_DIR"
