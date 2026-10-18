# 2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above (3/13)

Release: 2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above. Page 3 of 13. [Changelog index](../../CHANGELOG.md)

### lgwks_std Added (continued)

- **`ron::Error` and `ron::error::SpannedError` are re-exported.** The `ron`
  module's own functions returned them and no caller could name them, because
  the `ron` crate is an optional dependency and nothing re-exported the types.
  A facade that returns a type has to let a caller write it down, which is the
  same reason `wire` re-exports `rkyv`. Additive.

### lgwks_bot Breaking

**There is no longer any public API that starts concurrent work or a process and
hands the caller something droppable.** A `JoinHandle` a caller can drop is a
task whose outcome nobody ever sees; a `Child` is the same shape with a
process attached. Both were reachable, and both are gone. The only way to start
concurrent work is `Supervisor`, and the only way to start a process is a
supervised one. Nothing is weakened to pay for it — the guarantees move from
convention to the type system, because the constructors are gone.

Removed, each with what replaced it:

- `rt::task::spawn`, `rt::task::spawn_local`, `rt::task::spawn_blocking` — the
  three functions that returned a droppable `JoinHandle`. Fan-out is
  `rt::task::join_all_bounded` (bounded, ordered) or a `JoinSet` the caller
  owns; what outlives the call belongs in a `Supervisor`. `spawn_local`'s
  re-export of `LocalSet` went with it: a local set whose tasks could be
  detached is the same hole one thread down.
- The `rt::task` re-exports of `JoinHandle` and `LocalSet` — nothing public
  returns either any more. `JoinSet`, `JoinError`, and `AbortHandle` remain:
  those are the tracked envelope and the results it yields.
- `rt::runtime::Handle::spawn` — the same droppable handle, reached through the
  runtime instead of the module. This one was not in the original scope and is
  removed because the stated principle decides it: a handle that cannot start
  work but can *drive* it (`Handle::block_on`) still covers every legitimate use.
- `rt::process::{Child, ChildStdin, ChildStdout, ChildStderr}` — a `Child` is a
  handle to a running process, and `Child::kill` reaches neither a shell's
  grandchildren nor a process whose handle was dropped. `Command` stays public:
  a caller must still be able to describe what to run.

Added:

- `Supervisor::spawn_process(&mut Command) -> io::Result<TaskId>`. Takes a
  command, awaits an in-flight permit exactly as `spawn` does, and returns no
  handle — a `TaskId`, which cannot join, abort, or wait. The task it places is
  the process's only owner: the child is put in its own process group and the
  **group** is killed when the task is cancelled or the supervisor drops, so a
  shell cannot leave grandchildren behind. This is what `contract/APPROVED.toml`
  records `rustix` under `lgwks_std` for. A command that cannot start returns
  the `io::Error` to the caller without consuming a slot.
- `Supervisor::default()`. The ceiling is discovered from
  `std::thread::available_parallelism` instead of required, so the safe
  constructor is the one that resolves first; `Supervisor::new(max_in_flight)` is
  unchanged for callers with an opinion. Bounded either way.
- `TaskOutcome::Failed { task, status }`, `TaskOutcome::is_process_failure`,
  `TaskOutcome::exit_status`, `Stats::failed`, and `ShutdownReport::failed`. A
  process that exited non-zero, died from a signal, or whose status could not be
  read is a *failure* carrying its `ExitStatus` — not a completion, and not the
  same report as a kill this supervisor ordered. Without this the three cases
  were one increment, and a bot running a failing command read as a healthy one.
- `clippy.toml` bans `tokio::process::Command::spawn` with
  `Supervisor::spawn_process` as its named replacement. `Command` is the
  engine's own type, so its methods cannot be narrowed, and this is the one
  remaining path that is closed by a lint rather than by the type system — named
  here rather than left to be discovered.

Changed:

- The `process` feature now implies `sync`, its `io`, `lgwks_std/process`, and
  `lgwks_deps/tokio-process` edges unchanged. A `process` build without `sync`
  would expose `Command` and not the runner, leaving the banned `Command::spawn`
  as the only way to use it. No dependency was added: `rustix` stays
  `lgwks_std`'s, under its recorded `allowed_consumers`.
- `Supervisor::shutdown`'s grace before the abort is now a **wall-clock** bound
  (50 ms) rather than eight `yield_now` calls. A counted grace is a same-thread
  heuristic: `yield_now` reschedules the yielding task, so on a multi-threaded
  runtime it gives a body parked on another worker no chance to observe its
  cancellation, and a cancelled task was reported as `Aborted`. Found by this
  change's own process tests, which could not tell a killed command from a
  failed one because of it. A body that returns is settled the moment it does,
  so this is a ceiling on the wait, not a cost charged to every shutdown.
- `rt::task` no longer re-exports anything that starts work; `rt::process`
  exports `Command` and a module doc explaining the one path that is a lint
  rather than a type-level removal.
- `docs/guides/lgwks-bot/background-work.md`, `crates/lgwks-bot/README.md`, and
  the `rt::task` / `rt::process` module docs now describe the supervised-only
  surface rather than the deleted one.
- **`Bot::resolve_effect` takes the `PendingWork` it is about rather than
  `(WorkId, revision)`.** `PendingWork` carries the address, the generation and
  the attempt, and all three are compared before anything is read or written. A
  caller passes back the value `pending()` handed it, so "settle an attempt I
  invented" and "settle the wrong attempt" stop being representable rather than
  being refused after the fact. `WorkId` remains as the address a report names,
  which is what it always was: SPEC-02's "a slot index is instrumentation only".
  This is the intentional tightening RQ-059 asks to be documented — a program
  that named a slot and a revision still compiles if it reads its work from
  `pending()`, and a program that fabricated the pair no longer does.

- **`EffectEvent::to_bytes` returns `lgwks_std::wire::AlignedVec` rather than
  `Vec<u8>`.** The journal record is archived by the estate's binary format now
  instead of a framing this crate maintained beside it, and an `AlignedVec` is
  what the archive has to live in for a reader to access it in place rather than
  decode it. The tag bytes, the width constants and the hand-written
  `encode_into` are gone with it. **The bytes are what a chain head commits to**,
  so this is a durable break as well as a signature one: a journal written before
  this release does not verify against one written after it.

**No crate version is bumped.** The change is unpublished like the five public
modules above it, and it ships in the same release.

### lgwks_bot Added
