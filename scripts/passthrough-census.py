#!/usr/bin/env python3
"""The passthrough census: which plain-Rust shapes recur across `script!` flows.

Reads the document `lgwks-ast script map --json` prints on stdin and prints a
Markdown table of the most frequent passthrough shapes, with the files read
and the counts, as `docs/script-word-admission.md` section 7 describes. A
shape that recurs at three or more sites is a candidate word (SL-7); the
census lists candidates and never gates, because a gated share would be
gamed by moving code out of scripts.

A shape is the line with its bindings, receivers, arguments and literals
blanked and its keywords, types, called names and punctuation kept, so
`let a = site.get(id)` and `let b = page.get(n)` are one shape,
`let _ = _.get(_)`.

    lgwks-ast script map --json . | python3 scripts/passthrough-census.py [--top N]
    python3 scripts/passthrough-census.py --test
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import defaultdict

# Rust's strict and reserved keywords, which carry the line's structure.
KEYWORDS = frozenset(
    "as async await break const continue crate dyn else enum extern false fn for "
    "if impl in let loop match mod move mut pub ref return self Self static struct "
    "super trait true type unsafe use where while".split()
)

# One lexical token: a string or char literal, a number, an identifier
# (lifetimes and raw identifiers included), or one punctuation character.
TOKEN = re.compile(
    r'''b?"(?:\\.|[^"\\])*"'''
    r"""|b?'(?:\\.|[^'\\])'"""
    r"|'[A-Za-z_][A-Za-z0-9_]*"
    r"|\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d+)?[A-Za-z0-9_]*"
    r"|r#[A-Za-z_][A-Za-z0-9_]*|[A-Za-z_][A-Za-z0-9_]*"
    r"|\S"
)


def shape(text: str) -> str:
    """The line with names and literals blanked, tokens joined by one space
    except around the punctuation that never takes one."""
    tokens = TOKEN.findall(text)
    out: list[str] = []
    for index, token in enumerate(tokens):
        following = tokens[index + 1] if index + 1 < len(tokens) else ""
        if token[0] in "\"'" or token[:2] in ('b"', "b'"):
            out.append("_" if not token.startswith("'") or len(token) <= 3 else token)
        elif token[0].isdigit():
            out.append("0")
        elif token[0].isalpha() or token[0] == "_" or token.startswith("r#"):
            macro = following == "!" and tokens[index + 2 : index + 3] in (["("], ["["], ["{"])
            called = following == "(" or macro or (following == ":" and _is_path(tokens, index))
            if token in KEYWORDS or token[0].isupper() or called:
                out.append(token)
            else:
                out.append("_")
        else:
            out.append(token)
    return _join(out)


# Punctuation written with no space before it, and with none after it.
TIGHT_BEFORE = frozenset(".,;?)]:")
TIGHT_AFTER = frozenset(".([&!")


def _join(tokens: list[str]) -> str:
    """Tokens laid out the way rustfmt spaces them: `a.b(c, d)?`, `x: T`,
    `a::b`, `f!(x)`, and one space around everything else."""
    rendered = ""
    for index, token in enumerate(tokens):
        previous = tokens[index - 1] if index else ""
        path = token == ":" and (previous == ":" or tokens[index + 1 : index + 2] == [":"])
        glued = (
            not rendered
            or token in TIGHT_BEFORE
            or previous in TIGHT_AFTER
            or (token in "([" and (previous[-1:].isalnum() or previous in "_!)]>"))
            or (previous == ":" and rendered.endswith("::"))
            or path
            or (token == "!" and previous[-1:].isalnum() and tokens[index + 1 : index + 2] in (["("], ["["]))
        )
        rendered += token if glued else " " + token
    return rendered


def _is_path(tokens: list[str], index: int) -> bool:
    """Whether the identifier at `index` is a path segment (`a::b`)."""
    return tokens[index + 1 : index + 3] == [":", ":"]


def census(document: dict) -> tuple[dict, list[str], dict]:
    """Group every passthrough line of `document` by shape."""
    sites: dict[str, list[str]] = defaultdict(list)
    files: set[str] = set()
    totals = {"invocations": 0, "flows": 0, "steps": 0, "passthrough": 0, "refused": 0}
    for invocation in document.get("invocations", []):
        totals["invocations"] += 1
        if "refusal" in invocation:
            totals["refused"] += 1
            continue
        path = invocation["path"].removeprefix("./")
        files.add(path)
        for flow in invocation.get("map", {}).get("flows", []):
            totals["flows"] += 1
            totals["steps"] += _count_steps(flow.get("steps", []))
            for passed in flow.get("passthrough", []):
                totals["passthrough"] += 1
                sites[shape(passed["text"])].append(f"{path}:{passed['line']}")
    return sites, sorted(files), totals


def _count_steps(steps: list[dict]) -> int:
    return sum(1 + _count_steps(step.get("steps", [])) for step in steps)


def render(sites: dict, files: list[str], totals: dict, top: int) -> str:
    """The census as Markdown: totals, the files read, and the table."""
    lines = []
    read = totals["steps"] + totals["passthrough"]
    share = f"{100 * totals['passthrough'] / read:.1f}%" if read else "n/a"
    lines.append(
        f"{totals['invocations']} `script!` in {len(files)} files, {totals['flows']} flows; "
        f"{totals['steps']} word steps and {totals['passthrough']} passthrough lines "
        f"(passthrough share {share}); {totals['refused']} refused and not read; "
        f"{len(sites)} distinct shapes."
    )
    lines.append("")
    lines.append("| # | Lines | Files | Shape | Sites (first three) |")
    lines.append("|---|---|---|---|---|")
    ranked = sorted(sites.items(), key=lambda item: (-len(item[1]), item[0]))
    for rank, (key, where) in enumerate(ranked[:top], start=1):
        spread = len({site.rsplit(":", 1)[0] for site in where})
        shown = ", ".join(f"`{site}`" for site in where[:3])
        lines.append(f"| {rank} | {len(where)} | {spread} | `{key}` | {shown} |")
    lines.append("")
    lines.append("Files read:")
    lines.extend(f"- `{name}`" for name in files)
    return "\n".join(lines)


def self_test() -> int:
    cases = [
        ("let a = site.get(id)", "let _ = _.get(_)"),
        ("let b = page.get(n)", "let _ = _.get(_)"),
        ('site.note("busy", 3).await?', "_.note(_, 0).await?"),
        ("let o: u16 = Duration::from_millis(5)", "let _: _ = Duration::from_millis(0)"),
        ("let left = site.pair(\n    1,\n    items.len())", "let _ = _.pair(0, _.len())"),
        ("assert!(x > 2)", "assert!(_ > 0)"),
        ("let same = a != b", "let _ = _ != _"),
        ("let n = r#type.len()", "let _ = _.len()"),
    ]
    failed = [(text, want, shape(text)) for text, want in cases if shape(text) != want]
    for text, want, got in failed:
        print(f"shape({text!r}) = {got!r}, want {want!r}", file=sys.stderr)
    document = {
        "invocations": [
            {"path": "./a.rs", "line": 1, "map": {"flows": [{
                "steps": [{"steps": []}],
                "passthrough": [{"line": 3, "text": "let a = site.get(id)"},
                                {"line": 4, "text": "let b = page.get(n)"}]}]}},
            {"path": "b.rs", "line": 1, "refusal": {"line": 2, "column": 1, "message": "no"}},
        ]
    }
    sites, files, totals = census(document)
    expected = {"let _ = _.get(_)": ["a.rs:3", "a.rs:4"]}
    if dict(sites) != expected or files != ["a.rs"] or totals["refused"] != 1 or totals["steps"] != 1:
        print(f"census: {dict(sites)} {files} {totals}", file=sys.stderr)
        failed.append(("census", "", ""))
    print("passthrough-census self-test: " + ("FAILED" if failed else f"{len(cases) + 1} cases hold"))
    return 1 if failed else 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--top", type=int, default=20, help="rows to print (default 20)")
    parser.add_argument("--test", action="store_true", help="run the self-test and exit")
    args = parser.parse_args()
    if args.test:
        return self_test()
    try:
        document = json.load(sys.stdin)
    except json.JSONDecodeError as error:
        print(f"passthrough-census: stdin is not the map document: {error}", file=sys.stderr)
        return 2
    print(render(*census(document), args.top))
    return 0


if __name__ == "__main__":
    sys.exit(main())
