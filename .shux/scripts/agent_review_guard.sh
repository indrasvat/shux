#!/usr/bin/env bash
# Run an external reviewer command with process-tree cleanup.
#
# This is for AI reviewer CLIs (dootsabha, claude, codex, agy, gemini) that may
# spawn MCP servers or nested tools. It starts the command in its own process
# group, applies a timeout without relying on GNU timeout, and kills any new
# matching reviewer/MCP processes that survive.

set -euo pipefail

if [ "$#" -lt 3 ]; then
  echo "usage: .shux/scripts/agent_review_guard.sh <label> <timeout-seconds> <command> [args...]" >&2
  exit 2
fi

label="$1"
timeout_seconds="$2"
shift 2

case "${timeout_seconds}" in
  ''|*[!0-9]*)
    echo "agent review guard: timeout must be whole seconds, got: ${timeout_seconds}" >&2
    exit 2
    ;;
esac

python3 - "$label" "$timeout_seconds" "$@" <<'PY'
import os
import signal
import subprocess
import sys
import time


LEAK_PATTERNS = (
    "agy",
    "claude -p",
    "codex exec",
    "dootsabha",
    "gemini",
    "firebase-tools@latest mcp",
    "/firebase mcp",
    "xcodebuildmcp",
    "chrome-devtools-mcp",
    "node ./mcp/server.cjs",
    "antigravity_ide/out/mcp-server",
    ".gemini/extensions",
)


def matching_pids() -> dict[int, str]:
    result = subprocess.run(
        ["ps", "-axo", "pid=,args="],
        check=True,
        text=True,
        stdout=subprocess.PIPE,
    )
    matches: dict[int, str] = {}
    own_pid = os.getpid()
    for line in result.stdout.splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        pid_text, _, args = stripped.partition(" ")
        if not pid_text.isdigit():
            continue
        pid = int(pid_text)
        if pid == own_pid:
            continue
        lowered = args.lower()
        if "agent_review_guard.sh" in lowered:
            continue
        if any(pattern in lowered for pattern in LEAK_PATTERNS):
            matches[pid] = args
    return matches


def terminate_group(pid: int) -> None:
    for sig, delay in ((signal.SIGTERM, 1.5), (signal.SIGKILL, 0.0)):
        try:
            os.killpg(pid, sig)
        except ProcessLookupError:
            return
        except PermissionError:
            return
        if delay:
            deadline = time.monotonic() + delay
            while time.monotonic() < deadline:
                try:
                    os.killpg(pid, 0)
                except ProcessLookupError:
                    return
                except PermissionError:
                    return
                time.sleep(0.05)


def terminate_pids(pids: list[int]) -> None:
    for sig, delay in ((signal.SIGTERM, 1.0), (signal.SIGKILL, 0.0)):
        for pid in pids:
            try:
                os.kill(pid, sig)
            except ProcessLookupError:
                pass
            except PermissionError:
                pass
        if delay:
            time.sleep(delay)


def main() -> int:
    label = sys.argv[1]
    timeout_seconds = int(sys.argv[2])
    command = sys.argv[3:]

    baseline = matching_pids()
    proc = subprocess.Popen(command, preexec_fn=os.setsid)
    timed_out = False
    try:
        proc.wait(timeout=timeout_seconds)
    except subprocess.TimeoutExpired:
        timed_out = True
        terminate_group(proc.pid)
        try:
            proc.wait(timeout=2)
        except subprocess.TimeoutExpired:
            terminate_group(proc.pid)

    after = matching_pids()
    leaked = {
        pid: args
        for pid, args in after.items()
        if pid not in baseline and pid != proc.pid
    }
    if leaked:
        print(
            f"agent review guard: {label} leaked reviewer/MCP process(es):",
            file=sys.stderr,
        )
        for pid, args in sorted(leaked.items()):
            print(f"  pid={pid} {args}", file=sys.stderr)
        terminate_pids(sorted(leaked))
        return 1

    if timed_out:
        print(f"agent review guard: {label} timed out after {timeout_seconds}s", file=sys.stderr)
        return 124
    return proc.returncode or 0


if __name__ == "__main__":
    raise SystemExit(main())
PY
