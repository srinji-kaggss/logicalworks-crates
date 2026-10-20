//! One effect per caller key, on a receiver that keeps no idempotency key.
//!
//! [`idempotent`](crate::idempotent) covers a receiver that deduplicates on a
//! key the caller sends. Most receivers do not: an issue tracker, a chat
//! channel or a mail relay holds a second copy of anything sent twice. For
//! them the key has to travel *inside* the effect (a hidden trailer, a
//! reference field), and the discipline that keeps one effect per key is the
//! same whatever the receiver is. This module is that discipline, written once;
//! an adapter supplies only what is its own through [`KeyedReceiver`]:
//!
//! - **find** the effect a key names, however the receiver can be read;
//! - **create** it once;
//! - say whether a failed create **may have landed**;
//! - state how long a request the receiver accepted may still be **in flight**.
//!
//! `gh::issue` is the first adapter.
//!
//! # The guarantee and what it costs
//!
//! [`open_once`] reads the key first and answers [`Settlement::Found`] when an
//! earlier run already made the effect. When it finds nothing it does not
//! create at once: a run killed between sending its create and reading the
//! answer leaves that create in flight on the receiver, and a rerun that
//! creates before it lands makes the duplicate this module exists to prevent.
//! So the open waits, on the flow's [`Clock`], until the receiver's
//! [`in_flight_bound`](KeyedReceiver::in_flight_bound) has passed since the
//! open began, reads again, and only then creates. A new key costs that bound;
//! a key that exists costs one read.
//!
//! A create that answers is [`Settlement::Created`] and is never repeated. One
//! that fails, if the adapter says it created nothing, is
//! [`KeyedError::Refused`]. One that may have landed is read back over a
//! bounded schedule: found, it is [`Settlement::Reconciled`]; not found, it is
//! [`KeyedError::Indeterminate`], never a second create. A read that fails
//! during that read-back is kept and the schedule goes on, because one failed
//! read is not evidence that nothing landed.
//!
//! # Bounds, stated
//!
//! The read-back makes [`SETTLE_SCANS`] reads, the first at once and the waits
//! between them starting at 250 ms and doubling to 4 s (15.75 s in all), and
//! keeps reading past that until the in-flight bound has passed since the
//! create was sent. Two runs that open one key at the same instant on hosts
//! that share nothing can both find nothing and both create: no receiver this
//! serves offers a conditional create, and every later run answers the first
//! of them, which is the adapter's find to choose.

use std::fmt;
use std::time::Duration;

use crate::clock::Clock;
use crate::error::BotError;

/// How many reads the read-back after a lost create answer makes at least.
const SETTLE_SCANS: u8 = 8;

/// The wait before the read-back's second read; each later wait doubles.
const FIRST_SETTLE_DELAY: Duration = Duration::from_millis(250);

/// The longest single wait between read-back reads.
const MAX_SETTLE_DELAY: Duration = Duration::from_secs(4);

/// What a failed create establishes about the effect it was making.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CreateFailure {
    /// The receiver holds nothing from this attempt: it never started, or the
    /// receiver refused it as a request. A rerun may create.
    NothingLanded,
    /// The attempt reached the receiver and its answer is not one, so the
    /// effect may exist or may still be landing.
    MayHaveLanded,
}

/// What a receiver supplies for [`open_once`] to keep one effect per key.
///
/// Nothing here schedules, waits or decides: the adapter reads, writes and
/// classifies, and the discipline around those three is the module's.
pub trait KeyedReceiver {
    /// What one open asks for, its key included.
    type Draft;
    /// The effect as the receiver holds it.
    type Held;
    /// Why a read or a create failed.
    type Error: fmt::Display;

    /// The effect `draft`'s key names, or `None` when the receiver holds none.
    ///
    /// When a key ever names two effects, return the one it was first made
    /// as, so every run answers the same one.
    fn find(
        &self,
        draft: &Self::Draft,
    ) -> impl Future<Output = Result<Option<Self::Held>, Self::Error>> + Send;

    /// Make the effect once, confirmed from the receiver's own answer.
    fn create(
        &self,
        draft: &Self::Draft,
    ) -> impl Future<Output = Result<Self::Held, Self::Error>> + Send;

    /// Whether `failure`, from [`Self::create`], may have left an effect behind.
    fn create_failure(&self, failure: &Self::Error) -> CreateFailure;

    /// How long a request the receiver accepted may still land after it was
    /// sent: the receiver's own bound on processing one request.
    fn in_flight_bound(&self) -> Duration;
}

/// How an open came to its effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Settlement {
    /// This run created it, and the receiver's answer to the create shows it.
    Created,
    /// An earlier run with this key had made it; nothing was created.
    Found,
    /// This run's create lost its answer, and the read-back found the effect.
    Reconciled,
}

impl Settlement {
    /// The lowercase word a report prints.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Found => "found",
            Self::Reconciled => "reconciled",
        }
    }
}

/// Why an open did not settle on an effect.
#[derive(Debug)]
#[non_exhaustive]
pub enum KeyedError<E> {
    /// A read before any create failed, so nothing was created.
    Unread(E),
    /// The create failed and the adapter says it landed nothing.
    Refused(E),
    /// The create may have landed and the read-back did not find it.
    Indeterminate {
        /// Why the create's answer is not one.
        lost: E,
        /// The last read-back read that failed, when one did.
        unread: Option<E>,
    },
}

impl<E> KeyedError<E> {
    /// The variant's name, for a trace that need not render the adapter's error.
    const fn kind(&self) -> &'static str {
        match *self {
            Self::Unread(_) => "unread",
            Self::Refused(_) => "refused",
            Self::Indeterminate { .. } => "indeterminate",
        }
    }
}

impl<E: fmt::Display> KeyedError<E> {
    /// This failure as the crate error a verb returns, attributed to `domain`.
    ///
    /// An effect that may or may not exist is always
    /// [`BotError::EffectIndeterminate`], whatever the adapter's own mapping
    /// would say, so no adapter can turn a possible duplicate into a retry.
    pub fn into_bot_error(self, domain: &str, adapter: impl FnOnce(E) -> BotError) -> BotError {
        match self {
            Self::Unread(failure) | Self::Refused(failure) => adapter(failure),
            Self::Indeterminate { lost, unread } => BotError::EffectIndeterminate {
                domain: domain.to_owned(),
                cause: match unread {
                    None => format!(
                        "{lost}; the read-back did not find the key, so the effect may or may not \
                         exist, and a rerun with this key reads before it creates"
                    ),
                    Some(unread) => format!("{lost}; a read-back read failed too: {unread}"),
                },
            },
        }
    }
}

/// The read-back's schedule: at least [`SETTLE_SCANS`] reads, and reads until
/// `until` on the clock.
struct ReadBack {
    /// The wait before the next read.
    next: Duration,
    /// How many reads have been made.
    made: u8,
    /// The clock time the receiver's in-flight bound runs out.
    until: Duration,
}

impl ReadBack {
    /// Record one read, and return the wait before the next, or `None` once
    /// both bounds are spent.
    fn spent(&mut self, now: Duration) -> Option<Duration> {
        self.made = self.made.saturating_add(1);
        if self.made >= SETTLE_SCANS && now >= self.until {
            return None;
        }
        let wait = self.next;
        self.next = self.next.saturating_mul(2).min(MAX_SETTLE_DELAY);
        Some(wait)
    }
}

/// Find `draft`'s effect on `receiver`, or create it once.
///
/// Every wait is spent on `clock`, the flow's own: a wall clock waits the real
/// schedule, and a caller-advanceable clock spends it without waiting.
///
/// # Errors
///
/// [`KeyedError::Unread`] when a read before the create fails;
/// [`KeyedError::Refused`] when the create failed and landed nothing;
/// [`KeyedError::Indeterminate`] when the create may have landed and the
/// read-back could not find it.
pub async fn open_once<R: KeyedReceiver + Sync>(
    receiver: &R,
    draft: &R::Draft,
    clock: &Clock,
) -> Result<(R::Held, Settlement), KeyedError<R::Error>>
where
    R::Draft: Sync,
{
    let began = clock.now();
    let bound = receiver.in_flight_bound();
    if let Some(found) = receiver.find(draft).await.map_err(KeyedError::Unread)? {
        return Ok((found, Settlement::Found));
    }
    // An earlier run's create may still be in flight: it was sent before this
    // open began, so once the bound has passed since then it has landed or
    // never will, and the read after the wait sees it if it did.
    let fence = began.saturating_add(bound).saturating_sub(clock.now());
    if !fence.is_zero() {
        clock.sleep(fence).await;
        if let Some(found) = receiver.find(draft).await.map_err(KeyedError::Unread)? {
            return Ok((found, Settlement::Found));
        }
    }
    let sent = clock.now();
    let lost = match receiver.create(draft).await {
        Ok(created) => return Ok((created, Settlement::Created)),
        Err(failure) => match receiver.create_failure(&failure) {
            CreateFailure::NothingLanded => {
                lgwks_std::trace::debug!(error = %failure, "keyed: the create landed nothing");
                return Err(KeyedError::Refused(failure));
            }
            CreateFailure::MayHaveLanded => failure,
        },
    };
    let mut read_back = ReadBack {
        next: FIRST_SETTLE_DELAY,
        made: 0,
        until: sent.saturating_add(bound),
    };
    let mut unread = None;
    loop {
        match receiver.find(draft).await {
            Ok(Some(found)) => return Ok((found, Settlement::Reconciled)),
            Ok(None) => {}
            Err(failure) => {
                lgwks_std::trace::debug!(error = %failure, "keyed: a read-back read failed; the schedule goes on");
                unread = Some(failure);
            }
        }
        match read_back.spent(clock.now()) {
            Some(wait) => clock.sleep(wait).await,
            None => {
                let failure = KeyedError::Indeterminate { lost, unread };
                lgwks_std::trace::debug!(
                    error = failure.kind(),
                    "open_once: returning an error to the caller"
                );
                return Err(failure);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_read_back_reads_eight_times_over_fifteen_and_three_quarter_seconds() {
        let mut read_back = ReadBack {
            next: FIRST_SETTLE_DELAY,
            made: 0,
            until: Duration::ZERO,
        };
        let mut waited = Duration::ZERO;
        let mut reads = 1_u32;
        while let Some(wait) = read_back.spent(waited) {
            waited += wait;
            reads += 1;
        }
        assert_eq!(reads, 8);
        assert_eq!(waited, Duration::from_millis(15_750));
    }

    #[test]
    fn the_read_back_reads_on_until_the_in_flight_bound_has_passed() {
        let until = Duration::from_secs(30);
        let mut read_back = ReadBack {
            next: FIRST_SETTLE_DELAY,
            made: 0,
            until,
        };
        let mut waited = Duration::ZERO;
        while let Some(wait) = read_back.spent(waited) {
            waited += wait;
        }
        assert!(waited >= until, "stopped at {waited:?}, before {until:?}");
        assert!(waited < until + MAX_SETTLE_DELAY);
    }
}
