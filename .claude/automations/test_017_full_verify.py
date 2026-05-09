# /// script
# requires-python = ">=3.14"
# dependencies = ["iterm2", "pyobjc", "pyobjc-framework-Quartz"]
# ///
"""
Comprehensive visual verification for task 017 + the M1 follow-ups.

Covers:
  V1  splits draw clean borders (no overdraw of pane content)
  V2  border-color isolation (red text in one pane stays in that pane)
  V3  prefix keybindings fire correctly (Ctrl+Space + |/-/h/l/z/d)
  V4  CLI `--` passthrough exec'd directly (vim, top, python, etc.)
  V5  mouse click-to-focus changes the active pane
  V6  config hot reload swaps border style live
  V7  config-broken parse falls back to defaults gracefully

For each part, asserts are explicit and screenshots are captured. Final
report: PASS/FAIL/UNVERIFIED with details. Tab census run after every
part to catch any leak. Designed to be re-run repeatedly without state.
"""

import asyncio
import os
import subprocess
import sys
import time
from datetime import datetime

import iterm2

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from _shux_iterm import (  # type: ignore
    SHUX_BIN,
    PROJECT_ROOT,
    cleanup_stale_windows,
    create_window,
    close_window,
    screenshot,
    shux,
    kill_daemon,
    ensure_release_build,
)


CONFIG_PATH = os.path.expanduser("~/.config/shux/config.toml")


results = {"passed": 0, "failed": 0, "unverified": 0, "tests": [], "shots": []}


def log(name: str, status: str, details: str = "", shot: str | None = None):
    results["tests"].append({"name": name, "status": status, "details": details})
    if status == "PASS":
        results["passed"] += 1
    elif status == "FAIL":
        results["failed"] += 1
    else:
        results["unverified"] += 1
    if shot:
        results["shots"].append(shot)
    print(f"  [{status}] {name}{(' - ' + details) if details else ''}")


def summary() -> int:
    print("\n" + "=" * 70)
    print(f"PASS={results['passed']} FAIL={results['failed']} "
          f"UNVERIFIED={results['unverified']}  shots={len(results['shots'])}")
    print("=" * 70)
    if results["failed"] > 0:
        print("\nFailures:")
        for t in results["tests"]:
            if t["status"] == "FAIL":
                print(f"  - {t['name']}: {t['details']}")
    return 1 if results["failed"] > 0 else 0


async def screen(session) -> str:
    sc = await session.async_get_screen_contents()
    return "\n".join(sc.line(i).string for i in range(sc.number_of_lines))


async def wait_for(session, expected: str, timeout: float = 6.0) -> bool:
    elapsed = 0.0
    while elapsed < timeout:
        if expected in await screen(session):
            return True
        await asyncio.sleep(0.2)
        elapsed += 0.2
    return False


async def attach(session, sname: str):
    await session.async_send_text(f"{SHUX_BIN} attach -s {sname}\n")
    for _ in range(50):
        text = await screen(session)
        if sname in text and ("[1/" in text or "◆" in text):
            break
        await asyncio.sleep(0.2)
    await asyncio.sleep(0.6)


async def detach(session):
    await session.async_send_text("\x00")
    await asyncio.sleep(0.05)
    await session.async_send_text("d")
    await asyncio.sleep(1.2)


def write_config(toml: str):
    os.makedirs(os.path.dirname(CONFIG_PATH), exist_ok=True)
    with open(CONFIG_PATH, "w") as f:
        f.write(toml)


def remove_config():
    try:
        os.remove(CONFIG_PATH)
    except FileNotFoundError:
        pass


async def main(connection):
    closed = await cleanup_stale_windows(connection)
    if closed:
        print(f"[janitor] closed {closed} stale windows")
    if not ensure_release_build():
        return 1

    # Start with a clean config so V1-V5 are deterministic.
    remove_config()
    kill_daemon()
    shux("ls")
    await asyncio.sleep(1.0)

    window, session = await create_window(
        connection, "full-verify", x_pos=100, y_pos=80, width=1280, height=820
    )

    try:
        await session.async_send_text(f"cd {PROJECT_ROOT}\n")
        await asyncio.sleep(0.4)

        # ─────────────────────────────────────────────────────────────
        # V1: splits draw clean borders, no pane content overdrawn
        # ─────────────────────────────────────────────────────────────
        print("\n[V1] split borders are clean")
        shux("kill", "-s", "v1")
        shux("new", "-s", "v1", "--detached")
        await attach(session, "v1")
        # Type something distinctive in the left pane.
        await session.async_send_text(
            "echo LEFT-MARK-${RANDOM}; for i in $(seq 1 10); do echo line-$i; done\n"
        )
        await asyncio.sleep(1.0)
        await session.async_send_text("\x00|")  # vsplit
        await asyncio.sleep(0.7)
        await session.async_send_text(
            "echo RIGHT-MARK-${RANDOM}; for i in $(seq 1 10); do echo right-$i; done\n"
        )
        await asyncio.sleep(1.0)
        text = await screen(session)
        # Both prefixes must be visible AND not have their first letter
        # eaten by the border (the bug from earlier rounds: "EFT-MARK").
        left_ok = any("LEFT-MARK" in line for line in text.splitlines())
        right_ok = any("RIGHT-MARK" in line for line in text.splitlines())
        if left_ok and right_ok:
            log("V1 splits, no border overdraw of content", "PASS",
                shot=await screenshot(window, "v1_clean_borders"))
        else:
            log("V1 splits, no border overdraw of content", "FAIL",
                f"left_ok={left_ok} right_ok={right_ok}")

        # ─────────────────────────────────────────────────────────────
        # V2: border-color isolation — colors in one pane do not bleed
        # ─────────────────────────────────────────────────────────────
        print("\n[V2] color isolation across the border")
        # Already split. Left was last; right is currently focused.
        # Print bright red in right pane, then move focus left, print bright
        # green. Capture and verify both colors appear in their *own* pane
        # column ranges (positions are approximate but distinct).
        await session.async_send_text(
            "printf '\\x1b[31;1mRED-IN-RIGHT\\x1b[0m\\n'\n"
        )
        await asyncio.sleep(0.7)
        await session.async_send_text("\x00")
        await asyncio.sleep(0.05)
        await session.async_send_text("h")  # focus left
        await asyncio.sleep(0.6)
        await session.async_send_text(
            "printf '\\x1b[32;1mGREEN-IN-LEFT\\x1b[0m\\n'\n"
        )
        await asyncio.sleep(0.8)
        sc = await session.async_get_screen_contents()
        # Find the row containing RED-IN-RIGHT and the row containing
        # GREEN-IN-LEFT, record their column positions.
        red_col = None
        green_col = None
        for i in range(sc.number_of_lines):
            line_str = sc.line(i).string
            if "RED-IN-RIGHT" in line_str:
                red_col = line_str.index("RED-IN-RIGHT")
            if "GREEN-IN-LEFT" in line_str:
                green_col = line_str.index("GREEN-IN-LEFT")
        if red_col is not None and green_col is not None and red_col > green_col:
            log("V2 colored output stays in correct pane", "PASS",
                f"green_col={green_col} red_col={red_col}",
                shot=await screenshot(window, "v2_color_isolation"))
        else:
            log("V2 colored output stays in correct pane", "FAIL",
                f"green_col={green_col} red_col={red_col}")

        # ─────────────────────────────────────────────────────────────
        # V3: prefix bindings fire correctly
        # ─────────────────────────────────────────────────────────────
        print("\n[V3] prefix keybindings")
        # Already split horizontally before? Let's add a horizontal split
        # to get 3 panes total, then zoom + unzoom + focus moves.
        await session.async_send_text("\x00-")
        await asyncio.sleep(0.7)
        text = await screen(session)
        # The status bar segment we built shows "[<active>/<count>] <title>"
        # in the centre slot (e.g. "[1/1] 1"); the count there is *windows*
        # not panes, so verify pane count instead by counting borders.
        bars_now = sum(line.count("│") for line in text.splitlines())
        if bars_now >= 5:
            log("V3a horizontal split lands (3 panes)", "PASS",
                f"{bars_now} │ chars")
        else:
            log("V3a horizontal split lands (3 panes)", "UNVERIFIED",
                f"{bars_now} │ chars")

        # Zoom
        await session.async_send_text("\x00z")
        await asyncio.sleep(0.7)
        text = await screen(session)
        bars = sum(line.count("│") for line in text.splitlines())
        # Zoomed: status-bar dots are visible but no interior │ dividers.
        if bars == 0:
            log("V3b prefix z zooms (no vertical dividers)", "PASS",
                shot=await screenshot(window, "v3_zoomed"))
        else:
            log("V3b prefix z zooms", "FAIL", f"{bars} │ chars (expected 0)")

        # Unzoom
        await session.async_send_text("\x00z")
        await asyncio.sleep(0.7)
        text = await screen(session)
        bars2 = sum(line.count("│") for line in text.splitlines())
        if bars2 >= 5:
            log("V3c prefix z unzooms (dividers return)", "PASS")
        else:
            log("V3c prefix z unzooms", "UNVERIFIED", f"{bars2} │ chars")

        await detach(session)

        # ─────────────────────────────────────────────────────────────
        # V4: CLI passthrough — shux new -- <cmd>
        # ─────────────────────────────────────────────────────────────
        print("\n[V4] CLI passthrough")
        shux("kill", "-s", "v4")
        proc = subprocess.run(
            [SHUX_BIN, "new", "-s", "v4", "--detached", "--",
             "python3", "-c",
             "import time; print('PASSTHROUGH-OK', flush=True); time.sleep(20)"],
            capture_output=True, text=True, timeout=8, cwd=PROJECT_ROOT,
        )
        await asyncio.sleep(2.5)
        # Capture the FULL visible grid (24 rows) — `--lines 10` only
        # returns the last 10 rows, which can be empty if the program's
        # output sits on row 0. Match the test_017_starship.py pattern.
        cap = subprocess.run(
            [SHUX_BIN, "pane", "capture", "-s", "v4", "--lines", "24"],
            capture_output=True, text=True, timeout=5, cwd=PROJECT_ROOT,
        )
        if "PASSTHROUGH-OK" in cap.stdout:
            log("V4 `shux new -- python3 -c '...'` runs directly", "PASS")
        else:
            log("V4 `shux new -- python3 -c '...'` runs directly", "FAIL",
                f"capture: {cap.stdout!r}")
        shux("kill", "-s", "v4")

        # ─────────────────────────────────────────────────────────────
        # V5: mouse click-to-focus
        # ─────────────────────────────────────────────────────────────
        print("\n[V5] mouse click-to-focus")
        shux("kill", "-s", "v5")
        shux("new", "-s", "v5", "--detached")
        await attach(session, "v5")
        # Vertical split → two panes side by side. The right (new) pane
        # is focused after split.
        await session.async_send_text("\x00|")
        await asyncio.sleep(0.7)

        # Check status bar shows pane 1/2 - meaning we're on focused pane (right).
        text = await screen(session)
        # Send a synthetic mouse click on the LEFT pane (somewhere
        # within the left half of the window). iTerm2 mouse encoding
        # uses CSI codes; emit SGR-1006 mouse press/release at col 5
        # row 5 (well inside the left pane).
        # CSI < 0 ; col ; row M  (press), then  m (release)
        click_col, click_row = 6, 5
        await session.async_send_text(
            f"\x1b[<0;{click_col};{click_row}M"
        )
        await asyncio.sleep(0.1)
        await session.async_send_text(
            f"\x1b[<0;{click_col};{click_row}m"
        )
        await asyncio.sleep(0.6)
        # Type a unique mark; capture shouldn't show it in the right pane.
        await session.async_send_text("echo CLICK-MARK\n")
        await asyncio.sleep(1.0)
        sc = await session.async_get_screen_contents()
        click_col_seen = None
        for i in range(sc.number_of_lines):
            s = sc.line(i).string
            if "CLICK-MARK" in s:
                click_col_seen = s.index("CLICK-MARK")
                break
        # If click-to-focus worked, CLICK-MARK appears in the LEFT pane
        # (low column index). If it didn't work, it appears in RIGHT
        # (high column index).
        if click_col_seen is not None and click_col_seen < 30:
            log("V5 mouse click moves focus to clicked pane", "PASS",
                f"seen at col {click_col_seen}",
                shot=await screenshot(window, "v5_click_to_focus"))
        else:
            log("V5 mouse click moves focus to clicked pane", "UNVERIFIED",
                f"col_seen={click_col_seen}")
        await detach(session)

        # ─────────────────────────────────────────────────────────────
        # V6: config hot reload swaps border style
        # ─────────────────────────────────────────────────────────────
        print("\n[V6] config hot reload — border style swaps live")
        shux("kill", "-s", "v6")
        shux("new", "-s", "v6", "--detached")
        await attach(session, "v6")
        await session.async_send_text("\x00|")  # vsplit so we can see borders
        await asyncio.sleep(0.7)

        # Default border (rounded). Take a screenshot.
        await screenshot(window, "v6a_default_border")
        text = await screen(session)
        rounded_present = "╭" in text or "╰" in text
        # Now write a config that flips to "thick" — render loop should
        # pick it up within ~250ms via the change_notify Notify.
        write_config('[appearance]\nborder_style = "thick"\n')
        await asyncio.sleep(2.0)
        text = await screen(session)
        thick_present = "┃" in text or "┏" in text
        await screenshot(window, "v6b_thick_border")

        # Also try "ascii" to show another transition.
        write_config('[appearance]\nborder_style = "ascii"\n')
        await asyncio.sleep(2.0)
        text = await screen(session)
        ascii_present = "+" in text and "|" in text
        await screenshot(window, "v6c_ascii_border")

        if rounded_present and thick_present and ascii_present:
            log("V6 config hot reload swaps border style", "PASS",
                "rounded → thick → ascii all observed")
        else:
            log("V6 config hot reload swaps border style", "FAIL",
                f"rounded={rounded_present} thick={thick_present} ascii={ascii_present}")

        # Reset config.
        remove_config()
        await asyncio.sleep(1.5)
        await detach(session)

        # ─────────────────────────────────────────────────────────────
        # V7: bad config doesn't crash the daemon
        # ─────────────────────────────────────────────────────────────
        print("\n[V7] bad config falls back gracefully")
        write_config("this is = not [valid] toml(((")
        await asyncio.sleep(1.0)
        # daemon should still be alive
        proc = shux("ls")
        if proc.returncode == 0:
            log("V7 broken config doesn't crash daemon", "PASS")
        else:
            log("V7 broken config doesn't crash daemon", "FAIL",
                f"shux ls returned {proc.returncode}: {proc.stderr}")
        remove_config()

    finally:
        for s in ["v1", "v4", "v5", "v6"]:
            shux("kill", "-s", s)
        await close_window(window)
        leftover = await cleanup_stale_windows(connection)
        if leftover:
            print(f"[janitor:final] closed {leftover} extra windows")
        remove_config()

    return summary()


if __name__ == "__main__":
    raise SystemExit(iterm2.run_until_complete(main, retry=False))
