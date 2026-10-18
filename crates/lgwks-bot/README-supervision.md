# lgwks_bot: supervision

Pages of this README: [overview](README.md) · [execution model](README-execution.md) · [failure](README-failure.md) · [authority, domains and specs](README-authority.md) · [script!](README-script.md) · [async runtime](README-runtime.md) · [supervision](README-supervision.md)

### Supervision

`rt::supervise::Supervisor` is the way to run background work, and it exists so
that a caller never has to reason about a leak or a runaway loop.

A task cannot leak. `spawn` returns no handle, so there is nothing for a caller
to drop. There is no unbounded
constructor and no internal queue: the ceiling comes from `new(max_in_flight)`,
or from `default()` when the caller has no opinion — which is the constructor
that resolves first, since a safe default should not have to be remembered — and
the permit is acquired *before* the spawn, so waiting is real backpressure rather
than buffering. `try_spawn` refuses instead of growing, and counts the refusal.
Finished tasks are reaped at every entry point, which matters because a
`JoinSet` retains a completed task's slot until it is joined. `Drop` cancels and
aborts, so there is no `close()` to forget.

A process cannot leak either. `spawn_process(command)` starts a child under the
same ceiling and returns a `TaskId` rather than a `Child`, so there is no handle
to drop. The child is placed in its own process group and the **group** is
killed — when the task is cancelled, and when the supervisor drops or aborts —
because a shell's grandchildren are the case that matters and `Child::kill`
cannot reach them.

`run_process(command)` is the same machinery with the result handed back: it
awaits the child and returns a `ProcessRun` — the exit status or signal, the
captured stdout and stderr with their truncation accounting, whether a deadline
stopped it, and the process-group cleanup receipt. A stream set to
`StdioPolicy::Capture(limit)` keeps its first `limit` bytes, keeps draining so a
chatty child never blocks, and reports the exact total. Neither
`spawn_process` nor `run_process` is `#[cfg]`-gated differently: one owns
completion, the other reports it, and both are the supervisor.

A loop cannot run away. `repeat` cannot be written without a `Budget`, and every
iteration *races* the token rather than checking it between iterations, so a
cancel drops a body that is still awaiting rather than waiting for it to finish.
`Budget::Ongoing` is the unbounded case, and it is cancellation-bounded rather
than free-running.

A caller can tell how a task ended. Every task produces exactly one
`TaskOutcome`, carrying the `TaskId` the supervisor assigned in spawn order, and
`Stats` counts the outcomes separately: `succeeded`, `cancelled`, `aborted`,
`panicked`, and `failed` (the `process` feature). `Stats::completed` is the
resource count — a sum, not a success count — so a task that panicked before
producing its result can never be read as one that finished. A command that
exited non-zero is `Failed`, carrying its `ExitStatus`; one this supervisor
killed is `Cancelled`; and the two are never the same increment. The report
buffer is capped at the in-flight ceiling, and `Stats::reports_dropped` counts
what a caller that never drains it missed.

`a_supervisor_refuses_past_its_ceiling_and_reports_an_outcome_for_every_task`
(`crates/lgwks-bot/tests/it/rt_front_door.rs`) runs it: a `Supervisor::new(4)`
is given an ongoing repeating task and one `try_spawn`, which is admitted;
`shutdown` cancels, drains and joins, and the report says two tasks were
spawned, two completed, and two outcomes were recorded, each naming its task.

Invariants (enforced by `crates/lgwks-bot/tests/it/rt_async_tier.rs` and
`rt::supervise`'s unit tests):

- **INV-RT-SINGLE-ENTRY** — only `lgwks_deps` authors a `tokio` edge.
- **INV-RT-BOUNDED-FANOUT** — `join_all_bounded` never exceeds its limit, still
  runs every input, and returns results in input order.
- **INV-RT-PANIC-ISOLATION** — a panicking *input* is resumed on the awaiting
  task (it does not abort the process); a panicking *spawned task* becomes a
  `JoinError`.
- **INV-RT-NO-DETACH** — no public API starts a task or a process and returns a
  handle to it. Dropping a `JoinSet` aborts what it holds; dropping a
  `Supervisor` cancels and aborts everything it started. There is no
  `JoinHandle`, no `LocalSet`, and no `Child` on the public surface, so "started
  and then forgotten" is not writable rather than merely discouraged.
- **INV-RT-EXPLICIT-OWNER** — no hidden global reactor; the `Runtime` is owned.
- **INV-RT-SUPERVISED** — a `Supervisor` retains at most its in-flight bound
  however many times it is spawned into, refuses rather than growing, and stops
  every task it owns when it is dropped or shut down.
