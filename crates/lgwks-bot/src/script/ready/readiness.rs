//! The operations of a readiness: signal, fail, shut down, and the budgeted wait. Split from `ready.rs` (#526).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::rt::sync::watch;
use crate::script::error::FlowError;
use crate::script::{Scope, within};

use super::state::{Readiness, Settled, State, Status};
use super::{
    FailAfterReady, Generation, MAX_DEPENDANTS, MAX_NAME_BYTES, ReadinessError, Ready, ReadyOutcome,
};

impl<T: Clone> Readiness<T> {
    /// A readiness for the service called `what`, at generation
    /// [`Generation::FIRST`](super::Generation::FIRST), admitting up to [`MAX_DEPENDANTS`] dependants.
    ///
    /// # Errors
    ///
    /// [`ReadinessError::InvalidName`](super::ReadinessError::InvalidName) when `what` is empty or longer than
    /// [`MAX_NAME_BYTES`].
    pub fn new(what: &str) -> Result<Self, ReadinessError> {
        Self::named(what, Generation::FIRST, MAX_DEPENDANTS)
    }

    /// A readiness for the service called `what`, at a named generation and a
    /// named dependant cap.
    ///
    /// The form a restart uses, and the form a saturation sweep uses: the
    /// generation is supplied rather than drawn, so a caller can arm a restarted
    /// instance strictly newer than the one it replaced, and the cap is the
    /// bound the dependants are charged against rather than a fixed maximum.
    ///
    /// # Errors
    ///
    /// [`ReadinessError::InvalidName`](super::ReadinessError::InvalidName) when `what` is empty or longer than
    /// [`MAX_NAME_BYTES`].
    pub fn named(what: &str, generation: Generation, cap: usize) -> Result<Self, ReadinessError> {
        if what.is_empty() {
            let refusal = Err(ReadinessError::InvalidName {
                reason: "the name is empty",
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "named: returning an error to the caller");
            return refusal;
        }
        if what.len() > MAX_NAME_BYTES {
            let refusal = Err(ReadinessError::InvalidName {
                reason: "the name is longer than MAX_NAME_BYTES",
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "named: returning an error to the caller");
            return refusal;
        }
        let (settled, _receiver) = watch::channel(Settled::Waiting);
        Ok(Self {
            inner: Arc::new(State {
                what: Arc::from(what),
                cap: cap.max(1),
                status: Mutex::new(Status::Pending(generation)),
                admitted: AtomicU64::new(0),
                released_once: AtomicBool::new(false),
                dependants: Mutex::new(Vec::new()),
                settled,
            }),
        })
    }

    /// What this readiness is called.
    #[must_use]
    pub fn what(&self) -> &str {
        &self.inner.what
    }

    /// The generation this readiness is bound to.
    ///
    /// The generation a signal must name to be accepted. Read rather than
    /// remembered by a caller, so a signal cannot be built against a generation
    /// this readiness has already moved past.
    #[must_use]
    pub fn generation(&self) -> Generation {
        self.lock().generation()
    }

    /// Whether this readiness has settled at all.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        !matches!(self.settled_value(), Settled::Waiting)
    }

    /// What this readiness settled on, or `None` while it is waiting.
    ///
    /// The reading a dependant takes *before* it decides to wait, so a caller
    /// can tell "already released" from "still starting" without a timer. It is
    /// a point-in-time reading: `None` is not a promise that nothing has
    /// happened, only that nothing had at the instant it was read — the wait
    /// below is still the authority.
    #[must_use]
    pub fn outcome(&self) -> Option<ReadyOutcome<T>> {
        match self.settled_value() {
            Settled::Waiting => None,
            Settled::Ready { generation, value } => Some(ReadyOutcome::Ready { generation, value }),
            Settled::Failed { generation, reason } => {
                Some(ReadyOutcome::Failed { generation, reason })
            }
            Settled::Closed { generation } => Some(ReadyOutcome::Closed { generation }),
        }
    }

    /// How many dependants have been admitted so far.
    #[must_use]
    pub fn admitted(&self) -> u64 {
        self.inner.admitted.load(Ordering::SeqCst)
    }

    /// How many dependants this readiness admits.
    #[must_use]
    pub fn cap(&self) -> usize {
        self.inner.cap
    }

    /// How many of this readiness's dependants have been admitted and have not
    /// yet been cancelled by a failure after the release.
    ///
    /// The retained-token list's own length. A dependant that is released and
    /// still running holds a token here, because only a later failure can tell
    /// it to stop; a dependant cancelled by that failure is removed when it
    /// settles. A caller reading this after a failure sees how many were still
    /// reachable, which is the "did it reach the dependants still running"
    /// evidence the acceptance row asks for.
    #[must_use]
    pub fn dependants_live(&self) -> usize {
        crate::journal::owner::lock(&self.inner.dependants).len()
    }

    /// Signal that the instance at `generation` is ready, carrying `value`.
    ///
    /// Releases every dependant already admitted and every one admitted after.
    /// A second signal is refused rather than releasing twice: see
    /// [`ReadinessError::AlreadyReady`](super::ReadinessError::AlreadyReady).
    ///
    /// # Errors
    ///
    /// [`ReadinessError::UnknownGeneration`](super::ReadinessError::UnknownGeneration) when `generation` is not the one
    /// this readiness is bound to; [`ReadinessError::StaleGeneration`](super::ReadinessError::StaleGeneration) when it
    /// is older than the generation this readiness has settled at. A readiness
    /// that has already released refuses any further signal with
    /// [`ReadinessError::AlreadyReady`](super::ReadinessError::AlreadyReady).
    /// A readiness that has already failed refuses a signal at any generation
    /// with [`ReadinessError::AlreadyFailed`](super::ReadinessError::AlreadyFailed),
    /// and one that was shut down refuses it as a generation mismatch.
    pub fn ready(&self, generation: Generation, value: T) -> Result<(), ReadinessError> {
        let mut status = self.lock();
        let current = status.generation();
        // A read-only match against a copy of the generation, so the write below
        // is not happening inside a `&mut` borrow of the status it replaces.
        // The lock is held across the read and the write, so the check and the
        // transition are one decision.
        if current == generation && matches!(&*status, Status::Pending(_)) {
            *status = Status::Released(generation);
            // Published before the value, so a dependant that observes the
            // flag can never read a release that has not been written yet.
            self.inner.released_once.store(true, Ordering::SeqCst);
            drop(status);
            self.inner.settled.send_replace(Settled::Ready {
                generation,
                value: Arc::new(value),
            });
            return Ok(());
        }
        match *status {
            // A second signal at the generation this readiness is bound to is
            // the *duplicate* case, and it is the only one: a signal from any
            // other generation is a mismatch, not a duplicate. Distinguishing
            // them here rather than in `mismatch` is what lets a caller tell
            // "this readiness already released" from "that signal was stale" —
            // two different repairs, and both common after a restart.
            Status::Released(released) if released == generation => {
                Err(ReadinessError::AlreadyReady {
                    what: Arc::clone(&self.inner.what),
                    generation: released,
                })
            }
            Status::Pending(_) => Err(self.mismatch(current, generation, false)),
            Status::Released(_) => Err(self.mismatch(current, generation, false)),
            Status::Closed(_) => Err(self.mismatch(current, generation, true)),
            Status::Failed {
                generation: failed, ..
            } => Err(ReadinessError::AlreadyFailed {
                what: Arc::clone(&self.inner.what),
                generation: failed,
            }),
        }
    }

    /// Signal that the instance at `generation` failed, with `reason`.
    ///
    /// A failure **before** the release settles the readiness as
    /// a failure and every dependant waiting on it observes it through
    /// [`Readiness::wait`](super::Readiness::wait). A failure **after** the release also
    /// cancels every dependant that was admitted, which is what makes a service
    /// that dies mid-flight reach the dependants still running instead of leaving
    /// them talking to a dead process — see
    /// [`Readiness::fail`](super::Readiness::fail) and the module docs.
    ///
    /// Idempotent in the same way [`Readiness::ready`](super::Readiness::ready) is: a second failure for
    /// the same generation is [`ReadinessError::AlreadyFailed`](super::ReadinessError::AlreadyFailed) and cancels
    /// nothing further.
    ///
    /// # Errors
    ///
    /// As [`Readiness::ready`](super::Readiness::ready) on generation mismatch; a released generation may
    /// still be failed, because that failure is precisely the failure after the
    /// release.
    pub fn fail(&self, generation: Generation, reason: &str) -> Result<(), ReadinessError> {
        let mut status = self.lock();
        let current = status.generation();
        // Read before the generation is compared, because the comparison's
        // refusal depends on which arm this readiness is in.
        let closed = matches!(&*status, Status::Closed(_));
        let failed = matches!(&*status, Status::Failed { .. });
        if current != generation {
            let refusal = Err(self.mismatch(current, generation, closed));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "fail: returning an error to the caller");
            return refusal;
        }
        if failed {
            let refusal = Err(ReadinessError::AlreadyFailed {
                what: Arc::clone(&self.inner.what),
                generation: current,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "fail: returning an error to the caller");
            return refusal;
        }
        let reason: Arc<str> = Arc::from(reason);
        let released = matches!(*status, Status::Released(_));
        *status = Status::Failed {
            generation,
            reason: Arc::clone(&reason),
        };
        drop(status);
        if released {
            // The dependants were released and are running; this failure has to
            // reach them, and the only channel a `Readiness` owns for a released
            // dependant is the token it handed over at admission.
            self.cancel_dependants();
        }
        self.inner
            .settled
            .send_replace(Settled::Failed { generation, reason });
        Ok(())
    }

    /// Shut this readiness down at `generation`, releasing nothing.
    ///
    /// The distinct fact from [`Readiness::fail`](super::Readiness::fail): a shut-down readiness was
    /// never usable, so a dependant waiting on one is *stopped* rather than told
    /// a service failed. A service being torn down is not a service failure, and
    /// collapsing the two would make a routine restart look like an outage. A
    /// dependant that was already released is cancelled too, for the same reason
    /// a failure after the release cancels it: it was pointed at something that
    /// is gone.
    ///
    /// # Errors
    ///
    /// As [`Readiness::fail`](super::Readiness::fail), for the same reason: a signal about a generation
    /// this readiness has moved past cannot be applied to anything.
    pub fn shutdown(&self, generation: Generation) -> Result<(), ReadinessError> {
        let mut status = self.lock();
        let current = status.generation();
        let closed = matches!(&*status, Status::Closed(_));
        if current != generation {
            let refusal = Err(self.mismatch(current, generation, closed));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "shutdown: returning an error to the caller");
            return refusal;
        }
        if matches!(*status, Status::Failed { .. }) {
            let refusal = Err(ReadinessError::AlreadyFailed {
                what: Arc::clone(&self.inner.what),
                generation: current,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "shutdown: returning an error to the caller");
            return refusal;
        }
        *status = Status::Closed(generation);
        drop(status);
        self.cancel_dependants();
        self.inner
            .settled
            .send_replace(Settled::Closed { generation });
        Ok(())
    }

    /// Whether `generation` is older than the generation this readiness has
    /// settled at.
    ///
    /// The question a restarted instance asks before it re-signals: "am I an
    /// older instance talking?" `false` for a readiness that has not settled and
    /// for the generation it is bound to.
    #[must_use]
    pub fn stale(&self, generation: Generation) -> bool {
        generation < self.lock().generation()
    }

    /// Whether this readiness was released, and at which generation.
    #[must_use]
    pub fn released_at(&self) -> Option<Generation> {
        let status = self.lock();
        match *status {
            Status::Released(generation) => Some(generation),
            Status::Pending(_) | Status::Failed { .. } | Status::Closed(_) => None,
        }
    }

    /// Whether this readiness failed at `generation`, as a failure after the
    /// release would have recorded it.
    ///
    /// The fact a caller needs to distinguish "failed before anyone was released"
    /// from "released and then died": the first owes nobody a cancellation,
    /// because there was nobody to cancel.
    #[must_use]
    pub fn failed_at(&self) -> Option<FailAfterReady> {
        let status = self.lock();
        match *status {
            Status::Failed {
                generation,
                ref reason,
            } if self.released_before_failure() => Some(FailAfterReady {
                generation,
                reason: Arc::clone(reason),
            }),
            Status::Pending(_)
            | Status::Released(_)
            | Status::Closed(_)
            | Status::Failed { .. } => None,
        }
    }

    /// Whether this readiness ever released before it failed.
    ///
    /// A separate flag rather than an extra `Status` arm, because "failed" and
    /// "failed *after* being released" are the same status with a different
    /// history, and modelling the history as a third status would make every
    /// read of the current state branch on something that happened in the past.
    /// The flag is set inside the same critical section that writes the status,
    /// so the two cannot disagree.
    #[must_use]
    fn released_before_failure(&self) -> bool {
        self.inner.released_once.load(Ordering::SeqCst)
    }

    /// Await this readiness from `scope` under a `limit` budget, without
    /// polling it.
    ///
    /// The whole wait: one admission, one subscription, and a `select!` against
    /// the scope's stop and the `limit` budget. There is no sleep and no loop,
    /// so a dependant released immediately costs one poll, and a dependant that
    /// waits costs nothing per unit of waiting.
    ///
    /// The dependant is admitted **before** the wait, so a failure landing while
    /// this call is suspended reaches it. Admission is charged before a slot is
    /// taken, so a refused admission leaves this readiness's capacity exactly as
    /// it was.
    ///
    /// The `limit` is measured on the scope's declared
    /// [`Clock`](crate::script::Clock), so a scope built on a caller-advanceable clock
    /// reports the same [`FlowError::TimedOut`] an advance past the budget
    /// produces that a real overrun produces.
    ///
    /// # Errors
    ///
    /// [`ReadinessError::DependantsFull`](super::ReadinessError::DependantsFull) at the declared cap; the readiness's
    /// own failure through [`ReadinessError::NotReady`](super::ReadinessError::NotReady);
    /// [`FlowError::Cancelled`] when `scope` is stopped or when the readiness
    /// was shut down, since a deliberate teardown is a stop and not a failure;
    /// [`FlowError::TimedOut`] when `limit` passes. The last two are the scope's,
    /// not the readiness's, so an enclosing [`retry`](crate::script::retry) does
    /// not retry a cancelled dependant.
    ///
    /// A `limit` under one millisecond is raised to one millisecond.
    ///
    /// # Panics
    ///
    /// The returned future panics when it has to wait outside a runtime with the
    /// `time` driver, as [`within`] does.
    pub async fn wait(&self, scope: &Scope, limit: Duration) -> Result<Ready<T>, FlowError> {
        self.admit(scope)?;
        let settled = self.settled_value();
        let finished = match settled {
            Settled::Waiting => None,
            other => Some(other),
        };
        if let Some(settled) = finished {
            return self.resolve(settled, scope);
        }
        let limit = limit.max(Duration::from_millis(1));
        within(scope, "await-ready", limit, self.subscribe(scope)).await
    }

    /// The blocking half of [`Readiness::wait`](super::Readiness::wait), already charged a slot.
    ///
    /// Subscribes *before* the settled reading is re-taken, so there is no
    /// instant at which a signal could land unobserved: the `watch` value is the
    /// current fact whether the write happened before or after the subscribe.
    async fn subscribe(&self, scope: &Scope) -> Result<Ready<T>, FlowError> {
        let mut settled = self.inner.settled.subscribe();
        loop {
            // The reading is taken from the channel rather than from the status
            // lock, so two dependants admitted at different instants cannot read
            // different facts about the same readiness: the channel is the one
            // place the value is published, and every reader reads it.
            let current = settled.borrow_and_update().clone();
            match current {
                Settled::Waiting => {}
                settled_now => return self.resolve(settled_now, scope),
            }
            let changed = watch::Receiver::changed(&mut settled);
            let cancelled = scope.token().cancelled_owned();
            lgwks_deps::tokio::select! {
                biased;
                _ = changed => {}
                () = cancelled => {
                    return Err(FlowError::Cancelled {
                        at: scope.join("await-ready"),
                    });
                }
            }
        }
    }

    /// Turn one settled value into a [`Ready`](super::Ready) or a located refusal.
    ///
    /// Located at `await-ready`, the same path [`within`] uses for this wait's
    /// own timeout and cancellation. A readiness refusal and a budget overrun
    /// are two different facts about the *same* wait, so they are located at the
    /// same step: a reader who sees `use-db/await-ready` learns which wait to
    /// look at without having to know whether the service failed or the clock
    /// ran out.
    fn resolve(&self, settled: Settled<T>, scope: &Scope) -> Result<Ready<T>, FlowError> {
        let at = scope.join("await-ready");
        match settled {
            Settled::Waiting => Err(FlowError::failed(format!(
                "{} settled between a dependant's admission and its first read",
                self.inner.what
            ))),
            Settled::Ready { generation, value } => Ok(Ready { generation, value }),
            Settled::Failed { generation, reason } => Err(ReadinessError::NotReady {
                what: Arc::clone(&self.inner.what),
                generation,
                reason,
            }
            .located(at)),
            // A shut-down readiness is a **stop**, not a failure, so it is
            // reported as one. The typed refusal is what the *signalling* side
            // gets — "you signalled a readiness that was torn down" is a refusal
            // of that signal — while the *waiting* side gets
            // [`FlowError::Cancelled`], because a dependant whose service was
            // deliberately torn down was not failed by it. Reporting `Failed`
            // here would make a routine restart look like an outage in every
            // report that saw it, and would invite a `retry` that re-runs a
            // dependant against a service the operator asked to go away.
            Settled::Closed { generation } => Err(FlowError::Cancelled {
                at: Arc::from(format!(
                    "{}: {} was shut down ({generation})",
                    scope.join("await-ready"),
                    self.inner.what
                )),
            }),
        }
    }

    /// Register `scope`'s token as a dependant, charging a slot first.
    ///
    /// The charge is a `fetch_update` that refuses to exceed the cap, so the
    /// counter is the cap's own enforcement rather than a check that races
    /// another admission. The token is pushed only after the charge succeeds, so
    /// the retained list can never exceed the cap — the two are one decision.
    fn admit(&self, scope: &Scope) -> Result<(), FlowError> {
        let cap = self.inner.cap;
        // The comparison happens in the cap's own width rather than widening the
        // cap into the counter's: a cap this host could name but the counter
        // could not would then be enforced as "admit everything", and a caller who
        // asked for a bound would get none. Narrowing the *count* instead means a
        // count past what this host can name refuses, which is the fail-closed
        // direction, and it is inside the charge rather than beside it so the
        // counter's own update is what refuses and two admissions cannot both pass
        // a check that raced.
        let charged = self
            .inner
            .admitted
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
                let within_cap = match usize::try_from(count) {
                    Ok(admitted) => admitted < cap,
                    Err(_wider_than_this_host_counts) => false,
                };
                within_cap.then_some(count.saturating_add(1))
            });
        if charged.is_err() {
            let refusal = Err(ReadinessError::DependantsFull {
                what: Arc::clone(&self.inner.what),
                cap,
            }
            .located(Arc::from(scope.path())));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "admit: returning an error to the caller");
            return refusal;
        }
        crate::journal::owner::lock(&self.inner.dependants).push(scope.token().clone());
        Ok(())
    }

    /// Cancel every admitted dependant, through the tokens they handed over.
    ///
    /// The tokens are *taken* rather than iterated, so a token that is
    /// re-admitted while a cancellation is in progress cannot be cancelled twice
    /// by the same failure — the second failure finds an empty list. No `.await`
    /// happens under the lock: taking the list and cancelling happen in two
    /// separate steps, which is the invariant this crate states everywhere else.
    fn cancel_dependants(&self) -> usize {
        let mut retained = crate::journal::owner::lock(&self.inner.dependants);
        let tokens = std::mem::take(&mut *retained);
        drop(retained);
        let reached = tokens.len();
        for token in tokens {
            token.cancel();
        }
        reached
    }

    /// The settled value, read through a fresh subscription.
    fn settled_value(&self) -> Settled<T> {
        self.inner.settled.subscribe().borrow_and_update().clone()
    }

    /// Take the status lock.
    fn lock(&self) -> MutexGuard<'_, Status> {
        crate::journal::owner::lock(&self.inner.status)
    }

    /// The refusal for a signal naming `got` while the readiness stands at
    /// `current`.
    ///
    /// Three arms, decided by two facts rather than by a timestamp or a registry
    /// of issued generations: a readiness that is shut down refuses everything,
    /// a signal from a generation *before* the current one is a stale instance,
    /// and anything else is a generation this readiness never issued.
    fn mismatch(&self, current: Generation, got: Generation, closed: bool) -> ReadinessError {
        let what = Arc::clone(&self.inner.what);
        if closed {
            ReadinessError::Closed {
                what,
                generation: current,
            }
        } else if got < current {
            ReadinessError::StaleGeneration {
                what,
                expected: current,
                got,
            }
        } else {
            ReadinessError::UnknownGeneration { what, got }
        }
    }
}
