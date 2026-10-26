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

use super::supervise::{CleanupReceipt, Containment};

mod confine;
mod frames;
#[cfg(unix)]
mod resolve;

pub use confine::{Confinement, DenyBeneath, LandlockRules, PathAccess, SandboxProfile};
pub use frames::{DEFAULT_FRAME_CEILING, FrameRead, FrameReadError, Frames, read_frames};

/// The successor's half of
/// [`Supervisor::spawn_process_identified`](crate::rt::supervise::Supervisor::spawn_process_identified).
///
/// A supervisor killed with SIGKILL runs no cleanup, so its children's groups
/// outlive it. The identity each leader had, stored when it was spawned, is what
/// a later process hands to [`reap_orphaned_group`]: it stops the group only
/// while the pid still names that leader, and touches nothing when the pid is
/// gone or reissued. Named here so a consumer imports the record, its parse
/// error, the reap and its answer from the module that describes the process.
pub use lgwks_std::process::{
    OrphanReap, ProcessIdentity, ProcessIdentityError, reap_orphaned_group,
};

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
    /// Start the child from an empty environment.
    ///
    /// Every variable the supervisor would otherwise pass down is dropped, and so
    /// is every delta recorded before this one; deltas recorded after it apply
    /// to the empty set. The child therefore sees exactly the assignments made
    /// after the last `Clear`, which is the allowlist a caller running an
    /// untrusted command needs: nothing of the supervisor's environment reaches
    /// the child unless the caller names it again.
    Clear,
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
    /// A file standard output is written to, replacing `stdout`'s policy.
    stdout_file: Option<PathBuf>,
    /// A file standard error is written to, replacing `stderr`'s policy.
    stderr_file: Option<PathBuf>,
    /// Optional maximum runtime.
    deadline: Option<Duration>,
    /// How the child is confined past its process group.
    confinement: Confinement,
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
            stdout_file: None,
            stderr_file: None,
            deadline: None,
            confinement: Confinement::None,
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

    /// Start the child from an empty environment, dropping every inherited
    /// variable and every assignment recorded before this call.
    ///
    /// Assignments made after it are the child's whole environment, so a
    /// supervisor's credentials, tokens and paths reach an untrusted command
    /// only when the caller names them again. A program named without a
    /// directory is looked up in the `PATH` the child receives, so a cleared
    /// environment that sets no `PATH` searches the platform's default path
    /// rather than the supervisor's; name the program by its absolute path, or
    /// set `PATH` after the clear, when that difference matters.
    pub fn env_clear(&mut self) -> &mut Self {
        self.env.push(EnvDelta::Clear);
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
        self.stdout_file = None;
        self
    }

    /// Set the stderr policy.
    pub fn stderr(&mut self, policy: StdioPolicy) -> &mut Self {
        self.stderr = policy;
        self.stderr_file = None;
        self
    }

    /// Capture stdout, retaining at most `limit` bytes.
    ///
    /// Shorthand for `stdout(StdioPolicy::Capture(limit))`; the value is
    /// non-zero by its type, and every byte the child writes is counted even
    /// past the retained ceiling.
    pub fn capture_stdout(&mut self, limit: NonZeroUsize) -> &mut Self {
        self.stdout = StdioPolicy::Capture(limit);
        self.stdout_file = None;
        self
    }

    /// Capture stderr, retaining at most `limit` bytes.
    ///
    /// The stderr counterpart of [`ProcessSpec::capture_stdout`].
    pub fn capture_stderr(&mut self, limit: NonZeroUsize) -> &mut Self {
        self.stderr = StdioPolicy::Capture(limit);
        self.stderr_file = None;
        self
    }

    /// Write standard output to the file at `path`, creating it if it is absent
    /// and truncating it if it is not.
    ///
    /// For a tool that writes its result to stdout and has no output-path flag.
    /// The file is opened when the child starts, so a path that cannot be opened
    /// refuses the start as an [`io::Error`] and no child runs. The stream is not
    /// captured: [`ProcessRun::stdout`] is empty. Choosing a stdout policy or a
    /// capture afterwards replaces this.
    pub fn stdout_to_file(&mut self, path: impl AsRef<Path>) -> &mut Self {
        self.stdout = StdioPolicy::Null;
        self.stdout_file = Some(path.as_ref().to_path_buf());
        self
    }

    /// Write standard error to the file at `path`, as
    /// [`stdout_to_file`](Self::stdout_to_file) does for standard output.
    pub fn stderr_to_file(&mut self, path: impl AsRef<Path>) -> &mut Self {
        self.stderr = StdioPolicy::Null;
        self.stderr_file = Some(path.as_ref().to_path_buf());
        self
    }

    /// Set the maximum runtime before the supervisor stops the process group.
    pub fn deadline(&mut self, deadline: Duration) -> &mut Self {
        self.deadline = Some(deadline);
        self
    }

    /// Confine the child past its process group.
    ///
    /// A [`Confinement::SandboxProfile`] wraps the spawn in `sandbox-exec` on
    /// macOS, a [`Confinement::Landlock`] allowlist wraps it in `setpriv` on
    /// Linux, and a [`Confinement::DenyBeneath`] denial overmounts the denied
    /// subtrees on Linux or denies reads beneath them in the profile on
    /// macOS; each refuses the spawn anywhere else, so a confinement set by
    /// mistake on the wrong platform starts nothing rather than running the
    /// child unconfined. Confinement applies between the fork and the exec,
    /// so every descendant inherits it.
    pub fn confinement(&mut self, confinement: Confinement) -> &mut Self {
        self.confinement = confinement;
        self
    }

    /// Clone this spec with its program held behind a start gate.
    ///
    /// The gated child runs `/bin/sh`, which stops itself before it execs
    /// this spec's program afterwards, keeping the same pid: an identity read
    /// while the shell is stopped names the process the payload runs as, and
    /// the stop is synchronous — the payload cannot run before the release no
    /// matter when the child is scheduled. Crate-internal: the only producer
    /// is the gated identified spawn, which releases the child once the
    /// identity is recorded.
    ///
    /// The gate shell itself is confined exactly as this spec asks, because
    /// the confinement wraps the shell and inherits across its exec. If the
    /// stop itself failed the shell would exit before the exec, so a broken
    /// gate never runs the payload unrecorded.
    #[cfg(all(unix, feature = "process"))]
    pub(crate) fn gated(&self) -> Self {
        const SCRIPT: &str = "kill -STOP $$ || exit 99; exec \"$@\"";
        let mut gated = self.clone();
        let mut args = Vec::with_capacity(self.args.len().saturating_add(4));
        args.push("-c".into());
        args.push(SCRIPT.into());
        args.push("lgwks-gate".into());
        args.push(self.program.clone());
        args.extend(self.args.iter().cloned());
        gated.program = "/bin/sh".into();
        gated.args = args;
        gated
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

    /// How the child's standard input was wired at spawn.
    ///
    /// The policy is what was *requested*; it does not report what the child
    /// then read, which nothing here can know. `Capture` carries the retained
    /// byte ceiling, so this also answers how much input this child could have
    /// had captured rather than piped.
    #[must_use]
    pub const fn stdin_policy(&self) -> StdioPolicy {
        self.stdin
    }

    /// How the child's standard output was wired at spawn.
    ///
    /// `Capture` means [`Self::stdout`] holds what the child wrote, up to the
    /// ceiling this value names; `Inherit` and `Null` mean the supervisor never
    /// saw it and [`Self::stdout`] is empty rather than absent.
    #[must_use]
    pub const fn stdout_policy(&self) -> StdioPolicy {
        self.stdout
    }

    /// How the child's standard error was wired at spawn.
    ///
    /// The same three choices as [`Self::stdout_policy`], kept separate because
    /// a supervisor commonly captures diagnostics while letting ordinary output
    /// through, and a caller asking "did I get its stderr" reads this.
    #[must_use]
    pub const fn stderr_policy(&self) -> StdioPolicy {
        self.stderr
    }

    /// The optional runtime deadline, for inspection only.
    #[must_use]
    pub const fn deadline_duration(&self) -> Option<Duration> {
        self.deadline
    }

    /// How the child is confined past its process group, for inspection only.
    ///
    /// [`Confinement::None`] unless [`Self::confinement`] named a profile: a
    /// spec that never asked for confinement must not read as one that did.
    #[must_use]
    pub const fn confinement_policy(&self) -> &Confinement {
        &self.confinement
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

    /// Build the engine command's program: the spec's own, or the platform's
    /// confinement wrapper.
    ///
    /// A confined spawn replaces the program with the wrapper and carries the
    /// target as its operand (see [`Self::configure`]), so the wrapper confines
    /// itself before it execs the target and the boundary holds the child and
    /// every descendant.
    ///
    /// # Errors
    ///
    /// [`std::io::ErrorKind::InvalidInput`] for a confinement that could not
    /// confine anything, [`std::io::ErrorKind::NotFound`] for a Landlock rule
    /// naming a missing path, [`std::io::ErrorKind::Unsupported`] for a
    /// confinement this target has no wrapper for. Each refuses before the
    /// fork, so none can run a child unconfined.
    pub(crate) fn command(&self) -> io::Result<Command> {
        confine::command(&self.confinement, &self.resolved_program())
    }

    /// The program the engine command spawns: resolved against the child's
    /// `PATH` when it names no directory.
    ///
    /// `std` spawns through `posix_spawn` only when the program already holds
    /// a `/`; a bare name with a pinned `PATH` (see [`Self::env`]) falls back
    /// to `fork` + `execvp`, which copies the supervisor's page tables and
    /// runs every allocator's at-fork handlers on each spawn. Resolving here
    /// takes the fast path for the case production callers hit.
    ///
    /// The rule is that resolution may only ever name **the file `execvp`
    /// would have run**; anything it cannot decide stays bare and takes the
    /// fork path unchanged. The lookup folds the spec's own deltas over the
    /// inherited `PATH`, in order, so the search sees what the child will see
    /// (INV-BOT-159): after a `Clear` that sets no `PATH`, or a `Remove`, the
    /// path is unknown and the program stays bare. Each entry is judged the
    /// way `execve` judges it — symlinks followed — and an entry `execvp`
    /// would pass over (absent, a directory, no execute bit at all, a search
    /// it may not make) is passed over here. An entry it would *try* ends the
    /// walk: a regular file with every execute bit and a directly executable
    /// head (a `#!` line naming an existing executable interpreter, or this
    /// platform's native binary magic) resolves; anything else — partial
    /// execute bits, a head that could not be read, a shell-fallback script —
    /// stays bare, because whether `execvp` runs it or moves on is the
    /// kernel's call for this caller, not a guess this walk may make. A
    /// working directory that is not absolute stays bare too: `Command` would
    /// read a relative program path against the child's directory a second
    /// time.
    ///
    /// **Not claimed:** the walk happens before the fork rather than after, so
    /// a `PATH` entry replaced in between resolves to the earlier file; and a
    /// native binary whose loader is missing fails with `ENOENT` where
    /// `execvp` would have tried a later entry of the same name.
    pub(crate) fn resolved_program(&self) -> PathBuf {
        #[cfg(unix)]
        if let Some(resolved) =
            resolve::resolve_on_path(&self.program, &self.env, self.cwd.as_deref())
        {
            return resolved;
        }
        PathBuf::from(&self.program)
    }

    /// Configure the private engine command owned by the supervisor.
    ///
    /// # Errors
    ///
    /// The [`io::Error`] from opening a file named by
    /// [`stdout_to_file`](Self::stdout_to_file) or
    /// [`stderr_to_file`](Self::stderr_to_file).
    pub(crate) fn configure(&self, command: &mut Command) -> io::Result<()> {
        if confine::wraps(&self.confinement) {
            // The wrapper's first operand is the target: `sandbox-exec -p
            // <profile> <program> <args...>`, `setpriv <rules> -- <program>
            // <args...>`. The program travels as an argument here because
            // [`Self::command`] already replaced it with the wrapper.
            command.arg(&self.program);
        }
        command.args(&self.args);
        for delta in &self.env {
            match *delta {
                EnvDelta::Set { ref key, ref value } => {
                    command.env(key, value);
                }
                EnvDelta::Remove { ref key } => {
                    command.env_remove(key);
                }
                EnvDelta::Clear => {
                    command.env_clear();
                }
            }
        }
        if let Some(cwd) = self.cwd.as_ref() {
            command.current_dir(cwd);
        }
        command.stdin(self.stdin.into_stdio());
        command.stdout(match self.stdout_file.as_deref() {
            Some(path) => file_stdio(path)?,
            None => self.stdout.into_stdio(),
        });
        command.stderr(match self.stderr_file.as_deref() {
            Some(path) => file_stdio(path)?,
            None => self.stderr.into_stdio(),
        });
        Ok(())
    }
}

/// Open `path` for a child's output: created if absent, truncated if present.
fn file_stdio(path: &Path) -> io::Result<std::process::Stdio> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    Ok(std::process::Stdio::from(file))
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

    /// Read the retained bytes as length-framed records, under `ceiling`.
    ///
    /// The door a caller reads its own child's output through, so the frame
    /// grammar is reached from a [`ProcessRun`] without the caller re-plumbing
    /// the captured bytes into a reader. Infallible by construction: a byte
    /// slice never refuses a read, so there is no [`FrameReadError`] to report
    /// and the one slice path below owns the whole reading.
    ///
    /// # What a truncated capture means for the ending
    ///
    /// A [`CapturedStream`] that reports [`CapturedStream::truncated`] holds a
    /// **prefix the capture cut**, so the pass cannot end in
    /// [`FrameRead::EndOfStream`] and cannot report a child truncation as though
    /// it were one. It overrides exactly the three endings that are *about where
    /// the bytes stop* — [`FrameRead::EndOfStream`],
    /// [`FrameRead::TruncatedPrefix`] and [`FrameRead::TruncatedPayload`] — with
    /// [`FrameRead::CeilingReached`] carrying the capture's own retained
    /// capacity, and [`Frames::is_complete`] is `false` for each of them.
    ///
    /// The two it does **not** override are the two the cut could not have
    /// decided. A [`FrameRead::MalformedPrefix`] was reached from a whole prefix
    /// the capture did retain — a declared `0`, or a length past the reader's
    /// ceiling — so it is rot in the child's output and stands. A
    /// [`FrameRead::CeilingReached`] the *reader* reached stopped the pass before
    /// the cut mattered, so it names the reader's ceiling and stands too. A
    /// capture's cut is a fact about this pass's bounds; it is not a reason to
    /// discard what those bounds already decided.
    ///
    /// Otherwise the endings are the reader's own — the clean end, or the
    /// truncation a child actually performed on its own last record.
    ///
    /// [`FrameReadError`]: crate::rt::process::FrameReadError
    #[must_use]
    pub fn frames(&self, ceiling: usize) -> Frames {
        Frames::of_capture(
            self.bytes.as_slice(),
            ceiling,
            self.truncated,
            self.retained_capacity(),
        )
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
    pub(crate) status: Option<ExitStatus>,
    /// Whether this supervisor stopped the child because its deadline elapsed.
    pub(crate) deadline_fired: bool,
    /// Captured stdout, empty when the policy was not `Capture`.
    pub(crate) stdout: CapturedStream,
    /// Captured stderr, empty when the policy was not `Capture`.
    pub(crate) stderr: CapturedStream,
    /// Process-group cleanup evidence.
    pub(crate) cleanup: CleanupReceipt,
    /// What the cleanup read, signalled and could not account for.
    pub(crate) containment: Containment,
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

    /// The code the child passed to `exit`, or `None` if it did not exit.
    ///
    /// `None` covers both a child still running and one killed by a signal, and
    /// the two are told apart by [`Self::signal`] on Unix or by
    /// [`Self::deadline_fired`]. A code of `0` is a normal successful exit and
    /// is not the same answer as `None`.
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

    /// What the child wrote to standard output, up to the capture ceiling.
    ///
    /// Empty unless [`ProcessSpec::stdout_policy`] was `Capture`. The retained bytes
    /// are the *head* of the stream, so [`CapturedStream::truncated`] is how a
    /// caller knows the tail is missing; [`CapturedStream::total_bytes`] says
    /// how much there would have been.
    #[must_use]
    pub const fn stdout(&self) -> &CapturedStream {
        &self.stdout
    }

    /// What the child wrote to standard error, up to the capture ceiling.
    ///
    /// The same shape as [`Self::stdout`], and empty unless
    /// [`ProcessSpec::stderr_policy`] was `Capture`. A child that died before writing
    /// leaves an empty capture rather than none, so emptiness is never evidence
    /// that the policy was not `Capture`.
    #[must_use]
    pub const fn stderr(&self) -> &CapturedStream {
        &self.stderr
    }

    /// Process-group cleanup evidence.
    ///
    /// A borrow, where this returned the receipt by value: the receipt carries the
    /// pids it could not prove gone, so a by-value read would copy that list per
    /// call.
    #[must_use]
    pub const fn cleanup(&self) -> &CleanupReceipt {
        &self.cleanup
    }

    /// What the cleanup read, signalled, and could not account for.
    ///
    /// The receipt says whether the tree was stopped; this says how much was
    /// looked at to say it — which mechanism read the process table, how many
    /// descendants it named, how many a signal reached, which of them were still
    /// running when the drain ended, and which limit of the mechanism produced
    /// that. A caller that has to report or re-drive the cleanup reads this.
    #[must_use]
    pub const fn containment(&self) -> &Containment {
        &self.containment
    }

    // No constructor, deliberately. One over these six values is a second copy of
    // this struct's own field list, and a second copy of a field list is a place
    // a field can be forgotten — the supervisor is inside the crate, so it fills
    // the literal, and a caller outside it cannot construct a `#[non_exhaustive]`
    // type at all.
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
