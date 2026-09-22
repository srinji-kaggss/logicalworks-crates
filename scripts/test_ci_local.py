#!/usr/bin/env python3
"""Public CLI tests for the gate coordinator (issue #127 acceptance).

These drive ``scripts/ci_local.py`` through its documented entry points
against fixture trees and controlled fake commands. They are **not**
production CI execution and never claim to be: a real full Rust/CI run on the
final integrated revision is separate evidence, and this file says so.

Cases #127 named: missing PyYAML, an absent and a non-executable smoke helper,
a required checker removed from one execution surface (covered by
``check-gate-parity.py --test``), a forbidden file already committed, and
changed/untracked build inputs. Plus the two controls: all prerequisites
present, and an explicitly optional platform lane.

Run::

    python3 scripts/test_ci_local.py
"""

from __future__ import annotations

import os
import stat
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
CI_LOCAL = REPO / "scripts" / "ci_local.py"
PASS, FAIL, BLOCKED, SKIP = "pass", "fail", "blocked", "skip"


def run_cli(*args: str, root: Path, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    """Invoke the public CLI as a subprocess, the way CI and a developer do."""
    base = os.environ.copy()
    base["PATH"] = f"{Path.home() / '.cargo' / 'bin'}{os.pathsep}{base.get('PATH', '')}"
    if env:
        base.update(env)
    return subprocess.run(
        [sys.executable, str(CI_LOCAL), "--root", str(root), *args],
        capture_output=True,
        text=True,
        env=base,
        timeout=120,
        check=False,
    )


def write_manifest(root: Path, body: str) -> None:
    (root / "scripts").mkdir(parents=True, exist_ok=True)
    (root / "scripts" / "gate-lanes.toml").write_text(
        textwrap.dedent(body).lstrip(),
        encoding="utf-8",
    )


def git(root: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args], cwd=root, capture_output=True, text=True, check=check, timeout=30
    )


def seed(root: Path) -> None:
    """Commit the fixture so the receipt starts from a clean tree."""
    git(root, "add", "-A")
    git(root, "commit", "-q", "--no-verify", "-m", "seed")


def init_repo(root: Path) -> None:
    git(root, "init", "-q")
    git(root, "config", "user.email", "gate@example.invalid")
    git(root, "config", "user.name", "gate test")
    git(root, "config", "commit.gpgsign", "false")


def fake_bin(root: Path, **programs: str) -> Path:
    """A directory of controlled fake commands, prepended to PATH."""
    bin_dir = root / "fake-bin"
    bin_dir.mkdir(parents=True, exist_ok=True)
    for name, body in programs.items():
        path = bin_dir / name
        path.write_text(body, encoding="utf-8")
        path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return bin_dir


class CoordinatorDisposition(unittest.TestCase):
    """What the gate claims when a lane cannot run, and what it refuses to claim."""

    def test_all_prerequisites_present_is_green(self):
        """Control: a fixture with every need met and fake commands passes."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_manifest(
                root,
                """
                [toolchain]
                rust = "0.0.0"

                [[lane]]
                id = "alpha"
                surfaces = "local ci"
                required = true
                fast = true
                command = "true"
                ci_step = "Alpha"

                [[lane]]
                id = "beta"
                surfaces = "local"
                required = true
                fast = true
                builtin = "artifacts"
                """,
            )
            init_repo(root)
            (root / "keep.txt").write_text("ok\n", encoding="utf-8")
            seed(root)

            done = run_cli("--fast", "--receipt", root=root)
            self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
            self.assertIn("passed: 2", done.stdout)
            self.assertIn("failed: 0", done.stdout)
            self.assertIn("blocked: 0", done.stdout)

    def test_missing_pyyaml_blocks_rather_than_skips(self):
        """#127 defect 1: a missing checker dependency is `blocked`, never a pass."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_manifest(
                root,
                """
                [toolchain]
                rust = "0.0.0"

                [[lane]]
                id = "gate-parity"
                surfaces = "local"
                required = true
                fast = true
                command = "true"
                needs = ["module:yaml"]
                """,
            )
            init_repo(root)
            (root / "keep.txt").write_text("ok\n", encoding="utf-8")
            seed(root)

            shim = root / "shim"
            shim.mkdir()
            (shim / "yaml.py").write_text('raise ImportError("blocked for test")\n', encoding="utf-8")

            done = run_cli("--fast", root=root, env={"PYTHONPATH": str(shim)})
            self.assertNotEqual(done.returncode, 0, "a missing required dependency must not green the gate")
            self.assertIn("blocked", done.stdout + done.stderr)
            self.assertIn("module:yaml", done.stdout + done.stderr)
            self.assertNotIn("all gates passed", done.stdout)

    def test_absent_smoke_helper_blocks_package_smoke(self):
        """#127: an absent prerequisite is a blocked lane, not a silent skip."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_manifest(
                root,
                """
                [toolchain]
                rust = "0.0.0"

                [[lane]]
                id = "package-smoke"
                surfaces = "local ci"
                required = true
                fast = true
                command = "./scripts/lgwks-std-package-smoke.sh"
                ci_step = "Run package smoke test"
                needs = ["script:scripts/lgwks-std-package-smoke.sh"]
                """,
            )
            init_repo(root)
            (root / "keep.txt").write_text("ok\n", encoding="utf-8")
            seed(root)

            done = run_cli("--fast", root=root)
            self.assertNotEqual(done.returncode, 0)
            self.assertIn("blocked", done.stdout + done.stderr)
            self.assertIn("lgwks-std-package-smoke.sh", done.stdout + done.stderr)

    def test_non_executable_smoke_helper_blocks_package_smoke(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_manifest(
                root,
                """
                [toolchain]
                rust = "0.0.0"

                [[lane]]
                id = "package-smoke"
                surfaces = "local ci"
                required = true
                fast = true
                command = "./scripts/lgwks-std-package-smoke.sh"
                ci_step = "Run package smoke test"
                needs = ["script:scripts/lgwks-std-package-smoke.sh"]
                """,
            )
            init_repo(root)
            scripts = root / "scripts"
            scripts.mkdir(parents=True, exist_ok=True)
            helper = scripts / "lgwks-std-package-smoke.sh"
            helper.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            helper.chmod(0o644)
            (root / "keep.txt").write_text("ok\n", encoding="utf-8")
            seed(root)

            done = run_cli("--fast", root=root)
            self.assertNotEqual(done.returncode, 0)
            self.assertIn("blocked", done.stdout + done.stderr)

    def test_optional_platform_lane_is_skipped_never_passed(self):
        """Control #127 named: an out-of-scope optional lane is `skip`, not `pass`."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_manifest(
                root,
                """
                [toolchain]
                rust = "0.0.0"

                [[lane]]
                id = "always"
                surfaces = "local"
                required = true
                fast = true
                command = "true"

                [[lane]]
                id = "gpui-macos"
                surfaces = "local ci"
                required = false
                fast = true
                command = "true"
                ci_step = "Check normal GPUI renderer feature"
                platforms = ["nonexistent-os"]
                needs = ["bin:xcodebuild"]
                reason = "needs the Xcode Metal toolchain"
                """,
            )
            init_repo(root)
            (root / "keep.txt").write_text("ok\n", encoding="utf-8")
            seed(root)

            done = run_cli("--fast", "--receipt", root=root)
            self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
            self.assertIn("gpui-macos", done.stdout)
            self.assertIn("skip", done.stdout)
            self.assertIn("skipped: 1", done.stdout)
            receipt = (root / "evidence").glob("ci-local-*.md")
            text = next(receipt).read_text(encoding="utf-8")
            self.assertIn("gpui-macos", text)
            self.assertNotRegex(text, r"\| gpui-macos \| \*\*pass\*\*")
            self.assertIn("Not covered by this surface", text)

    def test_required_lane_with_a_missing_need_is_blocked_not_skipped(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_manifest(
                root,
                """
                [toolchain]
                rust = "0.0.0"

                [[lane]]
                id = "needs-missing-tool"
                surfaces = "local"
                required = true
                fast = true
                command = "true"
                needs = ["bin:definitely-not-a-real-binary"]
                """,
            )
            init_repo(root)
            (root / "keep.txt").write_text("ok\n", encoding="utf-8")
            seed(root)

            done = run_cli("--fast", root=root)
            self.assertNotEqual(done.returncode, 0)
            self.assertIn("blocked", done.stdout + done.stderr)
            self.assertNotIn("all gates passed", done.stdout)

    def test_ci_only_lane_is_named_as_uncovered(self):
        """A green local run is not evidence for a ci lane, and says so."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_manifest(
                root,
                """
                [toolchain]
                rust = "0.0.0"

                [[lane]]
                id = "local-one"
                surfaces = "local"
                required = true
                fast = true
                command = "true"

                [[lane]]
                id = "grammar-matrix"
                surfaces = "ci"
                required = true
                command = "true"
                ci_step = "Execute the lgwks-ast grammar matrix"
                reason = "CI runs it as a bash loop"
                """,
            )
            init_repo(root)
            (root / "keep.txt").write_text("ok\n", encoding="utf-8")
            seed(root)

            done = run_cli("--fast", "--receipt", root=root)
            self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
            text = next((root / "evidence").glob("ci-local-*.md")).read_text(encoding="utf-8")
            self.assertIn("Not covered by this surface", text)
            self.assertIn("grammar-matrix", text)
            self.assertNotRegex(text, r"\| grammar-matrix \| \*\*pass\*\*")


class CoordinatorArtifacts(unittest.TestCase):
    """Derived output must not be tracked or staged (#127 defect 3)."""

    def test_forbidden_file_already_committed_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_manifest(
                root,
                """
                [toolchain]
                rust = "0.0.0"

                [[lane]]
                id = "artifacts"
                surfaces = "local"
                required = true
                fast = true
                builtin = "artifacts"
                """,
            )
            init_repo(root)
            (root / "target").mkdir()
            (root / "target" / "leaked.rlib").write_text("derived\n", encoding="utf-8")
            (root / "keep.txt").write_text("ok\n", encoding="utf-8")
            # --no-verify: the estate pre-commit hook would refuse this commit,
            # which is the point. The fixture needs the bad state so the checker
            # can be shown to reject it.
            git(root, "add", "-f", ".")
            git(root, "commit", "-q", "--no-verify", "-m", "commit a derived artifact on purpose")

            done = run_cli("--fast", root=root)
            self.assertNotEqual(done.returncode, 0, "a committed derived artifact must refuse the gate")
            self.assertIn("fail", done.stdout + done.stderr)
            self.assertIn("target/leaked.rlib", done.stdout + done.stderr)

    def test_staged_but_uncommitted_forbidden_file_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_manifest(
                root,
                """
                [toolchain]
                rust = "0.0.0"

                [[lane]]
                id = "artifacts"
                surfaces = "local"
                required = true
                fast = true
                builtin = "artifacts"
                """,
            )
            init_repo(root)
            (root / "keep.txt").write_text("ok\n", encoding="utf-8")
            seed(root)
            (root / "graphify-out").mkdir()
            (root / "graphify-out" / "dump.json").write_text("{}\n", encoding="utf-8")
            git(root, "add", "-f", "graphify-out/dump.json")

            done = run_cli("--fast", root=root)
            self.assertNotEqual(done.returncode, 0)
            self.assertIn("graphify-out/dump.json", done.stdout + done.stderr)

    def test_untracked_build_output_alone_is_not_a_failure_but_is_recorded(self):
        """`target/` on a working tree is normal; it is receipt state, not a crime."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_manifest(
                root,
                """
                [toolchain]
                rust = "0.0.0"

                [[lane]]
                id = "artifacts"
                surfaces = "local"
                required = true
                fast = true
                builtin = "artifacts"
                """,
            )
            init_repo(root)
            (root / "keep.txt").write_text("ok\n", encoding="utf-8")
            seed(root)
            (root / "target").mkdir()
            (root / "target" / "scratch").write_text("local build\n", encoding="utf-8")

            done = run_cli("--fast", "--receipt", root=root)
            self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
            text = next((root / "evidence").glob("ci-local-*.md")).read_text(encoding="utf-8")
            self.assertIn("target/scratch", text)
            self.assertIn("UNVERIFIED", text)


class CoordinatorReceipt(unittest.TestCase):
    """The receipt names the inputs it describes (#127 defect 4)."""

    def test_receipt_records_full_commit_tree_and_untracked_inputs(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_manifest(
                root,
                """
                [toolchain]
                rust = "1.98.0"

                [[lane]]
                id = "alpha"
                surfaces = "local"
                required = true
                fast = true
                command = "true"
                """,
            )
            init_repo(root)
            (root / "keep.txt").write_text("ok\n", encoding="utf-8")
            seed(root)
            (root / "scratch.txt").write_text("untracked\n", encoding="utf-8")
            (root / "keep.txt").write_text("changed\n", encoding="utf-8")

            done = run_cli("--fast", "--receipt", root=root)
            self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
            text = next((root / "evidence").glob("ci-local-*.md")).read_text(encoding="utf-8")

            commit = subprocess.run(
                ["git", "rev-parse", "HEAD"], cwd=root, capture_output=True, text=True, check=True, timeout=30
            ).stdout.strip()
            tree = subprocess.run(
                ["git", "rev-parse", "HEAD^{tree}"], cwd=root, capture_output=True, text=True, check=True, timeout=30
            ).stdout.strip()

            self.assertRegex(text, rf"- commit: `{re_escape(commit)}`")
            self.assertRegex(text, rf"- tree: `{re_escape(tree)}`")
            self.assertIn("UNVERIFIED", text)
            self.assertIn("scratch.txt", text)
            self.assertIn("keep.txt", text)
            self.assertIn("untracked: `scratch.txt`", text)
            self.assertNotIn("identity: **verified**", text)

    def test_receipt_verified_only_from_a_clean_tree(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_manifest(
                root,
                """
                [toolchain]
                rust = "1.98.0"

                [[lane]]
                id = "alpha"
                surfaces = "local"
                required = true
                fast = true
                command = "true"
                """,
            )
            init_repo(root)
            (root / "keep.txt").write_text("ok\n", encoding="utf-8")
            seed(root)

            done = run_cli("--fast", "--receipt", root=root)
            self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
            text = next((root / "evidence").glob("ci-local-*.md")).read_text(encoding="utf-8")
            self.assertIn("identity: **verified**", text)
            self.assertIn("clean index, no untracked files", text)


class CoordinatorCli(unittest.TestCase):
    def test_list_prints_every_lane_and_surface(self):
        done = run_cli("--list", root=REPO)
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertIn("gate-parity", done.stdout)
        self.assertIn("grammar-matrix", done.stdout)
        self.assertIn("appcui-native", done.stdout)

    def test_unknown_lane_is_a_usage_error(self):
        done = run_cli("--lane", "not-a-lane", root=REPO)
        self.assertEqual(done.returncode, 2)
        self.assertIn("unknown lane id", done.stderr)

    def test_lane_mode_runs_exactly_the_named_lane(self):
        """What CI does per step: one lane, one exit code."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_manifest(
                root,
                """
                [toolchain]
                rust = "0.0.0"

                [[lane]]
                id = "alpha"
                surfaces = "local ci"
                required = true
                command = "true"
                ci_step = "Alpha"

                [[lane]]
                id = "beta"
                surfaces = "local"
                required = true
                command = "false"
                """,
            )
            done = run_cli("--lane", "alpha", root=root)
            self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
            self.assertIn("alpha: pass", done.stdout)
            self.assertNotIn("beta", done.stdout.split("─")[-1])


def re_escape(value: str) -> str:
    return "".join(f"\\{c}" if c in r"\^$.|?*+()[]{}" else c for c in value)


if __name__ == "__main__":
    suite = unittest.defaultTestLoader.loadTestsFromModule(sys.modules[__name__])
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    sys.exit(0 if result.wasSuccessful() else 1)
