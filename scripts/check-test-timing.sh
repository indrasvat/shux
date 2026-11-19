#!/usr/bin/env bash
# scripts/check-test-timing.sh — ban timing in disguise in test code.
#
# clippy's `disallowed-methods` bans raw sleeps everywhere; it cannot scope a
# ban to test code, so the shapes that hide a sleep are banned here, by the
# ast-grep rules in lint/ast-grep/ (docs/testing.md#enforcement): a wall-clock
# deadline or measurement, an assertion on a measured duration, a timed wait,
# absence asserted by letting a wait expire, and a fixture that sleeps then
# acts. Where time really is the subject, the escape is a reason directly above
# the suppression, which a rule enforces:
#
#     // why: <what makes the bound one load can only loosen>
#     // ast-grep-ignore: <rule-id>
#
# Usage:
#   check-test-timing.sh           scan the tree
#   check-test-timing.sh FILE...   scan these files (non-Rust paths are skipped)
#   check-test-timing.sh --hook    read a Claude Code hook payload on stdin;
#                                  exit 2 with the findings on stderr
#
# It fails, never passes, when ast-grep is missing or the wrong version, when
# ast-grep loads a different number of rules than lint/ast-grep/rules defines,
# or when a tree scan reads no file: `ast-grep scan` exits 0 on all three.
#
# Exit: 0 clean, 1 findings or a failed check, 2 findings or any failure in
# --hook mode (Claude Code shows the agent a PostToolUse hook's stderr only on 2).

set -euo pipefail

# Rules defined under lint/ast-grep/rules. A rule deleted or renamed without
# updating this fails the scan instead of silently guarding less.
EXPECTED_RULES=12
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

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

command -v ast-grep >/dev/null ||
    fail "ast-grep is not installed: run scripts/ensure-ast-grep.sh"
have="$(ast-grep --version | awk '{print $2}')"
[[ "$have" == "$AST_GREP_VERSION" ]] ||
    fail "ast-grep $have found, $AST_GREP_VERSION expected: run scripts/ensure-ast-grep.sh"

defined="$(cat lint/ast-grep/rules/*.yml 2>/dev/null | grep -c '^id:' || true)"
((defined == EXPECTED_RULES)) ||
    fail "lint/ast-grep/rules defines $defined rules, $EXPECTED_RULES expected"

files=()
if ((hook)); then
    payload="$(cat)"
    path="$(jq -r '.tool_input.file_path // empty' <<<"$payload")"
    [[ -n "$path" ]] && files=("$path")
else
    files=("$@")
fi
targets=()
for f in ${files[@]+"${files[@]}"}; do
    # Rule `files:` globs are relative to the root, so an absolute path would
    # match none of them and pass unscanned.
    f="${f#"$root"/}"
    [[ "$f" == *.rs && -f "$f" ]] && targets+=("$f")
done
# Asked about files, none of them Rust: nothing to check.
((${#files[@]} > 0 && ${#targets[@]} == 0)) && exit 0

summary="$(mktemp)"
findings="$(mktemp)"
trap 'rm -f "$summary" "$findings"' EXIT
status=0
ast-grep scan --inspect summary --color never ${targets[@]+"${targets[@]}"} >"$findings" 2>"$summary" || status=$?

loaded="$(sed -n 's/.*effectiveRuleCount=\([0-9]*\).*/\1/p' "$summary")"
scanned="$(sed -n 's/.*scannedFileCount=\([0-9]*\).*/\1/p' "$summary")"
[[ "$loaded" == "$EXPECTED_RULES" ]] ||
    fail "ast-grep loaded ${loaded:-no} rules, $EXPECTED_RULES expected (is sgconfig.yml intact?)"
((${#targets[@]} > 0)) || ((${scanned:-0} > 0)) ||
    fail "ast-grep scanned no file: a scan of nothing is not a pass"

case "$status" in
0) exit 0 ;;
1)
    if ((hook)); then
        cat "$findings" >&2
        exit 2
    fi
    cat "$findings"
    exit 1
    ;;
*)
    cat "$summary" "$findings" >&2
    fail "ast-grep exited $status"
    ;;
esac
