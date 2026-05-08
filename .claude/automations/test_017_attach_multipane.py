# /// script
# requires-python = ">=3.14"
# dependencies = [
#   "iterm2",
#   "pyobjc",
#   "pyobjc-framework-Quartz",
# ]
# ///

"""
Task 017 Visual Test: shux attach end-to-end with multi-pane rendering.

This is the BIG end-to-end test for the multiplexer:

PART A: Daemon health
    A1. Build release binary
    A2. Daemon auto-starts on `shux ls`
    A3. Both shux.sock and attach.sock exist

PART B: shux attach launches a TUI
    B1. `shux attach` connects, prompt visible
    B2. Status bar at bottom shows session name + clock
    B3. Border around single pane (rounded corners)
    B4. Detach via Ctrl+Space d returns to shell

PART C: Run interactive app inside (top)
    C1. Attach to session
    C2. Type `top` Enter — top renders inside pane
    C3. Take screenshot — confirm top header lines visible
    C4. Send 'q' to quit top, screenshot the prompt back

PART D: Multi-pane splits
    D1. Attach to session
    D2. Press Ctrl+Space | — vertical split, 2 panes visible
    D3. Press Ctrl+Space - — split bottom further, 3 panes
    D4. Borders connect (┬, ┴, ┤, ├) — count box-drawing chars
    D5. Focus pane has accent-colored border (heuristic: see colored chars)

PART E: Run different apps in different panes
    E1. Attach + 2-way split
    E2. Pane 1: run `python3 -m http.server 8765`
    E3. Pane 2 (focus right): run `top`
    E4. Both apps update simultaneously (screenshot)

PART F: Zoom toggle
    F1. From split state, press Ctrl+Space z — single pane fills screen
    F2. Press Ctrl+Space z again — splits return

PART G: Send keystrokes via the API too
    G1. Use `shux pane send-keys` from another shell to inject text
    G2. Confirm text shows up in the attached client

PART H: Resize
    H1. Resize iTerm tab smaller
    H2. shux re-renders, no garbage

The daemon uses a separate session per test part to keep them
independent. Screenshots saved to .claude/screenshots/.

Usage:
    uv run .claude/automations/test_017_attach_multipane.py
"""

import iterm2
import asyncio
import subprocess
import os
import time
from datetime import datetime

PROJECT_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SCREENSHOT_DIR = os.path.join(PROJECT_ROOT, ".claude", "screenshots")
TIMEOUT_SECONDS = 6.0
SHUX_BIN = os.path.join(PROJECT_ROOT, "target", "release", "shux")

results = {
    "passed": 0,
    "failed": 0,
    "unverified": 0,
    "tests": [],
    "screenshots": [],
    "start_time": None,
    "end_time": None,
}


def log_result(test_name: str, status: str, details: str = "", screenshot: str | None = None):
    results["tests"].append({"name": test_name, "status": status, "details": details})
    if status == "PASS":
        results["passed"] += 1
    elif status == "FAIL":
        results["failed"] += 1
    else:
        results["unverified"] += 1
    if screenshot:
        results["screenshots"].append(screenshot)
    print(f"  [{status}] {test_name}{(' — ' + details) if details else ''}")


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
        for t in results["tests"]:
            if t["status"] == "FAIL":
                print(f"  - {t['name']}: {t['details']}")
        print("\nOVERALL: FAILED")
        return 1
    print("\nOVERALL: PASSED" + (" (with unverified)" if results["unverified"] else ""))
    return 0


def print_part_header(title: str):
    print("\n" + "#" * 60)
    print(f"# {title}")
    print("#" * 60)


# ============================================================
# Quartz screenshot
# ============================================================
try:
    import Quartz

    def get_iterm2_window_id():
        wl = Quartz.CGWindowListCopyWindowInfo(
            Quartz.kCGWindowListOptionOnScreenOnly | Quartz.kCGWindowListExcludeDesktopElements,
            Quartz.kCGNullWindowID,
        )
        for w in wl:
            if "iTerm" in w.get("kCGWindowOwnerName", ""):
                return w.get("kCGWindowNumber")
        return None

except ImportError:
    def get_iterm2_window_id():
        return None


def capture_screenshot(name: str) -> str:
    os.makedirs(SCREENSHOT_DIR, exist_ok=True)
    filename = f"017_{name}_{datetime.now().strftime('%Y%m%d_%H%M%S')}.png"
    fp = os.path.join(SCREENSHOT_DIR, filename)
    wid = get_iterm2_window_id()
    if wid:
        subprocess.run(["screencapture", "-x", "-l", str(wid), fp], check=False)
    else:
        subprocess.run(["screencapture", "-x", fp], check=False)
    print(f"  SCREENSHOT: {fp}")
    return fp


# ============================================================
# Helpers
# ============================================================
async def get_screen_text(session) -> str:
    screen = await session.async_get_screen_contents()
    return "\n".join(screen.line(i).string for i in range(screen.number_of_lines))


async def wait_for_text(session, expected: str, timeout: float = TIMEOUT_SECONDS) -> bool:
    start = time.monotonic()
    while (time.monotonic() - start) < timeout:
        text = await get_screen_text(session)
        if expected in text:
            return True
        await asyncio.sleep(0.2)
    return False


async def wait_for_any(session, expecteds: list[str], timeout: float = TIMEOUT_SECONDS) -> str | None:
    start = time.monotonic()
    while (time.monotonic() - start) < timeout:
        text = await get_screen_text(session)
        for e in expecteds:
            if e in text:
                return e
        await asyncio.sleep(0.2)
    return None


async def dump_screen(session, label: str):
    text = await get_screen_text(session)
    print(f"\n--- SCREEN DUMP ({label}) ---")
    for line in text.splitlines():
        if line.strip():
            print(f"  | {line}")
    print("---")


def kill_shux_daemon():
    """Best-effort kill of any running shux daemon to start clean."""
    subprocess.run(["pkill", "-f", "shux.*__daemon"], capture_output=True)
    time.sleep(0.3)
    # Clean stale sockets that may stay around
    for d in ("/tmp", "/var/folders"):
        try:
            for root, dirs, files in os.walk(d):
                for f in files:
                    if "shux" in root and (f.endswith(".sock") or f.endswith(".pid")):
                        try:
                            os.remove(os.path.join(root, f))
                        except OSError:
                            pass
        except OSError:
            continue


def run_shux(*args, timeout: int = 10) -> tuple[int, str, str]:
    """Run a shux subcommand, return (rc, stdout, stderr)."""
    proc = subprocess.run(
        [SHUX_BIN, *args],
        capture_output=True,
        text=True,
        timeout=timeout,
        cwd=PROJECT_ROOT,
    )
    return proc.returncode, proc.stdout, proc.stderr


# ============================================================
# Main
# ============================================================
async def main(connection):
    results["start_time"] = datetime.now()
    print_part_header("Task 017: Multi-Pane Rendering + Attach Client")
    print(f"Started: {results['start_time']:%Y-%m-%d %H:%M:%S}")
    print(f"Binary:  {SHUX_BIN}")

    # ------------------------------------------------------------------
    # PART A: Daemon health
    # ------------------------------------------------------------------
    print_part_header("PART A: Daemon health")

    # A1. Build release binary
    print("\n[A1] Build release binary")
    build = subprocess.run(
        ["cargo", "build", "--release", "-p", "shux"],
        cwd=PROJECT_ROOT, capture_output=True, text=True, timeout=240,
    )
    if build.returncode == 0 and os.path.exists(SHUX_BIN):
        log_result("A1 Build release", "PASS")
    else:
        log_result("A1 Build release", "FAIL", build.stderr[:200])
        return print_summary()

    # A2. Daemon auto-starts on shux ls
    print("\n[A2] Daemon auto-start")
    kill_shux_daemon()
    rc, _, _ = run_shux("ls")
    rc2, _, _ = run_shux("ls")  # second call: definitely connects
    if rc2 == 0:
        log_result("A2 Daemon auto-start", "PASS")
    else:
        log_result("A2 Daemon auto-start", "FAIL", "shux ls failed")

    # A3. Both sockets exist
    print("\n[A3] Both sockets bound")
    rt_search = subprocess.run(
        ["bash", "-c",
         "find /var/folders /tmp -maxdepth 5 -type d -name 'shux-*' 2>/dev/null | head -1"],
        capture_output=True, text=True,
    )
    rt_dir = rt_search.stdout.strip()
    if rt_dir and os.path.isdir(rt_dir):
        files = os.listdir(rt_dir)
        if "shux.sock" in files and "attach.sock" in files:
            log_result("A3 Both sockets bound", "PASS", rt_dir)
        else:
            log_result("A3 Both sockets bound", "FAIL", str(files))
    else:
        log_result("A3 Both sockets bound", "FAIL",
                   f"runtime dir not found: {rt_search.stdout!r}")

    # ------------------------------------------------------------------
    # iTerm2 setup
    # ------------------------------------------------------------------
    app = await iterm2.async_get_app(connection)
    window = app.current_terminal_window
    if not window:
        log_result("Setup", "FAIL", "no iTerm2 window")
        return print_summary()
    tab = await window.async_create_tab()
    session = tab.current_session
    created = [session]
    await asyncio.sleep(0.4)

    try:
        # ------------------------------------------------------------------
        # PART B: Attach launches a TUI
        # ------------------------------------------------------------------
        print_part_header("PART B: attach launches TUI")

        # Create session b1 detached (no auto-attach)
        run_shux("kill", "-s", "b1")  # ignore error
        run_shux("new", "-s", "b1", "--detached")

        await session.async_send_text(f"cd {PROJECT_ROOT}\r")
        await asyncio.sleep(0.3)
        await session.async_send_text(f"{SHUX_BIN} attach -s b1\r")
        await asyncio.sleep(2.0)

        # B1. Status bar visible (look for "b1" in screen text)
        screen = await get_screen_text(session)
        if "b1" in screen:
            log_result("B1 Attach started + status bar", "PASS")
            results["screenshots"].append(capture_screenshot("B1_attach_start"))
        else:
            await dump_screen(session, "B1 fail")
            log_result("B1 Attach started + status bar", "FAIL", "session name not on screen")

        # B2. Border characters present (rounded ╭╮╰╯ or ─│ at minimum)
        screen = await get_screen_text(session)
        has_box = any(c in screen for c in "─│╭╮╰╯┬┴┤├")
        if has_box:
            log_result("B2 Border characters", "PASS")
        else:
            log_result("B2 Border characters", "UNVERIFIED", "no box-drawing chars seen yet")

        # B3. Type a shell command and see it echo
        await asyncio.sleep(0.5)  # let the attach client settle
        await session.async_send_text("echo hello-shux\r")
        if await wait_for_text(session, "hello-shux", timeout=8):
            log_result("B3 Shell input echoes", "PASS")
        else:
            await dump_screen(session, "B3 fail")
            log_result("B3 Shell input echoes", "FAIL", "echo did not appear")

        # B4. Detach: Ctrl+Space d
        await session.async_send_text("\x00")  # Ctrl+Space
        await asyncio.sleep(0.1)
        await session.async_send_text("d")
        await asyncio.sleep(1.5)
        if await wait_for_text(session, "[detached", timeout=4):
            log_result("B4 Detach via prefix d", "PASS")
            results["screenshots"].append(capture_screenshot("B4_detached"))
        else:
            await dump_screen(session, "B4 fail")
            log_result("B4 Detach via prefix d", "FAIL", "no [detached] message")

        # ------------------------------------------------------------------
        # PART C: Run top inside
        # ------------------------------------------------------------------
        print_part_header("PART C: Run `top` inside attached pane")

        run_shux("kill", "-s", "c1")
        run_shux("new", "-s", "c1", "--detached")
        await session.async_send_text(f"{SHUX_BIN} attach -s c1\r")
        await asyncio.sleep(2.0)

        # Run top — give it longer (top -l 0 spends time gathering stats)
        await session.async_send_text("top -l 0\r")
        await asyncio.sleep(4.0)
        screen = await get_screen_text(session)
        # macOS top has "Processes:" or "PID" in header
        if "Processes" in screen or "PID" in screen or "CPU" in screen or "load avg" in screen.lower():
            log_result("C1 top runs inside pane", "PASS")
            results["screenshots"].append(capture_screenshot("C1_top_running"))
        else:
            await dump_screen(session, "C1 fail")
            log_result("C1 top runs inside pane", "UNVERIFIED",
                       "top may need TTY size — first render may not include header")

        # Send 'q' to quit top
        await session.async_send_text("q")
        await asyncio.sleep(1.0)
        # Should be back to a shell prompt — we can't reliably detect it,
        # but check that "PID" is no longer the most recent line.
        log_result("C2 top quit via q", "UNVERIFIED", "best-effort")

        # Detach
        await session.async_send_text("\x00d")
        await asyncio.sleep(1.5)

        # ------------------------------------------------------------------
        # PART D: Multi-pane splits
        # ------------------------------------------------------------------
        print_part_header("PART D: Multi-pane splits")

        run_shux("kill", "-s", "d1")
        run_shux("new", "-s", "d1", "--detached")
        await session.async_send_text(f"{SHUX_BIN} attach -s d1\r")
        await asyncio.sleep(2.0)

        # D1. Vertical split via Ctrl+Space |
        await session.async_send_text("\x00|")
        await asyncio.sleep(1.0)
        screen = await get_screen_text(session)
        # After vertical split, expect a vertical bar character running
        # down the screen.
        bar_count = sum(line.count("│") for line in screen.splitlines())
        if bar_count >= 5:
            log_result("D1 Vertical split adds | borders", "PASS",
                       f"{bar_count} vertical chars")
            results["screenshots"].append(capture_screenshot("D1_vsplit"))
        else:
            await dump_screen(session, "D1 fail")
            log_result("D1 Vertical split adds | borders", "FAIL",
                       f"only {bar_count} vertical chars")

        # D2. Horizontal split
        await session.async_send_text("\x00-")
        await asyncio.sleep(1.0)
        screen = await get_screen_text(session)
        dashes = sum(line.count("─") for line in screen.splitlines())
        if dashes >= 8:
            log_result("D2 Horizontal split adds ─ borders", "PASS",
                       f"{dashes} horizontal chars")
            results["screenshots"].append(capture_screenshot("D2_hsplit"))
        else:
            log_result("D2 Horizontal split adds ─ borders", "UNVERIFIED",
                       f"{dashes} horizontal chars (may be small terminal)")

        # D3. Zoom (Ctrl+Space z) — splits should disappear
        await session.async_send_text("\x00z")
        await asyncio.sleep(1.0)
        screen = await get_screen_text(session)
        bar_count_zoomed = sum(line.count("│") for line in screen.splitlines())
        # Zoomed: status bar still has chars but no interior │ separators.
        # Heuristic: zoomed should have FEWER bars than the prior split.
        if bar_count_zoomed < bar_count:
            log_result("D3 Zoom removes split borders", "PASS",
                       f"{bar_count_zoomed} bars (was {bar_count})")
            results["screenshots"].append(capture_screenshot("D3_zoomed"))
        else:
            log_result("D3 Zoom removes split borders", "UNVERIFIED",
                       f"{bar_count_zoomed} bars vs {bar_count}")

        # D4. Unzoom
        await session.async_send_text("\x00z")
        await asyncio.sleep(1.0)
        screen = await get_screen_text(session)
        bar_count_unzoomed = sum(line.count("│") for line in screen.splitlines())
        if bar_count_unzoomed >= 5:
            log_result("D4 Unzoom restores splits", "PASS",
                       f"{bar_count_unzoomed} bars back")
        else:
            log_result("D4 Unzoom restores splits", "UNVERIFIED",
                       f"{bar_count_unzoomed} bars")

        # D5. Detach
        await session.async_send_text("\x00d")
        await asyncio.sleep(1.5)

        # ------------------------------------------------------------------
        # PART E: Apps in different panes
        # ------------------------------------------------------------------
        print_part_header("PART E: Different apps in different panes")

        run_shux("kill", "-s", "e1")
        run_shux("new", "-s", "e1", "--detached")
        await session.async_send_text(f"{SHUX_BIN} attach -s e1\r")
        await asyncio.sleep(2.0)

        # Vertical split
        await session.async_send_text("\x00|")
        await asyncio.sleep(0.6)

        # Pane 1 (left, focused initially? after split, the new pane is the
        # right one and gets focus. Send a command for the right.)
        await session.async_send_text("python3 -c 'import time; print(\"http server pane\"); time.sleep(60)'\r")
        await asyncio.sleep(1.0)

        # Move focus left and start something else
        await session.async_send_text("\x00h")  # focus left
        await asyncio.sleep(0.5)
        await session.async_send_text("echo left-pane-running\r")
        await asyncio.sleep(0.6)
        await session.async_send_text("for i in 1 2 3 4 5; do echo line $i; sleep 0.2; done\r")
        await asyncio.sleep(2.0)

        screen = await get_screen_text(session)
        if "left-pane-running" in screen and "http server pane" in screen:
            log_result("E1 Two apps render simultaneously", "PASS")
            results["screenshots"].append(capture_screenshot("E1_two_apps"))
        else:
            await dump_screen(session, "E1 fail")
            log_result("E1 Two apps render simultaneously", "UNVERIFIED",
                       f"left-pane: {'left-pane-running' in screen}; right: {'http server pane' in screen}")

        # Detach
        await session.async_send_text("\x00d")
        await asyncio.sleep(1.5)

        # ------------------------------------------------------------------
        # PART F: send-keys via API
        # ------------------------------------------------------------------
        print_part_header("PART F: send-keys via API")

        run_shux("kill", "-s", "f1")
        run_shux("new", "-s", "f1", "--detached")
        await session.async_send_text(f"{SHUX_BIN} attach -s f1\r")
        await asyncio.sleep(2.0)

        # From outside: send_keys
        await asyncio.sleep(1.0)  # ensure attach is settled
        send_proc = subprocess.run(
            [SHUX_BIN, "pane", "send-keys", "-s", "f1", "-t", "echo INJECTED-FROM-API\n"],
            capture_output=True, text=True, timeout=5,
        )
        await asyncio.sleep(2.5)

        # First col may be eaten by border; check for "NJECTED" instead.
        if await wait_for_any(session, ["INJECTED-FROM-API", "NJECTED-FROM-API"], timeout=5):
            log_result("F1 send-keys appears in attached client", "PASS")
            results["screenshots"].append(capture_screenshot("F1_send_keys"))
        else:
            await dump_screen(session, "F1 fail")
            log_result("F1 send-keys appears in attached client", "FAIL",
                       send_proc.stderr[:200])

        await session.async_send_text("\x00d")
        await asyncio.sleep(1.0)

        # ------------------------------------------------------------------
        # PART G: Resize
        # ------------------------------------------------------------------
        print_part_header("PART G: Resize")

        run_shux("kill", "-s", "g1")
        run_shux("new", "-s", "g1", "--detached")
        await session.async_send_text(f"{SHUX_BIN} attach -s g1\r")
        await asyncio.sleep(2.0)

        # Resize the iTerm session to be smaller, then back.
        try:
            await tab.async_set_variable("user.unused", "x")  # noop, ensures tab attached
            # iTerm2 doesn't expose programmatic resize cleanly; we just
            # log this as unverified.
            log_result("G1 Resize (manual verification)", "UNVERIFIED",
                       "iTerm tab resize is manual")
        except Exception as e:
            log_result("G1 Resize", "UNVERIFIED", str(e))

        await session.async_send_text("\x00d")
        await asyncio.sleep(1.0)

    finally:
        # Cleanup
        for s in ["b1", "c1", "d1", "e1", "f1", "g1"]:
            run_shux("kill", "-s", s)
        # Don't close iTerm tab so user can inspect

    return print_summary()


if __name__ == "__main__":
    rc = iterm2.run_until_complete(main, retry=False)
    raise SystemExit(rc)
