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
import sys
import tomllib
from pathlib import Path

# The standard library, plus the two relative-path prefixes every module may
# write without an edge.
CLEAN_ROOTS = frozenset(
    {"std", "core", "alloc", "proc_macro", "crate", "self", "super"}
)

# The surface that implements the capabilities the `STD-FIRST` patterns are
# about. A pattern's replacement lives here, so this crate is where its own
# reference implementation is allowed to be.
SURFACE_OWNER = "lgwks_std"

# Where source lives. `src` is the shipped surface; the rest are compiled by
# `--all-targets` and are held to the same rule, because a test that reaches a
# crate its manifest does not declare is the same unregistered edge.
SOURCE_GLOBS = (
    "crates/*/src/**/*.rs",
    "crates/*/tests/**/*.rs",
    "crates/*/examples/**/*.rs",
    "crates/*/benches/**/*.rs",
)

# `use path::to::thing;`, `pub use …`, `pub(crate) use …`. The whole statement
# is captured rather than only its first segment: what a statement binds is what
# a later statement may resolve through, and reading only the root loses that.
# `[^;]*` spans newlines, which a wrapped `use` group needs.
USE = re.compile(
    r"^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?use[ \t]+(?P<body>[^;]*);",
    re.MULTILINE,
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
    "crates/lgwks-bot/tests/rt_async_tier.rs:448": (
        "let joined = std::thread::spawn(move || handle.block_on(async { 5u8 })).join();",
        "the claim under test is that a `Handle` drives work from a thread the "
        "runtime did not create; the thread is joined on the same line, so "
        "nothing is leaked, and the file already carries an `#[expect]` saying so",
    ),
    "crates/lgwks-bot/tests/rt_process.rs:139": (
        'let path = std::env::temp_dir().join(format!("lgwks-bot-{}-{name}", std::process::id()));',
        "a scratch directory name, not an identity: the per-test `name` argument "
        "is what keeps two tests apart, and the pid only namespaces the directory "
        "under the temp root",
    ),
    "crates/lgwks-deps/src/vendor.rs:391": (
        "std::process::id(),",
        "a test fixture's directory name whose real discriminator is the `NEXT` "
        "atomic three lines above; the comment there records that the timestamp "
        "alone was already tried and was not sufficient",
    ),
    "crates/lgwks-deps/tests/check_cli.rs:435": (
        'std::env::temp_dir().join(format!("lgwks-deps-check-cli-{}-{tag}", std::process::id()));',
        "a scratch directory name, not an identity: the per-test `tag` is what "
        "keeps two tests apart",
    ),
}


def in_string_literal(line: str, column: int) -> bool:
    """Whether an offset on a line sits inside a `"…"` literal.

    A bare `name::` path can only be code, so the scan has to tell code from a
    string that happens to spell one. It matters here because
    `crates/lgwks-deps/src/scan.rs` *is* a source scanner: its pattern table
    contains `"tracing::"`, and reporting that string as a reach into `tracing`
    is the checker reporting a detector as the thing it detects.

    Odd unescaped quotes before the offset, which is true for the single-line
    strings a pattern table holds. It is honestly a heuristic and not a lexer: a
    raw string spanning lines, or a `"` inside a character literal, will confuse
    it. The failure direction is a missed finding rather than a false one only
    for text that looks like code and is not, which is the rarer case.
    """
    quotes = 0
    escaped = False
    for char in line[:column]:
        if escaped:
            escaped = False
        elif char == "\\":
            escaped = True
        elif char == '"':
            quotes += 1
    return quotes % 2 == 1


def split_top_level(text: str) -> list[str]:
    """Split a `use` group body on the commas outside nested braces."""
    parts: list[str] = []
    depth = 0
    current: list[str] = []
    for char in text:
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
        if char == "," and depth == 0:
            parts.append("".join(current))
            current = []
        else:
            current.append(char)
    parts.append("".join(current))
    return parts


def use_tree(body: str, prefix: str = "") -> tuple[set[str], set[str]]:
    """The crate roots a `use` specification reaches, and the names it binds.

    `use lgwks_std::json::serde;` reaches `lgwks_std` and binds `serde`.
    `use std::collections::{BTreeMap, BTreeSet};` reaches `std` and binds both.
    `use a::b::{self, c};` binds `b` as well as `c`, which is the arm a
    first-segment-only reading gets wrong.
    """
    body = body.strip()
    if not body:
        return set(), set()

    brace = body.find("{")
    if brace != -1 and body.endswith("}"):
        head = body[:brace].rstrip(":").strip()
        # The prefix is not decoration: in `use lgwks_deps::bevy_ecs::{
        # prelude::{…} }` the inner group's head is `prelude` only until the
        # outer path is put back in front of it, and a group read without its
        # prefix reports `prelude` and `schedule` as crates.
        if prefix:
            head = f"{prefix}::{head}" if head else prefix
        roots: set[str] = set()
        names: set[str] = set()
        for part in split_top_level(body[brace + 1 : -1]):
            part = part.strip()
            if not part:
                continue
            if part == "self":
                if head:
                    roots.add(head.split("::")[0])
                    names.add(head.split("::")[-1])
                continue
            sub_roots, sub_names = use_tree(part, head)
            roots |= sub_roots
            names |= sub_names
        return roots, names

    path = f"{prefix}::{body}" if prefix else body
    path = path.lstrip(":")
    if path.endswith("::*") or path == "*":
        root = path.split("::")[0]
        return ({root} if root else set()), set()

    if " as " in path:
        target, _, alias = path.rpartition(" as ")
        path = target.strip()
        name = alias.strip()
    else:
        name = path.split("::")[-1].strip()

    root = path.split("::")[0].strip()
    return ({root} if root else set()), ({name} if name else set())


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
    return {
        entry["crate"].replace("-", "_"): entry for entry in register.get("approved", [])
    }


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


def source_files(repo: Path) -> list[Path]:
    found: set[Path] = set()
    for pattern in SOURCE_GLOBS:
        found.update(repo.glob(pattern))
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
        names.update(m.group("name") for m in LOCAL_MODULE.finditer(cleaned_text(path)))
    return names


def justification(record: dict | None) -> str:
    """The one-line record behind an edge: who owns it and why it was approved."""
    if record is None:
        return "no record in contract/APPROVED.toml"
    return (
        f"owner={record.get('owner')} capability={record.get('capability')} "
        f"tier={record.get('tier')} — {record.get('reason')}"
    )


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
    args = parser.parse_args()
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

            # Every name this file binds, collected before any root is judged:
            # `use lgwks_std::json::serde;` at the top is what makes a later
            # `use serde::de::VariantAccess;` a path through `lgwks_std`.
            statements = list(USE.finditer(text))
            statements += list(EXTERN_CRATE.finditer(text))
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
                    rf"(?<![A-Za-z0-9_:]){re.escape(name)}::", text
                ):
                    number = line_of(text, match.start())
                    line_start = text.rfind("\n", 0, match.start()) + 1
                    line_text = text[line_start : text.find("\n", line_start)]
                    if in_string_literal(line_text, match.start() - line_start):
                        continue
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
                    f"{relative}:{line_of(text, match.start())} — UNDECLARED: "
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

    if findings:
        print(f"{len(findings)} finding(s):", file=sys.stderr)
        for finding in findings:
            print(f"  {finding}", file=sys.stderr)
        return 1

    print(
        f"std-first holds: {len(files)} source file(s) across {len(members)} crate(s); "
        f"{len(reached)} crate(s) reached past the clean roots "
        f"({', '.join(sorted(reached)) if reached else 'none'})"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
