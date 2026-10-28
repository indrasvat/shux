#!/usr/bin/env bash
# scripts/clippy-lanes.sh — clippy the integration tests `--all-targets` skips.
#
# `cargo clippy --all-targets` selects test targets with `test = true` only, so a
# `[[test]]` declared `test = false` (the lens lane in crates/shux) is never
# linted by it — and `--test NAME` is ignored when combined with `--tests`. Each
# such target is therefore named explicitly, per package.

set -euo pipefail

command -v jq >/dev/null || {
    echo "clippy-lanes: jq is required to list the test = false targets" >&2
    exit 1
}

lanes="$(cargo metadata --no-deps --format-version 1 --color never |
    jq -r '.packages[] | .name as $pkg | .targets[]
        | select(.kind == ["test"] and .test == false) | "\($pkg) \(.name)"')"

if [[ -z "$lanes" ]]; then
    echo "✓ no test = false targets to lint"
    exit 0
fi

count=0
while read -r pkg; do
    args=()
    while read -r p name; do
        [[ "$p" == "$pkg" ]] && args+=(--test "$name")
    done <<<"$lanes"
    count=$((count + ${#args[@]} / 2))
    cargo clippy -p "$pkg" "${args[@]}" -- -D warnings
done < <(cut -d' ' -f1 <<<"$lanes" | sort -u)
echo "✓ clippy clean on $count test = false target(s)"
