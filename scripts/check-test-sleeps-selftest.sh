#!/usr/bin/env bash
# scripts/check-test-sleeps-selftest.sh — prove check-test-sleeps.sh can FAIL.
#
# Runs the shipped guard, copied byte for byte into a throwaway tree (it finds
# the tree from its own location), against reintroduced sleeps, against the
# shapes it must never flag, and against empty input.

set -euo pipefail

src="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/check-test-sleeps.sh"
[[ -f "$src" ]] || { echo "✗ cannot find check-test-sleeps.sh" >&2; exit 1; }

work="$(mktemp -d "${TMPDIR:-/tmp}/shux-sleeps-selftest.XXXXXX")"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/scripts" "$work/crates/k/tests" "$work/crates/k/src"
cp "$src" "$work/scripts/"
guard="$work/scripts/check-test-sleeps.sh"
t="$work/crates/k/tests/t.rs"

fail=0
expect() { # expect <want-exit> <label> <cmd...>
    local want="$1" label="$2"
    shift 2
    local got=0
    "$@" >/dev/null 2>&1 || got=$?
    if [[ "$got" == "$want" ]]; then
        echo "  ✓ $label"
    else
        echo "  ✗ $label: exit $got, want $want"
        fail=1
    fi
}
case_file() { printf '%s\n' "$@" >"$t"; }

echo "check-test-sleeps self-test"

expect 1 "empty tree: no test files is an error, not a pass" bash "$guard"

case_file 'fn t() {' '    std::thread::sleep(d);' '}'
expect 1 "std::thread::sleep is flagged" bash "$guard"
case_file 'async fn t() {' '    tokio::time::sleep(d).await;' '}'
expect 1 "tokio::time::sleep is flagged" bash "$guard"
case_file 'async fn t() {' '    tokio::time::sleep_until(at).await;' '}'
expect 1 "tokio::time::sleep_until is flagged" bash "$guard"
case_file 'use tokio::time::{Duration, sleep};'
expect 1 "importing sleep is flagged" bash "$guard"
case_file 'use std::thread::sleep;'
expect 1 "importing thread::sleep is flagged" bash "$guard"
case_file 'fn t() {' '    #[expect(clippy::disallowed_methods, reason = "x")]' '    let a = 1;' \
    '    std::thread::sleep(d);' '}'
expect 1 "an annotation on the previous statement does not cover the next" bash "$guard"

case_file 'fn t() {' '    #[expect(' '        clippy::disallowed_methods,' \
    '        reason = "the deadline is the subject"' '    )]' '    std::thread::sleep(d);' '}'
expect 0 "an annotated statement passes" bash "$guard"
case_file '//! std::thread::sleep(d) in a doc' '/// tokio::time::sleep(d)' \
    '/* std::thread::sleep(d)' ' * tokio::time::sleep(d) */' \
    'fn t() { let s = "sleep 5"; wait_for_sleep(1); let x = 1; // thread::sleep(d)' '}'
expect 0 "comments, shell strings and look-alike names pass" bash "$guard"
case_file 'fn t() {' '    std::thread::sleep(d);' '}'
cp "$t" "$work/crates/k/src/lib.rs"
expect 1 "a src file is out of scope, but the test file is still caught" bash "$guard"
expect 0 "a src file passed explicitly is skipped" bash "$guard" "$work/crates/k/src/lib.rs"
expect 1 "a test file passed explicitly is scanned" bash "$guard" "$t"

hook() { printf '%s' "$1" | bash "$guard" --hook; }
expect 2 "hook: a raw sleep in a test file reports exit 2" hook "{\"tool_input\":{\"file_path\":\"$t\"}}"
expect 0 "hook: a src file is skipped" hook "{\"tool_input\":{\"file_path\":\"$work/crates/k/src/lib.rs\"}}"
expect 0 "hook: a payload without a file is skipped" hook '{"tool_input":{"command":"ls"}}'
case_file 'fn t() {}'
expect 0 "hook: a clean test file passes" hook "{\"tool_input\":{\"file_path\":\"$t\"}}"

if ((fail)); then
    echo "✗ check-test-sleeps self-test failed" >&2
    exit 1
fi
echo "✓ check-test-sleeps self-test passed"
