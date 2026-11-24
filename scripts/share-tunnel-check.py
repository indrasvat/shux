# /// script
# requires-python = ">=3.12"
# dependencies = ["playwright==1.48.0"]
# ///
"""`shux share` end to end, over a real Cloudflare quick tunnel (#297).

A real daemon shares one demo session through the cloudflared on PATH. Headless
Chrome opens the printed link at its public trycloudflare.com origin, as a
guest would, and must see the session live, colour probes included, while
every key, click and paste it makes reaches no pane: a recorder pane logs every
byte its terminal delivers. Then `shux share stop`, after which the link, and
the origin, must be dead. The only thing exposed is that demo session, for the
minute the run takes.

Usage: share-tunnel-check.py <shux binary> <evidence dir> [--showcase]
`--showcase` also shares a colourful second session and keeps a zoomed-in
guest frame of it (for the landing page). Exit 0 only when every check passed.
"""
import asyncio
import json
import os
import shutil
import ssl
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

from playwright.async_api import async_playwright

GREEN, RED, YELLOW, CYAN, OFF = "\033[1;32m", "\033[1;31m", "\033[1;33m", "\033[1;36m", "\033[0m"
BUDGET_S = 60
FAILURES: list[str] = []


def step(msg: str) -> None:
    print(f"{CYAN}▶ {msg}{OFF}", flush=True)


def check(ok: bool, what: str) -> None:
    if ok:
        print(f"  {GREEN}✓{OFF} {what}", flush=True)
    else:
        print(f"  {RED}✗ {what}{OFF}", flush=True)
        FAILURES.append(what)


def shux(bin_: str, env: dict[str, str], *args: str) -> dict:
    out = subprocess.run([bin_, "--format", "json", *args], env=env, capture_output=True, text=True)
    if out.returncode != 0:
        raise RuntimeError(f"shux {' '.join(args[:3])}: {out.stdout.strip()} {out.stderr.strip()}")
    return json.loads(out.stdout)


def rpc(bin_: str, env: dict[str, str], method: str, params: dict) -> dict:
    return shux(bin_, env, "rpc", "call", method, "--params", json.dumps(params))["result"]


def recorder(log: Path) -> list[str]:
    script = (
        "printf '\\033[38;2;120;220;180mTRUE%sCOLOR\\033[0m \\033[38;5;208mINDE%sXED\\033[0m "
        "\\033[34mBA%sSIC\\033[0m\\n' '' '' ''; stty raw -echo; printf 'REC%sORDING\\n' ''; "
        f"exec cat > '{log}'"
    )
    return ["sh", "-c", script]


TICKER = ["sh", "-c", 'i=0; while :; do i=$((i+1)); printf "\\033[1;35mLI%sVE %s\\033[0m\\n" "" "$i"; sleep 0.5; done']


DEMO_COMMITS = [
    ("README.md", "# lamp\n\nA tiny status board.\n", "docs: say what lamp is"),
    ("src/board.rs", "pub fn render(rows: &[Row]) -> String {\n    rows.iter().map(Row::line).collect()\n}\n", "feat(board): render one line per service"),
    ("src/row.rs", "pub struct Row { pub name: String, pub up: bool }\n", "feat(row): a service and whether it is up"),
    ("src/board.rs", "pub fn render(rows: &[Row]) -> String {\n    let mut out = String::new();\n    for r in rows {\n        out += &r.line();\n        out.push('\\n');\n    }\n    out\n}\n", "fix(board): one row per line, in order"),
    ("src/row.rs", "pub struct Row { pub name: String, pub up: bool }\n\nimpl Row {\n    pub fn line(&self) -> String {\n        let mark = if self.up { \"●\" } else { \"○\" };\n        format!(\"{mark} {}\", self.name)\n    }\n}\n", "feat(row): a filled dot for a service that is up"),
]


def demo_repo(path: Path) -> Path:
    """A small git history under a made-up author, for the showcase."""
    who = {"GIT_AUTHOR_NAME": "Demo Dev", "GIT_AUTHOR_EMAIL": "demo@example.invalid",
           "GIT_COMMITTER_NAME": "Demo Dev", "GIT_COMMITTER_EMAIL": "demo@example.invalid",
           "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1", "PATH": os.environ["PATH"]}
    git = lambda *a: subprocess.run(["git", *a], cwd=path, env=who, check=True, capture_output=True)  # noqa: E731
    path.mkdir()
    git("init", "-q", "-b", "main")
    for name, body, msg in DEMO_COMMITS:
        (path / name).parent.mkdir(parents=True, exist_ok=True)
        (path / name).write_text(body)
        git("add", name)
        git("commit", "-q", "-m", msg)
    (path / "src/row.rs").write_text(DEMO_COMMITS[-1][1].replace("○", "◌"))
    return path


async def until(page, cond: str) -> None:
    """Polls from outside the page: its CSP forbids the eval `wait_for_function` uses."""
    loop = asyncio.get_running_loop()
    deadline = loop.time() + BUDGET_S
    while not await page.evaluate(f"() => Boolean({cond})"):
        if loop.time() > deadline:
            raise TimeoutError(f"never held: {cond}")
        await asyncio.sleep(0.1)


async def settled(page, polls: int = 5) -> None:
    """Waits until the guest's screen text holds still for `polls` reads in a
    row, so a screenshot is of a drawn frame, not one mid-paint."""
    loop = asyncio.get_running_loop()
    deadline = loop.time() + BUDGET_S
    last, same = None, 0
    while same < polls:
        if loop.time() > deadline:
            raise TimeoutError("the guest's screen never settled")
        text = await page.evaluate("window.__shux.text()")
        same = same + 1 if text == last else 0
        last = text
        await asyncio.sleep(0.2)


def status_of(url: str) -> int:
    """The HTTP status a fresh client gets for `url`; 0 when nothing answers."""
    req = urllib.request.Request(url, headers={"User-Agent": "shux-share-check"})
    try:
        with urllib.request.urlopen(req, timeout=15, context=ssl.create_default_context()) as r:
            return r.status
    except urllib.error.HTTPError as e:
        return e.code
    except (urllib.error.URLError, TimeoutError, OSError):
        return 0


async def guest(browser, url: str, size: tuple[int, int]):
    ctx = await browser.new_context(viewport={"width": size[0], "height": size[1]}, device_scale_factor=2)
    page = await ctx.new_page()
    errors: list[str] = []
    page.on("pageerror", lambda e: errors.append(str(e)))
    # A name minted seconds ago may not resolve here yet; the edge already
    # serves it (the daemon checked), so only this machine's DNS can lag.
    # Cloudflare can also answer a fresh name with its own error page (1016)
    # for a few seconds while the route settles; a guest would reload.
    t0 = time.monotonic()
    while True:
        try:
            await page.goto(url, wait_until="load")
            if await page.query_selector("#watch"):
                if time.monotonic() - t0 > 1:
                    print(f"{YELLOW}  the page took {time.monotonic() - t0:.0f}s of reloads to come up{OFF}")
                return ctx, page, errors
        except Exception:  # noqa: BLE001 - retried until the budget, then raised
            if time.monotonic() - t0 > BUDGET_S:
                raise
        if time.monotonic() - t0 > BUDGET_S:
            raise TimeoutError(f"the guest page never came up: {(await page.inner_text('body'))[:160]!r}")
        await asyncio.sleep(2)


async def main() -> int:
    bin_, out = sys.argv[1], Path(sys.argv[2])
    showcase = "--showcase" in sys.argv[3:]
    out.mkdir(parents=True, exist_ok=True)
    if not shutil.which("cloudflared"):
        print(f"{RED}✗ cloudflared is not on PATH; this check needs the real one{OFF}")
        return 1
    work = Path(tempfile.mkdtemp(prefix="s297t-", dir="/tmp"))
    env = {
        **{k: v for k, v in os.environ.items() if k not in ("SHUX_SOCKET", "NO_COLOR", "SHUX_PANE_ID", "SHUX_TEST_SHARE_PROBE")},
        "XDG_RUNTIME_DIR": str(work / "run"),
        "XDG_CONFIG_HOME": str(work / "config"),
        "XDG_STATE_HOME": str(work / "state"),
        "SHELL": "/bin/sh",
    }
    (work / "run").mkdir(mode=0o700)
    (work / "state/shux").mkdir(parents=True)
    (work / "state/shux/onboarding.json").write_text('{"prefix_discovered": true, "welcome_toast_seen": true}')
    log = work / "rec.log"
    url = ""
    try:
        step("a demo session: a recorder pane and a ticker beside it")
        s = rpc(bin_, env, "session.create", {"name": "demo", "command": recorder(log)})
        rpc(bin_, env, "pane.split", {"pane_id": s["pane_id"], "direction": "horizontal", "command": TICKER})
        before = rpc(bin_, env, "pane.capture", {"pane_id": s["pane_id"]})

        step("shux share -s demo, through the real cloudflared")
        t0 = time.monotonic()
        made = shux(bin_, env, "share", "-s", "demo")
        url = made["url"]
        origin = url.split("/v/")[0]
        took = time.monotonic() - t0
        check(origin.startswith("https://") and origin.endswith(".trycloudflare.com"), f"a quick-tunnel origin ({took:.1f}s to a verified link)")
        listed = shux(bin_, env, "share", "list")
        check(listed["tunnel"] == "up" and len(listed["links"]) == 1, "share list: one link, tunnel up")

        async with async_playwright() as pw:
            chrome = await pw.chromium.launch(channel="chrome", headless=True)
            step("a guest opens the link in Chrome, over the public origin")
            ctx, page, errors = await guest(chrome, url, (1400, 900))
            check("#" not in page.url, "the secret left the address bar")
            await page.click("#watch")
            await until(page, "window.__shux && window.__shux.phase === 'live' && window.__shux.text().includes('RECORDING')")
            text = await page.evaluate("window.__shux.text()")
            check(all(p in text for p in ("TRUECOLOR", "INDEXED", "BASIC")), "colour probes reach the guest")
            # Live: the ticker's next line arrives (`until` fails the run if not).
            first = await page.evaluate("window.__shux.text()")
            await until(page, f"window.__shux.text() !== {json.dumps(first)}")
            await page.screenshot(path=str(out / "v1_guest-tunnel_1400_default.png"))
            wide = await page.evaluate("window.__shux.text()")
            await page.set_viewport_size({"width": 390, "height": 844})
            await until(page, f"window.__shux.text() !== {json.dumps(wide)}")
            await page.screenshot(path=str(out / "v1_guest-tunnel_390_default.png"))

            step("the guest types, pastes and clicks; the owner then types a marker")
            await page.click("#term")
            await page.keyboard.type("guest typed\n")
            await page.keyboard.press("Control+C")
            await page.mouse.wheel(0, 300)
            rpc(bin_, env, "pane.send_keys", {"pane_id": s["pane_id"], "text": "MARK"})
            deadline = time.monotonic() + BUDGET_S
            while not (log.exists() and log.read_bytes()) and time.monotonic() < deadline:
                await asyncio.sleep(0.1)
            got = log.read_bytes() if log.exists() else b""
            check(got == b"MARK", f"the pane received only the owner's marker ({got[:40]!r})")
            after = rpc(bin_, env, "pane.capture", {"pane_id": s["pane_id"]})
            check((before["cols"], before["rows"]) == (after["cols"], after["rows"]), "the guest never sized the pane")
            check(not errors, f"no page error ({errors[:2]})")

            if showcase:
                # lazygit on a small demo repo made here, under a made-up
                # author: colourful, and nothing of this machine's on screen
                # (no process list, user, email or branches), with a private
                # config so it never touches the real one.
                step("showcase: lazygit on a demo repo, shared to a second guest")
                repo = demo_repo(work / "demo-repo")
                lg = work / "lazygit.yml"
                lg.write_text("disableStartupPopups: true\ngui:\n  showRandomTip: false\n")
                show = rpc(bin_, env, "session.create", {
                    "name": "showcase", "cwd": str(repo),
                    "command": ["env", f"LG_CONFIG_FILE={lg}", "lazygit"],
                })
                rpc(bin_, env, "pane.set_size", {"pane_id": show["pane_id"], "cols": 118, "rows": 34})
                link = shux(bin_, env, "share", "-s", "showcase")
                sctx, spage, _ = await guest(chrome, link["url"], (1280, 760))
                try:
                    await spage.click("#watch", timeout=BUDGET_S * 1000)
                except Exception:
                    print(f"{YELLOW}  page: {spage.url.split('#')[0]} {(await spage.inner_text('body'))[:200]!r}{OFF}")
                    raise
                await until(spage, "window.__shux && window.__shux.phase === 'live' && window.__shux.text().includes('Commits')")
                # The commits panel, with its diff beside it.
                rpc(bin_, env, "pane.send_keys", {"pane_id": show["pane_id"], "text": "4"})
                await until(spage, "window.__shux.text().includes('Author: Demo Dev')")
                await settled(spage)
                await spage.screenshot(path=str(out / "showcase_guest_lazygit_1280.png"))
                await sctx.close()
                rpc(bin_, env, "session.kill", {"id": show["id"]})

            step("shux share stop: the guest is told, and the link is dead")
            await page.set_viewport_size({"width": 1400, "height": 900})
            stopped = shux(bin_, env, "share", "stop")
            check(stopped["links_ended"] >= 1, "stop ended the link")
            await until(page, "window.__shux.phase === 'ended'")
            msg = await page.inner_text("#gate-text")
            # A stream cut by the stop says "stopped"; one that was between
            # reconnects learns only that the link has ended.
            check(any(w in msg.lower() for w in ("stopped", "ended")), f"the guest is told it ended ({msg.strip()!r})")
            await ctx.close()
            await chrome.close()
        check(shux(bin_, env, "share", "list")["tunnel"] == "down", "share list: tunnel down")
        # The edge keeps the name a moment after the connector goes.
        dead = 0
        for _ in range(30):
            dead = status_of(url.split("#")[0])
            if dead != 200:
                break
            await asyncio.sleep(1)
        check(dead != 200, f"the link no longer answers ({dead or 'no answer'})")
    except Exception as e:  # noqa: BLE001 - reported as a failure, then cleaned up
        check(False, f"the run broke: {e}")
        try:
            for ev in rpc(bin_, env, "events.history", {"filter": ["share."], "count": 50})["events"]:
                print(f"{YELLOW}  event {ev['type']} {json.dumps(ev['data'].get('data'))}{OFF}")
        except Exception as e2:  # noqa: BLE001 - diagnostics only
            print(f"{YELLOW}  no events: {e2}{OFF}")
    finally:
        pid = (work / "run/shux/shux.pid").read_text().strip() if (work / "run/shux/shux.pid").exists() else ""
        if pid:
            subprocess.run([bin_, "share", "stop"], env=env, capture_output=True)
            os.kill(int(pid), 15)
            for _ in range(100):
                try:
                    os.kill(int(pid), 0)
                except ProcessLookupError:
                    break
                time.sleep(0.1)
        shutil.rmtree(work, ignore_errors=True)
    if FAILURES:
        print(f"{RED}✗ {len(FAILURES)} check(s) failed{OFF}")
        return 1
    print(f"{GREEN}✓ every tunnel check passed; evidence in {out}{OFF}")
    return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
