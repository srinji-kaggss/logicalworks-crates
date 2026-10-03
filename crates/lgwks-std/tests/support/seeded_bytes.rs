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
const TEXT_ALPHABET: &[u8] =
    b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 -_.~/:?";

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

/// Decodes one ASCII character to the nibble it denotes, or `None` when it is
/// outside the documented hex alphabet `[0-9a-fA-F]`.
///
/// Three families need this exact predicate as an oracle: the hex codec's own
/// reference decoder, the percent escaper's reference decoder, and the UUID
/// reference parser. Each uses it to compute an answer the shipped code never
/// returns, so the shared copy is a model rather than a wrapper — it does not
/// call into `lgwks_std` and must not start to.
#[must_use]
pub fn reference_nibble(character: u8) -> Option<u8> {
    match character {
        b'0'..=b'9' => character.checked_sub(b'0'),
        b'a'..=b'f' => character
            .checked_sub(b'a')
            .and_then(|nibble| nibble.checked_add(10)),
        b'A'..=b'F' => character
            .checked_sub(b'A')
            .and_then(|nibble| nibble.checked_add(10)),
        _ => None,
    }
}

/// The `0x00`..=`0xff` rendering of one byte as two uppercase hex digits, the
/// spelling RFC 3986 requires of an escape.
#[must_use]
pub fn reference_escape(byte: u8) -> String {
    let high = u32::from(byte >> 4);
    let low = u32::from(byte & 0x0f);
    let mut rendered = String::from("%");
    rendered.push(
        char::from_digit(high, 16)
            .unwrap_or('0')
            .to_ascii_uppercase(),
    );
    rendered.push(
        char::from_digit(low, 16)
            .unwrap_or('0')
            .to_ascii_uppercase(),
    );
    rendered
}
