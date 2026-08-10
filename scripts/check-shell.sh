#!/usr/bin/env bash
# scripts/check-shell.sh — shellcheck every shell script this repo tracks.
#
# Usage:
#   ./scripts/check-shell.sh
#
# Exit codes:
#   0 = every tracked script is clean
#   1 = shellcheck reported findings
#   2 = shellcheck is not installed
#
# ── Why this is a gate and not a habit ──────────────────────────────────────
#
# This repo is held together by shell: the QA gates, the leak guards, the
# evidence harnesses, the frozen-path checks. A defect in that layer does not
# announce itself the way a failing test does — it makes a guard quietly stop
# guarding. `cd` without `|| exit` in a script that rewrites a tracked source
# file, or an unquoted `${path#$ROOT/}` that silently stops stripping, are both
# real examples found in this tree the first time it was swept.
#
# Scripts are discovered from `git ls-files`, so a new one is covered the moment
# it is added and nobody has to remember to list it here.
#
# Suppressions are allowed but must carry a reason on the same comment block.
# Several patterns here are deliberate and shellcheck cannot know it — most
# notably SC2009 (`ps | grep` rather than `pgrep`), which CLAUDE.md's process
# hygiene rule REQUIRES: `pgrep -f` matches the checking process's own argv and
# reports phantom leaks.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

if ! command -v shellcheck >/dev/null 2>&1; then
    # Loud, not skipped. A guard that silently passes when its tool is missing
    # is worse than no guard: it reports success for work it never did.
    echo "✗ shellcheck is not installed, so no shell script was checked." >&2
    echo "  Install it:  apt-get install shellcheck  |  brew install shellcheck" >&2
    exit 2
fi

# Tracked scripts by extension, plus anything carrying a shell shebang — a file
# named `foo` with `#!/usr/bin/env bash` is just as load-bearing as `foo.sh`.
#
# The list lives in a FILE, not a variable: `$(...)` silently discards NUL bytes,
# so a NUL-delimited list round-tripped through a command substitution collapses
# to one mangled entry. Bash warns, but only on stderr, and the check would then
# have "passed" having examined nothing.
list_file="$(mktemp "${TMPDIR:-/tmp}/shux-shellcheck.XXXXXX")"
trap 'rm -f "${list_file}"' EXIT

{
    git ls-files -z -- '*.sh' '*.bash'
    while IFS= read -r -d '' file; do
        [[ -f "$file" ]] || continue
        case "$file" in
            *.sh | *.bash) continue ;;
        esac
        if head -c 128 -- "$file" 2>/dev/null | head -n 1 |
            grep -qE '^#!.*[ /](ba)?sh$'; then
            printf '%s\0' "$file"
        fi
    done < <(git ls-files -z)
} | sort -zu >"${list_file}"

count="$(tr -cd '\0' <"${list_file}" | wc -c | tr -d ' ')"

if [[ "$count" -eq 0 ]]; then
    # Zero scripts means the discovery above broke, not that the repo has no
    # shell. Prove the check ran on something.
    echo "✗ found no tracked shell scripts — discovery is broken, not the repo." >&2
    exit 1
fi

# `-f gcc` for a stable, parseable, colour-free format regardless of TTY or of
# any environment that might otherwise tint the output.
if xargs -0 shellcheck -f gcc <"${list_file}"; then
    echo "✓ shellcheck: ${count} tracked shell script(s) clean"
else
    echo >&2
    echo "✗ shellcheck found issues in the ${count} tracked script(s) above." >&2
    echo "  Fix them, or suppress with an inline directive AND a reason:" >&2
    echo "      # shellcheck disable=SCxxxx  # why this is correct here" >&2
    exit 1
fi
