#!/usr/bin/env bash
# scripts/check-build-rerun.sh — a build with nothing changed compiles nothing.
#
# The crates with a build script (shux-rpc, shux) are built twice; the second
# build must not recompile either. A `rerun-if-changed` on a path that does not
# exist (`.git/HEAD` inside a git worktree, where `.git` is a file) reruns the
# script on every build and rebuilds everything downstream of it.
#
# Exit: 0 no-op rebuild · 1 something recompiled, or the build failed.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
log="$(mktemp "${TMPDIR:-/tmp}/shux-build-rerun.XXXXXX")"
trap 'rm -f "$log"' EXIT

printf '\033[1;36m▶ %s\033[0m\n' "cargo build -p shux-rpc -p shux (warm-up)"
cargo build --locked --color never -p shux-rpc -p shux
printf '\033[1;36m▶ %s\033[0m\n' "cargo build -p shux-rpc -p shux (must compile nothing)"
cargo build --locked --color never -p shux-rpc -p shux 2>&1 | tee "$log"

if grep -E '^\s*Compiling (shux|shux-rpc) ' "$log"; then
    printf '\033[1;31m✗ %s\033[0m\n' "a build with nothing changed recompiled the crates above; a build script's rerun-if-changed names a path that is never stable" >&2
    exit 1
fi
printf '\033[1;32m✓ %s\033[0m\n' "no-op rebuild compiles nothing"
