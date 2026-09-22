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

Exit codes: 0 every applicable lane passed; 1 a lane failed or was blocked;
2 usage or manifest error.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib
import os
import platform
import re
import shutil
import subprocess
import sys
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


@dataclass
class LaneResult:
    lane_id: str
    status: str
    started: str
    ended: str
    detail: str = ""
    exit_code: int | None = None


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


def builtin_unwrap_scan(root: Path) -> tuple[int, str]:
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


def builtin_suppressions(root: Path) -> tuple[int, str]:
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


def builtin_artifacts(root: Path) -> tuple[int, str]:
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


def builtin_contract_drift(root: Path) -> tuple[int, str]:
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


def builtin_docsrs_metadata(root: Path) -> tuple[int, str]:
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

    env = os.environ.copy()
    env["RUSTDOCFLAGS"] = "-D warnings --cfg docsrs"
    log = [f"cargo doc plans: {len(commands)}"]
    for label, argv in commands:
        log.append(f"  {label}: {' '.join(argv)}")
        code = _run_argv(argv, root, env)
        if code != 0:
            return 1, "\n".join(log + [f"failed (exit {code}): {' '.join(argv)}"])
    return 0, "\n".join(log)


def builtin_readme_quickstart(root: Path) -> tuple[int, str]:
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


BUILTINS = {
    "unwrap-scan": builtin_unwrap_scan,
    "suppressions": builtin_suppressions,
    "artifacts": builtin_artifacts,
    "contract-drift": builtin_contract_drift,
    "docsrs-metadata": builtin_docsrs_metadata,
    "readme-quickstart": builtin_readme_quickstart,
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


def _run_argv(argv: list[str], root: Path, env: dict[str, str]) -> int:
    try:
        completed = subprocess.run(argv, cwd=root, env=env, timeout=3600, check=False)
    except (OSError, subprocess.TimeoutExpired):
        return 125
    return completed.returncode


def _run_command(command: str, root: Path, env: dict[str, str]) -> int:
    try:
        completed = subprocess.run(
            ["bash", "-c", command],
            cwd=root,
            env=env,
            timeout=3600,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return 125
    return completed.returncode


def _stamp() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def _child_env(root: Path) -> dict[str, str]:
    env = os.environ.copy()
    cargo = Path.home() / ".cargo" / "bin"
    env["PATH"] = f"{cargo}{os.pathsep}{env.get('PATH', '')}"
    env["CARGO_INCREMENTAL"] = "0"
    env["CARGO_TERM_COLOR"] = "never"
    env.setdefault("RUST_BACKTRACE", "1")
    return env


# ── execution ───────────────────────────────────────────────────────────────


def run_lane(lane: Lane, root: Path) -> LaneResult:
    started = _stamp()
    if not platform_applies(lane):
        return LaneResult(lane.id, SKIP, started, _stamp(), f"out of scope for {sys.platform}; declared for {list(lane.platforms)}")
    missing = missing_needs(lane, root)
    if missing:
        status = BLOCKED if lane.required else SKIP
        detail = f"missing prerequisite(s): {', '.join(missing)}"
        if lane.reason:
            detail += f" ({lane.reason})"
        return LaneResult(lane.id, status, started, _stamp(), detail)

    env = _child_env(root)
    if lane.builtin is not None:
        fn = BUILTINS.get(lane.builtin)
        if fn is None:
            return LaneResult(lane.id, FAIL, started, _stamp(), f"unknown builtin {lane.builtin!r}", 2)
        try:
            code, detail = fn(root)
        except Exception as error:  # noqa: BLE001 - a checker crash is a failed lane
            code, detail = 1, f"builtin {lane.builtin} raised {type(error).__name__}: {error}"
    else:
        code = _run_command(lane.command or "", root, env)
        detail = "" if code == 0 else f"exit {code}"

    status = PASS if code == 0 else FAIL
    return LaneResult(lane.id, status, started, _stamp(), detail, code)


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
    print(f"{'lane':<22} {'result':<8} detail")
    for result in results:
        detail = result.detail.replace("\n", " ")[:60]
        print(f"{result.lane_id:<22} {result.status:<8} {detail}")
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

    results: list[LaneResult] = []
    for lane in selected:
        print(f"\n==> {lane.id}", flush=True)
        result = run_lane(lane, root)
        results.append(result)
        stream = sys.stderr if result.status in (FAIL, BLOCKED) else sys.stdout
        print(f"<== {lane.id}: {result.status}" + (f" — {result.detail.splitlines()[0]}" if result.detail else ""), file=stream, flush=True)

    _print_table(results)

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
