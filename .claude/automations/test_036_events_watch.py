# /// script
# requires-python = ">=3.14"
# dependencies = ["iterm2", "pyobjc", "pyobjc-framework-Quartz"]
# ///
r"""
L4 visual test for PR 2a — events.watch RPC + CLI.

Demonstrates the agent-killer feature: an external watcher subscribes to the
daemon's typed event bus and sees lifecycle mutations stream in real time.
The convergent council finding (codex + gemini) was that this is the single
highest-value gap vs tmux for AI agent orchestration; this test proves the
end-to-end path works against a real daemon.

Layout:
  ┌────────────────────────────┬────────────────────────────┐
  │  pane A:                   │  pane B:                   │
  │  shux events watch \       │  (driver — runs `shux new` │
  │      --filter session.,    │   `shux pane split`,       │
  │      --filter window.,     │   `shux kill`, etc.)       │
  │      --filter pane.        │                            │
  │                            │                            │
  │  ← JSON Lines stream here  │  ← these mutations fire    │
  │    in real time            │    events into the bus     │
  └────────────────────────────┴────────────────────────────┘

Asserts:
  E1 — `events.history` returns >0 events after a session creation
  E2 — `events.watch --limit N` blocks until events arrive, returns N
  E3 — Filtering by event-type prefix narrows the stream correctly
  E4 — Sequence numbers strictly increase across mutations
  E5 — Visual: side-by-side iTerm panes show watcher output mirroring driver actions
"""
import asyncio
import json
import os
import subprocess
import sys
import time
from datetime import datetime

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


# ─────────────────────────────────────────────────────────────────
# Headless assertions (no iTerm needed — pure CLI)
# ─────────────────────────────────────────────────────────────────

def assert_e1_history_after_create():
    """E1 — events.history returns the SessionCreated event after a mutation."""
    name = f"e1-{int(time.time())}"
    r = shux("new", "-s", name, "--detached")
    assert r.returncode == 0, f"new failed: {r.stderr}"

    r = shux("events", "history", "--filter", "session.", "-n", "20")
    assert r.returncode == 0, f"history failed: {r.stderr}"

    found = False
    for line in r.stdout.strip().splitlines():
        if not line:
            continue
        ev = json.loads(line)
        if ev["type"] == "session.created" and ev["data"]["data"]["name"] == name:
            found = True
            assert ev["seq"] >= 1, "seq must be ≥1"
            assert ev["timestamp"] > 0, "timestamp must be set"
            break
    assert found, f"E1: SessionCreated for {name!r} not found in history"

    shux("kill", "-s", name)
    print(f"  ✓ E1: events.history returned SessionCreated for {name!r}")


def assert_e2_watch_blocks_and_returns():
    """E2 — events.watch --limit N blocks until N events, then exits."""
    name = f"e2-{int(time.time())}"

    # Start the watcher BEFORE the mutation, so it's blocking when the events fire.
    proc = subprocess.Popen(
        [SHUX_BIN, "events", "watch", "--filter", "session.",
         "--limit", "2", "--timeout-ms", "3000"],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
    )
    time.sleep(0.5)  # let the subscription settle

    shux("new", "-s", name, "--detached")
    time.sleep(0.5)
    shux("kill", "-s", name)

    out, err = proc.communicate(timeout=10)
    assert proc.returncode == 0, f"watch failed (rc={proc.returncode}): {err}"

    lines = [json.loads(line) for line in out.strip().splitlines() if line]
    assert len(lines) == 2, f"E2: expected 2 events, got {len(lines)}: {out!r}"
    assert lines[0]["type"] == "session.created"
    assert lines[1]["type"] == "session.killed"
    assert lines[0]["data"]["data"]["name"] == name
    assert lines[1]["data"]["data"]["name"] == name
    print(f"  ✓ E2: watch --limit 2 returned both create + kill for {name!r}")


def assert_e3_filter_isolates():
    """E3 — --filter pane. excludes session events.

    Avoid the live-stream race by replaying from history: create the session
    first (so events are durably in the bus), THEN start the watcher with
    --from-seq=0 and --limit=2 (one for the initial pane.created on session
    spawn, one for an explicit split). Verify both lines have type starting
    with `pane.`.
    """
    name = f"e3-{int(time.time())}"
    shux("new", "-s", name, "--detached")
    # Force a second pane event so we have a deterministic count to verify.
    shux("pane", "split", "-s", name, "-d", "v")
    time.sleep(0.2)

    proc = subprocess.run(
        [SHUX_BIN, "events", "watch", "--filter", "pane.",
         "--from-seq", "0", "--limit", "2", "--timeout-ms", "1500"],
        capture_output=True, text=True, timeout=10,
    )
    if proc.returncode != 0:
        print(f"  watch stderr: {proc.stderr!r}")
    assert proc.returncode == 0, f"watch failed: {proc.stderr}"

    lines = [json.loads(line) for line in proc.stdout.strip().splitlines() if line]
    assert len(lines) == 2, f"E3: expected 2 pane events, got {len(lines)}: {proc.stdout!r}"
    for ln in lines:
        assert ln["type"].startswith("pane."), f"E3: got non-pane event {ln['type']!r}"
    shux("kill", "-s", name)
    print(f"  ✓ E3: --filter pane. returned 2 pane events from history (no session/window leak)")


def assert_e4_seq_monotonic():
    """E4 — Sequence numbers strictly increase across multiple mutations."""
    name1 = f"e4a-{int(time.time())}"
    name2 = f"e4b-{int(time.time())}"

    shux("new", "-s", name1, "--detached")
    shux("new", "-s", name2, "--detached")

    r = shux("events", "history", "--filter", "session.", "-n", "20")
    assert r.returncode == 0
    seqs = []
    for line in r.stdout.strip().splitlines():
        ev = json.loads(line)
        if ev["data"]["data"].get("name") in (name1, name2):
            seqs.append(ev["seq"])
    assert len(seqs) >= 2, f"E4: expected ≥2 events, got {seqs}"
    for prev, nxt in zip(seqs, seqs[1:]):
        assert nxt > prev, f"E4: seq not monotonic: {seqs}"

    shux("kill", "-s", name1)
    shux("kill", "-s", name2)
    print(f"  ✓ E4: seqs {seqs} strictly monotonic")


# ─────────────────────────────────────────────────────────────────
# Visual side-by-side demo
# ─────────────────────────────────────────────────────────────────

async def visual_demo(connection):
    """Pop an iTerm window with two side-by-side panes:
       LEFT: shux events watch (live stream)
       RIGHT: driver pane that fires mutations
    Take screenshots showing the events arriving in the watcher pane.
    """
    await cleanup_stale_windows(connection)
    window, left = await create_window(
        connection,
        name="events-watch-demo",
        x_pos=120, width=1500, height=750,
    )
    try:
        # Split vertically so LEFT (the watcher) and RIGHT (the driver) sit side-by-side
        right = await left.async_split_pane(vertical=True)

        # Set up env so cd-and-run picks the right binary.
        for s in (left, right):
            await s.async_send_text(f'cd "{os.path.dirname(SHUX_BIN)}"\n')
            await asyncio.sleep(0.1)
            await s.async_send_text("clear\n")

        await asyncio.sleep(0.5)

        # Start the watcher on the left.
        await left.async_send_text(
            "./shux events watch --filter session. --filter window. "
            "--filter pane. --timeout-ms 5000\n"
        )
        await asyncio.sleep(1.5)

        await screenshot(window, "036_v1_watcher_idle")

        # Drive a session create from the right.
        await right.async_send_text(
            "./shux new -s demo-events --detached\n"
        )
        await asyncio.sleep(1.2)

        await screenshot(window, "036_v2_after_session_create")

        # Drive a window.create.
        await right.async_send_text(
            "./shux window new -s demo-events -t editor\n"
        )
        await asyncio.sleep(1.2)

        await screenshot(window, "036_v3_after_window_create")

        # Drive a pane split.
        await right.async_send_text(
            "./shux pane split -s demo-events -d v\n"
        )
        await asyncio.sleep(1.2)

        await screenshot(window, "036_v4_after_pane_split")

        # Kill the session — should fire window.killed and session.killed.
        await right.async_send_text("./shux kill -s demo-events\n")
        await asyncio.sleep(1.5)

        await screenshot(window, "036_v5_after_kill")
        return True
    finally:
        await close_window(window)


async def main(connection):
    print("=" * 60)
    print("PR 2a visual test — events.watch end-to-end")
    print("=" * 60)
    if not ensure_release_build():
        print("✗ release build missing")
        sys.exit(1)

    # Headless assertions first (faster; fail-loud if the contract is broken)
    print("\n[headless assertions]")
    kill_daemon()
    failures = 0
    for fn in (
        assert_e1_history_after_create,
        assert_e2_watch_blocks_and_returns,
        assert_e3_filter_isolates,
        assert_e4_seq_monotonic,
    ):
        try:
            kill_daemon()
            fn()
        except AssertionError as e:
            failures += 1
            print(f"  ✗ {fn.__name__}: {e}")
        except Exception as e:
            failures += 1
            print(f"  ✗ {fn.__name__} CRASHED: {type(e).__name__}: {e}")

    # Visual demo (best-effort; record screenshots even if anyway)
    print("\n[visual demo]")
    kill_daemon()
    visual_ok = False
    try:
        visual_ok = await visual_demo(connection)
    except Exception as e:
        print(f"  ✗ visual_demo: {type(e).__name__}: {e}")

    print("\n" + "=" * 60)
    print(f"PR 2a visual test summary:")
    print(f"  headless: {4 - failures}/4 PASS")
    print(f"  visual:   {'OK' if visual_ok else 'FAIL'}")
    print("=" * 60)
    sys.exit(0 if failures == 0 and visual_ok else 1)


if __name__ == "__main__":
    iterm2.run_until_complete(main)
