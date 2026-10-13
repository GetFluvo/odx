"""Time git-aggregate-rs against git-aggregator's gitaggregate.

Usage:
    python scripts/benchmark.py [--mode mirror|github] [--seed DIR]

Both tools aggregate tests/fixtures/benchmark/repos.yaml (12 OCA
repositories, 25 pull requests) in three scenarios:

- cold:  no cache and no target directories;
- warm:  target directories removed, the git-aggregate-rs cache kept
         (for gitaggregate the target directories are its cache, so warm
         is the same work as cold);
- no-op: everything from the previous run still in place.

In mirror mode `odx aggregate --locked` is timed too, on the same fixture
as a [tool.odx] project locked once beforehand. Each is timed with one job
and with one job per CPU; the median of the
runs after one warm-up run is reported. Every run's result is checked
against the expected trees in tests/fixtures/benchmark/trees.json, so a
timing is only accepted for the same work.

In mirror mode (the default) the repositories are served from local bare
mirrors over file://, so the network does not dominate the numbers;
--seed points at existing clones (for example ~/git/OCA) to build the
mirrors without downloading full histories. In github mode they are
fetched from https://github.com.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FIXTURE = ROOT / "tests" / "fixtures" / "benchmark"
STATIC = ROOT / "docs" / "_static"
GIT_AGGREGATOR = "git-aggregator==4.1"

# Mid-tone colours, readable on light and dark pages alike: the chart is an
# image, so it cannot follow the page's theme.
THEME = {"text": "#7d8590", "muted": "#7d8590", "accent": "#ef662f", "bar": "#7d8590"}

IDENTITY = {
    "GIT_AUTHOR_NAME": "bench",
    "GIT_AUTHOR_EMAIL": "bench@localhost",
    "GIT_COMMITTER_NAME": "bench",
    "GIT_COMMITTER_EMAIL": "bench@localhost",
}


def git(*args: str, cwd: Path | None = None) -> str:
    result = subprocess.run(
        ["git", *args], cwd=cwd, capture_output=True, text=True, check=True
    )
    return result.stdout.strip()


def fixture() -> dict[str, dict[str, list[str]]]:
    """Directory -> {"base": [sha], "prs": [numbers]} from the fixture."""
    repos: dict[str, dict[str, list[str]]] = {}
    current = None
    for line in (FIXTURE / "repos.yaml").read_text().splitlines():
        if re.match(r"^\./[\w-]+:$", line):
            current = line[2:-1]
            repos[current] = {"base": [], "prs": []}
        elif current and (m := re.match(r"^\s+- oca refs/pull/(\d+)/head$", line)):
            repos[current]["prs"].append(m.group(1))
        elif current and (m := re.match(r"^\s+- oca ([0-9a-f]{40})$", line)):
            repos[current]["base"].append(m.group(1))
    return repos


def build_mirrors(work: Path, seed: Path | None) -> Path:
    """Bare repositories with 17.0 and the fixture's pull request refs."""
    mirrors = work / "mirrors"
    for repo, refs in fixture().items():
        bare = mirrors / "OCA" / f"{repo}.git"
        if (bare / "HEAD").exists():
            continue
        print(f"mirroring {repo}", file=sys.stderr)
        git("init", "--quiet", "--bare", str(bare))
        if seed and (seed / repo).is_dir():
            git(
                "fetch",
                "--quiet",
                str(seed / repo),
                "+refs/remotes/origin/17.0:refs/heads/17.0",
                cwd=bare,
            )
        upstream = f"https://github.com/OCA/{repo}.git"
        specs = ["+refs/heads/17.0:refs/heads/17.0"] + [
            f"+refs/pull/{n}/head:refs/pull/{n}/head" for n in refs["prs"]
        ]
        git("fetch", "--quiet", upstream, *specs, cwd=bare)
        git("cat-file", "-e", f"{refs['base'][0]}^{{commit}}", cwd=bare)
    for repo, refs in fixture().items():
        # The pinned base as a branch, for the odx project ([tool.odx] bases
        # are branches).
        bare = mirrors / "OCA" / f"{repo}.git"
        git("update-ref", "refs/heads/bench-base", refs["base"][0], cwd=bare)
    return mirrors


def odx_project(run: Path, origin: str) -> None:
    """The fixture as a [tool.odx] project (mirror mode)."""
    # Merge commits, as git-aggregator makes: the same work.
    lines = [
        "[tool.odx]",
        'odoo_version = "17.0"',
        'mergiraf = "never"',
        'merge_strategy = "merge"',
        "",
    ]
    for repo, refs in fixture().items():
        merges = ", ".join(f"{{ pr = {n} }}" for n in refs["prs"])
        lines += [
            "[[tool.odx.repositories]]",
            f'name = "{repo}"',
            f'url = "{origin}/OCA/{repo}.git"',
            'base = "bench-base"',
            f'target_dir = "{repo}"',
            f"merges = [{merges}]",
            "",
        ]
    (run / "pyproject.toml").write_text("\n".join(lines))


def gitaggregate(work: Path) -> Path:
    """gitaggregate from git-aggregator, in a virtual environment of its own."""
    venv = work / "git-aggregator"
    program = venv / ("Scripts" if os.name == "nt" else "bin") / "gitaggregate"
    if not program.exists():
        subprocess.run(["uv", "venv", "--quiet", str(venv)], check=True)
        subprocess.run(
            ["uv", "pip", "install", "--quiet", "--python", str(venv), GIT_AGGREGATOR],
            check=True,
        )
    return program


def trees(run: Path) -> dict[str, str]:
    return {repo: git("rev-parse", "HEAD^{tree}", cwd=run / repo) for repo in fixture()}


class Tool:
    def __init__(
        self,
        name: str,
        command: list[str],
        run: Path,
        env: dict[str, str],
        cache: Path | None,
    ):
        self.name, self.command, self.run, self.env, self.cache = (
            name,
            command,
            run,
            env,
            cache,
        )

    def reset(self, scenario: str) -> None:
        if scenario in ("cold", "warm"):
            for repo in fixture():
                shutil.rmtree(self.run / repo, ignore_errors=True)
        if scenario == "cold" and self.cache:
            shutil.rmtree(self.cache, ignore_errors=True)

    def once(self, scenario: str) -> float:
        self.reset(scenario)
        start = time.perf_counter()
        result = subprocess.run(
            self.command,
            cwd=self.run,
            env=self.env,
            capture_output=True,
            text=True,
            check=False,
        )
        elapsed = time.perf_counter() - start
        if result.returncode:
            sys.exit(f"{self.name} ({scenario}) failed:\n{result.stderr}")
        return elapsed


def measure(tool: Tool, scenario: str, runs: int, expected: dict[str, str]) -> float:
    """Median seconds after a warm-up run; the result is checked every run."""
    tool.once(scenario)
    samples = []
    for _ in range(runs):
        samples.append(tool.once(scenario))
        if trees(tool.run) != expected:
            sys.exit(f"{tool.name} ({scenario}) built different trees")
    return statistics.median(samples)


def seconds(value: float) -> str:
    return f"{value:.2f}s" if value < 10 else f"{value:.1f}s"


def chart(
    results: list[tuple[str, float]], caption: str, theme: dict[str, str] = THEME
) -> str:
    """A horizontal bar chart, fastest first, as odoo-lint draws its own."""
    width, label_width, row = 640, 200, 34
    bar_space = width - label_width - 70
    longest = max(t for _, t in results)
    height = row * len(results) + 34
    font = "-apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif"
    parts = [
        (
            f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
            f'viewBox="0 0 {width} {height}" font-family="{font}" font-size="14">'
        )
    ]
    for i, (name, value) in enumerate(results):
        y = i * row + 6
        bar = max(2.0, bar_space * value / longest)
        first = i == 0
        fill = theme["accent"] if first else theme["bar"]
        weight = "bold" if first else "normal"
        parts.append(
            f'<text x="{label_width - 10}" y="{y + 17}" text-anchor="end" '
            f'fill="{theme["text"]}" font-weight="{weight}">{name}</text>'
        )
        parts.append(
            f'<rect x="{label_width}" y="{y + 3}" width="{bar:.1f}" height="20" '
            f'rx="3" fill="{fill}"/>'
        )
        parts.append(
            f'<text x="{label_width + bar + 8:.1f}" y="{y + 17}" '
            f'fill="{theme["text"]}" font-weight="{weight}">{seconds(value)}</text>'
        )
    parts.append(
        f'<text x="{label_width}" y="{height - 8}" fill="{theme["muted"]}" '
        f'font-size="12">{caption}</text>'
    )
    parts.append("</svg>\n")
    return "\n".join(parts)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--mode", choices=["mirror", "github"], default="mirror")
    parser.add_argument("--seed", type=Path, help="clones to build the mirrors from")
    parser.add_argument("--work", type=Path, default=ROOT / "target" / "bench")
    parser.add_argument(
        "--gar", type=Path, default=ROOT / "target" / "release" / "git-aggregate-rs"
    )
    parser.add_argument("--odx", type=Path, default=ROOT / "target" / "release" / "odx")
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument(
        "--record", action="store_true", help="write trees.json from gitaggregate"
    )
    parser.add_argument("--no-chart", action="store_true")
    args = parser.parse_args()

    work = args.work.resolve()
    work.mkdir(parents=True, exist_ok=True)
    origin = (
        "https://github.com"
        if args.mode == "github"
        else build_mirrors(work, args.seed and args.seed.expanduser()).as_uri()
    )
    config = work / "config"
    config.mkdir(exist_ok=True)
    env = dict(os.environ, ORIGIN=origin, ODX_CONFIG_DIR=str(config), **IDENTITY)
    jobs = os.cpu_count() or 1
    ga = str(gitaggregate(work))
    tools = []
    for name, program, cache, threads in [
        ("git-aggregate-rs", str(args.gar), True, jobs),
        ("git-aggregate-rs, 1 job", str(args.gar), True, 1),
        ("git-aggregator", ga, False, jobs),
        ("git-aggregator, 1 job", ga, False, 1),
    ]:
        run = work / "run" / re.sub(r"\W+", "-", name)
        run.mkdir(parents=True, exist_ok=True)
        shutil.copy(FIXTURE / "repos.yaml", run / "repos.yaml")
        tool_env = dict(env)
        cache_dir = None
        if cache:
            cache_dir = run / ".cache"
            tool_env["ODX_CACHE_DIR"] = str(cache_dir)
        tools.append(
            Tool(
                name,
                [program, "-c", "repos.yaml", "-e", "-j", str(threads)],
                run,
                tool_env,
                cache_dir,
            )
        )

    if args.mode == "mirror":
        # odx from odx.lock: locked once, outside the timings.
        run = work / "run" / "odx"
        run.mkdir(parents=True, exist_ok=True)
        odx_project(run, origin)
        odx_env = dict(env, ODX_CACHE_DIR=str(run / ".cache"))
        subprocess.run(
            [str(args.odx), "lock"],
            cwd=run,
            env=odx_env,
            check=True,
            capture_output=True,
        )
        for name, threads in [
            ("odx aggregate --locked", jobs),
            ("odx aggregate --locked, 1 job", 1),
        ]:
            tools.append(
                Tool(
                    name,
                    [str(args.odx), "-j", str(threads), "aggregate", "--locked"],
                    run,
                    odx_env,
                    run / ".cache",
                )
            )

    expected_file = FIXTURE / "trees.json"
    if args.record:
        reference = tools[3]
        reference.once("cold")
        expected_file.write_text(
            json.dumps(trees(reference.run), indent=2, sort_keys=True) + "\n"
        )
    expected = json.loads(expected_file.read_text())

    scenarios = ["cold", "warm", "no-op"]
    results: dict[str, dict[str, float]] = {}
    for tool in tools:
        results[tool.name] = {}
        for scenario in scenarios:
            results[tool.name][scenario] = measure(tool, scenario, args.runs, expected)
            print(
                f"{tool.name}: {scenario} {seconds(results[tool.name][scenario])}",
                file=sys.stderr,
            )

    version = subprocess.run(
        [str(args.gar), "--version"], capture_output=True, text=True, check=True
    ).stdout.split()[1]
    odx_version = (
        subprocess.run(
            [str(args.odx), "--version"], capture_output=True, text=True, check=True
        ).stdout.split()[1]
        if args.mode == "mirror"
        else ""
    )
    print("| Tool | cold | warm | no-op |\n| --- | --- | --- | --- |")
    for tool in tools:
        label = (
            tool.name.replace("git-aggregate-rs", f"git-aggregate-rs {version}")
            .replace(
                "git-aggregator", f"git-aggregator {GIT_AGGREGATOR.split('==')[1]}"
            )
            .replace("odx aggregate", f"odx {odx_version} aggregate")
        )
        cells = " | ".join(seconds(results[tool.name][s]) for s in scenarios)
        print(f"| {label} | {cells} |")
    print(
        f"\n{os.cpu_count()} CPUs, {platform.system()} {platform.machine()}, "
        f"{git('--version')}, {args.mode} mode, median of {args.runs}"
    )
    (args.work / "results.json").write_text(json.dumps(results, indent=2) + "\n")

    if not args.no_chart:
        rows = sorted(
            ((t.name, results[t.name]["cold"]) for t in tools), key=lambda r: r[1]
        )
        caption = "Aggregating 12 OCA repositories with 25 pull requests from scratch"
        (STATIC / "benchmark.svg").write_text(chart(rows, caption))


if __name__ == "__main__":
    main()
