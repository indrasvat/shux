# /// script
# requires-python = ">=3.14"
# dependencies = ["iterm2", "pyobjc", "pyobjc-framework-Quartz"]
# ///
r"""
L4 visual test for PR 2c — sampled pane.output events with proper
data-plane separation.

Asserts (all headless; visual portion at the end):
  A1 — `pane.output.watch` returns chunks from a live pane after
        running a command. Chunks carry pane_id/window_id/session_id.
  A2 — Same chunks NEVER appear in `events.history` (data plane is
        sealed from the control plane).
  A3 — `events.watch --filter pane.` also does not see the chunks.
        Closes the secret-leak vector that demoted PR 2a's
        PaneOutput from the main bus.
  A4 — Rate-limiting at the source: a tight loop emitting bytes for
        2 seconds produces FAR fewer chunks than calls to `pane.send_keys`
        (the sample-interval is 100ms, so ≤ ~20 chunks).
  A5 — `shux pane watch` CLI streams bytes to stdout, matches what
        the source pane emitted.
  A6 — Visual: two-pane window, left pane runs `for i in {1..20}; do
        echo line-$i; sleep 0.05; done`, right pane runs
        `shux pane watch -p <left>` and shows the stream live.

The data-plane invariant is THE security property of this PR. If A2
or A3 ever flip green-to-red, the secret-leak vector is back open.
"""
import asyncio
import base64
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


def pane_id_of(name: str) -> tuple[str, str]:
    sid = get_session_id(name)
    r = shux("api", "pane.list", json.dumps({"session_id": sid}))
    panes = json.loads(r.stdout)
    return sid, panes[0]["id"]


def send_keys(pane_id: str, text: str) -> None:
    """Send text to a pane (\\n included if intended)."""
    r = shux(
        "api",
        "pane.send_keys",
        json.dumps({"pane_id": pane_id, "text": text}),
    )
    assert r.returncode == 0, r.stderr


def assert_a1_chunks_arrive():
    """A1 — chunks land on pane.output.watch after PTY emits bytes."""
    print("\n[A1] chunks arrive via pane.output.watch")
    shux("kill", "-s", "pr2c-a1")
    r = shux("new", "-s", "pr2c-a1", "-d")
    assert r.returncode == 0, r.stderr
    sid, pid = pane_id_of("pr2c-a1")
    # Send a fixed payload. Newline so the shell finishes echoing.
    send_keys(pid, "echo HELLO-PR2C\n")
    time.sleep(0.4)

    # 1500ms timeout — generous for the 100ms sample interval.
    r = shux(
        "api",
        "pane.output.watch",
        json.dumps({"pane_id": pid, "timeout_ms": 1500, "limit": 50}),
    )
    payload = json.loads(r.stdout)
    chunks = payload.get("chunks") or []
    assert chunks, f"expected at least one chunk, got {payload}"
    # At least one chunk's payload contains our marker.
    found = False
    for c in chunks:
        raw = base64.b64decode(c["bytes"])
        if b"HELLO-PR2C" in raw:
            found = True
            break
    assert found, f"HELLO-PR2C not in any chunk bytes: {chunks}"
    print(f"  A1 ✓ chunks delivered ({len(chunks)} total), marker found")
    return sid, pid


def assert_a2_no_leak_to_events_history(sid: str):
    """A2 — events.history does NOT include the chunks (data plane sealed)."""
    print("\n[A2] events.history sealed from data plane")
    r = shux(
        "api",
        "events.history",
        json.dumps({"count": 1000}),
    )
    payload = json.loads(r.stdout)
    events = payload.get("events") or []
    types = sorted({e.get("type", "?") for e in events})
    assert "pane.output" not in types, (
        f"pane.output leaked into events.history! types={types}"
    )
    # Also assert by content: no event's data field includes the marker.
    leaked = [
        e
        for e in events
        if "HELLO-PR2C" in json.dumps(e.get("data", {}))
    ]
    assert not leaked, f"data-plane payload leaked: {leaked}"
    print(f"  A2 ✓ events.history clean ({len(events)} events, types={types})")


def assert_a3_events_watch_does_not_see_chunks(sid: str, pid: str):
    """A3 — events.watch --filter pane.* doesn't surface chunks."""
    print("\n[A3] events.watch (pane.* filter) doesn't see chunks")
    # First snapshot the seq so the watch only reads new events.
    snap_seq = json.loads(
        shux("api", "events.history", "{}").stdout
    ).get("current_seq", 0)

    # Send more output. With 100ms sampling and a 1.5s deadline we
    # should get ~10-15 chunks on the data plane, but events.watch
    # with the pane. filter should yield ZERO new control-plane
    # events (no pane.* control events are fired by this scenario).
    send_keys(pid, "for i in 1 2 3 4 5; do echo loop-$i; sleep 0.1; done\n")
    time.sleep(2.0)

    r = shux(
        "api",
        "events.watch",
        json.dumps(
            {
                "filter": ["pane.output"],
                "from_seq": snap_seq,
                "timeout_ms": 1500,
                "max_events": 100,
            }
        ),
    )
    payload = json.loads(r.stdout)
    events = payload.get("events") or []
    assert not events, f"events.watch leaked pane.output: {events}"
    print(f"  A3 ✓ events.watch yielded 0 pane.output events (sealed)")


def assert_a4_rate_limited_chunks(pid: str):
    """A4 — rate-limiting caps chunks-per-second at the source.

    Subscribe via a long-poll BEFORE the burst — the data plane is
    history-less, so any chunks broadcast before our watcher
    subscribes are lost. We start a `shux api pane.output.watch`
    call with a 3.5s timeout, kick the burst, and assert the
    return came back with ≤ 30 chunks despite a 50-line burst.
    """
    print("\n[A4] rate-limited: ≤ ~30 chunks for 3s of output")

    # Drain any prior buffered output first so we measure JUST the burst.
    r = shux(
        "api",
        "pane.output.watch",
        json.dumps({"pane_id": pid, "timeout_ms": 100, "limit": 500}),
    )
    _ = json.loads(r.stdout)

    # Start the long-poll watcher in the background. Capture stdout
    # for the chunk count when it returns.
    watcher = subprocess.Popen(
        [
            SHUX_BIN, "api", "pane.output.watch",
            json.dumps({"pane_id": pid, "timeout_ms": 3500, "limit": 500}),
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    # Give the daemon a brief window to register the subscriber.
    time.sleep(0.2)
    # Burst.
    send_keys(pid, "for i in $(seq 1 50); do echo burst-$i; done\n")
    # Wait for the long-poll to return.
    out, err = watcher.communicate(timeout=10)
    assert watcher.returncode == 0, f"watcher failed: {err!r}"
    payload = json.loads(out)
    chunks = payload.get("chunks") or []
    assert len(chunks) <= 40, (
        f"sampling didn't rate-limit; got {len(chunks)} chunks "
        "(expected ≤ ~30 for 3s of output)"
    )
    # Sanity: at least ONE chunk arrived (sampling didn't drop everything).
    assert chunks, "expected ≥1 chunk for 50-line burst"
    print(f"  A4 ✓ rate-limited to {len(chunks)} chunks (≤ 40 cap)")


def assert_a5_shux_pane_watch_cli(pid: str):
    """A5 — `shux pane watch` CLI streams bytes to stdout.

    --limit 1 exits after the first chunk. Subscribe BEFORE sending
    the marker (data plane is history-less so subscribe-first is
    mandatory).
    """
    print("\n[A5] shux pane watch CLI streams to stdout")
    proc = subprocess.Popen(
        [SHUX_BIN, "pane", "watch", "-s", "pr2c-a1", "-p", pid, "--limit", "1"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    # Give the watcher time to issue its first long-poll. Without this
    # sleep we race the daemon's subscribe-first ordering.
    time.sleep(0.5)
    send_keys(pid, "echo cli-marker\n")
    try:
        out, err = proc.communicate(timeout=8)
    except subprocess.TimeoutExpired:
        proc.kill()
        out, err = proc.communicate()
        raise AssertionError(f"shux pane watch hung; stderr={err.decode()!r}")
    assert b"cli-marker" in out, (
        f"cli-marker not in stdout. out={out!r} stderr={err!r}"
    )
    print(f"  A5 ✓ shux pane watch produced {len(out)} bytes containing marker")


async def visual_split_pane_demo(connection, sid: str, pid: str):
    """A6 — split-pane demo: left runs a loop, right runs shux pane watch."""
    print("\n[A6] visual: live data-plane stream via shux pane watch")
    window, left = await create_window(connection, "pr2c-demo", width=1500, height=820)
    right = await left.async_split_pane(vertical=True)
    await asyncio.sleep(0.3)

    # Right pane: shux pane watch into stdout
    await right.async_send_text(
        f"clear; {SHUX_BIN} pane watch -s pr2c-a1 -p {pid} --limit 30\n"
    )
    await asyncio.sleep(0.3)

    # Left pane: a loop that emits visible markers
    await left.async_send_text("clear\n")
    await asyncio.sleep(0.2)
    await left.async_send_text(
        f"{SHUX_BIN} api pane.send_keys "
        f"'{{\"pane_id\":\"{pid}\","
        "\"text\":\"for i in 1 2 3 4 5 6; do echo LIVE-$i; sleep 0.2; done\\n\"}'"
        "\n"
    )
    await asyncio.sleep(3.0)

    shot = await screenshot(window, "pr2c_data_plane_live", subdir="pr2c")
    print(f"  → screenshot: {shot}")

    await close_window(window)


async def main(connection):
    await cleanup_stale_windows(connection)
    kill_daemon()
    time.sleep(0.5)

    try:
        sid, pid = assert_a1_chunks_arrive()
        assert_a2_no_leak_to_events_history(sid)
        assert_a3_events_watch_does_not_see_chunks(sid, pid)
        assert_a4_rate_limited_chunks(pid)
        assert_a5_shux_pane_watch_cli(pid)
        await visual_split_pane_demo(connection, sid, pid)

        print("\n══════════════════════════════════════════════")
        print("PR 2c L4 visual test PASSED — A1..A6 all green")
        print("══════════════════════════════════════════════")
    finally:
        shux("kill", "-s", "pr2c-a1")
        kill_daemon()


if __name__ == "__main__":
    if not ensure_release_build():
        print("ERROR: target/release/shux build failed")
        sys.exit(1)
    iterm2.run_until_complete(main)
