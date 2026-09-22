//! Async child processes: how to describe one, and the one way to run it.
//!
//! [`Command`] describes what to run — the program, its arguments, its
//! environment, its working directory, and where its standard streams go. It is
//! the engine's own `Command`, so every builder method tokio documents is
//! available here.
//!
//! Running it is [`Supervisor::spawn_process`], and it is the only sanctioned
//! path: a process started there is bounded by the supervisor's in-flight
//! ceiling, terminated — **as a whole process group** — when the supervisor is
//! dropped or the task is cancelled, and reported through the supervisor's
//! terminal outcomes, so a process that exited non-zero is distinguishable from
//! one that was cancelled. A Unix process group is not a kernel job object: a
//! descendant that creates a new session can escape it, and the cleanup receipt
//! reports that limitation rather than claiming universal tree containment.
//!
//! # The handle that is not here
//!
//! `Child` and its three pipe types are deliberately not re-exported. A `Child`
//! is a handle to a running process: it can be dropped and the process keeps
//! running, which is exactly the shape this crate removes.
//!
//! A consumer that wants the process's output therefore hands the child a
//! **sink it owns** — `command.stdout(std::process::Stdio::from(file))`, or
//! `Stdio::null()`, or `Stdio::inherit()` (`Stdio` is `std::process`'s; the
//! engine's builder methods take it directly). `Stdio::piped()` is the one
//! form that does not work here: the read end belongs to the `Child`, which
//! nothing outside a supervisor can hold, so a child that filled the pipe
//! buffer would block with no one left to drain it. A consumer that wants the
//! exit status reads it from [`TaskOutcome::Failed`] or from
//! [`Supervisor::stats`].
//!
//! # The one path that is not closed by the type system
//!
//! [`Command::spawn`] still exists, because re-exporting the engine's `Command`
//! is how "describe what to run" keeps the whole builder surface; `Command` is
//! the engine's type, and its methods cannot be narrowed. That method is banned
//! by `clippy.toml` (see the `tokio::process::Command::spawn` entry), with
//! [`Supervisor::spawn_process`] as the named replacement, exactly as a bare
//! `tokio::spawn` is banned with `rt::task`'s own fan-out as its replacement.
//! The ban is a lint rather than a type-level removal, so it binds this
//! workspace and not a consumer outside it — the residual is named here rather
//! than left for a reader to discover.
//!
//! [`Command::spawn`]: lgwks_deps::tokio::process::Command::spawn
//! [`Supervisor::spawn_process`]: crate::rt::supervise::Supervisor::spawn_process
//! [`Supervisor::stats`]: crate::rt::supervise::Supervisor::stats
//! [`TaskOutcome::Failed`]: crate::rt::supervise::TaskOutcome::Failed

pub use lgwks_deps::tokio::process::Command;
