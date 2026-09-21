# AI workloads and AI consumers of the orchestration SDK

Status: **proposed**, tracked by [#87](https://github.com/srinji-kaggss/logicalworks-crates/issues/87).
These requirements specialize the generic task lifecycle; they do not install
a planner, model client or second agent framework inside `lgwks_bot`.
Primary research and exact current code are in [the evidence record](orchestration-evidence.md).

## Two different problems

**AI-01.** Evaluate both an AI writing a task script and an AI executing inside
a task. The former needs an inferable, small, compiled API; the latter needs
bounded authority, stable observations, reliable effects and compact evidence.
A tool schema alone solves neither. Keep the same interface usable without AI.

| Struggle | Required mechanism, not another prompt rule |
|---|---|
| The model picks the wrong API or invents a method | One canonical task facade, narrow typed descriptors, version-qualified docs and compiled examples |
| It recreates queues/retry loops in each script | Host-owned lifecycle with direct outputs and inherited policy |
| It interprets timeout as no effect | Typed occurrence knowledge and attempt-bound reconciliation |
| It forgets work after a context reset | Host-held run record and a compact queryable resume view |
| Parallel workers duplicate or overwrite work | Stable subjects, single dispatch owner, conflict domains and immutable worker inputs |
| It repeats the same failed action | Typed failure fingerprint, no-progress detection and finite repair/replan budgets |
| It says done because a tool returned zero | Independent domain postconditions and coverage evidence |
| It floods the model with tools/logs | Relevant schema discovery, bounded artifact retrieval, explicit omitted evidence |
| Untrusted repository/page content supplies instructions | Trust-labelled data, separate trusted task intent, confined adapters and dispatch policy |

## Data plane versus decision plane

**AI-02.** Treat model output as a typed proposal. Parse against a versioned
closed schema, reject unknown operations/fields where the schema forbids them,
validate bounds and resolve referenced artifacts before admission. A model may
propose task expansion or arguments; only the deterministic host may accept
that expansion and authorize effects. Existing dispatched operations are
immutable. Dynamic append records the parent's identity and reason.

A model-produced `done` flag is not a terminal receipt. A model-generated
executable or script is untrusted code: apply the sandbox/authority policy,
not merely JSON validation. No automatic capabilities derived from prose.
A page, PR comment, source file or tool response cannot change the trusted
user intent or install an adapter.

**AI-03.** Route model calls through an injected Execute/Query-compatible
adapter as appropriate to the effect contract. Network requests can cost money,
consume quota and disclose data even when their output is text. The adapter
must report transport/dispatch certainty; the orchestration crate need not
know a particular vendor. Record model/provider revision, request-policy
fingerprint, input artifact references and accepted output. Do not record
credentials or require private model reasoning.

A recorded decision is replayed from its result, not sampled again. Repeating
a model call because its output failed schema validation is a new, budgeted
operation, not deterministic replay. Replacing the model or prompt during a
run changes the decision provenance and cannot silently reuse incompatible
cached results.

## Context and memory contracts

**AI-04.** Large evidence stays in a bounded artifact store, referenced by
identity, exact revision, byte count, media/schema type and provenance. The
model receives a compact task view and requests bounded ranges as needed.
A tool result names omitted/truncated ranges and coverage, never presents a
preview as the complete evidence. Token estimates are estimates; byte limits
are enforced independently before allocation.

A resume view includes intent/version, current subject, completed steps,
verified results, blocked needs, uncertain effects, active resource owners and
next eligible work. It is derived from the run record, not solely from a
lossy LLM summary. Pinned user corrections and unresolved safety facts survive
compaction. Tenant and run boundaries apply to artifacts, prefix caches and
retrieval; a content digest is not permission to retrieve an artifact.

Long-term semantic memory policy remains with the surrounding context runtime.
The bot records generic provenance and continuity; it must not become a second
application memory database.

## Parallelism with purpose

**AI-05.** Default to the smallest composition that satisfies the task. More
workers are not a quality metric. Independent bounded read/analysis work may
speculate; consequential effects use a single authorized publication path.
Workers receive explicit subjects and immutable artifact references instead of
full copies of the conversation. Their outputs include coverage and evidence,
not just conclusions.

When a subject changes, invalidate only dependent results using recorded
input/digest edges; do not repeat unrelated completed work. A worker analysing
head A cannot have its answer silently relabelled as head B. Shared mutable
browser focus, filesystem paths and PR publication each need a conflict domain.
A deterministic reducer can make output ordering stable; it cannot guarantee
that a changing external system or sampled model produces the same observations.

## No-progress and intervention

**AI-06.** Track a normalized failure signature over operation, input digest,
subject revision, error class and relevant environment epoch. Retrying the same
unchanged state without new evidence spends a finite repair budget and then
returns a typed intervention, not another identical prompt. New external facts
can justify new work, but do not reset the root budget by themselves.

A blocked response carries the complete known NeedSet, already preserved work,
and the smallest permitted repair. Distinguish missing authority, unavailable
adapter, conflicting evidence, stale subject, malformed output and depleted
budget. The person answers the real question once; orchestration must not ask
for repeated approvals already covered by a valid standing contract. Material
changes to effect destination or irreversible consequences require a new
policy decision, not reuse of an unrelated prior consent.

**AI-07.** Cancellation, timeout and disconnect are not interchangeable. Scoped
work cancels under its declared lifetime; explicit durable submissions can
outlive the client. Resuming attaches to existing work rather than inventing a
new run. Repeated user delivery uses a request identity with payload equality;
the same key with different input is a conflict. Identical input without the
same request identity may be a genuinely new user request and must not be
silently deduplicated.

## Verification and evaluation

**AI-08.** Verification must read application state independently of the actor's
success assertion. Separate schema correctness, subject coverage, task quality
and effect occurrence. A PR review can be correctly published and still have
poor findings; a good draft can exist when publication failed. Report both.
Prompt injection resistance is a tested property of a concrete adapter and
policy combination, not a guarantee delivered by typed JSON.

**AI-09.** Evaluate with fixed model versions, matched task/evidence budgets,
held-out tasks and recorded prompts/tool definitions. Compare the old API and
new task facade on compile success, substantive correctness, amount of manual
orchestration, tool/API mistakes, recovery accuracy, tokens and interventions.
Measure sample variance and disclose tool/model failures. Compare same-model
runs before attributing improvement to architecture. No claim of frontier
performance follows from a nicer API, more agents or a microbenchmark.

The required workload set includes PR review, bounded multi-API fan-out,
readiness-before-request, watch overflow, missing authority, lost response,
subject change, isolated untrusted content, partial worker failure and context
reset. See [acceptance](orchestration-acceptance.spec.md).
