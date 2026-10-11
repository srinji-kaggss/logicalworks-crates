# `Supervisor::spawn_process_identified`: when the supervisor itself is killed

Part of [Bounded background work](../background-work.md).


Every guarantee above runs inside the supervisor's process. A supervisor killed
with `SIGKILL` runs no destructor, so its children's groups keep running with
their leader re-parented to init and nobody left who owns them. The only way back
to them is a record written before the kill.

`Supervisor::spawn_process_identified(spec)` starts the child exactly as
`spawn_process` does and also returns the identity of its group leader
(`crates/lgwks-bot/src/rt/supervise.rs:2822`): the pid **and** the instant the OS
records that pid's process started. Store `leader().to_string()` durably — a
database row, written before the lane does work — and a successor parses it back
and calls `reap_orphaned_group` (`crates/lgwks-std/src/process.rs:1287`):

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
