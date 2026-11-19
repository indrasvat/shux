#!/usr/bin/env bash
# scripts/check-test-ignores-selftest.sh — prove check-test-ignores.sh can FAIL.
#
# Runs the shipped guard, copied into a throwaway tree, with a stub `gh` that
# answers an open issue for #1, a closed one for #2, an open pull request for
# #4, and fails for anything else.

set -euo pipefail

src="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/check-test-ignores.sh"
[[ -f "$src" ]] || { echo "✗ cannot find check-test-ignores.sh" >&2; exit 1; }

work="$(mktemp -d "${TMPDIR:-/tmp}/shux-ignores-selftest.XXXXXX")"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/tree/scripts" "$work/tree/crates/k/tests" "$work/bin"
cp "$src" "$work/tree/scripts/"
cat >"$work/bin/gh" <<'EOF'
#!/usr/bin/env bash
[[ "$1" == api ]] || { echo "gh: unexpected call: $*" >&2; exit 1; }
case "${2##*/}" in
1) echo "open false" ;;
2) echo "closed false" ;;
4) echo "open true" ;;
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
t '#[test]' '#[cfg_attr(unix, ignore = "waits on a VT fix: see #1")]' 'fn t() {}'
expect 0 "a cfg_attr ignore naming an open issue passes" guard
t '#[test]' '#[ignore = r"see #1"]' 'fn t() {}'
expect 0 "a raw-string reason is read" guard
t '/* #[ignore] */' '#[test]' 'fn ok() {}' '// #[ignore]' 'const S: &str = "a // b";'
expect 0 "an ignore inside a comment is not one" guard

# Every one of these must fail.
bad=(
    $'#[test] #[ignore]\nfn t() {}'
    $'#[test]\n#[cfg_attr(target_os = "linux", ignore)]\nfn t() {}'
    $'#[test]\n#[cfg_attr(all(unix, not(windows)), ignore = "flaky")]\nfn t() {}'
    $'#[test]\n#[cfg_attr(unix, ignore = "see #2")]\nfn t() {}'
    $'#[test]\n#[ignore = "see #2"] #[allow(x)]\nfn t() {}'
    $'#[test]\n#[ignore = "flaky"] // see #1\nfn t() {}'
    $'#[test]\n#[ignore = "see #2, then see #1"]\nfn t() {}'
    $'#[test]\n#[ignore = "see #1"]\nfn t() {}\n#[test]\n#[ignore = "see #2"]\nfn u() {}'
    $'#[test]\n#[ignore =\n    "flaky"]\nfn t() {}'
    $'#[test]\n#[ignore="see #2"]\nfn t() {}'
    $'#[test]\n#[ignore = r"see #2"]\nfn t() {}'
    $'#[test]\n#[ignore = "a \\"q\\" see #2"]\nfn t() {}'
    $'macro_rules! m { () => { #[test] #[ignore] fn t() {} } }'
    $'#[test]\n#[ignore = "see #01"]\nfn t() {}'
    $'#[test]\n#[ignore = "see #4"]\nfn t() {}'
)
for code in "${bad[@]}"; do
    t "$code"
    expect 1 "fails: ${code//$'\n'/ ⏎ }" guard
done

if ((fail)); then
    printf '\033[1;31m✗ check-test-ignores self-test failed\033[0m\n'
    exit 1
fi
printf '\033[1;32m✓ check-test-ignores self-test passed\033[0m\n'
