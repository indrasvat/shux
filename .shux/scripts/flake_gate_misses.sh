#!/usr/bin/env bash
# .shux/scripts/flake_gate_misses.sh — would this gate still catch the flakes
# the flake gate has caught before?
#
# Each line of the input names a commit a flake-gate run failed at and the
# test that failed there: "<commit> <binary_id> <test>". For each commit, this
# runs this checkout's gate selection against that commit's merge base with
# origin/main and checks the failed test is in the list nextest returns. Any
# test left out fails the run.
#
#   $1    the input (default .shux/flake-gate-catches.txt): `#` comments and
#         blank lines aside, CRLF or LF, a last line with no newline too

set -euo pipefail

root="$(git rev-parse --show-toplevel)"
input="${1:-$root/.shux/flake-gate-catches.txt}"
command -v ast-grep >/dev/null || { echo "✗ no ast-grep on PATH" >&2; exit 1; }
[[ -f "$input" ]] || { echo "✗ no file $input" >&2; exit 1; }
catches="$(awk '{ sub(/\r$/, "") } !/^[[:space:]]*(#|$)/' "$input")"
[[ -n "$catches" ]] || { echo "✗ no catches in $input" >&2; exit 1; }
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target}"

scratch="$(mktemp -d "${TMPDIR:-/tmp}/shux-flake-misses.XXXXXX")"
wt="$scratch/wt"
cleanup() {
    git -C "$root" worktree remove --force "$wt" 2>/dev/null || true
    rm -rf "$scratch"
}
trap cleanup EXIT
git -C "$root" worktree add -q --detach "$wt" origin/main
# shellcheck disable=SC2016  # expands when the stub runs
printf '#!/usr/bin/env bash\nprintf "%%s" "$STRESS_FILTER" >"$FLAKE_PROOF_FILTER"\n' >"$scratch/stress-stub.sh"

# select_at <commit>: the tests this checkout's gate selects there, into $scratch/set.
select_at() {
    local commit="$1" base
    # A PR's commits outlive its squash merge only under refs/pull/*: fetch by id.
    git -C "$root" cat-file -e "$commit^{commit}" 2>/dev/null || git -C "$root" fetch -q origin "$commit"
    git -C "$wt" checkout -q -- . 2>/dev/null || true
    git -C "$wt" clean -qfd -- lint scripts
    git -C "$wt" checkout -q --detach "$commit"
    base="$(git -C "$root" merge-base "$commit" origin/main)"
    mkdir -p "$wt/lint/ast-grep"
    cp "$root/scripts/flake-gate.sh" "$root/scripts/test-only-files.sh" "$wt/scripts/"
    cp "$root/lint/ast-grep/test-spans.yml" "$wt/lint/ast-grep/"
    cp "$scratch/stress-stub.sh" "$wt/scripts/test-stress.sh"
    : >"$scratch/filter"
    printf '\033[1;36m▶ %s: selecting\033[0m\n' "$commit"
    (cd "$wt" && BASE="$base" FLAKE_PROOF_FILTER="$scratch/filter" bash scripts/flake-gate.sh) >"$scratch/gate.log" 2>&1 || {
        status=$?
        tail -20 "$scratch/gate.log" >&2
        echo "✗ the gate failed at $commit (exit $status)" >&2
        exit "$status"
    }
    if [[ -s "$scratch/filter" ]]; then
        (cd "$wt" && cargo nextest list --color never --workspace -E "$(cat "$scratch/filter")" --message-format oneline) >"$scratch/set"
    else
        : >"$scratch/set"
    fi
}

fail=0
n=0
done_at=""
while read -r commit bin test; do
    n=$((n + 1))
    if [[ "$commit" != "$done_at" ]]; then
        # Not from the loop's stdin, which is the list of catches.
        select_at "$commit" </dev/null
        done_at="$commit"
    fi
    selected="$(grep -c . "$scratch/set" || true)"
    printf '  %s %s\n' "$bin" "$test"
    if grep -qxF "$bin $test" "$scratch/set"; then
        printf '\033[1;32m  ✓ kept, among %d selected\033[0m\n' "$selected"
    else
        fail=1
        printf '\033[1;31m  ✗ dropped: %d selected, not this one\033[0m\n' "$selected"
    fi
done <<<"$catches"
if ((fail)); then
    printf '\033[1;31m✗ flake gate misses: a caught flake is no longer selected\033[0m\n'
    exit 1
fi
printf '\033[1;32m✓ flake gate misses: all %d caught flakes still selected\033[0m\n' "$n"
