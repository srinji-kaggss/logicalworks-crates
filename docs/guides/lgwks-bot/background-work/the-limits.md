# The limits

Part of [Bounded background work](../background-work.md).


These are the places the bounds stop applying.

**A bound on tasks is not a bound on time.** `crates/lgwks-bot/src/rt/mod.rs:86`
states it: this is not a scheduler with realtime guarantees, and future
completion order across worker threads is not deterministic. Only the result
order of `join_all_bounded` is. The same applies to a child process: the
supervisor bounds how many run at once and guarantees the kill reaches the group,
not how quickly the OS tears the group down.

**Cancellation drops a future. That is not the same as stopping a thread.** The
implementation races each iteration with `token.run_until_cancelled(body(...))`
(`crates/lgwks-bot/src/rt/supervise.rs:5969`), which drops the body's future. A
body that is awaiting returns promptly. What happens to work a body handed to
another thread is not established by the inspected source: `spawn_blocking`
hands the closure to a pool thread and offers no abort, and its bound is a
thread ceiling, not a deadline. If your body blocks a thread, treat the
supervisor's cancel as a request, not as an interruption.

**`spawn` can deadlock with itself.** The module documents this one
(`crates/lgwks-bot/src/rt/supervise.rs:92`): `spawn` awaits a permit, so a caller
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
(`crates/lgwks-bot/src/rt/supervise.rs:2033`). A body that returns is settled the
moment it does, so the grace is a ceiling on the wait and not a cost charged to
every shutdown.

**`lgwks_std::task` has no cancellation primitive.** Its `block_on`, `join_all`,
and `spawn_blocking` are for a build with no async runtime. There is a
`JoinHandle`, and no abort and no token.
