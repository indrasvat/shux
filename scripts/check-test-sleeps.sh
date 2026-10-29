#!/usr/bin/env bash
# scripts/check-test-sleeps.sh — flag raw sleeps in integration tests, and
# waivers of the sleep ban anywhere in crates/.
#
# A test waits for a condition, never for an amount of time (docs/testing.md).
# The authoritative gate is clippy's `disallowed-methods` (clippy.toml), which is
# type-resolved and covers every target. This is its fast, textual front line,
# cheap enough to run after every edit as a Claude Code hook:
#
# - in `crates/*/tests/`, a qualified sleep call or a sleep import on a code
#   line, unless the statement carrying it has
#   `#[expect(clippy::disallowed_methods, ...)]`;
# - in any `crates/` source, `allow(...)` of `clippy::disallowed_methods` or of
#   a group containing it, or an inner `#![expect(...)]` of it: the ban is
#   waived per statement, with `expect`, so a stale waiver fails the build.
#
# Comments, string literals and helpers whose names merely contain "sleep" are
# never flagged.
#
# Usage:
#   check-test-sleeps.sh FILE...   scan these files (non-crate paths are skipped)
#   check-test-sleeps.sh           scan every crates/**/*.rs
#   check-test-sleeps.sh --hook    read a Claude Code hook payload on stdin;
#                                  exit 2 with the findings on stderr
#
# Exit: 0 clean, 1 findings or usage error, 2 findings or any failure in --hook
# mode (Claude Code shows the agent a PostToolUse hook's stderr only on 2).

set -euo pipefail

hook=0
[[ "${1:-}" == "--hook" ]] && hook=1
if ((hook)); then
    trap 'echo "check-test-sleeps: hook failed (exit $?) — the sleep check did not run" >&2; exit 2' ERR
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Matched on the path's tail, so a worktree or symlinked checkout is covered.
in_crates() { [[ "$1" == *.rs && "/$1" == */crates/*/* ]]; }
is_test_file() { [[ "$1" == *.rs && "/$1" == */crates/*/tests/* ]]; }

# Prints `path:line: text` for every finding in "$1"; $2 is 1 for a test file.
scan() {
    awk -v path="$1" -v tests="$2" '
        function ends_statement(s) {
            sub(/[ \t]+$/, "", s)
            return s ~ /[;}]$/
        }
        {
            orig[NR] = $0
            # String literals emptied first, so a "/*" or "//" in one is inert.
            s = $0
            gsub(/"([^"\\]|\\.)*"/, "\"\"", s)
            # Block comments may span lines and need no leading `*`.
            if (in_block) {
                if (s !~ /\*\//) { code[NR] = ""; next }
                sub(/^([^*]|\*[^\/])*\*\//, "", s); in_block = 0
            }
            while (s ~ /\/\*/) {
                if (s ~ /\/\*([^*]|\*[^\/])*\*\//) { sub(/\/\*([^*]|\*[^\/])*\*\//, "", s); continue }
                sub(/\/\*.*$/, "", s); in_block = 1
            }
            sub(/\/\/.*/, "", s)
            code[NR] = s
        }
        END {
            for (n = 1; n <= NR; n++) {
                c = code[n]
                if (c ~ /#!?\[[ \t]*allow\(.*clippy::(disallowed_methods|style|all)/ \
                 || c ~ /#![ \t]*\[[ \t]*expect\(.*clippy::disallowed_methods/) {
                    printf "%s:%d: waiver: %s\n", path, n, orig[n]
                    continue
                }
                if (!tests) continue
                call = c ~ /(^|[^A-Za-z0-9_:])(::)?((std::)?thread|(tokio::)?time)::sleep(_until)?[ \t]*\(/
                imp = c ~ /^[ \t]*(pub[^ ]* +)?use +(::)?(std::thread|tokio::time)::(.*[^A-Za-z0-9_])?sleep(_until)?([^A-Za-z0-9_]|$)/
                if (!call && !imp) continue
                # The statement carrying it, or an attribute above it, names the lint.
                allowed = c ~ /clippy::disallowed_methods/
                for (k = n - 1; !allowed && k >= 1 && k >= n - 12; k--) {
                    if (code[k] ~ /clippy::disallowed_methods/) { allowed = 1; break }
                    if (ends_statement(code[k])) break
                }
                if (!allowed) printf "%s:%d: %s\n", path, n, orig[n]
            }
        }
    ' "$1"
}

files=()
if ((hook)); then
    command -v jq >/dev/null || {
        echo "check-test-sleeps: jq is required for --hook mode — the sleep check did not run" >&2
        exit 2
    }
    file="$(jq -r '.tool_input.file_path // empty')"
    [[ -n "$file" ]] || exit 0
    files=("$file")
elif (($# > 0)); then
    files=("$@")
else
    while IFS= read -r -d '' f; do files+=("$f"); done \
        < <(find "$root/crates" -name '*.rs' -not -path '*/target/*' -print0)
    if ((${#files[@]} == 0)); then
        echo "check-test-sleeps: found no sources under $root/crates" >&2
        exit 1
    fi
fi

findings=""
scanned=0
for f in "${files[@]}"; do
    [[ "$f" == /* ]] || f="$PWD/$f"
    in_crates "$f" || continue
    [[ -f "$f" ]] || continue
    scanned=$((scanned + 1))
    tests=0
    is_test_file "$f" && tests=1
    findings+="$(scan "$f" "$tests")"$'\n'
done
findings="$(printf '%s' "$findings" | sed '/^$/d')"

if [[ -z "$findings" ]]; then
    ((hook)) || echo "✓ no raw sleeps or sleep-ban waivers in $scanned file(s)"
    exit 0
fi

msg="✗ raw sleep in test code, or a waiver of the sleep ban (docs/testing.md: wait for a condition, not for time):
$findings
Use shux_testkit::{wait_for, wait_until, poll, ...} for a condition. If time itself is the
subject, put #[expect(clippy::disallowed_methods, reason = \"...\")] on the statement."
if ((hook)); then
    echo "$msg" >&2
    exit 2
fi
echo "$msg"
exit 1
