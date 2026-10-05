#!/usr/bin/env python3
"""Run the acceptance tests named in `docs/acceptance/t-rows.toml` and record the result.

Every acceptance row in `docs/orchestration-acceptance.spec.md` names the tests
that address it. That claim is only worth something if someone can check it
against a real run, so this script is the check: it reads the map, selects
exactly the named tests, runs them once, and writes what actually happened to
`evidence/<revision>.json`.

One receipt per revision, not one per row. A row that a receipt cannot find is a
row whose claim is weaker than the map says, and the receipt says so in the row's
own entry rather than in a summary the reader has to cross-reference.

The receipt is committed under `evidence/`, and the spec's per-row table is
*generated from it* rather than written by hand: `--write-table` rewrites the
block between the two marker comments in the spec, and `--check` compares the
committed block against what the map and the receipt in the tree would render.
A hand-edited table that no run produced is a claim with nothing behind it, so
`--check` is the gate that makes the table evidence rather than prose.

Usage:
    python3 scripts/acceptance-receipts.py [--map docs/acceptance/t-rows.toml]
                                         [--revision <sha>] [--out evidence]
                                         [--package lgwks_bot]
                                         [--features full]
                                         [--write-table] [--check]

Exit status is non-zero when a named test is missing from the run, when a named
test fails, or when a row's state claims more than the observed evidence, so the
same command works as a gate.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
import tomllib
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_MAP = Path("docs/acceptance/t-rows.toml")
DEFAULT_OUT = Path("evidence")
DEFAULT_SPEC = Path("docs/orchestration-acceptance.spec.md")

# The generated block's own delimiters, so `--write-table` and `--check` agree
# on exactly which lines belong to the run rather than to the prose around them.
TABLE_START = "<!-- acceptance-table: start -->"
TABLE_END = "<!-- acceptance-table: end -->"

# States that claim the row's falsifier is actually driven, in the order the
# spec's evidence ladder climbs. A receipt may only confirm a state at or below
# what the map claims; it can never promote one on its own.
VERIFIED_STATES = frozenset({"exercised", "independently_evidenced", "accepted"})


class Failure(Exception):
    """A condition that makes the receipt untrustworthy, named rather than guessed."""


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--map", default=str(DEFAULT_MAP), help="path to the row map")
    parser.add_argument("--revision", default=None, help="revision the receipt describes")
    parser.add_argument("--out", default=str(DEFAULT_OUT), help="directory for the receipt")
    parser.add_argument("--package", default="lgwks_bot", help="package under test")
    parser.add_argument("--features", default="full", help="features under test")
    parser.add_argument(
        "--spec", default=str(DEFAULT_SPEC), help="the document holding the generated table"
    )
    parser.add_argument(
        "--write-table",
        action="store_true",
        help="rewrite the spec's generated table from this run's receipt",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="exit non-zero when the committed table disagrees with the map and the receipt",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="print the selected filter and exit without running anything",
    )
    return parser.parse_args(argv)


def load_rows(map_path: Path) -> tuple[dict, list[dict]]:
    """Read the map and return its header plus every row, in file order."""
    if not map_path.is_file():
        raise Failure(f"row map not found: {map_path}")
    payload = tomllib.loads(map_path.read_text(encoding="utf-8"))
    rows = payload.get("row")
    if not isinstance(rows, list) or not rows:
        raise Failure(f"row map has no [[row]] entries: {map_path}")
    seen: set[str] = set()
    for row in rows:
        rid = str(row.get("id", ""))
        if not re.fullmatch(r"T\d{2}", rid):
            raise Failure(f"row id is not TNN: {rid!r}")
        if rid in seen:
            raise Failure(f"row id appears twice: {rid}")
        seen.add(rid)
        if not isinstance(row.get("tests"), list):
            raise Failure(f"{rid} has no tests list")
        if not str(row.get("state", "")):
            raise Failure(f"{rid} has no state")
    return payload, rows


def select_name(entry: str) -> str:
    """Reduce a map entry to the test name nextest's filter matches.

    The map writes every test as `<binary>::<test>`, and nextest matches its
    `test(/re/)` filter against the test alone. A Rust test name never contains
    `$`, and a nextest name never contains a space, so those separators are
    trustworthy when they appear on the reported side.
    """
    head, separator, rest = entry.partition("::")
    return rest if separator else entry


def names_for(entry: str) -> tuple[str, ...]:
    """Every spelling of one map entry that a nextest report may carry.

    A map entry names its binary as `binary::test`, and nextest names its own
    as `lgwks_bot::binary$test`, so the two never agree on a prefix. Accepting a
    tail that follows any of the three separators the two spellings use (`::`,
    `$`, a space) joins them without accepting a different test whose name
    merely starts the same way.
    """
    candidates = {entry}
    head, separator, rest = entry.partition("::")
    if separator and rest:
        candidates.add(rest)
    return tuple(sorted(candidates, key=len, reverse=True))


TAIL_SEPARATORS = ("::", "$", " ")


def filter_expression(rows: list[dict]) -> str:
    """One nextest filter expression naming every test the map carries.

    nextest matches its `test(/re/)` filter against the test name, so the
    expression is an alternation of the exact names the map carries, each
    escaped and anchored. Anchoring is what keeps a row's receipt from being
    satisfied by a longer name that happens to contain it.
    """
    names = {select_name(str(entry)) for row in rows for entry in row["tests"]}
    names.discard("")
    if not names:
        raise Failure("no test names to run")
    alternation = "|".join(f"^{re.escape(name)}$" for name in sorted(names))
    return f"test(/{alternation}/)"


def head_revision() -> str:
    """The revision this working tree builds, from git, named not guessed."""
    result = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise Failure(f"git rev-parse HEAD failed: {result.stderr.strip()}")
    return result.stdout.strip()


def run_nextest(expression: str, package: str, features: str) -> tuple[dict[str, str], float]:
    """Run the selected tests once and return their outcomes keyed by name.

    `libtest-json-plus` is the only nextest format that reports each test by
    name as it finishes, so a crash or a kill cannot silently become a pass.
    """
    env = dict(os.environ)
    env["NEXTEST_EXPERIMENTAL_LIBTEST_JSON"] = "1"
    command = [
        "cargo",
        "nextest",
        "run",
        "--locked",
        "-p",
        package,
        "--features",
        features,
        "--message-format",
        "libtest-json-plus",
        "--no-fail-fast",
        "-E",
        expression,
    ]
    started = time.monotonic()
    result = subprocess.run(
        command,
        cwd=ROOT,
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )
    elapsed = time.monotonic() - started
    outcomes: dict[str, str] = {}
    for line in result.stdout.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event.get("type") != "test" or "event" not in event:
            continue
        name = event.get("name")
        if not isinstance(name, str):
            continue
        if event["event"] in ("ok", "failed", "ignored", "timeout", "aborted"):
            outcomes[name] = event["event"]
    if not outcomes:
        raise Failure(
            "nextest reported no per-test events; "
            f"exit={result.returncode} stderr={result.stderr.strip()[:600]}"
        )
    return outcomes, elapsed


def build_receipt(
    rows: list[dict], outcomes: dict[str, str], elapsed: float, command: str, revision: str
) -> dict:
    """One receipt: per row, every named test and what the run actually observed."""
    by_test = outcomes
    receipt_rows = []
    missing: list[str] = []
    failing: list[str] = []
    unverified: list[str] = []
    for row in rows:
        rid = str(row["id"])
        tests = []
        for entry in row["tests"]:
            name = str(entry)
            observed = next(
                (
                    kind
                    for candidate in names_for(name)
                    for reported, kind in by_test.items()
                    if reported == candidate
                    or any(reported.endswith(sep + candidate) for sep in TAIL_SEPARATORS)
                ),
                None,
            )
            if observed is None:
                missing.append(f"{rid}::{name}")
            elif observed != "ok":
                failing.append(f"{rid}::{name}={observed}")
            tests.append({"name": name, "observed": observed or "missing"})
        claim = str(row["state"])
        passed = [t for t in tests if t["observed"] == "ok"]
        if claim in VERIFIED_STATES and not passed:
            unverified.append(rid)
        # The run can lower a claim, never raise one: a row the map calls
        # `present` stays `present` even when every named test passes, because
        # the gap sentence, not the test count, is what holds it there.
        if not tests:
            observed_state = "planned"
        elif len(passed) == len(tests):
            observed_state = claim
        elif passed:
            observed_state = "present"
        else:
            observed_state = "missing"
        entry = {
            "id": rid,
            "requires": str(row.get("requires", "")),
            "claimed_state": claim,
            "observed_state": observed_state,
            "tests_passed": len(passed),
            "tests_total": len(tests),
            "gap": str(row.get("gap", "")),
            "tests": tests,
        }
        receipt_rows.append(entry)
    receipt = {
        "revision": revision,
        "generated_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "command": command,
        "wall_seconds": round(elapsed, 3),
        "rows": receipt_rows,
        "totals": {
            "rows": len(receipt_rows),
            "tests_named": sum(len(r["tests"]) for r in receipt_rows),
            "tests_passed": sum(r["tests_passed"] for r in receipt_rows),
            "rows_claimed_verified": sum(
                1 for r in receipt_rows if r["claimed_state"] in VERIFIED_STATES
            ),
            "rows_observed_verified": sum(
                1 for r in receipt_rows
                if r["observed_state"] in VERIFIED_STATES and all(
                    t["observed"] == "ok" for t in r["tests"]
                )
            ),
            "rows_still_partial": sum(
                1 for r in receipt_rows if r["observed_state"] not in VERIFIED_STATES
            ),
        },
        "failures": {
            "named_test_missing_from_run": missing,
            "named_test_not_passing": failing,
            "row_claims_more_than_the_run_shows": unverified,
        },
    }
    return receipt


def render_table(rows: list[dict], receipt: dict) -> str:
    """The spec's per-row block, rendered from the map and one run's receipt.

    Rendered rather than written because a hand-maintained table drifts the
    moment a row's state or gap is edited in one place and not the other. The
    gap column is the map's own sentence, so the table cannot claim less than
    the map and cannot claim more than the run observed.
    """
    revision = str(receipt["revision"])
    observed = {str(entry["id"]): entry for entry in receipt["rows"]}
    lines = [
        TABLE_START,
        "",
        f"Generated by `scripts/acceptance-receipts.py` from"
        f" [`docs/acceptance/t-rows.toml`](acceptance/t-rows.toml) and the committed"
        f" receipt `evidence/{revision}.json`, for the {revision[:8]} tree. Every test the map"
        " names, run once through the real `cargo nextest run` at that revision. A row is"
        " `exercised` only when a named test drives its falsifier; `present` means named tests"
        " cover part of the text and the gap column says what is not asserted. No row is"
        " `accepted`, and nothing here claims the externally bounded subprocess and per-backend"
        " campaign has run. Re-generate with `--write-table`; `--check` fails when this block"
        " disagrees with the map and the receipt in the tree.",
        "",
        "| ID | State | Tests | Gap |",
        "|---|---|---|---|",
    ]
    for row in rows:
        row_seen = observed.get(str(row["id"]))
        if row_seen is None:
            raise Failure(f"the receipt has no entry for {row['id']}")
        passed = int(row_seen["tests_passed"])
        total = int(row_seen["tests_total"])
        gap = str(row.get("gap", "")).strip() or "\u2014"
        state = str(row_seen["observed_state"])
        lines.append(f"| {row['id']} | {state} | {passed}/{total} | {gap} |")
    lines.append("")
    totals = receipt["totals"]
    partial = [
        str(entry["id"]) for entry in receipt["rows"] if str(entry["observed_state"]) not in VERIFIED_STATES
    ]
    lines.append(
        f"At `{revision[:8]}`: {totals['rows']} rows, {totals['tests_passed']}/"
        f"{totals['tests_named']} named tests passing, {totals['rows_observed_verified']} rows"
        f" exercised, {totals['rows_still_partial']} still partial, {receipt['wall_seconds']}s"
        f" wall. Still partial: {', '.join(partial)}."
    )
    lines.append("")
    lines.append(TABLE_END)
    return "\n".join(lines)


def replace_table(spec_text: str, table: str) -> str:
    """Put `table` between the markers, or refuse if the markers are not there.

    A missing marker is a refusal rather than an append: a script that cannot
    find its own delimiters would otherwise add a second table beside the first
    and leave a reader to guess which one a run produced.
    """
    start = spec_text.find(TABLE_START)
    end = spec_text.find(TABLE_END)
    if start < 0 or end < 0 or end < start:
        raise Failure(
            f"the generated table's markers are missing or out of order in the spec; "
            f"expected {TABLE_START!r} before {TABLE_END!r}"
        )
    return spec_text[:start] + table + spec_text[end + len(TABLE_END) :]


def committed_table(spec_text: str) -> str:
    """The block currently between the markers, or a refusal."""
    start = spec_text.find(TABLE_START)
    end = spec_text.find(TABLE_END)
    if start < 0 or end < 0 or end < start:
        raise Failure(
            f"the generated table's markers are missing or out of order in the spec; "
            f"expected {TABLE_START!r} before {TABLE_END!r}"
        )
    return spec_text[start : end + len(TABLE_END)]


def latest_receipt(out_dir: Path) -> dict:
    """The receipt in the tree that the committed table should agree with."""
    if not out_dir.is_dir():
        raise Failure(f"no receipt directory at {out_dir}")
    receipts = sorted(path for path in out_dir.glob("*.json"))
    if not receipts:
        raise Failure(
            f"no receipt in {out_dir}; run without --check to record one, and commit it"
        )
    latest = receipts[-1]
    try:
        return json.loads(latest.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        raise Failure(f"{latest} is not JSON: {error}") from error


def first_difference(found: str, wanted: str) -> list[str]:
    """The first lines where two rendered blocks disagree, for a readable failure."""
    found_lines = found.splitlines()
    wanted_lines = wanted.splitlines()
    out = []
    for index in range(max(len(found_lines), len(wanted_lines))):
        left = found_lines[index] if index < len(found_lines) else "<end of block>"
        right = wanted_lines[index] if index < len(wanted_lines) else "<end of block>"
        if left != right:
            out.append(f"line {index + 1}: committed {left[:160]!r}")
            out.append(f"line {index + 1}: expected  {right[:160]!r}")
            break
    return out


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    try:
        header, rows = load_rows(ROOT / args.map)
        expression = filter_expression(rows)
        revision = args.revision or head_revision()
    except Failure as error:
        print(f"acceptance-receipts: {error}", file=sys.stderr)
        return 2

    command = (
        f"cargo nextest run --locked -p {args.package} --features {args.features} "
        f"--message-format libtest-json-plus --no-fail-fast -E '{expression}'"
    )
    if args.dry_run:
        print(command)
        return 0

    out_dir = ROOT / args.out
    spec_path = ROOT / args.spec
    try:
        if args.check:
            # The check reads the tree as committed: the map, the receipt, and
            # the table. It runs no test, so it is fast enough for a gate and
            # it fails for exactly one reason -- the committed table is not the
            # one this map and receipt would render.
            spec_text = spec_path.read_text(encoding="utf-8")
            wanted = render_table(rows, latest_receipt(out_dir))
            found = committed_table(spec_text)
            if found != wanted:
                print(
                    "acceptance-receipts: the committed per-row table in "
                    f"{args.spec} is not what the map and the receipt render. "
                    "Run with --write-table and commit the result.",
                    file=sys.stderr,
                )
                for line in first_difference(found, wanted):
                    print(f"  {line}", file=sys.stderr)
                return 1
            print(
                f"acceptance-receipts: the committed table in {args.spec} matches the map "
                "and the receipt in the tree"
            )
            return 0

        outcomes, elapsed = run_nextest(expression, args.package, args.features)
        receipt = build_receipt(rows, outcomes, elapsed, command, revision)
    except Failure as error:
        print(f"acceptance-receipts: {error}", file=sys.stderr)
        return 2

    out_dir.mkdir(parents=True, exist_ok=True)
    receipt_path = out_dir / f"{revision}.json"
    receipt_path.write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    if args.write_table:
        try:
            spec_text = spec_path.read_text(encoding="utf-8")
            spec_path.write_text(replace_table(spec_text, render_table(rows, receipt)), encoding="utf-8")
        except (Failure, OSError) as error:
            print(f"acceptance-receipts: {error}", file=sys.stderr)
            return 2
        print(f"rewrote the generated table in {args.spec} from {receipt_path.relative_to(ROOT)}")

    totals = receipt["totals"]
    print(
        f"receipt {receipt_path.relative_to(ROOT)} revision={receipt['revision']} "
        f"rows={totals['rows']} tests_passed={totals['tests_passed']}/"
        f"{totals['tests_named']} rows_observed_verified={totals['rows_observed_verified']}/"
        f"{totals['rows_claimed_verified']} wall={receipt['wall_seconds']}s "
        f"spec={header.get('spec', '')}"
    )
    for key, values in receipt["failures"].items():
        if values:
            print(f"{key}: {len(values)}", file=sys.stderr)
            for value in values[:20]:
                print(f"  {value}", file=sys.stderr)
    if any(receipt["failures"].values()):
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))