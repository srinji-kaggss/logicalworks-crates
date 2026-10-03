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
import fcntl
import json
import os
import pathlib
import re
import shutil
import statistics
import subprocess
import sys
import threading
import time
from contextlib import contextmanager

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parent.parent
TASKS_DIR = HERE / "tasks"
API_DIR = HERE / "api"
TEMPLATE = HERE / "template"
SUPPORT = HERE / "support"
REFERENCE = HERE / "reference"
COMMITTED_LOCK = HERE / "Cargo.lock"
BOT_DIR = REPO / "crates" / "lgwks-bot"

#: The prefix every trial crate's package name carries. The suffix is unique per
#: trial: a constant name makes cargo's `-C metadata` identical across trials, so
#: a shared `CARGO_TARGET_DIR` would let one trial's `oracle` test binary stand in
#: for another's. The hidden oracle is written with the `ai_trial` token and the
#: runner substitutes the trial's real name when it copies it in.
PACKAGE_PREFIX = "ai_trial"
#: The token the committed oracles use to name the crate under test.
ORACLE_CRATE_TOKEN = "ai_trial"

#: The template's placeholders, substituted when a trial directory is created.
NAME_TOKEN = "__NAME__"
BOT_PATH_TOKEN = "../../../crates/lgwks-bot"
SUPPORT_PATH_TOKEN = "../support"

#: The orchestration-site token list, the author-burden proxy. It counts the
#: occurrences of each token in the solution's `src/lib.rs`. The list is the
#: machinery a hand-written orchestration reaches for: task ownership, bound
#: admission, cancellation, racing, looping, shared state, channels and abort.
ORCHESTRATION_TOKENS = (
    "JoinSet",
    "spawn(",
    "Semaphore",
    "CancellationToken",
    "select!",
    "loop {",
    "Arc<",
    "Mutex",
    "channel(",
    "abort(",
)

FRAMING = (
    "You are authoring one Rust library crate. Read the task and the API sheet "
    "below. You may use only the items on the API sheet plus `std` and the "
    "harness crate `ai_task_support`. Reply with exactly one fenced ```rust "
    "block containing the complete contents of `src/lib.rs`, and nothing else."
)

RUST_FENCE = re.compile(r"```rust[ \t]*\r?\n(.*?)```", re.DOTALL)
ANY_FENCE = re.compile(r"```[a-zA-Z0-9_+-]*[ \t]*\r?\n(.*?)```", re.DOTALL)
TEST_LINE = re.compile(r"^test\s+(?P<name>[A-Za-z0-9_:]+)\s+\.\.\.\s+(?P<status>ok|FAILED|ignored)")
TEST_FN = re.compile(r"#\[test\]\s*(?:pub\s+)?fn\s+(?P<name>[A-Za-z0-9_]+)")

#: Serializes cargo within this process (flock also serializes across processes).
CARGO_MUTEX = threading.Lock()
RESULTS_MUTEX = threading.Lock()


# ── cargo serialization ──────────────────────────────────────────────────────


@contextmanager
def cargo_gate(work: pathlib.Path):
    """Hold the shared cargo lock for the duration of one cargo invocation."""
    CARGO_MUTEX.acquire()
    lock_path = work / ".cargo-gate.lock"
    descriptor = os.open(str(lock_path), os.O_CREAT | os.O_RDWR, 0o600)
    try:
        fcntl.flock(descriptor, fcntl.LOCK_EX)
        yield
    finally:
        fcntl.flock(descriptor, fcntl.LOCK_UN)
        os.close(descriptor)
        CARGO_MUTEX.release()


def cargo_env(work: pathlib.Path) -> dict:
    """The environment every cargo call in this run shares."""
    environment = dict(os.environ)
    environment["CARGO_TARGET_DIR"] = str(work / "target")
    return environment


def run_cargo(args, cwd: pathlib.Path, work: pathlib.Path, timeout: int = 1800):
    """Run one cargo command under the gate. Returns the completed process."""
    with cargo_gate(work):
        return subprocess.run(
            ["cargo", *args],
            cwd=str(cwd),
            env=cargo_env(work),
            capture_output=True,
            text=True,
            timeout=timeout,
        )


# ── prompt and model I/O ─────────────────────────────────────────────────────


def read_task_prompt(task: str) -> str:
    return (TASKS_DIR / task / "prompt.md").read_text()


def read_api_sheet(api: str) -> str:
    return (API_DIR / f"{api}.md").read_text()


def full_prompt(task: str, api: str) -> str:
    return (
        FRAMING
        + "\n\n=== TASK ===\n\n"
        + read_task_prompt(task)
        + "\n\n=== API SHEET ===\n\n"
        + read_api_sheet(api)
        + "\n"
    )


def sbpl_string(path: str) -> str:
    """A path as an SBPL string literal."""
    return '"' + path.replace("\\", "\\\\").replace('"', '\\"') + '"'


def closed_book_profile(scratch: pathlib.Path, work: pathlib.Path) -> str:
    """The `sandbox-exec` profile that makes a trial closed-book.

    The model CLI runs with tools, so without this it can read the hidden
    oracle, the reference solutions, the crate source, earlier sessions and the
    sibling trials' passing answers — the first matrix did exactly that. The
    profile denies the home directory, `/private/tmp` and the run's work
    directory, then re-allows only the CLI's own credentials and settings and
    this trial's scratch directory. Paths are resolved, because the kernel
    matches the real path (`/private/var/...`), never the `$TMPDIR` spelling.
    """
    home = os.path.realpath(os.path.expanduser("~"))
    config = os.path.join(home, ".commandcode")
    withheld = ("projects", "file-history", "history.jsonl", "plans", "scratchpad", "skills")
    return "\n".join(
        [
            "(version 1)",
            "(allow default)",
            "(deny file-read* file-write* (subpath {}) (subpath {}) (subpath {}))".format(
                sbpl_string(home), sbpl_string("/private/tmp"), sbpl_string(os.path.realpath(work))
            ),
            "(allow file-read-metadata (literal {}))".format(sbpl_string(home)),
            "(allow file-read* file-write* (subpath {}))".format(sbpl_string(config)),
            "(deny file-read* file-write* {})".format(
                " ".join(
                    "(subpath {})".format(sbpl_string(os.path.join(config, name)))
                    for name in withheld
                )
            ),
            "(allow file-read* file-write* (subpath {}))".format(
                sbpl_string(os.path.realpath(scratch))
            ),
        ]
    )


def call_model(
    cmd: str,
    model: str,
    prompt: str,
    scratch: pathlib.Path,
    work: pathlib.Path,
    timeout: int = 900,
):
    """Invoke the model CLI once, closed-book. Returns (stdout, stderr)."""
    if sys.platform != "darwin" or shutil.which("sandbox-exec") is None:
        raise RuntimeError(
            "a model trial must be closed-book, and this runner enforces that with "
            "macOS sandbox-exec, which this host lacks"
        )
    argv = [
        "sandbox-exec",
        "-p",
        closed_book_profile(scratch, work),
        cmd,
        "-p",
        "--yolo",
        "-t",
        "--skip-onboarding",
        "--no-auto-update",
        "--no-session",
        "--no-skills",
        "-m",
        model,
        "--effort",
        "high",
        "--max-turns",
        "6",
        "--output-format",
        "json",
    ]
    done = subprocess.run(
        argv,
        input=prompt,
        cwd=str(scratch),
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    return done.stdout, done.stderr


def parse_ndjson(raw: str):
    """Parse the final NDJSON result line: (final_text, tokens_in, tokens_out, ms)."""
    for line in reversed(raw.splitlines()):
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(record, dict):
            continue
        if "finalText" in record or record.get("type") == "result":
            usage = record.get("usage") or {}
            tokens_in = usage.get("inputTokens")
            tokens_out = usage.get("outputTokens")
            duration = record.get("durationMs")
            return (
                record.get("finalText") or "",
                int(tokens_in) if isinstance(tokens_in, (int, float)) else 0,
                int(tokens_out) if isinstance(tokens_out, (int, float)) else 0,
                int(duration) if isinstance(duration, (int, float)) else 0,
            )
    return "", 0, 0, 0


def extract_rust_block(text: str) -> str:
    """The last ```rust block, or the last block of any language, or empty."""
    blocks = RUST_FENCE.findall(text)
    if not blocks:
        blocks = ANY_FENCE.findall(text)
    return blocks[-1].rstrip() + "\n" if blocks else ""


def repair_prompt(original: str, code: str, errors: str) -> str:
    """The re-prompt after a failed compile: original prompt, code, first 80 errors.

    A reply with no fenced block is a failed attempt too: it is re-prompted
    under the same budget rather than compiled as an empty crate.
    """
    if not code:
        return (
            original
            + "\n\n=== PREVIOUS ATTEMPT ===\n\n"
            "Your previous reply contained no fenced ```rust block. Reply with "
            "exactly one fenced ```rust block containing the complete `src/lib.rs`, "
            "and nothing else.\n"
        )
    error_lines = "\n".join(errors.splitlines()[:80])
    return (
        original
        + "\n\n=== PREVIOUS ATTEMPT ===\n\n"
        "Your previous attempt did not compile. Here is the current complete "
        "`src/lib.rs`:\n\n```rust\n" + code + "\n```\n\n"
        "The compiler reported (first 80 lines):\n\n```\n" + error_lines + "\n```\n\n"
        "Reply with exactly one fenced ```rust block containing the corrected "
        "complete `src/lib.rs`, and nothing else.\n"
    )


# ── trial scaffolding ────────────────────────────────────────────────────────


def sanitize(text: str) -> str:
    return re.sub(r"[^A-Za-z0-9_.-]", "_", text)


def package_for(trial_id: str) -> str:
    """A cargo-valid, per-trial package name with a stable prefix."""
    suffix = re.sub(r"[^A-Za-z0-9_]", "_", trial_id)
    return f"{PACKAGE_PREFIX}_{suffix}"[:120]


def write_template(trial_dir: pathlib.Path, package: str) -> None:
    """Copy the template and rewrite its placeholders for a trial directory."""
    (trial_dir / "src").mkdir(parents=True, exist_ok=True)
    manifest = (TEMPLATE / "Cargo.toml").read_text()
    manifest = manifest.replace(NAME_TOKEN, package)
    manifest = manifest.replace(BOT_PATH_TOKEN, str(BOT_DIR.resolve()))
    manifest = manifest.replace(SUPPORT_PATH_TOKEN, str(SUPPORT.resolve()))
    (trial_dir / "Cargo.toml").write_text(manifest)
    lock = COMMITTED_LOCK.read_text()
    lock = lock.replace(f'name = "{NAME_TOKEN}"', f'name = "{package}"')
    (trial_dir / "Cargo.lock").write_text(lock)


def write_solution(trial_dir: pathlib.Path, code: str) -> None:
    (trial_dir / "src").mkdir(parents=True, exist_ok=True)
    (trial_dir / "src" / "lib.rs").write_text(code)


def count_consumer_lines(code: str) -> int:
    total = 0
    for line in code.splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        if stripped.startswith("//"):
            continue
        total += 1
    return total


def count_orchestration_sites(code: str) -> dict:
    counts = {token: code.count(token) for token in ORCHESTRATION_TOKENS}
    return {"total": sum(counts.values()), "tokens": counts}


def oracle_test_names(oracle_source: str) -> list:
    return [match.group("name") for match in TEST_FN.finditer(oracle_source)]


# ── one trial ────────────────────────────────────────────────────────────────


def run_job(job: dict, work: pathlib.Path, bot_sha: str) -> dict:
    model = job["model"]
    api = job["api"]
    task = job["task"]
    trial = job["trial"]
    mode = job["mode"]

    trial_id = f"{sanitize(model)}__{api}__{task}__t{trial}"
    package = package_for(trial_id)
    trial_dir = work / trial_id
    # Each run owns a fresh work directory, so a trial directory never exists
    # already; finding one means two runs collided, and nothing is deleted.
    trial_dir.mkdir(parents=True, exist_ok=False)

    # An empty scratch directory holds only the prompt text files the model reads.
    scratch = trial_dir / "scratch"
    scratch.mkdir()
    (scratch / "prompt.md").write_text(read_task_prompt(task))
    (scratch / "api.md").write_text(read_api_sheet(api))

    tokens_in = 0
    tokens_out = 0
    model_wall_ms = 0
    repairs = 0
    raw_calls = []

    if mode == "model":
        source_prompt = full_prompt(task, api)
        try:
            stdout, stderr = call_model(job["cmd"], model, source_prompt, scratch, work)
        except subprocess.TimeoutExpired:
            stdout, stderr = "", "model call exceeded the 900 s timeout"
        raw_calls.append(stdout + "\n" + stderr)
        text, tokens_in, tokens_out, model_wall_ms = parse_ndjson(stdout)
        if not text.strip():
            text, tokens_in, tokens_out, model_wall_ms = parse_ndjson(stderr)
        code = extract_rust_block(text)
    else:
        reference = REFERENCE / (
            f"mutant-{task}.rs" if mode == "mutant" else f"{api}-{task}.rs"
        )
        code = reference.read_text()
        source_prompt = full_prompt(task, api)

    write_template(trial_dir, package)
    if mode == "mutant":
        # A mutant is the old reference plus one mutation, never a forked copy:
        # the reference is placed beside it as `mod reference`.
        (trial_dir / "src").mkdir(parents=True, exist_ok=True)
        (trial_dir / "src" / "reference.rs").write_text((REFERENCE / f"old-{task}.rs").read_text())

    compile_wall_ms = 0.0

    def attempt(candidate: str):
        """Compile one candidate; a missing block is a failed attempt, not a crate."""
        nonlocal compile_wall_ms
        if not candidate:
            return False, "the reply contained no fenced rust block\n"
        write_solution(trial_dir, candidate)
        started = time.monotonic()
        build = run_cargo(["build", "--locked", "--offline"], trial_dir, work)
        compile_wall_ms += (time.monotonic() - started) * 1000.0
        return build.returncode == 0, (build.stderr or "") + "\n" + (build.stdout or "")

    # Compile, and for a real model, re-prompt on failure up to the repair budget.
    compiled, build_log = attempt(code)
    while not compiled and mode == "model" and repairs < job["max_repairs"]:
        repairs += 1
        try:
            stdout, stderr = call_model(
                job["cmd"], model, repair_prompt(source_prompt, code, build_log), scratch, work
            )
        except subprocess.TimeoutExpired:
            stdout, stderr = "", "model call exceeded the 900 s timeout"
        raw_calls.append(stdout + "\n" + stderr)
        text, more_in, more_out, more_ms = parse_ndjson(stdout)
        if not text.strip():
            text, more_in, more_out, more_ms = parse_ndjson(stderr)
        tokens_in += more_in
        tokens_out += more_out
        model_wall_ms += more_ms
        code = extract_rust_block(text) or code
        compiled, build_log = attempt(code)

    oracle_raw = (TASKS_DIR / task / "oracle.rs").read_text()
    oracle_source = oracle_raw.replace(ORACLE_CRATE_TOKEN, package)
    names = oracle_test_names(oracle_raw)
    oracle = {name: "not_run" for name in names}
    test_output = ""
    if compiled:
        tests_dir = trial_dir / "tests"
        tests_dir.mkdir(exist_ok=True)
        (tests_dir / "oracle.rs").write_text(oracle_source)
        test = run_cargo(
            ["test", "--locked", "--offline", "--", "--test-threads=1"], trial_dir, work
        )
        test_output = (test.stdout or "") + (test.stderr or "")
        for line in test_output.splitlines():
            match = TEST_LINE.match(line.strip())
            if match and match.group("name") in oracle:
                oracle[match.group("name")] = (
                    "pass" if match.group("status") == "ok" else "fail"
                )

    pass_count = sum(1 for value in oracle.values() if value == "pass")
    sites = count_orchestration_sites(code)

    (trial_dir / "lib.rs").write_text(code)
    (trial_dir / "raw.ndjson").write_text("\n".join(raw_calls))
    (trial_dir / "build.log").write_text(build_log)
    (trial_dir / "test.log").write_text(test_output)

    return {
        "model": model,
        "api": api,
        "task": task,
        "trial": trial,
        "trial_id": trial_id,
        "mode": mode,
        "compiled": compiled,
        "repairs": repairs,
        "oracle": oracle,
        "oracle_pass_count": pass_count,
        "oracle_total": len(oracle),
        "tokens_in": tokens_in,
        "tokens_out": tokens_out,
        "model_wall_ms": model_wall_ms,
        "compile_wall_ms": round(compile_wall_ms, 3),
        "consumer_lines": count_consumer_lines(code),
        "orchestration_sites": sites["total"],
        "orchestration_site_tokens": sites["tokens"],
        "orchestration_token_list": list(ORCHESTRATION_TOKENS),
        "lockfile": "bench/ai-authoring/Cargo.lock copied per trial, root package name rewritten",
        "lgwks_bot_sha": bot_sha,
        "lib_rs": code,
    }


# ── summary ──────────────────────────────────────────────────────────────────


def median_or_none(values):
    numeric = [value for value in values if value is not None]
    if not numeric:
        return None
    return statistics.median(numeric)


def summarize(records: list) -> dict:
    groups = {}
    for record in records:
        key = (record["model"], record["api"], record["task"])
        groups.setdefault(key, []).append(record)
    output = []
    for (model, api, task), rows in sorted(groups.items()):
        repairs = [row["repairs"] for row in rows]
        per_trial = [
            {
                "trial": row["trial"],
                "compiled": row["compiled"],
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
                "n": len(rows),
                "compiled": sum(1 for row in rows if row["compiled"]),
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
            }
        )
    return output


# ── main ─────────────────────────────────────────────────────────────────────


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--models",
        default="stealth/space-bunny-alpha,deepseek/deepseek-v4.1-flash",
        help="comma-separated fixed model ids",
    )
    parser.add_argument("--apis", default="old,new")
    parser.add_argument("--tasks", default="aggregate,pipeline")
    parser.add_argument("--trials", type=int, default=3)
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
    args = parser.parse_args()

    models = [item for item in args.models.split(",") if item]
    apis = [item for item in args.apis.split(",") if item]
    tasks = [item for item in args.tasks.split(",") if item]
    mode = "mutants" if args.mutants else ("dry" if args.dry_run else "models")
    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime()) + "-" + mode
    # A run never reuses or overwrites another run's evidence: both the work root
    # and the results directory are new, and an existing one is a refusal.
    work = (args.work or pathlib.Path(os.environ.get("TMPDIR", "/tmp")) / f"lgwks-ai-authoring-{stamp}").resolve()
    work.mkdir(parents=True, exist_ok=False)
    run_dir = args.out or (HERE / "runs" / stamp)
    run_dir = run_dir if run_dir.is_absolute() else (REPO / run_dir)
    run_dir.mkdir(parents=True, exist_ok=False)
    out = run_dir / "results.jsonl"

    bot_sha = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=str(REPO), capture_output=True, text=True, check=False
    ).stdout.strip()

    jobs = []
    if args.mutants:
        for task in tasks:
            jobs.append(
                {
                    "mode": "mutant",
                    "model": "__mutant__",
                    "api": "new",
                    "task": task,
                    "trial": 0,
                    "cmd": args.cmd,
                    "max_repairs": 0,
                }
            )
    elif args.dry_run:
        for api in apis:
            for task in tasks:
                for trial in range(args.trials):
                    jobs.append(
                        {
                            "mode": "dry",
                            "model": "__dry-run__",
                            "api": api,
                            "task": task,
                            "trial": trial,
                            "cmd": args.cmd,
                            "max_repairs": args.max_repairs,
                        }
                    )
    else:
        for model in models:
            for api in apis:
                for task in tasks:
                    for trial in range(args.trials):
                        jobs.append(
                            {
                                "mode": "model",
                                "model": model,
                                "api": api,
                                "task": task,
                                "trial": trial,
                                "cmd": args.cmd,
                                "max_repairs": args.max_repairs,
                            }
                        )

    records = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=max(1, args.parallel)) as pool:
        futures = {pool.submit(run_job, job, work, bot_sha): job for job in jobs}
        for future in concurrent.futures.as_completed(futures):
            job = futures[future]
            try:
                record = future.result()
            except Exception as error:  # a crashed trial is a recorded trial
                record = {
                    "model": job["model"],
                    "api": job["api"],
                    "task": job["task"],
                    "trial": job["trial"],
                    "trial_id": "crashed",
                    "mode": job["mode"],
                    "compiled": False,
                    "repairs": 0,
                    "oracle": {},
                    "oracle_pass_count": 0,
                    "oracle_total": 0,
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
                records.append(record)
            print(
                f"{record['model']:28} {record['api']:4} {record['task']:10} "
                f"t{record['trial']} compiled={record['compiled']} "
                f"oracle={record['oracle_pass_count']}/{record['oracle_total']}",
                flush=True,
            )

    records.sort(key=lambda row: (row["model"], row["api"], row["task"], row["trial"]))
    out.parent.mkdir(parents=True, exist_ok=True)
    with out.open("w") as handle:
        for record in records:
            handle.write(json.dumps(record) + "\n")

    summary = {
        "generated_by": "bench/ai-authoring/run.py",
        "lgwks_bot_sha": bot_sha,
        "protocol": {
            "same_prompt_skeleton": True,
            "same_sheet_budget": "each API sheet <= 250 lines",
            "same_oracle": True,
            "hidden_oracle": True,
            "repair_budget": args.max_repairs,
            "cargo_serialized": "one cargo process at a time under an fcntl lock",
            "closed_book": "each model call runs under sandbox-exec: home, /private/tmp and the work directory are unreadable except the CLI config and the trial scratch; no session, no skills",
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
