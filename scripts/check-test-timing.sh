#!/usr/bin/env bash
# scripts/check-test-timing.sh — ban timing in disguise in test code: the
# ast-grep rules in lint/ast-grep/ (docs/testing.md#enforcement).
#
# Usage:
#   check-test-timing.sh           scan the tree
#   check-test-timing.sh FILE...   scan these files, each from the checkout that
#                                  holds it (non-Rust paths are skipped)
#   check-test-timing.sh --hook    read a Claude Code hook payload on stdin;
#                                  exit 2 with the findings on stderr
#
# It fails, never passes, when ast-grep is missing or the wrong version, when
# ast-grep loads a different number of rules than lint/ast-grep/rules defines,
# when a tree scan reads no file, when a Rust path cannot be scanned, and when
# the rules' list of test-only src files is not scripts/test-only-files.sh's.
#
# Exit: 0 clean, 1 findings or a failed check, 2 findings or any failure in
# --hook mode (Claude Code shows the agent a PostToolUse hook's stderr only on 2).

set -euo pipefail

# Rules defined under lint/ast-grep/rules. A rule deleted or renamed without
# updating this fails the scan instead of silently guarding less.
EXPECTED_RULES=11
AST_GREP_VERSION=0.51.3

hook=0
[[ "${1:-}" == "--hook" ]] && hook=1
if ((hook)); then
    trap 'echo "check-test-timing: hook failed (exit $?) — the timing check did not run" >&2; exit 2' ERR
fi
fail() {
    echo "check-test-timing: $*" >&2
    exit $((hook ? 2 : 1))
}

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"

command -v ast-grep >/dev/null ||
    fail "ast-grep is not installed: run scripts/ensure-ast-grep.sh"
have="$(ast-grep --version | awk '{print $2}')"
[[ "$have" == "$AST_GREP_VERSION" ]] ||
    fail "ast-grep $have found, $AST_GREP_VERSION expected: run scripts/ensure-ast-grep.sh"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# Every `ast-grep-ignore` is `// ast-grep-ignore: <rule>[, <rule>]` on a line
# of its own, under a `// why:` of three words or more and over a line of code.
# Checked in text: ast-grep also honours trailing, bare, block and file-level
# forms, and a trailing one suppresses a finding on its own line.
suppressions() {
    awk '
        function report(msg) { printf "%s:%d: %s\n", FILENAME, FNR, msg; bad = 1 }
        FNR == 1 { pending = 0; prev = "" }
        pending {
            if ($0 ~ /^[[:space:]]*$/ || $0 ~ /^[[:space:]]*\/[\/*]/)
                report("an ast-grep-ignore must sit directly over the line it suppresses")
            pending = 0
        }
        /ast-grep-ignore/ {
            if ($0 !~ /^[[:space:]]*\/\/[[:space:]]*ast-grep-ignore:[[:space:]]*[a-z0-9-]+([[:space:]]*,[[:space:]]*[a-z0-9-]+)*[[:space:]]*$/)
                report("write a suppression as `// ast-grep-ignore: <rule>` on a line of its own")
            else if (prev !~ /^[[:space:]]*\/\/[[:space:]]*why:([[:space:]]+[^[:space:]]*[A-Za-z][^[:space:]]*)([[:space:]]+[^[:space:]]*[A-Za-z][^[:space:]]*)([[:space:]]+[^[:space:]]*[A-Za-z][^[:space:]]*)/)
                report("an ast-grep-ignore needs `// why: <a reason of three words or more>` on the line above")
            pending = 1
        }
        { prev = $0 }
        END { exit bad }
    ' "$@"
}

found=0
report() { # report <text>: findings go to stdout, or stderr in hook mode
    if ((hook)); then echo "$1" >&2; else echo "$1"; fi
    found=1
}

# Scan one checkout: $1 its root, the rest paths relative to it (none: the tree).
scan() {
    local root="$1"
    shift
    cd "$root"
    local defined
    defined="$(cat lint/ast-grep/rules/*.yml 2>/dev/null | grep -c '^id:' || true)"
    if [[ "$root" == "$here" ]]; then
        ((defined == EXPECTED_RULES)) ||
            fail "lint/ast-grep/rules defines $defined rules, $EXPECTED_RULES expected"
    fi
    ((defined > 0)) || fail "$root/lint/ast-grep/rules defines no rule"

    local sources=("$@")
    if (($# == 0)); then
        mapfile -t sources < <(find crates -name '*.rs' -not -path '*/target/*' | sort)
        # The test-only src files the rules scan whole: every rule file names
        # exactly the set their parents declare.
        local derived listed rule
        derived="$(bash "$here/scripts/test-only-files.sh" "$root" | cut -f1)"
        for rule in lint/ast-grep/rules/*.yml; do
            listed="$(sed -n 's/^  - "\(crates\/[^"*]*\/src\/[^"*]*\.rs\)"$/\1/p' "$rule" | sort -u)"
            [[ "$listed" == "$derived" ]] ||
                fail "$rule lists the test-only src files {$(paste -sd, - <<<"$listed")}," \
                    "but scripts/test-only-files.sh finds {$(paste -sd, - <<<"$derived")}: list exactly those"
        done
    fi

    local bad="" status=0
    if ((${#sources[@]} > 0)); then
        bad="$(suppressions "${sources[@]}")" || status=$?
        ((status <= 1)) || fail "the suppression check failed (awk exit $status)"
    fi

    local sg=0
    ast-grep scan --inspect summary --color never "$@" >"$tmp/findings" 2>"$tmp/summary" || sg=$?
    local loaded scanned
    loaded="$(sed -n 's/.*effectiveRuleCount=\([0-9]*\).*/\1/p' "$tmp/summary")"
    scanned="$(sed -n 's/.*scannedFileCount=\([0-9]*\).*/\1/p' "$tmp/summary")"
    [[ "$loaded" == "$defined" ]] ||
        fail "ast-grep loaded ${loaded:-no} rules in $root, where $defined are defined (is sgconfig.yml intact?)"
    ((${scanned:-0} > 0)) ||
        fail "ast-grep scanned no file in $root: a scan of nothing is not a pass"
    ((sg <= 1)) || { cat "$tmp/summary" "$tmp/findings" >&2; fail "ast-grep exited $sg"; }

    [[ -z "$bad" ]] || report "$bad"
    ((sg == 0)) || report "$(cat "$tmp/findings")"
}

files=()
if ((hook)); then
    payload="$(cat)"
    path="$(jq -r '.tool_input.file_path // empty' <<<"$payload")"
    [[ -n "$path" ]] && files=("$path")
else
    files=("$@")
fi

if ((${#files[@]} == 0)); then
    scan "$here"
else
    # Each file is scanned from the nearest ancestor holding sgconfig.yml: rule
    # `files:` globs are relative to it, so a path given from anywhere else
    # would match none of them and pass unscanned.
    declare -A by_root=()
    for f in "${files[@]}"; do
        [[ "$f" == *.rs ]] || continue
        [[ -f "$f" ]] || fail "$f: no such file, so it cannot be scanned"
        abs="$(cd "$(dirname "$f")" && pwd -P)/$(basename "$f")"
        dir="$(dirname "$abs")"
        while [[ "$dir" != / && ! -f "$dir/sgconfig.yml" ]]; do dir="$(dirname "$dir")"; done
        [[ -f "$dir/sgconfig.yml" ]] ||
            fail "$f: no sgconfig.yml above it, so no rules apply and it cannot be scanned"
        by_root[$dir]+="${abs#"$dir"/}"$'\n'
    done
    for root in "${!by_root[@]}"; do
        mapfile -t rel < <(printf '%s' "${by_root[$root]}")
        scan "$root" "${rel[@]}"
    done
fi

if ((found)); then
    exit $((hook ? 2 : 1))
fi
exit 0
