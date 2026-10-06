//! Seeded payload drawing shared by the deterministic simulation families.
//!
//! One seed drives every payload a family generates, so a failing assertion
//! names the seed that reproduces it. Nothing here interprets a payload: each
//! family decides what its payloads must mean, against a reference model
//! written beside its own assertions.
//!
//! This module is `#[path]`-included by the sweeps; it is not a test binary. It
//! carries its own tests rather than an `allow`: every including binary uses a
//! different subset of it, and a fixture that is only correct where its unused
//! half is silenced is a fixture nobody has read.

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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::seeded_sweep::{SWEEP_SEEDS, initial_trace};

    use super::{
        TEXT_ALPHABET, below, fold_bytes, fold_refusal, next_array, next_bytes, next_text,
        reference_escape, reference_nibble, repeated,
    };

    #[test]
    /// A wide payload is genuinely drawn, not one block repeated.
    fn sim_a_payload_is_drawn_not_repeated() {
        // The defect the stream's finaliser prevents, observed at the payload
        // level: an unfinalised stream's low byte repeats every 256 draws, so a
        // 4 KiB payload was sixteen copies of one 256-byte block.
        let mut state = SWEEP_SEEDS[0];
        let payload = next_bytes(&mut state, 4_096);
        assert_eq!(
            payload.len(),
            4_096,
            "the drawn payload must be exactly the requested length"
        );
        assert_ne!(
            payload[..256],
            payload[256..512],
            "the second 256-byte block repeated the first"
        );
        let distinct = payload.iter().collect::<BTreeSet<_>>().len();
        assert_eq!(
            distinct, 256,
            "a 4 KiB payload covered {distinct} of the 256 byte values"
        );
    }

    #[test]
    /// One seed, one payload.
    fn sim_a_drawn_payload_replays_from_the_same_seed() {
        let mut first = SWEEP_SEEDS[2];
        let mut second = SWEEP_SEEDS[2];
        assert_eq!(
            next_bytes(&mut first, 64),
            next_bytes(&mut second, 64),
            "the same seed must draw the same payload"
        );
    }

    #[test]
    /// Drawn text stays in the alphabet and reaches every character of it.
    fn sim_text_stays_inside_the_alphabet_and_reaches_it() {
        let mut state = SWEEP_SEEDS[1];
        let text = next_text(&mut state, 4_096);
        assert_eq!(
            text.chars().count(),
            4_096,
            "the drawn text must be exactly the requested character count"
        );
        let reached: BTreeSet<char> = text.chars().collect();
        for character in &reached {
            assert!(
                TEXT_ALPHABET
                    .iter()
                    .any(|byte| char::from(*byte) == *character),
                "character {character:?} is outside the documented alphabet"
            );
        }
        assert_eq!(
            reached.len(),
            TEXT_ALPHABET.len(),
            "the drawn text reached {} of the alphabet's {} characters, so one is unreachable",
            reached.len(),
            TEXT_ALPHABET.len()
        );
    }

    #[test]
    /// An index is inside its bound at every width a family asks for.
    fn an_index_is_inside_its_bound_at_every_drawn_bound() {
        for bound in [1_usize, 2, 6, 48, 4_096] {
            let mut state = SWEEP_SEEDS[0];
            for _ in 0..256 {
                assert!(
                    below(&mut state, bound) < bound,
                    "an index drawn for bound {bound} was not inside it"
                );
            }
        }
    }

    #[test]
    /// No value is below zero, so the draw yields the only one it can.
    fn a_zero_bound_yields_zero_rather_than_dividing_by_zero() {
        let mut state = SWEEP_SEEDS[0];
        assert_eq!(
            below(&mut state, 0),
            0,
            "an empty table has no index, so the draw must yield the only value it can"
        );
    }

    #[test]
    /// A fixed-width draw is the same stream as a vector draw of that width.
    fn sim_a_fixed_width_draw_equals_the_vector_draw_of_that_width() {
        let mut from_array = SWEEP_SEEDS[0];
        let mut from_vector = SWEEP_SEEDS[0];
        assert_eq!(
            next_array::<16>(&mut from_array).as_slice(),
            next_bytes(&mut from_vector, 16).as_slice(),
            "an array draw and a vector draw of one width must consume the stream identically"
        );
        assert_eq!(
            next_array::<0>(&mut from_array),
            [0_u8; 0],
            "a zero-width draw is the empty array and consumes no stream"
        );
    }

    #[test]
    /// The degenerate payload is the repeated byte.
    fn a_constant_payload_is_the_repeated_byte() {
        assert_eq!(
            repeated(0x5a, 4),
            vec![0x5a; 4],
            "a constant payload must be the repeated byte"
        );
        assert!(
            repeated(0x00, 0).is_empty(),
            "a zero-length constant payload is empty"
        );
    }

    #[test]
    /// A refusal carries its arm and both offsets into the trace.
    fn two_refusals_that_differ_only_in_a_location_are_two_traces() {
        let mut first = initial_trace();
        fold_refusal(&mut first, 1, 4, 9);
        let mut second = initial_trace();
        fold_refusal(&mut second, 1, 9, 4);
        let mut third = initial_trace();
        fold_refusal(&mut third, 2, 4, 9);
        assert_ne!(
            first, second,
            "two refusals differing only in which offset they named folded alike"
        );
        assert_ne!(first, third, "two refusals of different arms folded alike");
    }

    #[test]
    /// Folding a payload whole equals folding it in two runs.
    fn a_byte_fold_is_the_sum_of_its_byte_folds() {
        let mut whole = initial_trace();
        fold_bytes(&mut whole, b"abcd");
        let mut split = initial_trace();
        fold_bytes(&mut split, b"ab");
        fold_bytes(&mut split, b"cd");
        assert_eq!(
            whole, split,
            "folding a payload whole must equal folding it in two runs"
        );
    }

    #[test]
    /// The reference nibble decoder accepts exactly `[0-9a-fA-F]`, in either case.
    fn the_hex_alphabet_is_exactly_the_documented_one() {
        for nibble in 0..16_u8 {
            let index = usize::from(nibble);
            assert_eq!(
                reference_nibble(b"0123456789abcdef"[index]),
                Some(nibble),
                "the lowercase spelling of nibble {nibble} must decode to it"
            );
            assert_eq!(
                reference_nibble(b"0123456789ABCDEF"[index]),
                Some(nibble),
                "the uppercase spelling of nibble {nibble} must decode to it"
            );
        }
        for outside in [b'g', b'G', b'/', b' ', 0x00, b'@', b'`', 0x7f] {
            assert_eq!(
                reference_nibble(outside),
                None,
                "byte {outside:#04x} is outside the hex alphabet and must decode to nothing"
            );
        }
    }

    #[test]
    /// Every byte escapes to three characters, in the spelling RFC 3986 requires.
    fn an_escape_is_two_uppercase_digits_behind_a_percent() {
        assert_eq!(
            reference_escape(0x00),
            "%00",
            "the zero byte escapes as %00"
        );
        assert_eq!(
            reference_escape(0xff),
            "%FF",
            "the all-bits byte escapes as %FF, uppercase as RFC 3986 requires"
        );
        assert_eq!(
            reference_escape(0x2f),
            "%2F",
            "a lowercase letter in the escape must be rendered uppercase"
        );
        for byte in 0..=u8::MAX {
            let escaped = reference_escape(byte);
            assert_eq!(
                escaped.len(),
                3,
                "byte {byte:#04x} escaped to {escaped:?}, which is not three characters"
            );
        }
    }
}
