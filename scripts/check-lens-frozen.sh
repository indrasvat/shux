#!/usr/bin/env bash
# check-lens-frozen.sh — enforce the lens test-integrity protocol (PRD §16.2 /
# DEC-9). Two independently-trailered FROZEN lanes:
#
#   LENS lane  .shux/fixtures/lens/**   and   crates/shux/tests/lens_*
#                (EXCLUDING the gate paths below)      → LENS-TEST-CHANGE:
#   GATE lane  .shux/fixtures/lens-gate/** and crates/shux/tests/lens_gate_*
#                (the lens-gate contract, task 078)    → GATE-TEST-CHANGE:
#
# The GATE lane is a SUBSET of the LENS test-path prefix — `crates/shux/tests/
# lens_` also matches `lens_gate_*` — so the GATE lane is matched FIRST on its
# own regex and the LENS lane explicitly EXCLUDES the gate test paths. A commit
# touching both lanes needs BOTH trailers. (The fixtures paths do NOT collide:
# `^.shux/fixtures/lens/` does not match `.shux/fixtures/lens-gate/`.)
#
# Any diff touching a lane's paths is rejected UNLESS the commit message carries
# that lane's trailer with a NON-EMPTY reason, verified via `git
# interpret-trailers --parse` (a body-grep would accept the string anywhere,
# including quoted text — p0-council-r1 major 10). This catches silent
# assertion-weakening and helper extraction (the helpers live under the frozen
# paths too). All file listings use --no-renames so a `git mv` of a frozen file
# OUT of a guarded prefix decomposes into delete(old)+add(new) — the
# frozen-prefix delete is caught instead of only the (unguarded) destination
# path (PR #86 bot review).
#
# Modes:
#   check-lens-frozen.sh <commit-msg-file>
#       commit-msg hook: inspect the STAGED diff against that message.
#   check-lens-frozen.sh
#       CI / range mode: inspect every commit in BASE..HEAD
#       (BASE = $LENS_FROZEN_BASE, default origin/main) and require each lane's
#       trailer on any commit that touches that lane. Merge commits are diffed
#       against their FIRST PARENT — never skipped (skipping merges would be a
#       bypass). Shallow/rootless fallback inspects HEAD itself.

set -euo pipefail

# Frozen lanes as parallel arrays: (key, regex, exclude-regex). The GATE lane
# is checked on its own regex; the LENS lane subtracts the gate test paths so a
# `lens_gate_*` change is owned by exactly one lane (the GATE lane).
LANE_KEYS=('GATE-TEST-CHANGE' 'LENS-TEST-CHANGE')
LANE_RES=(
	'^(\.shux/fixtures/lens-gate/|crates/shux/tests/lens_gate(_|\.))'
	'^(\.shux/fixtures/lens/|crates/shux/tests/lens_)'
)
LANE_EXCLUDES=(
	''
	'^crates/shux/tests/lens_gate(_|\.)'
)

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "${REPO_ROOT}"

sgrep() { grep -E "$@" || true; }

# Filter a newline-separated file list (stdin) to one lane: keep lines matching
# $1 (regex) and, if $2 is non-empty, drop lines matching $2 (exclude regex).
lane_filter() {
	local re="$1" excl="$2"
	if [ -n "${excl}" ]; then
		sgrep "${re}" | { grep -vE "${excl}" || true; }
	else
		sgrep "${re}"
	fi
}

# True iff stdin (a full commit message) carries the given trailer key with a
# non-empty value, per git's own trailer parser.
has_trailer() {
	local key="$1"
	git interpret-trailers --parse 2>/dev/null |
		grep -qE "^${key}: [^[:space:]].*"
}

fail_frozen() {
	# $1 = trailer key, $2 = human context, $3 = newline-separated touched files
	local key="$1"
	echo "✗ lens frozen-path guard: $2" >&2
	echo "  touched frozen paths (${key} lane):" >&2
	printf '%s\n' "$3" | sed 's/^/    /' >&2
	echo "" >&2
	echo "  These paths are FROZEN (PRD §16.2 / task 078). To change them, add a" >&2
	echo "  commit trailer (with a non-empty reason) explaining why, e.g.:" >&2
	echo "" >&2
	echo "      ${key}: re-baseline after an intentional contract change" >&2
	echo "" >&2
	echo "  and record the council verdict for the change in the PR description." >&2
	exit 1
}

# All changed files for one commit (merge commits diff against parent 1; root
# commits diff against the empty tree). --no-renames so a move out of a guarded
# prefix is caught as a delete.
commit_changed_files() {
	local sha="$1"
	if git rev-parse --verify --quiet "${sha}^1" >/dev/null; then
		git diff --no-renames --name-only "${sha}^1" "${sha}"
	else
		git diff-tree --no-renames --no-commit-id --name-only -r --root "${sha}"
	fi
}

# Require the appropriate trailer(s) on one commit; report + set status=1.
check_commit() {
	local sha="$1"
	local all files msg i
	all="$(commit_changed_files "${sha}")"
	[ -n "${all}" ] || return 0
	msg="$(git log -1 --format='%B' "${sha}")"
	for i in "${!LANE_KEYS[@]}"; do
		files="$(printf '%s\n' "${all}" | lane_filter "${LANE_RES[$i]}" "${LANE_EXCLUDES[$i]}")"
		[ -n "${files}" ] || continue
		if ! printf '%s' "${msg}" | has_trailer "${LANE_KEYS[$i]}"; then
			echo "✗ commit ${sha} touches frozen ${LANE_KEYS[$i]} paths without that trailer:" >&2
			printf '%s\n' "${files}" | sed 's/^/    /' >&2
			status=1
		fi
	done
}

if [ "$#" -ge 1 ] && [ -n "${1:-}" ]; then
	# ── commit-msg mode ────────────────────────────────────────────────
	msg_file="$1"
	staged="$(git diff --cached --no-renames --name-only --diff-filter=ACMRD)"
	[ -n "${staged}" ] || exit 0
	hit=0
	for i in "${!LANE_KEYS[@]}"; do
		touched="$(printf '%s\n' "${staged}" | lane_filter "${LANE_RES[$i]}" "${LANE_EXCLUDES[$i]}")"
		[ -n "${touched}" ] || continue
		hit=1
		if ! has_trailer "${LANE_KEYS[$i]}" <"${msg_file}"; then
			fail_frozen "${LANE_KEYS[$i]}" \
				"staged commit touches frozen paths without a non-empty ${LANE_KEYS[$i]}: trailer" \
				"${touched}"
		fi
		echo "✓ lens frozen-path guard: ${LANE_KEYS[$i]}: trailer present for frozen change" >&2
	done
	[ "${hit}" -eq 1 ] || exit 0
	exit 0
fi

# ── CI / range mode ────────────────────────────────────────────────────────
status=0
base="${LENS_FROZEN_BASE:-origin/main}"
if git rev-parse --verify --quiet "${base}" >/dev/null; then
	range="${base}..HEAD"
	# rev-list includes merge commits; check_commit diffs them against their
	# first parent (never skipped).
	for sha in $(git rev-list "${range}"); do
		check_commit "${sha}"
	done
else
	# No base resolved. In CI this is a fail-OPEN hole (adv-gate M1): a
	# multi-commit PR could hide a frozen change in an earlier commit behind a
	# clean tip, and a HEAD-only check would pass it. So in CI, demand the
	# pipeline fetch the base (fail CLOSED) rather than silently degrade.
	# Locally the commit-msg hook is the authoritative per-commit guard, so
	# degrade to a best-effort HEAD inspection with a loud warning.
	if [ -n "${CI:-}" ] || [ -n "${GITHUB_ACTIONS:-}" ]; then
		echo "✗ lens frozen-path guard: base ref '${base}' does not resolve in CI." >&2
		echo "  Range mode cannot inspect BASE..HEAD, which would fail OPEN. Fetch the" >&2
		echo "  base (actions/checkout with fetch-depth: 0) or set LENS_FROZEN_BASE." >&2
		exit 1
	fi
	echo "⚠ lens frozen-path guard: base ref '${base}' unresolved; degrading to a" >&2
	echo "  HEAD-only check (the commit-msg hook is the authoritative local guard)." >&2
	check_commit "$(git rev-parse HEAD)"
fi

if [ "${status}" -ne 0 ]; then
	echo "" >&2
	echo "  Frozen (PRD §16.2 / task 078); add the lane's trailer + council verdict." >&2
	exit 1
fi

echo "✓ lens frozen-path guard: all inspected commits satisfy the frozen-lane policy" >&2
