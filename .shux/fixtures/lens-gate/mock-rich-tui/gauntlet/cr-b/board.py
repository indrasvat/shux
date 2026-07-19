"""Deploy board — a deterministic `rich` TUI used as the lens-gate gauntlet target.

Fixed data, no clock, no randomness, no network. The one time-like field is derived
from `SOURCE_DATE_EPOCH` (the gate pins it to 0), so every run paints the same frame.

Keys: `j`/`k` move the selection, `r` toggles the refreshed marker, `q` quits.
The app draws and then BLOCKS on stdin, so the pane holds a quiet frame at capture.
"""

import os
import sys
import termios
import tty
from datetime import UTC, datetime

from rich.console import Console
from rich.table import Table
from rich.text import Text

from palette import STATUS

# Deliberate color-class coverage so a monochrome/NO_COLOR regression cannot pass
# unnoticed: truecolor for the title, 256-indexed for the version column.
TITLE_STYLE = "bold #ff7a18"
VERSION_STYLE = "color(153)"

SERVICES = [
    ("api-gateway", "healthy", "4.2.1", "6/6", "18ms"),
    ("auth-worker", "healthy", "4.2.1", "3/3", "24ms"),
    ("billing-sync", "degraded", "3.9.7", "2/4", "310ms"),
    ("search-index", "healthy", "4.1.0", "4/4", "42ms"),
    ("mail-relay", "pending", "4.2.1", "0/2", "—"),
    ("media-proxy", "healthy", "4.2.1", "8/8", "11ms"),
    ("report-batch", "failed", "3.9.7", "0/1", "—"),
    ("webhook-fan", "healthy", "4.0.3", "5/5", "63ms"),
]


def deploy_stamp() -> str:
    epoch = int(os.environ.get("SOURCE_DATE_EPOCH", "0"))
    return datetime.fromtimestamp(epoch, UTC).strftime("%Y-%m-%d %H:%M UTC")


def render(console: Console, selected: int, refreshed: bool) -> None:
    console.clear()
    console.rule(Text("shux deploy board", style=TITLE_STYLE), style="blue")

    # Explicit widths (never `expand`): the rendered table must be narrower than the
    # 80-column pane, or the last cell lands on the wrap column and every row doubles.
    table = Table(pad_edge=False, header_style="bold white")
    table.add_column("SERVICE", width=20)
    table.add_column("STATUS", width=10)
    table.add_column("VERSION", width=9)
    table.add_column("REPLICAS", width=9, justify="right")
    table.add_column("LATENCY", width=9, justify="right")

    for index, (name, status, version, replicas, latency) in enumerate(SERVICES):
        marker = "▸ " if index == selected else "  "
        row_style = "reverse" if index == selected else ""
        table.add_row(
            Text(marker + name),
            Text(status, style=STATUS[status]),
            Text(version, style=VERSION_STYLE),
            Text(replicas),
            Text(latency),
            style=row_style,
        )

    console.print(table)

    counts = {key: 0 for key in STATUS}
    for _, status, *_ in SERVICES:
        counts[status] += 1

    summary = Text("  ")
    summary.append(f"{counts['healthy']} healthy", style=STATUS["healthy"])
    summary.append("  ·  ")
    summary.append(f"{counts['degraded']} degraded", style=STATUS["degraded"])
    summary.append("  ·  ")
    summary.append(f"{counts['failed']} failed", style=STATUS["failed"])
    summary.append("  ·  ")
    summary.append(f"{counts['pending']} pending", style=STATUS["pending"])
    console.print(summary)

    state = "refreshed" if refreshed else "cached"
    console.print(
        Text(f"  last deploy {deploy_stamp()}  ·  {state}", style="dim"),
    )


def main() -> int:
    console = Console(force_terminal=True)
    selected = 0
    refreshed = False

    fd = sys.stdin.fileno()
    saved = termios.tcgetattr(fd)
    try:
        # cbreak, never raw: raw clears ONLCR too, so every `\n` would stop mapping to
        # `\r\n` and the whole board would render as a staircase.
        tty.setcbreak(fd)
        while True:
            render(console, selected, refreshed)
            key = sys.stdin.read(1)
            if key in ("q", "\x03", ""):
                return 0
            if key == "j":
                selected = min(selected + 1, len(SERVICES) - 1)
            elif key == "k":
                selected = max(selected - 1, 0)
            elif key == "r":
                refreshed = not refreshed
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, saved)


if __name__ == "__main__":
    raise SystemExit(main())
