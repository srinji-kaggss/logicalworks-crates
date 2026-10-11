"""How the gate starts, bounds and stops its children, and where they write.

Split from ``ci_local.py`` under the size law (#408). Every child leads its own
process group and is recorded, so stopping the gate stops what it started; the
scratch volume is RAM-backed where the host allows and reclaimed after a crash.
"""

from __future__ import annotations

import contextlib
import os
import shutil
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path


# ── process helpers ─────────────────────────────────────────────────────────


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


def _run_command(command: str, root: Path, env: dict[str, str], out=None) -> int:
    return _spawn(["bash", "-c", command], root, env, out)


def _stamp() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def _child_env(root: Path, scratch: Path | None = None, target: Path | None = None) -> dict[str, str]:
    env = os.environ.copy()
    cargo = Path.home() / ".cargo" / "bin"
    env["PATH"] = f"{cargo}{os.pathsep}{env.get('PATH', '')}"
    env["CARGO_INCREMENTAL"] = "0"
    env["CARGO_TERM_COLOR"] = "never"
    env.setdefault("RUST_BACKTRACE", "1")
    if scratch is not None:
        env["TMPDIR"] = f"{scratch}{os.sep}"
    if target is not None:
        env["CARGO_TARGET_DIR"] = str(target)
    return env


# ── scratch ─────────────────────────────────────────────────────────────────

SCRATCH_PREFIX = "lgwks-gate-"
# 4 GiB of 512-byte sectors. A RAM disk takes memory only as it is written; the
# simulation suites leave ~400 KB behind and peak far below this.
RAM_DISK_SECTORS = 8 * 1024 * 1024


def _pid_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def _owner_pid(name: str) -> int | None:
    suffix = name[len(SCRATCH_PREFIX):]
    return int(suffix) if suffix.isdigit() else None


def _reclaim_stale_scratch() -> None:
    """Remove scratch volumes whose gate process is gone (kill -9, power loss)."""
    if sys.platform == "darwin":
        for volume in Path("/Volumes").glob(f"{SCRATCH_PREFIX}*"):
            pid = _owner_pid(volume.name)
            if pid is not None and not _pid_alive(pid):
                _capture(["diskutil", "eject", str(volume)], Path("/"))
    shm = Path("/dev/shm")
    if shm.is_dir():
        for directory in shm.glob(f"{SCRATCH_PREFIX}*"):
            pid = _owner_pid(directory.name)
            if pid is not None and not _pid_alive(pid):
                shutil.rmtree(directory, ignore_errors=True)


@contextlib.contextmanager
def gate_scratch(wanted: bool):
    """Yield a RAM-backed directory for the run's TMPDIR, or None.

    None means the lanes use the ordinary temporary directory; the run is then
    slower, never different. Creating the volume is best-effort for the same
    reason: a host that cannot make one still runs every lane.
    """
    if not wanted:
        yield None
        return
    _reclaim_stale_scratch()
    name = f"{SCRATCH_PREFIX}{os.getpid()}"
    if sys.platform == "darwin" and shutil.which("hdiutil") and shutil.which("diskutil"):
        attached = _capture(["hdiutil", "attach", "-nomount", f"ram://{RAM_DISK_SECTORS}"], Path("/"))
        device = next((word for word in attached.split() if word.startswith("/dev/disk")), None)
        if device is not None:
            erased = subprocess.run(
                ["diskutil", "erasevolume", "APFS", name, device],
                capture_output=True,
                text=True,
                timeout=120,
                check=False,
            )
            volume = Path("/Volumes") / name
            try:
                if erased.returncode == 0 and volume.is_dir():
                    yield volume
                    return
            finally:
                _capture(["hdiutil", "detach", device, "-force"], Path("/"))
    shm = Path("/dev/shm")
    if sys.platform.startswith("linux") and shm.is_dir() and os.access(shm, os.W_OK):
        directory = shm / name
        directory.mkdir(exist_ok=True)
        try:
            yield directory
        finally:
            shutil.rmtree(directory, ignore_errors=True)
        return
    yield None


def _capture(argv: list[str], root: Path) -> str:
    try:
        completed = subprocess.run(argv, cwd=root, capture_output=True, text=True, timeout=30, check=False)
    except (OSError, subprocess.TimeoutExpired):
        return "missing"
    return (completed.stdout or completed.stderr).strip() or "missing"
