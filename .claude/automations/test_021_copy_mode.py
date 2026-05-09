# /// script
# requires-python = ">=3.14"
# dependencies = ["iterm2", "pyobjc", "pyobjc-framework-Quartz"]
# ///
"""
Task 021 — copy mode (Prefix [) visual test.

Drives the multiplexer through:
  1. Attach + populate a single pane with a known string.
  2. Enter copy mode (Ctrl+Space then [).
  3. Move the cursor with hjkl, anchor with `v`, extend with `l`.
  4. Yank with `y`.
  5. Verify exit by typing into the now-released shell.

Three screenshots: pre-copy, mid-selection, post-yank. The OSC 52
clipboard write also gets dumped to /tmp/shux_yank.log on the
daemon side via a debug hook so we can verify the bytes that hit
the wire encode the right text — visual screenshots can't directly
capture clipboard state but the daemon log can.
"""

import asyncio
import os
import sys

import iterm2

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from _shux_iterm import (  # type: ignore
    SHUX_BIN,
    PROJECT_ROOT,
    cleanup_stale_windows,
    create_window,
    close_window,
    ensure_release_build,
    kill_daemon,
    screenshot,
    shux,
)


async def main(connection):
    closed = await cleanup_stale_windows(connection)
    if closed:
        print(f"[janitor] closed {closed} stale windows")
    if not ensure_release_build():
        return 1

    kill_daemon()
    shux("ls")
    await asyncio.sleep(1.0)

    window, session = await create_window(
        connection, "copymode", x_pos=180, y_pos=80, width=1280, height=800
    )

    try:
        shux("kill", "-s", "copytest")
        shux("new", "-s", "copytest", "--detached")

        await session.async_send_text(f"cd {PROJECT_ROOT}\n")
        await asyncio.sleep(0.3)
        await session.async_send_text(f"{SHUX_BIN} attach -s copytest\n")

        async def attached() -> bool:
            screen = await session.async_get_screen_contents()
            for i in range(screen.number_of_lines):
                line = screen.line(i).string
                if "copytest" in line and ("◆" in line or "[1/1]" in line):
                    return True
            return False

        attached_ok = False
        for _ in range(50):
            if await attached():
                attached_ok = True
                break
            await asyncio.sleep(0.2)
        if not attached_ok:
            print("[fail] attach never showed status bar")
            return 2

        # Print a known marker into the shell so we have something to
        # select. Use a hyphen-separated word to make selection bounds
        # easy to verify.
        await session.async_send_text("printf 'shux-copy-target\\n'\n")
        await asyncio.sleep(1.0)
        await screenshot(window, "021copy_01_attached_with_text")

        # Enter copy mode: Ctrl+Space then [.
        await session.async_send_text("\x00")
        await asyncio.sleep(0.05)
        await session.async_send_text("[")
        await asyncio.sleep(0.6)

        # Move cursor down a couple of rows to land on the "shux-copy-
        # target" line, then anchor and extend right.
        for _ in range(2):
            await session.async_send_text("j")
            await asyncio.sleep(0.05)
        await session.async_send_text("v")  # anchor selection
        await asyncio.sleep(0.1)
        for _ in range(15):
            await session.async_send_text("l")
            await asyncio.sleep(0.02)
        await asyncio.sleep(0.5)
        await screenshot(window, "021copy_02_selecting")

        # Yank — should emit OSC 52 and exit copy mode.
        await session.async_send_text("y")
        await asyncio.sleep(0.8)

        # Prove we're back in the shell: a typed `pwd` should run.
        await session.async_send_text("pwd\n")
        await asyncio.sleep(0.6)
        await screenshot(window, "021copy_03_after_yank")

        # Detach.
        await session.async_send_text("\x00")
        await asyncio.sleep(0.05)
        await session.async_send_text("d")
        await asyncio.sleep(0.6)

    finally:
        shux("kill", "-s", "copytest")
        await close_window(window)
        leftover = await cleanup_stale_windows(connection)
        if leftover:
            print(f"[janitor:final] closed {leftover} extra windows")

    return 0


if __name__ == "__main__":
    raise SystemExit(iterm2.run_until_complete(main, retry=False))
