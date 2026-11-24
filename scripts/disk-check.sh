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
case "$*" in
  "") ;;
  "--hook session" | "--hook stop") hook="$2" ;;
  *) echo "usage: scripts/disk-check.sh [--hook session|stop]" >&2; exit 2 ;;
esac

# One line of text; for Stop, a JSON string with control characters replaced.
say() {
  local m="${1//[[:cntrl:]]/?}"
  if [[ "${hook}" == stop ]]; then
    m="${m//\\/\\\\}"
    printf '{"systemMessage": "%s"}\n' "${m//\"/\\\"}"
  else
    printf '%s\n' "${m}"
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
# No leading zeros (bash reads them as octal), and small enough not to overflow.
[[ "${min}" =~ ^(0|[1-9][0-9]{0,5})$ ]] ||
  broken "SHUX_DISK_MIN_GIB='${min}' is not a whole number of GiB from 0 to 999999"

# -P: one line per filesystem, 1024-byte blocks, same columns on macOS and Linux.
# stdout only: a warning on stderr must not shift the lines read below.
if ! df_out="$(df -Pk -- "${ROOT}" 2>/dev/null)"; then
  broken "df -Pk failed: $(df -Pk -- "${ROOT}" 2>&1 >/dev/null | tr '\n' ' ')"
fi
# The data line's Capacity is the first N% field after three numbers; Available is
# just before it, the mount point after it. Filesystem names may hold spaces.
read -r avail mount < <(printf '%s\n' "${df_out}" | awk 'NR > 1 {
  for (i = 5; i <= NF; i++)
    if ($i ~ /^[0-9]+%$/ && $(i-1) ~ /^[0-9]+$/ && $(i-2) ~ /^[0-9]+$/ && $(i-3) ~ /^[0-9]+$/) {
      m = $(i+1); for (j = i + 2; j <= NF; j++) m = m " " $j
      line = $(i-1) " " m
      break
    }
} END { print line }') || :
[[ "${avail:-}" =~ ^[0-9]+$ ]] || broken "could not read free space from: ${df_out//$'\n'/ | }"

if [[ "${avail}" -lt $((min * 1048576)) ]]; then
  free="$(awk -v k="${avail}" 'BEGIN { printf "%.1f", k / 1048576 }')"
  say "⚠ disk: ${free} GiB free on ${mount} (below ${min} GiB). See what is regenerable: make prune (APPLY=1 removes it)"
fi
