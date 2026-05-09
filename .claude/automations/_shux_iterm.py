"""
Shared iTerm2 automation helpers for shux visual tests.

Implements the patterns from the iterm2-driver skill:
- Janitor at script start (close orphans from prior crashed runs)
- Window creation that refreshes after the stale-object init bug
- Position-based Quartz screenshot correlation (no focus required)
- Multi-level cleanup ready for use in try/finally blocks

Every shux automation script in this folder should import from here.
"""

import asyncio
import os
import subprocess
from datetime import datetime

import iterm2


# Project root and screenshot directory live one level above this file.
_THIS = os.path.abspath(__file__)
PROJECT_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(_THIS)))
SCREENSHOT_DIR = os.path.join(PROJECT_ROOT, ".claude", "screenshots")
SHUX_BIN = os.path.join(PROJECT_ROOT, "target", "release", "shux")

# Sessions whose name matches this prefix are considered ours and may be
# closed by the janitor. Keep it specific so we don't clobber a user
# tab that happens to be named "test".
SHUX_AUTO_PREFIX = "shux-auto-"


# ---------------------------------------------------------------------
# Quartz screenshot correlation (position-based, no focus needed)
# ---------------------------------------------------------------------
try:
    import Quartz  # noqa: F401

    def _quartz_window_list():
        return Quartz.CGWindowListCopyWindowInfo(
            Quartz.kCGWindowListOptionOnScreenOnly
            | Quartz.kCGWindowListExcludeDesktopElements,
            Quartz.kCGNullWindowID,
        )
except ImportError:  # pragma: no cover - dev environments without pyobjc
    def _quartz_window_list():
        return []


# ---------------------------------------------------------------------
# Janitor + window creation
# ---------------------------------------------------------------------

async def cleanup_stale_windows(connection, prefix: str = SHUX_AUTO_PREFIX) -> int:
    """Close any iTerm windows whose first session is named with `prefix`.
    Run at the start of every script to recover from crashed prior runs.
    Returns the number of windows closed."""
    app = await iterm2.async_get_app(connection)
    closed = 0
    for window in list(app.terminal_windows):
        # Inspect every session in the window; if any matches our prefix,
        # nuke the whole window (it's ours).
        ours = False
        for tab in window.tabs:
            for session in tab.sessions:
                name = session.name or ""
                if name.startswith(prefix):
                    ours = True
                    break
            if ours:
                break
        if not ours:
            continue
        for tab in list(window.tabs):
            for session in list(tab.sessions):
                try:
                    await session.async_send_text("\x03")
                    await asyncio.sleep(0.05)
                    await session.async_send_text("exit\n")
                    await asyncio.sleep(0.05)
                except Exception:
                    pass
                try:
                    await session.async_close(force=True)
                except Exception:
                    pass
        closed += 1
    return closed


async def create_window(
    connection,
    name: str,
    *,
    x_pos: int = 100,
    y_pos: int | None = None,
    width: int = 1100,
    height: int = 700,
):
    """Create an isolated iTerm window for this script.

    Implements the workaround for the stale-window-object bug:
    `Window.async_create()` returns BEFORE iTerm finishes init, so the
    returned object's `current_tab` is None. We sleep, refresh via
    `async_get_app()`, then probe for readiness.

    Returns `(window, session)`.
    """
    full_name = f"{SHUX_AUTO_PREFIX}{name}"
    window = await iterm2.Window.async_create(connection)
    await asyncio.sleep(0.5)  # let iTerm init the window

    # Refresh — the returned window is stale; get the live one from the app.
    app = await iterm2.async_get_app(connection)
    if window.current_tab is None:
        for w in app.terminal_windows:
            if w.window_id == window.window_id:
                window = w
                break

    # Readiness probe.
    for _ in range(20):
        if window.current_tab and window.current_tab.current_session:
            break
        await asyncio.sleep(0.2)
    if not window.current_tab or not window.current_tab.current_session:
        raise RuntimeError(f"window {full_name!r} not ready after refresh")

    session = window.current_tab.current_session
    await session.async_set_name(full_name)

    # Position the window: unique x_pos lets the Quartz screenshot
    # correlator pick the right window even if there are several open.
    frame = await window.async_get_frame()
    target_y = y_pos if y_pos is not None else frame.origin.y
    await window.async_set_frame(
        iterm2.Frame(
            iterm2.Point(x_pos, target_y),
            iterm2.Size(width, height),
        )
    )
    await asyncio.sleep(0.3)
    return window, session


async def close_window(window) -> None:
    """Force-close every session in a window. Safe to call from finally."""
    if window is None:
        return
    for tab in list(window.tabs):
        for session in list(tab.sessions):
            try:
                await session.async_send_text("\x03")
                await asyncio.sleep(0.05)
            except Exception:
                pass
            try:
                await session.async_close(force=True)
            except Exception:
                pass


# ---------------------------------------------------------------------
# Screenshots
# ---------------------------------------------------------------------

async def screenshot(window, name: str, *, subdir: str = "") -> str | None:
    """Capture a screenshot of `window` using Quartz position correlation.

    Works without focus and without minimization tricks. Returns the
    filepath if successful, None if no matching window was found.
    """
    out_dir = os.path.join(SCREENSHOT_DIR, subdir) if subdir else SCREENSHOT_DIR
    os.makedirs(out_dir, exist_ok=True)
    fp = os.path.join(out_dir, f"{name}_{datetime.now():%Y%m%d_%H%M%S}.png")

    frame = await window.async_get_frame()
    best_id, best_score = None, float("inf")
    for w in _quartz_window_list():
        if "iTerm" not in w.get("kCGWindowOwnerName", ""):
            continue
        b = w.get("kCGWindowBounds", {})
        score = (
            abs(float(b.get("X", 0)) - frame.origin.x) * 2
            + abs(float(b.get("Width", 0)) - frame.size.width)
            + abs(float(b.get("Height", 0)) - frame.size.height)
        )
        if score < best_score:
            best_score, best_id = score, w.get("kCGWindowNumber")

    if best_id is None or best_score >= 30:
        # Fall back: capture the whole display rather than nothing.
        subprocess.run(["screencapture", "-x", "-D", "1", fp], check=False)
        print(f"  [shot:fullscreen] {fp} (no Quartz match, score={best_score})")
        return fp

    subprocess.run(["screencapture", "-x", "-l", str(best_id), fp], check=False)
    print(f"  [shot] {fp}")
    return fp


# ---------------------------------------------------------------------
# Shux-side helpers
# ---------------------------------------------------------------------

def shux(*args, timeout: int = 10) -> subprocess.CompletedProcess:
    """Run a shux subcommand; return the CompletedProcess."""
    return subprocess.run(
        [SHUX_BIN, *args],
        capture_output=True,
        text=True,
        timeout=timeout,
        cwd=PROJECT_ROOT,
    )


def kill_daemon() -> None:
    """Best-effort: kill any running shux daemon and let it settle."""
    subprocess.run(["pkill", "-f", "shux.*__daemon"], capture_output=True)
    import time as _time
    _time.sleep(0.4)


def ensure_release_build() -> bool:
    """Build target/release/shux if it isn't there. Returns True on success."""
    if os.path.exists(SHUX_BIN):
        return True
    proc = subprocess.run(
        ["cargo", "build", "--release", "-p", "shux"],
        cwd=PROJECT_ROOT,
        capture_output=True,
        text=True,
        timeout=240,
    )
    if proc.returncode == 0 and os.path.exists(SHUX_BIN):
        return True
    print(proc.stderr[-500:])
    return False
