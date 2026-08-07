#!/usr/bin/env bash
# Run the checks that PARSE cargo output under CI's environment, locally.
#
# Why this exists, precisely:
#
# CI sets `CARGO_TERM_COLOR: always` workflow-wide, because GitHub's log viewer
# renders ANSI but cargo suppresses colour when stdout is not a TTY — without it
# every CI log is monochrome. That is a legitimate choice and this script does
# not argue with it.
#
# The consequence is that CI feeds every cargo-output parser COLOURED bytes,
# while locally those same parsers almost always see plain ones: cargo colours
# on a TTY, and scripts (and agents) pipe. So the one environment that exercises
# the harder input is the one you cannot iterate in.
#
# That is not hypothetical. `check-test-groups.sh` anchored on `group: <name> `,
# nextest emitted `group: \e[1;4mdaemon-pty\e[0m (...)`, every group came back
# "not declared", and the guard failed the build while reporting the one thing
# that was not wrong. It passed locally every single time. A guard that only
# fails in CI is the worst shape a guard can have.
#
# The fix is two-layered and this script is the second layer:
#
#   1. Every parser pins its own input with `--color never`. A parser cannot
#      rely on the environment to leave the bytes alone — not CI's, not a
#      developer's shell profile, not a future cargo default.
#   2. This script proves layer 1 still holds, by running those checks under
#      the environment CI actually uses. If someone adds a parser that forgets
#      `--color never`, this goes red on their machine rather than on the runner.
#
# Add a check here whenever you add something that reads cargo's human-readable
# output.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

# Exactly what .github/workflows/ci.yml sets at workflow level.
export CARGO_TERM_COLOR=always
export RUSTFLAGS="${RUSTFLAGS:--Dwarnings}"

status=0

run_under_ci_env() {
  local what="$1"
  shift
  if "$@" >/dev/null 2>&1; then
    echo "✓ ${what} survives CARGO_TERM_COLOR=always"
  else
    echo "✗ ${what} FAILS under CARGO_TERM_COLOR=always but passes without it." >&2
    echo "  This is what CI runs. Re-run it yourself to see the real output:" >&2
    echo "      CARGO_TERM_COLOR=always $*" >&2
    echo "  The usual cause is a parser reading cargo's human-readable output" >&2
    echo "  without '--color never', so ANSI escapes break an anchored match." >&2
    status=1
  fi
}

run_under_ci_env "test-group membership" bash scripts/check-test-groups.sh

if [[ "${status}" -ne 0 ]]; then
  echo >&2
  echo "See scripts/check-ci-parity.sh for why colour parity is checked at all." >&2
fi

exit "${status}"
