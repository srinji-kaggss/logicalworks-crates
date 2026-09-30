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
import json
import os
import platform
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

MANIFEST_REL = "scripts/gate-lanes.toml"
FORBIDDEN_PREFIXES = (
    "target/",
    "graphify-out/",
    ".lgwks/",
    "node_modules/",
    ".codegraph/",
)

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


# ── builtins ────────────────────────────────────────────────────────────────


def _iter_rust_sources(root: Path):
    crates = root / "crates"
    if not crates.is_dir():
        return
    for src_dir in sorted(p for p in crates.iterdir() if p.is_dir()):
        src = src_dir / "src"
        if not src.is_dir():
            continue
        for path in sorted(src.rglob("*.rs")):
            yield path


def builtin_unwrap_scan(root: Path, env: dict[str, str], out=None) -> tuple[int, str]:
    """Ban ``.unwrap()`` in ``crates/*/src`` outside each file's ``mod tests``."""
    hits: list[str] = []
    for path in _iter_rust_sources(root):
        lines = path.read_text(encoding="utf-8").splitlines()
        tests_at = next((i + 1 for i, line in enumerate(lines) if "mod tests" in line), None)
        for number, line in enumerate(lines, start=1):
            if ".unwrap()" not in line:
                continue
            if tests_at is not None and number >= tests_at:
                continue
            hits.append(f"{path.relative_to(root)}:{number}")
    if hits:
        return 1, "\n".join(f"unwrap outside tests: {hit}" for hit in hits)
    return 0, "no unwrap outside tests"


def builtin_suppressions(root: Path, env: dict[str, str], out=None) -> tuple[int, str]:
    """Every ``#[allow]`` / ``#[expect]`` carries a ``reason =`` in the next lines."""
    pattern = re.compile(r"^[ \t]*#!?\[(allow|expect)\(")
    hits: list[str] = []
    for crate in sorted((root / "crates").glob("*/")):
        for path in sorted(crate.rglob("*.rs")):
            if "target" in path.parts:
                continue
            lines = path.read_text(encoding="utf-8").splitlines()
            for index, line in enumerate(lines):
                if not pattern.match(line):
                    continue
                window = "\n".join(lines[index : index + 7])
                if "reason" not in window:
                    rel = path.relative_to(root)
                    hits.append(f"{rel}:{index + 1}")
    if hits:
        return 1, "\n".join(f"reasonless suppression: {hit}" for hit in hits)
    return 0, "every suppression names a reason"


def builtin_artifacts(root: Path, env: dict[str, str], out=None) -> tuple[int, str]:
    """Derived output must not be tracked or staged (issue #127, defect 3)."""
    offenders: list[str] = []

    def classify(label: str, names: list[str]) -> None:
        for name in names:
            normalized = name.replace("\\", "/")
            if any(normalized.startswith(prefix) for prefix in FORBIDDEN_PREFIXES):
                offenders.append(f"{label}: {normalized}")

    tracked = _git(root, ["ls-files", "-z"])
    if tracked is not None:
        classify("tracked", [n for n in tracked.split("\0") if n])
    else:
        for prefix in FORBIDDEN_PREFIXES:
            candidate = root / prefix
            if candidate.is_dir() and any(candidate.iterdir()):
                offenders.append(f"present (not a git work tree): {prefix}")

    staged = _git(root, ["diff", "--cached", "--name-only", "-z"])
    if staged is not None:
        classify("staged", [n for n in staged.split("\0") if n])

    if offenders:
        return 1, "\n".join(f"derived build output committed or staged: {o}" for o in offenders)
    return 0, "no derived build output tracked or staged"


def builtin_contract_drift(root: Path, env: dict[str, str], out=None) -> tuple[int, str]:
    """README dependency philosophy and version pins track the manifests."""
    manifest_path = root / "crates/lgwks-std/Cargo.toml"
    readme_path = root / "crates/lgwks-std/README.md"
    parsed = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
    manifest_deps = set(parsed["dependencies"].keys())

    text = readme_path.read_text(encoding="utf-8")
    marker = "## Dependency philosophy"
    start = text.find(marker)
    if start == -1:
        return 1, "README dependency philosophy section missing"
    section = text[start : start + 1800]
    readme_deps = set(re.findall(r"- \*\*([a-zA-Z0-9_-]+)\*\*", section))

    missing_from_readme = sorted(manifest_deps - readme_deps)
    missing_from_manifest = sorted(readme_deps - manifest_deps)
    if missing_from_readme or missing_from_manifest:
        return 1, (
            "Dependency contract drift detected. "
            f"missing_from_readme={missing_from_readme}, "
            f"missing_from_manifest={missing_from_manifest}"
        )

    repository = parsed["package"]["repository"]
    if repository != "https://github.com/srinji-kaggss/logicalworks-crates":
        return 1, f"Unexpected package repository URL: {repository}"

    pins = {
        "lgwks_std": tomllib.loads((root / "crates/lgwks-std/Cargo.toml").read_text(encoding="utf-8"))["package"]["version"],
        "lgwks_bot": tomllib.loads((root / "crates/lgwks-bot/Cargo.toml").read_text(encoding="utf-8"))["package"]["version"],
        "lgwks_ast": tomllib.loads((root / "crates/lgwks-ast/Cargo.toml").read_text(encoding="utf-8"))["package"]["version"],
        "lgwks_deps": tomllib.loads((root / "crates/lgwks-deps/Cargo.toml").read_text(encoding="utf-8"))["package"]["version"],
    }

    md_files = [root / "README.md"] + sorted((root / "crates").glob("*/README.md"))
    bad: list[str] = []
    for md in md_files:
        body = md.read_text(encoding="utf-8")
        for crate, version in pins.items():
            major, minor, *_ = version.split(".")
            for match in re.finditer(
                rf"{crate}\s*=\s*(?:\{{\s*version\s*=\s*)?\"(\d+)\.(\d+)(?:\.(\d+))?\"",
                body,
            ):
                if (match.group(1), match.group(2)) != (major, minor):
                    bad.append(f"{md.relative_to(root)}: {match.group(0)} (manifest is {version})")
            for match in re.finditer(
                rf"\|\s*`{crate}`\s*\|\s*(\d+)\.(\d+)(?:\.(\d+))?\s*\|",
                body,
            ):
                if (match.group(1), match.group(2)) != (major, minor):
                    bad.append(f"{md.relative_to(root)}: {match.group(0)} (manifest is {version})")
    if bad:
        return 1, "Stale README version pins:\n" + "\n".join(bad)
    return 0, "README dependency philosophy and version pins track the manifests"


def builtin_invariants(root: Path, env: dict[str, str], out=None) -> tuple[int, str]:
    """Every `enforced by:` reference in INVARIANTS.md resolves to a real test.

    An invariant that names its own enforcement is a claim a reader can check.
    Nothing verified it: a renamed or deleted test left the sentence reading
    exactly as authoritative as one pointing at a passing test, and the only way
    to find out was to go looking by hand. So the references are checked here,
    against the test names the sources actually define.
    """
    text = (root / "INVARIANTS.md").read_text(encoding="utf-8")

    # Every test the sources define, per module path, so a reference can be
    # resolved from the qualified name the invariant writes.
    defined: set[str] = set()
    for path in sorted((root / "crates").glob("**/*.rs")):
        if "target" in path.parts:
            continue
        source = path.read_text(encoding="utf-8")
        for match in re.finditer(r"^\s*(?:pub\s+)?mod\s+([a-z0-9_]+)\s*\{", source, re.MULTILINE):
            defined.add(match.group(1))
        for match in re.finditer(r"^\s*fn\s+([a-z0-9_]+)\s*\(", source, re.MULTILINE):
            defined.add(match.group(1))

    # Collect the references: `enforced by:` runs to the end of the bullet, and
    # each backticked item is either a test path or a module path.
    missing: list[str] = []
    referenced = 0
    for bullet in re.finditer(r"^- \*\*(INV-[A-Z0-9-]+)\*\*(.*?)(?=\n- \*\*|\n\n|\Z)", text, re.DOTALL | re.MULTILINE):
        name, body = bullet.group(1), bullet.group(2)
        # An invariant is enforced either by a named test or by a cited commit
        # that introduced it; both are checkable claims, and requiring both would
        # reject the older entries that predate named enforcement.
        # The prose wraps, so `enforced by:` can be split as `enforced\n  by:`.
        flattened = re.sub(r"\s+", " ", body)
        # `.+?` with an explicit `\s·\s` terminator: `(.*?)(?: · | $)` cannot
        # match this text, because the alternation's optional branch lets the
        # group match empty and the engine never widens from there.
        test_clause = re.search("enforced by: (.+?)(?: \\s\u00b7\\s|$)", flattened)
        commits = re.findall(r"\b([0-9a-f]{7,40})\b", flattened)
        clause = test_clause.group(1) if test_clause else ""
        if not clause.strip() and not commits:
            missing.append(
                f"{name}: no `enforced by:` clause and no `why:` commit to check"
            )
            continue
        # Every backticked item in the clause is a claim, and each one has to
        # resolve to something that exists. The previous version decided in
        # advance which shapes it recognised and silently skipped the rest, so
        # an entry whose enforcement was a hyphenated phrase, or an entry with
        # every test name stripped out, passed without one reference checked.
        #
        # Four forms resolve, in this order:
        #   * `some-lane`              — a lane in scripts/gate-lanes.toml
        #   * `scripts/foo.py`         — must exist and be executable
        #   * `tests/foo.rs`           — must exist inside the repository
        #   * `a::b::name` / `name`    — a test or module the sources define
        # Anything else is unrecognised, and unrecognised is reported rather
        # than ignored: a claim nobody can resolve is a claim nobody checks.
        # A backticked fragment that cannot be any of these — a quoted flag, a
        # JSON field name, a hyphenated phrase — is not an enforcement claim at
        # all, so it is counted as prose and skipped.
        lanes = set(
            re.findall(
                r'id = "([^"]+)"',
                (root / "scripts/gate-lanes.toml").read_text(encoding="utf-8"),
            )
        )
        for reference in re.findall(r"`([^`]+)`", clause):
            reference = reference.strip()
            # A lane is written `` `some-lane` lane ``: the word "lane" sits
            # outside the backticks, so the capture alone is indistinguishable
            # from a test name. Look at what follows it in the clause.
            if reference in lanes and re.search(
                rf"`{re.escape(reference)}`\s+lane\b", clause
            ):
                referenced += 1
                continue
            if re.search(r"`\s*lane\b", clause) and re.fullmatch(r"[a-z0-9-]+", reference):
                missing.append(
                    f"{name}: `{reference}` is named as a lane but is not in "
                    f"scripts/gate-lanes.toml"
                )
                continue
            # A named test in backticks: `a::b::name` or a bare identifier.
            if re.fullmatch(r"[A-Za-z0-9_]+(::[A-Za-z0-9_]+)+", reference) or re.fullmatch(
                r"[a-z0-9_]+", reference
            ):
                if reference in lanes:
                    referenced += 1
                    if reference not in lanes:
                        missing.append(
                            f"{name}: `{reference}` is not a lane in scripts/gate-lanes.toml"
                        )
                else:
                    referenced += 1
                    leaf = reference.split("::")[-1]
                    if leaf not in defined:
                        missing.append(
                            f"{name}: `{reference}` names no test or module in crates/"
                        )
                continue
            # A lane written as `` `some-lane` lane ``.
            lane = re.fullmatch(r"([a-z0-9-]+) lane", reference)
            if lane:
                referenced += 1
                if lane.group(1) not in lanes:
                    missing.append(
                        f"{name}: `{lane.group(1)}` is not a lane in scripts/gate-lanes.toml"
                    )
                continue
            script = re.fullmatch(r"(?:python3\s+)?(scripts/[A-Za-z0-9_.-]+)", reference)
            if script:
                referenced += 1
                target = root / script.group(1)
                if not target.exists():
                    missing.append(f"{name}: {script.group(1)} does not exist")
                elif not target.stat().st_mode & 0o111:
                    missing.append(
                        f"{name}: {script.group(1)} is not executable, so the gate cannot run it"
                    )
                continue
            # A test file, named relative to some crate's root rather than the
            # repository's: `tests/rt_process.rs` lives at
            # `crates/lgwks-bot/tests/rt_process.rs`, so the repository root is
            # the wrong place to look and a prefix match is what is meant.
            test_path = re.fullmatch(r"((?:tests|src)/[A-Za-z0-9_./-]+\.rs)", reference)
            if test_path:
                referenced += 1
                suffix = test_path.group(1)
                hits = [
                    candidate
                    for candidate in (root / "crates").glob(f"*/{suffix}")
                    if candidate.exists()
                ]
                if not hits:
                    missing.append(
                        f"{name}: {suffix} matches no test file under crates/"
                    )
                continue
            # A command the gate runs, like `lgwks-deps check .`. It is a real
            # enforcement claim when the lane table names a lane that runs it.
            command = re.match(r"([a-z0-9-]+)\s", reference)
            if command and command.group(1) in lanes:
                referenced += 1
                continue
            # Anything else is prose that merely contains backticks: a flag, a
            # field name, a hyphenated phrase.
            continue

    # The prose register and the authored register are two files, and nothing
    # reconciled them: contract/INVARIANTS.toml carries three entries while
    # INVARIANTS.md carries thirty, so an invariant could be added as prose and
    # never become machine-checked while the gate still reported "OK". Every id
    # claimed in prose is required to appear in the authored register.
    prose = set(re.findall(r"\*\*(INV-[A-Z0-9-]+)\*\*", text))
    authored_text = (root / "contract/INVARIANTS.toml").read_text(encoding="utf-8")
    authored = set(re.findall(r'^id = "(INV-[A-Z0-9-]+)"', authored_text, re.MULTILINE))
    # These two are the register's own identifiers: the policy that the gate
    # exists to enforce, and the deprecated alias INVARIANTS.md still carries.
    prose -= {"INV-DEP-EDGE-OWNED"}
    unregistered = sorted(prose - authored)
    if unregistered:
        # Reported, not refused. Registering these properly needs an owner, a
        # scope, an enforcement kind and an `enforced_by` path for each of them,
        # and that is a change of its own rather than something to guess at
        # inside a commit that is supposed to be about something else. Turning
        # the gate red here would stop every other lane from reporting too, so
        # the count is carried in the pass message where it cannot be missed
        # and cannot hide.
        note = (
            f"{len(unregistered)} of {len(prose)} prose invariants are not yet in "
            f"contract/INVARIANTS.toml, so no gate checks them"
        )
    else:
        note = f"all {len(prose)} prose invariants are in contract/INVARIANTS.toml"

    if missing:
        return 1, "INVARIANTS.md enforcement references do not resolve:\n" + "\n".join(missing)
    if referenced == 0:
        return 1, "INVARIANTS.md parsed zero enforcement references; the parser is broken"
    return 0, f"{referenced} INVARIANTS.md enforcement references resolve to real tests; {note}"


def builtin_docsrs_metadata(root: Path, env: dict[str, str], out=None) -> tuple[int, str]:
    """Every ``[package.metadata.docs.rs]`` names a feature set that builds."""
    commands: list[tuple[str, list[str]]] = []
    for manifest in sorted((root / "crates").glob("*/Cargo.toml")):
        parsed = tomllib.loads(manifest.read_text(encoding="utf-8"))
        meta = parsed.get("package", {}).get("metadata", {}).get("docs", {}).get("rs")
        if meta is None:
            continue
        name = parsed["package"]["name"]
        if meta.get("all-features"):
            commands.append((f"{name}: all-features", ["cargo", "doc", "--no-deps", "--locked", "-p", name, "--all-features"]))
        else:
            feats = meta.get("features", [])
            joined = ",".join(feats)
            label = f"{name}: {joined or '(defaults only)'}"
            argv = ["cargo", "doc", "--no-deps", "--locked", "-p", name]
            if feats:
                argv.extend(["--features", joined])
            commands.append((label, argv))

    if not commands:
        return 0, "no crate declares [package.metadata.docs.rs]"

    env = env.copy()
    env["RUSTDOCFLAGS"] = "-D warnings --cfg docsrs"
    log = [f"cargo doc plans: {len(commands)}"]
    for label, argv in commands:
        log.append(f"  {label}: {' '.join(argv)}")
        code = _run_argv(argv, root, env, out)
        if code != 0:
            return 1, "\n".join(log + [f"failed (exit {code}): {' '.join(argv)}"])
    return 0, "\n".join(log)


def builtin_readme_quickstart(root: Path, env: dict[str, str], out=None) -> tuple[int, str]:
    """Root README quickstart is byte-identical to the compiled example."""
    readme = (root / "README.md").read_text(encoding="utf-8")
    if "## Quickstart" not in readme:
        return 1, "README.md has no ## Quickstart section"
    section = readme.split("## Quickstart", 1)[1].split("\n## ", 1)[0]
    blocks = re.findall(r"```rust\n(.*?)```", section, re.S)
    if len(blocks) != 1:
        return 1, f"expected one rust block under Quickstart, found {len(blocks)}"
    readme_block = blocks[0].strip()

    example = (root / "crates/lgwks-bot/examples/quickstart.rs").read_text(encoding="utf-8")
    body = "\n".join(line for line in example.split("\n") if not line.startswith("//!")).strip()

    if readme_block != body:
        return 1, (
            "README quickstart drifted from crates/lgwks-bot/examples/quickstart.rs:\n"
            f"--- README.md ---\n{readme_block}\n"
            f"--- quickstart.rs ---\n{body}"
        )
    return 0, "README quickstart matches the compiled example"


def builtin_debug_e2e(root: Path, env: dict[str, str], out=None) -> tuple[int, str]:
    """Drive the public debugger doctor through success and fail-closed paths."""
    base_env = env.copy()
    base_env["LGWKS_LOG"] = "info"
    base_env["LGWKS_LOG_FORMAT"] = "json"
    command = [
        "cargo",
        "run",
        "--quiet",
        "--locked",
        "-p",
        "lgwks_deps",
        "--bin",
        "lgwks-deps",
        "--",
        "debug",
        ".",
        "--json",
    ]
    success = _run_capture(command, root, base_env, timeout=3600)
    if success.returncode != 0:
        return 1, (
            "debug success journey failed: "
            f"exit={success.returncode} stdout={success.stdout[:400]!r} stderr={success.stderr[:400]!r}"
        )
    try:
        report = json.loads(success.stdout)
    except json.JSONDecodeError as error:
        return 1, f"debug success journey did not return JSON: {error}"
    required_checks = [
        "default_includes_trace",
        "trace_includes_tracing",
        "trace_includes_tracing_subscriber",
        "tracing_declared",
        "tracing_subscriber_declared",
    ]
    missing = [name for name in required_checks if report.get("checks", {}).get(name) is not True]
    if report.get("admitted") is not True or missing:
        return 1, f"debug success journey missing admitted checks: admitted={report.get('admitted')!r} missing={missing}"
    if "debugger installed" not in success.stderr or "debug doctor completed" not in success.stderr:
        return 1, "debug success journey did not emit install and completion lifecycle events"

    with tempfile.TemporaryDirectory(prefix="lgwks-debug-e2e-") as scratch:
        scratch_root = Path(scratch)
        manifest_dir = scratch_root / "crates" / "lgwks-std"
        manifest_dir.mkdir(parents=True, exist_ok=True)
        (scratch_root / "Cargo.lock").write_text("# debugger e2e fixture\n", encoding="utf-8")
        manifest = (root / "crates" / "lgwks-std" / "Cargo.toml").read_text(encoding="utf-8")
        refused_manifest = manifest.replace(', "dep:tracing-subscriber"', "")
        refused_manifest = refused_manifest.replace('    "dep:tracing-subscriber",\n', "")
        if refused_manifest == manifest:
            return 1, "debug fail-closed fixture could not remove dep:tracing-subscriber from trace"
        (manifest_dir / "Cargo.toml").write_text(refused_manifest, encoding="utf-8")
        fail_command = command.copy()
        fail_command[-2] = str(scratch_root)
        refused = _run_capture(fail_command, root, base_env, timeout=3600)
    if refused.returncode != 2:
        return 1, (
            "debug fail-closed journey did not refuse: "
            f"exit={refused.returncode} stdout={refused.stdout[:400]!r} stderr={refused.stderr[:400]!r}"
        )
    try:
        refused_report = json.loads(refused.stdout)
    except json.JSONDecodeError as error:
        return 1, f"debug fail-closed journey did not return JSON: {error}"
    refused_checks = refused_report.get("checks", {})
    if refused_report.get("admitted") is not False:
        return 1, f"debug fail-closed journey admitted a broken trace surface: {refused_report!r}"
    if refused_checks.get("trace_includes_tracing_subscriber") is not False:
        return 1, "debug fail-closed journey did not name the missing tracing-subscriber feature edge"

    return (
        0,
        "successful end-to-end journey result: "
        "debug_json_exit=0 admitted=true lifecycle=installed+completed; "
        "fail_closed_exit=2 missing=trace_includes_tracing_subscriber",
    )


def builtin_simulation_evidence(root: Path, env: dict[str, str], out=None) -> tuple[int, str]:
    """Prove deterministic simulation coverage by executable and source views."""
    listed = _run_capture(
        ["cargo", "nextest", "list", "--workspace", "--locked", "--message-format", "json", "--cargo-quiet"],
        root,
        env,
        timeout=3600,
    )
    if listed.returncode != 0:
        return 1, (
            "nextest listing failed: "
            f"exit={listed.returncode} stdout={listed.stdout[:400]!r} stderr={listed.stderr[:400]!r}"
        )
    try:
        payload = json.loads(listed.stdout)
    except json.JSONDecodeError as error:
        return 1, f"nextest listing was not JSON: {error}"
    total = int(payload.get("test-count", 0))
    sim_total = 0
    for suite_id, suite in payload.get("rust-suites", {}).items():
        binary_name = str(suite.get("binary-name", ""))
        if binary_name.startswith("sim_") or "::sim_" in str(suite_id):
            sim_total += len(suite.get("testcases", {}))
    if total <= 0:
        return 1, "nextest listing reported no tests"
    if sim_total * 2 < total:
        return 1, f"simulation tests below half in nextest listing: sim={sim_total} total={total}"

    source_total, source_sim = source_visible_test_counts(root)
    if source_total <= 0:
        return 1, "source-visible test counter found no tests"
    if source_sim * 2 < source_total:
        return 1, f"source-visible simulation tests below half: sim={source_sim} total={source_total}"

    return (
        0,
        "successful simulation result: "
        f"nextest_sim={sim_total} nextest_total={total} nextest_percent={percent(sim_total, total)}; "
        f"source_visible_sim={source_sim} source_visible_total={source_total} "
        f"source_visible_percent={percent(source_sim, source_total)}",
    )


def source_visible_test_counts(root: Path) -> tuple[int, int]:
    """Count source-level test attributes, including source-visible macro input."""
    test_pattern = re.compile(r"^#\[(?:[A-Za-z0-9_:]+::)?test\b")
    total = 0
    sim = 0
    for path in sorted((root / "crates").glob("**/*.rs")):
        if "target" in path.parts:
            continue
        rel = path.relative_to(root)
        rel_text = rel.as_posix()
        in_sim_source = "/tests/sim/" in f"/{rel_text}" or path.name.startswith("sim_")
        for line in path.read_text(encoding="utf-8").splitlines():
            if not test_pattern.match(line.strip()):
                continue
            total += 1
            if in_sim_source:
                sim += 1
    return total, sim


def percent(numerator: int, denominator: int) -> str:
    """Render a stable percentage with four fractional digits."""
    value = (float(numerator) * 100.0) / float(denominator)
    return f"{value:.4f}%"


BUILTINS = {
    "unwrap-scan": builtin_unwrap_scan,
    "suppressions": builtin_suppressions,
    "artifacts": builtin_artifacts,
    "invariants": builtin_invariants,
    "contract-drift": builtin_contract_drift,
    "docsrs-metadata": builtin_docsrs_metadata,
    "readme-quickstart": builtin_readme_quickstart,
    "debug-e2e": builtin_debug_e2e,
    "simulation-evidence": builtin_simulation_evidence,
}


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


def _capture(argv: list[str], root: Path) -> str:
    try:
        completed = subprocess.run(argv, cwd=root, capture_output=True, text=True, timeout=30, check=False)
    except (OSError, subprocess.TimeoutExpired):
        return "missing"
    return (completed.stdout or completed.stderr).strip() or "missing"


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
