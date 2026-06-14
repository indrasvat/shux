#!/usr/bin/env bash
# Save a live shux session to a template, dry-run restore it, and capture pixels.
#
# Outputs:
#   .shux/out/human-session-source.toml
#   .shux/out/human-session-saved.toml
#   .shux/out/human-session-restore.json
#   .shux/out/human-session-window.png

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
source "$REPO_ROOT/.shux/scripts/lib/shux_harness.sh"
SHUX="${SHUX_BIN:-target/debug/shux}"
SESSION="${SESSION:-human-session-persist-$$}"
OUT_DIR="${OUT_DIR:-.shux/out}"
SOURCE="$OUT_DIR/human-session-source.toml"
SAVED="$OUT_DIR/human-session-saved.toml"
RESTORE_JSON="$OUT_DIR/human-session-restore.json"
PNG="$OUT_DIR/human-session-window.png"
RUNTIME_DIR="${SHUX_RUNTIME_DIR:-/tmp/shux-human-session-persistence-$$}"

mkdir -p "$OUT_DIR"
mkdir -p "$RUNTIME_DIR"
export XDG_RUNTIME_DIR="$RUNTIME_DIR"

if [[ ! -x "$SHUX" ]]; then
    cargo build -p shux
fi
shux_cmd() {
    "$SHUX" "$@"
}

trap 'shux_harness_cleanup_runtime "$RUNTIME_DIR" "$SHUX" "$SESSION"' EXIT
shux_harness_kill_session "$RUNTIME_DIR" "$SHUX" "$SESSION"

echo "==> unit coverage: session export + CLI parse"
cargo test -p shux session_persist --bin shux
cargo test -p shux cli::tests::test_cli_parse_session_save --bin shux
cargo test -p shux cli::tests::test_cli_parse_session_restore_dry_run --bin shux

cat > "$SOURCE" <<TOML
[session]
name = "$SESSION"
cwd = "$PWD"

[[windows]]
title = "persist-check"

[[windows.panes]]
command = ["bash", "-lc", "echo LEFT-PERSIST; sleep 9000"]

[[windows.panes]]
command = ["bash", "-lc", "echo RIGHT-PERSIST; sleep 9000"]
split = "vertical"
ratio = 0.45
TOML

shux_cmd state apply "$SOURCE" >/dev/null
shux_cmd pane wait-for -s "$SESSION" -t RIGHT-PERSIST --timeout-ms 5000 >/dev/null
shux_cmd window snapshot -s "$SESSION" --cols 120 --rows 32 -o "$PNG" >/dev/null
head -c 8 "$PNG" | od -A n -t x1 | tr -d ' \n' | grep -q '89504e470d0a1a0a'

shux_cmd session save -s "$SESSION" -o "$SAVED" >/dev/null
grep -q 'name = "'$SESSION'"' "$SAVED"
grep -q 'direction = "vertical"' "$SAVED"

shux_cmd session restore "$SAVED" --dry-run > "$RESTORE_JSON"
grep -q 'create_session' "$RESTORE_JSON"
grep -q "$SESSION" "$RESTORE_JSON"

echo "✓ session persistence dogfood captured: $PNG"
