//! Seeded replay of the version 7 surface a caller outside the crate can reach
//! (INV-ID-2, #715): `Uuid::new_v7`, `Uuid::unix_millis` and `V7Error`.
//!
//! The ordering rule itself, with a drawn clock and drawn entropy, is proven
//! inside the crate by `id::sim_v7`, because the seam that takes a clock reading
//! is private. These probes drive the shipped API: the process-wide generator on
//! the live clock and across threads, and the stamp reader on drawn bytes.
//!
//! The module is gated `feature = "random"` because that is what gates
//! `lgwks_std::id` itself (INV-DEP-6).
#![cfg(feature = "random")]

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use crate::seeded_bytes::next_array;
use crate::seeded_sweep::{
    SWEEP_SEEDS, assert_distinct_seeds_diverge, assert_same_seed_replays, fold, initial_trace,
    seeded_stream,
};

use lgwks_std::id::{ParseError, Uuid, V7Error};
use lgwks_std::seeded::Seeded;

/// A clock reading after 2026-01-01, which any host running these tests passes.
const AFTER_2026_MS: u64 = 1_767_225_600_000;

/// The word folded into a trace for a value that carries no stamp.
const NO_STAMP: u64 = u64::MAX;

/// Why a probe could not run to its assertions.
#[derive(Debug)]
enum ProbeError {
    /// The process-wide generator refused an identifier.
    Refused(V7Error),
    /// The canonical text of an issued identifier did not parse.
    Unparsed(ParseError),
    /// A worker thread panicked instead of returning its identifiers.
    Worker,
}

impl fmt::Display for ProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Refused(ref cause) => write!(formatter, "the generator refused: {cause}"),
            Self::Unparsed(ref cause) => write!(formatter, "issued text did not parse: {cause}"),
            Self::Worker => formatter.write_str("a worker thread panicked"),
        }
    }
}

impl Error for ProbeError {}

impl From<V7Error> for ProbeError {
    fn from(cause: V7Error) -> Self {
        Self::Refused(cause)
    }
}

impl From<ParseError> for ProbeError {
    fn from(cause: ParseError) -> Self {
        Self::Unparsed(cause)
    }
}

/// What every probe returns.
type ProbeResult = Result<(), ProbeError>;

/// The stamp a version 7 value carries, read from its first six bytes by a
/// fold written here, not by the method under test.
fn stamp_by_fold(id: &Uuid) -> u64 {
    id.as_bytes()
        .iter()
        .take(6)
        .fold(0_u64, |stamp, byte| (stamp << 8) | u64::from(*byte))
}

/// Sixteen drawn bytes, forced to a version 7 value when `as_v7` holds.
fn drawn_value(stream: &mut Seeded, as_v7: bool) -> [u8; 16] {
    let mut raw = next_array::<16>(stream);
    if as_v7 {
        raw[6] = (raw[6] & 0x0f) | 0x70;
        raw[8] = (raw[8] & 0x3f) | 0x80;
    }
    raw
}

/// Reads the stamp of 256 drawn values, half of them version 7, into a trace.
fn stamp_trace(seed: u64) -> u64 {
    let mut stream = seeded_stream(seed);
    let mut trace = initial_trace();
    for round in 0_usize..256 {
        let id = Uuid::from(drawn_value(&mut stream, round & 1 == 0));
        match id.unix_millis() {
            Some(stamp) => fold(&mut trace, stamp),
            None => fold(&mut trace, NO_STAMP),
        }
    }
    trace
}

#[test]
/// A stamp reads back from any version 7 value and from nothing else: the
/// answer for sixteen drawn bytes is the first six bytes exactly when the
/// version nibble is 7 and the variant bits are `10`.
fn unix_millis_reads_the_stamp_of_a_version_seven_value_and_no_other() {
    for seed in SWEEP_SEEDS {
        let mut stream = seeded_stream(seed);
        for round in 0_usize..512 {
            let raw = drawn_value(&mut stream, round & 1 == 0);
            let is_v7 = raw[6] >> 4 == 7 && raw[8] & 0xc0 == 0x80;
            let id = Uuid::from(raw);
            let expected = is_v7.then(|| stamp_by_fold(&id));
            assert_eq!(
                id.unix_millis(),
                expected,
                "seed {seed:#018x}: bytes {raw:02x?} read the wrong stamp"
            );
        }
    }
}

#[test]
/// The process-wide generator on the live clock and live entropy issues version
/// 7 values stamped after 2026 in strict order, as values and as text, and the
/// text parses back to the value it came from.
fn the_process_wide_generator_is_strictly_increasing_on_the_live_clock() -> ProbeResult {
    let mut previous: Option<Uuid> = None;
    for _ in 0..4_000 {
        let issued = Uuid::new_v7()?;
        assert_eq!(issued.version(), Some(7), "a live value reports version 7");
        assert!(
            issued.unix_millis() > Some(AFTER_2026_MS),
            "a live stamp {:?} is not after 2026",
            issued.unix_millis()
        );
        assert_eq!(
            Uuid::parse(&issued.to_string())?,
            issued,
            "the text of {issued} parses back to it"
        );
        if let Some(earlier) = previous {
            assert!(
                earlier < issued,
                "live values {earlier} and {issued} are out of order"
            );
            assert!(
                earlier.to_string() < issued.to_string(),
                "the text of {earlier} sorts after the text of {issued}"
            );
        }
        previous = Some(issued);
    }
    Ok(())
}

#[test]
/// A hundred scoped threads draw a hundred identifiers each from the
/// process-wide generator: each thread sees its own identifiers in strict order,
/// and the ten thousand values are all distinct.
fn the_process_wide_generator_never_repeats_across_threads() -> ProbeResult {
    let batches: Vec<Result<Vec<Uuid>, ProbeError>> = std::thread::scope(|scope| {
        // Every thread is started before any is joined, or the hundred would
        // run one after another and share nothing.
        let mut workers = Vec::with_capacity(100);
        for _ in 0..100 {
            workers.push(scope.spawn(|| {
                (0..100)
                    .map(|_| Uuid::new_v7().map_err(ProbeError::from))
                    .collect::<Result<Vec<Uuid>, ProbeError>>()
            }));
        }
        let mut joined = Vec::with_capacity(100);
        for worker in workers {
            joined.push(match worker.join() {
                Ok(batch) => batch,
                Err(_) => Err(ProbeError::Worker),
            });
        }
        joined
    });
    let mut seen = BTreeSet::new();
    for batch in batches {
        let ids = batch?;
        for pair in ids.windows(2) {
            assert!(
                pair[0] < pair[1],
                "a thread saw {} then {} out of order",
                pair[0],
                pair[1]
            );
        }
        for id in ids {
            assert_eq!(
                id.version(),
                Some(7),
                "the shared generator issues version 7"
            );
            assert!(
                seen.insert(*id.as_bytes()),
                "the shared generator repeated {id}"
            );
        }
    }
    assert_eq!(
        seen.len(),
        10_000,
        "every one of the ten thousand ids is distinct"
    );
    Ok(())
}

#[test]
/// The replay oracle: one seed, one stamp trace.
fn the_same_seed_replays_to_the_same_stamp_trace() {
    for seed in SWEEP_SEEDS {
        assert_same_seed_replays(stamp_trace, seed);
    }
}

#[test]
/// The divergence oracle: a sweep that ignored its seed would pass the replay.
fn distinct_stamp_seeds_diverge_in_their_trace() {
    assert_distinct_seeds_diverge(stamp_trace, SWEEP_SEEDS[0], SWEEP_SEEDS[1]);
}
