//! Admission: the synchronous entry, the owned reactor, the budget a run is
//! charged to, and the values a finished or declined run is assembled from.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, MutexGuard};
use std::time::Instant;

use crate::cap::{Cap, Deficit, Shortage, uncovered};
use crate::effect::RunId;
use crate::gate::GrantSet;
use crate::journal::owner::lock;
use crate::registry::DomainRegistry;
use crate::rt::runtime::{Handle, Runtime};
use crate::rt::sync::{OwnedSemaphorePermit, Semaphore};
use crate::script::run_store::AuthorityCheck;
use crate::script::{FlowError, Scope, TrailEntry};

use super::host::Installation;
use super::name::TaskName;
use super::{
    BODY_STEP, Disposition, EffectKnowledge, HELD_PERMITS, Host, HostError, RepairTicket, Report,
    Task, Ticket,
};

impl Host {
    /// Run `task` over `input` from synchronous code, driving it on a runtime
    /// this host owns.
    ///
    /// The host builds its reactor once and reuses it, so repeated calls do not
    /// pay a runtime per call. Inside an existing runtime this **refuses**
    /// rather than parking the calling thread: parking a thread that owns a
    /// reactor is a deadlock, not a slow call, so it returns
    /// [`HostError::InsideRuntime`] — the same verdict, and for the same
    /// reason, as the crate's synchronous tick. Await [`Host::run`] there
    /// instead.
    ///
    /// # Errors
    ///
    /// [`HostError::InsideRuntime`] when a runtime is already driving this
    /// thread; [`HostError::Runtime`] when the OS refuses the runtime's driver
    /// resources.
    pub fn block_on<I, O, F, Fut>(&self, task: &Task<F>, input: I) -> Result<Report<O>, HostError>
    where
        F: Fn(Scope, I) -> Fut,
        Fut: Future<Output = Result<O, FlowError>>,
    {
        // Checked before the reactor is taken, so the refusal cannot leave a
        // freshly built runtime behind for a caller that never used it.
        if crate::rt::runtime::inside_runtime() {
            let refusal = Err(HostError::InsideRuntime);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "block_on: returning an error to the caller");
            return refusal;
        }
        let handle = self.reactor()?;
        Ok(handle.block_on(self.run(task, input)))
    }

    /// This host's reactor, built on first use and kept for every later call.
    pub(super) fn reactor(&self) -> Result<Handle, HostError> {
        let mut slot = self.reactor_slot();
        if let Some(runtime) = slot.as_ref() {
            return Ok(runtime.handle());
        }
        let runtime = Runtime::new().map_err(|cause| HostError::Runtime { cause })?;
        let handle = runtime.handle();
        *slot = Some(runtime);
        Ok(handle)
    }

    /// The reactor slot's lock.
    ///
    /// Poisoning recovers rather than propagating: the slot holds a
    /// [`Runtime`], whose `Drop` is infallible, so no panic path can leave a
    /// half-installed reactor behind.
    pub(super) fn reactor_slot(&self) -> MutexGuard<'_, Option<Runtime>> {
        lock(&self.inner.reactor)
    }

    /// Wait for one of this host's permits, bounded by the run's deadline and
    /// cancellable by the host's stop.
    ///
    /// A nested run — one whose future already holds a permit for this host —
    /// acquires nothing and is charged to the permit its ancestor holds. That is
    /// the whole of the no-deadlock argument: a parent awaiting a child never
    /// holds the last permit while waiting, because it never waits for one.
    pub(super) async fn admit(
        &self,
        started: Instant,
        task: &TaskName,
    ) -> Result<Permit, AdmissionFailure> {
        // Both refusals here are distinct facts and both are reported as
        // `Refused`, never `Cancelled`: a run that never held a permit never ran
        // a body, so no step was entered and there is nothing that could have
        // been stopped mid-way. A `Cancelled` disposition means the run was
        // admitted and then stopped, which is a different thing to tell a
        // reader — and the reason this returns a [`Permit`] or a *timeout* is
        // that only a run which entered its body can be cancelled inside it.
        // Outside every scope a run holds nothing, so it is not nested.
        let nested = matches!(
            HELD_PERMITS.try_with(|held| held.contains(&self.inner.identity)),
            Ok(true)
        );
        if nested {
            return Ok(Permit::Charged);
        }
        if self.inner.token.is_cancelled() {
            let refusal = Err(AdmissionFailure::Refused(Refused {
                at: Arc::from(task.as_str()),
            }));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "admit: returning an error to the caller");
            return refusal;
        }
        // The wait for a permit is bounded in real time, charged to the run's
        // deadline. It is a wait on other runs releasing capacity — a real
        // event no logical clock can hurry — so the host's declared clock
        // governs the body's `within`, not this queue.
        let deadline = self.inner.limits.default_deadline();
        let remaining = deadline.saturating_sub(started.elapsed());
        let waiting = self
            .inner
            .token
            .run_until_cancelled(Semaphore::acquire_owned(Arc::clone(&self.inner.admission)));
        match crate::rt::time::timeout(remaining, waiting).await {
            Ok(Some(Ok(permit))) if !self.inner.token.is_cancelled() => Ok(Permit::Held(permit)),
            // A permit acquired after the stop is not an admission. The stop
            // ends the runs in flight, their permits go to the queued waiters in
            // release order, and a waiter polled before it observes the stop
            // finds a permit already assigned to it. Admitting it would count a
            // run that never ran as admitted, charge its root budget, and report
            // it `Cancelled` when its scope refuses entry. Which waiters get
            // there first is decided by the order the stop's wakes arrive in,
            // which the channel under the token does not fix, so the same
            // schedule could report the same run either way. Dropping the permit
            // hands it back and the run is `Refused`, whatever that order was.
            Ok(Some(Ok(_after_the_stop))) => Err(AdmissionFailure::Refused(Refused {
                at: Arc::from(task.as_str()),
            })),
            // The semaphore is closed only if a `Host` closed it, and it never
            // does. Typed rather than panicked because the invariant that makes
            // this unreachable is a claim, not a proof, and a budget that is
            // closed is a fact the caller needs rather than a panic.
            Ok(Some(Err(_closed))) => Err(AdmissionFailure::Failed(FlowError::failed(
                "the host's admission budget is closed",
            ))),
            Ok(None) => Err(AdmissionFailure::Refused(Refused {
                at: Arc::from(task.as_str()),
            })),
            Err(_elapsed) => Err(AdmissionFailure::DeadlineExceeded(FlowError::TimedOut {
                at: Arc::from(BODY_STEP),
                after: deadline,
            })),
        }
    }

    /// Build the report for a terminal state.
    ///
    /// The single place a [`Report`] is assembled, because the field that must
    /// agree with the disposition — an output exactly when it succeeded, an
    /// error exactly when it did not — is the field a hand-written literal at
    /// three call sites would get wrong, and a report that disagrees with itself
    /// is the one thing a caller cannot detect from the type.
    pub(super) fn report<O>(&self, terminal: Terminal<O>) -> Report<O> {
        let Terminal {
            started,
            task,
            disposition,
            output,
            error,
            trail,
            run,
            needs,
            repair,
        } = terminal;
        let (trail, dropped_steps) = match trail {
            TrailSnapshot::Taken((entries, dropped)) => (entries, dropped),
            TrailSnapshot::Empty => (Vec::new(), 0),
        };
        let steps = trail.iter().map(TrailEntry::shared_path).collect();
        // The two claims a report makes about durability, both derived from one
        // fact — whether this host holds a store — so they cannot disagree. A
        // report with a store says how many records it has; one without says
        // `None`, which is the honest "nothing survived" rather than a count of
        // zero records that reads like a run that did nothing.
        let effects = match (run, self.inner.store.as_ref()) {
            (Some(run), Some(store)) => EffectKnowledge::StepRecords {
                records: store.record_count(run),
            },
            _ => EffectKnowledge::None,
        };
        // A ticket names where a run stopped, so it exists for a stopped run
        // that has a store. A succeeded run has nothing to resume; a run with no
        // store has nothing to resume *from*. Both are absent, and a reader can
        // tell the two apart by `run_id`, which is the field that says whether
        // the run was durable at all.
        let ticket = run.and_then(|run| {
            if disposition.is_success() {
                return None;
            }
            Some(Ticket {
                run,
                tenant: self.inner.tenant.clone(),
                task: task.clone(),
                disposition,
                at: error
                    .as_ref()
                    .map(FlowError::at)
                    .filter(|path| !path.is_empty())
                    .map(Arc::from),
            })
        });
        Report {
            disposition,
            output,
            error,
            elapsed: started.elapsed(),
            steps,
            trail,
            dropped_steps,
            progress_capacity: self.inner.limits.progress_capacity(),
            effects,
            tenant: self.inner.tenant.clone(),
            task,
            run,
            ticket,
            needs,
            repair,
        }
    }

    /// Move the counters for a run that has just taken a permit, and hand back the
    /// guard that undoes them.
    ///
    /// The guard is what makes the counters exact. A run that finishes normally,
    /// a run whose body fails, a run that is cancelled and a run whose future
    /// the caller drops while suspended all release through it, so `in_flight`
    /// returns to zero without any path having to remember to decrement it.
    pub(super) fn charge(&self, permit: Permit) -> AdmissionCharge<'_> {
        let held = matches!(permit, Permit::Held(_));
        if held {
            let live = self.inner.in_flight.raise();
            self.inner.peak_in_flight.raise_to(live);
        }
        // Every run is counted, admitted or charged: this is the number of tasks
        // the host actually ran, which is not the number of permits it took.
        self.inner.admitted.fetch_add(1, Ordering::Relaxed);
        AdmissionCharge {
            installation: &self.inner,
            permit,
            held,
        }
    }
}

impl Default for LiveRuns {
    /// A count of zero.
    fn default() -> Self {
        Self(AtomicUsize::new(0))
    }
}

/// A high-water mark over an atomic counter.
///
/// The two operations a ceiling needs — raise to a new maximum, and lower by
/// one — are each written once here rather than at both call sites, so the
/// ordering and the "discard the previous value" shape cannot drift between the
/// path that takes a permit and the path that releases it.
pub(super) struct HighWater(AtomicUsize);

impl HighWater {
    /// Record that `live` tasks are in flight now.
    ///
    /// Relaxed throughout: these are counters read for reporting and for one
    /// assertion, not guards for anyone else's memory.
    pub(super) fn raise_to(&self, live: usize) {
        // Nothing closes an atomic, so the substitution always happens; the
        // previous value is bound and never read, which is how a returned value
        // is discarded without `let _ =` — the shape `let_underscore_must_use`
        // exists to refuse — or `drop` on a `Copy` value, which does nothing.
        let _previous = self
            .0
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |peak| {
                (live > peak).then_some(live)
            });
    }

    /// The most tasks that were ever in flight at once.
    pub(super) fn get(&self) -> usize {
        self.0.load(Ordering::Relaxed)
    }
}

/// One admitted run's claim on its host's budget.
///
/// Holds the permit a top-level run took and counts it down on drop. A nested
/// run holds nothing: it is charged to a permit its ancestor already holds, so
/// its drop has no permit to release and no count to undo.
pub(super) struct AdmissionCharge<'installation> {
    /// Where the counters live.
    pub(super) installation: &'installation Installation,
    /// The permit, for a run that took one.
    pub(super) permit: Permit,
    /// Whether `permit` is a real claim on the budget.
    pub(super) held: bool,
}

impl Drop for AdmissionCharge<'_> {
    /// Release the permit and undo the in-flight count.
    ///
    /// Infallible: releasing a permit and decrementing a counter cannot fail, and
    /// a panic in a destructor during an unwind would abort the process.
    fn drop(&mut self) {
        // Taking the permit out and dropping it is the release: an
        // `OwnedSemaphorePermit` returns its slot when it is dropped, and this
        // reads the field the type exists to hold, so nothing here is a
        // no-op that only looks like a release.
        if let Permit::Held(permit) = std::mem::replace(&mut self.permit, Permit::Charged) {
            drop(permit);
        }
        if self.held {
            // The count was raised before this guard was built and nothing else
            // lowers it, so the substitution always succeeds. It is written
            // through the same [`LiveRuns`] the raise path uses, so the two
            // cannot drift on ordering or on how the previous value is handled.
            self.installation.in_flight.release_one();
        }
    }
}

impl Default for HighWater {
    /// A mark of zero.
    fn default() -> Self {
        Self(AtomicUsize::new(0))
    }
}

/// The live count of top-level runs holding a permit.
///
/// One type for the raise and the release, because a count that is written in
/// two places with two orderings is a count whose two paths can disagree — and
/// the disagreement is invisible until the ceiling stops bounding anything.
pub(super) struct LiveRuns(AtomicUsize);

impl LiveRuns {
    /// Record that one more run holds a permit, and return the live count.
    pub(super) fn raise(&self) -> usize {
        self.0.fetch_add(1, Ordering::Relaxed).saturating_add(1)
    }

    /// Record that one fewer run holds a permit.
    pub(super) fn release_one(&self) {
        // `checked_sub` is what makes a double release a refusal rather than a
        // wrap to `usize::MAX`: the closure returns `None`, the fetch reports
        // it, and the counter is left alone for a reader to see. Nothing closes
        // an atomic, so the previous value is bound and never read rather than
        // discarded with `let _ =`, the shape `let_underscore_must_use` refuses.
        let _previous = self
            .0
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |live| {
                live.checked_sub(1)
            });
    }

    /// How many runs hold a permit right now.
    pub(super) fn get(&self) -> usize {
        self.0.load(Ordering::Relaxed)
    }
}

/// A run the host declined to admit, before any body ran.
///
/// Its own type rather than a `FlowError`, because the disposition it maps to
/// is not the one a located failure maps to: a run that never held a permit
/// never entered a step, so there is no step to be cancelled *at*. Reporting it
/// as `Cancelled` would tell a reader its body had started and then stopped.
#[derive(Debug)]
pub(super) struct Refused {
    /// The task it was refused for, which is the only location it has.
    pub(super) at: Arc<str>,
}

/// Why a run never reached its body, and the disposition each reason reports.
#[derive(Debug)]
pub(super) enum AdmissionFailure {
    /// The host was stopped, or stopped while this run waited.
    Refused(Refused),
    /// The budget could not be acquired at all.
    Failed(FlowError),
    /// The run's deadline passed while it waited for a permit.
    DeadlineExceeded(FlowError),
}

impl AdmissionFailure {
    /// The disposition this admission failure reports.
    pub(super) const fn disposition(&self) -> Disposition {
        match *self {
            Self::Refused(_) => Disposition::Refused,
            Self::DeadlineExceeded(_) => Disposition::DeadlineExceeded,
            Self::Failed(_) => Disposition::Failed,
        }
    }

    /// The located failure this admission failure reports.
    pub(super) fn into_error(self) -> FlowError {
        match self {
            Self::Refused(Refused { at }) => FlowError::Failed {
                at,
                reason: String::from("the host refused to admit this run"),
            },
            Self::Failed(error) | Self::DeadlineExceeded(error) => error,
        }
    }
}

/// The disposition a run's failure is reported as.
pub(super) fn disposition_of(error: &FlowError) -> Disposition {
    match *error {
        FlowError::Cancelled { .. } => Disposition::Cancelled,
        FlowError::TimedOut { .. } => Disposition::DeadlineExceeded,
        _ => Disposition::Failed,
    }
}

/// What a run ended with, assembled in one place.
///
/// Its own struct rather than eight parameters: the fields that must agree —
/// an output exactly when it succeeded, an error exactly when it did not — are
/// the ones a hand-written positional call gets wrong, and a report that
/// disagrees with itself is the one thing a caller cannot detect from the type.
pub(super) struct Terminal<O> {
    /// When the run started, for the elapsed measurement.
    pub(super) started: Instant,
    /// The task that ran.
    pub(super) task: TaskName,
    /// How it ended.
    pub(super) disposition: Disposition,
    /// Its output, when it succeeded.
    pub(super) output: Option<O>,
    /// Its located failure, when it did not.
    pub(super) error: Option<FlowError>,
    /// Its step trail.
    pub(super) trail: TrailSnapshot,
    /// The run id its records are keyed by.
    pub(super) run: Option<RunId>,
    /// The unmet capabilities this run was admitted short of.
    pub(super) needs: Option<Deficit>,
    /// The repair ticket a blocked run hands back.
    pub(super) repair: Option<RepairTicket>,
}

/// How a host declined a run before its body started.
///
/// The half of a [`Terminal`] that varies between the two reports describing a
/// declined run, gathered so `Host::not_admitted` takes one argument for the
/// refusal rather than five positional ones that could be silently transposed at
/// a call site.
pub(super) struct Declined {
    /// How the run ended.
    pub(super) disposition: Disposition,
    /// Why, located at the task because no step ran.
    pub(super) reason: String,
    /// The run id whose records are keyed by it.
    pub(super) run: Option<RunId>,
    /// The unmet capabilities, for a blocked run.
    pub(super) needs: Option<Deficit>,
    /// The repair ticket a blocked run hands back.
    pub(super) repair: Option<RepairTicket>,
}

impl Declined {
    /// A decline carrying no run identity and no unmet authority.
    pub(super) fn plain(disposition: Disposition, reason: String) -> Self {
        Self {
            disposition,
            reason,
            run: None,
            needs: None,
            repair: None,
        }
    }
}

/// The step paths a report carries.
///
/// A run that entered its task step has a ring to read and passes
/// `Taken`; a run refused before that has no ring at all and passes `Empty`. The
/// two are different facts rather than an `Option` over a snapshot: "no steps"
/// and "no trail to report" must not look the same in a report a reader is
/// reasoning from.
pub(super) enum TrailSnapshot {
    /// The ring's contents, and how many entries it dropped.
    Taken((Vec<TrailEntry>, usize)),
    /// No ring existed: the run was refused before its scope was built.
    Empty,
}

// ── Authority ───────────────────────────────────────────────────────────────

/// The authority a run's steps consult, erased for the task-local it rides in.
///
/// A handle rather than the concrete type for the same reason [`Records`] is: it
/// is installed as a `dyn` behind a task-local, and the script module must not
/// depend on [`Host`].
/// One run's authority: the host's base grant plus this run's repair delta.
///
/// The two are kept apart rather than merged, because merging loses the question
/// the repair door asks. A repair *adds* authority for one run and never widens
/// the host's, so a step's coverage is "either", and a reader of this type can see
/// which half answered.
pub(super) struct RunAuthority {
    /// What the host admits every run against.
    pub(super) base: GrantSet,
    /// What this run's repair authorized, limited to its ticket's needs.
    pub(super) delta: GrantSet,
    /// The registry the host was given, which a repair never changes: a repair
    /// widens what a run may reach, never which domains it can name.
    pub(super) domains: Option<&'static DomainRegistry>,
}

impl AuthorityCheck for RunAuthority {
    /// Every capability in `required` this run's authority does not cover.
    ///
    /// Checked against the base and the delta together, and named once each: a
    /// capability the base already covers is not reported as short however many
    /// times the delta repeats it, so the shortfall is what the run genuinely
    /// lacks rather than what its repair happened to mention.
    fn uncovered(&self, required: &[Cap]) -> Vec<Shortage> {
        uncovered(
            required,
            |cap| self.base.grants(cap) || self.delta.grants(cap),
            None,
        )
    }

    fn domains(&self) -> Option<&'static DomainRegistry> {
        self.domains
    }
}

/// What a run holds for the length of its body.
///
/// `Held` owns budget permit and releases it on drop; `Charged` holds
/// nothing, because a nested run shares its ancestor's permit. Both are dropped
/// on every exit path, so a suspended run the caller abandons returns its permit
/// and its in-flight count with it.
pub(super) enum Permit {
    /// This run took a permit out of the host's budget.
    Held(OwnedSemaphorePermit),
    /// This run is charged to a permit its future already holds.
    Charged,
}
