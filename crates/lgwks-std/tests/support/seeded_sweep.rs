//! Shared deterministic replay harness for the seeded simulation tests.
//!
//! Every seeded sweep in this crate needs the same three things: a reproducible
//! stream, a fold that mixes observations into a trace hash, and a seed value
//! whose identity can be named in a failure message. Those live here once so a
//! new sweep family cannot quietly diverge in how it seeds or how it replays.
//!
//! This module is `#[path]`-included by the sweeps; it is not a test binary of
//! its own. It carries its own tests rather than an `allow`: every including
//! binary uses a different subset of it, and a fixture that is only correct
//! where its unused half is silenced is a fixture nobody has read.

/// The seeds the sweep families replay.
///
/// Each is named rather than generated so a failure names the exact seed that
/// reproduces it.
pub const SWEEP_SEEDS: [u64; 4] = [
    0x5EED_0000_0000_0001,
    0x5EED_0000_0000_0002,
    0x5EED_0000_FFFF_FFFF,
    0xDEAD_BEEF_CAFE_0001,
];

/// The mixing constant for [`fold`]: the 64-bit FNV-1a prime.
const FNV_PRIME: u64 = 1_099_511_628_211;

/// The offset basis for [`fold`]: the 64-bit FNV-1a offset basis.
const FNV_BASIS: u64 = 14_695_981_039_346_656_037;

/// Advances a deterministic xorshift64* stream.
///
/// One `u64` of state drives every case in a sweep, so one seed reproduces the
/// whole run rather than only the cases it happens to name.
///
/// The finalising multiply is load-bearing rather than decorative. A
/// linear-congruential step has period `2**k` in its low `k` bits, so a family
/// drawing one byte per word out of an unfinalised stream saw the same 256
/// bytes repeat for ever: a 4 KiB payload was sixteen copies of one block, and
/// every wide-boundary case was a repeat of a narrow one. `xorshift64*` is the
/// standard remedy — it moves the weakness out of the word's low bits, so one
/// byte of a word is one byte of the stream's entropy.
///
/// The state must be non-zero: the all-zero word is xorshift's fixed point, and
/// every named seed is non-zero while xorshift's steps are invertible, so a
/// stream cannot reach it from one that is not.
pub fn next_seed(state: &mut u64) -> u64 {
    let mut word = *state;
    word ^= word >> 12;
    word ^= word << 25;
    word ^= word >> 27;
    *state = word;
    word.wrapping_mul(0x2545_f491_4f6c_dd1d)
}

/// One word of the stream read at the platform's index width.
///
/// The obvious spellings are all refused here: `as` is `as_conversions =
/// forbid`, `From<usize>` for `u64` does not exist, and substituting a
/// sentinel for a draw would be a value the trace invented. `u8` into `usize`
/// is the one infallible numeric conversion in the language, so the word is read
/// a byte at a time. A target narrower than 64 bits folds the bytes it cannot
/// address rather than dropping them, because a truncated draw is a different
/// stream and the trace is a property of the seed rather than of the platform.
#[must_use]
pub fn next_index(state: &mut u64) -> usize {
    let mut folded = 0_usize;
    for byte in next_seed(state).to_le_bytes() {
        folded = folded.wrapping_mul(31).wrapping_add(usize::from(byte));
    }
    folded
}

/// Folds one observed value into the running trace.
///
/// The trace is a hash rather than a list: a sweep can run thousands of cases
/// and still compare in one value, so a same-seed replay is one comparison.
pub fn fold(trace: &mut u64, value: u64) {
    *trace = trace.wrapping_mul(FNV_PRIME).wrapping_add(value);
}

/// Starts a trace with the FNV-1a basis.
pub fn initial_trace() -> u64 {
    FNV_BASIS
}

/// Folds one score's bit pattern into the trace.
///
/// Bit patterns, not the values, so two sweeps that differ only in a
/// floating-point detail still compare exactly.
pub fn fold_score(trace: &mut u64, score: f64) {
    fold(trace, score.to_bits());
}

/// The counter's own bytes read as the trace word, most significant first.
///
/// This is the one reading of a `usize` this crate folds: every bit of a counter
/// wider than the word reaches it, where a narrowing cast would have aliased two
/// counters onto one value — the single collision a replay trace cannot have.
/// `as` is `as_conversions = forbid` and `From<usize> for u64` does not exist,
/// so the word is mixed a byte at a time through `u8` into `u64`, the one
/// infallible numeric widening in the language.
///
/// It is a fold and not a cast: it is a value for entropy and for a trace, not a
/// copy of the counter, and no caller may read it as the counter itself.
#[must_use]
pub fn word_of(value: usize) -> u64 {
    let mut word = 0_u64;
    for byte in value.to_ne_bytes() {
        word = word.wrapping_mul(31).wrapping_add(u64::from(byte));
    }
    word
}

/// Folds one counted value into the trace, saturating a cast rather than
/// truncating it so a wide counter cannot alias a small one.
///
/// The whole counter reaches the trace through [`word_of`], so the fold is the
/// same reading everywhere: no caller can fold a counter one way and read it
/// another.
pub fn fold_usize(trace: &mut u64, value: usize) {
    fold(trace, word_of(value));
}

/// An elapsed duration in nanoseconds, as the trace word counts time.
///
/// `Duration::as_nanos` is a `u128` and the trace word is 64 bits, so the
/// conversion would need a checked narrowing at every sample. It is read as
/// seconds and sub-second nanoseconds instead — both of which `Duration` reports
/// in a width that fits — and combined with saturating arithmetic, so a duration
/// that somehow outlasted `u64::MAX` nanoseconds reads as the largest sample
/// rather than as a wrapped one.
#[must_use]
pub fn nanos_of(elapsed: std::time::Duration) -> u64 {
    elapsed
        .as_secs()
        .saturating_mul(1_000_000_000_u64)
        .saturating_add(u64::from(elapsed.subsec_nanos()))
}

/// Runs `sweep` twice under `seed` and asserts the traces agree.
///
/// This is the same-seed replay oracle every family uses. The assertion names
/// the seed so a failure is directly reproducible.
pub fn assert_same_seed_replays(sweep: impl Fn(u64) -> u64, seed: u64) {
    let first = sweep(seed);
    let replayed = sweep(seed);
    assert_eq!(
        first, replayed,
        "seed {seed:#018x}: the same seed must produce the same trace"
    );
}

/// Asserts that two distinct seeds do not collapse to the same trace.
///
/// Without this, a sweep that ignores its seed entirely would pass the replay
/// oracle while proving nothing.
pub fn assert_distinct_seeds_diverge(sweep: impl Fn(u64) -> u64, first: u64, second: u64) {
    assert_ne!(
        sweep(first),
        sweep(second),
        "seeds {first:#018x} and {second:#018x} must not produce the same trace, \
         or the sweep is not reading its seed"
    );
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{
        FNV_BASIS, SWEEP_SEEDS, assert_distinct_seeds_diverge, assert_same_seed_replays, fold,
        fold_score, fold_usize, initial_trace, nanos_of, next_index, next_seed, word_of,
    };

    /// A stand-in family: it makes every draw this module offers, so the stream
    /// is observed rather than assumed, and it reads its seed so the replay
    /// oracle has something to hold.
    fn draws(seed: u64) -> u64 {
        let mut state = seed;
        let mut trace = initial_trace();
        for _ in 0..64 {
            fold(&mut trace, next_seed(&mut state));
            fold_usize(&mut trace, next_index(&mut state));
            let score = f64::from(u32::from(next_seed(&mut state).to_le_bytes()[0]));
            fold_score(&mut trace, score);
        }
        trace
    }

    #[test]
    /// The replay oracle, run over the stream every family draws from.
    fn sim_the_named_seeds_replay_to_the_same_trace() {
        for seed in SWEEP_SEEDS {
            assert_same_seed_replays(draws, seed);
        }
    }

    #[test]
    /// Two named seeds must not read as one stream.
    fn sim_distinct_seeds_diverge_in_the_stream_draws() {
        assert_distinct_seeds_diverge(draws, SWEEP_SEEDS[0], SWEEP_SEEDS[1]);
    }

    #[test]
    /// The defect the finaliser prevents, observed at the byte a family draws.
    fn sim_the_low_byte_of_the_stream_has_no_period_of_its_own_width() {
        // An unfinalised linear-congruential step has period 256 in its low 8
        // bits, so `bytes[i]` equals `bytes[i + 256]` for ever and a 4 KiB
        // payload drawn one byte per word was sixteen copies of one block.
        let mut state = SWEEP_SEEDS[0];
        let bytes: Vec<u8> = (0..1_024)
            .map(|_| next_seed(&mut state).to_le_bytes()[0])
            .collect();
        assert_ne!(
            &bytes[..768],
            &bytes[256..1_024],
            "the low byte repeated with a period of 256 draws"
        );
    }

    #[test]
    /// A thousand draws of one seed are a thousand different words.
    fn sim_a_thousand_draws_are_a_thousand_distinct_words() {
        let mut state = SWEEP_SEEDS[0];
        let mut words = BTreeSet::new();
        for _ in 0..1_024 {
            words.insert(next_seed(&mut state));
        }
        assert_eq!(
            words.len(),
            1_024,
            "a thousand draws collapsed onto {} distinct words",
            words.len()
        );
    }

    #[test]
    /// Two counters differing only above the low bits must not collide.
    fn sim_a_counter_is_folded_whole_rather_than_truncated() {
        // Two counters differing only above the low bits must not collapse onto
        // one trace value, which is what a narrowing fold does on a target
        // narrower than 64 bits.
        let mut low = initial_trace();
        fold_usize(&mut low, 1);
        let mut high = initial_trace();
        fold_usize(
            &mut high,
            1_usize.wrapping_add(1_usize << (usize::BITS - 1)),
        );
        assert_ne!(
            low, high,
            "a counter differing only in its sign bit folded onto the same trace value"
        );
    }

    #[test]
    /// A counter's word reading and its fold are one reading, not two.
    fn sim_a_counters_word_and_its_fold_are_one_reading() {
        let mut folded = initial_trace();
        fold_usize(&mut folded, 4_096);
        let mut direct = initial_trace();
        fold(&mut direct, word_of(4_096));
        assert_eq!(
            folded, direct,
            "folding a counter and folding its word reading must be the same observation"
        );
        assert_ne!(
            word_of(1),
            word_of(1_usize.wrapping_add(1_usize << (usize::BITS - 1))),
            "two counters differing only in their sign bit read as one word"
        );
    }

    #[test]
    /// A duration is read as its whole nanosecond count, not narrowed.
    fn sim_a_duration_reads_as_its_whole_nanosecond_count() {
        assert_eq!(
            nanos_of(std::time::Duration::from_nanos(1_500)),
            1_500,
            "a duration under a second is its own nanosecond count"
        );
        assert_eq!(
            nanos_of(std::time::Duration::from_secs(2)),
            2_000_000_000,
            "a duration over a second carries both of its halves"
        );
        assert_eq!(
            nanos_of(std::time::Duration::ZERO),
            0,
            "no elapsed time is zero nanoseconds"
        );
    }

    #[test]
    /// The trace begins at the FNV-1a offset basis.
    fn an_empty_trace_starts_at_the_fnv_offset_basis() {
        assert_eq!(
            initial_trace(),
            FNV_BASIS,
            "the FNV-1a offset basis is the trace's starting value"
        );
    }
}
