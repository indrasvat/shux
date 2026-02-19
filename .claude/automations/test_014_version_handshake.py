# /// script
# requires-python = ">=3.14"
# dependencies = [
#   "iterm2",
#   "pyobjc",
#   "pyobjc-framework-Quartz",
# ]
# ///

"""
Task 014 Spike Fix: Version Handshake E2E Test

Verifies that a stale daemon (running from an older binary) is automatically
detected and restarted when the CLI binary version changes (e.g., after rebuild).

Test flow:
  1. Build binary (version 0.1.0), start daemon, verify working
  2. Bump workspace version to 0.1.99, rebuild
  3. Run CLI command — should auto-restart daemon (version mismatch)
  4. Verify new daemon PID differs from old
  5. Verify CLI commands succeed (no method_not_found)
  6. Restore version to 0.1.0, rebuild

Usage:
    uv run .claude/automations/test_014_version_handshake.py
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
CARGO_TOML = os.path.join(PROJECT_ROOT, "Cargo.toml")
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


def get_pid_file_path():
    """Get the shux PID file path (matches daemon.rs runtime_dir logic)."""
    import tempfile
    xdg = os.environ.get("XDG_RUNTIME_DIR")
    if xdg:
        return os.path.join(xdg, "shux", "shux.pid")
    uid = os.getuid()
    return os.path.join(tempfile.gettempdir(), f"shux-{uid}", "shux.pid")


def read_daemon_pid():
    """Read the daemon PID from the PID file."""
    pid_path = get_pid_file_path()
    try:
        with open(pid_path) as f:
            return int(f.read().strip())
    except (FileNotFoundError, ValueError):
        return None


def get_cargo_version():
    """Read the current version from Cargo.toml."""
    with open(CARGO_TOML) as f:
        for line in f:
            if line.startswith("version = "):
                return line.split('"')[1]
    return None


def set_cargo_version(new_version):
    """Modify the workspace Cargo.toml version."""
    with open(CARGO_TOML) as f:
        content = f.read()
    # Replace the version line (only the first occurrence in [workspace.package])
    lines = content.split("\n")
    for i, line in enumerate(lines):
        if line.startswith("version = "):
            lines[i] = f'version = "{new_version}"'
            break
    with open(CARGO_TOML, "w") as f:
        f.write("\n".join(lines))


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

    original_version = get_cargo_version()
    version_changed = False

    try:
        print(f"\nshux Version Handshake E2E Test — {datetime.now().isoformat()}")
        print(f"Project: {PROJECT_ROOT}")
        print(f"Binary: {SHUX_BIN}")
        print(f"Original version: {original_version}")
        print()

        # Kill any existing daemon
        subprocess.run(["pkill", "-f", "shux __daemon"], capture_output=True, timeout=5)
        await asyncio.sleep(1)

        # ══════════════════════════════════════════════════════
        # Phase 1 — Build v1, start daemon, verify working
        # ══════════════════════════════════════════════════════
        print("Phase 1 — Build v1 and verify baseline")

        # ── Test 1: Build v1 ────────────────────────────────
        result = subprocess.run(
            ["make", "build"], cwd=PROJECT_ROOT,
            capture_output=True, text=True, timeout=120,
        )
        record("1. Build v1", result.returncode == 0)
        if result.returncode != 0:
            print("  ABORTING: Build failed")
            return

        await send_and_wait(session, f"cd {PROJECT_ROOT}", 0.5)

        # ── Test 2: Start daemon and create session ─────────
        print("Test 2: Start daemon (v1)")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} new -s handshake-test -d", 3.0)
        content = await read_screen(session)
        has_created = "created" in content.lower() or "handshake-test" in content
        record("2. Daemon started, session created", has_created)

        # ── Test 3: Record v1 daemon PID ────────────────────
        pid_v1 = read_daemon_pid()
        record("3. V1 daemon PID recorded", pid_v1 is not None,
               f"PID={pid_v1}")

        # ── Test 4: Window commands work on v1 ──────────────
        print("Test 4: Window list works on v1")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} window list -s handshake-test", 2.0)
        content = await read_screen(session)
        has_pane = "pane" in content.lower()
        no_error = "error" not in content.lower() and "method_not_found" not in content
        record("4. Window list works v1", has_pane and no_error,
               "method_not_found!" if not no_error else "")
        take_screenshot("014_handshake_v1_working")

        # ── Test 5: Version API shows version + git_sha ────
        print("Test 5: Version check v1 (version + git_sha)")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} api system.version", 2.0)
        content = await read_screen(session)
        has_v1 = original_version in content
        has_sha = "git_sha" in content
        record("5. Daemon reports v1 + git_sha", has_v1 and has_sha,
               f"version={has_v1}, git_sha={has_sha}")
        take_screenshot("014_handshake_v1_version")

        # ══════════════════════════════════════════════════════
        # Phase 2 — Bump version, rebuild, test auto-restart
        # ══════════════════════════════════════════════════════
        print("\nPhase 2 — Bump version to 0.1.99, rebuild")

        # ── Test 6: Bump version ────────────────────────────
        set_cargo_version("0.1.99")
        version_changed = True
        new_ver = get_cargo_version()
        record("6. Version bumped", new_ver == "0.1.99",
               f"Cargo.toml now says {new_ver}")

        # ── Test 7: Rebuild with new version ────────────────
        print("Test 7: Rebuild with v2 (0.1.99)")
        result = subprocess.run(
            ["make", "build"], cwd=PROJECT_ROOT,
            capture_output=True, text=True, timeout=120,
        )
        record("7. Build v2", result.returncode == 0)
        if result.returncode != 0:
            print("  ABORTING: Build failed")
            return

        # ── Test 8: Run v2 CLI — should auto-restart daemon ─
        print("Test 8: V2 binary auto-restarts stale daemon")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} ls", 5.0)
        content = await read_screen(session)
        # After restart, the old session is gone (new daemon = fresh state)
        no_error = "error" not in content.lower() and "method_not_found" not in content
        record("8. V2 ls succeeds (no error)", no_error,
               "method_not_found!" if not no_error else "")
        take_screenshot("014_handshake_v2_restart")

        # ── Test 9: PID changed ─────────────────────────────
        pid_v2 = read_daemon_pid()
        pid_changed = pid_v2 is not None and pid_v1 is not None and pid_v2 != pid_v1
        record("9. Daemon PID changed", pid_changed,
               f"v1={pid_v1} → v2={pid_v2}")

        # ── Test 10: Version API now shows 0.1.99 + git_sha ─
        print("Test 10: Version check v2 (0.1.99 + git_sha)")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} api system.version", 2.0)
        content = await read_screen(session)
        has_v2 = "0.1.99" in content
        has_sha = "git_sha" in content
        record("10. Daemon reports v2 + git_sha", has_v2 and has_sha,
               f"version={has_v2}, git_sha={has_sha}")
        take_screenshot("014_handshake_v2_version")

        # ── Test 11: Create session on new daemon ───────────
        print("Test 11: Create session on v2 daemon")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} new -s handshake-v2 -d", 2.0)
        content = await read_screen(session)
        has_created = "created" in content.lower() or "handshake-v2" in content
        record("11. Session created on v2", has_created)

        # ── Test 12: Window commands work on v2 daemon ──────
        print("Test 12: Window list works on v2")
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} window list -s handshake-v2", 2.0)
        content = await read_screen(session)
        has_pane = "pane" in content.lower()
        no_error = "error" not in content.lower() and "method_not_found" not in content
        record("12. Window list works v2", has_pane and no_error,
               "method_not_found!" if not no_error else "")
        take_screenshot("014_handshake_v2_window_list")

        # ══════════════════════════════════════════════════════
        # Phase 3 — Same-version reconnect (no restart)
        # ══════════════════════════════════════════════════════
        print("\nPhase 3 — Same-version reconnect (no restart)")

        # ── Test 13: Same version doesn't restart ───────────
        print("Test 13: Same-version reconnect")
        pid_before = read_daemon_pid()
        await send_and_wait(session, "clear", 0.3)
        await send_and_wait(session, f"{SHUX_BIN} ls", 2.0)
        pid_after = read_daemon_pid()
        same_pid = pid_before == pid_after
        record("13. Same version, same PID", same_pid,
               f"before={pid_before}, after={pid_after}")

    finally:
        # ══════════════════════════════════════════════════════
        # Cleanup — ALWAYS restore version
        # ══════════════════════════════════════════════════════
        print("\nCleanup...")

        # Kill daemon
        subprocess.run(["pkill", "-f", "shux __daemon"], capture_output=True, timeout=5)
        await asyncio.sleep(1)

        # Restore original version
        if version_changed:
            set_cargo_version(original_version)
            restored = get_cargo_version()
            print(f"  Restored Cargo.toml version: {restored}")

            # Rebuild with original version
            print("  Rebuilding with original version...")
            result = subprocess.run(
                ["make", "build"], cwd=PROJECT_ROOT,
                capture_output=True, text=True, timeout=120,
            )
            if result.returncode == 0:
                print("  Rebuild OK")
            else:
                print(f"  WARNING: Rebuild failed! Manual fix needed.")

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
