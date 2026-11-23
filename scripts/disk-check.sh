#!/usr/bin/env bash
# Warn when the filesystem holding the repo has less than SHUX_DISK_MIN_GIB
# (default 40) free. Silent above it. docs/agents/disk.md.
#
#   disk-check.sh                  one line on stdout when low; exits 1 if it cannot measure
#   disk-check.sh --hook session   the same line, for a SessionStart hook (stdout reaches the agent)
#   disk-check.sh --hook stop      the line as Stop-hook JSON {"systemMessage": ...} (shown to the user)
#
# Under --hook it always exits 0, and a failure to measure is the message: a
# hook must not fail a session, and exit 2 from a Stop hook would block it.
set -euo pipefail

hook=""
case "$#" in
  0) ;;
  2) [[ "$1" == --hook && ( "$2" == session || "$2" == stop ) ]] || set -- usage ;;
  *) set -- usage ;;
esac
if [[ "${1:-}" == usage ]]; then
  echo "usage: scripts/disk-check.sh [--hook session|stop]" >&2
  exit 2
fi
[[ "$#" -eq 0 ]] || hook="$2"

say() {
  if [[ "${hook}" == stop ]]; then
    local m="${1//\\/\\\\}"
    printf '{"systemMessage": "%s"}\n' "${m//\"/\\\"}"
  else
    printf '%s\n' "$1"
  fi
}

# A measurement that failed: loud, and fatal unless running as a hook.
broken() {
  say "⚠ disk-check: $1"
  [[ -n "${hook}" ]] || exit 1
  exit 0
}

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
min="${SHUX_DISK_MIN_GIB:-40}"
[[ "${min}" =~ ^[0-9]+$ ]] || broken "SHUX_DISK_MIN_GIB='${min}' is not a whole number of GiB"

# -P: one line per filesystem, 1024-byte blocks, same columns on macOS and Linux.
if ! df_out="$(df -Pk -- "${ROOT}" 2>&1)"; then
  broken "df -Pk failed: ${df_out//$'\n'/ }"
fi
avail="$(printf '%s\n' "${df_out}" | awk 'NR == 2 { print $4 }')"
mount="$(printf '%s\n' "${df_out}" | awk 'NR == 2 { for (i = 6; i <= NF; i++) printf "%s%s", (i > 6 ? " " : ""), $i }')"
[[ "${avail}" =~ ^[0-9]+$ ]] || broken "could not read free space from: ${df_out//$'\n'/ | }"

if [[ "${avail}" -lt $((min * 1048576)) ]]; then
  free="$(awk -v k="${avail}" 'BEGIN { printf "%.1f", k / 1048576 }')"
  say "⚠ disk: ${free} GiB free on ${mount} (below ${min} GiB). See what is regenerable: make prune (APPLY=1 removes it)"
fi
