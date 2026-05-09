# /// script
# requires-python = ">=3.14"
# dependencies = ["iterm2", "pyobjc", "pyobjc-framework-Quartz"]
# ///
"""
Task 033 — help overlay (Ctrl+Space + ?) visual test.

Three screenshots, each driven by user-visible keystrokes:
  1. attached, no overlay — multipane shell with status bar
  2. overlay open — Ctrl+Space then ? produces the cheat sheet
  3. overlay dismissed — q clears the overlay, underlying content
     fully restored (no ghost pixels)

The rendering layer of the overlay is unit-tested in
shux-ui::help_overlay; this script exists to catch the integration
issues unit tests miss: input routing, dismiss handling, and the
force-redraw on dismiss that prevents ghost glyphs.
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
        connection, "helpoverlay", x_pos=160, y_pos=80, width=1280, height=800
    )

    try:
        shux("kill", "-s", "helptest")
        shux("new", "-s", "helptest", "--detached")

        await session.async_send_text(f"cd {PROJECT_ROOT}\n")
        await asyncio.sleep(0.3)
        await session.async_send_text(f"{SHUX_BIN} attach -s helptest\n")

        async def attached() -> bool:
            screen = await session.async_get_screen_contents()
            for i in range(screen.number_of_lines):
                line = screen.line(i).string
                if "helptest" in line and ("◆" in line or "[1/1]" in line):
                    return True
            return False

        attached_ok = False
        for _ in range(50):
            if await attached():
                attached_ok = True
                break
            await asyncio.sleep(0.2)
        if not attached_ok:
            print("[fail] shux attach never showed its status bar")
            return 2

        # Split so the underlying content is interesting (proves the
        # overlay doesn't damage the multipane render).
        await session.async_send_text("\x00|")
        await asyncio.sleep(0.8)
        await session.async_send_text("ls crates/ | head\n")
        await asyncio.sleep(1.0)
        await screenshot(window, "033help_01_attached_no_overlay")

        # Open the overlay: Ctrl+Space then ? — send each byte distinctly
        # so the prefix state machine in shux-ui sees them as a chord
        # rather than a smooshed sequence.
        await session.async_send_text("\x00")
        await asyncio.sleep(0.05)
        await session.async_send_text("?")
        await asyncio.sleep(1.0)
        await screenshot(window, "033help_02_overlay_open")

        # Try a key that the underlying shell would normally consume
        # (`l`). Help is up, so the keypress should be swallowed and
        # NOT reach bash. Then dismiss with q, and verify the shell
        # got nothing while the overlay was up.
        await session.async_send_text("l")
        await asyncio.sleep(0.4)
        await session.async_send_text("q")
        await asyncio.sleep(1.0)
        # Echo a marker so the post-dismiss screenshot proves bash is
        # alive and accepting input again.
        await session.async_send_text("echo overlay-dismissed\n")
        await asyncio.sleep(0.7)
        await screenshot(window, "033help_03_overlay_dismissed")

        await session.async_send_text("\x00")
        await asyncio.sleep(0.05)
        await session.async_send_text("d")
        await asyncio.sleep(0.6)

    finally:
        shux("kill", "-s", "helptest")
        await close_window(window)
        leftover = await cleanup_stale_windows(connection)
        if leftover:
            print(f"[janitor:final] closed {leftover} extra windows")

    return 0


if __name__ == "__main__":
    raise SystemExit(iterm2.run_until_complete(main, retry=False))
