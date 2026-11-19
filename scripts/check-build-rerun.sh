#!/usr/bin/env bash
# scripts/check-build-rerun.sh — a build with nothing changed compiles nothing.
#
# shux-rpc (the crate with a build script) and shux, which depends on it, are
# built, then built again after an unrelated ref moves; neither may recompile.
# A `rerun-if-changed` on a path that never exists (`.git/HEAD` in a worktree,
# where `.git` is a file), or on all of `refs/` (which every worktree's commits
# touch), rebuilds everything downstream on every build.
#
# Exit: 0 no-op rebuild · 1 something recompiled, or the build failed.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
log="$(mktemp "${TMPDIR:-/tmp}/shux-build-rerun.XXXXXX")"
probe_ref="refs/shux-build-rerun-probe/$$"
trap 'rm -f "$log"; git update-ref -d "$probe_ref" 2>/dev/null || true' EXIT

printf '\033[1;36m▶ %s\033[0m\n' "cargo build -p shux-rpc -p shux (warm-up)"
cargo build --locked --color never -p shux-rpc -p shux
git update-ref "$probe_ref" HEAD
printf '\033[1;36m▶ %s\033[0m\n' "an unrelated ref moved; cargo build -p shux-rpc -p shux (must compile nothing)"
cargo build --locked --color never -p shux-rpc -p shux 2>&1 | tee "$log"

if grep -E '^\s*Compiling (shux|shux-rpc) ' "$log"; then
    printf '\033[1;31m✗ %s\033[0m\n' "a build with nothing changed recompiled the crates above; a build script's rerun-if-changed names a path that is never stable" >&2
    exit 1
fi
printf '\033[1;32m✓ %s\033[0m\n' "no-op rebuild compiles nothing"
