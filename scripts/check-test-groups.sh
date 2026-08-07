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

# Groups that must exist and must not be empty. Add a group here when you add
# one to .config/nextest.toml — a group nobody asserts on is a group that can
# quietly stop matching.
REQUIRED_GROUPS=(process-table pty-pool wall-clock daemon-backed)

# Largest share of the suite any single throttled group may hold, in percent.
MAX_GROUP_PERCENT=${MAX_GROUP_PERCENT:-30}

# Deliberately NOT `raw=$(... )` under `set -e`: that aborts the script with
# nextest's own exit code and no explanation, which is the one failure mode a
# guard must never have. Capture, then report.
set +e
raw="$(cargo nextest show-config test-groups --workspace 2>/tmp/.shux-test-groups.err)"
show_status=$?
set -e

if [[ "${show_status}" -ne 0 ]]; then
  echo "✗ 'cargo nextest show-config test-groups' failed (exit ${show_status})." >&2
  echo "  .config/nextest.toml is almost certainly invalid — nextest said:" >&2
  sed 's/^/    /' /tmp/.shux-test-groups.err >&2 || true
  rm -f /tmp/.shux-test-groups.err
  exit 1
fi
rm -f /tmp/.shux-test-groups.err

if [[ -z "${raw}" ]]; then
  echo "✗ 'cargo nextest show-config test-groups' produced no output — no groups" >&2
  echo "  are configured at all, so nothing is bounded." >&2
  exit 1
fi

status=0

total="$(cargo nextest list --workspace --message-format json 2>/dev/null |
  python3 -c 'import json,sys; d=json.load(sys.stdin); print(sum(len(v["testcases"]) for v in d["rust-suites"].values()))')"

for group in "${REQUIRED_GROUPS[@]}"; do
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

  echo "✓ test-group '${group}': ${count} test(s) of ${total}"
done

if [[ "${status}" -ne 0 ]]; then
  echo >&2
  echo "See .config/nextest.toml for what each group protects." >&2
fi

exit "${status}"
