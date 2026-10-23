# ADR 0004: documentation governance is machine-checked

Status: **accepted**, 2026-10-22. Director's order of 2026-10-22: "U WILL ALSO
ADD FULL DOCUMENTATION GOVERNANCE WITH ADRS WHICH ARE ISO 42010 COMPLIANT, PRDS
WHICH ARE ISO29148 COMPLIANT." Issue: #732.

Stakeholders: the Director; the executing agent; a reviewer of a decision made before this record; an auditor of the product requirement set
Concerns: a decision recorded without its stakeholders, rationale or verification cannot be reviewed after the fact; a requirement without a fit criterion cannot be verified
Requirements: R13, R12, R1
Viewpoint: the record viewpoint — one view per record, each view the record's own headings against the 42010 or 29148 field list
Elements and relations: the two record sets (`docs/adr`, `docs/prd`), the `check-docs` reader and the `doc-governance` lane; relations: `lane runs reader`, `reader judges record`, `record traces to requirement`

## Context

The repository held two ADRs and no product requirement document. The ADR style
was good prose and no contract: nothing required a record to name its
stakeholders, its concerns, its viewpoint, its elements or its verification, so
a record could read as complete and answer none of the questions 42010 asks of
an architecture description. `REQUIREMENTS.md` held the immutable spine, and a
new product requirement had no home at all.

Two failures follow from prose-only governance, and both were observed here:
a decision's measurements were never re-run, and a requirement's status drifted
from its evidence without anything noticing.

## Decision

1. **Two record sets, two standards, one reader.** `docs/adr/` holds
   architecture decision records to ISO/IEC/IEEE 42010; `docs/prd/` holds
   product requirements to ISO/IEC/IEEE 29148. `docs/adr/README.md` and
   `docs/prd/README.md` list every required field and the clause it comes from.
2. **The fields are read, not trusted.** `check-docs` refuses a record missing a
   field, a status with no date, a section out of the set, a requirement whose
   statement is not normative, a priority outside `must|should|could`, a
   requirement with no fit criterion, no verification or no trace, and a repeated
   identifier. The `doc-governance` lane runs it in local CI and in CI.
3. **A record is superseded, never rewritten.** A later record names the one it
   supersedes in `## Correspondence`; the superseded record's status changes and
   nothing else in it does.
4. **The product requirements stay one set.** A PRD states *new* requirements and
   traces to the `R<id>` spine; it does not restate the spine, because a second
   copy of a policy is a second policy.
5. **Every record is indexed.** `docs/README.md` names each record, and the
   `llms-txt` lane already refuses a document the index does not hold.

## Alternatives rejected

- **Prose review only.** Two records that both look complete can both be missing
  their stakeholders; the failure is invisible and repeats.
- **A template with no reader.** A template is a suggestion. The failure #732
  names is exactly a suggestion that everyone believed was a rule.
- **One record set for both decisions and requirements.** 42010 and 29148 ask
  different questions of different things; one format would answer neither.
- **Copy `REQUIREMENTS.md` into a PRD.** That is the second policy the
  repository's own doctrine refuses.
- **A schema language or a Markdown parser crate.** The records are headings,
  lines and attributes; a dependency for that is a dependency for nothing.

## Consequences

- A new decision or requirement costs a few more lines, and every one of them is
  a field a reader needs.
- `check-docs` is a new binary in `lgwks_deps` with its own tests; it adds no
  dependency, and it reads files rather than building the workspace.
- ADRs 0001 and 0002 gained the fields 42010 requires. Their decisions, their
  measurements and their text are unchanged: the decision is immutable, the
  record's fields are not a rewrite of it.
- A record whose measured evidence goes stale is still review's problem; the
  reader judges the shape, and `## Verification` names the lane that judges the
  claim.

## Correspondence

Refined by ADR 0003, which it checks against. Relates to ADR 0001 and ADR 0002,
which it brought to the field set without changing either decision. Serves
`R13` (a claim carries its evidence class and gate) and PRD 0001.

## Verification

The `doc-governance` lane: `cargo run --locked -p lgwks_deps --bin check-docs`,
which exits 2 on any finding. Its reader is held by
`check_docs::tests::a_complete_record_passes` (the accepted shape), and by one
refusal per field:
`an_adr_without_stakeholders_is_refused`,
`an_adr_without_alternatives_is_refused`,
`a_requirement_without_a_fit_criterion_is_refused`,
`a_statement_without_must_and_an_unknown_priority_are_refused`,
`a_repeated_requirement_identifier_is_refused`,
`a_status_a_number_and_a_heading_are_read_exactly`.
