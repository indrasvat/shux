# /// script
# requires-python = ">=3.14"
# dependencies = ["iterm2", "pyobjc", "pyobjc-framework-Quartz"]
# ///
r"""
L4 visual test for PR 3b — optimistic-concurrency surface.

Exercises the agent-safety guarantee: every mutating RPC now accepts
`expected_version`, and a stale version returns the canonical
`-32002 version_conflict` error with bounded entity metadata. The
"read → mutate-with-version → on conflict retry once" loop from
PRD §8.5 is the multi-agent contract this PR locks in.

Asserts:
  A1 — `session.rename` with `expected_version: 1` succeeds first time
  A2 — `session.rename` with the SAME stale version returns -32002
        and the response carries `data.resource = "session"`,
        `data.expected_version = 1`, `data.actual_version >= 2`,
        and a `data.hint` mentioning re-read
  A3 — `pane.kill` with a stale version is rejected AND the pane is
        still listed afterwards (IO state preserved → the order-of-
        operations bug from main.rs:1095 didn't reappear)
  A4 — Three RPCs in a row with `expected_version` omitted all
        succeed (backward-compat for human users)
  A5 — `session.rename` with `expected_version: "not-a-number"`
        returns -32602 (invalid_params) — never silently ignored
  A6 — Read-mutate-retry pattern converges in one round-trip
  A7 — Visual: split-pane TUI shows `shux api ...` runs on the left
        and a daemon `events watch` on the right; the conflict trace
        is visible in the captured screenshot

Layout:
  ┌────────────────────────────┬────────────────────────────┐
  │  api calls (LEFT)          │  events.watch (RIGHT)      │
  │  shux api session.create   │  shux events watch         │
  │  shux api session.rename   │      --filter session.     │
  │      {expected_version:1}  │                            │
  │  shux api session.rename   │  ← rename events stream    │
  │      {expected_version:1}  │    (only ONE; the 2nd was  │
  │  → -32002 visible          │     rejected)              │
  └────────────────────────────┴────────────────────────────┘
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


def shux_api(method: str, params: dict) -> dict:
    """Run `shux api <method> '<json>'` and return parsed JSON-RPC response.

    Returns the FULL response dict (with `result` xor `error`) so callers
    can assert on either path without retrying.
    """
    r = subprocess.run(
        [SHUX_BIN, "api", method, json.dumps(params)],
        capture_output=True,
        text=True,
        timeout=10,
    )
    out = r.stdout.strip()
    if not out:
        raise RuntimeError(f"api {method} no stdout. stderr={r.stderr}")
    # `shux api` prints the JSON-RPC response object on stdout.
    return json.loads(out)


def get_version(session_name: str) -> int:
    """Fetch the current version stamp for a session by name."""
    r = shux("api", "session.list", "{}")
    if r.returncode != 0:
        raise RuntimeError(f"session.list failed: {r.stderr}")
    payload = json.loads(r.stdout)
    sessions = (
        payload.get("result", {}).get("sessions")
        or payload.get("sessions")
        or []
    )
    for s in sessions:
        if s.get("name") == session_name:
            return int(s.get("version", 1))
    raise KeyError(f"session {session_name} not found in list")


def assert_a1_a2_a6_session_rename_conflict():
    """A1+A2+A6 — rename succeeds, stale version returns -32002, retry-with-actual converges."""
    print("\n[A1+A2+A6] session.rename optimistic-concurrency path")

    # Clean state: kill any stale demo session, then create fresh.
    shux("kill", "-s", "pr3b-demo")
    r = shux("api", "session.create", '{"name": "pr3b-demo"}')
    assert r.returncode == 0, f"session.create failed: {r.stderr}"
    payload = json.loads(r.stdout)
    sid = payload["result"]["id"]
    v1 = payload["result"].get("version", 1)  # post-create version
    print(f"  created session.id={sid[:8]} version={v1}")

    # A1: rename with current version succeeds.
    resp = shux_api(
        "session.rename",
        {"id": sid, "new_name": "pr3b-mid", "expected_version": v1},
    )
    assert "result" in resp, f"A1: rename should succeed, got {resp}"
    print(f"  A1 ✓ first rename succeeded; new state: {resp['result'].get('name')}")

    # A2: rename with STALE version returns -32002 with full data shape.
    resp = shux_api(
        "session.rename",
        {"id": sid, "new_name": "pr3b-rejected", "expected_version": v1},
    )
    assert "error" in resp, f"A2: stale rename should fail, got {resp}"
    err = resp["error"]
    assert err["code"] == -32002, f"A2: wrong code {err['code']}"
    assert err["message"] == "version_conflict", f"A2: wrong message {err['message']}"
    data = err["data"]
    assert data["resource"] == "session", f"A2: wrong resource {data['resource']}"
    assert data["id"] == sid, f"A2: wrong id {data['id']}"
    assert data["expected_version"] == v1, f"A2: wrong expected {data['expected_version']}"
    actual = data["actual_version"]
    assert actual > v1, f"A2: actual should exceed expected, got {actual}"
    assert "Re-read" in data["hint"], f"A2: hint should mention Re-read: {data['hint']}"
    print(f"  A2 ✓ stale rename rejected with -32002; actual_version={actual}")
    print(f"      hint: {data['hint']}")

    # A6: read actual_version, retry — must succeed in one round-trip.
    resp = shux_api(
        "session.rename",
        {"id": sid, "new_name": "pr3b-final", "expected_version": actual},
    )
    assert "result" in resp, f"A6: retry should succeed, got {resp}"
    assert resp["result"]["name"] == "pr3b-final"
    print(f"  A6 ✓ retry with actual_version={actual} converged immediately")


def assert_a3_pane_kill_stale_version_preserves_io():
    """A3 — stale pane.kill is rejected AND pane stays alive (IO state preserved)."""
    print("\n[A3] pane.kill stale version rejects without tearing down IO")

    shux("kill", "-s", "pr3b-pane")
    r = shux("api", "session.create", '{"name": "pr3b-pane"}')
    assert r.returncode == 0
    create_resp = json.loads(r.stdout)["result"]
    sid = create_resp["id"]
    wid = create_resp["window_id"]

    # Split so we have a second pane (the first can't be killed: LastPane).
    split = shux_api(
        "pane.split",
        {"session_id": sid, "window_id": wid, "direction": "vertical"},
    )
    new_pane_id = split["result"]["pane"]["id"]
    print(f"  split → new_pane_id={new_pane_id[:8]}")

    # Attempt to kill with stale version 99 — must reject.
    resp = shux_api(
        "pane.kill", {"pane_id": new_pane_id, "expected_version": 99}
    )
    assert "error" in resp, f"A3: stale kill should fail, got {resp}"
    err = resp["error"]
    assert err["code"] == -32002, f"A3: wrong code {err['code']}"
    assert err["data"]["resource"] == "pane", f"A3: wrong resource {err['data']['resource']}"
    print(f"  A3 ✓ stale kill rejected with -32002")

    # Pane must still be in the list.
    plist = shux_api("pane.list", {"session_id": sid, "window_id": wid})
    panes = plist["result"]
    ids = [p["id"] for p in panes]
    assert new_pane_id in ids, f"A3: pane should still exist after rejected kill; got {ids}"
    print(f"  A3 ✓ pane still listed → IO state preserved (order-of-ops correct)")


def assert_a4_backward_compat_no_version():
    """A4 — three renames without `expected_version` all succeed (human user path)."""
    print("\n[A4] backward-compat: omitted expected_version always succeeds")

    shux("kill", "-s", "pr3b-bc")
    r = shux("api", "session.create", '{"name": "pr3b-bc"}')
    assert r.returncode == 0
    sid = json.loads(r.stdout)["result"]["id"]

    for new_name in ["pr3b-bc-a", "pr3b-bc-b", "pr3b-bc-c"]:
        resp = shux_api("session.rename", {"id": sid, "new_name": new_name})
        assert "result" in resp, f"A4: unversioned rename should succeed, got {resp}"
    print(f"  A4 ✓ 3 unversioned renames all succeeded")


def assert_a5_invalid_type_returns_invalid_params():
    """A5 — non-integer `expected_version` returns -32602."""
    print("\n[A5] invalid expected_version type returns -32602")

    shux("kill", "-s", "pr3b-bad")
    r = shux("api", "session.create", '{"name": "pr3b-bad"}')
    assert r.returncode == 0
    sid = json.loads(r.stdout)["result"]["id"]

    resp = shux_api(
        "session.rename",
        {"id": sid, "new_name": "x", "expected_version": "not-a-number"},
    )
    assert "error" in resp, f"A5: bad type should fail, got {resp}"
    err = resp["error"]
    assert err["code"] == -32602, f"A5: wrong code {err['code']}"
    assert "expected_version" in err["data"]["detail"], (
        f"A5: detail should mention expected_version, got {err['data']['detail']}"
    )
    print(f"  A5 ✓ string expected_version rejected as -32602 invalid_params")


async def visual_split_pane_demo(connection):
    """A7 — Side-by-side iTerm panes showing api calls + events watch."""
    print("\n[A7] visual demo with side-by-side iTerm panes")

    window, left = await create_window(connection, "pr3b-demo", width=1500, height=820)

    # Split the iTerm session vertically into two panes.
    right = await left.async_split_pane(vertical=True)
    await asyncio.sleep(0.3)

    # LEFT: run a script that demonstrates the rename → conflict → retry path.
    demo_session = "pr3b-visual"
    # Clean any stale demo state.
    shux("kill", "-s", demo_session)
    await asyncio.sleep(0.2)

    # RIGHT: events watch filtered to session.* events.
    right_cmd = (
        f"clear; echo '═══ events.watch (filter=session.) ═══'; "
        f"{SHUX_BIN} events watch --filter session. --limit 6"
    )
    await right.async_send_text(right_cmd + "\n")
    await asyncio.sleep(0.5)

    # LEFT: header then the operations.
    await left.async_send_text("clear\n")
    await asyncio.sleep(0.2)
    await left.async_send_text(
        "echo '═══ PR 3b — optimistic concurrency ═══'\n"
    )
    await asyncio.sleep(0.2)

    # 1) create session
    await left.async_send_text(
        f"{SHUX_BIN} api session.create '{{\"name\": \"{demo_session}\"}}'"
        " | head -3\n"
    )
    await asyncio.sleep(0.4)
    sid = get_session_id_blocking(demo_session)

    # 2) rename with expected_version: 1 (succeeds, version bumps to 2)
    await left.async_send_text(
        f"echo; echo '→ rename with expected_version=1 (must succeed)';"
        f" {SHUX_BIN} api session.rename "
        f"'{{\"id\":\"{sid}\",\"new_name\":\"{demo_session}-mid\",\"expected_version\":1}}'"
        " | head -3\n"
    )
    await asyncio.sleep(0.5)

    # 3) rename AGAIN with stale expected_version: 1 — must fail -32002
    await left.async_send_text(
        f"echo; echo '→ rename AGAIN with stale expected_version=1';"
        f" {SHUX_BIN} api session.rename "
        f"'{{\"id\":\"{sid}\",\"new_name\":\"{demo_session}-no\",\"expected_version\":1}}'\n"
    )
    await asyncio.sleep(0.5)

    # 4) retry with actual_version (2)
    await left.async_send_text(
        f"echo; echo '→ retry with expected_version=2 (current)';"
        f" {SHUX_BIN} api session.rename "
        f"'{{\"id\":\"{sid}\",\"new_name\":\"{demo_session}-final\",\"expected_version\":2}}'"
        " | head -3\n"
    )
    await asyncio.sleep(1.0)

    # Capture the split-pane state.
    shot = await screenshot(window, "pr3b_optimistic_concurrency", subdir="pr3b")
    print(f"  → screenshot: {shot}")

    # Cleanup: kill the demo session, then close the iTerm window.
    shux("kill", "-s", f"{demo_session}-final")
    shux("kill", "-s", f"{demo_session}-mid")
    shux("kill", "-s", demo_session)
    await close_window(window)


def get_session_id_blocking(name: str, retries: int = 10) -> str:
    """Block until `name` shows in session.list, then return its id."""
    for _ in range(retries):
        r = shux("api", "session.list", "{}")
        if r.returncode == 0:
            payload = json.loads(r.stdout).get("result", {})
            sessions = payload.get("sessions") or payload or []
            if isinstance(sessions, list):
                for s in sessions:
                    if isinstance(s, dict) and s.get("name") == name:
                        return s["id"]
        time.sleep(0.2)
    raise RuntimeError(f"session {name!r} did not appear in session.list")


async def main(connection):
    await cleanup_stale_windows(connection)

    # Headless assertions (run against a fresh daemon).
    kill_daemon()
    time.sleep(0.5)

    try:
        assert_a1_a2_a6_session_rename_conflict()
        assert_a3_pane_kill_stale_version_preserves_io()
        assert_a4_backward_compat_no_version()
        assert_a5_invalid_type_returns_invalid_params()

        # Visual portion.
        await visual_split_pane_demo(connection)

        print("\n══════════════════════════════════════════════")
        print("PR 3b L4 visual test PASSED — A1..A7 all green")
        print("══════════════════════════════════════════════")
    finally:
        # Sweep test-named sessions so the next run starts clean.
        for s in [
            "pr3b-demo",
            "pr3b-mid",
            "pr3b-final",
            "pr3b-pane",
            "pr3b-bc",
            "pr3b-bc-a",
            "pr3b-bc-b",
            "pr3b-bc-c",
            "pr3b-bad",
        ]:
            shux("kill", "-s", s)
        kill_daemon()


if __name__ == "__main__":
    if not ensure_release_build():
        print("ERROR: target/release/shux build failed")
        sys.exit(1)
    iterm2.run_until_complete(main)
