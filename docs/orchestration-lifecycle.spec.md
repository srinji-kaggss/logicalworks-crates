# Orchestration lifecycle and Rust invariants

Status: **proposed; not an implementation receipt**. Baseline and tracking are
in [the task-first contract](declarative-orchestration.spec.md).
The public API is small because the host owns these obligations once.

## One owner and independent facts

**LC-01.** One host reducer owns each run's transitions. Commands propose work;
observations and persisted acknowledgements advance state. Parallel workers
return typed facts, not arbitrary mutations of shared workflow state. For a
single-process host this can be one owner task. Distributed takeover requires
an actual store/security fencing mechanism, not a local mutex or TTL alone.

The report MUST preserve these orthogonal facts:

```text
Disposition = Completed | Blocked | Failed | Cancelled
EffectKnowledge = per-effect NotStarted | NotApplied | Applied | Unknown
Cleanup = Complete | Pending | Failed
Verification = Satisfied(evidence) | Unsatisfied(evidence) | Unmeasured(reason)
```

These are semantic categories, not mandated public enum spellings. A failed
workflow can still have applied or unknown effects. A cancelled task can have
pending cleanup. The public completion conversion must refuse to return a
verified success when any required effect or postcondition is unresolved.
Infrastructure errors after dispatch must carry a run/report reference and the
last known state, not erase occurrence uncertainty behind `Err`.

## Admission before work creation

**LC-02.** Reserve the relevant resource budget before starting an external
operation or materializing an expensive closure/payload. Bound active workers,
queued work, retained results, process output, event buffers and aggregate
artifact bytes independently. A limit on active tasks is not a memory bound
on the returned collection. A collecting operation must require a total bound;
large/unbounded sources use backpressured streams or artifact references.

Nested composition must not deadlock at concurrency one: a parent awaiting
children cannot occupy the execution permit all children need. Suspended
parents retain separately charged memory, not active compute permits. Resource
acquisition order must be fixed or atomic across multiple classes. Pipe drain,
cleanup and reconciliation work need reserved capacity so the main operation
cannot consume the resources necessary to finish itself.

**LC-03.** Default scheduling is local and bounded. Independent ready work
progresses without wave-wide head-of-line blocking. Conflicting writes acquire
explicit resource domains (for example one browser's focus/session epoch), not
one global lock over all runs. CPU work, model calls and I/O have separately
resolved limits. Fairness across runs must be tested at saturation; no inferred
unlimited fan-out from a declared DAG. An empty ready queue does not mean a
run finished while producers, unresolved events or held effects remain.

## Observation commits

**LC-04.** Commit observation payload, its comparison baseline, cursor/revision
and fingerprint atomically with respect to admission of work. A failed atomic
observation group must invalidate or retain as uncommitted every successful
member's new fingerprint; it may not remember that value as handled. The
current two-source counterexample is recorded in the evidence document.

A fingerprint is an invalidation contract, not an ordinary cached remote
ETag: if no observer updates the key, blindly comparing last time's ETag would
stop polling forever. Only a currently valid change witness may suppress an
observation. Disconnection, watch overflow or expired freshness invalidates it.
Equal keys must imply equal relevant values under the declared contract; hash
collision assumptions must be explicit. Preserve the no-fingerprint path.

## Attempts and external effects

**LC-05.** Before dispatch, record the exact operation input and attempt identity.
Coordinate with [#86](https://github.com/srinji-kaggss/logicalworks-crates/pull/86)
rather than inventing another `EffectKey`. Its identity work is not closure
until the actual dispatcher, ledger, settlement and recovery paths consume it.

The required binding is run + action + attempt + definition revision + action
payload digest + environment identity + environment epoch. Logical invocation
identity, physical dispatch attempt and retry-budget counters are different
concepts. A transport retransmission preserves the provider's logical
idempotency key and exact semantic payload; a new attempt gets a new attempt
identity. Neither is inferred from vector position or debug `type_name`.
Checked exhaustion refuses instead of wrapping or saturating an identity.

**LC-06.** Settlement compares the full subject before changing state. Delivery
of evidence for attempt A is idempotent for A even after attempt B starts;
it cannot authorize B's retry. Contradictory evidence is reported. Evidence of
non-occurrence is not renewed policy authority or proof of transient failure.
A permanent refusal needs an explicit authorized repair/new decision, not an
unbounded cycle of `NotApplied` budget resets. Validate evidence provenance,
not merely its structural fields.

A durable store operation must atomically bind expected state/owner epoch,
append the next event and reserve dispatch identity. An append acknowledgement
states its persistence level. At startup, attempted-without-outcome means
Unknown. Persisting intent cannot make an arbitrary non-idempotent endpoint
exactly-once; reconciliation or a supported idempotency protocol is still
required. A crash before recording a response may require holding work even
when the effect actually succeeded.

**LC-07.** Retry policy is a total typed decision over dispatch certainty,
transience, idempotency support, remaining budget and retry timing. Do not parse
human cause strings. Honor typed rate-limiting information. Choose one retry
owner; count adapter/network retries against the effective aggregate budget.
Do not retry an entire workflow to reach one unfinished successor. Retries,
model-output repair and replanning consume separately reported budgets.

## Cancellation, readiness and cleanup

**LC-08.** Cancellation stops new admission and propagates through owned work.
`run` and explicitly persistent `submit` have distinct lifetimes. Dropping a
scoped waiter requests cancellation; the installed host continues to own
cleanup until drain or an explicitly reported cleanup failure. Merely dropping
a result view may not silently convert scoped work into persistent work.

Drop cannot await, and Rust does not guarantee destructors run. Never make
memory safety depend on final polling or destructor execution. Cooperative
cancellation cannot preempt a non-yielding callback. Isolate work requiring
forcible limits in an owned process/sandbox. A shutdown timeout may bound how
long the caller waits, but cannot truthfully certify that uncooperative work
stopped. Expose pending cleanup and deny reuse of still-owned resources.

**LC-09.** Starting a resource returns typed readiness, not a PID and a sleep.
Startup failure belongs to the initiating call; later failure is attributed to
the same resource and propagates to its dependants. A readiness token carries
resource identity and generation and becomes unusable after close/replacement.
A bound listener or completed browser handshake, not elapsed time, establishes
readiness. Duplicate/stale readiness messages are rejected.

**LC-10.** Create the child-process owner immediately after successful OS spawn,
before scheduling its management future. It owns all descendants that the
backend claims to control. Normal leader exit is not proof the containment is
empty. Group signalling alone is not containment against `setsid`/escape;
stronger requirements need an appropriate sandbox/job/container backend or an
explicit unsupported-mode refusal.

Cancellation must stop descendants, drain/close streams, reap what can be
reaped and report cleanup evidence. Signal errors cannot be discarded on a
path that declares cleanup complete. No stale PID/group reuse signalling;
platform-specific identity/containment guarantees must be described and tested.
Do not silently replace a Unix process-tree guarantee with direct-child-only
behaviour on another OS. A backend may report a weaker explicit mode, but not
under the same stronger assurance label.

**LC-11.** Output is part of orchestration. Drain stdout and stderr concurrently
with bounded buffers; bound stdin, framing and total retained output. `piped()`
with no managed reader must be impossible on the supported facade. Choose an
explicit overflow policy: backpressure, fail, or truncate with a marked
incomplete artifact. Never parse truncated JSON as a valid script result.
Separate execution status, output availability and postcondition verification.

## Durability without fiction

**LC-12.** Local execution may retain ordinary borrowed Rust state. Durable
execution MUST use either a restartable registered body whose external calls
are recorded, or a versioned compiled program using those same operation
boundaries. Neither representation serializes an arbitrary Future or stack.
Replay validates step identity, operation ID, input digest and definition
revision before reusing a recorded output. Divergence is a typed stop, not
permission to start new external work.

An opaque script is one coarse effect unless it declares a checkpoint protocol.
A process restart does not resume an arbitrary instruction pointer. Don't
advertise durable step recovery by wrapping a shell script that has already
made unrecorded network mutations. Put those effects behind the host adapter,
or mark the coarse effect Unknown and use its documented reconciler.

**LC-13.** Failover fences dispatch owners, preserves logical idempotency and
blocks uncertain in-flight effects until reconciled. Leases require a
store/authority-verified owner epoch; TTL expiry alone cannot prove an old
worker stopped. Do not garbage-collect receipts, deduplication keys or pinned
artifacts before the declared replay/reconciliation horizon. Compaction records
supersession and preserves unsettled effects and still-referenced evidence.

## Rust implementation requirements

**LC-14.** Keep native async at static call sites; boxing belongs at an actual
heterogeneous boundary. Prefer typed IDs, validated private constructors,
associated output types and exhaustive internal state transitions over flags
that admit inconsistent combinations. `#[non_exhaustive]` public enums are
versioning tools, not substitutes for a specified wire protocol. Wire unknowns
must produce typed compatibility failures.

Do not claim `Evaluate` is pure because it takes `&self`: a closure can access
interior mutable or external state. Replay-safe evaluation must be restricted
to a validated expression representation or an explicitly trusted replay
contract. No async suspension while holding a mutable world/resource borrow
needed by sibling work. Local and parallel adapters must both have real tests.

**LC-15.** Cache validated metadata, normalize capability sets once where
possible, reuse bounded scratch, and avoid per-quiet-tick allocation. Preserve
cache/value atomicity while optimizing. A wake-aware local join should avoid
repolling unrelated pending children, but make the change only with recorded
fairness/cancellation tests. Prefer measurement over claims about zero-cost
abstraction, ECS or hand-written schedulers.

**LC-16.** A small explicit state module, dispatch module, resource-owner module
and report module may remain inside the existing crate; no new top-level crate
or dependency is required by this design. Split along invariants/ownership, not
arbitrary line-count targets. Do not lower existing lint, feature or test gates
to make the facade look simpler.
