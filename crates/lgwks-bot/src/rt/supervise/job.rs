//! The Windows process path: a job object owns the child's whole tree (#263).
//!
//! Windows has no process groups, so the Unix path's group, its descendant
//! capture and its signals have no counterpart here. What it has is stronger: a
//! job object, which the kernel keeps the membership of. This module starts a
//! child, assigns it to a fresh job the moment it exists, and from then on the
//! job is the tree's only owner:
//!
//! - **Cancel, deadline, exit.** The driver terminates the job and reads its
//!   membership until the kernel reports it empty ([`super::job_cleanup`]); the
//!   receipt names `ContainmentMechanism::JobObject` and is
//!   `CleanupReceipt::CleanupConfirmed` only on that empty read.
//! - **Drop and abort.** The job lives in the task's frame, and closing its
//!   handle kills every member (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`). A task
//!   aborted before its first poll, a supervisor dropped, even a supervisor
//!   killed with `TerminateProcess`, stops the tree.
//!
//! One gap is the kernel's, stated rather than hidden: a process the child
//! forked in the instants between its creation and its assignment is not a job
//! member. The start sweeps the process table once for exactly those and assigns
//! them.
//!
//! Output capture is not part of this path. A `std` child's pipes are anonymous
//! pipes, which the async engine cannot read on Windows, so a spec that captures
//! a stream is refused before the fork rather than started and left to hang;
//! `stdout_to_file`, `stderr_to_file`, inherit and null are the supported
//! streams.

use std::io;
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use lgwks_std::process::ProcessJob;

use super::ended::{ProcessEnd, ProcessObservation, RunBounds, task_end};
use super::job_cleanup::{JobOwner, JobReading, settle};
use super::lease::Lease;
use super::supervisor::Supervisor;
use super::{IdentifiedSpawn, LineObserver, TaskId, identify_leader};
use crate::rt::cancel::CancellationToken;
use crate::rt::clock::Clock;
use crate::rt::process::{CapturedStream, ProcessRun, ProcessRunError, ProcessSpec};

/// How often a running child is checked for its exit.
///
/// The cadence the Unix path observes at, so a Windows run adds no finer timer.
const EXIT_POLL: Duration = Duration::from_millis(5);

impl JobOwner for ProcessJob {
    fn terminate(&self) -> io::Result<()> {
        ProcessJob::terminate(self)
    }

    fn members(&self) -> io::Result<JobReading> {
        let set = ProcessJob::members(self)?;
        Ok(JobReading::new(set.pids().to_vec(), set.is_truncated()))
    }
}

/// A started child and the job that owns its tree.
struct JobChild {
    /// The direct child. Holding its handle keeps its pid from being reissued
    /// while the supervisor still names it.
    child: std::process::Child,
    /// The tree's owner. Dropping it closes the handle, and the kill-on-close
    /// limit then stops every member.
    job: ProcessJob,
    /// The child's pid.
    pid: i32,
}

/// Counts one process task that has not yet settled its job.
///
/// Moved into the task's future, so it is released when the task returns or is
/// aborted, and [`Supervisor::shutdown`] can tell a job still settling from a
/// body that ignores its token.
struct LiveJob(Arc<AtomicUsize>);

impl LiveJob {
    /// Count one more live process task.
    fn enter(live: &Arc<AtomicUsize>) -> Self {
        live.fetch_add(1, Ordering::AcqRel);
        Self(Arc::clone(live))
    }
}

impl Drop for LiveJob {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Kill and collect a child that cannot be given to a job, so a refused start
/// leaves nothing running.
fn abandon(mut child: std::process::Child) {
    if let Err(error) = child.kill() {
        lgwks_std::trace::debug!(%error, "start: the unassignable child could not be killed");
    }
    if let Err(error) = child.wait() {
        lgwks_std::trace::debug!(%error, "start: the unassignable child could not be collected");
    }
}

/// Build the private engine command for `spec`, start the child, and put it in
/// a job of its own.
///
/// The Windows counterpart of the Unix `start`: the one constructor both
/// [`Supervisor::spawn_process`] and [`Supervisor::run_process`] call, so the
/// ownership of a started child cannot drift between them.
///
/// # Errors
///
/// `Unsupported` for a spec that captures a stream, `InvalidData` for a pid that
/// does not fit the id type the process primitives speak, and the operating
/// system's error when the job cannot be made, the program cannot start, or the
/// child cannot be assigned. A child that started and could not be assigned is
/// killed before the error returns.
fn start(spec: &ProcessSpec, bounds: &RunBounds) -> io::Result<JobChild> {
    if bounds.out_limit.is_some() || bounds.err_limit.is_some() {
        let refusal = Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "lgwks_bot: a Windows process cannot capture a stream; use stdout_to_file or stderr_to_file",
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "start: returning an error to the caller");
        return refusal;
    }
    let mut command = spec.command()?;
    spec.configure(&mut command)?;
    let job = ProcessJob::new()?;
    let child = command.into_std().spawn()?;
    let pid = match i32::try_from(child.id()) {
        Ok(pid) => pid,
        Err(error) => {
            abandon(child);
            let refusal = Err(io::Error::new(io::ErrorKind::InvalidData, error));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "start: the child id does not fit an i32");
            return refusal;
        }
    };
    if let Err(error) = job.assign(pid) {
        abandon(child);
        lgwks_std::trace::debug!(error = %error, pid, "start: the child could not join its job");
        return Err(error);
    }
    adopt_early_forks(&job, pid);
    Ok(JobChild { child, job, pid })
}

/// Assign what the child forked before its own assignment landed.
///
/// A process created in that window is a descendant of the child but not a
/// member of its job. The process table names it, and the job can take it; a
/// process that is already a member, or has already gone, is refused and
/// skipped.
fn adopt_early_forks(job: &ProcessJob, pid: i32) {
    let early = match lgwks_std::process::capture_descendants(pid) {
        Ok(set) => set,
        Err(error) => {
            lgwks_std::trace::debug!(%error, pid, "start: the process table was unreadable; early forks are not swept");
            return;
        }
    };
    for descendant in early.pids() {
        if let Err(error) = job.assign(*descendant) {
            lgwks_std::trace::debug!(%error, pid = *descendant, "start: an early fork was not assigned");
        }
    }
}

/// Watch the child until it exits, the deadline passes, or the token is
/// cancelled, leaving it a running process the job still owns.
///
/// The exit is read with `try_wait`, which holds the child's handle and so its
/// pid; the status of an exit comes back with it.
async fn observe(
    clock: &Clock,
    child: &mut JobChild,
    deadline: Option<Duration>,
    token: &CancellationToken,
) -> (ProcessObservation, Option<ExitStatus>) {
    // Anchored on the governing clock's own timeline, as the Unix observation
    // is, so a caller-advanceable clock exhausts it in one step.
    let started_at = clock.now();
    let deadline = deadline.map(|duration| started_at.saturating_add(duration));
    loop {
        if token.is_cancelled() {
            return (ProcessObservation::Cancelled, None);
        }
        if deadline.is_some_and(|at| clock.now() >= at) {
            return (ProcessObservation::Deadline, None);
        }
        match child.child.try_wait() {
            Ok(Some(status)) => return (ProcessObservation::Exited, Some(status)),
            Ok(None) => {}
            Err(error) => {
                lgwks_std::trace::debug!(%error, pid = child.pid, "observe: the child's exit could not be read");
                return (ProcessObservation::Unobservable, None);
            }
        }
        if token
            .run_until_cancelled(crate::rt::time::sleep(EXIT_POLL))
            .await
            .is_none()
        {
            return (ProcessObservation::Cancelled, None);
        }
    }
}

/// Drive one started child to a terminal observation, then settle its job.
///
/// The Windows counterpart of the Unix `drive_process`: both produce the same
/// [`ProcessEnd`], so the mapping to a task outcome or a [`ProcessRun`] is one
/// body. The job is settled whatever the observation was, an exit included: a
/// child that finished by itself may have left descendants running, and the
/// membership read is what says it did not.
async fn drive_job(
    clock: &Clock,
    mut child: JobChild,
    token: &CancellationToken,
    deadline: Option<Duration>,
) -> ProcessEnd {
    let (observation, status) = observe(clock, &mut child, deadline, token).await;
    let cleanup = settle(&child.job).await;
    ProcessEnd {
        observation,
        status,
        cleanup,
        stdout: CapturedStream::default(),
        stderr: CapturedStream::default(),
    }
}

impl Supervisor {
    /// Start `spec` as a supervised child process, owned by a job object.
    ///
    /// Behaves as the Unix door does where it matters: a slot is awaited before
    /// anything starts, no handle is returned, and the process is reported like
    /// any other task. The tree is owned by a Windows job object rather than a
    /// process group, so cancel, deadline, exit and drop each stop every
    /// descendant, including one that outlived its parent.
    ///
    /// A spec that captures a stream is refused with
    /// [`io::ErrorKind::Unsupported`] before the fork; see the module docs.
    ///
    /// # Errors
    ///
    /// An [`io::Error`] when the supervisor was cancelled
    /// ([`SupervisorCancelled`](super::SupervisorCancelled)) or the process could
    /// not be started or assigned to its job. A refused start does not consume a
    /// slot.
    pub async fn spawn_process(&mut self, spec: &ProcessSpec) -> io::Result<TaskId> {
        let (task, ()) = self.spawn_job_reading(spec, |_| Ok(())).await?;
        Ok(task)
    }

    /// Start `spec` exactly as [`Supervisor::spawn_process`] does, and also
    /// return the identity of the process that leads its tree.
    ///
    /// The identity is read while the supervisor still holds the child's handle,
    /// so the pid cannot have been reissued. A successor hands it to
    /// [`reap_orphaned_group`](crate::rt::process::reap_orphaned_group).
    ///
    /// # Errors
    ///
    /// Every error [`Supervisor::spawn_process`] returns, and the read's own
    /// error when the leader's identity could not be read; that spawn's job is
    /// closed before the error returns, so no unrecorded tree is left running.
    pub async fn spawn_process_identified(
        &mut self,
        spec: &ProcessSpec,
    ) -> io::Result<IdentifiedSpawn> {
        let (task, leader) = self.spawn_job_reading(spec, identify_leader).await?;
        Ok(IdentifiedSpawn { task, leader })
    }

    /// The one admission, start and placement both Windows process spawns share.
    ///
    /// `read` sees the leader's pid while the child is started and owned by its
    /// job, and what it returns travels back beside the task id. A refusal from
    /// it drops the job, which kills the tree.
    async fn spawn_job_reading<T>(
        &mut self,
        spec: &ProcessSpec,
        read: impl FnOnce(i32) -> io::Result<T>,
    ) -> io::Result<(TaskId, T)> {
        let lease = self.admit_process().await?;
        let (token, bounds) = self.prepare_process(spec);
        let clock = self.clock.clone();
        let child = start(spec, &bounds)?;
        let live = LiveJob::enter(&self.live_jobs);
        let read = read(child.pid)?;
        let deadline = bounds.deadline;
        let task = self.place(lease, async move {
            let end = drive_job(&clock, child, &token, deadline).await;
            drop(live);
            task_end(end)
        });
        Ok((task, read))
    }

    /// The Windows body of the process driver every runner shares, from an
    /// admitted lease to a report.
    ///
    /// The lease is held for the whole run, so capacity comes back through the
    /// same door whether the call named a tenant or not. Nothing is captured on
    /// this target, so `on_line` observes nothing.
    pub(super) async fn drive_run(
        &mut self,
        lease: Lease,
        spec: &ProcessSpec,
        on_line: Option<LineObserver<'_>>,
    ) -> Result<ProcessRun, ProcessRunError> {
        let _held = lease;
        let _unobserved = on_line;
        if self.token.is_cancelled() {
            self.refused = self.refused.saturating_add(1);
            let refusal = Err(ProcessRunError::Refused);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "drive_run: returning an error to the caller");
            return refusal;
        }
        let (token, bounds) = self.prepare_process(spec);
        let clock = self.clock.clone();
        let child = match start(spec, &bounds) {
            Ok(started) => started,
            Err(source) => {
                let refusal = Err(ProcessRunError::NotStarted { source });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "drive_run: returning an error to the caller");
                return refusal;
            }
        };
        self.spawned = self.spawned.saturating_add(1);
        let end = drive_job(&clock, child, &token, bounds.deadline).await;
        self.finish_run(end)
    }
}
