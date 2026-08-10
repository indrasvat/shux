#!/usr/bin/env bash
# Verify agent_review_guard kills a timed-out process tree.

set -euo pipefail

marker="shux-agent-review-guard-selftest-$$"

set +e
.shux/scripts/agent_review_guard.sh selftest 1 python3 -c '
import subprocess
import sys
import time

marker = sys.argv[1]
subprocess.Popen([sys.executable, "-c", "import sys,time; assert sys.argv[1]; time.sleep(300)", marker])
time.sleep(300)
' "${marker}" >/tmp/shux-agent-review-guard-selftest.out 2>/tmp/shux-agent-review-guard-selftest.err
status=$?
set -e

if [ "${status}" -ne 124 ] && [ "${status}" -ne 1 ]; then
  echo "agent review guard self-test: expected timeout/leak status, got ${status}" >&2
  cat /tmp/shux-agent-review-guard-selftest.err >&2 || true
  exit 1
fi

sleep 1
# shellcheck disable=SC2009  # pgrep is FORBIDDEN here — see CLAUDE.md "Process
# hygiene": `pgrep -f`/`pkill -f` match on a substring of the whole argv, so the
# checking process matches its own command line and reports a phantom leak. This
# grep is scoped to a per-run unique `${marker}`, which is the house pattern.
if ps -axo pid=,args= | grep "${marker}" | grep -v grep >/dev/null 2>&1; then
  echo "agent review guard self-test: marked child survived cleanup" >&2
  # shellcheck disable=SC2009  # same reason as above
  ps -axo pid=,ppid=,pgid=,stat=,args= | grep "${marker}" | grep -v grep >&2 || true
  exit 1
fi

rm -f /tmp/shux-agent-review-guard-selftest.out /tmp/shux-agent-review-guard-selftest.err
