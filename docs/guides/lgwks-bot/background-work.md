# Bounded background work

Three APIs in this workspace bound background work. Pick by what you need
bounded: simultaneous tasks, iterations of a loop, or threads for a blocking
call. The units differ, and so do the guarantees.

## `Supervisor`: a ceiling on tasks

`rt::supervise::Supervisor` (feature `sync`) owns a set of background tasks and
stops them when it goes away. `Supervisor::new(max_in_flight)` takes the ceiling,
clamps it into `1..=Semaphore::MAX_PERMITS`, and offers no argument that produces
an unbounded supervisor (`crates/lgwks-bot/src/rt/supervise.rs:536`).

Four properties, all in the module documentation
(`crates/lgwks-bot/src/rt/supervise.rs:9`):

- `spawn` returns no handle. There is no `JoinHandle` for a caller to drop, so
  there is nothing to leak.
- `spawn` awaits a permit before it spawns, so waiting is real backpressure and
  not buffering. The pending work is your own collection.
- `try_spawn` refuses at the ceiling with `AtCapacity` and counts the refusal in
  `Stats::refused`, instead of growing.
- `Drop` cancels the token and aborts the set. There is no `close()` to forget.

`reap` is called at every entry point, which matters because a `JoinSet` retains
a finished task's slot until it is joined. Without reaping, the retained set
would grow with total spawns rather than with live tasks.

Every task ends in exactly one `TaskOutcome`, read through `next_report` (or
returned by `shutdown`), and each outcome carries the `TaskId` the supervisor
assigned in spawn order. `Stats` counts them in four separate ways —
`succeeded`, `cancelled`, `aborted`, `panicked` — so a caller can tell work that
finished from work that died. `Stats::completed` is the sum, a resource count
rather than a success count. The report buffer is capped at the in-flight
ceiling; a caller that spawns without ever reading its reports loses detail,
never memory, and `Stats::reports_dropped` says how much.

```rust
use lgwks_bot::Runtime;
use lgwks_bot::rt::supervise::{Budget, Supervisor};
use lgwks_bot::rt::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Runtime::new()?;
    runtime.block_on(async {
        let mut supervisor = Supervisor::new(4);

        // A bounded loop. `Budget` is a required argument, so there is no call
        // here that reads as "loop forever".
        supervisor
            .spawn_repeating(Budget::For(Duration::from_secs(30)), |_tick| async move {
                // one iteration of your work
            })
            .await;

        // Cancel and drain. Tasks built on `spawn_repeating` or `repeat`
        // observe the token and return; one that never does is aborted.
        // The report is the drained evidence: one terminal outcome per task,
        // each naming the task and saying how it ended.
        let report = supervisor.shutdown().await;
        for outcome in report.outcomes() {
            // `outcome.is_success()`, `outcome.is_panic()`,
            // `outcome.panic_message()`
        }
    });
    Ok(())
}
```

## `repeat`: a bound on iterations

`repeat(&token, budget, body)` is the only loop the module asks you to write, and
it cannot be written without a `Budget` (`crates/lgwks-bot/src/rt/supervise.rs:945`).
The variants are `Iterations(NonZeroU64)`, `For(Duration)`, and `Ongoing`.

Two details that decide how tight your bound really is:

- `Budget::For` checks its deadline between iterations, so a body that blocks for
  longer than the budget overruns it by one iteration
  (`crates/lgwks-bot/src/rt/supervise.rs:145`). Cancellation is not subject to
  that slack, because it interrupts the body itself.
- Every iteration races the token rather than checking it between iterations.
  A cancel drops a body that is still awaiting, and the loop reports
  `Outcome::Cancelled` rather than `Outcome::Exhausted`, so a completed run is
  distinguishable from an interrupted one.
- The loop also yields the executor every `YIELD_INTERVAL` iterations
  (`crates/lgwks-bot/src/rt/supervise.rs:131`), which is what keeps a body whose
  future is ready on its first poll from turning the whole loop into one
  uninterruptible poll. Without it a cancel ordered by another task could not be
  delivered until the budget ran out, and on a current-thread runtime the
  cancelling task could not be scheduled at all.

`Ongoing` is the unbounded budget, and it is bounded in the way that matters: it
is cancellation-terminated, not free-running. It ends when the supervisor that
owns the token ends.

## `join_all_bounded`: a ceiling on a fan-out

`rt::task::join_all_bounded(limit, futures)` (feature `sync`) spawns and awaits
at most `limit` tasks at once, spawning the next pending input as each completes
(`crates/lgwks-bot/src/rt/task.rs:107`). Result `i` is the output of input `i`,
whichever completes first, which is the property `JoinSet::join_next` does not
give you.

The retained `JoinSet` never exceeds `limit` entries. Only the output vector
grows with input length, so fanning out over thousands of inputs needs no manual
chunking.

## `spawn_blocking`: one thread per call

`lgwks_std::task::spawn_blocking` runs a closure on a dedicated OS thread and
returns a future for its result. Its documented bound
(`crates/lgwks-std/src/task.rs:265`) is one OS thread per call while the closure
runs, with no pooled thread between calls. The doc says the quiet part out loud:
"callers that need a ceiling on simultaneous threads (for example
`lgwks_bot::Bot::tick`) bound their own fan-out."

The tick does exactly that, on both adapters, because the wave loop lives in the
`observe_fold` system rather than in either entry point. `MAX_IN_FLIGHT_POLLS`
is 32 (`crates/lgwks-bot/src/ecs.rs:322`), and `observe_fold` polls sources in
waves of that size, because a source poll may occupy one `spawn_blocking` thread.
Chains beyond 32 are polled in additional waves, so the cap holds regardless of
how many chains a spec declares.

## The limits

These are the places the bounds stop applying.

**A bound on tasks is not a bound on time.** `crates/lgwks-bot/src/rt/mod.rs:47`
states it: this is not a scheduler with realtime guarantees, and future
completion order across worker threads is not deterministic. Only the result
order of `join_all_bounded` is.

**Cancellation drops a future. That is not the same as stopping a thread.** The
implementation races each iteration with `token.run_until_cancelled(body(...))`
(`crates/lgwks-bot/src/rt/supervise.rs:981`), which drops the body's future. A
body that is awaiting returns promptly. What happens to work a body handed to
another thread is not established by the inspected source: `spawn_blocking`
spawns an OS thread and offers no abort, and its documented bound is a thread per
call, not a deadline. If your body blocks a thread, treat the supervisor's cancel
as a request, not as an interruption.

**`spawn` can deadlock with itself.** The module documents this one
(`crates/lgwks-bot/src/rt/supervise.rs:92`): `spawn` awaits a permit, so a caller
that holds the last permits inside a task this supervisor owns waits for a slot
that task will never release. The bound is a backpressure contract, not a queue.
If you need to keep producing past the ceiling, use `try_spawn` and decide what
the refusal means.

**`Drop` aborts rather than joins.** A supervisor that goes out of scope cancels
and aborts, and does not wait. Use `shutdown().await` when you need to know the
tasks have finished before continuing — it gives a cooperative body a bounded
number of yields to return on its own before the abort lands, so a task that
observed its token is reported `Cancelled` and one that ignored it as `Aborted`.

**`lgwks_std::task` has no cancellation primitive.** Its `block_on`, `join_all`,
and `spawn_blocking` are for a build with no async runtime. There is a
`JoinHandle`, and no abort and no token.

## What the tests exercise

`crates/lgwks-bot/src/rt/supervise.rs:1002` runs the module's own tests under the
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
