impl Supervisor {
    /// Turn one join result into a counted, retained terminal outcome.
    ///
    /// The `JoinError` is read, not discarded: `is_panic` and `is_cancelled`
    /// are the two ways a task ends without returning, and the panic payload is
    /// carried into the report rather than logged and dropped.
    fn absorb(
        &mut self,
        joined: Result<(Id, TaskEnd), super::task::JoinError>,
        retention: Retention,
    ) {
        // The engine's id is the only identity a `JoinError` carries, which is
        // why the map exists at all; the success path has it in the value too,
        // and the two agree. Bound by `ref` so the match below can still
        // consume `joined` by value; a pattern on a reference is what this
        // crate's `clippy::pattern_type_mismatch` refuses.
        let raw = match joined {
            Ok((ref id, _)) => *id,
            Err(ref error) => error.id(),
        };
        let outcome = match joined {
            Ok((_, TaskEnd::Completed)) => {
                self.succeeded = self.succeeded.saturating_add(1);
                self.identities
                    .remove(&raw)
                    .map(|task| TaskOutcome::Completed {
                        task,
                        cleanup: None,
                        #[cfg(feature = "process")]
                        containment: None,
                    })
            }
            #[cfg(feature = "process")]
            Ok((_, TaskEnd::CompletedWithCleanup { cleanup })) => {
                self.succeeded = self.succeeded.saturating_add(1);
                let (receipt, containment) = cleanup.into_parts();
                self.identities
                    .remove(&raw)
                    .map(|task| TaskOutcome::Completed {
                        task,
                        cleanup: Some(receipt),
                        #[cfg(feature = "process")]
                        containment: Some(containment),
                    })
            }
            Ok((_, TaskEnd::Cancelled)) => {
                self.cancelled = self.cancelled.saturating_add(1);
                self.identities
                    .remove(&raw)
                    .map(|task| TaskOutcome::Cancelled {
                        task,
                        cleanup: None,
                        #[cfg(feature = "process")]
                        containment: None,
                    })
            }
            #[cfg(feature = "process")]
            Ok((_, TaskEnd::CancelledWithCleanup { cleanup })) => {
                self.cancelled = self.cancelled.saturating_add(1);
                let (receipt, containment) = cleanup.into_parts();
                self.identities
                    .remove(&raw)
                    .map(|task| TaskOutcome::Cancelled {
                        task,
                        cleanup: Some(receipt),
                        #[cfg(feature = "process")]
                        containment: Some(containment),
                    })
            }
            // Gated with the variant and with the feature that produces it:
            // nothing else in this crate can report a non-zero process exit.
            #[cfg(feature = "process")]
            Ok((_, TaskEnd::Failed { status, cleanup })) => {
                let (receipt, containment) = cleanup.into_parts();
                // Counted as its own kind rather than folded into `succeeded`:
                // a process that exited 1 is work that did not do what it was
                // asked, and a counter that cannot see the difference reports a
                // failing bot as a healthy one. `completed` still counts it, so
                // the totals keep adding up.
                self.failed = self.failed.saturating_add(1);
                self.identities
                    .remove(&raw)
                    .map(|task| TaskOutcome::Failed {
                        task,
                        status,
                        cleanup: Some(receipt),
                        #[cfg(feature = "process")]
                        containment: Some(containment),
                    })
            }
            Err(error) if error.is_panic() => {
                self.panicked = self.panicked.saturating_add(1);
                let message = panic_message(error.into_panic());
                self.identities
                    .remove(&raw)
                    .map(|task| TaskOutcome::Panicked { task, message })
            }
            Err(_) => {
                self.aborted = self.aborted.saturating_add(1);
                self.identities
                    .remove(&raw)
                    .map(|task| TaskOutcome::Aborted { task })
            }
        };
        self.completed = self.completed.saturating_add(1);
        // A missing identity is unreachable — `place` inserts before the task
        // can finish — but it is handled rather than asserted. The counters
        // above are already exact, so what is lost here is attribution alone,
        // and counting that as a dropped report is the honest reading: the
        // outcome happened and was not retained.
        let Some(outcome) = outcome else {
            self.reports_dropped = self.reports_dropped.saturating_add(1);
            return;
        };
        match retention {
            Retention::Capped if self.reports.len() >= self.report_cap => {
                self.reports_dropped = self.reports_dropped.saturating_add(1);
            }
            Retention::Capped | Retention::Draining => self.reports.push_back(outcome),
        }
    }
}

/// What one task a [`Supervisor`] owns is currently doing, as the owner reads
/// it.
///
/// A sum type rather than a flag so a state the owner learns to tell apart can be
/// added without changing what `Running` means. Today the owner distinguishes
/// one: a placed task holds a permit until it is reaped, whether or not its body
/// has returned, so capacity read from this listing is capacity the supervisor
/// will actually grant. Terminal outcomes are not listed here; they are in the
/// report stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TaskState {
    /// Placed and holding a permit, not yet reaped.
    Running {
        /// The task's position among the live tasks, in spawn order.
        ///
        /// Not a permit number: permits are interchangeable and carry no
        /// identity. Always below the in-flight ceiling.
        position: usize,
    },
}

impl TaskState {
    /// Whether the task is still doing work.
    #[must_use]
    pub const fn is_running(self) -> bool {
        matches!(self, Self::Running { .. })
    }
}

/// A bounded, point-in-time view of one [`Supervisor`].
///
/// This is **not** a second ledger. Every counter here is read from the same
/// fields the supervisor's own admission and reporting paths use, at the
/// instant of the call, and the snapshot owns nothing: no background sampler, no
/// retained history, no queue. That is the whole point — an inspection surface
/// that maintained its own state could disagree with the owner, and a
/// disagreement between the thing that runs work and the thing that reports on it
/// is worse than no inspection at all.
///
/// # Bounds
///
/// [`Self::live`] is capped at [`Self::live_limit`], and [`Self::live_truncated`]
/// says when it was. The live set is exactly the in-flight ceiling, which is
/// itself bounded, so the cap is a defence against a caller that raised the
/// ceiling rather than a routine truncation.
///
/// Terminal outcomes are **not** in this snapshot. They are already retained,
/// already bounded by the report cap, and already drainable through
/// [`Supervisor::next_report`]; a snapshot that carried its own copy of them
/// would be the second source of truth this type refuses to be. A caller that
/// needs the authoritative terminal record reads the report stream.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SupervisorSnapshot {
    /// The declared in-flight ceiling, as the supervisor holds it.
    max_in_flight: usize,
    /// The supervisor's own counters at this instant.
    stats: Stats,
    /// Live tasks, at most `live_limit` of them, in [`TaskId`] order.
    live: Vec<LiveTask>,
    /// How many live tasks the cap excluded.
    live_truncated: usize,
    /// Whether the supervisor has stopped admitting.
    cancelled: bool,
    /// Terminal outcomes waiting to be drained through
    /// [`Supervisor::next_report`].
    reports_pending: usize,
    /// Cleanup obligations still charged to this supervisor's capacity.
    #[cfg(all(unix, feature = "process"))]
    pending_cleanups: usize,
}

impl SupervisorSnapshot {
    /// The declared in-flight ceiling this supervisor was built with.
    #[must_use]
    pub const fn max_in_flight(&self) -> usize {
        self.max_in_flight
    }

    /// The supervisor's own counters at the instant of the snapshot.
    #[must_use]
    pub const fn stats(&self) -> Stats {
        self.stats
    }

    /// The live tasks, in spawn order, at most [`Self::live_limit`] of them.
    ///
    /// A borrow rather than the vector itself: the listing is a snapshot of the
    /// supervisor's state, and handing out the `Vec` would hand out the right to
    /// edit what a reader believes it observed. Nothing a caller does to the
    /// returned slice can change what the supervisor does next, and that is the
    /// whole claim of this type.
    #[must_use]
    pub fn live(&self) -> &[LiveTask] {
        &self.live
    }

    /// How many live tasks the cap excluded from [`Self::live`].
    ///
    /// Non-zero means the listing is a bounded prefix, and a caller drawing
    /// conclusions about capacity from it must account for what it did not see.
    #[must_use]
    pub const fn live_truncated(&self) -> usize {
        self.live_truncated
    }

    /// Whether the supervisor has stopped admitting.
    #[must_use]
    pub const fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    /// Terminal outcomes waiting to be drained through
    /// [`Supervisor::next_report`].
    ///
    /// The authoritative record is that stream; this is how many are queued, so a
    /// caller can size its drain before starting it.
    #[must_use]
    pub const fn reports_pending(&self) -> usize {
        self.reports_pending
    }

    /// Cleanup obligations still charged to this supervisor's capacity.
    #[cfg(all(unix, feature = "process"))]
    #[must_use]
    pub const fn pending_cleanups(&self) -> usize {
        self.pending_cleanups
    }

    /// The largest number of live tasks this snapshot can carry.
    ///
    /// Equal to the supervisor's in-flight ceiling, so a caller can size a
    /// buffer for [`Self::live`] once instead of guessing.
    #[must_use]
    pub const fn live_limit(&self) -> usize {
        self.max_in_flight
    }

    /// Whether the live listing came back empty and nothing was truncated.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.live.is_empty() && self.live_truncated == 0
    }

    /// Permits currently taken, from the live set.
    ///
    /// Equal to the number of `Running` entries, which is why it is derived here
    /// rather than tracked separately: a second counter would be a second thing
    /// that can disagree with the first.
    #[must_use]
    pub fn occupied(&self) -> usize {
        self.live
            .iter()
            .filter(|task| task.state.is_running())
            .count()
    }

    /// Permits this supervisor has free right now.
    #[must_use]
    pub fn free(&self) -> usize {
        self.max_in_flight.saturating_sub(self.occupied())
    }

    /// The next action the supervisor can take, as one value.
    ///
    /// The decision an owner actually has to make, rather than the four counters
    /// it is derived from. "Admission closed" is the one answer a caller that was
    /// about to spawn must not have to infer from three places.
    #[must_use]
    pub fn next_action(&self) -> NextAction {
        if self.cancelled {
            NextAction::Stopped
        } else if self.free() > 0 {
            NextAction::Admit
        } else {
            NextAction::WaitForCapacity
        }
    }
}

