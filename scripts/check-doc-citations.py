#!/usr/bin/env python3
"""Check that every `path:NNN` citation in the documentation still resolves.

The guides cite the source file and line a claim came from, e.g.
`crates/lgwks-bot/src/ecs.rs:1496`. Those citations are maintained by hand and
every merge moves the lines under them, so they rot silently: a page keeps
citing a line that a refactor deleted, and the reader has no way to tell an
accurate citation from a stale one.

It checks two layers.

The structural layer needs no state:

  * the cited file exists,
  * the cited line exists in it, and is not blank, and
  * the cited line is not one that cannot support any claim at all.

The third is a narrow subset of "is this the right line", and it is worth
separating from the rest because it is mechanical: a bare closing brace, or an
empty `///` or `//!`, carries no content a sentence could have been written
from. A citation pointing at one is stale beyond argument, whatever the claim
says. This is the shape a long refactor leaves behind — the symbol moved, the
line number stayed, and the number now lands on the `}` that closed it.

The lock is the layer that needs state. `scripts/doc-citations.lock` records the
text of every line the documentation cites, and this fails when any of them
differs. That is the check the structural layer cannot make. When a cited file
grows, the numbers
below the insertion point each land on *some other plausible line*, so the
citation resolves, is not blank, and is not a delimiter, and the page reads as
green while pointing at unrelated code. On 2026-09-21 `ecs.rs` grew by 1,070
lines; three of twenty-five moved citations were flagged and the other
twenty-two passed while `failures.md` cited `ecs.rs:2196` for `Bot::tick` and
had come to point at `/// want three different answers`.

The lock does not decide whether a line is the *right* one, which stays a human
call: the guides point at a definition, at a doc-comment, or at a re-export
depending on what the claim is about. It decides whether the line is the *same*
one a person last read. `--update` writes it, and the diff is then the review:
every line whose text changed under a citation is listed, and re-anchoring is
the act of confirming each one still supports the sentence that cites it.

Citation roots are resolved against the repository root, so a citation is
written the way a reader would open it: `crates/…/src/ecs.rs:1496`.

Usage:
    python3 scripts/check-doc-citations.py             # verify
    python3 scripts/check-doc-citations.py --update     # rewrite the lock

Exits non-zero, listing every failure, when any citation fails.
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

# Where the reviewed text of every cited line is recorded.
LOCK_NAME = "scripts/doc-citations.lock"

LOCK_HEADER = """\
# The line each documentation citation points at, as it read when a person last
# checked that it supports the sentence citing it.
#
# One entry per cited line: `<source path>:<line>` TAB `<the line, stripped>`.
# Generated and checked by `scripts/check-doc-citations.py`; regenerate with
# `--update`, and read the diff. A line whose text changes here is a line under
# a citation that moved, and re-anchoring is confirming each one still supports
# its claim rather than rewriting the entry to match.
"""

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

# A line no claim can have been written from. Kept deliberately small: every
# entry here is a line with no content at all, so there is no reading of any
# sentence for which it is the right citation. Anything that could plausibly be
# the intended target — an attribute, a `use`, a fragment of a signature — is
# left alone rather than guessed at.
UNSUPPORTIVE = (
    (re.compile(r"^[}\]\);]+$"), "it is only a closing delimiter"),
    (re.compile(r"^///$"), "it is an empty `///`"),
    (re.compile(r"^//!$"), "it is an empty `//!`"),
    (re.compile(r"^//$"), "it is an empty comment"),
)


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


def unsupportive_reason(line: str) -> str | None:
    """Why `line` cannot be the source of a citation, or `None` if it can."""
    stripped = line.strip()
    for pattern, reason in UNSUPPORTIVE:
        if pattern.match(stripped):
            return reason
    return None


def lock_key(path: str, number: int) -> str:
    return f"{path}:{number}"


def lock_sort_key(key: str) -> tuple[str, int]:
    path, _, number = key.rpartition(":")
    return (path, int(number))


def read_lock(lock: Path) -> dict[str, str]:
    """The lock as `key -> cited line text`.

    Absent is not an error here. A repository whose lock has not been generated
    yet reports its citations as unpinned; see `main`.
    """
    entries: dict[str, str] = {}
    if not lock.exists():
        return entries
    for line in lock.read_text(encoding="utf-8").splitlines():
        if not line or line.startswith("#"):
            continue
        key, tab, text = line.partition("\t")
        if tab:
            entries[key.strip()] = text
    return entries


def write_lock(lock: Path, entries: dict[str, str]) -> None:
    body = "".join(
        f"{key}\t{entries[key]}\n" for key in sorted(entries, key=lock_sort_key)
    )
    lock.write_text(LOCK_HEADER + body, encoding="utf-8")


def collect(
    repo: Path,
) -> tuple[dict[str, str], dict[str, list[str]], list[str], list[str]]:
    """Walk the documentation.

    Returns the cited line text per key, the pages citing each key, the pages
    whose citations are not checked, and the structural failures.
    """
    entries: dict[str, str] = {}
    sites: dict[str, list[str]] = {}
    skipped: list[str] = []
    failures: list[str] = []

    # Cache file contents: a page often cites one file many times, and the same
    # file is cited from several pages.
    contents: dict[Path, list[str] | None] = {}

    for document in document_paths(repo):
        relative_doc = document.relative_to(repo)
        if str(relative_doc) in EXTERNAL_CITATION_PAGES:
            skipped.append(str(relative_doc))
            continue

        text = document.read_text(encoding="utf-8")
        for match in CITATION.finditer(text):
            cited = (repo / match.group("path")).resolve()
            if cited not in contents:
                try:
                    contents[cited] = cited.read_text(encoding="utf-8").splitlines()
                except (OSError, UnicodeDecodeError):
                    contents[cited] = None

            lines = contents[cited]
            if lines is None:
                failures.append(f"{relative_doc}: {match.group(0)} — no such file")
                continue

            for number in line_numbers(match.group("lines")):
                if number < 1 or number > len(lines):
                    failures.append(
                        f"{relative_doc}: {match.group(0)} — "
                        f"line {number} is outside the file (it has {len(lines)})"
                    )
                    continue
                cited_line = lines[number - 1]
                if not cited_line.strip():
                    failures.append(
                        f"{relative_doc}: {match.group(0)} — line {number} is blank"
                    )
                    continue
                reason = unsupportive_reason(cited_line)
                if reason is not None:
                    failures.append(
                        f"{relative_doc}: {match.group(0)} — line {number} is "
                        f"`{cited_line.strip()[:40]}`, which supports no claim: {reason}"
                    )
                    continue

                key = lock_key(match.group("path"), number)
                entries[key] = cited_line.strip()
                sites.setdefault(key, []).append(str(relative_doc))

    return entries, sites, skipped, failures


def report(failures: list[str], limit: int) -> None:
    shown = failures[:limit]
    for failure in shown:
        print(f"  {failure}", file=sys.stderr)
    if len(failures) > len(shown):
        print(f"  … and {len(failures) - len(shown)} more", file=sys.stderr)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--repo",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
        help="repository root (default: the parent of this script)",
    )
    parser.add_argument(
        "--update",
        action="store_true",
        help="rewrite the lock from the current citations, then verify it",
    )
    parser.add_argument(
        "--limit",
        type=int,
        default=40,
        help="how many failures to print (default: 40)",
    )
    args = parser.parse_args()
    repo: Path = args.repo.resolve()
    lock = repo / LOCK_NAME

    entries, sites, skipped, failures = collect(repo)

    if failures:
        print(f"{len(failures)} unresolvable citation(s):", file=sys.stderr)
        report(failures, args.limit)
        print(
            "\nThe lock was not touched: a citation that does not resolve is "
            "fixed by re-anchoring it, not by recording where it landed.",
            file=sys.stderr,
        )
        return 1

    locked = read_lock(lock)

    if args.update:
        moved = [
            key
            for key in sorted(entries, key=lock_sort_key)
            if key in locked and locked[key] != entries[key]
        ]
        added = [key for key in entries if key not in locked]
        dropped = [key for key in locked if key not in entries]
        if moved:
            print(f"{len(moved)} cited line(s) changed text:", file=sys.stderr)
            for key in moved[: args.limit]:
                cited_by = ", ".join(sorted(set(sites.get(key, []))))
                print(f"  {key}  (cited by {cited_by})", file=sys.stderr)
                print(f"    was: {locked[key][:100]}", file=sys.stderr)
                print(f"    now: {entries[key][:100]}", file=sys.stderr)
        if dropped:
            print(f"{len(dropped)} entry(ies) are no longer cited:", file=sys.stderr)
            for key in dropped[: args.limit]:
                print(f"  {key}", file=sys.stderr)
        write_lock(lock, entries)
        summary = (
            f"lock rewritten: {len(entries)} citation(s) pinned, "
            f"{len(added)} new, {len(moved)} re-anchored, {len(dropped)} dropped"
        )
        if skipped:
            summary += f"; not checked: {', '.join(sorted(skipped))}"
        print(summary)
        # Confirm the file just written verifies, so `--update` cannot leave the
        # tree in a state the next check rejects.
        return 0 if read_lock(lock) == entries else 1

    if not locked:
        print(
            f"no lock at {LOCK_NAME}: every citation is unpinned. Generate it "
            f"with `python3 {Path(__file__).name} --update`, and review the "
            "result before committing it.",
            file=sys.stderr,
        )
        return 1

    unpinned = [key for key in sorted(entries, key=lock_sort_key) if key not in locked]
    shifted = [
        key
        for key in sorted(entries, key=lock_sort_key)
        if key in locked and locked[key] != entries[key]
    ]
    stale = [key for key in sorted(locked, key=lock_sort_key) if key not in entries]

    if unpinned or shifted or stale:
        if unpinned:
            print(
                f"{len(unpinned)} citation(s) are not in the lock. Add them with "
                "`--update` once the line each one names is confirmed to support "
                "the sentence citing it:",
                file=sys.stderr,
            )
            for key in unpinned[: args.limit]:
                print(f"  {key}  {entries[key][:80]}", file=sys.stderr)
        if shifted:
            print(
                f"{len(shifted)} cited line(s) no longer hold the text they were "
                "pinned to. The number resolves, which is exactly why this is "
                "the check that catches a citation that moved under a grown "
                "file:",
                file=sys.stderr,
            )
            for key in shifted[: args.limit]:
                cited_by = ", ".join(sorted(set(sites.get(key, []))))
                print(f"  {key}  (cited by {cited_by})", file=sys.stderr)
                print(f"    pinned: {locked[key][:100]}", file=sys.stderr)
                print(f"    found:  {entries[key][:100]}", file=sys.stderr)
        if stale:
            print(
                f"{len(stale)} lock entry(ies) are cited by nothing. A citation "
                "was removed or renumbered without the lock being regenerated:",
                file=sys.stderr,
            )
            for key in stale[: args.limit]:
                print(f"  {key}  {locked[key][:80]}", file=sys.stderr)
        print(
            "\nRe-anchor by content: find where the cited text lives now and "
            "point the citation at it, rather than shifting the number by the "
            "line delta. Then run `--update`.",
            file=sys.stderr,
        )
        return 1

    summary = (
        f"doc citations resolve: {len(entries)} line(s) pinned across "
        f"{len({page for pages in sites.values() for page in pages})} page(s), "
        "all unchanged"
    )
    if skipped:
        summary += f"; not checked: {', '.join(sorted(skipped))}"
    print(summary)
    return 0


if __name__ == "__main__":
    sys.exit(main())
