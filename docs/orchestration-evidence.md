# Declarative orchestration: evidence and review limits

Reviewed 2026-09-21. Source baseline:
`51897f8c0cda627d3b3abcee28bda6ebb690f7a1` of
`srinji-kaggss/logicalworks-crates`. Tracking [#87](https://github.com/srinji-kaggss/logicalworks-crates/issues/87).

## Assessment

Foundational Rust quality: **6.5/10, qualitative review judgment**, not a
benchmark, weighted statistical score or a penalty for missing CUA features.
The typed builder, explicit outcome categories and payload ownership are good
foundations. Remaining attempt/lifecycle mistakes and leaky public execution
boundaries prevent a high-assurance rating. The number is not a release gate;
the falsifiers in the acceptance specification are.

This is a source review and specification change. No Rust compiler is available
in this review environment; no new Rust test, HOL4 proof or real GitHub review
journey was executed. Existing exact-head
[CI 35647079835](https://github.com/srinji-kaggss/logicalworks-crates/actions/runs/35647079835)
reported success, and its test log was inspected in the preceding reassessment.
That is evidence for its existing tests, not for the proposed falsifiers.

## Repository evidence

All source links below are pinned rather than mutable main links.

| ID | Evidence | Finding / implication |
|---|---|---|
| E01 | [Typed builder](https://github.com/srinji-kaggss/logicalworks-crates/blob/51897f8c0cda627d3b3abcee28bda6ebb690f7a1/crates/lgwks-bot/src/ecs.rs#L2484-L2523) | Source/condition/action compatibility is enforced before public builder erasure. Preserve this repair. |
| E02 | [Bound payload and discarded action output](https://github.com/srinji-kaggss/logicalworks-crates/blob/51897f8c0cda627d3b3abcee28bda6ebb690f7a1/crates/lgwks-bot/src/ecs.rs#L2262-L2345) | The old mixed-input failure is repaired; the chain still reduces successful output to effect accounting, not a general typed workflow result. |
| E03 | [Settlement](https://github.com/srinji-kaggss/logicalworks-crates/blob/51897f8c0cda627d3b3abcee28bda6ebb690f7a1/crates/lgwks-bot/src/ecs.rs#L1052-L1110) | Duplicate record checked only when not settleable; revision is not a physical-attempt identity. |
| E04 | [Historical fingerprint update](https://github.com/srinji-kaggss/logicalworks-crates/blob/51897f8c0cda627d3b3abcee28bda6ebb690f7a1/crates/lgwks-bot/src/ecs.rs#L2094-L2200), [handover before schedule](https://github.com/srinji-kaggss/logicalworks-crates/blob/51897f8c0cda627d3b3abcee28bda6ebb690f7a1/crates/lgwks-bot/src/ecs.rs#L1921-L1942), [atomic fold refusal](https://github.com/srinji-kaggss/logicalworks-crates/blob/51897f8c0cda627d3b3abcee28bda6ebb690f7a1/crates/lgwks-bot/src/ecs.rs#L1350-L1415) | Historical cache defect: a successful fingerprint survived a different source's failure although its value did not commit. The detached-cache path is now retired. |
| E05 | [Process handover and disarming](https://github.com/srinji-kaggss/logicalworks-crates/blob/51897f8c0cda627d3b3abcee28bda6ebb690f7a1/crates/lgwks-bot/src/rt/supervise.rs#L939-L1008), [group guard](https://github.com/srinji-kaggss/logicalworks-crates/blob/51897f8c0cda627d3b3abcee28bda6ebb690f7a1/crates/lgwks-bot/src/rt/supervise.rs#L1230-L1320) | Guard is constructed in the management future; direct-child exit disarms descendant cleanup; signalling errors are discarded. |
| E06 | [Public process surface](https://github.com/srinji-kaggss/logicalworks-crates/blob/51897f8c0cda627d3b3abcee28bda6ebb690f7a1/crates/lgwks-bot/src/rt/process.rs), [method bans](https://github.com/srinji-kaggss/logicalworks-crates/blob/51897f8c0cda627d3b3abcee28bda6ebb690f7a1/clippy.toml) | Engine Command remains executable; banning spawn alone is not a description-only API. |
| E07 | [Task fan-out](https://github.com/srinji-kaggss/logicalworks-crates/blob/51897f8c0cda627d3b3abcee28bda6ebb690f7a1/crates/lgwks-bot/src/rt/task.rs#L43-L110) | Parallel Send/static bound is not a local non-Send composition facade; aggregate result memory is separately unbounded by the active-task limit. |
| E08 | [Process consumer polling](https://github.com/srinji-kaggss/logicalworks-crates/blob/51897f8c0cda627d3b3abcee28bda6ebb690f7a1/crates/lgwks-bot/tests/rt_process.rs#L39-L73), [domain stub](https://github.com/srinji-kaggss/logicalworks-crates/blob/51897f8c0cda627d3b3abcee28bda6ebb690f7a1/crates/lgwks-bot/src/domain/sys.rs#L64-L90) | Real subprocess support is not yet a result-bearing admitted bot operation. |
| E09 | [Materializer boundary](https://github.com/srinji-kaggss/logicalworks-crates/blob/51897f8c0cda627d3b3abcee28bda6ebb690f7a1/crates/lgwks-bot/src/lib.rs#L153-L178), [older SDK proposal](https://github.com/srinji-kaggss/logicalworks-crates/blob/51897f8c0cda627d3b3abcee28bda6ebb690f7a1/docs/async-sdk-shape.md) | Need one real registry and a task-first facade, not another parallel plan interpreter. |
| E10 | [Benchmark](https://github.com/srinji-kaggss/logicalworks-crates/blob/51897f8c0cda627d3b3abcee28bda6ebb690f7a1/bench/README.md), [proof boundary](https://github.com/srinji-kaggss/logicalworks-crates/blob/51897f8c0cda627d3b3abcee28bda6ebb690f7a1/proofs/README.md) | One-machine loop comparison is not frontier orchestration evidence; model theorems do not prove the Rust implementation. |

### Historical cache counterexample (fixed by retiring the detached cache)

A returns value 1 and fingerprint 1; B fails its poll. The former
`poll_sources` cache remembered A's 1 and cleared only B's failed key.
`observe_fold` committed no values, but the next tick skipped A because its key
was still 1. The retired cache can therefore not hide A's uncommitted value.
Repeat with an already established baseline to test loss of a subsequent
change. Atomicity must include the optimization cache, not just the value.

### Existing repair work

[#86](https://github.com/srinji-kaggss/logicalworks-crates/pull/86) was open,
not merged, at inspection; its then-head was
`1747cbb4a68c0f6ff8281bdd083e69f5f45f5c0f`. Its description states the EffectKey
identity layer is not yet wired to the ledger. This document relies on that
explicit scope statement, not a claim that every change on that branch was
reviewed. Reuse its vocabulary after checking the actual integration revision.
Do not close attempt-bound acceptance because an identity struct exists.

## Primary research and design transfer

Accessed 2026-09-21. These references constrain this proposal; none proves this
repository's behaviour. No third-party dependency is admitted by citing it.

| Ref | Primary source | Narrow transfer |
|---|---|---|
| R01 | [Trio core and Nursery.start](https://trio.readthedocs.io/en/stable/reference-core.html#trio.Nursery.start) | Structured ownership and readiness handoff remove caller spawn/sleep/hope. Rust requires its own lifetime-safe implementation. |
| R02 | [The scoped task trilemma](https://without.boats/blog/the-scoped-task-trilemma/) | Do not promise arbitrary borrowed asynchronous children with all parallel/lifetime properties. Expose honest local versus owned modes. |
| R03 | [Rust mem::forget](https://doc.rust-lang.org/std/mem/fn.forget.html) | Safe Rust can omit destruction; correctness/memory-safety arguments cannot assume Drop always runs. |
| R04 | [Tokio JoinSet](https://docs.rs/tokio/latest/tokio/task/struct.JoinSet.html) | Aborting owned tasks and waiting for their completion are different operations; translate engine details into task-domain outcomes. |
| R05 | [Tokio Command](https://docs.rs/tokio/latest/tokio/process/struct.Command.html) | Spawn/status/output are executable surfaces. A supported description-only facade requires encapsulation. |
| R06 | [Rust API Guidelines: type safety](https://rust-lang.github.io/api-guidelines/type-safety.html) | Use meaningful types and valid construction to reduce representable misuse; do not mistake typed data for external-system evidence. |
| R07 | [Temporal activities](https://docs.temporal.io/activities) | Explicit effect boundaries and idempotency are needed for useful retry/recovery; a whole opaque script has coarse recovery. |
| R08 | [Temporal workflow definitions](https://docs.temporal.io/workflow-definition), [event history](https://docs.temporal.io/encyclopedia/event-history) | Recorded operations/results and compatible definitions support replay; they do not serialize arbitrary Rust frames. |
| R09 | [Anthropic: effective agents](https://www.anthropic.com/engineering/building-effective-agents) | Use simple workflows when sufficient and measure added agent complexity rather than assume it helps. |
| R10 | [Anthropic: tool design](https://www.anthropic.com/engineering/writing-tools-for-agents) | Small clear tools, meaningful results and realistic evaluation are shared human/AI usability requirements. |
| R11 | [Anthropic: context engineering](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents) | Keep compact task context and retrieve evidence just in time; preserve provenance outside lossy summaries. |
| R12 | [GitHub review API](https://docs.github.com/en/rest/pulls/reviews#create-a-review-for-a-pull-request) | Explicit commit binding and readback are available; a prior head read is not an atomic freshness guard or a general idempotency protocol. |

## Open proof obligations

The ergonomic Rust callable shape, runtime/host ownership handover, total
memory bounds, actual store fencing, platform cleanup and model-task
improvement require implementations and measured evidence. The specifications
make those obligations explicit; they do not discharge them. Unsupported
backends must say unsupported, and all proposed tests start UNRUN.
