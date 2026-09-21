#!/usr/bin/env python3
"""Check that every `path:NNN` citation in the documentation resolves.

The guides cite the source file and line a claim came from, e.g.
`crates/lgwks-bot/src/ecs.rs:1496`. Those citations are maintained by hand and
every merge moves the lines under them, so they rot silently: a page keeps
citing a line that a refactor deleted, and the reader has no way to tell an
accurate citation from a stale one.

This checks the part that is not a judgement call:

  * the cited file exists, and
  * the cited line exists in it, and is not blank.

It deliberately does **not** try to decide whether the line is the *right* one.
The guides point at a definition, at a doc-comment, or at a re-export depending
on what the claim is about, so "is this the symbol it names" is a human call.
Claiming to check it would produce false positives, which is how a check gets
switched off instead of fixed.

Citation roots are resolved against the repository root, so a citation is
written the way a reader would open it: `crates/…/src/ecs.rs:1496`.

Usage:
    python3 scripts/check-doc-citations.py [--repo PATH]

Exits non-zero, listing every unresolvable citation, when any fails.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

# `crates/lgwks-bot/src/ecs.rs:1496`, and the same with a range or a scatter of
# line numbers (`ecs.rs:1408-1411`). The extension set is what the guides cite.
CITATION = re.compile(
    r"(?P<path>[A-Za-z0-9_./-]+\.(?:rs|py|sh|toml|md|json|yml|yaml))"
    r":(?P<lines>\d+(?:\s*[-,]\s*\d+)*)"
)

# Where the documentation lives. `docs/` and every markdown file at the root
# and beside a crate, since the root README cites source too.
DOC_GLOBS = ("docs/**/*.md", "*.md", "crates/*/README.md", "skills/**/*.md")

# Documents that cite a *different* repository, and are therefore not checked.
#
# `docs/bevy-admission.md` records the admission of four Bevy crates and cites
# Bevy's own source — `crates/bevy_time/src/virt.rs:75`, `state/resources.rs:181`
# — which are deliberately paths in Bevy, not in this tree. Teaching the checker
# to guess that a `crates/...` path might belong to someone else would make it
# wrong about the repository's own `crates/` tree, so the exemption is named
# here instead. A page on this list is a page whose citations nothing verifies;
# keep it short.
EXTERNAL_CITATION_PAGES = {
    "docs/bevy-admission.md": "cites the Bevy repository, not this one",
}


def document_paths(repo: Path) -> list[Path]:
    found: set[Path] = set()
    for pattern in DOC_GLOBS:
        found.update(repo.glob(pattern))
    return sorted(found)


def line_numbers(spec: str) -> list[int]:
    """Every line number a citation spec names.

    `1408` is one line; `1408-1411` and `1408,1411` both name several, and a
    citation is bad if *any* of them is out of range.
    """
    numbers: list[int] = []
    for part in re.split(r"[-,]", spec):
        part = part.strip()
        if part:
            numbers.append(int(part))
    return numbers


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--repo",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
        help="repository root (default: the parent of this script)",
    )
    args = parser.parse_args()
    repo: Path = args.repo.resolve()

    # Cache file contents: a page often cites one file many times, and the same
    # file is cited from several pages.
    contents: dict[Path, list[str] | None] = {}
    failures: list[str] = []
    checked = 0
    cited_files: set[Path] = set()

    skipped: list[str] = []

    for document in document_paths(repo):
        relative_doc = document.relative_to(repo)
        if str(relative_doc) in EXTERNAL_CITATION_PAGES:
            skipped.append(str(relative_doc))
            continue

        text = document.read_text(encoding="utf-8")
        for match in CITATION.finditer(text):
            cited = (repo / match.group("path")).resolve()
            checked += 1
            cited_files.add(cited)

            if cited not in contents:
                try:
                    contents[cited] = cited.read_text(encoding="utf-8").splitlines()
                except (OSError, UnicodeDecodeError):
                    contents[cited] = None

            lines = contents[cited]
            if lines is None:
                failures.append(
                    f"{relative_doc}: {match.group(0)} — no such file"
                )
                continue

            for number in line_numbers(match.group("lines")):
                if number < 1 or number > len(lines):
                    failures.append(
                        f"{relative_doc}: {match.group(0)} — "
                        f"line {number} is outside the file (it has {len(lines)})"
                    )
                elif not lines[number - 1].strip():
                    failures.append(
                        f"{relative_doc}: {match.group(0)} — line {number} is blank"
                    )

    if failures:
        print(f"{len(failures)} unresolvable citation(s):", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        return 1

    summary = (
        f"doc citations resolve: {checked} citation(s) across "
        f"{len(cited_files)} file(s)"
    )
    if skipped:
        summary += f"; not checked: {', '.join(sorted(skipped))}"
    print(summary)
    return 0


if __name__ == "__main__":
    sys.exit(main())
