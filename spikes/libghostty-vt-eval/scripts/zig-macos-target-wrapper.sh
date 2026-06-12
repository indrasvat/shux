#!/usr/bin/env bash
set -euo pipefail

real_zig="${SHUX_LIBGHOSTTY_REAL_ZIG:?set SHUX_LIBGHOSTTY_REAL_ZIG to the real zig binary}"

if [[ "${1:-}" == "build" ]]; then
  for arg in "$@"; do
    if [[ "$arg" == -Dtarget=* ]]; then
      exec "$real_zig" "$@"
    fi
  done

  arch="$(uname -m)"
  case "$arch" in
    arm64) zig_target="aarch64-macos-none" ;;
    x86_64) zig_target="x86_64-macos-none" ;;
    *) echo "unsupported macOS arch for libghostty spike Zig wrapper: $arch" >&2; exit 2 ;;
  esac

  exec "$real_zig" "$@" "-Dtarget=$zig_target"
fi

exec "$real_zig" "$@"
