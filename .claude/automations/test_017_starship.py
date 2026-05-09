# /// script
# requires-python = ">=3.14"
# dependencies = ["iterm2", "pyobjc", "pyobjc-framework-Quartz"]
# ///
"""
Task 017 — verify user dotfiles + starship load inside a shux pane.

Captures fullscreen screenshots showing the real starship prompt rendering
inside shux after the `bash -l -i` + `TERM_PROGRAM=shux` PTY fix.

Follows the iterm2-driver best practices from the skill:
- Janitor at start (close orphans from crashed prior runs)
- Own window via `iterm2.Window.async_create()` with stale-object refresh
- Position-based Quartz screenshot correlation
- Multi-level try/finally cleanup so the window always closes
"""

import asyncio

import iterm2

import sys
import os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from _shux_iterm import (  # type: ignore
    SHUX_BIN,
    PROJECT_ROOT,
    cleanup_stale_windows,
    create_window,
    close_window,
    screenshot,
    shux,
    kill_daemon,
    ensure_release_build,
)


async def main(connection):
    # 1. Janitor + build sanity.
    closed = await cleanup_stale_windows(connection)
    if closed:
        print(f"[janitor] closed {closed} stale windows")
    if not ensure_release_build():
        return 1

    # 2. Daemon clean start.
    kill_daemon()
    shux("ls")  # auto-spawns daemon
    await asyncio.sleep(1.0)

    # 3. Open our isolated window. Sized for a comfortable screenshot
    # (1280x800 fills most of a 13" laptop screen with margin).
    window, session = await create_window(
        connection, "starship", x_pos=120, y_pos=80, width=1280, height=800
    )

    try:
        # Pre-create a session in the daemon so the attach is instant.
        shux("kill", "-s", "ship")
        shux("new", "-s", "ship", "--detached")

        # Drive the iTerm session into shux attach.
        # Use \n (raw line-feed), not \r — the user's bashrc sources
        # ble.sh, whose readline replacement can interpret \r as
        # "insert-newline" in multiline mode and trap our automation in
        # an editor. \n bypasses the readline keymap entirely; bash
        # treats it as command-submit.
        await session.async_send_text(f"cd {PROJECT_ROOT}\n")
        await asyncio.sleep(0.5)
        await session.async_send_text(f"{SHUX_BIN} attach -s ship\n")

        # Wait for the shux status bar to appear before sending more input.
        # The session-name segment ("ship") in the bottom row is a reliable
        # signal that the attach client is fully up.
        async def attached() -> bool:
            screen = await session.async_get_screen_contents()
            for i in range(screen.number_of_lines):
                line = screen.line(i).string
                if "ship" in line and ("[1/1]" in line or "◆" in line):
                    return True
            return False

        for _ in range(40):  # up to ~8s
            if await attached():
                break
            await asyncio.sleep(0.2)

        # Now send a command into the shux pane. Inside attach the user's
        # bash + ble.sh receive these bytes; \n still works as submit.
        await session.async_send_text("git status -s; echo ---; date\n")
        await asyncio.sleep(1.5)
        await screenshot(window, "017starship_01_single_pane")

        # Vertical split, env probe in the new pane.
        await session.async_send_text("\x00|")  # Ctrl+Space then '|'
        await asyncio.sleep(0.8)
        await session.async_send_text(
            "ls crates/ && env | "
            "grep -E '^(SHUX|TERM_PROGRAM|COLORTERM)' | sort\n"
        )
        await asyncio.sleep(1.5)
        await screenshot(window, "017starship_02_two_panes")

        # Detach cleanly: Ctrl+Space d. Send each byte separately so the
        # client's prefix-key state machine doesn't see them merged with
        # the previous newline.
        await asyncio.sleep(0.3)
        await session.async_send_text("\x00")
        await asyncio.sleep(0.05)
        await session.async_send_text("d")
        await asyncio.sleep(1.0)

    finally:
        # Multi-level cleanup: kill the shux session, close the iTerm
        # window we created, then a final janitor sweep just in case
        # something we forgot is still around.
        shux("kill", "-s", "ship")
        await close_window(window)
        leftover = await cleanup_stale_windows(connection)
        if leftover:
            print(f"[janitor:final] closed {leftover} extra windows")

    return 0


if __name__ == "__main__":
    raise SystemExit(iterm2.run_until_complete(main, retry=False))
