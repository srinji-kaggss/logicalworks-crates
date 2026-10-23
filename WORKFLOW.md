# Logical Works Workflow

> Provenance: copied verbatim from the estate governance suite
> (`logical-DB/CODEBOOK.md`, `logical-DB/WORKFLOW.md`) on 2026-09-21 and
> maintained here as this repository's copy. Where a rule names this
> repository's own gates, `scripts/ci-local.sh` is the definition.
>
> Split on 2026-10-22: the estate size law holds every Markdown file to 120 lines
> (Director, 2026-10-10), and this file had grown to 445. Each section moved
> whole to `docs/workflow/`, kept its number, and nothing cites a moved section
> by file. One rule changed in the move: §4's storefront carve-out, superseded by
> ADR 0003 ("every capability is on by default").

How work moves through this repository. Code rules are in `CODEBOOK.md`.
Authority, decisions, and the completion ledger are in `GOVERNANCE.md`. The
agent entry point is `AGENTS.md`.

| § | Section | File |
|---|---|---|
| 1 | Local CI | [`docs/workflow/local-ci.md`](docs/workflow/local-ci.md) |
| 2, 3 | The delivery loop; RAG yourself into expertise | [`docs/workflow/delivery-loop.md`](docs/workflow/delivery-loop.md) |
| 4, 5 | One path, one opinion; compounding and surgical change | [`docs/workflow/one-path.md`](docs/workflow/one-path.md) |
| 6, 7 | Evidence proportionality; work ends with a PR | [`docs/workflow/evidence.md`](docs/workflow/evidence.md) |
| 8, 9 | Agent economics; data safety and the guard | [`docs/workflow/agents.md`](docs/workflow/agents.md) |
| 10, 11 | The runner map; the mandatory WWFD cycle | [`docs/workflow/local-ci.md`](docs/workflow/local-ci.md) |
| 12 | The CI specification | [`docs/workflow/ci-layout.md`](docs/workflow/ci-layout.md) |
| 12 | The CI run table, 25 runs | [`docs/workflow/ci-runs.md`](docs/workflow/ci-runs.md) |

## The rules a reader needs before the first tool call

These are copies of the shortest rules, and the section files are the authority.
They are repeated here because a rule that is one link away is a rule the first
call already broke.

1. The first substantive tool call reads the source you will edit, runs a build
   or a test, writes source, or asks CodeGraph where the code is. Never a ritual.
2. Within five minutes you have changed a source file or named `file:line` and
   the exact change it needs.
3. Process words never exceed 20% of a response.
4. `./scripts/ci-local.sh` is the gate; `scripts/gate-lanes.toml` is its one
   definition, and CI runs the same lanes.
5. Evidence is proportional: product runtime gets the nine-axis matrix, docs and
   CI get one line per file.
6. Work ends with a pushed branch and a PR carrying its receipts.
7. Deletion is `trash`. Never `rm`, `git clean` or `git reset --hard`.
8. A guard refusal is a correction: fix it and carry on.
