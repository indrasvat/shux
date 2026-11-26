#!/usr/bin/env bash
# scripts/test-stress-selftest.sh — prove test-stress.sh starts the slowest
# tests of its first run first in every later run, and changes nothing else.
#
# Runs the script (or $1) with a stub `cargo` that records each run's argv and
# the profile it ran in, and, when that profile asks for a JUnit report, writes
# one from STUB_TIMES ("<secs> <binary> <test>" lines).

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
n=$(($(find "$STUB_LOG" -name 'run*.args' | wc -l) + 1))
printf '%s\n' "$@" >"$STUB_LOG/run$n.args"
cfg="" profile="default" prev=""
for a in "$@"; do
    [[ "$prev" == --config-file ]] && cfg="$a"
    [[ "$prev" == --profile ]] && profile="$a"
    prev="$a"
done
[[ -n "$cfg" ]] || exit 0
cp "$cfg" "$STUB_LOG/run$n.toml"
# The profile's section, and the JUnit path it sets (a TOML basic string).
awk -v p="[profile.$profile]" '$0 == p { on = 1; next } /^\[/ { on = 0 } on' "$cfg" >"$STUB_LOG/run$n.profile"
if [[ -n "${STUB_HOG_DIES:-}" ]]; then
    for _ in $(seq 400); do [[ -s "$STUB_LOG/hog.pid" ]] && break; sleep 0.05; done
    for _ in $(seq 400); do
        s="$(ps -o stat= -p "$(cat "$STUB_LOG/hog.pid")" 2>/dev/null || true)"
        [[ -n "$s" && "$s" != Z* ]] || break
        sleep 0.05
    done
fi
junit="$(awk -v j="[profile.$profile.junit]" '$0 == j { on = 1; next } /^\[/ { on = 0 } on && /^path = "/' "$cfg" |
    sed -E 's/^path = "(.*)"$/\1/; s/\\"/"/g; s/\\\\/\\/g')"
if [[ -n "$junit" && -z "${STUB_NO_JUNIT:-}" ]]; then
    {
        echo '<testsuites>'
        while read -r t b name; do
            printf '        <testcase name="%s" classname="%s" timestamp="x" time="%s">\n        </testcase>\n' "$name" "$b" "$t"
        done <<<"$STUB_TIMES"
        echo '</testsuites>'
    } >"$junit"
fi
[[ "$n" != "${STUB_FAIL_RUN:-}" ]] || exit 100
exit "${STUB_STATUS:-0}"
EOF
# A hog that dies at once; cargo's first run waits until it is gone.
cat >"$work/bin/stress-ng" <<'EOF'
#!/usr/bin/env bash
echo "$$" >"$STUB_LOG/hog.pid"
EOF
chmod +x "$work/bin/cargo" "$work/bin/stress-ng"

fails=0
ok() { printf '  \033[32m✓\033[0m %s\n' "$1"; }
bad() { printf '  \033[31m✗\033[0m %s\n' "$1"; fails=$((fails + 1)); }
# run <count>: the script, with a fresh log; its exit status in $status.
run() {
    rm -rf "$work/log" "$work/tmp"
    mkdir -p "$work/log" "$work/tmp"
    status=0
    (cd "$work/repo" && PATH="$work/bin:$PATH" STUB_LOG="$work/log" STRESS_COUNT="$1" STRESS_LOAD="${LOAD:-0}" \
        TMPDIR="${STUB_TMPDIR:-$work/tmp}" bash scripts/test-stress.sh) >"$work/out" 2>&1 || status=$?
}
# priorities <run>: "<priority> <binary> <test>" per override the run's config adds.
priorities() {
    awk -F"'" '/^filter = / { f = $2; next } /^priority = / { split($0, p, " = "); print p[2], f }' "$work/log/run$1.toml" |
        sed -E 's/binary_id\(=([^)]*)\) & test\(=(.*)\)$/\1 \2/'
}
# ran <run> <profile>: the run ran tests (not a list), in that profile.
ran() {
    [[ -f "$work/log/run$1.args" ]] && grep -qx run "$work/log/run$1.args" &&
        [[ "$(grep -A1 -x -- --profile "$work/log/run$1.args" | tail -n 1)" == "$2" ]]
}

STUB_TIMES=$'0.5 p a\n9.0 p::it slow_one\n3.25 q b::c'
export STUB_TIMES
run 3
if ((status == 0)) && ran 1 stress-first && ran 2 stress-rest && ran 3 stress-rest && [[ ! -e "$work/log/run4.args" ]]; then
    ok "3 runs run the tests, in their own profiles, and no more"
else
    bad "3 runs run the tests, in their own profiles, and no more (exit $status, $(cat "$work/out"))"
fi
if grep -qx 'inherits = "default"' "$work/log/run1.profile" && ! grep -q '^priority' "$work/log/run1.toml"; then
    ok "the first run inherits the profile, writes a JUnit report and has no order"
else
    bad "the first run inherits the profile, writes a JUnit report and has no order"
fi
want=$'100 p::it slow_one\n99 q b::c\n98 p a'
for r in 2 3; do
    got="$(priorities "$r" 2>/dev/null || true)"
    if [[ "$got" == "$want" ]]; then ok "run $r starts the slowest first"; else bad "run $r starts the slowest first: got [$got]"; fi
    if [[ -f "$work/log/run$r.toml" ]] && ! grep -q 'junit' "$work/log/run$r.toml" &&
        grep -qx 'inherits = "default"' "$work/log/run$r.profile" &&
        cmp -s <(head -n 2 "$work/log/run$r.toml") "$work/repo/.config/nextest.toml"; then
        ok "run $r keeps the repo's config and profile, and writes no report"
    else
        bad "run $r keeps the repo's config and profile, and writes no report"
    fi
done
if [[ -z "$(find "$work/tmp" -mindepth 1)" ]]; then ok "it leaves no temp files"; else bad "it leaves no temp files"; fi

NEXTEST_PROFILE=ci run 2
if ((status == 0)) && grep -qx 'inherits = "ci"' "$work/log/run1.profile" && grep -qx 'inherits = "ci"' "$work/log/run2.profile"; then
    ok "every run inherits NEXTEST_PROFILE"
else
    bad "every run inherits NEXTEST_PROFILE (exit $status)"
fi

STUB_TIMES=$'5.0 p case)tail\n4.0 p a&amp;b\n3.0 p::x y\n2.0 p o,k'
run 2
got="$(priorities 2 2>/dev/null || true)"
if ((status == 0)) && [[ "$got" == "100 p::x y" ]]; then
    ok "a name a filterset or JUnit could misread keeps nextest's order"
else
    bad "a name a filterset or JUnit could misread keeps nextest's order: got [$got]"
fi

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
if ((status == 0)) && ran 1 stress-first && [[ ! -e "$work/log/run2.args" ]]; then
    ok "a single run needs no order"
else
    bad "a single run needs no order (exit $status)"
fi

mkdir -p "$work/q\"uote"
STUB_TMPDIR="$work/q\"uote" run 2
if ((status == 0)) && [[ "$(priorities 2 2>/dev/null || true)" == "100 p a" ]]; then
    ok "a temp dir with a quote in its name still yields a report"
else
    bad "a temp dir with a quote in its name still yields a report (exit $status)"
fi

STUB_NO_JUNIT=1 run 2
if ((status != 0)) && grep -q 'wrote no JUnit report' "$work/out"; then
    ok "a first run that wrote no report fails the stress run"
else
    bad "a first run that wrote no report fails the stress run (exit $status)"
fi

STUB_STATUS=100 run 2
if ((status == 100)) && grep -q '^priority' "$work/log/run2.toml" 2>/dev/null; then
    ok "a failing first run still orders the rest, and the stress run fails"
else
    bad "a failing first run still orders the rest, and the stress run fails (exit $status)"
fi

STUB_FAIL_RUN=2 run 3
if ((status == 100)) && [[ -f "$work/log/run3.args" ]]; then
    ok "a failure in a later run fails the stress run, after every run"
else
    bad "a failure in a later run fails the stress run, after every run (exit $status)"
fi

LOAD=1 STUB_HOG_DIES=1 run 2
if ((status == 1)) && grep -q 'stress-ng exited' "$work/out" && [[ -z "$(find "$work/tmp" -mindepth 1)" ]]; then
    ok "a hog that dies fails the stress run and leaves no temp files"
else
    bad "a hog that dies fails the stress run and leaves no temp files (exit $status)"
fi

if ((fails > 0)); then
    printf '\033[1;31m✗ test-stress self-test: %d check(s) failed\033[0m\n' "$fails"
    exit 1
fi
printf '\033[1;32m✓ test-stress self-test: every check holds\033[0m\n'
