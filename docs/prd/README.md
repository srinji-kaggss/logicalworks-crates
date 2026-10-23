# Product requirements

Every product requirement of this repository is written to **ISO/IEC/IEEE
29148**, one requirement per `### PR-<document>.<n>` block, and `check-docs` in
the `doc-governance` lane refuses a requirement that is missing an attribute.

`REQUIREMENTS.md` is the immutable spine (`R1`..`R13`), superseded never edited.
A PRD does not restate it: a PRD states a *new* requirement and traces to the
spine, so there is one policy and not two.

| Attribute | 29148 clause | What it must say |
|---|---|---|
| `### PR-0001.1 Title` | 5.2.5 — identifier | one identifier, one requirement, unique in the set |
| `**Statement.**` | 5.2.5 — singularity, unambiguity | one capability, normative: it says what the system MUST do |
| `**Rationale.**` | 5.2.5 — rationale | why the requirement exists |
| `**Source.**` | 5.2.7 — source | the authority and the date: the Director, a standard, an incident |
| `**Priority.**` | 5.2.7 — priority | `must`, `should` or `could` |
| `**Fit criterion.**` | 5.2.5 — verifiability | the observation that decides it, as a number or an outcome |
| `**Verified by.**` | 6.4 — verification | the test, gate or lane that makes the observation |
| `**Traces to.**` | 5.2.7 — traceability | the `R<id>` of `REQUIREMENTS.md` or the ADR this refines |

A PRD also holds four sections: `## Stakeholders and operational concept` (who
the product is for and the environment it runs in), `## Requirements`,
`## Assumptions and constraints`, and `## Exclusions` (what the document
deliberately does not require, so silence is never read as a requirement).

## Rules

1. Numbered, append-only: `NNNN-short-name.md`, four digits, one product scope
   each.
2. A requirement is superseded, never edited. A changed `**Statement.**` is a
   supersession: the old block keeps its identifier and gains
   `**Superseded by.** PR-<document>.<n>`, and the new block takes the next one.
3. A requirement with no `**Fit criterion.**` is not a requirement yet: it is a
   wish, and the lane refuses it by name.
4. The whole set's identifiers are unique across documents.

## Index

| Document | Scope |
|---|---|
| [0001](0001-discoverable-opinionated-estate.md) | Every capability on by default, one catalog, complete tracing, documentation governance |
