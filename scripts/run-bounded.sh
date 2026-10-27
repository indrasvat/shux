#!/usr/bin/env bash
# Run a command with a hard deadline, and leave nothing of it behind.
#
#   run-bounded.sh <seconds> <command> [args...]
#
# Exit status is the command's, or 124 on timeout (2 on a bad <seconds>).
# - Every process the command starts carries a unique RUN_BOUNDED_TOKEN in its
#   environment. On timeout or SIGINT/SIGTERM each one is found by that token
#   (plus the process group and the ppid tree), so a descendant that changed
#   group, called setsid or was re-parented to init is killed too. Escapes
#   left: one that scrubs its environment, or (macOS only, where `ps -E` cannot
#   read Apple platform binaries) a platform binary that also left the group
#   and the ppid tree. The runner says when discovery itself fails.
# - The command's stdout/stderr go to temp files that are copied out once it
#   is gone: a survivor can never hold the caller's pipe open past the deadline.
set -euo pipefail

# -c, not a heredoc on stdin: the command must inherit the caller's stdin.
PROGRAM="$(cat <<'PY'
import os
import secrets
import shutil
import signal
import subprocess
import sys
import tempfile
import time

if len(sys.argv) < 3:
    print("usage: run-bounded.sh <seconds> <command> [args...]", file=sys.stderr)
    sys.exit(2)
try:
    limit = int(sys.argv[1])
except ValueError:
    limit = 0
if limit <= 0:
    print(f"run-bounded: timeout {sys.argv[1]!r} must be a positive integer", file=sys.stderr)
    sys.exit(2)

token = f"rb-{os.getpid()}-{secrets.token_hex(8)}"
needle = f"RUN_BOUNDED_TOKEN={token}"
out = tempfile.TemporaryFile()
err = tempfile.TemporaryFile()
proc = subprocess.Popen(
    sys.argv[2:],
    stdout=out,
    stderr=err,
    start_new_session=True,
    env={**os.environ, "RUN_BOUNDED_TOKEN": token},
)


def tagged_pids():
    """Every live process carrying our token, or None if discovery failed."""
    found = set()
    if os.path.isdir("/proc/self"):
        for entry in os.listdir("/proc"):
            if not entry.isdigit():
                continue
            try:
                with open(f"/proc/{entry}/environ", "rb") as fh:
                    if needle.encode() in fh.read().split(b"\0"):
                        found.add(int(entry))
            except OSError:
                pass
        return found
    try:
        ps = subprocess.run(["ps", "-A", "-E", "-ww", "-o", "pid=,command="],
                            capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if ps.returncode != 0:
        return None
    for line in ps.stdout.splitlines():
        pid, _, rest = line.strip().partition(" ")
        if pid.isdigit() and needle in rest.split():
            found.add(int(pid))
    return found


def ppid_tree(root):
    try:
        ps = subprocess.run(["ps", "-Ao", "pid=,ppid="], capture_output=True, text=True, timeout=10)
        children = {}
        for line in ps.stdout.splitlines():
            pid, ppid = (int(f) for f in line.split())
            children.setdefault(ppid, []).append(pid)
    except (OSError, ValueError, subprocess.TimeoutExpired):
        return set()
    found, stack = set(), [root]
    while stack:
        for child in children.get(stack.pop(), []):
            found.add(child)
            stack.append(child)
    return found


def signal_all(pids, sig):
    try:
        os.killpg(proc.pid, sig)
    except (ProcessLookupError, PermissionError):
        pass
    for pid in pids - {os.getpid()}:
        try:
            os.kill(pid, sig)
        except (ProcessLookupError, PermissionError):
            pass


def kill_everything():
    """Kill the command and every process it started; return False if some
    could not be enumerated or survived."""
    complete = True
    for sig in (signal.SIGTERM, signal.SIGKILL):
        tagged = tagged_pids()
        if tagged is None:
            complete = False
            tagged = set()
        signal_all(tagged | ppid_tree(proc.pid) | {proc.pid}, sig)
        if sig == signal.SIGTERM:
            deadline = time.monotonic() + 3
            while time.monotonic() < deadline and proc.poll() is None:
                time.sleep(0.05)
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        complete = False
    time.sleep(0.1)
    survivors = tagged_pids()
    if survivors is None or survivors - {os.getpid()}:
        complete = False
    return complete


def flush_output():
    for src, dst in ((out, sys.stdout.buffer), (err, sys.stderr.buffer)):
        src.seek(0)
        shutil.copyfileobj(src, dst)
        dst.flush()


def stop(reason, code):
    complete = kill_everything()
    flush_output()
    print(f"run-bounded: `{' '.join(sys.argv[2:])}` {reason}; killed it and every descendant",
          file=sys.stderr)
    if not complete:
        print("run-bounded: WARNING: could not confirm every descendant is gone "
              f"(look for RUN_BOUNDED_TOKEN={token})", file=sys.stderr)
    sys.exit(code)


signal.signal(signal.SIGINT, lambda signum, _f: stop("was interrupted", 128 + signum))
signal.signal(signal.SIGTERM, lambda signum, _f: stop("was terminated", 128 + signum))

try:
    rc = proc.wait(timeout=limit)
except subprocess.TimeoutExpired:
    stop(f"did not finish in {limit}s", 124)
# The command finished; anything it left running in the background goes too.
leftovers = tagged_pids()
signal_all((leftovers or set()) - {proc.pid}, signal.SIGKILL)
flush_output()
if leftovers is None:
    print("run-bounded: WARNING: could not enumerate processes after the command exited; "
          f"background descendants may survive (look for RUN_BOUNDED_TOKEN={token})",
          file=sys.stderr)
sys.exit(rc)
PY
)"
exec python3 -c "${PROGRAM}" "$@"
