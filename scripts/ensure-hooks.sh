#!/usr/bin/env bash
# scripts/ensure-hooks.sh — the single source of truth for "does this checkout
# have its git hooks installed".
#
# Usage:
#   ./scripts/ensure-hooks.sh            # install them if missing (best effort)
#   ./scripts/ensure-hooks.sh --check    # verify only; fail if missing
#
# Exit codes:
#   0 = installed, or deliberately skipped
#   1 = --check and they are not installed
#   2 = lefthook itself is missing, so they cannot be installed
#
# Every entry point routes through here — the Makefile, the Claude SessionStart
# hook, and `scripts/setup-dev.sh` — so the CI guard below exists exactly once
# rather than being restated at each call site.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

MODE="${1:---install}"

# ── The CI guard. This is the only one; do not add another. ─────────────────
#
# Git hooks are a LOCAL aid: they run the same targets CI runs, so in CI they
# are pure duplication — and worse than duplication in one specific place.
# `release.yml` runs semantic-release, which COMMITS and PUSHES the version bump
# and changelog. With hooks installed that push would drag the whole test suite
# and cargo-deny into the release job.
#
# `CI` is the de-facto standard and GitHub Actions sets it to `true`; every
# other major provider sets it too. One variable, checked once.
if [[ -n "${CI:-}" ]]; then
    echo "✓ git hooks: skipped (CI=${CI}); CI runs the same targets directly"
    exit 0
fi

if ! command -v lefthook >/dev/null 2>&1; then
    if [[ "${MODE}" == "--check" ]]; then
        echo "✗ lefthook is not installed, so this checkout has no git hooks." >&2
        echo "  Run: make install-tools && make hooks" >&2
        exit 2
    fi
    # Best-effort install path: say so and carry on. `--check` is the gate, and
    # it will fail loudly, so nothing is silently masked by this.
    echo "! lefthook not found — git hooks NOT installed. Run: make install-tools" >&2
    exit 0
fi

if [[ "${MODE}" == "--check" ]]; then
    if lefthook check-install >/dev/null 2>&1; then
        echo "✓ git hooks installed"
        exit 0
    fi
    echo "✗ git hooks are not installed in this checkout, so pre-commit and" >&2
    echo "  pre-push run nothing. A fresh clone starts this way." >&2
    echo "  Run: make hooks" >&2
    exit 1
fi

lefthook install >/dev/null
echo "✓ git hooks installed"
