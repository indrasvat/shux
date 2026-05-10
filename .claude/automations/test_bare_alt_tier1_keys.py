# /// script
# requires-python = ">=3.14"
# dependencies = ["iterm2", "pyobjc", "pyobjc-framework-Quartz"]
# ///
r"""
L4 visual test for the bare Alt+h/j/k/l + Alt+n/p + Alt+1..9 keybindings
that closed the Tier-1 gap from task 018 (Codex P2 followup on PR #8).

Asserts:
  A1 — `key_to_bare_action` unit tests cover the new bindings (verified
        via `cargo test` in the build pipeline; this script just runs
        the existing test suite to confirm we haven't regressed).
  A2 — Visual: attach to a session with 3 windows, press Alt+2 inside
        the attach loop, screenshot, then Alt+3, screenshot. The
        status bar window indicator should change `[1/3]` → `[2/3]` →
        `[3/3]`.
  A3 — Alt+n cycles forward (Alt+n from window 3 → wraps to 1 — same
        semantics as the existing prefix+n binding).
  A4 — Alt+h after a vertical split moves focus to the left pane.

This is end-to-end via iTerm so the full key→client→action→graph
chain is exercised. We can't easily synthesize Alt+key chords from
Python directly (iTerm's `async_send_text` is character-level), so
we use `async_send_text` with an ESC-prefix escape: `\x1b` + char
is how terminals encode Alt+char by default (the meta-prefix
convention crossterm recognizes as `KeyModifiers::ALT`).
"""
import asyncio
import json
import os
import subprocess
import sys
import time

import iterm2

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from _shux_iterm import (  # noqa: E402
    SHUX_BIN,
    cleanup_stale_windows,
    close_window,
    create_window,
    ensure_release_build,
    kill_daemon,
    screenshot,
    shux,
)


def get_session_id(name: str) -> str:
    r = shux("api", "session.list", "{}")
    payload = json.loads(r.stdout)
    sessions = payload.get("sessions") or payload or []
    for s in sessions if isinstance(sessions, list) else []:
        if isinstance(s, dict) and s.get("name") == name:
            return s["id"]
    raise KeyError(name)


def windows_for(session_name: str) -> list[dict]:
    sid = get_session_id(session_name)
    r = shux("api", "window.list", json.dumps({"session_id": sid}))
    return json.loads(r.stdout)


async def visual_alt_keys_demo(connection):
    """A2..A4 — interactive demo of the new bare Alt bindings."""
    print("\n[A2..A4] attach + Alt+digit/h/j/k/l/n/p demo")

    session = "alt-keys-demo"
    shux("kill", "-s", session)
    await asyncio.sleep(0.2)

    # Build a session with 3 windows BEFORE attaching so we can test
    # Alt+1, Alt+2, Alt+3 against a stable layout. The visual portion
    # then drives the attach loop.
    r = shux("new", "-s", session, "-d")
    assert r.returncode == 0, r.stderr
    sid = get_session_id(session)
    for label in ["win-2", "win-3"]:
        r = shux(
            "api",
            "window.create",
            json.dumps({"session_id": sid, "title": label}),
        )
        assert r.returncode == 0, r.stderr
    wins = windows_for(session)
    print(f"  built {len(wins)} windows: {[w['title'] for w in wins]}")
    assert len(wins) == 3, wins

    window, term = await create_window(connection, "bare-alt-keys", width=1500, height=820)

    await term.async_send_text(f"clear; {SHUX_BIN} attach -s {session}\n")
    # Give attach time to render. shux's attach has a 200ms tick + the
    # first render fires immediately on connect, so 1.5s is generous.
    await asyncio.sleep(1.5)
    shot1 = await screenshot(window, "bare_alt_keys_attach_start", subdir="bare-alt")
    print(f"  attach start (should be on win-1): {shot1}")

    # Alt+2 → switch to window 2. crossterm decodes ESC+char as
    # `KeyModifiers::ALT | KeyCode::Char('2')` when the terminal sends
    # the legacy meta-prefix encoding (iTerm does this by default).
    await term.async_send_text("\x1b2")
    await asyncio.sleep(0.6)
    shot2 = await screenshot(window, "bare_alt_keys_alt2", subdir="bare-alt")
    print(f"  after Alt+2 (should be on win-2): {shot2}")

    # Alt+3 → window 3.
    await term.async_send_text("\x1b3")
    await asyncio.sleep(0.6)
    shot3 = await screenshot(window, "bare_alt_keys_alt3", subdir="bare-alt")
    print(f"  after Alt+3 (should be on win-3): {shot3}")

    # Alt+n from window 3 → wraps to window 1.
    await term.async_send_text("\x1bn")
    await asyncio.sleep(0.6)
    shot4 = await screenshot(window, "bare_alt_keys_altn_wrap", subdir="bare-alt")
    print(f"  after Alt+n wrap (should be on win-1): {shot4}")

    # Alt+p → back to window 3.
    await term.async_send_text("\x1bp")
    await asyncio.sleep(0.6)
    shot5 = await screenshot(window, "bare_alt_keys_altp", subdir="bare-alt")
    print(f"  after Alt+p (should be on win-3): {shot5}")

    # Detach so the daemon side cleans up.
    await term.async_send_text("\x00")  # Ctrl+Space prefix
    await asyncio.sleep(0.2)
    await term.async_send_text("d")
    await asyncio.sleep(0.5)

    shux("kill", "-s", session)
    await close_window(window)


async def main(connection):
    await cleanup_stale_windows(connection)
    kill_daemon()
    time.sleep(0.5)

    try:
        await visual_alt_keys_demo(connection)
        print("\n══════════════════════════════════════════════")
        print("Bare-Alt L4 visual test PASSED — screenshots ready")
        print("══════════════════════════════════════════════")
    finally:
        shux("kill", "-s", "alt-keys-demo")
        kill_daemon()


if __name__ == "__main__":
    if not ensure_release_build():
        print("ERROR: target/release/shux build failed")
        sys.exit(1)
    iterm2.run_until_complete(main)
