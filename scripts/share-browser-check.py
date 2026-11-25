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
import re
import shutil
import ssl
import subprocess
import sys
import tempfile
from pathlib import Path

from playwright.async_api import async_playwright

GREEN, RED, YELLOW, CYAN, OFF = "\033[1;32m", "\033[1;31m", "\033[1;33m", "\033[1;36m", "\033[0m"
BUDGET_MS = 30_000
# "23h 51m", "2h", "45m"; then a clock time and its zone: "3:55 PM PST".
LEFT = r"\d+[dhm](?: \d+[hm])?"
CLOCK = r"(?:tomorrow |\w{3}, \w{3} \d+, )?\d{1,2}:\d\d\s?[AP]M [A-Z][\w+:-]+"
# One locale and zone, so the wording the checks read does not depend on the machine.
PINNED = {"ignore_https_errors": True, "locale": "en-US", "timezone_id": "America/Los_Angeles"}
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


async def pan(page, kind: str, stage: str) -> None:
    """Pan the readable view: a wheel where the browser has one; mobile WebKit
    has none, so there the view is scrolled as a swipe would."""
    if kind == "webkit":
        await page.evaluate(f"{stage}.scrollBy(300, 0)")
    else:
        await page.mouse.move(200, 400)
        await page.mouse.wheel(700, 300)
    await until(page, f"{stage}.scrollLeft > 0")


async def until(page, cond: str) -> None:
    """Polls `cond` from outside the page: the page's CSP forbids the eval
    `wait_for_function` injects, and loosening it would test something else."""
    loop = asyncio.get_running_loop()
    deadline = loop.time() + BUDGET_MS / 1000
    while not await page.evaluate(f"() => Boolean({cond})"):
        if loop.time() > deadline:
            raise TimeoutError(f"never held: {cond}")
        await asyncio.sleep(0.05)


async def holds(page, cond: str) -> bool:
    """`until`, as a verdict for `check` instead of an abort."""
    try:
        await until(page, cond)
        return True
    except TimeoutError:
        return False


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
    page.on("console", lambda m: m.text.startswith("CSP-unsourced ") and print(f"  {YELLOW}! {m.text}{OFF}"))
    page.on("pageerror", lambda e: violations.append(str(e)))

    def response(r) -> None:
        # Only a redeem may answer 404 or 410: the status probe always says 200.
        expected = r.status in (404, 410) and r.url.endswith("/redeem")
        if r.status >= 400 and not expected:
            violations.append(f"{r.status} {r.url}")

    page.on("response", response)
    await page.goto(url, wait_until="load")
    return page


async def run_browser(kind: str, browser, rig: Rig, env: dict, bin_: str, work: Path, out: Path, proxy: Proxy):
    violations: list[str] = []
    ctx = await browser.new_context(**PINNED, viewport={"width": 1400, "height": 900})
    await ctx.add_init_script(
        # A violation with no source file came from no script of the page's: Playwright's
        # own injected script draws one in WebKit. It is shown, not counted.
        "document.addEventListener('securitypolicyviolation', e => console.error((e.sourceFile ? 'CSP ' : 'CSP-unsourced ')"
        " + e.violatedDirective + ' ' + e.blockedURI + ' at ' + e.sourceFile + ':' + e.lineNumber));"
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
    peeked = await holds(page, "document.querySelector('#gate-when')?.textContent.includes('until')")
    gate = " ".join((await page.inner_text("#gate")).split())
    check(peeked and "1 use left" in gate, f"the gate says until when, and the uses left ({gate!r})")
    # The time left, then the end in the guest's own time with its zone: "3:55 PM PST".
    check(re.search(rf"for {LEFT} more, until {CLOCK}", gate) is not None,
          f"the gate says how long is left and until when, with the zone ({gate!r})")
    for line in (
        "A live terminal session is being shared with you",
        "You'll watch it as it happens. You can't type into it.",
        "The person sharing it can see how many people are watching.",
        "Shared with shux",
        "shux.pages.dev",
    ):
        check(line in gate, f"the gate says {line!r}")
    check(f"{kind}-watched" not in gate, "the gate names no session before a use is spent")
    await page.screenshot(caret="initial", path=str(out / f"v3_gate-{kind}_1400.png"))
    gate_bar = " ".join((await page.inner_text("#bar")).split())
    check("Read-only ·" not in gate_bar, f"no separator dangles before a session name ({gate_bar!r})")
    listed = rig.call(op="list")
    check(any(g["id"] == link["id"] and g["redeemed"] == 0 for g in listed), "loading the page spends no use")
    await page.click("#watch")
    await live(page, "RECORDING")
    text = await page.evaluate("window.__shux.text()")
    check(all(p in text for p in ("TRUECOLOR", "INDEXED", "BASIC")), "colour probes reach the browser")
    check(f"{kind}-watched" not in text and "read-only" not in text, "no native status segment in the stream")
    bar = await page.inner_text("#bar")
    check("Read-only" in bar and "until" in bar, f"the bar says read-only and until when ({bar.strip()!r})")
    check(re.search(rf"{LEFT} left", bar) is not None and re.search(rf"until {CLOCK}", bar) is not None,
          f"the bar says how long is left and until when, with the zone ({bar.strip()!r})")
    check(await holds(page, "document.querySelector('#watching')?.textContent.includes('1 watching')"),
          f"the bar says how many watch ({bar.strip()!r})")
    await page.screenshot(caret="initial", path=str(out / f"v1_browser-{kind}_1400_default.png"))
    # The guest types, pastes and clicks: none of it may reach the pane.
    await page.click("#term")
    await page.keyboard.type("guest typed\n")
    toast = await holds(page, "document.querySelector('#toast') && !document.querySelector('#toast').hidden")
    said = await page.evaluate("document.querySelector('#toast')?.textContent ?? ''")
    check(toast and said == "View only: your keys aren't sent.", f"typing says it goes nowhere ({said!r})")
    await page.keyboard.press("Control+C")
    await page.evaluate("navigator.clipboard?.writeText?.('x').catch(()=>{})")
    await page.mouse.wheel(0, 300)
    await page.set_viewport_size({"width": 390, "height": 844})
    await page.screenshot(caret="initial", path=str(out / f"v1_browser-{kind}_390_default.png"))
    shux(bin_, env, "pane.send_keys", {"pane_id": pane, "text": "MARK"})
    for _ in range(300):
        if log.exists() and log.read_bytes():
            break
        await asyncio.sleep(0.05)
    check(log.read_bytes() == b"MARK", f"the pane received only the owner's marker ({log.read_bytes()[:40]!r})")
    after = shux(bin_, env, "pane.capture", {"pane_id": pane})
    check((before["cols"], before["rows"]) == (after["cols"], after["rows"]), "the guest never sized the pane")

    step(f"{kind}: refresh draws the screen again")
    await page.reload(wait_until="load")
    await live(page, "RECORDING")
    check(not await page.is_visible("#watch"), "a reload resumes on its cookie, spending no use")

    step(f"{kind}: a one-use link, raced by two browsers")
    raced = rig.call(op="create", session=s["id"], uses=1)
    a_ctx = await browser.new_context(**PINNED)
    b_ctx = await browser.new_context(**PINNED)
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
    await live(page, "RECORDING")
    check(await page.evaluate("window.__shux.connects") > 1, "the first link reconnected after the outage")

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

    step(f"{kind}: links that ended before they were opened say why")
    t_sess = shux(bin_, env, "session.create", {"name": f"{kind}-ends", "command": PRODUCER})
    revoked = rig.call(op="create", session=t_sess["id"], uses=1)
    rig.call(op="revoke", id=revoked["id"])
    expired = rig.call(op="create", session=t_sess["id"], uses=1, ttl_ms=1000)
    await asyncio.sleep(1.5)  # past the expired link's second
    for gone, want in ((revoked, "owner ended this link"), (expired, "has expired")):
        fresh = await browser.new_context(**PINNED)
        p = await open_link(fresh, gone["url"], violations)
        await p.click("#watch")
        msg = await ended(p)
        check(want in msg, f"a fresh browser is told why: {want} ({msg!r})")
        await fresh.close()
    # Left open on the first page, a link that runs out says so with no click.
    lapsing = rig.call(op="create", session=t_sess["id"], uses=1, ttl_ms=4000)
    fresh = await browser.new_context(**PINNED)
    p = await open_link(fresh, lapsing["url"], violations)
    await holds(p, "document.querySelector('#gate-when')?.textContent.includes('1m more')")
    msg = await ended(p)
    check("has expired" in msg, f"an open first page ends when its link does ({msg!r})")
    await fresh.close()
    shux(bin_, env, "session.kill", {"id": t_sess["id"]})

    step(f"{kind}: a phone reads the host's layout, and pans it")
    plog = work / f"{kind}-phone.log"
    # 30 characters: a long name must not push the state or the button off a phone.
    pname = f"{kind}-phone-".ljust(30, "x")
    ps = shux(bin_, env, "session.create", {"name": pname, "command": recorder(plog)})
    shux(bin_, env, "pane.set_size", {"pane_id": ps["pane_id"], "cols": 200, "rows": 58})
    psize = shux(bin_, env, "pane.capture", {"pane_id": ps["pane_id"]})
    plink = rig.call(op="create", session=ps["id"], uses=1)
    phone = await browser.new_context(
        **PINNED, viewport={"width": 390, "height": 844},
        device_scale_factor=3, is_mobile=True, has_touch=True,
    )
    pp = await open_link(phone, plink["url"], violations)
    await holds(pp, "document.querySelector('#gate-when')?.textContent.includes('until')")
    await pp.screenshot(caret="initial", path=str(out / f"v3_gate-{kind}_390.png"))
    await pp.tap("#watch")
    await live(pp, "RECORDING")
    glyph = await pp.evaluate("window.__shux.glyph")
    check(glyph is not None and glyph >= 9, f"text is readable at 390 px ({glyph} CSS px glyphs)")
    check(await holds(pp, "document.querySelector('#watching')?.textContent.includes('1 watching')"),
          "a phone's bar says how many watch")
    check(await holds(pp, "document.querySelector('#left')?.offsetWidth > 0"
                          " && document.querySelector('#left').textContent.endsWith(' left')"),
          "a phone's bar keeps the time left")
    # Safari's text is wider than Chromium's: at 280 px (a folding phone's
    # cover screen) Chromium's runs out of room the way Safari's does at 402.
    for width in (390, 280):
        await pp.set_viewport_size({"width": width, "height": 844})
        for sel in ("#zoom", "#state"):
            box = await pp.evaluate(
                f"(() => {{ const r = document.querySelector('{sel}').getBoundingClientRect();"
                " return [r.left, r.right, r.width, innerWidth]; })()"
            )
            check(box[2] > 0 and box[0] >= 0 and box[1] <= box[3], f"{sel} sits inside a {width} px phone ({box})")
    await pp.set_viewport_size({"width": 390, "height": 844})
    await pp.evaluate("window.scrollTo(0, 200)")
    scrolled = await pp.evaluate("[scrollY, document.documentElement.scrollHeight, innerHeight]")
    check(scrolled[0] == 0, f"the page itself does not scroll the bar away ({scrolled})")
    stage = "document.querySelector('#stage')"
    check(await pp.evaluate(f"{stage}.scrollWidth > {stage}.clientWidth"), "the readable view pans")
    await pp.screenshot(caret="initial", path=str(out / f"v2_phone-{kind}_390x844_readable.png"))
    await pan(pp, kind, stage)
    check(True, "a pan over the terminal moves the view")
    await pp.screenshot(caret="initial", path=str(out / f"v2_phone-{kind}_390x844_panned.png"))
    await pp.tap("#zoom")
    check(await pp.evaluate("window.__shux.mode") == "fit", "a tap shows the whole layout")
    await pp.screenshot(caret="initial", path=str(out / f"v2_phone-{kind}_390x844_fit.png"))
    await pp.tap("#zoom")
    check(await pp.evaluate("window.__shux.mode") == "readable", "another tap reads it again")
    await pp.tap("#term")
    typing = await pp.evaluate(
        "(() => { const a = document.activeElement;"
        " return !!a && (a.isContentEditable || a.tagName === 'TEXTAREA') && a.inputMode !== 'none'; })()"
    )
    check(not typing, "a tap on the terminal opens no keyboard")
    check(await holds(pp, "document.querySelector('#toast') && !document.querySelector('#toast').hidden"),
          "a tap to type says keys go nowhere")
    shux(bin_, env, "pane.send_keys", {"pane_id": ps["pane_id"], "text": "MARK"})
    for _ in range(300):
        if plog.exists() and plog.read_bytes():
            break
        await asyncio.sleep(0.05)
    check(plog.read_bytes() == b"MARK", f"no phone gesture reached the pane ({plog.read_bytes()[:40]!r})")
    after = shux(bin_, env, "pane.capture", {"pane_id": ps["pane_id"]})
    check((psize["cols"], psize["rows"]) == (after["cols"], after["rows"]), "the phone never sized the pane")
    await pan(pp, kind, stage)
    rig.call(op="revoke", id=plink["id"])
    msg = await ended(pp)
    box = await pp.evaluate(
        "(() => { const r = document.querySelector('#gate').getBoundingClientRect();"
        " return [r.left, r.right, innerWidth]; })()"
    )
    check(
        box[0] >= 0 and box[1] <= box[2] and abs((box[0] + box[1]) / 2 - box[2] / 2) <= 2,
        f"after a pan, the end message is centred on the phone ({box}, {msg!r})",
    )
    await pp.screenshot(caret="initial", path=str(out / f"v2_phone-{kind}_390x844_ended-after-pan.png"))
    await phone.close()
    shux(bin_, env, "session.kill", {"id": ps["id"]})
    check(not violations, f"no CSP violation or page error ({violations[:6]})")
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
            await run_browser("chromium", chrome, rig, env, bin_, work, out, proxy)
            await chrome.close()
            try:
                webkit = await pw.webkit.launch(headless=True)
            except Exception as e:  # noqa: BLE001 - reported, never skipped silently
                print(f"{YELLOW}⚠ WebKit not run: {str(e).splitlines()[0]}{OFF}")
            else:
                await run_browser("webkit", webkit, rig, env, bin_, work, out, proxy)
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
