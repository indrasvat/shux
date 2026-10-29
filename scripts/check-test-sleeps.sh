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
#   line, unless the item or statement carrying it (a fn, a `select!`, a
#   statement holding a closure) has
#   `#[expect(clippy::disallowed_methods, ...)]`;
# - in any `crates/` source, an attribute (inner, outer or inside `cfg_attr`)
#   that allows `clippy::disallowed_methods` or a group holding it, expects
#   such a group, or expects the lint crate-wide; and a `[lints.clippy]` table
#   in a Cargo.toml that relaxes either. The ban is waived per statement, with
#   `expect`, so a stale waiver fails the build.
#
# Comments, string, raw-string and char literals, and helpers whose names
# merely contain "sleep" are never flagged.
#
# Usage:
#   check-test-sleeps.sh FILE...   scan these files (other paths are skipped)
#   check-test-sleeps.sh           scan every crates/**/*.rs and Cargo.toml
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
is_manifest() { [[ "$1" == */Cargo.toml ]]; }
is_test_file() { [[ "$1" == *.rs && "/$1" == */crates/*/tests/* ]]; }

# Prints `path:line: text` for every finding in "$1"; $2 is 1 for a test file.
scan() {
    awk -v path="$1" -v tests="$2" -v q="'" '
        BEGIN { charlit = "^" q "(\\\\[^" q "]+|[^\\\\" q "])" q }
        function ident(ch) { return ch ~ /[A-Za-z0-9_]/ }
        # Code with comments removed and literals emptied. The lexer state
        # carries across lines: block comments nest, strings span lines.
        function strip(line,    out, i, n, c, two, rest, h) {
            out = ""; n = length(line)
            for (i = 1; i <= n; i++) {
                c = substr(line, i, 1); two = substr(line, i, 2)
                if (st == "block") {
                    if (two == "/*") { depth++; i++ }
                    else if (two == "*/") { i++; if (--depth == 0) st = "" }
                    continue
                }
                if (st == "str") {
                    if (c == "\\") i++
                    else if (c == "\"") { st = ""; out = out "\"" }
                    continue
                }
                if (st == "raw") {
                    if (c == "\"" && substr(line, i + 1, hashes) == closer) { i += hashes; st = ""; out = out "\"" }
                    continue
                }
                if (two == "//") break
                if (two == "/*") { st = "block"; depth = 1; i++; continue }
                if (c == "\"") { st = "str"; out = out "\""; continue }
                if (c == "r" && !(i > 1 && ident(substr(line, i - 1, 1)) && \
                        !(substr(line, i - 1, 1) == "b" && (i == 2 || !ident(substr(line, i - 2, 1)))))) {
                    rest = substr(line, i + 1)
                    if (match(rest, /^#*"/)) {
                        hashes = RLENGTH - 1; closer = substr(rest, 1, hashes)
                        st = "raw"; out = out "\""; i += RLENGTH; continue
                    }
                }
                if (c == q && match(substr(line, i), charlit)) { out = out q q; i += RLENGTH - 1; continue }
                out = out c
            }
            return out
        }
        # An attribute that waives the ban: allow of it or of a group holding
        # it, an expect of a group, or a crate-wide expect of it.
        function waiver(a) {
            if (a !~ /(allow|expect)[ \t]*\(/) return 0
            if (a ~ /allow[ \t]*\(.*clippy::(disallowed_methods|style|all)([^A-Za-z0-9_]|$)/) return 1
            if (a ~ /expect[ \t]*\(.*clippy::(style|all)([^A-Za-z0-9_]|$)/) return 1
            return a ~ /^#!/ && a ~ /clippy::disallowed_methods/
        }
        {
            orig[NR] = $0
            code[NR] = strip($0)
        }
        # Mark the lines of the item or statement the attribute at line n,
        # column p sits on: up to a `;` or `,` at its own depth, or the `}`
        # that closes its body.
        function cover(n, p,    k, i, line, ch, sq, depth, inside) {
            sq = 0; depth = 0; inside = 1
            for (k = n; k <= NR && k <= n + 400; k++) {
                covered[k] = 1; line = code[k]
                for (i = (k == n) ? p : 1; i <= length(line); i++) {
                    ch = substr(line, i, 1)
                    if (inside) {
                        if (ch == "[") sq++
                        else if (ch == "]" && --sq == 0) inside = 0
                        continue
                    }
                    if (index("([{", ch)) depth++
                    else if (index(")]}", ch)) {
                        if (--depth < 0 || (depth == 0 && ch == "}")) return
                    } else if (depth == 0 && (ch == ";" || ch == ",")) return
                }
            }
        }
        END {
            for (n = 1; n <= NR; n++) {
                c = code[n]
                # Collect a whole attribute, however many lines it spans.
                p = index(c, "#")
                if (p && substr(c, p) ~ /^#!?[ \t]*\[/) {
                    a = substr(c, p); open = gsub(/\[/, "[", a) - gsub(/\]/, "]", a)
                    for (k = n + 1; open > 0 && k <= NR && k <= n + 20; k++) {
                        a = a " " code[k]; open += gsub(/\[/, "[", code[k]) - gsub(/\]/, "]", code[k])
                    }
                    if (waiver(a)) { printf "%s:%d: waiver: %s\n", path, n, orig[n]; continue }
                    if (a ~ /^#[ \t]*\[/ && a ~ /expect[ \t]*\(/ && a ~ /clippy::disallowed_methods/) cover(n, p)
                }
                if (!tests || covered[n]) continue
                call = c ~ /(^|[^A-Za-z0-9_:])(::)?((std::)?thread|(tokio::)?time)::sleep(_until)?[ \t]*\(/
                imp = c ~ /^[ \t]*(pub[^ ]* +)?use +(::)?(std::thread|tokio::time)::(.*[^A-Za-z0-9_])?sleep(_until)?([^A-Za-z0-9_]|$)/
                if (call || imp) printf "%s:%d: %s\n", path, n, orig[n]
            }
        }
    ' "$1"
}

# Prints `path:line: text` for a Cargo.toml `[lints.clippy]` entry that allows
# or expects the ban or a group holding it.
scan_manifest() {
    awk -v path="$1" '
        /^[ \t]*\[/ { t = $0; gsub(/[ \t]/, "", t); clippy = (t == "[lints.clippy]" || t == "[workspace.lints.clippy]") }
        clippy && /^[ \t]*"?(disallowed[-_]methods|style|all)"?[ \t]*=/ && /"(allow|expect)"/ {
            printf "%s:%d: waiver: %s\n", path, NR, $0
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
    while IFS= read -r -d '' f; do files+=("$f"); done \
        < <(find "$root" -maxdepth 3 -name Cargo.toml -not -path '*/target*' -print0)
fi

findings=""
scanned=0
for f in "${files[@]}"; do
    [[ "$f" == /* ]] || f="$PWD/$f"
    [[ -f "$f" ]] || continue
    if is_manifest "$f"; then
        scanned=$((scanned + 1))
        findings+="$(scan_manifest "$f")"$'\n'
        continue
    fi
    in_crates "$f" || continue
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
