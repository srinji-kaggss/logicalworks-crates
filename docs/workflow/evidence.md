# Workflow §6, §7: evidence proportionality, and how work ends

Moved verbatim from `WORKFLOW.md` on 2026-10-22, when every Markdown file in a
repository was held to 120 lines (Director, 2026-10-10). No rule changed in the
move; `WORKFLOW.md` is the index and keeps the § numbers.

---

## 6. Evidence proportionality

| Change | Evidence |
|---|---|
| Product runtime | full nine-axis SLO matrix, measured per axis |
| Config, docs, CI, README, scripts | one line of receipt per file |
| Instruction or config loading | config-loading evidence only |

**The nine axes.** Frontier (design beats credible alternatives), Hyperscale
(>1M concurrent, correct), Idiomatic (ownership, errors, lifetimes sound),
Generalized (works across declared inputs), Decoupled (components independently
replaceable), Ephemeral (survives process loss), Portable (same semantics on all
targets), Multi-tenant (isolated under concurrent use), Performance (fastest
correct implementation).

Every claim carries evidence: command, exit code, result, artifact ID. Before
completion, account for every requirement, every failed check, and every
unknown. No hidden exclusions.

AI and model output is untrusted first-party evidence until independently
verified. Every capability claim separately states which of these it is:
**planned**; **present in code at an exact commit**; **exercised on the exact
claimed path**; **independently evidenced**; **safe to rely on or merge**. Never
collapse these into "done".

---

## 7. Work ends with a PR

- **Always commit.** Never leave verified-green work uncommitted and never end a
  turn with it in the working tree. Uncommitted work has not carried forward.
- Commit in small, individually-green steps.
- **A local commit is not the finish line.** Work ends with a GitHub PR: branch,
  commits, pushed, PR opened with its evidence. A branch with no PR is unfinished
  work, and a green tree on `main` that was never proposed has not shipped.
- The PR carries the receipts: gate commands and their results. Not a narrative
  about them.

### Merge gate

Source review may proceed against local receipts. Merge eligibility requires
executed reproduction of the affected lanes on hosted or self-hosted CI. A
bounded exception requires explicit Director authorization recorded in
`GOVERNANCE.md` with its replacement evidence enumerated. A local PASS claim
alone is never a satisfied execution gate.

Production completion additionally requires a ratified product scope and every
applicable acceptance row satisfied. A green PR does not close unfinished
roadmap issues. Do not copy historical checkmarks into a current release claim.
