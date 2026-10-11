"""The per-`(api, profile)` table the AI-authoring README carries.

One definition, so the table on the page and any number quoted in a review come
from the same aggregation over `results.jsonl`. Read-only: it opens a committed
run and prints, and writes nothing — a table generator that rewrote the run it
read would be a second source of truth in a directory whose whole point is that
one run's evidence is never edited.

    python3 bench/ai-authoring/profile_table.py bench/ai-authoring/runs/<stamp>
"""

from __future__ import annotations

import json
import pathlib
import statistics
import sys


def median(values):
    """The median of `values`, or `None` when there are none."""
    numbers = [value for value in values if value is not None]
    return statistics.median(numbers) if numbers else None


def load(run_dir: pathlib.Path) -> list:
    """Every trial record in `run_dir/results.jsonl`."""
    path = run_dir / "results.jsonl"
    if not path.exists():
        raise SystemExit(f"no results.jsonl in {run_dir}")
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def group(records: list, *keys: str) -> dict:
    """Group `records` by the tuple of `keys`, keeping every record in its group."""
    grouped: dict = {}
    for record in records:
        grouped.setdefault(tuple(record.get(key, "") for key in keys), []).append(record)
    return grouped


def cell(rows: list) -> dict:
    """The six numbers the README table shows for one group."""
    total = sum(row["oracle_total"] for row in rows)
    passed = sum(row["oracle_pass_count"] for row in rows)
    return {
        "n": len(rows),
        "pass_rate": (passed / total) if total else None,
        "repairs_median": median([row["repairs"] for row in rows]),
        "tokens_median": median([row["tokens_in"] + row["tokens_out"] for row in rows]),
        "lines_median": median([row["consumer_lines"] for row in rows]),
        "wall_median": median([row.get("oracle_wall_ms") for row in rows]),
        "rss_median": median([row.get("oracle_peak_rss_bytes") for row in rows]),
        "cleanup_all": all(row.get("cleanup_ok") for row in rows),
        "compiled": sum(1 for row in rows if row["compiled"]),
    }


def full_pass(row: dict) -> bool:
    """Compiled, and every oracle clause of the task passed."""
    return bool(row["compiled"]) and row["oracle_total"] > 0 and (
        row["oracle_pass_count"] == row["oracle_total"]
    )


def wilson(passed: int, n: int) -> tuple:
    """The 95% Wilson score interval for `passed` of `n`, as fractions.

    Wilson rather than the normal approximation because the cells are small and
    sit at 0 or n often: the normal interval collapses to a point there and
    would let two cells of 10/10 and 8/10 read as different (#270: no claim
    over overlapping intervals).
    """
    if n == 0:
        return (0.0, 1.0)
    z = 1.959963984540054
    p = passed / n
    centre = (p + z * z / (2 * n)) / (1 + z * z / n)
    half = (z / (1 + z * z / n)) * ((p * (1 - p) / n + z * z / (4 * n * n)) ** 0.5)
    return (max(0.0, centre - half), min(1.0, centre + half))


def pass_cell(rows: list) -> str:
    """`k/n [lo, hi]`: full passes and their 95% interval, in percent."""
    passed = sum(1 for row in rows if full_pass(row))
    low, high = wilson(passed, len(rows))
    return f"{passed}/{len(rows)} [{low * 100:.0f}, {high * 100:.0f}]"


def fmt(value, suffix: str = "") -> str:
    """One table cell: an int, a one-decimal float, or an em dash for `None`."""
    if value is None:
        return "—"
    if isinstance(value, float):
        return f"{value:.1f}{suffix}"
    return f"{value}{suffix}"


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    run_dir = pathlib.Path(sys.argv[1])
    records = load(run_dir)

    print("## Per-(api, profile), from the committed run\n")
    print(
        "| api | profile | n | compiled | oracle pass rate | repairs median | "
        "tokens median | consumer lines median | oracle wall median (ms) | "
        "peak RSS median (bytes) | cleanup |"
    )
    print("|---|---|---|---|---|---|---|---|---|---|---|")
    for (api, profile), rows in sorted(
        group(records, "api", "profile").items(),
        key=lambda item: (item[0][0], item[0][1]),
    ):
        numbers = cell(rows)
        print(
            f"| {api} | {profile} | {numbers['n']} | "
            f"{numbers['compiled']}/{numbers['n']} | "
            f"{fmt(numbers['pass_rate'])} | "
            f"{fmt(numbers['repairs_median'])} | "
            f"{fmt(numbers['tokens_median'])} | "
            f"{fmt(numbers['lines_median'])} | "
            f"{fmt(numbers['wall_median'])} | "
            f"{fmt(numbers['rss_median'])} | "
            f"{'yes' if numbers['cleanup_all'] else 'no'} |"
        )

    print("\n## Per-(task), same run\n")
    print("| task | api | n | pass rate | repairs median | lines median |")
    print("|---|---|---|---|---|---|")
    for (task, api), rows in sorted(
        group(records, "task", "api").items(), key=lambda item: (item[0][0], item[0][1])
    ):
        numbers = cell(rows)
        print(
            f"| {task} | {api} | {numbers['n']} | {fmt(numbers['pass_rate'])} | "
            f"{fmt(numbers['repairs_median'])} | {fmt(numbers['lines_median'])} |"
        )

    print("\n## Full passes per (task, arm), all models, with 95% Wilson intervals\n")
    arms = sorted({row["api"] for row in records})
    print("| task | " + " | ".join(arms) + " |")
    print("|---|" + "---|" * len(arms))
    by_task_arm = group(records, "task", "api")
    for task in sorted({row["task"] for row in records}):
        cells = [pass_cell(by_task_arm[(task, arm)]) if (task, arm) in by_task_arm else "—" for arm in arms]
        print(f"| {task} | " + " | ".join(cells) + " |")

    print("\n## Full passes per (model, task, arm), with 95% Wilson intervals\n")
    print("| model | task | " + " | ".join(arms) + " |")
    print("|---|---|" + "---|" * len(arms))
    by_cell = group(records, "model", "task", "api")
    for model in sorted({row["model"] for row in records}):
        for task in sorted({row["task"] for row in records}):
            cells = [
                pass_cell(by_cell[(model, task, arm)]) if (model, task, arm) in by_cell else "—"
                for arm in arms
            ]
            print(f"| `{model}` | {task} | " + " | ".join(cells) + " |")
    return 0


if __name__ == "__main__":
    sys.exit(main())