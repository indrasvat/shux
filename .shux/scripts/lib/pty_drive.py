#!/usr/bin/env python3
"""Drive a command on a real PTY: set a winsize, feed it raw bytes, capture output.

Used by the issue #174 harnesses to drive `shux attach` the way a terminal does
-- crossterm parses the mouse escape sequences we write here exactly as it would
parse a real click, so the client-side translation is exercised for real rather
than stubbed.

    pty_drive.py --cols 100 --rows 30 --log <path> \
                 --step 'sleep:1.5' --step 'send:\\x1b[<0;10;5M' ... -- cmd args

Steps run in order:
  sleep:<seconds>     wait
  send:<escaped>      write bytes to the PTY master (Python string escapes)

The child's output is streamed to --log as raw bytes. Exit status is the
child's, or 124 on --timeout.
"""

import argparse
import errno
import fcntl
import os
import pty
import select
import signal
import struct
import sys
import termios
import time


def set_winsize(fd, cols, rows, xpixel, ypixel):
    fcntl.ioctl(fd, termios.TIOCSWINSZ,
                struct.pack("HHHH", rows, cols, xpixel, ypixel))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cols", type=int, default=100)
    ap.add_argument("--rows", type=int, default=30)
    ap.add_argument("--xpixel", type=int, default=0)
    ap.add_argument("--ypixel", type=int, default=0)
    ap.add_argument("--log", required=True)
    ap.add_argument("--step", action="append", default=[])
    ap.add_argument("--timeout", type=float, default=60.0)
    ap.add_argument("cmd", nargs=argparse.REMAINDER)
    args = ap.parse_args()

    cmd = args.cmd
    if cmd and cmd[0] == "--":
        cmd = cmd[1:]
    if not cmd:
        sys.exit("no command given")

    pid, fd = pty.fork()
    if pid == 0:
        os.execvp(cmd[0], cmd)
        os._exit(127)  # unreachable

    set_winsize(fd, args.cols, args.rows, args.xpixel, args.ypixel)

    log = open(args.log, "wb")
    deadline = time.time() + args.timeout

    def pump(until):
        """Drain the PTY into the log until `until`, or the child exits."""
        while time.time() < until:
            try:
                r, _, _ = select.select([fd], [], [], 0.05)
            except (OSError, ValueError):
                return False
            if not r:
                continue
            try:
                chunk = os.read(fd, 65536)
            except OSError as e:
                if e.errno in (errno.EIO, errno.EBADF):
                    return False
                raise
            if not chunk:
                return False
            log.write(chunk)
            log.flush()
        return True

    alive = True
    for step in args.step:
        if time.time() > deadline:
            break
        kind, _, rest = step.partition(":")
        if kind == "sleep":
            alive = pump(min(time.time() + float(rest), deadline))
            if not alive:
                break
        elif kind == "send":
            data = rest.encode("utf-8").decode("unicode_escape").encode("latin-1")
            os.write(fd, data)
        else:
            sys.exit(f"unknown step {step!r}")

    if alive:
        pump(min(time.time() + 0.5, deadline))

    # Ask the child to leave, then reap it. SIGHUP is what a closing terminal
    # sends; `shux attach` treats it as a detach and exits its raw mode cleanly.
    try:
        os.kill(pid, signal.SIGHUP)
    except ProcessLookupError:
        pass
    end = time.time() + 5.0
    status = None
    while time.time() < end:
        pump(time.time() + 0.1)
        try:
            done, status = os.waitpid(pid, os.WNOHANG)
        except ChildProcessError:
            break
        if done:
            break
    else:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            os.waitpid(pid, 0)
        except ChildProcessError:
            pass
        log.close()
        os.close(fd)
        sys.exit(124)

    log.close()
    try:
        os.close(fd)
    except OSError:
        pass
    if status is None:
        # Never reaped. Not a clean exit, and callers run this bare under
        # `set -e`, so it must not look like one.
        sys.exit(125)
    if os.WIFEXITED(status):
        sys.exit(os.waitstatus_to_exitcode(status))
    if os.WIFSIGNALED(status):
        sig = os.WTERMSIG(status)
        # SIGHUP is what this script itself sends to ask the client to detach,
        # and SIGTERM is the same request by another route. Anything else means
        # the child DIED -- a SIGSEGV or SIGABRT in `shux attach` must not be
        # indistinguishable from a clean detach, because the harnesses invoke
        # this bare and that exit code is the only place a crash surfaces.
        if sig in (signal.SIGHUP, signal.SIGTERM):
            sys.exit(0)
        sys.stderr.write(f"child died on signal {sig} ({signal.Signals(sig).name})\n")
        sys.exit(128 + sig)
    sys.exit(0)


if __name__ == "__main__":
    main()
