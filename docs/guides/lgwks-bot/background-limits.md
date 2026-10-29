# Bounded background work: limits and tests

Pages of this guide: [tasks](background-work.md) ·
[child processes](background-processes.md) ·
[process results and confinement](process-results.md) ·
[loops, fan-out and blocking](bounded-loops.md) · [limits and tests](background-limits.md)

## The limits

These are the places the bounds stop applying.

**A bound on tasks is not a bound on time.** `crates/lgwks-bot/src/rt/mod.rs:85`
states it: this is not a scheduler with realtime guarantees, and future
completion order across worker threads is not deterministic. Only the result
order of `join_all_bounded` is. The same applies to a child process: the
supervisor bounds how many run at once and guarantees the kill reaches the group,
not how quickly the OS tears the group down.

**Cancellation drops a future. That is not the same as stopping a thread.** The
implementation races each iteration with `token.run_until_cancelled(body(...))`
(`crates/lgwks-bot/src/rt/supervise/repeat.rs:168`), which drops the body's future. A
body that is awaiting returns promptly. What happens to work a body handed to
another thread is not established by the inspected source: `spawn_blocking`
hands the closure to a pool thread and offers no abort, and its bound is a
thread ceiling, not a deadline. If your body blocks a thread, treat the
supervisor's cancel as a request, not as an interruption.

**`spawn` can deadlock with itself.** The module documents this one
(`crates/lgwks-bot/src/rt/supervise.rs:80`): `spawn` awaits a permit, so a caller
that holds the last permits inside a task this supervisor owns waits for a slot
that task will never release. The bound is a backpressure contract, not a queue.
If you need to keep producing past the ceiling, use `try_spawn` and decide what
the refusal means.

**`Drop` aborts rather than joins.** A supervisor that goes out of scope cancels
and aborts, and does not wait. Use `shutdown().await` when you need to know the
tasks have finished before continuing — it gives a cooperative body a bounded
wall-clock grace to return on its own before the abort lands, so a task that
observed its token is reported `Cancelled` and one that ignored it as `Aborted`.
The grace is time and not a count of yields, which matters on a multi-threaded
runtime: `yield_now` only reschedules the yielding task, so it cannot give a body
parked on another worker the thread wakeup its return actually needs, and a
counted grace reported cancelled work as aborted
(`crates/lgwks-bot/src/rt/supervise.rs:525`). A body that returns is settled the
moment it does, so the grace is a ceiling on the wait and not a cost charged to
every shutdown.

**`lgwks_std::task` has no cancellation primitive.** Its `block_on`, `join_all`,
and `spawn_blocking` are for a build with no async runtime. There is a
`JoinHandle`, and no abort and no token.

## What the tests exercise

`crates/lgwks-bot/src/rt/supervise/tests.rs:1` runs the module's own tests under the
ordinary workspace test run. They cover an iteration budget stopping at its
limit, an `Ongoing` budget stopping at a cancel, cancellation interrupting a body
that is still awaiting, `try_spawn` refusing at the bound rather than growing,
the retained set staying at the bound across 64 spawns, a dropped supervisor
cancelling its tasks, and `shutdown` draining every task. Four of them are the
regression for the terminal-outcome reports: a panicking task is not counted as
a success and its payload is preserved, a task that panicked after an effect is
not reported as one that finished, cooperative cancellation and abort are told
apart, and a shutdown that outlives the report cap still hands every outcome
over.

`crates/lgwks-bot/tests/it/rt_process.rs` is the black-box acceptance for
`spawn_process`, run with `--features full`: a zero exit reports `Completed`, a
non-zero exit reports `Failed` with its status and counts in `Stats::failed`, a
cancelled command is killed **with its grandchild** — a shell records both its own
and its backgrounded `sleep`'s pid, and the test asserts both are gone — and a
command that cannot start returns `NotFound` without consuming a slot.
`crates/lgwks-bot/tests/it/rt_async_tier.rs` holds the drain regression: on a real
multi-threaded runtime, a body that returns on its cancellation is reported
`Cancelled` and not `Aborted`.
`crates/lgwks-bot/tests/it/sim_landlock.rs` is the Landlock acceptance on
Linux: seeded grant sets decide every read for the child and its `setsid -w`
grandchild, 16 and 100 concurrent tenants never read each other's files, and
invalid, missing-path and foreign-platform allowlists start nothing.
