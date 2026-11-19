#!/usr/bin/env bash
# scripts/check-test-ignores-selftest.sh — prove check-test-ignores.sh can FAIL.
#
# Runs the shipped guard, copied into a throwaway tree, with a stub `gh` that
# answers OPEN for #1, CLOSED for #2 and fails for anything else.

set -euo pipefail

src="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/check-test-ignores.sh"
[[ -f "$src" ]] || { echo "✗ cannot find check-test-ignores.sh" >&2; exit 1; }

work="$(mktemp -d "${TMPDIR:-/tmp}/shux-ignores-selftest.XXXXXX")"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/tree/scripts" "$work/tree/crates/k/tests" "$work/bin"
cp "$src" "$work/tree/scripts/"
cat >"$work/bin/gh" <<'EOF'
#!/usr/bin/env bash
case "$3" in
1) echo OPEN ;;
2) echo CLOSED ;;
*) echo "gh: not found" >&2; exit 1 ;;
esac
EOF
chmod +x "$work/bin/gh"
guard() { env PATH="$work/bin:$PATH" bash "$work/tree/scripts/check-test-ignores.sh"; }
t() { printf '%s\n' "$@" >"$work/tree/crates/k/tests/t.rs"; }

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

echo "check-test-ignores self-test"

expect 1 "empty tree: no sources is an error, not a pass" guard
t '#[test]' 'fn ok() {}'
expect 0 "no ignores passes" guard
t '#[test]' '#[ignore]' 'fn t() {}'
expect 1 "a bare #[ignore] fails" guard
t '#[test]' '#[ignore = "flaky, will fix"]' 'fn t() {}'
expect 1 "a reason naming no issue fails" guard
t '#[test]' '#[ignore = "blocked on #1"]' 'fn t() {}'
expect 1 "an issue named without \"see #N\" fails" guard
t '#[test]' '#[ignore = "flaky under load: see #2"]' 'fn t() {}'
expect 1 "an ignore whose issue is closed fails" guard
t '#[test]' '#[ignore = "see #3"]' 'fn t() {}'
expect 1 "an issue gh cannot read fails, never passes unchecked" guard
t '#[test]' '#[ignore = "waits on a VT fix: see #1"]' 'fn t() {}'
expect 0 "an ignore naming an open issue passes" guard
t '#[test]' '#[ignore = "generator: writes fixtures; run by hand"]' 'fn t() {}'
expect 0 "a generator passes" guard
t '/// never #[ignore] a flaky test' '#[test]' 'fn ok() {}'
expect 0 "a doc comment mentioning #[ignore] is not one" guard

if ((fail)); then
    printf '\033[1;31m✗ check-test-ignores self-test failed\033[0m\n'
    exit 1
fi
printf '\033[1;32m✓ check-test-ignores self-test passed\033[0m\n'
