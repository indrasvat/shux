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

expect 1 "empty tree: no sources is an error, not a pass" bash "$guard"

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
case_file 'fn t() {' '/*' 'std::thread::sleep(d);' '*/' '    let s = "std::thread::sleep(d)";' \
    '    sim::time::sleep(d);' '}'
expect 0 "unstarred block comments, string literals and other modules named time pass" bash "$guard"
case_file 'fn t() {' '    let g = "crates/*/tests";' '    std::thread::sleep(d);' '}'
expect 1 "a /* inside a string does not open a comment" bash "$guard"
case_file 'fn t() {' '    let u = "http://x"; std::thread::sleep(d);' '}'
expect 1 "a // inside a string does not end the code" bash "$guard"
case_file 'fn t() {' '    #[expect(clippy::disallowed_methods, reason = "x")] std::thread::sleep(d);' '}'
expect 0 "an annotation on the same line passes" bash "$guard"
case_file 'fn t() {' '    #[expect(clippy::disallowed_methods, reason = "x")]' '    h.spawn(move || {' \
    '        std::thread::sleep(d);' '    });' '}'
expect 0 "an annotated statement holding a closure passes" bash "$guard"
case_file '#[expect(clippy::disallowed_methods, reason = "x")]' 'async fn t() {' '    let a = 1;' \
    '    tokio::time::sleep(d).await;' '}'
expect 0 "an annotated fn covers its whole body" bash "$guard"
case_file 'async fn t() {' '    #[expect(clippy::disallowed_methods, reason = "x")]' '    tokio::select! {' \
    '        _ = rx.recv() => {}' '        _ = tokio::time::sleep(d) => {}' '    }' '}'
expect 0 "an annotated select! covers its arms" bash "$guard"
case_file 'fn t() {' '    #[expect(clippy::disallowed_methods, reason = "x")]' \
    '    let h = std::thread::spawn(move || {' '        let a = 1;' '        std::thread::sleep(d);' \
    '    });' '}'
expect 0 "an annotated statement covers every line of its closure" bash "$guard"
case_file '#[expect(clippy::disallowed_methods, reason = "x")]' 'fn t() {' '    std::thread::sleep(d);' \
    '}' 'fn u() {' '    std::thread::sleep(d);' '}'
expect 1 "an annotated fn does not cover the next one" bash "$guard"
case_file 'fn t() {' '    #[expect(clippy::unwrap_used, reason = "not clippy::disallowed_methods")]' \
    '    std::thread::sleep(d);' '}'
expect 1 "a reason that names the lint is not an annotation" bash "$guard"
case_file 'fn t() {' '    std::thread::sleep(d);' '}'
cp "$t" "$work/crates/k/src/lib.rs"
expect 1 "a src file is out of scope, but the test file is still caught" bash "$guard"
expect 0 "a src file is scanned for waivers only" bash "$guard" "$work/crates/k/src/lib.rs"
expect 1 "a test file passed explicitly is scanned" bash "$guard" "$t"

case_file 'fn t() {}'
printf '%s\n' 'fn f() {' '    #[allow(clippy::disallowed_methods)]' '    std::thread::sleep(d);' '}' \
    >"$work/crates/k/src/lib.rs"
expect 1 "allow of the ban in a src file is flagged" bash "$guard"
printf '%s\n' '#![expect(clippy::disallowed_methods, reason = "x")]' >"$work/crates/k/src/lib.rs"
expect 1 "a crate-wide expect of the ban is flagged" bash "$guard"
printf '%s\n' '#![allow(clippy::style)]' >"$work/crates/k/src/lib.rs"
expect 1 "allow of a lint group holding the ban is flagged" bash "$guard"
src_file() { printf '%s\n' "$@" >"$work/crates/k/src/lib.rs"; }
src_file '#![cfg_attr(test, allow(clippy::disallowed_methods))]'
expect 1 "an allow inside cfg_attr is flagged" bash "$guard"
src_file 'fn f() {' '    #[allow(' '        clippy::disallowed_methods' '    )]' '    std::thread::sleep(d);' '}'
expect 1 "an allow spread over lines is flagged" bash "$guard"
src_file '#![expect(clippy::style, reason = "x")]'
expect 1 "a crate-wide expect of a group holding the ban is flagged" bash "$guard"
src_file '/* banner **/' '#[allow(clippy::disallowed_methods)]' 'fn f() {}'
expect 1 "a block comment closed by a run of stars ends there" bash "$guard"
src_file '#[allow(clippy::allow_attributes_without_reason)]' 'fn f() {}'
expect 0 "a lint whose name starts with all is not a waiver" bash "$guard"
src_file 'fn f() {' '    let s = "a /* b";' '    let t = "' '  #[allow(clippy::disallowed_methods)] /*' '";' '}'
expect 0 "an attribute or comment inside a multi-line string is inert" bash "$guard"
src_file 'fn f() {}'
printf '%s\n' '[lints.clippy]' 'disallowed_methods = "allow"' >"$work/crates/k/Cargo.toml"
expect 1 "a Cargo.toml lint table relaxing the ban is flagged" bash "$guard"
printf '%s\n' '[workspace.lints.clippy]' 'all = { level = "allow", priority = -1 }' >"$work/crates/k/Cargo.toml"
expect 1 "a Cargo.toml lint table relaxing a group holding it is flagged" bash "$guard"
printf '%s\n' '[workspace.lints.clippy]' 'all = { level = "warn", priority = -1 }' 'style = "warn"' \
    >"$work/crates/k/Cargo.toml"
expect 0 "a Cargo.toml lint table that warns relaxes nothing under -D warnings" bash "$guard"
printf '%s\n' '[lints.clippy]' 'disallowed_methods = "deny"' >"$work/crates/k/Cargo.toml"
expect 0 "a Cargo.toml lint table that keeps the ban passes" bash "$guard"
rm "$work/crates/k/Cargo.toml"
case_file 'fn t() {' "    let q = '\"'; std::thread::sleep(d); let p = '\"';" '}'
expect 1 "a char literal holding a quote does not hide a sleep" bash "$guard"
case_file 'fn t() {' '    let r = r#"a "//" b"#; std::thread::sleep(d);' '}'
expect 1 "a raw string holding a quote does not hide a sleep" bash "$guard"
case_file 'fn t<'"'"'a>(x: &'"'"'a str) {' '    std::thread::sleep(d);' '}'
expect 1 "a lifetime is not a char literal" bash "$guard"
case_file 'fn t() {' '    let r = r#"std::thread::sleep(d)"#;' '}'
expect 0 "a sleep inside a raw string passes" bash "$guard"
printf '%s\n' 'fn f() {}' >"$work/crates/k/src/lib.rs"
case_file 'fn t() {' '    std::thread::sleep(d);' '}'

hook() { printf '%s' "$1" | bash "$guard" --hook; }
expect 2 "hook: a raw sleep in a test file reports exit 2" hook "{\"tool_input\":{\"file_path\":\"$t\"}}"
expect 0 "hook: a clean src file passes" hook "{\"tool_input\":{\"file_path\":\"$work/crates/k/src/lib.rs\"}}"
mkdir -p "$work/.claude/worktrees/w/crates/k/tests"
cp "$t" "$work/.claude/worktrees/w/crates/k/tests/t.rs"
expect 2 "hook: a test file in a worktree is checked too" \
    hook "{\"tool_input\":{\"file_path\":\"$work/.claude/worktrees/w/crates/k/tests/t.rs\"}}"
expect 2 "hook: a malformed payload is surfaced, not swallowed" hook '{"tool_input":'
nojq="$work/nojq"
mkdir -p "$nojq"
for tool in bash dirname cat; do ln -s "$(command -v "$tool")" "$nojq/$tool"; done
expect 2 "hook: a missing jq is surfaced, not swallowed" \
    env PATH="$nojq" bash "$guard" --hook <<<"{\"tool_input\":{\"file_path\":\"$t\"}}"
expect 0 "hook: a payload without a file is skipped" hook '{"tool_input":{"command":"ls"}}'
case_file 'fn t() {}'
expect 0 "hook: a clean test file passes" hook "{\"tool_input\":{\"file_path\":\"$t\"}}"

if ((fail)); then
    echo "✗ check-test-sleeps self-test failed" >&2
    exit 1
fi
echo "✓ check-test-sleeps self-test passed"
