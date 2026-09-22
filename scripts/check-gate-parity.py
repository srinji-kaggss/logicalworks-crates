#!/usr/bin/env python3
"""Assert scripts/gate-lanes.toml and .github/workflows/ci.yml describe one gate.

Issue #127 refused the claim "the script is the single definition of the gate"
while the workflow was free to invent its own commands. A second hand-copied
list drifts, and each side still looks green on its own terms. This checker
makes the drift loud.

Rules
  1. Lane-id bijection. Every lane that names a ``ci_step`` claims a step of
     that name in ci.yml, and every gate step in ci.yml (everything but the
     toolchain pin and the checkout) is claimed by exactly one lane. A lane
     may instead name a ``ci_job``, for a job whose ``run:`` steps are
     unnamed; that job must exist.
  2. Command agreement. For a shared lane (``surfaces = "local ci"``) that
     carries a ``command``, every ``&&``-separated part of that command is a
     segment of the step's run body (split on newlines and ``&&``/``||``).
     A shortened command is a different command and fails; the CI step may
     append a diagnostic after ``||`` without failing.
  3. Builtin agreement. A ``builtin`` lane that runs in CI must be invoked as
     ``python3 scripts/ci_local.py --lane <id>``, so the text of the check
     exists once.
  4. Toolchain agreement. The manifest's ``toolchain.rust`` is the version
     every ``Pin Rust`` step installs.

Exit 0 when all rules hold. This script's own unittest block is the regression
net: a required checker removed from one execution surface must fail rule 1.
"""

from __future__ import annotations

import argparse
import re
import sys
import tomllib
import unittest
from dataclasses import dataclass
from pathlib import Path

MANIFEST_REL = "scripts/gate-lanes.toml"
WORKFLOW_REL = ".github/workflows/ci.yml"
SKIP_STEP_PREFIXES = ("Pin Rust", "Checkout", "Install", "Set up", "Upload", "Download", "Cache")


@dataclass(frozen=True)
class Step:
    name: str
    run: str
    job: str


@dataclass(frozen=True)
class Job:
    name: str
    runs_on: str
    steps: tuple[Step, ...]


@dataclass(frozen=True)
class Lane:
    id: str
    surfaces: tuple[str, ...]
    command: str | None
    builtin: str | None
    ci_step: str | None
    ci_job: str | None


def _normalise(text: str) -> str:
    return re.sub(r"\s+", " ", text or "").strip()


def _command_parts(command: str) -> list[str]:
    return [_normalise(part) for part in re.split(r"\s*&&\s*", command) if _normalise(part)]


def _step_segments(run: str) -> list[str]:
    """Split a step body into comparable command segments.

    A YAML ``run: >`` block is already folded to spaces by the parser, so
    ``&&`` / ``||`` are the separators. A ``run: |`` block keeps newlines and
    each line is its own command. Splitting on both gives one segment per
    command, which is what rule 2 compares for equality: a shortened command
    is a different command, not a prefix of one.
    """
    segments: list[str] = []
    for line in (run or "").splitlines():
        for piece in re.split(r"\s*(?:&&|\|\|)\s*", line):
            normalised = _normalise(piece)
            if normalised:
                segments.append(normalised)
    return segments


def parse_workflow(text: str) -> list[Job]:
    """Read job and step names and run bodies out of an Actions workflow.

    Uses PyYAML when it is importable (it also rejects duplicate keys, which a
    regex cannot). Falls back to indentation-aware scanning so the parity rule
    itself is not hostage to a YAML install; rule 1 is still enforced.
    """
    try:
        import yaml  # noqa: PLC0415 - optional
    except ImportError:
        return _parse_workflow_regex(text)
    return _parse_workflow_yaml(text, yaml)


def _parse_workflow_yaml(text: str, yaml_mod) -> list[Job]:
    document = yaml_mod.safe_load(text)
    if not isinstance(document, dict) or not isinstance(document.get("jobs"), dict):
        raise ValueError("workflow must declare a jobs mapping")
    jobs: list[Job] = []
    for job_id, job in document["jobs"].items():
        if not isinstance(job, dict):
            raise ValueError(f"{job_id}: job must be a mapping")
        name = job.get("name", job_id)
        runs_on = job.get("runs-on", "")
        if isinstance(runs_on, list):
            runs_on = ", ".join(str(label) for label in runs_on)
        steps: list[Step] = []
        for index, step in enumerate(job.get("steps") or []):
            if not isinstance(step, dict):
                continue
            if "run" not in step:
                continue
            step_name = step.get("name") or f"{job_id}:run[{index}]"
            steps.append(Step(name=str(step_name), run=str(step.get("run") or ""), job=str(name)))
        jobs.append(Job(name=str(name), runs_on=str(runs_on), steps=tuple(steps)))
    return jobs


def _parse_workflow_regex(text: str) -> list[Job]:
    jobs: list[Job] = []
    current_job_id = None
    current_job_name = ""
    current_runs_on = ""
    current_steps: list[Step] = []
    in_steps = False
    pending_name = None
    run_lines: list[str] = []
    in_run = False
    run_index = 0

    def flush_run() -> None:
        nonlocal pending_name, run_lines, in_run, run_index
        if not in_run and not run_lines:
            return
        name = pending_name or f"{current_job_id}:run[{run_index}]"
        current_steps.append(Step(name=name, run="\n".join(run_lines), job=current_job_name or current_job_id or "?"))
        run_index += 1
        pending_name = None
        run_lines = []
        in_run = False

    def flush_job() -> None:
        nonlocal current_job_id, current_steps
        if current_job_id is None:
            return
        flush_run()
        jobs.append(
            Job(
                name=current_job_name or current_job_id,
                runs_on=current_runs_on,
                steps=tuple(current_steps),
            )
        )
        current_job_id = None
        current_steps = []

    for raw in text.splitlines():
        job_match = re.match(r"^  ([A-Za-z0-9_-]+):\s*$", raw)
        if job_match and not raw.startswith("    "):
            flush_job()
            current_job_id = job_match.group(1)
            current_job_name = ""
            current_runs_on = ""
            current_steps = []
            in_steps = False
            run_index = 0
            continue
        if current_job_id is None:
            continue
        name_match = re.match(r"^    name:\s*(.+)$", raw)
        if name_match:
            current_job_name = name_match.group(1).strip().strip("'\"")
            continue
        runs_match = re.match(r"^    runs-on:\s*(.+)$", raw)
        if runs_match:
            current_runs_on = runs_match.group(1).strip()
            continue
        if re.match(r"^    steps:\s*$", raw):
            in_steps = True
            continue
        if not in_steps:
            continue
        step_match = re.match(r"^      - name:\s*(.+)$", raw)
        if step_match:
            flush_run()
            pending_name = step_match.group(1).strip().strip("'\"")
            continue
        run_match = re.match(r"^        run:\s*(.*)$", raw)
        if run_match:
            flush_run()
            body = run_match.group(1)
            if body in ("", "|", "|-", ">", ">-", "|+", ">+"):
                in_run = True
                run_lines = []
            else:
                run_lines = [body]
                in_run = False
                flush_run()
            continue
        if in_run:
            if raw.startswith("          ") or raw.strip() == "":
                run_lines.append(raw[10:] if raw.startswith("          ") else "")
            else:
                flush_run()
    flush_job()
    return jobs


def load_lanes(text: str) -> tuple[dict, list[Lane]]:
    table = tomllib.loads(text)
    lanes: list[Lane] = []
    for raw in table.get("lane", []):
        surfaces = raw.get("surfaces", "local")
        if isinstance(surfaces, str):
            surfaces = tuple(surfaces.split())
        else:
            surfaces = tuple(surfaces)
        lanes.append(
            Lane(
                id=str(raw.get("id", "")),
                surfaces=surfaces,
                command=raw.get("command"),
                builtin=raw.get("builtin"),
                ci_step=raw.get("ci_step"),
                ci_job=raw.get("ci_job"),
            )
        )
    return table, lanes


def is_gate_step(step: Step) -> bool:
    return not any(step.name.startswith(prefix) for prefix in SKIP_STEP_PREFIXES)


def check_parity(manifest_text: str, workflow_text: str) -> list[str]:
    """Return a list of rule violations. Empty means the two agree."""
    problems: list[str] = []
    table, lanes = load_lanes(manifest_text)
    jobs = parse_workflow(workflow_text)

    steps_by_name = {step.name: step for job in jobs for step in job.steps}
    gate_steps = {step.name: step for job in jobs for step in job.steps if is_gate_step(step)}
    job_names = {job.name for job in jobs}

    claimed_steps: dict[str, str] = {}
    claimed_jobs: dict[str, str] = {}
    for lane in lanes:
        if "ci" not in lane.surfaces:
            continue
        if lane.ci_step is None and lane.ci_job is None:
            problems.append(f"{lane.id}: ci lane names neither ci_step nor ci_job")
            continue
        if lane.ci_step is not None:
            if lane.ci_step not in steps_by_name:
                problems.append(f"{lane.id}: ci_step {lane.ci_step!r} is not a step in ci.yml")
                continue
            if lane.ci_step in claimed_steps:
                problems.append(
                    f"{lane.id}: ci_step {lane.ci_step!r} is already claimed by {claimed_steps[lane.ci_step]}"
                )
            claimed_steps[lane.ci_step] = lane.id
            step = steps_by_name[lane.ci_step]
            if lane.builtin is not None:
                expected = f"python3 scripts/ci_local.py --lane {lane.id}"
                if expected not in _normalise(step.run):
                    problems.append(
                        f"{lane.id}: builtin must run as {expected!r}, step runs {_normalise(step.run)[:80]!r}"
                    )
            elif lane.command and "local" in lane.surfaces:
                segments = _step_segments(step.run)
                for part in _command_parts(lane.command):
                    if part not in segments:
                        problems.append(
                            f"{lane.id}: command part {part!r} is not a command of ci.yml step {lane.ci_step!r} "
                            f"(which runs {segments[:4]})"
                        )
        if lane.ci_job is not None and lane.ci_job not in job_names:
            problems.append(f"{lane.id}: ci_job {lane.ci_job!r} is not a job in ci.yml")
        elif lane.ci_job is not None:
            claimed_jobs[lane.ci_job] = lane.id

    for name, step in sorted(gate_steps.items()):
        if name in claimed_steps:
            continue
        if step.job in claimed_jobs:
            continue
        problems.append(f"ci.yml step {name!r} is not claimed by any lane in {MANIFEST_REL}")

    expected_rust = str(table.get("toolchain", {}).get("rust", ""))
    if expected_rust:
        for job in jobs:
            if f"toolchain: \"{expected_rust}\"" in workflow_text or f"toolchain: '{expected_rust}'" in workflow_text:
                break
        else:
            pins = re.findall(r"toolchain:\s*[\"']([^\"']+)[\"']", workflow_text)
            if not pins:
                problems.append("ci.yml installs no pinned toolchain")
            elif any(pin != expected_rust for pin in pins):
                problems.append(
                    f"toolchain.rust is {expected_rust!r} but ci.yml pins {sorted(set(pins))}"
                )
    return problems


# ── regression net (issue #127 acceptance) ──────────────────────────────────


class ParityRegression(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        root = Path(__file__).resolve().parents[1]
        cls.manifest = (root / MANIFEST_REL).read_text(encoding="utf-8")
        cls.workflow = (root / WORKFLOW_REL).read_text(encoding="utf-8")

    def test_repository_currently_agrees(self):
        self.assertEqual(check_parity(self.manifest, self.workflow), [])

    def test_required_checker_removed_from_ci_fails(self):
        """A step deleted from the workflow is not silently covered."""
        mutated = self.workflow.replace("      - name: Check formatting", "      - name: Skipped formatting")
        problems = check_parity(self.manifest, mutated)
        self.assertTrue(problems, "renaming a gate step must break parity")
        self.assertTrue(any("Check formatting" in p for p in problems), problems)

    def test_required_checker_removed_from_manifest_fails(self):
        """A lane that stops claiming its CI step is not silently covered."""
        mutated = self.manifest.replace('ci_step = "Check formatting"\n', "", 1)
        problems = check_parity(mutated, self.workflow)
        self.assertTrue(problems, "dropping a lane's ci_step must break parity")
        self.assertTrue(any("Check formatting" in p for p in problems), problems)

    def test_substituted_command_fails(self):
        """Same step name, different command: the drift is a defect.

        A shortened command is not a prefix that counts; it is a weaker check.
        """
        mutated = self.manifest.replace(
            'command = "cargo fmt --all -- --check"',
            'command = "cargo fmt --all"',
        )
        problems = check_parity(mutated, self.workflow)
        self.assertTrue(problems, "a substituted command must break parity")
        self.assertTrue(any("cargo fmt --all" in p for p in problems), problems)

        mutated = self.manifest.replace(
            'command = "cargo fmt --all -- --check"',
            'command = "cargo clippy --workspace --all-targets --locked -- -D warnings"',
        )
        problems = check_parity(mutated, self.workflow)
        self.assertTrue(problems, "a wholesale substitution must break parity")

    def test_builtin_must_route_through_the_coordinator(self):
        """A builtin's text exists once; CI runs the lane, not a copy."""
        mutated = self.workflow.replace(
            "python3 scripts/ci_local.py --lane unwrap-scan",
            "grep -rn '\\.unwrap()' crates",
        )
        problems = check_parity(self.manifest, mutated)
        self.assertTrue(problems, "a pasted builtin body must break parity")
        self.assertTrue(any("unwrap-scan" in p for p in problems), problems)

    def test_toolchain_mismatch_fails(self):
        mutated = self.manifest.replace('rust = "1.98.0"', 'rust = "1.97.0"', 1)
        problems = check_parity(mutated, self.workflow)
        self.assertTrue(problems, "a toolchain drift must break parity")
        self.assertTrue(any("toolchain" in p for p in problems), problems)

    def test_unclaimed_gate_step_fails(self):
        mutated = self.workflow.replace(
            "      - name: Check formatting",
            "      - name: Check formatting\n      - name: Check spelling\n        run: codespell",
        )
        problems = check_parity(self.manifest, mutated)
        self.assertTrue(problems, "an unclaimed gate step must break parity")
        self.assertTrue(any("Check spelling" in p for p in problems), problems)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=None)
    parser.add_argument("--test", action="store_true", help="run the regression suite")
    args = parser.parse_args(argv)

    if args.test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(ParityRegression)
        return 0 if unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful() else 1

    root = (args.root or Path(__file__).resolve().parents[1]).resolve()
    manifest = (root / MANIFEST_REL).read_text(encoding="utf-8")
    workflow = (root / WORKFLOW_REL).read_text(encoding="utf-8")
    problems = check_parity(manifest, workflow)
    if problems:
        print(f"gate parity refused: {len(problems)} problem(s)", file=sys.stderr)
        for problem in problems:
            print(f"  - {problem}", file=sys.stderr)
        return 1
    _, lanes = load_lanes(manifest)
    shared = sum(1 for lane in lanes if "local" in lane.surfaces and "ci" in lane.surfaces)
    print(
        f"gate parity: {len(lanes)} lane(s) declared, {shared} shared between local and ci; "
        "lane ids, commands and toolchain agree"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
