# /// script
# requires-python = ">=3.14"
# dependencies = ["iterm2", "pyobjc", "pyobjc-framework-Quartz"]
# ///
"""
Task 017 Real-Apps Visual Demo — full-screen interactive tools inside shux.

Demos:
  D1: full-screen `top`
  D2: 2-pane split — `top` (left) + Python http server (right)
  D3: 3-pane grid — top + httpd + curl traffic loop
  D4: gemini CLI in a pane
  D5: codex + README.md side-by-side

Follows the iterm2-driver patterns: own window, janitor, position-based
screenshots, multi-level cleanup.
"""

import asyncio
import sys
import os

import iterm2

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


async def wait_for_attach(session, sname: str, timeout: float = 8.0) -> bool:
    elapsed = 0.0
    while elapsed < timeout:
        screen = await session.async_get_screen_contents()
        for i in range(screen.number_of_lines):
            line = screen.line(i).string
            if sname in line and ("[1/" in line or "◆" in line):
                return True
        await asyncio.sleep(0.2)
        elapsed += 0.2
    return False


async def attach(session, sname: str, *, settle: float = 1.0):
    await session.async_send_text(f"{SHUX_BIN} attach -s {sname}\n")
    await wait_for_attach(session, sname)
    await asyncio.sleep(settle)


async def detach(session):
    await session.async_send_text("\x00")
    await asyncio.sleep(0.05)
    await session.async_send_text("d")
    await asyncio.sleep(1.0)


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
        connection, "real-apps", x_pos=80, y_pos=80, width=1280, height=820
    )
    shots = []

    try:
        await session.async_send_text(f"cd {PROJECT_ROOT}\n")
        await asyncio.sleep(0.4)

        # ─── D1: full-screen top ────────────────────────────────────
        print("[D1] full-screen top")
        shux("kill", "-s", "demo1")
        shux("new", "-s", "demo1", "--detached")
        await attach(session, "demo1")
        await session.async_send_text("top\n")
        await asyncio.sleep(4.0)
        shots.append(await screenshot(window, "017real_01_top_fullscreen"))
        await session.async_send_text("q")
        await asyncio.sleep(0.6)
        await detach(session)

        # ─── D2: top + httpd ────────────────────────────────────────
        print("[D2] top + http server")
        shux("kill", "-s", "demo2")
        shux("new", "-s", "demo2", "--detached")
        await attach(session, "demo2")
        await session.async_send_text("\x00|")  # vsplit
        await asyncio.sleep(0.7)
        await session.async_send_text(
            "mkdir -p /tmp/shuxdemo && cd /tmp/shuxdemo && "
            "echo '<h1>Hello from shux pane</h1>' > index.html && "
            "python3 -m http.server 9876\n"
        )
        await asyncio.sleep(2.0)
        await session.async_send_text("\x00")  # focus left
        await asyncio.sleep(0.05)
        await session.async_send_text("h")
        await asyncio.sleep(0.4)
        await session.async_send_text("top\n")
        await asyncio.sleep(4.0)
        shots.append(await screenshot(window, "017real_02_top_plus_httpserver"))

        # ─── D3: 3-pane grid w/ live curl traffic ──────────────────
        print("[D3] three-pane grid + curl loop")
        # We're focused on top (left). Split horizontally to add a third pane.
        await session.async_send_text("\x00-")
        await asyncio.sleep(0.7)
        await session.async_send_text(
            "for i in 1 2 3 4 5 6 7 8; do "
            'echo "--- request $i ---"; '
            "curl -s -i http://127.0.0.1:9876/ | head -5; "
            "sleep 0.6; done\n"
        )
        await asyncio.sleep(7.0)
        shots.append(await screenshot(window, "017real_03_three_pane_grid"))
        await detach(session)

        # ─── D4: gemini CLI ────────────────────────────────────────
        print("[D4] gemini CLI")
        shux("kill", "-s", "demo4")
        shux("new", "-s", "demo4", "--detached")
        await attach(session, "demo4")
        await session.async_send_text(
            "gemini -m gemini-2.5-flash -p "
            "'In exactly 12 words, what is shux? It is a Rust terminal "
            "multiplexer with a typed JSON-RPC API for AI agents.'\n"
        )
        await asyncio.sleep(15.0)
        shots.append(await screenshot(window, "017real_04_gemini_cli"))
        await detach(session)

        # ─── D5: codex + README side-by-side ───────────────────────
        print("[D5] codex side-by-side")
        shux("kill", "-s", "demo5")
        shux("new", "-s", "demo5", "--detached")
        await attach(session, "demo5")
        await session.async_send_text("\x00|")
        await asyncio.sleep(0.6)
        await session.async_send_text(f"head -40 {PROJECT_ROOT}/README.md\n")
        await asyncio.sleep(1.0)
        await session.async_send_text("\x00")
        await asyncio.sleep(0.05)
        await session.async_send_text("h")
        await asyncio.sleep(0.3)
        await session.async_send_text(
            "codex exec --model gpt-5.4 "
            "'In one sentence: what is a tmux replacement?'\n"
        )
        await asyncio.sleep(20.0)
        shots.append(await screenshot(window, "017real_05_codex_side_by_side"))
        await detach(session)

    finally:
        for sname in ["demo1", "demo2", "demo3", "demo4", "demo5"]:
            shux("kill", "-s", sname)
        await close_window(window)
        leftover = await cleanup_stale_windows(connection)
        if leftover:
            print(f"[janitor:final] closed {leftover} extra windows")

    print("\n" + "=" * 60)
    print(f"{len([s for s in shots if s])} screenshots captured")
    print("=" * 60)
    return 0


if __name__ == "__main__":
    raise SystemExit(iterm2.run_until_complete(main, retry=False))
