#!/usr/bin/env python3
"""Run the acceptance tests named in `docs/acceptance/t-rows.toml`, record them, render the table.

Every acceptance row in `docs/orchestration-acceptance.spec.md` names the tests
that address it. That claim is only worth something if someone can check it
against a real run, so this script is the check: it reads the map, selects
exactly the named tests, runs them once, and records what actually happened.

**The receipt lives in a database, not in the repository.** Item 4 of #271 asks
for that explicitly, and the reason is durability rather than tidiness: a receipt
is the record the spec's table is rendered *from*, so it has to be queryable by
revision, by row and by platform, and it has to survive the run that wrote it.
One JSON file per revision in the tree gives none of those, and grows the
repository by a file per run. So the store is SQLite through the standard
library -- no new dependency, one file, and `--export` produces a single JSON
artifact for a CI upload when a portable copy is wanted.

By default the database lives outside the repository, under
`$XDG_STATE_HOME/lgwks-acceptance/receipts.sqlite` and
`~/.local/state/lgwks-acceptance/receipts.sqlite` when that is unset: state
belongs to the user's machine, and evidence is no more part of the source than
a build artifact is. `--db` points it anywhere else.

The spec's per-row table is *rendered from that database for one exact
revision*, not written by hand: `--write-table` rewrites the block between the
two marker comments, and `--check` compares the committed block against what the
map and that revision's receipt render. A hand-edited table that no run produced
is a claim with nothing behind it, so `--check` is the gate that makes the table
evidence rather than prose.

A row reads `accepted` only when the revision it was observed at is the head
being rendered. A receipt for an older commit is evidence about *that* code, and
promoting a row on the strength of it would be the one thing this script exists
to prevent.

Usage:
    python3 scripts/acceptance-receipts.py [--map docs/acceptance/t-rows.toml]
                                         [--db <path>] [--revision <sha>]
                                         [--spec docs/orchestration-acceptance.spec.md]
                                         [--package lgwks_bot] [--features full]
                                         [--write-table] [--check] [--export <path>]
                                         [--test]

Exit status is non-zero when a named test is missing from the run, when a named
test fails, or when a row's state claims more than the observed evidence, so the
same command works as a gate.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import re
import sqlite3
import subprocess
import sys
import time
import tomllib
import unittest
import xml.etree.ElementTree as ElementTree
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_MAP = Path("docs/acceptance/t-rows.toml")
DEFAULT_SPEC = Path("docs/orchestration-acceptance.spec.md")
DB_RELATIVE = Path("lgwks-acceptance/receipts.sqlite")

# The generated block's own delimiters, so `--write-table` and `--check` agree
# on exactly which lines belong to the run rather than to the prose around them.
TABLE_START = "<!-- acceptance-table: start -->"
TABLE_END = "<!-- acceptance-table: end -->"

# The evidence ladder. A receipt may confirm a state at or below the one the map
# claims; it can never promote one on its own, and the head is what promotes a
# row to `accepted`.
LADDER = ["planned", "missing", "present", "exercised", "independently_evidenced", "accepted"]

# States a row may hold when the receipt observed every named test passing.
OBSERVED_WHEN_ALL_PASS = frozenset({"exercised", "independently_evidenced", "accepted"})

SCHEMA = """
CREATE TABLE IF NOT EXISTS run (
    seq           INTEGER NOT NULL,
    revision      TEXT NOT NULL,
    platform      TEXT NOT NULL,
    feature_set   TEXT NOT NULL,
    package       TEXT NOT NULL,
    command       TEXT NOT NULL,
    wall_seconds  REAL NOT NULL,
    run_at        TEXT NOT NULL,
    artifact_path TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (revision, platform, feature_set)
);

CREATE TABLE IF NOT EXISTS test_result (
    revision    TEXT NOT NULL,
    row_id      TEXT NOT NULL,
    test        TEXT NOT NULL,
    platform    TEXT NOT NULL,
    feature_set TEXT NOT NULL,
    command     TEXT NOT NULL,
    status      TEXT NOT NULL,
    duration_ms REAL NOT NULL,
    run_at      TEXT NOT NULL,
    PRIMARY KEY (revision, row_id, test, platform, feature_set),
    FOREIGN KEY (revision, platform, feature_set)
        REFERENCES run (revision, platform, feature_set)
        ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS test_result_by_revision
    ON test_result (revision, row_id, platform, feature_set);
"""


class Failure(Exception):
    """A condition that makes the receipt untrustworthy, named rather than guessed."""


# ── the database ───────────────────────────────────────────────────────────


def default_db_path() -> Path:
    """Where a receipt lives when nobody says otherwise.

    `XDG_STATE_HOME` when the environment sets it, and `~/.local/state`
    otherwise -- the same fallback the specification gives. Neither is inside the
    repository, because evidence a run produces is not part of the source.
    """
    base = os.environ.get("XDG_STATE_HOME")
    root = Path(base) if base else Path.home() / ".local" / "state"
    return root / DB_RELATIVE


def connect(db_path: Path) -> sqlite3.Connection:
    """Open the receipt database, creating its schema, refusing a foreign file.

    WAL is on before anything is written: a reader rendering the spec's table
    while a run is still appending to the same file is the normal case here, not
    an edge one, and the default rollback journal would serialise them.
    """
    if db_path.exists() and db_path.stat().st_size:
        # Refuse a file that is not our database, rather than half-creating a
        # schema inside somebody's data: `--db` is a path a caller typed, and a
        # typo that lands on an existing file must fail loudly.
        with db_path.open("rb") as handle:
            header = handle.read(16)
        if not header.startswith(b"SQLite format 3\x00"):
            raise Failure(
                f"{db_path} is not a SQLite database (its first 16 bytes are {header!r}); "
                "refusing to write a receipt into it"
            )
    db_path.parent.mkdir(parents=True, exist_ok=True)
    connection = sqlite3.connect(db_path)
    connection.row_factory = sqlite3.Row
    connection.execute("PRAGMA journal_mode=WAL")
    connection.executescript(SCHEMA)
    return connection


def record_run(
    connection: sqlite3.Connection,
    *,
    revision: str,
    platform_name: str,
    feature_set: str,
    package: str,
    command: str,
    wall_seconds: float,
    run_at: str,
    artifact_path: str,
    results: list[tuple[str, str, str, float]],
) -> None:
    """Write one run and every one of its test results, in one transaction.

    `INSERT OR REPLACE` under a primary key of
    `(revision, row_id, test, platform, feature_set)` is what makes a re-run of
    the same revision idempotent: the second run overwrites the first rather than
    doubling every count, and a half-finished run leaves nothing behind because
    the whole write is one transaction.

    `seq` is assigned here rather than read from the clock, because "newest run
    first" has to be true for two runs recorded inside one second -- which is the
    normal case when a suite re-runs on a fix -- and a timestamp alone orders them
    by tie-break guess.
    """
    with connection:
        following = connection.execute("SELECT COALESCE(MAX(seq), 0) + 1 AS next FROM run").fetchone()
        connection.execute(
            """
            INSERT OR REPLACE INTO run
                (seq, revision, platform, feature_set, package, command,
                 wall_seconds, run_at, artifact_path)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            (
                int(following["next"]),
                revision,
                platform_name,
                feature_set,
                package,
                command,
                wall_seconds,
                run_at,
                artifact_path,
            ),
        )
        connection.executemany(
            """
            INSERT OR REPLACE INTO test_result
                (revision, row_id, test, platform, feature_set,
                 command, status, duration_ms, run_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            [
                (
                    revision,
                    row_id,
                    test,
                    platform_name,
                    feature_set,
                    command,
                    status,
                    duration_ms,
                    run_at,
                )
                for (row_id, test, status, duration_ms) in results
            ],
        )


def read_run(
    connection: sqlite3.Connection,
    revision: str,
    feature_set: str = "full",
    platform_name: str | None = None,
) -> sqlite3.Row:
    """The run recorded for `revision` on this machine and feature set, or a refusal.

    The feature set is a parameter rather than a lookup because the database may
    legitimately hold several for one revision -- a full-feature run and a
    `--no-default-features` run are different evidence about the same code -- and
    resolving that by picking one would let the wrong run satisfy the claim. The
    platform is a parameter for the same reason and for one more: a CI job reads
    back a Linux receipt from a machine that is not Linux, and a lookup keyed on
    the reader would find nothing.
    """
    wanted = platform_name or current_platform()
    row = connection.execute(
        "SELECT * FROM run WHERE revision = ? AND platform = ? AND feature_set = ?",
        (revision, wanted, feature_set),
    ).fetchone()
    if row is None:
        raise Failure(
            f"no receipt for revision {revision} on {wanted} with features {feature_set}; "
            "run without --check to record one"
        )
    return row


def read_results(
    connection: sqlite3.Connection,
    revision: str,
    feature_set: str = "full",
    platform_name: str | None = None,
) -> dict[tuple[str, str], sqlite3.Row]:
    """Every `(row_id, test)` outcome the revision recorded."""
    found = connection.execute(
        """
        SELECT row_id, test, status, duration_ms, command, run_at
          FROM test_result
         WHERE revision = ? AND platform = ? AND feature_set = ?
        """,
        (revision, platform_name or current_platform(), feature_set),
    ).fetchall()
    if not found:
        raise Failure(
            f"the receipt for revision {revision} records no test results; a partial run "
            "cannot render a table"
        )
    return {(row["row_id"], row["test"]): row for row in found}


def current_platform() -> str:
    """What a receipt is keyed on beside the revision and the feature set."""
    return f"{platform.system()}-{platform.machine()}"


def revisions(connection: sqlite3.Connection) -> list[str]:
    """Every revision the database knows, newest first by when it ran."""
    rows = connection.execute(
        "SELECT revision, MAX(seq) AS seen FROM run GROUP BY revision ORDER BY seen DESC"
    ).fetchall()
    return [row["revision"] for row in rows]


# ── the map ────────────────────────────────────────────────────────────────


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--map", default=str(DEFAULT_MAP), help="path to the row map")
    parser.add_argument("--db", default=None, help="receipt database (default: the state dir)")
    parser.add_argument("--revision", default=None, help="revision the receipt describes")
    parser.add_argument("--head", default=None, help="revision being rendered as the head")
    parser.add_argument("--spec", default=str(DEFAULT_SPEC), help="the document holding the table")
    parser.add_argument("--package", default="lgwks_bot", help="package under test")
    parser.add_argument("--features", default="full", help="features under test")
    parser.add_argument("--export", default=None, help="write this run's receipt as JSON here")
    parser.add_argument(
        "--from-junit",
        nargs="+",
        default=None,
        metavar="FILE",
        help="record the results CI already ran, from its JUnit reports, instead of running",
    )
    parser.add_argument(
        "--platform",
        default=None,
        help="the platform a --from-junit run reports for (default: this machine)",
    )
    parser.add_argument(
        "--write-table",
        action="store_true",
        help="rewrite the spec's generated table from the recorded receipt",
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
    parser.add_argument("--test", action="store_true", help="run this script's own suite")
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
        if str(row.get("state", "")) not in LADDER:
            raise Failure(f"{rid} claims state {row.get('state')!r}, which is not on the ladder")
    return payload, rows


def test_name(entry: str) -> str:
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

    A map entry names its binary as `binary::test`, and nextest names its own as
    `lgwks_bot::binary$test`, so the two never agree on a prefix. Accepting a
    tail that follows any of the three separators the two spellings use (`::`,
    `$`, a space) joins them without accepting a different test whose name merely
    starts the same way.
    """
    candidates = {entry}
    head, separator, rest = entry.partition("::")
    if separator and rest:
        candidates.add(rest)
    return tuple(sorted(candidates, key=len, reverse=True))


TAIL_SEPARATORS = ("::", "$", " ")


def names_for_names(entries: set[str]) -> set[str]:
    """Every spelling of a set of map entries, for a membership test."""
    return {candidate for entry in entries for candidate in names_for(entry)}


def filter_expression(rows: list[dict]) -> str:
    """One nextest filter expression naming every test the map carries.

    Anchoring each name is what keeps a row's receipt from being satisfied by a
    longer name that happens to contain it.
    """
    names = {test_name(str(entry)) for row in rows for entry in row["tests"]}
    names.discard("")
    if not names:
        raise Failure("no test names to run")
    alternation = "|".join(f"^{re.escape(name)}$" for name in sorted(names))
    return f"test(/{alternation}/)"


def head_revision() -> str:
    """The revision this working tree builds, from git, named not guessed."""
    result = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True, check=False
    )
    if result.returncode != 0:
        raise Failure(f"git rev-parse HEAD failed: {result.stderr.strip()}")
    return result.stdout.strip()


# ── the run ────────────────────────────────────────────────────────────────


def run_nextest(
    expression: str, package: str, features: str
) -> tuple[dict[str, tuple[str, float]], float]:
    """Run the selected tests once and return each test's status and duration.

    `libtest-json-plus` is the only nextest format that reports each test by name
    as it finishes, so a crash or a kill cannot silently become a pass.
    """
    env = dict(os.environ)
    env["NEXTEST_EXPERIMENTAL_LIBTEST_JSON"] = "1"
    command = [
        "cargo", "nextest", "run", "--locked", "-p", package, "--features", features,
        "--message-format", "libtest-json-plus", "--no-fail-fast", "-E", expression,
    ]
    started = time.monotonic()
    result = subprocess.run(
        command, cwd=ROOT, capture_output=True, text=True, env=env, check=False
    )
    elapsed = time.monotonic() - started
    outcomes: dict[str, tuple[str, float]] = {}
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
            outcomes[name] = (event["event"], float(event.get("exec_time", 0.0) or 0.0) * 1000.0)
    if not outcomes:
        raise Failure(
            "nextest reported no per-test events; "
            f"exit={result.returncode} stderr={result.stderr.strip()[:600]}"
        )
    return outcomes, elapsed


def match_outcome(outcomes: dict, entry: str) -> tuple[str, float] | None:
    """The observed outcome for a map entry, or `None` when the run never ran it."""
    for candidate in names_for(entry):
        for reported, observed in outcomes.items():
            if reported == candidate or any(
                reported.endswith(sep + candidate) for sep in TAIL_SEPARATORS
            ):
                return observed
    return None


# ── reading what CI already ran ─────────────────────────────────────────────


def parse_junit(path: Path) -> dict[str, tuple[str, float]]:
    """Read one JUnit file into `{test name: (status, duration_ms)}`.

    nextest writes one `<testcase>` per test, named `binary::module::test` for
    this crate's shape, with a `<failure>`, `<skipped>` or `<error>` child
    carrying the outcome. Parsed with the standard library's `xml.etree`, so a CI
    job with no Rust toolchain can read what the shards produced.

    A malformed file is a refusal rather than a partial parse: a JUnit file this
    reader could not finish is evidence of nothing, and treating it as an empty
    one would turn every row's named test into "not run" while looking like a
    clean report.
    """
    try:
        tree = ElementTree.parse(path)
    except (ElementTree.ParseError, OSError) as error:
        raise Failure(f"{path} is not readable JUnit XML: {error}") from error
    root = tree.getroot()
    # nextest emits `<testsuites>` wrapping one or more `<testsuite>`; a bare
    # `<testsuite>` is also valid JUnit and appears in plenty of tools' output.
    cases: dict[str, tuple[str, float]] = {}
    elements = (
        root.findall(".//testcase")
        if root.tag == "testsuites"
        else root.findall(".//testcase")
    )
    if not elements:
        raise Failure(f"{path} holds no <testcase> elements; it is not a test report")
    for case in elements:
        name = case.get("name")
        if not name:
            continue
        seconds = case.get("time")
        duration = (float(seconds) * 1000.0) if seconds else 0.0
        if case.find("failure") is not None or case.find("error") is not None:
            status = "failed"
        elif case.find("skipped") is not None:
            status = "ignored"
        else:
            status = "ok"
        cases[name] = (status, duration)
    if not cases:
        raise Failure(f"{path} names no testcase, so it cannot answer for any row")
    return cases


def merge_junit(paths: list[Path]) -> dict[str, tuple[str, float]]:
    """Every shard's results in one map, with a duplicate named rather than merged.

    The shards partition one suite, so a name appearing twice means the
    partitioning is wrong -- and silently keeping the first would make the
    receipt describe a run that could not have happened.
    """
    merged: dict[str, tuple[str, float]] = {}
    origin: dict[str, Path] = {}
    for path in paths:
        for name, observed in parse_junit(path).items():
            if name in merged:
                raise Failure(
                    f"{name} appears in both {origin[name]} and {path}; the shards partition "
                    "one suite, so a name may not be reported twice"
                )
            merged[name] = observed
            origin[name] = path
    return merged


def junit_command(paths: list[Path], package: str, features: str) -> str:
    """What the CI job it is reading from actually ran."""
    names = ", ".join(str(path) for path in paths)
    return (
        f"cargo nextest run --locked -p {package} --features {features} --profile ci "
        f"--partition count:4 (JUnit reports: {names})"
    )


# ── rendering ──────────────────────────────────────────────────────────────


def observed_state(claimed: str, passed: int, total: int, revision: str, head: str) -> str:
    """What one row reads as, given the map's claim and what the run observed.

    The run can lower a claim and never raise one: a row the map calls `present`
    stays `present` even when every named test passes, because the gap sentence,
    not the test count, is what holds it there. The single promotion is
    `accepted`, and it requires the receipt's revision to be the head being
    rendered -- evidence about an older commit is evidence about that commit, and
    letting it accept the current one is the exact failure this store exists to
    prevent.
    """
    if total == 0:
        return "planned"
    if passed != total:
        return "present" if passed else "missing"
    if claimed == "accepted" and revision != head:
        return "exercised"
    return claimed


def render_table(rows: list[dict], run: sqlite3.Row, results: dict, head: str) -> str:
    """The spec's per-row block, rendered from the map and one revision's receipt."""
    revision = str(run["revision"])
    feature_set = str(run["feature_set"])
    lines = [
        TABLE_START,
        "",
        f"Generated by `scripts/acceptance-receipts.py` from"
        f" [`docs/acceptance/t-rows.toml`](acceptance/t-rows.toml) and the receipt recorded for"
        f" `{revision[:8]}` on `{run['platform']}` with features `{feature_set}` at"
        f" {run['run_at']}, {run['wall_seconds']}s wall, over"
        f" {run['package']} with features `{feature_set}`. The full command is in the receipt's"
        " `run` table; it is not repeated here because its `-E` alternation names every test in"
        " the map and is several kilobytes of prose nobody reads. A row"
        " is `exercised` only when a named test drove its falsifier; `present` means named tests"
        " cover part of the text and the gap column says what is not asserted. A row reads"
        " `accepted` only when the revision it was observed at is the head being rendered, and"
        " none is here: the externally bounded subprocess and per-backend campaign has not run."
        " Re-record with `--write-table`; `--check` fails when this block disagrees with the map"
        " and the receipt.",
        "",
        "| ID | State | Tests | Gap |",
        "|---|---|---|---|",
    ]
    partial: list[str] = []
    named_total = 0
    passed_total = 0
    for row in rows:
        rid = str(row["id"])
        entries = [str(entry) for entry in row["tests"]]
        named_total += len(entries)
        passed = 0
        for entry in entries:
            seen = results.get((rid, entry))
            if seen is not None and seen["status"] == "ok":
                passed += 1
        passed_total += passed
        state = observed_state(str(row["state"]), passed, len(entries), revision, head)
        if state not in OBSERVED_WHEN_ALL_PASS:
            partial.append(rid)
        gap = str(row.get("gap", "")).strip() or "—"
        lines.append(f"| {rid} | {state} | {passed}/{len(entries)} | {gap} |")
    lines.append("")
    lines.append(
        f"At `{revision[:8]}` on `{run['platform']}`: {len(rows)} rows, {passed_total}/"
        f"{named_total} named tests passing, {len(rows) - len(partial)} rows exercised, "
        f"{len(partial)} still partial, {run['wall_seconds']}s wall. Still partial: "
        f"{', '.join(partial)}."
    )
    lines.append("")
    lines.append(TABLE_END)
    return "\n".join(lines)


def replace_table(spec_text: str, table: str) -> str:
    """Put `table` between the markers, or refuse if the markers are not there.

    A missing marker is a refusal rather than an append: a script that cannot find
    its own delimiters would otherwise add a second table beside the first and
    leave a reader to guess which one a run produced.
    """
    start = spec_text.find(TABLE_START)
    end = spec_text.find(TABLE_END)
    if start < 0 or end < 0 or end < start:
        raise Failure(
            f"the generated table's markers are missing or out of order; expected "
            f"{TABLE_START!r} before {TABLE_END!r}"
        )
    return spec_text[:start] + table + spec_text[end + len(TABLE_END) :]


def committed_table(spec_text: str) -> str:
    """The block currently between the markers, or a refusal."""
    start = spec_text.find(TABLE_START)
    end = spec_text.find(TABLE_END)
    if start < 0 or end < 0 or end < start:
        raise Failure(
            f"the generated table's markers are missing or out of order; expected "
            f"{TABLE_START!r} before {TABLE_END!r}"
        )
    return spec_text[start : end + len(TABLE_END)]


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


def export_json(path: Path, rows: list[dict], run: sqlite3.Row, results: dict, head: str) -> Path:
    """Write one receipt as a portable JSON artifact, for a CI upload.

    The database is the store; this is the copy a build can attach to a run, so
    it carries the same facts rather than a summary of them.
    """
    payload = {
        "revision": run["revision"],
        "platform": run["platform"],
        "feature_set": run["feature_set"],
        "package": run["package"],
        "command": run["command"],
        "wall_seconds": run["wall_seconds"],
        "run_at": run["run_at"],
        "head": head,
        "rows": [
            {
                "id": str(row["id"]),
                "requires": str(row.get("requires", "")),
                "claimed_state": str(row["state"]),
                "observed_state": observed_state(
                    str(row["state"]),
                    sum(
                        1
                        for entry in row["tests"]
                        if (results.get((str(row["id"]), str(entry))) or {"status": "missing"})[
                            "status"
                        ]
                        == "ok"
                    ),
                    len(row["tests"]),
                    str(run["revision"]),
                    head,
                ),
                "gap": str(row.get("gap", "")),
                "tests": [
                    {
                        "name": str(entry),
                        "status": (results.get((str(row["id"]), str(entry))) or {"status": "missing"})[
                            "status"
                        ],
                        "duration_ms": (
                            results.get((str(row["id"]), str(entry))) or {"duration_ms": None}
                        )["duration_ms"],
                    }
                    for entry in row["tests"]
                ],
            }
            for row in rows
        ],
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return path


# ── the generator's own regression suite ───────────────────────────────────


def fake_rows(states: dict[str, str] | None = None) -> list[dict]:
    """A two-row map, for the suite to render without reading the repository."""
    return [
        {
            "id": "T01",
            "requires": "DX-01",
            "state": (states or {}).get("T01", "exercised"),
            "gap": "",
            "tests": ["it::a::one_t01", "it::a::two_t01"],
        },
        {
            "id": "T02",
            "requires": "DX-02",
            "state": (states or {}).get("T02", "present"),
            "gap": "the second half is not asserted",
            "tests": ["it::b::one_t02"],
        },
    ]


def seed(
    connection: sqlite3.Connection,
    revision: str,
    *,
    feature_set: str = "full",
    statuses: dict[tuple[str, str], str] | None = None,
) -> sqlite3.Row:
    """Record a run directly, the way a real one would, without running cargo."""
    rows = fake_rows()
    statuses = statuses or {
        ("T01", "it::a::one_t01"): "ok",
        ("T01", "it::a::two_t01"): "ok",
        ("T02", "it::b::one_t02"): "ok",
    }
    results = [
        (row_id, test, status, 1.5)
        for (row_id, test), status in sorted(statuses.items())
    ]
    record_run(
        connection,
        revision=revision,
        platform_name=current_platform(),
        feature_set=feature_set,
        package="lgwks_bot",
        command="cargo nextest run --locked -p lgwks_bot --features full",
        wall_seconds=1.0,
        run_at="2026-10-05T00:00:00Z",
        artifact_path="",
        results=results,
    )
    return read_run(connection, revision, feature_set)


def junit_document(cases: list[tuple[str, str]]) -> str:
    """A nextest-shaped JUnit report for `cases`, each `(name, status)`."""
    parts = []
    for name, status in cases:
        child = {
            "ok": "",
            "failed": "<failure message=\"failed\">assertion failed</failure>",
            "ignored": "<skipped message=\"ignored\"/>",
        }[status]
        parts.append(
            f'    <testcase classname="lgwks_bot::it" name="{name}" time="0.010">{child}</testcase>'
        )
    return (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<testsuites><testsuite name="lgwks_bot" tests="%d">\n%s\n  </testsuite></testsuites>\n'
        % (len(cases), "\n".join(parts))
    )


class GeneratorRegression(unittest.TestCase):
    """The properties the receipt store has to hold, proved rather than asserted.

    Each case drives the real functions against a real database in a temporary
    directory, so a refactor that breaks the schema or the ordering fails here
    rather than in a run whose output nobody reads.
    """

    def setUp(self):
        import tempfile

        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.db = Path(self._tmp.name) / "receipts.sqlite"
        self.connection = connect(self.db)
        self.addCleanup(self.connection.close)

    def render(self, revision: str, head: str) -> str:
        return render_table(
            fake_rows(),
            read_run(self.connection, revision),
            read_results(self.connection, revision),
            head,
        )

    def test_a_rerun_of_one_revision_is_idempotent(self):
        """Recording the same revision twice leaves one run and one row per test."""
        seed(self.connection, "rev-one")
        seed(self.connection, "rev-one")
        runs = self.connection.execute(
            "SELECT COUNT(*) AS n FROM run WHERE revision = ?", ("rev-one",)
        ).fetchone()["n"]
        tests = self.connection.execute(
            "SELECT COUNT(*) AS n FROM test_result WHERE revision = ?", ("rev-one",)
        ).fetchone()["n"]
        self.assertEqual(runs, 1, "a re-run must overwrite its own run, not add a second")
        self.assertEqual(tests, 3, "and must not duplicate the test results")
        self.assertEqual(self.render("rev-one", "rev-one"),
                         self.render("rev-one", "rev-one"))

    def test_a_stale_revision_never_renders_accepted(self):
        """`accepted` needs the receipt's revision to be the head being rendered."""
        seed(self.connection, "old", statuses={
            ("T01", "it::a::one_t01"): "ok",
            ("T01", "it::a::two_t01"): "ok",
            ("T02", "it::b::one_t02"): "ok",
        })
        rows = fake_rows({"T01": "accepted", "T02": "accepted"})
        run = read_run(self.connection, "old")
        results = read_results(self.connection, "old")

        at_head = render_table(rows, run, results, head="old")
        self.assertIn("| T01 | accepted |", at_head,
                      "a receipt at the head may promote a row to accepted")

        stale = render_table(rows, run, results, head="newer")
        self.assertNotIn("accepted", stale.split("Still partial")[0].split("| ID |")[-1]
                         .replace("`accepted`", ""),
                         "a receipt for an older revision must not render accepted")
        self.assertIn("| T01 | exercised |", stale)
        self.assertIn("| T02 | exercised |", stale)

    def test_two_revisions_coexist_and_are_separately_readable(self):
        """A second revision is its own run, not an overwrite of the first."""
        seed(self.connection, "rev-one")
        seed(self.connection, "rev-two")
        self.assertEqual(revisions(self.connection), ["rev-two", "rev-one"])
        self.assertEqual(read_run(self.connection, "rev-one")["revision"], "rev-one")
        self.assertEqual(read_run(self.connection, "rev-two")["revision"], "rev-two")
        first = self.render("rev-one", "rev-one")
        second = self.render("rev-two", "rev-two")
        self.assertNotEqual(first, second, "each revision's table names its own revision")

    def test_a_corrupt_database_is_refused(self):
        """A file that is not a database is refused, not half-initialised."""
        path = Path(self._tmp.name) / "not-a-database.sqlite"
        path.write_bytes(b"this is not a sqlite file at all")
        with self.assertRaises(Failure) as caught:
            connect(path)
        self.assertIn("not a SQLite database", str(caught.exception))

    def test_a_partial_receipt_is_refused(self):
        """A run with no recorded test results cannot render a table."""
        with self.connection:
            self.connection.execute(
                """
                INSERT INTO run (seq, revision, platform, feature_set, package, command,
                                 wall_seconds, run_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (99, "rev-partial", current_platform(), "full", "lgwks_bot", "cmd", 1.0,
                 "2026-10-05T00:00:00Z"),
            )
        with self.assertRaises(Failure) as caught:
            read_results(self.connection, "rev-partial")
        self.assertIn("records no test results", str(caught.exception))

    def test_a_missing_revision_is_refused_by_name(self):
        """Rendering a revision the database never saw names the revision."""
        seed(self.connection, "rev-one")
        with self.assertRaises(Failure) as caught:
            read_run(self.connection, "rev-absent")
        self.assertIn("rev-absent", str(caught.exception))

    # ── the CI path: reading what the shards already ran ──

    def write_junit(self, name: str, cases: list[tuple[str, str]]) -> Path:
        path = Path(self._tmp.name) / name
        path.write_text(junit_document(cases), encoding="utf-8")
        return path

    def record_from_junit(self, paths: list[Path], revision: str = "rev-junit") -> dict:
        """Drive the recording path a CI job takes, without running cargo."""
        outcomes = merge_junit(paths)
        results = []
        for row in fake_rows():
            rid = str(row["id"])
            for entry in row["tests"]:
                observed = match_outcome(outcomes, str(entry))
                status, duration = observed if observed is not None else ("missing", 0.0)
                results.append((rid, str(entry), status, duration))
        record_run(
            self.connection,
            revision=revision,
            platform_name="linux-x86_64",
            feature_set="full",
            package="lgwks_bot",
            command=junit_command(paths, "lgwks_bot", "full"),
            wall_seconds=1.0,
            run_at="2026-10-05T00:00:00Z",
            artifact_path="",
            results=results,
        )
        return read_results(self.connection, revision, "full", "linux-x86_64")

    def test_a_malformed_junit_file_is_refused(self):
        """A file this reader cannot finish is evidence of nothing."""
        path = Path(self._tmp.name) / "broken.xml"
        path.write_text("<testsuites><testsuite><testcase name=\"a\"", encoding="utf-8")
        with self.assertRaises(Failure) as caught:
            parse_junit(path)
        self.assertIn("not readable JUnit", str(caught.exception))

    def test_junit_without_a_testcase_is_refused(self):
        """A JUnit file with no testcase cannot answer for any row."""
        path = Path(self._tmp.name) / "empty.xml"
        path.write_text('<?xml version="1.0"?><testsuites/>', encoding="utf-8")
        with self.assertRaises(Failure) as caught:
            parse_junit(path)
        self.assertIn("no <testcase>", str(caught.exception))

    def test_a_missing_named_test_is_recorded_not_run(self):
        """A test no shard reported is `missing`, never `ok`."""
        path = self.write_junit("one.xml", [("a::one_t01", "ok")])
        stored = self.record_from_junit([path])
        self.assertEqual(stored[("T01", "it::a::one_t01")]["status"], "ok")
        self.assertEqual(
            stored[("T01", "it::a::two_t01")]["status"],
            "missing",
            "a named test absent from every shard is not-run, and must never read as passed",
        )
        self.assertEqual(stored[("T02", "it::b::one_t02")]["status"], "missing")

    def test_a_failed_testcase_lowers_the_row(self):
        """A `<failure>` in the report lowers the row that names it."""
        path = self.write_junit(
            "one.xml",
            [
                ("a::one_t01", "ok"),
                ("a::two_t01", "failed"),
                ("b::one_t02", "ok"),
            ],
        )
        stored = self.record_from_junit([path])
        table = render_table(
            fake_rows(),
            read_run(self.connection, "rev-junit", "full", "linux-x86_64"),
            stored,
            head="rev-junit",
        )
        self.assertIn("| T01 | present | 1/2 |", table)
        self.assertNotIn("| T01 | exercised |", table)

    def test_four_shards_merge_into_one_receipt(self):
        """One suite across four partition reports is one receipt, not four."""
        shards = [
            self.write_junit("shard-1.xml", [("a::one_t01", "ok")]),
            self.write_junit("shard-2.xml", [("a::two_t01", "ok")]),
            self.write_junit("shard-3.xml", [("b::one_t02", "ok")]),
            self.write_junit("shard-4.xml", [("unrelated::elsewhere", "ok")]),
        ]
        stored = self.record_from_junit(shards)
        self.assertEqual(stored[("T01", "it::a::one_t01")]["status"], "ok")
        self.assertEqual(stored[("T01", "it::a::two_t01")]["status"], "ok")
        self.assertEqual(stored[("T02", "it::b::one_t02")]["status"], "ok")
        runs = self.connection.execute(
            "SELECT COUNT(*) AS n FROM run WHERE revision = ?", ("rev-junit",)
        ).fetchone()["n"]
        self.assertEqual(runs, 1, "four shard reports record one run")

    def test_a_duplicate_test_across_shards_is_refused(self):
        """Two shards reporting one name means the partitioning is wrong."""
        first = self.write_junit("shard-1.xml", [("a::one_t01", "ok")])
        second = self.write_junit("shard-2.xml", [("a::one_t01", "ok")])
        with self.assertRaises(Failure) as caught:
            merge_junit([first, second])
        self.assertIn("may not be reported twice", str(caught.exception))

    def test_a_junit_rerun_is_idempotent(self):
        """Recording the same shard reports twice leaves one run and one row per test."""
        shards = [
            self.write_junit("shard-1.xml", [("a::one_t01", "ok")]),
            self.write_junit("shard-2.xml", [("a::two_t01", "ok"), ("b::one_t02", "ok")]),
        ]
        self.record_from_junit(shards)
        self.record_from_junit(shards)
        self.assertEqual(
            self.connection.execute(
                "SELECT COUNT(*) AS n FROM run WHERE revision = ?", ("rev-junit",)
            ).fetchone()["n"],
            1,
        )
        self.assertEqual(
            self.connection.execute(
                "SELECT COUNT(*) AS n FROM test_result WHERE revision = ?", ("rev-junit",)
            ).fetchone()["n"],
            3,
            "a re-run must overwrite its own rows rather than duplicate them",
        )

    def test_a_green_run_never_promotes_a_partial_row(self):
        """Every named test passing does not turn a `present` row into `exercised`.

        The gap sentence, not the test count, is what holds a partial row down.
        A rule that read "all tests passed, therefore exercised" would silently
        close six rows' gaps the moment a run went green.
        """
        seed(self.connection, "rev-partial-row")
        table = self.render("rev-partial-row", "rev-partial-row")
        self.assertIn("| T02 | present | 1/1 |", table)
        self.assertNotIn("| T02 | exercised |", table)
        self.assertIn("Still partial: T02.", table)

    def test_a_failing_test_lowers_the_row_to_present(self):
        """A named test that failed cannot leave its row reading `exercised`."""
        seed(self.connection, "rev-fail", statuses={
            ("T01", "it::a::one_t01"): "ok",
            ("T01", "it::a::two_t01"): "failed",
            ("T02", "it::b::one_t02"): "ok",
        })
        table = self.render("rev-fail", "rev-fail")
        self.assertIn("| T01 | present | 1/2 |", table)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)

    if args.test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(GeneratorRegression)
        return 0 if unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful() else 1

    try:
        header, rows = load_rows(ROOT / args.map)
        expression = filter_expression(rows)
        revision = args.revision or head_revision()
        head = args.head or head_revision()
        db_path = Path(args.db) if args.db else default_db_path()
        db_path = db_path if db_path.is_absolute() else ROOT / db_path
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

    spec_path = ROOT / args.spec
    try:
        connection = connect(db_path)
    except (Failure, sqlite3.DatabaseError) as error:
        print(f"acceptance-receipts: {error}", file=sys.stderr)
        return 2

    try:
        if args.check:
            spec_text = spec_path.read_text(encoding="utf-8")
            wanted = render_table(
                rows,
                read_run(connection, revision, args.features, args.platform),
                read_results(connection, revision, args.features, args.platform),
                head,
            )
            found = committed_table(spec_text)
            if found != wanted:
                print(
                    f"acceptance-receipts: the committed per-row table in {args.spec} is not "
                    "what the map and the receipt render. Re-record with --write-table and "
                    "commit the result.",
                    file=sys.stderr,
                )
                for line in first_difference(found, wanted):
                    print(f"  {line}", file=sys.stderr)
                return 1
            print(
                f"acceptance-receipts: the committed table in {args.spec} matches the map and "
                f"the receipt for {revision[:8]} in {db_path}"
            )
            return 0

        if args.from_junit:
            # The CI path: the tests already ran, once, across the shards. This
            # job records what they reported rather than running them again, so
            # the receipt costs seconds and the suite is not executed twice.
            paths = [Path(entry) for entry in args.from_junit]
            outcomes = merge_junit(paths)
            elapsed = float(
                sum(duration for _, duration in outcomes.values())
            ) / 1000.0
            platform_name = args.platform or current_platform()
            command_text = junit_command(paths, args.package, args.features)
        else:
            outcomes, elapsed = run_nextest(expression, args.package, args.features)
            platform_name = current_platform()
            command_text = command
        results = []
        failures = []
        for row in rows:
            rid = str(row["id"])
            for entry in row["tests"]:
                name = str(entry)
                observed = match_outcome(outcomes, name)
                if observed is None:
                    status, duration = "missing", 0.0
                    failures.append(f"{rid}::{name} was not run")
                else:
                    status, duration = observed
                    if status != "ok":
                        failures.append(f"{rid}::{name}={status}")
                results.append((rid, name, status, duration))
        run_at = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        artifact = str(args.export) if args.export else ""
        record_run(
            connection,
            revision=revision,
            platform_name=platform_name,
            feature_set=args.features,
            package=args.package,
            command=command_text,
            wall_seconds=round(elapsed, 3),
            run_at=run_at,
            artifact_path=artifact,
            results=results,
        )
        run = read_run(connection, revision, args.features, platform_name)
        stored = read_results(connection, revision, args.features, platform_name)
    except (Failure, sqlite3.DatabaseError) as error:
        print(f"acceptance-receipts: {error}", file=sys.stderr)
        return 2
    finally:
        connection.close()

    # The summary counts the map's own slots, not whatever nextest happened to
    # run: two row slots may name one test, and a filter that matched a name
    # nobody asked for would otherwise inflate the denominator.
    requested = sum(len(row["tests"]) for row in rows)
    passed = sum(
        1
        for row in rows
        for entry in row["tests"]
        if (stored.get((str(row["id"]), str(entry))) or {"status": "missing"})["status"] == "ok"
    )
    asked = {str(entry) for row in rows for entry in row["tests"]}
    unrequested = sorted(
        {
            reported.split("$", 1)[-1]
            for reported in outcomes
            if not any(
                reported == candidate
                or any(reported.endswith(sep + candidate) for sep in TAIL_SEPARATORS)
                for candidate in names_for_names(asked)
            )
        }
    )
    if unrequested and not args.from_junit:
        print(
            f"acceptance-receipts: {len(unrequested)} test(s) the filter ran that no row names: "
            f"{', '.join(unrequested)}",
            file=sys.stderr,
        )
    partial = [
        str(row["id"])
        for row in rows
        if observed_state(
            str(row["state"]),
            sum(
                1
                for entry in row["tests"]
                if (stored.get((str(row["id"]), str(entry))) or {"status": "missing"})["status"] == "ok"
            ),
            len(row["tests"]),
            revision,
            head,
        )
        not in OBSERVED_WHEN_ALL_PASS
    ]

    if args.write_table:
        try:
            spec_path.write_text(
                replace_table(spec_path.read_text(encoding="utf-8"),
                              render_table(rows, run, stored, head)),
                encoding="utf-8",
            )
            print(f"rewrote the generated table in {args.spec}")
        except (Failure, OSError) as error:
            print(f"acceptance-receipts: {error}", file=sys.stderr)
            return 2

    if args.export:
        exported = export_json(Path(args.export), rows, run, stored, head)
        print(f"exported the receipt for {revision[:8]} to {exported}")

    print(
        f"receipt {db_path} revision={revision} rows={len(rows)} "
        f"tests_passed={passed}/{requested} rows_still_partial={len(partial)} "
        f"partial={','.join(partial) or '-'} wall={round(elapsed, 3)}s spec={header.get('spec', '')}"
    )
    if failures:
        print(f"named tests not passing: {len(failures)}", file=sys.stderr)
        for value in failures[:20]:
            print(f"  {value}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())