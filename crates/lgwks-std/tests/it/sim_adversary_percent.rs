//! A peer controls every character of the percent-encoded text a decoder reads.
//!
//! Percent-decoding sits in front of paths, queries and headers, so a decoder
//! that is lenient here is a traversal or an injection further in: an overlong
//! UTF-8 spelling of `/` walks out of a directory (CVE-2000-0884, the IIS
//! "Unicode" traversal), a decoder run twice turns `%2541` into `A` behind a
//! filter (CWE-174, double decode), a NUL that truncates a C string hides a
//! suffix (CWE-158), and a raw CR or LF splits a response (CWE-113). The attacks:
//! every malformed escape, every invalid byte sequence an escape can spell, a cut
//! at every length, look-alike percent signs and digits, and megabytes of text
//! with one bad escape in it.
//!
//! The reference is written here from RFC 3986 sections 2.1 to 2.3 and the UTF-8
//! of RFC 3629 (through `std::str::from_utf8`), so it shares no code with the
//! decoder it checks.

use lgwks_std::encoding::percent::{DecodeError, decode, encode_component};

use crate::adversary::{
    Attacker, TestResult, as_text, decoder_campaign, diverges_between_seeds,
    every_byte_at_every_position, replays_under_every_seed, sweep,
};

/// What a draw picks a glyph from when it wants more than ASCII: two- to
/// four-byte scalars, the controls an injection needs, and the escape character.
const GLYPHS: [char; 12] = [
    '\u{e9}',
    '\u{4e16}',
    '\u{1f600}',
    '\u{0}',
    '\r',
    '\n',
    '%',
    '+',
    '\u{7f}',
    '\u{80}',
    '\u{ffff}',
    '\u{10ffff}',
];

/// What a hostile draw picks from: mostly the escape character and the digits
/// that follow it, so malformed escapes are the common case and valid ones the
/// next most.
const NOISE: [char; 16] = [
    '%', '%', '%', '0', '1', '4', 'a', 'F', 'f', 'g', 'z', ' ', '+', '\u{e9}', '\u{4e16}', 'A',
];

/// The scalars an escape sequence can spell, one per UTF-8 length and at every
/// boundary of the encoding.
const SCALARS: [char; 10] = [
    '\u{7f}',
    '\u{80}',
    '\u{7ff}',
    '\u{800}',
    '\u{ffff}',
    '\u{10000}',
    '\u{10ffff}',
    '\u{e9}',
    '\u{4e16}',
    '\u{1f600}',
];

/// Text that precedes a hostile sequence, with the length it decodes to, so the
/// offset a refusal must name is known: nothing, plain text, dots, an escape.
const PREFIXES: [(&str, usize); 4] = [("", 0), ("ab", 2), ("..", 2), ("a%2Fb", 3)];

/// The value of one hex digit, from the standard library's own definition.
fn digit_value(byte: u8) -> Option<u8> {
    let value = char::from(byte).to_digit(16)?;
    u8::try_from(value).ok()
}

/// The answer RFC 3986 and RFC 3629 give for `text`: escapes left to right, a
/// `%` needs two following characters, then the bytes must be UTF-8.
fn reference_decode(text: &str) -> Result<String, DecodeError> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0_usize;
    while let Some(&byte) = bytes.get(at) {
        if byte == b'%' {
            out.push(reference_escape(bytes, at)?);
            at = at.saturating_add(3);
        } else {
            out.push(byte);
            at = at.saturating_add(1);
        }
    }
    String::from_utf8(out).map_err(|invalid| DecodeError::NotUtf8 {
        decoded_at: invalid.utf8_error().valid_up_to(),
    })
}

/// The byte the escape at `at` stands for, or its fault: fewer than two
/// characters after the `%`, then the first of the two that is not a hex digit.
fn reference_escape(bytes: &[u8], at: usize) -> Result<u8, DecodeError> {
    let (high_at, low_at) = (at.saturating_add(1), at.saturating_add(2));
    match (bytes.get(high_at), bytes.get(low_at)) {
        (Some(&high), Some(&low)) => {
            let high_value = digit_value(high).ok_or(DecodeError::NotHexDigit { at: high_at })?;
            let low_value = digit_value(low).ok_or(DecodeError::NotHexDigit { at: low_at })?;
            Ok(high_value.wrapping_shl(4) | low_value)
        }
        _ => Err(DecodeError::TruncatedEscape { at }),
    }
}

/// The component encoding of RFC 3986: unreserved characters (section 2.3) as
/// they are, every other byte as an uppercase `%XX`.
fn reference_encode(text: &str) -> String {
    let mut out = String::new();
    for &byte in text.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&escaped(&[byte]));
        }
    }
    out
}

/// Every byte of `bytes` as an uppercase escape, whether it needs one or not.
fn escaped(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("%{byte:02X}")).collect()
}

/// A string of `below(max)` characters, mostly ASCII, with the glyphs an
/// injection or a boundary needs mixed in.
fn draw_text(attacker: &mut Attacker, max: usize) -> String {
    let len = attacker.below(max);
    let mut text = String::new();
    for _ in 0..len {
        if attacker.below(4) == 0 {
            text.push(*attacker.pick(&GLYPHS));
        } else {
            text.push_str(&attacker.text(1));
        }
    }
    text
}

/// A string of `below(max)` characters drawn from the hostile table.
fn draw_noise(attacker: &mut Attacker, max: usize) -> String {
    let len = attacker.below(max);
    (0..len).map(|_| *attacker.pick(&NOISE)).collect()
}

/// A valid encoding of drawn text after `rounds` rounds of mutation, as text.
fn hostile_encoding(attacker: &mut Attacker, rounds: usize) -> String {
    let encoded = encode_component(&draw_text(attacker, 10));
    as_text(&attacker.mutate_n(encoded.as_bytes(), rounds))
}

/// One campaign: hostile escapes through `decode`, outcomes folded into a trace.
fn campaign(seed: u64) -> u64 {
    decoder_campaign(
        seed,
        |attacker| encode_component(&draw_text(attacker, 16)).into_bytes(),
        |hostile| format!("{:?}", decode(&as_text(hostile))),
    )
}

/// Whatever the attacker builds from a real encoding, `decode` gives the
/// reference's answer: the same text, or the same refusal at the same offset.
#[test]
fn sim_adversary_percent_hostile_encodings_get_the_reference_answer() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let hostile = hostile_encoding(attacker, 3);
        assert_eq!(
            decode(&hostile),
            reference_decode(&hostile),
            "seed {seed:#x} round {round}: {hostile:?}"
        );
        Ok(())
    })
}

/// Text built mostly of `%` and the characters around it, which is malformed
/// nearly every time, gets the reference's answer too.
#[test]
fn sim_adversary_percent_escape_heavy_noise_gets_the_reference_answer() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let noise = draw_noise(attacker, 14);
        assert_eq!(
            decode(&noise),
            reference_decode(&noise),
            "seed {seed:#x} round {round}: {noise:?}"
        );
        Ok(())
    })
}

/// Every ASCII byte value at every position of a short escape-bearing string is
/// decoded exactly as the reference says.
#[test]
fn sim_adversary_percent_every_ascii_byte_at_every_position() {
    every_byte_at_every_position(*b"%4a1", |input, position, candidate| {
        let Ok(text) = std::str::from_utf8(&input) else {
            return;
        };
        assert_eq!(
            decode(text),
            reference_decode(text),
            "{candidate:#04x} at {position}: {text:?}"
        );
    });
}

/// Every possible escape, `%00` to `%FF` in upper and lower case, decodes to the
/// reference's answer: a character below 0x80, otherwise an invalid lone byte.
#[test]
fn sim_adversary_percent_every_escape_value_is_decoded_or_refused() {
    for byte in 0..=u8::MAX {
        for text in [format!("%{byte:02X}"), format!("%{byte:02x}")] {
            assert_eq!(decode(&text), reference_decode(&text), "{text}");
            if byte < 0x80 {
                assert_eq!(
                    decode(&text),
                    Ok(char::from(byte).to_string()),
                    "{text} is the character {byte:#04x}"
                );
            } else {
                assert_eq!(
                    decode(&text),
                    Err(DecodeError::NotUtf8 { decoded_at: 0 }),
                    "{text} is a lone byte of a multi-byte sequence"
                );
            }
        }
    }
}

/// Decoding what `encode_component` wrote gives back the text, whatever the
/// text: ASCII, controls, the escape character itself, and every UTF-8 length.
#[test]
fn sim_adversary_percent_encode_then_decode_is_the_identity() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let text = draw_text(attacker, 24);
        assert_eq!(
            decode(&encode_component(&text)),
            Ok(text.clone()),
            "seed {seed:#x} round {round}: {text:?}"
        );
        Ok(())
    })
}

/// The encoder is the reference's: unreserved characters stay, every other byte
/// becomes an uppercase `%XX`, and nothing else appears in its output.
#[test]
fn sim_adversary_percent_encode_is_the_reference_spelling() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let text = draw_text(attacker, 24);
        let encoded = encode_component(&text);
        assert_eq!(
            encoded,
            reference_encode(&text),
            "seed {seed:#x} round {round}: {text:?}"
        );
        assert!(
            encoded
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-._~%".contains(&byte)),
            "seed {seed:#x} round {round}: {encoded} has a character a component cannot carry"
        );
        Ok(())
    })
}

/// The unreserved set is exactly RFC 3986 section 2.3: of the 128 ASCII
/// characters, the sixty-six letters, digits and `-._~` stay as they are and
/// every other one is escaped.
#[test]
fn sim_adversary_percent_the_unreserved_set_is_exactly_rfc_3986() {
    let mut kept = 0_usize;
    for byte in 0..0x80_u8 {
        let text = char::from(byte).to_string();
        let encoded = encode_component(&text);
        if encoded == text {
            kept = kept.saturating_add(1);
            assert!(
                byte.is_ascii_alphanumeric() || b"-._~".contains(&byte),
                "{byte:#04x} must be escaped but is kept"
            );
        } else {
            assert_eq!(encoded, format!("%{byte:02X}"), "{byte:#04x}");
        }
    }
    assert_eq!(kept, 66, "ASCII characters kept as they are");
}

/// An encoding never carries a raw CR, LF or NUL, so a value placed in a header
/// or a path cannot split the line it sits on (CWE-113).
#[test]
fn sim_adversary_percent_an_encoding_never_carries_a_raw_control() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let text = draw_text(attacker, 24);
        let encoded = encode_component(&text);
        assert!(
            !encoded.contains(['\r', '\n', '\0', ' ', '/', '?', '#', '&', '=']),
            "seed {seed:#x} round {round}: {encoded:?} carries a delimiter"
        );
        Ok(())
    })
}

/// Two different texts never share an encoding, so equal encodings are equal
/// texts and a comparison after encoding cannot confuse two values.
#[test]
fn sim_adversary_percent_encoding_is_injective() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let left = draw_text(attacker, 6);
        let right = draw_text(attacker, 6);
        assert_eq!(
            encode_component(&left) == encode_component(&right),
            left == right,
            "seed {seed:#x} round {round}: {left:?} against {right:?}"
        );
        Ok(())
    })
}

/// One decode undoes exactly one layer of encoding: text encoded twice and
/// decoded once is the once-encoded text, never the original (CWE-174).
#[test]
fn sim_adversary_percent_one_decode_removes_one_layer() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let text = draw_text(attacker, 16);
        let once = encode_component(&text);
        let twice = encode_component(&once);
        assert_eq!(
            decode(&twice),
            Ok(once),
            "seed {seed:#x} round {round}: {text:?}"
        );
        Ok(())
    })
}

/// The classic double-encoding payloads decode one layer and stop there.
#[test]
fn sim_adversary_percent_double_encoded_payloads_stop_after_one_layer() {
    for (input, once) in [
        ("%2541", "%41"),
        ("%252e%252e%252f", "%2e%2e%2f"),
        ("%255c", "%5c"),
        ("%25%32%35", "%25"),
        ("%2500", "%00"),
        ("%25252e", "%252e"),
    ] {
        assert_eq!(decode(input), Ok(once.to_owned()), "{input}");
    }
}

/// Hex digits in an escape may be either case, in any mixture, and name the same
/// character.
#[test]
fn sim_adversary_percent_escape_case_names_one_character() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let text = draw_text(attacker, 12);
        let upper = encode_component(&text);
        let mut digits_left = 0_u8;
        let mixed: String = upper
            .chars()
            .map(|glyph| {
                if glyph == '%' {
                    digits_left = 2;
                    return glyph;
                }
                if digits_left == 0 {
                    return glyph;
                }
                digits_left = digits_left.saturating_sub(1);
                if attacker.below(2) == 0 {
                    glyph.to_ascii_lowercase()
                } else {
                    glyph
                }
            })
            .collect();
        assert_eq!(
            decode(&mixed),
            Ok(text),
            "seed {seed:#x} round {round}: {mixed}"
        );
        Ok(())
    })
}

/// Escapes for the unreserved characters are accepted and decode to them, and
/// the literal and escaped spellings name the same text: the decoder does not
/// treat an unneeded escape as an attack, and does not treat `+` as a space.
#[test]
fn sim_adversary_percent_unneeded_escapes_and_plus_are_literal() {
    assert_eq!(
        decode("%41%61%7E%2D%2E%5F"),
        Ok("Aa~-._".to_owned()),
        "escaped"
    );
    assert_eq!(decode("Aa~-._"), Ok("Aa~-._".to_owned()), "literal");
    assert_eq!(decode("a+b"), Ok("a+b".to_owned()), "plus stays plus");
    assert_eq!(
        decode("a%2Bb"),
        Ok("a+b".to_owned()),
        "escaped plus is plus"
    );
    assert_eq!(
        decode("a b"),
        Ok("a b".to_owned()),
        "a raw space passes through"
    );
    assert_eq!(decode("a%20b"), Ok("a b".to_owned()), "an escaped space");
}

/// Overlong UTF-8 spellings of ASCII, including the `%C0%AF` that walked IIS out
/// of its web root, are refused at the first byte of the sequence wherever it
/// falls (CVE-2000-0884).
#[test]
fn sim_adversary_percent_overlong_utf8_is_refused() {
    let overlongs: [&[u8]; 8] = [
        &[0xc0, 0xaf],
        &[0xc0, 0x80],
        &[0xc1, 0xbf],
        &[0xe0, 0x80, 0xaf],
        &[0xe0, 0x9f, 0xbf],
        &[0xf0, 0x80, 0x80, 0xaf],
        &[0xf0, 0x8f, 0xbf, 0xbf],
        &[0xc0, 0xae, 0xc0, 0xae, 0xc0, 0xaf],
    ];
    for bytes in overlongs {
        for (prefix, decoded_at) in PREFIXES {
            let input = format!("{prefix}{}", escaped(bytes));
            assert_eq!(
                decode(&input),
                Err(DecodeError::NotUtf8 { decoded_at }),
                "{input}"
            );
        }
    }
}

/// Surrogate halves, scalars past U+10FFFF, the five- and six-byte forms RFC 2279
/// once allowed, and bytes that start no sequence at all are refused where the
/// sequence starts.
#[test]
fn sim_adversary_percent_surrogates_and_out_of_range_scalars_are_refused() {
    let invalid: [&[u8]; 10] = [
        &[0xed, 0xa0, 0x80],
        &[0xed, 0xaf, 0xbf],
        &[0xed, 0xb0, 0x80],
        &[0xed, 0xbf, 0xbf],
        &[0xf4, 0x90, 0x80, 0x80],
        &[0xf5, 0x80, 0x80, 0x80],
        &[0xf8, 0x88, 0x80, 0x80, 0x80],
        &[0xfe],
        &[0xff],
        &[0x80],
    ];
    for bytes in invalid {
        for (prefix, decoded_at) in PREFIXES {
            let input = format!("{prefix}{}", escaped(bytes));
            assert_eq!(
                decode(&input),
                Err(DecodeError::NotUtf8 { decoded_at }),
                "{input}"
            );
        }
    }
}

/// A multi-byte character whose escapes are cut short is refused at the byte
/// that starts it, with or without text after the cut, and the whole sequence
/// decodes to the character: the decoder never repairs, skips or substitutes.
#[test]
fn sim_adversary_percent_a_cut_sequence_is_refused_at_its_start() -> TestResult {
    for scalar in SCALARS {
        let bytes = scalar.to_string().into_bytes();
        for (prefix, decoded_at) in PREFIXES {
            let head = decode(prefix)?;
            let whole = format!("{prefix}{}", escaped(&bytes));
            assert_eq!(decode(&whole), Ok(format!("{head}{scalar}")), "{whole}");
            for cut in 1..bytes.len() {
                for suffix in ["", "cd", "%41"] {
                    let input = format!("{prefix}{}{suffix}", escaped(&bytes[..cut]));
                    assert_eq!(
                        decode(&input),
                        Err(DecodeError::NotUtf8 { decoded_at }),
                        "{scalar:?} cut after {cut}: {input}"
                    );
                }
            }
        }
    }
    Ok(())
}

/// A NUL is one character like any other: it is not a terminator, so nothing
/// after it is lost and nothing before it is hidden (CWE-158).
#[test]
fn sim_adversary_percent_a_nul_does_not_truncate() {
    assert_eq!(decode("%00"), Ok("\0".to_owned()), "alone");
    assert_eq!(decode("a%00b"), Ok("a\0b".to_owned()), "inside");
    assert_eq!(decode("%00%00"), Ok("\0\0".to_owned()), "twice");
    assert_eq!(decode("a%00"), Ok("a\0".to_owned()), "at the end");
    assert_eq!(
        decode("../../etc/passwd%00.png").map(|text| text.len()),
        Ok(21),
        "the suffix after the NUL is still there"
    );
}

/// CR and LF decode faithfully, and re-encoding the payload neutralises it: the
/// decoder reports exactly what was sent and the encoder is the defence
/// (CWE-113).
#[test]
fn sim_adversary_percent_a_crlf_payload_is_decoded_faithfully_and_neutralised() -> TestResult {
    assert_eq!(decode("%0D%0A"), Ok("\r\n".to_owned()), "upper case");
    assert_eq!(decode("%0d%0a"), Ok("\r\n".to_owned()), "lower case");
    let payload = decode("x%0d%0aSet-Cookie:%20a%3Db")?;
    assert_eq!(payload, "x\r\nSet-Cookie: a=b", "decoded");
    assert_eq!(
        encode_component(&payload),
        "x%0D%0ASet-Cookie%3A%20a%3Db",
        "re-encoded"
    );
    Ok(())
}

/// A valid encoding cut at every length gets the reference's answer, and a cut
/// inside an escape is a truncated escape at that escape's `%`.
#[test]
fn sim_adversary_percent_a_cut_inside_an_escape_names_the_escape() -> TestResult {
    sweep(256, |seed, round, attacker| {
        let encoded = encode_component(&draw_text(attacker, 12));
        for cut in 0..=encoded.len() {
            let prefix = &encoded[..cut];
            assert_eq!(
                decode(prefix),
                reference_decode(prefix),
                "seed {seed:#x} round {round}: cut at {cut} of {encoded}"
            );
            if let Some(at) = prefix.rfind('%').filter(|&at| cut.saturating_sub(at) < 3) {
                assert_eq!(
                    decode(prefix),
                    Err(DecodeError::TruncatedEscape { at }),
                    "seed {seed:#x} round {round}: cut at {cut} of {encoded}"
                );
            }
        }
        Ok(())
    })
}

/// Scalars at every UTF-8 length and boundary round-trip, and each is escaped as
/// three characters per byte.
#[test]
fn sim_adversary_percent_every_utf8_length_round_trips() {
    for scalar in SCALARS {
        let text = scalar.to_string();
        let encoded = encode_component(&text);
        assert_eq!(
            encoded.len(),
            scalar.len_utf8().saturating_mul(3),
            "{scalar:?}"
        );
        assert_eq!(decode(&encoded), Ok(text), "{scalar:?}");
    }
}

/// A bad escape anywhere in a megabyte of valid ones is named at exactly its
/// offset, in either digit, and a `%` cut off at the very end is a truncated
/// escape there.
#[test]
fn sim_adversary_percent_a_far_bad_escape_is_reported_exactly() -> TestResult {
    let count = 349_525_usize;
    let base = "%41".repeat(count);
    assert_eq!(decode(&base)?.len(), count, "every escape decodes");
    sweep(8, |seed, round, attacker| {
        let start = attacker.below(count).saturating_mul(3);
        for digit in [1_usize, 2] {
            let mut bytes = base.clone().into_bytes();
            bytes[start.saturating_add(digit)] = b'G';
            let text = String::from_utf8(bytes)?;
            assert_eq!(
                decode(&text),
                Err(DecodeError::NotHexDigit {
                    at: start.saturating_add(digit)
                }),
                "seed {seed:#x} round {round}: digit {digit}"
            );
        }
        Ok(())
    })?;
    let cut = format!("{base}%4");
    assert_eq!(
        decode(&cut),
        Err(DecodeError::TruncatedEscape { at: base.len() }),
        "a cut escape at the end"
    );
    Ok(())
}

/// The fullwidth percent sign is an ordinary character, and fullwidth or other
/// script digits after a real `%` are not hex digits: no look-alike starts an
/// escape or fills one.
#[test]
fn sim_adversary_percent_lookalikes_neither_start_nor_fill_an_escape() {
    for (input, expected) in [
        ("\u{ff05}41", Ok("\u{ff05}41".to_owned())),
        ("%\u{ff14}\u{ff11}", Err(DecodeError::NotHexDigit { at: 1 })),
        ("%4\u{ff11}", Err(DecodeError::NotHexDigit { at: 2 })),
        ("%\u{0661}1", Err(DecodeError::NotHexDigit { at: 1 })),
        (
            "%\u{1d7ce}\u{1d7ce}",
            Err(DecodeError::NotHexDigit { at: 1 }),
        ),
        ("%C3%A9", Ok("\u{e9}".to_owned())),
        ("\u{e9}", Ok("\u{e9}".to_owned())),
        ("%e9", Err(DecodeError::NotUtf8 { decoded_at: 0 })),
    ] {
        assert_eq!(decode(input), expected, "{input:?}");
        assert_eq!(
            decode(input),
            reference_decode(input),
            "{input:?}: reference"
        );
    }
}

/// The shortest malformed strings, each named exactly: a `%` needs two
/// characters after it, and the first of them is checked first.
#[test]
fn sim_adversary_percent_the_shortest_malformed_strings_are_named() {
    for (input, expected) in [
        ("%", DecodeError::TruncatedEscape { at: 0 }),
        ("%4", DecodeError::TruncatedEscape { at: 0 }),
        ("%%", DecodeError::TruncatedEscape { at: 0 }),
        ("%%%", DecodeError::NotHexDigit { at: 1 }),
        ("%4%", DecodeError::NotHexDigit { at: 2 }),
        ("a%", DecodeError::TruncatedEscape { at: 1 }),
        ("a%4", DecodeError::TruncatedEscape { at: 1 }),
        ("%zz", DecodeError::NotHexDigit { at: 1 }),
        ("%4z", DecodeError::NotHexDigit { at: 2 }),
        ("%z4", DecodeError::NotHexDigit { at: 1 }),
        ("%41%", DecodeError::TruncatedEscape { at: 3 }),
        ("%41%4", DecodeError::TruncatedEscape { at: 3 }),
        ("%%41", DecodeError::NotHexDigit { at: 1 }),
        ("\u{e9}%", DecodeError::TruncatedEscape { at: 2 }),
    ] {
        assert_eq!(decode(input), Err(expected), "{input:?}");
    }
    assert_eq!(decode(""), Ok(String::new()), "nothing decodes to nothing");
}

/// Every offset a refusal reports is where the problem is: a truncated escape
/// sits on a `%` with fewer than three bytes left, a bad digit sits on a byte
/// that is not a hex digit, and a UTF-8 offset is inside the decoded text.
#[test]
fn sim_adversary_percent_a_reported_offset_names_the_problem() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let hostile = hostile_encoding(attacker, 3);
        let bytes = hostile.as_bytes();
        let names_it = match decode(&hostile) {
            Err(DecodeError::TruncatedEscape { at }) => {
                bytes.get(at) == Some(&b'%') && bytes.len().saturating_sub(at) < 3
            }
            Err(DecodeError::NotHexDigit { at }) => match bytes.get(at) {
                Some(&byte) => digit_value(byte).is_none() && at > 0,
                None => false,
            },
            Err(DecodeError::NotUtf8 { decoded_at }) => decoded_at < bytes.len(),
            Ok(_) => true,
            Err(_) => false,
        };
        assert!(names_it, "seed {seed:#x} round {round}: {hostile:?}");
        Ok(())
    })
}

/// A refusal's message is one printable line, whatever the text was, so a hostile
/// string cannot forge a log line (CWE-117).
#[test]
fn sim_adversary_percent_a_refusal_message_is_one_safe_line() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let noise = draw_noise(attacker, 14);
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

/// Text that is accepted decodes to text that re-encodes and decodes to itself:
/// whatever spelling the attacker chose, the value has one canonical encoding.
#[test]
fn sim_adversary_percent_accepted_input_settles_on_one_encoding() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let hostile = hostile_encoding(attacker, 1);
        let Ok(text) = decode(&hostile) else {
            return Ok(());
        };
        let canonical = encode_component(&text);
        assert_eq!(
            decode(&canonical),
            Ok(text),
            "seed {seed:#x} round {round}: {hostile:?} settles on {canonical}"
        );
        Ok(())
    })
}

/// The same seed mounts the same attack on the decoder.
#[test]
fn sim_adversary_percent_the_same_seed_replays_the_same_campaign() {
    replays_under_every_seed(campaign);
}

/// Two seeds mount two attacks.
#[test]
fn sim_adversary_percent_distinct_seeds_mount_distinct_campaigns() {
    diverges_between_seeds(campaign);
}
