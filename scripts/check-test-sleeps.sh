#!/usr/bin/env bash
# scripts/check-test-sleeps.sh — flag raw sleeps in integration tests.
#
# A test waits for a condition, never for an amount of time (docs/testing.md).
# The authoritative gate is clippy's `disallowed-methods` (clippy.toml), which is
# type-resolved and covers every target. This is its fast, textual front line
# for `crates/*/tests/`, cheap enough to run after every edit as a Claude Code
# hook. It reports only qualified sleep calls and sleep imports on code lines,
# so it never flags a comment, a `sleep` shell command in a fixture string, or
# a helper whose name merely contains "sleep". A call is allowed when the
# statement carrying it has `#[expect(clippy::disallowed_methods, ...)]`.
#
# Usage:
#   check-test-sleeps.sh FILE...   scan these files (non-test paths are skipped)
#   check-test-sleeps.sh           scan every crates/*/tests/**/*.rs
#   check-test-sleeps.sh --hook    read a Claude Code hook payload on stdin;
#                                  exit 2 with the findings on stderr
#
# Exit: 0 clean, 1 findings or usage error, 2 findings in --hook mode.

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

is_test_file() {
    [[ "$1" == *.rs ]] || return 1
    case "${1#"$root"/}" in
        crates/*/tests/*) return 0 ;;
        *) return 1 ;;
    esac
}

# Prints `path:line: text` for every unannotated sleep in "$1".
scan() {
    awk -v path="$1" '
        function code(s) { sub(/\/\/.*/, "", s); return s }
        function ends_statement(s) {
            s = code(s); sub(/[ \t]+$/, "", s)
            return s ~ /[;{}]$/
        }
        { line[NR] = $0 }
        END {
            for (n = 1; n <= NR; n++) {
                if (line[n] ~ /^[ \t]*(\/\*|\*)/) continue
                c = code(line[n])
                hit = c ~ /(^|[^A-Za-z0-9_])(thread|time)::sleep(_until)?[ \t]*\(/ \
                   || c ~ /^[ \t]*(pub[^ ]* +)?use +(std::thread|tokio::time)::(.*[^A-Za-z0-9_])?sleep(_until)?([^A-Za-z0-9_]|$)/
                if (!hit) continue
                allowed = 0
                for (k = n - 1; k >= 1 && k >= n - 8; k--) {
                    if (line[k] ~ /clippy::disallowed_methods/) { allowed = 1; break }
                    if (ends_statement(line[k])) break
                }
                if (!allowed) printf "%s:%d: %s\n", path, n, line[n]
            }
        }
    ' "$1"
}

hook=0
files=()
if [[ "${1:-}" == "--hook" ]]; then
    hook=1
    command -v jq >/dev/null || {
        echo "check-test-sleeps: jq is required for --hook mode" >&2
        exit 1
    }
    file="$(jq -r '.tool_input.file_path // empty')"
    [[ -n "$file" ]] || exit 0
    files=("$file")
elif (($# > 0)); then
    files=("$@")
else
    while IFS= read -r -d '' f; do files+=("$f"); done \
        < <(find "$root/crates" -path '*/tests/*' -name '*.rs' -not -path '*/target/*' -print0)
    if ((${#files[@]} == 0)); then
        echo "check-test-sleeps: found no test files under $root/crates" >&2
        exit 1
    fi
fi

findings=""
scanned=0
for f in "${files[@]}"; do
    [[ "$f" == /* ]] || f="$PWD/$f"
    is_test_file "$f" || continue
    [[ -f "$f" ]] || continue
    scanned=$((scanned + 1))
    findings+="$(scan "$f")"$'\n'
done
findings="$(printf '%s' "$findings" | sed '/^$/d')"

if [[ -z "$findings" ]]; then
    ((hook)) || echo "✓ no raw sleeps in $scanned test file(s)"
    exit 0
fi

msg="✗ raw sleep in test code (docs/testing.md: wait for a condition, not for time):
$findings
Use shux_testkit::{wait_for, wait_until, poll, ...} for a condition. If time itself is the
subject, put #[expect(clippy::disallowed_methods, reason = \"...\")] on the statement."
if ((hook)); then
    echo "$msg" >&2
    exit 2
fi
echo "$msg"
exit 1
