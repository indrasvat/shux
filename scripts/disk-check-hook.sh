#!/usr/bin/env bash
# The Claude Code hook entry for `make disk-check` (docs/agents/disk.md).
#
#   disk-check-hook.sh session|stop
#
# Runs make, keeps only the line the hook may print (stop: exactly one JSON
# document, or nothing), prints a fallback when make itself fails, and always
# exits 0: exit 2 from a Stop hook would block the stop.
set -uo pipefail

mode="${1:-}"
case "${mode}" in session | stop) ;; *) echo "usage: scripts/disk-check-hook.sh session|stop" >&2; exit 0 ;; esac
fallback="⚠ make disk-check failed; run it in the repo to see why"
dir="${CLAUDE_PROJECT_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"

if out="$(make -s --no-print-directory -C "${dir}" disk-check HOOK="${mode}" 2>/dev/null)"; then
  line="$(printf '%s\n' "${out}" | awk -v mode="${mode}" '
    mode == "stop" && /^\{"systemMessage": ".*"\}$/ { keep = $0 }
    mode == "session" && /^⚠ / { keep = $0 }
    END { if (keep != "") print keep }')"
else
  line="${fallback}"
  [[ "${mode}" != stop ]] || line="{\"systemMessage\": \"${fallback}\"}"
fi
[[ -z "${line}" ]] || printf '%s\n' "${line}"
exit 0
