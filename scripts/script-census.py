#!/usr/bin/env python3
"""Census of passthrough Rust inside `script!` (SL-7, #389).

A line inside a `script!` either starts with a word (a step of the map) or is
plain Rust passed through as written (SL-3). Passthrough that recurs across
scripts is the signal that a word is missing, so this counts it by shape.

The map comes from the one parser: `lgwks-ast script map --json` (SL-2), so
this script reads no Rust grammar of its own. For each invocation it takes the
source lines from the `script!` line to its closing delimiter, removes the
lines the map names (flow headers and every step header, nested ones
included), blank lines and comments, and what is left is passthrough. A shape
keeps keywords, punctuation, method and macro names and collapses everything
else, so `let n = page.len()` and `let size = body.len()` count together.

The share is a signal, never a gate: a gated share would be gamed
(docs/script-language.spec.md, "How we know it works").

Usage:
  python3 scripts/script-census.py MAP.json [MAP.json ...] [--top N] [--json]
  python3 scripts/script-census.py --test

Each MAP.json is the stdout of `lgwks-ast script map --json <repo>` run from
the directory its paths are relative to; the repository is named by the
file's parent directory, or by `--repo NAME=MAP.json`.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import tempfile
import unittest
from collections import Counter
from pathlib import Path

KEYWORDS = frozenset(
    "as async await break const continue crate else enum false fn for if impl in let loop "
    "match mod move mut pub ref return self Self static struct super trait true type unsafe "
    "use where while dyn Some None Ok Err".split()
)
TOKEN = re.compile(
    r"""(?P<str>r?\#*"(?:\\.|[^"\\])*"\#*|'(?:\\.|[^'\\])'|b"(?:\\.|[^"\\])*")
      |(?P<num>\b\d[\d_]*(?:\.\d+)?(?:[a-z]+\d*)?\b)
      |(?P<ident>[A-Za-z_][A-Za-z0-9_]*)
      |(?P<punct>::|->|=>|==|!=|<=|>=|&&|\|\||\.\.=?|[^\sA-Za-z0-9_])""",
    re.VERBOSE,
)
OPEN, CLOSE = "([{", ")]}"
MAX_LINES = 20_000  # one invocation longer than this is not a hand-written script


def shape(line: str) -> str:
    """The line with names and literals collapsed and every argument list elided."""
    out: list[str] = []
    depth = 0
    previous = ""
    for match in TOKEN.finditer(line):
        kind = match.lastgroup
        text = match.group()
        if depth > 0:
            if text in OPEN:
                depth += 1
            elif text in CLOSE:
                depth -= 1
                if depth == 0:
                    out[-1] += ".." + text
            continue
        if kind in ("str", "num"):
            token = "lit"
        elif kind == "ident":
            following = line[match.end() : match.end() + 1]
            keep = text in KEYWORDS or previous == "." or following == "!" or text[0].isupper()
            token = text if keep else "x"
        else:
            token = text
        if text == "(" or text == "[":
            depth = 1
        out.append(token)
        previous = text
    joined = " ".join(out)
    for tight in (" . ", " ::", ":: ", " (", " [", " !", " ?", " ;", " ,"):
        joined = joined.replace(tight, tight.strip() + (" " if tight == " ," else ""))
    return joined.strip()


def header_lines(steps: list[dict]) -> set[int]:
    """Every line the map names, nested steps included."""
    lines: set[int] = set()
    pending = list(steps)
    while pending:
        step = pending.pop()
        lines.add(int(step["line"]))
        pending.extend(step.get("steps", []))
    return lines


def header_span(lines: list[tuple[int, str]], start: int) -> set[int]:
    """The lines of a flow header that opens at `start`: through the first ending in `:`.

    The map names a flow by its first line, and a long signature wraps, so the
    parameter and return lines that follow are header, not passthrough.
    """
    span: set[int] = set()
    for number, raw in lines:
        if number < start:
            continue
        span.add(number)
        if raw.rstrip().endswith(":"):
            break
    return span


def body_lines(text: list[str], start: int, column: int) -> list[tuple[int, str]]:
    """The 1-based lines of the invocation opening at `start`, to its closing delimiter."""
    depth = 0
    seen_open = False
    body: list[tuple[int, str]] = []
    for offset, raw in enumerate(text[start - 1 : start - 1 + MAX_LINES]):
        number = start + offset
        line = raw[column - 1 :] if offset == 0 else raw
        body.append((number, raw))
        for character in re.sub(r'"(?:\\.|[^"\\])*"|//.*', "", line):
            if character in OPEN:
                depth += 1
                seen_open = True
            elif character in CLOSE:
                depth -= 1
        if seen_open and depth <= 0:
            return body
    raise ValueError(f"no closing delimiter within {MAX_LINES} lines of line {start}")


def passthrough(document: dict, base: Path) -> tuple[list[tuple[str, int, str]], int]:
    """(path, line, text) of every passthrough line, and the count of script lines read."""
    found: list[tuple[str, int, str]] = []
    total = 0
    sources: dict[str, list[str]] = {}
    for invocation in document["invocations"]:
        if "map" not in invocation:
            continue  # refused: the parser's refusal is the finding, not a shape
        path = invocation["path"]
        text = sources.setdefault(path, (base / path).read_text(encoding="utf-8").splitlines())
        mapped = set()
        lines = body_lines(text, int(invocation["line"]), int(invocation["column"]))
        for flow in invocation["map"]["flows"]:
            mapped |= header_span(lines, int(flow["line"]))
            mapped |= header_lines(flow["steps"])
        for number, raw in lines[1:-1]:
            stripped = raw.strip()
            if not stripped or stripped.startswith("//"):
                continue
            total += 1
            if number not in mapped:
                found.append((path, number, stripped))
    return found, total


def census(maps: list[tuple[str, Path]]) -> dict:
    """Shapes counted across every map, with the repositories and sites behind each."""
    shapes: Counter[str] = Counter()
    sites: dict[str, list[str]] = {}
    repos: dict[str, dict[str, int]] = {}
    for repo, map_path in maps:
        document = json.loads(map_path.read_text(encoding="utf-8"))
        lines, total = passthrough(document, map_path.parent)
        repos[repo] = {
            "files": int(document["files"]),
            "invocations": len(document["invocations"]),
            "script_lines": total,
            "passthrough_lines": len(lines),
        }
        for path, number, text in lines:
            key = shape(text)
            shapes[key] += 1
            sites.setdefault(key, []).append(f"{repo}:{path}:{number}")
    return {
        "repositories": repos,
        "shapes": [
            {"shape": key, "count": count, "sites": sites[key]}
            for key, count in shapes.most_common()
        ],
    }


def render(result: dict, top: int) -> str:
    lines = ["| Repository | Files | script! | Script lines | Passthrough |", "|---|---|---|---|---|"]
    for repo, row in result["repositories"].items():
        share = row["passthrough_lines"] / row["script_lines"] if row["script_lines"] else 0.0
        lines.append(
            f"| {repo} | {row['files']} | {row['invocations']} | {row['script_lines']} | "
            f"{row['passthrough_lines']} ({share:.0%}) |"
        )
    lines += ["", "| Count | Shape | First sites |", "|---|---|---|"]
    for entry in result["shapes"][:top]:
        first = ", ".join(entry["sites"][:3])
        lines.append(f"| {entry['count']} | `{entry['shape']}` | {first} |")
    return "\n".join(lines)


class CensusRegression(unittest.TestCase):
    """The census on a fixture whose answer is known by construction."""

    SOURCE = (
        "fn host() {}\n"
        "lgwks_bot::script! {\n"
        "    flow sizes(pages: Vec<String>) -> usize:\n"
        "        let guard = state.lock(\"a\").await\n"
        "        // a comment is not a line of Rust\n"
        "        let sizes = each page in pages:\n"
        "            page.len()\n"
        "        let other = cache.lock(\"b\").await\n"
        "        give back sizes.len()\n"
        "}\n"
    )

    def document(self) -> dict:
        step = {"kind": "Each", "subject": "", "detail": "", "line": 6, "steps": []}
        give = {"kind": "GiveBack", "subject": "", "detail": "", "line": 9, "steps": []}
        return {
            "verb": "map",
            "files": 1,
            "refused": 0,
            "invocations": [
                {
                    "path": "src/lib.rs",
                    "line": 2,
                    "column": 1,
                    "map": {"flows": [{"name": "sizes", "signature": "", "line": 3, "steps": [step, give]}]},
                }
            ],
            "unreadable": [],
        }

    def test_mapped_lines_and_comments_are_not_passthrough(self) -> None:
        with tempfile.TemporaryDirectory() as scratch:
            base = Path(scratch)
            (base / "src").mkdir()
            (base / "src/lib.rs").write_text(self.SOURCE, encoding="utf-8")
            lines, total = passthrough(self.document(), base)
        self.assertEqual(total, 6)
        self.assertEqual([number for _, number, _ in lines], [4, 7, 8])

    def test_one_shape_counts_two_spellings(self) -> None:
        self.assertEqual(shape('let guard = state.lock("a").await'), "let x = x.lock(..).await")
        self.assertEqual(shape('let other = cache.lock("b").await'), "let x = x.lock(..).await")
        self.assertEqual(shape("page.len()"), "x.len(..)")
        self.assertEqual(shape("Ok(value?)"), "Ok(..)")
        self.assertEqual(shape('tracing::info!("x {y}")'), "x::info!(..)")

    def test_the_census_ranks_the_recurring_shape_first(self) -> None:
        with tempfile.TemporaryDirectory() as scratch:
            base = Path(scratch)
            (base / "src").mkdir()
            (base / "src/lib.rs").write_text(self.SOURCE, encoding="utf-8")
            (base / "map.json").write_text(json.dumps(self.document()), encoding="utf-8")
            result = census([("demo", base / "map.json")])
        self.assertEqual(result["shapes"][0]["shape"], "let x = x.lock(..).await")
        self.assertEqual(result["shapes"][0]["count"], 2)
        self.assertEqual(result["repositories"]["demo"]["passthrough_lines"], 3)

    def test_literals_collapse_and_nested_headers_are_mapped(self) -> None:
        self.assertEqual(shape("let n = 3"), shape('let s = "three"'))
        self.assertEqual(shape("let n = 3"), "let x = lit")
        inner = {"line": 9, "steps": []}
        self.assertEqual(header_lines([{"line": 4, "steps": [{"line": 6, "steps": [inner]}]}]), {4, 6, 9})

    def test_a_wrapped_flow_signature_is_header_not_passthrough(self) -> None:
        lines = [(3, "    flow f("), (4, "        stage: Stage,"), (5, "    ) -> u8:"), (6, "        x.y()")]
        self.assertEqual(header_span(lines, 3), {3, 4, 5})

    def test_a_brace_on_its_own_line_still_opens_the_body(self) -> None:
        lines = body_lines(["lgwks_bot::script!", "{", "    flow f():", "}", "fn after() {}"], 1, 1)
        self.assertEqual([number for number, _ in lines], [1, 2, 3, 4])

    def test_shapes_are_ranked_by_count(self) -> None:
        with tempfile.TemporaryDirectory() as scratch:
            base = Path(scratch)
            (base / "src").mkdir()
            source = self.SOURCE.replace("page.len()", "a.b()\n            a.b()\n            a.b()")
            (base / "src/lib.rs").write_text(source, encoding="utf-8")
            document = self.document()
            document["invocations"][0]["map"]["flows"][0]["steps"][1]["line"] = 11
            (base / "map.json").write_text(json.dumps(document), encoding="utf-8")
            counts = [entry["count"] for entry in census([("demo", base / "map.json")])["shapes"]]
        self.assertEqual(counts, [3, 2])

    def test_an_unclosed_invocation_is_refused_not_counted(self) -> None:
        with self.assertRaises(ValueError):
            body_lines(["script! {", "    flow f():"], 1, 1)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("maps", nargs="*", help="MAP.json or NAME=MAP.json")
    parser.add_argument("--top", type=int, default=25)
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--test", action="store_true")
    args = parser.parse_args(argv)
    if args.test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(CensusRegression)
        return 0 if unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful() else 1
    if not args.maps:
        parser.error("name at least one MAP.json")
    maps = []
    for entry in args.maps:
        name, _, path = entry.rpartition("=")
        map_path = Path(path)
        maps.append((name or map_path.resolve().parent.name, map_path))
    try:
        result = census(maps)
    except (OSError, ValueError, KeyError, json.JSONDecodeError) as error:
        print(f"script-census: {error}", file=sys.stderr)
        return 2
    print(json.dumps(result, indent=2) if args.json else render(result, args.top))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
