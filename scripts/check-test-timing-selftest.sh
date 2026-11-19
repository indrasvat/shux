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
    cp "$here/check-test-timing.sh" "$here/test-only-files.sh" "$work/tree/scripts/"
    cp "$repo/sgconfig.yml" "$work/tree/"
    cp -R "$repo/lint" "$work/tree/"
    # This tree has none of the repo's test-only src files.
    local r
    for r in "$work/tree/lint/ast-grep/rules/"*.yml; do
        grep -v '^  - "crates/[^"*]*/src/[^"*]*\.rs"$' "$r" >"$r.new"
        mv "$r.new" "$r"
    done
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
    local want="$1" label="${2//$'\n'/ ⏎ }"
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
    # The clock, through a `let`, an alias, or a duration method.
    'fn t() { let now = Instant::now(); let d = now + D; }'
    'fn t() { let d = I::now() + D; }'
    'fn t() { let d = Instant::now().checked_add(D).unwrap(); }'
    'fn t() { let took = Instant::now() - t0; }'
    'fn t() { let took = Instant::now().duration_since(t0); }'
    'fn t() { let d = SystemTime::now() + D; }'
    'fn t() { let dur = Instant::now() - t0; assert!(dur < LIMIT); }'
    'fn t() { while Instant::now() < deadline {} }'
    # Absence, read every way a short wait can be read.
    'async fn t() { timeout(SHORT, rx.recv()).await.unwrap_err(); }'
    'async fn t() { timeout(SHORT, rx.recv()).await.expect_err("nothing arrives"); }'
    'async fn t() { let r = timeout(SHORT, rx.recv()).await; assert!(r.is_err()); }'
    'async fn t() { let nothing = tokio::time::timeout(Duration::from_millis(50), sub.recv()).await; assert!(nothing.is_err(), "no event"); }'
    'async fn t() { let r = timeout(SHORT, rx.recv()).await; assert!(matches!(r, Err(_))); }'
    'async fn t() { assert!(matches!(timeout(SHORT, rx.recv()).await, Err(_))); }'
    'async fn t() { assert_eq!(timeout(SHORT, rx.recv()).await.is_err(), true); }'
    'async fn t() { assert!(!timeout(SHORT, rx.recv()).await.is_ok()); }'
    'async fn t() { if let Ok(_) = timeout(SHORT, rx.recv()).await { panic!() } }'
    'async fn t() { match timeout(SHORT, rx.recv()).await { Ok(_) => panic!("arrived"), Err(_) => {} } }'
    'async fn t() { loop { if timeout(SHORT, sub.recv()).await.is_err() { break; } } }'
    'fn t() { assert!(rx.recv_timeout(SHORT).is_err()); }'
    # BUDGET is the whole duration argument, never part of a name or a sum.
    'fn t() { let r = rx.recv_timeout(budget_for_nothing_ms(5)); }'
    'fn t() { let r = rx.recv_timeout(remaining_50ms); }'
    'async fn t() { assert!(timeout(BUDGET / 1000, rx.recv()).await.is_err()); }'
    # A sleep followed by any command, in any string form.
    'fn t() { let s = "sleep 0.2; ls"; }'
    'fn t() { let s = "sleep 1; rm f"; }'
    'fn t() { let s = "sleep 1 && kill -TERM $$"; }'
    'fn t() { let s = "sleep .5;echo x"; }'
    'fn t() { let s = r#"sleep 0.2; echo x"#; }'
    'fn t() { let s = concat!("sleep 0.2; ", "echo ready"); }'
    'fn t() { let s = format!("sleep {}; echo ready", d); }'
    $'fn t() { let s = "sleep 0.2\necho ready"; }'
    # Inside a macro's token tree.
    'fn t() { dbg!(start.elapsed()); }'
    'fn t() { tokio::select! { _ = rx.recv() => {}, _ = async { let d = Instant::now() + D; } => {} } }'
    'fn t() { proptest! { #[test] fn p(x in 0..1u8) { let d = Instant::now() + D; } } }'
    'fn t() { proptest! { #[test] fn p(x in 0..1u8) { let r = rx.recv_timeout(SHORT); } } }'
    # A paused clock only by its real attribute, or a pause that runs first.
    $'// start_paused = true\nfn t() { let d = Instant::now() + D; }'
    $'#[doc = "start_paused = true"]\nfn t() { let d = Instant::now() + D; }'
    'fn t() { if false { tokio::time::pause(); } let d = Instant::now() + D; }'
    'fn t() { let _ = || tokio::time::pause(); let d = Instant::now() + D; }'
    'fn t() { let d = Instant::now() + D; tokio::time::pause(); }'
)
for shape in "${shapes[@]}"; do
    fresh
    t "$shape"
    expect 1 "tests/: $shape" guard
    fresh
    u "$shape"
    expect 1 "cfg(test): $shape" guard
    # A proptest! block in src is a test outside cfg(test) code (below).
    [[ "$shape" == *proptest!* ]] && continue
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
passes=(
    'fn t() { let r = rx.recv_timeout(shux_testkit::BUDGET); }'
    'fn t() { record(Instant::now()); }'
    'fn t() { let at = Instant::now(); record(at); }'
    'fn t() { let r = Rec { at: Instant::now() }; }'
    'fn t() { let n = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(); }'
    'async fn t() { let v = timeout(Duration::from_secs(5), rx.recv()).await.expect("arrives"); }'
    'async fn t() { tokio::time::pause(); let d = Instant::now() + D; let e = d.elapsed(); }'
    'fn t() { for _ in 0..2 { let clock = test_clock::Frozen::new(); let e = s.elapsed(); } }'
    'fn t() { let s = "exec sleep 900\n"; }'
    $'fn t() { let s = "#!/bin/sh\nsleep 120\n"; }'
    'fn t() { let s = "while [ ! -e f ]; do sleep 0.05; done; echo go"; }'
    'fn t() { let s = "echo start; sleep 30; :"; }'
    'fn t() { let s = r#"trap : TERM; while :; do sleep 3600; done"#; }'
)
for shape in "${passes[@]}"; do
    fresh
    t "$shape"
    expect 0 "tests/ may: $shape" guard
done

# Every form of `cfg(test)` code is test code; `not(test)` is not.
cfg_forms=(
    $'#[cfg(all(test, unix))]\nmod tests { fn h() { let d = Instant::now() + D; } }'
    $'#[cfg(any(test, fuzzing))]\nmod testing { fn h() { let d = Instant::now() + D; } }'
    $'#[cfg(test)]\nfn helper() { let d = Instant::now() + D; }'
    $'#[cfg(test)]\nimpl Foo { fn helper() { let d = Instant::now() + D; } }'
    $'#[cfg(test)]\n// unit tests\nmod tests { fn h() { let d = Instant::now() + D; } }'
    $'#[cfg(test)]\n#[allow(x)]\nmod tests { fn h() { let d = Instant::now() + D; } }'
    $'mod a {\n#[cfg(test)]\nmod tests { fn h() { let d = Instant::now() + D; } } }'
    $'#![cfg(test)]\nfn h() { let d = Instant::now() + D; }'
)
for form in "${cfg_forms[@]}"; do
    fresh
    p "$form"
    expect 1 "src: $form" guard
done
fresh
p $'#[cfg(not(test))]\nmod real { fn h() { let d = Instant::now() + D; } }'
expect 0 "cfg(not(test)) code is production code" guard

# The escape needs its reason, and only one form of it is honoured.
fresh
t 'fn t() {' '    // ast-grep-ignore: test-wall-clock' '    let took = started.elapsed();' '}'
expect 1 "a suppression without a reason is flagged" guard
t 'fn t() {' '    // why: the bound is one load can only loosen' '    // ast-grep-ignore: test-wall-clock' \
    '    let took = started.elapsed();' '}'
expect 0 "a reasoned suppression passes" guard
bad_suppressions=(
    $'fn t() {\n    // why: .\n    // ast-grep-ignore: test-wall-clock\n    let took = started.elapsed();\n}'
    $'fn t() {\n    // why: load only loosens\n    // ast-grep-ignore\n    let took = started.elapsed();\n}'
    $'fn t() {\n    let took = started.elapsed(); // ast-grep-ignore\n}'
    $'fn t() {\n    let took = started.elapsed(); // ast-grep-ignore: test-wall-clock\n}'
    $'fn t() {\n    // why: load only loosens it\n    let took = started.elapsed(); // ast-grep-ignore: test-wall-clock\n}'
    $'fn t() {\n    /* ast-grep-ignore: test-wall-clock */\n    let took = started.elapsed();\n}'
    $'fn t() {\n    // why: load only loosens it\n    /* ast-grep-ignore: test-wall-clock */\n    let took = started.elapsed();\n}'
    $'fn t() {\n    // why: load only loosens it\n    /// ast-grep-ignore: test-wall-clock\n    let took = started.elapsed();\n}'
    $'// why: the whole file is exempt\n// ast-grep-ignore: test-wall-clock\n\nfn t() {\n    let took = started.elapsed();\n}'
    $'fn t() {\n    // why: load only loosens it\n\n    // ast-grep-ignore: test-wall-clock\n    let took = started.elapsed();\n}'
)
for form in "${bad_suppressions[@]}"; do
    t "$form"
    expect 1 "suppression: $form" guard
done

# A unit test the rules could not see.
fresh
p '#[test]' 'fn stray() { let d = Instant::now() + BUDGET; }'
expect 1 "a #[test] outside a cfg(test) module is flagged" guard
p '#[cfg(test)]' 'mod tests {' '    #[test]' '    fn inside() {}' '}'
expect 0 "a #[test] inside one passes" guard
for attr in '#[::tokio::test]' '#[test_log::test]' '#[rstest]' '#[tokio::test(flavor = "multi_thread")]'; do
    p "$attr" 'async fn stray() {}'
    expect 1 "$attr outside a cfg(test) module is flagged" guard
done
p 'proptest! { #[test] fn stray(x in 0..1u8) {} }'
expect 1 "a proptest! block outside a cfg(test) module is flagged" guard

# A src file its parent declares `#[cfg(test)] mod x;` is scanned whole only if
# the rules list it: one they do not is a hole, not a pass.
fresh
p '#[cfg(test)]' 'mod helper;'
printf '%s\n' 'pub fn h() {}' >"$work/tree/crates/k/src/helper.rs"
expect 1 "a test-only src file the rules do not list fails" guard

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
t 'fn t() { let took = started.elapsed(); }'
expect 1 "file mode flags a path relative to the working directory" \
    bash -c "cd '$work/tree/crates/k' && bash '$work/tree/scripts/check-test-timing.sh' tests/t.rs"
expect 1 "file mode fails a Rust path that does not exist" guard "$work/tree/crates/k/tests/gone.rs"
mkdir -p "$work/loose"
printf '%s\n' 'fn t() { let took = started.elapsed(); }' >"$work/loose/t.rs"
expect 1 "file mode fails a Rust file no sgconfig.yml governs" guard "$work/loose/t.rs"
# Another checkout (a sibling or nested worktree) is scanned with its own rules.
for other in "$work/sibling" "$work/tree/.claude/worktrees/nested"; do
    mkdir -p "$other/crates/k/tests"
    cp "$work/tree/sgconfig.yml" "$other/"
    cp -R "$work/tree/lint" "$other/"
    printf '%s\n' 'fn t() { let took = started.elapsed(); }' >"$other/crates/k/tests/t.rs"
    expect 2 "hook mode flags a file in another checkout: ${other#"$work"/}" \
        bash -c "$(declare -f hook_payload); hook_payload '$other/crates/k/tests/t.rs' | bash '$work/tree/scripts/check-test-timing.sh' --hook"
done

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
