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
# Within a .rs file, a change that stays inside test functions selects just
# those tests (docs/testing.md#the-flake-gate, lint/ast-grep/test-spans.yml).
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

# A test attribute (test-spans.yml's test-attr), or a cfg_attr that may be one.
test_attr='^[[:space:]]*#\[[[:space:]]*(::)?([A-Za-z_][A-Za-z0-9_]*::)*(test|rstest|proptest)[[:space:]]*[](]|proptest!|cfg_attr[[:space:]]*\(.*(^|[^A-Za-z0-9_])test([^A-Za-z0-9_]|$)'

# units <pkg>: every unit-test binary of <pkg>: its lib's, and its bins'.
units() {
    if [[ -f "crates/$1/src/main.rs" || -d "crates/$1/src/bin" ]]; then
        echo "binary_id(=$1) + binary_id($1::bin/*)"
    else
        echo "binary_id(=$1)"
    fi
}

# test_spans <file>: "s <first> <last> <path in file>" per test function, its
# attributes and doc comments included; "t <first> <last>" per stretch of
# cfg(test) code; "m <first> <last> <path in file>" per cfg(test) `mod x;`;
# "c <line> <column>" per line comment; "q <first> <last>" per string over
# several lines. An s or m span claims no line it shares with other code.
test_spans() {
    local out
    out="$(ast-grep scan -r "$spans_rules" --json=stream "$1")"
    jq -r '.metaVariables.single as $v |
        [.ruleId, .range.start.line + 1, .range.start.column, .range.end.line + 1, .range.end.column,
         .range.byteOffset.start, .range.byteOffset.end, ($v.NAME.text // "-"),
         (($v.LEAD.range.start.line // .range.start.line) + 1),
         ($v.LEAD.range.start.column // .range.start.column)] | @tsv' <<<"$out" |
        sort -t$'\t' -k6,6n -k7,7nr |
        awk -F'\t' -v OFS='\t' '
            function path(b,  i, p) {
                for (i = 1; i <= nm; i++) if (ms[i] < b && b < me[i]) p = p mn[i] "::"
                return p
            }
            # The first line a span starting at (line, column) may claim: not
            # one with code before it, a `#` aside (an attribute in proptest!).
            function from(l, c,  p) {
                p = substr(src[l], 1, c)
                sub(/#[[:space:]]*$/, "", p)
                return p ~ /[^[:space:]]/ ? l + 1 : l
            }
            function to(l, c) { return substr(src[l], c + 1) ~ /[^[:space:]]/ ? l - 1 : l }
            FILENAME == ARGV[1] { src[FNR] = $0; next }
            $1 == "mod" { nm++; ms[nm] = $6; me[nm] = $7; mn[nm] = $8 }
            $1 == "cmt" && !($2 in seen) { seen[$2] = 1; print "c", $2, $3 }
            $1 == "str" { print "q", $2, $4 }
            $1 == "cfgt" { print "t", $9, $4 }
            $1 == "cfgm" { print "m", from($9, $10), to($4, $5), path($6) $8 }
            $1 == "cfgo" { print "t", $2, 2147483647 }
            $1 == "cfgf" { print "t", 1, 2147483647 }
            $1 == "fn" { print "s", from($9, $10), to($4, $5), path($6) $8 }
            $1 == "plead" && !pl { pl = $2; pc = $3 }
            $1 == "ptattr" { pt = 1 }
            $1 == "pname" { ps = pl ? from(pl, pc) : from($2, $3); ok = pt; pl = pt = 0 }
            $1 == "ptest" && ok { print "s", ps, to($4, $5), path($6) $8 }' "$1" -
}

# touched_lines <side> <all|cfg> <spans> <diff> <source>: what one side (old
# or new) of the diff changes in test code (all: the whole file; cfg: its
# cfg(test) code, and any test function): "f <path>" per test function it
# reaches ("g <path>" when it removes all of it), "d <path>" per cfg(test)
# `mod x;` in a cfg file, and "coarse <line>" with the first line it reaches
# outside all of those. Blank and comment-only lines reach nothing, but a
# blank line in a string is content.
touched_lines() {
    awk -F'\t' -v side="$1" -v mode="$2" '
        FILENAME == ARGV[1] && $1 == "s" { ns++; s0[ns] = $2; s1[ns] = $3; sp[ns] = $4; next }
        FILENAME == ARGV[1] && $1 == "t" { nt++; t0[nt] = $2; t1[nt] = $3; next }
        FILENAME == ARGV[1] && $1 == "m" && mode == "cfg" { nd++; d0[nd] = $2; d1[nd] = $3; dp[nd] = $4; next }
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
            hit = 0
            for (i = 1; i <= nd; i++) if (d0[i] <= FNR && FNR <= d1[i]) { decl[dp[i]] = 1; hit = 1 }
            for (i = 1; i <= ns; i++) if (s0[i] <= FNR && FNR <= s1[i]) { fns[i] = 1; hit = 1 }
            if (hit) next
            hit = (mode == "all")
            for (i = 1; i <= nt && !hit; i++) hit = (t0[i] <= FNR && FNR <= t1[i])
            if (!hit) next
            match($0, /^[[:space:]]*/)
            if ((FNR in cmt) && cmt[FNR] == RLENGTH) next
            if (!coarse) coarse = FNR
        }
        END {
            for (i in fns) {
                gone = 1
                for (l = s0[i]; l <= s1[i] && gone; l++) gone = (l in want)
                print (gone ? "g" : "f") "\t" sp[i]
            }
            for (p in decl) print "d\t" p
            if (coarse) print "coarse\t" coarse
        }' "$3" "$4" "$5"
}

terms=()
# exact term -> the coarse term of its file; terms that may list nothing.
declare -A coarse_of=() may_be_empty=()
# fn_select <known> <file> <binary id> <module prefix> <all|cfg> <coarse term>:
# the tests a change to <file> reaches, one term each, into terms; the coarse
# term instead when it reaches test code outside a test function, or when
# <known> is "test" (a fixture it names changed). "all": the whole file is
# test code; "cfg": its cfg(test) code and its test functions are.
fn_select() {
    local known="$1" f="$2" bin="$3" prefix="${4:+$4::}" mode="$5" coarse="$6" side src why=""
    local -A reached=() gone=() declared=() present=()
    local kind name
    if [[ "$known" == test ]]; then
        terms+=("$coarse")
        return 0
    fi
    git diff -U0 --text "$base"...HEAD -- "$f" >"$tmp/diff"
    # The diff's line numbers are HEAD's, whatever the working tree holds.
    git show "HEAD:$f" >"$tmp/new.rs"
    if git cat-file -e "$base:$f" 2>/dev/null; then git show "$base:$f" >"$tmp/old.rs"; else : >"$tmp/old.rs"; fi
    for side in new old; do
        src="$tmp/$side.rs"
        test_spans "$src" >"$tmp/$side.spans"
        touched_lines "$side" "$mode" "$tmp/$side.spans" "$tmp/diff" "$src" >"$tmp/$side.hit"
        while IFS=$'\t' read -r kind name; do
            case "$side:$kind" in
            *:coarse) why="line $name ($side side) is test code outside a test function" ;;
            # A declaration changed, not just added: what it loads may have.
            old:d) why="it changes the cfg(test) declaration of mod $name" ;;
            new:d) declared[$name]=1 ;;
            *:f) reached[$name]=1 ;;
            old:g) gone[$name]=1 ;;
            new:g) reached[$name]=1 ;;
            esac
        done <"$tmp/$side.hit"
    done
    while IFS=$'\t' read -r kind _ _ name; do
        if [[ "$kind" == s ]]; then present[$name]=1; fi
    done <"$tmp/new.spans"
    # A test whose old lines all went, and that the new side lacks, is deleted
    # (a rename's new name is reached on its own). One the new side lacks but
    # whose old lines partly stay became something else: what calls it may break.
    for name in "${!reached[@]}"; do
        if [[ -z "$why" && -z "${present[$name]+set}" ]]; then why="test $name is no longer a test"; fi
    done
    if [[ -n "$why" ]]; then
        echo "flake-gate: $f: $why; selecting $coarse" >&2
        terms+=("$coarse")
        return 0
    fi
    for name in "${!gone[@]}"; do
        if [[ -n "${present[$name]+set}" ]]; then reached[$name]=1; fi
    done
    # A new cfg(test) `mod x;`: its file is new too, and selected on its own.
    for name in "${!declared[@]}"; do
        terms+=("binary_id(=$bin) & test(/^$prefix$name::/)")
        may_be_empty["binary_id(=$bin) & test(/^$prefix$name::/)"]=1
    done
    for name in "${!reached[@]}"; do
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
    if [[ "$kind" == tests && "$rest" != */* ]]; then
        fn_select "$known" "$f" "$pkg::$rest" "" all "binary_id(=$pkg::$rest)"
    elif [[ "$kind" == tests ]]; then
        local dir="${rest%%/*}" user
        while IFS= read -r user; do
            terms+=("binary_id(=$pkg::$(basename "$user" .rs))")
        done < <(grep -lE "^[[:space:]]*(#\[[[:space:]]*path[[:space:]]*=[[:space:]]*\"$dir/[^\"]*\"[[:space:]]*\]|(#\[[^]]*\][[:space:]]*)*(pub(\([^)]*\))?[[:space:]]+)?mod[[:space:]]+${dir}[[:space:]]*;)" \
            "crates/$pkg/tests/"*.rs || true)
    elif grep -rqE --include='*.rs' "#\[[[:space:]]*path[[:space:]]*=[[:space:]]*\"([^\"]*/)?$(basename "${f//./\\.}")\"" "crates/$pkg/src"; then
        # Loaded by #[path]: its module path is not its file path.
        echo "flake-gate: $f is a #[path] module; selecting $(units "$pkg")" >&2
        terms+=("$(units "$pkg")")
    elif [[ -n "${test_only[$f]+set}" ]]; then
        if [[ -f "$f" ]] && grep -qE "$test_attr" "$f"; then
            fn_select "$known" "$f" "$pkg" "${test_only[$f]}" all "binary_id(=$pkg) & test(/^${test_only[$f]}::/)"
        else
            users_of "$f" "$pkg" || terms+=("binary_id(=$pkg)")
        fi
    else
        # The unit-test binary and module path: src/main.rs is the bin named
        # after the package, src/bin/<name>[/…].rs the bin <name>.
        local bin="$pkg" path="${rest%/mod}"
        if [[ "$path" == main ]]; then
            bin="$pkg::bin/$pkg" path=""
        elif [[ "$path" =~ ^bin/([^/]+)(/(.+))?$ ]]; then
            bin="$pkg::bin/${BASH_REMATCH[1]}" path="${BASH_REMATCH[3]}"
            [[ "$path" != main ]] || path=""
        elif [[ "$path" == lib ]]; then
            path=""
        fi
        path="${path//\//::}"
        fn_select "$known" "$f" "$bin" "$path" cfg "binary_id(=$bin)${path:+ & test(/^$path::/)}"
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
# wider <term>: the next wider term for one that lists nothing; empty for the
# widest. A single test -> its file's coarse term -> its package's unit tests.
wider() {
    local module='^binary_id\(=([^:)]+)(::bin/[^)]+)?\) & test\(/'
    if [[ -n "${coarse_of[$1]+set}" ]]; then
        echo "${coarse_of[$1]}"
    elif [[ "$1" =~ $module ]]; then
        units "${BASH_REMATCH[1]}"
    fi
}
# lists <term>: whether the listing holds a test the term names.
lists() {
    local prefix
    case "$1" in
    *" + "*) return 0 ;;
    *"& test(="*) [[ "$1" =~ ^binary_id\(=(.+)\)\ \&\ test\(=(.+)\)$ ]]; prefix="${BASH_REMATCH[1]} ${BASH_REMATCH[2]}" ;;
    *"& test(/^"*) [[ "$1" =~ ^binary_id\(=(.+)\)\ \&\ test\(/\^(.+)/\)$ ]]; prefix="${BASH_REMATCH[1]} ${BASH_REMATCH[2]}" ;;
    *) [[ "$1" =~ ^binary_id\(=(.+)\)$ ]]; prefix="${BASH_REMATCH[1]} " ;;
    esac
    awk -v p="$prefix" -v exact="$([[ "$1" == *"& test(="* ]] && echo 1)" \
        '(exact ? $0 == p : index($0, p) == 1) { found = 1; exit } END { exit !found }' <<<"$listed"
}
# A term that lists nothing was worked out wrong (a module moved by #[path],
# an rstest case, a module owned by main.rs) or holds no tests of its own (a
# cfg(test) helper others use): it is widened until it lists something. Only a
# new module's declaration may list nothing.
for _ in 1 2 3; do
    list
    widened=0
    for i in "${!terms[@]}"; do
        t="${terms[$i]}"
        [[ -z "${may_be_empty[$t]+set}" ]] || continue
        lists "$t" && continue
        wide="$(wider "$t")"
        [[ -n "$wide" ]] || continue
        echo "flake-gate: nextest lists nothing for $t; selecting $wide" >&2
        terms[i]="$wide"
        widened=1
    done
    ((widened)) || break
done
((widened == 0)) || list

# A filter can name only files with no tests in them; nextest would refuse it.
n="$(grep -c . <<<"$listed" || true)"
if ((n == 0)); then
    echo "flake-gate: the changed files hold no tests; nothing to gate"
    exit 0
fi
printf '\033[1;36m▶ flake gate: %d test(s), %d runs, %d CPU hogs\033[0m\n' "$n" "$count" "$load"
printf '%s\n' "${terms[@]}" | sed 's/^/    /'
STRESS_FILTER="$filter" STRESS_COUNT="$count" STRESS_LOAD="$load" bash scripts/test-stress.sh
