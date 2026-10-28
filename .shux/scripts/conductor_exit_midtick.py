#!/usr/bin/env python3
"""Drive conductor over its stdio protocol, with no daemon: while it waits
for one pane's pane.status reply, deliver the other pane's exit, then
answer. The poll loop must skip the exited pane rather than die on it.
Exit 0 if conductor survives the tick and keeps serving, 1 otherwise."""

import json
import os
import select
import subprocess
import sys
import time

PLUGIN = sys.argv[1]
A, B, W = "aaaaaaaa-0000-0000-0000-000000000000", "bbbbbbbb-0000-0000-0000-000000000000", "w"


def event(kind, pane):
    data = {"pane_id": pane, "window_id": W, "session_id": "s", "command": ["claude"]}
    return {"jsonrpc": "2.0", "method": "event", "params": {"type": kind, "data": {"data": data}}}


env = dict(os.environ, SHUX_CONDUCTOR_POLL_MS="200", SHUX_CONDUCTOR_SNAPSHOTS="0",
           SHUX_CONDUCTOR_NOTIFY="off")
proc = subprocess.Popen(["bash", PLUGIN], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                        stderr=subprocess.PIPE, env=env, text=True, bufsize=1)


def send(msg):
    proc.stdin.write(json.dumps(msg) + "\n")
    proc.stdin.flush()


def read_line(timeout):
    ready, _, _ = select.select([proc.stdout], [], [], timeout)
    return proc.stdout.readline() if ready else ""


def stop(code):
    if proc.poll() is None:
        proc.kill()
    proc.wait(timeout=5)
    sys.exit(code)


send({"jsonrpc": "2.0", "method": "plugin.init", "id": "init", "params": {}})
read_line(5)  # manifest
send(event("pane.created", A))
send(event("pane.created", B))

deadline = time.time() + 10
exited_b = False
status_calls = 0
while time.time() < deadline:
    line = read_line(1)
    if not line:
        if proc.poll() is not None:
            print("conductor died:", proc.stderr.read()[-400:])
            stop(1)
        continue
    msg = json.loads(line)
    method, rid = msg.get("method"), msg.get("id")
    if method == "pane.status":
        status_calls += 1
        if not exited_b:
            # The other pane exits while this one's reply is awaited, so it
            # is still in the loop's list of panes for this tick.
            other = B if msg["params"]["pane_id"] == A else A
            send(event("pane.exited", other))
            exited_b = True
        send({"jsonrpc": "2.0", "id": rid,
              "result": {"enabled": True, "seen": True, "summary": {"state": "working"}}})
    elif rid is not None:
        send({"jsonrpc": "2.0", "id": rid, "result": {}})
    if exited_b and status_calls >= 4:
        break

time.sleep(0.5)
if proc.poll() is not None or not exited_b:
    print("conductor died after B exited mid-tick:", proc.stderr.read()[-400:])
    stop(1)
print("conductor survived an exit mid-tick")
stop(0)
