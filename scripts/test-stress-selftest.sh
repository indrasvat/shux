#!/usr/bin/env bash
# scripts/test-stress-selftest.sh — prove test-stress.sh starts the slowest
# tests of its first run first in every later run, and changes nothing else.
#
# Runs the script (or $1) with a stub `cargo` that records the nextest config
# each run was given and, when that config asks for a JUnit report, writes one
# from STUB_TIMES ("<secs> <binary> <test>" lines).

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
script="${1:-$here/test-stress.sh}"
[[ -f "$script" ]] || { echo "✗ missing $script" >&2; exit 1; }
work="$(mktemp -d "${TMPDIR:-/tmp}/shux-stress-selftest.XXXXXX")"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin" "$work/repo/scripts" "$work/repo/.config"
cp "$script" "$work/repo/scripts/test-stress.sh"
printf '[profile.default]\nretries = 0\n' >"$work/repo/.config/nextest.toml"
cat >"$work/bin/cargo" <<'EOF'
#!/usr/bin/env bash
n=$(($(find "$STUB_LOG" -name 'run*.toml' | wc -l) + 1))
cfg=""
prev=""
for a in "$@"; do
    [[ "$prev" == --config-file ]] && cfg="$a"
    prev="$a"
done
if [[ -z "$cfg" ]]; then : >"$STUB_LOG/run$n.toml"; echo "no --config-file" >"$STUB_LOG/run$n.noconfig"; exit 0; fi
cp "$cfg" "$STUB_LOG/run$n.toml"
junit="$(awk -F'"' '/^\[profile\.default\.junit\]/ { j = 1; next } j && /^path = / { print $2; exit }' "$cfg")"
if [[ -n "$junit" && -z "${STUB_NO_JUNIT:-}" ]]; then
    {
        echo '<testsuites>'
        while read -r t b name; do
            printf '        <testcase name="%s" classname="%s" timestamp="x" time="%s">\n        </testcase>\n' "$name" "$b" "$t"
        done <<<"$STUB_TIMES"
        echo '</testsuites>'
    } >"$junit"
fi
exit "${STUB_STATUS:-0}"
EOF
chmod +x "$work/bin/cargo"

fails=0
ok() { printf '  \033[32m✓\033[0m %s\n' "$1"; }
bad() { printf '  \033[31m✗\033[0m %s\n' "$1"; fails=$((fails + 1)); }
# run <count>: the script, with a fresh log; its exit status in $status.
run() {
    rm -rf "$work/log"
    mkdir -p "$work/log"
    status=0
    (cd "$work/repo" && PATH="$work/bin:$PATH" STUB_LOG="$work/log" STRESS_COUNT="$1" STRESS_LOAD=0 \
        bash scripts/test-stress.sh) >"$work/out" 2>&1 || status=$?
}
# priorities <run>: "<priority> <binary> <test>" per override the run's config adds.
priorities() {
    awk -F"'" '/^filter = / { f = $2; next } /^priority = / { split($0, p, " = "); print p[2], f }' "$work/log/run$1.toml" |
        sed -E 's/binary_id\(=([^)]*)\) & test\(=(.*)\)$/\1 \2/'
}

STUB_TIMES=$'0.5 p a\n9.0 p::it slow_one\n3.25 q b::c'
export STUB_TIMES
run 3
if ((status == 0)) && [[ -f "$work/log/run3.toml" && ! -e "$work/log/run1.noconfig" ]]; then
    ok "every run gets a nextest config"
else
    bad "every run gets a nextest config (exit $status, $(cat "$work/out"))"
fi
if grep -q '^\[profile\.default\.junit\]' "$work/log/run1.toml" && ! grep -q '^priority' "$work/log/run1.toml"; then
    ok "the first run writes a JUnit report and has no order of its own"
else
    bad "the first run writes a JUnit report and has no order of its own"
fi
want=$'100 p::it slow_one\n99 q b::c\n98 p a'
for r in 2 3; do
    got="$(priorities "$r" 2>/dev/null || true)"
    if [[ "$got" == "$want" ]]; then ok "run $r starts the slowest first"; else bad "run $r starts the slowest first: got [$got]"; fi
    if [[ -f "$work/log/run$r.toml" ]] && ! grep -q 'junit' "$work/log/run$r.toml" &&
        cmp -s <(head -n 2 "$work/log/run$r.toml") "$work/repo/.config/nextest.toml"; then
        ok "run $r keeps the repo's config and writes no report"
    else
        bad "run $r keeps the repo's config and writes no report"
    fi
done

STUB_TIMES="$(awk 'BEGIN { for (i = 1; i <= 150; i++) printf "%d.0 p t%d\n", i, i }')"
run 2
got="$(priorities 2 2>/dev/null || true)"
if [[ "$(grep -c . <<<"$got")" == 100 && "$(head -n 1 <<<"$got")" == "100 p t150" && "$(tail -n 1 <<<"$got")" == "1 p t51" ]]; then
    ok "only the 100 slowest are ordered"
else
    bad "only the 100 slowest are ordered: $(grep -c . <<<"$got") overrides, first [$(head -n 1 <<<"$got")]"
fi

STUB_TIMES='1.0 p a'
run 1
if ((status == 0)) && ! grep -q '^priority' "$work/log/run1.toml" 2>/dev/null; then
    ok "a single run needs no order"
else
    bad "a single run needs no order (exit $status)"
fi

STUB_NO_JUNIT=1 run 2
if ((status != 0)) && grep -q 'wrote no JUnit report' "$work/out"; then
    ok "a first run that wrote no report fails the stress run"
else
    bad "a first run that wrote no report fails the stress run (exit $status)"
fi
unset STUB_NO_JUNIT

STUB_STATUS=100 run 2
if ((status == 100)) && [[ -f "$work/log/run2.toml" ]] && grep -q '^priority' "$work/log/run2.toml"; then
    ok "a failing first run still orders the rest, and the stress run fails"
else
    bad "a failing first run still orders the rest, and the stress run fails (exit $status)"
fi

if ((fails > 0)); then
    printf '\033[1;31m✗ test-stress self-test: %d check(s) failed\033[0m\n' "$fails"
    exit 1
fi
printf '\033[1;32m✓ test-stress self-test: every check holds\033[0m\n'
