#!/usr/bin/env bash
# scripts/test-only-files.sh — the src files compiled only for tests.
#
# A file is test-only when its parent declares it under a `cfg` naming `test`
# (`#[cfg(test)] mod x;`, `cfg(all(test, …))`, `cfg(any(test, …))`, never
# `not(test)`), or when a test-only file declares it. Read from the parents'
# declarations, so a new one needs no list kept by hand.
#
# Usage: test-only-files.sh [ROOT]   (default: the checkout holding this script)
# Prints: <path>\t<package>\t<module path>, one per file, sorted.

set -euo pipefail

root="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
cd "$root"

# Every out-of-line `mod x;` a file declares: <name>\t<#[path] or ->\t<1 if cfg(test)>.
declared() {
    awk '
        function cfg_test(a,  s) {
            s = a
            while (gsub(/not[[:space:]]*\([^()]*\)/, "", s)) {}
            return s ~ /^#\[[[:space:]]*cfg[[:space:]]*\(/ && s ~ /(^|[^A-Za-z0-9_])test([^A-Za-z0-9_]|$)/
        }
        {
            line = $0
            sub(/^[[:space:]]+/, "", line)
            if (line ~ /^\/\// || line ~ /^\/\*.*\*\/[[:space:]]*$/) next
            # Leading attributes, one at a time; they carry over to the next line.
            while (match(line, /^#\[[^]]*\][[:space:]]*/)) {
                a = substr(line, 1, RLENGTH)
                sub(/[[:space:]]+$/, "", a)
                if (cfg_test(a)) test = 1
                if (a ~ /^#\[[[:space:]]*path[[:space:]]*=/) {
                    p = a
                    sub(/^[^"]*"/, "", p)
                    sub(/".*$/, "", p)
                }
                line = substr(line, RLENGTH + 1)
            }
            if (line == "") next
            if (match(line, /^(pub(\([^)]*\))?[[:space:]]+)?mod[[:space:]]+[A-Za-z_][A-Za-z0-9_]*[[:space:]]*;/)) {
                m = line
                sub(/^(pub(\([^)]*\))?[[:space:]]+)?mod[[:space:]]+/, "", m)
                sub(/[^A-Za-z0-9_].*$/, "", m)
                printf "%s\t%s\t%d\n", m, (p == "" ? "-" : p), test
            }
            test = 0
            p = ""
        }' "$1"
}

# The file a `mod <name>;` in <parent> resolves to, honouring #[path].
child_file() {
    local parent="$1" name="$2" path="$3" dir base
    dir="$(dirname "$parent")"
    base="$(basename "$parent" .rs)"
    if [[ "$path" != - ]]; then
        echo "$dir/$path"
        return
    fi
    case "$base" in
    mod | lib | main) ;;
    *) [[ "$parent" == */src/bin/*.rs ]] || dir="$dir/$base" ;;
    esac
    if [[ -f "$dir/$name.rs" ]]; then
        echo "$dir/$name.rs"
    else
        echo "$dir/$name/mod.rs"
    fi
}

# A src file's module path, from its location: crates/p/src/a/b.rs -> a::b.
mod_path() {
    local rest="${1#crates/*/src/}"
    rest="${rest%.rs}"
    rest="${rest%/mod}"
    case "$rest" in lib | main) echo "" ;; *) echo "${rest//\//::}" ;; esac
}

# Breadth-first: cfg(test) declarations seed the set, then everything a
# test-only file declares joins it.
queue=()
while IFS= read -r f; do
    while IFS=$'\t' read -r name path test; do
        ((test)) || continue
        child="$(child_file "$f" "$name" "$path")"
        [[ -f "$child" ]] || continue
        parent_mod="$(mod_path "$f")"
        queue+=("$child"$'\t'"${parent_mod:+$parent_mod::}$name")
    done < <(declared "$f")
done < <(find crates -path '*/src/*' -name '*.rs' -not -path '*/target/*' | sort)

declare -A seen=()
out=()
i=0
while ((i < ${#queue[@]})); do
    entry="${queue[i]}"
    i=$((i + 1))
    file="${entry%%$'\t'*}"
    modp="${entry#*$'\t'}"
    file="$(cd "$(dirname "$file")" && pwd -P)/$(basename "$file")"
    file="${file#"$(pwd -P)"/}"
    [[ -z "${seen[$file]:-}" ]] || continue
    seen[$file]=1
    pkg="${file#crates/}"
    pkg="${pkg%%/*}"
    out+=("$file"$'\t'"$pkg"$'\t'"$modp")
    while IFS=$'\t' read -r name path _; do
        child="$(child_file "$file" "$name" "$path")"
        [[ -f "$child" ]] && queue+=("$child"$'\t'"$modp::$name")
    done < <(declared "$file")
done

((${#out[@]} == 0)) || printf '%s\n' "${out[@]}" | sort
