#!/usr/bin/env bash
# Prove run-bounded.sh enforces its deadline, kills every descendant (one in
# its own process group, as nextest's are, and one that double-forked into a
# new session), never leaves the caller's pipe held, passes exit codes
# through, refuses a deadline that would disable it, and says so when it
# cannot enumerate processes.
set -euo pipefail

RUN="${RUN_BOUNDED:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/run-bounded.sh}"
work="$(mktemp -d)"
cleanup() {
  local f
  for f in "${work}"/*.pid; do
    if [[ -s "${f}" ]]; then
      kill -KILL "$(cat "${f}")" 2>/dev/null || true  # most are already gone
    fi
  done
  find "${work}" -delete
}
trap cleanup EXIT

die() { echo "✗ run-bounded self-test: $*" >&2; exit 1; }

gone() {
  local pid
  pid="$(cat "$1")"
  [[ -n "${pid}" ]] || die "$2: the fixture never recorded its pid"
  sleep 0.3
  ! kill -0 "${pid}" 2>/dev/null || die "$2: process ${pid} outlived the deadline"
}

# capture.py <seconds> <stdout-file> <stderr-file> <command...>
# Runs the command on real pipes and reads them under its own deadline, so a
# descendant that keeps a pipe open fails this test instead of hanging it.
# Exit: the command's status, or 99 if a pipe was still held at the deadline.
cat >"${work}/capture.py" <<'EOF'
import selectors, subprocess, sys, time
limit, out_path, err_path, cmd = float(sys.argv[1]), sys.argv[2], sys.argv[3], sys.argv[4:]
proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
sel = selectors.DefaultSelector()
bufs = {proc.stdout: bytearray(), proc.stderr: bytearray()}
for f in bufs:
    sel.register(f, selectors.EVENT_READ)
deadline = time.monotonic() + limit
while sel.get_map() and time.monotonic() < deadline:
    for key, _ in sel.select(timeout=max(0.0, deadline - time.monotonic())):
        chunk = key.fileobj.read1(65536)
        if chunk:
            bufs[key.fileobj] += chunk
        else:
            sel.unregister(key.fileobj)
open(out_path, "wb").write(bufs[proc.stdout])
open(err_path, "wb").write(bufs[proc.stderr])
if sel.get_map():
    sys.exit(99)
try:
    sys.exit(proc.wait(timeout=max(0.1, deadline - time.monotonic())))
except subprocess.TimeoutExpired:
    sys.exit(99)
EOF
bounded_capture() { python3 "${work}/capture.py" "$@"; }

# A grandchild that double-forks into its own session and is re-parented to
# init before the deadline: no pgid or ppid link leads back to the command.
cat >"${work}/detach.py" <<'EOF'
import os, sys, time
if os.fork():
    if len(sys.argv) > 2:  # "exit": leave once the grandchild is detached
        while not os.path.exists(sys.argv[1]):
            time.sleep(0.05)
        sys.exit(0)
    time.sleep(300)
    sys.exit(0)
os.setsid()
if os.fork():
    sys.exit(0)
with open(sys.argv[1], "w") as fh:
    fh.write(str(os.getpid()))
print("detached-alive", flush=True)
time.sleep(300)
EOF

start=$(date +%s)
rc=0
bounded_capture 15 "${work}/o1" "${work}/e1" bash "${RUN}" 2 bash -c \
  "set -m; sleep 300 & echo \$! > '${work}/pgroup.pid'; python3 '${work}/detach.py' '${work}/detached.pid' & echo early; sleep 300" \
  || rc=$?
took=$(( $(date +%s) - start ))
[[ "${rc}" -ne 99 ]] || die "a descendant still held the caller's pipe ${took}s after a 2s deadline"
[[ "${rc}" -eq 124 ]] || die "timeout exited ${rc}, want 124"
[[ "${took}" -le 12 ]] || die "a 2s deadline took ${took}s to release the caller"
[[ "$(cat "${work}/o1")" == *early* ]] || die "output from before the deadline was lost"
gone "${work}/pgroup.pid" "a grandchild in its own process group"
gone "${work}/detached.pid" "a double-forked grandchild in a new session"

rc=0; bash "${RUN}" 5 bash -c 'exit 7' || rc=$?
[[ "${rc}" -eq 7 ]] || die "exit 7 came back as ${rc}"

rc=0; out="$(bash "${RUN}" 5 printf 'a\nb\n')" || rc=$?
[[ "${rc}" -eq 0 && "${out}" == $'a\nb' ]] || die "output or status changed (${rc}: '${out}')"

rc=0; err="$(bash "${RUN}" 5 bash -c 'echo oops >&2; exit 3' 2>&1 >/dev/null)" || rc=$?
[[ "${rc}" -eq 3 && "${err}" == oops ]] || die "stderr or status changed (${rc}: '${err}')"

# A command that exits normally but leaves a background process behind.
rc=0; bash "${RUN}" 5 bash -c "sleep 300 & echo \$! > '${work}/leftover.pid'" >/dev/null || rc=$?
[[ "${rc}" -eq 0 ]] || die "a clean exit with a background child came back as ${rc}"
gone "${work}/leftover.pid" "a background child left by a command that exited"

rc=0; out="$(printf 'through-the-runner\n' | bash "${RUN}" 5 cat)" || rc=$?
[[ "${rc}" -eq 0 && "${out}" == through-the-runner ]] || die "stdin did not reach the command (${rc}: '${out}')"

for bad in 0 -3 abc ""; do
  rc=0; bash "${RUN}" "${bad}" true 2>/dev/null || rc=$?
  [[ "${rc}" -eq 2 ]] || die "deadline '${bad}' was accepted (exit ${rc})"
done
rc=0; bash "${RUN}" 5 2>/dev/null || rc=$?
[[ "${rc}" -eq 2 ]] || die "a missing command was accepted (exit ${rc})"

# Without /proc, discovery goes through ps: a failing ps must be reported, not
# read as "nothing to kill".
if [[ ! -d /proc/self ]]; then
  mkdir -p "${work}/shim"
  printf '#!/bin/sh\nexit 1\n' >"${work}/shim/ps"
  chmod +x "${work}/shim/ps"
  rc=0
  PATH="${work}/shim:${PATH}" bounded_capture 15 "${work}/o2" "${work}/e2" bash "${RUN}" 1 bash -c \
    "set -m; sleep 300 & echo \$! > '${work}/blind.pid'; sleep 300" || rc=$?
  [[ "${rc}" -ne 99 ]] || die "with ps failing, a survivor held the caller's pipe"
  [[ "${rc}" -eq 124 ]] || die "with ps failing, timeout exited ${rc}, want 124"
  grep -q "could not confirm every descendant is gone" "${work}/e2" \
    || die "a failing ps was not reported: '$(cat "${work}/e2")'"

  # Same, when the command exits normally and leaves a detached process.
  rc=0
  PATH="${work}/shim:${PATH}" bounded_capture 15 "${work}/o3" "${work}/e3" bash "${RUN}" 10 \
    python3 "${work}/detach.py" "${work}/blind2.pid" exit || rc=$?
  [[ "${rc}" -eq 0 ]] || die "a command that exited 0 with ps failing came back as ${rc}"
  [[ "${rc}" -ne 99 ]] || die "after a normal exit with ps failing, a survivor held the caller's pipe"
  grep -q "could not enumerate processes after the command exited" "${work}/e3" \
    || die "a failing ps after a normal exit was not reported: '$(cat "${work}/e3")'"
fi

echo "✓ run-bounded self-test: deadline enforced, every descendant killed, caller never blocked"
