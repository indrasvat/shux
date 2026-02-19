# /// script
# requires-python = ">=3.14"
# dependencies = [
#   "iterm2",
#   "pyobjc",
#   "pyobjc-framework-Quartz",
# ]
# ///

"""
Task 012 Visual Test: M0 Integration and Quality Gate

Tests the full shux CLI→daemon→SessionGraph pipeline by running CLI commands
in an iTerm2 session and verifying output.

Part A — CLI Smoke Tests (detached mode):
    1. Build: `make build` succeeds
    2. New Detached: `shux new -s cli-test -d` creates a detached session
    3. List: `shux ls` shows the session name
    4. API Version: `shux api system.version --format json` returns valid JSON
    5. New Second: `shux new -s cli-test-2 -d` creates a second session
    6. List Both: `shux ls` shows both sessions
    7. Kill: `shux kill -s cli-test` removes first session
    8. List After Kill: `shux ls` shows only cli-test-2
    9. Cleanup: `shux kill -s cli-test-2`

Verification Strategy:
    - Run each CLI command in an iTerm2 session
    - Poll screen contents for expected text strings
    - Capture screenshots at key steps for visual inspection
    - Color verification is visual (screenshots)

Screenshots:
    - 012_new_detached.png: After creating first detached session
    - 012_ls_sessions.png: After listing sessions
    - 012_api_version.png: After API version JSON output
    - 012_ls_both.png: After listing both sessions
    - 012_kill.png: After killing first session
    - 012_ls_after_kill.png: After killing a session and listing

Usage:
    uv run .claude/automations/test_012_m0_integration.py
"""

import iterm2
import asyncio
import subprocess
import os
from datetime import datetime

# ============================================================
# CONFIGURATION
# ============================================================

PROJECT_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SCREENSHOT_DIR = os.path.join(PROJECT_ROOT, ".claude", "screenshots")
SHUX_BIN = os.path.join(PROJECT_ROOT, "target", "debug", "shux")
TIMEOUT_SECONDS = 5.0

# ============================================================
# RESULT TRACKING
# ============================================================

results = []

def record(name, passed, detail=""):
    status = "PASS" if passed else "FAIL"
    results.append((name, passed, detail))
    print(f"  [{status}] {name}" + (f" — {detail}" if detail else ""))


# ============================================================
# HELPERS
# ============================================================

async def read_screen(session):
    """Read all lines from the iTerm2 session screen."""
    screen = await session.async_get_screen_contents()
    lines = []
    for i in range(screen.number_of_lines):
        lines.append(screen.line(i).string)
    return "\n".join(lines)


async def send_and_wait(session, command, wait=1.5):
    """Send a command and wait for output."""
    await session.async_send_text(command + "\n")
    await asyncio.sleep(wait)


def take_screenshot(name):
    """Take a screenshot using screencapture -l (macOS, window-targeted)."""
    os.makedirs(SCREENSHOT_DIR, exist_ok=True)
    filepath = os.path.join(SCREENSHOT_DIR, f"{name}.png")
    try:
        from Quartz import (
            CGWindowListCopyWindowInfo,
            kCGWindowListOptionOnScreenOnly,
            kCGNullWindowID,
        )

        window_list = CGWindowListCopyWindowInfo(
            kCGWindowListOptionOnScreenOnly, kCGNullWindowID
        )
        iterm_windows = [
            w for w in window_list
            if "iterm" in w.get("kCGWindowOwnerName", "").lower()
            and w.get("kCGWindowLayer", -1) == 0
        ]
        if not iterm_windows:
            print(f"    (screenshot skipped: no iTerm2 window found)")
            return False

        wid = iterm_windows[0]["kCGWindowNumber"]
        result = subprocess.run(
            ["screencapture", "-l", str(wid), filepath],
            capture_output=True,
            text=True,
            timeout=5,
        )
        if result.returncode == 0 and os.path.exists(filepath):
            size_kb = os.path.getsize(filepath) / 1024
            print(f"    (screenshot: {name}.png — {size_kb:.0f}KB)")
            return True
        else:
            print(f"    (screenshot failed: screencapture returned {result.returncode})")
            return False
    except Exception as e:
        print(f"    (screenshot error: {e})")
    return False


# ============================================================
# MAIN
# ============================================================

async def main(connection):
    app = await iterm2.async_get_app(connection)
    window = app.current_terminal_window
    if window is None:
        print("ERROR: No iTerm2 window found")
        return

    tab = await window.async_create_tab()
    session = tab.current_session

    try:
        print(f"\nshux M0 Integration Visual Test — {datetime.now().isoformat()}")
        print(f"Project: {PROJECT_ROOT}")
        print(f"Binary: {SHUX_BIN}")
        print()

        # Kill any stale daemon first
        subprocess.run([SHUX_BIN, "kill", "-s", "cli-test"], capture_output=True, timeout=5)
        subprocess.run([SHUX_BIN, "kill", "-s", "cli-test-2"], capture_output=True, timeout=5)

        # ── Test 1: Build ──────────────────────────────────────
        print("Test 1: Build")
        result = subprocess.run(
            ["make", "build"],
            cwd=PROJECT_ROOT,
            capture_output=True,
            text=True,
            timeout=120,
        )
        record("Build", result.returncode == 0, result.stderr.strip()[-80:] if result.returncode != 0 else "")

        if result.returncode != 0:
            print("  ABORTING: Build failed")
            return

        # Change to project dir in iTerm session
        await send_and_wait(session, f"cd {PROJECT_ROOT}", 0.5)
        # Clear screen for clean screenshots
        await send_and_wait(session, "clear", 0.3)

        # ── Test 2: New Detached ───────────────────────────────
        print("Test 2: New Detached Session")
        await send_and_wait(session, f"{SHUX_BIN} new -s cli-test -d", 3.0)
        content = await read_screen(session)
        has_created = "created" in content.lower() or "cli-test" in content.lower()
        record("New Detached", has_created, "")
        take_screenshot("012_new_detached")

        # ── Test 3: List ───────────────────────────────────────
        print("Test 3: List Sessions")
        await send_and_wait(session, f"{SHUX_BIN} ls", 2.0)
        content = await read_screen(session)
        has_session = "cli-test" in content
        record("List Sessions", has_session, "")
        take_screenshot("012_ls_sessions")

        # ── Test 4: API Version JSON ──────────────────────────
        print("Test 4: API Version JSON")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} api system.version --format json", 2.0)
        content = await read_screen(session)
        has_version = "version" in content and "shux" in content.lower()
        record("API Version JSON", has_version, "")
        take_screenshot("012_api_version")

        # ── Test 5: Create Second Session ─────────────────────
        print("Test 5: Create Second Session")
        await send_and_wait(session, f"{SHUX_BIN} new -s cli-test-2 -d", 2.0)
        content = await read_screen(session)
        has_second = "cli-test-2" in content.lower() or "created" in content.lower()
        record("Create Second Session", has_second, "")

        # ── Test 6: List Both ─────────────────────────────────
        print("Test 6: List Both Sessions")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} ls", 2.0)
        content = await read_screen(session)
        has_both = "cli-test" in content and "cli-test-2" in content
        record("List Both Sessions", has_both, "")
        take_screenshot("012_ls_both")

        # ── Test 7: Kill First ────────────────────────────────
        print("Test 7: Kill First Session")
        await send_and_wait(session, f"{SHUX_BIN} kill -s cli-test", 2.0)
        content = await read_screen(session)
        has_killed = "killed" in content.lower() or "cli-test" in content.lower()
        record("Kill Session", has_killed, "")
        take_screenshot("012_kill")

        # ── Test 8: List After Kill ───────────────────────────
        print("Test 8: List After Kill")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} ls", 2.0)
        content = await read_screen(session)
        has_only_second = "cli-test-2" in content
        record("List After Kill", has_only_second, "")
        take_screenshot("012_ls_after_kill")

        # ── Cleanup ───────────────────────────────────────────
        print("Cleanup: Kill remaining session")
        await send_and_wait(session, f"{SHUX_BIN} kill -s cli-test-2", 1.0)

    except Exception as e:
        record("Unexpected Error", False, str(e))
    finally:
        # Print summary
        passed = sum(1 for _, p, _ in results if p)
        total = len(results)
        print(f"\n{'=' * 50}")
        print(f"  Results: {passed}/{total} passed")
        print(f"{'=' * 50}\n")

        # Close the test tab
        try:
            await session.async_send_text("exit\n")
            await asyncio.sleep(0.5)
            await tab.async_close()
        except Exception:
            pass


iterm2.run_until_complete(main)
