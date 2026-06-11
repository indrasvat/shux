#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
shux_bin="${SHUX_BIN:-${repo_root}/target/release/shux}"
out_dir="${SHUX_ATTACH_COLOR_OUT:-${repo_root}/.shux/out/issue-69}"

mkdir -p "${out_dir}"

uv run python - "${shux_bin}" "${out_dir}" <<'PY'
import os
import pty
import re
import select
import shutil
import struct
import subprocess
import sys
import termios
import time
from pathlib import Path

shux = Path(sys.argv[1])
out_dir = Path(sys.argv[2])
session = "issue-69-attach-color"
runtime = Path("/tmp/s69color")
fixture = (
    "printf '\\033[38;2;10;20;30;48;2;40;50;60mTRUECOLOR\\033[0m "
    "\\033[38;5;196mINDEXED\\033[0m "
    "\\033[31mBASIC\\033[0m\\n'; sleep 300"
)


def run(cmd: list[str], env: dict[str, str], timeout: int = 10) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
    )


def set_winsize(fd: int, rows: int = 12, cols: int = 80) -> None:
    import fcntl

    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))


def capture_attach(env: dict[str, str], seconds: float = 3.0) -> bytes:
    master, slave = pty.openpty()
    set_winsize(slave)
    proc = subprocess.Popen(
        [str(shux), "session", "attach", session],
        env={**env, "TERM": "xterm-256color", "COLORTERM": "truecolor"},
        stdin=slave,
        stdout=slave,
        stderr=slave,
        close_fds=True,
    )
    os.close(slave)
    chunks: list[bytes] = []
    deadline = time.time() + seconds
    try:
        while time.time() < deadline:
            readable, _, _ = select.select([master], [], [], 0.1)
            if master in readable:
                try:
                    data = os.read(master, 65536)
                except OSError:
                    break
                if not data:
                    break
                chunks.append(data)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=1)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=1)
        os.close(master)
    return b"".join(chunks)


def main() -> int:
    shutil.rmtree(runtime, ignore_errors=True)
    runtime.mkdir(parents=True)
    env = os.environ.copy()
    env["XDG_RUNTIME_DIR"] = str(runtime)
    env["TERM"] = "xterm-256color"
    env["COLORTERM"] = "truecolor"
    env["CLICOLOR"] = "1"
    env["NO_COLOR"] = "1"
    env.pop("SHUX_SOCKET", None)

    create = run(
        [
            str(shux),
            "--format",
            "json",
            "session",
            "create",
            session,
            "-d",
            "--title",
            "issue 69 attach color",
            "--",
            "sh",
            "-lc",
            fixture,
        ],
        env,
        timeout=15,
    )
    if create.returncode != 0:
        sys.stderr.write(create.stdout)
        sys.stderr.write(create.stderr)
        return create.returncode

    try:
        run([str(shux), "pane", "set-size", "-s", session, "--cols", "80", "--rows", "12"], env)
        time.sleep(1.0)
        png = out_dir / "attach-color-fixture.png"
        snap = run([str(shux), "pane", "snapshot", "-s", session, "-o", str(png)], env, timeout=20)
        if snap.returncode != 0:
            sys.stderr.write(snap.stdout)
            sys.stderr.write(snap.stderr)
            return snap.returncode

        attach = capture_attach(env)
        attach_log = out_dir / "attach-color-fixture.bytes"
        attach_log.write_bytes(attach)

        required = {
            b"\x1b[38;2;10;20;30m": "truecolor foreground",
            b"\x1b[48;2;40;50;60m": "truecolor background",
            b"\x1b[38;5;196m": "256-color foreground",
            b"\x1b[38;5;1m": "basic indexed foreground",
        }
        missing = [name for seq, name in required.items() if seq not in attach]
        empty_sgr = len(re.findall(rb"\x1b\[m", attach))

        print(f"snapshot={png}")
        print(f"attach_log={attach_log}")
        print(f"attach_bytes={len(attach)} empty_sgr={empty_sgr}")

        if missing:
            sys.stderr.write(f"missing pane color SGR: {', '.join(missing)}\n")
            return 1
        return 0
    finally:
        run([str(shux), "session", "kill", session], env)
        shutil.rmtree(runtime, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
PY
