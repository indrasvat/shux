#!/usr/bin/env bash
# Assert no test silently stopped being compiled.
#
# Moving `#[cfg(test)] mod tests` blocks between files has one characteristic
# failure, and it is not a broken assertion: a module never gets declared in its
# new parent, so its tests vanish from the binary. That compiles clean, `make
# check` goes green, and the coverage is simply gone. Counting tests does not
# catch it either — one test lost and one gained is the same number.
#
# So the NAMES are compared, against the same names the base commit produced.
# There is no checked-in list to update: the baseline is computed from git, in a
# throwaway worktree, and cached by commit sha. A guard that made every
# test-adding PR edit a tracked file would be a merge conflict waiting for the
# second open branch (issue #123, and the reason `check-test-groups.sh` asserts
# structure rather than pinning counts).
#
# Comparison is on `<binary-id> <leaf test name>`, as a multiset. The module
# path is deliberately dropped: relocating `tests::foo` to `pane_io::tests::foo`
# is the whole point of a split, and pinning the full path would fail on every
# move it is supposed to protect. The leaf name plus the binary is what says
# "this test still runs".
#
# Removals fail. Additions are reported and allowed — a refactor should not add
# tests, but a guard landing alongside its own invariant test would otherwise
# have to fail itself.
#
#   scripts/check-test-inventory.sh [BASE_REF]     # default: origin/main
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

BASE_REF="${1:-${BASE_REF:-origin/main}}"

if ! command -v cargo-nextest >/dev/null 2>&1; then
  echo "error: cargo-nextest is not installed. Run: make setup-nextest" >&2
  exit 2
fi

if ! BASE_SHA="$(git rev-parse --verify "${BASE_REF}^{commit}" 2>/dev/null)"; then
  echo "error: cannot resolve base ref '${BASE_REF}'. Fetch it first: git fetch origin main" >&2
  exit 2
fi
# Compare against the commit this branch actually forked from, not the tip of
# main — otherwise every test another branch merged in the meantime reads as a
# test this branch added.
if MERGE_BASE="$(git merge-base HEAD "${BASE_SHA}" 2>/dev/null)"; then
  BASE_SHA="${MERGE_BASE}"
fi

CACHE_DIR="${REPO_ROOT}/.shux/out/test-inventory"
mkdir -p "${CACHE_DIR}"
BASE_LIST="${CACHE_DIR}/${BASE_SHA}.txt"

# `<binary-id>\t<leaf test name>\t<occurrence>`, sorted, one line per test.
# `cargo nextest list` prints `<binary-id> <module::path::test_name>`;
# everything up to the last `::` is the module path, and that is exactly what a
# move is allowed to change. The occurrence index keeps two same-named tests in
# one binary distinguishable, so losing one of them still reads as a removal
# while the lines stay sorted (which `comm` requires).
normalize() {
  sed -e 's/[[:space:]]*$//' -e '/^$/d' \
    | awk '{ id = $1; $1 = ""; sub(/^ /, ""); leaf = $0; sub(/^.*::/, "", leaf); print id "\t" leaf }' \
    | sort \
    | awk '{ print $0 "\t" (++seen[$0]) }'
}

list_tests() {
  # `--color never` at the call site: CI exports CARGO_TERM_COLOR=always, and a
  # parser that only sees the uncoloured form is a guard that only fails locally.
  # stderr is left alone on purpose — a compile failure here must be readable,
  # not swallowed into an empty list.
  cargo nextest list --workspace --color never | normalize
}

if [[ ! -s "${BASE_LIST}" ]]; then
  echo "▶ listing tests at base ${BASE_SHA:0:12} (cached afterwards)..."
  WORKTREE="$(mktemp -d "${TMPDIR:-/tmp}/shux-test-inventory-XXXXXX")"
  cleanup() {
    git worktree remove --force "${WORKTREE}" >/dev/null 2>&1 || true
    rm -rf "${WORKTREE}"
  }
  trap cleanup EXIT
  git worktree add --detach --quiet "${WORKTREE}" "${BASE_SHA}"
  # A separate target dir: sharing the working tree's would make the two
  # checkouts evict each other's artifacts on every run.
  (
    cd "${WORKTREE}"
    CARGO_TARGET_DIR="${CACHE_DIR}/target" list_tests
  ) >"${BASE_LIST}.tmp"
  if [[ ! -s "${BASE_LIST}.tmp" ]]; then
    rm -f "${BASE_LIST}.tmp"
    echo "error: listed zero tests at base ${BASE_SHA:0:12} — refusing to compare against nothing" >&2
    exit 2
  fi
  mv "${BASE_LIST}.tmp" "${BASE_LIST}"
  cleanup
  trap - EXIT
fi

CURRENT_LIST="$(mktemp "${TMPDIR:-/tmp}/shux-test-inventory-cur-XXXXXX")"
trap 'rm -f "${CURRENT_LIST}"' EXIT
list_tests >"${CURRENT_LIST}"

if [[ ! -s "${CURRENT_LIST}" ]]; then
  echo "error: listed zero tests in the working tree — the suite does not build" >&2
  exit 2
fi

BASE_COUNT="$(wc -l <"${BASE_LIST}" | tr -d ' ')"
CUR_COUNT="$(wc -l <"${CURRENT_LIST}" | tr -d ' ')"

removed="$(comm -23 "${BASE_LIST}" "${CURRENT_LIST}" || true)"
added="$(comm -13 "${BASE_LIST}" "${CURRENT_LIST}" || true)"

echo "  base ${BASE_SHA:0:12}: ${BASE_COUNT} tests"
echo "  working tree:      ${CUR_COUNT} tests"

if [[ -n "${added}" ]]; then
  echo ""
  echo "  added (allowed):"
  echo "${added}" | cut -f1,2 | sed 's/^/    + /'
fi

if [[ -n "${removed}" ]]; then
  echo ""
  echo "error: these tests no longer run:" >&2
  echo "${removed}" | cut -f1,2 | sed 's/^/    - /' >&2
  echo "" >&2
  echo "A test that disappears from the listing is a defect in the move, not a" >&2
  echo "tidy-up: an undeclared module compiles clean and covers nothing." >&2
  exit 1
fi

echo "✓ every test at ${BASE_SHA:0:12} still runs"
