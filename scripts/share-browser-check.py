# /// script
# requires-python = ">=3.12"
# dependencies = ["playwright==1.48.0"]
# ///
"""The share gateway (#297) in a real browser, through a local TLS proxy.

The rig (crates/shux/examples/share_rig.rs) runs the daemon's services and the
gateway; this script puts a TLS-terminating proxy in front, as a tunnel would,
and drives headless Chrome (and WebKit, when Playwright has it installed)
through every lifecycle the design names. No network is used.

Usage: share-browser-check.py <shux binary> <share_rig binary> <evidence dir>
Exit 0 only when every check passed.
"""
import asyncio
import json
import os
import shutil
import ssl
import subprocess
import sys
import tempfile
from pathlib import Path

from playwright.async_api import async_playwright

GREEN, RED, YELLOW, CYAN, OFF = "\033[1;32m", "\033[1;31m", "\033[1;33m", "\033[1;36m", "\033[0m"
BUDGET_MS = 30_000
FAILURES: list[str] = []


def step(msg: str) -> None:
    print(f"{CYAN}▶ {msg}{OFF}", flush=True)


def check(ok: bool, what: str) -> None:
    if ok:
        print(f"  {GREEN}✓{OFF} {what}", flush=True)
    else:
        print(f"  {RED}✗ {what}{OFF}", flush=True)
        FAILURES.append(what)


class Proxy:
    """TLS in, plaintext to the gateway: what a tunnel's edge does."""

    def __init__(self, upstream: tuple[str, int], ctx: ssl.SSLContext):
        self.upstream, self.ctx = upstream, ctx
        self.server: asyncio.base_events.Server | None = None
        self.port = 0
        self.conns: set[asyncio.StreamWriter] = set()

    async def start(self) -> None:
        self.server = await asyncio.start_server(self.handle, "127.0.0.1", self.port, ssl=self.ctx)
        self.port = self.server.sockets[0].getsockname()[1]

    async def outage(self) -> None:
        """Closes the listener and every open connection."""
        assert self.server
        self.server.close()
        for w in list(self.conns):
            w.transport.abort()
        await self.server.wait_closed()

    async def handle(self, r: asyncio.StreamReader, w: asyncio.StreamWriter) -> None:
        try:
            ur, uw = await asyncio.open_connection(*self.upstream)
        except OSError:
            w.close()
            return
        self.conns.update({w, uw})

        async def pipe(src: asyncio.StreamReader, dst: asyncio.StreamWriter) -> None:
            try:
                while data := await src.read(65536):
                    dst.write(data)
                    await dst.drain()
            except (ConnectionError, ssl.SSLError):
                pass
            finally:
                dst.transport.abort()

        await asyncio.gather(pipe(r, uw), pipe(ur, w))
        self.conns.difference_update({w, uw})


class Rig:
    def __init__(self, rig_bin: str, env: dict[str, str]):
        self.proc = subprocess.Popen([rig_bin], stdin=subprocess.PIPE, stdout=subprocess.PIPE, env=env, text=True)
        hello = json.loads(self.proc.stdout.readline())
        host, port = hello["addr"].rsplit(":", 1)
        self.addr = (host, int(port))

    def call(self, **req) -> object:
        self.proc.stdin.write(json.dumps(req) + "\n")
        self.proc.stdin.flush()
        return json.loads(self.proc.stdout.readline())

    def quit(self) -> None:
        if self.proc.poll() is None:
            self.proc.stdin.write(json.dumps({"op": "quit"}) + "\n")
            self.proc.stdin.flush()
            self.proc.wait(timeout=30)


def shux(bin_: str, env: dict[str, str], method: str, params: dict) -> dict:
    out = subprocess.run(
        [bin_, "--format", "json", "rpc", "call", method, "--params", json.dumps(params)],
        env=env, capture_output=True, text=True, check=True,
    )
    return json.loads(out.stdout)["result"]


def recorder(log: Path) -> list[str]:
    script = (
        "printf '\\033[38;2;120;220;180mTRUE%sCOLOR\\033[0m \\033[38;5;208mINDE%sXED\\033[0m "
        "\\033[34mBA%sSIC\\033[0m\\n' '' '' ''; stty raw -echo; printf 'REC%sORDING\\n' ''; "
        f"exec cat > '{log}'"
    )
    return ["sh", "-c", script]


PRODUCER = ["sh", "-c", 'i=0; while :; do i=$((i+1)); echo "TICK $i"; done']


async def until(page, cond: str) -> None:
    """Polls `cond` from outside the page: the page's CSP forbids the eval
    `wait_for_function` injects, and loosening it would test something else."""
    loop = asyncio.get_running_loop()
    deadline = loop.time() + BUDGET_MS / 1000
    while not await page.evaluate(f"() => Boolean({cond})"):
        if loop.time() > deadline:
            raise TimeoutError(f"never held: {cond}")
        await asyncio.sleep(0.05)


async def live(page, word: str | None = None) -> None:
    cond = "window.__shux && window.__shux.phase === 'live'"
    if word:
        cond += f" && window.__shux.text().includes({json.dumps(word)})"
    await until(page, cond)


async def ended(page) -> str:
    await until(page, "window.__shux && window.__shux.phase === 'ended'")
    return await page.inner_text("#gate-text")


async def open_link(ctx, url: str, violations: list[str]):
    page = await ctx.new_page()
    page.on("console", lambda m: m.text.startswith("CSP ") and violations.append(m.text))
    page.on("pageerror", lambda e: violations.append(str(e)))

    def response(r) -> None:
        # An ended link answers its redeem and status with 410; nothing else fails.
        expected = r.status == 410 and r.url.endswith(("/redeem", "/status"))
        if r.status >= 400 and not expected:
            violations.append(f"{r.status} {r.url}")

    page.on("response", response)
    await page.goto(url, wait_until="load")
    return page


async def run_browser(kind: str, browser, rig: Rig, origin: str, env: dict, bin_: str, work: Path, out: Path, proxy: Proxy):
    violations: list[str] = []
    ctx = await browser.new_context(ignore_https_errors=True, viewport={"width": 1400, "height": 900})
    await ctx.add_init_script(
        "document.addEventListener('securitypolicyviolation', e => console.error('CSP ' + e.violatedDirective + ' ' + e.blockedURI));"
    )

    step(f"{kind}: a recorder pane, viewed read-only")
    log = work / f"{kind}-rec.log"
    s = shux(bin_, env, "session.create", {"name": f"{kind}-watched", "command": recorder(log)})
    pane = s["pane_id"]
    before = shux(bin_, env, "pane.capture", {"pane_id": pane})
    link = rig.call(op="create", session=s["id"], uses=1)
    page = await open_link(ctx, link["url"], violations)
    check("#" not in page.url, "the fragment leaves the address bar before anything else")
    check(await page.is_visible("#watch"), "redemption waits for a click on Watch session")
    listed = rig.call(op="list")
    check(any(g["id"] == link["id"] and g["redeemed"] == 0 for g in listed), "loading the page spends no use")
    await page.click("#watch")
    await live(page, "RECORDING")
    text = await page.evaluate("window.__shux.text()")
    check(all(p in text for p in ("TRUECOLOR", "INDEXED", "BASIC")), "colour probes reach the browser")
    check(f"{kind}-watched" not in text and "read-only" not in text, "no native status segment in the stream")
    bar = await page.inner_text("#bar")
    check("Read-only" in bar and "expires at" in bar, f"the bar says read-only and when it expires ({bar.strip()!r})")
    await page.screenshot(path=str(out / f"v1_browser-{kind}_1400_default.png"))
    # The guest types, pastes and clicks: none of it may reach the pane.
    await page.click("#term")
    await page.keyboard.type("guest typed\n")
    await page.keyboard.press("Control+C")
    await page.evaluate("navigator.clipboard?.writeText?.('x').catch(()=>{})")
    await page.mouse.wheel(0, 300)
    await page.set_viewport_size({"width": 390, "height": 844})
    await page.screenshot(path=str(out / f"v1_browser-{kind}_390_default.png"))
    shux(bin_, env, "pane.send_keys", {"pane_id": pane, "text": "MARK"})
    for _ in range(300):
        if log.exists() and log.read_bytes():
            break
        await asyncio.sleep(0.05)
    check(log.read_bytes() == b"MARK", f"the pane received only the owner's marker ({log.read_bytes()[:40]!r})")
    after = shux(bin_, env, "pane.capture", {"pane_id": pane})
    check((before["cols"], before["rows"]) == (after["cols"], after["rows"]), "the guest never sized the pane")

    step(f"{kind}: refresh draws the screen again")
    connects = await page.evaluate("window.__shux.connects")
    await page.reload(wait_until="load")
    await live(page, "RECORDING")
    check(await page.evaluate("window.__shux.connects") == 1, f"a reload resumes on its cookie (was {connects})")

    step(f"{kind}: a one-use link, raced by two browsers")
    raced = rig.call(op="create", session=s["id"], uses=1)
    a_ctx = await browser.new_context(ignore_https_errors=True)
    b_ctx = await browser.new_context(ignore_https_errors=True)
    a = await open_link(a_ctx, raced["url"], violations)
    b = await open_link(b_ctx, raced["url"], violations)
    await asyncio.gather(a.click("#watch"), b.click("#watch"))
    phases = []
    for p in (a, b):
        await until(p, "['live','ended'].includes(window.__shux.phase)")
        phases.append(await p.evaluate("window.__shux.phase"))
    check(sorted(phases) == ["ended", "live"], f"exactly one browser got in ({phases})")
    await a_ctx.close()
    await b_ctx.close()

    step(f"{kind}: two links on one origin, then revoke during a reconnect")
    p_sess = shux(bin_, env, "session.create", {"name": f"{kind}-ticks", "command": PRODUCER})
    second = rig.call(op="create", session=p_sess["id"], uses=1)
    other = await open_link(ctx, second["url"], violations)
    await other.click("#watch")
    await live(other, "TICK")
    check(await page.evaluate("window.__shux.phase") == "live", "the first link still streams beside the second")
    await proxy.outage()
    await until(other, "window.__shux.phase !== 'live'")
    rig.call(op="revoke", id=second["id"])
    await proxy.start()
    msg = await ended(other)
    check("ended" in msg, f"a revoked link stays ended through a reconnect ({msg!r})")
    await live(page)
    check(True, "the first link reconnected after the outage")

    step(f"{kind}: a stalled renderer is dropped, and comes back whole")
    third = rig.call(op="create", session=p_sess["id"], uses=1)
    stall = await open_link(ctx, third["url"], violations)
    await stall.click("#watch")
    await live(stall, "TICK")
    before_connects = await stall.evaluate("window.__shux.connects")
    await stall.evaluate("window.__shux.stall = true")
    await until(stall, "window.__shux.phase !== 'live'")
    await stall.evaluate("window.__shux.stall = false")
    await live(stall, "TICK")
    check(await stall.evaluate("window.__shux.connects") == before_connects + 1, "the lagging viewer reconnected once, to a full redraw")

    step(f"{kind}: deleting the session ends its viewer")
    shux(bin_, env, "session.kill", {"id": p_sess["id"]})
    msg = await ended(stall)
    check("session ended" in msg.lower(), f"the viewer says the session ended ({msg!r})")

    shux(bin_, env, "session.kill", {"id": s["id"]})
    await ended(page)
    check(not violations, f"no CSP violation or page error ({violations[:3]})")
    await ctx.close()


async def main() -> int:
    bin_, rig_bin, out = sys.argv[1], sys.argv[2], Path(sys.argv[3])
    out.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix="s297-", dir="/tmp"))
    env = {
        **{k: v for k, v in os.environ.items() if k not in ("SHUX_SOCKET", "NO_COLOR")},
        "XDG_RUNTIME_DIR": str(work / "run"),
        "XDG_CONFIG_HOME": str(work / "config"),
        "XDG_STATE_HOME": str(work / "state"),
        "SHELL": "/bin/sh",
    }
    (work / "state/shux").mkdir(parents=True)
    (work / "state/shux/onboarding.json").write_text('{"prefix_discovered": true, "welcome_toast_seen": true}')
    env["SHUX_SOCKET"] = str(work / "run" / "rig.sock")
    (work / "run").mkdir(mode=0o700)
    cert, key = work / "cert.pem", work / "key.pem"
    subprocess.run(
        ["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=localhost",
         "-addext", "subjectAltName=DNS:localhost", "-keyout", str(key), "-out", str(cert)],
        check=True, capture_output=True,
    )
    ctx = ssl.create_default_context(ssl.Purpose.CLIENT_AUTH)
    ctx.load_cert_chain(cert, key)
    rig = Rig(rig_bin, env)
    try:
        proxy = Proxy(rig.addr, ctx)
        await proxy.start()
        origin = f"https://localhost:{proxy.port}"
        rig.call(op="origin", origin=origin)
        print(f"{CYAN}gateway {rig.addr[0]}:{rig.addr[1]} behind {origin}{OFF}")
        async with async_playwright() as pw:
            chrome = await pw.chromium.launch(channel="chrome", headless=True)
            await run_browser("chromium", chrome, rig, origin, env, bin_, work, out, proxy)
            await chrome.close()
            try:
                webkit = await pw.webkit.launch(headless=True)
            except Exception as e:  # noqa: BLE001 - reported, never skipped silently
                print(f"{YELLOW}⚠ WebKit not run: {str(e).splitlines()[0]}{OFF}")
            else:
                await run_browser("webkit", webkit, rig, origin, env, bin_, work, out, proxy)
                await webkit.close()
        step("stop ends everything and closes the listener")
        rig.call(op="stop")
        check(rig.call(op="list") == [], "nothing listed after stop")
    finally:
        rig.quit()
        shutil.rmtree(work, ignore_errors=True)
    if FAILURES:
        print(f"{RED}✗ {len(FAILURES)} browser check(s) failed{OFF}")
        return 1
    print(f"{GREEN}✓ every browser check passed; evidence in {out}{OFF}")
    return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
