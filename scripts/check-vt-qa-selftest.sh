#!/usr/bin/env bash
# scripts/check-vt-qa-selftest.sh — prove the VT QA trigger can still FAIL.
#
# `scripts/check-vt-qa.sh` decides whether a diff owes SOLID-QA evidence. A
# trigger that has stopped triggering reports "no evidence required" forever and
# looks exactly like a clean run, so the only way to trust its PASS is to watch
# it fail on a diff that owes evidence and pass on empty input.
#
# Every case runs the real guard against a throwaway repository, so this asserts
# the shipped script rather than a copy of its logic.

set -euo pipefail

GUARD="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/check-vt-qa.sh"
[[ -x "$GUARD" || -f "$GUARD" ]] || {
    echo "✗ cannot find check-vt-qa.sh next to this script" >&2
    exit 1
}

work="$(mktemp -d "${TMPDIR:-/tmp}/shux-vt-qa-selftest.XXXXXX")"
trap 'rm -rf "${work}"' EXIT

git -C "$work" init -q
git -C "$work" config user.email selftest@example.invalid
git -C "$work" config user.name selftest
echo base >"$work/README.md"
git -C "$work" add -A
git -C "$work" -c commit.gpgsign=false commit -qm base
base="$(git -C "$work" rev-parse HEAD)"

failures=0

# Stage `$1…` as new files in the throwaway repo and require exit code `$2`.
expect() {
    local want="$1" label="$2" got=0
    shift 2
    git -C "$work" reset -q
    local f
    for f in "$@"; do
        mkdir -p "$work/$(dirname "$f")"
        echo "// $label" >"$work/$f"
        git -C "$work" add -- "$f"
    done
    (cd "$work" && VT_QA_BASE="$base" bash "$GUARD" >/dev/null 2>&1) || got=$?
    if [[ "$got" == "$want" ]]; then
        printf '  \033[32m✓\033[0m %s (exit %s)\n' "$label" "$got"
    else
        printf '  \033[31m✗\033[0m %s: expected exit %s, got %s\n' "$label" "$want" "$got"
        failures=$((failures + 1))
    fi
}

echo "▶ VT QA guard self-test"

# Empty input must not be mistaken for a clean audit.
expect 0 "empty diff owes nothing"

# The positive control: a shipping VT source file with no evidence in the diff.
# If this ever passes, the gate has stopped gating.
expect 2 "shux-vt/src owes evidence" crates/shux-vt/src/lib.rs
expect 2 "shux-raster/src owes evidence" crates/shux-raster/src/lib.rs
expect 2 "pty capture owes evidence" crates/shux-pty/src/capture.rs

# The exemption under test: trees that ship no cells and no pixels.
expect 0 "shux-vt/tests owes nothing" crates/shux-vt/tests/some_test.rs
expect 0 "shux-vt/benches owes nothing" crates/shux-vt/benches/some_bench.rs

# The exemption must not SUPPRESS a real trigger in the same diff.
expect 2 "src+tests still owes evidence" \
    crates/shux-vt/src/lib.rs crates/shux-vt/tests/some_test.rs

# A path outside the gated set stays outside it.
expect 0 "unrelated crate owes nothing" crates/shux-core/src/session.rs

if [[ "$failures" -gt 0 ]]; then
    echo "✗ VT QA guard self-test: $failures case(s) failed" >&2
    exit 1
fi
echo "✓ VT QA guard self-test: trigger fires and stays quiet where it should"
