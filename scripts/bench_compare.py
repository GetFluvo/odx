"""Compare two benchmark runs as a pull request comment.

Usage:
    python scripts/bench_compare.py BASE.json HEAD.json [--runs N]

BASE.json and HEAD.json are the results.json files scripts/benchmark.py
writes (seconds per tool and scenario). Prints Markdown: one row per tool
with base, head and the change per scenario; changes beyond the noise
threshold are in bold. The first line is a marker, so the comment can be
found and updated.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path

MARKER = "<!-- odx:bench -->"
SCENARIOS = ["cold", "warm", "no-op"]
# GitHub runners vary by about this much between runs of the same binary;
# the short warm and no-op runs vary most.
NOISE = {"cold": 0.10, "warm": 0.20, "no-op": 0.20}
# A tenth of a second run varies by a few hundredths, whatever the change.
FLOOR = 0.05


def cell(base: float | None, head: float | None, noise: float) -> str:
    if base is None or head is None:
        return "-" if head is None else f"{head:.2f} s"
    change = (head - base) / base if base else 0.0
    text = f"{base:.2f} s → {head:.2f} s ({change:+.0%})"
    return f"**{text}**" if abs(change) > noise and abs(head - base) > FLOOR else text


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("base", type=Path)
    parser.add_argument("head", type=Path)
    parser.add_argument("--runs", type=int, default=3)
    args = parser.parse_args()
    base = json.loads(args.base.read_text())
    head = json.loads(args.head.read_text())
    lines = [
        MARKER,
        "### Benchmark",
        "",
        "| Tool | " + " | ".join(SCENARIOS) + " |",
        "| --- |" + " --- |" * len(SCENARIOS),
    ]
    for tool in head:
        cells = " | ".join(
            cell(base.get(tool, {}).get(s), head[tool].get(s), NOISE[s])
            for s in SCENARIOS
        )
        lines.append(f"| {tool} | {cells} |")
    lines += [
        "",
        (
            f"Base against this pull request, median of {args.runs} runs on a GitHub "
            f"runner ({os.cpu_count()} CPUs), the fixture of docs/benchmarks.md from "
            f"local mirrors. Not a gate: changes within ±{NOISE['cold']:.0%} (cold) "
            f"and ±{NOISE['warm']:.0%} (warm, no-op), or of {FLOOR:.2f} s at most, "
            "are runner noise; larger ones are in bold."
        ),
    ]
    print("\n".join(lines))


if __name__ == "__main__":
    main()
