#!/usr/bin/env python3
"""Build every way, run every scenario, and print the comparison, per README.md.

python3 bench/orchestration/run.py [--runs 5] [--json results.json]

Builds go into a temporary directory that is removed at the end, so a run
leaves only what `--json` names. Runs are sequential, one process at a time:
a timing taken while another suite runs is not a measurement.
"""

import argparse
import json
import pathlib
import re
import shutil
import statistics
import subprocess
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parent.parent
EXAMPLE = REPO / "crates/lgwks-bot/examples/compare_orchestration.rs"
SCENARIOS = ("throughput", "failfast", "cancel", "storm", "deadline")
TENANTS = {"throughput": 2, "failfast": 1, "cancel": 1, "storm": 1, "deadline": 1}
# The code each way's author writes for the orchestration, by marker. A way
# built on a helper counts the helper.
WAYS = {
    "rust-script": [(EXAMPLE, "script")],
    "rust-join_all": [(EXAMPLE, "hand"), (EXAMPLE, "join_all")],
    "rust-joinset": [(EXAMPLE, "hand"), (EXAMPLE, "joinset")],
    "python-asyncio": [(HERE / "python/asyncio_taskgroup.py", "asyncio")],
    "python-trio": [(HERE / "python/trio_nursery.py", "trio")],
    "go-errgroup": [(HERE / "go/main.go", "errgroup")],
    "node-pool": [(HERE / "node/pool.mjs", "pool")],
    "node-effect": [(HERE / "node/effect.mjs", "effect")],
}
WORST = ("peak_in_flight", "attempts", "duplicates", "live_at_return", "live_after_grace", "finished_after_return")
MEDIAN = ("wall_ms", "per_s", "p50_ms", "p99_ms", "rss_mb")


def sh(command, cwd=None):
    """Run a setup command, failing loudly with its output."""
    done = subprocess.run(command, cwd=cwd, capture_output=True, text=True, check=False)
    if done.returncode != 0:
        sys.exit(f"setup failed: {' '.join(map(str, command))}\n{done.stdout}{done.stderr}")
    return done.stdout


def build(work):
    """Every way's command line, built into `work`."""
    sh(["cargo", "build", "--locked", "--release", "-p", "lgwks_bot", "--example", "compare_orchestration"], REPO)
    target = json.loads(sh(["cargo", "metadata", "--format-version=1", "--no-deps"], REPO))["target_directory"]
    rust = pathlib.Path(target) / "release/examples/compare_orchestration"
    shutil.copytree(HERE / "python", work / "python")
    venv = work / "venv"
    sh(["uv", "venv", "--quiet", "--python", sys.executable, str(venv)])
    sh(["uv", "pip", "install", "--quiet", "--require-hashes", "--python", str(venv / "bin/python"),
        "-r", str(HERE / "python/requirements.txt")])
    python = str(venv / "bin/python")
    go = work / "go-errgroup"
    sh(["go", "build", "-o", str(go), "."], HERE / "go")
    node = work / "node"
    node.mkdir()
    for name in ("package.json", "package-lock.json", "site_model.mjs", "pool.mjs", "effect.mjs"):
        shutil.copy(HERE / "node" / name, node / name)
    sh(["npm", "ci", "--silent"], node)
    return {
        "rust-script": [str(rust), "script"],
        "rust-join_all": [str(rust), "join_all"],
        "rust-joinset": [str(rust), "joinset"],
        "python-asyncio": [python, str(work / "python/asyncio_taskgroup.py")],
        "python-trio": [python, str(work / "python/trio_nursery.py")],
        "go-errgroup": [str(go)],
        "node-pool": ["node", str(node / "pool.mjs")],
        "node-effect": ["node", str(node / "effect.mjs")],
    }


def once(command, scenario):
    """One run: the way's JSON line plus its peak RSS from `time -l`."""
    done = subprocess.run(["/usr/bin/time", "-l", *command, scenario], capture_output=True, text=True, check=False)
    lines = [line for line in done.stdout.splitlines() if line.startswith("{")]
    if done.returncode != 0 or len(lines) != 1:
        return {"crashed": True, "exit": done.returncode, "stderr": done.stderr[-2_000:]}
    row = json.loads(lines[0])
    rss = re.search(r"(\d+)\s+maximum resident set size", done.stderr)
    row["rss_mb"] = round(int(rss.group(1)) / 1_048_576, 1) if rss else None
    return row


def lines_of(file, marker):
    """Code lines between `BEGIN marker` and `END marker`, comments and blanks out."""
    text = pathlib.Path(file).read_text().splitlines()
    begin = next(i for i, line in enumerate(text) if line.strip().endswith(f"BEGIN {marker}"))
    end = next(i for i, line in enumerate(text) if line.strip().endswith(f"END {marker}"))
    comment = ("#", "//", "///")
    return sum(1 for line in text[begin + 1 : end] if line.strip() and not line.strip().startswith(comment))


def summarise(runs):
    """Median and range for timings, worst case for invariants, across runs."""
    good = [run for run in runs if not run.get("crashed")]
    summary = {"runs": len(runs), "crashed": len(runs) - len(good), "ok_runs": sum(run["ok"] for run in good)}
    if not good:
        summary["stderr"] = runs[0].get("stderr", "")
        return summary
    for key in MEDIAN:
        values = [run[key] for run in good if run.get(key) is not None]
        if values:
            summary[key] = {"median": statistics.median(values), "min": min(values), "max": max(values)}
    for key in WORST:
        summary[key] = max(run[key] for run in good)
    summary["bound"] = good[0]["bound"]
    summary["error"] = good[0]["error"]
    return summary


def verdicts(results):
    """The semantic invariants, judged from the worst case of every run."""
    table = {}
    for way, by_scenario in results.items():
        get = lambda scenario, key: by_scenario[scenario].get(key)
        failfast_err = get("failfast", "error") or ""
        table[way] = {
            # The bound is per tenant; throughput runs two tenants at once.
            "bound held": all(get(s, "peak_in_flight") <= get(s, "bound") * TENANTS[s] for s in SCENARIOS),
            "stops at first failure": get("failfast", "attempts") < 2_000,
            "nothing live at return": all(get(s, "live_at_return") == 0 for s in ("failfast", "cancel", "storm")),
            "nothing finishes after return": all(get(s, "finished_after_return") == 0 for s in SCENARIOS),
            "cancel reaches every body": get("cancel", "live_at_return") == 0,
            "storm attempts (1,000 items)": get("storm", "attempts"),
            "deadline honoured": get("deadline", "wall_ms")["max"] < 150,
            "no duplicate effect": get("throughput", "duplicates") == 0,
            "error names the failing item": "500" in failfast_err,
        }
    return table


def render(report):
    """The README tables for a saved report."""
    results, verdict, loc = report["results"], verdicts(report["results"]), report["lines"]
    span = lambda cell, key: f"{cell[key]['median']:g} ({cell[key]['min']:g}-{cell[key]['max']:g})"
    out = [f"Median (min-max) of {report['runs_per_cell']} runs per cell.", "",
           "**Throughput** (2 tenants x 10,000 items):", "",
           "| way | items/s | p50 ms | p99 ms | peak RSS MB | lines |", "|---|---:|---:|---:|---:|---:|"]
    for way, cells in results.items():
        cell = cells["throughput"]
        out.append(f"| `{way}` | {span(cell, 'per_s')} | {cell['p50_ms']['median']:g} | "
                   f"{cell['p99_ms']['median']:g} | {cell['rss_mb']['median']:g} | {loc[way]} |")
    out += ["", "**Failure behaviour** (worst case over every run):", "",
            "| way | failfast ms | failfast attempts | live at return (failfast / cancel / storm) | "
            "storm attempts | deadline ms | failfast error |", "|---|---:|---:|---|---:|---:|---|"]
    for way, cells in results.items():
        live = " / ".join(str(cells[s]["live_at_return"]) for s in ("failfast", "cancel", "storm"))
        error = cells["failfast"]["error"].replace("|", "\\|")
        out.append(f"| `{way}` | {cells['failfast']['wall_ms']['median']:g} | {cells['failfast']['attempts']} | "
                   f"{live} | {cells['storm']['attempts']} | {cells['deadline']['wall_ms']['median']:g} | `{error}` |")
    names = list(next(iter(verdict.values())))
    out += ["", "**Semantic invariants:**", "", "| way | " + " | ".join(names) + " |",
            "|---|" + "---|" * len(names)]
    for way, row in verdict.items():
        marks = ["yes" if value is True else "**no**" if value is False else str(value) for value in row.values()]
        out.append(f"| `{way}` | " + " | ".join(marks) + " |")
    return "\n".join(out)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--json", type=pathlib.Path)
    parser.add_argument("--render", type=pathlib.Path, help="print the README tables for a saved --json report")
    args = parser.parse_args()
    if args.render:
        print(render(json.loads(args.render.read_text())))
        return
    work = pathlib.Path(tempfile.mkdtemp(prefix="orchestration-bench-"))
    try:
        commands = build(work)
        results = {}
        for way, command in commands.items():
            results[way] = {}
            for scenario in SCENARIOS:
                runs = [once(command, scenario) for _ in range(args.runs)]
                results[way][scenario] = summarise(runs)
                print(f"{way:15} {scenario:10} {json.dumps(results[way][scenario])}", flush=True)
    finally:
        shutil.rmtree(work, ignore_errors=True)
    loc = {way: sum(lines_of(file, marker) for file, marker in parts) for way, parts in WAYS.items()}
    report = {"runs_per_cell": args.runs, "results": results, "verdicts": verdicts(results), "lines": loc}
    print(json.dumps({"verdicts": report["verdicts"], "lines": loc}, indent=1))
    if args.json:
        args.json.write_text(json.dumps(report, indent=1) + "\n")


if __name__ == "__main__":
    main()
