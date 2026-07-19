"""Shared status palette.

Single source of truth for status colours, so the table and the summary line can't
drift apart.
"""

STATUS = {
    "healthy": "green",
    "degraded": "yellow",
    "failed": "bright_red",
    "pending": "cyan",
}
