# /// script
# requires-python = ">=3.14"
# dependencies = ["iterm2", "pyobjc", "pyobjc-framework-Quartz"]
# ///
"""
OOB status bar visual verification.

Runs through:
  1. First-attach welcome toast renders (~3s overlay)
  2. Bar shows session + branch (auto-detected from cwd) + window + hint
  3. After detach + re-attach, toast is gone (state file persisted)
  4. After Ctrl+Space tap, hint is gone (replaced by uptime at wide widths)
  5. Multi-pane: zoom flag `Z` appears in the center zone when zoomed

Each step screenshots so we have visual proof.
"""

import asyncio
import os
import sys
from datetime import datetime

import iterm2

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from _shux_iterm import (  # type: ignore
    SHUX_BIN,
    cleanup_stale_windows,
    create_window,
    close_window,
    screenshot,
    shux,
    kill_daemon,
)


results: list[tuple[str, str, str]] = []


def log(name: str, status: str, details: str = ""):
    icon = "✓" if status == "PASS" else "✗"
    results.append((name, status, details))
    msg = f"  {icon} {name}"
    if details:
        msg += f" — {details}"
    print(msg)


async def screen(session) -> str:
    contents = await session.async_get_screen_contents()
    return "\n".join(
        contents.line(i).string for i in range(contents.number_of_lines)
    )


async def main(connection):
    await cleanup_stale_windows(connection)
    kill_daemon()
    await asyncio.sleep(0.4)
    # Reset onboarding state so the toast actually fires.
    state_file = os.path.expanduser("~/.local/state/shux/onboarding.json")
    if os.path.exists(state_file):
        os.remove(state_file)

    window, sess = await create_window(connection, "oob-statusbar", x_pos=180, width=1300, height=820)
    subdir = "oob_bar"

    try:
        # Part A: first attach — welcome toast should render
        shux("session", "kill", "demo")
        await sess.async_send_text(f"{SHUX_BIN}\n")
        await asyncio.sleep(0.8)  # let attach start, toast appear

        # Capture mid-toast: toast dwell is ~3s, take a shot at +1s
        await screenshot(window, "live_01_welcome_toast", subdir=subdir)
        scr = await screen(sess)
        if "welcome to shux" in scr.lower() or "prefix is" in scr.lower():
            log("welcome toast renders on first attach", "PASS")
        else:
            log("welcome toast renders on first attach", "FAIL", f"screen: {scr[:200]!r}")

        # Wait for dwell to complete, capture the post-toast view
        await asyncio.sleep(3.5)
        await screenshot(window, "live_02_toast_dismissed", subdir=subdir)
        scr = await screen(sess)
        if "welcome to shux" not in scr.lower():
            log("toast auto-dismisses after dwell", "PASS")
        else:
            log("toast auto-dismisses after dwell", "FAIL")

        # Bar should show session + branch + hint
        if "^Sp ?" in scr and "help" in scr:
            log("bar shows onboarding hint on first attach", "PASS")
        else:
            log("bar shows onboarding hint on first attach", "FAIL", "no ^Sp ? help string")

        # Part B: tap the prefix → hint should dismiss permanently
        await sess.async_send_text("\x00")  # Ctrl+Space
        await asyncio.sleep(0.5)
        # Escape to clear prefix-armed state
        await sess.async_send_text("\x1b")
        await asyncio.sleep(0.5)

        # Detach (Ctrl+Space d)
        await sess.async_send_text("\x00d")
        await asyncio.sleep(1.0)
        scr = await screen(sess)
        if "detached" in scr.lower():
            log("Ctrl+Space d detaches cleanly", "PASS")
        else:
            log("Ctrl+Space d detaches cleanly", "FAIL", f"screen: {scr[-200:]!r}")

        # Re-attach: toast should NOT show; hint should be dismissed (replaced by uptime at wide widths)
        await sess.async_send_text(f"{SHUX_BIN}\n")
        await asyncio.sleep(1.0)
        await screenshot(window, "live_03_post_dismissal", subdir=subdir)
        scr = await screen(sess)
        if "welcome to shux" not in scr.lower():
            log("toast does not show on second attach", "PASS")
        else:
            log("toast does not show on second attach", "FAIL")
        if "^Sp ? help" not in scr.replace(" ", "") and "^Sp?help" not in scr:
            log("hint dismissed after prefix tap", "PASS")
        else:
            log("hint dismissed after prefix tap", "FAIL", "hint still visible")

        # Part C: split + zoom to verify the Z flag in center zone
        await sess.async_send_text("\x00|")  # prefix + | → split vertical
        await asyncio.sleep(0.5)
        await screenshot(window, "live_04_split", subdir=subdir)
        await sess.async_send_text("\x00z")  # prefix + z → zoom toggle
        await asyncio.sleep(0.6)
        await screenshot(window, "live_05_zoomed_with_Z_flag", subdir=subdir)
        scr = await screen(sess)
        # Look for the Z flag in the status row at the bottom of the screen
        last_few = "\n".join(scr.splitlines()[-3:])
        if "Z " in last_few or "Z " in last_few:
            log("zoom shows Z flag in center zone", "PASS")
        else:
            log("zoom shows Z flag in center zone", "FAIL", f"tail: {last_few!r}")

        # Detach and clean up
        await sess.async_send_text("\x00d")
        await asyncio.sleep(0.8)
        shux("session", "kill", "default")

    finally:
        try:
            await close_window(window)
        except Exception:
            pass
        kill_daemon()

    passed = sum(1 for _, s, _ in results if s == "PASS")
    failed = sum(1 for _, s, _ in results if s == "FAIL")
    print()
    print(f"  {passed} passed, {failed} failed, {len(results)} total")
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    print(f"shux OOB statusbar live test  ({datetime.now():%Y-%m-%d %H:%M:%S})")
    sys.exit(iterm2.run_until_complete(main))
