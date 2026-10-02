//! Shared deterministic replay harness for the seeded simulation tests.
//!
//! Every seeded sweep in this crate needs the same three things: a reproducible
//! stream, a fold that mixes observations into a trace hash, and a seed value
//! whose identity can be named in a failure message. Those live here once so a
//! new sweep family cannot quietly diverge in how it seeds or how it replays.
//!
//! This module is `#[path]`-included by the sweeps; it is not a test binary of
//! its own.

#![allow(dead_code, reason = "each including binary uses a different subset")]

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

/// Advances a deterministic xorshift-style stream.
///
/// One `u64` of state drives every case in a sweep, so one seed reproduces the
/// whole run rather than only the cases it happens to name.
pub fn next_seed(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1);
    *state
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

/// Folds one counted value into the trace, saturating a cast rather than
/// truncating it so a wide counter cannot alias a small one.
pub fn fold_usize(trace: &mut u64, value: usize) {
    fold(trace, u64::try_from(value).unwrap_or(u64::MAX));
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
