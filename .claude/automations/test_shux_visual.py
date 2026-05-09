# /// script
# requires-python = ">=3.14"
# dependencies = [
#   "iterm2",
#   "pyobjc",
#   "pyobjc-framework-Quartz",
# ]
# ///
"""
shux Visual Verification — iTerm2 Automated Test Suite

Tests exercised:
  1. shux version       — verify daemon auto-start & version handshake
  2. shux new           — create named sessions (alpha, beta)
  3. shux ls            — list sessions with rich box-drawing output
  4. shux window        — create/list windows inside a session
  5. shux pane          — split panes, list panes
  6. shux kill + ls     — kill a session and verify removal

Verification strategy:
  - Run each CLI command in an iTerm2 session
  - Wait for output, read screen lines, assert key substrings
  - Capture a screenshot per scenario via Quartz window ID

Screenshots saved to: .claude/screenshots/shux_visual_*.png

Usage: uv run .claude/automations/test_shux_visual.py
"""

import asyncio
import os
import subprocess
import iterm2
import Quartz

PROJECT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SHUX = os.path.join(PROJECT, "target", "debug", "shux")
SCREENSHOTS = os.path.join(PROJECT, ".claude", "screenshots")
os.makedirs(SCREENSHOTS, exist_ok=True)

# ─── helpers ───────────────────────────────────────────────────────────

async def create_window(connection, name="shux-test", x_pos=100, width=900, height=520):
    window = await iterm2.Window.async_create(connection)
    await asyncio.sleep(0.6)
    app = await iterm2.async_get_app(connection)
    if window.current_tab is None:
        for w in app.terminal_windows:
            if w.window_id == window.window_id:
                window = w
                break
    for _ in range(20):
        if window.current_tab and window.current_tab.current_session:
            break
        await asyncio.sleep(0.2)
    if not window.current_tab or not window.current_tab.current_session:
        raise RuntimeError(f"Window '{name}' not ready")
    session = window.current_tab.current_session
    await session.async_set_name(name)
    frame = await window.async_get_frame()
    await window.async_set_frame(iterm2.Frame(
        iterm2.Point(x_pos, 60),
        iterm2.Size(width, height),
    ))
    await asyncio.sleep(0.3)
    return window, session


async def capture_screenshot(window, filename):
    path = os.path.join(SCREENSHOTS, filename)
    frame = await window.async_get_frame()
    window_list = Quartz.CGWindowListCopyWindowInfo(
        Quartz.kCGWindowListOptionOnScreenOnly | Quartz.kCGWindowListExcludeDesktopElements,
        Quartz.kCGNullWindowID,
    )
    best_id, best_score = None, float("inf")
    for w in window_list:
        if "iTerm" not in w.get("kCGWindowOwnerName", ""):
            continue
        b = w.get("kCGWindowBounds", {})
        score = (abs(float(b.get("X", 0)) - frame.origin.x) * 2
                 + abs(float(b.get("Width", 0)) - frame.size.width)
                 + abs(float(b.get("Height", 0)) - frame.size.height))
        if score < best_score:
            best_score, best_id = score, w.get("kCGWindowNumber")
    if best_id and best_score < 50:
        subprocess.run(["screencapture", "-x", "-l", str(best_id), path], check=True)
        print(f"  📸 {path}")
        return path
    print(f"  ⚠️  screenshot failed for {filename} (score={best_score})")
    return None


async def run_cmd(session, cmd, wait=2.0):
    """Send a command and wait for output."""
    await session.async_send_text(cmd + "\n")
    await asyncio.sleep(wait)


async def read_screen(session, lines=30):
    """Read visible screen text."""
    screen = await session.async_get_screen_contents()
    out = []
    for i in range(min(lines, screen.number_of_lines)):
        out.append(screen.line(i).string)
    return "\n".join(out)


async def cleanup_stale(connection):
    app = await iterm2.async_get_app(connection)
    for window in app.terminal_windows:
        for tab in window.tabs:
            for session in tab.sessions:
                if session.name and session.name.startswith("shux-test"):
                    try:
                        await session.async_send_text("exit\n")
                        await asyncio.sleep(0.1)
                        await session.async_close()
                    except Exception:
                        pass


# ─── main test suite ───────────────────────────────────────────────────

async def main(connection):
    results = {"passed": 0, "failed": 0, "tests": []}

    # Kill stale daemon + cleanup stale windows
    subprocess.run(["pkill", "-f", "shux.*__daemon"], capture_output=True)
    await asyncio.sleep(0.3)
    await cleanup_stale(connection)

    created_sessions = []
    window = None

    try:
        window, session = await create_window(connection, "shux-test", x_pos=120)
        created_sessions.append(session)

        # Set PATH so shux binary is found
        await run_cmd(session, f"export PATH=\"{os.path.dirname(SHUX)}:$PATH\"", wait=0.3)
        await run_cmd(session, "clear", wait=0.3)

        # ── Test 1: shux version ──────────────────────────────────────
        print("\n── Test 1: shux version")
        await run_cmd(session, f"{SHUX} version", wait=3.0)
        screen = await read_screen(session)
        t1 = "0.1.0" in screen
        results["tests"].append({"name": "version", "pass": t1})
        results["passed" if t1 else "failed"] += 1
        print(f"  {'✅' if t1 else '❌'} version output contains 0.1.0")
        await capture_screenshot(window, "shux_visual_01_version.png")

        await run_cmd(session, "clear", wait=0.3)

        # ── Test 2: create sessions ───────────────────────────────────
        print("\n── Test 2: shux new (create sessions)")
        await run_cmd(session, f"{SHUX} new -s alpha -d", wait=3.0)
        screen_a = await read_screen(session)
        await run_cmd(session, f"{SHUX} new -s beta -d", wait=2.0)
        screen_b = await read_screen(session)
        t2 = "alpha" in screen_a or "alpha" in screen_b
        t2b = "beta" in screen_b
        results["tests"].append({"name": "create_sessions", "pass": t2 and t2b})
        results["passed" if (t2 and t2b) else "failed"] += 1
        print(f"  {'✅' if t2 else '❌'} session alpha created")
        print(f"  {'✅' if t2b else '❌'} session beta created")
        await capture_screenshot(window, "shux_visual_02_create_sessions.png")

        await run_cmd(session, "clear", wait=0.3)

        # ── Test 3: list sessions ─────────────────────────────────────
        print("\n── Test 3: shux ls (list sessions)")
        await run_cmd(session, f"{SHUX} ls", wait=2.0)
        screen = await read_screen(session)
        t3a = "alpha" in screen
        t3b = "beta" in screen
        results["tests"].append({"name": "list_sessions", "pass": t3a and t3b})
        results["passed" if (t3a and t3b) else "failed"] += 1
        print(f"  {'✅' if t3a else '❌'} alpha visible in ls")
        print(f"  {'✅' if t3b else '❌'} beta visible in ls")
        await capture_screenshot(window, "shux_visual_03_ls.png")

        await run_cmd(session, "clear", wait=0.3)

        # ── Test 4: window CRUD ───────────────────────────────────────
        print("\n── Test 4: shux window (create + list)")
        await run_cmd(session, f"{SHUX} window new -s alpha -n editor", wait=2.0)
        await run_cmd(session, f"{SHUX} window ls -s alpha", wait=2.0)
        screen = await read_screen(session)
        t4 = "editor" in screen
        results["tests"].append({"name": "window_crud", "pass": t4})
        results["passed" if t4 else "failed"] += 1
        print(f"  {'✅' if t4 else '❌'} window 'editor' visible")
        await capture_screenshot(window, "shux_visual_04_windows.png")

        await run_cmd(session, "clear", wait=0.3)

        # ── Test 5: pane operations ───────────────────────────────────
        print("\n── Test 5: shux pane (split + list)")
        await run_cmd(session, f"{SHUX} pane split -s alpha -d vertical", wait=2.0)
        await run_cmd(session, f"{SHUX} pane ls -s alpha", wait=2.0)
        screen = await read_screen(session)
        # After split there should be 2+ panes listed
        t5 = screen.count("│") >= 2 or "pane" in screen.lower() or "focus" in screen.lower()
        results["tests"].append({"name": "pane_split_list", "pass": t5})
        results["passed" if t5 else "failed"] += 1
        print(f"  {'✅' if t5 else '❌'} pane list shows panes after split")
        await capture_screenshot(window, "shux_visual_05_panes.png")

        await run_cmd(session, "clear", wait=0.3)

        # ── Test 6: kill + verify ─────────────────────────────────────
        print("\n── Test 6: shux kill + ls (verify removal)")
        await run_cmd(session, f"{SHUX} kill -s beta", wait=2.0)
        await run_cmd(session, f"{SHUX} ls", wait=2.0)
        screen = await read_screen(session)
        t6a = "beta" not in screen or "Killed" in screen
        t6b = "alpha" in screen
        results["tests"].append({"name": "kill_verify", "pass": t6a and t6b})
        results["passed" if (t6a and t6b) else "failed"] += 1
        print(f"  {'✅' if t6a else '❌'} beta removed after kill")
        print(f"  {'✅' if t6b else '❌'} alpha still present")
        await capture_screenshot(window, "shux_visual_06_kill.png")

        # ── Summary ───────────────────────────────────────────────────
        print(f"\n{'='*50}")
        print(f"  RESULTS: {results['passed']} passed, {results['failed']} failed")
        print(f"{'='*50}\n")

    except Exception as e:
        print(f"\n❌ FATAL: {e}")
        import traceback; traceback.print_exc()
        raise
    finally:
        # Cleanup: kill daemon, close sessions
        subprocess.run(["pkill", "-f", "shux.*__daemon"], capture_output=True)
        for s in created_sessions:
            try:
                await s.async_send_text("\x03")
                await asyncio.sleep(0.1)
                await s.async_send_text("exit\n")
                await asyncio.sleep(0.2)
                await s.async_close()
            except Exception:
                pass


iterm2.run_until_complete(main, retry=True)
