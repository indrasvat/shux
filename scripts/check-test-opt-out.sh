#!/usr/bin/env bash
# scripts/check-test-opt-out.sh — fail if an integration test opts out of `make test`.
#
# A `[[test]]` declared `test = false` is skipped by `nextest run --workspace` (what
# `make test` and CI run) and by `clippy --all-targets`. Sixteen lens suites sat
# there, unrun by CI, long after the reason for it had gone (#245). Daemon-backed
# suites are bounded by nextest's `daemon-pty` group instead; see
# .config/nextest.toml.

set -euo pipefail

command -v jq >/dev/null || {
    echo "check-test-opt-out: jq is required" >&2
    exit 1
}

metadata="$(cargo metadata --no-deps --format-version 1 --color never)"

# Integration tests exist in this workspace, so finding none means the query broke.
total="$(jq '[.packages[].targets[] | select(.kind == ["test"])] | length' <<<"$metadata")"
if [[ "$total" -eq 0 ]]; then
    echo "check-test-opt-out: found no integration test targets; the metadata query is broken" >&2
    exit 1
fi

opted_out="$(jq -r '.packages[] | .name as $pkg | .targets[]
    | select(.kind == ["test"] and .test == false) | "  \($pkg) \(.name)"' <<<"$metadata")"
if [[ -n "$opted_out" ]]; then
    echo "check-test-opt-out: these integration tests declare test = false, so neither" >&2
    echo "make test nor CI runs them:" >&2
    echo "$opted_out" >&2
    echo "Remove the [[test]] entry; bound a daemon-backed suite with a nextest group." >&2
    exit 1
fi
echo "✓ all $total integration test targets run under make test"
