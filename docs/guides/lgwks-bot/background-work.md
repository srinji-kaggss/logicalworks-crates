# Bounded background work

Four APIs in this workspace bound background work. Pick by what you need
bounded: simultaneous tasks, iterations of a loop, child processes, or threads
for a blocking call. The units differ, and so do the guarantees.

## `Supervisor`: a ceiling on tasks

`rt::supervise::Supervisor` (feature `sync`) owns a set of background tasks and
stops them when it goes away. `Supervisor::new(max_in_flight)` takes the ceiling,
clamps it into `1..=Semaphore::MAX_PERMITS`, and offers no argument that produces
an unbounded supervisor (`crates/lgwks-bot/src/rt/supervise.rs:747`).
`Supervisor::default()` is the constructor for the caller who has no opinion: it
discovers the ceiling from `std::thread::available_parallelism`, so the safe
default is the *first* thing that resolves rather than something to remember to
ask for, and it is still a real ceiling.

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
assigned in spawn order. `Stats` counts them separately — `succeeded`,
`cancelled`, `aborted`, `panicked`, and (with the `process` feature) `failed` —
so a caller can tell work that finished from work that died from a command that
exited non-zero. A process that exited 3 is a `Failed` outcome carrying its
`ExitStatus`, which is not the same report as one this supervisor killed.
`Stats::completed` is the resource count, a sum rather than a success count. The
report buffer is capped at the in-flight ceiling; a caller that spawns without
ever reading its reports loses detail, never memory, and
`Stats::reports_dropped` says how much.

```rust
use lgwks_bot::Runtime;
use lgwks_bot::rt::supervise::{Budget, Supervisor};
use lgwks_bot::rt::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Runtime::new()?;
    runtime.block_on(async {
        // `new(4)` when you have an opinion about the ceiling, `default()` when
        // you do not; both are bounded.
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

## `Supervisor::spawn_process`: a child process nobody can abandon

`Supervisor::spawn_process(spec)` starts a child under the same in-flight
ceiling as `spawn`, and returns a `TaskId` — not a `Child`
(`crates/lgwks-bot/src/rt/supervise.rs:994`). `rt::process::ProcessSpec` lets
you say what to run without exposing an executable engine handle.
The task this places is the only owner the process has:

```rust
use lgwks_bot::rt::process::ProcessSpec;
use lgwks_bot::rt::supervise::Supervisor;

async fn deploy(supervisor: &mut Supervisor) -> std::io::Result<()> {
    // `ProcessSpec` describes; the supervisor starts, bounds, and owns.
    let mut spec = ProcessSpec::new("sh");
    spec.arg("-c").arg("make -j4");
    supervisor.spawn_process(&spec).await?;
    Ok(())
}
```

Three differences from a raw `Child`:

- **The whole process group is killed, not the child.** A shell started this way
  can spawn a pipeline, and `Child::kill` reaches only the shell — its
  grandchildren keep running with nobody holding their handles. The kill is
  `killpg` on the child's own group (`lgwks_std::process::kill_process_group`),
  and it lands both when the task is cancelled and when the supervisor is dropped
  or aborted.
- **Starting can fail, and the caller is told.** An `io::Error` comes back when
  the program does not exist or is not executable; a failed start does not
  consume a slot.
- **The exit is reported.** Exit zero is `TaskOutcome::Completed`; a non-zero
  exit or a signal death is `TaskOutcome::Failed` carrying the `ExitStatus`; a
  kill this supervisor ordered is `TaskOutcome::Cancelled`. `Stats::failed`
  counts the middle one separately, so a bot whose command fails is not reported
  as a healthy one.

## `repeat`: a bound on iterations

`repeat(&token, budget, body)` is the only loop the module asks you to write, and
it cannot be written without a `Budget` (`crates/lgwks-bot/src/rt/supervise.rs:1473`).
The variants are `Iterations(NonZeroU64)`, `For(Duration)`, and `Ongoing`.

Two details that decide how tight your bound really is:

- `Budget::For` checks its deadline between iterations, so a body that blocks for
  longer than the budget overruns it by one iteration
  (`crates/lgwks-bot/src/rt/supervise.rs:1486`). Cancellation is not subject to
  that slack, because it interrupts the body itself.
- Every iteration races the token rather than checking it between iterations.
  A cancel drops a body that is still awaiting, and the loop reports
  `Outcome::Cancelled` rather than `Outcome::Exhausted`, so a completed run is
  distinguishable from an interrupted one.
- The loop also yields the executor every `YIELD_INTERVAL` iterations
  (`crates/lgwks-bot/src/rt/supervise.rs:142`), which is what keeps a body whose
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
(`crates/lgwks-bot/src/rt/task.rs:93`). Result `i` is the output of input `i`,
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
is 32 (`crates/lgwks-bot/src/ecs.rs:1304`), and `observe_fold` polls sources in
waves of that size, because a source poll may occupy one `spawn_blocking` thread.
Chains beyond 32 are polled in additional waves, so the cap holds regardless of
how many chains a spec declares.

## The limits

These are the places the bounds stop applying.

**A bound on tasks is not a bound on time.** `crates/lgwks-bot/src/rt/mod.rs:75`
states it: this is not a scheduler with realtime guarantees, and future
completion order across worker threads is not deterministic. Only the result
order of `join_all_bounded` is. The same applies to a child process: the
supervisor bounds how many run at once and guarantees the kill reaches the group,
not how quickly the OS tears the group down.

**Cancellation drops a future. That is not the same as stopping a thread.** The
implementation races each iteration with `token.run_until_cancelled(body(...))`
(`crates/lgwks-bot/src/rt/supervise.rs:1509`), which drops the body's future. A
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
wall-clock grace to return on its own before the abort lands, so a task that
observed its token is reported `Cancelled` and one that ignored it as `Aborted`.
The grace is time and not a count of yields, which matters on a multi-threaded
runtime: `yield_now` only reschedules the yielding task, so it cannot give a body
parked on another worker the thread wakeup its return actually needs, and a
counted grace reported cancelled work as aborted
(`crates/lgwks-bot/src/rt/supervise.rs:695`). A body that returns is settled the
moment it does, so the grace is a ceiling on the wait and not a cost charged to
every shutdown.

**`lgwks_std::task` has no cancellation primitive.** Its `block_on`, `join_all`,
and `spawn_blocking` are for a build with no async runtime. There is a
`JoinHandle`, and no abort and no token.

## What the tests exercise

`crates/lgwks-bot/src/rt/supervise.rs:1529` runs the module's own tests under the
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

`crates/lgwks-bot/tests/rt_process.rs` is the black-box acceptance for
`spawn_process`, run with `--features full`: a zero exit reports `Completed`, a
non-zero exit reports `Failed` with its status and counts in `Stats::failed`, a
cancelled command is killed **with its grandchild** — a shell records both its own
and its backgrounded `sleep`'s pid, and the test asserts both are gone — and a
command that cannot start returns `NotFound` without consuming a slot.
`crates/lgwks-bot/tests/rt_async_tier.rs` holds the drain regression: on a real
multi-threaded runtime, a body that returns on its cancellation is reported
`Cancelled` and not `Aborted`.
