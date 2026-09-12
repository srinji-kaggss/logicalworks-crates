//! Async child processes.
//!
//! The child's lifetime is a resource: dropping a [`Child`] without waiting
//! leaves the process running unless it was killed first. Callers that must own
//! the process use [`Child::kill`] and [`Child::wait`] explicitly, or
//! [`Child::start_kill`] when waiting is not possible.
//!
//! [`Child::kill`]: lgwks_deps::tokio::process::Child::kill
//! [`Child::wait`]: lgwks_deps::tokio::process::Child::wait
//! [`Child::start_kill`]: lgwks_deps::tokio::process::Child::start_kill

pub use lgwks_deps::tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
