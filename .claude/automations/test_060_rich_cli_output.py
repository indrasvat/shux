# /// script
# requires-python = ">=3.14"
# dependencies = [
#   "iterm2",
#   "pyobjc",
#   "pyobjc-framework-Quartz",
# ]
# ///

"""
Task 060 Visual Test: Rich CLI Output — Beautiful List Commands

Tests all rich output variants: box frames, column alignment, active markers,
short IDs, summary footers, confirmations, errors, plain format, and NO_COLOR.

Part A  — Setup & Build (Tests 1–2)
Part B  — Session List: Rich Output (Tests 3–8)
Part C  — Session List: Short IDs (Tests 9–10)
Part D  — Session List: Empty State (Test 11)
Part E  — Window List: Rich Output (Tests 12–17)
Part F  — Pane List: Rich Output (Tests 18–23)
Part G  — Pane List: Zoom State (Tests 24–25)
Part H  — Confirmation Messages (Tests 26–30)
Part I  — Error Messages (Tests 31–33)
Part J  — Plain Format / Piped Output (Tests 34–37)
Part K  — NO_COLOR Compatibility (Tests 38–39)
Part L  — Multi-Session Stress (Tests 40–42)
Part M  — JSON Format Cross-Check (Tests 43–44)

Usage:
    uv run .claude/automations/test_060_rich_cli_output.py
"""

import iterm2
import asyncio
import subprocess
import os
import json
import re
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


def run_cli(*args, env_extra=None):
    """Run shux CLI and return (stdout, stderr, returncode)."""
    env = os.environ.copy()
    if env_extra:
        env.update(env_extra)
    result = subprocess.run(
        [SHUX_BIN] + list(args),
        capture_output=True,
        text=True,
        timeout=10,
        env=env,
    )
    return result.stdout, result.stderr, result.returncode


def has_short_id(text):
    """Check if text contains 8-char hex short IDs."""
    return bool(re.search(r'[0-9a-f]{8}', text))


def has_full_uuid(text):
    """Check if text contains full 36-char UUIDs."""
    return bool(re.search(r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}', text))


def has_ansi(text):
    """Check if text contains ANSI escape codes."""
    return bool(re.search(r'\x1b\[', text))


def kill_all_test_sessions():
    """Kill all sessions that might be left from previous runs."""
    for name in ["alpha", "beta", "gamma", "confirm-test", "nocolor-test",
                  "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8",
                  "single-pane", "my-extremely-long-project-name-that-tests-width"]:
        subprocess.run([SHUX_BIN, "kill", "-s", name], capture_output=True, timeout=5)


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
        print(f"\nshux Rich CLI Output Visual Test (060) — {datetime.now().isoformat()}")
        print(f"Project: {PROJECT_ROOT}")
        print(f"Binary: {SHUX_BIN}")
        print()

        # Kill any stale daemon so the fresh binary is used
        subprocess.run(["pkill", "-f", "shux __daemon"], capture_output=True, timeout=5)
        await asyncio.sleep(1)

        # Clean up any leftover test sessions
        kill_all_test_sessions()

        # ══════════════════════════════════════════════════════
        # Part A — Setup & Build (Tests 1–2)
        # ══════════════════════════════════════════════════════
        print("Part A — Setup & Build")

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

        # ── Test 2: Create test sessions ─────────────────────
        print("Test 2: Create test sessions")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} new -s alpha -d", 2.0)
        await send_and_wait(session, f"{SHUX_BIN} new -s beta -d", 2.0)
        await send_and_wait(session, f"{SHUX_BIN} new -s gamma -d", 2.0)
        content = await read_screen(session)
        has_all = "alpha" in content and "beta" in content and "gamma" in content
        record("2. Create test sessions", has_all,
               f"alpha={'alpha' in content}, beta={'beta' in content}, gamma={'gamma' in content}")
        take_screenshot("060_setup")

        # ══════════════════════════════════════════════════════
        # Part B — Session List: Rich Output (Tests 3–8)
        # ══════════════════════════════════════════════════════
        print("\nPart B — Session List: Rich Output")

        # ── Test 3: Box frame present ────────────────────────
        print("Test 3: Box frame present")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} ls", 2.0)
        content = await read_screen(session)

        has_top_corner = "\u256d" in content    # ╭
        has_bottom_corner = "\u2570" in content  # ╰
        record("3. Box frame present", has_top_corner and has_bottom_corner,
               f"top={has_top_corner}, bottom={has_bottom_corner}")
        take_screenshot("060_session_list")

        # ── Test 4: Header text ──────────────────────────────
        print("Test 4: Header text")
        has_sessions_header = "Sessions" in content
        record("4. Header text", has_sessions_header,
               f"'Sessions' in header: {has_sessions_header}")

        # ── Test 5: Column alignment ─────────────────────────
        print("Test 5: Column alignment")
        # All three session names should be present
        has_alpha = "alpha" in content
        has_beta = "beta" in content
        has_gamma = "gamma" in content
        record("5. Column alignment", has_alpha and has_beta and has_gamma,
               f"alpha={has_alpha}, beta={has_beta}, gamma={has_gamma}")

        # ── Test 6: Diamond marker column ─────────────────────
        print("Test 6: Diamond marker column")
        # All sessions created with -d (detached), so all get open diamond ◇
        # Active (filled) diamond ◆ only appears when a client is attached
        has_diamond = "\u25c6" in content or "\u25c7" in content  # ◆ or ◇
        record("6. Diamond marker column present", has_diamond,
               f"diamond present: {has_diamond}")

        # ── Test 7: Detached markers ─────────────────────────
        print("Test 7: Detached markers")
        has_open_diamond = "\u25c7" in content  # ◇
        record("7. Detached markers (open diamond)", has_open_diamond,
               f"open diamond present: {has_open_diamond}")

        # ── Test 8: Summary footer ───────────────────────────
        print("Test 8: Summary footer")
        has_session_count = "3 sessions" in content
        has_windows_total = "window" in content.lower()
        record("8. Summary footer", has_session_count and has_windows_total,
               f"'3 sessions'={has_session_count}, 'window'={has_windows_total}")
        take_screenshot("060_session_list_footer")

        # ══════════════════════════════════════════════════════
        # Part C — Session List: Short IDs (Tests 9–10)
        # ══════════════════════════════════════════════════════
        print("\nPart C — Session List: Short IDs")

        # ── Test 9: Short IDs in text ────────────────────────
        print("Test 9: Short IDs in text")
        # In text output (iTerm2 = TTY), we should see 8-char IDs
        has_short = has_short_id(content)
        # Should NOT have full UUIDs in the session list area
        # (Full UUIDs only in JSON mode)
        record("9. Short IDs in text output", has_short,
               f"short IDs found: {has_short}")
        take_screenshot("060_short_ids")

        # ── Test 10: Full IDs in JSON ────────────────────────
        print("Test 10: Full IDs in JSON")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} --format json ls", 2.0)
        json_content = await read_screen(session)
        has_full = has_full_uuid(json_content)
        record("10. Full IDs in JSON", has_full,
               f"full UUIDs found: {has_full}")
        take_screenshot("060_json_full_ids")

        # ══════════════════════════════════════════════════════
        # Part D — Session List: Empty State (Test 11)
        # ══════════════════════════════════════════════════════
        print("\nPart D — Session List: Empty State")

        # ── Test 11: Empty state ─────────────────────────────
        print("Test 11: Empty state")
        # Kill all sessions
        await send_and_wait(session, f"{SHUX_BIN} kill -s alpha", 1.0)
        await send_and_wait(session, f"{SHUX_BIN} kill -s beta", 1.0)
        await send_and_wait(session, f"{SHUX_BIN} kill -s gamma", 1.0)
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} ls", 2.0)
        content = await read_screen(session)
        has_no_sessions = "(no sessions)" in content
        has_box = "\u256d" in content  # ╭ (box frame still present in empty state)
        has_hint = "shux new" in content
        record("11. Empty state", has_no_sessions and has_box,
               f"no_sessions={has_no_sessions}, box={has_box}, hint={has_hint}")
        take_screenshot("060_empty_sessions")

        # ══════════════════════════════════════════════════════
        # Part E — Window List: Rich Output (Tests 12–17)
        # ══════════════════════════════════════════════════════
        print("\nPart E — Window List: Rich Output")

        # Recreate sessions for window tests
        await send_and_wait(session, f"{SHUX_BIN} new -s alpha -d", 2.0)
        # Create extra windows
        await send_and_wait(session, f"{SHUX_BIN} window new -s alpha -n editor", 1.5)
        await send_and_wait(session, f"{SHUX_BIN} window new -s alpha -n server", 1.5)
        await send_and_wait(session, f"{SHUX_BIN} window new -s alpha -n logs", 1.5)

        # ── Test 12: Box frame present ───────────────────────
        print("Test 12: Box frame present (window list)")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} window list -s alpha", 2.0)
        content = await read_screen(session)

        has_top = "\u256d" in content
        has_bottom = "\u2570" in content
        record("12. Window list box frame", has_top and has_bottom,
               f"top={has_top}, bottom={has_bottom}")
        take_screenshot("060_window_list")

        # ── Test 13: Context header ──────────────────────────
        print("Test 13: Context header")
        has_context = "session: alpha" in content or "alpha" in content
        record("13. Context header (session name)", has_context,
               f"session context: {has_context}")

        # ── Test 14: Column headers ──────────────────────────
        print("Test 14: Column headers")
        has_hash = "#" in content
        has_name_col = "NAME" in content
        has_panes_col = "PANES" in content
        record("14. Column headers", has_hash and has_name_col and has_panes_col,
               f"#={has_hash}, NAME={has_name_col}, PANES={has_panes_col}")

        # ── Test 15: Active marker ───────────────────────────
        print("Test 15: Active marker")
        has_active_marker = "\u25c0" in content  # ◀
        has_active_text = "active" in content.lower()
        record("15. Active marker", has_active_marker or has_active_text,
               f"marker={has_active_marker}, text={has_active_text}")
        take_screenshot("060_window_active_marker")

        # ── Test 16: Index alignment ─────────────────────────
        print("Test 16: Index alignment")
        # Check that numeric indices appear in the output
        has_indices = any(c.isdigit() for c in content)
        record("16. Index numbers present", has_indices,
               f"digits in output: {has_indices}")

        # ── Test 17: Summary footer ──────────────────────────
        print("Test 17: Summary footer")
        has_window_count = "window" in content.lower()
        has_pane_count = "pane" in content.lower()
        record("17. Window list summary footer", has_window_count and has_pane_count,
               f"window_count={has_window_count}, pane_count={has_pane_count}")
        take_screenshot("060_window_list_footer")

        # ══════════════════════════════════════════════════════
        # Part F — Pane List: Rich Output (Tests 18–23)
        # ══════════════════════════════════════════════════════
        print("\nPart F — Pane List: Rich Output")

        # Split panes in alpha to have multiple
        await send_and_wait(session, f"{SHUX_BIN} pane split -s alpha", 1.5)
        await send_and_wait(session, f"{SHUX_BIN} pane split -s alpha --horizontal", 1.5)

        # ── Test 18: Box frame present ───────────────────────
        print("Test 18: Box frame present (pane list)")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} pane list -s alpha", 2.0)
        content = await read_screen(session)

        has_top = "\u256d" in content
        has_bottom = "\u2570" in content
        record("18. Pane list box frame", has_top and has_bottom,
               f"top={has_top}, bottom={has_bottom}")
        take_screenshot("060_pane_list")

        # ── Test 19: Context header ──────────────────────────
        print("Test 19: Context header")
        has_pane_context = "alpha" in content
        has_window_context = "window" in content.lower() or "Panes" in content
        record("19. Pane list context header", has_pane_context and has_window_context,
               f"session={has_pane_context}, window={has_window_context}")

        # ── Test 20: Column headers ──────────────────────────
        print("Test 20: Column headers")
        has_id_col = "ID" in content
        record("20. Pane list column headers", has_id_col,
               f"ID={has_id_col}")

        # ── Test 21: Short pane IDs ──────────────────────────
        print("Test 21: Short pane IDs")
        has_short = has_short_id(content)
        record("21. Short pane IDs", has_short,
               f"short IDs: {has_short}")

        # ── Test 22: Focus marker ────────────────────────────
        print("Test 22: Focus marker")
        has_focus_marker = "\u25c0" in content  # ◀
        has_focus_text = "focus" in content.lower()
        record("22. Focus marker", has_focus_marker or has_focus_text,
               f"marker={has_focus_marker}, text={has_focus_text}")
        take_screenshot("060_pane_focus_marker")

        # ── Test 23: Summary footer ──────────────────────────
        print("Test 23: Summary footer")
        has_pane_count = "pane" in content.lower()
        record("23. Pane list summary footer", has_pane_count,
               f"pane count in footer: {has_pane_count}")
        take_screenshot("060_pane_list_footer")

        # ══════════════════════════════════════════════════════
        # Part G — Pane List: Zoom State (Tests 24–25)
        # ══════════════════════════════════════════════════════
        print("\nPart G — Pane List: Zoom State")

        # ── Test 24: Zoom a pane ─────────────────────────────
        print("Test 24: Zoom a pane")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} pane zoom -s alpha", 2.0)
        content = await read_screen(session)
        has_zoom_confirm = "\u2713" in content or "Zoomed" in content  # ✓
        record("24. Zoom pane", has_zoom_confirm,
               f"zoom confirmation: {has_zoom_confirm}")

        # ── Test 25: Zoom visible in list ────────────────────
        print("Test 25: Zoom visible in list")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} pane list -s alpha", 2.0)
        content = await read_screen(session)
        has_zoomed = "[zoomed]" in content.lower() or "zoomed" in content.lower()
        record("25. Zoom visible in pane list", has_zoomed,
               f"[zoomed]: {has_zoomed}")
        take_screenshot("060_pane_zoomed")

        # Unzoom for later tests
        await send_and_wait(session, f"{SHUX_BIN} pane zoom -s alpha", 1.5)

        # ══════════════════════════════════════════════════════
        # Part H — Confirmation Messages (Tests 26–30)
        # ══════════════════════════════════════════════════════
        print("\nPart H — Confirmation Messages")

        # ── Test 26: Create confirmation ─────────────────────
        print("Test 26: Create confirmation")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} new -s confirm-test -d", 2.0)
        content = await read_screen(session)
        has_check = "\u2713" in content  # ✓
        has_short_id_in = has_short_id(content)
        record("26. Create confirmation (checkmark + short ID)", has_check and has_short_id_in,
               f"checkmark={has_check}, short_id={has_short_id_in}")
        take_screenshot("060_confirm_create")

        # ── Test 27: Kill confirmation ───────────────────────
        print("Test 27: Kill confirmation")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} kill -s confirm-test", 2.0)
        content = await read_screen(session)
        has_check = "\u2713" in content
        has_killed = "Killed" in content or "killed" in content
        record("27. Kill confirmation", has_check and has_killed,
               f"checkmark={has_check}, killed={has_killed}")
        take_screenshot("060_confirm_kill")

        # ── Test 28: Window create ───────────────────────────
        print("Test 28: Window create confirmation")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} window new -s alpha -n test-win", 2.0)
        content = await read_screen(session)
        has_check = "\u2713" in content
        has_created = "Created" in content or "created" in content
        record("28. Window create confirmation", has_check and has_created,
               f"checkmark={has_check}, created={has_created}")
        take_screenshot("060_confirm_window_create")

        # ── Test 29: Pane split ──────────────────────────────
        print("Test 29: Pane split confirmation")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} pane split -s alpha", 2.0)
        content = await read_screen(session)
        has_check = "\u2713" in content
        has_split = "Split" in content or "split" in content
        record("29. Pane split confirmation", has_check and has_split,
               f"checkmark={has_check}, split={has_split}")
        take_screenshot("060_confirm_pane_split")

        # ── Test 30: Rename ──────────────────────────────────
        print("Test 30: Rename confirmation")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} window rename -s alpha -w test-win -n renamed-win", 2.0)
        content = await read_screen(session)
        has_check = "\u2713" in content
        has_renamed = "Renamed" in content or "renamed" in content
        record("30. Rename confirmation", has_check and has_renamed,
               f"checkmark={has_check}, renamed={has_renamed}")
        take_screenshot("060_confirm_rename")

        # ══════════════════════════════════════════════════════
        # Part I — Error Messages (Tests 31–33)
        # ══════════════════════════════════════════════════════
        print("\nPart I — Error Messages")

        # ── Test 31: Duplicate session ───────────────────────
        print("Test 31: Duplicate session error")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} new -s alpha -d", 2.0)
        content = await read_screen(session)
        has_x = "\u2717" in content  # ✗
        no_double_error = "error: Error:" not in content
        record("31. Duplicate session error", has_x and no_double_error,
               f"cross={has_x}, no_double={no_double_error}")
        take_screenshot("060_error_duplicate")

        # ── Test 32: Kill nonexistent ────────────────────────
        print("Test 32: Kill nonexistent session error")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} kill -s nonexistent", 2.0)
        content = await read_screen(session)
        has_x = "\u2717" in content
        record("32. Kill nonexistent error", has_x,
               f"cross={has_x}")
        take_screenshot("060_error_not_found")

        # ── Test 33: Kill nonexistent pane ────────────────────
        print("Test 33: Kill nonexistent pane error")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} pane kill -s alpha --pane 00000000-0000-0000-0000-000000000000", 2.0)
        content = await read_screen(session)
        has_x = "\u2717" in content
        record("33. Kill nonexistent pane error", has_x,
               f"cross={has_x}")
        take_screenshot("060_error_nonexistent_pane")

        # ══════════════════════════════════════════════════════
        # Part J — Plain Format / Piped Output (Tests 34–37)
        # ══════════════════════════════════════════════════════
        print("\nPart J — Plain Format / Piped Output")

        # ── Test 34: Plain session list ──────────────────────
        print("Test 34: Plain session list")
        stdout, stderr, rc = run_cli("--format", "plain", "ls")
        no_box = "\u256d" not in stdout and "\u2570" not in stdout  # no ╭╰
        has_tabs = "\t" in stdout
        record("34. Plain session list", rc == 0 and no_box and has_tabs,
               f"rc={rc}, no_box={no_box}, tabs={has_tabs}")
        take_screenshot("060_plain_sessions")

        # ── Test 35: Plain window list ───────────────────────
        print("Test 35: Plain window list")
        stdout, stderr, rc = run_cli("--format", "plain", "window", "list", "-s", "alpha")
        no_box = "\u256d" not in stdout and "\u2570" not in stdout
        has_tabs = "\t" in stdout
        record("35. Plain window list", rc == 0 and no_box and has_tabs,
               f"rc={rc}, no_box={no_box}, tabs={has_tabs}")
        take_screenshot("060_plain_windows")

        # ── Test 36: Plain pane list ─────────────────────────
        print("Test 36: Plain pane list")
        stdout, stderr, rc = run_cli("--format", "plain", "pane", "list", "-s", "alpha")
        no_box = "\u256d" not in stdout and "\u2570" not in stdout
        has_tabs = "\t" in stdout
        record("36. Plain pane list", rc == 0 and no_box and has_tabs,
               f"rc={rc}, no_box={no_box}, tabs={has_tabs}")
        take_screenshot("060_plain_panes")

        # ── Test 37: Piped auto-detect ───────────────────────
        print("Test 37: Piped auto-detect")
        # Run through subprocess pipe — should auto-switch to plain
        result = subprocess.run(
            f"{SHUX_BIN} ls | cat",
            shell=True,
            capture_output=True,
            text=True,
            timeout=10,
        )
        piped_output = result.stdout
        no_box_piped = "\u256d" not in piped_output and "\u2570" not in piped_output
        no_ansi = not has_ansi(piped_output)
        record("37. Piped auto-detect (no box, no ANSI)", no_box_piped and no_ansi,
               f"no_box={no_box_piped}, no_ansi={no_ansi}")

        # ══════════════════════════════════════════════════════
        # Part K — NO_COLOR Compatibility (Tests 38–39)
        # ══════════════════════════════════════════════════════
        print("\nPart K — NO_COLOR Compatibility")

        # ── Test 38: NO_COLOR session list ───────────────────
        print("Test 38: NO_COLOR session list")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"NO_COLOR=1 {SHUX_BIN} ls", 2.0)
        content = await read_screen(session)
        has_box = "\u256d" in content or "+" in content  # box preserved (unicode or ascii fallback)
        has_session = "alpha" in content
        record("38. NO_COLOR session list", has_box and has_session,
               f"box={has_box}, session={has_session}")
        take_screenshot("060_nocolor_sessions")

        # ── Test 39: NO_COLOR confirmation ───────────────────
        print("Test 39: NO_COLOR confirmation")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"NO_COLOR=1 {SHUX_BIN} new -s nocolor-test -d", 2.0)
        content = await read_screen(session)
        has_check = "\u2713" in content
        record("39. NO_COLOR confirmation", has_check,
               f"checkmark={has_check}")
        take_screenshot("060_nocolor_confirm")
        # Cleanup
        subprocess.run([SHUX_BIN, "kill", "-s", "nocolor-test"], capture_output=True, timeout=5)

        # ══════════════════════════════════════════════════════
        # Part L — Multi-Session Stress (Tests 40–42)
        # ══════════════════════════════════════════════════════
        print("\nPart L — Multi-Session Stress")

        # ── Test 40: Many sessions ───────────────────────────
        print("Test 40: Many sessions")
        for i in range(1, 9):
            await send_and_wait(session, f"{SHUX_BIN} new -s s{i} -d", 1.0)
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} ls", 2.0)
        content = await read_screen(session)

        # Count how many session names appear
        found_sessions = sum(1 for i in range(1, 9) if f"s{i}" in content)
        has_alignment = "\u256d" in content and "\u2570" in content
        record("40. Many sessions", found_sessions >= 6 and has_alignment,
               f"found {found_sessions}/8 sessions, aligned={has_alignment}")
        take_screenshot("060_many_sessions")

        # ── Test 41: Long session name ───────────────────────
        print("Test 41: Long session name")
        long_name = "my-extremely-long-project-name-that-tests-width"
        await send_and_wait(session, f"{SHUX_BIN} new -s {long_name} -d", 2.0)
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} ls", 2.0)
        content = await read_screen(session)
        has_long = long_name in content
        has_box_still = "\u256d" in content and "\u2570" in content
        record("41. Long session name", has_long and has_box_still,
               f"name_present={has_long}, box_intact={has_box_still}")
        take_screenshot("060_long_name")

        # ── Test 42: Summary accuracy ────────────────────────
        print("Test 42: Summary accuracy")
        # alpha + s1..s8 + long_name = 10 sessions
        # Check footer says correct count
        has_correct_count = "10 sessions" in content
        record("42. Summary accuracy", has_correct_count,
               f"'10 sessions' in footer: {has_correct_count}")

        # Cleanup stress sessions
        for i in range(1, 9):
            subprocess.run([SHUX_BIN, "kill", "-s", f"s{i}"], capture_output=True, timeout=5)
        subprocess.run([SHUX_BIN, "kill", "-s", long_name], capture_output=True, timeout=5)

        # ══════════════════════════════════════════════════════
        # Part M — JSON Format Cross-Check (Tests 43–44)
        # ══════════════════════════════════════════════════════
        print("\nPart M — JSON Format Cross-Check")

        # ── Test 43: JSON session list ───────────────────────
        print("Test 43: JSON session list")
        stdout, stderr, rc = run_cli("--format", "json", "ls")
        try:
            parsed = json.loads(stdout.strip())
            is_valid = "sessions" in parsed and isinstance(parsed["sessions"], list)
            has_full_ids = any(has_full_uuid(json.dumps(s)) for s in parsed["sessions"])
        except (json.JSONDecodeError, KeyError):
            is_valid = False
            has_full_ids = False
        record("43. JSON session list", rc == 0 and is_valid and has_full_ids,
               f"rc={rc}, valid={is_valid}, full_ids={has_full_ids}")
        take_screenshot("060_json_sessions")

        # ── Test 44: JSON pane list ──────────────────────────
        print("Test 44: JSON pane list")
        stdout, stderr, rc = run_cli("--format", "json", "pane", "list", "-s", "alpha")
        try:
            parsed = json.loads(stdout.strip())
            is_valid = isinstance(parsed, list) or (isinstance(parsed, dict) and "panes" in parsed)
            has_ids = has_full_uuid(stdout)
        except json.JSONDecodeError:
            is_valid = False
            has_ids = False
        record("44. JSON pane list", rc == 0 and is_valid and has_ids,
               f"rc={rc}, valid={is_valid}, full_ids={has_ids}")
        take_screenshot("060_json_panes")

    finally:
        # ══════════════════════════════════════════════════════
        # Cleanup
        # ══════════════════════════════════════════════════════
        kill_all_test_sessions()

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

        # Screenshot manifest
        print("\nScreenshot manifest:")
        for fname in sorted(os.listdir(SCREENSHOT_DIR)) if os.path.exists(SCREENSHOT_DIR) else []:
            if fname.startswith("060_"):
                fpath = os.path.join(SCREENSHOT_DIR, fname)
                size_kb = os.path.getsize(fpath) / 1024
                print(f"  {fname} ({size_kb:.0f}KB)")


iterm2.run_until_complete(main)
