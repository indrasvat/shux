#!/usr/bin/env bash
# scripts/flake-gate.sh — run the tests a branch adds or changes, many times,
# under CPU load, before they merge (docs/testing.md#the-flake-gate).
#
# "Touched" is read from `git diff BASE...HEAD`:
#   crates/<pkg>/tests/<name>.rs       that integration-test binary
#   crates/<pkg>/tests/<dir>/…         every binary in <pkg> that declares `mod <dir>`
#                                      or a `#[path = "<dir>/…"]` module
#   crates/<pkg>/src/<path>.rs         the unit tests under that module path, when a
#                                      hunk reaches its `cfg(test)` code
#   a test-only src file               (scripts/test-only-files.sh) its module's tests;
#                                      if it holds none, the tests of the modules that
#                                      name it, else the package's whole unit-test binary
#   any other file under tests/ or     the tests whose sources name it (a proptest
#   .shux/fixtures/                    regressions file: the binary it is named after)
# shux-testkit's dependents are not re-run when it changes: that is most of the
# suite, which CI runs once anyway; its own tests are gated like any others.
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
# Captured, not read through `< <(…)`: a producer that fails must fail the gate.
diff_out="$(git diff --name-only --diff-filter=AMR "$base"...HEAD -- 'crates/*' '.shux/fixtures/*')"
mapfile -t changed <<<"$diff_out"
test_only_out="$(bash scripts/test-only-files.sh "$root")"

declare -A test_only=()
while IFS=$'\t' read -r file _ modp; do
    [[ -n "$file" ]] && test_only[$file]="$modp"
done <<<"$test_only_out"

# A test attribute: the set test-outside-cfg-test recognises.
test_attr='^[[:space:]]*#\[[[:space:]]*(::)?([A-Za-z_][A-Za-z0-9_]*::)*(test|rstest|proptest)([^A-Za-z0-9_]|$)|proptest!'

# Whether the diff reaches a src file's test code: a hunk at or past the first
# item under a `cfg` naming `test` (attributes and comments may sit between),
# not counting `mod x;`, whose code is x's file.
# Clippy's `items_after_test_module` keeps the test module last, so what comes
# after that line is test code, give or take a cfg(test) helper above it.
touches_tests() {
    local start
    start="$(awk '
        function cfg_test(a,  s) {
            s = a
            while (gsub(/not[[:space:]]*\([^()]*\)/, "", s)) {}
            return s ~ /^[[:space:]]*#!?\[[[:space:]]*cfg[[:space:]]*\(/ && s ~ /(^|[^A-Za-z0-9_])test([^A-Za-z0-9_]|$)/
        }
        cfg_test($0) { if (!at) at = FNR; next }
        at && (/^[[:space:]]*(#\[|\/\/)/ || /^[[:space:]]*$/) { next }
        at && /^[[:space:]]*(pub(\([^)]*\))?[[:space:]]+)?mod[[:space:]]+[A-Za-z0-9_]+[[:space:]]*;/ { at = 0; next }
        at && /^[[:space:]]*(pub(\([^)]*\))?[[:space:]]+)?(mod|fn|impl|async fn|const|static|struct|enum|use)[[:space:](<]/ { print at; exit }
        { at = 0 }' "$1")"
    [[ -n "$start" ]] || return 1
    git diff -U0 "$base"...HEAD -- "$1" |
        awk -v start="$start" '/^@@/ {
            split($3, r, ","); first = substr(r[1], 2); n = (r[2] == "" ? 1 : r[2])
            if (first + n - 1 >= start || first >= start) hit = 1
        } END { exit !hit }'
}

terms=()
# users_of <helper> <pkg>: the unit tests of every module in <pkg> that names
# the test-only helper as `<leaf>::`; fails when none does, or when a user is
# itself a helper with no tests, whose own users this does not follow.
users_of() {
    local f="$1" pkg="$2" leaf="${test_only[$1]##*::}" user path found=()
    while IFS= read -r user; do
        [[ -n "$user" && "$user" != "$f" ]] || continue
        if [[ -n "${test_only[$user]+set}" ]]; then
            grep -qE "$test_attr" "$user" || return 1
            path="${test_only[$user]}"
        else
            path="${user#"crates/$pkg/src/"}"
            path="${path%.rs}"
            path="${path%/mod}"
            [[ "$path" != lib && "$path" != main ]] || return 1
            path="${path//\//::}"
        fi
        found+=("binary_id(=$pkg) & test(/^${path}::/)")
    done < <(grep -rlE --include='*.rs' "(^|[^A-Za-z0-9_])${leaf}::" "crates/$pkg/src" || true)
    ((${#found[@]})) || return 1
    terms+=("${found[@]}")
}

# select_rs <file> [test]: the tests a change to this .rs file can break;
# "test" when the change is known to reach test code (a fixture it names).
select_rs() {
    local f="$1" known="${2:-}"
    [[ "$f" =~ ^crates/([^/]+)/(tests|src)/(.+)\.rs$ ]] || return 0
    local pkg="${BASH_REMATCH[1]}" kind="${BASH_REMATCH[2]}" rest="${BASH_REMATCH[3]}"
    if [[ "$kind" == tests && "$rest" != */* ]]; then
        terms+=("binary_id(=$pkg::$rest)")
    elif [[ "$kind" == tests ]]; then
        local dir="${rest%%/*}" user
        while IFS= read -r user; do
            terms+=("binary_id(=$pkg::$(basename "$user" .rs))")
        done < <(grep -lE "^[[:space:]]*(#\[[[:space:]]*path[[:space:]]*=[[:space:]]*\"$dir/[^\"]*\"[[:space:]]*\]|(pub(\([^)]*\))?[[:space:]]+)?mod[[:space:]]+${dir}[[:space:]]*;)" \
            "crates/$pkg/tests/"*.rs || true)
    elif [[ -n "${test_only[$f]+set}" ]]; then
        if [[ -f "$f" ]] && grep -qE "$test_attr" "$f"; then
            terms+=("binary_id(=$pkg) & test(/^${test_only[$f]}::/)")
        else
            users_of "$f" "$pkg" || terms+=("binary_id(=$pkg)")
        fi
    else
        [[ "$known" == test ]] || touches_tests "$f" || return 0
        local path="${rest%/mod}"
        case "$path" in
        lib | main) terms+=("binary_id(=$pkg)") ;;
        *) terms+=("binary_id(=$pkg) & test(/^${path//\//::}::/)") ;;
        esac
    fi
}

for f in ${changed[@]+"${changed[@]}"}; do
    if [[ "$f" == *.rs ]]; then
        select_rs "$f"
    elif [[ "$f" =~ ^crates/([^/]+)/tests/([^/]+)\.proptest-regressions$ ]]; then
        select_rs "crates/${BASH_REMATCH[1]}/tests/${BASH_REMATCH[2]}.rs"
    elif [[ "$f" == crates/*/tests/* || "$f" == .shux/fixtures/* ]]; then
        # A fixture: the test sources that name it, or else its directory.
        users=""
        for needle in "$(basename "$f")" "$(basename "$(dirname "$f")")"; do
            users="$(grep -rslF --include='*.rs' -- "$needle" crates/*/tests crates/*/src || true)"
            [[ -z "$users" ]] || break
        done
        while IFS= read -r user; do
            [[ -z "$user" ]] || select_rs "$user" test
        done <<<"$users"
    fi
done

if ((${#terms[@]} == 0)); then
    echo "flake-gate: no test code changed since $(git rev-parse --short "$base"); nothing to gate"
    exit 0
fi
mapfile -t terms < <(printf '%s\n' "${terms[@]}" | sort -u)
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
