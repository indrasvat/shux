# /// script
# requires-python = ">=3.14"
# dependencies = [
#   "iterm2",
#   "pyobjc",
#   "pyobjc-framework-Quartz",
# ]
# ///

"""
Task 013 Visual Test: Session CRUD (API + CLI)

Tests the full session lifecycle: create, list, rename, kill, ensure
through the shux CLI in an iTerm2 session, verifying styled output.

Part A — Session Creation & Styled Output (Tests 1–5)
Part B — Session Listing & Formatting (Tests 6–9)
Part C — Ensure (Idempotent Create) (Tests 10–12)
Part D — Session Rename (Tests 13–14)
Part E — Session Kill & Cleanup (Tests 15–17)
Part F — Error Handling & Validation (Tests 18–20)

Usage:
    uv run .claude/automations/test_013_session_crud.py
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


# Session names used by tests — all cleaned up in finally block
TEST_SESSIONS = ["alpha", "beta", "beta-renamed", "gamma", "delta", "epsilon"]


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
        print(f"\nshux Session CRUD Visual Test (013) — {datetime.now().isoformat()}")
        print(f"Project: {PROJECT_ROOT}")
        print(f"Binary: {SHUX_BIN}")
        print()

        # Kill any stale test sessions first
        for name in TEST_SESSIONS:
            subprocess.run([SHUX_BIN, "kill", "-s", name], capture_output=True, timeout=5)
        # Also kill auto-named sessions
        for i in range(5):
            subprocess.run([SHUX_BIN, "kill", "-s", f"session-{i}"], capture_output=True, timeout=5)

        # ══════════════════════════════════════════════════════
        # Part A — Session Creation & Styled Output (Tests 1–5)
        # ══════════════════════════════════════════════════════
        print("Part A — Session Creation & Styled Output")

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

        # Change to project dir in iTerm session
        await send_and_wait(session, f"cd {PROJECT_ROOT}", 0.5)

        # ── Test 2: Create Detached (alpha) ───────────────────
        print("Test 2: Create Detached (alpha)")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} new -s alpha -d", 3.0)
        content = await read_screen(session)
        has_alpha = "alpha" in content.lower()
        has_created = "created" in content.lower()
        record("2. Create alpha", has_alpha and has_created,
               "" if has_alpha else "missing 'alpha' in output")
        take_screenshot("013_create_alpha")

        # ── Test 3: Create Second (beta) ──────────────────────
        print("Test 3: Create Second (beta)")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} new -s beta -d", 2.0)
        content = await read_screen(session)
        has_beta = "beta" in content.lower()
        record("3. Create beta", has_beta, "")
        take_screenshot("013_create_beta")

        # ── Test 4: Create Third (gamma) ──────────────────────
        print("Test 4: Create Third (gamma)")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} new -s gamma -d", 2.0)
        content = await read_screen(session)
        has_gamma = "gamma" in content.lower()
        record("4. Create gamma", has_gamma, "")
        take_screenshot("013_create_gamma")

        # ── Test 5: Create with Auto-name ─────────────────────
        print("Test 5: Create with Auto-name")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} new -d", 2.0)
        content = await read_screen(session)
        has_auto = "session-" in content.lower() or "created" in content.lower()
        has_error = "error" in content.lower()
        record("5. Create auto-name", has_auto and not has_error,
               "got error" if has_error else "")
        take_screenshot("013_create_autoname")

        # ══════════════════════════════════════════════════════
        # Part B — Session Listing & Formatting (Tests 6–9)
        # ══════════════════════════════════════════════════════
        print("\nPart B — Session Listing & Formatting")

        # ── Test 6: List All Sessions ─────────────────────────
        print("Test 6: List All Sessions")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} ls", 2.0)
        content = await read_screen(session)
        has_all = "alpha" in content and "beta" in content and "gamma" in content
        record("6. List all sessions", has_all,
               "" if has_all else "missing one or more sessions in ls output")
        take_screenshot("013_ls_all")

        # ── Test 7: List Ordering ─────────────────────────────
        print("Test 7: List Ordering (creation order)")
        lines = content.split("\n")
        alpha_line = -1
        beta_line = -1
        gamma_line = -1
        for i, l in enumerate(lines):
            if "alpha" in l and alpha_line == -1:
                alpha_line = i
            if "beta" in l and beta_line == -1:
                beta_line = i
            if "gamma" in l and gamma_line == -1:
                gamma_line = i
        ordered = (alpha_line >= 0 and beta_line >= 0 and gamma_line >= 0
                   and alpha_line < beta_line < gamma_line)
        record("7. List ordering", ordered,
               f"lines: alpha={alpha_line} beta={beta_line} gamma={gamma_line}")

        # ── Test 8: List JSON Format ──────────────────────────
        print("Test 8: List JSON Format")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} ls --format json", 2.0)
        content = await read_screen(session)
        has_json = "[" in content and '"name"' in content
        has_names = "alpha" in content and "beta" in content and "gamma" in content
        record("8. List JSON format", has_json and has_names,
               "" if has_json else "JSON markers not found")
        take_screenshot("013_ls_json")

        # ── Test 9: List Shows Window Count ───────────────────
        print("Test 9: List Shows Window Count")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} ls", 2.0)
        content = await read_screen(session)
        has_window_info = "window" in content.lower() or "1 window" in content.lower()
        record("9. List shows window info", has_window_info, "")

        # ══════════════════════════════════════════════════════
        # Part C — Ensure (Idempotent Create) (Tests 10–12)
        # ══════════════════════════════════════════════════════
        print("\nPart C — Ensure (Idempotent Create)")

        # ── Test 10: Ensure Existing (alpha) ──────────────────
        print("Test 10: Ensure Existing (alpha)")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} new -s alpha --ensure -d", 2.0)
        content = await read_screen(session)
        has_alpha = "alpha" in content.lower()
        no_error = "error" not in content.lower()
        record("10. Ensure existing", has_alpha and no_error,
               "got error" if not no_error else "")
        take_screenshot("013_ensure_existing")

        # ── Test 11: Ensure New (delta) ───────────────────────
        print("Test 11: Ensure New (delta)")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} new -s delta --ensure -d", 2.0)
        content = await read_screen(session)
        has_delta = "delta" in content.lower()
        has_created_or_ensured = "created" in content.lower() or "ensured" in content.lower()
        record("11. Ensure new (delta)", has_delta and has_created_or_ensured, "")
        take_screenshot("013_ensure_new")

        # ── Test 12: Ensure Triple Idempotency (epsilon) ──────
        print("Test 12: Ensure Triple Idempotency (epsilon)")
        await send_and_wait(session, f"{SHUX_BIN} new -s epsilon --ensure -d", 2.0)
        await send_and_wait(session, f"{SHUX_BIN} new -s epsilon --ensure -d", 2.0)
        await send_and_wait(session, f"{SHUX_BIN} new -s epsilon --ensure -d", 2.0)
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} ls", 2.0)
        content = await read_screen(session)
        # Count occurrences of "epsilon" — should be exactly 1 session entry
        epsilon_count = content.lower().count("epsilon")
        record("12. Ensure triple idempotency", epsilon_count == 1,
               f"epsilon appears {epsilon_count} times (expected 1)")
        take_screenshot("013_ensure_triple")

        # ══════════════════════════════════════════════════════
        # Part D — Session Rename (Tests 13–14)
        # ══════════════════════════════════════════════════════
        print("\nPart D — Session Rename")

        # ── Test 13: Rename Session (beta → beta-renamed) ─────
        print("Test 13: Rename Session (beta → beta-renamed)")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} rename -s beta -n beta-renamed", 2.0)
        content = await read_screen(session)
        has_renamed = "renamed" in content.lower()
        record("13a. Rename output", has_renamed, "")
        # Verify with ls
        await send_and_wait(session, f"{SHUX_BIN} ls", 2.0)
        content = await read_screen(session)
        has_new_name = "beta-renamed" in content
        # Check that standalone "beta" (without "-renamed") is gone
        # Split to check individual lines for "beta" that isn't "beta-renamed"
        lines = content.split("\n")
        standalone_beta = any(
            "beta" in line and "beta-renamed" not in line
            for line in lines
            if "beta" in line
        )
        record("13b. Rename verified in ls", has_new_name and not standalone_beta,
               "standalone beta still visible" if standalone_beta else "")
        take_screenshot("013_rename")

        # ── Test 14: Rename Conflict (gamma → alpha) ──────────
        print("Test 14: Rename Conflict (gamma → alpha)")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} rename -s gamma -n alpha", 2.0)
        content = await read_screen(session)
        has_error = ("error" in content.lower() or "conflict" in content.lower()
                     or "exists" in content.lower())
        record("14. Rename conflict", has_error,
               "" if has_error else "expected error for name conflict")
        take_screenshot("013_rename_conflict")

        # ══════════════════════════════════════════════════════
        # Part E — Session Kill & Cleanup (Tests 15–17)
        # ══════════════════════════════════════════════════════
        print("\nPart E — Session Kill & Cleanup")

        # ── Test 15: Kill Session (gamma) ─────────────────────
        print("Test 15: Kill Session (gamma)")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} kill -s gamma", 2.0)
        content = await read_screen(session)
        has_killed = "killed" in content.lower()
        has_gamma = "gamma" in content.lower()
        record("15. Kill gamma", has_killed and has_gamma, "")
        take_screenshot("013_kill_gamma")

        # ── Test 16: Verify Kill Removed ──────────────────────
        print("Test 16: Verify Kill Removed")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} ls", 2.0)
        content = await read_screen(session)
        gamma_gone = "gamma" not in content
        has_alpha = "alpha" in content
        has_beta_renamed = "beta-renamed" in content
        record("16. Verify gamma removed", gamma_gone and has_alpha and has_beta_renamed,
               "gamma still visible" if not gamma_gone else "")
        take_screenshot("013_ls_after_kill")

        # ── Test 17: Kill Nonexistent ─────────────────────────
        print("Test 17: Kill Nonexistent")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} kill -s nonexistent-session", 2.0)
        content = await read_screen(session)
        has_error = ("error" in content.lower() or "not found" in content.lower())
        record("17. Kill nonexistent", has_error,
               "" if has_error else "expected error for nonexistent session")
        take_screenshot("013_kill_nonexistent")

        # ══════════════════════════════════════════════════════
        # Part F — Error Handling & Validation (Tests 18–20)
        # ══════════════════════════════════════════════════════
        print("\nPart F — Error Handling & Validation")

        # ── Test 18: Create Empty Name ────────────────────────
        print("Test 18: Create Empty Name")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f'{SHUX_BIN} new -s "" -d', 2.0)
        content = await read_screen(session)
        has_error = ("error" in content.lower() or "cannot be empty" in content.lower()
                     or "required" in content.lower())
        record("18. Create empty name", has_error,
               "" if has_error else "expected error for empty name")
        take_screenshot("013_err_empty_name")

        # ── Test 19: Create Invalid Name (Spaces) ────────────
        print("Test 19: Create Invalid Name (spaces)")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f'{SHUX_BIN} new -s "bad name" -d', 2.0)
        content = await read_screen(session)
        has_error = ("error" in content.lower() or "invalid" in content.lower())
        record("19. Create invalid name (spaces)", has_error,
               "" if has_error else "expected error for invalid name")
        take_screenshot("013_err_invalid_name")

        # ── Test 20: Create Duplicate Name ────────────────────
        print("Test 20: Create Duplicate Name (alpha)")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} new -s alpha -d", 2.0)
        content = await read_screen(session)
        has_error = ("error" in content.lower() or "exists" in content.lower()
                     or "conflict" in content.lower())
        record("20. Create duplicate name", has_error,
               "" if has_error else "expected error for duplicate name")
        take_screenshot("013_err_duplicate")

    except Exception as e:
        record("Unexpected Error", False, str(e))
    finally:
        # Cleanup: kill ALL test sessions
        print("\nCleanup: Killing test sessions...")
        for name in TEST_SESSIONS:
            subprocess.run([SHUX_BIN, "kill", "-s", name], capture_output=True, timeout=5)
        for i in range(5):
            subprocess.run([SHUX_BIN, "kill", "-s", f"session-{i}"], capture_output=True, timeout=5)

        # Print summary
        passed = sum(1 for _, p, _ in results if p)
        total = len(results)
        print(f"\n{'=' * 50}")
        print(f"  Results: {passed}/{total} passed")
        if passed < total:
            print("  Failures:")
            for name, p, detail in results:
                if not p:
                    print(f"    FAIL: {name}" + (f" — {detail}" if detail else ""))
        print(f"{'=' * 50}\n")

        # Close the test tab
        try:
            await session.async_send_text("exit\n")
            await asyncio.sleep(0.5)
            await tab.async_close()
        except Exception:
            pass


iterm2.run_until_complete(main)
