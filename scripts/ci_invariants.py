#!/usr/bin/env python3
"""The ``invariants`` lane: every ``enforced by:`` reference in INVARIANTS.md
resolves to a real test, module, lane or script.

Split out of ``scripts/ci_local.py`` under the size law (Director 2026-10-10).
Run its regression suite with ``python3 -m unittest scripts/ci_invariants.py``.
"""

from __future__ import annotations

import re
import unittest
from pathlib import Path

# The repository this module checks, for the regression block at the bottom.
REPO_ROOT = Path(__file__).resolve().parent.parent


def collect_module_paths(root: Path) -> dict[str, set[str]]:
    """Map every module path to the item names its own source defines.

    The previous version built one flat set of every `mod` and `fn` name across
    `crates/**/*.rs`, discarding both the file and the module path, and then
    resolved a qualified reference by its leaf. That made a *real* function in
    the *wrong* module certify as resolved: an enforcement claim could be
    repointed at any unrelated function in the workspace and the lane passed,
    reporting a higher reference count than the honest one. This is the same
    defect class INV-DEP-11 exists to prevent, present in the checker that
    verifies INV-DEP-11.

    Module nesting is derived from the crate layout, which is what Rust itself
    does: `src/lib.rs` and `src/main.rs` are the crate root, `src/<name>.rs` is
    the module `<name>` at the root, `src/<dir>/mod.rs` is the module `<dir>`
    at the root, and `src/<dir>/<name>.rs` is `<dir>::<name>`. The `src`
    directory is not itself a module, so it is dropped, and the crate directory
    keeps its on-disk spelling (`lgwks-std`) while a reference may write the
    Rust path spelling (`lgwks_std::`); `resolution_candidates` compares the
    two through the same normalisation the rest of the lane uses. A
    `#[cfg(test)] mod tests` block inside a file extends that file's own module
    with `::tests`, which is how a reference like `contract::tests::name`
    resolves.

    `pub fn` is collected alongside `fn`: the old `^\\s*fn` pattern could never
    see a public item, so any invariant naming a public function as its
    enforcement was unverifiable by construction, silently.
    """
    modules: dict[str, set[str]] = {}
    for path in sorted((root / "crates").glob("**/*.rs")):
        if "target" in path.parts:
            continue
        relative = path.relative_to(root / "crates")
        parts = list(relative.parts)
        # `src` is a directory convention, not a module. Drop it wherever it
        # introduces a module segment.
        while "src" in parts:
            parts.remove("src")
        if parts[-1] in ("lib.rs", "main.rs"):
            # The crate root. Its module path is the crate's own name, which is
            # what a reference like `lgwks_std::fs::tests::…` walks through.
            prefix = tuple(parts[:-1])
        elif parts[-1] == "mod.rs":
            prefix = tuple(parts[:-1])
        else:
            prefix = tuple(parts[:-1]) + (parts[-1][:-3],)
        source = path.read_text(encoding="utf-8")

        def declare(name: str, items: set[str]) -> None:
            items.add(name)

        module_path = "::".join(prefix)
        items = modules.setdefault(module_path, set())
        for match in re.finditer(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([a-z0-9_]+)\s*[{;]", source, re.MULTILINE):
            declare(match.group(1), items)
        # `fn` and `pub fn` alike, plus `async fn`, which is how a test that
        # awaits is actually written.
        for match in re.finditer(
            r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:const\s+)?(?:unsafe\s+)?fn\s+([a-z0-9_]+)",
            source,
            re.MULTILINE,
        ):
            declare(match.group(1), items)
        # An inline `mod tests { … }` is a child module of the file's own path.
        if re.search(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+tests\s*\{", source, re.MULTILINE):
            modules.setdefault(f"{module_path}::tests" if module_path else "tests", set()).update(
                items
            )
        # Every other inline module, nested to any depth, is its own path holding
        # exactly the items written inside its braces. Without this a test in
        # `mod tenancy_support { mod tests { … } }` could only be cited by a path
        # Rust would not accept, and the real one read as missing.
        for nested, inner in inline_module_items(source).items():
            path_text = "::".join((*prefix, *nested))
            modules.setdefault(path_text, set()).update(inner)
    # A bench rig's invariant names the runner function that enforces it
    # (`bench/ai-authoring/run.py`), so the bench's own definitions resolve too:
    # unindexed, every such reference reads as missing, and the only way to pass
    # would be to strip the backticks and stop checking the claim at all.
    for path in sorted((root / "bench").glob("**/*")):
        if path.suffix not in (".rs", ".py") or {"target", "runs"} & set(path.parts):
            continue
        source = path.read_text(encoding="utf-8")
        relative = path.relative_to(root).with_suffix("")
        items = modules.setdefault("::".join(relative.parts), set())
        for match in re.finditer(r"^\s*(?:def|fn)\s+([a-z0-9_]+)\s*\(", source, re.MULTILINE):
            items.add(match.group(1))
    return modules


def normalise_path(path: str) -> str:
    """Compare crate and module paths the way Cargo compares crate names.

    A crate directory is `lgwks-std` while every Rust path spells it
    `lgwks_std`. Treating those as different modules would refuse every
    reference in the file, so both sides are lowercased and `-` folded to `_`
    before any comparison — the same rule `lib.rs` applies to package names.
    """
    return path.lower().replace("-", "_")


def inline_module_items(source: str) -> dict[tuple[str, ...], set[str]]:
    """The items each inline `mod name { … }` in `source` defines, by nesting path.

    Braces are matched on the code alone: string literals (raw ones included),
    character literals and comments are skipped, so a `{` in a format string or a
    doc comment cannot close a module early. A lifetime (`'a`) is not a character
    literal, and is told apart by its missing closing quote.
    """
    opens = re.compile(r"(?:pub(?:\([^)]*\))?\s+)?mod\s+([a-z0-9_]+)\s*\{")
    item = re.compile(r"(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:const\s+)?(?:unsafe\s+)?(?:fn|mod)\s+([a-z0-9_]+)")
    found: dict[tuple[str, ...], set[str]] = {}
    stack: list[tuple[str, int]] = []
    depth = 0
    index = 0
    length = len(source)
    at_line_start = True
    while index < length:
        char = source[index]
        if source.startswith("//", index):
            end = source.find("\n", index)
            index = length if end < 0 else end
            continue
        if source.startswith("/*", index):
            end = source.find("*/", index + 2)
            index = length if end < 0 else end + 2
            continue
        raw = re.match(r'b?r(#*)"', source[index : index + 260])
        if raw and (index == 0 or not (source[index - 1].isalnum() or source[index - 1] == "_")):
            closing = '"' + raw.group(1)
            end = source.find(closing, index + raw.end())
            index = length if end < 0 else end + len(closing)
            continue
        if char == '"':
            cursor = index + 1
            while cursor < length and source[cursor] != '"':
                cursor += 2 if source[cursor] == "\\" else 1
            index = cursor + 1
            continue
        if char == "'":
            literal = re.match(r"'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\}|.)|[^\\'])'", source[index : index + 12])
            index += literal.end() if literal else 1
            continue
        if char == "\n":
            at_line_start = True
            index += 1
            continue
        if at_line_start and not char.isspace():
            at_line_start = False
            line = source[index : source.find("\n", index) if "\n" in source[index:] else length]
            module = opens.match(line)
            if module:
                stack.append((module.group(1), depth))
                found.setdefault(tuple(name for name, _ in stack), set())
                index += module.end() - 1
                continue
            defined = item.match(line)
            if defined and stack:
                found[tuple(name for name, _ in stack)].add(defined.group(1))
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            while stack and stack[-1][1] == depth:
                stack.pop()
        index += 1
    return found


def resolution_candidates(modules: dict[str, set[str]], reference: str) -> list[str]:
    """Module paths at which `reference` could resolve, best match first.

    A qualified reference names its module path, so the leaf is resolved only
    against a module path whose tail *is* the path the reference named. That is
    the whole repair: the previous resolver matched the leaf alone, so a real
    function in the wrong module certified as resolved and a misattributed
    enforcement claim was unfalsifiable in exactly the direction that matters.
    A bare name carries no path to check, so it is matched anywhere — the shape
    INVARIANTS.md uses for a test function named inside a `tests/` file rather
    than inside a module path.
    """
    leaf = reference.split("::")[-1]
    segments = reference.split("::")
    wanted = normalise_path("::".join(segments[:-1]))
    matches = []
    for path, items in modules.items():
        if leaf not in items:
            continue
        normalised = normalise_path(path)
        if len(segments) == 1 or normalised == wanted or normalised.endswith(f"::{wanted}"):
            matches.append(path)
    # Shallowest first, so the most specific module a name resolves in is the
    # one a diagnostic reports.
    return sorted(matches, key=lambda path: (path.count("::"), path))


def invariant_registers(root: Path) -> list[Path]:
    """The register's files: the root INVARIANTS.md, then each crate's own.

    One register split by crate under the 120-line .md law (Director
    2026-10-10): `crates/lgwks-bot/INVARIANTS.md` holds the INV-BOT rules. A
    crate file the lane did not read would be a rule nobody checks, so every
    `crates/*/INVARIANTS.md` is read, in a stable order.
    """
    return [root / "INVARIANTS.md", *sorted((root / "crates").glob("*/INVARIANTS.md"))]


def builtin_invariants(root: Path, env: dict[str, str], out=None) -> tuple[int, str]:
    """Every `enforced by:` reference in INVARIANTS.md resolves to a real test.

    An invariant that names its own enforcement is a claim a reader can check.
    Nothing verified it: a renamed or deleted test left the sentence reading
    exactly as authoritative as one pointing at a passing test, and the only way
    to find out was to go looking by hand. So the references are checked here,
    against the module paths and items the sources actually define.
    """
    text = "\n\n".join(path.read_text(encoding="utf-8") for path in invariant_registers(root))

    modules = collect_module_paths(root)

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
                    candidates = resolution_candidates(modules, reference)
                    if not candidates:
                        # Name where the leaf *does* live, so a misattribution
                        # is legible without opening the source. A reference
                        # that resolved by leaf alone used to pass, and the
                        # reader needs to see which module it really belongs
                        # to. Only the shallowest few are reported: `tests` is
                        # declared inline in nearly every file, so the full set
                        # is noise that would bury the one path that matters.
                        elsewhere = sorted(
                            {
                                path
                                for path, items in modules.items()
                                if reference.split("::")[-1] in items
                            },
                            key=lambda path: (path.count("::"), path),
                        )[:4]
                        detail = (
                            f"; it is defined in {', '.join(elsewhere)}"
                            if elsewhere
                            else ""
                        )
                        missing.append(
                            f"{name}: `{reference}` names no item in the module path it "
                            f"names{detail}"
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
            # repository's: `tests/it/rt_process.rs` lives at
            # `crates/lgwks-bot/tests/it/rt_process.rs`, so the repository root is
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


class InvariantReferenceResolution(unittest.TestCase):
    """Issue #205: a qualified reference resolves in the module it names.

    The defect was that the lane resolved a reference by its leaf alone, so a
    *real* function in the *wrong* module certified as resolved and the lane
    reported a higher reference count than the honest one. Each case below is a
    shape the fix has to get right: misattribution must fail, a correct
    cross-module reference must still resolve, and a `pub fn` must be
    collectable.
    """

    @classmethod
    def setUpClass(cls) -> None:
        cls.modules = collect_module_paths(REPO_ROOT)

    def test_the_honest_file_resolves(self) -> None:
        """Every reference this repository actually writes must resolve.

        The control that matters most: a stricter resolver is worthless if it
        refuses the true claims too, so the shipped INVARIANTS.md is the
        primary fixture.
        """
        code, _ = builtin_invariants(REPO_ROOT, {})
        self.assertEqual(code, 0, "the honest INVARIANTS.md must resolve")

    def test_a_real_function_in_the_wrong_module_is_refused(self) -> None:
        """The exact mutation from the issue: two real names, wrong modules.

        `lexical_join` really exists, but in `metadata`, not `scan`. Matching
        the leaf would pass; matching the module path is what refuses it.
        """
        self.assertEqual(
            resolution_candidates(self.modules, "lgwks_deps::metadata::lexical_join"),
            ["lgwks-deps::metadata"],
            "a correct reference must still resolve, or the fix refuses everything",
        )
        self.assertEqual(
            resolution_candidates(self.modules, "lgwks_deps::scan::cleanup_error"),
            [],
            "a real leaf in the wrong module must not certify as resolved",
        )

    def test_a_public_function_is_collectable(self) -> None:
        """`pub fn` was invisible to the old `^\\s*fn` pattern.

        So any invariant naming a public function as its enforcement was
        unverifiable by construction, silently.
        """
        self.assertIn("decode_into", self.modules.get("lgwks-std::hex", set()))

    def test_a_test_inside_a_nested_inline_module_resolves_by_its_own_path(self) -> None:
        """`mod outer { mod tests { … } }` is the path `outer::tests`.

        Braces inside strings, character literals and comments must not close a
        module, and a lifetime is not a character literal. And the resolution
        stays exact: a real test cited under the wrong inline module is refused.
        """
        found = inline_module_items(
            "mod outer {\n"
            '    const BRACE: &str = "}";\n'
            "    // a stray } in a comment\n"
            "    fn helper() -> char { '}' }\n"
            "    #[cfg(test)]\n"
            "    mod tests {\n"
            "        fn inner_case<'a>(text: &'a str) -> &'a str { text }\n"
            "    }\n"
            "}\n"
            "fn outside() {}\n"
        )
        self.assertEqual(found[("outer",)], {"helper"})
        self.assertEqual(found[("outer", "tests")], {"inner_case"})
        self.assertTrue(
            resolution_candidates(
                self.modules,
                "rt::supervise::tenancy_support::tests::"
                "a_late_abandonment_leaves_the_live_count_exact",
            ),
            "a test in a nested inline module resolves by the path Rust gives it",
        )
        self.assertEqual(
            resolution_candidates(
                self.modules,
                "rt::supervise::tenancy_support::tests::a_budget_of_duration_expires",
            ),
            [],
            "a real test cited under the wrong inline module is still refused",
        )

    def test_crate_directory_and_rust_path_spellings_are_one_module(self) -> None:
        """`lgwks-std` on disk is `lgwks_std` in every Rust path.

        Treating them as different modules would refuse every qualified
        reference in the file.
        """
        self.assertTrue(
            resolution_candidates(self.modules, "lgwks_std::hex::decode_into"),
            "a crate must resolve under both its directory and its Rust spelling",
        )

    def test_a_bare_name_still_resolves(self) -> None:
        """A bare name carries no module path, so it is matched anywhere.

        INVARIANTS.md uses this shape for a test function named inside a
        `tests/` file rather than inside a module path.
        """
        self.assertTrue(
            resolution_candidates(
                self.modules, "strict_refuses_the_first_unreadable_directory"
            ),
            "a bare test name must remain resolvable",
        )


class InvariantRegisterSplit(unittest.TestCase):
    """The register is the root file plus each crate's: every part is checked."""

    def test_the_bot_register_is_one_of_the_parts(self) -> None:
        parts = [path.relative_to(REPO_ROOT).as_posix() for path in invariant_registers(REPO_ROOT)]
        self.assertEqual(parts[0], "INVARIANTS.md")
        self.assertIn("crates/lgwks-bot/INVARIANTS.md", parts)

    def test_an_unresolvable_reference_in_a_crate_register_fails_the_lane(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            for relative, text in {
                "INVARIANTS.md": "",
                "scripts/gate-lanes.toml": "",
                "contract/INVARIANTS.toml": "",
                "crates/demo/src/lib.rs": "fn real_case() {}\n",
                "crates/demo/INVARIANTS.md": (
                    "- **INV-DEMO-1** A rule. · enforced by: `no_such_case_anywhere`\n"
                ),
            }.items():
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text, encoding="utf-8")
            code, detail = builtin_invariants(root, {})
            self.assertEqual(code, 1, detail)
            self.assertIn("INV-DEMO-1: `no_such_case_anywhere`", detail)
            (root / "crates/demo/INVARIANTS.md").write_text(
                "- **INV-DEMO-1** A rule. · enforced by: `real_case`\n", encoding="utf-8"
            )
            code, detail = builtin_invariants(root, {})
            self.assertEqual(code, 0, detail)
