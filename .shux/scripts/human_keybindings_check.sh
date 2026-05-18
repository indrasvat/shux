#!/usr/bin/env bash
# Validate the configurable attach keybinding surface with good and bad configs.
#
# Outputs:
#   .shux/out/human-keybindings-valid.toml
#   .shux/out/human-keybindings-invalid.toml
#   .shux/out/human-keybindings-invalid.log

set -euo pipefail

SHUX="${SHUX_BIN:-target/debug/shux}"
OUT_DIR="${OUT_DIR:-.shux/out}"
VALID="$OUT_DIR/human-keybindings-valid.toml"
INVALID="$OUT_DIR/human-keybindings-invalid.toml"
INVALID_LOG="$OUT_DIR/human-keybindings-invalid.log"
RUNTIME_DIR="${SHUX_RUNTIME_DIR:-/tmp/shux-human-keybindings-$$}"

mkdir -p "$OUT_DIR"
mkdir -p "$RUNTIME_DIR"
export XDG_RUNTIME_DIR="$RUNTIME_DIR"

if [[ ! -x "$SHUX" ]]; then
    cargo build -p shux
fi
shux_cmd() {
    "$SHUX" "$@"
}

echo "==> unit coverage: keybinding registry"
cargo test -p shux-ui keybinding --lib
cargo test -p shux config_validate::tests::validate_accepts_keybinding_overrides --bin shux
cargo test -p shux config_validate::tests::validate_rejects_unknown_keybinding_action --bin shux

printf '%s\n' \
    '[keys]' \
    'prefix = "ctrl-a"' \
    '' \
    '[keybindings]' \
    '"alt-left" = "focus-left"' \
    '"alt-right" = "focus-right"' \
    '"prefix c" = "new-window"' \
    '"ctrl-a [" = "copy-mode"' \
    '"prefix d" = "detach"' \
    > "$VALID"

shux_cmd config validate "$VALID" >/dev/null

printf '%s\n' \
    '[keybindings]' \
    '"alt-x" = "launch-moon"' \
    > "$INVALID"

if shux_cmd config validate "$INVALID" >"$INVALID_LOG" 2>&1; then
    echo "invalid keybinding config unexpectedly passed" >&2
    exit 1
fi
grep -q 'UnknownAction' "$INVALID_LOG"

echo "✓ keybinding validation dogfood passed"
