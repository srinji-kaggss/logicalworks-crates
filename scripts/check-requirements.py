#!/usr/bin/env python3
"""Check that `REQUIREMENTS.md` has not drifted without a recorded supersession.

The product requirements are the repository's normative spine. A requirement is
superseded, never edited, so the gate needs a memory of what each normative
sentence said the last time a person decided it. That memory is
`scripts/requirements.lock`: one entry per requirement id, holding a short
digest of its normative sentence and the sentence itself.

The check is deliberately narrow. It does not judge whether a requirement is
good, complete, or true. It answers one question: *has any normative sentence
changed since it was last recorded, and if so, did someone say so out loud?*

A mismatch is not silently repaired. `--update` rewrites the lock, but only for
requirements whose id appears in the Supersession log section of
`REQUIREMENTS.md`. An edit with no log entry is refused, which is the whole
point: the log is permanent and is the only evidence a requirement moved.

Usage:
    python3 scripts/check-requirements.py             # verify
    python3 scripts/check-requirements.py --update    # rewrite the lock

Exit status is non-zero on any mismatch or on a refused update.
"""

from __future__ import annotations

import argparse
import hashlib
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
REQUIREMENTS = ROOT / "REQUIREMENTS.md"
LOCK = ROOT / "scripts" / "requirements.lock"

LOCK_NAME = "scripts/requirements.lock"

LOCK_HEADER = """\
# The normative sentence of each product requirement, as it read when a person
# last decided it. `REQUIREMENTS.md` is superseded, never edited, so this lock
# is the memory the gate checks an edit against.
#
# One entry per requirement: `R<id>` TAB `<sha256, first 12 hex>` TAB
# `<the normative sentence, whitespace-collapsed>`.
# Generated and checked by `scripts/check-requirements.py`. A changed digest
# with no Supersession log entry naming that id is refused; record the
# supersession first, then `--update`.
"""

# A requirement heading: `### R12. Human and in-real-life mess is in scope...`
HEADING = re.compile(r"^###\s+R(?P<id>\d+)\.\s+(?P<title>.+?)\s*$")

# A requirement's normative sentence is its opening blockquote. The block runs
# from the first `>` line after the heading to the first line that is neither a
# continuation nor blank-then-more-quote.
BLOCKQUOTE_LINE = re.compile(r"^\s*>\s?(?P<body>.*)$")

# Supersession log section, so `--update` can demand a receipt per change.
LOG_HEADING = re.compile(r"^##\s+Supersession log\s*$")
LOG_ENTRY = re.compile(r"\bR(?P<id>\d+)\b")


def normative_sentence(block_lines: list[str]) -> str:
    """Collapse a blockquote to one comparable sentence.

    Whitespace collapses because the source is hard-wrapped and the lock should
    record the words a person chose rather than the column they wrapped at.
    """
    return " ".join(" ".join(block_lines).split())


def parse_requirements(text: str) -> tuple[dict[int, str], set[int]]:
    """Return `{id: normative sentence}` and the set of ids in the supersession log.

    An id appearing in the log is a receipt that the requirement moved. The log
    is scanned across the whole document after its heading so a reason written
    in prose still counts as naming the requirement.
    """
    lines = text.splitlines()
    sentences: dict[int, str] = {}
    logged: set[int] = set()

    index = 0
    while index < len(lines):
        line = lines[index]

        if LOG_HEADING.match(line):
            index += 1
            while index < len(lines) and not lines[index].startswith("## "):
                for match in LOG_ENTRY.finditer(lines[index]):
                    logged.add(int(match.group("id")))
                index += 1
            continue

        heading = HEADING.match(line)
        if not heading:
            index += 1
            continue

        req_id = int(heading.group("id"))
        index += 1

        # Skip blanks between the heading and its blockquote.
        while index < len(lines) and not lines[index].strip():
            index += 1

        quote: list[str] = []
        while index < len(lines):
            quoted = BLOCKQUOTE_LINE.match(lines[index])
            if quoted:
                quote.append(quoted.group("body"))
                index += 1
                continue
            # A hard-wrapped quote continues on an unquoted blank line only if
            # the following line is quoted again; otherwise the block is over.
            if not lines[index].strip() and index + 1 < len(lines):
                if BLOCKQUOTE_LINE.match(lines[index + 1]):
                    quote.append("")
                    index += 1
                    continue
            break

        if not quote:
            raise SystemExit(
                f"REQUIREMENTS.md: R{req_id} has no normative blockquote. "
                "A requirement without a `>` sentence is not written yet."
            )

        sentences[req_id] = normative_sentence(quote)

    return sentences, logged


def parse_lock(text: str) -> dict[int, tuple[str, str]]:
    """Return `{id: (digest, sentence)}` from the lock file."""
    entries: dict[int, tuple[str, str]] = {}
    for line_no, line in enumerate(text.splitlines(), start=1):
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        parts = line.split("\t", 2)
        if len(parts) != 3:
            raise SystemExit(
                f"{LOCK_NAME}:{line_no}: expected `R<id> TAB digest TAB sentence`"
            )
        key, digest, sentence = parts
        match = re.fullmatch(r"R(\d+)", key.strip())
        if not match:
            raise SystemExit(f"{LOCK_NAME}:{line_no}: bad requirement id {key!r}")
        entries[int(match.group(1))] = (digest.strip(), sentence)
    return entries


def digest_of(sentence: str) -> str:
    return hashlib.sha256(sentence.encode("utf-8")).hexdigest()[:12]


def write_lock(sentences: dict[int, str]) -> None:
    rows = [LOCK_HEADER]
    for req_id in sorted(sentences):
        sentence = sentences[req_id]
        rows.append(f"R{req_id}\t{digest_of(sentence)}\t{sentence}\n")
    LOCK.write_text("".join(rows), encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--update",
        action="store_true",
        help="rewrite the lock, refusing any change with no Supersession log entry",
    )
    args = parser.parse_args()

    if not REQUIREMENTS.is_file():
        print("REQUIREMENTS.md is missing", file=sys.stderr)
        return 1

    text = REQUIREMENTS.read_text(encoding="utf-8")
    sentences, logged = parse_requirements(text)

    if not sentences:
        print("REQUIREMENTS.md declares no requirements", file=sys.stderr)
        return 1

    if not LOCK.is_file():
        if args.update:
            write_lock(sentences)
            print(f"wrote {LOCK_NAME} — {len(sentences)} requirement(s)")
            return 0
        print(
            f"{LOCK_NAME} is missing. Run with --update to record the current text.",
            file=sys.stderr,
        )
        return 1

    locked = parse_lock(LOCK.read_text(encoding="utf-8"))

    problems: list[str] = []
    changed: list[int] = []

    for req_id in sorted(sentences):
        sentence = sentences[req_id]
        if req_id not in locked:
            problems.append(f"R{req_id}: present in REQUIREMENTS.md, absent from the lock")
            changed.append(req_id)
            continue
        old_digest, old_sentence = locked[req_id]
        if digest_of(sentence) != old_digest or sentence != old_sentence:
            problems.append(
                f"R{req_id}: normative sentence changed\n"
                f"    was: {old_sentence}\n"
                f"    now: {sentence}"
            )
            changed.append(req_id)

    for req_id in sorted(locked):
        if req_id not in sentences:
            problems.append(f"R{req_id}: present in the lock, absent from REQUIREMENTS.md")
            changed.append(req_id)

    unlogged = sorted(set(changed) - logged)

    if problems:
        for problem in problems:
            print(problem, file=sys.stderr)
        if unlogged:
            names = ", ".join(f"R{i}" for i in unlogged)
            print(
                f"\n{len(unlogged)} change(s) have no Supersession log entry: {names}.\n"
                "A requirement is superseded, never edited. Add an entry to the "
                "Supersession log in REQUIREMENTS.md naming the id and why the "
                "previous statement was wrong or incomplete, then re-run with "
                "--update.",
                file=sys.stderr,
            )
            return 1
        if args.update:
            write_lock(sentences)
            print(f"updated {LOCK_NAME} — {len(changed)} supersession(s) recorded")
            return 0
        print(
            "\nEvery change has a Supersession log entry. Re-run with --update to "
            "record the new text.",
            file=sys.stderr,
        )
        return 1

    if args.update:
        print(f"{LOCK_NAME} already matches REQUIREMENTS.md; nothing to do")
        return 0

    print(f"requirements: {len(sentences)} declared, all digests match the lock")
    return 0


if __name__ == "__main__":
    sys.exit(main())
