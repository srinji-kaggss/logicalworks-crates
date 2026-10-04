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
//! run's [`Report`](crate::task::Report) carries it. A caller that wants to
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
//! [`Supervisor`](crate::rt::supervise::Supervisor)), and it does not make a
//! dependant's own work safe once released — a released dependant is told the
//! service was ready, which is all a readiness fact ever asserts.
//!
//! # Example
//!
//! ```
//! use lgwks_bot::block_on;
//! use lgwks_bot::script::{FlowError, Generation, Readiness, Scope, Tenant};
//! # use std::time::Duration;
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let scope = Scope::root(Tenant::new("acme")?);
//! let readiness: Readiness<&'static str> = Readiness::new("db")?;
//!
//! // A service body signals when it is ready. In a real step this is the line
//! // the child printed, observed through the supervisor; here it is the body.
//! let service = {
//!     let readiness = readiness.clone();
//!     async move { readiness.ready(Generation::FIRST, "db.internal:5432") }
//! };
//! // A dependant waits with its own budget: no sleep, no poll loop. It is
//! // admitted before it waits, so a failure that lands now reaches it.
//! let dependant = async {
//!     let ready = readiness.wait(&scope, Duration::from_secs(5)).await?;
//!     Ok::<_, FlowError>(*ready.awaited())
//! };
//! let (signalled, served) = block_on(async { lgwks_bot::join!(service, dependant) });
//! signalled?;
//! assert_eq!(served?, "db.internal:5432");
//!
//! // A second signal is a typed refusal, not a second release.
//! let duplicate = readiness.ready(Generation::FIRST, "elsewhere");
//! assert!(duplicate.is_err(), "a duplicate release must be refused");
//! # Ok(())
//! # }
//! ```

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::rt::sync::CancellationToken;
use crate::rt::sync::watch;

use super::error::FlowError;
use super::{Scope, within};

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
        Self(
            COUNTER
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                    Some(value.saturating_add(1))
                })
                .unwrap_or(u64::MAX),
        )
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

/// Why a readiness signal was refused.
///
/// Every arm is a *refusal that released nothing*, and each names which of the
/// facts a caller turns on: the service never became ready, a stale instance
/// tried to release dependants, a signal came from a generation this readiness
/// never issued, a second release was attempted, or the readiness's own bound
/// was reached. [`ReadinessError::released`] is `false` for all of them, which
/// is what makes a refusal safe to retry rather than a silent no-op.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReadinessError {
    /// The service failed before it was ready. No dependant was released.
    NotReady {
        /// The readiness that failed.
        what: Arc<str>,
        /// The generation that failed.
        generation: Generation,
        /// What the service said.
        reason: Arc<str>,
    },
    /// The signal came from an instance older than the one dependants are
    /// waiting on, or older than the one already released.
    ///
    /// Released nothing. This is the fence a restarted service needs: an older
    /// instance that says "ready" after a newer one has taken over must not
    /// release dependants onto a service that is gone.
    StaleGeneration {
        /// The readiness the signal was sent to.
        what: Arc<str>,
        /// The generation the readiness is currently bound to.
        expected: Generation,
        /// The generation the signal carried.
        got: Generation,
    },
    /// The signal names a generation this readiness never issued.
    ///
    /// Distinct from [`Self::StaleGeneration`] because the two are different
    /// facts: an older generation is a *previous* instance of this service, and
    /// a generation this readiness never issued is a signal from somewhere else
    /// entirely — another service's readiness, or a caller that guessed. A
    /// caller deciding whether to re-arm its service wants the first and wants
    /// the second refused.
    UnknownGeneration {
        /// The readiness the signal was sent to.
        what: Arc<str>,
        /// The generation the signal carried.
        got: Generation,
    },
    /// This readiness has already released, so a second signal at the same
    /// generation is refused.
    ///
    /// A duplicate is not a second release. The generation released first is
    /// kept, so a dependant that arrives late still reads the first release
    /// rather than being sent to a restarted instance. A signal carrying a
    /// *different* generation is [`Self::StaleGeneration`] or
    /// [`Self::UnknownGeneration`] instead: "already ready" and "that signal was
    /// from somewhere else" are different facts with different repairs.
    AlreadyReady {
        /// The readiness that is already ready.
        what: Arc<str>,
        /// The generation that was released.
        generation: Generation,
    },
    /// This readiness is already failed, so a signal is refused.
    ///
    /// The failed generation and its reason are kept, so a service that recovers
    /// by restarting must be armed with a *new* [`Readiness`] rather than
    /// re-signalling a dead one.
    AlreadyFailed {
        /// The readiness that already failed.
        what: Arc<str>,
        /// The generation that failed.
        generation: Generation,
    },
    /// This readiness was shut down, so a signal is refused.
    Closed {
        /// The readiness that is shut down.
        what: Arc<str>,
        /// The generation that shut down.
        generation: Generation,
    },
    /// No dependant may be registered: the declared cap is reached.
    ///
    /// Charged **before** a slot is taken, so a refused admission never
    /// consumes capacity and a caller that reduces its dependants sees the cap
    /// again rather than a permanently exhausted readiness.
    DependantsFull {
        /// The readiness that is full.
        what: Arc<str>,
        /// The cap that was reached.
        cap: usize,
    },
    /// The readiness's name is empty or longer than [`MAX_NAME_BYTES`].
    InvalidName {
        /// What is wrong with the name.
        reason: &'static str,
    },
}

impl ReadinessError {
    /// The readiness this refusal names.
    #[must_use]
    pub fn what(&self) -> &str {
        match *self {
            Self::NotReady { ref what, .. }
            | Self::StaleGeneration { ref what, .. }
            | Self::UnknownGeneration { ref what, .. }
            | Self::AlreadyReady { ref what, .. }
            | Self::AlreadyFailed { ref what, .. }
            | Self::Closed { ref what, .. }
            | Self::DependantsFull { ref what, .. } => what,
            Self::InvalidName { .. } => "",
        }
    }

    /// Whether this refusal released a dependant.
    ///
    /// `false` for every arm. It is stated rather than assumed because "the
    /// signal was refused" and "nothing happened" are different facts, and only
    /// the second one is what makes a refusal safe to retry.
    #[must_use]
    pub const fn released(&self) -> bool {
        false
    }

    /// Whether repeating this exact signal would produce the same refusal.
    ///
    /// The retry contract of a readiness, stated once. A stale signal, a signal
    /// from a generation this readiness never issued, a duplicate release and a
    /// full cap are all permanent: the world does not change between two
    /// identical calls, so an enclosing [`retry`](super::retry) that treated
    /// them as retryable would loop on a stale instance forever.
    #[must_use]
    pub const fn is_permanent(&self) -> bool {
        !matches!(self, Self::NotReady { .. })
    }

    /// Locate this refusal at `at`, for a flow that reports it through `?`.
    ///
    /// The arms map to the flow's own two retryable shapes on purpose. A
    /// failure before readiness is [`FlowError::Transient`]: the service may
    /// come up on a second attempt, and refusing to retry it would make a
    /// transient outage permanent. Everything else is
    /// [`FlowError::Failed`], because every one of them repeats.
    #[must_use]
    pub fn located(self, at: Arc<str>) -> FlowError {
        match self {
            Self::NotReady {
                what,
                generation,
                reason,
            } => FlowError::Transient {
                at,
                reason: format!("{what} did not become ready ({generation}): {reason}"),
            },
            Self::StaleGeneration {
                what,
                expected,
                got,
            } => FlowError::Failed {
                at,
                reason: format!("{what} refused a stale signal: expected {expected}, got {got}"),
            },
            Self::UnknownGeneration { what, got } => FlowError::Failed {
                at,
                reason: format!("{what} refused a signal from {got}, which it never issued"),
            },
            Self::AlreadyReady { what, generation } => FlowError::Failed {
                at,
                reason: format!(
                    "{what} is already ready ({generation}); a duplicate signal is refused"
                ),
            },
            Self::AlreadyFailed { what, generation } => FlowError::Failed {
                at,
                reason: format!("{what} already failed ({generation}); arm a new readiness"),
            },
            Self::Closed { what, generation } => FlowError::Failed {
                at,
                reason: format!("{what} was shut down ({generation}); a signal is refused"),
            },
            Self::DependantsFull { what, cap } => FlowError::Failed {
                at,
                reason: format!("{what} is at its declared cap of {cap} dependants"),
            },
            Self::InvalidName { reason } => FlowError::Failed {
                at,
                reason: format!("invalid readiness name: {reason}"),
            },
        }
    }
}

impl fmt::Display for ReadinessError {
    /// The refusal's own text, located at the empty path: this is a `Display`
    /// for a `Debug` line, and a caller that wants it located in a flow uses
    /// [`ReadinessError::located`].
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.clone().located(Arc::from("")).to_string())
    }
}

impl std::error::Error for ReadinessError {}

impl From<ReadinessError> for FlowError {
    /// Lets `?` carry a readiness refusal out of a flow body; the location is
    /// filled at the step boundary the same way every other flow error is.
    fn from(source: ReadinessError) -> Self {
        source.located(Arc::from(""))
    }
}

/// What one [`Readiness`] settled on.
///
/// A sum type rather than a `Result`, because it is *not* an error: a readiness
/// that failed is a fact the caller reads, and a failure after the release is
/// still a fact rather than a missing value. [`Readiness::outcome`] returns
/// `Option<ReadyOutcome>` — `None` is "has not settled", the one thing no
/// variant of this type can mean.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReadyOutcome<T> {
    /// The instance released its dependants, carrying this evidence.
    Ready {
        /// The generation that was released.
        generation: Generation,
        /// The evidence the service published, shared with every dependant.
        value: Arc<T>,
    },
    /// The instance failed.
    Failed {
        /// The generation that failed.
        generation: Generation,
        /// What the service said.
        reason: Arc<str>,
    },
    /// The instance was shut down, which is a stop and not a failure.
    Closed {
        /// The generation that shut down.
        generation: Generation,
    },
}

impl<T> ReadyOutcome<T> {
    /// The generation this outcome settled at.
    #[must_use]
    pub const fn generation(&self) -> Generation {
        match *self {
            Self::Ready { generation, .. }
            | Self::Failed { generation, .. }
            | Self::Closed { generation } => generation,
        }
    }

    /// Whether the instance was released.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        matches!(*self, Self::Ready { .. })
    }
}

/// A dependency's proof that the instance it waited on was released.
///
/// Holding one is holding a statement about a generation: it cannot be built
/// except by a wait that observed the release, so the value a dependant uses is
/// the value the readiness published for its generation rather than anything it
/// inferred from a timer or a poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ready<T> {
    /// The generation that was released.
    generation: Generation,
    /// The evidence, shared with every other dependant.
    value: Arc<T>,
}

impl<T> Ready<T> {
    /// The generation this release was published for, so a dependant can tell which instance of the service its evidence came from.
    #[must_use]
    pub const fn generation(&self) -> Generation {
        self.generation
    }

    /// The evidence the service published at that generation.
    ///
    /// A borrow, so a dependant reads the service's own answer rather than a
    /// copy it made and could have edited. Every dependant gets the same value:
    /// the `Arc` is what makes a thousand dependants cost one allocation rather
    /// than a thousand copies of whatever a service published.
    #[must_use]
    pub fn awaited(&self) -> &T {
        &self.value
    }

    /// The evidence, shared with the dependants that also received it.
    #[must_use]
    pub fn shared(&self) -> Arc<T> {
        Arc::clone(&self.value)
    }
}

/// A readiness that was released and then failed.
///
/// Recorded on the readiness rather than returned from [`Readiness::fail`],
/// because a failure after the release reaches the dependants through their
/// tokens rather than through a return value: by the time the service's body
/// calls `fail`, the dependants have already been released and are running, and
/// there is no caller left to hand a value to.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FailAfterReady {
    /// The generation that was released and then failed.
    generation: Generation,
    /// What the service said.
    reason: Arc<str>,
}

impl FailAfterReady {
    /// The generation that was released and then failed.
    #[must_use]
    pub const fn generation(&self) -> Generation {
        self.generation
    }

    /// What the service said.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// A readiness fact: one instance of a long-lived service, and the dependants
/// waiting on it.
///
/// Cheap to clone, and every clone is the same fact: one state machine, one slot
/// per admitted dependant, one released generation. Two clones are one readiness,
/// which is what lets a service body hold one while a dependant awaits it
/// without either owning the other's lifetime.
///
/// `T` is the evidence the readiness carries — a bound address, a captured
/// output line, a service name. It is the *same* value for every dependant, read
/// through [`Ready::awaited`], so a service reachable only through what it
/// printed hands that print to its dependants instead of making each of them
/// re-read the child's output.
///
/// # A readiness is per-instance, not per-service
///
/// A restart is a **new** [`Readiness`] at a **new** [`Generation`], never the
/// same readiness re-signalled. That is what makes a stale signal refused rather
/// than accepted: the old instance holds a handle to a readiness that has
/// already settled, and any signal it sends is refused against that readiness's
/// own released generation. See [`Readiness::fail`] for the
/// alternative, where one readiness outlives its release and records the failure
/// on itself.
#[derive(Debug)]
pub struct Readiness<T> {
    /// The shared state. Behind an `Arc` so a clone is a pointer copy and the
    /// state has one owner however many handles exist.
    inner: Arc<State<T>>,
}

/// The state behind one [`Readiness`], shared by every clone of it.
#[derive(Debug)]
struct State<T> {
    /// What this readiness is called, for every error it raises.
    what: Arc<str>,
    /// How many dependants may be admitted. One or more; a readiness that
    /// admitted none could never release one.
    cap: usize,
    /// The instance this readiness is bound to.
    ///
    /// Behind a lock rather than an atomic, because every decision below
    /// compares the generation and then writes the state, and a compare and a
    /// write that are not one decision is how two signals both decide they are
    /// current. The critical sections are a few plain reads and writes with no
    /// user code between them, and no `.await` happens while one is held.
    status: Mutex<Status>,
    /// How many dependants have been admitted. Charged before a slot is taken,
    /// and refused outright when the charge would exceed `cap`.
    admitted: AtomicU64,
    /// Whether this readiness ever released before it failed.
    ///
    /// Set inside the same critical section that writes
    /// [`Status::Released`], so "failed after ready" and "failed before ready"
    /// cannot disagree. One flag rather than a third `Status` arm because the two
    /// failures are the same *current* state with a different history, and
    /// modelling the history as a state would make every read of the current
    /// state branch on something that happened in the past.
    released_once: AtomicBool,
    /// The tokens of the dependants admitted so far, each one cancelled when a
    /// failure lands after the release.
    ///
    /// Bounded by `cap` because a slot is claimed before a token is pushed, and
    /// a token is never pushed without a slot: the two are one step in
    /// [`Readiness::admit`].
    dependants: Mutex<Vec<CancellationToken>>,
    /// What this readiness settled on, once.
    ///
    /// A `watch` carries the *value* rather than an event, so a dependant that
    /// subscribes after the release still reads it: there is no window in which a
    /// signal lands between "check the state" and "subscribe", and therefore no
    /// way to hang forever having missed it. That is the whole reason this is a
    /// `watch` and not a `Notify`.
    settled: watch::Sender<Settled<T>>,
}

/// How a [`Readiness`] currently stands.
///
/// A sum type rather than a `bool` plus a generation, because "not yet ready",
/// "ready at generation 3" and "failed at generation 2" are three states a
/// boolean cannot hold: the second still needs its generation and the third
/// still needs its reason, and a `Ready(Option<Generation>)` would make
/// "failed" and "released at no generation" the same value.
#[derive(Debug)]
enum Status {
    /// The bound instance has not settled yet.
    Pending(Generation),
    /// The bound instance released its dependants at this generation.
    Released(Generation),
    /// The bound instance failed at this generation, with this reason.
    Failed {
        /// The generation that failed.
        generation: Generation,
        /// What the service said.
        reason: Arc<str>,
    },
    /// The bound instance was shut down. Nothing further may be signalled.
    Closed(Generation),
}

impl Status {
    /// The generation this state is bound to, whichever arm it is.
    const fn generation(&self) -> Generation {
        match *self {
            Self::Pending(generation) | Self::Released(generation) | Self::Closed(generation) => {
                generation
            }
            Self::Failed { generation, .. } => generation,
        }
    }
}

/// The one fact a [`Readiness`] settled on, as a value rather than an event.
///
/// Every receiver of this channel sees the same payload, and the payload is
/// what a late subscriber reads — which is the property that makes the wait
/// unconditional: there is no "you missed it" arm. `Clone` is hand-written
/// rather than derived so the clone stops compiling the moment a variant stops
/// being `Clone`, which is how a new payload type would learn it owes a clone
/// here rather than in a `Debug` line.
#[derive(Debug)]
enum Settled<T> {
    /// Not settled yet. A dependant waits for the change.
    Waiting,
    /// The instance released its dependants, carrying this evidence.
    Ready {
        /// The generation that was released.
        generation: Generation,
        /// The evidence, shared with every dependant.
        value: Arc<T>,
    },
    /// The instance failed.
    Failed {
        /// The generation that failed.
        generation: Generation,
        /// What the service said.
        reason: Arc<str>,
    },
    /// The instance was shut down.
    Closed {
        /// The generation that shut down.
        generation: Generation,
    },
}

impl<T: Clone> Clone for Settled<T> {
    /// Every field is an `Arc`, a `Generation` or a unit, so the clone is a
    /// pointer copy per field and shares every payload rather than copying one.
    fn clone(&self) -> Self {
        match *self {
            Self::Waiting => Self::Waiting,
            Self::Ready {
                generation,
                ref value,
            } => Self::Ready {
                generation,
                value: Arc::clone(value),
            },
            Self::Failed {
                generation,
                ref reason,
            } => Self::Failed {
                generation,
                reason: Arc::clone(reason),
            },
            Self::Closed { generation } => Self::Closed { generation },
        }
    }
}

impl<T> Clone for Readiness<T> {
    /// A pointer copy: every clone shares one state, so a clone is the *same*
    /// readiness rather than a second one that could be released independently.
    /// There is no `T: Clone` bound, because nothing owned by the payload is
    /// copied — the `Arc` is the whole point.
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T: Clone> Readiness<T> {
    /// A readiness for the service called `what`, at generation
    /// [`Generation::FIRST`], admitting up to [`MAX_DEPENDANTS`] dependants.
    ///
    /// # Errors
    ///
    /// [`ReadinessError::InvalidName`] when `what` is empty or longer than
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
    /// [`ReadinessError::InvalidName`] when `what` is empty or longer than
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
        self.inner
            .dependants
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Signal that the instance at `generation` is ready, carrying `value`.
    ///
    /// Releases every dependant already admitted and every one admitted after.
    /// A second signal is refused rather than releasing twice: see
    /// [`ReadinessError::AlreadyReady`].
    ///
    /// # Errors
    ///
    /// [`ReadinessError::UnknownGeneration`] when `generation` is not the one
    /// this readiness is bound to; [`ReadinessError::StaleGeneration`] when it
    /// is older than the generation this readiness has settled at. A readiness
    /// that has already released refuses any further signal with
    /// [`ReadinessError::AlreadyReady`].
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
    /// [`Readiness::wait`]. A failure **after** the release also
    /// cancels every dependant that was admitted, which is what makes a service
    /// that dies mid-flight reach the dependants still running instead of leaving
    /// them talking to a dead process — see
    /// [`Readiness::fail`] and the module docs.
    ///
    /// Idempotent in the same way [`Readiness::ready`] is: a second failure for
    /// the same generation is [`ReadinessError::AlreadyFailed`] and cancels
    /// nothing further.
    ///
    /// # Errors
    ///
    /// As [`Readiness::ready`] on generation mismatch; a released generation may
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
    /// The distinct fact from [`Readiness::fail`]: a shut-down readiness was
    /// never usable, so a dependant waiting on one is *stopped* rather than told
    /// a service failed. A service being torn down is not a service failure, and
    /// collapsing the two would make a routine restart look like an outage. A
    /// dependant that was already released is cancelled too, for the same reason
    /// a failure after the release cancels it: it was pointed at something that
    /// is gone.
    ///
    /// # Errors
    ///
    /// As [`Readiness::fail`], for the same reason: a signal about a generation
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
    /// [`Clock`](super::Clock), so a scope built on a caller-advanceable clock
    /// reports the same [`FlowError::TimedOut`] an advance past the budget
    /// produces that a real overrun produces.
    ///
    /// # Errors
    ///
    /// [`ReadinessError::DependantsFull`] at the declared cap; the readiness's
    /// own failure or shutdown through [`ReadinessError`]; [`FlowError::Cancelled`]
    /// when `scope` is stopped; [`FlowError::TimedOut`] when `limit` passes. The
    /// last two are the scope's, not the readiness's, so an enclosing
    /// [`retry`](super::retry) does not retry a cancelled dependant.
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

    /// The blocking half of [`Readiness::wait`], already charged a slot.
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

    /// Turn one settled value into a [`Ready`] or a located refusal.
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
        // The cap is compared in the counter's own width rather than widened back, so
        // the bound is enforced on `u64` and a cap too large for that width
        // admits everything rather than admitting nothing. Every real cap fits:
        // `MAX_DEPENDANTS` is 65 536, and a caller that asked for more than a
        // `u64` of dependants is asking for a number, not a bound.
        let cap_as_count = u64::try_from(cap).unwrap_or(u64::MAX);
        let charged = self
            .inner
            .admitted
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
                (count < cap_as_count).then_some(count.saturating_add(1))
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
        self.inner
            .dependants
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(scope.token().clone());
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
        let tokens = std::mem::take(
            &mut *self
                .inner
                .dependants
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
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
        self.inner
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
