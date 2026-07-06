#!/usr/bin/env bash
# check-lens-frozen.sh — enforce the lens test-integrity protocol (PRD §16.2 /
# DEC-9). The lens fixtures and red suite are FROZEN after Phase P0:
#
#   .shux/fixtures/lens/**   and   crates/shux/tests/lens_*
#
# Any diff touching those paths is rejected UNLESS the commit message carries a
#   LENS-TEST-CHANGE: <reason>
# trailer with a NON-EMPTY reason, verified via `git interpret-trailers --parse`
# (a body-grep would accept the string anywhere, including quoted text —
# p0-council-r1 major 10). This catches silent assertion-weakening and helper
# extraction (the helpers live under the frozen paths too).
#
# Modes:
#   check-lens-frozen.sh <commit-msg-file>
#       commit-msg hook: inspect the STAGED diff against that message.
#   check-lens-frozen.sh
#       CI / range mode: inspect every commit in BASE..HEAD
#       (BASE = $LENS_FROZEN_BASE, default origin/main) and require the trailer
#       on any commit that touches a frozen path. Merge commits are diffed
#       against their FIRST PARENT — never skipped (skipping merges would be a
#       bypass). Shallow/rootless fallback inspects HEAD itself.

set -euo pipefail

FROZEN_RE='^(\.shux/fixtures/lens/|crates/shux/tests/lens_)'
TRAILER_KEY='LENS-TEST-CHANGE'

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "${REPO_ROOT}"

sgrep() { grep -E "$@" || true; }

# True iff stdin (a full commit message) carries a LENS-TEST-CHANGE trailer
# with a non-empty value, per git's own trailer parser.
has_trailer() {
	git interpret-trailers --parse 2>/dev/null |
		grep -qE "^${TRAILER_KEY}: [^[:space:]].*"
}

fail_frozen() {
	# $1 = human context, $2 = newline-separated touched files
	echo "✗ lens frozen-path guard: $1" >&2
	echo "  touched frozen paths:" >&2
	printf '%s\n' "$2" | sed 's/^/    /' >&2
	echo "" >&2
	echo "  These paths are FROZEN after Phase P0 (PRD §16.2). To change them," >&2
	echo "  add a commit trailer (with a non-empty reason) explaining why, e.g.:" >&2
	echo "" >&2
	echo "      ${TRAILER_KEY}: re-baseline g2 golden after raster font bump" >&2
	echo "" >&2
	echo "  and record the council verdict for the change in the PR description." >&2
	exit 1
}

# First-parent file list for one commit (merge commits diff against parent 1;
# root commits diff against the empty tree).
commit_frozen_files() {
	local sha="$1"
	if git rev-parse --verify --quiet "${sha}^1" >/dev/null; then
		git diff --name-only "${sha}^1" "${sha}" | sgrep "${FROZEN_RE}"
	else
		git diff-tree --no-commit-id --name-only -r --root "${sha}" | sgrep "${FROZEN_RE}"
	fi
}

# Require the trailer on one commit; report + set status=1 otherwise.
check_commit() {
	local sha="$1"
	local files
	files="$(commit_frozen_files "${sha}")"
	[ -n "${files}" ] || return 0
	if ! git log -1 --format='%B' "${sha}" | has_trailer; then
		echo "✗ commit ${sha} touches frozen lens paths without a non-empty ${TRAILER_KEY}: trailer:" >&2
		printf '%s\n' "${files}" | sed 's/^/    /' >&2
		status=1
	fi
}

if [ "$#" -ge 1 ] && [ -n "${1:-}" ]; then
	# ── commit-msg mode ────────────────────────────────────────────────
	msg_file="$1"
	touched="$(git diff --cached --name-only --diff-filter=ACMRD | sgrep "${FROZEN_RE}")"
	[ -n "${touched}" ] || exit 0
	if ! has_trailer <"${msg_file}"; then
		fail_frozen "staged commit touches frozen lens paths without a non-empty ${TRAILER_KEY}: trailer" "${touched}"
	fi
	echo "✓ lens frozen-path guard: ${TRAILER_KEY}: trailer present for frozen change" >&2
	exit 0
fi

# ── CI / range mode ────────────────────────────────────────────────────────
status=0
base="${LENS_FROZEN_BASE:-origin/main}"
if git rev-parse --verify --quiet "${base}" >/dev/null; then
	range="${base}..HEAD"
	any_touch="$(git diff --name-only "${range}" -- 2>/dev/null | sgrep "${FROZEN_RE}")"
	if [ -n "${any_touch}" ]; then
		# rev-list includes merge commits; check_commit diffs them against
		# their first parent (never skipped).
		for sha in $(git rev-list "${range}"); do
			check_commit "${sha}"
		done
	fi
else
	# No usable base (shallow clone / detached / root): inspect HEAD itself.
	check_commit "$(git rev-parse HEAD)"
fi

if [ "${status}" -ne 0 ]; then
	echo "" >&2
	echo "  Frozen after Phase P0 (PRD §16.2); add a ${TRAILER_KEY}: trailer + council verdict." >&2
	exit 1
fi

echo "✓ lens frozen-path guard: all inspected commits satisfy the ${TRAILER_KEY}: policy" >&2
