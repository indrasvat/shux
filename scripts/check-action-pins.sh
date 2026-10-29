#!/usr/bin/env bash
# scripts/check-action-pins.sh — every third-party action is pinned to a commit.
#
# A tag (`@v4`) or branch (`@stable`) can be moved by whoever controls the
# action's repository, and the next run executes whatever it points at with
# this repository's tokens. A 40-hex commit SHA cannot move. Local actions
# (`./path`) are this repository's own code and need no pin; `docker://` images
# must be pinned by `@sha256:` digest.
#
# Scans every workflow and composite action under .github/. Finding no files to
# scan is an error, not a pass.

set -euo pipefail

root="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
cd "$root"

mapfile -t files < <(find .github -type f \( -name '*.yml' -o -name '*.yaml' \) \
    \( -path '.github/workflows/*' -o -name 'action.yml' -o -name 'action.yaml' \) \
    2>/dev/null | sort)
if [[ ${#files[@]} -eq 0 ]]; then
    echo "✗ no workflows or actions under ${root}/.github — this guard would check nothing" >&2
    exit 2
fi

bad=0
scanned=0
for f in "${files[@]}"; do
    n=0
    while IFS= read -r line || [[ -n "$line" ]]; do
        n=$((n + 1))
        # `uses:` as a mapping key, optionally a list item: `- uses: x` / `uses: x`.
        [[ "$line" =~ ^[[:space:]]*(-[[:space:]]+)?uses:[[:space:]]*(.*)$ ]] || continue
        ref="${BASH_REMATCH[2]}"
        # A YAML comment needs whitespace before the `#`.
        ref="$(sed -E 's/[[:space:]]+#.*$//; s/[[:space:]]+$//' <<<"$ref")"
        ref="${ref#\"}"; ref="${ref%\"}"; ref="${ref#\'}"; ref="${ref%\'}"
        scanned=$((scanned + 1))
        case "$ref" in
            ./*) continue ;;
            docker://*@sha256:*)
                [[ "$ref" =~ @sha256:[0-9a-f]{64}$ ]] && continue ;;
            *)
                [[ "$ref" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_./-]+@[0-9a-f]{40}$ ]] && continue ;;
        esac
        echo "  ${f}:${n}: uses: ${ref:-<empty>}" >&2
        bad=1
    done <"$f"
done

if [[ $scanned -eq 0 ]]; then
    echo "✗ no \`uses:\` lines in ${#files[@]} file(s) — this guard would check nothing" >&2
    exit 2
fi
if [[ $bad -ne 0 ]]; then
    echo "✗ actions above are not pinned to a 40-character commit SHA." >&2
    echo "  Pin as \`owner/repo@<sha> # vX.Y.Z\`; Dependabot keeps the pin current." >&2
    exit 1
fi
echo "✓ ${scanned} action reference(s) in ${#files[@]} file(s) pinned to commits"
