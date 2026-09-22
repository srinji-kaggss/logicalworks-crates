//! Non-executable descriptions of supervised child processes.
//!
//! [`ProcessSpec`] is data: it records what the supervisor may run, but it is
//! not an engine command and has no process-control methods. The only execution
//! path is [`Supervisor::spawn_process`](crate::rt::supervise::Supervisor::spawn_process), which keeps the child and its process
//! group under supervisor ownership.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

use lgwks_deps::tokio::process::Command;

/// How one standard stream is connected when a process starts.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum StdioPolicy {
    /// Inherit the supervisor process's stream.
    #[default]
    Inherit,
    /// Connect the stream to the platform null device.
    Null,
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
        }
    }
}
