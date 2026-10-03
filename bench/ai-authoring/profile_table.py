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
    return 0


if __name__ == "__main__":
    sys.exit(main())