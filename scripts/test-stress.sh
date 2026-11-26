#!/usr/bin/env bash
# scripts/test-stress.sh — repeat tests, optionally under CPU load, to expose
# timing dependence (docs/testing.md).
#
#   STRESS_COUNT   runs of the selected tests            (default 10)
#   STRESS_FILTER  nextest filterset                      (default all())
#   STRESS_LOAD    CPU hogs to run alongside, via stress-ng (default 0)
#   NEXTEST_JOBS   nextest -j                             (Makefile default)
#
# Every run after the first starts the tests that were slowest in the first:
# a run lasts until its slowest test ends, and one that starts last stretches
# the run by its whole length. The tests and their number are the same.

set -euo pipefail

count="${STRESS_COUNT:-10}"
filter="${STRESS_FILTER:-all()}"
load="${STRESS_LOAD:-0}"
jobs="${NEXTEST_JOBS:-16}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/shux-test-stress.XXXXXX")"
hog=""
cleanup() {
    if [[ -n "$hog" ]]; then kill "$hog" 2>/dev/null; wait "$hog" 2>/dev/null || true; fi
    rm -rf "$tmp"
}
trap cleanup EXIT
# The first run writes its JUnit report; later runs read their order from it.
config="$tmp/nextest.toml"
cp "$root/.config/nextest.toml" "$config"
printf '\n[profile.default.junit]\npath = "%s"\n' "$tmp/first.xml" >>"$config"

if ((load > 0)); then
    command -v stress-ng >/dev/null || {
        echo "test-stress: STRESS_LOAD=$load needs stress-ng, which is not installed" >&2
        exit 1
    }
    stress-ng --cpu "$load" --timeout 0 >/dev/null 2>&1 &
    hog=$!
fi

# slowest_first: the repo config, plus a nextest priority (100 down to 1) for
# the 100 slowest tests of the first run.
slowest_first() {
    [[ -s "$tmp/first.xml" ]] || { echo "test-stress: the first run wrote no JUnit report" >&2; exit 1; }
    cp "$root/.config/nextest.toml" "$config"
    awk '/<testcase / {
            n = $0; sub(/.* name="/, "", n); sub(/".*/, "", n)
            b = $0; sub(/.* classname="/, "", b); sub(/".*/, "", b)
            t = $0; sub(/.* time="/, "", t); sub(/".*/, "", t)
            print t "\t" b "\t" n
         }' "$tmp/first.xml" | sort -t$'\t' -k1,1gr |
        awk -F'\t' 'NR <= 100 { printf "\n[[profile.default.overrides]]\nfilter = '\''binary_id(=%s) & test(=%s)'\''\npriority = %d\n", $2, $3, 101 - NR }' \
            >>"$config"
}
# A hog that died leaves the run unloaded, which proves nothing about load.
hogs_alive() { ((load == 0)) || kill -0 "$hog" 2>/dev/null; }

echo "▶ $count run(s) of '$filter' with $load CPU hog(s), -j $jobs"
hogs_alive || { echo "test-stress: stress-ng exited before the run started" >&2; exit 1; }
# One nextest run per iteration: `--stress-count` under this repo's
# `fail-fast = false` exits 0 when only some iterations fail.
failed=0
for ((i = 1; i <= count; i++)); do
    status=0
    (cd "$root" && cargo nextest run --config-file "$config" -j "$jobs" --status-level fail --workspace -E "$filter") ||
        status=$?
    # 100 is a test failure; anything else (a build error, no test matched) ends the run.
    case "$status" in
    0) ;;
    100) failed=$((failed + 1)) ;;
    *)
        echo "test-stress: run $i exited $status, not a test failure: stopping" >&2
        exit "$status"
        ;;
    esac
    hogs_alive || { echo "test-stress: stress-ng exited during the run, so it was not loaded throughout" >&2; exit 1; }
    if ((i == 1 && count > 1)); then slowest_first; fi
done
if ((failed > 0)); then
    printf '\033[1;31m✗ %d of %d run(s) failed\033[0m\n' "$failed" "$count"
    exit 100
fi
printf '\033[1;32m✓ %d of %d run(s) passed\033[0m\n' "$count" "$count"
