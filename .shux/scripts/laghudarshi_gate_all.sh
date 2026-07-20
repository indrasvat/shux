#!/usr/bin/env bash
# Run the remaining task-084 gauntlet cells SERIALLY (seed -> agent -> verify).
#
# Serial is not a preference: CLAUDE.md forbids parallel daemon-backed shux work, and each
# cell spawns its own gate daemon. Every cell is independent — one failure does not stop
# the rest, because the point is the full picture across agents.
#
# Usage: .shux/scripts/laghudarshi_gate_all.sh <agent>:<cr> [<agent>:<cr> ...]

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RESULTS="${REPO_ROOT}/.local/084-gauntlet/results.tsv"
TIMEOUT_S="${GAUNTLET_TIMEOUT_S:-1800}"

[ "$#" -ge 1 ] || { echo "usage: $0 <agent>:<cr> [...]" >&2; exit 2; }

mkdir -p "$(dirname "${RESULTS}")"
[ -f "${RESULTS}" ] || printf 'agent\tcr\tseed\tagent_rc\tverdict\n' > "${RESULTS}"

for cell in "$@"; do
  agent="${cell%%:*}"
  cr="${cell##*:}"
  echo "═══ ${agent} / ${cr} ═══"

  if ! "${REPO_ROOT}/.shux/scripts/laghudarshi_gate_gauntlet.sh" seed "${agent}" "${cr}"; then
    printf '%s\t%s\tSEED-FAILED\t-\t-\n' "${agent}" "${cr}" >> "${RESULTS}"
    continue
  fi

  "${REPO_ROOT}/.shux/scripts/laghudarshi_gate_agent.sh" "${agent}" "${cr}" "${TIMEOUT_S}"
  arc="$(cat "${REPO_ROOT}/.local/084-gauntlet/${agent}/${cr}/evidence/agent.exit" 2>/dev/null || echo '?')"

  verdict=FAIL
  "${REPO_ROOT}/.shux/scripts/laghudarshi_gate_gauntlet.sh" verify "${agent}" "${cr}" && verdict=PASS

  printf '%s\t%s\tok\t%s\t%s\n' "${agent}" "${cr}" "${arc}" "${verdict}" >> "${RESULTS}"
  echo "═══ ${agent} / ${cr} -> ${verdict} ═══"
  pkill -f "shux __daemon" 2>/dev/null || true
done

echo
echo "── gauntlet results ──"
column -t -s "$(printf '\t')" < "${RESULTS}"
