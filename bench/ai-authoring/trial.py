"""One trial of the authoring bench: scaffold, author, compile, repair and grade (#270)."""

import pathlib
import re
import subprocess
import time

from harness import (
    BOT_DIR,
    BOT_PATH_TOKEN,
    COMMITTED_LOCK,
    NAME_TOKEN,
    ORACLE_CRATE_TOKEN,
    PACKAGE_PREFIX,
    REFERENCE,
    SUPPORT,
    SUPPORT_PATH_TOKEN,
    TASKS_DIR,
    TEMPLATE,
    TEST_FN,
    TEST_LINE,
    parse_peak_rss,
    reservation_for,
    run_cargo,
    run_timed_cargo,
)
from prompts import (
    call_model,
    extract_rust_block,
    full_prompt,
    parse_ndjson,
    profile_preamble,
    read_api_sheet,
    read_task_prompt,
    repair_prompt,
)

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
    "capture": "cancelling_leaves_no_descendant",
    "tenant-cache": "dropping_mid_flight_leaves_the_cache_usable",
    "durable-retry": "dropping_mid_wait_applies_nothing_twice",
}


def cleanup_ok(task: str, oracle: dict) -> bool:
    """Whether the task's own drop clause passed.

    The drop clause is the cleanup half of every task's contract: a task whose
    "dropping the future leaves nothing live" test did not run — because the
    crate never compiled — has not demonstrated cleanup at all, and reporting
    that as a clean run would read a compile failure as a cleanup result.
    """
    clause = DROP_CLAUSE_BY_TASK.get(task)
    return clause is not None and oracle.get(clause) == "pass"


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
MUTANT_BASE = {
    "aggregate": "old",
    "pipeline": "old",
    "recovery": "new",
    "capture": "new",
    "tenant-cache": "new",
    "durable-retry": "new",
}

#: The reference an arm's dry run compiles, when it is another arm's, keyed by
#: arm or by `(arm, task)`. The `lexicon` arm authors `script!` from the
#: generated lexicon rather than the script sheet, so its surface and reference
#: are the script arm's. The fan sheet is the new sheet plus `FanOut`, and a
#: sequential durable resume has nothing to fan out, so the new arm's recovery
#: reference is a fan solution as written.
REFERENCE_SURFACE = {"lexicon": "script", ("fan", "recovery"): "new"}

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

#: Tasks the old `lgwks_bot::rt` surface cannot express at all, for the same
#: reason `recovery` is new-only: a cell asking for one would measure a model
#: guessing at a capability the sheet does not offer.
#:
#: `capture` has no old-API arm because the old sheet offers no supervised
#: process spawn: `Supervisor::run_process` is outside the sheet, so the task
#: that needs owned processes, bounded capture and group cleanup cannot be
#: written from it. The omission is recorded in the run's protocol block like
#: `recovery`'s, rather than left for a reader to notice.
NO_OLD_TASKS = {"capture"}

#: Why `capture` has no old-API arm.
NO_OLD_TASK_WHY = (
    "the old rt sheet offers no supervised process spawn, so the task that "
    "needs owned processes, bounded capture and group cleanup cannot be "
    "written from it"
)

#: Tasks the `futures` sheet cannot express at all.
#:
#: `recovery` has no futures arm for the same reason it has no old arm: the
#: futures sheet offers no durable run store, no `remember`, no run identity
#: and no resume, so a cell asking for one would measure a model guessing at
#: a capability the sheet does not offer.
NO_FUTURES_TASKS = {"recovery"}

#: Why `recovery` has no futures arm.
NO_FUTURES_TASK_WHY = (
    "the futures sheet offers no durable run store, no remember, no run "
    "identity and no resume, so it keeps no record of a completed unit to "
    "consult and cannot express 'finish the work without redoing a completed "
    "unit'"
)


# ── one trial ────────────────────────────────────────────────────────────────


def budgeted_call(job: dict, prompt: str, scratch: pathlib.Path, work: pathlib.Path):
    """One model call under the run's `TokenBudget`, parsed; `None` when refused.

    Returns `(raw, text, tokens_in, tokens_out, model_ms)`. A refused call never
    reaches the model, so it costs nothing and the ceiling holds.
    """
    budget = job.get("budget")
    reservation = reservation_for(prompt)
    if budget is not None and not budget.admit(reservation):
        return None
    used = 0
    try:
        try:
            stdout, stderr = call_model(job["cmd"], job["model"], prompt, scratch, work)
        except subprocess.TimeoutExpired:
            stdout, stderr = "", "model call exceeded the 900 s timeout"
        text, tokens_in, tokens_out, model_ms = parse_ndjson(stdout)
        if not text.strip():
            text, tokens_in, tokens_out, model_ms = parse_ndjson(stderr)
        used = tokens_in + tokens_out
        return stdout + "\n" + stderr, text, tokens_in, tokens_out, model_ms
    finally:
        if budget is not None:
            budget.settle(reservation, used)


def trial_id_of(job: dict) -> str:
    """The trial's directory name and package seed.

    The profile is in the id, so two profiles of one (model, api, task) never
    share a package name and therefore never share a `-C metadata`: a shared
    one would let one trial's oracle binary stand in for another's, which is
    the same defect the per-trial package name exists to prevent.
    """
    profile = job.get("profile", "")
    return f"{sanitize(job['model'])}__{job['api']}__{job['task']}__{profile or 'none'}__t{job['trial']}"


def run_job(job: dict, work: pathlib.Path, bot_sha: str) -> dict:
    model = job["model"]
    api = job["api"]
    task = job["task"]
    trial = job["trial"]
    mode = job["mode"]
    profile = job.get("profile", "")

    trial_id = trial_id_of(job)
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
    budget_refused = False

    if mode == "model":
        source_prompt = full_prompt(task, api, profile)
        called = budgeted_call(job, source_prompt, scratch, work)
        if called is None:
            # Past the ceiling: the trial is recorded as never run, not as failed.
            return {
                "model": model, "api": api, "task": task, "profile": profile,
                "trial": trial, "trial_id": trial_id, "mode": mode,
                "budget_refused": True, "compiled": False, "repairs": 0,
                "oracle": {}, "oracle_pass_count": 0, "oracle_total": 0,
                "cleanup_ok": False, "oracle_wall_ms": 0.0, "oracle_peak_rss_bytes": None,
                "tokens_in": 0, "tokens_out": 0, "model_wall_ms": 0, "compile_wall_ms": 0,
                "consumer_lines": 0, "orchestration_sites": 0, "orchestration_site_tokens": {},
                "orchestration_token_list": list(ORCHESTRATION_TOKENS),
                "lockfile": "", "lgwks_bot_sha": bot_sha, "lib_rs": "",
            }
        raw, text, tokens_in, tokens_out, model_wall_ms = called
        raw_calls.append(raw)
        code = extract_rust_block(text)
    else:
        # The lexicon arm writes the same `script!` language from a different
        # sheet, so the script reference is its reference.
        surface = REFERENCE_SURFACE.get((api, task), REFERENCE_SURFACE.get(api, api))
        reference = REFERENCE / (
            f"mutant-{task}.rs" if mode == "mutant" else f"{surface}-{task}.rs"
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
        called = budgeted_call(job, repair_prompt(source_prompt, code, build_log), scratch, work)
        if called is None:
            budget_refused = True
            break
        repairs += 1
        raw, text, more_in, more_out, more_ms = called
        raw_calls.append(raw)
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
        timed_out = False
        try:
            test = run_timed_cargo(
                ["test", "--locked", "--offline", "--", "--test-threads=1"], trial_dir, work
            )
            test_stdout, test_stderr = test.stdout or "", test.stderr or ""
        except subprocess.TimeoutExpired as expired:
            # A hung oracle is this solution's failure, recorded as one: the clauses
            # that finished keep their verdicts and the rest are `timeout`.
            timed_out = True
            test_stdout = (expired.output or b"").decode() if isinstance(expired.output, bytes) else (expired.output or "")
            test_stderr = (expired.stderr or b"").decode() if isinstance(expired.stderr, bytes) else (expired.stderr or "")
        oracle_wall_ms = (time.monotonic() - started) * 1000.0
        test_output = test_stdout + test_stderr
        oracle_peak_rss_bytes = parse_peak_rss(test_stderr)
        for line in test_output.splitlines():
            match = TEST_LINE.match(line.strip())
            if match and match.group("name") in oracle:
                oracle[match.group("name")] = (
                    "pass" if match.group("status") == "ok" else "fail"
                )
        if timed_out:
            oracle = {
                name: ("timeout" if status == "not_run" else status)
                for name, status in oracle.items()
            }

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
        "profile": profile,
        "trial": trial,
        "trial_id": trial_id,
        "mode": mode,
        "budget_refused": budget_refused,
        "compiled": compiled,
        "repairs": repairs,
        "oracle": oracle,
        "oracle_pass_count": pass_count,
        "oracle_total": len(oracle),
        "cleanup_ok": cleanup_ok(task, oracle),
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
