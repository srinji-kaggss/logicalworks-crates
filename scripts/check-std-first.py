#!/usr/bin/env python3
"""Audit every crate against `std`-first, `lgwks_std`-first, and `lgwks_deps`.

`AGENTS.md` says the workspace compiles against `std` and `lgwks_std` first and
that every other edge is a registered decision. Every crate here resolves
against four surfaces plus the standard library:

  `std`, `core`, `alloc`  the language's own
  `lgwks_std`              the core surface
  `lgwks_bot`              async, runners, the actor roles
  `lgwks_ast`              the standalone parser
  `lgwks_deps`             the storefront, which owns every third-party edge

Anything else is a crate reached past those, and the rule is that it is reached
only through an approval that names the crate reaching for it.

`lgwks-deps check` enforces the *manifest* half of that: an authored external
edge with no approval is refused. It cannot see the other half, because two
failures live only in the source:

  * a `use` of a crate that is not a declared dependency of the crate writing
    it — a build that happens to work because something else in the graph
    re-exports it, which is an edge nobody registered and `cargo` will not
    report until the day the other crate drops it;
  * a capability reimplemented by hand that `std` or `lgwks_std` already
    supplies — a hand-rolled hex encoder beside `lgwks_std::hex`, a
    `SystemTime::now()` used as an identity beside `lgwks_std::random`, an FFI
    entropy call beside `lgwks_std::random::fill_bytes`.

Both are invisible to the manifest gate and both are exactly the drift the
`std`-first rule exists to prevent, so this reads the source.

# What it reports

  `UNDECLARED`   a `use` whose root is neither a clean root, nor a module the
                 crate declares, nor a name the file binds by an earlier `use`.
  `UNAUTHORIZED` a crate the source reaches whose approval in `contract/
                 APPROVED.toml` does not admit *this* crate to it.
  `STD-FIRST`    a line matching a capability `std` or `lgwks_std` already
                 provides, named in the finding.
  `POLICY-ONLY`  a storefront edge no workspace code consumes whose register
                 `reason` does not open with `policy-only:`, or one that is
                 consumed and still says it is not (#418).

# Resolution, and why the obvious reading is wrong

The root of `use serde::de::VariantAccess;` is `serde`, and reporting it as an
undeclared dependency is wrong in this workspace: `crates/lgwks-bot/src/
session.rs` writes `use lgwks_std::json::serde;` first, so the later statement
resolves *through* `lgwks_std` and reaches no edge at all — which is exactly
what the rule asks for. A checker that reads only the root therefore reports the
correct pattern as a defect. So this parses the whole `use` tree: it collects
the names each file binds and the modules each crate declares, and a root is
clean when it is one of those.

# Justification

Every crate reached past the clean roots is printed with the record that
approves it — owner, capability, and the reason the approver wrote — so a
reader can see *why* the edge is there rather than only that it is allowed.
`--justify` prints those records whether or not there is a finding.

# Deliberate non-findings

The `STD-FIRST` patterns are a fixed, named list rather than a heuristic,
because a checker that guesses produces false positives and a checker with
false positives gets switched off. An earlier revision of this script carried
`std::fs::|std::process::Command|std::thread::spawn` as one pattern; it reported
91 hits, every one of them a legitimate call in a build tool or a fixture, and
"a wrapper exists" is not a finding. `lgwks_std` is exempt from all of them,
because it is the surface where the named capability is implemented — flagging
`lgwks_std::encoding` for containing a base64 alphabet would be the checker
reporting its own reference implementation as a violation of itself.

Usage:
    python3 scripts/check-std-first.py [--repo PATH] [--justify]

Exits non-zero when any finding is reported.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path

from rust_source import USE, CodeOnlyRegression, code_only, use_tree

# The standard library, plus the two relative-path prefixes every module may
# write without an edge.
CLEAN_ROOTS = frozenset(
    {"std", "core", "alloc", "proc_macro", "crate", "self", "super"}
)

# The surface that implements the capabilities the `STD-FIRST` patterns are
# about. A pattern's replacement lives here, so this crate is where its own
# reference implementation is allowed to be.
SURFACE_OWNER = "lgwks_std"

# The register `reason` prefix of an admitted-not-implemented edge (#418). An
# approval is a recorded policy decision, and a reader of `APPROVED.toml` or of
# `--justify` cannot tell one nobody consumes from a shipped capability unless
# the record says so. The register's schema refuses unknown keys, so the marker
# lives in the field it already has, and `policy_only_findings` keeps it true
# in both directions.
POLICY_ONLY = "policy-only:"

# Where source lives. `src` is the shipped surface; the rest are compiled by
# `--all-targets` and are held to the same rule, because a test that reaches a
# crate its manifest does not declare is the same unregistered edge.
SOURCE_GLOBS = (
    "crates/*/src/**/*.rs",
    "crates/*/tests/**/*.rs",
    "crates/*/examples/**/*.rs",
    "crates/*/benches/**/*.rs",
)

# A module the crate declares itself, in either form: `mod cap;` and
# `mod cap { … }` both bind `cap` in the file that declares it, so a later
# `pub use cap::…` resolves to it rather than to a crate.
LOCAL_MODULE = re.compile(r"\bmod\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)\s*[;{]", re.MULTILINE)

EXTERN_CRATE = re.compile(
    r"^[ \t]*extern[ \t]+crate[ \t]+(?P<root>[A-Za-z_][A-Za-z0-9_]*)\b",
    re.MULTILINE,
)

# A capability `std` or `lgwks_std` already provides, reimplemented by hand.
#
# Each pattern names the replacement, because a finding that does not say what
# to use instead is a complaint rather than a repair. The patterns are literal
# rather than heuristic: `{:02x}` in a format string is a hex encoder, and a
# base64 alphabet written out is a base64 encoder. Neither is a guess about
# intent. A finding is a question for a reader, not a verdict, and that is the
# most a source scan can honestly be.
STD_FIRST = (
    (
        re.compile(r"\{\s*:0?2[xX]\s*\}"),
        "hand-formatted hex; `lgwks_std::hex::encode` is the estate's encoder",
    ),
    (
        re.compile(r"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789\+/"),
        "a base64 alphabet written out; `lgwks_std::encoding` owns base64",
    ),
    (
        re.compile(r"\bextern\s+\"C\"\b"),
        "raw FFI; `lgwks_std` is where an audited syscall is allowed to live",
    ),
    (
        re.compile(r"std::process::id\s*\(|std::thread::current\s*\(\s*\)\s*\.id\s*\("),
        "a thread or process id used as an identity; both are reused by the OS, "
        "so `lgwks_std::random` is the distinguishable source",
    ),
    (
        re.compile(r"\bstd::thread::spawn\b"),
        "an untracked thread; `lgwks_std::task` and the bot's `JoinSet` are where "
        "a spawn is owned",
    ),
    (
        re.compile(r"\brand\s*::|\bgetrandom\s*::|/dev/urandom"),
        "entropy from outside `lgwks_std::random`; INV-RANDOM-ONE-SOURCE admits "
        "one CSPRNG backend and `lgwks_std` owns it",
    ),
)


# A line a pattern matches that is not the pattern's subject.
#
# Each entry pins the line's stripped text as well as its number, because a
# `path:line` key alone covers whatever later occupies that number: a file that
# grows above the exemption hands it to an unrelated line, and the finding it
# was suppressing reappears only by luck. With the text pinned, a moved
# exemption is a `STALE EXEMPTION` finding rather than a silent yes.
#
# The list is deliberately exact-line rather than per-file. A glob would cover
# the next genuine occurrence added to the same file, and this list exists to be
# the audit's record of what a reader looked at and decided — not a mute button.
EXEMPT: dict[str, tuple[str, str]] = {
    "crates/lgwks-ast/src/script/sim_script.rs:702": (
        'line: "let planted = std::thread::spawn(work)",',
        "a script line planted to prove the `script!` parser refuses an unowned "
        "spawn (\"a spawned task has no owner\"); it is parsed as data and never "
        "compiled, so it spawns nothing",
    ),
}


def crate_name(manifest: dict) -> str | None:
    """The name a crate is imported under: the package name, dashes to underscores."""
    name = manifest.get("package", {}).get("name")
    return name.replace("-", "_") if name else None


def declared_dependencies(manifest: dict) -> set[str]:
    """Every crate this manifest declares, across every dependency table.

    Target-specific tables (`[target.'cfg(unix)'.dependencies]`) are included:
    an edge is an edge whether or not this host compiles it.
    """
    found: set[str] = set()
    tables = ("dependencies", "dev-dependencies", "build-dependencies")
    for table in tables:
        section = manifest.get(table)
        if isinstance(section, dict):
            found.update(name.replace("-", "_") for name in section)
    for target in manifest.get("target", {}).values():
        if not isinstance(target, dict):
            continue
        for table in tables:
            section = target.get(table)
            if isinstance(section, dict):
                found.update(name.replace("-", "_") for name in section)
    return found


def approvals(repo: Path) -> dict[str, dict]:
    """`contract/APPROVED.toml` keyed by crate name, dashes normalised to underscores."""
    with (repo / "contract/APPROVED.toml").open("rb") as handle:
        register = tomllib.load(handle)
    # One crate can be approved once per owner (`tracing` is registered for
    # `lgwks_std` and again for `lgwks_ast`). Keying by name alone made the last
    # record win and silently dropped the earlier consumer, so the records are
    # merged: the first keeps its owner and every record's consumers are admitted.
    merged: dict[str, dict] = {}
    for entry in register.get("approved", []):
        key = entry["crate"].replace("-", "_")
        if key not in merged:
            merged[key] = dict(entry)
            continue
        union = admitted_consumers(merged[key]) | admitted_consumers(entry)
        merged[key]["allowed_consumers"] = ", ".join(sorted(union))
    return merged


def admitted_consumers(record: dict) -> set[str]:
    """The crates an approval names as allowed to reach the edge."""
    raw = record.get("allowed_consumers", "")
    return {name.strip().replace("-", "_") for name in raw.split(",") if name.strip()}


def workspace_members(repo: Path) -> dict[str, Path]:
    """Workspace member crate name -> the directory holding it."""
    with (repo / "Cargo.toml").open("rb") as handle:
        root = tomllib.load(handle)
    members: dict[str, Path] = {}
    for member in root.get("workspace", {}).get("members", []):
        directory = repo / member
        with (directory / "Cargo.toml").open("rb") as handle:
            name = crate_name(tomllib.load(handle))
        if name:
            members[name] = directory
    return members


def repository_files(repo: Path) -> set[Path] | None:
    """The files git says the repository holds, or None outside a checkout.

    Tracked files plus untracked ones `.gitignore` does not exclude: a new file
    is checked before it is staged, and build output is not. None, not an empty
    set, when git cannot answer, because "no files" would pass every check.
    """
    listing = subprocess.run(
        ["git", "-C", str(repo), "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
        capture_output=True,
        check=False,
    )
    if listing.returncode != 0:
        return None
    return {repo / name for name in listing.stdout.decode("utf-8", "surrogateescape").split("\0") if name}


def source_files(repo: Path) -> list[Path]:
    """Every file of the repository's source that a `SOURCE_GLOBS` pattern names.

    The glob alone reads the disk, and the disk holds more than the repository:
    a fixture compiled in place leaves `target/debug/build/*/out/*.rs` under
    `crates/*/tests/`, and this gate failed on one machine and passed on
    another over two such generated files (2026-09-30). Intersecting with what
    git holds makes the verdict a property of the repository. Outside a git
    checkout the disk is the only view, and the glob is used as is.
    """
    held = repository_files(repo)
    found: set[Path] = set()
    for pattern in SOURCE_GLOBS:
        matched = set(repo.glob(pattern))
        found.update(matched if held is None else matched & held)
    return sorted(found)


def cleaned_text(path: Path) -> str:
    """The file's text with comment-only lines blanked, newlines preserved.

    A pattern quoted inside a comment is documentation about the pattern, not an
    instance of it, and reporting one is the false positive that gets a checker
    switched off. Blanking rather than deleting keeps every offset on its own
    line, so a reported line number is the one a reader opens.
    """
    lines = path.read_text(encoding="utf-8").splitlines()
    return "\n".join("" if line.lstrip().startswith("//") else line for line in lines)


def line_of(text: str, offset: int) -> int:
    """The 1-based line number an offset falls on."""
    return text.count("\n", 0, offset) + 1


def owning_crate(repo: Path, path: Path, by_directory: dict[str, str]) -> str | None:
    """The crate a source file belongs to, from its directory under `crates/`.

    Keyed on the directory and not on the package name: the two are equal in
    this workspace once dashes are normalised, which is exactly why reading the
    package name off the path looks right and is not. A crate whose directory is
    `fake` and whose package is `lgwks_fake` would be attributed to a crate that
    holds no files, and the audit would scan nothing and report a clean pass —
    measured, against a control written to fail.
    """
    try:
        relative = path.relative_to(repo).parts
    except ValueError:
        return None
    if len(relative) < 2 or relative[0] != "crates":
        return None
    return by_directory.get(relative[1])


def module_names(paths: list[Path]) -> set[str]:
    """Every module name declared across a crate's source."""
    names: set[str] = set()
    for path in paths:
        names.update(m.group("name") for m in LOCAL_MODULE.finditer(code_only(cleaned_text(path))))
    return names


def justification(record: dict | None) -> str:
    """The one-line record behind an edge: who owns it and why it was approved."""
    if record is None:
        return "no record in contract/APPROVED.toml"
    return (
        f"owner={record.get('owner')} capability={record.get('capability')} "
        f"tier={record.get('tier')} — {record.get('reason')}"
    )


def storefront_consumers(repo: Path, files: list[Path], owned: set[str]) -> dict[str, set[str]]:
    """Each `lgwks_deps`-only edge -> the files whose code consumes it.

    Consumption is a `lgwks_deps::<edge>` path in another workspace crate, or
    a `<edge>::` path in the storefront's own source past `lib.rs`, whose
    re-export lines are the admission, not a use of it. Tests, examples and
    fixtures of the storefront itself are not consumers: a fixture that names
    `gpui` proves the gate sees the edge, not that anything ships with it.
    """
    own_src = "crates/lgwks-deps/src/"
    consumers: dict[str, set[str]] = {name: set() for name in owned}
    for path in files:
        relative = path.relative_to(repo).as_posix()
        code = code_only(cleaned_text(path))
        if relative.startswith(own_src):
            if relative == own_src + "lib.rs":
                continue
            for name in owned:
                if re.search(rf"(?<![A-Za-z0-9_:]){re.escape(name)}::", code):
                    consumers[name].add(relative)
            continue
        if relative.startswith("crates/lgwks-deps/"):
            continue
        for match in re.finditer(r"\blgwks_deps::(\{[^}]*\}|[A-Za-z_][A-Za-z0-9_]*)", code):
            for name in re.findall(r"[A-Za-z_][A-Za-z0-9_]*", match.group(1)):
                if name in consumers:
                    consumers[name].add(relative)
    return consumers


def policy_only_findings(approved: dict[str, dict], consumers: dict[str, set[str]]) -> list[str]:
    """The register lines whose `policy-only:` marker disagrees with the source.

    Both directions fail: an unconsumed edge left unmarked reads as a delivered
    capability, and a consumed edge still marked hides that it shipped.
    """
    findings: list[str] = []
    for name in sorted(consumers):
        marked = approved[name].get("reason", "").startswith(POLICY_ONLY)
        used = sorted(consumers[name])
        if not used and not marked:
            findings.append(
                f"contract/APPROVED.toml `{name}` — POLICY-ONLY: no workspace code consumes "
                f"it, so its reason must open with `{POLICY_ONLY}` (#418)"
            )
        elif used and marked:
            findings.append(
                f"contract/APPROVED.toml `{name}` — POLICY-ONLY: marked `{POLICY_ONLY}` but "
                f"consumed by {used[0]}; drop the marker"
            )
    return findings


def storefront_only(approved: dict[str, dict]) -> set[str]:
    """The edges the storefront owns and admits no other crate to."""
    return {
        name for name, record in approved.items()
        if record.get("owner") == "lgwks_deps" and admitted_consumers(record) == {"lgwks_deps"}
    }


class PolicyOnlyRegression(unittest.TestCase):
    """The #418 marker: a storefront edge says whether anything consumes it."""

    def write(self, repo: Path, relative: str, text: str) -> Path:
        path = repo / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
        return path

    def consumers_of(self, sources: dict[str, str], owned: set[str]) -> dict[str, set[str]]:
        with tempfile.TemporaryDirectory() as scratch:
            repo = Path(scratch)
            files = [self.write(repo, rel, text) for rel, text in sources.items()]
            return storefront_consumers(repo, files, owned)

    def test_a_facade_path_in_another_crate_consumes_and_a_reexport_does_not(self) -> None:
        found = self.consumers_of(
            {
                "crates/lgwks-deps/src/lib.rs": "pub use gpui::{self, App as GpuiApp};\npub use bevy_ecs;\n",
                "crates/lgwks-deps/tests/fixtures/app/src/main.rs": "use lgwks_deps::gpui::App;\n",
                "crates/lgwks-bot/src/ecs.rs": "use lgwks_deps::{bevy_ecs::{self, prelude::World}};\n",
                "crates/lgwks-bot/src/note.rs": '// lgwks_deps::gpui\nlet s = "lgwks_deps::gpui";\n',
            },
            {"gpui", "bevy_ecs"},
        )
        self.assertEqual(found, {"gpui": set(), "bevy_ecs": {"crates/lgwks-bot/src/ecs.rs"}})

    def test_the_storefronts_own_source_past_lib_rs_consumes(self) -> None:
        found = self.consumers_of(
            {
                "crates/lgwks-deps/src/scan.rs": "let f = syn::parse_file(t);\n",
                "crates/lgwks-deps/src/report.rs": "let g = not_gpui::App::new();\n",
            },
            {"syn", "gpui"},
        )
        self.assertEqual(found, {"syn": {"crates/lgwks-deps/src/scan.rs"}, "gpui": set()})

    def test_the_marker_must_match_consumption_in_both_directions(self) -> None:
        approved = {
            "gpui": {"reason": "A GPU UI."},
            "candle_core": {"reason": f"{POLICY_ONLY} tensors."},
            "tokio": {"reason": f"{POLICY_ONLY} runtime."},
            "syn": {"reason": "Parsing."},
        }
        consumers = {"gpui": set(), "candle_core": set(), "tokio": {"crates/lgwks-bot/src/lib.rs"}, "syn": {"x.rs"}}
        found = policy_only_findings(approved, consumers)
        self.assertEqual(len(found), 2)
        self.assertIn("`gpui` — POLICY-ONLY: no workspace code consumes", found[0])
        self.assertIn("`tokio` — POLICY-ONLY: marked", found[1])
        self.assertIn("crates/lgwks-bot/src/lib.rs", found[1])

    def test_only_edges_admitting_the_storefront_alone_are_judged(self) -> None:
        approved = {
            "gpui": {"owner": "lgwks_deps", "allowed_consumers": "lgwks_deps"},
            "proptest": {"owner": "lgwks_deps", "allowed_consumers": "lgwks_std, lgwks_deps"},
            "regex": {"owner": "lgwks_std", "allowed_consumers": "lgwks_std"},
        }
        self.assertEqual(storefront_only(approved), {"gpui"})


class SourceFilesRegression(unittest.TestCase):
    """Which files the scan reads: the repository's, not whatever is on disk."""

    def test_ignored_build_output_is_not_source_and_a_new_file_is(self) -> None:
        with tempfile.TemporaryDirectory() as scratch:
            repo = Path(scratch)
            subprocess.run(["git", "init", "-q", str(repo)], check=True)
            (repo / ".gitignore").write_text("target/\n", encoding="utf-8")
            kept = repo / "crates/demo/tests/unstaged.rs"
            generated = repo / "crates/demo/tests/fixtures/app/target/debug/build/out/private.rs"
            for path in (kept, generated):
                path.parent.mkdir(parents=True)
                path.write_text("use serde_core::Serialize;\n", encoding="utf-8")
            self.assertEqual(source_files(repo), [kept])

    def test_outside_a_checkout_the_disk_is_scanned(self) -> None:
        with tempfile.TemporaryDirectory() as scratch:
            repo = Path(scratch)
            source = repo / "crates/demo/src/lib.rs"
            source.parent.mkdir(parents=True)
            source.write_text("", encoding="utf-8")
            if repository_files(repo) is not None:
                self.skipTest("the scratch directory is inside a git checkout")
            self.assertEqual(source_files(repo), [source])


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--repo",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
        help="repository root (default: the parent of this script)",
    )
    parser.add_argument(
        "--justify",
        action="store_true",
        help="print the approval record for every crate reached past the clean roots",
    )
    parser.add_argument("--test", action="store_true", help="run the regression suite")
    args = parser.parse_args()
    if args.test:
        loader = unittest.defaultTestLoader
        suite = unittest.TestSuite(
            [
                loader.loadTestsFromTestCase(case)
                for case in (CodeOnlyRegression, SourceFilesRegression, PolicyOnlyRegression)
            ]
        )
        return 0 if unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful() else 1
    repo: Path = args.repo.resolve()
    home: Path = Path(__file__).resolve().parent.parent

    members = workspace_members(repo)
    approved = approvals(repo)

    declared: dict[str, set[str]] = {}
    for name, directory in members.items():
        with (directory / "Cargo.toml").open("rb") as handle:
            declared[name] = declared_dependencies(tomllib.load(handle))

    files = source_files(repo)
    by_directory = {directory.name: name for name, directory in members.items()}
    by_crate: dict[str, list[Path]] = {name: [] for name in members}
    unattributed: list[Path] = []
    for path in files:
        crate = owning_crate(repo, path, by_directory)
        if crate is None:
            unattributed.append(path.relative_to(repo))
        else:
            by_crate[crate].append(path)
    modules = {name: module_names(paths) for name, paths in by_crate.items()}

    # A source file under `crates/` that no member claims is a file this audit
    # did not read, and an unread file is not a passing one. Empty crates are
    # reported for the same reason: `--all-targets` compiles no source there, so
    # a crate that contributes none is either misconfigured or mis-named, and
    # either way the clean result would be about nothing.
    findings: list[str] = [
        f"{path} — UNATTRIBUTED: no workspace member owns this path" for path in unattributed
    ]
    findings += [
        f"crates/{directory} — EMPTY: `{name}` contributed no source file to the audit"
        for name, directory in sorted((n, d.name) for n, d in members.items())
        if not by_crate[name]
    ]

    # Crate reached past the clean roots -> the crates that reach it.
    reached: dict[str, set[str]] = {}
    # Every crate the estate knows about, which is what a bare `name::` path
    # can resolve to. A workspace member or a clean root is never foreign.
    foreign = (set(approved) | {d for deps in declared.values() for d in deps}) - set(members) - CLEAN_ROOTS
    # One finding per (file, line, root): a crate named twice on one line, or
    # reached by both a `use` and a bare path, is one problem and not two.
    seen: set[str] = set()

    # Verify every exemption before it is used, rather than only when a pattern
    # happens to match it. An exemption whose line moved is the case that
    # matters, and that is precisely the case in which no pattern matches the
    # recorded number and a match-driven check would never look.
    # `EXEMPT` names lines in *this* repository, so it is only meaningful when
    # the audit is pointed at it. `--repo` exists for running the checker
    # against a deliberately-failing control, and reporting every exemption as
    # a missing file would bury the control's own findings under noise.
    valid_exemptions: set[str] = set()
    for key, (expected, _) in sorted(EXEMPT.items() if repo == home else []):
        document, _, number = key.rpartition(":")
        target = repo / document
        if not target.exists():
            findings.append(f"{key} — STALE EXEMPTION: no such file")
            continue
        lines = target.read_text(encoding="utf-8").splitlines()
        index = int(number)
        if index < 1 or index > len(lines):
            findings.append(
                f"{key} — STALE EXEMPTION: the file has {len(lines)} line(s)"
            )
            continue
        actual = lines[index - 1].strip()
        if actual != expected:
            findings.append(
                f"{key} — STALE EXEMPTION: line {index} is now "
                f"`{actual[:70]}`, not the `{expected[:70]}` it was granted for"
            )
            continue
        valid_exemptions.add(key)

    for crate, paths in by_crate.items():
        clean = CLEAN_ROOTS | set(members) | modules[crate]
        for path in paths:
            relative = path.relative_to(repo)
            text = cleaned_text(path)
            # `use` statements and path roots are read from code only; the
            # `STD-FIRST` patterns below keep reading `text`, because a format
            # string or a written-out alphabet is exactly what they look for.
            code = code_only(text)

            # Every name this file binds, collected before any root is judged:
            # `use lgwks_std::json::serde;` at the top is what makes a later
            # `use serde::de::VariantAccess;` a path through `lgwks_std`.
            statements = list(USE.finditer(code))
            statements += list(EXTERN_CRATE.finditer(code))
            bindings: set[str] = set()
            for match in statements:
                if "body" in match.groupdict():
                    _, names = use_tree(match.group("body"))
                    bindings |= names

            resolvable = clean | bindings | declared[crate]

            # A crate can also be reached without a `use` at all: edition 2018
            # lets `regex::Regex::new(…)` stand on its own, and that is how an
            # undeclared edge hides from a scan that reads only `use`
            # statements. So every crate the register knows about, and every
            # crate the workspace declares, is looked for as a path root too.
            # A control crate written to fail only on its manifest was reached
            # this way and the first version of this scan reported it clean.
            for name in sorted(foreign):
                # `(?<![A-Za-z0-9_:])` keeps `lgwks_deps::tokio::` from being
                # read as a reach into `tokio`: the estate reaches tokio
                # *through* the storefront that owns it, which is the rule
                # working rather than a violation of it.
                for match in re.finditer(
                    rf"(?<![A-Za-z0-9_:]){re.escape(name)}::", code
                ):
                    number = line_of(code, match.start())
                    # A declared edge is reached, whether or not this reports
                    # it. Recording it only in the undeclared branch left a
                    # crate that declares an unapproved edge and uses it
                    # entirely unexamined by the register check below —
                    # measured, against a control written to fail on exactly
                    # that crate.
                    if name in declared[crate]:
                        reached.setdefault(name, set()).add(crate)
                        continue
                    # `resolvable`, not `clean`: the per-file `bindings` are
                    # what make session.rs's bare `serde::` a path through
                    # `lgwks_std::json::serde` rather than an edge, and testing
                    # against the per-crate set alone loses exactly that.
                    if name in resolvable:
                        continue
                    key = f"{relative}:{number}:{name}"
                    if key in seen:
                        continue
                    seen.add(key)
                    findings.append(
                        f"{relative}:{number} — UNDECLARED: `{name}::` is reached with "
                        f"no `use` and `{crate}` does not declare it"
                    )

            for match in statements:
                root = (
                    match.group("root")
                    if "root" in match.groupdict() and match.group("root")
                    else next(iter(use_tree(match.group("body"))[0]), "")
                )
                if not root or root in clean:
                    continue
                reached.setdefault(root, set()).add(crate)
                if root in resolvable:
                    continue
                findings.append(
                    f"{relative}:{line_of(code, match.start())} — UNDECLARED: "
                    f"`{root}` is neither a clean root, nor a module `{crate}` "
                    f"declares, nor a name this file binds"
                )

            if crate == SURFACE_OWNER:
                continue
            for pattern, why in STD_FIRST:
                for match in pattern.finditer(text):
                    key = f"{relative}:{line_of(text, match.start())}"
                    if key in valid_exemptions:
                        continue
                    findings.append(f"{key} — STD-FIRST: {why}")

    # The register half, asked once per edge rather than per use site: an edge
    # that is reached must be approved *for the crate reaching it*. Only edges
    # this crate actually declares are judged here — an undeclared one is
    # already the finding above, and reporting it twice reads as two problems.
    for dep in sorted(reached):
        if any(dep in declared[crate] for crate in reached[dep]) is False:
            continue
        record = approved.get(dep)
        consumers = sorted(
            crate for crate in reached[dep] if dep in declared[crate]
        )
        if record is None:
            findings.append(
                f"{consumers} declare `{dep}`, which has no record in "
                f"contract/APPROVED.toml"
            )
            continue
        admitted = admitted_consumers(record)
        for crate in consumers:
            if crate not in admitted:
                findings.append(
                    f"crates/{crate}/Cargo.toml — UNAUTHORIZED: `{dep}` is owned by "
                    f"`{record.get('owner')}` and admits {sorted(admitted)}, not `{crate}`"
                )

    owned = storefront_only(approved)
    consumers = storefront_consumers(repo, files, owned)
    findings += policy_only_findings(approved, consumers)

    if args.justify and reached:
        print("Crates reached past std / lgwks_std / lgwks_bot / lgwks_ast / lgwks_deps:")
        for dep in sorted(reached):
            # Declaring the edge and resolving a name through it are different
            # claims, and flattening them misreports the good case:
            # `lgwks_bot` writes `use lgwks_std::json::serde;` and authors no
            # serde edge at all, which is the rule being followed rather than
            # an unregistered dependency.
            declaring = sorted(c for c in reached[dep] if dep in declared[c])
            through = sorted(c for c in reached[dep] if dep not in declared[c])
            print(f"  {dep} — declared by {declaring}")
            if through:
                print(f"    resolved through a surface by {through}")
            print(f"    {justification(approved.get(dep))}")
        print()
        policy_only = sorted(name for name in owned if not consumers[name])
        print(f"Admitted, not implemented ({POLICY_ONLY} {len(policy_only)} of {len(owned)} storefront-only edges):")
        print(f"  {', '.join(policy_only) if policy_only else 'none'}")
        print()

    if findings:
        print(f"{len(findings)} finding(s):", file=sys.stderr)
        for finding in findings:
            print(f"  {finding}", file=sys.stderr)
        return 1

    print(
        f"std-first holds: {len(files)} source file(s) across {len(members)} crate(s); "
        f"{len(reached)} crate(s) reached past the clean roots "
        f"({', '.join(sorted(reached)) if reached else 'none'}); "
        f"{sum(1 for name in owned if not consumers[name])} storefront edge(s) marked {POLICY_ONLY}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
