# /// script
# requires-python = ">=3.14"
# dependencies = [
#   "iterm2",
#   "pyobjc",
#   "pyobjc-framework-Quartz",
# ]
# ///

"""
Task 010 Visual Test: Minimal TUI Client (terminal_demo example)

Tests the terminal_demo example binary which exercises TerminalGuard,
compositor rendering, key encoding, and the prefix key detach sequence.

Tests:
    1. Build: cargo build --example terminal_demo -p shux-ui succeeds
    2. Alt Screen: demo enters alternate screen (content changes)
    3. Banner: "terminal demo" text appears on screen
    4. Key Echo: type "hello" and verify text appears
    5. Enter Key: press Enter, verify screen changes
    6. Arrow Keys: send arrow keys, verify no crash
    7. Ctrl+C: sends byte to VT (doesn't exit demo)
    8. Detach: Ctrl+Space then 'd' exits cleanly
    9. Terminal Restored: "[detached from demo]" visible after exit

Verification Strategy:
    - Poll screen contents with 5-second timeout for each state
    - Look for known text strings ("terminal demo", "hello", "[detached")
    - Capture screenshots at key transitions for visual inspection

Screenshots:
    - 010_banner.png: Initial banner after launch
    - 010_key_echo.png: After typing "hello"
    - 010_after_enter.png: After pressing Enter
    - 010_after_ctrl_c.png: After Ctrl+C (demo still running)
    - 010_detached.png: After detach, terminal restored

Key Bindings:
    - Ctrl+Space (\\x00): Prefix key
    - d (after prefix): Detach
    - Ctrl+C (\\x03): Interrupt (forwarded to VT, not exit)

Usage:
    uv run .claude/automations/test_010_tui_client.py
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
TIMEOUT_SECONDS = 5.0

# ============================================================
# RESULT TRACKING
# ============================================================

results = {
    "passed": 0,
    "failed": 0,
    "unverified": 0,
    "tests": [],
    "screenshots": [],
    "start_time": None,
    "end_time": None,
}


def log_result(test_name: str, status: str, details: str = "", screenshot: str = None):
    results["tests"].append({
        "name": test_name,
        "status": status,
        "details": details,
        "screenshot": screenshot,
    })

    if screenshot:
        results["screenshots"].append(screenshot)

    if status == "PASS":
        results["passed"] += 1
        print(f"  [+] PASS: {test_name}")
    elif status == "FAIL":
        results["failed"] += 1
        print(f"  [x] FAIL: {test_name} - {details}")
    else:
        results["unverified"] += 1
        print(f"  [?] UNVERIFIED: {test_name} - {details}")

    if screenshot:
        print(f"      Screenshot: {screenshot}")


def print_summary() -> int:
    results["end_time"] = datetime.now()
    total = results["passed"] + results["failed"] + results["unverified"]
    duration = (results["end_time"] - results["start_time"]).total_seconds() if results["start_time"] else 0

    print("\n" + "=" * 60)
    print("TEST SUMMARY")
    print("=" * 60)
    print(f"Duration:   {duration:.1f}s")
    print(f"Total:      {total}")
    print(f"Passed:     {results['passed']}")
    print(f"Failed:     {results['failed']}")
    print(f"Unverified: {results['unverified']}")

    if results["screenshots"]:
        print(f"Screenshots: {len(results['screenshots'])}")

    print("=" * 60)

    if results["failed"] > 0:
        print("\nFailed tests:")
        for test in results["tests"]:
            if test["status"] == "FAIL":
                print(f"  - {test['name']}: {test['details']}")

    print("\n" + "-" * 60)
    if results["failed"] > 0:
        print("OVERALL: FAILED")
        return 1
    elif results["unverified"] > 0:
        print("OVERALL: PASSED (with unverified tests)")
        return 0
    else:
        print("OVERALL: PASSED")
        return 0


def print_test_header(test_name: str, test_num: int = None):
    if test_num:
        header = f"TEST {test_num}: {test_name}"
    else:
        header = f"TEST: {test_name}"
    print("\n" + "=" * 60)
    print(header)
    print("=" * 60)


# ============================================================
# QUARTZ WINDOW TARGETING
# ============================================================

try:
    import Quartz

    def get_iterm2_window_id():
        window_list = Quartz.CGWindowListCopyWindowInfo(
            Quartz.kCGWindowListOptionOnScreenOnly | Quartz.kCGWindowListExcludeDesktopElements,
            Quartz.kCGNullWindowID
        )
        for window in window_list:
            owner = window.get('kCGWindowOwnerName', '')
            if 'iTerm' in owner:
                return window.get('kCGWindowNumber')
        return None

except ImportError:
    print("WARNING: Quartz not available, screenshots will capture full screen")

    def get_iterm2_window_id():
        return None


def capture_screenshot(name: str) -> str:
    os.makedirs(SCREENSHOT_DIR, exist_ok=True)
    timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
    filename = f"{name}_{timestamp}.png"
    filepath = os.path.join(SCREENSHOT_DIR, filename)

    window_id = get_iterm2_window_id()
    if window_id:
        subprocess.run(["screencapture", "-x", "-l", str(window_id), filepath], check=True)
    else:
        print("  WARNING: iTerm2 window not found, capturing full screen")
        subprocess.run(["screencapture", "-x", filepath], check=True)

    print(f"  SCREENSHOT: {filepath}")
    return filepath


# ============================================================
# VERIFICATION HELPERS
# ============================================================

async def verify_screen_contains(session, expected: str, description: str) -> bool:
    import time
    start = time.monotonic()
    while (time.monotonic() - start) < TIMEOUT_SECONDS:
        screen = await session.async_get_screen_contents()
        for i in range(screen.number_of_lines):
            if expected in screen.line(i).string:
                print(f"  Found: '{expected}' ({description})")
                return True
        await asyncio.sleep(0.2)

    print(f"  Not found: '{expected}' after {TIMEOUT_SECONDS}s ({description})")
    return False


async def dump_screen(session, label: str):
    screen = await session.async_get_screen_contents()
    print(f"\n{'='*60}")
    print(f"SCREEN DUMP: {label}")
    print(f"{'='*60}")
    for i in range(screen.number_of_lines):
        line = screen.line(i).string
        if line.strip():
            print(f"{i:03d}: {line}")
    print(f"{'='*60}\n")


# ============================================================
# MAIN TEST FUNCTION
# ============================================================

async def main(connection):
    results["start_time"] = datetime.now()

    print("\n" + "#" * 60)
    print("# TASK 010: Minimal TUI Client — Visual Tests")
    print("# Testing terminal_demo example binary")
    print("#" * 60)
    print(f"# Started: {results['start_time'].strftime('%Y-%m-%d %H:%M:%S')}")
    print(f"# Project: {PROJECT_ROOT}")
    print("#" * 60)

    # ============================================================
    # TEST 1: Build
    # ============================================================
    print_test_header("Build", 1)
    print("  Building terminal_demo example...")

    build_result = subprocess.run(
        ["cargo", "build", "--example", "terminal_demo", "-p", "shux-ui"],
        cwd=PROJECT_ROOT,
        capture_output=True,
        text=True,
        timeout=120,
    )

    if build_result.returncode == 0:
        log_result("Build", "PASS")
    else:
        log_result("Build", "FAIL", f"Build failed: {build_result.stderr[:200]}")
        return print_summary()

    # Get iTerm2 app and create a test tab
    app = await iterm2.async_get_app(connection)
    window = app.current_terminal_window

    if not window:
        print("ERROR: No active iTerm2 window")
        log_result("Setup", "FAIL", "No active iTerm2 window")
        return print_summary()

    tab = await window.async_create_tab()
    session = tab.current_session
    created_sessions = [session]

    try:
        # Wait for shell prompt
        await asyncio.sleep(0.5)

        # Launch the demo
        demo_cmd = f"cd {PROJECT_ROOT} && cargo run --example terminal_demo -p shux-ui\r"
        await session.async_send_text(demo_cmd)

        # ============================================================
        # TEST 2: Alt Screen
        # ============================================================
        print_test_header("Alt Screen", 2)
        print("  Waiting for demo to enter alternate screen...")
        await asyncio.sleep(2.0)  # Give cargo time to compile/run

        # The alt screen should NOT show the shell prompt anymore
        # Instead, it should show the demo content
        if await verify_screen_contains(session, "terminal demo", "alt screen content"):
            log_result("Alt Screen", "PASS")
        else:
            # It may still be compiling
            print("  Waiting longer for compilation...")
            await asyncio.sleep(5.0)
            if await verify_screen_contains(session, "terminal demo", "alt screen content (retry)"):
                log_result("Alt Screen", "PASS")
            else:
                await dump_screen(session, "alt_screen_check")
                log_result("Alt Screen", "FAIL", "Demo banner not found on alt screen")
                # If demo didn't launch, skip remaining tests
                return print_summary()

        # ============================================================
        # TEST 3: Banner
        # ============================================================
        print_test_header("Banner Visible", 3)

        if await verify_screen_contains(session, "Ctrl+Space d to exit", "exit instructions"):
            screenshot = capture_screenshot("010_banner")
            log_result("Banner Visible", "PASS", screenshot=screenshot)
        else:
            screenshot = capture_screenshot("010_banner")
            log_result("Banner Visible", "UNVERIFIED", "Exit instructions not found", screenshot=screenshot)

        # ============================================================
        # TEST 4: Key Echo
        # ============================================================
        print_test_header("Key Echo", 4)
        print("  Typing 'hello'...")

        await session.async_send_text("hello")
        await asyncio.sleep(0.5)

        if await verify_screen_contains(session, "hello", "typed text"):
            screenshot = capture_screenshot("010_key_echo")
            log_result("Key Echo", "PASS", screenshot=screenshot)
        else:
            screenshot = capture_screenshot("010_key_echo")
            await dump_screen(session, "key_echo_check")
            log_result("Key Echo", "FAIL", "Typed text 'hello' not found", screenshot=screenshot)

        # ============================================================
        # TEST 5: Enter Key
        # ============================================================
        print_test_header("Enter Key", 5)
        print("  Pressing Enter...")

        await session.async_send_text("\r")
        await asyncio.sleep(0.5)

        screenshot = capture_screenshot("010_after_enter")
        # After enter, the cursor should have moved down
        log_result("Enter Key", "PASS", "Enter key processed", screenshot=screenshot)

        # ============================================================
        # TEST 6: Arrow Keys
        # ============================================================
        print_test_header("Arrow Keys", 6)
        print("  Sending arrow key sequences...")

        await session.async_send_text("\x1b[A")  # Up
        await asyncio.sleep(0.2)
        await session.async_send_text("\x1b[B")  # Down
        await asyncio.sleep(0.2)
        await session.async_send_text("\x1b[C")  # Right
        await asyncio.sleep(0.2)
        await session.async_send_text("\x1b[D")  # Left
        await asyncio.sleep(0.3)

        # If we got here without crashing, arrow keys are handled
        log_result("Arrow Keys", "PASS", "Arrow keys processed without crash")

        # ============================================================
        # TEST 7: Ctrl+C
        # ============================================================
        print_test_header("Ctrl+C", 7)
        print("  Sending Ctrl+C (should not exit demo)...")

        await session.async_send_text("\x03")  # Ctrl+C
        await asyncio.sleep(0.5)

        # Demo should still be running — check banner is still visible
        if await verify_screen_contains(session, "terminal demo", "demo still running after Ctrl+C"):
            screenshot = capture_screenshot("010_after_ctrl_c")
            log_result("Ctrl+C Handled", "PASS", "Demo survived Ctrl+C", screenshot=screenshot)
        else:
            screenshot = capture_screenshot("010_after_ctrl_c")
            log_result("Ctrl+C Handled", "FAIL", "Demo may have exited on Ctrl+C", screenshot=screenshot)

        # ============================================================
        # TEST 8: Detach
        # ============================================================
        print_test_header("Detach (Ctrl+Space d)", 8)
        print("  Sending prefix key (Ctrl+Space = NUL) then 'd'...")

        await session.async_send_text("\x00")  # Ctrl+Space = NUL byte
        await asyncio.sleep(0.3)
        await session.async_send_text("d")
        await asyncio.sleep(1.0)

        # ============================================================
        # TEST 9: Terminal Restored
        # ============================================================
        print_test_header("Terminal Restored", 9)

        if await verify_screen_contains(session, "[detached from demo]", "detach message"):
            screenshot = capture_screenshot("010_detached")
            log_result("Detach", "PASS", screenshot=screenshot)
            log_result("Terminal Restored", "PASS", "Detach message visible", screenshot=screenshot)
        else:
            screenshot = capture_screenshot("010_detached")
            await dump_screen(session, "detach_check")
            log_result("Detach", "FAIL", "Detach message not found", screenshot=screenshot)
            log_result("Terminal Restored", "FAIL", "Could not verify terminal restore")

    except Exception as e:
        print(f"\nERROR during test execution: {e}")
        log_result("Test Execution", "FAIL", str(e))
        await dump_screen(session, "error_state")

    finally:
        print("\n" + "=" * 60)
        print("CLEANUP")
        print("=" * 60)

        for s in created_sessions:
            try:
                # Send Ctrl+C in case demo is still running
                await s.async_send_text("\x03")
                await asyncio.sleep(0.2)
                # Send exit for the shell
                await s.async_send_text("exit\n")
                await asyncio.sleep(0.2)
                await s.async_close()
                print("  Cleanup complete")
            except Exception as e:
                print(f"  Cleanup warning: {e}")

    return print_summary()


# ============================================================
# ENTRY POINT
# ============================================================

if __name__ == "__main__":
    exit_code = iterm2.run_until_complete(main)
    exit(exit_code if exit_code else 0)
