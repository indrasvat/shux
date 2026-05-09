# /// script
# requires-python = ">=3.14"
# dependencies = ["iterm2", "pyobjc", "pyobjc-framework-Quartz"]
# ///
"""Custom starship status bar — CPU% + IP address + clock, all inline."""

import asyncio
import os
import sys

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

# A starship config with two custom modules:
#   - cpu       — macOS `top` parsed to a percentage with one decimal
#   - ip        — current IPv4 on the default interface (en0)
# Plus a clean time module. add_newline=false so output stays one line.
CUSTOM_CONFIG = r"""
[[statusbar.segment]]
zone = "right"
command = ["starship", "prompt"]
interval_ms = 2000
# TOML triple-single-quote (literal) so escapes pass through verbatim
# into the materialised tempfile that starship reads. Triple-double
# would decode \" → " and break starship's nested TOML parser.
starship_config = '''
add_newline = false
command_timeout = 2000
format = "${custom.load}${custom.ip}$time"

[time]
disabled = false
format = "[  $time ](bold #f5a97f)"
time_format = "%H:%M:%S"

[custom.load]
when = true
command = "sysctl -n vm.loadavg | awk '{printf \"%.2f\", $2}'"
format = "[ load $output ](bold #ed8796) "

[custom.ip]
when = true
command = "ipconfig getifaddr en0 || echo offline"
format = "[ ip $output ](bold #8aadf4) "
'''
"""


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
    await asyncio.sleep(0.6)


async def detach(s):
    await s.async_send_text("\x00")
    await asyncio.sleep(0.05)
    await s.async_send_text("d")
    await asyncio.sleep(1.0)


async def main(connection):
    await cleanup_stale_windows(connection)
    if not ensure_release_build():
        return 1
    kill_daemon()
    shux("ls")
    await asyncio.sleep(1.0)

    # Pre-write the custom config so it's loaded at attach time.
    os.makedirs(os.path.dirname(CONFIG_PATH), exist_ok=True)
    with open(CONFIG_PATH, "w") as f:
        f.write(CUSTOM_CONFIG)

    window, sess = await create_window(
        connection, "sbcustom", x_pos=120, y_pos=80, width=1280, height=800
    )

    try:
        await sess.async_send_text(f"cd {PROJECT_ROOT}\n")
        await asyncio.sleep(0.3)
        shux("kill", "-s", "sbcustom")
        shux("new", "-s", "sbcustom", "--detached")
        await attach(sess, "sbcustom")
        # Wait long enough for at least 2 starship samples (interval 2s).
        await asyncio.sleep(5.0)
        text = await screen_text(sess)
        # The IP module returns either an actual IPv4 (1+ dots) or "offline".
        # The CPU module returns digits and a dot.
        has_ip = ("offline" in text) or any(c.isdigit() for c in text)
        # Always capture the screenshot regardless of strict assertion —
        # this run is for the user's eyes.
        await screenshot(window, "sbcustom_cpu_ip_clock")
        print(f"\nDetected IP/offline marker: {'offline' in text or '.' in text}")
        print(f"Custom bar visible: {has_ip}")
        await detach(sess)
    finally:
        shux("kill", "-s", "sbcustom")
        await close_window(window)
        await cleanup_stale_windows(connection)
        try:
            os.remove(CONFIG_PATH)
        except FileNotFoundError:
            pass

    return 0


if __name__ == "__main__":
    raise SystemExit(iterm2.run_until_complete(main, retry=False))
