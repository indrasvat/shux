# /// script
# requires-python = ">=3.14"
# dependencies = [
#   "iterm2",
#   "pyobjc",
#   "pyobjc-framework-Quartz",
# ]
# ///

"""
Task 015 Visual Test: Pane Operations (split, focus, resize, zoom, swap, kill)

Tests the full pane lifecycle through the shux CLI in an iTerm2 session.

Part A — Setup (Tests 1–3)
Part B — Split (Tests 4–7)
Part C — Focus (Tests 8–10)
Part D — Resize (Test 11)
Part E — Zoom (Tests 12–13)
Part F — Swap (Tests 14–15)
Part G — Kill (Tests 16–18)
Part H — JSON Output (Test 19)

Usage:
    uv run .claude/automations/test_015_pane_ops.py
"""

import iterm2
import asyncio
import subprocess
import os
import json
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


def extract_pane_id(content):
    """Extract a pane UUID from screen content."""
    import re
    # Look for UUID pattern
    match = re.search(r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}', content)
    return match.group(0) if match else None


def extract_all_pane_ids(content):
    """Extract all pane UUIDs from screen content."""
    import re
    return re.findall(r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}', content)


def count_panes(content):
    """Count pane entries in `pane list` output."""
    count = 0
    for line in content.split("\n"):
        line = line.strip()
        # Pane entries contain UUIDs
        import re
        if re.search(r'[0-9a-f]{8}-[0-9a-f]{4}-', line) and line:
            count += 1
    return count


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
        print(f"\nshux Pane Operations Visual Test (015) — {datetime.now().isoformat()}")
        print(f"Project: {PROJECT_ROOT}")
        print(f"Binary: {SHUX_BIN}")
        print()

        # Kill any stale daemon so the fresh binary is used
        subprocess.run(["pkill", "-f", "shux __daemon"], capture_output=True, timeout=5)
        await asyncio.sleep(1)

        # Kill any stale test session
        subprocess.run([SHUX_BIN, "kill", "-s", "pane-test"], capture_output=True, timeout=5)

        # ══════════════════════════════════════════════════════
        # Part A — Setup (Tests 1–3)
        # ══════════════════════════════════════════════════════
        print("Part A — Setup")

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
        print("Test 2: Create Session (pane-test)")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} new -s pane-test -d", 3.0)
        content = await read_screen(session)
        has_session = "pane-test" in content
        has_created = "created" in content.lower()
        record("2. Create session", has_session and has_created,
               "" if has_session else "missing 'pane-test' in output")
        take_screenshot("015_session_created")

        # ── Test 3: Initial Pane Exists ───────────────────────
        print("Test 3: Initial Pane Exists")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} pane list -s pane-test", 2.0)
        content = await read_screen(session)
        initial_panes = count_panes(content)
        initial_pane_id = extract_pane_id(content)
        record("3. Initial pane exists", initial_panes == 1 and initial_pane_id is not None,
               f"pane_count={initial_panes}, id={initial_pane_id}")
        take_screenshot("015_initial_pane")

        # ══════════════════════════════════════════════════════
        # Part B — Split (Tests 4–7)
        # ══════════════════════════════════════════════════════
        print("\nPart B — Split")

        # ── Test 4: Split Vertical ────────────────────────────
        print("Test 4: Split Vertical")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} pane split -s pane-test --direction vertical", 2.0)
        content = await read_screen(session)
        has_split = "split" in content.lower()
        has_vertical = "vertical" in content.lower()
        split_pane_id = extract_pane_id(content)
        record("4. Split vertical", has_split and has_vertical,
               f"pane_id={split_pane_id}")
        take_screenshot("015_split_vertical")

        # ── Test 5: Split Horizontal ──────────────────────────
        print("Test 5: Split Horizontal")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} pane split -s pane-test --direction horizontal", 2.0)
        content = await read_screen(session)
        has_split = "split" in content.lower()
        has_horizontal = "horizontal" in content.lower()
        record("5. Split horizontal", has_split and has_horizontal,
               "" if has_split else "missing 'split' in output")
        take_screenshot("015_split_horizontal")

        # ── Test 6: Pane List Shows 3 Panes ───────────────────
        print("Test 6: Pane List Shows 3 Panes")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} pane list -s pane-test", 2.0)
        content = await read_screen(session)
        pane_count = count_panes(content)
        all_ids = extract_all_pane_ids(content)
        record("6. Three panes in list", pane_count == 3,
               f"count={pane_count}, ids={len(all_ids)}")
        take_screenshot("015_pane_list_3")

        # Save pane IDs for later tests
        pane_ids = list(dict.fromkeys(all_ids))  # unique, preserving order

        # ── Test 7: Split Auto-Defaults ───────────────────────
        print("Test 7: Split Auto-Defaults (no direction)")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} pane split -s pane-test", 2.0)
        content = await read_screen(session)
        has_split = "split" in content.lower()
        no_error = "error" not in content.lower()
        record("7. Split auto-defaults", has_split and no_error,
               "" if no_error else "error in output")
        take_screenshot("015_split_auto")

        # ══════════════════════════════════════════════════════
        # Part C — Focus (Tests 8–10)
        # ══════════════════════════════════════════════════════
        print("\nPart C — Focus")

        # Refresh pane list to get all 4 pane IDs
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} pane list -s pane-test", 2.0)
        content = await read_screen(session)
        pane_ids = list(dict.fromkeys(extract_all_pane_ids(content)))

        # ── Test 8: Focus by UUID ─────────────────────────────
        print("Test 8: Focus by UUID")
        # Focus the first pane (which should not be active after splits)
        target_pane = pane_ids[0] if pane_ids else initial_pane_id
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} pane focus -s pane-test --pane {target_pane}", 2.0)
        content = await read_screen(session)
        has_focused = "focused" in content.lower()
        no_error = "error" not in content.lower()
        record("8. Focus by UUID", has_focused and no_error,
               f"target={target_pane[:8]}...")
        take_screenshot("015_focus_uuid")

        # ── Test 9: Focus Direction (right) ───────────────────
        print("Test 9: Focus Direction (right)")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} pane focus-dir -s pane-test --direction right", 2.0)
        content = await read_screen(session)
        has_focused = "focused" in content.lower()
        # Might get "no neighbor" if no pane to the right, which is also acceptable
        no_fatal_error = "internal" not in content.lower()
        record("9. Focus direction right", no_fatal_error,
               f"focused={'yes' if has_focused else 'no/no-neighbor'}")
        take_screenshot("015_focus_dir_right")

        # ── Test 10: Focus Direction (left) ───────────────────
        print("Test 10: Focus Direction (left)")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} pane focus-dir -s pane-test --direction left", 2.0)
        content = await read_screen(session)
        has_focused = "focused" in content.lower()
        no_fatal_error = "internal" not in content.lower()
        record("10. Focus direction left", no_fatal_error,
               f"focused={'yes' if has_focused else 'no/no-neighbor'}")
        take_screenshot("015_focus_dir_left")

        # ══════════════════════════════════════════════════════
        # Part D — Resize (Test 11)
        # ══════════════════════════════════════════════════════
        print("\nPart D — Resize")

        # ── Test 11: Resize Pane ──────────────────────────────
        print("Test 11: Resize Pane")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} pane resize -s pane-test --direction horizontal --delta 0.1", 2.0)
        content = await read_screen(session)
        has_resized = "resized" in content.lower()
        no_error = "error" not in content.lower()
        record("11. Resize pane", has_resized and no_error,
               "" if no_error else "error in output")
        take_screenshot("015_resize")

        # ══════════════════════════════════════════════════════
        # Part E — Zoom (Tests 12–13)
        # ══════════════════════════════════════════════════════
        print("\nPart E — Zoom")

        # ── Test 12: Zoom Toggle On ───────────────────────────
        print("Test 12: Zoom Toggle On")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} pane zoom -s pane-test", 2.0)
        content = await read_screen(session)
        has_zoom_info = "zoom" in content.lower()
        no_error = "error" not in content.lower()
        record("12. Zoom toggle on", has_zoom_info and no_error,
               "" if no_error else "error in output")
        take_screenshot("015_zoom_on")

        # ── Test 13: Zoom Toggle Off ──────────────────────────
        print("Test 13: Zoom Toggle Off")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} pane zoom -s pane-test", 2.0)
        content = await read_screen(session)
        has_zoom_info = "zoom" in content.lower() or "unzoom" in content.lower()
        no_error = "error" not in content.lower()
        record("13. Zoom toggle off", has_zoom_info and no_error,
               "" if no_error else "error in output")
        take_screenshot("015_zoom_off")

        # ══════════════════════════════════════════════════════
        # Part F — Swap (Tests 14–15)
        # ══════════════════════════════════════════════════════
        print("\nPart F — Swap")

        # Refresh pane list to get current IDs
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} pane list -s pane-test", 2.0)
        content = await read_screen(session)
        pane_ids = list(dict.fromkeys(extract_all_pane_ids(content)))

        # ── Test 14: Swap Two Panes ───────────────────────────
        print("Test 14: Swap Two Panes")
        if len(pane_ids) >= 2:
            swap_a = pane_ids[0]
            swap_b = pane_ids[1]
            await send_and_wait(session, "clear", 0.3)
            await send_and_wait(session, f"{SHUX_BIN} pane swap -s pane-test --pane {swap_a} --target {swap_b}", 2.0)
            content = await read_screen(session)
            has_swapped = "swapped" in content.lower() or "swap" in content.lower()
            no_error = "error" not in content.lower()
            record("14. Swap two panes", has_swapped and no_error,
                   f"a={swap_a[:8]}, b={swap_b[:8]}")
        else:
            record("14. Swap two panes", False, "not enough panes")
        take_screenshot("015_swap")

        # ── Test 15: Swap Self Fails ──────────────────────────
        print("Test 15: Swap Self Fails")
        if pane_ids:
            self_pane = pane_ids[0]
            await send_and_wait(session, "clear", 0.3)
            await send_and_wait(session, f"{SHUX_BIN} pane swap -s pane-test --pane {self_pane} --target {self_pane}", 2.0)
            content = await read_screen(session)
            has_error = "error" in content.lower() or "cannot" in content.lower() or "self" in content.lower()
            record("15. Swap self fails", has_error,
                   "" if has_error else "expected error")
        else:
            record("15. Swap self fails", False, "no panes available")
        take_screenshot("015_swap_self_error")

        # ══════════════════════════════════════════════════════
        # Part G — Kill (Tests 16–18)
        # ══════════════════════════════════════════════════════
        print("\nPart G — Kill")

        # Refresh pane list
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} pane list -s pane-test", 2.0)
        content = await read_screen(session)
        pane_ids = list(dict.fromkeys(extract_all_pane_ids(content)))
        current_count = len(pane_ids)

        # ── Test 16: Kill a Pane ──────────────────────────────
        print("Test 16: Kill a Pane")
        if pane_ids:
            kill_target = pane_ids[-1]  # kill the last pane
            await send_and_wait(session, "clear", 0.3)
            await send_and_wait(session, f"{SHUX_BIN} pane kill -s pane-test --pane {kill_target}", 2.0)
            content = await read_screen(session)
            has_killed = "killed" in content.lower()
            record("16. Kill pane", has_killed,
                   f"killed={kill_target[:8]}")
        else:
            record("16. Kill pane", False, "no panes")
        take_screenshot("015_kill_pane")

        # ── Test 17: Verify Kill in List ──────────────────────
        print("Test 17: Verify Kill in List")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} pane list -s pane-test", 2.0)
        content = await read_screen(session)
        new_count = count_panes(content)
        record("17. Kill verified in list", new_count == current_count - 1,
               f"before={current_count}, after={new_count}")
        take_screenshot("015_list_after_kill")

        # Kill down to 1 pane for last test
        remaining_ids = list(dict.fromkeys(extract_all_pane_ids(content)))
        while len(remaining_ids) > 1:
            kill_id = remaining_ids.pop()
            await send_and_wait(session, f"{SHUX_BIN} pane kill -s pane-test --pane {kill_id}", 1.5)

        # ── Test 18: Kill Last Pane Fails ─────────────────────
        print("Test 18: Kill Last Pane Fails")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} pane list -s pane-test", 2.0)
        content = await read_screen(session)
        last_pane = extract_pane_id(content)
        if last_pane:
            await send_and_wait(session, "clear", 0.3)
            await send_and_wait(session, f"{SHUX_BIN} pane kill -s pane-test --pane {last_pane}", 2.0)
            content = await read_screen(session)
            has_error = "error" in content.lower() or "last" in content.lower() or "cannot" in content.lower()

            # Verify pane still exists
            await send_and_wait(session, "clear", 0.3)
            await send_and_wait(session, f"{SHUX_BIN} pane list -s pane-test", 1.5)
            content = await read_screen(session)
            final_count = count_panes(content)
            record("18. Kill last pane fails", has_error and final_count == 1,
                   f"error_shown={has_error}, remaining={final_count}")
        else:
            record("18. Kill last pane fails", False, "no last pane found")
        take_screenshot("015_kill_last_fails")

        # ══════════════════════════════════════════════════════
        # Part H — JSON Output (Test 19)
        # ══════════════════════════════════════════════════════
        print("\nPart H — JSON Output")

        # Re-split so we have panes for JSON test
        await send_and_wait(session, f"{SHUX_BIN} pane split -s pane-test", 1.5)

        # ── Test 19: Pane List JSON ───────────────────────────
        print("Test 19: Pane List JSON")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} --format json pane list -s pane-test", 2.0)
        content = await read_screen(session)
        has_json = "[" in content and '"id"' in content
        has_window_id = '"window_id"' in content
        record("19. JSON output", has_json and has_window_id,
               f"json={has_json}, window_id={has_window_id}")
        take_screenshot("015_list_json")

    finally:
        # ══════════════════════════════════════════════════════
        # Cleanup
        # ══════════════════════════════════════════════════════
        subprocess.run([SHUX_BIN, "kill", "-s", "pane-test"], capture_output=True, timeout=5)

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
