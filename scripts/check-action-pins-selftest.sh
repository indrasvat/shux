#!/usr/bin/env bash
# scripts/check-action-pins-selftest.sh — prove check-action-pins.sh can FAIL.
#
# Runs the shipped guard against a throwaway tree holding reintroduced unpinned
# references, the shapes it must accept, and empty input.

set -euo pipefail

guard="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/check-action-pins.sh"
[[ -f "$guard" ]] || { echo "✗ cannot find check-action-pins.sh" >&2; exit 1; }

work="$(mktemp -d "${TMPDIR:-/tmp}/shux-pins-selftest.XXXXXX")"
trap 'rm -rf "$work"' EXIT
wf="$work/.github/workflows/ci.yml"
sha=11d5960a326750d6838e8c26ee5ac8ec0a2d0a5c

fail=0
expect() { # expect <want-exit> <label>
    local want="$1" label="$2" got=0
    bash "$guard" "$work" >/dev/null 2>&1 || got=$?
    if [[ "$got" == "$want" ]]; then
        echo "  ✓ $label"
    else
        echo "  ✗ $label: exit $got, want $want"
        fail=1
    fi
}
workflow() { mkdir -p "$(dirname "$wf")"; printf '%s\n' 'jobs:' '  a:' '    steps:' "$@" >"$wf"; }

echo "check-action-pins self-test"

expect 2 "no .github: nothing to scan is an error, not a pass"
workflow '      - run: true'
expect 2 "workflows with no uses: lines are an error, not a pass"

workflow "      - uses: actions/checkout@v4"
expect 1 "a tag is flagged"
workflow "      - uses: dtolnay/rust-toolchain@stable"
expect 1 "a branch is flagged"
workflow "      - uses: actions/checkout@${sha:0:12}"
expect 1 "a short SHA is flagged"
workflow "      - uses: actions/checkout"
expect 1 "no ref at all is flagged"
workflow "      - uses: actions/checkout@${sha}" "      - name: x" "        uses: actions/cache@v4"
expect 1 "one unpinned step among pinned ones is flagged"
workflow "      - uses: \"actions/checkout@v4\" # ${sha}"
expect 1 "a SHA in the comment does not pin a quoted tag"
workflow "      - uses: docker://alpine:3.20"
expect 1 "a docker image by tag is flagged"
mkdir -p "$work/.github/actions/x"
workflow "      - uses: actions/checkout@${sha}"
printf '%s\n' 'runs:' '  using: composite' '  steps:' '    - uses: actions/cache@v4' \
    >"$work/.github/actions/x/action.yml"
expect 1 "a composite action's unpinned step is flagged"
rm -rf "$work/.github/actions"

workflow "      - uses: actions/checkout@${sha} # v4.4.0" \
    "      - uses: 'actions/cache/restore@${sha}'" \
    "      - uses: ./.github/actions/local" \
    "      - uses: docker://alpine@sha256:$(printf 'a%.0s' {1..64})" \
    "        # uses: actions/checkout@v4"
expect 0 "pinned, quoted, sub-path, local and digest-pinned references pass"

if [[ $fail -ne 0 ]]; then
    echo "✗ check-action-pins self-test failed" >&2
    exit 1
fi
echo "✓ check-action-pins self-test passed"
