# /// script
# requires-python = ">=3.14"
# dependencies = [
#   "iterm2",
#   "pyobjc",
#   "pyobjc-framework-Quartz",
# ]
# ///

"""
Task 011 Visual Test: CLI Styling and ASCII Banner

Tests the shux CLI binary for correct colored output — ASCII art banner
with cyan→blue→indigo gradient, clap-styled help (cyan headers, green
commands, yellow placeholders), and styled version/subcommand output.

Tests:
    1. Build: `make build` succeeds
    2. Help Banner: `shux --help` displays the ASCII art "shux" banner
    3. Help Colors: Headers (Usage, Commands, Options) are present
    4. Help Commands: All subcommands listed (new, attach, ls, kill, api, version)
    5. Version Styled: `shux version` shows "shux" and "daemon not running"
    6. Subcommand Help: `shux new --help` shows styled subcommand help
    7. Short Help: `shux -h` also shows banner and commands

Verification Strategy:
    - Run each CLI command in an iTerm2 session
    - Poll screen contents for known text strings
    - Capture screenshots at each step for visual inspection of colors
    - Color verification is visual (screenshots) since screen content
      API strips ANSI codes

Screenshots:
    - 011_help_banner.png: Full --help output with ASCII banner
    - 011_version.png: Styled version output
    - 011_new_help.png: Subcommand help for `shux new`
    - 011_short_help.png: Short -h help output

Usage:
    uv run .claude/automations/test_011_cli_styling.py
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

async def get_all_screen_text(session) -> str:
    """Get all non-empty lines from the screen as a single string."""
    screen = await session.async_get_screen_contents()
    lines = []
    for i in range(screen.number_of_lines):
        line = screen.line(i).string
        if line.strip():
            lines.append(line)
    return "\n".join(lines)


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


async def verify_screen_contains_all(session, expected_list: list[str], description: str) -> tuple[bool, list[str]]:
    """Check that ALL expected strings appear on screen. Returns (all_found, missing_list)."""
    import time
    start = time.monotonic()
    while (time.monotonic() - start) < TIMEOUT_SECONDS:
        text = await get_all_screen_text(session)
        missing = [e for e in expected_list if e not in text]
        if not missing:
            print(f"  Found all {len(expected_list)} strings ({description})")
            return True, []
        await asyncio.sleep(0.2)

    print(f"  Missing {len(missing)} of {len(expected_list)} strings ({description}): {missing}")
    return False, missing


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
    print("# TASK 011: CLI Styling — Visual Tests")
    print("# Testing shux CLI colored output and ASCII banner")
    print("#" * 60)
    print(f"# Started: {results['start_time'].strftime('%Y-%m-%d %H:%M:%S')}")
    print(f"# Project: {PROJECT_ROOT}")
    print(f"# Binary:  {SHUX_BIN}")
    print("#" * 60)

    # ============================================================
    # TEST 1: Build
    # ============================================================
    print_test_header("Build", 1)
    print("  Building shux binary...")

    build_result = subprocess.run(
        ["make", "build"],
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

    try:
        # Wait for shell prompt
        await asyncio.sleep(0.5)

        # ============================================================
        # TEST 2: Help Banner
        # ============================================================
        print_test_header("Help Banner", 2)
        print("  Running: shux --help")

        await session.async_send_text(f"{SHUX_BIN} --help\r")
        await asyncio.sleep(0.5)

        # The ASCII art contains these figlet fragments
        banner_fragments = ["|___/", r"\__,_", "/ __||"]
        found_banner, missing = await verify_screen_contains_all(
            session, banner_fragments, "ASCII art banner"
        )

        if found_banner:
            log_result("Help Banner", "PASS", screenshot=capture_screenshot("011_help_banner"))
        else:
            await dump_screen(session, "help --help output")
            log_result("Help Banner", "FAIL", f"Missing banner fragments: {missing}",
                       screenshot=capture_screenshot("011_help_banner"))

        # ============================================================
        # TEST 3: Help Headers
        # ============================================================
        print_test_header("Help Headers", 3)

        # Check that clap headers and structural elements are present
        # (colors are verified visually via screenshot)
        headers = ["Usage:", "Commands:", "Options:"]
        found_headers, missing = await verify_screen_contains_all(
            session, headers, "clap section headers"
        )

        if found_headers:
            log_result("Help Headers", "PASS")
        else:
            log_result("Help Headers", "FAIL", f"Missing headers: {missing}")

        # ============================================================
        # TEST 4: Help Commands
        # ============================================================
        print_test_header("Help Commands", 4)

        commands = ["new", "attach", "ls", "kill", "api", "version"]
        found_cmds, missing = await verify_screen_contains_all(
            session, commands, "subcommand names"
        )

        if found_cmds:
            log_result("Help Commands", "PASS")
        else:
            log_result("Help Commands", "FAIL", f"Missing commands: {missing}")

        # ============================================================
        # TEST 5: Version Styled
        # ============================================================
        print_test_header("Version Styled", 5)
        print("  Running: shux version")

        # Clear screen first
        await session.async_send_text("clear\r")
        await asyncio.sleep(0.3)

        await session.async_send_text(f"{SHUX_BIN} version\r")
        await asyncio.sleep(0.5)

        version_strings = ["shux", "daemon not running"]
        found_version, missing = await verify_screen_contains_all(
            session, version_strings, "styled version output"
        )

        if found_version:
            log_result("Version Styled", "PASS", screenshot=capture_screenshot("011_version"))
        else:
            await dump_screen(session, "version output")
            log_result("Version Styled", "FAIL", f"Missing: {missing}",
                       screenshot=capture_screenshot("011_version"))

        # ============================================================
        # TEST 6: Subcommand Help
        # ============================================================
        print_test_header("Subcommand Help", 6)
        print("  Running: shux new --help")

        await session.async_send_text("clear\r")
        await asyncio.sleep(0.3)

        await session.async_send_text(f"{SHUX_BIN} new --help\r")
        await asyncio.sleep(0.5)

        new_help_strings = ["Create a new session", "--session", "--ensure", "--detached"]
        found_new, missing = await verify_screen_contains_all(
            session, new_help_strings, "new subcommand help"
        )

        if found_new:
            log_result("Subcommand Help", "PASS", screenshot=capture_screenshot("011_new_help"))
        else:
            await dump_screen(session, "new --help output")
            log_result("Subcommand Help", "FAIL", f"Missing: {missing}",
                       screenshot=capture_screenshot("011_new_help"))

        # ============================================================
        # TEST 7: Short Help
        # ============================================================
        print_test_header("Short Help", 7)
        print("  Running: shux -h")

        await session.async_send_text("clear\r")
        await asyncio.sleep(0.3)

        await session.async_send_text(f"{SHUX_BIN} -h\r")
        await asyncio.sleep(0.5)

        # Short help should also have the banner and basic structure
        short_help_strings = ["|___/", "Usage:", "Commands:"]
        found_short, missing = await verify_screen_contains_all(
            session, short_help_strings, "short help banner + structure"
        )

        if found_short:
            log_result("Short Help", "PASS", screenshot=capture_screenshot("011_short_help"))
        else:
            await dump_screen(session, "-h output")
            log_result("Short Help", "FAIL", f"Missing: {missing}",
                       screenshot=capture_screenshot("011_short_help"))

    except Exception as e:
        print(f"\nERROR: Unexpected exception: {e}")
        import traceback
        traceback.print_exc()
        log_result("Unexpected Error", "FAIL", str(e))

    finally:
        # Cleanup: close the test tab
        await asyncio.sleep(0.2)
        await session.async_send_text("exit\r")
        await asyncio.sleep(0.3)
        try:
            await session.async_close()
        except Exception:
            pass

    return print_summary()


iterm2.run_until_complete(main)
