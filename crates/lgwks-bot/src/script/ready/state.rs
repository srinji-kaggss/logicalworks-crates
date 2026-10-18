//! The shared state behind one readiness: its status, its settled value and its admitted dependants.
//! Split from `ready.rs` (#526).

use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};

use crate::rt::sync::CancellationToken;
use crate::rt::sync::watch;

use super::Generation;

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
/// through [`Ready::awaited`](super::Ready::awaited), so a service reachable only through what it
/// printed hands that print to its dependants instead of making each of them
/// re-read the child's output.
///
/// # A readiness is per-instance, not per-service
///
/// A restart is a **new** [`Readiness`](super::Readiness) at a **new** [`Generation`](super::Generation), never the
/// same readiness re-signalled. That is what makes a stale signal refused rather
/// than accepted: the old instance holds a handle to a readiness that has
/// already settled, and any signal it sends is refused against that readiness's
/// own released generation. See [`Readiness::fail`](super::Readiness::fail) for the
/// alternative, where one readiness outlives its release and records the failure
/// on itself.
#[derive(Debug)]
pub struct Readiness<T> {
    /// The shared state. Behind an `Arc` so a clone is a pointer copy and the
    /// state has one owner however many handles exist.
    pub(super) inner: Arc<State<T>>,
}

/// The state behind one [`Readiness`](super::Readiness), shared by every clone of it.
#[derive(Debug)]
pub(super) struct State<T> {
    /// What this readiness is called, for every error it raises.
    pub(super) what: Arc<str>,
    /// How many dependants may be admitted. One or more; a readiness that
    /// admitted none could never release one.
    pub(super) cap: usize,
    /// The instance this readiness is bound to.
    ///
    /// Behind a lock rather than an atomic, because every decision below
    /// compares the generation and then writes the state, and a compare and a
    /// write that are not one decision is how two signals both decide they are
    /// current. The critical sections are a few plain reads and writes with no
    /// user code between them, and no `.await` happens while one is held.
    pub(super) status: Mutex<Status>,
    /// How many dependants have been admitted. Charged before a slot is taken,
    /// and refused outright when the charge would exceed `cap`.
    pub(super) admitted: AtomicU64,
    /// Whether this readiness ever released before it failed.
    ///
    /// Set inside the same critical section that writes
    /// [`Status::Released`], so "failed after ready" and "failed before ready"
    /// cannot disagree. One flag rather than a third `Status` arm because the two
    /// failures are the same *current* state with a different history, and
    /// modelling the history as a state would make every read of the current
    /// state branch on something that happened in the past.
    pub(super) released_once: AtomicBool,
    /// The tokens of the dependants admitted so far, each one cancelled when a
    /// failure lands after the release.
    ///
    /// Bounded by `cap` because a slot is claimed before a token is pushed, and
    /// a token is never pushed without a slot: the two are one step in
    /// [`Readiness::admit`](super::Readiness::admit).
    pub(super) dependants: Mutex<Vec<CancellationToken>>,
    /// What this readiness settled on, once.
    ///
    /// A `watch` carries the *value* rather than an event, so a dependant that
    /// subscribes after the release still reads it: there is no window in which a
    /// signal lands between "check the state" and "subscribe", and therefore no
    /// way to hang forever having missed it. That is the whole reason this is a
    /// `watch` and not a `Notify`.
    pub(super) settled: watch::Sender<Settled<T>>,
}

/// How a [`Readiness`](super::Readiness) currently stands.
///
/// A sum type rather than a `bool` plus a generation, because "not yet ready",
/// "ready at generation 3" and "failed at generation 2" are three states a
/// boolean cannot hold: the second still needs its generation and the third
/// still needs its reason, and a `Ready(Option<Generation>)` would make
/// "failed" and "released at no generation" the same value.
#[derive(Debug)]
pub(super) enum Status {
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
    pub(super) const fn generation(&self) -> Generation {
        match *self {
            Self::Pending(generation) | Self::Released(generation) | Self::Closed(generation) => {
                generation
            }
            Self::Failed { generation, .. } => generation,
        }
    }
}

/// The one fact a [`Readiness`](super::Readiness) settled on, as a value rather than an event.
///
/// Every receiver of this channel sees the same payload, and the payload is
/// what a late subscriber reads — which is the property that makes the wait
/// unconditional: there is no "you missed it" arm. `Clone` is hand-written
/// rather than derived so the clone stops compiling the moment a variant stops
/// being `Clone`, which is how a new payload type would learn it owes a clone
/// here rather than in a `Debug` line.
#[derive(Debug)]
pub(super) enum Settled<T> {
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
