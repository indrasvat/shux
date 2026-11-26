#!/usr/bin/env bash
# scripts/flake-gate-selftest.sh — prove flake-gate.sh selects what a branch touched.
#
# Runs the shipped gate in a throwaway git repo, with a stub `cargo` and a stub
# test-stress.sh that prints the filter it was handed, against one change per
# selection rule and changes that must select nothing. The stub lists one test
# for any filter, plus "<binary> <name>" for each single test the filter names,
# except the one in FLAKE_SELFTEST_UNLISTED.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
spans="$here/../lint/ast-grep/test-spans.yml"
for need in "$here/flake-gate.sh" "$here/test-only-files.sh" "$spans"; do
    [[ -f "$need" ]] || { echo "✗ missing $need" >&2; exit 1; }
done
command -v ast-grep >/dev/null || { echo "✗ no ast-grep on PATH (make flake-gate puts the pinned one there)" >&2; exit 1; }

work="$(mktemp -d "${TMPDIR:-/tmp}/shux-flake-selftest.XXXXXX")"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin" "$work/repo/scripts" "$work/repo/lint/ast-grep"
cat >"$work/bin/cargo" <<'EOF'
#!/usr/bin/env bash
while (($#)) && [[ "$1" != -E ]]; do shift; done
echo "k::t"
grep -oE 'binary_id\(=[^)]*\) & test\(=[^)]*\)' <<<"${2:-}" |
    sed -E 's/^binary_id\(=(.*)\) & test\(=(.*)\)$/\1 \2/' |
    grep -vxF "${FLAKE_SELFTEST_UNLISTED:-none}" || true
EOF
chmod +x "$work/bin/cargo"
cd "$work/repo"
git init -q -b main
git config user.email selftest@example.invalid
git config user.name selftest
cp "$here/flake-gate.sh" "$here/test-only-files.sh" scripts/
cp "$spans" lint/ast-grep/
# shellcheck disable=SC2016  # the stub prints its own environment when it runs
printf '#!/usr/bin/env bash\necho "FILTER: $STRESS_FILTER"\n' >scripts/test-stress.sh

w() { mkdir -p "$(dirname "$1")"; printf '%s\n' "${@:2}" >"$1"; }
w crates/p/src/lib.rs 'pub mod foo;' '#[cfg(any(test, fuzzing))]' 'pub mod fz;' 'pub fn a() {}' \
    '#[cfg(test)]' 'mod tests {' '    #[test]' '    fn t() {}' '}'
w crates/p/src/foo/mod.rs '#[cfg(test)]' 'mod tests;' '#[cfg(test)]' 'pub(crate) mod harness;' \
    '#[cfg(test)]' 'pub(crate) mod lonely;' 'pub fn b() {}'
w crates/p/src/foo/tests.rs '#[test]' 'fn t() { let x = 1; super::harness::h(); }'
w crates/p/src/foo/harness.rs 'pub fn h() { let x = 1; }'
w crates/p/src/foo/lonely.rs 'pub fn l() { let x = 1; }'
w crates/p/src/fz.rs '#[test]' 'fn t() { let x = 1; }'
w crates/p/src/allcfg.rs 'pub fn c() {}' '#[cfg(all(test, unix))]' 'mod tests {' '    #[test]' '    fn t() { let x = 1; }' '}'
w crates/p/src/cmt.rs 'pub fn c() {}' '#[cfg(test)]' '// unit tests' 'mod tests {' '    #[test]' '    fn t() { let x = 1; }' '}'
w crates/p/src/pubmod.rs 'pub fn c() {}' '#[cfg(test)]' 'pub(crate) mod tests {' '    #[test]' '    fn t() { let x = 1; }' '}'
w crates/p/src/helperfn.rs 'pub fn c() { let y = 1; }' '#[cfg(test)]' 'fn helper() { let x = 1; }' \
    '#[cfg(test)]' 'mod tests {' '    #[test]' '    fn t() {}' '}'
w crates/p/src/fns.rs 'pub fn c() { let y = 1; }' '' '#[cfg(test)]' 'mod tests {' '    use super::*;' '' \
    '    const LIMIT: u8 = 1;' '' '    macro_rules! check { () => { let m = 1; }; }' '' \
    '    /// Doc.' '    #[test]' '    fn a() { let a = 1; }' '' \
    '    #[test]' '    fn b() {' '        let b = 1;' '        let b2 = 1;' '    }' '' \
    '    #[test]' '    fn gone() { let g = 1; }' '' '    #[test]' '    fn old_name() { let r = 1; }' '' \
    '    fn outer() {' '        #[test]' '        fn nested() { let n = 1; }' '    }' '' \
    '    const TEXT: &str = "' '// not a comment' '";' '' \
    '    mod inner {' '        #[test]' '        fn d() { let d = 1; }' '    }' '' \
    '    proptest! {' '        #![proptest_config(ProptestConfig::with_cases(4))]' '        #[test]' \
    '        fn prop(x in 0..4u8) { let p = x; }' '    }' '    // trailing note' '}'
w crates/p/src/early.rs 'use std::fmt;' '#[cfg(test)]' 'use std::fmt::Write;' \
    '#[cfg(test)]' '#[path = "early_more.rs"]' 'mod more;' 'pub fn c() { let y = 1; }' \
    '#[cfg(test)]' 'mod tests {' '    #[test]' '    fn t() { let x = 1; }' '}'
w crates/p/src/orphan.rs '#[cfg(test)]' 'mod tests {' '    #[test]' '    fn t() { let x = 1; }' '}' \
    'pub fn g(v: u8) -> u8 {' '    match v {' '        #[cfg(test)]' '        9 => 0,' '        _ => 1,' '    }' '}'
w crates/p/src/main.rs 'fn main() {}' '#[cfg(test)]' 'mod tests {' '    #[test]' '    fn t() { let x = 1; }' '}'
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
git add crates .shux scripts lint
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
fns() { perl -0pi -e "$1" crates/p/src/fns.rs; }
fns_coarse='binary_id(=p) & test(/^fns::/)'

case_ "an integration-test binary, outside its tests" "echo 'const C: u8 = 0;' >>crates/p/tests/old.rs" 'binary_id(=p::old)'
case_ "an integration test, inside it" "perl -pi -e 's/fn t\\(\\) \\{\\}/fn t() { let z = 0; }/' crates/p/tests/old.rs" 'binary_id(=p::old) & test(=t)'
case_ "a test in a test module in a file of its own" "edit crates/p/src/foo/tests.rs" 'binary_id(=p) & test(=foo::tests::t)'
case_ "a use in a test module in a file of its own" "perl -0pi -e 's/^/use std::fmt;\\n/' crates/p/src/foo/tests.rs" 'binary_id(=p) & test(/^foo::tests::/)'
case_ "a test-only helper with no tests: the tests that name it" "edit crates/p/src/foo/harness.rs" 'binary_id(=p) & test(/^foo::tests::/)'
case_ "a test-only helper nothing names: the whole unit-test binary" "edit crates/p/src/foo/lonely.rs" 'binary_id(=p)'
case_ "a file its parent gates on cfg(any(test, fuzzing))" "edit crates/p/src/fz.rs" 'binary_id(=p) & test(=fz::t)'
case_ "a cfg(all(test, unix)) module" "edit crates/p/src/allcfg.rs" 'binary_id(=p) & test(=allcfg::tests::t)'
case_ "a comment between cfg(test) and the module" "edit crates/p/src/cmt.rs" 'binary_id(=p) & test(=cmt::tests::t)'
case_ "a pub(crate) test module" "edit crates/p/src/pubmod.rs" 'binary_id(=p) & test(=pubmod::tests::t)'
case_ "a cfg(test) helper fn above the test module" "edit crates/p/src/helperfn.rs" 'binary_id(=p) & test(/^helperfn::/)'
case_ "production code above all cfg(test) code" "perl -pi -e 's/let y = 1;/let y = 2;/' crates/p/src/helperfn.rs" -
case_ "a shared helper declared as mod and as pub mod" "edit crates/p/tests/common/mod.rs" 'binary_id(=p::a)' 'binary_id(=p::b)'
case_ "a helper reached by #[path]" "edit crates/p/tests/sub/helper.rs" 'binary_id(=p::a)'
case_ "a renamed test binary" "git mv crates/p/tests/old.rs crates/p/tests/new.rs" 'binary_id(=p::new) & test(=t)'
case_ "a production change in lib.rs" "perl -pi -e 's/pub fn a\\(\\) \\{\\}/pub fn a() { let y = 0; }/' crates/p/src/lib.rs" -
case_ "a fixture a test names" "echo two >>crates/p/tests/data/input.txt" 'binary_id(=p::a)'
case_ "a proptest regressions file" "echo 'cc 01' >>crates/p/tests/a.proptest-regressions" 'binary_id(=p::a)'
case_ "a .shux fixture a test names" "echo y >>.shux/fixtures/frames/frame.bin" 'binary_id(=p::b)'
case_ "shux-testkit's own tests" "edit crates/shux-testkit/src/lib.rs" 'binary_id(=shux-testkit) & test(=tests::t)'

echo "  single tests (crates/p/src/fns.rs)"
case_ "a line inside a test" "fns 's/let a = 1;/let a = 2;/'" 'binary_id(=p) & test(=fns::tests::a)'
case_ "a test's doc comment" "fns 's|/// Doc\\.|/// Docs.|'" 'binary_id(=p) & test(=fns::tests::a)'
case_ "a test's attribute" "fns 's/#\\[test\\]\\n    fn b/#[tokio::test]\\n    async fn b/'" 'binary_id(=p) & test(=fns::tests::b)'
case_ "a line deleted from inside a test" "fns 's/        let b2 = 1;\\n//'" 'binary_id(=p) & test(=fns::tests::b)'
case_ "a new test" "fns 's/(        let b2 = 1;\\n    \\}\\n)/\$1\\n    #[test]\\n    fn fresh() {}\\n/'" 'binary_id(=p) & test(=fns::tests::fresh)'
case_ "a deleted test selects nothing" "fns 's/    #\\[test\\]\\n    fn gone\\(\\) \\{ let g = 1; \\}\\n\\n//'" -
case_ "a renamed test: the new name" "fns 's/fn old_name/fn new_name/'" 'binary_id(=p) & test(=fns::tests::new_name)'
case_ "two tests" "fns 's/let a = 1;/let a = 2;/; s/let d = 1;/let d = 2;/'" \
    'binary_id(=p) & test(=fns::tests::a)' 'binary_id(=p) & test(=fns::tests::inner::d)'
case_ "a test in a nested mod" "fns 's/let d = 1;/let d = 2;/'" 'binary_id(=p) & test(=fns::tests::inner::d)'
case_ "a proptest! fn" "fns 's/let p = x;/let p = x + 1;/'" 'binary_id(=p) & test(=fns::tests::prop)'
case_ "a comment on no item" "fns 's|// trailing note|// trailing notes|'" -
case_ "a test and a helper: the whole module" "fns 's/let a = 1;/let a = 2;/; s/const LIMIT: u8 = 1/const LIMIT: u8 = 2/'" "$fns_coarse"
case_ "a use in the test module" "fns 's/use super::\\*;/use super::c;/'" "$fns_coarse"
case_ "a const in the test module" "fns 's/const LIMIT: u8 = 1/const LIMIT: u8 = 2/'" "$fns_coarse"
case_ "a macro in the test module" "fns 's/let m = 1;/let m = 2;/'" "$fns_coarse"
case_ "a string line that reads as a comment" "fns 's|// not a comment|// not a comment!|'" "$fns_coarse"
case_ "a blank line in a string" "fns 's|// not a comment\\n|// not a comment\\n\\n|'" "$fns_coarse"
case_ "a test nested in a fn" "fns 's/let n = 1;/let n = 2;/'" "$fns_coarse"
case_ "a proptest! config, which every fn in it shares" "fns 's/with_cases\\(4\\)/with_cases(8)/'" "$fns_coarse"
case_ "production code above the test module" "fns 's/let y = 1;/let y = 2;/'" -
FLAKE_SELFTEST_UNLISTED='p fns::tests::a' \
    case_ "a test nextest does not list by that name: the whole module" "fns 's/let a = 1;/let a = 2;/'" "$fns_coarse"
case_ "production code after a cfg(test) use" "perl -pi -e 's/let y = 1;/let y = 2;/' crates/p/src/early.rs" -
case_ "a cfg(test) use" "perl -pi -e 's/fmt::Write;/fmt::Display;/' crates/p/src/early.rs" 'binary_id(=p) & test(/^early::/)'
case_ "a cfg(test) mod x; declaration: x's tests" "perl -pi -e 's/early_more/early_other/' crates/p/src/early.rs" 'binary_id(=p) & test(/^early::more::/)'
case_ "a new test module and its declaration" "perl -0pi -e 's/^/#[cfg(test)]\\nmod added;\\n/' crates/p/src/early.rs; w crates/p/src/early/added.rs '#[test]' 'fn t() {}'" \
    'binary_id(=p) & test(/^early::added::/)' 'binary_id(=p) & test(=early::added::t)'
case_ "cfg(test) on a match arm: test code to the end" "perl -pi -e 's/9 => 0/9 => 2/' crates/p/src/orphan.rs" 'binary_id(=p) & test(/^orphan::/)'
case_ "a test in main.rs: the whole binary" "edit crates/p/src/main.rs" 'binary_id(=p)'

# Discovery that fails must fail the gate, never read as "nothing to gate".
git checkout -q -f main
git checkout -q -B feat
echo "// y" >>crates/p/tests/old.rs
git commit -qam change
cp scripts/test-only-files.sh "$work/test-only-files.sh"
printf '#!/usr/bin/env bash\nexit 3\n' >scripts/test-only-files.sh
if out="$(PATH="$work/bin:$PATH" BASE=main bash scripts/flake-gate.sh 2>&1)"; then
    echo "  ✗ a failing test-only-files.sh fails the gate: it exited 0: $out"
    fail=1
else
    echo "  ✓ a failing test-only-files.sh fails the gate"
fi
cp "$work/test-only-files.sh" scripts/test-only-files.sh

# Without ast-grep the gate cannot narrow, and must say so rather than guess.
no_sg_path="$(tr ':' '\n' <<<"$PATH" | while IFS= read -r d; do [[ -x "$d/ast-grep" ]] || printf '%s:' "$d"; done)"
if out="$(PATH="$work/bin:$no_sg_path" BASE=main "$BASH" scripts/flake-gate.sh 2>&1)"; then
    echo "  ✗ a missing ast-grep fails the gate: it exited 0: $out"
    fail=1
elif [[ "$out" != *"no ast-grep on PATH"* ]]; then
    echo "  ✗ a missing ast-grep fails the gate: it failed, but not for that: $out"
    fail=1
else
    echo "  ✓ a missing ast-grep fails the gate"
fi

if ((fail)); then
    printf '\033[1;31m✗ flake-gate self-test failed\033[0m\n'
    exit 1
fi
printf '\033[1;32m✓ flake-gate self-test passed\033[0m\n'
