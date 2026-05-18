#!/usr/bin/env bash
# Focused copy-mode regression + dogfood capture for human-interactive work.
#
# Outputs:
#   .shux/out/human-copy-mode.png
#   .shux/out/human-copy-mode.txt

set -euo pipefail

SHUX="${SHUX_BIN:-target/debug/shux}"
SESSION="${SESSION:-human-copy-mode-$$}"
OUT_DIR="${OUT_DIR:-.shux/out}"
PNG="$OUT_DIR/human-copy-mode.png"
TXT="$OUT_DIR/human-copy-mode.txt"
RUNTIME_DIR="${SHUX_RUNTIME_DIR:-/tmp/shux-human-copy-mode-$$}"

mkdir -p "$OUT_DIR"
mkdir -p "$RUNTIME_DIR"
export XDG_RUNTIME_DIR="$RUNTIME_DIR"

if [[ ! -x "$SHUX" ]]; then
    cargo build -p shux
fi
shux_cmd() {
    "$SHUX" "$@"
}

trap 'shux_cmd session kill "$SESSION" >/dev/null 2>&1 || true' EXIT
shux_cmd session kill "$SESSION" >/dev/null 2>&1 || true

echo "==> unit coverage: scrollback copy-mode navigation/search/render"
cargo test -p shux-ui copy_mode --lib

echo "==> dogfood: spawn scrollback-heavy pane"
shux_cmd --format json session create "$SESSION" -d --title copy-mode-check -- \
    bash -lc 'for i in $(seq -w 0 500); do printf "copy-line-%s  searchable payload %s\n" "$i" "$i"; done; sleep 9000' \
    >/dev/null

shux_cmd pane set-size -s "$SESSION" --cols 96 --rows 18 >/dev/null
shux_cmd pane wait-for -s "$SESSION" -t copy-line-500 --timeout-ms 5000 >/dev/null
shux_cmd pane capture -s "$SESSION" --lines 30 > "$TXT"
grep -q 'copy-line-500' "$TXT"

shux_cmd pane snapshot -s "$SESSION" -o "$PNG" >/dev/null
head -c 8 "$PNG" | od -A n -t x1 | tr -d ' \n' | grep -q '89504e470d0a1a0a'

echo "✓ copy-mode dogfood captured: $PNG"
