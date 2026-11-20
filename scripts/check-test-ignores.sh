#!/usr/bin/env bash
# scripts/check-test-ignores.sh — an ignored test names why, and the why is live.
#
# Every ignore in crates/ (`#[ignore]`, or `ignore` inside a `#[cfg_attr(…)]`)
# carries a reason that either names OPEN issues (`see #N`, every one of them
# an issue and open) or starts with `generator:` (a test run by hand to write
# fixtures; taken at its word). A bare ignore fails, and so does one whose
# issue is closed or is a pull request: an ignore must not outlive its fix.
# Issue state comes from `gh`; when `gh` cannot answer, this fails rather than
# passing unchecked.
#
# Usage: check-test-ignores.sh        (SHUX_REPO overrides github.com/indrasvat/shux)
# Exit: 0 every ignore is accounted for, 1 otherwise.

set -euo pipefail

repo="${SHUX_REPO:-indrasvat/shux}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

mapfile -t sources < <(find crates -name '*.rs' -not -path '*/target/*' | sort)
((${#sources[@]} > 0)) || { echo "check-test-ignores: no Rust sources under crates/" >&2; exit 1; }

# path:line<TAB>reason, or path:line<TAB>- for a bare ignore. Comments are
# blanked first (strings kept, newlines kept), so only code counts; the reason
# is the string literal's own text, never a comment after it.
found="$(perl -0777 -ne '
    my $str = qr{r(\#*)"(?s:.*?)"\g{-1}|"(?:[^"\\]|\\(?s:.))*"};
    my $src = $_;
    $src =~ s{($str|\x27(?:[^\x27\\]|\\.)\x27)|(//[^\n]*)|(/\*(?s:.*?)\*/)}{
        defined $1 ? $1 : defined $3 ? ($3 =~ tr/\n//cdr) : ""
    }ge;
    my $reason = sub {
        my $s = shift;
        return "-" unless defined $s;
        $s =~ s/^r#*"//; $s =~ s/^"//; $s =~ s/"#*$//;
        $s =~ s/\\(.)/$1/g;
        $s =~ s/\t/ /g;
        return $s;
    };
    my $at = sub { my $line = 1 + (substr($src, 0, shift) =~ tr/\n//); "$ARGV:$line" };
    while ($src =~ /#\s*\[\s*ignore\s*(?:=\s*($str))?\s*\]/g) {
        print $at->($-[0]), "\t", $reason->($1), "\n";
    }
    while ($src =~ /#\s*\[\s*cfg_attr\s*(?<args>\((?:[^()"]++|$str|(?&args))*\))\s*\]/g) {
        my ($pos, $args) = ($-[0], $+{args});
        while ($args =~ /[(,]\s*ignore\s*(?:=\s*($str))?\s*[,)]/g) {
            print $at->($pos), "\t", $reason->($1), "\n";
        }
    }
' "${sources[@]}")"

fail=0
declare -A state=()
while IFS=$'\t' read -r where reason; do
    [[ -n "$where" ]] || continue
    if [[ "$reason" == - ]]; then
        echo "✗ $where: a bare ignore; say why: \"see #N\" for an open issue, or \"generator: …\""
        fail=1
        continue
    fi
    [[ "$reason" == generator:* ]] && continue
    issues=()
    rest="$reason"
    while [[ "$rest" =~ see\ \#([1-9][0-9]*)([^0-9]|$) ]]; do
        issues+=("${BASH_REMATCH[1]}")
        rest="${rest#*"see #${BASH_REMATCH[1]}"}"
    done
    if ((${#issues[@]} == 0)); then
        echo "✗ $where: the reason \"$reason\" names no issue (\"see #N\") and is not a generator"
        fail=1
        continue
    fi
    for n in "${issues[@]}"; do
        if [[ -z "${state[$n]:-}" ]]; then
            state[$n]="$(gh api "repos/$repo/issues/$n" --jq '.state + " " + (has("pull_request") | tostring)')" || {
                echo "check-test-ignores: gh could not read issue #$n of $repo, so the ignore at $where is unchecked" >&2
                exit 1
            }
        fi
        case "${state[$n]}" in
        "open false") ;;
        *" true")
            echo "✗ $where: ignored for #$n, which is a pull request: name the issue it fixes"
            fail=1
            ;;
        *)
            echo "✗ $where: ignored for #$n, which is ${state[$n]%% *}: fix or delete the test, then drop the ignore"
            fail=1
            ;;
        esac
    done
done <<<"$found"

if ((fail)); then
    exit 1
fi
echo "✓ every ignore names a generator or an open issue"
