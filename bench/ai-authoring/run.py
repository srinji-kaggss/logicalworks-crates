#!/usr/bin/env python3
"""Fixed-model AI authoring/execution matrix for issue #87 step 7 / #152.

Each trial asks a fixed model to author one held-out orchestration task against
one API surface (the old `lgwks_bot::rt` surface or the new task/Host + script
facade), under the same prompt skeleton, the same API-sheet length budget, the
same hidden oracle and the same repair budget, and records everything: tokens,
compiler repairs, consumer lines, orchestration sites, oracle correctness and
wall time. Every trial is recorded; no best run is selected.

It is deliberately plumbing-first: `--dry-run` replaces the model with a canned
reference solution so the whole pipeline (template, lockfile, offline build,
hidden oracle, serialization) can be exercised without a credential. The real
matrix is left to the reviewer.

    python3 bench/ai-authoring/run.py --dry-run --trials 1
    python3 bench/ai-authoring/run.py --mutants
    python3 bench/ai-authoring/run.py            # the real matrix

Only the Python standard library is used. `cargo` is invoked one process at a
time, serialized by an `fcntl` lock over a shared lock file, because two cargo
processes sharing one target directory is not a measurement.
"""

import argparse
import concurrent.futures
import json
import os
import pathlib
import statistics
import subprocess
import sys
import threading
import time

# The sibling modules, imported from this file's own directory, so the bench runs
# from any working directory and `regrade.py` can load this file by path.
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

from harness import HERE, ORACLE_CRATE_TOKEN, REPO, TASKS_DIR, TEST_LINE, run_timed_cargo  # noqa: E402
from prompts import DEFAULT_PROFILES, PROFILES  # noqa: E402
from trial import (  # noqa: E402
    NEW_ONLY_TASK_WHY,
    NEW_ONLY_TASKS,
    NO_FUTURES_TASK_WHY,
    NO_FUTURES_TASKS,
    NO_OLD_TASK_WHY,
    NO_OLD_TASKS,
    ORCHESTRATION_TOKENS,
    oracle_test_names,
    run_job,
    trial_id_of,
)

#: `regrade.py` grades a run through this module's names; they are its interface.
__all__ = ["ORACLE_CRATE_TOKEN", "TASKS_DIR", "TEST_LINE", "oracle_test_names", "run_timed_cargo"]

RESULTS_MUTEX = threading.Lock()

#: Where a run records its work root and commit, so `--resume` finds both.
MANIFEST = "run.json"

#: Every finished trial, appended and fsynced as it lands. The 2026-10-11 matrix
#: (990 trials, hours of model calls) held its records in memory until the last
#: trial: a crash, a kill or a dead battery at hour fourteen would have kept
#: none. This log is what `--resume` reads; the sorted `results.jsonl` replaces
#: it when the run ends.
PARTIAL = "results.partial.jsonl"


def append_durably(path: pathlib.Path, record: dict) -> None:
    """Append one record as a line and fsync it before returning."""
    with path.open("a") as handle:
        handle.write(json.dumps(record) + "\n")
        handle.flush()
        os.fsync(handle.fileno())


def resume_state(run_dir: pathlib.Path, mode: str, bot_sha: str):
    """`(run_dir, work, finished records by trial id)` for an interrupted run.

    Refused when the run finished, was another mode, or ran at another commit:
    trials graded against two different crate trees are not one measurement.
    A torn last line (the interruption landed mid-append) is dropped, and that
    trial runs again.
    """
    run_dir = run_dir.resolve()
    manifest = json.loads((run_dir / MANIFEST).read_text())
    if (run_dir / "results.jsonl").exists():
        raise SystemExit(f"{run_dir} finished; there is nothing to resume")
    if manifest["mode"] != mode:
        raise SystemExit(f"{run_dir} is a {manifest['mode']} run, not {mode}")
    if manifest["lgwks_bot_sha"] != bot_sha:
        raise SystemExit(
            f"{run_dir} ran at {manifest['lgwks_bot_sha']}, HEAD is {bot_sha}; "
            "check that commit out to resume it"
        )
    done = {}
    partial = run_dir / PARTIAL
    lines = partial.read_text().splitlines() if partial.exists() else []
    for line in lines:
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        if record.get("trial_id") not in (None, "crashed"):
            done[record["trial_id"]] = record
    return run_dir, pathlib.Path(manifest["work"]), done


def set_aside_interrupted(work: pathlib.Path, jobs: list) -> None:
    """Rename each unfinished trial's directory so its rerun starts clean.

    Renamed, not deleted: what an interrupted trial left is evidence of the
    interruption, and a trial directory is never reused (`run_job` refuses one
    that exists).
    """
    for job in jobs:
        trial_dir = work / trial_id_of(job)
        if trial_dir.exists():
            attempt = 1
            while trial_dir.with_name(f"{trial_dir.name}.interrupted-{attempt}").exists():
                attempt += 1
            trial_dir.rename(trial_dir.with_name(f"{trial_dir.name}.interrupted-{attempt}"))


# ── summary ──────────────────────────────────────────────────────────────────


def median_or_none(values):
    numeric = [value for value in values if value is not None]
    if not numeric:
        return None
    return statistics.median(numeric)


def summarize(records: list) -> dict:
    """The per-`(model, api, task, profile)` aggregate, with every trial listed.

    Grouping by profile is what makes the profile axis readable at all: without
    it, five trials that each behaved differently collapse into one row and the
    profile comparison has to be reconstructed from `results.jsonl` by hand.
    """
    groups = {}
    for record in records:
        key = (
            record["model"],
            record["api"],
            record["task"],
            record.get("profile", ""),
        )
        groups.setdefault(key, []).append(record)
    output = []
    for (model, api, task, profile), rows in sorted(groups.items()):
        repairs = [row["repairs"] for row in rows]
        per_trial = [
            {
                "trial": row["trial"],
                "compiled": row["compiled"],
                "cleanup_ok": row.get("cleanup_ok"),
                "oracle_pass_count": row["oracle_pass_count"],
                "oracle_total": row["oracle_total"],
                "oracle": row["oracle"],
            }
            for row in sorted(rows, key=lambda row: row["trial"])
        ]
        total = sum(row["oracle_total"] for row in rows)
        passed = sum(row["oracle_pass_count"] for row in rows)
        output.append(
            {
                "model": model,
                "api": api,
                "task": task,
                "profile": profile,
                "n": len(rows),
                "compiled": sum(1 for row in rows if row["compiled"]),
                "cleanup_ok_all": all(row.get("cleanup_ok") for row in rows),
                "repairs_mean": statistics.mean(repairs) if repairs else None,
                "repairs_median": median_or_none(repairs),
                "oracle_pass_rate": (passed / total) if total else None,
                "oracle_per_trial": per_trial,
                "tokens_in_median": median_or_none([row["tokens_in"] for row in rows]),
                "tokens_out_median": median_or_none([row["tokens_out"] for row in rows]),
                "consumer_lines_median": median_or_none(
                    [row["consumer_lines"] for row in rows]
                ),
                "orchestration_sites_median": median_or_none(
                    [row["orchestration_sites"] for row in rows]
                ),
                "oracle_wall_ms_median": median_or_none(
                    [row.get("oracle_wall_ms") for row in rows]
                ),
                "oracle_peak_rss_bytes_median": median_or_none(
                    [row.get("oracle_peak_rss_bytes") for row in rows]
                ),
            }
        )
    return output


# ── main ─────────────────────────────────────────────────────────────────────


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--models",
        # #270's planned run: at least three models, at least one a frontier
        # closed model. `deepseek-v4.1-flash` carries over from the 2026-10-04
        # run; `stealth/space-bunny-alpha` is no longer served, and `gpt-6-sol`
        # answers 403 MODEL_NOT_IN_PLAN on this account.
        default="deepseek/deepseek-v4.1-flash,claude-sonnet-5-5,moonshotai/kimi-k3",
        help="comma-separated fixed model ids",
    )
    parser.add_argument("--apis", default="old,new,fan,futures,script,lexicon")
    parser.add_argument(
        "--tasks",
        default="aggregate,pipeline,recovery,capture,tenant-cache,durable-retry",
    )
    parser.add_argument(
        "--profiles",
        default=",".join(DEFAULT_PROFILES),
        help=(
            "comma-separated user profiles to run, each a fixed persona preamble "
            "prepended to the same prompt skeleton (default: all five: "
            + ", ".join(DEFAULT_PROFILES)
            + ")"
        ),
    )
    # Two trials under each of the five profiles: ten per (model, arm, task)
    # cell, #270's floor.
    parser.add_argument("--trials", type=int, default=2)
    parser.add_argument("--parallel", type=int, default=4)
    parser.add_argument(
        "--out",
        type=pathlib.Path,
        default=None,
        help="results directory (default: bench/ai-authoring/runs/<UTC stamp>-<mode>/); never overwritten",
    )
    parser.add_argument(
        "--work",
        type=pathlib.Path,
        default=None,
        help="trial work root (default: a fresh $TMPDIR/lgwks-ai-authoring-<UTC stamp>-<mode>/)",
    )
    parser.add_argument("--max-repairs", type=int, default=4)
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="use the canned reference solutions instead of a model",
    )
    parser.add_argument(
        "--mutants",
        action="store_true",
        help="run the deliberate mutants and print which oracle clauses they fail",
    )
    parser.add_argument(
        "--cmd",
        default=os.environ.get("AI_AUTHORING_CMD", "cmd"),
        help="the model CLI to invoke (default: cmd, or $AI_AUTHORING_CMD)",
    )
    parser.add_argument(
        "--resume",
        type=pathlib.Path,
        default=None,
        help=(
            "an interrupted run's results directory: its finished trials are kept "
            "and only the rest run, in its own work root, at the same commit"
        ),
    )
    args = parser.parse_args()

    models = [item for item in args.models.split(",") if item]
    apis = [item for item in args.apis.split(",") if item]
    tasks = [item for item in args.tasks.split(",") if item]
    profiles = [item for item in args.profiles.split(",") if item]
    unknown = [item for item in profiles if item not in PROFILES]
    if unknown:
        parser.error(
            "unknown profile(s): "
            + ", ".join(unknown)
            + "; the known profiles are "
            + ", ".join(sorted(PROFILES))
        )
    mode = "mutants" if args.mutants else ("dry" if args.dry_run else "models")
    bot_sha = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=str(REPO), capture_output=True, text=True, check=False
    ).stdout.strip()
    if args.resume is not None:
        run_dir, work, done = resume_state(args.resume, mode, bot_sha)
    else:
        stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime()) + "-" + mode
        # A run never reuses or overwrites another run's evidence: both the work
        # root and the results directory are new, and an existing one is a refusal.
        work = (args.work or pathlib.Path(os.environ.get("TMPDIR", "/tmp")) / f"lgwks-ai-authoring-{stamp}").resolve()
        work.mkdir(parents=True, exist_ok=False)
        run_dir = args.out or (HERE / "runs" / stamp)
        run_dir = run_dir if run_dir.is_absolute() else (REPO / run_dir)
        run_dir.mkdir(parents=True, exist_ok=False)
        (run_dir / MANIFEST).write_text(
            json.dumps({"work": str(work), "mode": mode, "lgwks_bot_sha": bot_sha}) + "\n"
        )
        done = {}
    out = run_dir / "results.jsonl"

    jobs = []
    if args.mutants:
        for task in tasks:
            jobs.append(
                {
                    "mode": "mutant",
                    "model": "__mutant__",
                    "api": "new",
                    "task": task,
                    "profile": "",
                    "trial": 0,
                    "cmd": args.cmd,
                    "max_repairs": 0,
                }
            )
    elif args.dry_run:
        for api in apis:
            for task in tasks:
                # The dry run proves the plumbing, so it runs the *first* profile
                # rather than all five: the profile only reaches the prompt the
                # model reads, and a canned solution never reads one. Five copies
                # of the same reference would be five identical trials wearing
                # different labels.
                for profile in profiles[:1]:
                    for trial in range(args.trials):
                        jobs.append(
                            {
                                "mode": "dry",
                                "model": "__dry-run__",
                                "api": api,
                                "task": task,
                                "profile": profile,
                                "trial": trial,
                                "cmd": args.cmd,
                                "max_repairs": args.max_repairs,
                            }
                        )
    else:
        for model in models:
            for api in apis:
                for task in tasks:
                    for profile in profiles:
                        for trial in range(args.trials):
                            jobs.append(
                                {
                                    "mode": "model",
                                    "model": model,
                                    "api": api,
                                    "task": task,
                                    "profile": profile,
                                    "trial": trial,
                                    "cmd": args.cmd,
                                    "max_repairs": args.max_repairs,
                                }
                            )
    # `recovery` has no old-API arm, because the old `rt` surface has no durable
    # run store, no `remember`, no run identity and no resume — so it cannot
    # express the task at all, and a cell asking for one would measure a model
    # guessing at a capability the API does not have. `capture` has no old-API
    # arm either, because the old sheet offers no supervised process spawn, and
    # `recovery` has no futures arm either, because that sheet offers no
    # durable store. All three omissions are recorded in the run's protocol
    # block rather than left for a reader to notice.
    skipped = (
        [
            {"task": task, "api": "old", "why": NEW_ONLY_TASK_WHY}
            for task in tasks
            if task in NEW_ONLY_TASKS
        ]
        + [
            {"task": task, "api": "old", "why": NO_OLD_TASK_WHY}
            for task in tasks
            if task in NO_OLD_TASKS
        ]
        + [
            {"task": task, "api": "futures", "why": NO_FUTURES_TASK_WHY}
            for task in tasks
            if task in NO_FUTURES_TASKS
        ]
    )
    jobs = [
        job
        for job in jobs
        if not (
            (
                (job["task"] in NEW_ONLY_TASKS or job["task"] in NO_OLD_TASKS)
                and job["api"] == "old"
            )
            or (job["task"] in NO_FUTURES_TASKS and job["api"] == "futures")
        )
    ]
    if skipped:
        print(f"skipped {len(skipped)} cell(s): " + ", ".join(
            f"{item['task']}/{item['api']}" for item in skipped
        ))
        for item in skipped:
            print(f"  {item['task']}/{item['api']}: {item['why']}")

    records = [done[trial_id_of(job)] for job in jobs if trial_id_of(job) in done]
    jobs = [job for job in jobs if trial_id_of(job) not in done]
    if args.resume is not None:
        print(f"resuming: {len(records)} trial(s) kept, {len(jobs)} to run")
        set_aside_interrupted(work, jobs)
    def run_and_record(job: dict) -> dict:
        """Run one trial and append its record durably before returning.

        The append is the worker's, not the collecting loop's: the loop runs
        behind the workers, so a record appended there could be lost to an
        interruption after its trial finished (the resume probe lost two of
        three that way). A crashed trial is a recorded trial.
        """
        try:
            record = run_job(job, work, bot_sha)
        except Exception as error:
            record = {
                "model": job["model"],
                "api": job["api"],
                "task": job["task"],
                "profile": job.get("profile", ""),
                "trial": job["trial"],
                "trial_id": "crashed",
                "mode": job["mode"],
                "compiled": False,
                "repairs": 0,
                "oracle": {},
                "oracle_pass_count": 0,
                "oracle_total": 0,
                "cleanup_ok": False,
                "oracle_wall_ms": 0.0,
                "oracle_peak_rss_bytes": None,
                "tokens_in": 0,
                "tokens_out": 0,
                "model_wall_ms": 0,
                "compile_wall_ms": 0,
                "consumer_lines": 0,
                "orchestration_sites": 0,
                "orchestration_site_tokens": {},
                "orchestration_token_list": list(ORCHESTRATION_TOKENS),
                "lockfile": "",
                "lgwks_bot_sha": bot_sha,
                "error": str(error),
                "lib_rs": "",
            }
        with RESULTS_MUTEX:
            append_durably(run_dir / PARTIAL, record)
        return record

    with concurrent.futures.ThreadPoolExecutor(max_workers=max(1, args.parallel)) as pool:
        futures = [pool.submit(run_and_record, job) for job in jobs]
        for future in concurrent.futures.as_completed(futures):
            record = future.result()
            records.append(record)
            print(
                f"{record['model']:28} {record['api']:4} {record['task']:10} "
                f"{record.get('profile') or '-':13} t{record['trial']} "
                f"compiled={record['compiled']} "
                f"oracle={record['oracle_pass_count']}/{record['oracle_total']}",
                flush=True,
            )

    records.sort(
        key=lambda row: (
            row["model"],
            row["api"],
            row["task"],
            row.get("profile", ""),
            row["trial"],
        )
    )
    # The sorted file replaces nothing until it is whole: written beside its
    # final name, then renamed over it, so an interruption here still leaves
    # the partial log every finished trial was appended to.
    staged = out.with_name(out.name + ".staged")
    with staged.open("w") as handle:
        for record in records:
            handle.write(json.dumps(record) + "\n")
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(staged, out)
    (run_dir / PARTIAL).unlink(missing_ok=True)

    summary = {
        "generated_by": "bench/ai-authoring/run.py",
        "lgwks_bot_sha": bot_sha,
        "protocol": {
            "same_prompt_skeleton": True,
            "same_sheet_budget": (
                "each API sheet <= 250 lines, except the lexicon arm's: its sheet is "
                "the generated docs/script-lexicon.md, whose budget is one page "
                "(<= 4k tokens, #385) rather than a line count"
            ),
            "same_oracle": True,
            "hidden_oracle": True,
            "repair_budget": args.max_repairs,
            "cargo_serialized": "one cargo process at a time under an fcntl lock",
            "closed_book": "each model call runs under sandbox-exec: home, /private/tmp and the work directory are unreadable except the CLI config and the trial scratch; no session, no skills",
            "profiles": list(profiles),
            "profile_text_is_fixed": (
                "each profile is one fixed persona preamble prepended to the same "
                "prompt skeleton; the framing, the API sheet, the oracle, the "
                "repair budget and the sandbox are identical across profiles"
            ),
            "oracle_measured_under_time_l": (
                "oracle_wall_ms is the wall time of the cargo test invocation that "
                "runs the oracle, under /usr/bin/time -l; oracle_peak_rss_bytes is "
                "that process's maximum resident set size in bytes, or null on a "
                "host with no /usr/bin/time"
            ),
            "cleanup_ok_is_the_drop_clause": (
                "cleanup_ok is true only when the task's 'dropping the future "
                "leaves nothing live' oracle test passed; a crate that never "
                "compiled reports false rather than a cleanup result"
            ),
            "skipped_cells": skipped,
        },
        "groups": summarize(records),
    }
    summary_path = run_dir / "summary.json"
    summary_path.write_text(json.dumps(summary, indent=2) + "\n")

    print(f"\nwrote {out}")
    print(f"wrote {summary_path}")

    if args.mutants:
        print("\nmutant verdicts (which oracle clause each mutant fails):")
        for record in records:
            failed = [name for name, status in record["oracle"].items() if status == "fail"]
            ran = record["oracle_pass_count"] + len(failed)
            print(
                f"  {record['task']:10} failed {len(failed)}/{ran}: "
                + (", ".join(failed) if failed else "(none — the oracle did NOT catch the mutant)")
            )
    return 0


if __name__ == "__main__":
    sys.exit(main())
