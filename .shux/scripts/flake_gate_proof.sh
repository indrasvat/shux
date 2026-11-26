#!/usr/bin/env bash
# shellcheck disable=SC2016  # the edits below are perl expressions, single-quoted for perl
# .shux/scripts/flake_gate_proof.sh — prove, on real builds, that the flake
# gate selects fewer tests than the per-module gate it replaced, and never
# drops the test a change edits or adds.
#
# For each kind of change it commits a synthetic edit to a scratch worktree of
# HEAD, runs both gates' selection (the old one from FLAKE_PROOF_BEFORE), lists
# what each filter selects with `cargo nextest list`, and asserts:
#   a change inside test functions   after ⊂ before (strictly), and after
#                                    holds the edited or added test
#   a change to a test helper        after ⊇ before: the old answer was right
#
#   $1                  count (default) or time: also run each gate's 20 runs
#   FLAKE_PROOF_BEFORE  the old gate's commit (default 8ef34f8, the last per-module one)
#   FLAKE_PROOF_OUT     where cases.jsonl and the selected sets go
#                       (default .shux/out/flake-gate-proof)

set -euo pipefail

mode="${1:-count}"
[[ "$mode" == count || "$mode" == time ]] || { echo "usage: $0 [count|time]" >&2; exit 2; }
root="$(git rev-parse --show-toplevel)"
before_ref="${FLAKE_PROOF_BEFORE:-8ef34f8}"
out="${FLAKE_PROOF_OUT:-$root/.shux/out/flake-gate-proof}"
command -v ast-grep >/dev/null || { echo "✗ no ast-grep on PATH (make flake-gate-proof puts the pinned one there)" >&2; exit 1; }
mkdir -p "$out"
: >"$out/cases.jsonl"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target}"

scratch="$(mktemp -d "${TMPDIR:-/tmp}/shux-flake-proof.XXXXXX")"
wt="$scratch/wt"
git -C "$root" show "$before_ref:scripts/flake-gate.sh" >"$scratch/gate-before.sh"
cp "$root/scripts/flake-gate.sh" "$scratch/gate-after.sh"
cp "$root/scripts/test-stress.sh" "$scratch/test-stress.sh"
cleanup() {
    git -C "$root" worktree remove --force "$wt" 2>/dev/null || true
    rm -rf "$scratch"
}
trap cleanup EXIT
git -C "$root" worktree add -q --detach "$wt" HEAD

# The stub test-stress.sh a count run gets: it hands back the filter.
# shellcheck disable=SC2016  # expands when the stub runs
printf '#!/usr/bin/env bash\nprintf "%%s" "$STRESS_FILTER" >"$FLAKE_PROOF_FILTER"\n' >"$scratch/stress-stub.sh"

fail=0
# pick <side>: run that gate on the scratch commit; the tests it selects
# into $scratch/<side>.set, its seconds into $scratch/<side>.secs.
pick() {
    local side="$1" start filter
    cp "$scratch/gate-$side.sh" "$wt/scripts/flake-gate.sh"
    : >"$scratch/filter"
    if [[ "$mode" == time ]]; then
        # Real runs: the filter still reaches the stub first, for the listing.
        cp "$scratch/stress-stub.sh" "$wt/scripts/test-stress.sh"
        (cd "$wt" && BASE=HEAD~1 FLAKE_PROOF_FILTER="$scratch/filter" bash scripts/flake-gate.sh) >"$scratch/$side.log" 2>&1
        cp "$scratch/test-stress.sh" "$wt/scripts/test-stress.sh"
        start="$(date +%s)"
        (cd "$wt" && BASE=HEAD~1 bash scripts/flake-gate.sh)
        echo $(($(date +%s) - start)) >"$scratch/$side.secs"
    else
        cp "$scratch/stress-stub.sh" "$wt/scripts/test-stress.sh"
        (cd "$wt" && BASE=HEAD~1 FLAKE_PROOF_FILTER="$scratch/filter" bash scripts/flake-gate.sh) >"$scratch/$side.log" 2>&1
        echo null >"$scratch/$side.secs"
    fi
    filter="$(cat "$scratch/filter")"
    if [[ -z "$filter" ]]; then
        : >"$scratch/$side.set"
    else
        (cd "$wt" && cargo nextest list --color never --workspace -E "$filter" --message-format oneline) |
            sort -u >"$scratch/$side.set"
    fi
    git -C "$wt" checkout -q -- scripts
}

# case_ <name> <file> <perl edit> <narrow|superset> [<test it must keep>]
case_() {
    local name="$1" file="$2" edit="$3" kind="$4" must="${5:-}" b a extra
    git -C "$wt" checkout -q --detach HEAD 2>/dev/null
    git -C "$wt" reset -q --hard "$(git -C "$root" rev-parse HEAD)"
    perl -0pi -e "$edit" "$wt/$file"
    if git -C "$wt" diff --quiet; then
        printf '\033[1;31m✗ %s: the edit changed nothing\033[0m\n' "$name"
        fail=1
        return
    fi
    git -C "$wt" -c core.hooksPath=/dev/null -c user.name=flake-gate-proof -c user.email=flake-gate-proof@example.invalid commit -qam "flake gate proof: $name"
    printf '\033[1;36m▶ %s\033[0m\n' "$name"
    pick before
    pick after
    cp "$scratch/before.set" "$out/$name.before"
    cp "$scratch/after.set" "$out/$name.after"
    b="$(grep -c . "$scratch/before.set" || true)"
    a="$(grep -c . "$scratch/after.set" || true)"
    extra="$(comm -13 "$scratch/before.set" "$scratch/after.set" | grep -c . || true)"
    local ok=1 why=""
    if [[ -n "$must" ]] && ! grep -qxF "$must" "$scratch/after.set"; then
        ok=0 why="after drops $must"
    elif [[ "$kind" == narrow ]] && ! ((extra == 0 && a < b)); then
        ok=0 why="after is not a strict subset of before ($extra outside it)"
    elif [[ "$kind" == superset ]] && [[ -n "$(comm -23 "$scratch/before.set" "$scratch/after.set")" ]]; then
        ok=0 why="after misses tests before selects"
    fi
    printf '{"case":"%s","kind":"%s","before":{"tests":%d,"secs":%s},"after":{"tests":%d,"secs":%s},"must":"%s","ok":%s}\n' \
        "$name" "$kind" "$b" "$(cat "$scratch/before.secs")" "$a" "$(cat "$scratch/after.secs")" "$must" \
        "$([[ $ok == 1 ]] && echo true || echo false)" >>"$out/cases.jsonl"
    if ((ok)); then
        printf '\033[1;32m  ✓ before %d → after %d%s\033[0m\n' "$b" "$a" "${must:+, keeps $must}"
    else
        printf '\033[1;31m  ✗ before %d → after %d: %s\033[0m\n' "$b" "$a" "$why"
        fail=1
    fi
}

case_ unit-test-edit crates/shux/src/client.rs \
    's/(fn a_connection_reset_while_connecting_means_nothing_is_listening\(\) \{\n)/$1        let _proof = 1;\n/' \
    narrow 'shux client::tests::a_connection_reset_while_connecting_means_nothing_is_listening'
case_ unit-test-new crates/shux/src/client.rs \
    's/(\nmod tests \{\n)/$1    #[test]\n    fn flake_gate_proof_new() {}\n/' \
    narrow 'shux client::tests::flake_gate_proof_new'
case_ integration-test-edit crates/shux/tests/m0_integration.rs \
    's/(async fn test_m0_system_version\(\) \{\n)/$1    let _proof = 1;\n/' \
    narrow 'shux::m0_integration test_m0_system_version'
case_ integration-test-new crates/shux/tests/cli_integration.rs \
    's/\z/\n#[test]\nfn flake_gate_proof_new() {}\n/' \
    narrow 'shux::cli_integration flake_gate_proof_new'
case_ helper-change crates/shux/src/client.rs \
    's/(\nmod tests \{\n    use super::\*;\n)/$1\n    #[allow(dead_code)]\n    fn flake_gate_proof_helper() {}\n/' \
    superset

if ((fail)); then
    printf '\033[1;31m✗ flake gate proof failed (%s)\033[0m\n' "$out/cases.jsonl"
    exit 1
fi
printf '\033[1;32m✓ flake gate proof: every case holds (%s)\033[0m\n' "$out/cases.jsonl"
