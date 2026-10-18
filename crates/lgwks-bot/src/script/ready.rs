//! A typed, generation-bound readiness fact, and the wait that consumes it.
//!
//! A step that starts something long-lived — a supervised process, an async
//! service body, a worker — has one thing to tell whoever depends on it: *when
//! may I use it?* The three ways that question has been answered outside this
//! type are all defects waiting to happen:
//!
//! | Old answer | What it costs |
//! |---|---|
//! | sleep for a guessed interval, then assume | a readiness wait that is a lie whenever the service is slower or faster than the guess, and a fixed cost on every run even when readiness is immediate |
//! | poll a flag in a loop | an unbounded loop inside a task, a wake per iteration, and no deadline of its own |
//! | a `bool` nobody bound to who set it | a late message from a *restarted, older* instance releases dependants onto a service that is gone |
//!
//! [`Readiness`] replaces all three with one fact and one wait:
//!
//! - **Generation-bound.** A [`Ready`] carries the [`Generation`] its instance
//!   was admitted under. A signal from an older generation is
//!   [`ReadinessError::StaleGeneration`] and releases nothing — see
//!   [`Generation`] for why the counter and not a timestamp.
//! - **Exactly one release.** A second ready signal from the same generation is
//!   [`ReadinessError::AlreadyReady`], a typed refusal that leaves the released
//!   generation untouched. It is *not* a second release, and not a silent
//!   no-op that a caller cannot tell from a lost signal.
//! - **Bounded.** A readiness admits at most [`MAX_DEPENDANTS`] dependants. The
//!   cap is declared here rather than left to the caller's arithmetic, and the
//!   admission is charged **before** a slot is taken.
//! - **Cancellable and deadline-charged.** [`Readiness::wait`] races the scope's
//!   stop and the step's own budget, so a stop ends the wait as
//!   [`FlowError::Cancelled`] and an over-budget wait is [`FlowError::TimedOut`].
//!   No readiness path sleeps or polls.
//! - **Failure after ready is not silence.** A readiness that has already
//!   released its dependants can still be failed, and every dependant still
//!   running is told through its own cancellation token — see
//!   [`Readiness::fail`].
//!
//! # Why the dependants are cancelled rather than messaged
//!
//! A dependant is a scope. What the readiness owes it is "you may proceed", and
//! what a later failure means is "what you were allowed to do is over". A
//! `Readiness` cannot push a *value* into a scope it does not own — a dependant
//! may be anywhere in a fan-out — so failure after ready is delivered the way
//! this crate already delivers a stop: the dependant's own
//! [`CancellationToken`], cancelled at the instant the readiness fails. The
//! dependant then reports [`FlowError::Cancelled`] at its own path, and the
//! run's [`Report`] carries it. A caller that wants to
//! distinguish "my scope was stopped by the host" from "the service I depended
//! on failed" reads [`Readiness::outcome`] before waiting, or races the token
//! against its own work.
//!
//! The token is the **dependant's own scope token**, not a shared one: the
//! readiness records it as each dependant is admitted and cancels exactly that,
//! so one dependant's failure-to-proceed never reaches its siblings.
//!
//! # What this does not do
//!
//! It does not decide *what* "ready" means for a given service, it does not
//! supervise the service itself (that is
//! [`Supervisor`]), and it does not make a
//! dependant's own work safe once released — a released dependant is told the
//! service was ready, which is all a readiness fact ever asserts.
//!
//! # Example
//!
//! `a_duplicate_ready_signal_is_refused_and_releases_nothing` and
//! `a_wait_is_budgeted_cancellable_and_immediate_when_already_released` (`tests/it/ready.rs`) run it:
//! a second ready signal is a typed refusal and not a second release, and a dependant waits under
//! its own budget with no sleep and no poll loop.

mod error;
mod outcome;
mod readiness;
mod state;

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

pub use error::ReadinessError;
pub use outcome::{FailAfterReady, Ready, ReadyOutcome};
pub use state::Readiness;

/// How many dependants one [`Readiness`] admits.
///
/// Declared rather than left to the caller's arithmetic. Ten thousand waiters
/// on one service is a real shape — the saturation tier this crate's simulation
/// layer sweeps — so the cap is set where it is measured rather than at a
/// power of two a caller could exceed; but a readiness that admits dependants
/// without bound is a queue with no policy, which is the defect this module
/// exists to remove.
pub const MAX_DEPENDANTS: usize = 65_536;

/// The most bytes of a readiness's own name that are retained.
///
/// A name is part of every error location and every log line a dependant
/// produces, so it is bounded like any other payload copied into each of them.
pub const MAX_NAME_BYTES: usize = 128;

/// The identity of one *instance* of a service a [`Readiness`] describes.
///
/// A monotone counter, not a timestamp and not a random token, because the two
/// facts a generation has to decide are both orderings: is this signal *newer*
/// than what the readiness has already seen, and is it the generation this
/// dependant was admitted under? A counter answers both exactly. A timestamp
/// would have to trust the clocks agree across hosts (INV-BOT-30 says they are
/// not measured), and a random token would answer "same or not" but never
/// "older", so a restarted instance could neither be accepted nor distinguished
/// from the current one.
///
/// [`Generation::FIRST`] is the first instance. [`Generation::next`] saturates
/// at `u64::MAX` and never wraps, so a wrap cannot make an old instance look
/// current. Two services' generations are unrelated — each readiness compares
/// only the generations it was itself bound to — so the counter being global
/// costs a service nothing and buys two restarts on one host distinct identities
/// for free.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Generation(u64);

impl Generation {
    /// The generation of the first instance of a service.
    pub const FIRST: Self = Self(0);

    /// The next generation, for a restarted instance.
    ///
    /// Monotone and saturating: two instances can never carry the same
    /// generation, and a counter that has reached its ceiling keeps reporting
    /// the ceiling rather than wrapping into a value an old instance already
    /// used. A generation is only ever *compared* against another generation
    /// the same readiness issued, so a global counter is an identity source
    /// rather than an ordering the crate depends on being comparable across
    /// services.
    #[must_use]
    pub fn next() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut held = COUNTER.load(Ordering::SeqCst);
        loop {
            let next = held.saturating_add(1);
            match COUNTER.compare_exchange_weak(held, next, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => return Self(next),
                // Another thread moved the counter between the read and the
                // exchange, or the exchange lost a spurious race. Either way the
                // observed value is the one the counter really holds, so this
                // thread re-reads from there rather than handing out a
                // generation another thread may already have issued — which is
                // the collision a saturating `try_update` would have turned into
                // a second `u64::MAX`.
                Err(observed) => held = observed,
            }
        }
    }

    /// The number behind this generation.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The generation `number` counts to, when a caller restores one it recorded.
    ///
    /// The counterpart to [`Generation::next`] for the case where the ordering is
    /// *known* rather than drawn: a host that recorded "the service was on
    /// generation 7" and restarts it must be able to arm generation 8 and refuse
    /// the 7 that a surviving handle still carries. Without this, a caller
    /// reproducing a recorded generation would have to walk [`Generation::next`]
    /// `number` times, which is a silent way to get a different generation than
    /// the one recorded.
    ///
    /// Saturation is not applied: `number` is the caller's own count, and a
    /// `u64` is wider than any restart sequence that fits in a durable record.
    #[must_use]
    pub const fn at(number: u64) -> Self {
        Self(number)
    }
}

impl fmt::Display for Generation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "generation {}", self.0)
    }
}
