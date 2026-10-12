//! Settling a job object's tree: the cleanup proof of the Windows process path
//! (#263).
//!
//! A job object is the kernel's own owner of a tree. A process assigned to it
//! stays in it however it forks, and the kernel can say, at any moment, which
//! processes it still holds. So this cleanup is not a capture of a process
//! table followed by signals aimed at what the capture named: it terminates the
//! job once and then reads the job's membership until it is empty. An empty
//! read is the proof, and it is the only thing that earns
//! [`CleanupReceipt::CleanupConfirmed`].
//!
//! The decision table lives here, behind [`JobOwner`], and not beside the
//! `kernel32` calls, so the same code that settles a Windows job is driven on
//! every host against a scripted job (`tests` below). Only the owner that wraps
//! the real job object is Windows-only.

use std::io;
use std::time::Duration;

use super::{CleanupReceipt, Containment, ContainmentMechanism, ProcessCleanup, ResidualRisk};

/// How many membership reads one settle makes before it reports what it saw.
///
/// With `JOB_SETTLE_POLL` this is two seconds, the longest a settle waits on a
/// job that was told to terminate. A terminated member leaves the kernel's list
/// in milliseconds; a member that never leaves is reported by name, not waited
/// on for ever.
#[cfg(windows)]
const JOB_SETTLE_PASSES: u32 = 400;

/// The wait between two membership reads.
#[cfg(windows)]
const JOB_SETTLE_POLL: Duration = Duration::from_millis(5);

/// What one read of a job's membership named.
pub(super) struct JobReading {
    /// The member process ids, sorted and deduplicated.
    pids: Vec<i32>,
    /// Whether the read held only a prefix of the membership.
    truncated: bool,
}

impl JobReading {
    /// A reading of `pids`, `truncated` when the kernel listed fewer than it
    /// held.
    pub(super) const fn new(pids: Vec<i32>, truncated: bool) -> Self {
        Self { pids, truncated }
    }

    /// Whether this reading proves the job holds no process at all.
    ///
    /// An empty prefix of a truncated list proves nothing, so truncation is
    /// part of the question.
    fn proves_empty(&self) -> bool {
        self.pids.is_empty() && !self.truncated
    }
}

/// The kernel owner of one tree: stop every member at once, and say who is left.
pub(super) trait JobOwner: Sync {
    /// Stop every process in the job.
    ///
    /// # Errors
    ///
    /// The operating system's refusal.
    fn terminate(&self) -> io::Result<()>;

    /// The processes the job holds now.
    ///
    /// # Errors
    ///
    /// The operating system's error when the membership cannot be read.
    fn members(&self) -> io::Result<JobReading>;
}

/// Terminate `job` and read its membership until it is empty, within the
/// settle's own bound.
#[cfg(windows)]
pub(super) async fn settle(job: &dyn JobOwner) -> ProcessCleanup {
    settle_within(job, JOB_SETTLE_PASSES, JOB_SETTLE_POLL).await
}

/// [`settle`] with its bound named, so a test spends neither seconds nor sleeps.
///
/// The membership is read before the terminate to name the tree the receipt
/// speaks for; a job already proved empty is not terminated at all, because
/// there is nothing in it to stop. After the terminate the reads decide:
///
/// - an empty read is [`CleanupReceipt::CleanupConfirmed`];
/// - members still listed when the bound ends are
///   [`CleanupReceipt::CleanupSurvivors`], by pid;
/// - a membership that could not be read at the end is
///   [`CleanupReceipt::CleanupPending`], never a claim of absence;
/// - a terminate the operating system refused is
///   [`CleanupReceipt::CleanupFailed`].
pub(super) async fn settle_within(
    job: &dyn JobOwner,
    passes: u32,
    poll: Duration,
) -> ProcessCleanup {
    let mut containment = Containment {
        mechanism: ContainmentMechanism::JobObject,
        ..Containment::default()
    };
    let mut empty_already = false;
    match job.members() {
        Ok(reading) => {
            containment.captured = reading.pids.len();
            empty_already = reading.proves_empty();
        }
        Err(error) => {
            lgwks_std::trace::debug!(%error, "settle: the job membership was unreadable before the terminate");
            containment.note(ResidualRisk::TableUnreadable);
        }
    }
    if !empty_already {
        match job.terminate() {
            // One terminate reaches every member the job held.
            Ok(()) => {
                for _member in 0..containment.captured {
                    containment.record_signal();
                }
            }
            Err(error) => {
                lgwks_std::trace::debug!(%error, "settle: the job terminate was refused");
                return ProcessCleanup {
                    receipt: CleanupReceipt::CleanupFailed,
                    containment,
                };
            }
        }
    }
    let mut last: Option<Vec<i32>> = None;
    for pass in 0..passes {
        match job.members() {
            Ok(reading) if reading.proves_empty() => {
                containment.record_survivors(&[]);
                return ProcessCleanup {
                    receipt: CleanupReceipt::CleanupConfirmed,
                    containment,
                };
            }
            // A truncated list names a prefix of the members, never none of
            // them; an empty prefix is read as an unreadable membership.
            Ok(reading) if reading.pids.is_empty() => last = None,
            Ok(reading) => last = Some(reading.pids),
            Err(error) => {
                lgwks_std::trace::debug!(%error, "settle: the job membership was unreadable");
                last = None;
            }
        }
        if pass.saturating_add(1) < passes {
            crate::rt::time::sleep(poll).await;
        }
    }
    let receipt = match last {
        Some(survivors) => {
            containment.record_survivors(&survivors);
            CleanupReceipt::CleanupSurvivors { survivors }
        }
        None => CleanupReceipt::CleanupPending,
    };
    ProcessCleanup {
        receipt,
        containment,
    }
}

#[cfg(test)]
mod tests {
    use super::{JobOwner, JobReading, settle_within};
    use crate::rt::runtime::block_on;
    use crate::rt::supervise::{CleanupReceipt, ContainmentMechanism};
    use std::collections::VecDeque;
    use std::io;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// What one scripted membership read answers.
    #[derive(Clone)]
    enum Read {
        /// These members, whole.
        Members(Vec<i32>),
        /// These members, and more the list could not hold.
        Truncated(Vec<i32>),
        /// The read failed.
        Unreadable,
    }

    /// A job whose membership answers from a script and repeats its last answer.
    struct ScriptedJob {
        /// The reads still to answer; the last one is kept for every later read.
        reads: Mutex<VecDeque<Read>>,
        /// Whether the terminate is refused.
        refuse: bool,
        /// How many terminates were asked of the job.
        terminated: AtomicUsize,
    }

    impl ScriptedJob {
        /// A job answering `reads` in order.
        fn answering(reads: Vec<Read>) -> Self {
            Self {
                reads: Mutex::new(reads.into()),
                refuse: false,
                terminated: AtomicUsize::new(0),
            }
        }
    }

    impl JobOwner for ScriptedJob {
        fn terminate(&self) -> io::Result<()> {
            self.terminated.fetch_add(1, Ordering::Relaxed);
            if self.refuse {
                return Err(io::Error::from(io::ErrorKind::PermissionDenied));
            }
            Ok(())
        }

        fn members(&self) -> io::Result<JobReading> {
            let mut reads = crate::journal::owner::lock(&self.reads);
            let next = if reads.len() > 1 {
                reads.pop_front()
            } else {
                reads.front().cloned()
            };
            match next {
                Some(Read::Members(pids)) => Ok(JobReading::new(pids, false)),
                Some(Read::Truncated(pids)) => Ok(JobReading::new(pids, true)),
                Some(Read::Unreadable) | None => Err(io::Error::other("scripted unreadable")),
            }
        }
    }

    /// The poll the settle sleeps between reads: short enough to cost nothing.
    const POLL: Duration = Duration::from_millis(1);

    #[test]
    fn an_empty_job_is_confirmed_without_a_terminate() {
        let job = ScriptedJob::answering(vec![Read::Members(vec![])]);
        let cleanup = block_on(settle_within(&job, 3, POLL));
        assert_eq!(cleanup.receipt, CleanupReceipt::CleanupConfirmed);
        assert_eq!(
            job.terminated.load(Ordering::Relaxed),
            0,
            "a job the kernel reports empty has nothing to terminate"
        );
        assert_eq!(
            cleanup.containment.mechanism(),
            ContainmentMechanism::JobObject
        );
        assert!(
            cleanup.containment.is_complete(),
            "an empty job is a whole tree, not a leader that left a tree unseen"
        );
    }

    #[test]
    fn a_job_that_empties_after_the_terminate_is_confirmed_and_counts_its_members() {
        let job = ScriptedJob::answering(vec![
            Read::Members(vec![10, 11, 12]),
            Read::Members(vec![11]),
            Read::Members(vec![]),
        ]);
        let cleanup = block_on(settle_within(&job, 5, POLL));
        assert_eq!(cleanup.receipt, CleanupReceipt::CleanupConfirmed);
        assert_eq!(job.terminated.load(Ordering::Relaxed), 1);
        assert_eq!(cleanup.containment.captured(), 3);
        assert_eq!(cleanup.containment.signalled(), 3);
        assert!(cleanup.containment.survivors().is_empty());
        assert!(cleanup.containment.is_complete());
    }

    #[test]
    fn members_that_outlive_the_bound_are_survivors_by_pid() {
        let job = ScriptedJob::answering(vec![Read::Members(vec![7, 9])]);
        let cleanup = block_on(settle_within(&job, 3, POLL));
        assert_eq!(
            cleanup.receipt,
            CleanupReceipt::CleanupSurvivors {
                survivors: vec![7, 9]
            }
        );
        assert_eq!(cleanup.containment.survivors(), &[7, 9]);
        assert!(!cleanup.containment.is_complete());
    }

    #[test]
    fn a_refused_terminate_is_a_failed_cleanup_and_reads_nothing_further() {
        let mut job = ScriptedJob::answering(vec![Read::Members(vec![5])]);
        job.refuse = true;
        let cleanup = block_on(settle_within(&job, 3, POLL));
        assert_eq!(cleanup.receipt, CleanupReceipt::CleanupFailed);
        assert_eq!(job.terminated.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_membership_that_cannot_be_read_never_claims_absence() {
        let job = ScriptedJob::answering(vec![Read::Members(vec![5]), Read::Unreadable]);
        let cleanup = block_on(settle_within(&job, 3, POLL));
        assert_eq!(cleanup.receipt, CleanupReceipt::CleanupPending);
        assert_eq!(job.terminated.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_truncated_list_is_not_an_empty_job_even_when_the_prefix_is_empty() {
        let job = ScriptedJob::answering(vec![Read::Truncated(vec![])]);
        let cleanup = block_on(settle_within(&job, 2, POLL));
        assert_eq!(
            cleanup.receipt,
            CleanupReceipt::CleanupPending,
            "a prefix proves nothing about the members it left out"
        );
        assert_eq!(job.terminated.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_truncated_tree_is_confirmed_only_by_a_whole_empty_read() {
        let job = ScriptedJob::answering(vec![
            Read::Truncated(vec![1, 2, 3]),
            Read::Truncated(vec![2, 3]),
            Read::Members(vec![]),
        ]);
        let cleanup = block_on(settle_within(&job, 5, POLL));
        assert_eq!(cleanup.receipt, CleanupReceipt::CleanupConfirmed);
        assert_eq!(cleanup.containment.captured(), 3);
    }
}
