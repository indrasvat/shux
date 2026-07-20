#!/usr/bin/env bash
# Drive ONE cold-context agent through ONE change request of the task-084 gate gauntlet.
#
# The agent gets a fresh context, the seeded working copy, the change-request brief, and
# the shux skill — no hints about which commands to run. Its whole process tree is cleaned
# up by `agent_review_guard.sh`, and its transcript is captured for the friction log.
#
# SAFETY: the agents run with approvals bypassed, because a cold agent that stops to ask
# for permission cannot be measured unattended. That is only acceptable because the target
# is a disposable copy this harness generated under `.local/084-gauntlet/` (gitignored) —
# never a real checkout. The guard below enforces that, and this script is a local
# developer tool: nothing in CI or the shipped product invokes it.
#
# The transcript is read for FRICTION only. Pass/fail comes from
# `laghudarshi_gate_gauntlet.sh verify`, which re-runs the gate itself and inspects the
# golden tree — never from anything the agent claims.
#
# Usage: .shux/scripts/laghudarshi_gate_agent.sh <claude|codex|agy> <cr-a|cr-b> [timeout-s]

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
FIXTURE="${REPO_ROOT}/.shux/fixtures/lens-gate/mock-rich-tui"
GAUNTLET_ROOT="${REPO_ROOT}/.local/084-gauntlet"
GUARD="${REPO_ROOT}/.shux/scripts/agent_review_guard.sh"

die() { echo "gauntlet-agent: $*" >&2; exit 2; }

[ "$#" -ge 2 ] || die "usage: $0 <claude|codex|agy> <cr-a|cr-b> [timeout-s]"
agent="$1"; cr="$2"; timeout_s="${3:-1800}"

case "${cr}" in cr-a|cr-b) ;; *) die "unknown change request: ${cr}" ;; esac

WORK="${GAUNTLET_ROOT}/${agent}/${cr}/work"
EVID="${GAUNTLET_ROOT}/${agent}/${cr}/evidence"
[ -d "${WORK}" ] || die "no seeded copy at ${WORK} -- run the gauntlet seed phase first"

# The agents below run with approvals bypassed, which is only defensible because the
# target is a THROWAWAY scratch copy this harness generated. Refuse to run if the working
# copy is not inside the gauntlet root, so this can never be pointed at a real checkout.
work_real="$(cd "${WORK}" && pwd -P)"
root_real="$(cd "${GAUNTLET_ROOT}" && pwd -P)"
case "${work_real}/" in
  "${root_real}"/*) ;;
  *) die "refusing to run: ${work_real} is outside the gauntlet scratch root ${root_real}" ;;
esac
[ -f "${WORK}/board.py" ] && [ -d "${WORK}/goldens" ] \
  || die "refusing to run: ${work_real} does not look like a seeded gauntlet copy"

SKILL_DIR="${GAUNTLET_ROOT}/${agent}/${cr}/shux-skill"
SHUX_BIN="${REPO_ROOT}/target/release/shux"
[ -x "${SHUX_BIN}" ] || die "no branch build at ${SHUX_BIN} (run: make release)"

BRIEF="$(cat "${FIXTURE}/gauntlet/${cr}.md")"
PROMPT="${BRIEF}

Your shell is already in the project directory; work only inside it.

The project's visual gate is driven by the \`shux\` tool. Use this build of it:

  ${SHUX_BIN}

(An older \`shux\` is also installed on PATH; it predates this project's gate, so use the
absolute path above.) Its documentation is at ${SKILL_DIR} — start with SKILL.md;
references/gate.md covers the gate specifically. Read what you need from there.

When you believe you are done, say so and stop."

# The agents MUST use this branch's build, not the released `shux` on PATH: the released
# binary predates BOTH `lens gate` as documented and the scenario `cwd` key the mock needs,
# and BOTH report version 0.44.0, so they are indistinguishable by `--version`. Exporting
# PATH is NOT sufficient — codex (and any agent that shells out via `bash -lc`) runs a
# LOGIN shell, which rebuilds PATH from the user's profile and discards the prepend. So the
# prompt names the absolute path; the export stays as a fallback for non-login shells.
export PATH="${REPO_ROOT}/target/release:${PATH}"
export XDG_RUNTIME_DIR="/tmp/sx084-gauntlet"
mkdir -p "${XDG_RUNTIME_DIR}"

transcript="${EVID}/transcript.txt"

echo "gauntlet-agent: running ${agent} on ${cr} (timeout ${timeout_s}s)"
set +e
case "${agent}" in
  claude)
    ( cd "${WORK}" && "${GUARD}" "gauntlet-${agent}-${cr}" "${timeout_s}" \
        claude -p "${PROMPT}" --dangerously-skip-permissions ) > "${transcript}" 2>&1
    ;;
  codex)
    "${GUARD}" "gauntlet-${agent}-${cr}" "${timeout_s}" \
      codex exec --dangerously-bypass-approvals-and-sandbox -C "${WORK}" "${PROMPT}" \
      > "${transcript}" 2>&1
    ;;
  agy)
    ( cd "${WORK}" && "${GUARD}" "gauntlet-${agent}-${cr}" "${timeout_s}" \
        agy -p "${PROMPT}" --dangerously-skip-permissions ) > "${transcript}" 2>&1
    ;;
  *) die "unknown agent: ${agent}" ;;
esac
rc=$?
set -e

echo "${rc}" > "${EVID}/agent.exit"
# The gate spawns a per-XDG daemon and there is no `daemon stop` verb yet (084 F5).
pkill -f "shux __daemon" 2>/dev/null || true

echo "gauntlet-agent: ${agent}/${cr} finished rc=${rc}; transcript at ${transcript}"
exit 0
