//! Seeded deterministic replay of the version 7 ordering rule (INV-ID-2, #715).
//!
//! One seed draws every clock reading and every byte of entropy, a failure
//! prints the seed that produced it, and the same seed must produce the same
//! trace hash. Every answer is checked against a model written here from the
//! documented rule and a bit-level layout built with 128-bit shifts, not from
//! the implementation under test.
//!
//! The rule the model states: a later millisecond takes that stamp and reseeds
//! the counter below 2,048; the same millisecond or a clock that stepped
//! backwards keeps the last stamp and adds one; a spent counter borrows the
//! next millisecond; a stamp past 48 bits is refused and changes nothing.
//!
//! These probes live in the crate because the generator's seam, which takes a
//! clock reading and ten bytes of entropy, is private (`Uuid::new_v4` is driven
//! the same way through `new_v4_with`). What a downstream caller can reach,
//! `Uuid::new_v7` and `Uuid::unix_millis`, is probed through the public API in
//! `tests/it/sim_id_v7.rs`.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use super::v7::{V7Error, V7Generator};
use super::{ParseError, Uuid};
use crate::seeded::{DrawError, Seeded};

/// The seeds the sweeps replay, named so a failure names the one that
/// reproduces it.
const SEEDS: [u64; 4] = [
    0x5EED_0000_0000_0001,
    0x5EED_0000_0000_0002,
    0x5EED_0000_FFFF_FFFF,
    0xDEAD_BEEF_CAFE_0001,
];

/// The mixing constant of the trace fold: the 64-bit FNV-1a prime.
const FNV_PRIME: u64 = 1_099_511_628_211;

/// The trace's starting value: the 64-bit FNV-1a offset basis.
const FNV_BASIS: u64 = 14_695_981_039_346_656_037;

/// The largest 48-bit stamp.
const MAX_MS: u64 = 0xFFFF_FFFF_FFFF;

/// The largest value of the 12-bit counter.
const COUNTER_MAX: u16 = 0x0FFF;

/// The largest counter a reseed may give: the top bit of the twelve is clear.
const SEED_MAX: u16 = 0x07FF;

/// A clock reading well inside the 48-bit range: late 2023, plus a drawn offset.
const BASE_MS: u64 = 1_700_000_000_000;

/// Why a probe of the version 7 rule could not run to its assertions.
#[derive(Debug)]
enum RuleError {
    /// A generator refused an identifier the model said it must issue.
    Refused(V7Error),
    /// Canonical text of an issued identifier did not parse.
    Unparsed(ParseError),
    /// The seeded stream refused a draw.
    Undrawn(DrawError),
    /// The model refused a clock reading the generator accepted.
    Diverged {
        /// The clock reading both were given.
        now: u64,
    },
}

impl fmt::Display for RuleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Refused(ref cause) => write!(formatter, "a generator refused: {cause}"),
            Self::Unparsed(ref cause) => write!(formatter, "issued text did not parse: {cause}"),
            Self::Undrawn(ref cause) => write!(formatter, "the seeded stream refused: {cause}"),
            Self::Diverged { now } => write!(formatter, "the model refused clock {now}"),
        }
    }
}

impl Error for RuleError {}

impl From<V7Error> for RuleError {
    fn from(cause: V7Error) -> Self {
        Self::Refused(cause)
    }
}

impl From<ParseError> for RuleError {
    fn from(cause: ParseError) -> Self {
        Self::Unparsed(cause)
    }
}

impl From<DrawError> for RuleError {
    fn from(cause: DrawError) -> Self {
        Self::Undrawn(cause)
    }
}

/// What every probe returns.
type RuleResult = Result<(), RuleError>;

/// The stamp and counter a conforming generator holds, from the documented rule.
#[derive(Default)]
struct Model {
    /// The stamp and counter of the last identifier issued.
    last: Option<(u64, u16)>,
}

impl Model {
    /// The stamp and counter the rule assigns to one more identifier, or `None`
    /// where it refuses; a refusal changes nothing.
    fn issue(&mut self, now: u64, seed: u16) -> Option<(u64, u16)> {
        let next = match self.last {
            Some((stamp, counter)) if now <= stamp => {
                if counter < COUNTER_MAX {
                    (stamp, counter.saturating_add(1))
                } else if stamp < MAX_MS {
                    (stamp.saturating_add(1), seed)
                } else {
                    return None;
                }
            }
            _ if now > MAX_MS => return None,
            _ => (now, seed),
        };
        self.last = Some(next);
        Some(next)
    }
}

/// Folds one observed value into the running trace.
fn fold(trace: &mut u64, value: u64) {
    *trace = trace.wrapping_mul(FNV_PRIME).wrapping_add(value);
}

/// Folds every byte of `bytes` into the running trace.
fn fold_bytes(trace: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        fold(trace, u64::from(*byte));
    }
}

/// A value in `0..bound` from the stream; a zero bound answers zero.
///
/// The remainder over-represents the low residues by at most `bound / 2^64`,
/// which for the bounds drawn here (at most 2^32) is below any count a sweep of
/// thousands of draws can see.
fn draw_below(stream: &mut Seeded, bound: u64) -> u64 {
    stream.next_u64().rem_euclid(bound.max(1))
}

/// Ten bytes of entropy, one per stream word.
fn draw_entropy(stream: &mut Seeded) -> [u8; 10] {
    let mut drawn = [0_u8; 10];
    for slot in drawn.iter_mut() {
        *slot = stream.next_u64().to_le_bytes()[0];
    }
    drawn
}

/// A clock reading inside the 48-bit range, drawn from the stream.
fn draw_clock(stream: &mut Seeded) -> u64 {
    BASE_MS.saturating_add(draw_below(stream, 0xFFFF_FFFF))
}

/// The stamp an issued identifier carries, read from its first six bytes.
fn stamp_of(id: &Uuid) -> u64 {
    let [ts0, ts1, ts2, ts3, ts4, ts5, ..] = *id.as_bytes();
    u64::from_be_bytes([0, 0, ts0, ts1, ts2, ts3, ts4, ts5])
}

/// The 12-bit counter an issued identifier carries.
fn counter_of(id: &Uuid) -> u16 {
    let [_, _, _, _, _, _, high, low, ..] = *id.as_bytes();
    (u16::from(high & 0x0f) << 8) | u16::from(low)
}

/// The counter seed the documentation gives for ten bytes of entropy: the first
/// two bytes, big-endian, with the top bit of the twelve cleared.
fn reference_seed(entropy: &[u8; 10]) -> u16 {
    let [high, low, ..] = *entropy;
    ((u16::from(high) << 8) | u16::from(low)) & SEED_MAX
}

/// The sixteen bytes the documented layout gives, built from 128-bit shifts: a
/// 48-bit stamp, version 7, a 12-bit counter, variant `10` and 62 random bits.
fn reference_bytes(stamp: u64, counter: u16, entropy: &[u8; 10]) -> [u8; 16] {
    let [_, _, r0, r1, r2, r3, r4, r5, r6, r7] = *entropy;
    let random = u64::from_be_bytes([r0, r1, r2, r3, r4, r5, r6, r7]) & 0x3FFF_FFFF_FFFF_FFFF;
    let value = (u128::from(stamp) << 80)
        | (0x7_u128 << 76)
        | (u128::from(counter) << 64)
        | (0b10_u128 << 62)
        | u128::from(random);
    value.to_be_bytes()
}

/// Issues one identifier and asserts it is the one the model and the layout
/// reference say it must be, returning it.
fn issue_checked(
    generator: &mut V7Generator,
    model: &mut Model,
    now: u64,
    entropy: [u8; 10],
    seed: u64,
) -> Result<Uuid, RuleError> {
    let issued = generator.issue_at(now, entropy)?;
    let (stamp, counter) = model
        .issue(now, reference_seed(&entropy))
        .ok_or(RuleError::Diverged { now })?;
    assert_eq!(
        (stamp_of(&issued), counter_of(&issued)),
        (stamp, counter),
        "seed {seed:#018x}: the stamp and counter follow the documented rule at clock {now}"
    );
    assert_eq!(
        issued.as_bytes(),
        &reference_bytes(stamp, counter, &entropy),
        "seed {seed:#018x}: the bytes follow the documented layout"
    );
    assert_eq!(
        issued.version(),
        Some(7),
        "seed {seed:#018x}: an issued value reports version 7"
    );
    assert_eq!(
        issued.unix_millis(),
        Some(stamp),
        "seed {seed:#018x}: unix_millis reads the stamp back"
    );
    Ok(issued)
}

/// Draws a run of clock readings from a drawn start, each at most `spread`
/// later or, one time in four, up to 5,000 ms earlier, and returns them with
/// the number of backward steps taken.
fn draw_clock_run(stream: &mut Seeded, count: usize, spread: u64) -> (Vec<u64>, usize) {
    let mut now = draw_clock(stream);
    let mut backwards = 0_usize;
    let mut run = Vec::with_capacity(count);
    for _ in 0..count {
        if draw_below(stream, 4) == 0 {
            now = now.saturating_sub(draw_below(stream, 5_000).saturating_add(1));
            backwards = backwards.saturating_add(1);
        } else {
            now = now.saturating_add(draw_below(stream, spread.saturating_add(1)));
        }
        run.push(now);
    }
    (run, backwards)
}

/// Runs the whole mixed sweep for one seed and returns its trace.
fn v7_trace(seed: u64) -> u64 {
    let mut stream = Seeded::from_seed(seed);
    let mut trace = FNV_BASIS;
    let mut generator = V7Generator::new();
    let (run, _) = draw_clock_run(&mut stream, 256, 3);
    for now in run {
        let entropy = draw_entropy(&mut stream);
        match generator.issue_at(now, entropy) {
            Ok(issued) => fold_bytes(&mut trace, issued.as_bytes()),
            Err(_) => fold(&mut trace, 0),
        }
    }
    trace
}

#[test]
/// A fresh generator stamps the drawn millisecond, version 7, the counter seed
/// and the variant, with the entropy's 62 bits behind them, byte for byte as a
/// 128-bit shift reference builds it, and the canonical text parses back.
fn the_layout_of_an_issued_id_matches_a_bit_level_reference() -> RuleResult {
    for seed in SEEDS {
        let mut stream = Seeded::from_seed(seed);
        for _ in 0..256 {
            let now = draw_clock(&mut stream);
            let entropy = draw_entropy(&mut stream);
            let mut generator = V7Generator::new();
            let issued = issue_checked(&mut generator, &mut Model::default(), now, entropy, seed)?;
            let parsed = Uuid::parse(&issued.to_string())?;
            assert_eq!(parsed, issued, "seed {seed:#018x}: the text round-trips");
            assert_eq!(
                parsed.unix_millis(),
                Some(now),
                "seed {seed:#018x}: a parsed value still reads its stamp"
            );
        }
    }
    Ok(())
}

#[test]
/// With the clock only moving forward, in steps of zero to three milliseconds
/// so most readings repeat, every identifier is strictly greater than the one
/// before it as a value and as text.
fn a_forward_clock_issues_strictly_increasing_ids_in_value_and_text_order() -> RuleResult {
    for seed in SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let mut generator = V7Generator::new();
        let mut model = Model::default();
        let mut now = draw_clock(&mut stream);
        let mut previous: Option<Uuid> = None;
        for _ in 0..1_024 {
            now = now.saturating_add(draw_below(&mut stream, 4));
            let entropy = draw_entropy(&mut stream);
            let issued = issue_checked(&mut generator, &mut model, now, entropy, seed)?;
            if let Some(earlier) = previous {
                assert!(
                    earlier < issued,
                    "seed {seed:#018x}: {earlier} must sort before {issued}"
                );
                assert!(
                    earlier.to_string() < issued.to_string(),
                    "seed {seed:#018x}: the text of {earlier} must sort before the text of {issued}"
                );
            }
            previous = Some(issued);
        }
    }
    Ok(())
}

#[test]
/// Two identifiers issued in one millisecond share the stamp and differ by
/// exactly one counter step, in issue order, while their random fields stay the
/// entropy each was given.
fn two_ids_in_one_millisecond_differ_by_exactly_one_counter_step() -> RuleResult {
    for seed in SEEDS {
        let mut stream = Seeded::from_seed(seed);
        for _ in 0..512 {
            let now = draw_clock(&mut stream);
            let first_entropy = draw_entropy(&mut stream);
            let second_entropy = draw_entropy(&mut stream);
            let mut generator = V7Generator::new();
            let mut model = Model::default();
            let first = issue_checked(&mut generator, &mut model, now, first_entropy, seed)?;
            let second = issue_checked(&mut generator, &mut model, now, second_entropy, seed)?;
            assert_eq!(
                stamp_of(&first),
                stamp_of(&second),
                "seed {seed:#018x}: one millisecond is one stamp"
            );
            assert_eq!(
                counter_of(&second),
                counter_of(&first).saturating_add(1),
                "seed {seed:#018x}: the counter steps by one"
            );
            assert!(
                first < second,
                "seed {seed:#018x}: the second sorts after the first, whatever the entropy"
            );
        }
    }
    Ok(())
}

#[test]
/// A clock that steps back up to five seconds keeps the last stamp and adds one
/// to the counter, so no identifier ever carries a lower stamp or a lower value
/// than the one before it; the sweep takes backward steps, not merely allows them.
fn a_clock_stepping_backwards_keeps_the_last_stamp_and_the_order() -> RuleResult {
    let mut backward_steps = 0_usize;
    for seed in SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let (run, backwards) = draw_clock_run(&mut stream, 1_024, 3);
        backward_steps = backward_steps.saturating_add(backwards);
        let mut generator = V7Generator::new();
        let mut model = Model::default();
        let mut previous: Option<Uuid> = None;
        for now in run {
            let entropy = draw_entropy(&mut stream);
            let issued = issue_checked(&mut generator, &mut model, now, entropy, seed)?;
            if let Some(earlier) = previous {
                assert!(
                    stamp_of(&earlier) <= stamp_of(&issued),
                    "seed {seed:#018x}: a stamp went backwards at clock {now}"
                );
                assert!(
                    earlier < issued,
                    "seed {seed:#018x}: a backward clock minted a value that sorts first"
                );
            }
            previous = Some(issued);
        }
    }
    assert!(
        backward_steps > 0,
        "the sweep drew no backward step, so it proved nothing about one"
    );
    Ok(())
}

#[test]
/// Five thousand identifiers in one millisecond are issued in strict order: the
/// counter spends its remaining values, the stamp borrows the next millisecond
/// with a counter reseeded below 2,048, and nothing waits or repeats.
fn a_spent_counter_borrows_the_next_millisecond_and_never_repeats() -> RuleResult {
    for seed in SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let now = draw_clock(&mut stream);
        let mut generator = V7Generator::new();
        let mut model = Model::default();
        let mut previous: Option<Uuid> = None;
        let mut in_first_stamp = 0_usize;
        let mut first_borrowed: Option<u16> = None;
        for _ in 0..5_000 {
            let entropy = draw_entropy(&mut stream);
            let issued = issue_checked(&mut generator, &mut model, now, entropy, seed)?;
            if let Some(earlier) = previous {
                assert!(
                    earlier < issued,
                    "seed {seed:#018x}: the burst repeated or reordered a value"
                );
            }
            if stamp_of(&issued) == now {
                in_first_stamp = in_first_stamp.saturating_add(1);
            } else if first_borrowed.is_none() {
                assert_eq!(
                    stamp_of(&issued),
                    now.saturating_add(1),
                    "seed {seed:#018x}: a spent counter borrows exactly one millisecond"
                );
                first_borrowed = Some(counter_of(&issued));
            }
            previous = Some(issued);
        }
        assert!(
            (2_049..=4_096).contains(&in_first_stamp),
            "seed {seed:#018x}: {in_first_stamp} ids fit the first stamp, outside 2049..=4096"
        );
        assert!(
            first_borrowed.is_some(),
            "seed {seed:#018x}: five thousand ids in one millisecond never borrowed"
        );
        if let Some(reseeded) = first_borrowed {
            assert!(
                reseeded <= SEED_MAX,
                "seed {seed:#018x}: the borrowed stamp started at {reseeded}, not below 2,048"
            );
        }
    }
    Ok(())
}

#[test]
/// Whatever the entropy, a counter reseeded by a later millisecond is below
/// 2,048, so a stamp always has 2,048 increments left.
fn a_later_millisecond_reseeds_the_counter_below_the_rollover_guard() -> RuleResult {
    for seed in SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let mut generator = V7Generator::new();
        let mut now = draw_clock(&mut stream);
        for _ in 0..1_024 {
            now = now
                .saturating_add(1)
                .saturating_add(draw_below(&mut stream, 50));
            let mut entropy = draw_entropy(&mut stream);
            entropy[0] |= 0xF8;
            let issued = generator.issue_at(now, entropy)?;
            assert!(
                counter_of(&issued) <= SEED_MAX,
                "seed {seed:#018x}: a reseeded counter {} reached the upper half",
                counter_of(&issued)
            );
        }
    }
    Ok(())
}

#[test]
/// A stamp past 48 bits is refused naming the millisecond and leaves the
/// generator equal to its twin that never saw the call, and a spent counter at
/// the largest stamp is refused the same way instead of wrapping.
fn a_stamp_past_forty_eight_bits_is_refused_and_leaves_the_generator_untouched() -> RuleResult {
    for seed in SEEDS {
        let mut stream = Seeded::from_seed(seed);
        for _ in 0..128 {
            let mut generator = V7Generator::new();
            let warm = draw_clock(&mut stream);
            for _ in 0..draw_below(&mut stream, 8) {
                generator.issue_at(warm, draw_entropy(&mut stream))?;
            }
            let twin = generator.clone();
            let too_far = MAX_MS
                .saturating_add(1)
                .saturating_add(draw_below(&mut stream, 1 << 20));
            assert_eq!(
                generator.issue_at(too_far, draw_entropy(&mut stream)),
                Err(V7Error::TimestampOutOfRange { unix_ms: too_far }),
                "seed {seed:#018x}: a stamp past 48 bits is named"
            );
            assert_eq!(
                generator, twin,
                "seed {seed:#018x}: a refusal must change nothing"
            );
        }
        let mut generator = V7Generator::new();
        let entropy = draw_entropy(&mut stream);
        let allowed = 4_096_u32.saturating_sub(u32::from(reference_seed(&entropy)));
        for _ in 0..allowed {
            let issued = generator.issue_at(MAX_MS, entropy)?;
            assert_eq!(
                issued.unix_millis(),
                Some(MAX_MS),
                "seed {seed:#018x}: the largest stamp is accepted"
            );
        }
        let twin = generator.clone();
        assert_eq!(
            generator.issue_at(MAX_MS, entropy),
            Err(V7Error::TimestampOutOfRange {
                unix_ms: MAX_MS.saturating_add(1)
            }),
            "seed {seed:#018x}: a spent counter at the largest stamp is refused, not wrapped"
        );
        assert_eq!(
            generator, twin,
            "seed {seed:#018x}: that refusal changed nothing"
        );
    }
    Ok(())
}

#[test]
/// A hundred tenants draw from the one process generator over a shared clock:
/// the twenty thousand identifiers form one strict order, each tenant's own
/// are in that order whatever the others do, and none repeats.
fn a_hundred_tenants_sharing_one_generator_keep_one_strict_order() -> RuleResult {
    for seed in SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let mut generator = V7Generator::new();
        let mut model = Model::default();
        let mut last_of: Vec<Option<Uuid>> = vec![None; 100];
        let mut previous: Option<Uuid> = None;
        let mut seen = BTreeSet::new();
        let mut now = draw_clock(&mut stream);
        for _ in 0..20_000 {
            now = now.saturating_add(draw_below(&mut stream, 2));
            let tenant = stream.index(100)?;
            let entropy = draw_entropy(&mut stream);
            let issued = issue_checked(&mut generator, &mut model, now, entropy, seed)?;
            if let Some(earlier) = previous {
                assert!(
                    earlier < issued,
                    "seed {seed:#018x}: the shared order broke at tenant {tenant}"
                );
            }
            previous = Some(issued);
            if let Some(slot) = last_of.get_mut(tenant) {
                if let Some(earlier) = *slot {
                    assert!(
                        earlier < issued,
                        "seed {seed:#018x}: tenant {tenant} went out of order"
                    );
                }
                *slot = Some(issued);
            }
            assert!(
                seen.insert(*issued.as_bytes()),
                "seed {seed:#018x}: two tenants were issued the same value {issued}"
            );
        }
    }
    Ok(())
}

#[test]
/// The replay oracle: one seed, one trace.
fn the_same_seed_replays_to_the_same_v7_trace() {
    for seed in SEEDS {
        assert_eq!(
            v7_trace(seed),
            v7_trace(seed),
            "seed {seed:#018x}: the same seed must produce the same trace"
        );
    }
}

#[test]
/// The divergence oracle: a sweep that ignored its seed would pass the replay.
fn distinct_v7_seeds_diverge_in_their_trace() {
    assert_ne!(
        v7_trace(SEEDS[0]),
        v7_trace(SEEDS[1]),
        "two seeds must not produce one trace, or the sweep is not reading its seed"
    );
}

#[test]
/// Two seeds draw two different clocks, so the sweeps are not one clock checked
/// over and over.
fn two_seeds_draw_two_different_clocks() {
    let first = draw_clock(&mut Seeded::from_seed(SEEDS[0]));
    let second = draw_clock(&mut Seeded::from_seed(SEEDS[1]));
    assert_ne!(first, second, "the two sweep seeds drew the same clock");
}
