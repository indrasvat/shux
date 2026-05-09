# /// script
# requires-python = ">=3.14"
# dependencies = [
#   "iterm2",
#   "pyobjc",
#   "pyobjc-framework-Quartz",
# ]
# ///
"""
Task 017 Real-Apps Visual Demo — full-screen interactive tools inside shux.

Drives shux attach through real interactive workflows that exercise
PTY/VT/render plumbing end-to-end, then captures screenshots:

  Demo 1: `top` running in a full-screen single pane
  Demo 2: 2-pane split — `top` (left) + Python http server (right)
  Demo 3: 3-pane grid — `top` (top-left), `httpd` (right), `curl loop` (bottom-left)
  Demo 4: `gemini` CLI inside a pane, with a question typed and answered
  Demo 5: vim opens a file inside a pane

Each pane runs the real binary on a real PTY; keystrokes pass through the
attach client → daemon → pane writer → PTY → child. Screenshots prove the
whole pipeline works.

Usage:
    uv run .claude/automations/test_017_real_apps.py
"""

import iterm2
import asyncio
import subprocess
import os
import time
from datetime import datetime

PROJECT_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SCREENSHOT_DIR = os.path.join(PROJECT_ROOT, ".claude", "screenshots")
SHUX_BIN = os.path.join(PROJECT_ROOT, "target", "release", "shux")


def kill_daemon():
    subprocess.run(["pkill", "-f", "shux.*__daemon"], capture_output=True)
    time.sleep(0.4)


def run_shux(*args, timeout=10):
    return subprocess.run(
        [SHUX_BIN, *args], capture_output=True, text=True, timeout=timeout, cwd=PROJECT_ROOT
    )


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


def shot(name: str) -> str:
    os.makedirs(SCREENSHOT_DIR, exist_ok=True)
    fp = os.path.join(SCREENSHOT_DIR, f"017real_{name}_{datetime.now():%Y%m%d_%H%M%S}.png")
    wid = get_iterm2_window_id()
    if wid:
        subprocess.run(["screencapture", "-x", "-l", str(wid), fp], check=False)
    else:
        subprocess.run(["screencapture", "-x", fp], check=False)
    print(f"  [shot] {fp}")
    return fp


async def detach(session):
    await session.async_send_text("\x00d")
    await asyncio.sleep(1.5)


async def attach(session, sname: str):
    """Send `shux attach -s <name>` to the iTerm session."""
    await session.async_send_text(f"{SHUX_BIN} attach -s {sname}\r")
    await asyncio.sleep(2.0)


async def main(connection):
    print("\n" + "#" * 60)
    print("# shux task 017 — REAL APPS visual demo")
    print(f"# Started: {datetime.now():%Y-%m-%d %H:%M:%S}")
    print("#" * 60)

    # Build & start daemon clean
    print("\n[setup] cargo build --release")
    b = subprocess.run(
        ["cargo", "build", "--release", "-p", "shux"],
        cwd=PROJECT_ROOT, capture_output=True, text=True, timeout=240,
    )
    if b.returncode != 0:
        print(b.stderr[-500:])
        return 1
    kill_daemon()
    run_shux("ls")
    time.sleep(1.0)

    app = await iterm2.async_get_app(connection)
    window = app.current_terminal_window
    if not window:
        print("ERROR: no iTerm2 window")
        return 1
    tab = await window.async_create_tab()
    session = tab.current_session
    await asyncio.sleep(0.5)

    shots = []

    try:
        # ──────────────────────────────────────────────────────────
        # Demo 1: full-screen top
        # ──────────────────────────────────────────────────────────
        print("\n[demo 1] full-screen `top`")
        run_shux("kill", "-s", "demo1")
        run_shux("new", "-s", "demo1", "--detached")
        await attach(session, "demo1")
        await session.async_send_text("top\r")
        # Give top time to gather two sample windows so the header is filled.
        await asyncio.sleep(4.0)
        shots.append(shot("01_top_fullscreen"))
        # Quit top, detach
        await session.async_send_text("q")
        await asyncio.sleep(0.6)
        await detach(session)

        # ──────────────────────────────────────────────────────────
        # Demo 2: top + http server side-by-side
        # ──────────────────────────────────────────────────────────
        print("\n[demo 2] top + http server (vertical split)")
        run_shux("kill", "-s", "demo2")
        run_shux("new", "-s", "demo2", "--detached")
        await attach(session, "demo2")
        # Right-pane (new) starts as focus after vertical split.
        await session.async_send_text("\x00|")
        await asyncio.sleep(0.8)
        # In the right pane: launch a tiny Python HTTP server in /tmp/shuxdemo
        await session.async_send_text(
            "mkdir -p /tmp/shuxdemo && cd /tmp/shuxdemo && "
            "echo '<h1>Hello from shux pane</h1>' > index.html && "
            "python3 -m http.server 9876\r"
        )
        await asyncio.sleep(2.0)
        # Move focus left and launch top
        await session.async_send_text("\x00h")
        await asyncio.sleep(0.4)
        await session.async_send_text("top\r")
        await asyncio.sleep(4.0)
        shots.append(shot("02_top_plus_httpserver"))

        # ──────────────────────────────────────────────────────────
        # Demo 3: 3-pane grid — top + httpd + curl traffic
        # ──────────────────────────────────────────────────────────
        print("\n[demo 3] 3-pane grid + live curl traffic")
        # We're focused on top (left). Split it horizontally to add a third pane.
        await session.async_send_text("\x00-")
        await asyncio.sleep(0.8)
        # The new pane is our 'curl loop' pane (bottom-left).
        await session.async_send_text(
            "for i in 1 2 3 4 5 6 7 8; do "
            "echo \"--- request $i ---\"; "
            "curl -s -i http://127.0.0.1:9876/ | head -5; "
            "sleep 0.6; done\r"
        )
        await asyncio.sleep(7.0)
        shots.append(shot("03_three_pane_grid"))

        # Kill curl pane and the http pane to clean up
        await session.async_send_text("\x00x")  # kill bottom-left (curl)
        await asyncio.sleep(0.4)
        await session.async_send_text("\x00l")  # focus right (httpd)
        await asyncio.sleep(0.3)
        await session.async_send_text("\x03")  # Ctrl+C to httpd
        await asyncio.sleep(0.5)
        await detach(session)

        # ──────────────────────────────────────────────────────────
        # Demo 4: gemini CLI inside a pane
        # ──────────────────────────────────────────────────────────
        print("\n[demo 4] gemini CLI inside a pane")
        run_shux("kill", "-s", "demo4")
        run_shux("new", "-s", "demo4", "--detached")
        await attach(session, "demo4")
        # Run gemini with a non-interactive prompt. -p makes it print and exit.
        await session.async_send_text(
            "gemini -m gemini-2.5-flash -p "
            "'In exactly 12 words, what is shux? It is a Rust terminal "
            "multiplexer with a typed JSON-RPC API for AI agents.'\r"
        )
        # gemini takes ~5-15s; capture once we expect output to be in.
        await asyncio.sleep(15.0)
        shots.append(shot("04_gemini_cli"))
        await detach(session)

        # ──────────────────────────────────────────────────────────
        # Demo 5: codex CLI side-by-side with a tail of the README
        # ──────────────────────────────────────────────────────────
        print("\n[demo 5] codex side-by-side with `tail -f` log")
        run_shux("kill", "-s", "demo5")
        run_shux("new", "-s", "demo5", "--detached")
        await attach(session, "demo5")
        # Vertical split. Right pane: tail of README. Left pane: codex.
        await session.async_send_text("\x00|")
        await asyncio.sleep(0.6)
        # Right pane just got focused after vsplit
        await session.async_send_text(f"head -40 {PROJECT_ROOT}/README.md\r")
        await asyncio.sleep(1.0)
        # Focus left, run codex
        await session.async_send_text("\x00h")
        await asyncio.sleep(0.3)
        await session.async_send_text(
            "codex exec --model gpt-5.4 "
            "'In one sentence: what is a tmux replacement?'\r"
        )
        await asyncio.sleep(20.0)
        shots.append(shot("05_codex_side_by_side"))
        await detach(session)

    except Exception as e:
        print(f"ERROR: {e}")
        import traceback
        traceback.print_exc()

    finally:
        for s in ["demo1", "demo2", "demo3", "demo4", "demo5"]:
            run_shux("kill", "-s", s)

    print("\n" + "=" * 60)
    print(f"{len(shots)} screenshots captured:")
    for s in shots:
        print(f"  {s}")
    print("=" * 60)
    return 0


if __name__ == "__main__":
    rc = iterm2.run_until_complete(main, retry=False)
    raise SystemExit(rc)
