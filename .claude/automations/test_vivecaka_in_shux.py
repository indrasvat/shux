# /// script
# requires-python = ">=3.14"
# dependencies = ["iterm2", "pyobjc", "pyobjc-framework-Quartz"]
# ///
"""
Real-world test: launch vivecaka (TUI PR reviewer) INSIDE a shux pane,
navigate to PR #1, view its description, comments, files. Screenshots
prove the multiplexer carries a complex 24-bit-color TUI cleanly.

Steps:
  V1  shux attach + launch `vivecaka --repo indrasvat/shux`, screenshot
      the PR list view
  V2  arrow-down to the shux PR, screenshot the highlighted row
  V3  Enter to open it, screenshot the PR detail (description, meta)
  V4  navigate within the PR (tabs/sections — vivecaka uses numbers
      or shortcuts), screenshot conversation/comments view
  V5  open files tab, screenshot the diff view
  V6  return / exit cleanly

We're not asserting much programmatically — vivecaka's exact UI text
varies. The point is: a complex TUI works, screenshots are captured.
"""

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

    window, sess = await create_window(
        connection, "vivecaka", x_pos=120, y_pos=80, width=1400, height=900
    )

    try:
        await sess.async_send_text(f"cd {PROJECT_ROOT}\n")
        await asyncio.sleep(0.3)
        shux("kill", "-s", "vc")
        shux("new", "-s", "vc", "--detached")
        await attach(sess, "vc")

        # ─── V1: launch vivecaka ─────────────────────────────────────
        print("\n[V1] launching vivecaka --repo indrasvat/shux")
        await sess.async_send_text("vivecaka --repo indrasvat/shux\n")
        # vivecaka spawns a TUI and fetches PRs — give it time
        await asyncio.sleep(6.0)
        await screenshot(window, "vivecaka_01_pr_list")

        # ─── V2: highlight the PR with arrow keys ────────────────────
        # vivecaka opens with the first PR usually highlighted; we
        # just confirm it's interactive by pressing j (vim-down) and k
        # (vim-up) and then capturing.
        print("[V2] navigate within list (j/k)")
        await sess.async_send_text("j")
        await asyncio.sleep(0.4)
        await sess.async_send_text("k")
        await asyncio.sleep(0.4)
        await screenshot(window, "vivecaka_02_after_nav")

        # ─── V3: open the PR ─────────────────────────────────────────
        print("[V3] Enter -> open PR detail")
        await sess.async_send_text("\r")
        await asyncio.sleep(3.0)
        await screenshot(window, "vivecaka_03_pr_detail")

        # ─── V4: try a Tab/sections shortcut ─────────────────────────
        # Most TUI PR reviewers use Tab to cycle sections. Try Tab,
        # then capture; then '2' (common shortcut for "comments").
        print("[V4] Tab to next section, then '2' for comments")
        await sess.async_send_text("\t")
        await asyncio.sleep(1.0)
        await screenshot(window, "vivecaka_04_after_tab")

        await sess.async_send_text("2")
        await asyncio.sleep(1.5)
        await screenshot(window, "vivecaka_05_section_2")

        # ─── V5: try files tab ───────────────────────────────────────
        # '3' or 'f' is often files in TUI PR reviewers.
        print("[V5] '3' / 'f' for files / diff")
        await sess.async_send_text("3")
        await asyncio.sleep(1.5)
        await screenshot(window, "vivecaka_06_section_3")

        # And one more deep capture — scroll within whatever pane we're on
        await sess.async_send_text("j" * 6)
        await asyncio.sleep(0.8)
        await screenshot(window, "vivecaka_07_scrolled")

        # ─── V6: exit cleanly ────────────────────────────────────────
        print("[V6] q to exit vivecaka")
        await sess.async_send_text("q")
        await asyncio.sleep(1.0)
        await sess.async_send_text("q")  # might need second q
        await asyncio.sleep(1.0)
        await screenshot(window, "vivecaka_08_exited")
        await detach(sess)
    finally:
        shux("kill", "-s", "vc")
        await close_window(window)
        await cleanup_stale_windows(connection)

    print("\nDone — see screenshots vivecaka_*.png in .claude/screenshots/")
    return 0


if __name__ == "__main__":
    raise SystemExit(iterm2.run_until_complete(main, retry=False))
