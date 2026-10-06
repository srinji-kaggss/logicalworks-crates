//! Seeded payload drawing shared by the deterministic simulation families.
//!
//! One seed drives every payload a family generates, so a failing assertion
//! names the seed that reproduces it. Nothing here interprets a payload: each
//! family decides what its payloads must mean, against a reference model
//! written beside its own assertions.
//!
//! This module is `#[path]`-included by the consolidated `it` binary, the only
//! one that includes it, and every item here has a caller there, so it needs
//! neither an `allow` nor tests of its own: the families that draw from it are
//! its tests.

use crate::seeded_sweep::{fold, fold_usize, next_index, next_seed};

/// The printable ASCII alphabet a text-shaped payload is drawn from.
const TEXT_ALPHABET: &[u8] =
    b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 -_.~/:?";

/// One byte drawn from the sweep stream rooted at `state`.
///
/// The low byte of a finalised word is the byte, which is the same value
/// `x % 256` names and one instruction rather than a checked conversion: the
/// stream's low byte is exactly as good as any other byte of it.
#[must_use]
pub fn next_byte(state: &mut u64) -> u8 {
    next_seed(state).to_le_bytes()[0]
}

/// `len` bytes drawn from the sweep stream rooted at `state`.
#[must_use]
pub fn next_bytes(state: &mut u64, len: usize) -> Vec<u8> {
    (0..len).map(|_| next_byte(state)).collect()
}

/// `LEN` bytes drawn from the sweep stream rooted at `state`, as an array.
///
/// Families that hand a fixed-width value to the crate under test — a UUID's
/// sixteen bytes, a codec's input word — draw it here rather than converting a
/// `Vec` whose length they already declared: the array is filled slot by slot,
/// so there is no length conversion that can fail and no heap allocation for a
/// width the caller wrote down.
#[must_use]
pub fn next_array<const LEN: usize>(state: &mut u64) -> [u8; LEN] {
    let mut drawn = [0_u8; LEN];
    for slot in drawn.iter_mut() {
        *slot = next_byte(state);
    }
    drawn
}

/// `len` printable ASCII characters drawn from the sweep stream rooted at
/// `state`.
///
/// Every character of the alphabet is reachable. The draw is modulo-biased —
/// 256 is not a multiple of the alphabet's length, so the first few characters
/// are one draw more likely than the rest — which is the documented entropy
/// mapping of [`crate::seeded_sweep::next_seed`] applied one byte at a time and
/// is enough for a payload a family asserts a contract about.
#[must_use]
pub fn next_text(state: &mut u64, len: usize) -> String {
    (0..len)
        .map(|_| {
            let offset = usize::from(next_byte(state)).rem_euclid(TEXT_ALPHABET.len());
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
/// The whole word is drawn and reduced by the bound, so a bound wider than one
/// byte of the stream still gets a whole-word draw. A zero bound has no entry
/// to name and yields `0`; every caller indexes a table whose length it has
/// already checked, so an empty table is not a case this draws for.
#[must_use]
pub fn below(state: &mut u64, bound: usize) -> usize {
    next_index(state).rem_euclid(bound.max(1))
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

/// The uppercase hex digits an escape renders, indexed by nibble: the table is
/// the rule rather than a call into `lgwks_std`, so this reference renderer
/// cannot agree with the shipped escaper by construction.
const UPPER_HEX: &[u8; 16] = b"0123456789ABCDEF";

/// The `0x00`..=`0xff` rendering of one byte as two uppercase hex digits, the
/// spelling RFC 3986 requires of an escape.
#[must_use]
pub fn reference_escape(byte: u8) -> String {
    let high = usize::from(byte >> 4);
    let low = usize::from(byte & 0x0f);
    let mut rendered = String::with_capacity(3);
    rendered.push('%');
    rendered.push(char::from(UPPER_HEX[high]));
    rendered.push(char::from(UPPER_HEX[low]));
    rendered
}
