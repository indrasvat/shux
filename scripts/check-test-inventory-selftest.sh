#!/usr/bin/env bash
# scripts/check-test-inventory-selftest.sh — prove check-test-inventory.sh's
# comparison fails a test that vanished, and passes only the removals a
# TEST-MOVE: or TEST-RETIRE: trailer accounts for.
#
# Runs the shipped script's `--compare` in a throwaway repo whose commits carry
# the trailers, on hand-written lists in `normalize`'s form.

set -euo pipefail
export LC_ALL=C

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/shux-inventory-selftest.XXXXXX")"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/repo/scripts"
cp "$here/check-test-inventory.sh" "$work/repo/scripts/"
cd "$work/repo"
git init -q -b main
git config user.email selftest@example.invalid
git config user.name selftest
git commit -q --allow-empty -m base
base="$(git rev-parse HEAD)"

# lst <file> <binary-id> <leaf>...: a list in normalize's form.
lst() {
    local f="$1" bin="$2"
    shift 2
    printf "$bin\t%s\t1\n" "$@" | sort >"$f"
}
lst "$work/base" shux::m0 a b
lst "$work/two" shux::m0 a b
lst "$work/one" shux::m0 a
printf 'shux::m0\ta\t1\nshux::m0\tb\t1\nshux\tc\t1\n' | sort >"$work/base3"
printf 'shux::m0\ta\t1\nshux::m0\tb\t1\nshux-core\tc\t1\n' | sort >"$work/moved"
: >"$work/none"

fail=0
# expect <label> <want exit> <base list> <current list> [trailer]...
expect() {
    local label="$1" want="$2" b="$3" c="$4" status=0
    shift 4
    git checkout -q -f main
    git checkout -q -B feat
    local msg="change"
    if (($#)); then msg="$(printf 'change\n\n%s\n' "$(printf '%s\n' "$@")")"; fi
    git commit -q --allow-empty -m "$msg"
    bash scripts/check-test-inventory.sh --compare "$b" "$c" "$base" >"$work/out" 2>&1 || status=$?
    if ((status == want)); then
        echo "  ✓ $label"
    else
        echo "  ✗ $label: exit $status, want $want"
        sed 's/^/      /' "$work/out"
        fail=1
    fi
}

echo "check-test-inventory self-test"
expect "nothing removed passes" 0 "$work/base" "$work/two"
expect "a removed test fails" 1 "$work/base" "$work/one"
expect "a removed test fails though an unrelated TEST-MOVE: trailer is there" 1 "$work/base" "$work/one" \
    'TEST-MOVE: something else moved'
expect "TEST-RETIRE: naming the test lets it go" 0 "$work/base" "$work/one" \
    'TEST-RETIRE: shux::m0 b duplicated by c'
expect "TEST-RETIRE: naming another test does not" 1 "$work/base" "$work/one" \
    'TEST-RETIRE: shux::m0 a duplicated by c'
expect "TEST-RETIRE: in another binary does not" 1 "$work/base" "$work/one" \
    'TEST-RETIRE: shux::other b duplicated by c'
expect "TEST-RETIRE: without a reason fails" 1 "$work/base" "$work/one" \
    'TEST-RETIRE: shux::m0 b'
expect "a whole binary retired, and gone, passes" 0 "$work/base" "$work/none" \
    'TEST-RETIRE: shux::m0 * tested a copy of production'
expect "a whole binary retired, but still listing a test, fails" 1 "$work/base" "$work/one" \
    'TEST-RETIRE: shux::m0 * tested a copy of production'
expect "a move to another package with TEST-MOVE: passes" 0 "$work/base3" "$work/moved" \
    'TEST-MOVE: c moved to shux-core'
expect "a move to another package without it fails" 1 "$work/base3" "$work/moved"

if ((fail)); then
    printf '\033[1;31m✗ check-test-inventory self-test failed\033[0m\n'
    exit 1
fi
printf '\033[1;32m✓ check-test-inventory self-test passed\033[0m\n'
