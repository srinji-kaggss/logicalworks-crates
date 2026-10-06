//! Seeded deterministic replay of the `hex` transcoding contracts.
//!
//! One seed draws every payload, a failure prints the seed that produced it,
//! and the same seed must produce the same trace hash. Every probe drives the
//! public API of the shipped crate, and every answer is checked against a
//! reference transcoder written here from the documented contract rather than
//! from the implementation under test.
//!
//! The property INV-HEX-1 names is the atomicity of `decode_into`: it requires
//! the exact destination length, and every refusal — a short destination, a
//! long one, an odd input, a non-digit anywhere — leaves the caller's buffer
//! byte for byte as it was.

use crate::seeded_bytes;
use crate::seeded_sweep;

use lgwks_std::hex::{DecodeError, decode, decode_into, encode};

use seeded_bytes::{
    below, fold_bytes, fold_refusal, next_byte, next_bytes, next_text, reference_nibble, repeated,
};
use seeded_sweep::{
    SWEEP_SEEDS, assert_distinct_seeds_diverge, assert_same_seed_replays, fold_usize, initial_trace,
};

/// The widest payload any family in this file generates, so a boundary case is
/// a wide payload and not only a one-byte one.
const WIDE_PAYLOAD_BYTES: usize = 4_096;

/// The payload lengths every family in this file states its properties at.
const BOUNDARY_LENGTHS: [usize; 6] = [0, 1, 2, 3, 255, WIDE_PAYLOAD_BYTES];

/// A character the hex alphabet never contains, used to corrupt an input.
const ALIEN_DIGIT: u8 = b'g';

/// How many payloads the case-drawing family redraws before it insists.
///
/// It is a bound and not a loop condition that never ends: six of a byte's
/// sixteen low-nibble values are letters, so a payload of one byte or more
/// carries one with probability at least three in eight per byte.
const LETTER_DRAW_LIMIT: usize = 8;

/// The lowercase hex alphabet, indexed by nibble: the table is the documented
/// two-characters-per-byte rule rather than a call into the shipped encoder, so
/// this reference cannot agree with it by construction.
const LOWER_HEX: &[u8; 16] = b"0123456789abcdef";

/// What the reference transcoder says about one payload.
enum Reference {
    /// The payload decoded to these bytes.
    Decoded(Vec<u8>),
    /// The payload was refused, for this reason.
    Refused(Refusal),
}

/// Why the reference transcoder refused a payload.
enum Refusal {
    /// An odd number of characters: no whole final pair exists.
    Odd {
        /// The observed length, which is also the reported offset.
        len: usize,
    },
    /// A character outside `[0-9a-fA-F]`, at its own offset.
    NotDigit {
        /// Offset of the first offending character.
        at: usize,
        /// The offending byte, verbatim.
        byte: u8,
    },
}

/// Encodes bytes as lowercase base-16, written from the documented two
/// characters per byte and lowercase digit rule rather than from the shipped
/// encoder.
fn reference_encode(bytes: &[u8]) -> String {
    let mut rendered = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        rendered.push(char::from(LOWER_HEX[usize::from(byte >> 4)]));
        rendered.push(char::from(LOWER_HEX[usize::from(byte & 0x0f)]));
    }
    rendered
}

/// Decodes a hex payload under the documented contract: even length, every
/// character in the alphabet, and the first offending character reported rather
/// than any later one.
///
/// Each byte is assembled arithmetically from its two nibbles, so the value
/// this returns is computed here rather than read back out of the shipped
/// decoder.
fn reference_decode(text: &[u8]) -> Reference {
    if !text.len().is_multiple_of(2) {
        return Reference::Refused(Refusal::Odd { len: text.len() });
    }
    let mut decoded = Vec::new();
    let (pairs, _) = text.as_chunks::<2>();
    for (index, pair) in pairs.iter().enumerate() {
        let base = index.saturating_mul(2);
        let Some(high) = reference_nibble(pair[0]) else {
            return Reference::Refused(Refusal::NotDigit {
                at: base,
                byte: pair[0],
            });
        };
        let Some(low) = reference_nibble(pair[1]) else {
            return Reference::Refused(Refusal::NotDigit {
                at: base.saturating_add(1),
                byte: pair[1],
            });
        };
        decoded.push((high << 4) | low);
    }
    Reference::Decoded(decoded)
}

/// The rendered character count a payload of `length` bytes must produce.
fn encoded_len(length: usize) -> usize {
    length.saturating_mul(2)
}

/// The destination widths a family hands to `decode_into` beside the exact one,
/// so a short and a long buffer are both always on the table. An entry equal to
/// the exact width is skipped by the caller rather than admitted as a refusal.
fn wrong_lengths(exact: usize) -> [usize; 3] {
    [
        exact.saturating_sub(1),
        exact.saturating_add(1),
        exact.saturating_add(2),
    ]
}

/// Replaces the character at `position`, or reports that the position is past
/// the end so the caller can skip the case rather than write out of bounds.
fn corrupt_at(text: &mut [u8], position: usize, alien: u8) -> bool {
    match text.get_mut(position) {
        Some(slot) => {
            *slot = alien;
            true
        }
        None => false,
    }
}

/// Runs the seeded round-trip, refusal and boundary sweep and returns its trace.
fn hex_trace(seed: u64) -> u64 {
    let mut state = seed;
    let mut trace = initial_trace();

    for _ in 0..96 {
        let length = BOUNDARY_LENGTHS[below(&mut state, BOUNDARY_LENGTHS.len())];
        let payload = next_bytes(&mut state, length);
        let encoded = encode(&payload);

        assert_eq!(
            encoded.len(),
            encoded_len(length),
            "seed {seed}: a payload of {length} bytes renders as two characters per byte"
        );
        assert_eq!(
            encoded,
            reference_encode(&payload),
            "seed {seed}: the shipped rendering must agree with the reference"
        );

        let decoded = decode(&encoded);
        assert_eq!(
            decoded.as_deref(),
            Ok(payload.as_slice()),
            "seed {seed}: a payload of {length} bytes must decode back to itself"
        );
        fold_bytes(&mut trace, &payload);
        fold_bytes(&mut trace, encoded.as_bytes());
        fold_usize(&mut trace, payload.len());

        let mut destination = vec![0xa5; length];
        let written = decode_into(&encoded, &mut destination);
        assert_eq!(
            written,
            Ok(()),
            "seed {seed}: the exact-width destination must admit the rendered payload"
        );
        assert_eq!(
            destination, payload,
            "seed {seed}: an accepted decode_into writes every payload byte"
        );
    }
    trace
}

#[test]
/// Every seeded payload must survive `encode` then `decode` unchanged, and the
/// rendered text must be what the reference transcoder renders.
fn a_seeded_payload_round_trips_through_encode_and_decode() -> Result<(), DecodeError> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..64 {
            let length = below(&mut state, 48);
            let payload = next_bytes(&mut state, length);
            let encoded = encode(&payload);
            assert_eq!(
                encoded,
                reference_encode(&payload),
                "seed {seed}: the rendering must be the reference rendering"
            );
            let reference = reference_decode(encoded.as_bytes());
            let Reference::Decoded(expected) = reference else {
                return Err(DecodeError::OddLength {
                    len: encoded.len(),
                    at: encoded.len(),
                });
            };
            assert_eq!(
                decode(&encoded)?,
                expected,
                "seed {seed}: decode must agree with the reference decode"
            );
        }
    }
    Ok(())
}

#[test]
/// `decode_into` is exact about the destination width: every length beside the
/// required one is refused with the required and the actual count named, and a
/// buffer of the right width is admitted.
fn decode_into_requires_the_exact_destination_length() -> Result<(), DecodeError> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..48 {
            let length = below(&mut state, 40);
            let payload = next_bytes(&mut state, length);
            let encoded = encode(&payload);

            for wrong in wrong_lengths(length) {
                if wrong == length {
                    continue;
                }
                let mut destination = vec![next_byte(&mut state); wrong];
                let untouched = destination.clone();
                assert_eq!(
                    decode_into(&encoded, &mut destination),
                    Err(DecodeError::OutputLength {
                        expected: length,
                        actual: wrong
                    }),
                    "seed {seed}: a {wrong}-byte destination cannot hold {length} decoded bytes"
                );
                assert_eq!(
                    destination, untouched,
                    "seed {seed}: the refused width must leave the destination untouched"
                );
            }

            let mut exact = vec![0x00; length];
            decode_into(&encoded, &mut exact)?;
            assert_eq!(
                exact, payload,
                "seed {seed}: the exact-width destination must receive the payload"
            );
        }
    }
    Ok(())
}

#[test]
/// The INV-HEX-1 atom: whatever refuses, the caller's buffer is returned
/// unchanged. Every position of every seeded payload is corrupted in turn, at
/// an alien character, and the sentinel destination is compared afterwards.
fn a_refused_decode_into_never_writes_a_prefix_of_the_destination() {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..24 {
            let length = below(&mut state, 16);
            let payload = next_bytes(&mut state, length);
            let sentinel = next_byte(&mut state);

            for position in 0..encoded_len(length) {
                let mut corrupted = encode(&payload).into_bytes();
                if !corrupt_at(&mut corrupted, position, ALIEN_DIGIT) {
                    continue;
                }
                let mut destination = vec![sentinel; length];
                assert!(
                    decode_into(&corrupted, &mut destination).is_err(),
                    "seed {seed}: an alien digit at position {position} must be refused"
                );
                assert!(
                    destination.iter().all(|byte| *byte == sentinel),
                    "seed {seed}: position {position} was refused yet wrote into the destination"
                );
            }
        }
    }
}

#[test]
/// A non-digit is named at its own offset and reported verbatim, and it is the
/// *first* one that is named rather than any later one.
fn a_non_digit_is_reported_at_its_first_exact_offset() -> Result<(), DecodeError> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..32 {
            let length = below(&mut state, 12);
            let payload = next_bytes(&mut state, length);
            let alien = next_byte(&mut state).max(ALIEN_DIGIT);
            let position = below(&mut state, encoded_len(length));

            let mut corrupted = encode(&payload).into_bytes();
            if !corrupt_at(&mut corrupted, position, alien) {
                continue;
            }
            match reference_decode(&corrupted) {
                Reference::Refused(Refusal::NotDigit { at, byte }) => assert_eq!(
                    decode(&corrupted),
                    Err(DecodeError::NotHexDigit { at, byte }),
                    "seed {seed}: position {position} must be named at offset {at}"
                ),
                Reference::Decoded(expected) => assert_eq!(
                    decode(&corrupted),
                    Ok(expected),
                    "seed {seed}: position {position} did not make the input malformed"
                ),
                Reference::Refused(Refusal::Odd { len }) => {
                    return Err(DecodeError::OddLength { len, at: len });
                }
            }
        }
    }
    Ok(())
}

#[test]
/// An odd-length input is refused on its length, and the length check runs
/// before the destination width is consulted, so an odd input with a wrong
/// width still reports the odd length.
fn an_odd_length_is_refused_before_any_destination_width_check() -> Result<(), DecodeError> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..24 {
            let odd = below(&mut state, 24) | 1;
            let text = next_text(&mut state, odd);
            assert_eq!(
                decode(text.as_bytes()),
                Err(DecodeError::OddLength { len: odd, at: odd }),
                "seed {seed}: {odd} characters is not a whole number of pairs"
            );

            let mut narrow = [0x5a];
            assert_eq!(
                decode_into(text.as_bytes(), &mut narrow),
                Err(DecodeError::OddLength { len: odd, at: odd }),
                "seed {seed}: the length refusal precedes any width refusal"
            );
            assert_eq!(
                narrow,
                [0x5a],
                "seed {seed}: the length refusal leaves the destination untouched"
            );

            let mut wide = [0x5a; 8];
            assert_eq!(
                decode_into(text.as_bytes(), &mut wide),
                Err(DecodeError::OddLength { len: odd, at: odd }),
                "seed {seed}: a wide destination does not move the length check earlier"
            );
        }
    }
    Ok(())
}

#[test]
/// The endpoints are exact: the empty payload renders as nothing and decodes
/// back to nothing, and a one-byte payload at each extreme renders as the two
/// characters the reference renders.
fn the_empty_and_single_byte_payloads_are_exact_endpoints() -> Result<(), DecodeError> {
    assert_eq!(
        encode([]),
        String::new(),
        "the empty payload renders as nothing"
    );
    assert_eq!(
        decode("")?,
        Vec::<u8>::new(),
        "the empty text decodes to nothing"
    );
    assert_eq!(
        decode_into("", &mut []),
        Ok(()),
        "the empty text needs an empty destination"
    );

    for extreme in [0x00_u8, 0x0f, 0x10, 0x7f, 0x80, 0xff] {
        let rendered = encode([extreme]);
        assert_eq!(
            rendered,
            reference_encode(&[extreme]),
            "byte {extreme:#04x} renders as the reference spelling"
        );
        assert_eq!(
            decode(&rendered)?,
            vec![extreme],
            "byte {extreme:#04x} decodes back to itself"
        );
        let mut destination = [0_u8; 1];
        decode_into(&rendered, &mut destination)?;
        assert_eq!(
            destination,
            [extreme],
            "byte {extreme:#04x} fills a one-byte destination exactly"
        );
    }
    Ok(())
}

#[test]
/// Two spellings of one payload are one payload: the uppercase and lowercase
/// renderings decode to the same bytes, and `decode_into` writes those bytes
/// into either sentinel destination.
fn uppercase_and_lowercase_spellings_decode_to_the_same_bytes() -> Result<(), DecodeError> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..32 {
            let span = below(&mut state, 24).saturating_add(1);
            // The case needs a payload that renders two spellings, which means a
            // hex letter in it, and that is drawn rather than assumed: a byte's
            // low nibble is a letter in six of its sixteen values, so a payload
            // of a byte or more almost always has one. The redraw is bounded, and
            // the assertion below says so if the bound is what ended it.
            let mut payload = next_bytes(&mut state, span);
            for _ in 0..LETTER_DRAW_LIMIT {
                if payload.iter().any(|byte| byte & 0x0f >= 10) {
                    break;
                }
                payload = next_bytes(&mut state, span);
            }
            let lower = encode(&payload);
            let upper = lower.to_ascii_uppercase();
            assert_ne!(
                lower, upper,
                "seed {seed}: {LETTER_DRAW_LIMIT} payloads of {span} bytes carried no hex letter"
            );

            let from_lower = decode(&lower)?;
            let from_upper = decode(&upper)?;
            assert_eq!(
                from_lower, from_upper,
                "seed {seed}: case must not change the decoded bytes"
            );
            assert_eq!(
                from_lower, payload,
                "seed {seed}: either spelling decodes to the drawn payload"
            );

            let mut destination = vec![0x3c; payload.len()];
            decode_into(&upper, &mut destination)?;
            assert_eq!(
                destination, payload,
                "seed {seed}: the uppercase spelling fills the destination identically"
            );
        }
    }
    Ok(())
}

#[test]
/// A refusal arm and its offsets are folded into the trace, so a family that
/// converged on one arm for every corruption would still diverge here.
fn refusals_report_their_arm_and_both_of_their_offsets() -> Result<(), DecodeError> {
    let mut trace = initial_trace();
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..16 {
            let length = below(&mut state, 10);
            let payload = next_bytes(&mut state, length);
            let mut corrupted = encode(&payload).into_bytes();
            let position = below(&mut state, encoded_len(length));
            if !corrupt_at(&mut corrupted, position, b'z') {
                continue;
            }
            match decode(&corrupted) {
                Ok(bytes) => fold_usize(&mut trace, bytes.len()),
                Err(DecodeError::NotHexDigit { at, byte }) => {
                    fold_refusal(&mut trace, 1, at, usize::from(byte));
                }
                Err(DecodeError::OddLength { len, at }) => fold_refusal(&mut trace, 2, len, at),
                Err(DecodeError::OutputLength { expected, actual }) => {
                    fold_refusal(&mut trace, 3, expected, actual);
                }
                Err(other) => fold_refusal(&mut trace, 4, 0, format!("{other}").len()),
            }
        }
    }
    assert_ne!(
        trace,
        initial_trace(),
        "the refusal family must fold at least one observation"
    );
    Ok(())
}

#[test]
/// A payload of constant bytes is exact at every boundary length, including one
/// that is entirely `0x00` and one that is entirely `0xff`, so a zero-filled
/// payload and a set-bit payload are not the same case.
fn constant_payloads_are_exact_at_every_boundary_length() -> Result<(), DecodeError> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for length in BOUNDARY_LENGTHS {
            let fill = next_byte(&mut state);
            let payload = repeated(fill, length);
            let encoded = encode(&payload);
            assert_eq!(
                encoded,
                reference_encode(&payload),
                "seed {seed}: a constant {length}-byte payload of {fill:#04x} renders as the reference"
            );
            assert_eq!(
                decode(&encoded)?,
                payload,
                "seed {seed}: a constant {length}-byte payload of {fill:#04x} decodes back"
            );
        }
    }
    Ok(())
}

#[test]
/// A truncated prefix of a rendered payload is never decoded as the payload:
/// every strict prefix is either an odd-length refusal or a shorter value.
fn every_truncated_prefix_is_refused_or_is_a_shorter_value() -> Result<(), DecodeError> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..16 {
            let payload = next_bytes(&mut state, 8);
            let encoded = encode(&payload);
            for cut in 0..encoded.len() {
                // `encode` emits ASCII, so every byte offset in the rendering is
                // also a character boundary and the prefix at `cut` exists for
                // every `cut` in this loop. The same file asserts the rendering
                // against the reference alphabet above, so this is a property
                // the family has already established rather than a hope.
                let prefix = &encoded[..cut];
                if !cut.is_multiple_of(2) {
                    assert_eq!(
                        decode(prefix),
                        Err(DecodeError::OddLength { len: cut, at: cut }),
                        "seed {seed}: a {cut}-character prefix is an odd run"
                    );
                } else {
                    assert_eq!(
                        decode(prefix)?.len(),
                        cut.saturating_div(2),
                        "seed {seed}: a {cut}-character prefix cannot decode to more bytes"
                    );
                }
            }
        }
    }
    Ok(())
}

#[test]
/// The wide boundary is a real payload and not a short one wearing its name: it
/// renders as twice its length and decodes back into the drawn bytes.
fn the_wide_payload_boundary_is_exercised_at_its_declared_length() -> Result<(), DecodeError> {
    let mut state = SWEEP_SEEDS[0];
    let payload = next_bytes(&mut state, WIDE_PAYLOAD_BYTES);
    let encoded = encode(&payload);
    assert_eq!(
        encoded.len(),
        encoded_len(WIDE_PAYLOAD_BYTES),
        "the wide boundary renders two characters per byte"
    );
    assert_eq!(
        decode(&encoded)?,
        payload,
        "the wide boundary payload decodes back to itself"
    );
    let mut destination = vec![0x11; WIDE_PAYLOAD_BYTES];
    decode_into(&encoded, &mut destination)?;
    assert_eq!(
        destination, payload,
        "the wide boundary fills its destination exactly"
    );

    let mut truncated = encoded.into_bytes();
    truncated.pop();
    let mut untouched = vec![0x11; WIDE_PAYLOAD_BYTES];
    assert_eq!(
        decode_into(&truncated, &mut untouched),
        Err(DecodeError::OddLength {
            len: WIDE_PAYLOAD_BYTES.saturating_mul(2).saturating_sub(1),
            at: WIDE_PAYLOAD_BYTES.saturating_mul(2).saturating_sub(1)
        }),
        "the wide boundary truncated by one character is odd"
    );
    assert!(
        untouched.iter().all(|byte| *byte == 0x11),
        "the wide boundary truncation left the destination untouched"
    );
    Ok(())
}

#[test]
/// The replay oracle: one seed, one trace.
fn the_same_seed_replays_to_the_same_hex_trace() {
    for seed in SWEEP_SEEDS {
        assert_same_seed_replays(hex_trace, seed);
    }
}

#[test]
/// The divergence oracle: a sweep that ignored its seed would pass the replay.
fn distinct_hex_seeds_diverge_in_their_trace() {
    assert_distinct_seeds_diverge(hex_trace, SWEEP_SEEDS[0], SWEEP_SEEDS[1]);
}

#[test]
/// A second seed's stream produces a different payload at the same requested
/// length, so the round-trip family is not one payload checked repeatedly.
fn a_seed_draws_a_different_payload_at_the_same_length() {
    let mut first = SWEEP_SEEDS[0];
    let mut second = SWEEP_SEEDS[1];
    let length = 32;
    let left = next_bytes(&mut first, length);
    let right = next_bytes(&mut second, length);
    assert_eq!(
        left.len(),
        length,
        "both seeds were asked for the same length"
    );
    assert_ne!(
        left, right,
        "seed {} and seed {} drew the same payload",
        SWEEP_SEEDS[0], SWEEP_SEEDS[1]
    );
    let mut trace = initial_trace();
    fold_bytes(&mut trace, &left);
    fold_usize(&mut trace, right.len());
    assert_ne!(
        trace,
        initial_trace(),
        "the two payload streams must fold into distinct traces"
    );
}
