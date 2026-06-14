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
if ps -axo pid=,args= | grep "${marker}" | grep -v grep >/dev/null 2>&1; then
  echo "agent review guard self-test: marked child survived cleanup" >&2
  ps -axo pid=,ppid=,pgid=,stat=,args= | grep "${marker}" | grep -v grep >&2 || true
  exit 1
fi

rm -f /tmp/shux-agent-review-guard-selftest.out /tmp/shux-agent-review-guard-selftest.err
