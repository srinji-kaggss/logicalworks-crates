# Product requirements

Status: **immutable.** This file is the product's normative spine. Everything
else in the repository is measured against it: issues, pull requests, the
release gate, and any readiness claim.

A requirement here is **superseded, never edited**. To change one, add an entry
to the Supersession log naming its id and why the previous statement was wrong
or incomplete, then run `python3 scripts/check-requirements.py --update`. The
gate refuses a changed normative sentence with no supersession entry, and the
entry is permanent. Silence is not consent to drift.

Every requirement carries four things, and a requirement without all four is
not written yet:

- **Invariants** that must hold for it to be true.
- **Callers** whose lives it changes.
- **Falsifier**: one discriminating observation that would refute it. Not a
  checklist. One test.
- **Status**: `met`, `partial`, `unmet`, or `future`, with the evidence class
  named as measured, proved, argued, or unmeasured.

---

## The vision

**The problem.** Code orchestration is brittle at every level. Doing a small
amount of real work requires too much downstream machinery: a queue, a
supervisor, a tick loop, a retry controller, a cancellation tree, a process
manager, a record of what happened. Each is a dependency. Each is re-implemented
wrong. The logic is rarely the failure; the plumbing around it is.

**The promise.** The bot contract does that work. A developer writes code and
their own logic, strings it together, and is done. The seven lifecycle
obligations that every brittle codebase re-derives sit below the task surface,
where one audited implementation owns them.

**Three shapes call the contract.**

1. A developer writes code and strings it together.
2. An AI needs to poll. It calls the bot. The bot owns observation and change
   detection; the agent does not keep a shadow previous-value cache.
3. An AI needs risky code checked. The bot checks the code **in process**, over
   its structure. It does not execute the code to find out what the code does.

**The execution model.** A worker substrate for ordinary code, at native
speed. In-process is the default tier and costs nothing to reach. A VM or a
runtime is invoked only when the developer declares `N` parallel things, or
when the work is computationally bounded onto another lane. Escalation is
declared at construction. Nothing escalates on a heuristic.

**The hard part is not the work.** The hard part is meeting every abstract,
weird, human and in-real-life complexity and slop: the target that is gone
rather than moved, the dialog that stole focus, the file that was half-written,
the clock that disagrees, the credential that expired mid-run, the operator who
cancelled, the intermediary that retried a non-idempotent effect. Those are
requirements. A demo that only handles well-formed inputs is not evidence.

---

## A. Authoring

### R1. A task author writes logic, never lifecycle.

> A developer MUST supply typed work and its logic and receive a result. The
> contract MUST own reaping, readiness polling, retry ownership,
> duplicate-effect suppression, cancellation, continuation persistence, and
> reporting.

**Invariants.** Those seven obligations live below the task surface. None is
reachable as a consumer-written helper that a correct example requires. A
successful function return is not by itself proof of an external state change;
the report separates disposition, output, external-effect knowledge, cleanup
and verification.

**Callers.** A developer authoring a task. An AI authoring a task.

**Falsifier.** Author the reference journey in
`docs/pr-review-orchestration.spec.md` as a task and count the lifecycle lines
below the task body. Any non-zero count refutes R1, including a count hidden in
a helper the example needs for correctness.

**Status.** `unmet`, argued. The contract is specified (`DX-01` in
`docs/declarative-orchestration.spec.md`) and the facade is not implemented
([#87](https://github.com/srinji-kaggss/logicalworks-crates/issues/87)).

### R2. Composition is stringing, not plumbing.

> Combining two or more tasks MUST require no new queue, supervisor, retry
> controller or reporting subsystem, and the composed unit MUST report as one.

**Invariants.** Composition introduces no second lifecycle owner. A partial
failure inside a composition is carried on the composed report, not
accumulated by the caller.

**Callers.** A developer composing tasks. An AI composing tasks.

**Falsifier.** Compose two tasks with `all`, kill one mid-flight. If the call
site needs a join handle, a retry counter, or a partial-failure accumulator to
produce a correct report, R2 is refuted.

**Status.** `unmet`, argued. `all`, `map` and `watch` are named as composition
operations in `DX-02`; none is implemented.

## B. Execution tiers

### R3. In-process is the default tier.

> Ordinary work MUST run in the calling process with no VM and no runtime
> constructed for it.

**Invariants.** An observe to evaluate to execute path that declares no
cardinality `N` and opts into no compute lane constructs no runtime, spawns no
isolate, and starts no thread.

**Callers.** Every task that is not escalated.

**Falsifier.** A single-source, single-condition, single-effect task that
constructs a runtime in order to run. Measure construction, not use.

**Status.** `partial`, measured. The synchronous `Bot::tick` path is what
`bench/` measures and it is in-process. Whether a default-feature build
constructs a runtime before the first escalated unit is **unmeasured**.

### R4. Escalation is declared, never inferred.

> A VM or runtime MUST be invoked only when the developer declares cardinality
> `N` for parallel work, or opts the work onto a blocking compute lane. A
> workload MUST NOT escalate on a heuristic.

**Invariants.** The bound is stated at construction. Fan-out is capped at the
declared `N`. The compute lane is opt-in and its use is reported. A workload
that needs more than it declared is refused rather than grown.

**Callers.** A developer declaring `N`. A developer declaring a compute lane.

**Falsifier.** A workload whose parallelism is discovered at run time rather
than declared, or a runtime constructed with no `N` and no compute opt-in.

**Status.** `partial`, argued. `join_all_bounded` and `Supervisor`'s in-flight
ceiling carry the shape; "stated at construction and refused beyond" is the
rule and is not uniformly enforced across the surface.

### R5. Escalation does not transfer ownership.

> Moving work to a runtime or a compute lane MUST NOT hand the caller a
> droppable handle. The escalated unit MUST return a result and a report, and
> the contract MUST still own the bounds, the cancellation and the record.

**Invariants.** No API on the task surface returns a `JoinHandle` or any handle
whose `Drop` starts work or abandons work. A `!Send` future is driven, never
spawned.

**Callers.** Every escalated unit.

**Falsifier.** A call site that stores a handle in order to keep work alive, or
to abandon it.

**Status.** `met` for the current surface, argued and partially measured. The
crate's thesis is that no verb starts work and hands back a handle to it;
`spawn_local` was withdrawn for exactly this reason, and
`tests/rt_async_tier.rs::a_non_send_future_is_driven_rather_than_spawned` is
the discriminating case.

## C. Call shapes

### R6. A developer strings code together and is done.

> The ordinary non-AI task MUST need no model client, no agent persona and no
> remote service.

**Invariants.** An in-process task's correctness depends on no network and no
model. Configuration of a host is explicit, reusable and inspectable, and its
cost counts against usability.

**Callers.** A developer.

**Falsifier.** An in-process task that imports a model client, or reaches a
network, in order to complete.

**Status.** `met` for the design, argued. `DX-01` states it. Usability of the
host configuration is **unmeasured**, and `docs/async-sdk-shape.md` warns that
a tiny call site hiding hundreds of orchestration lines fails this contract as
surely as a verbose one.

### R7. An AI that needs to poll calls the bot.

> Polling, change detection and the reaction that follows MUST be substrate.
> An agent MUST call them rather than reimplement them.

**Invariants.** Observation is a callable surface. Change detection is the
bot's. A source that holds still fires nothing, and the bot names which sources
moved. An event identity and a content value are distinguishable, so a
returning event retires and an unchanged source under a new run does not.

**Callers.** An AI agent that needs polling.

**Falsifier.** An agent path that maintains its own previous-value cache and
edge detector beside the bot.

**Status.** `partial`, measured. Change detection and `revisions()` exist and
are tested in `tests/ecs_tick.rs`; the identity split is tested in
`tests/durable_dispatch.rs`. A calling convention for an external agent is
**unmet**.

### R8. An AI that needs risky code checked calls the bot to check it in process.

> Untrusted code MUST be analysed in process over its structure. It MUST NOT be
> executed in order to be inspected.

**Invariants.** The check path never executes the subject code, in this process
or another. The verdict is typed, never free text. A check that cannot decide
returns undecidable rather than a guess.

**Callers.** An AI reviewing or gating code.

**Falsifier.** A hostile corpus: an infinite loop, a panic, an immediate
process exit, a filesystem delete. If any of them runs, in any process, to
produce the verdict, R8 is refuted.

**Status.** `future`. `lgwks_ast` is the substrate (33 grammars in CI) and the
check is not written. This is the shape's "eventually" and it is stated here so
it cannot quietly become out of scope.

## D. Correctness

### R9. Authority is proof-carrying at the invocation.

> No effect MUST fire without a grant minted by the one issuer, carried on the
> call and checked on the call.

**Invariants.** One mint site. A grant is a value, not a configuration lookup.
A wider grant fires a superset and never a different set. Ambiguity is refused
at build rather than misordered at run time.

**Callers.** Every effect.

**Falsifier.** `T4` in `proofs/`: an entry that fires without the grant that
permits it, for any grant, any environment, any entry list.

**Status.** `met` at the design level, **proved for the model and argued for
the implementation**. `proofs/README.md` states the refinement gap plainly: the
theorems are about the abstract model, and nothing proves `spec.rs` implements
it. `tests/authority.rs` covers the Rust surface.

### R10. The record decides what happened.

> After a loss, absent, contradicted, superseded and unreadable MUST be four
> distinct answers. An unknown outcome MUST be a barrier and MUST NOT be
> auto-replayed. A read failure MUST NOT be reported as absence.

**Invariants.** One fold derives attempt status; no second authority derives a
second answer. A journal error propagates as refusal. A settled outcome is not
overwritten by different evidence. An external handoff is refused before the
effect leaves when the record cannot outlive the process.

**Callers.** Every effect that leaves the process.

**Falsifier.**
`tests/durable_dispatch.rs::a_transient_journal_read_failure_is_not_reported_as_no_such_work`
and
`tests/durable_dispatch.rs::a_reconstructed_bot_retires_an_unchanged_state_rather_than_firing_again`.
Either regressing refutes R10.

**Status.** `partial`, measured against an in-memory journal, **unmeasured
against a real store under a real process kill**.
[#109](https://github.com/srinji-kaggss/logicalworks-crates/issues/109) holds
the eight external observations and none has been run.

### R11. Bounds are declared at construction.

> Every queue, channel, fan-out, loop and retry MUST have a bound stated where
> it is built. Work MUST NOT be fire-and-forget. A loop MUST NOT spin without a
> bound or a cancellation branch.

**Invariants.** Capacity and overflow strategy at construction. Every spawn
tracked in a supervisor or `JoinSet` with a `CancellationToken`. No `unsafe`.
No production `.unwrap()` or `.expect()`.

**Callers.** Every background unit.

**Falsifier.** A channel with no capacity, a spawn with no owner, a loop with
no exit. In this workspace that is also a hard compile failure: `forbid`, not
`deny`, so a single `#[allow]` anywhere is `E0453` rather than silence.

**Status.** `met` as a compile-time contract, measured. The lint table is in
the workspace manifest and the anti-pattern coverage ceiling is published in
`~/.agents/reference/rust-anti-patterns.md` rather than overclaimed.

## E. The mess

### R12. Human and in-real-life mess is in scope and the register is closed.

> Every class of real-world mess this product claims to survive MUST be named
> in a finite register, and each row MUST be judged handled or declared out of
> scope with a reason. An unlisted class MUST be a defect in the register, not
> a silent pass. A release MUST NOT claim readiness with a row unjudged.

**Invariants.** The register is finite and closed for a given release. A row's
judgment names its evidence class. Adding a class is a supersession of this
requirement's register, not a quiet edit.

**Callers.** Every task that touches a human or a real system.

**The register**, as it stands. Twelve rows, of which seven have evidence.

| # | Class | Judgment | Evidence |
|---|---|---|---|
| M1 | The request may or may not have landed before the process died | handled | `tests/durable_dispatch.rs`, unknown-outcome barrier |
| M2 | The record itself cannot be read | handled | `tests/durable_dispatch.rs`, read failure is refusal |
| M3 | An event returns, or a payload is equal but the event is new | handled | `tests/durable_dispatch.rs`, identity split |
| M4 | A spawned process left orphans | partial | `tests/process_ownership.rs` (T19/T20); T21/T22 open |
| M5 | A locator resolved to the wrong frame or wrong kind | handled | `tests/locator_eligibility.rs` |
| M6 | A contradictory outcome arrives after settlement | handled | `tests/durable_dispatch.rs`, refused |
| M7 | A retrying intermediary re-sends a non-idempotent effect | partial | designed (second `OutcomeObserved` with different evidence is refused); untested against a real intermediary |
| M8 | The target is gone, not moved | unjudged | studied in `docs/frontier.md`; false-heal 40.5 to 57.1 per cent on removed elements, and the decoy and drift score ranges overlap so no threshold separates them |
| M9 | A dialog or another application stole focus mid-flow | out of scope, unreasoned | no evidence |
| M10 | A file was half-written when read | out of scope, unreasoned | no evidence |
| M11 | Clock skew between two hosts | out of scope, unreasoned | no evidence |
| M12 | A credential expired mid-run | out of scope, unreasoned | no evidence |

Three further classes are named and not yet rows: locale and format shifts,
concurrent editors on one record, and software-update drift in an observation
surface. They join the register as a supersession of R12.

**Falsifier.** A release that claims readiness with any row unjudged, or with a
row judged `handled` on evidence of class `unmeasured`.

**Status.** `unmet`, measured only on the seven rows above. Closing M8 through
M12 and the three unrowed classes is most of the remaining product work.

## F. Evidence

### R13. Every claim carries its axis, its gate and its evidence class.

> A readiness or performance claim MUST name the axis it addresses, the gate it
> is measured against, and whether its evidence is measured, proved, argued or
> unmeasured. Losses MUST be published at the same volume as wins. Exclusions
> MUST be named with their direction of bias.

**Invariants.** No number without a method. No relative claim without an
absolute. No exclusion without a stated bias. A comparator is the null
hypothesis (a hand-rolled loop doing identical work) or a named rival with a
stated reason it should win.

**Callers.** Every release claim. Every readiness document.

**Falsifier.** A readiness score with no evidence class, or a performance claim
with no absolute number and no named exclusions. The benchmark fairness gate in
`bench/README.md` is the model: it counts effects *and* condition evaluations,
and a mismatch aborts the run rather than printing a ratio.

**Status.** `met` as a rule going forward, measured by
`docs/production-readiness.md` and `bench/README.md`. It is a rule this
repository has already broken and corrected.

---

## Supersession log

Newest first. One entry per requirement whose normative sentence changed.

*(empty: this revision establishes the requirements.)*

---

## Relationship to the rest of the repository

`docs/production-readiness.md` scores the axes against these requirements; it
is an assessment and is allowed to be wrong. `docs/declarative-orchestration.spec.md`
and `docs/async-sdk-shape.md` specify the surfaces R1, R2 and R6 need;
`docs/effect-kernel.md` specifies the surface R10 needs;
`docs/framework-comparison.md` and `docs/frontier.md` establish the field R9's
axis sits in. Where those documents disagree with this one, this one is the
requirement and the others are the work.

Issue [#109](https://github.com/srinji-kaggss/logicalworks-crates/issues/109)
is the release gate for R10. Issue
[#87](https://github.com/srinji-kaggss/logicalworks-crates/issues/87) is the
implementation of R1, R2 and R6. R8 and the unrowed classes of R12 have no
tracker yet and this paragraph is where they are recorded until they do.
