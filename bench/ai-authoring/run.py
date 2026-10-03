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

#: The five required user profiles, as fixed persona preambles.
#:
#: Each is prepended to the SAME prompt skeleton — same task, same API sheet,
#: same framing, same repair budget, same sandbox — so the only thing that
#: differs between two cells of the same (model, api, task) is the sentence
#: below. The profiles are fixed text and never generated, because a profile
#: that varied per trial would be a second uncontrolled variable and the
#: comparison would be measuring the draw rather than the profile.
#:
#: `agent` is deliberately the last instruction in the preamble it belongs to:
#: the framing that follows it ("reply with exactly one fenced ```rust block")
#: is the same for every profile, so `agent` cannot be given a shorter reply
#: contract than the others without changing more than the persona.
PROFILES = {
    "first-time": (
        "You have never used this crate's orchestration API before. Read the API "
        "sheet literally and follow it exactly as written. Where it names an item "
        "or a signature, use that item with that signature rather than reaching "
        "for something you remember from elsewhere. If something is not on the "
        "sheet, it is not available to you."
    ),
    "expert-hurry": (
        "You know this domain well and you are in a hurry. You are looking for "
        "the shortest thing that might work, and you will not re-read the API "
        "sheet's prose a second time. Skim it, write the minimal correct answer, "
        "and stop."
    ),
    "anxious": (
        "You do not trust this API to clean up after itself. Assume every "
        "resource you start stays live unless you explicitly stop it, and assume "
        "every fallible call needs your own error handling rather than an "
        "implicit one. Be explicit about cleanup and about every error path."
    ),
    "misuser": (
        "You are confident but mistaken. The most plausible mistake here is to "
        "write the obvious first solution, which quietly ignores the deadline or "
        "the path that runs when the work is dropped. Write it anyway; when the "
        "compiler or the oracle tells you it is wrong, read what it says and fix "
        "it from that evidence."
    ),
    "agent": (
        "You are an AI coding agent working on an automated pipeline. Output only "
        "code. Do not explain, do not ask questions, and do not produce anything "
        "outside the single required fenced block."
    ),
}

#: The profiles a run covers when `--profiles` is not given.
DEFAULT_PROFILES = tuple(PROFILES)

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


#: The cleanup clause of each task: the oracle test that says a dropped future
#: left nothing live.
#:
#: Spelled per task because the harness's instruments are named per task — a
#: fetch, a stage, a unit — and "the drop clause" has to name one test rather
#: than a shape. `cleanup_ok` reads this map, so a task whose oracle renames its
#: drop clause stops reporting a cleanup verdict rather than silently reporting
#: the wrong one.
DROP_CLAUSE_BY_TASK = {
    "aggregate": "dropping_the_future_leaves_no_fetch_live",
    "pipeline": "dropping_the_future_leaves_no_stage_live",
    "recovery": "dropping_the_future_leaves_no_unit_live",
}

#: The `/usr/bin/time` that measures the oracle process. macOS's `-l` is the
#: long form, and it is the one that reports `maximum resident set size` in
#: bytes; without it there is no RSS number to record rather than a default one.
TIME_BIN = "/usr/bin/time"


def run_timed_cargo(args, cwd: pathlib.Path, work: pathlib.Path, timeout: int = 1800):
    """Run one cargo command under `/usr/bin/time -l`, under the same gate.

    The timer wraps cargo, so what is measured is the cargo invocation that runs
    the oracle's test binary — the compile is already done by this point, so the
    number is the oracle's own cost rather than a build's. Falls back to a plain
    cargo call when this host has no `/usr/bin/time`, so a Linux runner still
    produces verdicts; the peak RSS is then `None`, which says "not measured"
    rather than reporting a number nobody took.
    """
    if not pathlib.Path(TIME_BIN).exists():
        return run_cargo(args, cwd, work, timeout)
    with cargo_gate(work):
        return subprocess.run(
            [TIME_BIN, "-l", "cargo", *args],
            cwd=str(cwd),
            env=cargo_env(work),
            capture_output=True,
            text=True,
            timeout=timeout,
        )


def parse_peak_rss(stderr: str):
    """The peak RSS in bytes from a `/usr/bin/time -l` report, or `None`.

    The line is matched by its *label*, not by its start, because macOS and GNU
    order the two fields differently: macOS prints `67911680  maximum resident
    set size` and GNU prints `maximum resident set size (kbytes): 67911680`.
    Matching on the line's first token silently loses the metric on one of the
    two — and `None` is a loss that looks like "not measured" forever.

    The value is taken as the first or last token depending on which side the
    label sits, because the units differ too: macOS reports bytes and this
    metric is named `_bytes`, while the GNU form reports kibibytes. A GNU host
    therefore multiplies by 1024 rather than reporting a number 1024 times too
    small under a name that says bytes.
    """
    for line in stderr.splitlines():
        parts = line.strip().split()
        # The label is matched case-insensitively because the two forms differ
        # in case as well as order: GNU capitalises `Maximum resident set size`
        # and macOS does not. A case-sensitive match reads the GNU form as an
        # absent measurement rather than a wrong one, which is the harder of the
        # two to notice.
        lowered = [part.lower() for part in parts]
        if "maximum" not in lowered:
            continue
        label = lowered.index("maximum")
        if label == 0:
            # GNU order: `maximum resident set size (kbytes): <value>`.
            candidate = parts[-1]
            return int(candidate) * 1024 if candidate.isdigit() else None
        # macOS order: `<value>  maximum resident set size`.
        candidate = parts[label - 1]
        return int(candidate) if candidate.isdigit() else None
    return None


# ── prompt and model I/O ─────────────────────────────────────────────────────


def read_task_prompt(task: str) -> str:
    return (TASKS_DIR / task / "prompt.md").read_text()


def read_api_sheet(api: str) -> str:
    return (API_DIR / f"{api}.md").read_text()


def profile_preamble(profile: str) -> str:
    """The fixed persona text for `profile`, or an empty string for no profile."""
    return PROFILES.get(profile, "")


def full_prompt(task: str, api: str, profile: str = "") -> str:
    """The one prompt skeleton, with `profile`'s preamble in front of it.

    The preamble goes first and the framing last, so the reply contract — one
    fenced block, nothing else — is the final instruction the model reads and
    every profile answers under the same one.
    """
    preamble = profile_preamble(profile)
    parts = []
    if preamble:
        parts.append("=== ROLE ===\n\n" + preamble)
    parts.append("=== TASK ===\n\n" + read_task_prompt(task))
    parts.append("=== API SHEET ===\n\n" + read_api_sheet(api))
    return FRAMING + "\n\n" + "\n\n".join(parts) + "\n"


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


#: The reference a mutant is derived from, per task.
#:
#: `aggregate` and `pipeline` have both an old and a new reference, so their
#: mutants are the *old* one plus a mutation. `recovery` has only a new
#: reference — the old `rt` surface cannot express the task at all — so its
#: mutant is the new one plus a mutation. Naming it here rather than guessing
#: `old-` from the task name is what keeps "the mutant is one mutation away
#: from a reference that passes" true for a task with only one reference.
MUTANT_BASE = {"aggregate": "old", "pipeline": "old", "recovery": "new"}

#: Tasks the old `lgwks_bot::rt` surface cannot express at all, with the reason.
#:
#: A cell that asked a model for one of these against the old sheet would be
#: measuring a model guessing at a capability the API does not have, and any
#: score it produced would be about the sheet's silence rather than about the
#: model. The reason is recorded in the run's protocol block so the omission is
#: in the evidence rather than inferred from a missing row.
NEW_ONLY_TASKS = {"recovery"}

#: Why `recovery` has no old-API arm.
NEW_ONLY_TASK_WHY = (
    "the old rt surface has no durable run store, no remember, no run identity "
    "and no resume, so it keeps no record of a completed unit to consult and "
    "cannot express 'finish the work without redoing a completed unit'"
)


# ── one trial ────────────────────────────────────────────────────────────────


def run_job(job: dict, work: pathlib.Path, bot_sha: str) -> dict:
    model = job["model"]
    api = job["api"]
    task = job["task"]
    trial = job["trial"]
    mode = job["mode"]
    profile = job.get("profile", "")

    # The profile is in the trial id, so two profiles of one (model, api, task)
    # never share a package name and therefore never share a `-C metadata`: a
    # shared one would let one trial's oracle binary stand in for another's,
    # which is the same defect the per-trial package name exists to prevent.
    trial_id = f"{sanitize(model)}__{api}__{task}__{profile or 'none'}__t{trial}"
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
    if profile:
        (scratch / "role.md").write_text(profile_preamble(profile))

    tokens_in = 0
    tokens_out = 0
    model_wall_ms = 0
    repairs = 0
    raw_calls = []

    if mode == "model":
        source_prompt = full_prompt(task, api, profile)
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
        source_prompt = full_prompt(task, api, profile)

    write_template(trial_dir, package)
    if mode == "mutant":
        # A mutant is its reference plus one mutation, never a forked copy: the
        # reference is placed beside it as `mod reference`, so the two cannot
        # drift and a mutant that stops differing from its reference in anything
        # but the intended mutation shows up as a diff rather than as a verdict.
        base = MUTANT_BASE.get(task)
        if base is None:
            raise KeyError(f"no mutant base is declared for task {task!r}")
        (trial_dir / "src").mkdir(parents=True, exist_ok=True)
        (trial_dir / "src" / "reference.rs").write_text(
            (REFERENCE / f"{base}-{task}.rs").read_text()
        )

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
    oracle_wall_ms = 0.0
    oracle_peak_rss_bytes = None
    if compiled:
        tests_dir = trial_dir / "tests"
        tests_dir.mkdir(exist_ok=True)
        (tests_dir / "oracle.rs").write_text(oracle_source)
        # The oracle runs under `/usr/bin/time -l`, so the wall time and the peak
        # RSS of the *test process* are both measured rather than inferred: the
        # `time -l` report goes to stderr, which is appended to the same stream
        # the verdicts are parsed out of. A plain cargo invocation would give the
        # oracle's cost as "whatever cargo printed", which is not a measurement of
        # anything the task cares about.
        started = time.monotonic()
        test = run_timed_cargo(
            ["test", "--locked", "--offline", "--", "--test-threads=1"], trial_dir, work
        )
        oracle_wall_ms = (time.monotonic() - started) * 1000.0
        test_output = (test.stdout or "") + (test.stderr or "")
        oracle_peak_rss_bytes = parse_peak_rss(test.stderr or "")
        for line in test_output.splitlines():
            match = TEST_LINE.match(line.strip())
            if match and match.group("name") in oracle:
                oracle[match.group("name")] = (
                    "pass" if match.group("status") == "ok" else "fail"
                )

    pass_count = sum(1 for value in oracle.values() if value == "pass")
    sites = count_orchestration_sites(code)
    # The drop clause is the cleanup half of every task's contract: a task whose
    # "dropping the future leaves nothing live" test did not run — because the
    # crate never compiled — has not demonstrated cleanup at all, and reporting
    # that as a clean run would read a compile failure as a cleanup result.
    cleanup_ok = oracle.get(DROP_CLAUSE_BY_TASK.get(task, ""), "not_run") == "pass"

    (trial_dir / "lib.rs").write_text(code)
    (trial_dir / "raw.ndjson").write_text("\n".join(raw_calls))
    (trial_dir / "build.log").write_text(build_log)
    (trial_dir / "test.log").write_text(test_output)

    return {
        "model": model,
        "api": api,
        "task": task,
        "profile": profile,
        "trial": trial,
        "trial_id": trial_id,
        "mode": mode,
        "compiled": compiled,
        "repairs": repairs,
        "oracle": oracle,
        "oracle_pass_count": pass_count,
        "oracle_total": len(oracle),
        "cleanup_ok": cleanup_ok,
        "oracle_wall_ms": round(oracle_wall_ms, 3),
        "oracle_peak_rss_bytes": oracle_peak_rss_bytes,
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
        default="stealth/space-bunny-alpha,deepseek/deepseek-v4.1-flash",
        help="comma-separated fixed model ids",
    )
    parser.add_argument("--apis", default="old,new")
    parser.add_argument("--tasks", default="aggregate,pipeline,recovery")
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
    # guessing at a capability the API does not have. The omission is recorded in
    # the run's protocol block rather than left for a reader to notice.
    skipped = [
        {"task": task, "api": "old", "why": NEW_ONLY_TASK_WHY}
        for task in tasks
        if task in NEW_ONLY_TASKS
    ]
    jobs = [job for job in jobs if not (job["task"] in NEW_ONLY_TASKS and job["api"] == "old")]
    if skipped:
        print(f"skipped {len(skipped)} old-API cell(s): " + ", ".join(
            f"{item['task']}/old" for item in skipped
        ))
        for item in skipped:
            print(f"  {item['task']}/old: {item['why']}")

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
