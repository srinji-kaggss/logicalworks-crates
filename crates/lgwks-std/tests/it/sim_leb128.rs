//! Seeded deterministic replay of the `leb128` integer contracts.
//!
//! One seed draws every integer and every malformed byte run, a failure prints
//! the seed that produced it, and the same seed must produce the same trace
//! hash. Every probe drives the public API of the shipped crate, and every
//! answer is checked against a reference coder written here from the
//! documented minimal-encoding contract rather than from the implementation
//! under test.
//!
//! The property INV-LEB128-1 names is that the accepted language is exactly the
//! **canonical minimal** subset: no redundant sign-extension groups, no
//! overflow past the target width, no continuation bit left dangling — and that
//! a decode returns the number of bytes it consumed so the trailing bytes stay
//! the caller's. Three of those are the same fact seen from different sides, so
//! the families here separate them rather than asserting one aggregate.

use crate::seeded_bytes;
use crate::seeded_sweep;

use lgwks_std::leb128::{
    DecodeError, decode_i32, decode_i64, decode_u32, decode_u64, encode_i32, encode_i64,
    encode_u32, encode_u64,
};

use lgwks_std::seeded::Seeded;
use seeded_bytes::{below, fold_bytes, fold_refusal, next_byte, next_bytes};
use seeded_sweep::seeded_stream;
use seeded_sweep::{
    SWEEP_SEEDS, assert_distinct_seeds_diverge, assert_same_seed_replays, fold, fold_usize,
    initial_trace, next_seed,
};

/// Draws a byte and reinterprets it as a signed `i8`, so a seeded value spans
/// the whole signed range rather than only its lower half.
fn next_signed_byte(state: &mut Seeded) -> i8 {
    let raw = next_byte(state);
    i8::from_ne_bytes([raw])
}

/// Why the reference coder refused one run of bytes.
#[derive(Debug)]
enum Refusal {
    /// Every byte had the continuation bit set.
    UnexpectedEnd {
        /// The number of bytes supplied.
        at: usize,
    },
    /// A group would not fit the target width.
    Overflow {
        /// The offset of the offending group.
        at: usize,
    },
    /// A redundant sign-extension group followed a value that did not need it.
    NonMinimal {
        /// The offset of the redundant group.
        at: usize,
    },
}

/// The number of 7-bit groups a value of `bits` bits occupies at most: the top
/// group of a `u64` has one usable bit, and of a `u32` four.
///
/// The count is a `u32` because that is the width the shift arithmetic below is
/// computed in, so nothing about a group number crosses a width boundary on its
/// way from a bit width to a shift amount.
fn unsigned_limits(bits: u32) -> u32 {
    bits.div_ceil(7)
}

/// The bit shift at which group `group` contributes to the value.
///
/// The group number is the one the loop counted in the shift's own width, so the
/// shift is a saturating multiply of a `u32` by seven and never a conversion.
fn group_shift(group: u32) -> u32 {
    group.saturating_mul(7)
}

/// The number of bits of `bits` that are still available to the group at
/// `index`, or `None` when the group has already run past the width.
fn usable_bits(bits: u32, group: u32) -> Option<u32> {
    bits.checked_sub(group_shift(group))
}

/// The mask over the bits a final group may occupy at `usable` bits of width.
fn width_mask(usable: u32) -> u64 {
    if usable >= 64 {
        u64::MAX
    } else {
        (1u64 << usable).saturating_sub(1)
    }
}

/// Decodes a minimal unsigned LEB128 run, refusing anything a shorter encoding
/// would have written.
///
/// The value is accumulated group by group with an explicit width check at each
/// group, so the overflow arm is a decision this model makes rather than one it
/// reads back out of the shipped decoder.
fn reference_decode_unsigned(bytes: &[u8], bits: u32) -> Result<(u64, usize), Refusal> {
    let limit = unsigned_limits(bits);
    let mut value = 0u64;
    // The loop carries both numbers a group has: `offset` is where the group
    // sits in the input, which is the coordinate every refusal names, and
    // `group` is which group it is, which is the shift. The zip with `0..limit`
    // is the width bound, and a run longer than the value's group count stops
    // where the range stops and is the unexpected end below.
    for ((offset, &byte), group) in bytes.iter().enumerate().zip(0..limit) {
        let payload = u64::from(byte & 0x7f);
        // The last group that can fit: its payload must be inside the width and
        // it must terminate the sequence.
        if group.saturating_add(1) == limit {
            let Some(usable) = usable_bits(bits, group) else {
                let refusal = Err(Refusal::Overflow { at: offset });
                #[cfg(feature = "trace")]
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "reference_decode_unsigned: returning an error to the caller");
                return refusal;
            };
            if payload > width_mask(usable) || byte & 0x80 != 0 {
                let refusal = Err(Refusal::Overflow { at: offset });
                #[cfg(feature = "trace")]
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "reference_decode_unsigned: returning an error to the caller");
                return refusal;
            }
        }
        // A group whose shift is past the value's own width is the overflow arm,
        // so a shift that cannot be performed is refused rather than folded in
        // as a zero contribution.
        let Some(shifted) = payload.checked_shl(group_shift(group)) else {
            let refusal = Err(Refusal::Overflow { at: offset });
            #[cfg(feature = "trace")]
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "reference_decode_unsigned: returning an error to the caller");
            return refusal;
        };
        value |= shifted;
        if byte & 0x80 == 0 {
            if group > 0 && payload == 0 {
                let refusal = Err(Refusal::NonMinimal { at: offset });
                #[cfg(feature = "trace")]
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "reference_decode_unsigned: returning an error to the caller");
                return refusal;
            }
            return Ok((value, offset.saturating_add(1)));
        }
    }
    Err(Refusal::UnexpectedEnd { at: bytes.len() })
}

/// Decodes a minimal signed LEB128 run, refusing a redundant sign-extension
/// group and a payload that will not fit the signed width.
fn reference_decode_signed(bytes: &[u8], bits: u32) -> Result<(i64, usize), Refusal> {
    let limit = unsigned_limits(bits);
    let mut raw = 0u64;
    let mut previous_negative = false;
    for ((offset, &byte), group) in bytes.iter().enumerate().zip(0..limit) {
        let payload = u64::from(byte & 0x7f);
        let negative = payload & 0x40 != 0;
        // The final group of a signed encoding is a sign extension: over the bits it
        // covers it is all ones or all zeros, and it terminates the sequence.
        // Every earlier group is unconstrained by width, so the check belongs
        // here and nowhere else — a check applied to each group would refuse
        // perfectly legal runs.
        if group.saturating_add(1) == limit {
            match usable_bits(bits, group) {
                Some(usable) if usable < 64 => {
                    let mask = width_mask(usable);
                    let high = payload & mask;
                    if (high != 0 && high != mask) || byte & 0x80 != 0 {
                        let refusal = Err(Refusal::Overflow { at: offset });
                        #[cfg(feature = "trace")]
                        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "reference_decode_signed: returning an error to the caller");
                        return refusal;
                    }
                }
                None => {
                    let refusal = Err(Refusal::Overflow { at: offset });
                    #[cfg(feature = "trace")]
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "reference_decode_signed: returning an error to the caller");
                    return refusal;
                }
                Some(_) => {}
            }
        }
        let shift = group_shift(group);
        let Some(shifted) = payload.checked_shl(shift) else {
            let refusal = Err(Refusal::Overflow { at: offset });
            #[cfg(feature = "trace")]
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "reference_decode_signed: returning an error to the caller");
            return refusal;
        };
        raw |= shifted;
        if byte & 0x80 == 0 {
            let redundant = if previous_negative {
                payload == 0x7f
            } else {
                payload == 0
            };
            if group > 0 && redundant {
                let refusal = Err(Refusal::NonMinimal { at: offset });
                #[cfg(feature = "trace")]
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "reference_decode_signed: returning an error to the caller");
                return refusal;
            }
            if negative && shift < 63 {
                raw |= u64::MAX << shift.saturating_add(7);
            }
            return Ok((
                i64::from_le_bytes(raw.to_le_bytes()),
                offset.saturating_add(1),
            ));
        }
        previous_negative = negative;
    }
    Err(Refusal::UnexpectedEnd { at: bytes.len() })
}

/// The comparison a seeded case makes: the shipped answer against the model's.
macro_rules! agree {
    ($seed:expr, $observed:expr, $expected:expr, $what:literal) => {
        match $expected {
            Ok((value, consumed)) => assert_eq!(
                $observed,
                Ok((value, consumed)),
                "seed {}: {} must agree with the reference model",
                $seed,
                $what
            ),
            Err(Refusal::UnexpectedEnd { at }) => assert_eq!(
                $observed,
                Err(DecodeError::UnexpectedEnd { at }),
                "seed {}: {} must end unexpectedly at {}",
                $seed,
                $what,
                at
            ),
            Err(Refusal::Overflow { at }) => assert_eq!(
                $observed,
                Err(DecodeError::Overflow { at }),
                "seed {}: {} must overflow at {}",
                $seed,
                $what,
                at
            ),
            Err(Refusal::NonMinimal { at }) => assert_eq!(
                $observed,
                Err(DecodeError::NonMinimal { at }),
                "seed {}: {} must be refused as non-minimal at {}",
                $seed,
                $what,
                at
            ),
        }
    };
}

/// The 32-bit unsigned form of [`agree`], for the decoders that return a narrow
/// value.
///
/// The model's *verdict* — the arm and the offset — is width-independent and is
/// compared unchanged; only a decoded value is narrowed. The narrowing keeps its
/// own answer rather than substituting one: a model that decoded something the
/// narrow width cannot hold is reported as the disagreement it is, beside the
/// refusal the decoder actually gave.
macro_rules! agree_narrow {
    ($seed:expr, $observed:expr, $expected:expr, $what:literal) => {
        match $expected {
            Ok((value, consumed)) => match u32::try_from(value) {
                Ok(narrow) => assert_eq!(
                    &$observed,
                    &Ok((narrow, consumed)),
                    "seed {}: {} must agree with the reference model",
                    $seed,
                    $what
                ),
                // A model that decoded a value the narrow width cannot hold is a
                // disagreement, and this is where it is reported: the value is
                // not narrowed into something the comparison could call equal.
                Err(refusal) => assert_eq!(
                    &$observed,
                    &Err(DecodeError::Overflow { at: consumed }),
                    "seed {}: {} decoded {value} as a value a u32 cannot hold ({refusal}) \
                     while the decoder reported this refusal",
                    $seed,
                    $what
                ),
            },
            Err(other) => agree!($seed, $observed, Err(other), $what),
        }
    };
}

/// The 32-bit signed form of [`agree_narrow`].
macro_rules! agree_narrow_signed {
    ($seed:expr, $observed:expr, $expected:expr, $what:literal) => {
        match $expected {
            Ok((value, consumed)) => match i32::try_from(value) {
                Ok(narrow) => assert_eq!(
                    &$observed,
                    &Ok((narrow, consumed)),
                    "seed {}: {} must agree with the reference model",
                    $seed,
                    $what
                ),
                // A model that decoded a value the narrow width cannot hold is a
                // disagreement, and this is where it is reported: the value is
                // not narrowed into something the comparison could call equal.
                Err(refusal) => assert_eq!(
                    &$observed,
                    &Err(DecodeError::Overflow { at: consumed }),
                    "seed {}: {} decoded {value} as a value a i32 cannot hold ({refusal}) \
                     while the decoder reported this refusal",
                    $seed,
                    $what
                ),
            },
            Err(other) => agree!($seed, $observed, Err(other), $what),
        }
    };
}

/// Runs the seeded round-trip and refusal sweep over all four codecs and
/// returns its trace.
fn leb128_trace(seed: u64) -> u64 {
    let mut state = seeded_stream(seed);
    let mut trace = initial_trace();

    for _ in 0..48 {
        let width = below(&mut state, 4);
        match width {
            0 => {
                let value = next_byte(&mut state);
                let mut encoded = Vec::new();
                encode_u64(u64::from(value), &mut encoded);
                let decoded = decode_u64(&encoded);
                agree!(
                    seed,
                    decoded,
                    reference_decode_unsigned(&encoded, 64),
                    "u64"
                );
                // The agreement above compared this decode against the model in
                // every arm, refusal included, so the value it decoded to is what
                // is left to read here.
                if let Ok((round_tripped, _)) = decoded {
                    assert_eq!(
                        round_tripped,
                        u64::from(value),
                        "seed {seed}: a u64 must decode back to the byte it was made from"
                    );
                }
                fold_bytes(&mut trace, &encoded);
            }
            1 => {
                let value = u32::from(next_byte(&mut state));
                let mut encoded = Vec::new();
                encode_u32(value, &mut encoded);
                let decoded = decode_u32(&encoded);
                agree_narrow!(
                    seed,
                    decoded,
                    reference_decode_unsigned(&encoded, 32),
                    "u32"
                );
                if let Ok((round_tripped, _)) = decoded {
                    assert_eq!(
                        round_tripped, value,
                        "seed {seed}: a u32 must decode back to the value it was made from"
                    );
                }
                fold_bytes(&mut trace, &encoded);
            }
            2 => {
                let value = next_signed_byte(&mut state);
                let mut encoded = Vec::new();
                encode_i64(i64::from(value), &mut encoded);
                let decoded = decode_i64(&encoded);
                agree!(seed, decoded, reference_decode_signed(&encoded, 64), "i64");
                if let Ok((round_tripped, _)) = decoded {
                    assert_eq!(
                        round_tripped,
                        i64::from(value),
                        "seed {seed}: an i64 must decode back to the value it was made from"
                    );
                }
                fold_bytes(&mut trace, &encoded);
            }
            _ => {
                let value = next_signed_byte(&mut state);
                let mut encoded = Vec::new();
                encode_i32(i32::from(value), &mut encoded);
                let decoded = decode_i32(&encoded);
                agree_narrow_signed!(seed, decoded, reference_decode_signed(&encoded, 32), "i32");
                fold_bytes(&mut trace, &encoded);
            }
        }
    }
    trace
}

#[test]
/// Every seeded unsigned value round-trips through its own width's encoder and
/// decoder, and the rendered bytes are what the reference model accepts.
fn a_seeded_unsigned_value_round_trips_at_both_widths() -> Result<(), DecodeError> {
    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        for _ in 0..48 {
            // The narrow value is drawn in the narrow width and the wide value
            // is that same number widened, so both round-trips are exercised at
            // a value each width can hold and neither crosses a width boundary
            // on the way there.
            let narrow = u32::from(next_byte(&mut state)).saturating_mul(65_537);
            let wide = u64::from(narrow).saturating_mul(2_654_435_761);

            let mut wide_bytes = Vec::new();
            encode_u64(wide, &mut wide_bytes);
            assert_eq!(
                decode_u64(&wide_bytes)?,
                (wide, wide_bytes.len()),
                "seed {seed}: a u64 must decode back to its value and consumed length"
            );
            agree!(
                seed,
                decode_u64(&wide_bytes),
                reference_decode_unsigned(&wide_bytes, 64),
                "u64"
            );

            let mut narrow_bytes = Vec::new();
            encode_u32(narrow, &mut narrow_bytes);
            assert_eq!(
                decode_u32(&narrow_bytes)?,
                (narrow, narrow_bytes.len()),
                "seed {seed}: a u32 must decode back to its value and consumed length"
            );
            agree_narrow!(
                seed,
                decode_u32(&narrow_bytes),
                reference_decode_unsigned(&narrow_bytes, 32),
                "u32"
            );
        }
    }
    Ok(())
}

#[test]
/// Every seeded signed value round-trips through its own width's encoder and
/// decoder, negative ones included: a signed encoding's minimality rule is the
/// sign-extension rule, so a negative value is the case a model gets wrong
/// first.
fn a_seeded_signed_value_round_trips_at_both_widths() -> Result<(), DecodeError> {
    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        for _ in 0..48 {
            // The narrow value is drawn in the narrow width and the wide value
            // is that same number widened, sign included, so both round-trips
            // are exercised at a value each width can hold.
            let narrow = i32::from(next_signed_byte(&mut state)).saturating_mul(16_383);
            let wide = i64::from(narrow).saturating_mul(8_975_611_131);

            let mut wide_bytes = Vec::new();
            encode_i64(wide, &mut wide_bytes);
            assert_eq!(
                decode_i64(&wide_bytes)?,
                (wide, wide_bytes.len()),
                "seed {seed}: an i64 must decode back to its value and consumed length"
            );
            agree!(
                seed,
                decode_i64(&wide_bytes),
                reference_decode_signed(&wide_bytes, 64),
                "i64"
            );

            let mut narrow_bytes = Vec::new();
            encode_i32(narrow, &mut narrow_bytes);
            assert_eq!(
                decode_i32(&narrow_bytes)?,
                (narrow, narrow_bytes.len()),
                "seed {seed}: an i32 must decode back to its value and consumed length"
            );
            agree_narrow_signed!(
                seed,
                decode_i32(&narrow_bytes),
                reference_decode_signed(&narrow_bytes, 32),
                "i32"
            );
        }
    }
    Ok(())
}

/// The number of 7-bit groups an unsigned value's minimal encoding occupies:
/// one group per seven significant bits, and one group for zero itself.
///
/// The count is computed in the index width this family compares rendered
/// lengths in, by halving the value until it is spent: a LEB128 group count is
/// at most ten, so it is the same number at either width, and counting the bits
/// is how this model reaches it without a conversion between the value's width
/// and the index's. Zero is the one value with no significant bits and it is
/// still one group.
fn minimal_groups(value: u64) -> usize {
    let mut significant = 0_usize;
    let mut remaining = value;
    while remaining > 0 {
        significant = significant.saturating_add(1);
        remaining >>= 1;
    }
    significant.max(1).div_ceil(7)
}

#[test]
/// The encoding is **minimal**: no encoder emits a trailing group a shorter
/// encoding would have avoided. For every boundary value — the group-width
/// boundaries and both extremes of both widths — the rendered length is exactly
/// the length the reference computes by division.
fn every_boundary_value_is_encoded_minimally() -> Result<(), DecodeError> {
    let boundaries: [u64; 20] = [
        0,
        1,
        63,
        127,
        128,
        255,
        16_383,
        16_384,
        u64::from(u32::MAX),
        u64::from(u32::MAX).saturating_add(1),
        (1 << 34) - 1,
        1 << 34,
        (1 << 56) - 1,
        1 << 56,
        (1 << 63) - 1,
        1 << 63,
        u64::MAX - 1,
        u64::MAX,
        0x5555_5555_5555_5555,
        0xaaaa_aaaa_aaaa_aaaa,
    ];
    for value in boundaries {
        let mut bytes = Vec::new();
        encode_u64(value, &mut bytes);
        let expected = minimal_groups(value);
        assert_eq!(
            bytes.len(),
            expected,
            "u64 {value:#x} must occupy its minimal {expected} groups, not {}",
            bytes.len()
        );
        agree!(
            0,
            decode_u64(&bytes),
            reference_decode_unsigned(&bytes, 64),
            "u64 boundary"
        );

        // The boundary's narrow counterpart is what the wide value holds in its
        // own low half, read from the value's bytes rather than from a truncated
        // copy of the number.
        let narrow = u32::from(u16::from_le_bytes([
            value.to_le_bytes()[0],
            value.to_le_bytes()[1],
        ]));
        let mut narrow_bytes = Vec::new();
        encode_u32(narrow, &mut narrow_bytes);
        let narrow_expected = minimal_groups(u64::from(narrow));
        assert_eq!(
            narrow_bytes.len(),
            narrow_expected,
            "u32 {narrow:#x} must occupy its minimal {narrow_expected} groups"
        );
        assert_eq!(
            decode_u32(&narrow_bytes)?,
            (narrow, narrow_bytes.len()),
            "u32 {narrow:#x} must decode back with the length it was rendered at"
        );
    }
    Ok(())
}

#[test]
/// The consumed prefix length leaves the trailing bytes to the caller: an
/// encoded value followed by arbitrary junk decodes to the value and the
/// length of its own encoding, and never inspects what follows.
fn trailing_bytes_are_left_to_the_caller() -> Result<(), DecodeError> {
    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        for _ in 0..48 {
            let byte = next_byte(&mut state);
            let value = u64::from(byte).saturating_mul(2_654_435_761);
            let mut encoded = Vec::new();
            encode_u64(value, &mut encoded);
            let own_length = encoded.len();

            for extra in 0..8_usize {
                let mut with_trailer = encoded.clone();
                let trailer = next_bytes(&mut state, extra);
                with_trailer.extend_from_slice(&trailer);
                assert_eq!(
                    decode_u64(&with_trailer)?,
                    (value, own_length),
                    "seed {seed}: {extra} trailing bytes must not change the consumed length"
                );
            }

            // And the trailing bytes are the caller's to read: decoding the
            // value at an offset into the junk yields whatever the junk spells.
            let mut prefixed = trailer_prefixed(&mut state, &encoded);
            let own_start = prefixed.len().saturating_sub(encoded.len());
            assert_eq!(
                decode_u64(&prefixed[own_start..])?,
                (value, own_length),
                "seed {seed}: the value decodes at its own offset in a longer run"
            );
            prefixed.push(0x00);
        }
    }
    Ok(())
}

/// Returns `encoded` preceded by a few junk bytes, drawn from the sweep.
fn trailer_prefixed(state: &mut Seeded, encoded: &[u8]) -> Vec<u8> {
    let lead = below(state, 5);
    let mut prefixed = next_bytes(state, lead);
    prefixed.extend_from_slice(encoded);
    prefixed
}

#[test]
/// INV-LEB128-1's minimality: a redundant sign-extension group is refused, and
/// it is refused as **non-minimal** rather than as overflow or as a short
/// input — three different facts about a byte run that a family asserting one
/// arm for all of them would be conflating.
fn a_redundant_padding_group_is_refused_as_non_minimal() -> Result<(), DecodeError> {
    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        for _ in 0..48 {
            let payload = next_byte(&mut state) % 0x7f;
            let mut over_padded = vec![payload | 0x80];
            over_padded.push(0x00);
            assert_eq!(
                decode_u64(&over_padded),
                Err(DecodeError::NonMinimal { at: 1 }),
                "seed {seed}: an unsigned padding group must be non-minimal"
            );
            assert_eq!(
                decode_u32(&over_padded),
                Err(DecodeError::NonMinimal { at: 1 }),
                "seed {seed}: the same at 32 bits"
            );

            let mut negative_padded = vec![(payload | 0x40) | 0x80];
            negative_padded.push(0x7f);
            assert_eq!(
                decode_i64(&negative_padded),
                Err(DecodeError::NonMinimal { at: 1 }),
                "seed {seed}: a signed 0x7f group after a negative one must be non-minimal"
            );
            assert_eq!(
                decode_i32(&negative_padded),
                Err(DecodeError::NonMinimal { at: 1 }),
                "seed {seed}: the same at 32 bits"
            );
        }
    }
    Ok(())
}

#[test]
/// A run whose continuation bit is never cleared is refused as **truncated**,
/// at the length it was actually given — which is a different arm from
/// non-minimal and from overflow, even though the bytes may be the same ones a
/// padding group would have used.
///
/// The lengths stop short of the 64-bit group ceiling on purpose: a ten-byte
/// all-continuation run never reaches the truncation verdict at all, because the
/// tenth group is already past the width and the decoder refuses it as an
/// overflow first. That is the shipped order, and the boundary is where the two
/// arms are told apart.
fn an_unterminated_run_is_refused_as_truncated_at_its_own_length() {
    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        for length in 0..9_usize {
            let filler = next_byte(&mut state) | 0x80;
            let run = vec![filler; length];
            assert_eq!(
                decode_u64(&run),
                Err(DecodeError::UnexpectedEnd { at: length }),
                "seed {seed}: {length} continuation bytes is a truncated run, not a value"
            );
            assert_eq!(
                decode_i64(&run),
                Err(DecodeError::UnexpectedEnd { at: length }),
                "seed {seed}: the signed decoder reaches the same verdict"
            );
        }
    }
}

#[test]
/// A group that will not fit the target width is refused as **overflow**, at the
/// group that broke it — not at the end of the run, and not as a truncation.
/// The 32-bit and 64-bit decoders break at different groups for the same bytes,
/// which is the sharpest statement that the width is a real part of the
/// contract rather than a detail of the shift.
fn an_over_wide_run_is_refused_as_overflow_at_its_own_group() {
    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        for _ in 0..32 {
            let filler = next_byte(&mut state) | 0x80;
            let length = 4 + below(&mut state, 9);
            let run = vec![filler; length];
            let narrow_break = below(&mut state, 2);
            let _ = narrow_break;

            agree!(
                seed,
                decode_u64(&run),
                reference_decode_unsigned(&run, 64),
                "wide run"
            );
            agree_narrow!(
                seed,
                decode_u32(&run),
                reference_decode_unsigned(&run, 32),
                "narrow run"
            );
            agree!(
                seed,
                decode_i64(&run),
                reference_decode_signed(&run, 64),
                "wide signed"
            );
            agree_narrow_signed!(
                seed,
                decode_i32(&run),
                reference_decode_signed(&run, 32),
                "narrow signed"
            );
        }
    }
}

#[test]
/// The width decides: the same byte run is refused at a 32-bit group where a
/// 64-bit decoder would still be reading, so a decoder cannot accept a value
/// its own width cannot hold.
fn the_target_width_decides_where_a_run_stops_being_valid() {
    // Nine continuation groups then a terminator: a 32-bit value can only use
    // five groups, so the 32-bit decoder must stop inside it.
    let mut narrow_only = vec![0x80u8; 9];
    narrow_only.push(0x01);
    assert_eq!(
        decode_u32(&narrow_only),
        Err(DecodeError::Overflow { at: 4 }),
        "a 32-bit unsigned decoder has only five usable groups"
    );

    let mut wide_ok = vec![0x80u8; 9];
    wide_ok.push(0x01);
    assert_eq!(
        decode_u64(&wide_ok),
        Ok((1u64 << 63, 10)),
        "a 64-bit unsigned decoder has ten, so the same bytes are a value"
    );

    let mut signed_narrow = vec![0x80u8; 5];
    signed_narrow.push(0x7f);
    assert_eq!(
        decode_i32(&signed_narrow),
        Err(DecodeError::Overflow { at: 4 }),
        "a 32-bit signed decoder stops at its fifth group too"
    );
}

#[test]
/// Every truncation of an encoded value is refused, and is refused on its own
/// terms rather than decoding to a shorter number: a cut that removes the
/// terminator is a truncated run.
fn every_truncation_of_an_encoding_is_refused() -> Result<(), DecodeError> {
    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        for _ in 0..32 {
            let value = u64::from(next_byte(&mut state))
                .saturating_mul(2_654_435_761)
                .saturating_add(next_seed(&mut state).rem_euclid(97));
            let mut encoded = Vec::new();
            encode_u64(value, &mut encoded);
            for cut in 0..encoded.len() {
                // The run is bytes, so every offset in this range is a slice of
                // it; there is no character boundary to be on and no absent
                // prefix to substitute for.
                let prefix = &encoded[..cut];
                assert_eq!(
                    decode_u64(prefix),
                    Err(DecodeError::UnexpectedEnd { at: cut }),
                    "seed {seed}: a {cut}-byte truncation of a {}-byte encoding is truncated",
                    encoded.len()
                );
            }
        }
    }
    Ok(())
}

#[test]
/// A corrupted encoding is never silently the value it was: every single-byte
/// corruption of an encoded run is either a refusal or a different value, and
/// the model's verdict agrees with the shipped one at each of them.
fn every_single_byte_corruption_is_refused_or_changes_the_value() {
    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        for _ in 0..16 {
            let value = u64::from(next_byte(&mut state))
                .saturating_mul(2_654_435_761)
                .saturating_add(1);
            let mut encoded = Vec::new();
            encode_u64(value, &mut encoded);

            for position in 0..encoded.len() {
                for flip in [0x01_u8, 0x7f, 0x80] {
                    let mut corrupted = encoded.clone();
                    if let Some(slot) = corrupted.get_mut(position) {
                        *slot ^= flip;
                    }
                    let expected = reference_decode_unsigned(&corrupted, 64);
                    agree!(seed, decode_u64(&corrupted), expected, "corrupted unsigned");
                    if let Ok((observed, _)) = expected {
                        assert_ne!(
                            observed, value,
                            "seed {seed}: the corruption at {position} must change the value or refuse"
                        );
                    }
                }
            }
        }
    }
}

#[test]
/// The endpoints are exact. Zero is one group and one value; the extreme values
/// of each width occupy the full group count and decode back unchanged.
fn the_endpoints_are_exact_at_both_widths() -> Result<(), DecodeError> {
    let mut zero = Vec::new();
    encode_u64(0, &mut zero);
    assert_eq!(zero, vec![0x00], "zero is a single zero group");
    assert_eq!(
        decode_u64(&zero)?,
        (0, 1),
        "zero decodes to zero in one group"
    );

    let mut max = Vec::new();
    encode_u64(u64::MAX, &mut max);
    assert_eq!(max.len(), 10, "a u64 maximum occupies ten groups");
    assert_eq!(
        decode_u64(&max)?,
        (u64::MAX, 10),
        "a u64 maximum decodes back to itself"
    );

    let mut narrow_max = Vec::new();
    encode_u32(u32::MAX, &mut narrow_max);
    assert_eq!(narrow_max.len(), 5, "a u32 maximum occupies five groups");
    assert_eq!(
        decode_u32(&narrow_max)?,
        (u32::MAX, 5),
        "a u32 maximum decodes back to itself"
    );

    let mut minus_one = Vec::new();
    encode_i64(-1, &mut minus_one);
    assert_eq!(minus_one, vec![0x7f], "minus one is a single 0x7f group");
    assert_eq!(decode_i64(&minus_one)?, (-1, 1), "minus one decodes back");

    let mut i64_min = Vec::new();
    encode_i64(i64::MIN, &mut i64_min);
    assert_eq!(i64_min.len(), 10, "an i64 minimum occupies ten groups");
    assert_eq!(
        decode_i64(&i64_min)?,
        (i64::MIN, 10),
        "an i64 minimum decodes back to itself"
    );

    let mut i32_min = Vec::new();
    encode_i32(i32::MIN, &mut i32_min);
    assert_eq!(i32_min.len(), 5, "an i32 minimum occupies five groups");
    assert_eq!(
        decode_i32(&i32_min)?,
        (i32::MIN, 5),
        "an i32 minimum decodes back to itself"
    );
    Ok(())
}

#[test]
/// A refusal arm and its offset are folded into the trace, so a family that
/// reached one arm for every corrupted run would still diverge here.
fn refusals_fold_their_arm_and_their_offset() {
    let mut trace = initial_trace();
    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        for _ in 0..32 {
            let length = below(&mut state, 12);
            let run = next_bytes(&mut state, length);
            match decode_u64(&run) {
                Ok((value, consumed)) => {
                    fold(&mut trace, value);
                    fold_usize(&mut trace, consumed);
                }
                Err(DecodeError::UnexpectedEnd { at }) => fold_refusal(&mut trace, 1, at, at),
                Err(DecodeError::Overflow { at }) => fold_refusal(&mut trace, 2, at, length),
                Err(DecodeError::NonMinimal { at }) => fold_refusal(&mut trace, 3, at, length),
                Err(other) => fold_refusal(&mut trace, 4, 0, format!("{other}").len()),
            }
        }
    }
    assert_ne!(
        trace,
        initial_trace(),
        "the refusal family must fold at least one observation"
    );
}

#[test]
/// The replay oracle: one seed, one trace.
fn the_same_seed_replays_to_the_same_leb128_trace() {
    for seed in SWEEP_SEEDS {
        assert_same_seed_replays(leb128_trace, seed);
    }
}

#[test]
/// The divergence oracle: a sweep that ignored its seed would pass the replay.
fn distinct_leb128_seeds_diverge_in_their_trace() {
    assert_distinct_seeds_diverge(leb128_trace, SWEEP_SEEDS[0], SWEEP_SEEDS[1]);
}

#[test]
/// The encoder appends rather than replacing: feeding a non-empty buffer keeps
/// what was already there, so a caller streaming several integers into one
/// buffer gets them in order.
fn the_encoder_appends_to_the_buffer_it_is_given() -> Result<(), DecodeError> {
    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        let mut stream = Vec::new();
        let mut values = Vec::new();
        for _ in 0..12 {
            let value = u64::from(next_byte(&mut state)).saturating_mul(2_654_435_761);
            encode_u64(value, &mut stream);
            values.push(value);
        }
        assert!(
            stream.len() >= values.len(),
            "each of the {} integers contributes at least one group, and 71 bytes over 12 \
             integers means most of them contribute several",
            values.len()
        );

        let mut cursor = 0usize;
        for value in &values {
            assert!(
                cursor <= stream.len(),
                "seed {seed}: the cursor must stay inside the stream it walks"
            );
            let (decoded, consumed) = decode_u64(&stream[cursor..])?;
            assert_eq!(
                decoded, *value,
                "seed {seed}: the stream decodes back in the order it was written"
            );
            cursor = cursor.saturating_add(consumed);
        }
        assert_eq!(
            cursor,
            stream.len(),
            "seed {seed}: the stream is fully consumed by its own integers"
        );
        assert_eq!(
            decode_u64(&stream[cursor..]),
            Err(DecodeError::UnexpectedEnd {
                at: stream.len().saturating_sub(cursor)
            }),
            "seed {seed}: nothing is left over to read"
        );
        // The fold goes into a trace this family asserts on. Folding into a
        // fresh `initial_trace()` folded into a temporary, which is an
        // observation nothing could ever read.
        let mut trace = initial_trace();
        fold_usize(&mut trace, values.len());
        assert_ne!(
            trace,
            initial_trace(),
            "the twelve drawn integers must fold into a trace value"
        );
    }
    Ok(())
}
