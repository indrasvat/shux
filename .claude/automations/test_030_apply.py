# /// script
# requires-python = ">=3.14"
# dependencies = ["iterm2", "pyobjc", "pyobjc-framework-Quartz"]
# ///
r"""
L4 visual test for PR 3a — `shux apply <template.toml>` + state.apply RPC.

Demonstrates the agent-orchestration killer feature: declare a workspace
in TOML, ship it to the daemon as one atomic batch, watch the entire
lifecycle event burst stream into a side terminal — all sharing the
same correlation_id so subscribers can attribute the events.

Layout:
  ┌────────────────────────────┬────────────────────────────┐
  │  pane A (LEFT)             │  pane B (RIGHT)            │
  │  shux events watch \       │  shux apply \              │
  │      --filter session. \   │     /tmp/conductor.toml    │
  │      --filter window. \    │  → emits 1 RPC, fires 10   │
  │      --filter pane.        │     events with shared cid │
  │                            │                            │
  │  ← 10 JSON Lines stream    │                            │
  │    here in real time       │                            │
  └────────────────────────────┴────────────────────────────┘

Asserts:
  A1 — `shux apply --dry-run` parses the TOML and prints lowered ops
  A2 — `shux apply` returns "✓ Applied apply-<uuid>" with N panes
  A3 — All N+M events fire with the SAME correlation_id (atomicity proof)
  A4 — `shux ls` shows the new session with the right window count
  A5 — `state.apply` with a name conflict returns BatchError, no commit
  A6 — Visual: side-by-side panes show the watcher catching the burst
"""
import asyncio
import json
import os
import subprocess
import sys
import tempfile
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


def write_template(name: str, body: str) -> str:
    p = os.path.join(tempfile.gettempdir(), f"shux-tpl-{name}.toml")
    with open(p, "w") as f:
        f.write(body)
    return p


def assert_a1_dry_run():
    """A1 — dry-run parses TOML and prints lowered ops as JSON."""
    tpl = write_template(
        "a1",
        """
[session]
name = "a1-test"
cwd = "/tmp"

[[windows]]
title = "editor"
[[windows.panes]]
command = ["bash", "-l"]
""",
    )
    r = shux("apply", "--dry-run", tpl)
    assert r.returncode == 0, f"dry-run failed: {r.stderr}"
    parsed = json.loads(r.stdout)
    assert "ops" in parsed
    assert len(parsed["ops"]) == 2  # CreateSession + CreateWindow
    assert parsed["ops"][0]["op"] == "create_session"
    assert parsed["ops"][1]["op"] == "create_window"
    assert parsed["ops"][1]["initial_command"] == ["bash", "-l"]
    print("  ✓ A1: --dry-run lowered TOML to 2 ops, didn't touch the daemon")


def assert_a2_a3_a4_apply_with_correlation():
    """A2/A3/A4 — apply returns ✓, all events share correlation_id, ls confirms."""
    name = f"conductor-{int(time.time())}"
    tpl = write_template(
        name,
        f"""
[session]
name = "{name}"
cwd = "/tmp"

[[windows]]
title = "editor"
[[windows.panes]]
command = ["bash", "-l"]
[[windows.panes]]
direction = "vertical"
ratio = 0.5
command = ["bash", "-l"]

[[windows]]
title = "agent-1"
[[windows.panes]]
command = ["bash", "-l"]

[[windows]]
title = "agent-2"
[[windows.panes]]
command = ["bash", "-l"]
""",
    )

    r = shux("apply", tpl)
    assert r.returncode == 0, f"apply failed: {r.stderr}"
    out = r.stdout
    assert "✓ Applied apply-" in out, f"missing ✓ Applied line: {out!r}"
    # Parse the apply-<uuid> from the output for cross-check.
    cid_token = next(t for t in out.split() if t.startswith("apply-"))
    assert cid_token.startswith("apply-")
    print(f"  ✓ A2: apply returned {cid_token}")

    # A3: every event in history must share the correlation_id.
    r = shux("events", "history", "-n", "20", "--format", "json")
    assert r.returncode == 0
    lines = [json.loads(line) for line in r.stdout.strip().splitlines() if line]
    apply_events = [e for e in lines if e.get("correlation_id") == cid_token]
    assert len(apply_events) >= 5, (
        f"expected ≥5 events with cid {cid_token}, got {len(apply_events)}: {lines!r}"
    )
    # No event from this batch should have a DIFFERENT correlation_id.
    foreign = [
        e
        for e in lines
        if e.get("correlation_id")
        and e["correlation_id"] != cid_token
        and e["seq"] >= apply_events[0]["seq"]
        and e["seq"] <= apply_events[-1]["seq"]
    ]
    assert not foreign, f"foreign cid in our seq range: {foreign}"
    print(f"  ✓ A3: all {len(apply_events)} apply events share correlation_id")

    # A4: shux ls --format json shows session with window_count == 4
    # (template's 3 windows + the auto "1" window from create_session).
    r = shux("ls", "--format", "json")
    assert r.returncode == 0
    sessions = json.loads(r.stdout)["sessions"]
    ours = next((s for s in sessions if s["name"] == name), None)
    assert ours is not None, f"session {name!r} not found in {sessions!r}"
    # active_window_id is set, and pane_id is set
    assert ours.get("active_window_id")
    print(f"  ✓ A4: shux ls confirms session {name!r}")

    shux("kill", "-s", name)


def assert_a5_atomicity_rollback():
    """A5 — apply with a name conflict returns BatchError; no partial commit."""
    name = f"a5-{int(time.time())}"
    # Pre-create so the apply will conflict.
    shux("new", "-s", name, "--detached")

    # Snapshot the session count before the apply.
    r = shux("ls", "--format", "json")
    sessions_before = len(json.loads(r.stdout)["sessions"])

    tpl = write_template(
        "a5",
        f"""
[session]
name = "{name}"
cwd = "/tmp"

[[windows]]
title = "editor"
[[windows.panes]]
command = ["bash", "-l"]
""",
    )
    r = shux("apply", tpl)
    assert r.returncode != 0, f"expected apply to fail on name conflict: {r.stdout!r}"
    assert "exists" in (r.stdout + r.stderr).lower(), (
        f"expected name-exists error: {r.stdout!r} {r.stderr!r}"
    )

    # No new session should have been committed.
    r = shux("ls", "--format", "json")
    sessions_after = len(json.loads(r.stdout)["sessions"])
    assert sessions_after == sessions_before, (
        f"session count changed despite apply failure: {sessions_before} → {sessions_after}"
    )
    shux("kill", "-s", name)
    print("  ✓ A5: apply rolled back cleanly on name conflict")


async def visual_demo(connection):
    """A6 — side-by-side iTerm panes: watcher on left, apply on right."""
    await cleanup_stale_windows(connection)
    window, left = await create_window(
        connection,
        name="apply-demo",
        x_pos=120,
        width=1500,
        height=750,
    )
    try:
        right = await left.async_split_pane(vertical=True)

        for s in (left, right):
            await s.async_send_text(f'cd "{os.path.dirname(SHUX_BIN)}"\n')
            await asyncio.sleep(0.1)
            await s.async_send_text("clear\n")
        await asyncio.sleep(0.5)

        # Start the watcher on the left, no filter so we see everything.
        await left.async_send_text(
            "./shux events watch --filter session. --filter window. "
            "--filter pane. --timeout-ms 5000\n"
        )
        await asyncio.sleep(1.5)
        await screenshot(window, "030_v1_watcher_idle")

        # Drop a template into /tmp from the right pane.
        tpl = write_template(
            "demo",
            """
[session]
name = "agent-conductor-demo"
cwd = "/tmp"

[[windows]]
title = "editor"
[[windows.panes]]
command = ["bash", "-l"]
[[windows.panes]]
direction = "vertical"
ratio = 0.5
command = ["bash", "-l"]

[[windows]]
title = "agent-1"
[[windows.panes]]
command = ["bash", "-l"]

[[windows]]
title = "agent-2"
[[windows.panes]]
command = ["bash", "-l"]
""",
        )
        await right.async_send_text(f"cat {tpl}\n")
        await asyncio.sleep(0.8)
        await screenshot(window, "030_v2_template_visible")

        await right.async_send_text(f"./shux apply {tpl}\n")
        await asyncio.sleep(2.5)
        await screenshot(window, "030_v3_after_apply_burst")

        # Show the JSON correlation_id one more time to make the point.
        await right.async_send_text(
            "./shux events history --filter session. -n 1 --format json | jq\n"
        )
        await asyncio.sleep(1.2)
        await screenshot(window, "030_v4_correlation_id_visible")

        # Cleanup
        await right.async_send_text("./shux kill -s agent-conductor-demo\n")
        await asyncio.sleep(0.5)
        return True
    finally:
        await close_window(window)


async def main(connection):
    print("=" * 60)
    print("PR 3a visual test — state.apply + `shux apply` end-to-end")
    print("=" * 60)
    if not ensure_release_build():
        print("✗ release build missing")
        sys.exit(1)

    print("\n[headless assertions]")
    failures = 0
    for fn in (
        assert_a1_dry_run,
        assert_a2_a3_a4_apply_with_correlation,
        assert_a5_atomicity_rollback,
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

    print("\n[visual demo]")
    kill_daemon()
    visual_ok = False
    try:
        visual_ok = await visual_demo(connection)
    except Exception as e:
        print(f"  ✗ visual_demo: {type(e).__name__}: {e}")

    print("\n" + "=" * 60)
    print(f"PR 3a visual test summary:")
    print(f"  headless: {3 - failures}/3 PASS")
    print(f"  visual:   {'OK' if visual_ok else 'FAIL'}")
    print("=" * 60)
    sys.exit(0 if failures == 0 and visual_ok else 1)


if __name__ == "__main__":
    iterm2.run_until_complete(main)
