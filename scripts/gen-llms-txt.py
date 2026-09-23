#!/usr/bin/env python3
"""Generate llms.txt from the repository's own documentation index.

`llms.txt` is a curated index an agent can fetch once instead of crawling the
repository. Its whole value is being current: an index that points at documents
which moved is worse than no index, because the agent trusts it and stops
looking. So this file is generated and never hand-edited, and CI re-runs this
script and fails when the committed copy differs.

The index has exactly one source: `README.md`. Its crates table and its
Documentation table are already the human-facing index, and a second list would
be a second thing to drift out of date. This script reads those two tables,
checks every path they name exists, checks no document in `docs/` is missing
from the table, and writes `llms.txt`.

Usage:
    python3 scripts/gen-llms-txt.py

Exit status is non-zero if the index is inconsistent, so the same script works
as a check.
"""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
README = ROOT / "README.md"
OUTPUT = ROOT / "llms.txt"

# Table rows whose first cell carries a link: `| [`path`](path) | description |`
LINK_ROW = re.compile(r"^\|\s*\[`(?P<text>[^`]+)`\]\((?P<target>[^)]+)\)")


def cells(row: str) -> list[str]:
    """Split a markdown table row into trimmed cells, dropping the outer pipes."""
    return [cell.strip() for cell in row.strip().strip("|").split("|")]


def section(text: str, heading: str) -> list[str]:
    """Return the lines of the `## <heading>` section, excluding the heading."""
    lines = text.splitlines()
    try:
        start = next(i for i, line in enumerate(lines) if line.strip() == f"## {heading}")
    except StopIteration:
        raise SystemExit(f"README.md has no '## {heading}' section") from None
    body: list[str] = []
    for line in lines[start + 1 :]:
        if line.startswith("## "):
            break
        body.append(line)
    return body


def title_of(path: Path) -> str:
    """The document's own H1, skipping YAML front matter, else its path."""
    lines = path.read_text(encoding="utf-8").splitlines()
    index = 0
    if lines and lines[0].strip() == "---":
        index = next((i + 1 for i, line in enumerate(lines[1:], start=1) if line.strip() == "---"), 0)
    for line in lines[index:]:
        if line.startswith("# "):
            return line[2:].strip()
    return path.name


def first_paragraph(text: str) -> str:
    """The paragraph under the H1, joined onto one line."""
    lines = text.splitlines()
    try:
        start = next(i for i, line in enumerate(lines) if line.startswith("# "))
    except StopIteration:
        raise SystemExit("README.md has no H1") from None
    paragraph: list[str] = []
    for line in lines[start + 1 :]:
        if not line.strip():
            if paragraph:
                break
            continue
        paragraph.append(line.strip())
    return " ".join(paragraph)


def repository_url() -> str:
    """The crate's `repository` field, which is the repository root."""
    manifest = tomllib.loads((ROOT / "crates/lgwks-std/Cargo.toml").read_text(encoding="utf-8"))
    url = manifest["package"]["repository"]
    return url.rstrip("/")


def main() -> int:
    readme = README.read_text(encoding="utf-8")
    repo = repository_url()
    problems: list[str] = []

    def blob(path: str) -> str:
        # `HEAD`, and the comment that used to sit here said the opposite of what
        # this does: it claimed HEAD was the ref a release could not move, when
        # HEAD is precisely the ref that moves on every commit. A tag is the ref
        # a release cannot move.
        #
        # HEAD is still the right target for *this* file. It is regenerated per
        # commit and CI fails if it drifts, and a committed SHA cannot name its
        # own commit without going stale the moment it lands. The cost is that
        # every link below tracks `main` and none of them is a stable reference,
        # so the index says so at the top rather than leaving a reader to infer
        # it. A release-pinned index is a separate artifact and does not exist.
        return f"{repo}/blob/HEAD/{path}"

    crates: list[tuple[str, str, str]] = []
    for row in section(readme, "Crates"):
        if not row.startswith("| `"):
            continue
        row_cells = cells(row)
        crate = row_cells[0].strip("`")
        match = re.search(r"\((https://docs\.rs/[^)]+)\)", row_cells[2])
        if match is None:
            problems.append(f"crates table row for {crate} has no docs.rs link")
            continue
        crates.append((crate, match.group(1), row_cells[-1]))

    documents: list[tuple[str, str, str]] = []
    for row in section(readme, "Documentation"):
        match = LINK_ROW.match(row)
        if match is None:
            continue
        target, description = match.group("target"), cells(row)[-1]
        path = ROOT / target
        if not path.is_file():
            problems.append(f"the Documentation table names {target}, which does not exist")
            continue
        documents.append((title_of(path), target, description))

    # A document nobody indexed is invisible to every reader that arrives
    # through this file, which for an agent is every reader.
    #
    # Recursive, not `glob("*.md")`. The flat form only ever saw the top level,
    # so the moment `docs/guides/` existed it would have passed by not looking.
    # It would in fact have passed on the day those guides landed, but only
    # because they happened to be added to the README table, which is luck
    # rather than a rule. A check that cannot fail is not a check.
    #
    # A document deliberately kept out of the consumer index is named here with
    # its reason, so the way to exempt one is to write down why rather than to
    # move it somewhere the check does not reach.
    NOT_INDEXED: dict[str, str] = {}
    indexed = {target for _, target, _ in documents}
    for path in sorted((ROOT / "docs").rglob("*.md")):
        relative = path.relative_to(ROOT).as_posix()
        if relative in indexed or relative in NOT_INDEXED:
            continue
        problems.append(f"{relative} exists but the README Documentation table omits it")

    # A crate nobody indexed is invisible to every reader that arrives through
    # this file, which for an agent is every reader.
    for directory in sorted(path for path in (ROOT / "crates").iterdir() if path.is_dir()):
        crate = directory.name.replace("-", "_")
        if crate not in {name for name, _, _ in crates}:
            problems.append(f"crates/{directory.name} exists but the README crates table omits it")

    if problems:
        for problem in problems:
            print(f"error: {problem}", file=sys.stderr)
        return 1

    out: list[str] = [
        "# logicalworks-crates",
        "",
        f"> {first_paragraph(readme)}",
        "",
        f"Source: {repo}",
        "",
        "Development index. Every link below resolves against `main` as it stood "
        "when this file was generated, and `main` moves. The crates.io links name "
        "the latest published version and the docs.rs links describe it; the "
        "documentation links describe unreleased source. For a released version, "
        "use that release's tag rather than anything below.",
        "",
        "## Crates",
        "",
    ]
    out += [f"- [{name}]({docs}) — {description}" for name, docs, description in crates]
    out += ["", "## Documentation", ""]
    out += [f"- [{title}]({blob(target)}) — {description}" for title, target, description in documents]

    # Only what the Documentation table does not already carry. The README
    # indexes the changelog and the security policy, and listing them twice
    # would make the file look longer without making it say more.
    optional = [
        ("CHANGELOG.md", "Per-release changes across all four crates"),
        ("SECURITY.md", "Attack surface, reporting process, and advisories assessed"),
        ("AGENTS.md", "How to add a dependency to this repository"),
        ("REQUIREMENTS.md", "Immutable product requirements, superseded never edited"),
        ("INVARIANTS.md", "Short enforced rule list for this repository"),
        ("contract/APPROVED.toml", "Every approved external edge, and its owner"),
    ]
    remaining = [(target, description) for target, description in optional if target not in indexed]
    if remaining:
        out += ["", "## Optional", ""]
        out += [f"- [{target}]({blob(target)}) — {description}" for target, description in remaining]
    out += [""]

    text = "\n".join(out)
    OUTPUT.write_text(text, encoding="utf-8")
    print(f"wrote {OUTPUT.relative_to(ROOT)} — {len(crates)} crates, {len(documents)} documents")
    return 0


if __name__ == "__main__":
    sys.exit(main())
