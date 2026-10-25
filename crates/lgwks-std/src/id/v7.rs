//! `v7` owns time-ordered identifiers: UUID version 7 (RFC 9562 section 5.7)
//! with the dedicated-counter ordering of section 6.2, method 1, so a key that
//! sorts is also a key that was issued in that order (INV-ID-2).
//!
//! # Layout
//!
//! ```text
//!   0..48   unix_ts_ms  milliseconds since 1970-01-01T00:00:00Z, big-endian
//!  48..52   version     0b0111
//!  52..64   counter     12 bits, one step per identifier within a stamp
//!  64..66   variant     0b10
//!  66..128  random      62 bits of OS entropy
//! ```
//!
//! # What a generator promises
//!
//! Every identifier a [`V7Generator`] issues is strictly greater, as a value
//! and as text, than the one before it. The rule is one comparison:
//!
//! - **A later millisecond** takes that millisecond as its stamp and reseeds the
//!   counter from entropy with the counter's top bit clear, so 2,048 more
//!   identifiers fit in the stamp before it overflows.
//! - **The same millisecond, or a clock that stepped backwards**, keeps the last
//!   stamp and adds one to the counter. A backwards step is the same case as the
//!   same millisecond: taking the earlier stamp would issue a value that sorts
//!   before one already issued.
//! - **A counter that would pass 4095** borrows the next millisecond and reseeds.
//!   It never waits on the clock and never repeats a value, so a burst past
//!   2,048,000 identifiers a second only moves the stamp ahead of the wall
//!   clock, by one millisecond per 2,048 identifiers past the first 2,048.
//! - **A stamp past 48 bits** (the year 10889) is refused with
//!   [`V7Error::TimestampOutOfRange`], never wrapped, and the generator is left
//!   as it was.
//!
//! The order holds for one generator, and [`crate::id::Uuid::new_v7`] is one
//! generator for the whole process, so every thread and every tenant of the
//! process shares it. Across a restart there is no memory: the first stamp is
//! the wall clock, so order across a restart rests on the clock not having
//! stepped back past the last stamp issued. A host that needs more keeps the
//! last identifier it issued and refuses a lower one.
//!
//! The generator itself is private to the `id` module. Its clock and entropy
//! are the two arguments of [`V7Generator::issue_at`], so the seeded simulations
//! in `id::sim_v7` drive the whole ordering rule with a drawn clock and drawn
//! entropy, the way `Uuid::new_v4` is driven through its entropy seam.

use std::error::Error;
use std::fmt;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::Uuid;
use crate::random::{self, EntropyError};

/// The largest millisecond a version 7 identifier holds: 48 bits.
const MAX_UNIX_MS: u64 = 0xFFFF_FFFF_FFFF;

/// The largest value of the 12-bit counter.
const COUNTER_MAX: u16 = 0x0FFF;

/// The bits of a reseeded counter that come from entropy. The top bit is left
/// clear so 2,048 increments remain before the counter overflows (RFC 9562
/// section 6.2, method 1).
const COUNTER_SEED_MASK: u16 = 0x07FF;

/// The entropy one identifier consumes: two bytes seed the counter and eight
/// fill the random field.
const ENTROPY_BYTES: usize = 10;

/// The version nibble, in the high four bits of the seventh byte.
const VERSION_BITS: u8 = 0x70;

/// The RFC variant, in the high two bits of the ninth byte.
const VARIANT_BITS: u8 = 0x80;

/// The mask that keeps the 62 random bits below the variant.
const RANDOM_HEAD_MASK: u8 = 0x3f;

/// The mask that keeps the counter's high four bits in the seventh byte.
const COUNTER_HEAD_MASK: u8 = 0x0f;

/// A source of strictly increasing UUID version 7 values.
///
/// Two integers of state: the stamp and counter of the last identifier issued.
/// The process owns one, behind [`Uuid::new_v7`](crate::id::Uuid::new_v7). The
/// clock and the entropy are inputs of [`V7Generator::issue_at`], so the
/// ordering rule runs without either.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct V7Generator {
    /// The stamp and counter of the last identifier issued, or `None` before
    /// the first.
    last: Option<(u64, u16)>,
}

impl V7Generator {
    /// A generator that has issued nothing.
    #[must_use]
    pub(super) const fn new() -> Self {
        Self { last: None }
    }

    /// Issues the next identifier for a clock reading of `now_ms` milliseconds
    /// since the Unix epoch and ten bytes of `entropy`.
    ///
    /// The first two bytes seed the counter when the stamp moves forward; the
    /// other eight are the random field. A refusal leaves the generator as it
    /// was, so the same call can be made again with a later clock.
    pub(super) fn issue_at(
        &mut self,
        now_ms: u64,
        entropy: [u8; ENTROPY_BYTES],
    ) -> Result<Uuid, V7Error> {
        let (stamp, counter) = self.next_position(now_ms, counter_seed(entropy))?;
        self.last = Some((stamp, counter));
        Ok(assemble(stamp, counter, entropy))
    }

    /// The stamp and counter of the next identifier, without recording them.
    ///
    /// A reading at or before the last stamp is the same case as the same
    /// millisecond, and cannot pass 48 bits because the last stamp did not; a
    /// later reading is checked against the width before it becomes a stamp.
    fn next_position(&self, now_ms: u64, seed: u16) -> Result<(u64, u16), V7Error> {
        match self.last {
            Some((last_stamp, last_counter)) if now_ms <= last_stamp => {
                step(last_stamp, last_counter, seed)
            }
            _ if now_ms > MAX_UNIX_MS => Err(V7Error::TimestampOutOfRange { unix_ms: now_ms }),
            _ => Ok((now_ms, seed)),
        }
    }
}

/// The counter a fresh stamp starts from: eleven entropy bits.
fn counter_seed(entropy: [u8; ENTROPY_BYTES]) -> u16 {
    let [high, low, ..] = entropy;
    u16::from_be_bytes([high, low]) & COUNTER_SEED_MASK
}

/// The stamp and counter after one more identifier in a stamp already issued:
/// the counter plus one, or the next millisecond with a reseeded counter once
/// the counter is spent.
fn step(stamp: u64, counter: u16, seed: u16) -> Result<(u64, u16), V7Error> {
    match counter.checked_add(1) {
        Some(next) if next <= COUNTER_MAX => Ok((stamp, next)),
        _ => match stamp.checked_add(1) {
            Some(borrowed) if borrowed <= MAX_UNIX_MS => Ok((borrowed, seed)),
            _ => Err(V7Error::TimestampOutOfRange {
                unix_ms: stamp.saturating_add(1),
            }),
        },
    }
}

/// The sixteen bytes of one identifier: the stamp, the version, the counter,
/// the variant and the random field.
fn assemble(stamp: u64, counter: u16, entropy: [u8; ENTROPY_BYTES]) -> Uuid {
    let [_, _, ts0, ts1, ts2, ts3, ts4, ts5] = stamp.to_be_bytes();
    let [counter_high, counter_low] = counter.to_be_bytes();
    let [_, _, rand0, rand1, rand2, rand3, rand4, rand5, rand6, rand7] = entropy;
    Uuid::from([
        ts0,
        ts1,
        ts2,
        ts3,
        ts4,
        ts5,
        VERSION_BITS | (counter_high & COUNTER_HEAD_MASK),
        counter_low,
        VARIANT_BITS | (rand0 & RANDOM_HEAD_MASK),
        rand1,
        rand2,
        rand3,
        rand4,
        rand5,
        rand6,
        rand7,
    ])
}

/// The wall clock in whole milliseconds since the Unix epoch.
fn wall_clock_ms() -> Result<u64, V7Error> {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(since) => Ok(since
            .as_secs()
            .saturating_mul(1000)
            .saturating_add(u64::from(since.subsec_millis()))),
        Err(before) => Err(V7Error::ClockBeforeEpoch {
            by: before.duration(),
        }),
    }
}

/// Reads the clock, then the entropy, refusing at the first that refuses so a
/// failed clock costs no entropy.
fn read_inputs(
    clock: impl FnOnce() -> Result<u64, V7Error>,
    entropy: impl FnOnce() -> Result<[u8; ENTROPY_BYTES], EntropyError>,
) -> Result<(u64, [u8; ENTROPY_BYTES]), V7Error> {
    let now_ms = clock()?;
    let drawn = entropy().map_err(V7Error::Entropy)?;
    Ok((now_ms, drawn))
}

/// The generator behind `Uuid::new_v7`, one per process.
static SHARED: Mutex<V7Generator> = Mutex::new(V7Generator::new());

/// Issues from the process-wide generator.
///
/// The clock and entropy are read before the lock is taken, so the lock covers
/// two integer comparisons. A thread that read an earlier millisecond and
/// locks later is clamped to the last stamp, not reordered. The lock's state is
/// two plain integers that a panic cannot leave torn, so a poisoned lock is
/// used as it stands.
pub(super) fn new_v7() -> Result<Uuid, V7Error> {
    let (now_ms, entropy) = read_inputs(wall_clock_ms, random::bytes)?;
    let mut generator = match SHARED.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    generator.issue_at(now_ms, entropy)
}

/// Why a version 7 identifier was not issued.
///
/// Variants are stable and machine-readable; callers match on them rather than
/// on the message text, and `#[non_exhaustive]` keeps a future reason an
/// additive change.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum V7Error {
    /// The stamp does not fit in 48 bits, either because the clock read past
    /// the year 10889 or because a spent counter borrowed past it.
    TimestampOutOfRange {
        /// The millisecond that did not fit.
        unix_ms: u64,
    },
    /// The wall clock reads before 1970-01-01T00:00:00Z.
    ClockBeforeEpoch {
        /// How far before the epoch the clock reads.
        by: Duration,
    },
    /// The OS entropy source could not be read; nothing was substituted.
    Entropy(EntropyError),
}

impl fmt::Display for V7Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::TimestampOutOfRange { unix_ms } => write!(
                formatter,
                "millisecond {unix_ms} does not fit the 48-bit stamp of a version 7 UUID"
            ),
            Self::ClockBeforeEpoch { by } => write!(
                formatter,
                "the wall clock reads {by:?} before the Unix epoch"
            ),
            Self::Entropy(ref cause) => {
                write!(formatter, "version 7 UUID entropy was refused: {cause}")
            }
        }
    }
}

impl Error for V7Error {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match *self {
            Self::Entropy(ref cause) => Some(cause),
            Self::TimestampOutOfRange { .. } | Self::ClockBeforeEpoch { .. } => None,
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    /// A refused clock is returned as it came and the entropy is never read.
    #[test]
    fn a_refused_clock_costs_no_entropy() {
        let drawn = Cell::new(false);
        let refusal = V7Error::ClockBeforeEpoch {
            by: Duration::from_secs(1),
        };
        let result = read_inputs(
            || Err(refusal.clone()),
            || {
                drawn.set(true);
                Ok([0; ENTROPY_BYTES])
            },
        );
        assert_eq!(result, Err(refusal), "the clock refusal passes through");
        assert!(!drawn.get(), "entropy was read after the clock refused");
    }

    /// A clock that answers is read once and its entropy follows.
    #[test]
    fn an_answering_clock_is_followed_by_one_entropy_read() {
        let reads = Cell::new(0_u32);
        let result = read_inputs(
            || Ok(7),
            || {
                reads.set(reads.get().saturating_add(1));
                Ok([3; ENTROPY_BYTES])
            },
        );
        assert_eq!(
            result,
            Ok((7, [3; ENTROPY_BYTES])),
            "both inputs reach the caller as read"
        );
        assert_eq!(reads.get(), 1, "entropy is read exactly once");
    }

    /// The two refusals that carry no cause have no source and read as prose.
    #[test]
    fn the_stamp_and_clock_refusals_have_no_source() {
        let stamp = V7Error::TimestampOutOfRange {
            unix_ms: MAX_UNIX_MS.saturating_add(1),
        };
        let clock = V7Error::ClockBeforeEpoch {
            by: Duration::from_millis(5),
        };
        assert!(
            Error::source(&stamp).is_none(),
            "a stamp refusal has no cause"
        );
        assert!(
            Error::source(&clock).is_none(),
            "a clock refusal has no cause"
        );
        assert!(
            format!("{stamp}").contains("48-bit"),
            "the stamp refusal names the width"
        );
        assert!(
            format!("{clock}").contains("before the Unix epoch"),
            "the clock refusal says which side of the epoch"
        );
    }
}
