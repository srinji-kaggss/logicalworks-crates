# Bounded background work: child processes

Pages of this guide: [tasks](background-work.md) ·
[child processes](background-processes.md) ·
[process results and confinement](process-results.md) ·
[loops, fan-out and blocking](bounded-loops.md) · [limits and tests](background-limits.md)

## `Supervisor::spawn_process`: a child process nobody can abandon

`Supervisor::spawn_process(spec)` starts a child under the same in-flight
ceiling as `spawn`, and returns a `TaskId` — not a `Child`
(`crates/lgwks-bot/src/rt/supervise/run.rs:95`). `rt::process::ProcessSpec` lets
you say what to run without exposing an executable engine handle.
The task this places is the only owner the process has:

`a_borrowed_process_spec_is_started_and_owned_by_the_supervisor`
(`crates/lgwks-bot/tests/it/rt_front_door.rs`) runs it: `ProcessSpec` describes
the command (`sh -c`), and the supervisor starts, bounds and owns it through
`spawn_process(&spec)`.

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
  door. Targets with neither refuse `spawn_process` with `Unsupported`;
  direct-child cleanup is not advertised as containment.
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
(`crates/lgwks-bot/src/rt/supervise/run.rs:125`): the pid **and** the instant the OS
records that pid's process started. Store `leader().to_string()` durably — a
database row, written before the lane does work — and a successor parses it back
and calls `reap_orphaned_group` (`crates/lgwks-std/src/process/identity.rs:386`):

`a_stored_leader_identity_parses_back_and_a_gone_group_is_not_signalled`
(`crates/lgwks-bot/tests/it/rt_front_door.rs`) runs it: `spawn_process_identified`
returns the leader, `leader().to_string()` is the text a lane stores, and once the
group is gone that text parses back to a `ProcessIdentity` and
`reap_orphaned_group` signals nothing.

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
