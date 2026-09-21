# The async surface as an SDK

Status: **design contract; task-first facade not implemented**. Reviewed against
`51897f8c0cda627d3b3abcee28bda6ebb690f7a1`, 2026-09-21.
Tracking: [#87](https://github.com/srinji-kaggss/logicalworks-crates/issues/87).
This revision replaces the earlier scope-first proposal; it does not declare
its proposed symbols available in any `lgwks_bot` version or on main.

## The contract

A task author supplies **what to do and the substantive script**, not the
machinery that keeps it running. Reviewing a PR must not require another
application-specific queue, supervisor, tick loop, retry controller, child
process manager, capability-repair loop or reporting subsystem.

Illustrative target, not a compilable example of a shipped API:

```text
let review = task("review-pr", review_script);
let report = host.run(review, pr).await?;
```

A host is configured once with explicit policy and adapters. That configuration
is real work and must be included in usability measurements. A tiny call site
that hides hundreds of bespoke orchestration lines in `review_script` fails
this contract just as surely as a verbose call site does.

The task-first specification consists of:

| Document | Authority |
|---|---|
| [Declarative orchestration](declarative-orchestration.spec.md) | Author-facing semantics, composition, ownership boundaries and migration |
| [Lifecycle](orchestration-lifecycle.spec.md) | Scheduling, attempts, cancellation, blocked work, durability and resources |
| [AI orchestration](ai-orchestration.spec.md) | AI as API consumer and workload, context/evidence, bounded proposals and evaluation |
| [PR review journey](pr-review-orchestration.spec.md) | The required end-to-end task plus script and its external-system boundaries |
| [Acceptance](orchestration-acceptance.spec.md) | Falsifiers, developer-experience criteria and evidence required for release |
| [Evidence](orchestration-evidence.md) | Pinned code findings and primary research; limits of this review |

The specifications prescribe future behaviour. Actual source and exact-version
release notes remain authoritative about what runs today. Where this contract
conflicts with an older design sketch, it governs the proposed task-first
surface, not a silent change to existing runtime behaviour.

## Three audiences, one engine

The **task author** uses typed inputs/results and ordinary async/await. No
`Pin`, engine `JoinError`, worker count, task-plumbing `Arc`, semaphore,
`JoinSet`, detach, readiness sleep or PID cleanup belongs in the common task.
That is an ergonomic acceptance criterion, not a prohibition on domain logic
that genuinely uses an `Arc` or an advanced integrator choosing a lower level.

The **host integrator** installs policy, storage, authority, sandbox and domain
adapters, and chooses the host lifetime once. There is no implicit global
runtime or automatic authority escalation. An existing async host drives work;
a synchronous application explicitly owns runtime entry once.

The **adapter author** implements the real external contracts: typed I/O,
authority requirements, dispatch certainty, readiness, bounded observations,
reconciliation and cleanup. The SDK cannot derive those facts from an opaque
Rust closure. Domain adapters continue to implement Observe, Evaluate, Execute
and Query; composition and supervision are not a fifth domain verb.

## Existing mechanisms to retain

The current typed observation builder is valuable: it relates source output,
condition input and action input before erasure. Keep it. Keep the retained
payload binding and the distinction between refused, not-delivered and
unsettled effects. Keep bounded admission and explicit snapshot authority.
See the [source evidence](orchestration-evidence.md#repository-evidence).

`Runtime`, `Supervisor`, the ECS schedule and the local future driver are
implementation/integration facilities beneath the task-first facade, not a
checklist every application repeats. This proposal does not authorize replacing
Tokio, removing ECS, adding a parallel workflow engine, or adding dependencies
outside `AGENTS.md`'s ownership policy.

## Corrections to the earlier proposal

1. **Scope/spawn is not the front door.** Structured lifetime remains necessary,
   but consumers should name business work rather than manage child handles.
   A result reference is not ownership of the execution; dropping a view must
   not abandon its owner.
2. **No second mandatory `PlanSpec`.** Native task code, existing `BotSpec` and
   existing `FlowSpec` must converge on registered typed operations and one
   execution/recording path. They are not equivalent today: `FlowSpec`'s Route
   is a node transition, not a tool call. Evolve versioned node forms where
   needed instead of asserting that a materializer already exists.
3. **No magical scoped parallel borrowing.** Same-task local composition may
   borrow and remain non-Send. Parallel/remote work crosses an explicit owned
   boundary. Do not promise all lifetime/execution properties simultaneously;
   see the Rust research in the evidence document.
4. **No destructor-as-join claim.** Rust allows safe values to be forgotten and
   Drop cannot await. The host owns cleanup, reports unresolved cleanup and
   provides an explicit drain. Memory safety must not depend on a caller
   polling a future to completion.
5. **No absolute no-spawn claim about `lgwks_std::task`.** It has a
   `spawn_blocking` facility. Scheduling policy must cover actual entry paths,
   not names the guide wishes were absent.
6. **No reopening owner decisions by implication.** The first-party scheduler
   direction and the accepted existing time layers remain recorded decisions.
   Exact simulated replay is a separate, unproved capability; wall-clock APIs
   alone do not establish it. Any additional clock seam requires its own
   implementation and acceptance, not a claim that it has already landed.

## Entry modes and guarantees

`run` is a scoped invocation under a live host. A deliberately persistent
submission is a separately named operation on a configured durable host; it is
not the accidental consequence of dropping a handle. Local borrowed tasks
cannot be submitted as remote/durable work.

An arbitrary async function may run locally. It does not thereby become
serializable or restartable. Durable tasks use versioned registered operation
boundaries and recorded results; opaque scripts are a single effect unless
they opt into an explicit checkpoint protocol. Model calls are external,
potentially costly operations, not pure expressions recomputed during replay.

## Delivery order

Repair the concrete lifecycle/cache defects first or alongside the facade;
then land one typed local task returning a real result, bounded composition,
owned process/readiness operations, registry materialization and the verified
PR-review journey. Durable resumption and AI proposal helpers build on those
same boundaries. A backend without a real consumer/test is not closure.

The [acceptance specification](orchestration-acceptance.spec.md) is the release
contract. Merging these documents alone does not close #87.
