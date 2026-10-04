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

// The one frame grammar this crate already has (INV-BOT-51): a `u32`
// big-endian length prefix and the payload it names. Its reading half is reused
// rather than restated, so "what a torn tail is" has one answer here too rather
// than one per reader of a child's output.
use crate::journal::frame::{LENGTH_BYTES, declared_length, is_possible_length};

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
        let mut pass = Frames::of_slice(self.bytes.as_slice(), ceiling);
        if self.truncated
            && matches!(
                pass.ended,
                FrameRead::EndOfStream
                    | FrameRead::TruncatedPrefix { .. }
                    | FrameRead::TruncatedPayload { .. }
            )
        {
            pass.ended = FrameRead::CeilingReached {
                ceiling: self.retained_capacity(),
            };
        }
        pass
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
    /// Empty unless [`Self::stdout_policy`] was `Capture`. The retained bytes
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
    /// [`Self::stderr_policy`] was `Capture`. A child that died before writing
    /// leaves an empty capture rather than none, so emptiness is never evidence
    /// that the policy was not `Capture`.
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
/// The retained-byte ceiling [`read_frames`] uses when the caller names none.
///
/// The one number a caller needs in order to read a child's framed output
/// without inventing a bound: generous enough for any ordinary record, and
/// finite, so a stream of length prefixes is bounded whether or not the caller
/// thought about it.
pub const DEFAULT_FRAME_CEILING: usize = 64 * 1024;

/// How one length-prefixed record read from a child's captured stream ended.
///
/// A child's exit code is a transport fact; this is the *result* fact, and the
/// reason the type exists is that the two come apart in exactly one way: a prefix
/// that arrived whose payload did not. A reader that trusted the prefix would
/// hand the caller a payload the child never finished writing and report it as a
/// successful decode (T05). So a record is [`Self::Frame`] only when every byte
/// its prefix named is present, and every other reading is either a clean end or
/// a refusal carrying no payload at all.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum FrameRead {
    /// One whole record: its prefix arrived and every byte it named followed.
    Frame {
        /// The payload length the record's prefix declared.
        ///
        /// Equal to `payload.len()`. The two agreeing is what makes this a whole
        /// record rather than the prefix of one, which is why the length is
        /// carried beside the bytes rather than left to be recomputed.
        declared: usize,
        /// The declared payload, exactly `declared` bytes.
        payload: Vec<u8>,
    },
    /// The stream ended cleanly, before another prefix began.
    ///
    /// Not a refusal: a stream that ended between records was read completely,
    /// and reporting it as a failure would be inventing one.
    EndOfStream,
    /// The stream ended part-way through a length prefix.
    ///
    /// Carries the one to three bytes that did arrive, which is the only thing
    /// that separates this from [`Self::EndOfStream`]: these bytes were part-way
    /// through being written, and nothing ever named them.
    TruncatedPrefix {
        /// The prefix bytes that arrived, fewer than the four a length prefix holds.
        partial: Vec<u8>,
    },
    /// The stream ended part-way through a payload a whole prefix had already
    /// named. This is the refusal a prefix alone cannot decide.
    ///
    /// The writer computed and emitted the length before the payload, so these
    /// bytes were addressed and are missing rather than never written — which is
    /// why this is a refusal and not a torn tail to be trimmed away.
    TruncatedPayload {
        /// The payload length the whole prefix declared.
        declared: usize,
        /// The payload bytes that arrived before the stream ended.
        partial: Vec<u8>,
    },
    /// A whole prefix declared a payload length this reader refuses to accept.
    ///
    /// Exactly two lengths: **zero**, or **one past the ceiling in force**. Both
    /// name no record this grammar writes, so a complete prefix carrying one is
    /// rot or a hand rather than an interrupted append, and it is refused rather
    /// than trimmed — the same rule [`crate::journal`] applies to the frames its
    /// own stores read.
    ///
    /// A length that is legal in itself but larger than the room *remaining*
    /// after earlier records is **not** this: it is a well-formed record with
    /// nowhere to go, so it is [`Self::CeilingReached`]. Conflating the two would
    /// call a child's legal record corrupt.
    MalformedPrefix {
        /// The payload length the prefix declared.
        declared: usize,
        /// The ceiling in force when the prefix was refused.
        ceiling: usize,
    },
    /// The caller's ceiling left no room for another record.
    ///
    /// Two ways in, and neither is a malformed byte stream: the pass retained
    /// `ceiling` payload bytes in total, or a legal record declared more than the
    /// room remaining. In the second case the child's prefix was well-formed and
    /// the record simply did not fit, so calling it rot would report a child's
    /// legal record as corruption.
    ///
    /// A [`CapturedStream::frames`] reading of a *truncated* capture also ends
    /// here, carrying the capture's retained capacity rather than the caller's
    /// ceiling: the bytes read are a prefix the capture cut, so nothing about
    /// them can decide whether the child's stream ended. That override covers the
    /// clean end and the two truncations only — an ending the pass already
    /// reached from a whole prefix, or from the caller's own ceiling, names what
    /// decided it and is not replaced. See
    /// [`CapturedStream::frames`](crate::rt::process::CapturedStream::frames).
    ///
    /// The one reading not about the bytes at all, and the reason a bounded
    /// reader is honest about having stopped early rather than claiming the
    /// stream had finished.
    CeilingReached {
        /// The ceiling that stopped the pass.
        ceiling: usize,
    },
}

impl FrameRead {
    /// Whether this record is one whole, complete frame.
    ///
    /// `false` for the clean end, for every truncation and for the ceiling, so a
    /// caller asking this question cannot read "the stream stopped" as "the
    /// record decoded".
    #[must_use]
    pub const fn is_frame(&self) -> bool {
        matches!(*self, Self::Frame { .. })
    }

    /// The payload of a whole frame.
    ///
    /// `None` for every other reading, so a truncated or malformed record cannot
    /// be unwrapped into payload at all: there is no path from a refusal to the
    /// bytes a caller would then decode.
    #[must_use]
    pub fn payload(&self) -> Option<&[u8]> {
        // `*self` as the scrutinee, with `ref` on the one non-`Copy` payload: the
        // workspace forbids `clippy::pattern_type_mismatch`, which refuses a
        // pattern of the referent's type matched against a reference, and every
        // state is named rather than covered by a wildcard so a new variant
        // cannot be added without this question being asked again.
        match *self {
            Self::Frame { ref payload, .. } => Some(payload.as_slice()),
            Self::EndOfStream
            | Self::TruncatedPrefix { .. }
            | Self::TruncatedPayload { .. }
            | Self::MalformedPrefix { .. }
            | Self::CeilingReached { .. } => None,
        }
    }

    /// Whether this reading is a refusal rather than a record or a clean end.
    ///
    /// Separated from [`Self::is_frame`] because "not a frame" and "a problem"
    /// are different questions, and answering both with one `match` is how a
    /// clean end starts being reported as a failed decode.
    #[must_use]
    pub const fn is_refusal(&self) -> bool {
        matches!(
            *self,
            Self::TruncatedPrefix { .. }
                | Self::TruncatedPayload { .. }
                | Self::MalformedPrefix { .. }
                | Self::CeilingReached { .. }
        )
    }

    /// Whether the bytes this reading holds are an incomplete output.
    ///
    /// True for the two truncations, false for a malformed prefix and for the
    /// ceiling: the first two are output that arrived part-way, and the last two
    /// are output this reader would not accept or had no declared room to keep.
    #[must_use]
    pub const fn is_truncated(&self) -> bool {
        matches!(
            *self,
            Self::TruncatedPrefix { .. } | Self::TruncatedPayload { .. }
        )
    }
}

/// Why a framed read could not read the whole stream.
///
/// The one failure a stream can produce that is not about its bytes: the stream
/// itself refused. A stream that merely *ends* never produces this — a clean end
/// and the two truncations are readings reported through [`Frames::ended`],
/// because a device that ran dry is not a device that failed.
#[derive(Debug)]
#[non_exhaustive]
pub struct FrameReadError {
    /// The platform's refusal, kept as its own error so `source()` is the cause
    /// rather than a rendering of it.
    source: io::Error,
    /// Whole records retained before it.
    frames: usize,
    /// Payload bytes retained before it.
    payload_bytes: usize,
}

impl FrameReadError {
    /// How many whole records had been retained when the stream refused.
    #[must_use]
    pub const fn frames(&self) -> usize {
        self.frames
    }

    /// How many payload bytes had been retained when the stream refused.
    #[must_use]
    pub const fn payload_bytes(&self) -> usize {
        self.payload_bytes
    }

    /// The platform's refusal, by kind.
    ///
    /// Named by kind rather than by text, because whether a retry could help is a
    /// question about the kind: an interruption is worth retrying and a malformed
    /// stream never is.
    #[must_use]
    pub fn kind(&self) -> io::ErrorKind {
        self.source.kind()
    }
}

impl fmt::Display for FrameReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "reading framed output failed after {} records and {} payload bytes: {}",
            self.frames, self.payload_bytes, self.source
        )
    }
}

impl std::error::Error for FrameReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// The records one bounded pass read, and the single reading that ended it.
///
/// A pass reads records until the stream ends, a record is refused, or the
/// ceiling is reached. Exactly one of those is reported, as a [`FrameRead`], so a
/// caller that read to the end has one answer about why the stream stopped rather
/// than an empty vector and an absence to interpret.
///
/// # Bounds
///
/// The `ceiling` caps the payload bytes one pass retains, and it is charged
/// **before** a payload is read. A prefix declaring a length past the ceiling is
/// [`FrameRead::MalformedPrefix`] — it names no record this grammar writes — and
/// a legal length that exceeds only the *room remaining* is
/// [`FrameRead::CeilingReached`], a well-formed record with nowhere to go. Either
/// way no payload byte is read, so a stream cannot ask for an allocation by
/// claiming a large record. Every payload buffer the pass holds is therefore at
/// most `ceiling` bytes, and the retained records together at most `ceiling`
/// bytes.
///
/// ```rust
/// # use lgwks_bot::rt::process::{FrameRead, read_frames};
/// // One whole record, then a prefix whose payload never arrived.
/// let mut stream: &[u8] = &[0, 0, 0, 2, 7, 9, 0, 0, 0, 5];
/// let frames = read_frames(&mut stream, 64).expect("a byte slice does not fail");
/// assert_eq!(frames.records().len(), 1, "the whole record before the cut");
/// assert_eq!(frames.records()[0].payload(), Some(&[7, 9][..]));
/// assert_eq!(
///     frames.ended(),
///     &FrameRead::TruncatedPayload { declared: 5, partial: Vec::new() },
///     "a prefix naming five bytes that delivered none is a truncation, never a frame"
/// );
/// assert!(!frames.is_complete(), "a cut record is not a complete reading");
/// ```
///
/// # Errors
///
/// [`FrameReadError`] when `stream` itself refuses, carrying what had been
/// retained. A stream ending is never an error; see [`Frames::ended`].
#[must_use = "the records and the reason the stream stopped are the whole result"]
pub fn read_frames<R: std::io::Read>(
    stream: &mut R,
    ceiling: usize,
) -> Result<Frames, FrameReadError> {
    Frames::read(stream, ceiling)
}

/// The records one pass read and why it stopped.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Frames {
    /// The whole records, in stream order.
    records: Vec<FrameRead>,
    /// The single reading that ended the pass.
    ended: FrameRead,
    /// Payload bytes retained across every whole record.
    retained_bytes: usize,
}

impl Frames {
    /// Read `stream` to its end, its first refusal, or `ceiling` payload bytes.
    ///
    /// The one constructor that can refuse, so a pass over a real reader runs to
    /// one of its three endings or reports the device's refusal; `of_slice`
    /// is the same pass over bytes already in hand and cannot refuse.
    ///
    /// # Errors
    ///
    /// [`FrameReadError`] when `stream` refuses, carrying the whole records and
    /// payload bytes retained before it did.
    pub fn read<R: std::io::Read>(stream: &mut R, ceiling: usize) -> Result<Self, FrameReadError> {
        let pass = read_pass(stream, ceiling);
        match pass {
            Ok((records, ended, retained_bytes)) => Ok(Self {
                records,
                ended,
                retained_bytes,
            }),
            Err(Failed {
                source,
                records,
                retained_bytes,
            }) => Err(FrameReadError {
                source,
                frames: records.len(),
                payload_bytes: retained_bytes,
            }),
        }
    }

    /// The same pass over bytes already in hand, with the device's refusal
    /// dropped because a slice cannot produce one.
    ///
    /// [`Self::read`] maps the impossible arm to an error carrying nothing,
    /// which would report "the device refused" for a stream that never had one;
    /// this path is the one that cannot lie about it. Both run [`read_pass`], so
    /// there is one reading rule rather than one per source of bytes.
    fn of_slice(bytes: &[u8], ceiling: usize) -> Self {
        let mut stream = bytes;
        match read_pass(&mut stream, ceiling) {
            Ok((records, ended, retained_bytes)) => Self {
                records,
                ended,
                retained_bytes,
            },
            // A slice's reads cannot fail, so the arm is unreachable by
            // construction rather than by assumption. If it were ever reached the
            // pass would have stopped *without a reading* — not at a clean end,
            // which claims every record was decoded, and not by dropping what was
            // already whole. So it fails closed: whatever the pass had read is
            // kept, and the ending is the bound it charged against, with
            // `is_complete()` false. The empty `EndOfStream` this arm used to
            // return was the fail-open direction, reporting a pass that stopped
            // early as one that read the stream to the end.
            Err(Failed {
                records,
                retained_bytes,
                ..
            }) => Self {
                records,
                ended: FrameRead::CeilingReached {
                    ceiling: retained_bytes,
                },
                retained_bytes,
            },
        }
    }

    /// The whole records the pass read, in stream order.
    ///
    /// A borrow rather than the `Vec`: handing out the vector would hand out the
    /// right to edit what a reader believes it observed, and the retained-byte
    /// count this pass reports would then describe bytes the caller had changed.
    #[must_use]
    pub fn records(&self) -> &[FrameRead] {
        &self.records
    }

    /// Why the pass stopped: the clean end, the first refusal, or the ceiling.
    ///
    /// Exactly one, and never absent, because a caller must not have to tell "the
    /// stream ended" from "this stopped reading" by the absence of an error.
    #[must_use]
    pub const fn ended(&self) -> &FrameRead {
        &self.ended
    }

    /// Every payload byte retained across the whole records.
    ///
    /// At most the ceiling in force, and independent of how many bytes the stream
    /// carried: a reader that drained far more than it kept reports the kept
    /// count here and the drained count through
    /// [`CapturedStream::total_bytes`].
    #[must_use]
    pub const fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    /// Whether every record the stream carried was decoded whole.
    ///
    /// The question a caller usually wants, and false for a truncated or
    /// malformed tail rather than true-with-a-caveat: a stream whose last record
    /// was cut off has no whole reading of itself.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        matches!(self.ended, FrameRead::EndOfStream)
    }
}

/// What one bounded pass retained when the device refused mid-stream.
///
/// The records and payload bytes are carried out with the refusal rather than
/// dropped, because "the reader failed" and "nothing had been read yet" are
/// different facts and a caller retrying wants to know which it is.
struct Failed {
    /// The device's refusal.
    source: io::Error,
    /// Whole records retained before it.
    records: Vec<FrameRead>,
    /// Payload bytes retained before it.
    retained_bytes: usize,
}

/// Read `stream` to its end, its first refusal, or `ceiling` payload bytes.
///
/// The one pass, shared by the fallible reader path and the infallible slice
/// path, so "how a stream of records ends" has one definition rather than one per
/// source of bytes. The ending travels out as the `Ok` payload rather than
/// through an out-parameter, which is what keeps "the stream ended cleanly"
/// distinct from "a record was refused".
fn read_pass<R: std::io::Read>(
    stream: &mut R,
    ceiling: usize,
) -> Result<(Vec<FrameRead>, FrameRead, usize), Failed> {
    let mut records: Vec<FrameRead> = Vec::new();
    let mut retained = 0_usize;
    let ended = loop {
        match read_one(stream, ceiling, &mut retained, &mut records) {
            Ok(Some(ended)) => break ended,
            // A whole record: keep reading.
            Ok(None) => {}
            Err(source) => {
                let refusal = Err(Failed {
                    source,
                    records,
                    retained_bytes: retained,
                });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "read_pass: returning an error to the caller");
                return refusal;
            }
        }
    };
    Ok((records, ended, retained))
}

/// Read one record from `stream`, charging the ceiling before the payload.
///
/// `Ok(None)` is a whole record already pushed onto `records`; `Ok(Some(..))` is
/// the reading that ends the pass. Returning the ending rather than a boolean is
/// what keeps "the stream ended cleanly" distinct from "a record was refused",
/// which a caller must never confuse.
fn read_one<R: std::io::Read>(
    stream: &mut R,
    ceiling: usize,
    retained: &mut usize,
    records: &mut Vec<FrameRead>,
) -> Result<Option<FrameRead>, io::Error> {
    if *retained >= ceiling {
        return Ok(Some(FrameRead::CeilingReached { ceiling }));
    }
    let mut prefix = [0_u8; LENGTH_BYTES];
    // The counted read, not a bare `read`: a prefix that arrived in two pieces is
    // still a whole prefix, and a reader that read once and called a short result
    // a torn tail would trim records that were never torn.
    match read_counted(stream, &mut prefix)? {
        // Nothing at all: the stream ended between records, which is a complete
        // read rather than an absence of one.
        0 => return Ok(Some(FrameRead::EndOfStream)),
        // One to three bytes of a four-byte prefix: an interrupted write. Exactly
        // those bytes are reported, never zero-padded to the prefix's width,
        // because how much of the prefix arrived is the whole fact here.
        filled if filled < LENGTH_BYTES => {
            return Ok(Some(FrameRead::TruncatedPrefix {
                partial: prefix[..filled].to_vec(),
            }));
        }
        _ => {}
    }
    let declared = declared_length(&prefix);
    // The charge is decided by the prefix, before a byte of payload is read, so
    // the allocation below cannot exceed what the caller declared.
    //
    // Three lengths, three readings, and the split is the whole point:
    //
    // - `declared == 0 || declared > ceiling` names no record this reader would
    //   ever write, so it is rot or a hand: [`FrameRead::MalformedPrefix`].
    // - a legal `declared` that exceeds only the *remaining* room is not rot —
    //   it is a well-formed record with nowhere to go, so it is
    //   [`FrameRead::CeilingReached`] and the pass stops without reading it.
    // - anything else fits, and is allocated at exactly `declared`.
    if !is_possible_length(declared, ceiling) {
        return Ok(Some(FrameRead::MalformedPrefix { declared, ceiling }));
    }
    let remaining = ceiling.saturating_sub(*retained);
    if declared > remaining {
        return Ok(Some(FrameRead::CeilingReached { ceiling }));
    }
    // One exactly-sized buffer per record, allocated only after the charge above
    // and then *moved* into the record: a record owns its bytes, so no payload is
    // ever copied twice and there is no shared buffer to clear between records.
    let mut payload: Vec<u8> = vec![0; declared];
    let read = read_counted(stream, &mut payload)?;
    *retained = retained.saturating_add(declared);
    if read < declared {
        // Charged its declared length: that is the room this pass gave up on the
        // record's behalf, so the accounting describes what was reserved rather
        // than only what arrived. Truncating in place keeps the partial the same
        // allocation rather than a second copy of the bytes that did arrive.
        payload.truncate(read);
        return Ok(Some(FrameRead::TruncatedPayload {
            declared,
            partial: payload,
        }));
    }
    records.push(FrameRead::Frame { declared, payload });
    Ok(None)
}

/// Read into `buf` until it is full or the stream ends, reporting how many arrived.
///
/// The same counted read the crate's frame grammar uses for its own stores,
/// reached through one private helper so "read a frame's bytes or say how many
/// came" has one definition rather than one per reader.
fn read_counted<R: std::io::Read>(stream: &mut R, buf: &mut [u8]) -> Result<usize, io::Error> {
    match crate::journal::frame::read_exact_or_eof(stream, buf) {
        Ok(None) => Ok(0),
        Ok(Some(filled)) => Ok(filled),
        // The counted read's only refusal is the device's, and it is kept as its
        // own error rather than flattened: a caller decides a retry by kind.
        Err(crate::journal::JournalError::Storage(source)) => Err(source),
        Err(other) => Err(io::Error::other(other.to_string())),
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
