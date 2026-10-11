# Bounded background work

Four APIs in this workspace bound background work. Pick by what you need
bounded: simultaneous tasks, iterations of a loop, child processes, or threads
for a blocking call. The units differ, and so do the guarantees.

## `Supervisor::run_process`: the same runner, with the result in hand

`Supervisor::run_process(spec)` is `spawn_process` with the completion handed
back: it awaits the child and returns a `ProcessRun`
(`crates/lgwks-bot/src/rt/supervise.rs:3066`). It reuses the same process-group
ownership, deadline and cleanup machinery, so a deadline stops the **group**,
and the run reports the same `CleanupReceipt` a supervised task would.

The report is data, and none of it is a verdict about the work:
`ProcessRun::status` (with `exit_code` and `signal`), `stdout` and `stderr` as
`CapturedStream`s, `deadline_fired`, and `cleanup`
(`crates/lgwks-bot/src/rt/process.rs:980`). Exit zero is reported as exit zero;
judging whether the command did what it was asked is the caller's job.

A stream whose policy is `StdioPolicy::Capture(limit)`
(`crates/lgwks-bot/src/rt/process.rs:64`) keeps its first `limit` bytes — the
ceiling is a `NonZeroUsize`, so it cannot be unbounded — keeps draining the pipe
so a child that writes past the ceiling never blocks, and reports the exact
byte total with a `truncated` flag. The retained buffer is sized once to the
ceiling, so the memory a capture can hold is bounded by the policy rather than
by what the child wrote.

## `repeat`: a bound on iterations

`repeat(&token, budget, body)` is the only loop the module asks you to write, and
it cannot be written without a `Budget` (`crates/lgwks-bot/src/rt/supervise.rs:57`).
The variants are `Iterations(NonZeroU64)`, `For(Duration)`, and `Ongoing`.

Two details that decide how tight your bound really is:

- `Budget::For` checks its deadline between iterations, so a body that blocks for
  longer than the budget overruns it by one iteration
  (`crates/lgwks-bot/src/rt/supervise.rs:233`). Cancellation is not subject to
  that slack, because it interrupts the body itself.
- Every iteration races the token rather than checking it between iterations.
  A cancel drops a body that is still awaiting, and the loop reports
  `Outcome::Cancelled` rather than `Outcome::Exhausted`, so a completed run is
  distinguishable from an interrupted one.
- The loop also yields the executor every `YIELD_INTERVAL` iterations
  (`crates/lgwks-bot/src/rt/supervise.rs:217`), which is what keeps a body whose
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
(`crates/lgwks-bot/src/rt/task.rs:170`). Result `i` is the output of input `i`,
whichever completes first, which is the property `JoinSet::join_next` does not
give you.

The retained `JoinSet` never exceeds `limit` entries. Only the output vector
grows with input length, so fanning out over thousands of inputs needs no manual
chunking.

## What the tests exercise

`crates/lgwks-bot/src/rt/supervise.rs:6078` runs the module's own tests under the
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

## In this guide

- [`Supervisor`: a ceiling on tasks](background-work/supervisor-a-ceiling-on-tasks.md)
- [`Supervisor::spawn_process`: a child process nobody can abandon](background-work/supervisorspawnprocess-a-child-process-nobody-can-abandon.md)
- [`Supervisor::spawn_process_identified`: when the supervisor itself is killed](background-work/supervisorspawnprocessidentified-when-the-supervisor-itself-is-killed.md)
- [Reading a child's output as framed records](background-work/reading-a-childs-output-as-framed-records.md)
- [`spawn_blocking`: a bounded pool](background-work/spawnblocking-a-bounded-pool.md)
- [The limits](background-work/the-limits.md)
