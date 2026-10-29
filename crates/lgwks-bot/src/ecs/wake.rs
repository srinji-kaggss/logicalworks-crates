//! Wakes and deadlines: which sources a tick polls, and when the world needs a
//! tick at all (ADR 0006 R5).
//!
//! Every source declares a [`Cadence`]. A tick polls a `Tick` source always, an
//! `Every` source once its period has passed on the bot clock, and a `Wake`
//! source only when its [`WakeHandle`] was signalled since its last poll. The
//! driver (`drive`, with the HTTP edge that runs it) asks when the next tick is
//! due and parks until a wake or that deadline, so a world with nothing due
//! runs no tick.
//!
//! Wakes coalesce: a source woken twice before the tick that polls it is polled
//! once, because the poll reads the source as it stands, and the second wake
//! described a state the poll already sees.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::Waker;
use std::time::Duration;

// `self` is load-bearing: the `Resource` derive expands to `bevy_ecs::…` paths.
use lgwks_deps::bevy_ecs::{self, prelude::Resource};

use crate::error::BotError;
use crate::verb::Cadence;

use super::bot::EcsBot;

/// The driver's half: when the next tick is due, and the future it parks on.
#[cfg(feature = "edge")]
mod drive;
#[cfg(all(test, feature = "edge"))]
mod sim_wake;
#[cfg(feature = "edge")]
pub(crate) use drive::NextTick;

/// What the world shares with every [`WakeHandle`] it gave out.
#[derive(Debug)]
struct Shared {
    /// One flag per chain: woken since the tick that last took its wakes.
    pending: Box<[AtomicBool]>,
    /// Set by any wake, cleared when a tick takes the wakes.
    any: AtomicBool,
    /// The driver parked until a wake, if one is.
    parked: Mutex<Option<Waker>>,
}

impl Shared {
    /// The parked driver's slot, whatever a panicking holder left in it.
    fn parked(&self) -> MutexGuard<'_, Option<Waker>> {
        match self.parked.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

/// The signal that one `Wake` source has something new to read.
///
/// Cheap to clone, and `Send + Sync`, so it goes wherever the source's world
/// is pushed at it: a socket reader, another thread, an HTTP handler.
#[derive(Clone, Debug)]
pub struct WakeHandle {
    /// The world's wake state.
    shared: Arc<Shared>,
    /// The chain whose source this handle wakes.
    chain: usize,
}

impl WakeHandle {
    /// Ask the world to poll this source on its next tick, and to run that tick
    /// now. A wake already pending costs one atomic load and nothing else.
    pub fn wake(&self) {
        let Some(flag) = self.shared.pending.get(self.chain) else {
            return;
        };
        if flag.load(Ordering::Acquire) || flag.swap(true, Ordering::AcqRel) {
            return;
        }
        self.shared.any.store(true, Ordering::Release);
        let parked = self.shared.parked().take();
        if let Some(driver) = parked {
            driver.wake();
        }
    }
}

/// Each chain's cadence, its next `Every` deadline, and the world's wakes.
#[derive(Resource, Debug)]
pub(super) struct Cadences {
    /// Each chain's declared cadence, in chain order.
    kinds: Box<[Cadence]>,
    /// Each `Every` chain's next deadline, in bot-clock nanoseconds; zero for a
    /// chain never polled, which is due at once.
    due: Box<[AtomicU64]>,
    /// The wakes, shared with every handle.
    shared: Arc<Shared>,
}

impl Cadences {
    /// The cadences `kinds` declare. Every `Wake` chain starts woken, so the
    /// first tick reads its baseline.
    ///
    /// # Errors
    ///
    /// [`BotError::IncompleteSpec`] for an `Every` with a zero period, which
    /// would make every instant a deadline.
    pub(super) fn new(kinds: Vec<Cadence>) -> Result<Self, BotError> {
        if kinds.contains(&Cadence::Every(Duration::ZERO)) {
            let refusal = Err(BotError::IncompleteSpec {
                field: "cadence",
                cause: String::from("a source declared Every with a zero period"),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "cadences: returning an error to the caller");
            return refusal;
        }
        let pending = kinds
            .iter()
            .map(|kind| AtomicBool::new(*kind == Cadence::Wake))
            .collect();
        let any = AtomicBool::new(kinds.contains(&Cadence::Wake));
        Ok(Self {
            due: kinds.iter().map(|_| AtomicU64::new(0)).collect(),
            kinds: kinds.into_boxed_slice(),
            shared: Arc::new(Shared {
                pending,
                any,
                parked: Mutex::new(None),
            }),
        })
    }

    /// Move the wakes signalled since the last tick into `woken`, one flag per
    /// chain, and clear them, so a wake that lands during this tick's polls
    /// asks for the next one.
    pub(super) fn take_wakes(&self, woken: &mut Vec<bool>) {
        self.shared.any.store(false, Ordering::Release);
        woken.clear();
        woken.extend(
            self.shared
                .pending
                .iter()
                .map(|flag| flag.swap(false, Ordering::AcqRel)),
        );
    }

    /// Whether `chain` is due a poll on the tick read at `now`, given the wakes
    /// that tick took. A due `Every` chain's next deadline is set here, so a
    /// chain is due once per period whatever its poll returns.
    pub(super) fn poll_due(&self, chain: usize, now: Duration, woken: &[bool]) -> bool {
        match self.kinds.get(chain).copied() {
            Some(Cadence::Every(period)) => {
                let Some(due) = self.due.get(chain) else {
                    return true;
                };
                let now = nanos(now);
                if due.load(Ordering::Acquire) > now {
                    return false;
                }
                due.store(now.saturating_add(nanos(period)), Ordering::Release);
                true
            }
            Some(Cadence::Wake) => matches!(woken.get(chain).copied(), Some(true)),
            _ => true,
        }
    }
}

/// A duration as whole nanoseconds; a span past `u64`'s 584 years saturates at
/// the last instant `u64` can name, which no deadline reaches.
fn nanos(span: Duration) -> u64 {
    span.as_secs()
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::from(span.subsec_nanos()))
}

impl WakeHandle {
    /// The handle that wakes the source of `world`'s chain `chain`, counted in
    /// declaration order, or `None` when that source did not declare
    /// [`Cadence::Wake`].
    #[must_use]
    pub fn for_source(world: &EcsBot, chain: usize) -> Option<Self> {
        let cadences = world.world.resource::<Cadences>();
        (cadences.kinds.get(chain) == Some(&Cadence::Wake)).then(|| Self {
            shared: Arc::clone(&cadences.shared),
            chain,
        })
    }
}
