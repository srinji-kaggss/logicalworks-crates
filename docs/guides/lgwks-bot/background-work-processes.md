# Bounded background work: child processes

Part of [Bounded background work](background-work.md).

## `Supervisor::spawn_process`: a child process nobody can abandon

`Supervisor::spawn_process(spec)` starts a child under the same in-flight
ceiling as `spawn`, and returns a `TaskId` — not a `Child`
(`crates/lgwks-bot/src/rt/supervise/impl_process.rs:68`). `rt::process::ProcessSpec` lets
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

- **The whole tree is stopped, not just the child.** A shell started this way
  can spawn a pipeline, and `Child::kill` reaches only the shell — its
  grandchildren keep running with nobody holding their handles. On Unix the kill
  is `killpg` on the child's own group (`lgwks_std::process::kill_process_group`),
  while the unreaped leader pins that group id. A later signal-zero probe can
  verify absence without signalling a potentially reused id. On Windows the child
  is assigned to a job object the moment it starts
  (`crates/lgwks-bot/src/rt/supervise/job.rs`); cancel, deadline and exit
  terminate the job, and the receipt is `CleanupConfirmed` only when the kernel
  reads the job's membership empty (`ContainmentMechanism::JobObject`). Dropping
  the task closes the job, which kills what is left. A Windows spec cannot capture
  a stream (`capture_stdout` is refused before the fork); `stdout_to_file` is its
  door. Targets with neither refuse `spawn_process` with `Unsupported`.
- **Starting can fail, and the caller is told.** An `io::Error` comes back when
  the program does not exist or is not executable; a failed start does not
  consume a slot.
- **The exit is reported.** Exit zero is `TaskOutcome::Completed`; a non-zero
  exit or a signal death is `TaskOutcome::Failed` carrying the `ExitStatus`; a
  kill this supervisor ordered is `TaskOutcome::Cancelled`. `Stats::failed`
  counts the middle one separately, so a bot whose command fails is not reported
  as a healthy one.
- **Pending cleanup keeps its capacity.** If the bounded termination pass cannot
  prove absence, the group identity and permit transfer to a supervisor-owned
  cleanup entry. `Supervisor::reap` and `next_report` make bounded observations;
  only an absent group produces `TaskOutcome::CleanupSettled` and releases the
  permit. After `shutdown`, retain its report while cleanup is pending, call
  `ShutdownReport::reap_pending_cleanups` to observe again, then consume outcomes.
  `ShutdownReport::into_outcomes` returns the report in `Err` rather than
  discarding pending ownership.

## `Supervisor::spawn_process_identified`: when the supervisor itself is killed

Every guarantee above runs inside the supervisor's process. A supervisor killed
with `SIGKILL` runs no destructor, so its children's groups keep running with
their leader re-parented to init and nobody left who owns them. The only way back
to them is a record written before the kill.

`Supervisor::spawn_process_identified(spec)` starts the child exactly as
`spawn_process` does and also returns the identity of its group leader
(`crates/lgwks-bot/src/rt/supervise/impl_process.rs:98`): the pid **and** the instant the OS
records that pid's process started. Store `leader().to_string()` durably — a
database row, written before the lane does work — and a successor parses it back
and calls `reap_orphaned_group` (`crates/lgwks-std/src/process/identity.rs:386`):

```rust
use lgwks_bot::rt::process::{OrphanReap, ProcessIdentity, reap_orphaned_group};

fn reap(stored: &str) -> Result<(), Box<dyn std::error::Error>> {
    let leader: ProcessIdentity = stored.parse()?;
    match reap_orphaned_group(&leader)? {
        OrphanReap::Signalled { .. } => {} // the dead supervisor's lane is stopped
        _ => {} // the pid is gone or now names another process: nothing was sent
    }
    Ok(())
}
```

The start instant is what makes the reap safe. A pid alone does not say which
process it names: after the leader exits the OS may hand the number to a
stranger. The reap re-reads the start of the recorded pid and signals only on an
exact match; a pid nobody holds is `LeaderGone` and one whose start differs is
`LeaderReused`, and neither is signalled. On Linux the start is the boot id and
the kernel's start tick; elsewhere it is `ps -o lstart`, read in UTC under the C
locale, with a resolution of one second. `spawn_process` does not pay for the read
— on macOS it is one `ps` run — which is why it is a separate door.

**Not covered:** the read and the signal are two calls, so a pid reissued between
them would be signalled (the allocator has to wrap its whole range first); a group
whose leader already exited is not reachable from the record; and storing the
record is the caller's job.

## `Supervisor::run_process`: the same runner, with the result in hand

`Supervisor::run_process(spec)` is `spawn_process` with the completion handed
back: it awaits the child and returns a `ProcessRun`
(`crates/lgwks-bot/src/rt/supervise/impl_process.rs:225`). It reuses the same process-group
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
