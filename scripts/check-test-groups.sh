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


def _scan(text):
    """Yield (index, char, depth, in_quote) with quoting and nesting resolved.

    A `/` only opens a regex when it directly follows `(`. Otherwise it is an
    ordinary character in an exact match, and treating it as a delimiter
    silently swallows the rest of the expression — `binary_id(=shux::bin/shux)`
    is exactly that case, and it once made this whole check a no-op.
    """
    depth, quote, previous = 0, None, ""
    for index, char in enumerate(text):
        if quote:
            yield index, char, depth, True
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
        yield index, char, depth, False
        if not char.isspace():
            previous = char


def _top_level_operands(text):
    """Spans of the `+`-separated operands at depth 0 of `text`."""
    spans, start = [], 0
    for index, char, depth, quoted in _scan(text):
        if not quoted and char == "+" and depth == 0:
            spans.append((start, index))
            start = index + 1
    spans.append((start, len(text)))
    return spans


def _paren_interiors(text):
    """Spans of the interior of each outermost parenthesised group in `text`."""
    groups, opened = [], None
    for index, char, depth, quoted in _scan(text):
        if quoted:
            continue
        if char == "(" and depth == 1:
            opened = index + 1
        elif char == ")" and depth == 0 and opened is not None:
            groups.append((opened, index))
            opened = None
    return groups


def union_operands(text, base=0):
    """Spans of every union operand in `text`, at EVERY nesting depth.

    Splitting only at depth 0 misses the unions this config already has:
    `binary_id(X) & (test(a) + test(b))`. Rename `test(a)` and the outer operand
    — and the group — stay non-empty, so the guard passes while a
    process-counting test escapes its single-threaded group. Nesting is where
    the interesting unions live, so the walk has to be recursive.
    """
    found = []
    operands = _top_level_operands(text)
    if len(operands) > 1:
        found.extend((base + s, base + e) for s, e in operands)
    for start, end in operands:
        for open_at, close_at in _paren_interiors(text[start:end]):
            found.extend(
                union_operands(
                    text[start + open_at : start + close_at], base + start + open_at
                )
            )
    return found


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

    # Each union operand must CONTRIBUTE. Neutralising one with `none()` and
    # re-evaluating the whole expression tests it in its real surrounding
    # context, which is what makes nested unions checkable at all: for
    # `A & (B + C)`, dropping B leaves `A & (none() + C)` = `A & C`, and if that
    # still matches everything the original did, B was dead weight.
    expression = override["filter"]
    for start, end in union_operands(expression):
        neutralised = f"{expression[:start]} none() {expression[end:]}"
        without = matched_tests(neutralised)
        if without is not None and without == tests:
            err(
                f"group '{group}' has a union operand that matches nothing the rest"
                " of the filterset does not already match, so the tests it names are"
                f" not actually bounded by it:\n      {expression[start:end].strip()}"
            )

    selections.append((group, expression, tests))

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
