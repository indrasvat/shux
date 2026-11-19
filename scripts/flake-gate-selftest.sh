#!/usr/bin/env bash
# scripts/flake-gate-selftest.sh — prove flake-gate.sh selects what a branch touched.
#
# Runs the shipped gate in a throwaway git repo, with a stub `cargo` that lists
# one test for any filter and a stub test-stress.sh that prints the filter it
# was handed, against one change per selection rule and changes that must
# select nothing.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
for need in flake-gate.sh test-only-files.sh; do
    [[ -f "$here/$need" ]] || { echo "✗ missing $here/$need" >&2; exit 1; }
done

work="$(mktemp -d "${TMPDIR:-/tmp}/shux-flake-selftest.XXXXXX")"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin" "$work/repo/scripts"
printf '#!/usr/bin/env bash\necho "k::t"\n' >"$work/bin/cargo"
chmod +x "$work/bin/cargo"
cd "$work/repo"
git init -q -b main
git config user.email selftest@example.invalid
git config user.name selftest
cp "$here/flake-gate.sh" "$here/test-only-files.sh" scripts/
# shellcheck disable=SC2016  # the stub prints its own environment when it runs
printf '#!/usr/bin/env bash\necho "FILTER: $STRESS_FILTER"\n' >scripts/test-stress.sh

w() { mkdir -p "$(dirname "$1")"; printf '%s\n' "${@:2}" >"$1"; }
w crates/p/src/lib.rs 'pub mod foo;' '#[cfg(any(test, fuzzing))]' 'pub mod fz;' 'pub fn a() {}' \
    '#[cfg(test)]' 'mod tests {' '    #[test]' '    fn t() {}' '}'
w crates/p/src/foo/mod.rs '#[cfg(test)]' 'mod tests;' '#[cfg(test)]' 'pub(crate) mod harness;' 'pub fn b() {}'
w crates/p/src/foo/tests.rs '#[test]' 'fn t() { let x = 1; }'
w crates/p/src/foo/harness.rs 'pub fn h() { let x = 1; }'
w crates/p/src/fz.rs '#[test]' 'fn t() { let x = 1; }'
w crates/p/src/allcfg.rs 'pub fn c() {}' '#[cfg(all(test, unix))]' 'mod tests {' '    #[test]' '    fn t() { let x = 1; }' '}'
w crates/p/src/cmt.rs 'pub fn c() {}' '#[cfg(test)]' '// unit tests' 'mod tests {' '    #[test]' '    fn t() { let x = 1; }' '}'
w crates/p/src/pubmod.rs 'pub fn c() {}' '#[cfg(test)]' 'pub(crate) mod tests {' '    #[test]' '    fn t() { let x = 1; }' '}'
w crates/p/src/helperfn.rs 'pub fn c() { let y = 1; }' '#[cfg(test)]' 'fn helper() { let x = 1; }' \
    '#[cfg(test)]' 'mod tests {' '    #[test]' '    fn t() {}' '}'
w crates/p/tests/common/mod.rs 'pub fn h() { let x = 1; }'
w crates/p/tests/sub/helper.rs 'pub fn h() { let x = 1; }'
w crates/p/tests/data/input.txt 'one'
w crates/p/tests/a.rs 'mod common;' '#[path = "sub/helper.rs"]' 'mod helper;' \
    '#[test] fn t() { let _ = include_str!("data/input.txt"); }'
w crates/p/tests/a.proptest-regressions 'cc 00'
w crates/p/tests/b.rs 'pub mod common;' '#[test] fn t() { let _ = "frame.bin"; }'
w crates/p/tests/old.rs '#[test] fn t() {}'
w crates/shux-testkit/src/lib.rs 'pub fn w() {}' '#[cfg(test)]' 'mod tests {' '    #[test]' '    fn t() { let x = 1; }' '}'
w .shux/fixtures/frames/frame.bin 'x'
git add crates .shux scripts
git commit -qm base

fail=0
# case <label> <change (shell)> <expected term>... ("-" for nothing to gate)
case_() {
    local label="$1" change="$2"
    shift 2
    git checkout -q -f main
    git checkout -q -B feat
    eval "$change"
    git add -A crates .shux
    git commit -qm change --allow-empty
    local out want got
    out="$(PATH="$work/bin:$PATH" BASE=main bash scripts/flake-gate.sh 2>&1)" || {
        echo "  ✗ $label: the gate exited non-zero: $out"
        fail=1
        return
    }
    if [[ "$1" == - ]]; then
        want="nothing to gate"
        got="$(grep -o 'nothing to gate' <<<"$out" || true)"
    else
        want="FILTER: $(printf '%s\n' "$@" | sort -u | awk '{ printf "%s(%s)", (NR > 1 ? " + " : ""), $0 }')"
        got="$(grep '^FILTER: ' <<<"$out" || true)"
    fi
    if [[ "$got" == "$want" ]]; then
        echo "  ✓ $label"
    else
        echo "  ✗ $label: got \"${got:-$out}\", want \"$want\""
        fail=1
    fi
}
edit() { perl -pi -e 's/let x = 1;/let x = 2;/' "$@"; }

echo "flake-gate self-test"
case_ "an integration-test binary" "edit crates/p/tests/old.rs; echo '// x' >>crates/p/tests/old.rs" 'binary_id(=p::old)'
case_ "a test module in a file of its own" "edit crates/p/src/foo/tests.rs" 'binary_id(=p) & test(/^foo::tests::/)'
case_ "a test-only helper with no tests: the whole unit-test binary" "edit crates/p/src/foo/harness.rs" 'binary_id(=p)'
case_ "a file its parent gates on cfg(any(test, fuzzing))" "edit crates/p/src/fz.rs" 'binary_id(=p) & test(/^fz::/)'
case_ "a cfg(all(test, unix)) module" "edit crates/p/src/allcfg.rs" 'binary_id(=p) & test(/^allcfg::/)'
case_ "a comment between cfg(test) and the module" "edit crates/p/src/cmt.rs" 'binary_id(=p) & test(/^cmt::/)'
case_ "a pub(crate) test module" "edit crates/p/src/pubmod.rs" 'binary_id(=p) & test(/^pubmod::/)'
case_ "a cfg(test) helper fn above the test module" "edit crates/p/src/helperfn.rs" 'binary_id(=p) & test(/^helperfn::/)'
case_ "production code above all cfg(test) code" "perl -pi -e 's/let y = 1;/let y = 2;/' crates/p/src/helperfn.rs" -
case_ "a shared helper declared as mod and as pub mod" "edit crates/p/tests/common/mod.rs" 'binary_id(=p::a)' 'binary_id(=p::b)'
case_ "a helper reached by #[path]" "edit crates/p/tests/sub/helper.rs" 'binary_id(=p::a)'
case_ "a renamed test binary" "git mv crates/p/tests/old.rs crates/p/tests/new.rs" 'binary_id(=p::new)'
case_ "a production change in lib.rs" "perl -pi -e 's/pub fn a\\(\\) \\{\\}/pub fn a() { let y = 0; }/' crates/p/src/lib.rs" -
case_ "a fixture a test names" "echo two >>crates/p/tests/data/input.txt" 'binary_id(=p::a)'
case_ "a proptest regressions file" "echo 'cc 01' >>crates/p/tests/a.proptest-regressions" 'binary_id(=p::a)'
case_ "a .shux fixture a test names" "echo y >>.shux/fixtures/frames/frame.bin" 'binary_id(=p::b)'
case_ "shux-testkit's own tests" "edit crates/shux-testkit/src/lib.rs" 'binary_id(=shux-testkit)'

if ((fail)); then
    printf '\033[1;31m✗ flake-gate self-test failed\033[0m\n'
    exit 1
fi
printf '\033[1;32m✓ flake-gate self-test passed\033[0m\n'
