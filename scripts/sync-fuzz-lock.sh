#!/usr/bin/env bash
# scripts/sync-fuzz-lock.sh — keep fuzz/Cargo.lock at the workspace version.
#
# The fuzz crate is its own workspace with its own lockfile, and records every
# shux crate it builds at the workspace version. A release that bumps
# Cargo.toml but not this lock leaves the next build rewriting it, so every
# checkout goes dirty.
#
# Usage:
#   scripts/sync-fuzz-lock.sh           rewrite the lock to the workspace version
#   scripts/sync-fuzz-lock.sh --check   fail if any shux crate in it disagrees, or a
#                                       dependency the workspace added is missing
#   LOCK=<path> scripts/sync-fuzz-lock.sh --check   check another lockfile
set -euo pipefail

LOCK="${LOCK:-fuzz/Cargo.lock}"
mode="${1:-sync}"

version=$(awk -F'"' '/^\[workspace.package\]/ { ws = 1; next } /^\[/ { ws = 0 } ws && /^version = "/ { print $2; exit }' Cargo.toml)
if [[ -z "$version" ]]; then
    echo "✗ no [workspace.package] version in Cargo.toml" >&2
    exit 1
fi
if [[ ! -s "$LOCK" ]]; then
    echo "✗ $LOCK is missing or empty" >&2
    exit 1
fi

# The workspace's crates: every shux package but the fuzz crate itself.
listed() {
    awk -F'"' '/^name = "/ { name = $2; next }
        /^version = "/ && name ~ /^shux/ && name != "shux-fuzz" { print name " " $2; name = "" }' "$1"
}

if [[ "$mode" == "--check" ]]; then
    found=$(listed "$LOCK")
    if [[ -z "$found" ]]; then
        echo "✗ $LOCK lists no shux crate: nothing was checked" >&2
        exit 1
    fi
    stale=$(awk -v want="$version" '$2 != want' <<<"$found")
    if [[ -n "$stale" ]]; then
        echo "✗ $LOCK disagrees with the workspace version $version:" >&2
        awk '{ print "    " $0 }' <<<"$stale" >&2
        echo "  fix: scripts/sync-fuzz-lock.sh" >&2
        exit 1
    fi
    # A dependency added to a shux crate is missing until cargo resolves it.
    # Not --offline: a fresh checkout has not fetched the fuzz-only crates.
    if [[ "$LOCK" == fuzz/Cargo.lock ]] && ! cargo metadata --locked --format-version 1 --manifest-path fuzz/Cargo.toml >/dev/null; then
        echo "✗ $LOCK lacks a dependency the workspace now uses" >&2
        echo "  fix: scripts/sync-fuzz-lock.sh" >&2
        exit 1
    fi
    echo "✓ $LOCK is at the workspace version $version ($(wc -l <<<"$found" | tr -d ' ') crates)"
    exit 0
fi

tmp=$(mktemp)
awk -v want="$version" '/^name = "/ { name = $0 }
    /^version = "/ && name ~ /^name = "shux/ && name != "name = \"shux-fuzz\"" { $0 = "version = \"" want "\""; name = "" }
    { print }' "$LOCK" > "$tmp"
cat "$tmp" > "$LOCK"
rm -f "$tmp"
if [[ "$LOCK" == fuzz/Cargo.lock ]]; then
    cargo metadata --format-version 1 --manifest-path fuzz/Cargo.toml >/dev/null
fi
echo "Updated $LOCK to version $version"
