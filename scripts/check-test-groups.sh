#!/usr/bin/env bash
# Fail if any nextest test-group is empty.
#
# A test group is the only thing standing between a machine-global resource and
# a suite that now runs fully parallel. It is also completely silent when it is
# wrong: nextest accepts a filterset that matches nothing, prints a group with
# no members, and runs the whole suite at full concurrency. Nothing goes red.
# That is how `test(=shux::bin/shux::tests::foo)` — a filter that reads exactly
# like the identifier nextest itself prints, and matches zero tests — got
# written in the first place.
#
# So the membership is asserted, not assumed. Each declared group must contain
# at least one test, and (optionally) exactly the number of tests we expect.
#
#   scripts/check-test-groups.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

if ! command -v cargo-nextest >/dev/null 2>&1; then
  echo "error: cargo-nextest is not installed. Run: make setup-nextest" >&2
  exit 2
fi

# Groups that must exist, with the EXACT number of tests each must hold.
#
# An exact count, not just "non-empty". A test belongs to at most one group and
# the first matching override wins, so a group listed earlier silently swallows
# binaries a later one names — and both groups still look healthy. That is not
# hypothetical: `pty-pool` and `daemon-backed` shipped as two groups sharing
# four binaries, the earlier one took all of them, and the later one's tuning
# comments described a membership it did not have. Nothing was red.
#
# Update these when you deliberately add or remove tests. The number moving on
# its own is the signal.
#
#   <group>:<expected test count>
REQUIRED_GROUPS=(
  process-table:5
  daemon-pty:263
  wall-clock:2
)

# Largest share of the suite any single throttled group may hold, in percent.
MAX_GROUP_PERCENT=${MAX_GROUP_PERCENT:-30}

# Deliberately NOT `raw=$(... )` under `set -e`: that aborts the script with
# nextest's own exit code and no explanation, which is the one failure mode a
# guard must never have. Capture, then report.
# A per-run file, not a fixed literal. Two concurrent `make check` runs sharing
# one path clobber and then delete each other's diagnostic — in the guard.
err_file="$(mktemp "${TMPDIR:-/tmp}/shux-test-groups.XXXXXX")"
trap 'rm -f "${err_file}"' EXIT

# `--color never` is load-bearing, not cosmetic. CI sets `CARGO_TERM_COLOR:
# always` workflow-wide, which makes nextest wrap the group NAME in SGR codes —
# `group: \e[1;4mdaemon-pty\e[0m (max threads = ...)`. The parse below anchors on
# the literal name, so every group came back "not declared" and this guard failed
# the build while reporting the one thing that was not wrong. It passed locally
# and only ever failed in CI, which is the worst shape a guard can have.
#
# The `sed` is belt-and-braces for any other source of colour (a `NEXTEST_*`
# override, a future default): a guard should not be re-breakable by an
# environment variable.
set +e
raw="$(cargo nextest --color never show-config test-groups --workspace 2>"${err_file}" \
  | sed $'s/\033\[[0-9;]*m//g')"
show_status=${PIPESTATUS[0]}
set -e

if [[ "${show_status}" -ne 0 ]]; then
  echo "✗ 'cargo nextest show-config test-groups' failed (exit ${show_status})." >&2
  echo "  .config/nextest.toml is almost certainly invalid — nextest said:" >&2
  sed 's/^/    /' ${err_file} >&2 || true
  rm -f ${err_file}
  exit 1
fi
rm -f ${err_file}

if [[ -z "${raw}" ]]; then
  echo "✗ 'cargo nextest show-config test-groups' produced no output — no groups" >&2
  echo "  are configured at all, so nothing is bounded." >&2
  exit 1
fi

status=0

total="$(cargo nextest list --workspace --message-format json 2>/dev/null |
  python3 -c 'import json,sys; d=json.load(sys.stdin); print(sum(len(v["testcases"]) for v in d["rust-suites"].values()))')"

for entry in "${REQUIRED_GROUPS[@]}"; do
  group="${entry%%:*}"
  expected="${entry##*:}"
  # Slice the block between this group's header and the next one, counting the
  # test lines (indented deeper than the binary-id headers, which end in ':').
  # Declared-ness and membership come from the SAME pass: two independent
  # parses of the same text can disagree, and when they do the guard reports
  # the wrong failure — which is how "not declared" ended up being printed for
  # a group that was declared perfectly well.
  read -r declared count <<<"$(
    printf '%s\n' "${raw}" | awk -v g="group: ${group} " '
      index($0, g) == 1 { inblock = 1; seen = 1; next }
      /^group: / { inblock = 0 }
      inblock && /^ {10,}[^ ]/ && $0 !~ /:$/ { n++ }
      END { print (seen ? "yes" : "no"), n + 0 }
    '
  )"

  if [[ "${declared}" != "yes" ]]; then
    echo "✗ test-group '${group}' is not declared in .config/nextest.toml" >&2
    status=1
    continue
  fi

  if [[ "${count}" -eq 0 ]]; then
    echo "✗ test-group '${group}' matched ZERO tests — its filterset is dead," >&2
    echo "  so every test it was meant to bound is now running unbounded." >&2
    status=1
    continue
  fi

  # A group that swallows the workspace is the opposite failure and just as
  # silent: the suite still passes, it merely takes six minutes again.
  #
  # The ceiling is a FRACTION, not `== total`. An overbroad filterset almost
  # never captures literally everything — `+ all()` in one of two groups here
  # captures 1928 of 1934, which an `== total` check waves straight through
  # while the suite crawls. These groups exist to ration genuinely scarce
  # machine-global resources; if a third of the suite needs rationing, the
  # premise is wrong and that deserves a human, not a silent pass.
  ceiling=$(( total * MAX_GROUP_PERCENT / 100 ))
  if [[ "${count}" -gt "${ceiling}" ]]; then
    echo "✗ test-group '${group}' holds ${count} of ${total} tests (>${MAX_GROUP_PERCENT}%)." >&2
    echo "  A filterset this broad throttles most of the suite. Check for a" >&2
    echo "  stray 'all()', or a 'test(=<binary-id>::<name>)' filter — that form" >&2
    echo "  matches every test rather than the one it names." >&2
    status=1
    continue
  fi

  if [[ "${count}" -ne "${expected}" ]]; then
    echo "✗ test-group '${group}' holds ${count} tests; expected ${expected}." >&2
    echo "  Either a test moved in or out of the group, or an EARLIER group in" >&2
    echo "  .config/nextest.toml is now capturing binaries this one names —" >&2
    echo "  first match wins, so overlap is silent. Run:" >&2
    echo "      cargo nextest show-config test-groups --workspace" >&2
    echo "  If the change is intentional, update REQUIRED_GROUPS in $0." >&2
    status=1
    continue
  fi

  echo "✓ test-group '${group}': ${count} test(s) of ${total} (expected ${expected})"
done

if [[ "${status}" -ne 0 ]]; then
  echo >&2
  echo "See .config/nextest.toml for what each group protects." >&2
fi

exit "${status}"
