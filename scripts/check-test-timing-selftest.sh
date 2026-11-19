#!/usr/bin/env bash
# scripts/check-test-timing-selftest.sh — prove check-test-timing.sh can FAIL.
#
# Runs the shipped guard and rules, copied into a throwaway tree, against every
# banned shape reintroduced (in an integration test and in a `#[cfg(test)]`
# module), against the shapes it must never flag, and against empty input, a
# missing rule, a broken config and the wrong ast-grep.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/.." && pwd)"
for need in "$here/check-test-timing.sh" "$repo/sgconfig.yml" "$repo/lint/ast-grep/rules"; do
    [[ -e "$need" ]] || { echo "✗ missing $need" >&2; exit 1; }
done

work="$(mktemp -d "${TMPDIR:-/tmp}/shux-timing-selftest.XXXXXX")"
trap 'rm -rf "$work"' EXIT
fresh() { # a clean copy of the guard, its config and rules, and no sources
    rm -rf "${work:?}/tree"
    mkdir -p "$work/tree/scripts" "$work/tree/crates/k/tests" "$work/tree/crates/k/src"
    cp "$here/check-test-timing.sh" "$work/tree/scripts/"
    cp "$repo/sgconfig.yml" "$work/tree/"
    cp -R "$repo/lint" "$work/tree/"
}
guard() { bash "$work/tree/scripts/check-test-timing.sh" "$@"; }
t() { printf '%s\n' "$@" >"$work/tree/crates/k/tests/t.rs"; }
u() { # a src file whose body sits in a `#[cfg(test)] mod tests`
    { echo 'pub fn prod() {}'; echo '#[cfg(test)]'; echo 'mod tests {'; printf '%s\n' "$@"; echo '}'; } \
        >"$work/tree/crates/k/src/lib.rs"
}
p() { printf '%s\n' "$@" >"$work/tree/crates/k/src/lib.rs"; }

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

echo "check-test-timing self-test"

fresh
expect 1 "empty tree: a scan of nothing is an error, not a pass" guard
t 'fn ok() {}'
expect 0 "a clean test file passes" guard

# Each banned shape, in an integration test and in a unit-test module.
shapes=(
    'fn t() { let deadline = Instant::now() + BUDGET; }'
    'fn t() { let took = started.elapsed(); }'
    'fn t() { assert!(stats.total_time_us < 100_000); }'
    'fn t() { assert!(waited_ms >= 5000); }'
    'fn t() { let r = rx.recv_timeout(Duration::from_millis(50)); }'
    'fn t() { std::thread::park_timeout(d); }'
    'async fn t() { assert!(tokio::time::timeout(SHORT, rx.recv()).await.is_err()); }'
    'async fn t() { if timeout(SHORT, sub.recv()).await.is_err() { return; } }'
    'fn t() { assert!(!poll_until(SHORT, || done())); }'
    'fn t() { let s = "sleep 0.1; echo ready"; }'
)
for shape in "${shapes[@]}"; do
    fresh
    t "$shape"
    expect 1 "tests/: $shape" guard
    fresh
    u "$shape"
    expect 1 "cfg(test): $shape" guard
    fresh
    p "pub $shape"
    expect 0 "production code may: $shape" guard
done

# Shapes that must pass.
fresh
t '#[tokio::test(start_paused = true)]' 'async fn t() { let d = Instant::now() + D; let e = d.elapsed(); }'
expect 0 "a test on a paused clock may read it" guard
t 'fn t() { let r = rx.recv_timeout(BUDGET); }'
expect 0 "a BUDGET-bounded timed wait bounds failure only" guard
t 'async fn t() { assert!(timeout(Duration::ZERO, f).await.is_err()); }'
expect 0 "a zero timeout is one poll" guard
t 'fn t() { let s = "trap : TERM; while :; do sleep 3600; done"; }'
expect 0 "a hold-open loop is not sleep-then-act" guard
t '// tokio::time::timeout(SHORT, rx.recv()).await.is_err()' 'fn ok() {}'
expect 0 "comments are not code" guard

# The escape needs its reason.
fresh
t 'fn t() {' '    // ast-grep-ignore: test-wall-clock' '    let took = started.elapsed();' '}'
expect 1 "a suppression without a reason is flagged" guard
t 'fn t() {' '    // why: the bound is one load can only loosen' '    // ast-grep-ignore: test-wall-clock' \
    '    let took = started.elapsed();' '}'
expect 0 "a reasoned suppression passes" guard

# A unit test the rules could not see.
fresh
p '#[test]' 'fn stray() { let d = Instant::now() + BUDGET; }'
expect 1 "a #[test] outside a cfg(test) module is flagged" guard
p '#[cfg(test)]' 'mod tests {' '    #[test]' '    fn inside() {}' '}'
expect 0 "a #[test] inside one passes" guard

# File and hook modes, with absolute paths as the hook sends them.
fresh
t 'fn t() { let took = started.elapsed(); }'
expect 1 "file mode flags the file" guard "$work/tree/crates/k/tests/t.rs"
expect 0 "file mode skips a non-Rust path" guard "$work/tree/sgconfig.yml"
hook_payload() { printf '{"tool_input":{"file_path":"%s"}}' "$1"; }
expect 2 "hook mode exits 2 on a finding" \
    bash -c "$(declare -f hook_payload); hook_payload '$work/tree/crates/k/tests/t.rs' | bash '$work/tree/scripts/check-test-timing.sh' --hook"
t 'fn ok() {}'
expect 0 "hook mode passes a clean file" \
    bash -c "$(declare -f hook_payload); hook_payload '$work/tree/crates/k/tests/t.rs' | bash '$work/tree/scripts/check-test-timing.sh' --hook"

# Broken machinery fails loudly.
fresh
t 'fn t() { let took = started.elapsed(); }'
rm "$work/tree/lint/ast-grep/rules/test-wall-clock.yml"
expect 1 "a deleted rule file fails the count" guard
fresh
t 'fn ok() {}'
printf 'ruleDirs: [nowhere]\n' >"$work/tree/sgconfig.yml"
expect 1 "a config that loads no rules fails" guard
fresh
t 'fn ok() {}'
mkdir -p "$work/bin"
printf '#!/bin/sh\necho "ast-grep 0.1.0"\n' >"$work/bin/ast-grep"
chmod +x "$work/bin/ast-grep"
expect 1 "the wrong ast-grep version fails" env PATH="$work/bin:$PATH" bash "$work/tree/scripts/check-test-timing.sh"

if ((fail)); then
    printf '\033[1;31m✗ check-test-timing self-test failed\033[0m\n'
    exit 1
fi
printf '\033[1;32m✓ check-test-timing self-test passed\033[0m\n'
