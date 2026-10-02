//! Non-executable descriptions of supervised child processes.
//!
//! [`ProcessSpec`] is data: it records what the supervisor may run, but it is
//! not an engine command and has no process-control methods. The two execution
//! paths are
//! [`Supervisor::spawn_process`](crate::rt::supervise::Supervisor::spawn_process),
//! which reports the child's outcome later, and
//! [`Supervisor::run_process`](crate::rt::supervise::Supervisor::run_process),
//! which awaits the child and returns its captured output. Both keep the child
//! and its process group under supervisor ownership, and neither is reachable
//! from a `ProcessSpec`.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

use lgwks_deps::tokio::process::Command;

use super::supervise::CleanupReceipt;

/// How one standard stream is connected when a process starts.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum StdioPolicy {
    /// Inherit the supervisor process's stream.
    #[default]
    Inherit,
    /// Connect the stream to the platform null device.
    Null,
    /// Capture the stream for the supervisor to read back.
    ///
    /// The value is the retained-byte ceiling and is non-zero by construction,
    /// so an unbounded capture is not expressible. The **first** `limit` bytes
    /// are retained and the head of the stream is what survives truncation;
    /// the pipe keeps being drained after the ceiling is reached so a chatty
    /// child can never block on a full pipe, and every byte the child wrote is
    /// counted in [`CapturedStream::total_bytes`]. A stream with this policy is
    /// available to [`Supervisor::run_process`](crate::rt::supervise::Supervisor::run_process);
    /// [`Supervisor::spawn_process`](crate::rt::supervise::Supervisor::spawn_process)
    /// drains it too, so a captured stream never fills, but does not report it.
    Capture(NonZeroUsize),
}

/// An environment delta applied to a process at start.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum EnvDelta {
    /// Set `key` to `value`.
    ///
    /// The key and value are platform-native strings.
    Set {
        /// Variable name.
        key: OsString,
        /// Variable value.
        value: OsString,
    },
    /// Remove `key` from the inherited environment.
    Remove {
        /// Variable name.
        key: OsString,
    },
}

/// Pure, inspectable data describing one supervised process.
///
/// A `ProcessSpec` cannot spawn, wait, inspect status, kill, or dereference an
/// engine handle. It is deliberately `Clone` so callers can retain an audit
/// description while the supervisor owns the execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessSpec {
    /// Program path or name.
    program: OsString,
    /// Ordered argument list.
    args: Vec<OsString>,
    /// Ordered environment changes.
    env: Vec<EnvDelta>,
    /// Optional working directory.
    cwd: Option<PathBuf>,
    /// Standard input policy.
    stdin: StdioPolicy,
    /// Standard output policy.
    stdout: StdioPolicy,
    /// Standard error policy.
    stderr: StdioPolicy,
    /// Optional maximum runtime.
    deadline: Option<Duration>,
}

impl ProcessSpec {
    /// Describe a program without starting it.
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().to_os_string(),
            args: Vec::new(),
            env: Vec::new(),
            cwd: None,
            stdin: StdioPolicy::default(),
            stdout: StdioPolicy::default(),
            stderr: StdioPolicy::default(),
            deadline: None,
        }
    }

    /// Add one argument.
    pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    /// Add an environment assignment.
    pub fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.env.push(EnvDelta::Set {
            key: key.as_ref().to_os_string(),
            value: value.as_ref().to_os_string(),
        });
        self
    }

    /// Remove one inherited environment variable.
    pub fn env_remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
        self.env.push(EnvDelta::Remove {
            key: key.as_ref().to_os_string(),
        });
        self
    }

    /// Set the process working directory.
    pub fn current_dir(&mut self, cwd: impl AsRef<Path>) -> &mut Self {
        self.cwd = Some(cwd.as_ref().to_path_buf());
        self
    }

    /// Set the stdin policy.
    pub fn stdin(&mut self, policy: StdioPolicy) -> &mut Self {
        self.stdin = policy;
        self
    }

    /// Set the stdout policy.
    pub fn stdout(&mut self, policy: StdioPolicy) -> &mut Self {
        self.stdout = policy;
        self
    }

    /// Set the stderr policy.
    pub fn stderr(&mut self, policy: StdioPolicy) -> &mut Self {
        self.stderr = policy;
        self
    }

    /// Capture stdout, retaining at most `limit` bytes.
    ///
    /// Shorthand for `stdout(StdioPolicy::Capture(limit))`; the value is
    /// non-zero by its type, and every byte the child writes is counted even
    /// past the retained ceiling.
    pub fn capture_stdout(&mut self, limit: NonZeroUsize) -> &mut Self {
        self.stdout = StdioPolicy::Capture(limit);
        self
    }

    /// Capture stderr, retaining at most `limit` bytes.
    ///
    /// The stderr counterpart of [`ProcessSpec::capture_stdout`].
    pub fn capture_stderr(&mut self, limit: NonZeroUsize) -> &mut Self {
        self.stderr = StdioPolicy::Capture(limit);
        self
    }

    /// Set the maximum runtime before the supervisor stops the process group.
    pub fn deadline(&mut self, deadline: Duration) -> &mut Self {
        self.deadline = Some(deadline);
        self
    }

    /// The program to execute, for inspection only.
    #[must_use]
    pub fn program(&self) -> &OsStr {
        &self.program
    }

    /// The ordered arguments, for inspection only.
    #[must_use]
    pub fn args(&self) -> &[OsString] {
        &self.args
    }

    /// The ordered environment deltas, for inspection only.
    #[must_use]
    pub fn env_deltas(&self) -> &[EnvDelta] {
        &self.env
    }

    /// The optional working directory, for inspection only.
    #[must_use]
    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    /// The stdin policy, for inspection only.
    #[must_use]
    pub const fn stdin_policy(&self) -> StdioPolicy {
        self.stdin
    }

    /// The stdout policy, for inspection only.
    #[must_use]
    pub const fn stdout_policy(&self) -> StdioPolicy {
        self.stdout
    }

    /// The stderr policy, for inspection only.
    #[must_use]
    pub const fn stderr_policy(&self) -> StdioPolicy {
        self.stderr
    }

    /// The optional runtime deadline, for inspection only.
    #[must_use]
    pub const fn deadline_duration(&self) -> Option<Duration> {
        self.deadline
    }

    /// The retained-byte ceiling for stdout, or `None` when it is not captured.
    #[must_use]
    pub(crate) const fn stdout_capture(&self) -> Option<NonZeroUsize> {
        match self.stdout {
            StdioPolicy::Capture(limit) => Some(limit),
            StdioPolicy::Inherit | StdioPolicy::Null => None,
        }
    }

    /// The retained-byte ceiling for stderr, or `None` when it is not captured.
    #[must_use]
    pub(crate) const fn stderr_capture(&self) -> Option<NonZeroUsize> {
        match self.stderr {
            StdioPolicy::Capture(limit) => Some(limit),
            StdioPolicy::Inherit | StdioPolicy::Null => None,
        }
    }

    /// Configure the private engine command owned by the supervisor.
    pub(crate) fn configure(&self, command: &mut Command) {
        command.args(&self.args);
        for delta in &self.env {
            match *delta {
                EnvDelta::Set { ref key, ref value } => {
                    command.env(key, value);
                }
                EnvDelta::Remove { ref key } => {
                    command.env_remove(key);
                }
            }
        }
        if let Some(cwd) = self.cwd.as_ref() {
            command.current_dir(cwd);
        }
        command.stdin(self.stdin.into_stdio());
        command.stdout(self.stdout.into_stdio());
        command.stderr(self.stderr.into_stdio());
    }
}

impl StdioPolicy {
    /// Convert the data policy into the private engine's standard stream value.
    fn into_stdio(self) -> std::process::Stdio {
        match self {
            Self::Inherit => std::process::Stdio::inherit(),
            Self::Null => std::process::Stdio::null(),
            // The supervisor reads the pipe back while the child runs, so a
            // captured stream never fills and blocks the child.
            Self::Capture(_) => std::process::Stdio::piped(),
        }
    }
}

/// The bytes retained from one captured standard stream, with its ceiling
/// accounting.
///
/// The head of the stream is what survives truncation: the first
/// [`StdioPolicy::Capture`] bytes, and no more. `total_bytes` is every byte the
/// child wrote, so `truncated` is decidable without retaining the tail, and a
/// reader that needs the exact size of a large output still has it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct CapturedStream {
    /// The retained head of the stream, at most the capture ceiling.
    bytes: Vec<u8>,
    /// Every byte the child wrote to this stream.
    total_bytes: u64,
    /// Whether the child wrote more than the retained ceiling.
    truncated: bool,
}

impl CapturedStream {
    /// The retained bytes, at most the capture ceiling.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Every byte the child wrote, retained or not.
    #[must_use]
    pub const fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// Whether the child wrote more than the retained ceiling.
    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }

    /// The capacity of the retained buffer, which bounds what a capture can
    /// allocate.
    ///
    /// The buffer is sized once to the capture ceiling, so this never exceeds
    /// that ceiling however much the child wrote; it is the allocation bound a
    /// peak-memory check asserts against.
    #[must_use]
    pub fn retained_capacity(&self) -> usize {
        self.bytes.capacity()
    }

    /// Build one capture result. Crate-internal: the only producer is the
    /// supervisor's pipe drain.
    #[cfg(unix)]
    pub(crate) fn from_parts(bytes: Vec<u8>, total_bytes: u64, truncated: bool) -> Self {
        Self {
            bytes,
            total_bytes,
            truncated,
        }
    }
}

/// What a process run by
/// [`Supervisor::run_process`](crate::rt::supervise::Supervisor::run_process)
/// reported: how it ended, what it wrote, and how its process group was cleaned
/// up.
///
/// A successful function return is not a claim the command succeeded: the exit
/// status is data here and judging it is the caller's job. A deadline kill is
/// reported through [`ProcessRun::deadline_fired`] rather than as a normal
/// exit, so "the command finished" and "the supervisor stopped it" are never
/// the same report.
#[derive(Debug)]
#[non_exhaustive]
pub struct ProcessRun {
    /// The status the engine reported, or `None` when the supervisor stopped
    /// the child (deadline) or could not read a status.
    status: Option<ExitStatus>,
    /// Whether this supervisor stopped the child because its deadline elapsed.
    deadline_fired: bool,
    /// Captured stdout, empty when the policy was not `Capture`.
    stdout: CapturedStream,
    /// Captured stderr, empty when the policy was not `Capture`.
    stderr: CapturedStream,
    /// Process-group cleanup evidence.
    cleanup: CleanupReceipt,
}

impl ProcessRun {
    /// The exit status the engine reported, when there is one.
    ///
    /// `None` means the child was stopped by the deadline or its status could
    /// not be read; it is not a success. On Unix a signal death reports
    /// [`ExitStatus::code`]`() == None` too, so use
    /// [`ProcessRun::signal`] to tell the two apart.
    #[must_use]
    pub const fn status(&self) -> Option<ExitStatus> {
        self.status
    }

    /// The exit code, when the child exited normally.
    #[must_use]
    pub fn exit_code(&self) -> Option<i32> {
        self.status.and_then(|status| status.code())
    }

    /// The terminating signal, when the child was killed by one (Unix).
    ///
    /// Distinct from an exit code: `kill -TERM $$` reports a signal and no
    /// code, while `exit 0` reports code 0 and no signal. Neither is a claim
    /// about the work the command was asked to do.
    #[cfg(unix)]
    #[must_use]
    pub fn signal(&self) -> Option<i32> {
        use std::os::unix::process::ExitStatusExt;
        self.status.and_then(|status| status.signal())
    }

    /// Whether the supervisor stopped the child because its deadline elapsed.
    ///
    /// A deadline kill reaps the whole process group (INV-BOT-9), so this is a
    /// stop the supervisor ordered, not an exit the command chose.
    #[must_use]
    pub const fn deadline_fired(&self) -> bool {
        self.deadline_fired
    }

    /// The captured stdout.
    #[must_use]
    pub const fn stdout(&self) -> &CapturedStream {
        &self.stdout
    }

    /// The captured stderr.
    #[must_use]
    pub const fn stderr(&self) -> &CapturedStream {
        &self.stderr
    }

    /// Process-group cleanup evidence.
    #[must_use]
    pub const fn cleanup(&self) -> CleanupReceipt {
        self.cleanup
    }

    /// Build one run report. Crate-internal: the only producer is the
    /// supervisor.
    #[cfg(unix)]
    pub(crate) fn new(
        status: Option<ExitStatus>,
        deadline_fired: bool,
        stdout: CapturedStream,
        stderr: CapturedStream,
        cleanup: CleanupReceipt,
    ) -> Self {
        Self {
            status,
            deadline_fired,
            stdout,
            stderr,
            cleanup,
        }
    }
}

/// Why [`Supervisor::run_process`](crate::rt::supervise::Supervisor::run_process)
/// could not produce a [`ProcessRun`].
///
/// The vocabulary draws the one distinction a caller's next move turns on:
/// whether the program ever ran. [`ProcessRunError::Refused`] and
/// [`ProcessRunError::NotStarted`] establish that nothing ran, so a retry is
/// safe; [`ProcessRunError::AfterStart`] establishes that the child was
/// started and its outcome is unknown, so a retry is a possible duplicate.
#[derive(Debug)]
#[non_exhaustive]
pub enum ProcessRunError {
    /// The supervisor refused to start anything: it was cancelled before the
    /// fork, so no instruction of the program ran.
    Refused,
    /// The platform refused to start the program at all — it does not exist,
    /// is not executable, or the fork was refused. Nothing ran.
    NotStarted {
        /// The platform's refusal.
        source: io::Error,
    },
    /// The process started and was then stopped by this supervisor before it
    /// exited on its own, or its terminal status could not be read. It ran, so
    /// the effect is indeterminate.
    AfterStart {
        /// Why the run could not be settled.
        source: io::Error,
    },
}

impl fmt::Display for ProcessRunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Refused => formatter.write_str("the supervisor refused to start a process"),
            Self::NotStarted { ref source } => {
                write!(formatter, "the program did not start: {source}")
            }
            Self::AfterStart { ref source } => {
                write!(
                    formatter,
                    "the process started but did not settle: {source}"
                )
            }
        }
    }
}

impl std::error::Error for ProcessRunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Refused => None,
            Self::NotStarted { ref source } | Self::AfterStart { ref source } => Some(source),
        }
    }
}
