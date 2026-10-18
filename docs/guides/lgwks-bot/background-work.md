# Bounded background work

Four APIs in this workspace bound background work. Pick by what you need
bounded: simultaneous tasks, iterations of a loop, child processes, or threads
for a blocking call. The units differ, and so do the guarantees.

Pages of this guide: [tasks](background-work.md) ·
[child processes](background-processes.md) ·
[process results and confinement](process-results.md) ·
[loops, fan-out and blocking](bounded-loops.md) · [limits and tests](background-limits.md)

## `Supervisor`: a ceiling on tasks

`rt::supervise::Supervisor` (feature `sync`) owns a set of background tasks and
stops them when it goes away. `Supervisor::new(max_in_flight)` takes the ceiling,
clamps it into `1..=Semaphore::MAX_PERMITS`, and offers no argument that produces
an unbounded supervisor (`crates/lgwks-bot/src/rt/supervise/supervisor.rs:247`).
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

A flow that only needs bounded fan-out and a deadline never builds a
`Supervisor`; it writes the two bounds as words (run once, it prints
`sizes [1, 3, 0]` for the pages `"a"`, `"bcd"` and `""`):

```rust
lgwks_bot::script! {
    /// Measure every page at once and give up after thirty seconds.
    flow sweep(pages: Vec<String>) -> Vec<usize>:
        let sizes = within 30s:
            let lens = each page in pages:
                page.len()
            lens
        give back sizes
}
```

`each` runs every page at once, as many as the machine sustains, returns the
results in input order and cancels the rest at the first failure. `within 30s`
ends the block with `TimedOut` when the deadline passes. Reach for `Supervisor`
directly when the work must outlive one flow: a loop bounded by a `Budget`,
cancelled and drained by `shutdown`, whose report holds one terminal outcome per
task.
