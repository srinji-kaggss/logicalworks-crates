# Architecture decision records

Every architecture decision in this repository is a record here, and the record
is held to **ISO/IEC/IEEE 42010** by `check-docs` in the `doc-governance` lane.
Prose review does not catch a missing field; a refusal does.

| Field | 42010 clause | What it must say |
|---|---|---|
| `Status:` | 5.2 — an AD identifies its state | `proposed`, `accepted`, `superseded`, `rejected` or `deprecated`, and the `YYYY-MM-DD` it was decided |
| `Stakeholders:` | 5.1 — stakeholders | who holds a concern about this decision |
| `Concerns:` | 5.1 — concerns | the interest in the system this record answers |
| `Requirements:` | 6.5 — rationale | the `REQUIREMENTS.md` ids (`R1`) the decision serves; a decision that traces to nothing is not one |
| `Viewpoint:` | 5.2 — viewpoints | the model kind and conventions the view is written in |
| `Elements and relations:` | 5.2 — views | the elements (crates, modules, features) and the relations between them the record describes |
| `## Context` | 6.5 — rationale | the situation the decision answers |
| `## Decision` | 6.5 — rationale | what was decided, in the imperative |
| `## Alternatives rejected` | 8.2 — rationale | each credible alternative, why it lost, and what it cost |
| `## Consequences` | 8.2 — rationale | what the decision costs, including what stays open |
| `## Correspondence` | 7.3 — correspondence | the other records this one supersedes, refines or relates to |
| `## Verification` | 8.3 — conformance | the test, gate or lane that holds the decision true, or `review only` with its owner |

## Rules

1. Numbered, append-only: `NNNN-short-name.md`, four digits, one decision each.
2. **A record is superseded, never rewritten.** A later decision adds a record
   that names the one it supersedes in its `## Correspondence`. The superseded
   record's `Status:` changes to `superseded` and nothing else in it changes.
3. A decision that changes the architecture without a record is a change nobody
   can review after the fact, and the `doc-governance` lane refuses the tree for
   the missing *field*, not for the missing judgement: review is still the owner
   of whether the decision was right.
4. The measurements a record cites are commands and their output. An estimate
   is labelled one and is replaced by a measurement before the claim stands.

## Index

| Record | Decision |
|---|---|
| [0001](0001-one-rust-reader.md) | One Rust reader, `lgwks_ast` |
| [0002](0002-scan-routed-through-lgwks-ast.md) | The scan reads Rust through `lgwks_ast` |
| [0003](0003-every-capability-on-by-default.md) | Every capability is on by default |
| [0004](0004-documentation-governance.md) | ADRs are 42010, PRDs are 29148, both machine-checked |
| [0005](0005-bot-core-is-an-event-driven-ecs.md) | The bot core is an event-driven ECS; tokio and the executors are drivers below it (superseded by 0006) |
| [0006](0006-one-ecs-world-one-tokio-axum-driver.md) | One ECS world is the core; one driver on tokio and axum carries events in and effects out |

New records go at the bottom of that table.
