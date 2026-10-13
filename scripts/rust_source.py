#!/usr/bin/env python3
"""Read Rust source as code: the lexer and `use`-tree reader behind
`scripts/check-std-first.py`.

Split out of that script under the size law (Director 2026-10-10): this file
owns *reading* Rust, the audit owns *judging* what it reads. Nothing here
knows about the register, the workspace, or a finding.

Usage:
    python3 scripts/check-std-first.py --test    (runs this file's suite too)
"""

from __future__ import annotations

import re
import unittest

# `use path::to::thing;`, `pub use …`, `pub(crate) use …`. The whole statement
# is captured rather than only its first segment: what a statement binds is what
# a later statement may resolve through, and reading only the root loses that.
# `[^;]*` spans newlines, which a wrapped `use` group needs.
USE = re.compile(
    r"^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?use[ \t]+(?P<body>[^;]*);",
    re.MULTILINE,
)

def code_only(text: str) -> str:
    """The text with every comment and literal blanked, offsets preserved.

    A `use` statement or a bare `name::` path can only be code, so the scan has
    to tell code from a string or comment that happens to spell one. Two real
    cases forced this. `crates/lgwks-deps/src/scan.rs` *is* a source scanner,
    and its pattern table holds `"tracing::"`; reporting that as a reach into
    `tracing` is the checker reporting a detector as the thing it detects. And
    `crates/lgwks-ast/src/script/refuse.rs` (then in `lgwks-macros`) writes a diagnostic whose continuation
    line starts with `use `…`;`, which the `use` pattern read as a statement:
    the prose became a "crate", bound itself as a name, resolved through that
    binding, and was printed among the crates the estate reaches (#195).

    This is a lexer for exactly the Rust forms that can hide code-like text:
    line and nested block comments, string and byte-string literals with
    escapes, raw strings with any number of `#`, and character literals, which
    are told apart from lifetimes by their closing quote. Every blanked
    character becomes a space and every newline is kept, so an offset and a
    line number mean the same thing in the result as in the file.
    """
    out = list(text)
    length = len(text)

    def blank(start: int, end: int) -> None:
        for index in range(start, min(end, length)):
            if out[index] != "\n":
                out[index] = " "

    index = 0
    while index < length:
        char = text[index]
        after = text[index + 1] if index + 1 < length else ""
        if char == "/" and after == "/":
            end = text.find("\n", index)
            end = length if end == -1 else end
            blank(index, end)
            index = end
        elif char == "/" and after == "*":
            depth, cursor = 1, index + 2
            while cursor < length and depth:
                pair = text[cursor : cursor + 2]
                if pair == "/*":
                    depth, cursor = depth + 1, cursor + 2
                elif pair == "*/":
                    depth, cursor = depth - 1, cursor + 2
                else:
                    cursor += 1
            blank(index, cursor)
            index = cursor
        elif char == "r" and (index == 0 or not (text[index - 1].isalnum() or text[index - 1] == "_") or text[index - 1] == "b"):
            hashes = 0
            while index + 1 + hashes < length and text[index + 1 + hashes] == "#":
                hashes += 1
            if index + 1 + hashes < length and text[index + 1 + hashes] == '"':
                closing = '"' + "#" * hashes
                end = text.find(closing, index + 2 + hashes)
                end = length if end == -1 else end + len(closing)
                blank(index, end)
                index = end
            else:
                index += 1
        elif char == '"':
            cursor = index + 1
            while cursor < length and text[cursor] != '"':
                cursor += 2 if text[cursor] == "\\" else 1
            blank(index, cursor + 1)
            index = cursor + 1
        elif char == "'":
            if after == "\\":
                end = text.find("'", index + 3 if text[index + 2 : index + 3] == "'" else index + 2)
                end = length if end == -1 else end + 1
                blank(index, end)
                index = end
            elif index + 2 < length and text[index + 2] == "'" and after != "\n":
                blank(index, index + 3)
                index += 3
            else:
                index += 1
        else:
            index += 1
    return "".join(out)


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


class CodeOnlyRegression(unittest.TestCase):
    """What `code_only` must blank, and what it must leave as code."""

    def scan_uses(self, source: str) -> list[str]:
        return [m.group("body") for m in USE.finditer(code_only(source))]

    def test_a_use_line_inside_a_multiline_string_is_not_a_statement(self) -> None:
        # The refuse.rs shape that put prose into the crate list (#195).
        source = 'let m = "flows stop here: \\\n    use `?` or `.or_fail()?` (permanent)";\nuse std::fmt;\n'
        self.assertEqual(self.scan_uses(source), ["std::fmt"])

    def test_raw_strings_comments_and_char_literals_hide_code_like_text(self) -> None:
        source = (
            'let a = r#"use serde::Value; "quoted" tokio::spawn"#;\n'
            "/* use regex::Regex; /* nested */ still comment */\n"
            "let q = '\"'; let e = '\\''; // tokio::spawn\n"
            "use lgwks_std::json;\n"
        )
        code = code_only(source)
        self.assertEqual(self.scan_uses(source), ["lgwks_std::json"])
        self.assertNotIn("tokio::", code)
        self.assertNotIn("regex::", code)

    def test_lifetimes_stay_code_and_offsets_are_preserved(self) -> None:
        source = "fn f<'a>(x: &'a str) -> &'a str { serde_json::from_str(x) }\n\"\n\""
        code = code_only(source)
        self.assertEqual(len(code), len(source))
        self.assertEqual(code.count("\n"), source.count("\n"))
        self.assertIn("serde_json::", code)
        self.assertIn("'a", code)
