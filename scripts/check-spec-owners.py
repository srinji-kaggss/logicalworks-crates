#!/usr/bin/env python3
"""Every requirement a spec defines has an owner: a falsifier row that requires it.

A requirement written as `**DX-07.**` in a `docs/*.spec.md` page is a claim
about the public interface. It is owned when some row of
`docs/acceptance/t-rows.toml` names it in `requires`, because that row's tests
are what the `Acceptance receipts` job records per revision. A requirement no
row requires is unowned: nothing would notice it drifting (#420, where the
declarative spec still pointed at its closed tracker #87).

Only the families listed in `CHECKED` are refused today; a family joins when
each of its ids is owned or carries a reason in `EXEMPT`. A reason names what
holds the requirement instead of a row, so an exemption is a statement a
reviewer can check, never a bare allowlist entry.

Usage:
  python3 scripts/check-spec-owners.py          # exit 1 on an unowned requirement
  python3 scripts/check-spec-owners.py --test
"""

from __future__ import annotations

import re
import sys
import tomllib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFINITION = re.compile(r"\*\*([A-Z]{2,3})-(\d{2})\.\*\*")
# The families whose every id must be owned. AI, LC and PR join when their
# unowned ids (AI-01/08/09, LC-14/15/16, PR-01) are owned or given a reason.
CHECKED = ("DX",)
EXEMPT = {
    "DX-02": "a vocabulary (Task, Host, Report), not a behaviour: T01 drives exactly those three names",
    "DX-12": "a release discipline (published API changes, compiled examples): held by the "
    "readme-quickstart lane and the CHANGELOG, not by a falsifier row",
}


def defined(pages: list[Path]) -> dict[str, str]:
    """Every requirement id defined in `pages`, with the page that defines it."""
    found: dict[str, str] = {}
    for page in pages:
        for family, number in DEFINITION.findall(page.read_text(encoding="utf-8")):
            found.setdefault(f"{family}-{number}", page.name)
    return found


def required(rows: list[dict]) -> set[str]:
    """Every id the rows require; `DX-01/03/04` names DX-01, DX-03 and DX-04.

    A bare number takes the family of the nearest id before it, so
    `DX-05/LC-08` names DX-05 and LC-08, never DX-08.
    """
    ids: set[str] = set()
    for row in rows:
        family = ""
        for part in re.split(r"[,;/]\s*", str(row.get("requires", ""))):
            token = re.fullmatch(r"(?:([A-Z]{2,3})-)?(\d{2})", part.strip())
            if token is None:
                family = ""
                continue
            family = token.group(1) or family
            if family:
                ids.add(f"{family}-{token.group(2)}")
    return ids


def unowned(found: dict[str, str], owned: set[str]) -> list[str]:
    """The checked ids no row requires and no reason exempts, in order."""
    return sorted(
        key
        for key in found
        if key.split("-")[0] in CHECKED and key not in owned and key not in EXEMPT
    )


class SpecOwnerRegression(unittest.TestCase):
    """The check against a known map, and against this repository."""

    def test_a_slash_list_names_every_id(self) -> None:
        self.assertEqual(required([{"requires": "DX-01/03/04"}]), {"DX-01", "DX-03", "DX-04"})
        self.assertEqual(required([{"requires": "DX-05/LC-08"}]), {"DX-05", "LC-08"})
        # A part that is not an id breaks the run: a number after it has no family.
        self.assertEqual(required([{"requires": "DX-01/see below/05"}]), {"DX-01"})

    def test_an_unrequired_id_is_reported_and_an_owned_one_is_not(self) -> None:
        found = {"DX-98": "a.spec.md", "DX-01": "a.spec.md", "DX-99": "a.spec.md", "LC-99": "b.spec.md"}
        self.assertEqual(unowned(found, {"DX-01"}), ["DX-98", "DX-99"])

    def test_an_exempt_id_is_not_reported(self) -> None:
        self.assertEqual(unowned({"DX-02": "a.spec.md", "DX-12": "a.spec.md"}, set()), [])

    def test_every_exemption_names_a_defined_id(self) -> None:
        found = defined(sorted((ROOT / "docs").glob("*.spec.md")))
        self.assertEqual(sorted(set(EXEMPT) - set(found)), [])

    def test_this_repository_owns_every_checked_requirement(self) -> None:
        self.assertEqual(main([]), 0)


def main(argv: list[str]) -> int:
    if argv == ["--test"]:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(SpecOwnerRegression)
        return 0 if unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful() else 1
    found = defined(sorted((ROOT / "docs").glob("*.spec.md")))
    rows = tomllib.loads((ROOT / "docs/acceptance/t-rows.toml").read_text(encoding="utf-8"))["row"]
    missing = unowned(found, required(rows))
    checked = [key for key in found if key.split("-")[0] in CHECKED]
    if missing:
        for key in missing:
            print(f"spec owners: {key} ({found[key]}) is required by no row of docs/acceptance/t-rows.toml")
        return 1
    print(
        f"spec owners: {len(checked)} {'/'.join(CHECKED)} requirement(s), each required by a "
        f"falsifier row or exempt with a reason ({len(EXEMPT)} exempt)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
