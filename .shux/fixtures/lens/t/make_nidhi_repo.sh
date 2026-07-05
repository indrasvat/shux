#!/bin/sh
# make_nidhi_repo.sh — T-tier fixture builder (§13 TEST-3, T1/T2).
#
# Builds a deterministic git repo for `nidhi` to render: pinned author/committer
# dates (relative "N years ago" strings stay stable) and EXACTLY 3 stashes whose
# messages exercise Devanagari + CJK + emoji. No sleeps; fully reproducible.
#
# Usage: make_nidhi_repo.sh <target-dir>

set -eu

target="${1:?usage: make_nidhi_repo.sh <target-dir>}"

# Pin every timestamp so relative-time rendering is stable for years.
GIT_AUTHOR_DATE='2020-01-01T00:00:00Z'
GIT_COMMITTER_DATE='2020-01-01T00:00:00Z'
GIT_AUTHOR_NAME='lens-fixture'
GIT_AUTHOR_EMAIL='lens@example.invalid'
GIT_COMMITTER_NAME='lens-fixture'
GIT_COMMITTER_EMAIL='lens@example.invalid'
export GIT_AUTHOR_DATE GIT_COMMITTER_DATE
export GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL GIT_COMMITTER_NAME GIT_COMMITTER_EMAIL

mkdir -p "$target"
cd "$target"

git init -q -b main
printf 'base\n' >file.txt
git add file.txt
git commit -q -m 'base commit'

# Exactly 3 stashes (stash@{0} is the newest). Each needs a tracked-file change.
printf 'wip\n' >>file.txt
git stash push -q -m 'WIP: विवेचक समीक्षा ✓'

printf 'fix\n' >>file.txt
git stash push -q -m 'fix: 終端テスト 🎯'

printf 'chore\n' >>file.txt
git stash push -q -m 'chore: निधि संग्रह'

# Report the stash count so callers can assert the fixture built correctly.
git stash list | wc -l | tr -d ' '
