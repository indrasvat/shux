# /// script
# requires-python = ">=3.14"
# dependencies = ["iterm2", "pyobjc", "pyobjc-framework-Quartz"]
# ///
"""
Task 024 — verify the [theme] section drives border colors and status
bar palette through hot reload.

Walks through three states, each captured as a screenshot:
  1. baseline (no [theme] in user config) — matches the pre-theme look
  2. magenta override on border_focused via hot reload
  3. theme cleared again — borders snap back to the default sapphire

The point is to prove (a) themed values actually reach the renderer
and (b) hot reload re-resolves the theme without restart, so the live
edit story matches the existing border_style hot-reload story.
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


CONFIG_DIR = os.path.expanduser("~/.config/shux")
CONFIG_PATH = os.path.join(CONFIG_DIR, "config.toml")


def write_config(theme_block: str) -> None:
    """Replace the user's shux config with a minimal one + the supplied
    [theme] block (which can be empty). Backs up any existing config the
    first time we run so we can restore at exit."""
    os.makedirs(CONFIG_DIR, exist_ok=True)
    body = (
        "# autotest config\n"
        "[appearance]\n"
        'border_style = "rounded"\n'
        "\n"
        f"{theme_block.strip()}\n"
    )
    with open(CONFIG_PATH, "w") as f:
        f.write(body)


def backup_config() -> str | None:
    """Move the existing config aside; return the backup path or None."""
    if not os.path.exists(CONFIG_PATH):
        return None
    bak = CONFIG_PATH + ".autotest.bak"
    os.replace(CONFIG_PATH, bak)
    return bak


def restore_config(bak: str | None) -> None:
    if bak and os.path.exists(bak):
        os.replace(bak, CONFIG_PATH)
    elif not bak and os.path.exists(CONFIG_PATH):
        # No prior config existed; remove the test one we wrote.
        os.remove(CONFIG_PATH)


async def main(connection):
    closed = await cleanup_stale_windows(connection)
    if closed:
        print(f"[janitor] closed {closed} stale windows")
    if not ensure_release_build():
        return 1

    backup = backup_config()
    write_config("")  # baseline: no [theme] block

    kill_daemon()
    shux("ls")  # boot daemon
    await asyncio.sleep(1.0)

    window, session = await create_window(
        connection, "theme", x_pos=140, y_pos=80, width=1280, height=800
    )

    try:
        shux("kill", "-s", "themetest")
        shux("new", "-s", "themetest", "--detached")

        await session.async_send_text(f"cd {PROJECT_ROOT}\n")
        await asyncio.sleep(0.3)
        await session.async_send_text(f"{SHUX_BIN} attach -s themetest\n")

        # Wait for the shux status bar (`◆ themetest`) — the most
        # reliable signal that the attach client is fully up.
        async def attached() -> bool:
            screen = await session.async_get_screen_contents()
            for i in range(screen.number_of_lines):
                line = screen.line(i).string
                if "themetest" in line and ("◆" in line or "[1/1]" in line):
                    return True
            return False

        attached_ok = False
        for _ in range(50):  # up to ~10s
            if await attached():
                attached_ok = True
                break
            await asyncio.sleep(0.2)
        if not attached_ok:
            print("[fail] shux attach never showed its status bar")
            return 2

        # Split so the focused-pane border is unmistakably visible.
        await session.async_send_text("\x00|")
        await asyncio.sleep(1.0)
        await session.async_send_text("echo baseline-theme\n")
        await asyncio.sleep(1.5)
        await screenshot(window, "024theme_01_baseline_sapphire")

        # Hot-reload to a magenta border. Watcher debounce is ~150ms;
        # give the daemon 2.5s of slack so the redraw definitely lands.
        write_config(
            '[theme]\n'
            'border_focused = "#ff5fff"\n'
            'status_accent = "#ff5fff"\n'
            'status_bg = "#1a0e2e"\n'
        )
        await asyncio.sleep(2.5)
        await session.async_send_text("echo magenta-override\n")
        await asyncio.sleep(1.5)
        await screenshot(window, "024theme_02_magenta_override")

        # Hot-reload back to baseline — verifies the hot path also
        # handles "going back to defaults".
        write_config("")
        await asyncio.sleep(2.5)
        await session.async_send_text("echo back-to-default\n")
        await asyncio.sleep(1.5)
        await screenshot(window, "024theme_03_back_to_default")

        # Detach cleanly.
        await session.async_send_text("\x00")
        await asyncio.sleep(0.05)
        await session.async_send_text("d")
        await asyncio.sleep(0.8)

    finally:
        shux("kill", "-s", "themetest")
        await close_window(window)
        restore_config(backup)
        leftover = await cleanup_stale_windows(connection)
        if leftover:
            print(f"[janitor:final] closed {leftover} extra windows")

    return 0


if __name__ == "__main__":
    raise SystemExit(iterm2.run_until_complete(main, retry=False))
