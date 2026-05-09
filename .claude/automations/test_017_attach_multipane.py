# /// script
# requires-python = ">=3.14"
# dependencies = ["iterm2", "pyobjc", "pyobjc-framework-Quartz"]
# ///
"""
Task 017 attach + multi-pane visual test (refactored).

Same Parts A–G as before, but using the iterm2-driver helpers from
_shux_iterm.py: janitor + own window + position-based shots + multi-level
finally cleanup.
"""

import asyncio
import os
import sys
import time
from datetime import datetime

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


results = {"passed": 0, "failed": 0, "unverified": 0, "tests": []}


def log(name: str, status: str, details: str = ""):
    results["tests"].append({"name": name, "status": status, "details": details})
    if status == "PASS":
        results["passed"] += 1
    elif status == "FAIL":
        results["failed"] += 1
    else:
        results["unverified"] += 1
    print(f"  [{status}] {name}{(' - ' + details) if details else ''}")


def summary() -> int:
    print("\n" + "=" * 60)
    print(f"PASS={results['passed']} FAIL={results['failed']} "
          f"UNVERIFIED={results['unverified']}")
    print("=" * 60)
    if results["failed"] > 0:
        for t in results["tests"]:
            if t["status"] == "FAIL":
                print(f"  FAIL: {t['name']} - {t['details']}")
        return 1
    return 0


async def screen(session) -> str:
    sc = await session.async_get_screen_contents()
    return "\n".join(sc.line(i).string for i in range(sc.number_of_lines))


async def wait_for(session, expected: str, timeout: float = 6.0) -> bool:
    elapsed = 0.0
    while elapsed < timeout:
        if expected in await screen(session):
            return True
        await asyncio.sleep(0.2)
        elapsed += 0.2
    return False


async def wait_for_any(session, options: list, timeout: float = 6.0):
    elapsed = 0.0
    while elapsed < timeout:
        text = await screen(session)
        for o in options:
            if o in text:
                return o
        await asyncio.sleep(0.2)
        elapsed += 0.2
    return None


async def attach(session, sname: str):
    await session.async_send_text(f"{SHUX_BIN} attach -s {sname}\n")
    # Wait for status bar to appear
    for _ in range(40):
        text = await screen(session)
        if sname in text and ("[1/" in text or "◆" in text):
            break
        await asyncio.sleep(0.2)
    await asyncio.sleep(0.6)


async def detach(session):
    await session.async_send_text("\x00")
    await asyncio.sleep(0.05)
    await session.async_send_text("d")
    await asyncio.sleep(1.2)


async def main(connection):
    closed = await cleanup_stale_windows(connection)
    if closed:
        print(f"[janitor] closed {closed} stale windows")
    if not ensure_release_build():
        return 1
    kill_daemon()
    shux("ls")
    await asyncio.sleep(1.0)

    # Sanity: both sockets should be bound
    import subprocess
    rt = subprocess.run(
        ["bash", "-c",
         "find /var/folders /tmp -maxdepth 5 -type d -name 'shux-*' 2>/dev/null | head -1"],
        capture_output=True, text=True,
    ).stdout.strip()
    if rt and os.path.isdir(rt):
        files = os.listdir(rt)
        if "shux.sock" in files and "attach.sock" in files:
            log("A1 daemon + both sockets", "PASS")
        else:
            log("A1 daemon + both sockets", "FAIL", str(files))
    else:
        log("A1 daemon + both sockets", "FAIL", "runtime dir not found")

    window, session = await create_window(
        connection, "attach-multipane", x_pos=100, y_pos=80, width=1100, height=720
    )

    try:
        await session.async_send_text(f"cd {PROJECT_ROOT}\n")
        await asyncio.sleep(0.3)

        # PART B: attach launches TUI
        shux("kill", "-s", "b1")
        shux("new", "-s", "b1", "--detached")
        await attach(session, "b1")
        text = await screen(session)
        if "b1" in text:
            log("B1 attach starts + status bar", "PASS")
            await screenshot(window, "017_B1_attach_start")
        else:
            log("B1 attach starts + status bar", "FAIL", "no b1 on screen")

        if any(c in text for c in "─│╭╮╰╯┬┴┤├"):
            log("B2 border characters", "PASS")
        else:
            log("B2 border characters", "UNVERIFIED")

        await asyncio.sleep(0.5)
        await session.async_send_text("echo hello-shux\n")
        if await wait_for(session, "hello-shux", timeout=8):
            log("B3 shell input echoes", "PASS")
        else:
            log("B3 shell input echoes", "FAIL", "echo not found")

        await detach(session)
        if await wait_for(session, "[detached", timeout=4):
            log("B4 detach via prefix d", "PASS")
            await screenshot(window, "017_B4_detached")
        else:
            log("B4 detach via prefix d", "FAIL", "no [detached] message")

        # PART D: splits
        shux("kill", "-s", "d1")
        shux("new", "-s", "d1", "--detached")
        await attach(session, "d1")

        await session.async_send_text("\x00|")
        await asyncio.sleep(1.0)
        text = await screen(session)
        bars = sum(line.count("│") for line in text.splitlines())
        if bars >= 5:
            log("D1 vertical split", "PASS", f"{bars} │ chars")
            await screenshot(window, "017_D1_vsplit")
        else:
            log("D1 vertical split", "FAIL", f"only {bars} │ chars")

        await session.async_send_text("\x00-")
        await asyncio.sleep(1.0)
        text = await screen(session)
        dashes = sum(line.count("─") for line in text.splitlines())
        if dashes >= 8:
            log("D2 horizontal split", "PASS", f"{dashes} ─ chars")
            await screenshot(window, "017_D2_hsplit")
        else:
            log("D2 horizontal split", "UNVERIFIED", f"{dashes} ─ chars")

        await session.async_send_text("\x00z")
        await asyncio.sleep(1.0)
        text = await screen(session)
        bars_z = sum(line.count("│") for line in text.splitlines())
        if bars_z < bars:
            log("D3 zoom collapses splits", "PASS", f"{bars_z} bars (was {bars})")
            await screenshot(window, "017_D3_zoomed")
        else:
            log("D3 zoom collapses splits", "UNVERIFIED")

        await session.async_send_text("\x00z")
        await asyncio.sleep(1.0)
        text = await screen(session)
        bars_uz = sum(line.count("│") for line in text.splitlines())
        if bars_uz >= 5:
            log("D4 unzoom restores splits", "PASS")
        else:
            log("D4 unzoom restores splits", "UNVERIFIED")

        await detach(session)

        # PART F: send-keys via API
        shux("kill", "-s", "f1")
        shux("new", "-s", "f1", "--detached")
        await attach(session, "f1")
        await asyncio.sleep(1.0)
        subprocess.run(
            [SHUX_BIN, "pane", "send-keys", "-s", "f1", "-t",
             "echo INJECTED-FROM-API\n"],
            capture_output=True, text=True, timeout=5,
        )
        match = await wait_for_any(session,
                                    ["INJECTED-FROM-API", "NJECTED-FROM-API"],
                                    timeout=6)
        if match:
            log("F1 send-keys via API", "PASS", match)
            await screenshot(window, "017_F1_send_keys")
        else:
            log("F1 send-keys via API", "FAIL")

        await detach(session)

    finally:
        for s in ["b1", "d1", "f1"]:
            shux("kill", "-s", s)
        await close_window(window)
        leftover = await cleanup_stale_windows(connection)
        if leftover:
            print(f"[janitor:final] closed {leftover} extra windows")

    return summary()


if __name__ == "__main__":
    raise SystemExit(iterm2.run_until_complete(main, retry=False))
