//! Supervised runs into the world (ADR 0006 R3).
//!
//! A supervisor bound to a [`RunFeed`] writes every task it places as an
//! `admitted` event and every terminal outcome as a `settled` one, through the
//! feed's journal, so the world a bot binds the feed to fires on supervised
//! work exactly as it does on a host's runs.

use std::collections::BTreeMap;

use super::TaskId;
use super::outcome::TaskOutcome;
use super::supervisor::Supervisor;
use crate::ecs::{RunFeed, RunOutcome, RunTicket};
#[cfg(all(any(unix, windows), feature = "process"))]
use crate::rt::process::{ProcessRun, ProcessRunError};

/// How an inline process run ended, as the world reads it.
#[cfg(all(any(unix, windows), feature = "process"))]
pub(super) fn ended_as(run: &Result<ProcessRun, ProcessRunError>) -> RunOutcome {
    match *run {
        Ok(ref report) if report.deadline_fired() => RunOutcome::TimedOut,
        Ok(ref report) if report.status().is_some_and(|status| status.success()) => {
            RunOutcome::Succeeded
        }
        Ok(_) => RunOutcome::Failed,
        Err(ProcessRunError::Refused | ProcessRunError::NotStarted { .. }) => RunOutcome::Refused,
        Err(_) => RunOutcome::Cancelled,
    }
}

/// The feed a supervisor writes to, and the tickets of the tasks it has not
/// settled yet.
///
/// The open tickets are bounded by the in-flight ceiling: a task is admitted
/// when it is placed and removed when its outcome is absorbed. A task the feed
/// refused runs anyway and is traced; the feed's own counts say how many it
/// recorded.
pub(super) struct SupervisedRuns {
    /// The world's record.
    feed: RunFeed,
    /// One ticket per placed task that has not ended.
    open: BTreeMap<TaskId, RunTicket>,
}

impl Supervisor {
    /// Write every task this supervisor places and every outcome it absorbs
    /// into `feed`, the record a bot's world binds with
    /// [`RunFeed::bind`].
    #[must_use]
    pub fn with_runs(mut self, feed: RunFeed) -> Self {
        self.runs = Some(SupervisedRuns {
            feed,
            open: BTreeMap::new(),
        });
        self
    }

    /// Admit `task` into the bound feed.
    pub(super) fn admit_world_run(&mut self, task: TaskId) {
        if let Some(ticket) = self.admit_inline_run()
            && let Some(runs) = self.runs.as_mut()
        {
            runs.open.insert(task, ticket);
        }
    }

    /// Admit a run into the bound feed and hand its ticket to the caller: the
    /// form a process run driven inline uses, since it is never absorbed.
    pub(super) fn admit_inline_run(&mut self) -> Option<RunTicket> {
        let runs = self.runs.as_ref()?;
        match runs.feed.admit() {
            Ok(ticket) => Some(ticket),
            Err(error) => {
                lgwks_std::trace::warn!(%error, "supervisor: the run feed refused an admission");
                None
            }
        }
    }

    /// Settle the run an absorbed `outcome` ends.
    pub(super) fn settle_world_run(&mut self, outcome: &TaskOutcome) {
        let ended = match *outcome {
            TaskOutcome::Completed { .. } => RunOutcome::Succeeded,
            TaskOutcome::Cancelled { .. } => RunOutcome::Cancelled,
            TaskOutcome::Failed { .. } => RunOutcome::Failed,
            TaskOutcome::Panicked { .. } => RunOutcome::Panicked,
            TaskOutcome::Aborted { .. } => RunOutcome::Aborted,
            // A cleanup receipt follows an outcome already settled; it ends no run.
            #[cfg(all(unix, feature = "process"))]
            TaskOutcome::CleanupSettled { .. } => return,
        };
        let task = outcome.task();
        let ticket = self.runs.as_mut().and_then(|runs| runs.open.remove(&task));
        self.settle_inline_run(ticket, ended);
    }

    /// Settle a run whose ticket the caller holds.
    pub(super) fn settle_inline_run(&mut self, ticket: Option<RunTicket>, ended: RunOutcome) {
        let (Some(runs), Some(ticket)) = (self.runs.as_ref(), ticket) else {
            return;
        };
        if let Err(error) = runs.feed.settle(ticket, ended) {
            lgwks_std::trace::warn!(%error, "supervisor: the run feed refused a settlement");
        }
    }
}
