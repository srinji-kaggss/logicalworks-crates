# `Supervisor::spawn_process`: a child process nobody can abandon

Part of [Bounded background work](../background-work.md).


`Supervisor::spawn_process(spec)` starts a child under the same in-flight
ceiling as `spawn`, and returns a `TaskId` — not a `Child`
(`crates/lgwks-bot/src/rt/supervise.rs:2759`). `rt::process::ProcessSpec` lets
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

- **On Unix, the whole process group is killed, not just the child.** A shell started this way
  can spawn a pipeline, and `Child::kill` reaches only the shell — its
  grandchildren keep running with nobody holding their handles. The kill is
  `killpg` on the child's own group (`lgwks_std::process::kill_process_group`),
  while the unreaped leader pins that group id. A later signal-zero probe can
  verify absence without signalling a potentially reused id. Other targets
  refuse `spawn_process` with `Unsupported`; direct-child cleanup is not
  advertised as process-group containment.
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
