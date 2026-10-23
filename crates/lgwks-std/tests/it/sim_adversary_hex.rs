//! A peer controls every character of the hex a decoder is handed.
//!
//! Hex is the text form of digests, identifiers and keys, so a decoder that
//! accepts a second spelling of the same bytes lets an attacker present two
//! different strings as one identity (CWE-176, improper handling of Unicode
//! encoding; CWE-20), and one that reports the wrong place sends a caller to the
//! wrong byte. The attacks: a bad character at every position and of every byte
//! value, several bad characters at once, an odd length hiding a bad digit,
//! lookalike and multi-byte glyphs, `0x` prefixes and padding, a destination of
//! the wrong size, a destination that must stay untouched when the input is bad,
//! and inputs large enough to expose a quadratic or truncating decoder.
//!
//! The reference is written here from RFC 4648 section 8: two digits per byte,
//! `0-9a-fA-F` only (the crate also documents that uppercase is accepted), the
//! length check first, then the first bad digit by offset.

use lgwks_std::hex::{DecodeError, decode, decode_into, encode};

use crate::adversary::{
    Attacker, TestResult, as_text, decoder_campaign, diverges_between_seeds,
    every_byte_at_every_position, replays_under_every_seed, sweep,
};

/// The byte a destination is filled with before a decode, so a write is visible.
const SENTINEL: u8 = 0xa5;

/// The value of one hex digit, from the standard library's own definition.
fn digit_value(byte: u8) -> Option<u8> {
    let value = char::from(byte).to_digit(16)?;
    u8::try_from(value).ok()
}

/// The answer RFC 4648 gives for `input`, in the order the crate documents:
/// odd length first, then the first character that is not a hex digit.
fn reference_decode(input: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let digits = reference_digits(input)?;
    let (pairs, _) = digits.as_chunks::<2>();
    Ok(pairs
        .iter()
        .map(|&[high, low]| high.wrapping_shl(4) | low)
        .collect())
}

/// The value of every digit of `input`, or the first fault: an odd length, then
/// the first character that is not a hex digit.
fn reference_digits(input: &[u8]) -> Result<Vec<u8>, DecodeError> {
    if !input.len().is_multiple_of(2) {
        Err(DecodeError::OddLength {
            len: input.len(),
            at: input.len(),
        })
    } else {
        input
            .iter()
            .enumerate()
            .map(|(at, &byte)| digit_value(byte).ok_or(DecodeError::NotHexDigit { at, byte }))
            .collect()
    }
}

/// The reference spelling of `bytes`: `{:02x}` per byte.
fn reference_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A valid lowercase hex string of `below(max)` random bytes, with the bytes.
fn draw_pair(attacker: &mut Attacker, max: usize) -> (Vec<u8>, String) {
    attacker.spelled(max, reference_encode)
}

/// The text with each ASCII letter upper or lower case as `pattern` says.
fn recased(text: &str, pattern: &[bool]) -> String {
    text.chars()
        .zip(pattern)
        .map(|(glyph, &upper)| {
            if upper {
                glyph.to_ascii_uppercase()
            } else {
                glyph.to_ascii_lowercase()
            }
        })
        .collect()
}

/// Whether every byte of `bytes` is the byte `want`.
fn all_equal(bytes: &[u8], want: u8) -> bool {
    bytes.iter().all(|&byte| byte == want)
}

/// One campaign: hostile hex through `decode`, outcomes folded into a trace.
fn campaign(seed: u64) -> u64 {
    decoder_campaign(
        seed,
        |attacker| encode(attacker.bytes(24)).into_bytes(),
        |hostile| format!("{:?}", decode(hostile)),
    )
}

/// Whatever the attacker builds, `decode` gives the reference's answer: the same
/// bytes, or the same refusal at the same offset.
#[test]
fn sim_adversary_hex_hostile_input_gets_the_reference_answer() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let (_, text) = draw_pair(attacker, 20);
        let hostile = attacker.mutate_n(text.as_bytes(), 3);
        assert_eq!(
            decode(&hostile),
            reference_decode(&hostile),
            "seed {seed:#x} round {round}: {hostile:02x?}"
        );
        Ok(())
    })
}

/// Pure noise, not a mutation of anything valid, gets the reference answer too.
#[test]
fn sim_adversary_hex_random_bytes_get_the_reference_answer() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let len = attacker.below(24);
        let noise = attacker.bytes(len);
        assert_eq!(
            decode(&noise),
            reference_decode(&noise),
            "seed {seed:#x} round {round}: {noise:02x?}"
        );
        Ok(())
    })
}

/// Every one of the 256 byte values, at every position of a four-character
/// string, is a digit exactly when the reference says so and is otherwise named
/// with its offset and value.
#[test]
fn sim_adversary_hex_every_byte_value_at_every_position() {
    every_byte_at_every_position(*b"00ff", |input, position, candidate| {
        assert_eq!(
            decode(input),
            reference_decode(&input),
            "{candidate:#04x} at {position}: a digit or a named refusal"
        );
        if digit_value(candidate).is_none() {
            assert_eq!(
                decode(input),
                Err(DecodeError::NotHexDigit {
                    at: position,
                    byte: candidate
                }),
                "{candidate:#04x} at {position} must be named"
            );
        }
    });
}

/// With several bad characters the first by offset is the one reported, so a
/// caller fixing errors left to right always makes progress.
#[test]
fn sim_adversary_hex_the_first_bad_character_wins() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let pairs = 2_usize.saturating_add(attacker.below(14));
        let mut input = reference_encode(&attacker.bytes(pairs)).into_bytes();
        let first = attacker.below(input.len());
        let second = first
            .saturating_add(1)
            .saturating_add(attacker.below(input.len()));
        for at in [first, second] {
            if let Some(slot) = input.get_mut(at) {
                *slot = b'g';
            }
        }
        assert_eq!(
            decode(&input),
            Err(DecodeError::NotHexDigit {
                at: first,
                byte: b'g'
            }),
            "seed {seed:#x} round {round}: bad characters at {first} and {second}"
        );
        Ok(())
    })
}

/// An odd length is refused as odd even when it also hides a bad digit: the
/// length is checked before any character is read.
#[test]
fn sim_adversary_hex_an_odd_length_hides_nothing() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let pairs = 1_usize.saturating_add(attacker.below(12));
        let mut input = reference_encode(&attacker.bytes(pairs)).into_bytes();
        input.push(b'z');
        if let Some(slot) = input.first_mut() {
            *slot = b'!';
        }
        assert_eq!(
            decode(&input),
            Err(DecodeError::OddLength {
                len: input.len(),
                at: input.len()
            }),
            "seed {seed:#x} round {round}: odd input with a bad digit"
        );
        Ok(())
    })
}

/// Uppercase, lowercase and any mixture decode to the same bytes, so the
/// documented tolerance cannot be used to present two identities.
#[test]
fn sim_adversary_hex_case_is_the_only_freedom_and_it_names_one_value() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let (bytes, text) = draw_pair(attacker, 24);
        let pattern: Vec<bool> = text.chars().map(|_| attacker.below(2) == 0).collect();
        let mixed = recased(&text, &pattern);
        for variant in [text.to_ascii_uppercase(), text.clone(), mixed] {
            assert_eq!(
                decode(&variant),
                Ok(bytes.clone()),
                "seed {seed:#x} round {round}: {variant}"
            );
        }
        Ok(())
    })
}

/// Two accepted strings decode to equal bytes exactly when they are equal
/// ignoring ASCII case: nothing else, no padding or prefix, is ever a synonym.
#[test]
fn sim_adversary_hex_decoding_is_injective_up_to_case() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let (_, text) = draw_pair(attacker, 6);
        let other = attacker.mutate_n(text.as_bytes(), 2);
        let (Ok(left), Ok(right)) = (decode(&text), decode(&other)) else {
            return Ok(());
        };
        assert_eq!(
            left == right,
            text.as_bytes().eq_ignore_ascii_case(&other),
            "seed {seed:#x} round {round}: {text} against {other:02x?}"
        );
        Ok(())
    })
}

/// Anything accepted re-encodes to its lowercase form: encode is the canonical
/// writer for what decode reads.
#[test]
fn sim_adversary_hex_accepted_input_re_encodes_to_its_lowercase_form() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let (_, text) = draw_pair(attacker, 16);
        let hostile = attacker.mutate_n(text.as_bytes(), 2);
        let Ok(bytes) = decode(&hostile) else {
            return Ok(());
        };
        assert_eq!(
            encode(&bytes).into_bytes(),
            hostile.to_ascii_lowercase(),
            "seed {seed:#x} round {round}: {hostile:02x?}"
        );
        Ok(())
    })
}

/// `encode` is two lowercase digits per byte and equals the `{:02x}` reference.
#[test]
fn sim_adversary_hex_encode_is_the_reference_spelling() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let len = attacker.below(64);
        let bytes = attacker.bytes(len);
        let text = encode(&bytes);
        assert_eq!(
            text,
            reference_encode(&bytes),
            "seed {seed:#x} round {round}"
        );
        assert!(
            text.bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "seed {seed:#x} round {round}: {text} is not lowercase hex"
        );
        Ok(())
    })
}

/// Every byte value round-trips on its own and inside a hostile neighbourhood.
#[test]
fn sim_adversary_hex_every_byte_value_round_trips() {
    for byte in 0..=u8::MAX {
        for padding in [&[][..], &[0x00], &[0xff, 0x80], &[0x0f, 0xf0, 0x7f]] {
            let mut bytes = padding.to_vec();
            bytes.push(byte);
            assert_eq!(decode(encode(&bytes)), Ok(bytes), "byte {byte:#04x}");
        }
    }
}

/// `&str`, `&[u8]`, `Vec<u8>` and `String` are one input to the decoder.
#[test]
fn sim_adversary_hex_every_input_type_is_one_input() -> TestResult {
    sweep(256, |seed, round, attacker| {
        let (_, text) = draw_pair(attacker, 16);
        let hostile = attacker.mutate_n(text.as_bytes(), 2);
        let lossy = as_text(&hostile);
        if lossy.as_bytes() == hostile.as_slice() {
            assert_eq!(
                decode(lossy.as_str()),
                decode(&hostile),
                "seed {seed:#x} round {round}: str"
            );
            assert_eq!(
                decode(lossy),
                decode(hostile.clone()),
                "seed {seed:#x} round {round}: String"
            );
        }
        assert_eq!(
            decode(hostile.as_slice()),
            decode(&hostile),
            "seed {seed:#x} round {round}: slice"
        );
        Ok(())
    })
}

/// Prefixes, signs, padding and separators are not hex: each is refused at the
/// character that is not a digit.
#[test]
fn sim_adversary_hex_prefixes_and_separators_are_refused() {
    for (input, at, byte) in [
        ("0x12", 1_usize, b'x'),
        ("0X12", 1, b'X'),
        ("+123", 0, b'+'),
        ("-123", 0, b'-'),
        (" 123", 0, b' '),
        ("123 ", 3, b' '),
        ("12:3", 2, b':'),
        ("12\n3", 2, b'\n'),
        ("12\t3", 2, b'\t'),
        ("\\x12", 0, b'\\'),
        ("12\0\0", 2, 0),
        ("12=!", 2, b'='),
    ] {
        assert_eq!(
            decode(input),
            Err(DecodeError::NotHexDigit { at, byte }),
            "{input:?}"
        );
    }
}

/// Multi-byte glyphs, including fullwidth and mathematical digits that render as
/// hex digits, are never taken for digits: the answer is the reference's, which
/// names the glyph's first byte (or the odd length when that comes first).
#[test]
fn sim_adversary_hex_lookalike_glyphs_are_never_digits() {
    for glyph in [
        '\u{ff11}',
        '\u{ff21}',
        '\u{0661}',
        '\u{1d7ce}',
        '\u{216d}',
        '\u{00e9}',
        '\u{1f4a9}',
    ] {
        let input = format!("00{glyph}0");
        assert_eq!(
            decode(&input),
            reference_decode(input.as_bytes()),
            "{glyph:?}: must match the reference"
        );
        assert!(decode(&input).is_err(), "{glyph:?} was taken for a digit");
    }
}

/// A bad character `position` bytes into a megabyte of valid hex is named at
/// exactly that offset: the decoder neither stops early nor loses its place.
#[test]
fn sim_adversary_hex_a_far_offset_is_reported_exactly() -> TestResult {
    let size = 1_048_576_usize;
    let base = vec![b'f'; size];
    sweep(8, |seed, round, attacker| {
        let position = attacker.below(size);
        let mut input = base.clone();
        input[position] = b'G';
        assert_eq!(
            decode(&input),
            Err(DecodeError::NotHexDigit {
                at: position,
                byte: b'G'
            }),
            "seed {seed:#x} round {round}"
        );
        Ok(())
    })
}

/// A megabyte of valid hex decodes to half a megabyte of the right bytes, and
/// one more character makes it odd.
#[test]
fn sim_adversary_hex_a_large_input_decodes_completely() -> TestResult {
    let input = vec![b'F'; 1_048_576];
    let bytes = decode(&input)?;
    assert_eq!(bytes.len(), 524_288, "half a megabyte of bytes");
    assert!(
        all_equal(&bytes, 0xff),
        "every byte of the megabyte is 0xff"
    );
    let mut odd = input;
    odd.push(b'f');
    assert_eq!(
        decode(&odd),
        Err(DecodeError::OddLength {
            len: 1_048_577,
            at: 1_048_577
        }),
        "one more character"
    );
    Ok(())
}

/// An empty string is the empty byte string, both ways.
#[test]
fn sim_adversary_hex_nothing_decodes_to_nothing() {
    assert_eq!(decode(""), Ok(Vec::new()), "empty decodes to empty");
    assert_eq!(encode([0_u8; 0]), "", "empty encodes to empty");
    assert_eq!(
        decode_into("", &mut []),
        Ok(()),
        "empty fills an empty destination"
    );
}

/// A destination of the right size is filled with exactly what `decode` returns.
#[test]
fn sim_adversary_hex_decode_into_agrees_with_decode() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let (bytes, text) = draw_pair(attacker, 20);
        let mut output = vec![SENTINEL; bytes.len()];
        assert_eq!(
            decode_into(&text, &mut output),
            Ok(()),
            "seed {seed:#x} round {round}"
        );
        assert_eq!(output, bytes, "seed {seed:#x} round {round}");
        Ok(())
    })
}

/// Any refusal leaves every byte of the destination as it was, so a caller that
/// ignores the error still never reads half-decoded bytes (CWE-908).
#[test]
fn sim_adversary_hex_a_refusal_leaves_the_destination_untouched() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let (_, text) = draw_pair(attacker, 16);
        let hostile = attacker.mutate_n(text.as_bytes(), 3);
        let slack = attacker.below(4);
        let mut output = vec![SENTINEL; hostile.len().div_ceil(2).saturating_add(slack)];
        if decode_into(&hostile, &mut output).is_err() {
            assert!(
                all_equal(&output, SENTINEL),
                "seed {seed:#x} round {round}: {hostile:02x?} wrote into the destination"
            );
        }
        Ok(())
    })
}

/// A destination one byte short, one byte long or empty is an `OutputLength`
/// that names both sizes, and is not written.
#[test]
fn sim_adversary_hex_a_wrong_sized_destination_is_named() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let (bytes, text) = draw_pair(attacker, 16);
        let expected = bytes.len();
        for actual in [
            expected.saturating_add(1),
            expected.saturating_sub(1),
            0,
            expected.saturating_add(9),
        ] {
            if actual == expected {
                continue;
            }
            let mut output = vec![SENTINEL; actual];
            assert_eq!(
                decode_into(&text, &mut output),
                Err(DecodeError::OutputLength { expected, actual }),
                "seed {seed:#x} round {round}: destination of {actual}"
            );
            assert!(
                all_equal(&output, SENTINEL),
                "seed {seed:#x} round {round}: wrote into a wrong-sized destination"
            );
        }
        Ok(())
    })
}

/// The refusals of `decode_into` come in a fixed order: odd length, then the
/// destination's size, then the digits. An attacker cannot make one hide
/// another.
#[test]
fn sim_adversary_hex_decode_into_refuses_in_a_fixed_order() {
    let mut four = [SENTINEL; 4];
    assert_eq!(
        decode_into("abc", &mut four),
        Err(DecodeError::OddLength { len: 3, at: 3 }),
        "odd length beats a wrong destination"
    );
    assert_eq!(
        decode_into("zz", &mut four),
        Err(DecodeError::OutputLength {
            expected: 1,
            actual: 4
        }),
        "a wrong destination beats a bad digit"
    );
    let mut one = [SENTINEL; 1];
    assert_eq!(
        decode_into("zz", &mut one),
        Err(DecodeError::NotHexDigit { at: 0, byte: b'z' }),
        "a bad digit is named when the sizes agree"
    );
    assert_eq!(one, [SENTINEL], "and the destination is untouched");
}

/// A refusal's message is one printable line, whatever the bad byte was, so a
/// hostile string cannot forge a log line (CWE-117).
#[test]
fn sim_adversary_hex_a_refusal_message_is_one_safe_line() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let len = 1_usize.saturating_add(attacker.below(12));
        let noise = attacker.bytes(len);
        if let Err(error) = decode(&noise) {
            let message = error.to_string();
            assert!(
                !message.contains(['\n', '\r', '\u{1b}', '\0']),
                "seed {seed:#x} round {round}: {message:?} can forge a log line"
            );
        }
        Ok(())
    })
}

/// Every offset a refusal reports lies inside the input and names the byte that
/// is there (or is the input's length for an odd one), so a caller slicing at it
/// cannot be sent past the end.
#[test]
fn sim_adversary_hex_a_reported_offset_never_leaves_the_input() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let len = attacker.below(24);
        let noise = attacker.bytes(len);
        let inside = match decode(&noise) {
            Err(DecodeError::NotHexDigit { at, byte }) => noise.get(at) == Some(&byte),
            Err(DecodeError::OddLength { len, at }) => len == noise.len() && at == noise.len(),
            Err(DecodeError::OutputLength { .. }) | Ok(_) => true,
            Err(_) => false,
        };
        assert!(inside, "seed {seed:#x} round {round}: {noise:02x?}");
        Ok(())
    })
}

/// The same seed mounts the same attack on the decoder.
#[test]
fn sim_adversary_hex_the_same_seed_replays_the_same_campaign() {
    replays_under_every_seed(campaign);
}

/// Two seeds mount two attacks.
#[test]
fn sim_adversary_hex_distinct_seeds_mount_distinct_campaigns() {
    diverges_between_seeds(campaign);
}
