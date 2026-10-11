"""The gate's builtin lanes: checks implemented here rather than shelled out.

Split from ``ci_local.py`` under the size law (#408). Each checker takes the
repository root and the child environment and returns ``(exit code, detail)``;
``BUILTINS`` maps a lane's ``builtin`` name to its checker.
"""

from __future__ import annotations

import json
import re
import tempfile
import tomllib
from pathlib import Path

from ci_invariants import builtin_invariants
from ci_process import _git, _run_argv, _run_capture

FORBIDDEN_PREFIXES = (
    "target/",
    "graphify-out/",
    ".lgwks/",
    "node_modules/",
    ".codegraph/",
)


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
        whole_binary = binary_name.startswith("sim_") or "::sim_" in str(suite_id)
        for case in suite.get("testcases", {}):
            # A consolidated binary (`tests/it/`, #272) keeps each former
            # `sim_*` file as a module, so a case counts when any module on
            # its path is a simulation; the test function's own name does not.
            modules = str(case).split("::")[:-1]
            if whole_binary or any(module.startswith("sim_") for module in modules):
                sim_total += 1
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
