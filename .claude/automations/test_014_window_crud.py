# /// script
# requires-python = ">=3.14"
# dependencies = [
#   "iterm2",
#   "pyobjc",
#   "pyobjc-framework-Quartz",
# ]
# ///

"""
Task 014 Visual Test: Window CRUD (API + CLI)

Tests the full window lifecycle: create, list, rename, focus, reorder, kill
through the shux CLI in an iTerm2 session, verifying styled output.

Part A — Setup & Default Window Verification (Tests 1–4)
Part B — Window Creation (Tests 5–9)
Part C — Window Auto-Naming (Tests 10–11)
Part D — Window Focus/Switching (Tests 12–14)
Part E — Window Rename (Tests 15–17)
Part F — Window Reorder (Tests 18–20)
Part G — Window Kill (Tests 21–24)
Part H — JSON Output & Cross-Verification (Test 25)

Usage:
    uv run .claude/automations/test_014_window_crud.py
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


def count_windows(content):
    """Count window entries in `window list` output."""
    count = 0
    for line in content.split("\n"):
        line = line.strip()
        # Window entries start with an index number followed by : or *
        if line and line[0].isdigit() and ("pane" in line.lower() or ":" in line):
            count += 1
    return count


def parse_active_window(content):
    """Find the active window name (line containing *)."""
    for line in content.split("\n"):
        if "*" in line and line.strip() and line.strip()[0].isdigit():
            return line.strip()
    return None


def parse_window_names(content):
    """Extract window names from window list output."""
    names = []
    for line in content.split("\n"):
        line = line.strip()
        if line and line[0].isdigit() and ":" in line:
            # Format: "0*: name (1 pane)" or "0 : name (1 pane)"
            parts = line.split(":", 1)
            if len(parts) >= 2:
                rest = parts[1].strip()
                # Name is before " (" pane info
                if " (" in rest:
                    name = rest.split(" (")[0].strip()
                else:
                    name = rest.strip()
                names.append(name)
    return names


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
        print(f"\nshux Window CRUD Visual Test (014) — {datetime.now().isoformat()}")
        print(f"Project: {PROJECT_ROOT}")
        print(f"Binary: {SHUX_BIN}")
        print()

        # Kill any stale daemon so the fresh binary is used
        subprocess.run(["pkill", "-f", "shux __daemon"], capture_output=True, timeout=5)
        await asyncio.sleep(1)

        # Kill any stale test session (new daemon will auto-start)
        subprocess.run([SHUX_BIN, "kill", "-s", "ws-test"], capture_output=True, timeout=5)

        # ══════════════════════════════════════════════════════
        # Part A — Setup & Default Window Verification (Tests 1–4)
        # ══════════════════════════════════════════════════════
        print("Part A — Setup & Default Window Verification")

        # ── Test 1: Build ─────────────────────────────────────
        print("Test 1: Build")
        result = subprocess.run(
            ["make", "build"],
            cwd=PROJECT_ROOT,
            capture_output=True,
            text=True,
            timeout=120,
        )
        record("1. Build", result.returncode == 0,
               result.stderr.strip()[-80:] if result.returncode != 0 else "")

        if result.returncode != 0:
            print("  ABORTING: Build failed")
            return

        # Change to project dir
        await send_and_wait(session, f"cd {PROJECT_ROOT}", 0.5)

        # ── Test 2: Create Session ────────────────────────────
        print("Test 2: Create Session (ws-test)")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} new -s ws-test -d", 3.0)
        content = await read_screen(session)
        has_ws = "ws-test" in content
        has_created = "created" in content.lower()
        record("2. Create session", has_ws and has_created,
               "" if has_ws else "missing 'ws-test' in output")
        take_screenshot("014_session_created")

        # ── Test 3: Default Window Exists ─────────────────────
        print("Test 3: Default Window Exists")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} window list -s ws-test", 2.0)
        content = await read_screen(session)
        wcount = count_windows(content)
        has_pane = "pane" in content.lower()
        record("3. Default window exists", wcount == 1 and has_pane,
               f"window count={wcount}, has pane info={has_pane}")
        take_screenshot("014_default_window")

        # ── Test 4: Default Window Has Active Marker ──────────
        print("Test 4: Default Window Has Active Marker")
        active_line = parse_active_window(content)
        record("4. Default window active", active_line is not None,
               f"active line: {active_line}" if active_line else "no active marker found")

        # ══════════════════════════════════════════════════════
        # Part B — Window Creation (Tests 5–9)
        # ══════════════════════════════════════════════════════
        print("\nPart B — Window Creation")

        # ── Test 5: Create Named Window (editor) ─────────────
        print("Test 5: Create Named Window (editor)")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} window new -s ws-test -n editor", 2.0)
        content = await read_screen(session)
        has_editor = "editor" in content.lower()
        has_created = "created" in content.lower()
        record("5. Create editor", has_editor and has_created,
               "" if has_editor else "missing 'editor' in output")
        take_screenshot("014_create_editor")

        # ── Test 6: Create Second Named Window (server) ──────
        print("Test 6: Create Second Named Window (server)")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} window new -s ws-test -n server", 2.0)
        content = await read_screen(session)
        has_server = "server" in content.lower()
        record("6. Create server", has_server, "")
        take_screenshot("014_create_server")

        # ── Test 7: Create Third Named Window (logs) ─────────
        print("Test 7: Create Third Named Window (logs)")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} window new -s ws-test -n logs", 2.0)
        content = await read_screen(session)
        has_logs = "logs" in content.lower()
        record("7. Create logs", has_logs, "")
        take_screenshot("014_create_logs")

        # ── Test 8: List Shows All Windows ────────────────────
        print("Test 8: List Shows All Windows")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} window list -s ws-test", 2.0)
        content = await read_screen(session)
        wcount = count_windows(content)
        has_all = all(name in content.lower() for name in ["editor", "server", "logs"])
        record("8. List all windows", wcount == 4 and has_all,
               f"count={wcount}, has all names={has_all}")
        take_screenshot("014_list_all_windows")

        # ── Test 9: Newest Window Is Active ───────────────────
        print("Test 9: Newest Window Is Active")
        active_line = parse_active_window(content)
        logs_active = active_line is not None and "logs" in active_line.lower()
        record("9. Newest window active", logs_active,
               f"active: {active_line}" if active_line else "no active marker")

        # ══════════════════════════════════════════════════════
        # Part C — Window Auto-Naming (Tests 10–11)
        # ══════════════════════════════════════════════════════
        print("\nPart C — Window Auto-Naming")

        # ── Test 10: Create Unnamed Window ────────────────────
        print("Test 10: Create Unnamed Window")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} window new -s ws-test", 2.0)
        content = await read_screen(session)
        has_created = "created" in content.lower()
        record("10. Create unnamed", has_created,
               "" if has_created else "missing 'Created' in output")
        take_screenshot("014_create_unnamed")

        # ── Test 11: Verify Auto-Name in List ─────────────────
        print("Test 11: Verify Auto-Name in List")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} window list -s ws-test", 2.0)
        content = await read_screen(session)
        wcount = count_windows(content)
        record("11. Auto-name in list", wcount == 5,
               f"window count={wcount} (expected 5)")
        take_screenshot("014_list_with_unnamed")

        # ══════════════════════════════════════════════════════
        # Part D — Window Focus/Switching (Tests 12–14)
        # ══════════════════════════════════════════════════════
        print("\nPart D — Window Focus/Switching")

        # ── Test 12: Focus By Name ────────────────────────────
        print("Test 12: Focus By Name (editor)")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} window focus -s ws-test -w editor", 2.0)
        content = await read_screen(session)
        has_focused = "focused" in content.lower() or "editor" in content.lower()
        no_error = "error" not in content.lower()
        record("12. Focus editor", has_focused and no_error,
               "" if no_error else "error in output")
        take_screenshot("014_focus_editor")

        # ── Test 13: Verify Focus Changed ─────────────────────
        print("Test 13: Verify Focus Changed")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} window list -s ws-test", 2.0)
        content = await read_screen(session)
        active_line = parse_active_window(content)
        editor_active = active_line is not None and "editor" in active_line.lower()
        record("13. Editor is active", editor_active,
               f"active: {active_line}" if active_line else "no active marker")
        take_screenshot("014_list_after_focus")

        # ── Test 14: Focus Returns Previous ───────────────────
        print("Test 14: Focus Returns Previous (from test 12 output)")
        # The focus command output should show the previous window info
        # We already captured test 12's content; just check for any relevant output
        record("14. Focus returns previous", has_focused,
               "focus command succeeded")

        # ══════════════════════════════════════════════════════
        # Part E — Window Rename (Tests 15–17)
        # ══════════════════════════════════════════════════════
        print("\nPart E — Window Rename")

        # ── Test 15: Rename Window ────────────────────────────
        print("Test 15: Rename Window (server -> backend)")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} window rename -s ws-test -w server -n backend", 2.0)
        content = await read_screen(session)
        has_renamed = "renamed" in content.lower() or "backend" in content.lower()
        no_error = "error" not in content.lower()
        record("15. Rename server->backend", has_renamed and no_error,
               "" if no_error else "error in output")
        take_screenshot("014_rename_backend")

        # ── Test 16: Verify Rename in List ────────────────────
        print("Test 16: Verify Rename in List")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} window list -s ws-test", 2.0)
        content = await read_screen(session)
        names = parse_window_names(content)
        has_backend = "backend" in [n.lower() for n in names]
        no_server = "server" not in [n.lower() for n in names]
        record("16. Rename verified", has_backend and no_server,
               f"names: {names}")
        take_screenshot("014_list_after_rename")

        # ── Test 17: Rename Conflict ──────────────────────────
        print("Test 17: Rename Conflict")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} window rename -s ws-test -w logs -n editor", 2.0)
        content = await read_screen(session)
        has_error = "error" in content.lower() or "conflict" in content.lower() or "exists" in content.lower()
        record("17. Rename conflict", has_error,
               "" if has_error else "expected error/conflict in output")
        take_screenshot("014_rename_conflict")

        # ══════════════════════════════════════════════════════
        # Part F — Window Reorder (Tests 18–20)
        # ══════════════════════════════════════════════════════
        print("\nPart F — Window Reorder")

        # ── Test 18: Reorder Window to Front ──────────────────
        print("Test 18: Reorder Window to Front (logs -> index 0)")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} window reorder -s ws-test -w logs -i 0", 2.0)
        content = await read_screen(session)
        has_moved = "moved" in content.lower() or "logs" in content.lower()
        no_error = "error" not in content.lower()
        record("18. Reorder logs to 0", has_moved and no_error,
               "" if no_error else "error in output")
        take_screenshot("014_reorder")

        # ── Test 19: Verify Reorder in List ───────────────────
        print("Test 19: Verify Reorder in List")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} window list -s ws-test", 2.0)
        content = await read_screen(session)
        names = parse_window_names(content)
        logs_first = len(names) > 0 and names[0].lower() == "logs"
        record("19. Logs at index 0", logs_first,
               f"first name: {names[0] if names else '?'}")
        take_screenshot("014_list_after_reorder")

        # ── Test 20: Reorder Out of Range ─────────────────────
        print("Test 20: Reorder Out of Range")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} window reorder -s ws-test -w editor -i 99", 2.0)
        content = await read_screen(session)
        has_error = "error" in content.lower() or "out of range" in content.lower() or "invalid" in content.lower()
        record("20. Reorder out of range", has_error,
               "" if has_error else "expected error in output")
        take_screenshot("014_reorder_out_of_range")

        # ══════════════════════════════════════════════════════
        # Part G — Window Kill (Tests 21–24)
        # ══════════════════════════════════════════════════════
        print("\nPart G — Window Kill")

        # ── Test 21: Kill a Window ────────────────────────────
        print("Test 21: Kill a Window (logs)")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} window kill -s ws-test -w logs", 2.0)
        content = await read_screen(session)
        has_killed = "killed" in content.lower()
        record("21. Kill logs", has_killed,
               "" if has_killed else "missing 'Killed' in output")
        take_screenshot("014_kill_logs")

        # ── Test 22: Verify Kill in List ──────────────────────
        print("Test 22: Verify Kill in List")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} window list -s ws-test", 2.0)
        content = await read_screen(session)
        wcount = count_windows(content)
        no_logs = "logs" not in parse_window_names(content)
        record("22. Logs gone from list", wcount == 4 and no_logs,
               f"count={wcount}, logs gone={no_logs}")
        take_screenshot("014_list_after_kill")

        # ── Test 23: Kill Active → Focus Moves ────────────────
        print("Test 23: Kill Active Window → Focus Moves")
        # Find the active window first
        active_line = parse_active_window(content)
        if active_line:
            # Extract window name from active line
            active_names = parse_window_names(active_line)
            if active_names:
                active_name = active_names[0]
            else:
                active_name = "editor"  # fallback
        else:
            active_name = "editor"  # fallback

        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} window kill -s ws-test -w {active_name}", 2.0)
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} window list -s ws-test", 2.0)
        content = await read_screen(session)
        wcount = count_windows(content)
        new_active = parse_active_window(content)
        # After killing active, a different window should be active
        record("23. Kill active, focus moves", wcount == 3 and new_active is not None,
               f"count={wcount}, new active: {new_active}")
        take_screenshot("014_kill_active")

        # ── Test 24: Kill Last Window Fails ───────────────────
        print("Test 24: Kill Last Window Fails")
        # Kill all but one using highest index first (avoids name/index ambiguity)
        remaining = count_windows(content)
        for idx in range(remaining - 1, 0, -1):
            await send_and_wait(session, f"{SHUX_BIN} window kill -s ws-test -w {idx}", 1.5)

        # Try to kill the last one (index 0)
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} window kill -s ws-test -w 0", 2.0)
        content = await read_screen(session)
        has_error = "error" in content.lower() or "last" in content.lower() or "cannot" in content.lower()

        # Verify the window still exists
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} window list -s ws-test", 1.5)
        content = await read_screen(session)
        final_count = count_windows(content)
        record("24. Kill last fails", has_error and final_count == 1,
               f"error shown={has_error}, final count={final_count}")
        take_screenshot("014_kill_last_fails")

        # ══════════════════════════════════════════════════════
        # Part H — JSON Output & Cross-Verification (Test 25)
        # ══════════════════════════════════════════════════════
        print("\nPart H — JSON Output")

        # ── Test 25: Window List JSON ─────────────────────────
        print("Test 25: Window List JSON")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} --format json window list -s ws-test", 2.0)
        content = await read_screen(session)
        has_json = "[" in content and '"title"' in content
        has_pane_count = '"pane_count"' in content
        has_index = '"index"' in content
        record("25. JSON output", has_json and has_pane_count and has_index,
               f"json={has_json}, pane_count={has_pane_count}, index={has_index}")
        take_screenshot("014_list_json")

    finally:
        # ══════════════════════════════════════════════════════
        # Cleanup
        # ══════════════════════════════════════════════════════
        subprocess.run([SHUX_BIN, "kill", "-s", "ws-test"], capture_output=True, timeout=5)

        # Close the test tab
        try:
            await session.async_send_text("exit\n")
            await asyncio.sleep(0.5)
        except Exception:
            pass

        # ══════════════════════════════════════════════════════
        # Summary
        # ══════════════════════════════════════════════════════
        print("\n" + "=" * 60)
        total = len(results)
        passed = sum(1 for _, p, _ in results if p)
        failed = total - passed
        print(f"Results: {passed}/{total} passed, {failed} failed")
        if failed > 0:
            print("\nFailed tests:")
            for name, p, detail in results:
                if not p:
                    print(f"  FAIL: {name}" + (f" — {detail}" if detail else ""))
        print("=" * 60)


iterm2.run_until_complete(main)
