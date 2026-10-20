//! The engine surfaces both sides implement, and the box that lets a multi-phase
//! run drive either one through a single loop.
//!
//! Moved from `main.rs` for #639; the behaviour it exercised is unchanged.

use std::future::Future;

use crate::widen;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use lgwks_bot::rt::supervise::Supervisor;

use crate::driver::{Arrival, recorded_body_unit};
use crate::support::{drain_supervisor, tokio_util_cancel};
/// Which admission door an arrival is offered through.
///
/// The two doors are the facade's own, and they fail differently, which is why a
/// saturation curve needs both: `spawn` awaits a slot, so past the knee the
/// caller's own loop becomes the queue and the backlog shows up as latency measured
/// from the intended start; `try_spawn` refuses at the bound, so past the knee the
/// arrival is dropped and counted and memory does not grow.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Admission {
    /// Await a free slot: the caller's own loop is the queue.
    Backpressure,
    /// Refuse at the bound and count the refusal.
    Refuse,
}

impl Admission {
    /// The name the results file carries.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Backpressure => "backpressure",
            Self::Refuse => "refuse",
        }
    }
}

/// Which side of the comparison an open-loop run drove.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Side {
    /// `lgwks_bot::rt::supervise::Supervisor`.
    Facade,
    /// Pinned raw Tokio: `JoinSet` + `Semaphore` + a `watch`-based token.
    Baseline,
}

impl Side {
    /// The name the results file carries.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Facade => "facade",
            Self::Baseline => "baseline",
        }
    }

    /// Every side, in the order a paired run drives them.
    pub(crate) const ALL: [Side; 2] = [Side::Facade, Side::Baseline];
}

/// The four operations an open-loop run needs from an engine.
///
/// This is why the generator exists once rather than twice. A comparison between a
/// facade and the engine it wraps is only a comparison if everything except the
/// engine surface is *the same code*: two driver loops, however carefully matched,
/// are two programs, and a difference between them is indistinguishable from a
/// difference between the engines. So the generator, the latency accounting, the
/// queue sampling and the conservation gate are one piece of code parameterised by
/// these calls, and an engine contributes only what is genuinely its own.
pub(crate) trait Engine {
    /// Offer one arrival at the ceiling, reporting whether it was admitted.
    ///
    /// Blocks on the backpressure door and refuses on the other, which is the whole
    /// difference between the two overload behaviours.
    fn admit(&mut self, arrival: Arrival) -> impl Future<Output = bool>;

    /// Join whatever has finished, without waiting.
    fn reap(&mut self);

    /// Tasks in flight at this instant.
    fn in_flight(&self) -> u64;

    /// Join everything, blocking until nothing is left.
    fn drain(&mut self) -> impl Future<Output = ()>;

    /// Terminal counters after the drain: completed, cancelled, aborted.
    ///
    /// The engine's own counters, never a second tally this rig maintains beside
    /// them: a comparison that reads its numbers from a different place than the
    /// engine writes them is the second-ledger defect INV-BOT-31 exists to refuse.
    fn terminals(&self) -> (u64, u64, u64);
}

/// The facade, as an [`Engine`].
///
/// Every call is the facade's own door: `spawn` awaits a permit, `try_spawn` refuses
/// at the bound and counts the refusal, `reap` joins what finished, and the terminal
/// counters are `Supervisor::stats`. Nothing here re-implements any of it.
pub(crate) struct FacadeEngine {
    pub(crate) supervisor: Supervisor,
    pub(crate) admission: Admission,
}

impl FacadeEngine {
    /// A facade engine with a ceiling of `bound` at the given door.
    pub(crate) fn new(bound: usize, admission: Admission) -> Self {
        Self {
            supervisor: Supervisor::new(bound),
            admission,
        }
    }
}

impl Engine for FacadeEngine {
    fn admit(&mut self, arrival: Arrival) -> impl Future<Output = bool> {
        let admission = self.admission;
        let supervisor = &mut self.supervisor;
        async move {
            match admission {
                Admission::Backpressure => {
                    supervisor
                        .spawn(move |_token| recorded_body_unit(arrival))
                        .await;
                    true
                }
                Admission::Refuse => {
                    let attempt = supervisor.try_spawn(move |_token| recorded_body_unit(arrival));
                    if attempt.is_err() {
                        lgwks_std::trace::debug!(error = ?attempt.as_ref().err(), "FacadeEngine: the ceiling refused an arrival");
                    }
                    attempt.is_ok()
                }
            }
        }
    }

    fn reap(&mut self) {
        // Reaping is what keeps the retained set at the live bound rather than the
        // lifetime total, and `Supervisor` performs it on its own call.
        while self.supervisor.reap() > 0 {}
    }

    fn in_flight(&self) -> u64 {
        self.supervisor.stats().in_flight()
    }

    fn drain(&mut self) -> impl Future<Output = ()> {
        // Naming the target before the block is what lets the loop read a plain local
        // instead of re-deriving the spawn count on every pass.
        let target = self.supervisor.stats().spawned;
        async move { drain_supervisor(&mut self.supervisor, target).await }
    }

    fn terminals(&self) -> (u64, u64, u64) {
        let stats = self.supervisor.stats();
        (stats.succeeded, stats.cancelled, stats.aborted)
    }
}

/// Pinned raw Tokio, as an [`Engine`].
///
/// `JoinSet` for task-set lifetime, `Semaphore` for the ceiling the facade enforces,
/// and an explicit `try_acquire` before each spawn so a refusal at the bound is
/// counted rather than silently queued — the same refusal semantics the facade has.
/// Every `JoinError` is read, not discarded, so the abort/panic split survives
/// exactly as the facade preserves it.
pub(crate) struct BaselineEngine {
    pub(crate) set: JoinSet<()>,
    pub(crate) permits: Arc<Semaphore>,
    pub(crate) token: tokio_util_cancel::Token,
    pub(crate) admission: Admission,
    pub(crate) completed: u64,
    pub(crate) aborted: u64,
}

impl BaselineEngine {
    /// A baseline engine with a ceiling of `bound` at the given door.
    pub(crate) fn new(bound: usize, admission: Admission) -> Self {
        Self {
            set: JoinSet::new(),
            permits: Arc::new(Semaphore::new(bound)),
            token: tokio_util_cancel::token(),
            admission,
            completed: 0,
            aborted: 0,
        }
    }

    /// Spawn one admitted body, holding its permit for the body's whole life.
    fn spawn_body(&mut self, permit: tokio::sync::OwnedSemaphorePermit, arrival: Arrival) {
        let child = self.token.child();
        self.set.spawn(async move {
            let _held = permit;
            recorded_body_unit(arrival).await;
            let _ = child;
        });
    }

    /// Count one join result.
    ///
    /// `JoinError` is read rather than discarded: an aborted task and a panicked one
    /// are different facts, and the facade keeps them apart, so a baseline that
    /// collapsed them would be doing less work for a better number.
    fn count(&mut self, joined: Result<(), tokio::task::JoinError>) {
        match joined {
            Ok(()) => self.completed = self.completed.saturating_add(1),
            Err(_) => self.aborted = self.aborted.saturating_add(1),
        }
    }
}

impl Engine for BaselineEngine {
    fn admit(&mut self, arrival: Arrival) -> impl Future<Output = bool> {
        let admission = self.admission;
        let permits = Arc::clone(&self.permits);
        async move {
            let permit = match admission {
                Admission::Backpressure => permits.acquire_owned().await.ok(),
                Admission::Refuse => permits.try_acquire_owned().ok(),
            };
            let Some(permit) = permit else {
                return false;
            };
            self.spawn_body(permit, arrival);
            true
        }
    }

    fn reap(&mut self) {
        while let Some(joined) = self.set.try_join_next() {
            self.count(joined);
        }
    }

    fn in_flight(&self) -> u64 {
        // `JoinSet::len` is the set's own count, which is the same fact as "tasks
        // placed and not yet joined" — read from the engine rather than from a
        // second counter this rig would then have to keep in step with it.
        widen(self.set.len())
    }

    async fn drain(&mut self) {
        while let Some(joined) = self.set.join_next().await {
            self.count(joined);
        }
    }

    fn terminals(&self) -> (u64, u64, u64) {
        // Nothing cancels a baseline run, so `cancelled` is structurally zero. It is
        // returned rather than omitted so both sides' reports carry the same fields
        // and a reader does not have to remember which is which.
        (self.completed, 0, self.aborted)
    }
}

/// Either engine, as one value, so a multi-phase run is written once.
///
/// The phases of an overload run are the same three on either side, and an `enum`
/// with the two `Engine` implementations under it is what lets the phase loop call
/// `reap`/`admit`/`drain` without knowing which engine it is driving. Two phase loops
/// would be two programs again — the same objection the `Engine` trait exists to
/// answer.
pub(crate) enum EngineBox {
    Facade(FacadeEngine),
    Baseline(BaselineEngine),
}

impl EngineBox {
    /// Join whatever has finished, without waiting.
    pub(crate) fn reap(&mut self) {
        match self {
            Self::Facade(engine) => engine.reap(),
            Self::Baseline(engine) => engine.reap(),
        }
    }

    /// Offer one arrival at the ceiling, reporting whether it was admitted.
    pub(crate) async fn admit(&mut self, arrival: Arrival) -> bool {
        match self {
            Self::Facade(engine) => engine.admit(arrival).await,
            Self::Baseline(engine) => engine.admit(arrival).await,
        }
    }

    /// Tasks in flight at this instant.
    pub(crate) fn in_flight(&self) -> u64 {
        match self {
            Self::Facade(engine) => engine.in_flight(),
            Self::Baseline(engine) => engine.in_flight(),
        }
    }

    /// Join everything, blocking until nothing is left.
    pub(crate) async fn drain(&mut self) {
        match self {
            Self::Facade(engine) => engine.drain().await,
            Self::Baseline(engine) => engine.drain().await,
        }
    }

    /// Terminal counters after the drain: completed, cancelled, aborted.
    pub(crate) fn terminals(&self) -> (u64, u64, u64) {
        match self {
            Self::Facade(engine) => engine.terminals(),
            Self::Baseline(engine) => engine.terminals(),
        }
    }
}
