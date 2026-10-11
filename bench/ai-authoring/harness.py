"""Shared paths, cargo serialization and the oracle's timed run for the authoring bench (#270)."""

import fcntl
import os
import pathlib
import re
import signal
import subprocess
import threading
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

TEST_LINE = re.compile(r"^test\s+(?P<name>[A-Za-z0-9_:]+)\s+\.\.\.\s+(?P<status>ok|FAILED|ignored)")
TEST_FN = re.compile(r"#\[test\]\s*(?:pub\s+)?fn\s+(?P<name>[A-Za-z0-9_]+)")

#: Serializes cargo within this process (flock also serializes across processes).
CARGO_MUTEX = threading.Lock()


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


#: How long the oracle's own test process may run before it is killed. The
#: oracles finish in about a second; a solution that leaks a task, or waits on
#: something that never resolves, is a failed trial, not a reason to hold the
#: whole matrix for half an hour.
ORACLE_TIMEOUT_S = 180


def run_group(argv, cwd: pathlib.Path, env, timeout: int, stdin_text=None):
    """`subprocess.run`, except that a timeout kills the whole process group.

    `subprocess.run` kills only the direct child on a timeout. Here the direct
    child is `cargo` (or `/usr/bin/time`), and the process that is actually stuck
    is the oracle's test binary underneath it, so a plain timeout left that
    binary running with its parent gone: three of them were found parented to
    pid 1 more than a day later. Each command runs in its own session so the
    group can be killed as one.
    """
    process = subprocess.Popen(
        argv,
        cwd=str(cwd),
        env=env,
        stdin=subprocess.PIPE if stdin_text is not None else None,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    try:
        stdout, stderr = process.communicate(input=stdin_text, timeout=timeout)
    except subprocess.TimeoutExpired as expired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        stdout, stderr = process.communicate()
        raise subprocess.TimeoutExpired(argv, timeout, output=stdout, stderr=stderr) from expired
    return subprocess.CompletedProcess(argv, process.returncode, stdout, stderr)


def run_cargo(args, cwd: pathlib.Path, work: pathlib.Path, timeout: int = 1800):
    """Run one cargo command under the gate. Returns the completed process."""
    with cargo_gate(work):
        return run_group(["cargo", *args], cwd, cargo_env(work), timeout)


#: The `/usr/bin/time` that measures the oracle process. macOS's `-l` is the
#: long form, and it is the one that reports `maximum resident set size` in
#: bytes; without it there is no RSS number to record rather than a default one.
TIME_BIN = "/usr/bin/time"


def run_timed_cargo(args, cwd: pathlib.Path, work: pathlib.Path, timeout: int = ORACLE_TIMEOUT_S):
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
        return run_group([TIME_BIN, "-l", "cargo", *args], cwd, cargo_env(work), timeout)


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
