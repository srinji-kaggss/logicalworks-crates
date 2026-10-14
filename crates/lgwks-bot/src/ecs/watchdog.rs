//! The poll watchdog: a wall deadline on sources that stop answering.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Waker};
use std::thread;
use std::time::Duration;

#[cfg(doc)]
use lgwks_deps::bevy_ecs::schedule::IntoScheduleConfigs;

use crate::journal::owner::{lock, wait_timeout};

use crate::error::BotError;
use crate::spec::Erased;
#[cfg(doc)]
use crate::verb::Observe;

#[cfg(doc)]
use super::builder::EcsBuilder;
// The one declared clock every deadline in this crate names (INV-BOT-30). Only
// its wall watchdog is read below — a source that stopped answering is not
// waiting for time, so a logical counter a caller can advance is the wrong
// authority for it — but it is reached through the crate's clock rather than
// through a free-floating `Instant`, which is what makes the elapsed time that
// cancelled a poll traceable to a named authority.
use crate::clock::Clock;

/// Upper bound on sources polled simultaneously by one tick. A source poll may
/// occupy one `spawn_blocking` thread, so this caps the blocking-thread fan-out
/// regardless of how many chains a spec declares. Chains beyond the cap are
/// polled in additional waves.
///
/// Also the width of the deadline watchdog: a wave is one unit of waiting, so a
/// wave has one watcher and the number of threads a tick can spend on the
/// deadline is the number of waves rather than the number of chains.
pub(super) const MAX_IN_FLIGHT_POLLS: usize = 32;

/// How long one source poll may take before the tick stops waiting for it.
///
/// `MAX_IN_FLIGHT_POLLS` above bounds how many sources are polled at once; this
/// bounds how long the tick waits for any one of them. Without it a source that
/// never resolves holds the whole tick — the observation wave is joined through
/// `lgwks_std::task::join_all_boxed`, which polls on the calling thread and
/// returns only when the wave has resolved — and every other chain's action is
/// held behind a source nobody can make progress for. That is T06's slow-source
/// half, and it was the one claim this substrate could not make.
///
/// Thirty seconds, and the reasoning is about what the number has to be rather
/// than about taste. It is long enough that a source reading a socket, a
/// `spawn_blocking` thread, or an ordinary remote call finishes first, so an
/// ordinary tick is never cut short by it; and it is short enough that "the
/// source is wedged" is a fact a caller waits one poll budget to observe rather
/// than a change observed at the next deploy. [`MAX_POLL_DEADLINE`] is the
/// ceiling a caller may raise it to, and the builder refuses anything past it.
pub const DEFAULT_POLL_DEADLINE: Duration = Duration::from_secs(30);

/// The largest per-poll deadline [`EcsBuilder::with_poll_deadline`] will grant.
///
/// A ceiling rather than an unconstrained field, because the deadline is the one
/// bound between a wedged source and a bot that never ticks again. A caller who
/// needs longer than this is making a statement about their whole fan-out, and
/// the refusal is where that statement belongs.
pub const MAX_POLL_DEADLINE: Duration = Duration::from_secs(600);

/// One observation wave's deadline watchdog, shared with every poll in it.
///
/// Two halves under two primitives rather than a set of atomics, because the
/// invariant is an *ordering* between three writes across two threads and a
/// lock is what makes "install the waker, then read the flag" one indivisible
/// step. With atomics the same sequence needs a re-check after every publish,
/// and the window that produces is exactly the lost wakeup that hangs a tick.
///
/// One per **wave**, not per poll. Every poll in a wave starts together under
/// the same budget, so the wave has one deadline to watch and one thread to
/// watch it with: a bot of *n* chains spent a thread per chain per tick before,
/// and a wave whose sources all answered on their first poll had nothing to
/// watch at all. The per-poll *report* is unchanged — a poll is still named
/// stalled, with its own chain index — because what a watcher does is set a flag
/// every poll in the wave reads, not decide any one poll's fate.
#[derive(Debug)]
pub(super) struct PollWatchdog {
    /// What the two threads share.
    ///
    /// Shared *by* `Arc` rather than moved into the reaper, because both sides
    /// read and write it: the polls install wakers and register as pending while
    /// the reaper is waiting on the same flag that decides whether it is still
    /// live. A copy on each side is two clocks with one name.
    pub(super) state: Arc<Mutex<WatchdogState>>,
    /// How the reaper waits, and how the wave ends that wait early.
    ///
    /// A condition variable rather than a sleep, because a wave that resolves
    /// must not leave a thread parked for the rest of its budget: that is the
    /// cost this was built to remove, not to move.
    pub(super) settled: Arc<Condvar>,
    /// The reaper's handle, owned by the wave that started it.
    ///
    /// Its own lock rather than a field of the guarded state, because the wave
    /// takes it while the guarded state is *not* held, and nesting one mutex
    /// inside another would be an acquisition order every reader has to hold in
    /// their head for a handle two stores touch.
    pub(super) reaper: Mutex<Option<thread::JoinHandle<()>>>,
}

impl PollWatchdog {
    /// A watchdog for one wave of `members` polls under `deadline`.
    ///
    /// The width is taken at construction because the reaper decides the wave is
    /// over by comparing a resolved count against it, and a width it had to read
    /// out of shared state would be one more thing the two threads could
    /// disagree about.
    pub(super) fn new(deadline: Duration, members: usize) -> Self {
        Self {
            state: Arc::new(Mutex::new(WatchdogState {
                members,
                resolved: 0,
                expired: false,
                released: false,
                deadline,
                pending: vec![false; members],
                wakers: (0..members).map(|_| None).collect(),
            })),
            settled: Arc::new(Condvar::new()),
            reaper: Mutex::new(None),
        }
    }

    /// Mark one poll of this wave as finished.
    ///
    /// Once per poll, and by the *last* poll the wave is over and the reaper is
    /// released — written and notified inside one critical section, because
    /// setting `released` without the notification is the lost wakeup that leaves
    /// a thread parked for the rest of the budget on a wave nobody waits for.
    pub(super) fn poll_finished(&self) {
        let mut state = lock(&self.state);
        state.resolved = state.resolved.saturating_add(1);
        if state.resolved >= state.members {
            state.released = true;
            self.settled.notify_all();
        }
    }

    /// Note that `slot` may still have to be waited for.
    ///
    /// Set once, under the same lock the poll read `expired` under, so a reaper
    /// firing on that very turn cannot decide the wave was over while this poll
    /// was still outstanding.
    pub(super) fn poll_pending(&self, slot: usize) {
        let mut state = lock(&self.state);
        if let Some(pending) = state.pending.get_mut(slot) {
            *pending = true;
        }
    }

    /// Whether the wave's deadline has already passed.
    pub(super) fn expired(&self) -> bool {
        lock(&self.state).expired
    }

    /// Start the wave's reaper, unless one is running, the wave is over, or
    /// nothing in it is still unresolved.
    ///
    /// Returns `false` only when the host refused the thread *and* some poll is
    /// still unresolved. The caller reports that as a stall for the poll that
    /// asked and the wave stops, rather than waiting for a deadline nobody is
    /// watching — which is the failure this watchdog exists to prevent, so a
    /// substrate that cannot watch must not pretend it is watching.
    ///
    /// Lazy, because a poll that resolved on its first turn never reaches this
    /// with anything to watch: `true` with no thread started is the ordinary
    /// answer. Once per wave, because every poll in a wave shares one deadline
    /// and one thread watching it is the whole of the bound.
    pub(super) fn start(&self, clock: &Clock, armed: &AtomicBool) -> bool {
        // Serialized on the handle's own lock for the whole decision *and* the
        // spawn, so "already started" is only ever answered for a thread that
        // really exists. A second poll arriving while the first is still
        // spawning waits for the fact rather than reading an optimistic flag,
        // and a refused spawn cannot leave a poll parked against a thread that
        // never started. The lock order is reaper → state here and nowhere the
        // reverse, and the reaper thread itself takes neither.
        let mut owned = lock(&self.reaper);
        if owned.is_some() {
            return true;
        }
        {
            let state = lock(&self.state);
            // A wave with nothing pending — every source answered on its first
            // poll — has nothing to watch, and an expired or released wave is
            // already over: neither starts a thread.
            if state.expired || state.released || !state.pending.iter().any(|held| *held) {
                return true;
            }
        }
        let shared = WatchdogShared {
            deadline: lock(&self.state).deadline,
            state: Arc::clone(&self.state),
            settled: Arc::clone(&self.settled),
        };
        // `std::thread::Builder` rather than the `std::thread::spawn` this
        // workspace bans: the handle is stored in the wave and joined by the wave,
        // so the thread is owned for its whole life rather than being
        // fire-and-forget with an invisible panic.
        let built = thread::Builder::new()
            .name("lgwks-poll-deadline".into())
            .spawn({
                let clock = clock.clone();
                move || reaper(&clock, &shared)
            });
        match built {
            Ok(handle) => {
                *owned = Some(handle);
                // The wave-level mirror the report reads, set only on the path
                // that really did start one, so a zero in the report is an honest
                // statement that no thread was started.
                armed.store(true, Ordering::Relaxed);
                true
            }
            // Not stored, so the next poll that goes Pending tries again rather
            // than concluding from one refusal that the wave is already watched.
            Err(_) => false,
        }
    }

    /// Join the reaper if one was started.
    ///
    /// Split from the wave's own teardown because the join has to *happen* before
    /// the wave reads its polls' outcomes — that ordering is what makes a
    /// resolved wave leave no thread parked behind it.
    pub(super) fn join(&self) {
        let handle = lock(&self.reaper).take();
        if let Some(handle) = handle {
            // A reaper panic is not this wave's outcome to report: the polls'
            // own answers are the observation, and the thread has either done its
            // work or given up. Joining rather than detaching is what keeps the
            // thread owned to the wave that started it.
            let _joined = handle.join();
        }
    }
}

/// The guarded half of [`PollWatchdog`].
#[derive(Debug, Default)]
pub(super) struct WatchdogState {
    /// Whether the wave's deadline has passed. Read on the polling thread,
    /// written by the reaper.
    pub(super) expired: bool,
    /// Whether every poll in the wave has finished, so the reaper has nothing
    /// left to do.
    ///
    /// Distinct from the resolved count, because reaching the width is what sets
    /// this and reading the count is not reading the fact.
    pub(super) released: bool,
    /// How many polls in the wave have finished.
    pub(super) resolved: usize,
    /// How many polls the wave has.
    pub(super) members: usize,
    /// Whether each slot's poll may still have to be waited for.
    ///
    /// One entry per poll, indexed by the slot its waker occupies, rather than a
    /// count: the question that matters before a thread is started is *which*
    /// polls are outstanding, because that is the same index the wakers are
    /// stored at, and a count a poll could decrement twice would answer it wrong.
    pub(super) pending: Vec<bool>,
    /// The budget this wave is watched against.
    pub(super) deadline: Duration,
    /// The wave's tasks to wake when the deadline passes.
    ///
    /// One slot per poll, left installed across the reaper's wait rather than
    /// cleared before it: clearing is what loses the wakeup. The reaper clears a
    /// slot on the way *out* by taking it, and a poll installs its own on every
    /// turn, so the one case that must not happen — a deadline expiring against a
    /// poll already parked with its slot empty — cannot be constructed.
    pub(super) wakers: Vec<Option<Waker>>,
}

/// What a spawned reaper runs on: the budget and the two halves both sides
/// share.
pub(super) struct WatchdogShared {
    /// The budget this wave is watched against.
    pub(super) deadline: Duration,
    /// What the two threads share.
    pub(super) state: Arc<Mutex<WatchdogState>>,
    /// How the reaper waits, and how the wave ends that wait early.
    pub(super) settled: Arc<Condvar>,
}

/// Watch one wave's deadline and wake every poll still in it when it passes.
///
/// The remaining budget is recomputed on every pass rather than slept once, so
/// the deadline is honoured to the quantum rather than to a multiple of it, and
/// a spurious wake — which a condition variable is free to produce — costs one
/// extra turn rather than a missed deadline.
///
/// The wakers are *not* cleared before the wait, and `released` is *not* set
/// without the notification. Those are the two halves of the same mistake: a
/// poll installs its waker on the turn that finds `expired` false and then
/// parks, so a reaper that cleared the slots on the way into its wait would
/// expire against empty ones; and a reaper that published `released` before
/// notifying would sleep out the whole budget on a wave that had already
/// returned. Both are avoided by making the notification happen while the lock
/// is held, which is what `settled.notify_all()` inside the same critical
/// section as the write buys.
pub(super) fn reaper(clock: &Clock, watchdog: &WatchdogShared) {
    let fired = clock.wall_watchdog();
    let mut state = lock(&watchdog.state);
    loop {
        // A wave that finished before this thread reached its first wait must
        // not be waited for. `released` is set and notified by the last poll's
        // `Drop`; a notification that lands before the reaper waits is a wakeup
        // a condition variable does not replay, so a per-poll watchdog that was
        // spawned *before* the poll never saw the race, while a wave-level one
        // spawned *during* the poll does — the source that yields and answers
        // on its very next turn resolves in less time than this thread takes to
        // start. Checked under the same guard `wait_timeout` releases
        // atomically, so no poll can set `released` between this check and the
        // wait.
        if state.released {
            return;
        }
        let remaining = watchdog.deadline.saturating_sub(fired.elapsed());
        if remaining.is_zero() {
            state.expired = true;
            // Woken with the lock held, because each waker must be taken here: a
            // poll that installs one immediately afterwards would otherwise park
            // against a slot nobody holds.
            for slot in &mut state.wakers {
                if let Some(waker) = slot.take() {
                    waker.wake();
                }
            }
            return;
        }
        // Waited on a condition variable, never on a sleep. A poll that installs
        // a waker while the reaper is parked against the *same* condition wakes
        // it (`poll_finished` notifies inside its critical section), so a wave
        // that resolved is not paid for with a thread parked out the rest of the
        // budget; and a poll that installs a waker *after* the reaper's last
        // check is one the reaper can still reach, because the waker slots are
        // left installed across the wait rather than cleared before it. Clearing
        // a slot on the way into the wait is the mistake this comment exists for:
        // it loses the wakeup on exactly the turn the wave needs it, and the
        // symptom is a tick that never returns.
        let (woken, _) = wait_timeout(&watchdog.settled, state, remaining);
        state = woken;
        if state.expired || state.released {
            return;
        }
    }
}

/// One source poll's half of a wave: the future, its chain index, and the slot
/// its waker occupies.
///
/// A named struct rather than positional arguments because the one thing a
/// caller must not get wrong is *which* wave a poll belongs to, and a parameter
/// list is where that goes wrong.
pub(super) struct WavePoll<'wave> {
    /// The chain this poll reads, named in its own report.
    pub(super) chain: usize,
    /// The wave's deadline watchdog.
    pub(super) watchdog: &'wave PollWatchdog,
    /// This poll's position in the wave's waker slots.
    pub(super) slot: usize,
    /// Whether the waker for this poll is installed in its slot.
    ///
    /// Part of the poll rather than the guarded state, so "install, then read"
    /// can be *one* critical section: reading it back out of shared state would
    /// need a second lock acquisition, and the window between the two is the
    /// lost wakeup this whole structure exists to close.
    pub(super) installed: bool,
    /// The source's own poll.
    ///
    /// Borrowed from the world's chains for the wave's lifetime rather than
    /// `'static`, because the chain is erased in place and the poll is not: a
    /// `'static` box would have to own a copy of the source it is reading, and
    /// the substrate's promise is that it polls *the* source the caller declared.
    pub(super) poll: Pin<Box<dyn Future<Output = Result<Option<Erased>, BotError>> + 'wave>>,
}

impl WavePoll<'_> {
    /// The typed stall this poll reports when it cannot be waited for any more.
    ///
    /// A whole poll result rather than a bare error, because every caller of it
    /// is a `Poll::Ready` arm of this poll's own turn: a stall is what the
    /// source's slot holds, and a caller that had to wrap it would be the second
    /// place that shape is written.
    pub(super) fn stalled(&self, deadline: Duration) -> Result<Option<Erased>, BotError> {
        Err(BotError::PollStalled {
            chain: self.chain,
            deadline,
        })
    }

    /// Turn this poll once under the wave's deadline.
    ///
    /// Install, register, read `expired`, and poll are sequenced so that no arm
    /// can park against a deadline nobody is watching and no deadline can expire
    /// against a poll whose waker is not installed. The two registrations are the
    /// half that is easy to get wrong and impossible to notice: a poll that
    /// registered as pending *after* the reaper checked would never be woken, and
    /// a poll that started a thread *after* the reaper decided the wave was
    /// released would wait forever on a thread that has already gone.
    pub(super) fn turn(
        &mut self,
        cx: &mut Context<'_>,
        clock: &Clock,
        armed: &AtomicBool,
        deadline: Duration,
    ) -> Poll<Result<Option<Erased>, BotError>> {
        // Copied out of `self` so the guard borrows the wave, not this poll,
        // and the poll can still install its own waker under it.
        let watchdog = self.watchdog;
        let cut_off = {
            let mut state = lock(&watchdog.state);
            // An expired wave still turns every poll once more, and only a poll
            // still pending after that turn is cut off. The reaper is another
            // thread, so the deadline can pass between any two turns of a poll —
            // before a sibling's first turn, or between a yielding source's
            // `Pending` and the turn it woke itself for. Refusing that turn
            // unpolled would report a source that was ready to answer as stalled
            // on nothing but how the host scheduled two threads, so the same seed
            // would decide a different stall set on a loaded host than an idle
            // one. One more turn is bounded: a wedged poll answers `Pending` and
            // is cut off on it, so an expired wave ends on its next pass.
            if state.expired && self.installed {
                true
            } else {
                self.install(&mut state, cx);
                false
            }
        };
        match self.poll.as_mut().poll(cx) {
            Poll::Ready(outcome) => Poll::Ready(outcome),
            Poll::Pending if cut_off => Poll::Ready(self.stalled(deadline)),
            Poll::Pending => self.park(clock, armed, deadline),
        }
    }

    /// Install this turn's waker in the poll's slot, unless the one already
    /// there will wake the same task.
    ///
    /// Whether the installed waker is one the executor will recognise: a poll
    /// that has not installed one yet, or whose installed waker belongs to a
    /// different task, has to write again — and writing the *same* waker on
    /// every turn would be a clone per turn for nothing.
    pub(super) fn install(&mut self, state: &mut WatchdogState, cx: &Context<'_>) {
        let stale = if self.installed {
            state
                .wakers
                .get(self.slot)
                .and_then(|slot| slot.as_ref())
                .is_none_or(|installed| !installed.will_wake(cx.waker()))
        } else {
            true
        };
        if let (true, Some(slot)) = (stale, state.wakers.get_mut(self.slot)) {
            *slot = Some(cx.waker().clone());
            self.installed = true;
        }
    }

    /// Register a poll that answered `Pending` on a live wave, arm the wave's
    /// reaper, and report whether it may still wait.
    pub(super) fn park(
        &self,
        clock: &Clock,
        armed: &AtomicBool,
        deadline: Duration,
    ) -> Poll<Result<Option<Erased>, BotError>> {
        // Registered before the wave is armed, so the reaper's first look at the
        // wave already sees this poll as outstanding. The registration and the
        // `expired` read below are two critical sections rather than one, and
        // that is safe here in a way it is not in `turn`: `start` takes the same
        // lock, so a reaper cannot be running to fire between them until this
        // poll has registered.
        self.watchdog.poll_pending(self.slot);
        // Started only now, because a wave with nothing pending has nothing to
        // watch. The first poll in the wave to go Pending is the one that spends
        // the thread, and a wave whose every source answered spends none — the
        // ordinary tick. Registering first and arming second is what makes a
        // wave that will never be woken again, a wedged source, still bounded:
        // the reaper exists before this turn returns `Pending`, so the deadline
        // can still fire and wake it. A spawn the host refuses reports this poll
        // stalled and leaves every already-resolved poll in the wave with its
        // answer, because a poll that answered never reaches this arm. An expiry
        // read here costs the poll nothing it was about to read: it was polled
        // this turn and answered `Pending`.
        if !self.watchdog.start(clock, armed) || self.watchdog.expired() {
            Poll::Ready(self.stalled(deadline))
        } else {
            Poll::Pending
        }
    }
}

impl Drop for WavePoll<'_> {
    /// Count this poll as finished under the wave's lock.
    ///
    /// A `Drop` rather than a call at the end of the poll body, because a poll
    /// future dropped *before* it resolves — a tick cancelled mid-wave — is no
    /// longer going to ask, and a counter that missed it would leave a reaper
    /// parked for the rest of the budget on a wave nobody is waiting for.
    fn drop(&mut self) {
        self.watchdog.poll_finished();
    }
}

/// Run one wave's polls under a shared `deadline`, reporting a typed stall per
/// poll rather than waiting forever.
///
/// This is the whole of T06's slow-source half, and it is one function because
/// there is exactly one place in this crate where a source poll is awaited and
/// exactly one place that can therefore bound it. Putting the bound anywhere
/// else would mean a second place that could forget it.
///
/// # Why the *watchdog* half of the declared clock
///
/// The crate's one declared clock ([`crate::rt::clock::Clock`], INV-BOT-30) has
/// two halves, and this takes the one that is always on: a real-time watchdog
/// that keeps running when the logical counter is frozen. That is the correct
/// authority here, and the reason is in the clock's own contract — a source that
/// stopped answering is not waiting for time to pass, it has stopped making
/// progress entirely. A logical deadline would either expire against a poll that
/// was merely slow (because a test advanced the clock) or wait on the wedged one
/// forever (because nothing advanced it).
///
/// The declaration is still load-bearing: the watchdog is obtained *through* a
/// [`crate::rt::clock::Clock`], so the elapsed time that cancelled these polls
/// traces back to a clock the crate named rather than to a free-floating
/// `Instant` someone sampled at a call site.
///
/// # What "cancelled" means
///
/// A poll future is dropped where it stands. It committed nothing, so its
/// chain's baseline and its forced-refresh mark are exactly as they were, and
/// the next tick re-polls it — the same rule a failed poll already follows, and
/// for the same reason: the value this poll was supposed to replace is still
/// there. Dropping a future is cooperative, so a poll that already handed work
/// to `spawn_blocking` has its handle released and its thread runs to
/// completion; that is the documented behaviour of the estate's executor, and it
/// is why the stall report is about *this bot's* observation and not about the
/// source's side effects.
///
/// A sibling poll in the same wave is not stopped: the chains beside a wedged
/// source still commit and act in the same tick. The budget bounds the *wave's*
/// wait rather than each poll's separately, and the difference that makes is
/// bounded by the cost of starting the wave — every poll begins together, so
/// there is no second budget to spend and no poll can be cancelled twice.
pub(super) async fn bounded_wave(
    deadline: Duration,
    watchdog: &PollWatchdog,
    polls: impl IntoIterator<Item = WavePoll<'_>>,
) -> (Vec<Result<Option<Erased>, BotError>>, bool) {
    let clock = Clock::wall();
    // The wave-level mirror the report reads, set only on the path that really
    // did start a reaper, so a report beside a zero is an honest statement that
    // no thread was started.
    let armed = AtomicBool::new(false);

    let joined = lgwks_std::task::join_all_boxed(polls.into_iter().map(|mut poll| {
        let clock = clock.clone();
        let armed = &armed;
        Box::pin(std::future::poll_fn(move |cx: &mut Context<'_>| {
            poll.turn(cx, &clock, armed, deadline)
        }))
    }));
    let results = joined.await;
    // Joined before the results are read, so a wave that resolved leaves no
    // thread parked behind it and an expired wave has its reaper reaped before
    // the caller acts on what it decided. The watchdog is the wave's own — the
    // one its polls borrowed — so there is exactly one handle in this scope and
    // no thread can be started and left unjoined.
    watchdog.join();
    (results, armed.load(Ordering::Relaxed))
}
