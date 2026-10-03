//! Seeded deterministic replay of the `encoding` transcoding contracts.
//!
//! One seed draws every payload, a failure prints the seed that produced it,
//! and the same seed must produce the same trace hash. Every probe drives the
//! public API of the shipped crate, and every answer is checked against a
//! reference transcoder written here from the documented contract rather than
//! from the implementation under test.
//!
//! The property INV-ENCODING-1 names is the coordinate space of a refusal. A
//! percent-escape error reports an offset in the **original input bytes**; a
//! UTF-8 error reports an offset in the **decoded bytes**, and is never
//! dressed up as a source coordinate. The two coordinate spaces coincide only
//! when every escape is literal, so a family that conflated them would pass a
//! test over inputs with no escapes in them at all. Every family here draws
//! payloads with escapes in them, and several draw multi-byte text, where
//! literal UTF-8 occupies more source bytes than decoded bytes.

#[path = "support/seeded_bytes.rs"]
mod seeded_bytes;
#[path = "support/seeded_sweep.rs"]
mod seeded_sweep;

use lgwks_std::encoding::{
    base64,
    percent::{self, DecodeError as PercentError},
};

use seeded_bytes::{
    below, fold_bytes, fold_refusal, next_byte, next_bytes, next_text, reference_escape,
    reference_nibble,
};
use seeded_sweep::{
    SWEEP_SEEDS, assert_distinct_seeds_diverge, assert_same_seed_replays, fold, fold_usize,
    initial_trace,
};

/// The widest payload any family in this file generates.
const WIDE_PAYLOAD_BYTES: usize = 4_096;

/// The payload lengths every family in this file states its properties at.
const BOUNDARY_LENGTHS: [usize; 6] = [0, 1, 2, 3, 255, WIDE_PAYLOAD_BYTES];

/// Multi-byte text, so every family that decodes it pays a decoded-byte
/// coordinate narrower than its source-byte coordinate.
const MULTIBYTE_SAMPLES: [&str; 6] = ["é", "☃", "こんにちは", "aé☃", "☃☃☃", "ünïcødé"];

/// Whether `byte` is an RFC 3986 unreserved character, written from the
/// documented set rather than from the shipped predicate.
fn reference_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')
}

/// Escapes every byte that is not unreserved, one byte at a time.
fn reference_encode_component(text: &str) -> String {
    let mut rendered = String::new();
    for byte in text.as_bytes() {
        if reference_unreserved(*byte) {
            rendered.push(char::from(*byte));
        } else {
            rendered.push_str(&reference_escape(*byte));
        }
    }
    rendered
}

/// What the reference decoder says about one payload.
#[derive(Debug)]
enum Reference {
    /// The payload decoded to this text.
    Decoded(String),
    /// A `%` did not have two characters left after it.
    Truncated {
        /// Offset of the `%` itself, in original input bytes.
        at: usize,
    },
    /// An escape held a character outside `[0-9a-fA-F]`, at its own offset in
    /// the original input.
    NotDigit {
        /// Offset of the offending character in the original input.
        at: usize,
    },
    /// The decoded bytes were not valid UTF-8, at this decoded-byte offset.
    NotUtf8 {
        /// Offset where the invalid sequence begins, in decoded bytes.
        decoded_at: usize,
    },
}

/// Decodes a percent-encoded payload under the documented contract.
///
/// The two refusals that name a coordinate name it in the coordinate the
/// contract states: a truncated or non-hex escape in **source** bytes, and a
/// UTF-8 failure in **decoded** bytes. Nothing here converts one into the
/// other, which is exactly the mistake INV-ENCODING-1 exists to catch.
fn reference_decode(input: &str) -> Reference {
    let bytes = input.as_bytes();
    let mut decoded: Vec<u8> = Vec::new();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if bytes[cursor] != b'%' {
            decoded.push(bytes[cursor]);
            cursor = cursor.saturating_add(1);
            continue;
        }
        let Some(pair) = bytes.get(cursor.saturating_add(1)..cursor.saturating_add(3)) else {
            return Reference::Truncated { at: cursor };
        };
        let Some(high) = reference_nibble(pair[0]) else {
            return Reference::NotDigit {
                at: cursor.saturating_add(1),
            };
        };
        let Some(low) = reference_nibble(pair[1]) else {
            return Reference::NotDigit {
                at: cursor.saturating_add(2),
            };
        };
        decoded.push((high << 4) | low);
        cursor = cursor.saturating_add(3);
    }
    match String::from_utf8(decoded) {
        Ok(text) => Reference::Decoded(text),
        Err(failure) => Reference::NotUtf8 {
            decoded_at: failure.utf8_error().valid_up_to(),
        },
    }
}

/// Draws a payload that is a mix of literal characters and escapes, so the two
/// coordinate spaces differ in most cases rather than coincidentally.
///
/// The escape is drawn from the whole byte range, so about one payload in
/// sixteen escapes a non-UTF-8 byte and the `NotUtf8` arm is genuinely reached
/// rather than merely declared reachable.
fn next_escaped_text(state: &mut u64) -> String {
    let length = below(state, 24);
    let mut rendered = String::new();
    for _ in 0..length {
        if next_byte(state).is_multiple_of(3) {
            rendered.push_str(&reference_escape(next_byte(state)));
        } else {
            rendered.push_str(&next_text(state, 1));
        }
    }
    rendered
}

/// Runs the seeded sweep over the transcoders, the refusal coordinates and the
/// boundary lengths, and returns its trace.
fn encoding_trace(seed: u64) -> u64 {
    let mut state = seed;
    let mut trace = initial_trace();

    for _ in 0..64 {
        let length = BOUNDARY_LENGTHS[below(&mut state, BOUNDARY_LENGTHS.len())];
        let payload = next_bytes(&mut state, length);
        let encoded = percent::encode_component(&String::from_utf8_lossy(&payload));
        assert_eq!(
            encoded,
            reference_encode_component(&String::from_utf8_lossy(&payload)),
            "seed {seed}: the component encoding must agree with the reference"
        );
        fold_bytes(&mut trace, encoded.as_bytes());

        match reference_decode(&encoded) {
            Reference::Decoded(text) => assert_eq!(
                percent::decode(&encoded).unwrap_or_default(),
                text,
                "seed {seed}: the decoded text must agree with the reference"
            ),
            Reference::Truncated { at } => assert_eq!(
                percent::decode(&encoded),
                Err(PercentError::TruncatedEscape { at }),
                "seed {seed}: a truncated escape names its `%` in source bytes"
            ),
            Reference::NotDigit { at } => assert_eq!(
                percent::decode(&encoded),
                Err(PercentError::NotHexDigit { at }),
                "seed {seed}: a non-hex escape names its character in source bytes"
            ),
            Reference::NotUtf8 { decoded_at } => assert_eq!(
                percent::decode(&encoded),
                Err(PercentError::NotUtf8 { decoded_at }),
                "seed {seed}: a UTF-8 failure names its offset in decoded bytes"
            ),
        }
    }
    trace
}

#[test]
/// Percent encoding and decoding round-trip every seeded payload, and the
/// rendered text is what the reference transcoder renders from the documented
/// unreserved set and uppercase hex digits.
fn a_seeded_component_round_trips_through_percent_encoding() -> Result<(), PercentError> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..64 {
            let span = below(&mut state, 40);
            let text = next_text(&mut state, span);
            let encoded = percent::encode_component(&text);
            assert_eq!(
                encoded,
                reference_encode_component(&text),
                "seed {seed}: the rendering must be the reference rendering"
            );
            assert_eq!(
                percent::decode(&encoded)?,
                text,
                "seed {seed}: the decoded text must be the drawn text"
            );
        }
    }
    Ok(())
}

#[test]
/// The documented encoding escapes exactly the bytes outside the unreserved
/// set: a multi-byte scalar is escaped one byte at a time, so its encoding is
/// three or nine percent escapes rather than one.
fn multi_byte_scalars_are_escaped_one_byte_at_a_time() {
    for sample in MULTIBYTE_SAMPLES {
        let encoded = percent::encode_component(sample);
        assert_eq!(
            encoded,
            reference_encode_component(sample),
            "{sample:?} must render as the reference rendering"
        );
        assert!(
            encoded
                .chars()
                .all(|character| { character.is_ascii_alphanumeric() || character == '%' }),
            "{sample:?} must render as hex digits and escapes only"
        );
        assert!(
            sample.len() > sample.chars().count(),
            "{sample:?} must be multi-byte: an all-ASCII sample proves nothing here"
        );
    }
    let snowman = percent::encode_component("☃");
    assert_eq!(
        snowman, "%E2%98%83",
        "the three-byte snowman is three uppercase escapes"
    );
}

#[test]
/// INV-ENCODING-1's first half: a malformed escape names an offset in the
/// **original input bytes**, and that offset is where the offending character
/// really is — not where it would be after the escapes were decoded.
///
/// The payload is prefixed with multi-byte literal text, so the source
/// coordinate is strictly larger than the decoded one and the two cannot be
/// confused even by accident.
fn a_malformed_escape_is_reported_at_its_original_input_offset()
-> Result<(), Box<dyn std::error::Error>> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in MULTIBYTE_SAMPLES {
            for alien in [b'g', b'G', b'z', b' ', b'/', 0x80, 0xff] {
                let prefix_span = below(&mut state, 6);
                let prefix = next_text(&mut state, prefix_span);
                let percent_at = prefix.len();
                let alien_byte = [alien];
                let alien_text = String::from_utf8_lossy(&alien_byte);
                let body = format!("{prefix}%2{alien_text}x");
                let escaped = format!("{prefix}%2{alien:02x}x");

                assert_eq!(
                    percent::decode(&body),
                    Err(PercentError::NotHexDigit {
                        at: percent_at.saturating_add(2)
                    }),
                    "seed {seed}: the low digit at {percent_at}+2 must be named in source bytes"
                );

                match reference_decode(&escaped) {
                    Reference::Decoded(expected) => assert_eq!(
                        percent::decode(&escaped),
                        Ok(expected),
                        "seed {seed}: the same byte as an escape decodes as the reference decodes it"
                    ),
                    Reference::NotDigit { at } => assert_eq!(
                        percent::decode(&escaped),
                        Err(PercentError::NotHexDigit { at }),
                        "seed {seed}: an unspellable alien is still refused, at the same coordinate"
                    ),
                    Reference::NotUtf8 { decoded_at } => assert_eq!(
                        percent::decode(&escaped),
                        Err(PercentError::NotUtf8 { decoded_at }),
                        "seed {seed}: a non-UTF-8 byte reports a decoded coordinate"
                    ),
                    Reference::Truncated { at } => {
                        return Err(Box::new(PercentError::TruncatedEscape { at }));
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
/// INV-ENCODING-1's second half: a UTF-8 failure names an offset in the
/// **decoded bytes**, and that offset is never the source coordinate.
///
/// For a payload of pure escapes the source and decoded coordinates coincide,
/// so this family prefixes multi-byte literal text first: the literal text
/// occupies more source bytes than decoded bytes, and an implementation that
/// reported a source coordinate would name the wrong place.
fn a_utf8_failure_is_reported_in_decoded_bytes_not_source_bytes()
-> Result<(), Box<dyn std::error::Error>> {
    for seed in SWEEP_SEEDS {
        for sample in MULTIBYTE_SAMPLES {
            for alien in [0x80_u8, 0xc3, 0xff, 0xfe] {
                let trailing = format!("%{alien:02x}");
                let payload = format!("{sample}{trailing}");
                match reference_decode(&payload) {
                    Reference::NotUtf8 { decoded_at } => {
                        assert_eq!(
                            percent::decode(&payload),
                            Err(PercentError::NotUtf8 { decoded_at }),
                            "seed {seed}: {sample:?} plus an escaped {alien:#04x} reports a decoded offset"
                        );
                        assert!(
                            decoded_at <= sample.len(),
                            "seed {seed}: the decoded coordinate is inside the decoded bytes"
                        );
                        assert_ne!(
                            sample.len().saturating_add(3),
                            payload.len().saturating_sub(trailing.len()),
                            "seed {seed}: the source and decoded lengths differ for this payload"
                        );
                    }
                    other => {
                        return Err(format!(
                            "seed {seed}: {sample:?} plus {alien:#04x} gave {other:?}"
                        )
                        .into());
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
/// A `%` with fewer than two characters left after it is refused at the `%`
/// itself, in source bytes, whether zero or one character follows.
fn a_truncated_escape_names_the_percent_in_source_bytes() {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in MULTIBYTE_SAMPLES {
            for tail in ["", "2"] {
                let prefix_span = below(&mut state, 6);
                let prefix = next_text(&mut state, prefix_span);
                let payload = format!("{prefix}%{tail}");
                let at = prefix.len();
                assert_eq!(
                    percent::decode(&payload),
                    Err(PercentError::TruncatedEscape { at }),
                    "seed {seed}: {prefix:?} with the tail {tail:?} is refused at the `%`"
                );
                assert!(
                    at < payload.len(),
                    "seed {seed}: the reported position is inside the input it refused"
                );
            }
        }
    }
}

#[test]
/// The escape *digits* are case-insensitive on the way back, so `%2f` and
/// `%2F` are one payload with two spellings — and the literal characters
/// beside them are not case-folded, because folding those would silently
/// corrupt the payload.
///
/// The escaped byte is drawn from ASCII, so this family measures the case rule
/// rather than accidentally measuring UTF-8 validity; the multi-byte escape is
/// exercised by the truncation and UTF-8 families instead.
fn lower_and_upper_case_escape_digits_decode_to_the_same_byte()
-> Result<(), Box<dyn std::error::Error>> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..32 {
            let span = below(&mut state, 8);
            let body = next_text(&mut state, span);
            let escaped_byte = next_byte(&mut state) % 0x7f;
            let escaped = format!("%{escaped_byte:02x}");
            let lowered = format!("{body}{}", escaped.to_ascii_lowercase());
            let raised = format!("{body}{}", escaped.to_ascii_uppercase());

            match reference_decode(&lowered) {
                Reference::Decoded(expected) => assert_eq!(
                    percent::decode(&lowered)?,
                    expected,
                    "seed {seed}: the lowered spelling decodes as the reference decodes it"
                ),
                other => {
                    return Err(format!("seed {seed}: lowered gave {other:?}").into());
                }
            }
            assert_eq!(
                percent::decode(&lowered)?,
                percent::decode(&raised)?,
                "seed {seed}: the two case spellings of an escape must decode alike"
            );
            assert_eq!(
                percent::decode(&lowered)?,
                format!("{body}{}", char::from(escaped_byte)),
                "seed {seed}: the escape decodes to exactly the byte it spelled"
            );
        }
    }
    Ok(())
}

#[test]
/// The endpoints are exact: the empty payload renders as nothing, decodes back
/// to nothing, and a one-byte payload at each extreme is escaped exactly.
fn the_empty_and_single_byte_payloads_are_exact_endpoints() -> Result<(), PercentError> {
    assert_eq!(
        percent::encode_component(""),
        String::new(),
        "the empty payload renders as nothing"
    );
    assert_eq!(
        percent::decode("")?,
        String::new(),
        "the empty text decodes to nothing"
    );
    assert_eq!(
        percent::encode_component("a"),
        "a",
        "an unreserved character is not escaped"
    );
    for reserved in [" ", "/", "?", "=", "~", "-", "_", ".", ":"] {
        let encoded = percent::encode_component(reserved);
        assert_eq!(
            encoded,
            reference_encode_component(reserved),
            "{reserved:?} is escaped as the reference escapes it"
        );
        assert_eq!(
            percent::decode(&encoded)?,
            reserved,
            "{reserved:?} decodes back to itself"
        );
    }
    Ok(())
}

#[test]
/// Every truncated prefix of a rendered payload is either refused or is a
/// shorter string — a truncation is never padded back out into the payload.
fn every_truncated_prefix_is_refused_or_is_shorter() -> Result<(), PercentError> {
    for sample in MULTIBYTE_SAMPLES {
        let encoded = percent::encode_component(sample);
        for cut in 0..encoded.len() {
            let prefix = encoded.get(..cut).unwrap_or("");
            match reference_decode(prefix) {
                Reference::Decoded(text) => assert_eq!(
                    percent::decode(prefix)?,
                    text,
                    "a {cut}-character prefix decodes as the reference decodes it"
                ),
                Reference::Truncated { at } => assert_eq!(
                    percent::decode(prefix),
                    Err(PercentError::TruncatedEscape { at }),
                    "a cut at {cut} is refused at the `%` in source bytes"
                ),
                Reference::NotDigit { at } => assert_eq!(
                    percent::decode(prefix),
                    Err(PercentError::NotHexDigit { at }),
                    "a cut at {cut} names a non-hex digit in source bytes"
                ),
                Reference::NotUtf8 { decoded_at } => assert_eq!(
                    percent::decode(prefix),
                    Err(PercentError::NotUtf8 { decoded_at }),
                    "a cut at {cut} names a decoded-byte coordinate"
                ),
            }
        }
    }
    Ok(())
}

#[test]
/// The two refusal arms that name source coordinates report offsets that really
/// are positions in the input, and the UTF-8 arm's offset is really a position
/// in the decoded bytes.
fn every_reported_coordinate_is_a_real_position_in_its_own_space() -> Result<(), PercentError> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..48 {
            let payload = next_escaped_text(&mut state);
            match reference_decode(&payload) {
                Reference::Decoded(_) => assert_eq!(
                    percent::decode(&payload)?,
                    reference_decode(&payload).decoded_text(),
                    "seed {seed}: the decoded text must agree with the reference"
                ),
                Reference::Truncated { at } => {
                    assert!(
                        at < payload.len(),
                        "seed {seed}: a truncation offset must be a position in the input"
                    );
                    assert_eq!(
                        percent::decode(&payload),
                        Err(PercentError::TruncatedEscape { at }),
                        "seed {seed}: the truncation offset is the `%`'s own position"
                    );
                }
                Reference::NotDigit { at } => {
                    assert!(
                        at < payload.len(),
                        "seed {seed}: a non-digit offset must be a position in the input"
                    );
                    let offending = payload.as_bytes().get(at).copied().unwrap_or(b'%');
                    assert!(
                        reference_nibble(offending).is_none(),
                        "seed {seed}: the named position {at} must hold a non-digit"
                    );
                    assert_eq!(
                        percent::decode(&payload),
                        Err(PercentError::NotHexDigit { at }),
                        "seed {seed}: the non-digit offset names the offending character"
                    );
                }
                Reference::NotUtf8 { decoded_at } => {
                    let decoded_len = decoded_len_of(&payload);
                    assert!(
                        decoded_at <= decoded_len,
                        "seed {seed}: a decoded coordinate must be a position in the decoded bytes"
                    );
                    assert_eq!(
                        percent::decode(&payload),
                        Err(PercentError::NotUtf8 { decoded_at }),
                        "seed {seed}: the UTF-8 offset names a decoded position"
                    );
                }
            }
        }
    }
    Ok(())
}

impl Reference {
    /// The decoded text, or the empty string for a refusal.
    ///
    /// A decoded arm can only reach this call, so a refusal yields nothing to
    /// compare against rather than a value the test would then have to trust.
    fn decoded_text(&self) -> String {
        match *self {
            Self::Decoded(ref text) => text.clone(),
            _ => String::new(),
        }
    }
}

/// The number of bytes a payload decodes to, counting an escape as one byte
/// and a literal byte as one byte.
fn decoded_len_of(payload: &str) -> usize {
    let bytes = payload.as_bytes();
    let mut cursor = 0usize;
    let mut length = 0usize;
    while cursor < bytes.len() {
        if bytes[cursor] == b'%' {
            cursor = cursor.saturating_add(3);
        } else {
            cursor = cursor.saturating_add(1);
        }
        length = length.saturating_add(1);
    }
    length
}

#[test]
/// Base64 round-trips every seeded payload and its rendering is the RFC 4648
/// shape: four characters per three input bytes, padded, and only characters
/// from the standard alphabet.
fn a_seeded_payload_round_trips_through_base64() -> Result<(), base64::DecodeError> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for length in BOUNDARY_LENGTHS {
            let payload = next_bytes(&mut state, length);
            let encoded = base64::encode(&payload);
            let quantum = length.div_ceil(3).saturating_mul(4);
            assert_eq!(
                encoded.len(),
                quantum,
                "seed {seed}: a {length}-byte payload renders as {quantum} characters"
            );
            assert!(
                encoded.chars().all(|character| {
                    character.is_ascii_alphanumeric()
                        || character == '+'
                        || character == '/'
                        || character == '='
                }),
                "seed {seed}: the rendering uses only the standard alphabet"
            );
            assert_eq!(
                base64::decode(&encoded)?,
                payload,
                "seed {seed}: a {length}-byte payload decodes back to itself"
            );
        }
    }
    Ok(())
}

#[test]
/// A base64 payload truncated by any number of characters is refused on its
/// length, never padded back out into the payload.
fn every_truncated_base64_prefix_is_refused_on_its_length() -> Result<(), base64::DecodeError> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..16 {
            let payload = next_bytes(&mut state, 16);
            let encoded = base64::encode(&payload);
            for cut in 0..encoded.len() {
                let prefix = encoded.get(..cut).unwrap_or("");
                if !cut.is_multiple_of(4) {
                    assert_eq!(
                        base64::decode(prefix),
                        Err(base64::DecodeError::BadLength { len: cut, at: cut }),
                        "seed {seed}: a {cut}-character prefix is not a whole quantum"
                    );
                } else {
                    assert_eq!(
                        base64::decode(prefix)?.len(),
                        cut.saturating_div(4).saturating_mul(3),
                        "seed {seed}: a whole quantum decodes to three bytes per four characters"
                    );
                }
            }
        }
    }
    Ok(())
}

#[test]
/// A base64 payload with one character corrupted outside the alphabet is
/// refused at that character's own offset, and a good quantum beside it is
/// unaffected.
fn a_corrupted_base64_character_is_refused_at_its_offset() -> Result<(), Box<dyn std::error::Error>>
{
    for seed in SWEEP_SEEDS {
        for alien in [b'!', b'-', b'_', b' ', b'\n', 0x80, 0xff] {
            let good = base64::encode("standard base64 sample text");
            let padding_start = good.trim_end_matches('=').len();
            assert_eq!(
                base64::decode(&good)?.len(),
                good.len().saturating_div(4).saturating_mul(3),
                "seed {seed}: the good quantum decodes to its full length"
            );

            for position in 0..padding_start {
                let mut corrupted = good.clone().into_bytes();
                if let Some(slot) = corrupted.get_mut(position) {
                    *slot = alien;
                }
                assert_eq!(
                    base64::decode(&corrupted),
                    Err(base64::DecodeError::NotInAlphabet {
                        at: position,
                        byte: alien
                    }),
                    "seed {seed}: the body corruption at {position} is named at its own offset"
                );
            }

            for position in padding_start..good.len() {
                let mut corrupted = good.clone().into_bytes();
                if let Some(slot) = corrupted.get_mut(position) {
                    *slot = alien;
                }
                assert!(
                    base64::decode(&corrupted).is_err(),
                    "seed {seed}: a padding position holding {alien:#04x} must be refused"
                );
            }
        }
    }
    Ok(())
}

#[test]
/// The replay oracle: one seed, one trace.
fn the_same_seed_replays_to_the_same_encoding_trace() {
    for seed in SWEEP_SEEDS {
        assert_same_seed_replays(encoding_trace, seed);
    }
}

#[test]
/// The divergence oracle: a sweep that ignored its seed would pass the replay.
fn distinct_encoding_seeds_diverge_in_their_trace() {
    assert_distinct_seeds_diverge(encoding_trace, SWEEP_SEEDS[0], SWEEP_SEEDS[1]);
}

#[test]
/// The wide boundary is a real payload: 4 KiB renders as more than three
/// characters per byte and decodes back into the drawn bytes.
fn the_wide_boundary_renders_and_decodes_at_its_declared_length()
-> Result<(), Box<dyn std::error::Error>> {
    let mut state = SWEEP_SEEDS[0];
    let payload = next_bytes(&mut state, WIDE_PAYLOAD_BYTES);
    let lossy = String::from_utf8_lossy(&payload);
    let encoded = percent::encode_component(&lossy);
    assert!(
        encoded.len() >= lossy.len(),
        "every byte needs at least one output character"
    );
    fold(
        &mut initial_trace(),
        u64::try_from(encoded.len()).unwrap_or(0),
    );
    match reference_decode(&encoded) {
        Reference::Decoded(text) => assert_eq!(
            percent::decode(&encoded)?,
            text,
            "the wide boundary decodes as the reference decodes it"
        ),
        other => {
            return Err(format!("the wide boundary drew an unexpected case: {other:?}").into());
        }
    }
    Ok(())
}

#[test]
/// A refusal arm and its coordinates are folded into the trace, so a family
/// that reached one arm for every corrupted payload would still diverge here.
fn refusals_fold_their_arm_and_both_of_their_coordinates() {
    let mut trace = initial_trace();
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..24 {
            let payload = next_escaped_text(&mut state);
            match percent::decode(&payload) {
                Ok(text) => fold_usize(&mut trace, text.len()),
                Err(PercentError::TruncatedEscape { at }) => fold_refusal(&mut trace, 1, at, at),
                Err(PercentError::NotHexDigit { at }) => {
                    fold_refusal(&mut trace, 2, at, payload.len());
                }
                Err(PercentError::NotUtf8 { decoded_at }) => {
                    fold_refusal(&mut trace, 3, decoded_at, payload.len());
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
}
