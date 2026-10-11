# `spawn_blocking`: a bounded pool

Part of [Bounded background work](../background-work.md).


`lgwks_std::task::spawn_blocking` runs a closure on a process-wide blocking
pool and returns a future for its result
(`crates/lgwks-std/src/task.rs:419`). At most 512 pool threads run at once;
a job submitted while all of them are busy waits its turn, and a thread with
no work for ten seconds exits, so an idle process holds none. Jobs that wait
on each other must number fewer than the ceiling, or the waiters hold every
thread the awaited job needs.

`spawn_blocking` keeps its 1.0 contract: its wait queue is not bounded and it
never refuses. `try_spawn_blocking` is the bounded form. When 16,384 jobs are
already waiting it refuses with `SpawnError::AtCapacity`, and when no thread
can be started it refuses with `SpawnError::Os`; either way the job never ran.
`lgwks_bot`'s `net::Endpoint` poll uses the bounded form and reports a refusal
as a `Refused` domain error, because nothing was sent.

**A process that wants a different ceiling sets it once, before first use.**
`configure_blocking_pool(threads)` builds the pool at that ceiling; the pool's
own creation arbitrates, so a configure that loses the race to a first job is
refused with `PoolConfigError::InUse` rather than silently ignored, and a
second configure is refused with `AlreadyConfigured` naming the ceiling in
force. Asking again for the ceiling already in force is not a refusal: the pool
is at it.

**A process that owns the pool can shut it down.**
`shutdown_blocking_pool(within)` closes admission, lets the queued and running
jobs finish, and joins every pool thread inside the deadline — no thread
outlives a `PoolShutdown::Drained`. It never cancels queued or running work: a
thread with no job leaves instead of parking, and a parked thread is woken to
take a waiting job or leave. When the deadline expires first it reports
`PoolShutdown::DeadlineExceeded { joined, running, queued }`, the threads still
executing and the jobs still waiting, and their join handles stay registered so
a later shutdown joins them. After a shutdown both entry points refuse as
`SpawnError::Shutdown`; the never-refusing `spawn_blocking` fails its awaiter
with that refusal as the payload, as it does for `Os`. A dropped `JoinHandle`
still does not stop its thread — the pool's shutdown is the way to stop it
being idle, not a way to cancel a running closure.

The tick still bounds its own fan-out on both adapters, because the wave loop
lives in the `observe_fold` system rather than in either entry point.
`MAX_IN_FLIGHT_POLLS` is 32 (`crates/lgwks-bot/src/ecs/components.rs:613`), and
`observe_fold` polls sources in waves of that size. Chains beyond 32 are polled
in additional waves, so the cap holds regardless of how many chains a spec
declares.
