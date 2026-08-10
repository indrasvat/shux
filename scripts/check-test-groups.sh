#!/usr/bin/env bash
# Assert every nextest test-group actually bounds the tests it claims to.
#
# A test group is the only thing standing between a machine-global resource and
# a suite that now runs fully parallel. It is also completely silent when it is
# wrong: nextest accepts a filterset that matches nothing, prints a group with
# no members, and runs the whole suite at full concurrency. Nothing goes red.
# That is how `test(=shux::bin/shux::tests::foo)` — a filter that reads exactly
# like the identifier nextest itself prints, and matches zero tests — got
# written in the first place.
#
# So membership is asserted, not assumed. Three invariants, all derived from the
# config and the suite rather than from a number someone remembered to update:
#
#   1. every declared group is used, and every used group is declared
#   2. every top-level ARM of every filterset matches at least one test
#   3. no test is claimed by two groups
#
# (2) is per-ARM and not merely per-filterset, because `A + B` where A has gone
# dead still matches everything B matches: the group looks healthy and half its
# intended membership runs unbounded. A pinned count caught that only by
# accident of the number moving.
#
# (3) is the one that used to be enforced by pinning exact per-group counts. The
# real invariant behind that number was never the count: a test belongs to at
# most one group and the FIRST matching override wins, so an earlier, looser
# group silently swallows binaries a later, tighter one names — and both groups
# still look healthy. It happened here, with `pty-pool` and `daemon-backed`
# sharing four binaries. Asserting non-overlap says that directly, and unlike a
# pinned count it does not have to be edited by every PR that adds a test, which
# made it a guaranteed merge conflict whenever two branches added tests at once
# (issue #123).
#
#   scripts/check-test-groups.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

if ! command -v cargo-nextest >/dev/null 2>&1; then
  echo "error: cargo-nextest is not installed. Run: make setup-nextest" >&2
  exit 2
fi

# `tomllib` reads the group config from the same file nextest does, so the guard
# and the runtime cannot disagree about what is declared. It is stdlib from 3.11.
if ! python3 -c 'import tomllib' 2>/dev/null; then
  echo "error: python3 with tomllib (3.11+) is required to read .config/nextest.toml" >&2
  exit 2
fi

# Largest share of the suite any single throttled group may hold, in percent.
#
# A fraction, not `== total`: an overbroad filterset almost never captures
# literally everything — `+ all()` in one of two groups here captured 1928 of
# 1934, which an `== total` check waves straight through while the suite crawls.
# These groups ration genuinely scarce machine-global resources; if a third of
# the suite needs rationing, the premise is wrong and that deserves a human.
export MAX_GROUP_PERCENT="${MAX_GROUP_PERCENT:-30}"

# `--color never` is pinned at the call site below even though this guard now
# reads `--message-format json`, which carries no SGR codes. CI sets
# `CARGO_TERM_COLOR: always` workflow-wide, and the previous version of this
# script parsed nextest's *human* output anchored on a literal group name: every
# group came back "not declared" and the guard failed the build while reporting
# the one thing that was not wrong. It passed locally every time, which is the
# worst shape a guard can have. `make check-ci-parity` runs this under CI's
# environment so that never regresses silently.
exec python3 - "$@" <<'PY'
import json
import os
import subprocess
import sys
import tomllib
from itertools import combinations

MAX_GROUP_PERCENT = int(os.environ.get("MAX_GROUP_PERCENT", "30"))

RED, GREEN, BLUE, OFF = "\033[31m", "\033[32m", "\033[34m", "\033[0m"

status = 0


def err(msg):
    global status
    print(f"  {RED}✗{OFF} {msg}", file=sys.stderr)
    status = 1


def ok(msg):
    print(f"  {GREEN}✓{OFF} {msg}")


def matched_tests(filterset):
    """The tests a filterset selects, evaluated on its own.

    Deliberately NOT `show-config test-groups`, which reports groups already
    resolved by first-match-wins — the very step that hides overlap. Each
    override is evaluated independently so a collision is visible.
    """
    proc = subprocess.run(
        [
            "cargo", "nextest", "list", "--workspace",
            "--color", "never", "--message-format", "json",
            "-E", filterset,
        ],
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        # nextest validates the WHOLE config before it evaluates `-E`, so a bad
        # filterset in any override fails every call here, including `all()`.
        # Print nextest's own diagnostic: it names the offending override.
        err(f"`cargo nextest list -E` failed (exit {proc.returncode}) evaluating:")
        print(f"      {filterset.strip()}", file=sys.stderr)
        for line in proc.stderr.strip().splitlines()[-8:]:
            print(f"      {line}", file=sys.stderr)
        return None
    suites = json.loads(proc.stdout)["rust-suites"]
    return {
        f"{binary_id}::{name}"
        for binary_id, suite in suites.items()
        for name, case in suite["testcases"].items()
        # `filter-match` is load-bearing: `list --message-format json` reports
        # EVERY test in the workspace and marks which ones the filterset
        # selected. Counting the testcases themselves returns the whole suite
        # for every filterset, so every group looks enormous and identical.
        if case.get("filter-match", {}).get("status") == "matches"
    }


def union_arms(filterset):
    """Split a filterset into its top-level `+` arms.

    Union is the only operator that can hide a dead sub-expression: `A + B`
    stays non-empty when A matches nothing. `&` and `!` cannot — an empty
    operand empties the whole expression, which check (2) already catches.

    Depth-aware, and skips `+` inside parens, quoted strings and `/regex/`
    literals. A `/` only opens a regex when it directly follows `(` — otherwise
    it is an ordinary character in an exact match, and treating it as a
    delimiter silently swallows the rest of the expression. `binary_id(=shux::
    bin/shux)` is exactly that case, and it made this whole check a no-op.

    Anything it cannot parse confidently degrades to a single arm, which is the
    old whole-filterset behaviour: weaker, never wrong.
    """
    arms, current, depth = [], [], 0
    quote = None
    previous = ""
    for char in filterset:
        if quote:
            current.append(char)
            if char == quote:
                quote = None
            previous = char
            continue
        if char in "'\"" or (char == "/" and previous == "("):
            quote = char
        elif char == "(":
            depth += 1
        elif char == ")":
            depth -= 1
        elif char == "+" and depth == 0:
            arms.append("".join(current))
            current = []
            previous = char
            continue
        current.append(char)
        if not char.isspace():
            previous = char
    arms.append("".join(current))
    if quote or depth != 0:
        return [filterset]
    return [arm for arm in (a.strip() for a in arms) if arm]


print(f"{BLUE}▶ nextest test-group membership{OFF}")

with open(".config/nextest.toml", "rb") as fh:
    config = tomllib.load(fh)

declared = set(config.get("test-groups", {}))
overrides = [
    override
    for override in config.get("profile", {}).get("default", {}).get("overrides", [])
    if "test-group" in override
]

if not declared:
    err("no test groups are declared in .config/nextest.toml — nothing is bounded")
    sys.exit(1)
if not overrides:
    err("no override assigns any test group — every group is inert")
    sys.exit(1)

for override in overrides:
    if override["test-group"] not in declared:
        err(f"override assigns undeclared group '{override['test-group']}'")
assigned = {override["test-group"] for override in overrides}
for group in sorted(declared - assigned):
    err(f"group '{group}' is declared but no override assigns it — it bounds nothing")

total = matched_tests("all()")
if total is None:
    sys.exit(1)

selections = []
for override in overrides:
    tests = matched_tests(override["filter"])
    if tests is None:
        continue
    group = override["test-group"]
    if not tests:
        err(
            f"group '{group}' has an override matching ZERO tests — its filterset is"
            " dead, so every test it was meant to bound now runs unbounded"
        )
        continue

    arms = union_arms(override["filter"])
    if len(arms) > 1:
        for arm in arms:
            matched = matched_tests(arm)
            if matched is not None and not matched:
                err(
                    f"group '{group}' has a union arm matching ZERO tests, so the"
                    " tests it names run unbounded while the group still looks"
                    f" healthy:\n      {arm}"
                )

    selections.append((group, override["filter"], tests))

# ── No test may be claimed by two groups ────────────────────────────────────
overlapping = set()
for (group_a, _, tests_a), (group_b, _, tests_b) in combinations(selections, 2):
    if group_a == group_b:
        continue
    shared = tests_a & tests_b
    if shared:
        overlapping.update((group_a, group_b))
        err(
            f"groups '{group_a}' and '{group_b}' both claim {len(shared)} test(s)."
            " First match wins, so the later group silently bounds nothing for them:"
        )
        for name in sorted(shared)[:5]:
            print(f"      {name}", file=sys.stderr)
        if len(shared) > 5:
            print(f"      … and {len(shared) - 5} more", file=sys.stderr)
        print(
            "      Exclude them from the looser filterset, e.g."
            " `& !binary_id(=<id>)`.",
            file=sys.stderr,
        )

# ── No group may swallow the suite ──────────────────────────────────────────
ceiling = len(total) * MAX_GROUP_PERCENT // 100
by_group = {}
for group, _, tests in selections:
    by_group.setdefault(group, set()).update(tests)

for group in sorted(by_group):
    count = len(by_group[group])
    if count > ceiling:
        err(
            f"group '{group}' holds {count} of {len(total)} tests"
            f" (>{MAX_GROUP_PERCENT}%). A filterset this broad throttles"
            " most of the suite — check for a stray `all()`, or a"
            " `test(=<binary-id>::<name>)` filter, which matches every test rather"
            " than the one it names."
        )
    elif group in overlapping:
        err(f"group '{group}': {count} test(s) of {len(total)}, but overlapping (above)")
    else:
        ok(f"group '{group}': {count} test(s) of {len(total)}, exclusively claimed")

if status:
    print(file=sys.stderr)
    print("See .config/nextest.toml for what each group protects.", file=sys.stderr)

sys.exit(status)
PY
