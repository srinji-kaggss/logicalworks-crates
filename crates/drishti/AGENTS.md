# Drishti contributor contract

Read README.md, VISION.md, docs/decisions.md, docs/codebase.md, and docs/assurance.md before changing this project. The vision is primary; mechanisms and scores cannot replace it.

## Scope and truth

- Published 0.0.1 is documentation-only. The working tree realizes the presentation seats named in D15/D16 (`design`, `ink_lab`, `runtime`, `native`). Do not turn other planned modules into empty runtime scaffolds or advertise undocumented functionality.
- Separate user direction, proposals, blocked obligations, implemented behavior, and executed evidence. A declaration is not a capability.
- Keep one owner per concern: requirements in contract/requirements.tsv; journeys in experience/journeys.tsv; structure in docs/codebase.md; decisions/gaps in docs/decisions.md; observed status in docs/status.md.
- Preserve existing work and source provenance. Do not edit sibling repositories or introduce dependencies merely to complete this repository's plan.
- Public package contents must contain no credentials, private source copies, proprietary detector implementation, user data, or machine-specific required paths.

## Before runtime implementation

Resolve correctness-critical prerequisites for the exact implementation scope. State domain, owner, trust boundary, states/transitions, ordering, failure/recovery, resource limits, compatibility, and independent falsification cases. Retain the wider unmet requirements; a narrow proof is not a reduced product.

Publish the lead-in: **This is how the code I have written or will write attempts to be frontier and universal.** Follow it with concrete research, invariants, supported domain and limits, matched comparison criteria, and actual/planned verification evidence. The sentence alone does not admit implementation.

Use DMADV for new design and DMAIC for measured improvement. Every Director counterexample is an acceptance case. Unknown or unavailable evidence blocks claims; do not lower gates, hide skips, or invent certification or universal superiority.

## Dependencies and authority

Use std and the existing logicalworks-crates surfaces first. New third-party engines are optional registered features owned by lgwks_deps, not direct imports or a new owner crate. Verify the actual consumer API; documentation does not create a missing re-export.

Reuse GPUI for presentation. Do not create competing LOK/Braid/Secure Authority/Forge authority types in their integration profile. Standalone hosts supply their own domain authority through an explicit contract. Trusted native code is not a sandbox.

## Verification

Run the exact commands in README.md. Tests validate the document bundle only. Product acceptance additionally requires all applicable docs/assurance.md families, real public journeys, independent evidence, resource measurements, and restored Keel integration. No test output may be labeled a runtime pass when it only built documentation.

Report changed files, executed commands, results, and blockers. Commit, push, publish, or change repository visibility only with explicit task authority. Crates.io publication is public and effectively persistent; review the exact packaged files first.
