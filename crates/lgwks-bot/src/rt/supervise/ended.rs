//! How one supervised process run ended: the shape every process driver produces.
//!
//! The Unix driver (`super::drive`) and the Windows one (`super::job`) observe a
//! child differently and clean up its tree differently, and both hand the
//! supervisor the same [`ProcessEnd`], so the mapping to a task outcome or a
//! `ProcessRun` is one body and the two platforms cannot drift into reporting
//! the same ending two ways.

use super::supervisor::{ProcessCleanup, TaskEnd};
use crate::rt::process::CapturedStream;
use std::num::NonZeroUsize;
use std::process::ExitStatus;
use std::time::Duration;

/// What happened while observing a supervised child without releasing its pid.
#[derive(Clone, Copy)]
pub(super) enum ProcessObservation {
    /// The child exited; the status is retrieved after cleanup on Unix.
    Exited,
    /// The supervisor's cancellation token was set.
    Cancelled,
    /// The process deadline elapsed before an exit was observed.
    Deadline,
    /// The platform could not perform non-reaping exit observation.
    Unobservable,
}

/// The terminal observation of one supervised child, before it is mapped to a
/// public report.
///
/// One driver produces this shape for both `Supervisor::spawn_process` and
/// `Supervisor::run_process`, so the two cannot drift into cleaning up a tree
/// differently: the mapping to a `TaskEnd` or a `ProcessRun` happens at the
/// edge.
pub(super) struct ProcessEnd {
    /// What the observation saw.
    pub(super) observation: ProcessObservation,
    /// The status the engine reported, when there is one.
    pub(super) status: Option<ExitStatus>,
    /// The cleanup receipt and what it was based on.
    pub(super) cleanup: ProcessCleanup,
    /// Captured stdout.
    pub(super) stdout: CapturedStream,
    /// Captured stderr.
    pub(super) stderr: CapturedStream,
}

/// Map a driven child's terminal observation to the supervisor's internal
/// task-end, so the public `TaskOutcome` is derived from one place.
pub(super) fn task_end(end: ProcessEnd) -> TaskEnd {
    // The signal phase ran while the leader was unreaped; signals are over by the
    // time this is constructed. A `Pending` cleanup stays in this task through the
    // bounded post-reap, signal-zero probe.
    let ProcessEnd {
        observation,
        status,
        cleanup,
        ..
    } = end;
    match observation {
        ProcessObservation::Exited => match status {
            Some(ref observed) if observed.success() => TaskEnd::CompletedWithCleanup { cleanup },
            status => TaskEnd::Failed { status, cleanup },
        },
        ProcessObservation::Cancelled => TaskEnd::CancelledWithCleanup { cleanup },
        ProcessObservation::Deadline | ProcessObservation::Unobservable => TaskEnd::Failed {
            status: None,
            cleanup,
        },
    }
}

/// The bounds one process run is held to, as its spec declares them.
///
/// One value rather than three parameters: `Supervisor::prepare_process`
/// resolves all three from the same spec, so a deadline can never travel with
/// another run's capture ceilings.
#[derive(Debug, Clone, Copy)]
pub(super) struct RunBounds {
    /// How long the child may run before it is stopped.
    pub(super) deadline: Option<Duration>,
    /// The most stdout bytes retained.
    pub(super) out_limit: Option<NonZeroUsize>,
    /// The most stderr bytes retained.
    pub(super) err_limit: Option<NonZeroUsize>,
}
