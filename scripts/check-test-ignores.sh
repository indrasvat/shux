#!/usr/bin/env bash
# scripts/check-test-ignores.sh — an ignored test names why, and the why is live.
#
# Every `#[ignore]` in crates/ carries a reason that either names an OPEN issue
# (`see #N`) or starts with `generator:` (a test run by hand to write
# fixtures). A bare `#[ignore]` fails, and so does one whose issue is closed:
# an ignore must not outlive its fix. Issue state comes from `gh`; when `gh`
# cannot answer, this fails rather than passing unchecked.
#
# Usage: check-test-ignores.sh        (SHUX_REPO overrides github.com/indrasvat/shux)
# Exit: 0 every ignore is accounted for, 1 otherwise.

set -euo pipefail

repo="${SHUX_REPO:-indrasvat/shux}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

mapfile -t sources < <(find crates -name '*.rs' -not -path '*/target/*' | sort)
((${#sources[@]} > 0)) || { echo "check-test-ignores: no Rust sources under crates/" >&2; exit 1; }

# path:line<TAB>reason, or path:line<TAB> for a bare ignore. Only attribute
# lines count: a doc comment that mentions `#[ignore]` is not one.
found="$(awk '
    /^[[:space:]]*#\[ignore[[:space:]]*\]/ { printf "%s:%d\t\n", FILENAME, FNR; next }
    /^[[:space:]]*#\[ignore[[:space:]]*=/ {
        r = $0
        sub(/^[^"]*"/, "", r)
        sub(/"[[:space:]]*\][[:space:]]*$/, "", r)
        printf "%s:%d\t%s\n", FILENAME, FNR, r
    }
' "${sources[@]}")"

fail=0
declare -A state=()
while IFS=$'\t' read -r where reason; do
    [[ -n "$where" ]] || continue
    if [[ -z "$reason" ]]; then
        echo "✗ $where: a bare #[ignore]; say why: \"see #N\" for an open issue, or \"generator: …\""
        fail=1
        continue
    fi
    [[ "$reason" == generator:* ]] && continue
    if [[ ! "$reason" =~ see\ \#([0-9]+) ]]; then
        echo "✗ $where: the reason \"$reason\" names no issue (\"see #N\") and is not a generator"
        fail=1
        continue
    fi
    n="${BASH_REMATCH[1]}"
    if [[ -z "${state[$n]:-}" ]]; then
        state[$n]="$(gh issue view "$n" --repo "$repo" --json state -q .state)" || {
            echo "check-test-ignores: gh could not read issue #$n of $repo, so the ignore at $where is unchecked" >&2
            exit 1
        }
    fi
    if [[ "${state[$n]}" != "OPEN" ]]; then
        echo "✗ $where: ignored for #$n, which is ${state[$n]}: fix or delete the test, then drop the ignore"
        fail=1
    fi
done <<<"$found"

if ((fail)); then
    exit 1
fi
echo "✓ every #[ignore] names a generator or an open issue"
