# Task-first declarative orchestration

Status: **implemented in part.** `lgwks_bot::task` (`task`, `Host`, `Report`,
behind the `script` feature) is the front door this page specifies; the page
is the target that surface is measured against, and a requirement the surface
does not yet meet stays normative here. Baseline:
`51897f8c0cda627d3b3abcee28bda6ebb690f7a1`. The first tracker, #87, is closed.

**Owner: the falsifier rows.** Each requirement is falsified by the rows of
[orchestration-acceptance.spec.md](orchestration-acceptance.spec.md) whose
`requires` names it in [`t-rows.toml`](acceptance/t-rows.toml), and CI records
a receipt for every row at every revision (`Acceptance receipts`):
DX-01/03/04 T01, DX-04/05 T02, DX-05 T03, DX-06 T10, DX-07 T09, DX-08 T23 and
T24, DX-09 T25, DX-10 T22, DX-11 T36. DX-02 is the vocabulary T01 drives, and
DX-12 is a release discipline the readme-quickstart lane and the CHANGELOG
hold. `scripts/check-spec-owners.py` refuses a DX requirement that no row
requires and no stated reason exempts, so a new one cannot land unowned.

Normative words MUST/SHOULD apply to the proposed surface, not to the current
release. Code-shaped blocks are design examples, deliberately labelled `text`.
Composition, admission, the registry, ownership and visibility (DX-06 to
DX-12) continue in [part two](declarative-orchestration-composition.spec.md).

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

Acceptance is defined in [orchestration-acceptance.spec.md](orchestration-acceptance.spec.md).
Research and current defects: [orchestration-evidence.md](orchestration-evidence.md).
