//! The driver's half of the wakes: when the next tick is due, and the future a
//! driver parks on until a source is woken.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll};
use std::time::Duration;

use crate::clock::Clock;
use crate::verb::Cadence;

use super::super::bot::EcsBot;
use super::super::ledger::Ledger;
use super::super::resources::Policy;
use super::{Cadences, Shared, nanos};

/// When the world's next tick is due, as [`EcsBot::next_tick`] reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NextTick {
    /// A source was woken, or a deadline has passed: tick now.
    Now,
    /// Nothing is due for this long on the bot clock.
    In(Duration),
    /// Nothing is due until a source is woken.
    Idle,
}

impl Cadences {
    /// The earliest `Every` deadline, in bot-clock nanoseconds.
    fn earliest(&self) -> Option<u64> {
        self.kinds
            .iter()
            .zip(&self.due)
            .filter(|&(kind, _)| matches!(*kind, Cadence::Every(_)))
            .map(|(_, due)| due.load(Ordering::Acquire))
            .min()
    }
}

/// Resolves once a source is woken; see [`EcsBot::woken`].
#[derive(Debug)]
pub(crate) struct Woken<'a> {
    /// The world's wakes.
    shared: &'a Shared,
}

impl Future for Woken<'_> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        // Parked before the flag is read: a wake that lands between the two
        // finds the waker and wakes it, and one that landed before is seen here.
        *self.shared.parked() = Some(context.waker().clone());
        if self.shared.any.load(Ordering::Acquire) {
            return Poll::Ready(());
        }
        Poll::Pending
    }
}

impl EcsBot {
    /// When this world's next tick is due on its bot clock: now, when a source
    /// was woken or a deadline has passed; in the span to the earliest `Every`
    /// deadline or retry backoff still ahead; or idle, when nothing is due
    /// until a wake.
    pub(crate) fn next_tick(&self) -> NextTick {
        let cadences = self.world.resource::<Cadences>();
        if cadences.shared.any.load(Ordering::Acquire) {
            return NextTick::Now;
        }
        let now = self.world.resource::<Policy>().schedule.clock.now();
        let retry = self.world.non_send::<Ledger>().next_retry(now);
        let deadline = match (cadences.earliest(), retry.map(nanos)) {
            (Some(every), Some(retry)) => Some(every.min(retry)),
            (every, retry) => every.or(retry),
        };
        let now = nanos(now);
        match deadline {
            None => NextTick::Idle,
            Some(at) if at <= now => NextTick::Now,
            Some(at) => NextTick::In(Duration::from_nanos(at.saturating_sub(now))),
        }
    }

    /// The bot clock every deadline above is read on, which the driver waits on.
    pub(crate) fn clock(&self) -> &Clock {
        &self.world.resource::<Policy>().schedule.clock
    }

    /// Resolves once any source of this world is woken, at once if one already
    /// was: what a driver awaits while [`Self::next_tick`] is not `Now`.
    pub(crate) fn woken(&self) -> Woken<'_> {
        Woken {
            shared: &self.world.resource::<Cadences>().shared,
        }
    }
}
