# /// script
# requires-python = ">=3.11"
# dependencies = ["matplotlib>=3.9"]
# ///
"""Charts for the flake gate's before/after evidence.

Reads what flake_gate_proof.sh and flake_gate_replay.sh wrote and renders PNGs
at 2x into .shux/out/flake-gate-charts/:

  uv run --script .shux/scripts/flake_gate_charts.py [ROOT]
"""

import json
import sys
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402

BEFORE = "#9aa0a6"  # muted grey
AFTER = "#1a73e8"  # strong blue
PROJECTED = {BEFORE: "#e3e5e8", AFTER: "#c2d7fb"}  # light fills, hatched
DPI = 200

root = Path(sys.argv[1] if len(sys.argv) > 1 else ".").resolve()
out_dir = root / ".shux/out/flake-gate-charts"
out_dir.mkdir(parents=True, exist_ok=True)


def rows(path: Path) -> list[dict]:
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def finish(fig, ax, name: str, source: str) -> None:
    ax.spines[["top", "right"]].set_visible(False)
    fig.text(0.01, 0.01, f"Source: {source}", fontsize=8, color="#5f6368")
    fig.tight_layout(rect=(0, 0.03, 1, 1))
    fig.savefig(out_dir / name, dpi=DPI)
    plt.close(fig)
    print(out_dir / name)


def bars(ax, labels, before, after, log: bool) -> None:
    ys = range(len(labels))
    h = 0.38
    ax.barh([y - h / 2 for y in ys], before, h, color=BEFORE, label="before (per-module gate)")
    ax.barh([y + h / 2 for y in ys], after, h, color=AFTER, label="after (per-test gate)")
    for y, b, a in zip(ys, before, after):
        ax.text(max(b, 1), y - h / 2, f" {b}", va="center", fontsize=8, color="#3c4043")
        ax.text(max(a, 1), y + h / 2, f" {a}", va="center", fontsize=8, color=AFTER, fontweight="bold")
    ax.set_yticks(list(ys), labels)
    ax.invert_yaxis()
    if log:
        ax.set_xscale("log")
    legend_above(ax)


def legend_above(ax, **kw) -> None:
    ax.legend(loc="lower left", bbox_to_anchor=(0, 1.0), ncol=4, frameon=False, fontsize=9, **kw)


# (c) synthetic change kinds
proof = root / ".shux/out/flake-gate-proof/cases.jsonl"
if proof.exists():
    cases = rows(proof)
    names = {
        "unit-test-edit": "edit inside a unit test",
        "unit-test-new": "new unit test",
        "integration-test-edit": "edit inside an integration test",
        "integration-test-new": "new integration test",
        "helper-change": "test helper changed\n(≥ before by design)",
    }
    fig, ax = plt.subplots(figsize=(9, 4.2))
    bars(
        ax,
        [names.get(c["case"], c["case"]) for c in cases],
        [c["before"]["tests"] for c in cases],
        [c["after"]["tests"] for c in cases],
        log=True,
    )
    ax.set_xlabel("tests selected (log scale), from cargo nextest list -E")
    ax.set_title("Flake gate: tests selected per kind of change", fontweight="bold", pad=26)
    finish(fig, ax, "change-kinds-tests.png", ".shux/scripts/flake_gate_proof.sh on HEAD")
    timed = [c for c in cases if c["before"]["secs"] is not None]
    if timed:
        fig, ax = plt.subplots(figsize=(9, 4.2))
        bars(
            ax,
            [names.get(c["case"], c["case"]) for c in timed],
            [c["before"]["secs"] for c in timed],
            [c["after"]["secs"] for c in timed],
            log=False,
        )
        ax.set_xlabel("seconds for 20 runs under CPU load")
        ax.set_title("Flake gate: wall clock per kind of change", fontweight="bold", pad=26)
        finish(fig, ax, "change-kinds-time.png", ".shux/scripts/flake_gate_proof.sh time")

# (a) and (b) replayed PRs
replay = root / ".shux/out/flake-gate-replay/prs.jsonl"
if replay.exists():
    prs = sorted(rows(replay), key=lambda r: r["pr"])
    labels = [f"#{r['pr']}" for r in prs]
    fig, ax = plt.subplots(figsize=(9, max(4, 0.32 * len(prs) + 1.5)))
    bars(ax, labels, [r["before"]["tests"] for r in prs], [r["after"]["tests"] for r in prs], log=True)
    ax.set_xlabel("tests selected (log scale), from cargo nextest list -E")
    tb = sum(r["before"]["tests"] for r in prs)
    ta = sum(r["after"]["tests"] for r in prs)
    ax.set_title(f"Flake gate on the last {len(prs)} merged PRs: tests selected\n"
                 f"total {tb} → {ta} ({100 * (tb - ta) / max(tb, 1):.0f}% fewer)",
                 fontweight="bold", pad=26)
    finish(fig, ax, "prs-tests.png", ".shux/scripts/flake_gate_replay.sh, each PR against its parent")

    # Time: measured where timed; elsewhere seconds-per-test from the measured runs.
    measured = [(r["before"], r["after"]) for r in prs if r["before"]["secs"] is not None]
    if measured:
        pts = [(s["tests"], s["secs"]) for pair in measured for s in pair if s["tests"]]
        rate = sum(t for _, t in pts) / max(sum(n for n, _ in pts), 1)
        fig, ax = plt.subplots(figsize=(9, max(4, 0.32 * len(prs) + 1.5)))
        ys = range(len(prs))
        h = 0.38
        for y, r in zip(ys, prs):
            for side, off, color in (("before", -h / 2, BEFORE), ("after", h / 2, AFTER)):
                s = r[side]
                real = s["secs"] is not None
                secs = s["secs"] if real else s["tests"] * rate
                ax.barh(y + off, secs / 60, h, color=color if real else PROJECTED[color],
                        hatch=None if real else "///", edgecolor="white" if real else color)
                ax.text(secs / 60, y + off, f" {secs / 60:.1f}{'' if real else '*'}", va="center", fontsize=8)
        ax.set_yticks(list(ys), labels)
        ax.invert_yaxis()
        ax.set_xlabel("minutes for 20 runs under CPU load")
        ax.set_title(f"Flake gate on merged PRs: wall clock\n"
                     f"* projected at {rate:.2f} s per selected test, the rate of the {len(pts)} measured runs",
                     fontweight="bold", pad=26)
        from matplotlib.patches import Patch

        legend_above(ax, handles=[Patch(color=BEFORE, label="before, measured"),
                                  Patch(color=AFTER, label="after, measured"),
                                  Patch(facecolor=PROJECTED[BEFORE], hatch="///", edgecolor=BEFORE,
                                        label="before, projected*"),
                                  Patch(facecolor=PROJECTED[AFTER], hatch="///", edgecolor=AFTER,
                                        label="after, projected*")])
        finish(fig, ax, "prs-time.png", "measured: 20 gated runs on the PR's own diff (#320, #321); * projected")
