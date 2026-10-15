use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll, Waker};
use std::time::Instant;

use crate::rt::sync::{OwnedSemaphorePermit, Semaphore};

use super::lease::Lease;
use crate::rt::tenancy::{Arrival, DeficitRoundRobin, Grant, GrantOutcome, TryArrival};
use crate::script::Tenant;

/// The round scheduler's lock, held for one decision.
type RoundGuard<'shell> = std::sync::MutexGuard<'shell, DeficitRoundRobin<Arc<WaitSlot>>>;

/// The permit pool and the round scheduler, with the two directions a
/// permit travels: out of a completed task into the next eligible tenant's
/// waiter.
pub(super) struct TenancyShell {
    /// The supervisor's own pool. The tenancy layer never makes a permit; it
    /// decides which tenant an existing one belongs to.
    permits: Arc<Semaphore>,
    /// The pure round scheduler, and through it every parked waiter.
    round: std::sync::Mutex<DeficitRoundRobin<Arc<WaitSlot>>>,
    /// Cumulative wall time spent inside [`TenancyShell::admit`] and
    /// [`TenancyShell::try_admit`], in nanoseconds: the scheduler lock plus
    /// the round and the pump, which is #351's admission side of a
    /// neighbour's admission wait. Releases take the same lock from the
    /// completing task and are counted separately, so a submitter's own
    /// admission time reads without the completions landing beside it.
    admit_nanos: AtomicU64,
    /// How many arrival decisions `admit_nanos` covers.
    admit_ops: AtomicU64,
    /// Cumulative wall time spent inside [`TenancyShell::release`], in
    /// nanoseconds: the same lock and round, taken from the completing
    /// task rather than the submitter.
    release_nanos: AtomicU64,
    /// How many releases `release_nanos` covers.
    release_ops: AtomicU64,
}

/// One parked admission: where a waiter learns that a permit is coming.
///
/// The state is behind its own mutex rather than the shell's, because the
/// lock order is shell then slot: a permit is handed to a slot while the
/// scheduler is held, and a waiter that goes away touches its slot alone
/// and then, separately, the scheduler. Nested in the other order the two
/// would deadlock.
struct WaitSlot {
    /// The permit, the abandonment mark, the handover mark and the waker.
    state: std::sync::Mutex<SlotState>,
}

/// The fields behind a [`WaitSlot`].
struct SlotState {
    /// The permit, once a grant has arrived.
    permit: Option<OwnedSemaphorePermit>,
    /// The owner went away before a permit arrived.
    abandoned: bool,
    /// A permit was handed over, so this slot is spent.
    handed: bool,
    /// The round took this waiter off its queue to deliver to it and found
    /// the owner gone. The waiter is no longer in any queue, so the owner's
    /// own report must not count it as an abandoned waiter there.
    withdrawn: bool,
    /// The task waiting for the handover.
    waker: Option<Waker>,
}

impl WaitSlot {
    /// A slot nobody is waiting on yet.
    fn fresh() -> Arc<Self> {
        Arc::new(Self {
            state: std::sync::Mutex::new(SlotState {
                permit: None,
                abandoned: false,
                handed: false,
                withdrawn: false,
                waker: None,
            }),
        })
    }

    /// Take the lock. A poisoned lock still guards a consistent value: each
    /// critical section is a few plain writes that cannot be left half done,
    /// so a panic elsewhere is not a reason to strand a waiter.
    fn lock(&self) -> std::sync::MutexGuard<'_, SlotState> {
        crate::journal::owner::lock(&self.state)
    }

    /// Whether a grant may still be handed to this slot.
    ///
    /// The round scheduler asks this of every queue head it reaches and
    /// abandons the ones that answer no, so a waiter whose caller has gone
    /// costs one skip rather than a blocked pool.
    fn is_live(&self) -> bool {
        let state = self.lock();
        !state.abandoned && !state.handed
    }

    /// Hand `permit` to the waiter, or refuse it and return the permit.
    ///
    /// `Some(permit)` means the owner left between the round's liveness
    /// check and this handoff; the caller recycles the permit rather than
    /// dropping it, because a permit is capacity and capacity is what the
    /// next eligible tenant is waiting for. The slot is marked withdrawn in
    /// the same critical section, so the owner's report, which reaches the
    /// round only after the caller's pump releases it, knows the round
    /// already took this waiter off its queue.
    fn deliver(&self, permit: OwnedSemaphorePermit) -> Option<OwnedSemaphorePermit> {
        let mut state = self.lock();
        if state.abandoned {
            state.withdrawn = true;
            return Some(permit);
        }
        if state.handed {
            return Some(permit);
        }
        state.permit = Some(permit);
        state.handed = true;
        // The waker is taken rather than cloned and kept: it is woken once,
        // and a stale waker retained for a second wake would wake a task
        // that has already left.
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
        None
    }

    /// Take the permit, if one has arrived.
    fn take_permit(&self) -> Option<OwnedSemaphorePermit> {
        self.lock().permit.take()
    }

    /// The slot half of an owner's departure: collect a handed permit, or
    /// mark the slot abandoned so no grant can deliver to it.
    ///
    /// Taken under the slot lock alone. The round half,
    /// [`TenancyShell::report_abandoned`], runs after this lock is released,
    /// so the two critical sections nest only in the order the shell uses —
    /// and a pump that holds the round lock can find this slot abandoned
    /// between its liveness check and its delivery.
    fn leave(&self) -> Departure {
        let mut state = self.lock();
        if state.handed {
            return Departure::Handed(state.permit.take());
        }
        state.abandoned = true;
        Departure::Abandoned
    }

    /// Record the waiting task, keeping the one already registered.
    fn register(&self, waker: &Waker) {
        let mut state = self.lock();
        let stale = state
            .waker
            .as_ref()
            .is_none_or(|current| !current.will_wake(waker));
        if stale {
            state.waker = Some(waker.clone());
        }
    }
}

/// What an owner leaving its slot found there.
enum Departure {
    /// A permit had been handed over; it is returned here unless the owner
    /// already collected it.
    Handed(Option<OwnedSemaphorePermit>),
    /// No permit had arrived. The slot is now abandoned, and the round has
    /// still to hear about it.
    Abandoned,
}

/// The future a parked admission awaits: the permit the round will hand it.
///
/// The only way a queued waiter learns of its grant is being woken by
/// [`WaitSlot::deliver`], which runs on whichever task released the permit —
/// so a grant is delivered by the completion hook rather than polled for.
/// The caller races this against its own cancellation token, which wakes on
/// the token in turn; neither direction needs an interval.
pub(super) struct WaitPermit {
    /// Where the grant is delivered.
    slot: Arc<WaitSlot>,
    /// The shell, for the release that a collected permit or an abandoned
    /// waiter owes.
    shell: Arc<TenancyShell>,
    /// The tenant this waiter belongs to.
    tenant: Tenant,
}

impl WaitPermit {
    /// A waiter for `tenant`, parked on `slot`.
    fn new(slot: Arc<WaitSlot>, shell: Arc<TenancyShell>, tenant: Tenant) -> Self {
        Self {
            slot,
            shell,
            tenant,
        }
    }
}

impl Future for WaitPermit {
    /// The permit, handed over by the round.
    type Output = OwnedSemaphorePermit;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        // Register before reading, never after: the round may hand the
        // permit over between the read and the registration, and a lost
        // wakeup here parks a waiter that has already been granted.
        this.slot.register(context.waker());
        match this.slot.take_permit() {
            Some(permit) => Poll::Ready(permit),
            None => Poll::Pending,
        }
    }
}

impl Drop for WaitPermit {
    /// Give back whatever this waiter was owed, or record that it is owed
    /// nothing.
    ///
    /// Two cases, and both owe the pool an exact answer. A waiter dropped
    /// *after* a permit was handed to it but before it was taken back
    /// returns the permit through the release path — the same path a
    /// completed task takes, because a grant nobody collects is capacity
    /// nobody else may reach. A waiter dropped with no permit is marked
    /// abandoned, which stops it counting against its tenant's queue bound
    /// and lets the next round skip it.
    ///
    /// The slot lock is released before the scheduler lock is taken, so the
    /// two critical sections nest only in the order the shell uses.
    fn drop(&mut self) {
        match self.slot.leave() {
            Departure::Handed(Some(permit)) => self.shell.release(&self.tenant, permit),
            Departure::Handed(None) => {}
            Departure::Abandoned => self.shell.report_abandoned(&self.slot, &self.tenant),
        }
    }
}

/// What one admission attempt decided.
pub(super) enum Admission {
    /// Admitted at once; the caller owns the lease.
    Admitted(Lease),
    /// Parked; the caller owns the waiter and must await it.
    Queued(WaitPermit),
    /// Not admitted: the tenant's bounded queue is at its bound, or nothing
    /// could take a permit right now. `limit` is the tenant's declared queue
    /// bound in both cases, so the caller can name it.
    Refused {
        /// The bound the tenant's queue reached.
        limit: usize,
    },
    /// Not admitted: the supervisor's own waiting bound is reached across
    /// every tenant, so the tenant may still have room and is refused
    /// anyway. The load is the supervisor's, not this tenant's.
    SupervisorQueueFull {
        /// The bound the supervisor's waiting total reached.
        limit: usize,
    },
}

impl TenancyShell {
    /// A shell over `permits`, enforcing `policy`.
    pub(super) fn new(permits: Arc<Semaphore>, policy: super::TenancyPolicy) -> Self {
        Self {
            permits,
            round: std::sync::Mutex::new(DeficitRoundRobin::new(policy)),
            admit_nanos: AtomicU64::new(0),
            admit_ops: AtomicU64::new(0),
            release_nanos: AtomicU64::new(0),
            release_ops: AtomicU64::new(0),
        }
    }

    /// Run one decision under the round lock, charging its own work to
    /// `ops` and `nanos` and not the wait to take the lock.
    ///
    /// `decide` runs with the lock held and is charged, through the unlock.
    /// The wait to take the lock is not: it is another decision holding the
    /// round — a completing task's release, charged to that release's own
    /// counter — and on an oversubscribed host it is mostly the time that
    /// holder spent descheduled. Charging it here made one host stall read
    /// as a 114x arrival cost (main run 37933518033), which is the
    /// scheduling share INV-BOT-151 does not claim to isolate.
    ///
    /// The two reads of the clock cost tens of nanoseconds and are
    /// symmetric across tenants, so a comparison of two arms' counters
    /// measures the decisions rather than the clock.
    fn timed<T>(
        &self,
        ops: &AtomicU64,
        nanos: &AtomicU64,
        decide: impl FnOnce(&mut RoundGuard<'_>) -> T,
    ) -> T {
        let mut round = self.lock();
        let held = Instant::now();
        let outcome = decide(&mut round);
        drop(round);
        let charged = held.elapsed();
        // A saturating read: the only input that does not fit is a decision
        // longer than 584 years, which pins the counter rather than wrapping
        // it, the same way the crate's counters saturate rather than wrap.
        let mut elapsed = u64::MAX;
        if let Ok(fits) = u64::try_from(charged.as_nanos()) {
            elapsed = fits;
        }
        ops.fetch_add(1, Ordering::Relaxed);
        nanos.fetch_add(elapsed, Ordering::Relaxed);
        outcome
    }

    /// Take the lock. A poisoned lock still guards a consistent value: every
    /// critical section is a handful of plain writes over queues and
    /// counters, so a panic elsewhere is not a reason to strand permits.
    fn lock(&self) -> RoundGuard<'_> {
        crate::journal::owner::lock(&self.round)
    }

    /// One permit from the pool, if the pool has one free.
    fn take_from_pool(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.permits).try_acquire_owned().ok()
    }

    /// A lease charged to `tenant`, so its drop returns the permit to the
    /// round rather than to the pool.
    fn lease(self: &Arc<Self>, permit: OwnedSemaphorePermit, tenant: &Tenant) -> Lease {
        Lease::tenanted(permit, Arc::clone(self), tenant.clone())
    }

    /// Admit one arrival for `tenant`, park it, or refuse it.
    ///
    /// Every step that could change the answer happens under the scheduler
    /// lock, so an arrival cannot observe a half-completed round. The
    /// receiver is `self: &Arc<Self>` because an admitted lease has to keep
    /// the shell alive for as long as it holds the permit.
    pub(super) fn admit(self: &Arc<Self>, tenant: &Tenant) -> Admission {
        self.timed(&self.admit_ops, &self.admit_nanos, |round| {
            // The slot is built only if the arrival parks: an arrival
            // admitted at once or refused allocates nothing (#375).
            let mut parked = None;
            let park = || {
                let slot = WaitSlot::fresh();
                parked = Some(Arc::clone(&slot));
                slot
            };
            let outcome = round.arrive_with(tenant, park, || self.take_from_pool());
            match (outcome, parked) {
                (Arrival::Immediate(permit), _) => Admission::Admitted(self.lease(permit, tenant)),
                (Arrival::Queued, Some(slot)) => {
                    // An arrival that could not take a permit may still be
                    // servable right now — the ring may have drained between its
                    // check and this one — so the round is pumped before the
                    // caller parks. A pump that grants to this very waiter wakes
                    // it, and the caller's poll collects the permit.
                    self.pump(round, None);
                    Admission::Queued(WaitPermit::new(slot, Arc::clone(self), tenant.clone()))
                }
                // `arrive_with` queues only by calling `park`, so a queued
                // arrival always has its slot. Answered rather than
                // asserted: with no slot nothing was queued either, and the
                // caller is told the queue is full instead of parking on a
                // waiter no grant can reach.
                (Arrival::Queued, None) => Admission::Refused {
                    limit: round.policy().queue_per_tenant(),
                },
                (Arrival::Refused { limit }, _) => Admission::Refused { limit },
                (Arrival::SupervisorFull { limit }, _) => Admission::SupervisorQueueFull { limit },
            }
        })
    }

    /// Admit one arrival for `tenant` that refuses rather than waits.
    ///
    /// The non-blocking door: no tenant is added to any queue, so a caller
    /// that keeps hammering a full supervisor accumulates no waiters. The
    /// refusal path has to cost nothing as well as say so.
    pub(super) fn try_admit(self: &Arc<Self>, tenant: &Tenant) -> Admission {
        self.timed(&self.admit_ops, &self.admit_nanos, |round| {
            let outcome = round.try_arrive(tenant, || self.take_from_pool());
            match outcome {
                TryArrival::Immediate(permit) => Admission::Admitted(self.lease(permit, tenant)),
                TryArrival::Contended => Admission::Refused {
                    limit: round.policy().queue_per_tenant(),
                },
            }
        })
    }

    /// One of this supervisor's in-flight admissions ended, carrying its
    /// permit back into the round.
    ///
    /// The one place a permit is handed to another tenant: the completed
    /// task's own lease calls it, so a task that ends hands its capacity
    /// straight to the next eligible waiter rather than returning it to a
    /// pool for the next arrival to win.
    pub(super) fn release(&self, tenant: &Tenant, permit: OwnedSemaphorePermit) {
        self.timed(&self.release_ops, &self.release_nanos, |round| {
            round.note_release(tenant);
            self.pump(round, Some(permit));
        });
    }

    /// Hand permits to eligible tenants until none can take one.
    ///
    /// `spare` is a permit already off the pool — a completed task's — and
    /// the loop reaches for another only when the round says a queued tenant
    /// could take it. A contended pool is therefore drained exactly as far
    /// as the queued work justifies and no further.
    fn pump(&self, round: &mut RoundGuard<'_>, spare: Option<OwnedSemaphorePermit>) {
        let mut spare = spare;
        loop {
            let permit = match spare.take() {
                Some(permit) => permit,
                None if round.has_eligible() => match self.take_from_pool() {
                    Some(permit) => permit,
                    None => return,
                },
                None => return,
            };
            let mut is_live = |slot: &Arc<WaitSlot>| slot.is_live();
            match round.grant(permit, &mut is_live) {
                GrantOutcome::Idle(permit) => {
                    // Nobody could take it. Dropping the permit returns it to
                    // the pool, which is what it was before this release, and
                    // the loop stops because an idle round says no tenant is
                    // waiting under a ceiling.
                    drop(permit);
                    return;
                }
                GrantOutcome::Granted(grant) => spare = Self::settle(round, grant),
            }
        }
    }

    /// Deliver one grant, or recycle its permit when the owner has left.
    ///
    /// Returns the permit when the delivery failed: the owner left between
    /// the round's liveness check and this handoff. The admission the grant
    /// charged is charged back, and the permit recycles into the same pump,
    /// so a grant that lost its waiter still reaches a live one.
    ///
    /// The round's abandoned count is not touched here. The owner has not
    /// reported yet — its report needs the round lock this pump holds — and
    /// when it does, it finds its slot withdrawn and reports nothing,
    /// because the waiter it would count is no longer in the queue.
    /// Correcting the count here instead would subtract before the owner
    /// added, and a count that saturates at zero loses the subtraction.
    fn settle(
        round: &mut RoundGuard<'_>,
        grant: Grant<Arc<WaitSlot>, OwnedSemaphorePermit>,
    ) -> Option<OwnedSemaphorePermit> {
        let returned = grant.waiter.deliver(grant.permit)?;
        round.note_release(&grant.tenant);
        Some(returned)
    }

    /// The round half of an owner's departure without a permit.
    ///
    /// The compaction inside `note_abandoned` needs to know which waiters are
    /// still owned, and the answer lives on each slot. Called after the slot
    /// lock is released, so the round lock is taken first and the slot locks
    /// inside it, in the order the pump uses.
    ///
    /// A withdrawn slot reports nothing: a pump already took its waiter off
    /// the queue and found it gone, and `note_abandoned` counts waiters that
    /// are still in a queue. The flag is read under the round lock, after
    /// any pump that could withdraw the slot has finished.
    fn report_abandoned(&self, slot: &WaitSlot, tenant: &Tenant) {
        let mut round = self.lock();
        if slot.lock().withdrawn {
            return;
        }
        let mut is_live = |waiter: &Arc<WaitSlot>| waiter.is_live();
        round.note_abandoned(tenant, &mut is_live);
    }

    /// How many admissions `tenant` holds, and how many it is waiting for.
    ///
    /// Read from the scheduler's own fields, so the report cannot disagree
    /// with the admission that produced it (INV-BOT-31's rule, applied to
    /// the per-tenant axis).
    pub(super) fn counts_of(&self, tenant: &Tenant) -> (usize, usize) {
        let round = self.lock();
        (round.in_flight_of(tenant), round.queued_of(tenant))
    }

    /// Cumulative admission-side decisions and their wall time: `(admit_ops,
    /// admit_nanos)` over [`TenancyShell::admit`] and
    /// [`TenancyShell::try_admit`], `(release_ops, release_nanos)` over
    /// [`TenancyShell::release`], all in nanoseconds.
    ///
    /// Read from the counters [`TenancyShell::timed`] charges, so the report
    /// cannot disagree with the decisions it measured (INV-BOT-31's rule,
    /// applied to the admission-cost axis).
    pub(super) fn decision_cost(&self) -> (u64, u64, u64, u64) {
        (
            self.admit_ops.load(Ordering::Relaxed),
            self.admit_nanos.load(Ordering::Relaxed),
            self.release_ops.load(Ordering::Relaxed),
            self.release_nanos.load(Ordering::Relaxed),
        )
    }

    /// How many of `tenant`'s waiting admissions this shell still holds, live or
    /// abandoned. Read from the round's own deque, so a test can see the
    /// retention rather than infer it from the live count.
    ///
    /// A test seam rather than production API: the report a caller needs is
    /// [`counts_of`](Self::counts_of), and the retained-but-abandoned count
    /// is a fact about callers that walked away rather than about capacity.
    #[cfg(test)]
    pub(super) fn retained_waiters(&self, tenant: &Tenant) -> usize {
        self.lock().retained_of(tenant)
    }

    /// How many tenants hold at least one waiter.
    pub(super) fn tenants_waiting(&self) -> usize {
        self.lock().tenants_with_queues()
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::sync::Arc;

    use crate::rt::sync::Semaphore;

    use super::{Departure, TenancyShell, WaitSlot};
    use crate::rt::tenancy::{Arrival, GrantOutcome, TenancyPolicy};
    use crate::script::Tenant;

    /// An owner that walks away between the round's choice and the delivery
    /// leaves its tenant's live count exact.
    ///
    /// The interleaving is the one the locks force, driven step by step
    /// through the shell's own functions: the pump holds the round lock from
    /// its grant to its delivery, so an owner leaving in that window runs
    /// its slot half first, the pump's delivery fails, and the owner's round
    /// half runs only once the pump lets go. The tenant still has three
    /// owners waiting behind the one that left, and its live count must say
    /// so. Three, because with fewer the miscount is large enough to trigger
    /// the round's compaction, which recounts the queue and hides it.
    #[test]
    fn a_late_abandonment_leaves_the_live_count_exact() -> Result<(), Box<dyn Error>> {
        let shell = TenancyShell::new(Arc::new(Semaphore::new(0)), TenancyPolicy::new(4, 4));
        let tenant = Tenant::new("late")?;
        let leaving = WaitSlot::fresh();
        let staying = [WaitSlot::fresh(), WaitSlot::fresh(), WaitSlot::fresh()];
        {
            let mut round = shell.lock();
            for slot in std::iter::once(&leaving).chain(&staying) {
                let arrival = round.arrive(&tenant, Arc::clone(slot), || None::<()>);
                assert!(
                    matches!(arrival, Arrival::Queued),
                    "a spent pool queues every arrival"
                );
            }
        }
        let spare = Arc::new(Semaphore::new(1)).try_acquire_owned()?;
        let mut round = shell.lock();
        let mut is_live = |slot: &Arc<WaitSlot>| slot.is_live();
        let grant = match round.grant(spare, &mut is_live) {
            GrantOutcome::Granted(grant) => Some(grant),
            GrantOutcome::Idle(_) => None,
        }
        .ok_or("a queued waiter takes the permit")?;
        assert!(
            Arc::ptr_eq(&grant.waiter, &leaving),
            "the round chose the oldest waiter"
        );
        assert!(
            matches!(leaving.leave(), Departure::Abandoned),
            "the owner leaves before any permit reached it"
        );
        let recycled = TenancyShell::settle(&mut round, grant);
        drop(round);
        shell.report_abandoned(&leaving, &tenant);

        assert!(
            recycled.is_some(),
            "the failed delivery recycled its permit"
        );
        let (in_flight, queued) = shell.counts_of(&tenant);
        assert_eq!(
            in_flight, 0,
            "the failed delivery charged its admission back"
        );
        assert_eq!(
            queued,
            staying.len(),
            "three owners are still waiting, and the live count must say so"
        );
        assert_eq!(
            shell.retained_waiters(&tenant),
            staying.len(),
            "the round retains exactly the waiter still owned"
        );
        Ok(())
    }

    /// Every admit, non-blocking admit and release charges exactly one
    /// admission-side decision, so the counters a neighbour's split is read
    /// from count the decisions the round actually took.
    #[test]
    fn each_admission_path_charges_one_decision() -> Result<(), Box<dyn Error>> {
        use super::Admission;

        let shell = Arc::new(TenancyShell::new(
            Arc::new(Semaphore::new(1)),
            TenancyPolicy::new(1, 4),
        ));
        let tenant = Tenant::new("metered")?;
        assert_eq!(
            shell.decision_cost().0,
            0,
            "a fresh shell has taken no decisions"
        );
        let admitted = match shell.admit(&tenant) {
            Admission::Admitted(lease) => Some(lease),
            Admission::Queued(_)
            | Admission::Refused { .. }
            | Admission::SupervisorQueueFull { .. } => None,
        };
        let lease = admitted.ok_or("the pool's one permit admits at once")?;
        let (arrivals, _, releases, _) = shell.decision_cost();
        assert_eq!(arrivals, 1, "one arrival is one arrival decision");
        assert_eq!(releases, 0, "nothing has been released yet");
        drop(lease);
        let (arrivals, admit_nanos, releases, release_nanos) = shell.decision_cost();
        assert_eq!(arrivals, 1, "the release takes no arrival decision");
        assert_eq!(releases, 1, "the lease's release is the release decision");
        assert!(
            admit_nanos > 0 && release_nanos > 0,
            "both halves charged the time they held the lock"
        );
        Ok(())
    }
}
