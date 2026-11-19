#!/usr/bin/env bash
# scripts/flake-gate.sh — run the tests a branch adds or changes, many times,
# under CPU load, before they merge (docs/testing.md#the-flake-gate).
#
# A test that passes once on an idle machine says little about a loaded CI
# runner, and process races (job control, `ps` seeing a fork, a pane exiting
# between two reads) are beyond any lint. So every test the branch touches runs
# FLAKE_COUNT times next to FLAKE_LOAD CPU hogs, through scripts/test-stress.sh,
# which fails on any failed run.
#
# "Touched" is read from `git diff BASE...HEAD`:
#   crates/<pkg>/tests/<name>.rs       that integration-test binary
#   crates/<pkg>/tests/<dir>/…         every binary in <pkg> that declares `mod <dir>`
#   crates/<pkg>/src/<path>.rs         the unit tests under that module path
#
#   BASE          ref to diff against   (default origin/main)
#   FLAKE_COUNT   runs                  (default 20)
#   FLAKE_LOAD    CPU hogs              (default twice the core count)

set -euo pipefail

base_ref="${BASE:-origin/main}"
count="${FLAKE_COUNT:-20}"
cores="$(getconf _NPROCESSORS_ONLN)"
load="${FLAKE_LOAD:-$((2 * cores))}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

base="$(git merge-base HEAD "$base_ref")" ||
    { echo "flake-gate: no merge base with $base_ref (is it fetched?)" >&2; exit 1; }
mapfile -t changed < <(git diff --name-only --diff-filter=AMR "$base"...HEAD -- 'crates/*.rs')

# Whether the diff changes a src file's `#[cfg(test)] mod`. Clippy's
# `items_after_test_module` keeps that module last, so any hunk ending at or
# past its first line touches test code; a change above it is production code,
# which the suite at large already runs.
touches_tests() {
    local start
    start="$(awk '/^[[:space:]]*#\[cfg\(test\)\]/ { at = FNR; next }
        at && /^[[:space:]]*#\[/ { next }
        at && /^[[:space:]]*(pub(\([^)]*\))? )?mod [A-Za-z_0-9]+ *\{/ { print at; exit }
        { at = 0 }' "$1")"
    [[ -n "$start" ]] || return 1
    git diff -U0 "$base"...HEAD -- "$1" |
        awk -v start="$start" '/^@@/ {
            split($3, r, ","); first = substr(r[1], 2); n = (r[2] == "" ? 1 : r[2])
            if (first + n - 1 >= start || first >= start) hit = 1
        } END { exit !hit }'
}

terms=()
add() {
    local t
    for t in ${terms[@]+"${terms[@]}"}; do [[ "$t" == "$1" ]] && return; done
    terms+=("$1")
}
for f in ${changed[@]+"${changed[@]}"}; do
    [[ "$f" =~ ^crates/([^/]+)/(tests|src)/(.+)\.rs$ ]] || continue
    pkg="${BASH_REMATCH[1]}" kind="${BASH_REMATCH[2]}" rest="${BASH_REMATCH[3]}"
    [[ "$pkg" == shux-testkit ]] && continue
    if [[ "$kind" == tests && "$rest" != */* ]]; then
        add "binary_id(=$pkg::$rest)"
    elif [[ "$kind" == tests ]]; then
        dir="${rest%%/*}"
        while IFS= read -r user; do
            add "binary_id(=$pkg::$(basename "$user" .rs))"
        done < <(grep -lE "^[[:space:]]*(#\[path = \"$dir/mod\.rs\"\]|mod $dir;)" "crates/$pkg/tests/"*.rs || true)
    else
        touches_tests "$f" || continue
        path="${rest%/mod}"
        case "$path" in
        lib | main) add "binary_id(=$pkg)" ;;
        *) add "binary_id(=$pkg) & test(/^${path//\//::}::/)" ;;
        esac
    fi
done

if ((${#terms[@]} == 0)); then
    echo "flake-gate: no test code changed since $(git rev-parse --short "$base"); nothing to gate"
    exit 0
fi
filter="$(printf '(%s) + ' "${terms[@]}")"
filter="${filter% + }"

# A filter can name only files with no tests in them; nextest would refuse it.
listed="$(cargo nextest list --color never --workspace -E "$filter" --message-format oneline)"
n="$(grep -c . <<<"$listed" || true)"
if ((n == 0)); then
    echo "flake-gate: the changed files hold no tests; nothing to gate"
    exit 0
fi
printf '\033[1;36m▶ flake gate: %d test(s), %d runs, %d CPU hogs\033[0m\n' "$n" "$count" "$load"
printf '%s\n' "${terms[@]}" | sed 's/^/    /'
STRESS_FILTER="$filter" STRESS_COUNT="$count" STRESS_LOAD="$load" bash scripts/test-stress.sh
