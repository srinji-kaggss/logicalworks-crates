//! A peer controls every character of the identifier text a parser is handed.
//!
//! Identifiers are keys: they index stores, name tenants and appear in URLs, so a
//! parser that accepts a second spelling of one identifier lets two records
//! share a name, and one that reports the wrong place misleads the operator
//! reading the log. The attacks: a hostile byte of every value at every one of
//! the thirty-six positions, a hyphen moved to every other position, every length
//! from nothing to eighty bytes, the braced, URN and unhyphenated forms other
//! tools print, look-alike hyphens and digits from other scripts, whitespace
//! around a valid text, and a megabyte of text.
//!
//! The reference is written here from RFC 4122 section 3 and 4.1: thirty-two hex
//! digits in an 8-4-4-4-12 layout, hyphens at offsets 8, 13, 18 and 23, the
//! variant in the top bits of byte 8 and the version in the top nibble of byte 6.
//! It reports the first offending offset from the left, which is the order the
//! parser's groups run in.
#![cfg(feature = "random")]

use lgwks_std::id::{ParseError, Uuid};

use crate::adversary::{
    Attacker, TestResult, as_text, decoder_campaign, diverges_between_seeds,
    every_byte_at_every_position, replays_under_every_seed, sweep,
};

/// Where the four hyphens of the canonical form sit.
const HYPHENS: [usize; 4] = [8, 13, 18, 23];

/// Where each of the five groups of hex digits starts.
const GROUP_STARTS: [usize; 5] = [0, 9, 14, 19, 24];

/// How many characters each group holds.
const GROUP_WIDTHS: [usize; 5] = [8, 4, 4, 4, 12];

/// A valid identifier, used where a template is needed.
const SAMPLE: &[u8; 36] = b"123e4567-e89b-12d3-a456-426614174000";

/// The printable ASCII characters a corruption is drawn from.
const PRINTABLE: &[u8] =
    b" !\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~";

/// The value of one hex digit, from the standard library's own definition.
fn digit_value(byte: u8) -> Option<u8> {
    let value = char::from(byte).to_digit(16)?;
    u8::try_from(value).ok()
}

/// The canonical lowercase text of sixteen bytes.
fn render(bytes: &[u8; 16]) -> String {
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// The answer RFC 4122 gives for `text`: the bytes, or the first offending
/// offset from the left.
fn reference_parse(text: &str) -> Result<[u8; 16], ParseError> {
    let bytes = text.as_bytes();
    if bytes.len() == 36 {
        let mut digits = Vec::with_capacity(32);
        for (at, &byte) in bytes.iter().enumerate() {
            digits.extend(reference_digit(at, byte)?);
        }
        let mut out = [0_u8; 16];
        let (pairs, _) = digits.as_chunks::<2>();
        for (slot, &[high, low]) in out.iter_mut().zip(pairs) {
            *slot = high.wrapping_shl(4) | low;
        }
        Ok(out)
    } else {
        Err(ParseError::WrongLength {
            len: bytes.len(),
            at: bytes.len(),
        })
    }
}

/// What the character at `at` contributes: nothing for a hyphen where one
/// belongs, the digit's value for a hex digit, or the fault found there.
fn reference_digit(at: usize, byte: u8) -> Result<Option<u8>, ParseError> {
    if HYPHENS.contains(&at) {
        if byte == b'-' {
            Ok(None)
        } else {
            Err(ParseError::MissingHyphen { at })
        }
    } else {
        let group = HYPHENS.iter().filter(|&&hyphen| hyphen < at).count();
        digit_value(byte).map(Some).ok_or(ParseError::NotHexDigit {
            group_at: GROUP_STARTS[group],
            at,
        })
    }
}

/// What `Uuid::parse` made of `text`, as the bytes it holds.
fn parsed(text: &str) -> Result<[u8; 16], ParseError> {
    Uuid::parse(text).map(|uuid| *uuid.as_bytes())
}

/// A valid identifier of drawn bytes, with the bytes.
fn draw_valid(attacker: &mut Attacker) -> ([u8; 16], String) {
    let bytes = attacker.array::<16>();
    let text = render(&bytes);
    (bytes, text)
}

/// `text` with `count` positions replaced by drawn printable ASCII.
fn corrupted(text: &str, count: usize, attacker: &mut Attacker) -> String {
    let mut bytes = text.as_bytes().to_vec();
    for _ in 0..count {
        let at = attacker.below(bytes.len().max(1));
        let glyph = *attacker.pick(PRINTABLE);
        if let Some(slot) = bytes.get_mut(at) {
            *slot = glyph;
        }
    }
    as_text(&bytes)
}

/// One campaign: hostile identifiers through `parse`, outcomes folded.
fn campaign(seed: u64) -> u64 {
    decoder_campaign(
        seed,
        |attacker| render(&attacker.array::<16>()).into_bytes(),
        |hostile| format!("{:?}", Uuid::parse(&as_text(hostile))),
    )
}

/// Whatever the attacker builds from a real identifier, `parse` gives the
/// reference's answer: the same bytes, or the same refusal at the same offset.
#[test]
fn sim_adversary_uuid_hostile_identifiers_get_the_reference_answer() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let (_, text) = draw_valid(attacker);
        let hostile = as_text(&attacker.mutate_n(text.as_bytes(), 3));
        assert_eq!(
            parsed(&hostile),
            reference_parse(&hostile),
            "seed {seed:#x} round {round}: {hostile:?}"
        );
        Ok(())
    })
}

/// A few printable characters dropped into a valid identifier, which keeps the
/// length and so reaches every layout check, gets the reference's answer.
#[test]
fn sim_adversary_uuid_corrupted_characters_get_the_reference_answer() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let (_, text) = draw_valid(attacker);
        let count = 1_usize.saturating_add(attacker.below(4));
        let hostile = corrupted(&text, count, attacker);
        assert_eq!(
            parsed(&hostile),
            reference_parse(&hostile),
            "seed {seed:#x} round {round}: {hostile:?}"
        );
        Ok(())
    })
}

/// Every ASCII byte at every one of the thirty-six positions of a valid
/// identifier gets the reference's answer.
#[test]
fn sim_adversary_uuid_every_ascii_byte_at_every_position() {
    every_byte_at_every_position(*SAMPLE, |input, position, candidate| {
        let Ok(text) = std::str::from_utf8(&input) else {
            return;
        };
        assert_eq!(
            parsed(text),
            reference_parse(text),
            "{candidate:#04x} at {position}: {text:?}"
        );
    });
}

/// A hyphen swapped with a digit anywhere is refused where the layout first
/// breaks, never accepted and never reported at a later offset.
#[test]
fn sim_adversary_uuid_a_misplaced_hyphen_is_named_at_its_first_break() {
    for hyphen in HYPHENS {
        for other in (0..36).filter(|position| !HYPHENS.contains(position)) {
            let mut bytes = SAMPLE.to_vec();
            bytes.swap(hyphen, other);
            let text = as_text(&bytes);
            assert_eq!(
                parsed(&text),
                reference_parse(&text),
                "hyphen {hyphen} swapped with {other}"
            );
            assert!(
                parsed(&text).is_err(),
                "hyphen {hyphen} swapped with {other} was accepted"
            );
        }
    }
}

/// Every length from nothing to eighty bytes, made by cutting or extending a
/// valid identifier, is a wrong length that names the length unless it is 36.
#[test]
fn sim_adversary_uuid_every_length_is_exact_or_named() {
    for len in 0..=80_usize {
        let mut bytes = SAMPLE.to_vec();
        bytes.truncate(len);
        while bytes.len() < len {
            bytes.push(b'a');
        }
        let text = as_text(&bytes);
        assert_eq!(parsed(&text), reference_parse(&text), "length {len}");
        if len != 36 {
            assert_eq!(
                parsed(&text),
                Err(ParseError::WrongLength { len, at: len }),
                "length {len}"
            );
        }
    }
}

/// The forms other tools print are not this form, and each is refused by its
/// length: braces, the URN prefix, no hyphens, padding and line endings.
#[test]
fn sim_adversary_uuid_other_tools_forms_are_refused() {
    let plain = as_text(SAMPLE);
    let bare = plain.replace('-', "");
    for text in [
        format!("{{{plain}}}"),
        format!("urn:uuid:{plain}"),
        format!("({plain})"),
        bare,
        format!(" {plain}"),
        format!("{plain} "),
        format!("{plain}\n"),
        format!("{plain}\r\n"),
        format!("\t{plain}"),
        format!("{plain}\0"),
        plain.to_ascii_uppercase().replace('-', "_"),
    ] {
        assert_eq!(parsed(&text), reference_parse(&text), "{text:?}: reference");
        assert!(parsed(&text).is_err(), "{text:?} was accepted");
    }
}

/// Upper, lower and mixed case name one identifier, and the identifier always
/// prints in lowercase: case is the only freedom, and it is not retained.
#[test]
fn sim_adversary_uuid_case_is_the_only_freedom_and_it_is_not_kept() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let (bytes, text) = draw_valid(attacker);
        let mixed: String = text
            .chars()
            .map(|glyph| {
                if attacker.below(2) == 0 {
                    glyph.to_ascii_uppercase()
                } else {
                    glyph
                }
            })
            .collect();
        for variant in [text.to_ascii_uppercase(), text.clone(), mixed] {
            let uuid = Uuid::parse(&variant)?;
            assert_eq!(uuid.as_bytes(), &bytes, "seed {seed:#x} round {round}");
            assert_eq!(
                uuid.to_string(),
                text,
                "seed {seed:#x} round {round}: {variant}"
            );
        }
        Ok(())
    })
}

/// Anything accepted prints as its own lowercase form, so no accepted string has
/// a second printed identity.
#[test]
fn sim_adversary_uuid_accepted_text_prints_as_its_lowercase_form() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let (_, text) = draw_valid(attacker);
        let hostile = corrupted(&text, 1, attacker);
        let Ok(uuid) = Uuid::parse(&hostile) else {
            return Ok(());
        };
        assert_eq!(
            uuid.to_string(),
            hostile.to_ascii_lowercase(),
            "seed {seed:#x} round {round}"
        );
        Ok(())
    })
}

/// Raw bytes survive printing and parsing, and `as_bytes` hands back what went
/// in, whatever the version and variant bits say.
#[test]
fn sim_adversary_uuid_any_sixteen_bytes_survive_a_print_parse_cycle() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let bytes = attacker.array::<16>();
        let uuid = Uuid::from(bytes);
        assert_eq!(uuid.as_bytes(), &bytes, "seed {seed:#x} round {round}");
        assert_eq!(
            Uuid::parse(&uuid.to_string())?,
            uuid,
            "seed {seed:#x} round {round}: {uuid}"
        );
        Ok(())
    })
}

/// The printed form has the canonical shape for every value: thirty-six
/// characters, hyphens at 8, 13, 18 and 23, lowercase hex between.
#[test]
fn sim_adversary_uuid_the_printed_form_has_the_canonical_shape() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let uuid = Uuid::from(attacker.array::<16>());
        let text = uuid.to_string();
        assert_eq!(
            text,
            render(uuid.as_bytes()),
            "seed {seed:#x} round {round}"
        );
        for (at, byte) in text.bytes().enumerate() {
            let expected = if HYPHENS.contains(&at) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            };
            assert!(expected, "seed {seed:#x} round {round}: {text} at {at}");
        }
        Ok(())
    })
}

/// The version is the top nibble of byte 6 exactly when the top bits of byte 8
/// are the RFC variant `10`, for every pair of those two bytes.
#[test]
fn sim_adversary_uuid_the_version_follows_the_variant_for_every_byte_pair() {
    for byte6 in 0..=u8::MAX {
        for byte8 in 0..=u8::MAX {
            let mut raw = [0_u8; 16];
            raw[6] = byte6;
            raw[8] = byte8;
            let expected = ((byte8 & 0xc0) == 0x80).then_some(byte6.wrapping_shr(4));
            assert_eq!(
                Uuid::from(raw).version(),
                expected,
                "byte 6 {byte6:#04x}, byte 8 {byte8:#04x}"
            );
        }
    }
}

/// Of the sixteen first digits of the fourth group, exactly `8 9 a b` carry the
/// RFC variant: the NCS, Microsoft and reserved variants have no version.
#[test]
fn sim_adversary_uuid_only_four_leading_digits_carry_the_rfc_variant() {
    for digit in "0123456789abcdef".chars() {
        let text = format!("123e4567-e89b-12d3-{digit}456-426614174000");
        let version = Uuid::parse(&text).map(|uuid| uuid.version());
        let carries = matches!(digit, '8' | '9' | 'a' | 'b');
        assert_eq!(
            version,
            Ok(carries.then_some(1)),
            "variant digit {digit}: {text}"
        );
    }
}

/// The identifiers RFC 4122 prints parse to the version they name, and the nil
/// and max identifiers carry none.
#[test]
fn sim_adversary_uuid_the_rfc_examples_carry_their_versions() {
    for (text, version) in [
        ("6ba7b810-9dad-11d1-80b4-00c04fd430c8", Some(1)),
        ("6ba7b811-9dad-11d1-80b4-00c04fd430c8", Some(1)),
        ("f81d4fae-7dec-11d0-a765-00a0c91e6bf6", Some(1)),
        ("00000000-0000-0000-0000-000000000000", None),
        ("ffffffff-ffff-ffff-ffff-ffffffffffff", None),
    ] {
        assert_eq!(
            Uuid::parse(text).map(|uuid| uuid.version()),
            Ok(version),
            "{text}"
        );
    }
}

/// A non-hex refusal names the group it is in and an offset inside that group's
/// span, and the byte there is not a hex digit.
#[test]
fn sim_adversary_uuid_a_bad_digit_names_its_group() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let (_, text) = draw_valid(attacker);
        let hostile = corrupted(&text, 2, attacker);
        let Err(ParseError::NotHexDigit { group_at, at }) = Uuid::parse(&hostile) else {
            return Ok(());
        };
        let Some(group) = GROUP_STARTS.iter().position(|&start| start == group_at) else {
            return Err(format!("group start {group_at} is not a group").into());
        };
        let in_span = at >= group_at && at < group_at.saturating_add(GROUP_WIDTHS[group]);
        let byte_is_bad = match hostile.as_bytes().get(at) {
            Some(&byte) => digit_value(byte).is_none(),
            None => false,
        };
        assert!(
            in_span && byte_is_bad,
            "seed {seed:#x} round {round}: {hostile:?} named group {group_at} offset {at}"
        );
        Ok(())
    })
}

/// A missing hyphen is named at one of the four separator offsets and the byte
/// there is not a hyphen.
#[test]
fn sim_adversary_uuid_a_missing_hyphen_names_a_separator() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let (_, text) = draw_valid(attacker);
        let hostile = corrupted(&text, 3, attacker);
        let Err(ParseError::MissingHyphen { at }) = Uuid::parse(&hostile) else {
            return Ok(());
        };
        assert!(
            HYPHENS.contains(&at) && hostile.as_bytes().get(at) != Some(&b'-'),
            "seed {seed:#x} round {round}: {hostile:?} named offset {at}"
        );
        Ok(())
    })
}

/// Hyphen look-alikes (the Unicode hyphen, minus sign, en dash and non-breaking
/// hyphen) and fullwidth digits are never accepted: each is multi-byte, so the
/// length or the layout refuses it.
#[test]
fn sim_adversary_uuid_lookalike_glyphs_are_refused() {
    for glyph in [
        '\u{2010}', '\u{2011}', '\u{2013}', '\u{2212}', '\u{ff0d}', '\u{ff11}',
    ] {
        for position in [0_usize, 8, 13, 18, 23, 24, 35] {
            let mut text = as_text(SAMPLE);
            text.replace_range(position..position.saturating_add(1), &glyph.to_string());
            assert_eq!(
                parsed(&text),
                reference_parse(&text),
                "{glyph:?} at {position}"
            );
            assert!(
                parsed(&text).is_err(),
                "{glyph:?} at {position} was accepted"
            );
        }
    }
}

/// Text order is value order: of two identifiers, the one that sorts first as
/// text is the one that sorts first as a value, so an index over either agrees.
#[test]
fn sim_adversary_uuid_text_order_is_value_order() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let left = Uuid::from(attacker.array::<16>());
        let right = Uuid::from(attacker.array::<16>());
        assert_eq!(
            left.cmp(&right),
            left.to_string().cmp(&right.to_string()),
            "seed {seed:#x} round {round}: {left} against {right}"
        );
        Ok(())
    })
}

/// Two accepted strings are one identifier exactly when they are equal ignoring
/// case: no other difference is forgiven.
#[test]
fn sim_adversary_uuid_two_strings_are_one_identifier_only_up_to_case() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let (_, text) = draw_valid(attacker);
        let other = corrupted(&text, 1, attacker);
        let (Ok(left), Ok(right)) = (Uuid::parse(&text), Uuid::parse(&other)) else {
            return Ok(());
        };
        assert_eq!(
            left == right,
            text.eq_ignore_ascii_case(&other),
            "seed {seed:#x} round {round}: {text} against {other}"
        );
        Ok(())
    })
}

/// Distinct values print distinctly: four thousand drawn identifiers have as many
/// distinct texts as distinct values.
#[test]
fn sim_adversary_uuid_printing_is_injective() -> TestResult {
    sweep(1, |seed, _, attacker| {
        let mut values = std::collections::BTreeSet::new();
        let mut texts = std::collections::BTreeSet::new();
        for _ in 0..4_096 {
            let uuid = Uuid::from(attacker.array::<16>());
            values.insert(uuid);
            texts.insert(uuid.to_string());
        }
        assert_eq!(values.len(), texts.len(), "seed {seed:#x}");
        assert_eq!(values.len(), 4_096, "seed {seed:#x}: draws collided");
        Ok(())
    })
}

/// A megabyte of text is a wrong length that names its size, without scanning
/// it for digits first.
#[test]
fn sim_adversary_uuid_a_megabyte_is_a_wrong_length() {
    let text = "a".repeat(1_048_576);
    assert_eq!(
        Uuid::parse(&text),
        Err(ParseError::WrongLength {
            len: 1_048_576,
            at: 1_048_576
        }),
        "a megabyte of digits"
    );
}

/// A refusal's message is one printable line, whatever the bad byte was, so a
/// hostile string cannot forge a log line (CWE-117).
#[test]
fn sim_adversary_uuid_a_refusal_message_is_one_safe_line() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let (_, text) = draw_valid(attacker);
        let hostile = as_text(&attacker.mutate_n(text.as_bytes(), 3));
        if let Err(error) = Uuid::parse(&hostile) {
            let message = error.to_string();
            assert!(
                !message.contains(['\n', '\r', '\u{1b}', '\0']),
                "seed {seed:#x} round {round}: {message:?} can forge a log line"
            );
        }
        Ok(())
    })
}

/// The same seed mounts the same attack on the parser.
#[test]
fn sim_adversary_uuid_the_same_seed_replays_the_same_campaign() {
    replays_under_every_seed(campaign);
}

/// Two seeds mount two attacks.
#[test]
fn sim_adversary_uuid_distinct_seeds_mount_distinct_campaigns() {
    diverges_between_seeds(campaign);
}
