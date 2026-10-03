//! Seeded payload drawing shared by the deterministic simulation families.
//!
//! One seed drives every payload a family generates, so a failing assertion
//! names the seed that reproduces it. Nothing here interprets a payload: each
//! family decides what its payloads must mean, against a reference model
//! written beside its own assertions.
//!
//! This module is `#[path]`-included by the sweeps; it is not a test binary.

#![allow(dead_code, reason = "each including binary uses a different subset")]

use crate::seeded_sweep::{fold, fold_usize, next_seed};

/// The printable ASCII alphabet a text-shaped payload is drawn from.
const TEXT_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 -_.~/:?";

/// One byte drawn from the sweep stream rooted at `state`.
#[must_use]
pub fn next_byte(state: &mut u64) -> u8 {
    u8::try_from(next_seed(state).rem_euclid(256)).unwrap_or(0)
}

/// `len` bytes drawn from the sweep stream rooted at `state`.
#[must_use]
pub fn next_bytes(state: &mut u64, len: usize) -> Vec<u8> {
    (0..len).map(|_| next_byte(state)).collect()
}

/// `len` printable ASCII characters drawn from the sweep stream rooted at
/// `state`.
///
/// The bound is the alphabet's own length, so every character of it is
/// reachable and none is drawn twice as often as another.
#[must_use]
pub fn next_text(state: &mut u64, len: usize) -> String {
    let ceiling = u64::try_from(TEXT_ALPHABET.len()).unwrap_or(u64::MAX);
    (0..len)
        .map(|_| {
            let offset = usize::try_from(next_seed(state).rem_euclid(ceiling)).unwrap_or(0);
            char::from(TEXT_ALPHABET[offset])
        })
        .collect()
}

/// `len` copies of one byte, the degenerate payload every family's boundaries
/// are stated at.
#[must_use]
pub fn repeated(byte: u8, len: usize) -> Vec<u8> {
    vec![byte; len]
}

/// An index in `0..bound`, drawn from the sweep stream rooted at `state`.
///
/// A zero bound yields `0` rather than dividing by zero: every caller draws
/// from a table whose length it has already checked.
#[must_use]
pub fn below(state: &mut u64, bound: usize) -> usize {
    let ceiling = u64::try_from(bound.max(1)).unwrap_or(u64::MAX);
    usize::try_from(next_seed(state).rem_euclid(ceiling)).unwrap_or(0)
}

/// Folds every byte of `bytes` into the running trace.
pub fn fold_bytes(trace: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        fold(trace, u64::from(*byte));
    }
}

/// Folds one refusal into the trace as its arm, its first offset and its
/// second offset, so two refusals that differ only in a location cannot
/// collapse into one trace value.
pub fn fold_refusal(trace: &mut u64, arm: u64, first: usize, second: usize) {
    fold(trace, arm);
    fold_usize(trace, first);
    fold_usize(trace, second);
}