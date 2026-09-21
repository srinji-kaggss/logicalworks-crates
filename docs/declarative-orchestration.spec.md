# Task-first declarative orchestration

Status: **proposed specification; no API implementation claimed**.
Baseline: `51897f8c0cda627d3b3abcee28bda6ebb690f7a1`.
Tracker: [#87](https://github.com/srinji-kaggss/logicalworks-crates/issues/87).
Normative words MUST/SHOULD apply to the proposed surface, not to the current
release. Code-shaped blocks are design examples, deliberately labelled `text`.

## Objective and falsifier

**DX-01.** A developer MUST be able to supply a typed task plus its domain
script and receive a useful result without implementing orchestration. The
canonical task is PR review, but the substrate MUST also support ordinary
non-AI work. No model client, agent persona or remote service is required for
an in-process task.

The falsifier is not a long example. It is any example whose correctness needs
consumer-written lifecycle machinery hidden in a helper: reaping, readiness
polling, retry ownership, duplicate-effect suppression, byte accounting or
continuation persistence. Those obligations belong below the task script.

## Semantic vocabulary

**DX-02.** The common path has three concepts: **Task** (typed work definition),
**Host** (installed execution/policy owner), **Report** (result and evidence).
Advanced operations expose a run identity and typed intervention needs only
when those distinctions matter. `task`, `all`, `map`, `watch`, readiness and
verification are composition/policy operations, not new domain verbs.

- A task definition binds a stable logical name, input/output schema, body or
  script identity and a versioned execution contract. A name is not a secret,
  a grant, a replay key or evidence of identity by itself.
- A native body is a Rust async callable; no trait implementation per ordinary
  workflow is required. Adapter authors implement the four domain traits.
- A script definition binds executable/artifact digest, argument schema,
  result schema and supported checkpoint semantics. A mutable filesystem path
  alone is not a durable script identity. Shell interpretation is opt-in;
  untrusted input travels as typed data/argv, not interpolated shell source.
- A report separates workflow disposition, useful typed output, external-effect
  knowledge, cleanup status and verification evidence. A successful function
  return does not by itself prove a remote state change.

## Host once, task often

**DX-03.** Host configuration MUST be explicit, reusable and inspectable. It
installs authorized credentials, policy, available adapters, limits and storage
once. An already running application MUST not create a nested runtime per task.
A synchronous entry point may own a runtime once; it must refuse unsupported
nesting rather than block its reactor.

The default local profile MUST have finite resource ceilings, no ambient
network/process authority and no durability claim. Installing an application
profile is a deliberate integration action, not something task content or an
LLM may cause. Resolve unspecified ceilings from the host profile; expose them
in the admission report. CPU parallelism is not an adequate default bound for
all I/O services, pipes, model requests and idle background resources.

Illustrative consumer:

```text
let review = task("review-pr", review_script);
let report = host.run(review, request).await?;
```

The task registration header and one-time host setup must accompany the full
example. Their absence from the two-line excerpt does not make them free.

## Straight-line work with real result flow

**DX-04.** The facade MUST return an operation's typed output, not only the
number of effects fired. Operation inputs follow from the preceding output;
no unconstrained caller-selected type variable may cross a chain boundary.
Keep `EcsObserveBuilder<S>`'s existing type relationship.

Business code may await `work.query(call)` and `work.execute(call)` using a
typed descriptor that contains the adapter plus its input. The host issues the
appropriate proof; the author does not manually mint one at each call. Calls
retain typed identity through dispatch and verification. A result is not
silently boxed and discarded by the task path.

**DX-05.** Local async composition MUST support non-Send futures and borrowed
inputs without author-written `Pin<Box<...>>`, task-plumbing `Arc`, `'static`
coercions or engine errors. Parallel work explicitly consumes owned data;
remote/durable work additionally uses registered serializable schema IDs.
These are different supported modes, not a promise that arbitrary borrowing
can cross threads or process restarts. Prototype the generic callable API on
the workspace MSRV before freezing names; do not erase lifetime failures with
unsafe transmutation or an unbounded heap allocation.

## Composition without a second language to learn

**DX-06.** Support these semantic operations on the same execution core:

| Operation | Required meaning |
|---|---|
| Sequential await | A successor consumes its actual predecessor result |
| `all` / bounded `map` | Independent work progresses within inherited limits; deterministic result association |
| `collect_outcomes` | Explicit opt-out from fail-fast; errors remain attributed, not dropped |
| Optional branch | Intentional non-execution is typed and distinct from failed prerequisites |
| Watch / bounded stream | Event subscription with cursors, backpressure and explicit overflow policy |
| Start resource | Typed readiness result before dependent work can use the resource |
| Verify | Read actual postconditions and evaluate them; not another unrestricted effect verb |

Fail-fast applies within a declared task group. Unrelated runs do not fail
because a sibling observer is unavailable. Ordering of independent completions
need not match wall-clock arrival; output association and recorded reducer
order must be reproducible from the same observed event trace.

A success dependency MUST distinguish `Succeeded`, intentional `Skipped`,
`Failed` and `Unknown`. Skipping a required reserve operation cannot authorize
its send successor. Existing `.on` behaviour MUST not silently change under a
minor documentation edit; introduce explicit versioned semantics for the new
composition path and migration tests for old chains.

**DX-07.** A watch MUST distinguish replaceable latest-state observations from
non-coalescible events. Latest-state mode may supersede intermediate values
only with declared semantics and counters. Event mode keeps stable IDs,
acknowledgements and a bounded pending policy; it must not turn two identical
commands into one because their payloads compare equal. Overflow is a reported
state, never silent loss. One-shot tasks must not require a fake source that
changes once merely to start their script.

## Admission and targeted repair

**DX-08.** Report all presently knowable unmet needs as one typed `NeedSet`,
including missing adapters, invalid inputs, absent authority and unavailable
execution modes. Do not claim to discover requirements of unreachable future
branches or opaque scripts; collect all declared requirements and all needs
at the current admission boundary. Attribute each need to the operation that
requires it and derive a machine-readable repair proposal.

A repair proposal MUST NOT be a grant. The trusted host may accept, narrow or
refuse it. No grant-all fallback. Dynamic needs hold only dependent work and
preserve completed results. A retry of the admission request must not rerun
an already successful external effect. Workflows awaiting a person release
active execution slots while retaining bounded continuation state.

The existing `GrantSet` remains a snapshot: it has no revoke operation, so a
running-authority amendment is a new, recorded host admission epoch rather than
a retroactive change to an `Auth`. The dispatch boundary must revalidate under
the applicable host policy before new effects. This requires an actual authority adapter to enforce externally;
a comment or cloned capability list is not live authority.

## Definition, compilation and registry

**DX-09.** There MUST be one registry from stable operation ID + schema version
to typed constructor, declared requirements and result codec. Native tasks,
`BotSpec` and `FlowSpec` use that same registry and execution/recording path.
Do not add a mandatory second workflow manifest, parallel untyped interpreter
or a proc-macro that conceals authority. Existing `FlowSpec::Route` does not
already implement an external dispatch; any call node extension needs a wire
version and explicit lowering.

The runtime's internal program graph is an implementation representation,
not another user-authored configuration file. Static definitions can compile
fully; a native async body discovers operations as it executes. Do not claim
to infer arbitrary Rust control flow or data dependencies automatically.
Explicit composition creates the dependency edges. Dynamic expansion is a
validated, bounded, recorded append using stable step keys, never replacement
of already dispatched work.

Unknown operations, unsupported schema versions, incompatible types and
missing adapters fail before their effect. A valid JSON document is not
therefore executable. Materialization retains host-supplied authority; neither
wire data nor the model supplies a capability proof.

## Encapsulation and ownership

**DX-10.** Separate construction from execution in public types. A
`ProcessSpec`/equivalent owns its data with private fields and validated
constructors; it must expose no `spawn`, `status`, `output`, mutable engine
reference or `DerefMut` escape. The host is the supported executor. This
prevents accidental bypass inside the SDK; arbitrary in-process Rust still
requires a sandbox when confinement is needed.

`lgwks_bot` owns generic orchestration and local adapters. Intelligence/planning,
context policy, durable storage, authorization, sandboxing and workspaces
remain narrow integration boundaries with Forge, Logical-DB, Secure-Authority,
Nova, Moo and Sentinel where those products are installed. None is a mandatory
network service for a small local task. No second implementation of these
products is authorized by this specification.

## Visibility and compatibility

**DX-11.** A task must explain, without a debugger: what is executing, what
blocked progress, what will happen next, what completed, what remains uncertain,
and which specific input/authority/evidence can unblock it. Progress is an
optional bounded subscription from the same state reducer, not a second task
ledger. The common synchronous result path needs no subscription loop.

**DX-12.** Preserve existing users while making the new front door canonical.
Publish explicit API/version changes. Mark every design-only example as such;
replace it with a compiled, exercised example only when the implementation
exists. Keep expert primitives where genuinely necessary, but do not make them
mandatory in task tutorials. Scope names, traits and facade ownership are not
performance proofs; measure matching semantics before replacing the engine.

Acceptance is defined in [orchestration-acceptance.spec.md](orchestration-acceptance.spec.md).
Research and current defects: [orchestration-evidence.md](orchestration-evidence.md).
