#!/usr/bin/env python3
"""Child processes for the local gate: every child the gate starts leads its own
process group and is recorded, so stopping the gate stops what it started.

Split out of ``scripts/ci_local.py`` under the size law (Director 2026-10-10);
the gate driver and its builtins both start children through here.
"""

from __future__ import annotations

import contextlib
import os
import signal
import subprocess
import threading
from pathlib import Path


def _git(root: Path, args: list[str]) -> str | None:
    try:
        completed = subprocess.run(
            ["git", *args],
            cwd=root,
            capture_output=True,
            text=True,
            timeout=60,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if completed.returncode != 0:
        return None
    return completed.stdout


# Every child the gate starts leads its own process group and is recorded here,
# so stopping the gate stops what it started. Before this, killing ci_local.py
# left its nextest run building and testing on, holding the target lock and the
# CPU that the next run then measured against.
_CHILDREN: set[subprocess.Popen] = set()
_CHILDREN_LOCK = threading.Lock()
# Set once the gate is stopping: no new child starts after it, so a group that
# is between lanes cannot launch the next one into a scratch volume being torn down.
_STOPPING = threading.Event()


def _spawn(argv: list[str], root: Path, env: dict[str, str], out=None) -> int:
    """Run ``argv`` in its own process group, bounded by one hour."""
    if _STOPPING.is_set():
        return 130
    try:
        child = subprocess.Popen(
            argv,
            cwd=root,
            env=env,
            stdout=out,
            stderr=subprocess.STDOUT if out is not None else None,
            start_new_session=True,
        )
    except OSError:
        return 125
    with _CHILDREN_LOCK:
        _CHILDREN.add(child)
    try:
        return child.wait(timeout=3600)
    except subprocess.TimeoutExpired:
        _stop(child)
        return 125
    finally:
        with _CHILDREN_LOCK:
            _CHILDREN.discard(child)


def _stop(child: subprocess.Popen) -> None:
    """Terminate a child's whole process group, then reap it."""
    with contextlib.suppress(ProcessLookupError, PermissionError):
        os.killpg(child.pid, signal.SIGTERM)
    try:
        child.wait(timeout=10)
    except subprocess.TimeoutExpired:
        with contextlib.suppress(ProcessLookupError, PermissionError):
            os.killpg(child.pid, signal.SIGKILL)
        with contextlib.suppress(subprocess.TimeoutExpired):
            child.wait(timeout=10)


def _stop_all_children() -> None:
    _STOPPING.set()
    with _CHILDREN_LOCK:
        live = list(_CHILDREN)
    for child in live:
        _stop(child)


def _run_argv(argv: list[str], root: Path, env: dict[str, str], out=None) -> int:
    return _spawn(argv, root, env, out)


def _run_capture(
    argv: list[str],
    root: Path,
    env: dict[str, str],
    timeout: int,
) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            argv,
            cwd=root,
            env=env,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
    except OSError as error:
        return subprocess.CompletedProcess(argv, 125, "", str(error))
    except subprocess.TimeoutExpired as error:
        stdout = error.stdout if isinstance(error.stdout, str) else ""
        stderr = error.stderr if isinstance(error.stderr, str) else "timed out"
        return subprocess.CompletedProcess(
            argv,
            125,
            stdout,
            stderr,
        )
