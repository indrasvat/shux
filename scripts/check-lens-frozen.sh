#!/usr/bin/env bash
# check-lens-frozen.sh — enforce the lens test-integrity protocol (PRD §16.2 /
# DEC-9). The lens fixtures and red suite are FROZEN after Phase P0:
#
#   .shux/fixtures/lens/**   and   crates/shux/tests/lens_*
#
# Any diff touching those paths is rejected UNLESS the commit message carries a
#   LENS-TEST-CHANGE: <reason>
# trailer. This catches silent assertion-weakening and helper extraction (the
# helpers live under the frozen paths too).
#
# Modes:
#   check-lens-frozen.sh <commit-msg-file>
#       commit-msg hook: inspect the STAGED diff against that message.
#   check-lens-frozen.sh
#       CI / range mode: inspect every commit in BASE..HEAD
#       (BASE = $LENS_FROZEN_BASE, default origin/main) and require the trailer
#       on any commit that touches a frozen path.

set -euo pipefail

FROZEN_RE='^(\.shux/fixtures/lens/|crates/shux/tests/lens_)'
TRAILER='LENS-TEST-CHANGE:'

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "${REPO_ROOT}"

sgrep() { grep -E "$@" || true; }

fail_frozen() {
	# $1 = human context, $2 = newline-separated touched files
	echo "✗ lens frozen-path guard: $1" >&2
	echo "  touched frozen paths:" >&2
	printf '%s\n' "$2" | sed 's/^/    /' >&2
	echo "" >&2
	echo "  These paths are FROZEN after Phase P0 (PRD §16.2). To change them," >&2
	echo "  add a commit trailer explaining why, e.g.:" >&2
	echo "" >&2
	echo "      ${TRAILER} re-baseline g2 golden after raster font bump" >&2
	echo "" >&2
	echo "  and record the council verdict for the change in the PR description." >&2
	exit 1
}

if [ "$#" -ge 1 ] && [ -n "${1:-}" ]; then
	# ── commit-msg mode ────────────────────────────────────────────────
	msg_file="$1"
	touched="$(git diff --cached --name-only --diff-filter=ACMRD | sgrep "${FROZEN_RE}")"
	[ -n "${touched}" ] || exit 0
	if ! grep -qF "${TRAILER}" "${msg_file}"; then
		fail_frozen "staged commit touches frozen lens paths without a ${TRAILER} trailer" "${touched}"
	fi
	echo "✓ lens frozen-path guard: ${TRAILER} trailer present for frozen change" >&2
	exit 0
fi

# ── CI / range mode ────────────────────────────────────────────────────────
base="${LENS_FROZEN_BASE:-origin/main}"
if ! git rev-parse --verify --quiet "${base}" >/dev/null; then
	# No usable base (shallow clone / detached): compare against the parent.
	base="$(git rev-parse --verify --quiet HEAD~1 || true)"
fi
[ -n "${base}" ] || exit 0

range="${base}..HEAD"
any_touch="$(git diff --name-only "${range}" | sgrep "${FROZEN_RE}")"
[ -n "${any_touch}" ] || exit 0

status=0
for sha in $(git rev-list "${range}"); do
	files="$(git diff-tree --no-commit-id --name-only -r "${sha}" | sgrep "${FROZEN_RE}")"
	[ -n "${files}" ] || continue
	if ! git log -1 --format='%B' "${sha}" | grep -qF "${TRAILER}"; then
		echo "✗ commit ${sha} touches frozen lens paths without ${TRAILER}:" >&2
		printf '%s\n' "${files}" | sed 's/^/    /' >&2
		status=1
	fi
done

if [ "${status}" -ne 0 ]; then
	echo "" >&2
	echo "  Frozen after Phase P0 (PRD §16.2); add a ${TRAILER} trailer + council verdict." >&2
	exit 1
fi

echo "✓ lens frozen-path guard: all frozen-path commits carry ${TRAILER}" >&2
