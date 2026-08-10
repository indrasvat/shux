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

_raw_percent = os.environ.get("MAX_GROUP_PERCENT", "30")
try:
    MAX_GROUP_PERCENT = int(_raw_percent)
except ValueError:
    print(f"error: MAX_GROUP_PERCENT={_raw_percent!r} is not an integer", file=sys.stderr)
    sys.exit(2)
if not 0 < MAX_GROUP_PERCENT <= 100:
    print(f"error: MAX_GROUP_PERCENT={MAX_GROUP_PERCENT} must be in 1..100", file=sys.stderr)
    sys.exit(2)

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


def _word_bounded(text, index, length):
    """True when text[index:index+length] is not glued to an identifier."""
    before = text[index - 1] if index else " "
    after = text[index + length] if index + length < len(text) else " "
    return not (before.isalnum() or before == "_") and not (
        after.isalnum() or after == "_"
    )


def union_operands(text):
    """Spans of every union operand in `text`, at EVERY nesting depth.

    ONE pass over the original string, tracking depth, quotes, `/regex/` and
    backslash escapes together, recording absolute spans. The previous version
    recursed into parenthesised substrings and re-scanned them, which broke three
    ways at once:

      * `|` and the `or` keyword are unions in nextest exactly like `+`, and were
        not recognised at all — so an expression joined with `|` had NO operands
        checked, and a dead arm in one was invisible;
      * a `\/` inside a regex ended the quote early, leaving a stray `)` to
        desynchronise depth and disable the scan for the rest of the string;
      * a substring handed to the recursion lost its opening `(`, so a leading
        `/` was no longer "directly after `(`" and a regex quantifier like `.+`
        was split as if it were a union — rejecting a perfectly valid config.

    Spans are into the ORIGINAL text, so the caller can neutralise an operand in
    place without any coordinate translation.
    """
    frames = [{"start": 0, "seps": []}]
    found = []

    def close(frame, stop):
        if not frame["seps"]:
            return
        cuts = [frame["start"]]
        for sep_start, sep_end in frame["seps"]:
            cuts.append(sep_start)
            cuts.append(sep_end)
        cuts.append(stop)
        for lo, hi in zip(cuts[::2], cuts[1::2]):
            span = text[lo:hi]
            lead = len(span) - len(span.lstrip())
            trail = len(span) - len(span.rstrip())
            if span.strip():
                found.append((lo + lead, hi - trail))

    quote = None
    escaped = False
    previous = ""
    index = 0
    while index < len(text):
        char = text[index]
        if quote:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == quote:
                quote = None
            index += 1
            continue
        if char in "'\"" or (char == "/" and previous == "("):
            quote = char
            previous = char
            index += 1
            continue
        if char == "(":
            frames.append({"start": index + 1, "seps": []})
            previous = char
            index += 1
            continue
        if char == ")":
            if len(frames) > 1:
                close(frames.pop(), index)
            previous = char
            index += 1
            continue
        width = 0
        if char in "+|":
            width = 1
        elif text.startswith("or", index) and _word_bounded(text, index, 2):
            width = 2
        if width:
            frames[-1]["seps"].append((index, index + width))
            previous = "+"
            index += width
            continue
        if not char.isspace():
            previous = char
        index += 1

    close(frames[0], len(text))
    return found


print(f"{BLUE}▶ nextest test-group membership{OFF}")

try:
    with open(".config/nextest.toml", "rb") as fh:
        config = tomllib.load(fh)
except (OSError, tomllib.TOMLDecodeError) as exc:
    err(f".config/nextest.toml could not be read: {exc}")
    sys.exit(1)

declared = set(config.get("test-groups", {}))

# EVERY profile that assigns groups, not just `default`. Per-profile overrides
# take precedence over the default profile's, so a `[[profile.ci.overrides]]`
# can move tests out of a group in the exact profile CI selects while the
# default profile — and therefore this guard — still looks healthy. Nothing in
# this repo currently selects a non-default profile, which makes this latent
# rather than live; it is checked so it cannot go live unnoticed.
overrides = []
for profile_name, profile in sorted(config.get("profile", {}).items()):
    for override in profile.get("overrides", []):
        if "test-group" not in override:
            continue
        # A `platform` key makes membership conditional on the HOST. `-E` knows
        # nothing about it, so the guard would compute a full group while
        # nextest bounds nobody here — reproduced: guard 293, reality 0.
        if "platform" in override:
            err(
                f"profile '{profile_name}' has a platform-conditional grouped"
                f" override (platform = {override['platform']!r}). This guard"
                " evaluates filtersets with `-E`, which ignores `platform`, so it"
                " cannot tell whether the group bounds anything on this host."
                " Drop `platform`, or split the group so membership is"
                " unconditional."
            )
            continue
        overrides.append((profile_name, override))

if not declared:
    err("no test groups are declared in .config/nextest.toml — nothing is bounded")
    sys.exit(1)
if not overrides:
    err("no override assigns any test group — every group is inert")
    sys.exit(1)

for profile_name, override in overrides:
    if override["test-group"] not in declared:
        err(
            f"profile '{profile_name}' assigns undeclared group"
            f" '{override['test-group']}'"
        )
assigned = {override["test-group"] for _, override in overrides}
for group in sorted(declared - assigned):
    err(f"group '{group}' is declared but no override assigns it — it bounds nothing")

total = matched_tests("all()")
if total is None:
    sys.exit(1)

selections = []
for profile_name, override in overrides:
    # nextest accepts a grouped override with NO filter — it then matches
    # everything. `override["filter"]` died on it with a bare KeyError before the
    # ceiling could report the whole suite being throttled.
    expression = override.get("filter", "all()")
    if not isinstance(expression, str):
        err(f"profile '{profile_name}' has a non-string filter: {expression!r}")
        continue
    tests = matched_tests(expression)
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

# ── Everything that needs a bound must have one ─────────────────────────────
#
# The three checks above all ask "are the arms you wrote alive and disjoint?".
# None of them notices an arm being NARROWED. Adding `& !binary_id(=…)` to the
# daemon-pty filterset dropped four suites — 76 tests, three of them the ones
# this config's own comments cite as measured failures under oversubscription —
# into no group at all, and the guard called every group healthy. That is the
# coverage the pinned per-group counts used to provide, and dropping them lost
# it.
#
# Restored WITHOUT a shared ledger: the requirement is derived from the packages
# themselves. Every integration test in the packages whose integration tests
# boot a daemon or allocate a PTY must be claimed by SOME group. Narrowing a
# filterset now leaves tests uncovered and fails here. A deliberate un-grouping
# is still possible — it just has to say so by editing this predicate, which is
# code-derived and per-branch, not a count every PR has to bump.
MUST_BE_BOUNDED = "(package(shux) & kind(test)) + (package(shux-pty) & kind(test))"

needs_bound = matched_tests(MUST_BE_BOUNDED)
if needs_bound is not None:
    claimed = set().union(*(tests for _, _, tests in selections)) if selections else set()
    unbounded = needs_bound - claimed
    if unbounded:
        err(
            f"{len(unbounded)} daemon-backed test(s) belong to NO group, so they run"
            " unbounded. A filterset was narrowed (or a suite was added) without a"
            " group to hold it:"
        )
        for name in sorted(unbounded)[:8]:
            print(f"      {name}", file=sys.stderr)
        if len(unbounded) > 8:
            print(f"      … and {len(unbounded) - 8} more", file=sys.stderr)
    else:
        ok(f"coverage: all {len(needs_bound)} daemon-backed test(s) are bounded")

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
