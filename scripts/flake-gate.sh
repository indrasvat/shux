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
# Narrowed to single tests: when every hunk reaching the test code of
# tests/<name>.rs, a src file (not main.rs) or a test-only file with tests lies
# inside test functions (lint/ast-grep/test-spans.yml: attributes, doc comments
# and `proptest!` fns included; blank and comment-only lines aside), the gate
# selects just those tests, and a deleted one selects nothing. A hunk anywhere
# else in test code (a helper, a `use`, a const, a macro) keeps the rule above,
# and so does a test nextest does not list under the name worked out for it.
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
spans_rules="$root/lint/ast-grep/test-spans.yml"
command -v ast-grep >/dev/null ||
    { echo "flake-gate: no ast-grep on PATH (make flake-gate puts the pinned one there)" >&2; exit 1; }
tmp="$(mktemp -d "${TMPDIR:-/tmp}/shux-flake-gate.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

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

# test_spans <file>: "s <first> <last> <path in file>" per test function, its
# attributes and doc comments included; "t <first> <last>" per stretch of
# cfg(test) code; "c <line> <column>" per line comment; "q <first> <last>" per
# string over several lines.
test_spans() {
    local out
    out="$(ast-grep scan -r "$spans_rules" --json=stream "$1")"
    jq -r '[.ruleId, .range.start.line + 1, .range.start.column, .range.end.line + 1,
            .range.byteOffset.start, .range.byteOffset.end,
            (.metaVariables.single.NAME.text // "-")] | @tsv' <<<"$out" |
        sort -t$'\t' -k5,5n -k6,6nr |
        awk -F'\t' -v OFS='\t' '
            function path(b,  i, p) {
                for (i = 1; i <= nm; i++) if (ms[i] < b && b < me[i]) p = p mn[i] "::"
                return p
            }
            $1 == "mod" { nm++; ms[nm] = $5; me[nm] = $6; mn[nm] = $7 }
            $1 == "cmt" && !($2 in seen) { seen[$2] = 1; print "c", $2, $3 }
            $1 == "str" { print "q", $2, $4 }
            $1 == "cfga" { if (!cfg) cfg = $2 }
            $1 == "cfgt" { print "t", (cfg ? cfg : $2), $4; cfg = 0 }
            $1 == "cfgf" { print "t", 1, 2147483647 }
            $1 == "lead" || $1 == "plead" { if (!lead || $2 < lead) lead = $2 }
            $1 == "fn" { print "s", (lead ? lead : $2), $4, path($5) $7; lead = 0 }
            $1 == "pfn" { ps = (lead ? lead : $2); pn = path($5) $7; lead = 0; open = 1 }
            $1 == "pend" && open { print "s", ps, $4, pn; open = 0 }'
}

# touched_lines <side> <all|cfg> <spans> <diff> <source>: what one side (old
# or new) of the diff changes in test code (all: the whole file; cfg: its
# cfg(test) code): "f <path>" per test function it reaches, and "coarse
# <line>" with the first line it reaches outside every one. Blank and
# comment-only lines reach nothing, but a blank line in a string is content.
touched_lines() {
    awk -F'\t' -v side="$1" -v mode="$2" '
        FILENAME == ARGV[1] && $1 == "s" { ns++; s0[ns] = $2; s1[ns] = $3; sp[ns] = $4; next }
        FILENAME == ARGV[1] && $1 == "t" { nt++; t0[nt] = $2; t1[nt] = $3; next }
        FILENAME == ARGV[1] && $1 == "c" { cmt[$2] = $3; next }
        FILENAME == ARGV[1] && $1 == "q" { for (l = $2; l <= $3; l++) instr[l] = 1; next }
        FILENAME == ARGV[2] {
            if ($0 !~ /^@@ /) next
            split($0, h, " ")
            split(substr(side == "new" ? h[3] : h[2], 2), r, ",")
            for (l = r[1]; l < r[1] + (r[2] == "" ? 1 : r[2]); l++) want[l] = 1
            next
        }
        !(FNR in want) || (/^[[:space:]]*$/ && !(FNR in instr)) { next }
        {
            hit = (mode == "all")
            for (i = 1; i <= nt && !hit; i++) hit = (t0[i] <= FNR && FNR <= t1[i])
            if (!hit) next
            hit = 0
            for (i = 1; i <= ns; i++) if (s0[i] <= FNR && FNR <= s1[i]) { fns[sp[i]] = 1; hit = 1 }
            if (hit) next
            match($0, /^[[:space:]]*/)
            if ((FNR in cmt) && cmt[FNR] == RLENGTH) next
            if (!coarse) coarse = FNR
        }
        END { for (p in fns) print "f\t" p; if (coarse) print "coarse\t" coarse }' "$3" "$4" "$5"
}

terms=()
declare -A coarse_of=()
# fn_select <file> <binary id> <module prefix> <all|cfg> <coarse term>: the
# tests a change to <file> reaches, one term each, into terms; the coarse term
# instead when it reaches test code outside a test function. "all": the whole
# file is test code; "cfg": its cfg(test) code is.
fn_select() {
    local f="$1" bin="$2" prefix="${3:+$3::}" mode="$4" coarse="$5" side src
    local -A reached=() present=()
    local kind name
    git diff -U0 "$base"...HEAD -- "$f" >"$tmp/diff"
    # The diff's line numbers are HEAD's, whatever the working tree holds.
    git show "HEAD:$f" >"$tmp/new.rs"
    if git cat-file -e "$base:$f" 2>/dev/null; then git show "$base:$f" >"$tmp/old.rs"; else : >"$tmp/old.rs"; fi
    for side in new old; do
        src="$tmp/$side.rs"
        test_spans "$src" >"$tmp/$side.spans"
        touched_lines "$side" "$mode" "$tmp/$side.spans" "$tmp/diff" "$src" >"$tmp/$side.hit"
        while IFS=$'\t' read -r kind name; do
            if [[ "$kind" == coarse ]]; then
                echo "flake-gate: $f:$name ($side side) is test code outside a test function: $coarse" >&2
                terms+=("$coarse")
                return 0
            fi
            [[ -z "$kind" ]] || reached[$name]=1
        done <"$tmp/$side.hit"
    done
    if [[ -z "$bin" ]]; then
        ((${#reached[@]} == 0)) || terms+=("$coarse")
        return 0
    fi
    while IFS=$'\t' read -r kind _ _ name; do
        if [[ "$kind" == s ]]; then present[$name]=1; fi
    done <"$tmp/new.spans"
    # A test gone from the new side (deleted, or renamed: the new name is
    # reached on its own) selects nothing.
    for name in "${!reached[@]}"; do
        [[ -n "${present[$name]+set}" ]] || continue
        terms+=("binary_id(=$bin) & test(=$prefix$name)")
        coarse_of["binary_id(=$bin) & test(=$prefix$name)"]="$coarse"
    done
}

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
    if [[ "$kind" == tests && "$rest" != */* && "$known" != test ]]; then
        fn_select "$f" "$pkg::$rest" "" all "binary_id(=$pkg::$rest)"
    elif [[ "$kind" == tests && "$rest" != */* ]]; then
        terms+=("binary_id(=$pkg::$rest)")
    elif [[ "$kind" == tests ]]; then
        local dir="${rest%%/*}" user
        while IFS= read -r user; do
            terms+=("binary_id(=$pkg::$(basename "$user" .rs))")
        done < <(grep -lE "^[[:space:]]*(#\[[[:space:]]*path[[:space:]]*=[[:space:]]*\"$dir/[^\"]*\"[[:space:]]*\]|(pub(\([^)]*\))?[[:space:]]+)?mod[[:space:]]+${dir}[[:space:]]*;)" \
            "crates/$pkg/tests/"*.rs || true)
    elif [[ -n "${test_only[$f]+set}" ]]; then
        local coarse="binary_id(=$pkg) & test(/^${test_only[$f]}::/)"
        if [[ -f "$f" ]] && grep -qE "$test_attr" "$f"; then
            if [[ "$known" == test ]]; then
                terms+=("$coarse")
            else
                fn_select "$f" "$pkg" "${test_only[$f]}" all "$coarse"
            fi
        else
            users_of "$f" "$pkg" || terms+=("binary_id(=$pkg)")
        fi
    else
        local path="${rest%/mod}" coarse
        case "$path" in
        lib | main) coarse="binary_id(=$pkg)" ;;
        *) coarse="binary_id(=$pkg) & test(/^${path//\//::}::/)" ;;
        esac
        if [[ "$known" == test ]]; then
            terms+=("$coarse")
        elif [[ "$path" == main ]]; then
            # Not narrowed: main.rs's tests are not in binary_id(=$pkg).
            fn_select "$f" "" "" cfg "$coarse"
        elif [[ "$path" == lib ]]; then
            fn_select "$f" "$pkg" "" cfg "$coarse"
        else
            fn_select "$f" "$pkg" "${path//\//::}" cfg "$coarse"
        fi
    fi
}

for f in ${changed[@]+"${changed[@]}"}; do
    if [[ "$f" == *.rs ]]; then
        select_rs "$f"
    elif [[ "$f" =~ ^crates/([^/]+)/tests/([^/]+)\.proptest-regressions$ ]]; then
        select_rs "crates/${BASH_REMATCH[1]}/tests/${BASH_REMATCH[2]}.rs" test
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
list() {
    mapfile -t terms < <(printf '%s\n' "${terms[@]}" | sort -u)
    filter="$(printf '(%s) + ' "${terms[@]}")"
    filter="${filter% + }"
    listed="$(cargo nextest list --color never --workspace -E "$filter" --message-format oneline)"
}
list
# A test listed under another name than the one worked out for it (a module
# moved by #[path], an rstest case, a cfg'd-out test) is selected by its file's
# coarse term instead.
missed=0
for i in "${!terms[@]}"; do
    t="${terms[$i]}"
    [[ -n "${coarse_of[$t]+set}" && "$t" =~ ^binary_id\(=(.+)\)\ \&\ test\(=(.+)\)$ ]] || continue
    grep -qxF "${BASH_REMATCH[1]} ${BASH_REMATCH[2]}" <<<"$listed" && continue
    echo "flake-gate: nextest lists no ${BASH_REMATCH[1]} ${BASH_REMATCH[2]}; selecting ${coarse_of[$t]}" >&2
    terms[i]="${coarse_of[$t]}"
    missed=1
done
((missed == 0)) || list

# A filter can name only files with no tests in them; nextest would refuse it.
n="$(grep -c . <<<"$listed" || true)"
if ((n == 0)); then
    echo "flake-gate: the changed files hold no tests; nothing to gate"
    exit 0
fi
printf '\033[1;36m▶ flake gate: %d test(s), %d runs, %d CPU hogs\033[0m\n' "$n" "$count" "$load"
printf '%s\n' "${terms[@]}" | sed 's/^/    /'
STRESS_FILTER="$filter" STRESS_COUNT="$count" STRESS_LOAD="$load" bash scripts/test-stress.sh
