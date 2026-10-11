# Bounded background work

Four APIs in this workspace bound background work. Pick by what you need
bounded: simultaneous tasks, iterations of a loop, child processes, or threads
for a blocking call. The units differ, and so do the guarantees.

## `Supervisor`: a ceiling on tasks

`rt::supervise::Supervisor` (feature `sync`) owns a set of background tasks and
stops them when it goes away. `Supervisor::new(max_in_flight)` takes the ceiling,
clamps it into `1..=Semaphore::MAX_PERMITS`, and offers no argument that produces
an unbounded supervisor (`crates/lgwks-bot/src/rt/supervise/supervisor.rs:240`).
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
- `try_spawn` refuses at the ceiling with `TrySpawnRefusal::AtCapacity` and counts the refusal in
  `Stats::refused`, instead of growing.
- `Drop` cancels the token and aborts the set. There is no `close()` to forget.

`reap` is called at every entry point, which matters because a `JoinSet` retains
a finished task's slot until it is joined. Without reaping, the retained set
would grow with total spawns rather than with live tasks.

To wait for everything you placed without stopping it, await `wait_idle()`. It
joins every task on the set's own wakeup — no timer, so no 1 ms tick per wait —
cancels nothing, and returns how many tasks it joined; the supervisor is usable
afterwards. A body that never returns keeps it waiting, so pair it with a
`Budget` or use `shutdown` when the work must end. Polling `reap` in a sleep loop
is the shape it replaces: on Tokio that loop costs a timer tick per round.

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

## The other pages

- [Supervised child processes](background-processes.md): `spawn_process`, `spawn_process_identified`, `run_process`.
- [Framed child output](framed-output.md): reading captured output as records.
- [Bounded loops, fan-outs and blocking calls](bounded-loops.md): `repeat`, `join_all_bounded`, `spawn_blocking`.
- [Where the bounds stop](background-limits.md): the limits, and what the tests exercise.
