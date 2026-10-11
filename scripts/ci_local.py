#!/usr/bin/env python3
"""Local gate coordinator.

Executes the lanes declared in ``scripts/gate-lanes.toml``. That file is the
one definition of what the gate checks; ``scripts/check-gate-parity.py`` keeps
``.github/workflows/ci.yml`` honest against it.

Disposition of a lane that cannot run (issue #127, defect 1):

* platform mismatch            -> ``skip``  (out of scope for this host)
* missing need, ``required``   -> ``blocked`` (the gate refuses; never a pass)
* missing need, optional       -> ``skip``  (named, never counted as ``pass``)

A green exit is every applicable required lane at ``pass``. Skips are listed
and are not evidence for the surface that did not run them.

Usage::

    ./scripts/ci-local.sh              full gate
    ./scripts/ci-local.sh --fast       the lanes marked fast
    ./scripts/ci-local.sh --lane ID    one lane (what CI runs per step)
    ./scripts/ci-local.sh --list       print lane ids and surfaces
    ./scripts/ci-local.sh --receipt    also write evidence/ci-local-*.md
    ./scripts/ci-local.sh --jobs 1     one lane at a time, output streamed

How a multi-lane run is scheduled, and why (the gate must finish in under
five minutes of wall time; measured 2026-09-30 it took 17 min 24 s):

* Lanes that share a feature set carry the same ``group`` in the manifest.
  Groups run concurrently, lanes within a group in order. Each group builds in
  its own target directory (``<target>/gate-<group>``), because cargo holds one
  lock per target directory for a whole build, and one shared directory
  serialized ~440 s of compiles no matter how many lanes ran at once.
* Every lane's ``TMPDIR`` is a RAM-backed volume (an APFS RAM disk on macOS,
  ``/dev/shm`` on Linux). The simulation suites write a real file journal and
  ``sync_all`` it; on a physical disk those syncs queue behind each other
  (macOS issues ``F_FULLFSYNC``), and the ``sim_scale`` binary took 104 s on
  disk against 21.6 s on a RAM disk, same 164 tests, same syscalls.
* A lane's output goes to ``<target>/gate-logs/<lane>.log``; a lane that fails
  prints the tail of its log and the path.

Exit codes: 0 every applicable lane passed; 1 a lane failed or was blocked;
2 usage or manifest error.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import contextlib
import hashlib
import importlib
import os
import platform
import shutil
import signal
import sys
import threading
import time
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

# The builtin checkers and the process helpers live beside this file (#408,
# split under the size law); a run from any working directory still finds them.
sys.path.insert(0, str(Path(__file__).resolve().parent))
from ci_builtins import BUILTINS  # noqa: E402
from ci_process import (  # noqa: E402
    _STOPPING,
    _capture,
    _child_env,
    _git,
    _run_command,
    _stamp,
    _stop_all_children,
    gate_scratch,
)

MANIFEST_REL = "scripts/gate-lanes.toml"

PASS, FAIL, BLOCKED, SKIP = "pass", "fail", "blocked", "skip"


# ── manifest ────────────────────────────────────────────────────────────────


@dataclass(frozen=True)
class Lane:
    id: str
    surfaces: tuple[str, ...]
    required: bool
    command: str | None = None
    builtin: str | None = None
    ci_step: str | None = None
    ci_job: str | None = None
    needs: tuple[str, ...] = ()
    platforms: tuple[str, ...] = ()
    fast: bool = False
    reason: str = ""
    group: str | None = None


@dataclass
class LaneResult:
    lane_id: str
    status: str
    started: str
    ended: str
    detail: str = ""
    exit_code: int | None = None
    seconds: float = 0.0


def _as_tuple(value: object, where: str) -> tuple[str, ...]:
    """Accept a whitespace-separated string or a list of strings."""
    if value is None:
        return ()
    if isinstance(value, str):
        return tuple(value.split())
    if not isinstance(value, list) or not all(isinstance(v, str) for v in value):
        raise ValueError(f"{where}: expected a string or list of strings")
    return tuple(value)


def load_lanes(root: Path) -> tuple[dict, list[Lane]]:
    """Return the manifest table and its lanes, rejecting a malformed lane."""
    path = root / MANIFEST_REL
    with path.open("rb") as handle:
        table = tomllib.load(handle)
    lanes: list[Lane] = []
    seen: set[str] = set()
    for index, raw in enumerate(table.get("lane", [])):
        where = f"{MANIFEST_REL} lane[{index}]"
        lane_id = raw.get("id")
        if not isinstance(lane_id, str) or not lane_id:
            raise ValueError(f"{where}: missing id")
        if lane_id in seen:
            raise ValueError(f"{where}: duplicate id {lane_id!r}")
        seen.add(lane_id)
        surfaces = _as_tuple(raw.get("surfaces"), f"{where} surfaces")
        if not surfaces or not set(surfaces) <= {"local", "ci"}:
            raise ValueError(f"{where} surfaces: expected local/ci, got {raw.get('surfaces')!r}")
        command = raw.get("command")
        builtin = raw.get("builtin")
        if (command is None) == (builtin is None):
            raise ValueError(f"{where}: exactly one of command/builtin")
        if "ci" in surfaces and not (raw.get("ci_step") or raw.get("ci_job")):
            raise ValueError(f"{where}: a ci lane needs ci_step or ci_job")
        lanes.append(
            Lane(
                id=lane_id,
                surfaces=surfaces,
                required=bool(raw.get("required", True)),
                command=command,
                builtin=builtin,
                ci_step=raw.get("ci_step"),
                ci_job=raw.get("ci_job"),
                needs=_as_tuple(raw.get("needs"), f"{where} needs"),
                platforms=_as_tuple(raw.get("platforms"), f"{where} platforms"),
                fast=bool(raw.get("fast", False)),
                reason=str(raw.get("reason", "")),
                group=raw.get("group"),
            )
        )
    return table, lanes


# ── prerequisites ───────────────────────────────────────────────────────────


def check_need(need: str, root: Path) -> bool:
    """True when the named prerequisite is available under ``root``."""
    kind, _, name = need.partition(":")
    if kind == "module" and name:
        try:
            importlib.import_module(name)
        except Exception:  # noqa: BLE001 - an unimportable module is an absent one
            return False
        return True
    if kind == "script" and name:
        target = root / name
        return target.is_file() and os.access(target, os.X_OK)
    if kind == "bin" and name:
        return shutil.which(name) is not None
    if kind == "os" and name:
        return sys.platform == name
    raise ValueError(f"unknown need {need!r}")


def missing_needs(lane: Lane, root: Path) -> list[str]:
    return [need for need in lane.needs if not check_need(need, root)]


def platform_applies(lane: Lane) -> bool:
    return not lane.platforms or sys.platform in lane.platforms


# ── execution ───────────────────────────────────────────────────────────────


def run_lane(lane: Lane, root: Path, env: dict[str, str] | None = None, log: Path | None = None) -> LaneResult:
    started = _stamp()
    clock = time.monotonic()
    if not platform_applies(lane):
        return LaneResult(lane.id, SKIP, started, _stamp(), f"out of scope for {sys.platform}; declared for {list(lane.platforms)}")
    missing = missing_needs(lane, root)
    if missing:
        status = BLOCKED if lane.required else SKIP
        detail = f"missing prerequisite(s): {', '.join(missing)}"
        if lane.reason:
            detail += f" ({lane.reason})"
        return LaneResult(lane.id, status, started, _stamp(), detail)

    env = env if env is not None else _child_env(root)
    with contextlib.ExitStack() as stack:
        out = None
        if log is not None:
            log.parent.mkdir(parents=True, exist_ok=True)
            out = stack.enter_context(log.open("w", encoding="utf-8"))
        if lane.builtin is not None:
            fn = BUILTINS.get(lane.builtin)
            if fn is None:
                return LaneResult(lane.id, FAIL, started, _stamp(), f"unknown builtin {lane.builtin!r}", 2)
            try:
                code, detail = fn(root, env, out)
            except Exception as error:  # noqa: BLE001 - a checker crash is a failed lane
                code, detail = 1, f"builtin {lane.builtin} raised {type(error).__name__}: {error}"
            if out is not None:
                out.write(detail + "\n")
        else:
            code = _run_command(lane.command or "", root, env, out)
            detail = "" if code == 0 else f"exit {code}"

    status = PASS if code == 0 else FAIL
    return LaneResult(lane.id, status, started, _stamp(), detail, code, time.monotonic() - clock)


def _log_tail(log: Path, lines: int = 150) -> str:
    try:
        text = log.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError:
        return ""
    return "\n".join(text[-lines:])


def _target_base(root: Path) -> Path:
    configured = os.environ.get("CARGO_TARGET_DIR")
    base = Path(configured) if configured else root / "target"
    return base if base.is_absolute() else root / base


def _report(result: LaneResult, log: Path | None, lock: threading.Lock) -> None:
    line = f"<== {result.lane_id}: {result.status} ({result.seconds:.1f} s)"
    if result.detail:
        line += f" — {result.detail.splitlines()[0]}"
    bad = result.status in (FAIL, BLOCKED)
    with lock:
        print(line, file=sys.stderr if bad else sys.stdout, flush=True)
        if bad and log is not None and log.exists():
            print(f"---- last lines of {log} ----", file=sys.stderr)
            print(_log_tail(log), file=sys.stderr, flush=True)


def run_parallel(selected: list[Lane], root: Path, scratch: Path | None, jobs: int) -> list[LaneResult]:
    """Run lane groups concurrently; lanes inside one group run in order."""
    groups: dict[str, list[Lane]] = {}
    for lane in selected:
        key = f"gate-{lane.group}" if lane.group else f"lane-{lane.id}"
        groups.setdefault(key, []).append(lane)
    # Cargo groups first: they are the long poles, so they must not queue
    # behind one-second checkers when `jobs` is below the group count.
    ordered = sorted(groups.items(), key=lambda item: not item[0].startswith("gate-"))
    base = _target_base(root)
    logs = base / "gate-logs"
    lock = threading.Lock()
    results: dict[str, LaneResult] = {}

    def run_group(key: str, lanes: list[Lane]) -> None:
        target = base / key if key.startswith("gate-") else None
        env = _child_env(root, scratch, target)
        for lane in lanes:
            if _STOPPING.is_set():
                return
            with lock:
                print(f"==> {lane.id}" + (f"  [{key}]" if target else ""), flush=True)
            log = logs / f"{lane.id}.log"
            result = run_lane(lane, root, env, log)
            results[lane.id] = result
            _report(result, log, lock)

    with concurrent.futures.ThreadPoolExecutor(max_workers=max(1, min(jobs, len(ordered)))) as pool:
        futures = [pool.submit(run_group, key, lanes) for key, lanes in ordered]
        try:
            for future in concurrent.futures.as_completed(futures):
                future.result()
        except BaseException:
            # Stop the children before the pool's exit waits on the threads
            # that are waiting on them.
            _stop_all_children()
            raise
    return [results[lane.id] for lane in selected if lane.id in results]


# ── receipt ─────────────────────────────────────────────────────────────────


@dataclass
class TreeIdentity:
    commit: str
    tree: str
    worktree_digest: str
    dirty: list[str] = field(default_factory=list)
    untracked: list[str] = field(default_factory=list)

    @property
    def verified(self) -> bool:
        return not self.dirty and not self.untracked and self.commit != "unknown"


def tree_identity(root: Path) -> TreeIdentity:
    commit = _git(root, ["rev-parse", "HEAD"]) or "unknown"
    commit = commit.strip() or "unknown"
    tree = _git(root, ["rev-parse", "HEAD^{tree}"]) or "unknown"
    tree = tree.strip() or "unknown"
    listed = _git(root, ["ls-files", "-s"]) or ""
    digest = hashlib.sha256(listed.encode("utf-8", "replace")).hexdigest()[:16]
    status = _git(root, ["status", "--porcelain", "-uall", "-z"]) or ""
    dirty: list[str] = []
    untracked: list[str] = []
    for record in status.split("\0"):
        if not record:
            continue
        code, _, name = record.partition(" ")
        name = name.strip()
        if code.startswith("??") or code.strip() == "??":
            untracked.append(name)
        else:
            dirty.append(f"{code.strip()} {name}".strip())
    return TreeIdentity(commit, tree, digest, dirty, untracked)


def build_receipt(
    root: Path,
    table: dict,
    lanes: list[Lane],
    results: list[LaneResult],
    identity: TreeIdentity,
    mode: str,
) -> str:
    by_id = {result.lane_id: result for result in results}
    covered = [lane for lane in lanes if "local" in lane.surfaces]
    uncovered = [lane for lane in lanes if "local" not in lane.surfaces]
    toolchain = table.get("toolchain", {}).get("rust", "unspecified")

    lines = [
        "# Local gate receipt",
        "",
        f"Recorded: {_stamp()}",
        f"Mode: {mode}",
        f"Host: {platform.system()} {platform.machine()}",
        f"rustc: {shutil.which('rustc') and _capture(['rustc', '--version'], root) or 'missing'}",
        f"Toolchain pin: rust {toolchain}",
        "",
        "## Input identity",
        "",
        f"- commit: `{identity.commit}`",
        f"- tree: `{identity.tree}`",
        f"- index digest: `{identity.worktree_digest}`",
    ]
    if identity.verified:
        lines.append("- identity: **verified** (clean index, no untracked files)")
    else:
        lines.append(
            f"- identity: **UNVERIFIED** ({len(identity.dirty)} modified/staged, "
            f"{len(identity.untracked)} untracked)"
        )
        for entry in identity.dirty:
            lines.append(f"  - changed: `{entry}`")
        for entry in identity.untracked:
            lines.append(f"  - untracked: `{entry}`")
    lines += [
        "",
        "This receipt describes the working tree above, not a commit, unless",
        "identity is verified. A lane that did not run on this surface is not",
        "evidence for it.",
        "",
        "## Lanes run on this surface",
        "",
        "| Lane | Result | Started | Ended | Detail |",
        "|---|---|---|---|---|",
    ]
    for lane in covered:
        result = by_id.get(lane.id)
        if result is None:
            continue
        detail = result.detail.replace("|", "\\|").replace("\n", " ")[:200]
        lines.append(f"| {lane.id} | **{result.status}** | {result.started} | {result.ended} | {detail} |")

    lines += ["", "## Not covered by this surface", ""]
    if uncovered:
        for lane in uncovered:
            reason = lane.reason or f"declared surfaces: {', '.join(lane.surfaces)}"
            lines.append(f"- `{lane.id}` — {reason}")
    else:
        lines.append("- none")

    counts = {status: 0 for status in (PASS, FAIL, BLOCKED, SKIP)}
    for result in results:
        counts[result.status] = counts.get(result.status, 0) + 1
    lines += [
        "",
        f"passed: {counts[PASS]}   failed: {counts[FAIL]}   "
        f"blocked: {counts[BLOCKED]}   skipped: {counts[SKIP]}",
        "",
    ]
    return "\n".join(lines)


# ── entry point ─────────────────────────────────────────────────────────────


def _print_table(results: list[LaneResult]) -> None:
    print()
    print("─" * 60)
    print(f"{'lane':<22} {'result':<8} {'secs':>7}  detail")
    for result in results:
        detail = result.detail.replace("\n", " ")[:60]
        print(f"{result.lane_id:<22} {result.status:<8} {result.seconds:>7.1f}  {detail}")
    counts = {status: 0 for status in (PASS, FAIL, BLOCKED, SKIP)}
    for result in results:
        counts[result.status] = counts.get(result.status, 0) + 1
    print()
    print(
        f"passed: {counts[PASS]}   failed: {counts[FAIL]}   "
        f"blocked: {counts[BLOCKED]}   skipped: {counts[SKIP]}"
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Run the local gate from scripts/gate-lanes.toml")
    parser.add_argument("--root", type=Path, default=None, help="repository root (default: parent of scripts/)")
    parser.add_argument("--lane", action="append", default=[], help="run only this lane id (repeatable)")
    parser.add_argument("--fast", action="store_true", help="run only the lanes marked fast")
    parser.add_argument("--list", action="store_true", help="print lane ids and exit")
    parser.add_argument("--receipt", action="store_true", help="write evidence/ci-local-<commit>.md")
    parser.add_argument(
        "--jobs",
        type=int,
        default=0,
        help="lane groups run at once (default: all; 1 runs lanes in order with output streamed)",
    )
    args = parser.parse_args(argv)

    root = (args.root or Path(__file__).resolve().parents[1]).resolve()
    try:
        table, lanes = load_lanes(root)
    except (OSError, ValueError, tomllib.TOMLDecodeError) as error:
        print(f"ci-local: cannot load {MANIFEST_REL}: {error}", file=sys.stderr)
        return 2

    if args.list:
        for lane in lanes:
            print(f"{lane.id:<22} {' '.join(lane.surfaces):<9} {'required' if lane.required else 'optional'}")
        return 0

    if args.lane:
        wanted = set(args.lane)
        unknown = wanted - {lane.id for lane in lanes}
        if unknown:
            print(f"ci-local: unknown lane id(s): {', '.join(sorted(unknown))}", file=sys.stderr)
            return 2
        selected = [lane for lane in lanes if lane.id in wanted]
    elif args.fast:
        selected = [lane for lane in lanes if lane.fast and "local" in lane.surfaces]
    else:
        selected = [lane for lane in lanes if "local" in lane.surfaces]

    if not selected:
        print("ci-local: no lanes selected", file=sys.stderr)
        return 2

    identity = tree_identity(root)
    print("logicalworks-crates local gate")
    print(f"  root:    {root}")
    print(f"  commit:  {identity.commit}")
    if identity.verified:
        print("  tree:    clean")
    else:
        print(f"  tree:    UNVERIFIED ({len(identity.dirty)} changed, {len(identity.untracked)} untracked)")
    print(f"  host:    {platform.system()} {platform.machine()}")
    print(f"  rustc:   {_capture(['rustc', '--version'], root)}")
    print(f"  mode:    {'fast' if args.fast else ('lane=' + ','.join(args.lane) if args.lane else 'full')}")
    print(f"  started: {_stamp()}")

    # SIGTERM runs the same cleanup as Ctrl-C: children stopped, scratch detached.
    def terminate(signum, _frame):
        raise KeyboardInterrupt(f"signal {signum}")

    with contextlib.suppress(ValueError):
        signal.signal(signal.SIGTERM, terminate)

    parallel = len(selected) > 1 and args.jobs != 1
    wall = time.monotonic()
    results: list[LaneResult] = []
    interrupted = False
    with gate_scratch(any(lane.group for lane in selected)) as scratch:
        try:
            print(f"  scratch: {scratch or 'system temporary directory'}")
            print(f"  mode:    {'parallel groups' if parallel else 'one lane at a time'}", flush=True)
            if parallel:
                results = run_parallel(selected, root, scratch, args.jobs or len(selected))
            else:
                lock = threading.Lock()
                env = _child_env(root, scratch)
                for lane in selected:
                    print(f"\n==> {lane.id}", flush=True)
                    result = run_lane(lane, root, env)
                    results.append(result)
                    _report(result, None, lock)
        except KeyboardInterrupt:
            interrupted = True
        finally:
            # Children first, then the scratch volume they write into.
            _stop_all_children()
    if interrupted:
        print("ci-local: interrupted; every child process was stopped", file=sys.stderr)
        return 130

    _print_table(results)
    print(f"wall: {time.monotonic() - wall:.1f} s")

    if args.receipt:
        evidence = root / "evidence"
        evidence.mkdir(exist_ok=True)
        stem = identity.commit[:12] if identity.commit != "unknown" else "unknown"
        path = evidence / f"ci-local-{stem}.md"
        path.write_text(build_receipt(root, table, lanes, results, identity, "fast" if args.fast else "full"), encoding="utf-8")
        print(f"receipt: {path}")

    bad = [r for r in results if r.status in (FAIL, BLOCKED)]
    if bad:
        print("\nrefused lanes:", file=sys.stderr)
        for result in bad:
            print(f"  - {result.lane_id}: {result.status}" + (f" — {result.detail.splitlines()[0]}" if result.detail else ""), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
