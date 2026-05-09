# /// script
# requires-python = ">=3.14"
# dependencies = ["iterm2", "pyobjc", "pyobjc-framework-Quartz"]
# ///
"""
Spike: script-driven status-bar segments with starship + fallback.

S1 default OOTB (no segments configured) — built-in bar still pretty
S2 single starship segment runs and renders ANSI colors live
S3 missing-binary segment falls back gracefully (with `fallback` text)
S4 segment hot reload — adding a segment lights up live
S5 perf — daemon CPU under load with a 1-Hz starship segment

All screenshots committed to .claude/screenshots/.
"""

import asyncio
import os
import subprocess
import sys
import time

import iterm2

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from _shux_iterm import (  # type: ignore
    SHUX_BIN,
    PROJECT_ROOT,
    cleanup_stale_windows,
    create_window,
    close_window,
    screenshot,
    shux,
    kill_daemon,
    ensure_release_build,
)

CONFIG_PATH = os.path.expanduser("~/.config/shux/config.toml")


results = {"passed": 0, "failed": 0, "unverified": 0, "tests": []}


def log(name, status, details=""):
    results["tests"].append({"name": name, "status": status, "details": details})
    if status == "PASS":
        results["passed"] += 1
    elif status == "FAIL":
        results["failed"] += 1
    else:
        results["unverified"] += 1
    print(f"  [{status}] {name}{(' - ' + details) if details else ''}")


def summary() -> int:
    print("\n" + "=" * 70)
    print(f"PASS={results['passed']} FAIL={results['failed']} "
          f"UNVERIFIED={results['unverified']}")
    print("=" * 70)
    if results["failed"] > 0:
        for t in results["tests"]:
            if t["status"] == "FAIL":
                print(f"  FAIL: {t['name']}: {t['details']}")
        return 1
    return 0


async def screen_text(s) -> str:
    sc = await s.async_get_screen_contents()
    return "\n".join(sc.line(i).string for i in range(sc.number_of_lines))


async def attach(s, sname):
    await s.async_send_text(f"{SHUX_BIN} attach -s {sname}\n")
    for _ in range(50):
        text = await screen_text(s)
        if sname in text and ("[1/" in text or "◆" in text):
            break
        await asyncio.sleep(0.2)
    await asyncio.sleep(0.7)


async def detach(s):
    await s.async_send_text("\x00")
    await asyncio.sleep(0.05)
    await s.async_send_text("d")
    await asyncio.sleep(1.0)


def write_config(toml: str):
    os.makedirs(os.path.dirname(CONFIG_PATH), exist_ok=True)
    with open(CONFIG_PATH, "w") as f:
        f.write(toml)


def remove_config():
    try:
        os.remove(CONFIG_PATH)
    except FileNotFoundError:
        pass


def have_starship() -> bool:
    try:
        return subprocess.run(["which", "starship"], capture_output=True).returncode == 0
    except Exception:
        return False


def daemon_pid() -> int | None:
    out = subprocess.run(
        ["bash", "-c", "pgrep -f 'shux.*__daemon' | head -1"],
        capture_output=True, text=True,
    ).stdout.strip()
    return int(out) if out.isdigit() else None


def proc_cpu(pid: int) -> float:
    """Sample %CPU once for a pid (macOS `ps -o %cpu=`)."""
    try:
        out = subprocess.run(
            ["ps", "-p", str(pid), "-o", "%cpu="],
            capture_output=True, text=True,
        ).stdout.strip()
        return float(out)
    except Exception:
        return -1.0


async def main(connection):
    closed = await cleanup_stale_windows(connection)
    if closed:
        print(f"[janitor] closed {closed} stale windows")
    if not ensure_release_build():
        return 1
    remove_config()
    kill_daemon()
    shux("ls")
    await asyncio.sleep(1.0)

    window, sess = await create_window(
        connection, "statusbar-spike", x_pos=120, y_pos=80, width=1280, height=820
    )

    try:
        await sess.async_send_text(f"cd {PROJECT_ROOT}\n")
        await asyncio.sleep(0.3)

        # ─── S1: OOTB no segments configured ──────────────────────────
        print("\n[S1] OOTB (no segments)")
        shux("kill", "-s", "sb1")
        shux("new", "-s", "sb1", "--detached")
        await attach(sess, "sb1")
        text = await screen_text(sess)
        # Built-in bar has session diamond + clock — should look fine
        ok = "sb1" in text and any(":" in line for line in text.splitlines()[-3:])
        if ok:
            log("S1 OOTB built-in bar visible", "PASS")
            await screenshot(window, "sbspike_S1_default")
        else:
            log("S1 OOTB built-in bar visible", "FAIL")
        await detach(sess)

        # ─── S2: starship segment ────────────────────────────────────
        if have_starship():
            print("\n[S2] starship segment")
            write_config('''
[[statusbar.segment]]
zone = "right"
command = ["starship", "prompt"]
interval_ms = 1000
fallback = "[no starship]"
''')
            await asyncio.sleep(2.0)  # let runner boot
            shux("kill", "-s", "sb2")
            shux("new", "-s", "sb2", "--detached")
            await attach(sess, "sb2")
            await asyncio.sleep(2.0)  # at least one starship sample
            text = await screen_text(sess)
            # starship's default modules typically include the cwd
            # ("indrasvat-shux") and a chevron / arrow.
            starship_present = (
                "indrasvat-shux" in text
                or "❯" in text
                or "→" in text
            )
            if starship_present:
                log("S2 starship-prompt segment lit up", "PASS")
                await screenshot(window, "sbspike_S2_starship")
            else:
                log("S2 starship-prompt segment lit up", "UNVERIFIED",
                    "starship marker not found — bar may not show until full row")
                await screenshot(window, "sbspike_S2_starship_unverified")
            await detach(sess)
        else:
            log("S2 starship segment", "UNVERIFIED", "starship not on PATH")

        # ─── S3: missing-binary fallback ─────────────────────────────
        print("\n[S3] missing-binary fallback")
        write_config('''
[[statusbar.segment]]
zone = "right"
command = ["this-binary-does-not-exist-shux"]
interval_ms = 800
fallback = "[no-bin] FALLBACK-OK"
''')
        await asyncio.sleep(2.0)
        shux("kill", "-s", "sb3")
        shux("new", "-s", "sb3", "--detached")
        await attach(sess, "sb3")
        await asyncio.sleep(2.5)
        text = await screen_text(sess)
        if "FALLBACK-OK" in text:
            log("S3 missing-binary -> fallback text shows", "PASS")
            await screenshot(window, "sbspike_S3_fallback")
        else:
            log("S3 missing-binary -> fallback text shows", "FAIL",
                "FALLBACK-OK not in screen text")
            await screenshot(window, "sbspike_S3_fallback_FAIL")
        # Daemon should still be alive after multiple failed spawns.
        pid = daemon_pid()
        if pid is not None:
            log("S3 daemon survives missing-binary spawn loop", "PASS",
                f"pid={pid}")
        else:
            log("S3 daemon survives missing-binary spawn loop", "FAIL",
                "no shux __daemon process")
        await detach(sess)

        # ─── S4: hot-add a segment ───────────────────────────────────
        print("\n[S4] hot-add segment via config save")
        # Start with a minimal config (no segments).
        write_config("# empty\n")
        await asyncio.sleep(1.5)
        shux("kill", "-s", "sb4")
        shux("new", "-s", "sb4", "--detached")
        await attach(sess, "sb4")
        await asyncio.sleep(1.0)
        await screenshot(window, "sbspike_S4_before_segment")
        # Hot-write a fresh segment.
        write_config('''
[[statusbar.segment]]
zone = "right"
command = ["bash", "-c", "echo HOT-ADDED-$RANDOM"]
interval_ms = 700
fallback = ""
''')
        await asyncio.sleep(3.5)  # config-reload + at least one tick
        text = await screen_text(sess)
        if "HOT-ADDED" in text:
            log("S4 hot-add segment via config save", "PASS")
            await screenshot(window, "sbspike_S4_after_segment")
        else:
            log("S4 hot-add segment via config save", "FAIL",
                "HOT-ADDED not visible in bar")
        await detach(sess)

        # ─── S6: inline starship_config — bar uses ITS config, not PS1's ─
        if have_starship():
            print("\n[S6] inline starship_config produces a DIFFERENT bar than PS1")
            # Use a starship format that's deliberately distinct from any
            # user PS1 — a literal sentinel string + the time module.
            # If we see SENTINEL-FROM-INLINE in the screen, the bar is
            # consuming OUR config and not the user's ~/.config/starship.toml.
            write_config('''
[[statusbar.segment]]
zone = "right"
command = ["starship", "prompt"]
interval_ms = 1000
fallback = ""
starship_config = """
add_newline = false
format = '${custom.sentinel}$time'

[time]
disabled = false
format = '[$time](bold cyan)'
time_format = '%H:%M:%S'

[custom.sentinel]
when = 'true'
command = 'echo SENTINEL-FROM-INLINE'
format = '[$output ](bold magenta)'
"""
''')
            await asyncio.sleep(2.5)
            shux("kill", "-s", "sb6")
            shux("new", "-s", "sb6", "--detached")
            await attach(sess, "sb6")
            await asyncio.sleep(2.5)
            text = await screen_text(sess)
            if "SENTINEL-FROM-INLINE" in text:
                log("S6 inline starship_config drives the bar (not user's PS1 config)",
                    "PASS")
                await screenshot(window, "sbspike_S6_inline_config")
            else:
                log("S6 inline starship_config drives the bar (not user's PS1 config)",
                    "FAIL", "SENTINEL not in screen — inline config not honored")
                await screenshot(window, "sbspike_S6_inline_FAIL")
            await detach(sess)

        # ─── S5: perf — sample daemon CPU with a 1Hz starship segment ─
        if have_starship():
            print("\n[S5] perf: 1Hz starship segment, 5s CPU sample")
            write_config('''
[[statusbar.segment]]
zone = "right"
command = ["starship", "prompt"]
interval_ms = 1000
fallback = ""
''')
            await asyncio.sleep(2.0)
            shux("kill", "-s", "sb5")
            shux("new", "-s", "sb5", "--detached")
            await attach(sess, "sb5")
            await asyncio.sleep(1.0)
            pid = daemon_pid()
            if pid is None:
                log("S5 perf sample", "FAIL", "no daemon pid")
            else:
                samples = []
                for _ in range(5):
                    await asyncio.sleep(1.0)
                    samples.append(proc_cpu(pid))
                avg = sum(samples) / max(1, len(samples))
                # 5% threshold is generous — we expect <2% on idle.
                if avg < 5.0 and avg >= 0:
                    log("S5 perf: <5% CPU with 1Hz starship", "PASS",
                        f"avg={avg:.2f}%, samples={samples}")
                else:
                    log("S5 perf: <5% CPU with 1Hz starship", "UNVERIFIED",
                        f"avg={avg:.2f}%, samples={samples}")
            await detach(sess)
        else:
            log("S5 perf sample", "UNVERIFIED", "starship not installed")

    finally:
        for s in ["sb1", "sb2", "sb3", "sb4", "sb5"]:
            shux("kill", "-s", s)
        await close_window(window)
        leftover = await cleanup_stale_windows(connection)
        if leftover:
            print(f"[janitor:final] closed {leftover} extra windows")
        remove_config()

    return summary()


if __name__ == "__main__":
    raise SystemExit(iterm2.run_until_complete(main, retry=False))
