#!/usr/bin/env python3
# Spike: drive each ACP-capable agent and dump the FULL handshake +
# session/new exchange to a file. No truncation. Used to confirm the
# protocol shape before designing shux-conductor.
import json, subprocess, time, os, sys
from pathlib import Path

OUT = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("/tmp/acp_full.log")
OUT.write_text("")  # truncate

def log(line):
    with OUT.open("a") as f:
        f.write(line + "\n")
    print(line, flush=True)

def probe(label, argv, cwd):
    log(f"\n========== {label} ({' '.join(argv)}) ==========")
    proc = subprocess.Popen(
        argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, cwd=cwd, text=True, bufsize=0,
    )

    def send(method, params, _id=None):
        msg = {"jsonrpc": "2.0", "method": method, "params": params}
        if _id is not None:
            msg["id"] = _id
        line = json.dumps(msg)
        log(f"→  {line}")
        proc.stdin.write(line + "\n"); proc.stdin.flush()

    def read(timeout=5.0):
        import select
        end = time.time() + timeout
        while time.time() < end:
            rlist, _, _ = select.select([proc.stdout], [], [], 0.2)
            if proc.stdout in rlist:
                line = proc.stdout.readline()
                if not line: return
                log(f"←  {line.rstrip()}")

    try:
        send("initialize", {
            "protocolVersion": 1,
            "clientCapabilities": {"fs": {"readTextFile": True, "writeTextFile": True}},
        }, _id=1)
        read(3.0)
        send("session/new", {"cwd": cwd, "mcpServers": []}, _id=2)
        read(3.0)
    finally:
        proc.terminate()
        try: proc.wait(timeout=1)
        except subprocess.TimeoutExpired: proc.kill()
        err = proc.stderr.read() if proc.stderr else ""
        if err.strip():
            log(f"stderr({len(err)}B):\n{err[:2000]}")

probe("opencode acp", ["opencode", "acp"], cwd="/tmp")
probe("gemini --acp", ["gemini", "--acp"], cwd="/tmp")
log("\n[DONE]")
