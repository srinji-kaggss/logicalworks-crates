//! A peer controls every character of the base64 a decoder is handed.
//!
//! Base64 carries tokens, keys and signatures, so the attacks that matter are
//! the ones that make two strings mean one message (malleability: a second
//! spelling slips past a deny-list or a hash compare, the trailing-bit reports
//! against PEM and OpenSSH parsers), or that make one string mean two things to
//! two parsers (CWE-444 style desync through padding, whitespace and the
//! URL-safe alphabet). The attacks: a non-canonical final character, padding in
//! every wrong place and count, concatenated encodings, the URL-safe alphabet,
//! MIME line breaks, truncation at every length, glyphs from other scripts, and
//! inputs large enough to expose a decoder that loses its offset.
//!
//! The reference is written here from RFC 4648 sections 4 and 3.5 (a canonical
//! decoder rejects non-zero pad bits), as bits, so it shares no code with the
//! shift-and-mask decoder it checks.

use lgwks_std::encoding::base64::{DecodeError, decode, encode};

use crate::adversary::{
    Attacker, TestResult, decoder_campaign, diverges_between_seeds, every_byte_at_every_position,
    replays_under_every_seed, sweep,
};

/// RFC 4648 table 1: the sextet for index `n` is the character at `n`.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// The ways a transport wraps or pads base64 text: MIME, PEM, and careless
/// pasting.
const BREAKERS: [&str; 5] = ["\n", "\r\n", "\r", " ", "\t"];

/// The sextet a character stands for, from the table.
fn reference_sextet(byte: u8) -> Option<u8> {
    let index = ALPHABET.iter().position(|&candidate| candidate == byte)?;
    u8::try_from(index).ok()
}

/// The bits of `bytes`, most significant first.
fn bits_of(bytes: &[u8]) -> Vec<bool> {
    let mut bits = Vec::with_capacity(bytes.len().saturating_mul(8));
    for &byte in bytes {
        for shift in (0..8_u32).rev() {
            bits.push((byte.wrapping_shr(shift) & 1) == 1);
        }
    }
    bits
}

/// The padded encoding of `bytes`, built from bits and the table.
fn reference_encode(bytes: &[u8]) -> String {
    let mut out = String::new();
    for group in bits_of(bytes).chunks(6) {
        let mut value = 0_usize;
        for index in 0..6 {
            value = value.wrapping_shl(1) | usize::from(group.get(index) == Some(&true));
        }
        out.push(char::from(ALPHABET[value]));
    }
    while !out.len().is_multiple_of(4) {
        out.push('=');
    }
    out
}

/// The text of a padded encoding without its padding, or the first structural
/// fault RFC 4648 section 4 finds: a length off the quantum, more than two pad
/// characters, or a pad character before the end.
fn reference_body(input: &[u8]) -> Result<&[u8], DecodeError> {
    let padding = input.iter().rev().take_while(|&&byte| byte == b'=').count();
    let body = &input[..input.len().saturating_sub(padding)];
    if !input.len().is_multiple_of(4) {
        Err(DecodeError::BadLength {
            len: input.len(),
            at: input.len(),
        })
    } else if padding > 2 {
        Err(DecodeError::MisplacedPadding {
            at: input.len().saturating_sub(padding),
        })
    } else if let Some(at) = body.iter().position(|&byte| byte == b'=') {
        Err(DecodeError::MisplacedPadding { at })
    } else {
        Ok(body)
    }
}

/// The bits of `body`, six per character, or the first character outside the
/// alphabet.
fn reference_bits(body: &[u8]) -> Result<Vec<bool>, DecodeError> {
    let mut bits = Vec::with_capacity(body.len().saturating_mul(6));
    for (at, &byte) in body.iter().enumerate() {
        let value = reference_sextet(byte).ok_or(DecodeError::NotInAlphabet { at, byte })?;
        for shift in (0..6_u32).rev() {
            bits.push((value.wrapping_shr(shift) & 1) == 1);
        }
    }
    Ok(bits)
}

/// The answer RFC 4648 gives for `input`, in the order the crate documents.
fn reference_decode(input: &[u8]) -> Result<Vec<u8>, DecodeError> {
    if input.is_empty() {
        return Ok(Vec::new());
    }
    let body = reference_body(input)?;
    let bits = reference_bits(body)?;
    let whole = bits.len().div_euclid(8).saturating_mul(8);
    let (data, rest) = bits.split_at(whole);
    if rest.contains(&true) {
        Err(DecodeError::MisplacedPadding { at: body.len() })
    } else {
        Ok(data
            .chunks(8)
            .map(|octet| {
                octet
                    .iter()
                    .fold(0_u8, |value, &bit| value.wrapping_shl(1) | u8::from(bit))
            })
            .collect())
    }
}

/// A valid encoding of `below(max)` random bytes, with the bytes.
fn draw_pair(attacker: &mut Attacker, max: usize) -> (Vec<u8>, String) {
    attacker.spelled(max, reference_encode)
}

/// `text` with the character at `at` replaced by a different alphabet character.
fn substituted(text: &str, at: usize, attacker: &mut Attacker) -> Vec<u8> {
    let mut bytes = text.as_bytes().to_vec();
    if let Some(slot) = bytes.get_mut(at) {
        let mut replacement = *attacker.pick(ALPHABET);
        while replacement == *slot {
            replacement = *attacker.pick(ALPHABET);
        }
        *slot = replacement;
    }
    bytes
}

/// A valid encoding with one character swapped, and what the decoder made of it
/// when it accepted it: the draw most likely to land on something decodable.
fn accepted_swap(attacker: &mut Attacker) -> Option<(Vec<u8>, Vec<u8>)> {
    let (_, text) = draw_pair(attacker, 16);
    let at = attacker.below(text.len().max(1));
    let hostile = substituted(&text, at, attacker);
    let bytes = decode(&hostile).ok()?;
    Some((hostile, bytes))
}

/// `text` with `1..=5` transport breaks inserted at random positions.
fn wrapped(text: &str, attacker: &mut Attacker) -> String {
    let mut out = text.to_owned();
    for _ in 0..1_usize.saturating_add(attacker.below(5)) {
        let at = attacker.below(out.len().saturating_add(1));
        out.insert_str(at, attacker.pick(&BREAKERS));
    }
    out
}

/// One campaign: hostile base64 through `decode`, outcomes folded into a trace.
fn campaign(seed: u64) -> u64 {
    decoder_campaign(
        seed,
        |attacker| reference_encode(&attacker.bytes(24)).into_bytes(),
        |hostile| {
            let verdict = decode(hostile);
            format!("{verdict:?}")
        },
    )
}

/// The vectors of RFC 4648 section 10 encode and decode both ways.
#[test]
fn sim_adversary_base64_the_rfc_vectors_hold_both_ways() -> TestResult {
    for (plain, text) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ] {
        assert_eq!(encode(plain), text, "encoding {plain:?}");
        assert_eq!(
            reference_encode(plain.as_bytes()),
            text,
            "reference {plain:?}"
        );
        assert_eq!(decode(text)?, plain.as_bytes(), "decoding {text:?}");
    }
    Ok(())
}

/// Whatever the attacker builds, `decode` gives the reference's answer.
#[test]
fn sim_adversary_base64_hostile_input_gets_the_reference_answer() -> TestResult {
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

/// Pure noise gets the reference answer too.
#[test]
fn sim_adversary_base64_random_bytes_get_the_reference_answer() -> TestResult {
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

/// One alphabet character swapped for another, which mostly stays decodable:
/// the answer is the reference's, and an accepted result is never the original
/// message (a changed character is a changed message or a refusal).
#[test]
fn sim_adversary_base64_a_swapped_character_is_never_the_same_message() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let (bytes, text) = draw_pair(attacker, 20);
        if text.is_empty() {
            return Ok(());
        }
        let at = attacker.below(text.len());
        let hostile = substituted(&text, at, attacker);
        assert_eq!(
            decode(&hostile),
            reference_decode(&hostile),
            "seed {seed:#x} round {round}: {hostile:02x?}"
        );
        if let Ok(decoded) = decode(&hostile) {
            assert_ne!(
                decoded, bytes,
                "seed {seed:#x} round {round}: {text} and {hostile:02x?} are one message"
            );
        }
        Ok(())
    })
}

/// Every byte value at every position of a clean quantum gets the reference's
/// answer: a character exactly when the table says so, otherwise named with its
/// offset and value.
#[test]
fn sim_adversary_base64_every_byte_value_at_every_position() {
    every_byte_at_every_position(*b"Zm9v", |input, position, candidate| {
        assert_eq!(
            decode(input),
            reference_decode(&input),
            "{candidate:#04x} at {position}: a character or a named refusal"
        );
    });
}

/// Anything accepted is the canonical spelling of what it decodes to: no second
/// string names the same message, so a deny-list or hash over the text cannot be
/// stepped around.
#[test]
fn sim_adversary_base64_accepted_input_is_the_canonical_spelling() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let Some((hostile, bytes)) = accepted_swap(attacker) else {
            return Ok(());
        };
        assert_eq!(
            encode(&bytes).into_bytes(),
            hostile,
            "seed {seed:#x} round {round}: accepted but not canonical"
        );
        Ok(())
    })
}

/// The padding of an accepted input is exactly what its message length needs:
/// there is no way to spell the same length with more or less.
#[test]
fn sim_adversary_base64_accepted_padding_matches_the_message_length() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let Some((hostile, bytes)) = accepted_swap(attacker) else {
            return Ok(());
        };
        let padding = hostile.iter().filter(|&&byte| byte == b'=').count();
        assert_eq!(
            padding,
            3_usize
                .saturating_sub(bytes.len().rem_euclid(3))
                .rem_euclid(3),
            "seed {seed:#x} round {round}: {} bytes",
            bytes.len()
        );
        Ok(())
    })
}

/// A final character whose unused low bits are set is refused, for every one of
/// the sixty-four characters: after one message byte only four characters are
/// canonical, after two only sixteen.
#[test]
fn sim_adversary_base64_nonzero_pad_bits_are_refused() {
    let (mut one_byte, mut two_bytes) = (0_usize, 0_usize);
    for &last in ALPHABET {
        let Some(value) = reference_sextet(last) else {
            continue;
        };
        let single = format!("Z{}==", char::from(last));
        if (value & 0x0f) == 0 {
            one_byte = one_byte.saturating_add(1);
            // `Z` is sextet 25: its six bits, then the top two of this character.
            let first = 25_u8.wrapping_shl(2) | value.wrapping_shr(4);
            assert_eq!(decode(&single), Ok(vec![first]), "{single}");
        } else {
            assert_eq!(
                decode(&single),
                Err(DecodeError::MisplacedPadding { at: 2 }),
                "{single}: pad bits set after one byte"
            );
        }
        let pair = format!("Zm{}=", char::from(last));
        if (value & 0x03) == 0 {
            two_bytes = two_bytes.saturating_add(1);
            // `Z` is 25 and `m` is 38: the first byte is 25 then the top two of
            // 38, the second is its low four then the top four of this one.
            let first = 25_u8.wrapping_shl(2) | 38_u8.wrapping_shr(4);
            let second = (38_u8 & 0x0f).wrapping_shl(4) | value.wrapping_shr(2);
            assert_eq!(decode(&pair), Ok(vec![first, second]), "{pair}");
        } else {
            assert_eq!(
                decode(&pair),
                Err(DecodeError::MisplacedPadding { at: 3 }),
                "{pair}: pad bits set after two bytes"
            );
        }
    }
    assert_eq!(one_byte, 4, "canonical final characters after one byte");
    assert_eq!(two_bytes, 16, "canonical final characters after two bytes");
}

/// Padding in every wrong place is named at the offset the grammar gives.
#[test]
fn sim_adversary_base64_padding_in_the_wrong_place_is_named() {
    for (input, at) in [
        ("====", 0_usize),
        ("A===", 1),
        ("AA=A", 2),
        ("=AAA", 0),
        ("A=AA", 1),
        ("AAAA====", 4),
        ("AA==AA==", 2),
        ("AAA=AAA=", 3),
        ("AAAAA===", 5),
    ] {
        assert_eq!(
            decode(input),
            Err(DecodeError::MisplacedPadding { at }),
            "{input}"
        );
        assert_eq!(
            decode(input),
            reference_decode(input.as_bytes()),
            "{input}: reference"
        );
    }
}

/// A run of padding longer than two, at the end of inputs of several sizes, is
/// refused at the start of the run.
#[test]
fn sim_adversary_base64_a_long_run_of_padding_is_refused() {
    for total in [4_usize, 8, 16, 64, 1_024] {
        for run in 3..=total {
            let mut input = vec![b'A'; total.saturating_sub(run)];
            input.extend(std::iter::repeat_n(b'=', run));
            assert_eq!(
                decode(&input),
                Err(DecodeError::MisplacedPadding {
                    at: total.saturating_sub(run)
                }),
                "{total} characters, a run of {run}"
            );
        }
    }
}

/// Two padded encodings joined are refused at the first padding character, and
/// two unpadded ones joined decode to the two messages joined: concatenation
/// cannot smuggle a second message past a length check.
#[test]
fn sim_adversary_base64_joined_encodings_are_refused_at_the_padding() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let (left, left_text) = draw_pair(attacker, 12);
        let (right, right_text) = draw_pair(attacker, 12);
        let joined = format!("{left_text}{right_text}");
        assert_eq!(
            decode(&joined),
            reference_decode(joined.as_bytes()),
            "seed {seed:#x} round {round}: {joined}"
        );
        match (left_text.find('='), right.is_empty()) {
            (Some(at), false) => assert_eq!(
                decode(&joined),
                Err(DecodeError::MisplacedPadding { at }),
                "seed {seed:#x} round {round}: {joined}"
            ),
            (None, _) => {
                let mut both = left;
                both.extend(right);
                assert_eq!(decode(&joined), Ok(both), "seed {seed:#x} round {round}");
            }
            (Some(_), true) => {}
        }
        Ok(())
    })
}

/// The URL-safe alphabet is not this alphabet: `-` and `_` are refused at their
/// first position, so a token cannot be read as two different messages by a
/// standard decoder and a URL-safe one.
#[test]
fn sim_adversary_base64_the_url_safe_alphabet_is_refused() -> TestResult {
    let mut converted = 0_usize;
    sweep(512, |seed, round, attacker| {
        let len = 1_usize.saturating_add(attacker.below(32));
        let text = encode(attacker.bytes(len));
        let Some(at) = text.find(['+', '/']) else {
            return Ok(());
        };
        converted = converted.saturating_add(1);
        let url_safe = text.replace('+', "-").replace('/', "_");
        let byte = url_safe.as_bytes()[at];
        assert_eq!(
            decode(&url_safe),
            Err(DecodeError::NotInAlphabet { at, byte }),
            "seed {seed:#x} round {round}: {url_safe}"
        );
        Ok(())
    })?;
    assert!(converted > 400, "only {converted} messages used + or /");
    Ok(())
}

/// MIME and PEM line breaks, spaces and tabs inside the text are refused,
/// wherever they fall and however many there are: the decoder never skips
/// whitespace to reach a valid message.
#[test]
fn sim_adversary_base64_transport_breaks_are_refused() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let len = 4_usize.saturating_add(attacker.below(60));
        let text = encode(attacker.bytes(len));
        let broken = wrapped(&text, attacker);
        assert_eq!(
            decode(&broken),
            reference_decode(broken.as_bytes()),
            "seed {seed:#x} round {round}: {broken:?}"
        );
        assert!(
            decode(&broken).is_err(),
            "seed {seed:#x} round {round}: {broken:?} was accepted"
        );
        Ok(())
    })
}

/// A valid encoding cut at every length: a cut that is not a whole number of
/// quanta is a bad length that names the cut, and a cut that is gives back the
/// matching prefix of the message.
#[test]
fn sim_adversary_base64_truncation_is_caught_unless_it_lands_on_a_quantum() -> TestResult {
    sweep(256, |seed, round, attacker| {
        let (bytes, text) = draw_pair(attacker, 40);
        for cut in 0..text.len() {
            let prefix = &text.as_bytes()[..cut];
            if cut.is_multiple_of(4) {
                let kept = cut.div_euclid(4).saturating_mul(3);
                assert_eq!(
                    decode(prefix),
                    Ok(bytes[..kept].to_vec()),
                    "seed {seed:#x} round {round}: cut at {cut}"
                );
            } else {
                assert_eq!(
                    decode(prefix),
                    Err(DecodeError::BadLength { len: cut, at: cut }),
                    "seed {seed:#x} round {round}: cut at {cut}"
                );
            }
        }
        Ok(())
    })
}

/// Look-alike letters from other scripts, fullwidth forms and emoji are never
/// taken for alphabet characters: the answer is the reference's and it is a
/// refusal.
#[test]
fn sim_adversary_base64_lookalike_glyphs_are_never_characters() {
    for glyph in [
        '\u{ff21}',
        '\u{0410}',
        '\u{0391}',
        '\u{ff0b}',
        '\u{ff0f}',
        '\u{ff1d}',
        '\u{1d400}',
        '\u{1f4a9}',
    ] {
        for template in ["Zm9#", "#m9v", "Zm#v", "Zm9v#Zm9v"] {
            let input = template.replace('#', &glyph.to_string());
            assert_eq!(
                decode(&input),
                reference_decode(input.as_bytes()),
                "{glyph:?} in {template}"
            );
            assert!(
                decode(&input).is_err(),
                "{glyph:?} in {template} was accepted"
            );
        }
    }
}

/// A run of one character of every length is a bad length unless the length is a
/// quantum, and then it decodes to the right number of bytes.
#[test]
fn sim_adversary_base64_every_length_is_a_quantum_or_a_bad_length() {
    for len in 1..=64_usize {
        let input = vec![b'A'; len];
        if len.is_multiple_of(4) {
            let expected = len.div_euclid(4).saturating_mul(3);
            assert_eq!(
                decode(&input).map(|bytes| bytes.len()),
                Ok(expected),
                "length {len}"
            );
        } else {
            assert_eq!(
                decode(&input),
                Err(DecodeError::BadLength { len, at: len }),
                "length {len}"
            );
        }
    }
}

/// The encoder's output is the reference's, four characters per three bytes
/// rounded up, with padding only at the end and only as many as the message
/// needs.
#[test]
fn sim_adversary_base64_encode_is_the_reference_spelling() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let len = attacker.below(100);
        let bytes = attacker.bytes(len);
        let text = encode(&bytes);
        assert_eq!(
            text,
            reference_encode(&bytes),
            "seed {seed:#x} round {round}"
        );
        assert_eq!(
            text.len(),
            len.div_ceil(3).saturating_mul(4),
            "seed {seed:#x} round {round}: length"
        );
        let padding = text.bytes().rev().take_while(|&byte| byte == b'=').count();
        assert_eq!(
            padding,
            3_usize.saturating_sub(len.rem_euclid(3)).rem_euclid(3),
            "seed {seed:#x} round {round}: padding of {len} bytes"
        );
        Ok(())
    })
}

/// Messages of every length up to three hundred round-trip, at every residue.
#[test]
fn sim_adversary_base64_every_length_round_trips() -> TestResult {
    sweep(1, |seed, _, attacker| {
        for len in 0..=300_usize {
            let bytes = attacker.bytes(len);
            assert_eq!(
                decode(encode(&bytes))?,
                bytes,
                "seed {seed:#x}: {len} bytes"
            );
        }
        Ok(())
    })
}

/// Every one-byte and every two-byte message round-trips and has the padding its
/// length calls for: the whole of the small message space, not a sample.
#[test]
fn sim_adversary_base64_every_short_message_round_trips() -> TestResult {
    for first in 0..=u8::MAX {
        let single = [first];
        let text = encode(single);
        assert!(text.ends_with("=="), "{first:#04x}: {text}");
        assert_eq!(decode(&text)?, single, "{first:#04x}");
        for second in 0..=u8::MAX {
            let pair = [first, second];
            let text = encode(pair);
            assert!(
                text.ends_with('=') && !text.ends_with("=="),
                "{pair:02x?}: {text}"
            );
            assert_eq!(decode(&text)?, pair, "{pair:02x?}");
        }
    }
    Ok(())
}

/// Two accepted strings decode to equal bytes exactly when they are equal: the
/// decoding is injective, so equal messages have one name.
#[test]
fn sim_adversary_base64_decoding_is_injective() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let (_, text) = draw_pair(attacker, 8);
        let at = attacker.below(text.len().max(1));
        let other = substituted(&text, at, attacker);
        let (Ok(left), Ok(right)) = (decode(&text), decode(&other)) else {
            return Ok(());
        };
        assert_eq!(
            left == right,
            text.as_bytes() == other.as_slice(),
            "seed {seed:#x} round {round}: {text} against {other:02x?}"
        );
        Ok(())
    })
}

/// Three quarters of a megabyte round-trips, and a bad character anywhere in the
/// resulting megabyte of text is named at exactly its offset.
#[test]
fn sim_adversary_base64_a_far_offset_is_reported_exactly() -> TestResult {
    let bytes = vec![0xa7_u8; 786_432];
    let text = encode(&bytes);
    assert_eq!(text.len(), 1_048_576, "four characters per three bytes");
    assert_eq!(decode(&text)?, bytes, "the megabyte round-trips");
    sweep(8, |seed, round, attacker| {
        let position = attacker.below(text.len());
        let mut hostile = text.clone().into_bytes();
        hostile[position] = b'!';
        assert_eq!(
            decode(&hostile),
            Err(DecodeError::NotInAlphabet {
                at: position,
                byte: b'!'
            }),
            "seed {seed:#x} round {round}"
        );
        Ok(())
    })
}

/// A refusal's message is one printable line, whatever the bad byte was, so a
/// hostile string cannot forge a log line (CWE-117).
#[test]
fn sim_adversary_base64_a_refusal_message_is_one_safe_line() -> TestResult {
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
/// is there, so a caller slicing at it cannot be sent past the end.
#[test]
fn sim_adversary_base64_a_reported_offset_never_leaves_the_input() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let (_, text) = draw_pair(attacker, 12);
        let hostile = attacker.mutate_n(text.as_bytes(), 3);
        let inside = match decode(&hostile) {
            Err(DecodeError::NotInAlphabet { at, byte }) => hostile.get(at) == Some(&byte),
            Err(DecodeError::BadLength { len, at }) => len == hostile.len() && at == len,
            Err(DecodeError::MisplacedPadding { at }) => at <= hostile.len(),
            Ok(_) => true,
            Err(_) => false,
        };
        assert!(inside, "seed {seed:#x} round {round}: {hostile:02x?}");
        Ok(())
    })
}

/// Nothing decodes to nothing, whatever type carries it, and one space is a bad
/// length rather than an empty message.
#[test]
fn sim_adversary_base64_nothing_decodes_to_nothing() {
    assert_eq!(decode(""), Ok(Vec::new()), "str");
    assert_eq!(decode(b""), Ok(Vec::new()), "bytes");
    assert_eq!(decode(String::new()), Ok(Vec::new()), "String");
    assert_eq!(encode([0_u8; 0]), "", "empty encodes to empty");
    assert_eq!(
        decode(" "),
        Err(DecodeError::BadLength { len: 1, at: 1 }),
        "a space is not nothing"
    );
    assert_eq!(
        decode("AAAA"),
        Ok(vec![0, 0, 0]),
        "four zero sextets are three zero bytes"
    );
}

/// The same seed mounts the same attack on the decoder.
#[test]
fn sim_adversary_base64_the_same_seed_replays_the_same_campaign() {
    replays_under_every_seed(campaign);
}

/// Two seeds mount two attacks.
#[test]
fn sim_adversary_base64_distinct_seeds_mount_distinct_campaigns() {
    diverges_between_seeds(campaign);
}
