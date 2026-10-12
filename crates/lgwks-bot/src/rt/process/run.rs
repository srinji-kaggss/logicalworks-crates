//! What a finished run hands back: the captured streams with their ceiling accounting, and the ProcessRun report.

use std::process::ExitStatus;

use super::super::supervise::{CleanupReceipt, Containment};
use super::{FrameRead, Frames};

/// The bytes retained from one captured standard stream, with its ceiling
/// accounting.
///
/// The head of the stream is what survives truncation: the first
/// [`StdioPolicy::Capture`](super::StdioPolicy::Capture) bytes, and no more. `total_bytes` is every byte the
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
    /// Empty unless [`ProcessSpec::stdout_policy`](super::ProcessSpec::stdout_policy) was `Capture`. The retained bytes
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
    /// [`ProcessSpec::stderr_policy`](super::ProcessSpec::stderr_policy) was `Capture`. A child that died before writing
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
