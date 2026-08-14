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
# move it is supposed to protect. The `::bin/<name>` target suffix is dropped
# for the same reason (see `normalize`). The leaf name plus the package — and,
# for integration tests, the file — is what says "this test still runs".
#
# Removals fail. Additions are reported and allowed — a refactor should not add
# tests, but a guard landing alongside its own invariant test would otherwise
# have to fail itself.
#
# Wired into `make ci` and the CI `Test (ubuntu)` job on pull requests only.
# NOT into `make check`: the first run against a given base pays a full
# workspace build in the worktree, and the pre-commit gate is the wrong place
# for that. On a push to main the merge-base is the head, so there is nothing
# to compare and the run would be pure cost — hence the pull-request condition
# in `.github/workflows/ci.yml`.
#
# The baseline is cached per commit sha, so a run that already has the file for
# its merge-base does no build at all. `--write-baseline` populates that file for
# the CURRENT checkout from an already-warm target dir, which is what lets CI on
# main publish its own list for PRs to compare against instead of every PR paying
# a from-cold rebuild of the merge-base (issue #157).
#
#   scripts/check-test-inventory.sh [BASE_REF]     # default: origin/main
#   scripts/check-test-inventory.sh --write-baseline
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

WRITE_BASELINE=0
if [[ "${1:-}" == "--write-baseline" ]]; then
  WRITE_BASELINE=1
  shift
fi

BASE_REF="${1:-${BASE_REF:-origin/main}}"

if ! command -v cargo-nextest >/dev/null 2>&1; then
  echo "error: cargo-nextest is not installed. Run: make setup-nextest" >&2
  exit 2
fi

CACHE_DIR="${REPO_ROOT}/.shux/out/test-inventory"
mkdir -p "${CACHE_DIR}"

# `<binary-id>\t<leaf test name>\t<occurrence>`, sorted, one line per test.
# `cargo nextest list` prints `<binary-id> <module::path::test_name>`;
# everything up to the last `::` is the module path, and that is exactly what a
# move is allowed to change. The occurrence index keeps two same-named tests in
# one binary distinguishable, so losing one of them still reads as a removal
# while the lines stay sorted (which `comm` requires).
#
# The `::bin/<name>` suffix is dropped for the same reason the module path is.
# A package's unit tests are compiled into whichever target owns its module
# tree, and giving `crates/shux` a library target moved all 506 of them from
# `shux::bin/shux` to `shux` without changing one line of test code. Keeping the
# target in the key made that read as 506 removals plus 506 additions — a guard
# firing on the refactor it exists to protect. Integration-test binaries
# (`shux::m0_integration`) keep their own ids: those are separate files, and a
# test vanishing from one is a real loss.
#
# This does mean a unit test moving between the bin and lib target of one
# package is invisible here. That is the intent — it still runs, which is the
# only question this guard asks. Deleting it outright still drops the leaf name
# from the package's multiset, and still fails.
normalize() {
  sed -e 's/[[:space:]]*$//' -e '/^$/d' \
    | awk '{ id = $1; $1 = ""; sub(/^ /, ""); leaf = $0; sub(/^.*::/, "", leaf);
             sub(/::bin\/[^\/]+$/, "", id); print id "\t" leaf }' \
    | sort \
    | awk '{ print $0 "\t" (++seen[$0]) }'
}

# Baselines are cached by content, so they must also be keyed by the code that
# produced them. `normalize` just changed shape; a baseline written by the old
# one and compared by the new one reports every renamed target as removed. CI
# already folds this script's hash into its cache key for exactly that reason —
# the local cache needs the same discriminator or a local run silently compares
# two incompatible formats and blames the working tree.
# `sha256sum` is GNU coreutils and absent on macOS; `shasum -a 256` ships with
# perl on both. Same fallback order as scripts/build-release.sh, and both print
# `<hex>  <path>`, so the leading 12 characters agree either way.
if command -v sha256sum >/dev/null 2>&1; then
  SCRIPT_DISC="$(sha256sum "${BASH_SOURCE[0]}" | cut -c1-12)"
else
  SCRIPT_DISC="$(shasum -a 256 "${BASH_SOURCE[0]}" | cut -c1-12)"
fi

list_tests() {
  # `--color never` at the call site: CI exports CARGO_TERM_COLOR=always, and a
  # parser that only sees the uncoloured form is a guard that only fails locally.
  # stderr is left alone on purpose — a compile failure here must be readable,
  # not swallowed into an empty list.
  cargo nextest list --workspace --color never | normalize
}

# `--write-baseline`: publish THIS commit's list so later runs can compare
# against it without rebuilding it. Shares `list_tests` with the comparison path
# below deliberately — a second copy of `normalize` would drift, and the drift
# would read as "every test was removed".
# Baselines are named by the `crates/` TREE hash, not the commit sha. The test
# list is a function of the source, and two commits with identical sources have
# identical lists — so this is both more accurate and the thing that makes the
# cache actually hit. semantic-release lands `chore(release): X.Y.Z [skip ci]`
# on main, touching only CHANGELOG.md / Cargo.toml / Cargo.lock and running no
# CI. Main's HEAD is therefore usually a commit that published no baseline, and
# the PRs that fork from it would miss on every lookup keyed by sha. The tree
# hash is unchanged across every release commit in this repo's history.
#
# It fails in the safe direction too: drop a workspace member from the root
# Cargo.toml without touching crates/ and the tree still matches, so the cached
# list still names the tests that stopped compiling — and the guard reports them
# removed, which is exactly right.
crates_tree() { git rev-parse "${1}^{commit}:crates"; }

if [[ "${WRITE_BASELINE}" -eq 1 ]]; then
  HEAD_SHA="$(crates_tree HEAD)"
  OUT="${CACHE_DIR}/${HEAD_SHA}-${SCRIPT_DISC}.txt"
  list_tests >"${OUT}.tmp"
  if [[ ! -s "${OUT}.tmp" ]]; then
    rm -f "${OUT}.tmp"
    echo "error: listed zero tests at HEAD — refusing to publish an empty baseline" >&2
    exit 2
  fi
  mv "${OUT}.tmp" "${OUT}"
  echo "✓ published baseline for crates tree ${HEAD_SHA:0:12}: $(wc -l <"${OUT}" | tr -d ' ') tests"
  exit 0
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
BASE_TREE="$(crates_tree "${BASE_SHA}")"
BASE_LIST="${CACHE_DIR}/${BASE_TREE}-${SCRIPT_DISC}.txt"

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
