#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${SHUX_BIN:-$ROOT/target/debug/shux}"
OUT="${SHUX_OUT_DIR:-$ROOT/.shux/out/issue-63}"
MATRIX_COLS="${SHUX_MATRIX_COLS:-108}"
MATRIX_ROWS="${SHUX_MATRIX_ROWS:-32}"
mkdir -p "$OUT"

SESSIONS=()

cleanup() {
  local session
  for session in "${SESSIONS[@]:-}"; do
    "$BIN" session kill "$session" >/dev/null 2>&1 || true
  done
}
trap cleanup EXIT

require_bin() {
  if [[ ! -x "$BIN" ]]; then
    echo "missing shux binary: $BIN" >&2
    echo "run: make build" >&2
    exit 1
  fi
}

validate_png() {
  local png="$1"
  [[ -s "$png" ]] || {
    echo "empty PNG: $png" >&2
    exit 1
  }
  if command -v sips >/dev/null 2>&1; then
    local width height
    width="$(sips -g pixelWidth "$png" 2>/dev/null | awk '/pixelWidth/ {print $2}')"
    height="$(sips -g pixelHeight "$png" 2>/dev/null | awk '/pixelHeight/ {print $2}')"
    if [[ -z "$width" || -z "$height" || "$width" -le 1 || "$height" -le 1 ]]; then
      echo "invalid PNG dimensions for $png: ${width:-?}x${height:-?}" >&2
      exit 1
    fi
  fi
}

snapshot_pair() {
  local session="$1"
  local label="$2"
  "$BIN" pane snapshot -s "$session" -o "$OUT/$label-pane.png" >/dev/null
  "$BIN" window snapshot -s "$session" --cols 110 --rows 34 -o "$OUT/$label-window.png" >/dev/null
  validate_png "$OUT/$label-pane.png"
  validate_png "$OUT/$label-window.png"
  printf '%s\n' "$label-pane.png" "$label-window.png" >>"$OUT/manifest.txt"
}

set_matrix_size() {
  "$BIN" pane set-size -s "$1" --cols "$MATRIX_COLS" --rows "$MATRIX_ROWS" >/dev/null
}

create_session() {
  local session="$1"
  shift
  SESSIONS+=("$session")
  "$BIN" session create "$session" -d --title "$session" -- "$@" >/dev/null
}

require_bin
: >"$OUT/manifest.txt"
printf 'issue-63 render matrix\nstarted=%s\n\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >"$OUT/manifest.txt"

primitive_session="issue63-primitives-$$"
create_session "$primitive_session" python3 -u -c 'import sys,time
w=lambda s: (sys.stdout.write(s), sys.stdout.flush())
w("\x1b[?1049h\x1b[?2026h")
w("\x1b[2;1Hhadolint x")
w("\x1b[2;40H\x1b[1`\x1b[K    1. Fix blockers")
w("\x1b[4;1HAAAAA\x1b[1S\x1b[1T")
w("\x1b[?2026l")
w("\x1b[8;1Hissue63-primitives-ready")
time.sleep(600)'
set_matrix_size "$primitive_session"
"$BIN" pane wait-for -s "$primitive_session" --text issue63-primitives-ready >/dev/null
snapshot_pair "$primitive_session" primitives

color_session="issue63-color-meta-$$"
create_session "$color_session" python3 -u -c 'import sys,time
w=lambda s: (sys.stdout.write(s), sys.stdout.flush())
w("\x1b]2;osc-title-proof\x07")
w("\x1b]10;#ffcc00\x1b\\\x1b]11;#071820\x1b\\\x1b]12;#00ff80\x1b\\")
w("default-fg-bg\n")
w("\x1b[38;2;255;0;0mred\x1b[0m ")
w("\x1b[4:3;58:2::0:255:255mcurly-cyan-underline\x1b[0m ")
w("\x1b]8;;https://example.invalid/a;b\x07link\x1b]8;;\x07 normal\n")
w("issue63-color-meta-ready\n")
time.sleep(600)'
set_matrix_size "$color_session"
"$BIN" pane wait-for -s "$color_session" --text issue63-color-meta-ready >/dev/null
snapshot_pair "$color_session" color-meta

title_session="issue63-title-$$"
SESSIONS+=("$title_session")
"$BIN" session create "$title_session" -d -- python3 -u -c 'import sys,time
w=lambda s: (sys.stdout.write(s), sys.stdout.flush())
w("\x1b]2;osc-title-proof\x07")
w("issue63-title-ready\n")
time.sleep(600)' >/dev/null
set_matrix_size "$title_session"
"$BIN" pane wait-for -s "$title_session" --text issue63-title-ready >/dev/null
snapshot_pair "$title_session" title

cursor_session="issue63-cursor-$$"
create_session "$cursor_session" python3 -u -c 'import sys,time
w=lambda s: (sys.stdout.write(s), sys.stdout.flush())
w("visible block cursor here")
w("\x1b[3 q\x1b]12;#00ff80\x1b\\\x1b[2;1Hunderline cursor requested")
w("\x1b[5 q\x1b[3;1Hbar cursor requested")
w("\x1b[?25l\x1b[4;1Hcursor hidden line")
w("\x1b[?25h\x1b[6 q\x1b[5;1Hissue63-cursor-ready")
time.sleep(600)'
set_matrix_size "$cursor_session"
"$BIN" pane wait-for -s "$cursor_session" --text issue63-cursor-ready >/dev/null
snapshot_pair "$cursor_session" cursor

sync_session="issue63-sync-$$"
create_session "$sync_session" python3 -u -c 'import sys,time
w=lambda s: (sys.stdout.write(s), sys.stdout.flush())
w("stable-before\n")
w("\x1b[?2026h")
w("\x1b[2;1Hpartial-hidden")
time.sleep(2)
w("\x1b[?2026l")
w("\x1b[3;1Hissue63-sync-ready")
time.sleep(600)'
set_matrix_size "$sync_session"
sleep 0.7
"$BIN" pane capture -s "$sync_session" -l 5 >"$OUT/sync-held.txt"
if grep -q partial-hidden "$OUT/sync-held.txt"; then
  echo "synchronized output leaked partial frame into capture" >&2
  exit 1
fi
"$BIN" pane snapshot -s "$sync_session" -o "$OUT/sync-held-pane.png" >/dev/null
validate_png "$OUT/sync-held-pane.png"
printf '%s\n' "sync-held-pane.png" >>"$OUT/manifest.txt"
"$BIN" pane wait-for -s "$sync_session" --text issue63-sync-ready >/dev/null
snapshot_pair "$sync_session" sync-released

if command -v vim >/dev/null 2>&1; then
  vim_session="issue63-vim-$$"
  create_session "$vim_session" vim -Nu NONE -n \
    -c 'set noruler noshowcmd laststatus=2' \
    -c 'call setline(1, ["issue63-vim-ready", "color/align smoke", "underline cursor below"])' \
    -c 'normal! G$'
  set_matrix_size "$vim_session"
  "$BIN" pane wait-for -s "$vim_session" --text issue63-vim-ready >/dev/null
  snapshot_pair "$vim_session" vim
fi

printf '\ncompleted=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >>"$OUT/manifest.txt"
echo "issue-63 render matrix wrote $OUT"
