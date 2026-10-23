//! A hostile peer controls every byte of a JSON token: a number, a literal, a
//! string, an escape.
//!
//! The failures this layer of a parser has produced are the ones that let two
//! parsers read one document differently, and the ones that silently change a
//! value: a number the grammar forbids that one reader accepts, the 2^53 precision
//! cliff RFC 7493 warns about, a lone surrogate turned into a replacement
//! character, an overlong UTF-8 sequence that smuggles a quote past a filter
//! (CVE-2000-0884 for the same trick in a path), a raw control character that
//! ends a log line (CWE-117).
//!
//! The references are RFC 8259, written here as a grammar and checked against the
//! parser byte for byte (every byte at every position of the literals, every
//! unescaped byte, every escape letter, every `\u` code unit, all 1,048,576
//! surrogate pairs), and the standard library's own UTF-8 and float parsers.
#![cfg(feature = "json")]

use lgwks_std::json::{Value, from_slice, from_str, to_string};

use crate::adversary::{Attacker, TestResult, as_text, every_byte_at_every_position, sweep};
use crate::json_attacks::{accepts, draw_text};

/// The decimal digits.
const DIGITS: [char; 10] = ['0', '1', '2', '3', '4', '5', '6', '7', '8', '9'];

/// Numbers the grammar forbids or that sit at an edge of it.
const HOSTILE_NUMBERS: [&str; 30] = [
    "01",
    "-01",
    "+1",
    ".5",
    "5.",
    "1.e3",
    "1e",
    "1e+",
    "0x1f",
    "NaN",
    "Infinity",
    "-Infinity",
    "1_0",
    "1,0",
    "--1",
    "-",
    "00",
    "-0",
    "0e0",
    "0.0e-0",
    "1E+2",
    "1e999",
    "-1e999",
    "1e-999",
    "123456789012345678901234567890",
    "0.1e1",
    "1.5",
    "-12.75E-2",
    "9007199254740993",
    "0",
];

/// The bytes RFC 8259 section 2 allows around a value.
const WHITESPACE: [u8; 4] = [0x20, 0x09, 0x0a, 0x0d];

/// Escape letters RFC 8259 section 7 defines, with the character each stands for.
const ESCAPES: [(u8, char); 8] = [
    (b'"', '"'),
    (b'\\', '\\'),
    (b'/', '/'),
    (b'b', '\u{8}'),
    (b'f', '\u{c}'),
    (b'n', '\n'),
    (b'r', '\r'),
    (b't', '\t'),
];

/// Whether `bytes` holds a digit at `at`.
fn digit_at(bytes: &[u8], at: usize) -> bool {
    matches!(bytes.get(at), Some(&byte) if byte.is_ascii_digit())
}

/// The position after the run of digits that starts at `at`.
fn after_digits(bytes: &[u8], at: usize) -> usize {
    let mut end = at;
    while digit_at(bytes, end) {
        end = end.saturating_add(1);
    }
    end
}

/// RFC 8259 section 6: `-? (0 | [1-9][0-9]*) (. [0-9]+)? ([eE] [+-]? [0-9]+)?`.
fn grammar_accepts(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut at = usize::from(bytes.first() == Some(&b'-'));
    match bytes.get(at).copied() {
        Some(b'0') => at = at.saturating_add(1),
        Some(b'1'..=b'9') => at = after_digits(bytes, at),
        _ => return false,
    }
    if bytes.get(at) == Some(&b'.') {
        let end = after_digits(bytes, at.saturating_add(1));
        if end == at.saturating_add(1) {
            return false;
        }
        at = end;
    }
    if matches!(bytes.get(at), Some(&(b'e' | b'E'))) {
        at = at.saturating_add(1);
        if matches!(bytes.get(at), Some(&(b'+' | b'-'))) {
            at = at.saturating_add(1);
        }
        let end = after_digits(bytes, at);
        if end == at {
            return false;
        }
        at = end;
    }
    at == bytes.len()
}

/// Whether the standard library reads `text` as a finite float, which is the
/// facade's condition for a number that is not out of range.
fn finite(text: &str) -> bool {
    match text.parse::<f64>() {
        Ok(number) => number.is_finite(),
        Err(_) => false,
    }
}

/// The answer the grammar and the float range give for a document of one number
/// with optional surrounding whitespace.
fn number_reference(text: &str) -> bool {
    let bare = text.trim_matches(|ch| matches!(ch, ' ' | '\t' | '\n' | '\r'));
    grammar_accepts(bare) && finite(bare)
}

/// A number spelled as the grammar allows, of a drawn shape.
fn draw_number_text(attacker: &mut Attacker) -> String {
    let mut out = String::new();
    if attacker.below(4) == 0 {
        out.push('-');
    }
    if attacker.below(3) == 0 {
        out.push('0');
    } else {
        out.push(*attacker.pick(&DIGITS[1..]));
        for _ in 0..attacker.below(20) {
            out.push(*attacker.pick(&DIGITS));
        }
    }
    if attacker.below(2) == 0 {
        out.push('.');
        for _ in 0..attacker.below(10).saturating_add(1) {
            out.push(*attacker.pick(&DIGITS));
        }
    }
    if attacker.below(3) == 0 {
        out.push(*attacker.pick(&['e', 'E']));
        match attacker.below(3) {
            0 => out.push('+'),
            1 => out.push('-'),
            _ => {}
        }
        for _ in 0..attacker.below(4).saturating_add(1) {
            out.push(*attacker.pick(&DIGITS));
        }
    }
    out
}

/// What the facade made of `text` read as a string value, or nothing.
fn read_string(text: &str) -> Option<String> {
    from_str::<String>(text).ok()
}

/// The scalar a surrogate pair stands for (Unicode section 3.9, D75).
fn pair_scalar(high: u32, low: u32) -> u32 {
    0x1_0000_u32
        .saturating_add(high.saturating_sub(0xd800).saturating_mul(0x400))
        .saturating_add(low.saturating_sub(0xdc00))
}

/// Asserts that `input` is a literal's template exactly when the parser reads it.
fn only_the_template(template: &[u8], input: &[u8], position: usize, candidate: u8) {
    assert_eq!(
        from_slice::<Value>(input).is_ok(),
        input == template,
        "{candidate:#04x} at {position} of {:?}",
        as_text(template)
    );
}

/// Every finite float the attacker names prints to text that reads back to the
/// same bits, so a number crosses the wire without a rounding step.
#[test]
fn sim_adversary_json_floats_cross_the_wire_bit_for_bit() -> TestResult {
    sweep(4_096, |seed, round, attacker| {
        let number = f64::from_bits(attacker.word());
        if !number.is_finite() {
            return Ok(());
        }
        let text = to_string(&number)?;
        assert_eq!(
            from_str::<f64>(&text)?.to_bits(),
            number.to_bits(),
            "seed {seed:#x} round {round}: {text}"
        );
        Ok(())
    })
}

/// JSON has no `NaN` or `Infinity`: a non-finite float becomes `null` rather
/// than text no parser would read.
#[test]
fn sim_adversary_json_non_finite_floats_become_null() -> TestResult {
    for number in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -f64::NAN] {
        assert_eq!(to_string(&number)?, "null", "{number} must print as null");
        assert_eq!(Value::from(number), Value::Null, "{number} as a value");
    }
    Ok(())
}

/// Integers at the edges of 64 bits and of the 53 bits a double holds are exact
/// as text and as a typed value, and parse as the integers they are, not as
/// floats that lost the low bits (RFC 7493 section 2.2).
#[test]
fn sim_adversary_json_integer_boundaries_are_exact() -> TestResult {
    let unsigned = [
        0_u64,
        1,
        9_007_199_254_740_991,
        9_007_199_254_740_992,
        9_007_199_254_740_993,
        u64::MAX.saturating_sub(1),
        u64::MAX,
    ];
    for number in unsigned {
        let text = to_string(&number)?;
        assert_eq!(text, number.to_string(), "{number} as text");
        assert_eq!(from_str::<u64>(&text)?, number, "{number} typed");
        assert_eq!(
            from_str::<Value>(&text)?.as_u64(),
            Some(number),
            "{number} as a value"
        );
    }
    let signed = [
        i64::MIN,
        i64::MIN.saturating_add(1),
        -9_007_199_254_740_993,
        -1,
        i64::MAX,
    ];
    for number in signed {
        let text = to_string(&number)?;
        assert_eq!(text, number.to_string(), "{number} as text");
        assert_eq!(from_str::<i64>(&text)?, number, "{number} typed");
        assert_eq!(
            from_str::<Value>(&text)?.as_i64(),
            Some(number),
            "{number} as a value"
        );
    }
    Ok(())
}

/// A typed integer is read exactly or refused, never truncated: for decimal
/// text of any width, the facade agrees with the standard library about whether
/// it fits and about the value.
#[test]
fn sim_adversary_json_typed_integers_are_exact_or_refused() -> TestResult {
    sweep(4_096, |seed, round, attacker| {
        let mut digits = String::new();
        digits.push(*attacker.pick(&DIGITS[1..]));
        for _ in 0..attacker.below(24) {
            digits.push(*attacker.pick(&DIGITS));
        }
        let negative = format!("-{digits}");
        for text in [digits, negative] {
            assert_eq!(
                from_str::<u64>(&text).ok(),
                text.parse::<u64>().ok(),
                "seed {seed:#x} round {round}: u64 {text}"
            );
            assert_eq!(
                from_str::<i64>(&text).ok(),
                text.parse::<i64>().ok(),
                "seed {seed:#x} round {round}: i64 {text}"
            );
        }
        Ok(())
    })
}

/// The number grammar of RFC 8259 section 6 is the parser's: hostile numbers
/// from a table and mutated valid numbers are accepted exactly when the grammar
/// accepts them and they are finite.
#[test]
fn sim_adversary_json_numbers_follow_the_rfc_grammar() -> TestResult {
    for text in HOSTILE_NUMBERS {
        assert_eq!(
            accepts(text),
            number_reference(text),
            "table entry {text:?}"
        );
    }
    sweep(4_096, |seed, round, attacker| {
        let base = draw_number_text(attacker);
        assert_eq!(
            accepts(&base),
            number_reference(&base),
            "seed {seed:#x} round {round}: valid {base:?}"
        );
        let rounds = attacker.below(4);
        let hostile = as_text(&attacker.mutate_n(base.as_bytes(), rounds));
        assert_eq!(
            accepts(&hostile),
            number_reference(&hostile),
            "seed {seed:#x} round {round}: mutated {hostile:?}"
        );
        Ok(())
    })
}

/// `true`, `false` and `null` are spelled one way: any byte put at any position
/// of a literal gives a document the parser reads only if the byte changed
/// nothing.
#[test]
fn sim_adversary_json_literals_have_one_spelling() {
    every_byte_at_every_position(*b"true", |input, position, candidate| {
        only_the_template(b"true", &input, position, candidate);
    });
    every_byte_at_every_position(*b"null", |input, position, candidate| {
        only_the_template(b"null", &input, position, candidate);
    });
    every_byte_at_every_position(*b"false", |input, position, candidate| {
        only_the_template(b"false", &input, position, candidate);
    });
}

/// Around a value, every byte of the 256 is refused except the four of RFC 8259
/// section 2, before the value and after it.
#[test]
fn sim_adversary_json_only_four_bytes_are_whitespace() {
    for byte in 0..=u8::MAX {
        let allowed = WHITESPACE.contains(&byte);
        assert_eq!(
            from_slice::<Value>(&[byte, b'n', b'u', b'l', b'l']).is_ok(),
            allowed,
            "{byte:#04x} before"
        );
        assert_eq!(
            from_slice::<Value>(&[b'n', b'u', b'l', b'l', byte]).is_ok(),
            allowed,
            "{byte:#04x} after"
        );
    }
}

/// A byte-order mark and the spaces that only look like spaces are not
/// whitespace: RFC 8259 section 8.1 forbids a mark and section 2 names four
/// bytes.
#[test]
fn sim_adversary_json_a_mark_and_look_alike_spaces_are_refused() {
    for prefix in [
        "\u{feff}", "\u{a0}", "\u{2028}", "\u{2003}", "\u{3000}", "\u{b}", "\u{c}", "\u{85}",
    ] {
        let before = format!("{prefix}null");
        let after = format!("null{prefix}");
        assert!(!accepts(&before), "{prefix:?} before a value");
        assert!(!accepts(&after), "{prefix:?} after a value");
    }
    assert!(
        accepts(" \t\r\n null \t\r\n"),
        "the four real whitespace bytes in a mix"
    );
}

/// Every byte put inside a string is accepted only where RFC 8259 section 7
/// allows it raw: not a control character below 0x20, not a quote, not a
/// backslash, and not a lone byte of a multi-byte character.
#[test]
fn sim_adversary_json_raw_bytes_in_strings_follow_the_rfc() {
    for byte in 0..=u8::MAX {
        let allowed = (0x20..=0x7f).contains(&byte) && byte != b'"' && byte != b'\\';
        let document = [b'"', b'a', byte, b'c', b'"'];
        assert_eq!(
            from_slice::<String>(&document).is_ok(),
            allowed,
            "{byte:#04x} in a string"
        );
    }
}

/// After a backslash exactly eight letters are valid, each standing for its
/// character; every other byte, `u` without four digits included, is refused.
#[test]
fn sim_adversary_json_escape_letters_are_exactly_the_rfc_set() {
    for byte in 0..=u8::MAX {
        let document = [b'"', b'\\', byte, b'"'];
        let read = from_slice::<String>(&document).ok();
        let expected = ESCAPES
            .iter()
            .find(|entry| entry.0 == byte)
            .map(|entry| String::from(entry.1));
        assert_eq!(read, expected, "escape letter {byte:#04x}");
    }
}

/// In `\uXXXX` each of the four positions takes a hex digit of either case and
/// nothing else, whatever byte the attacker puts there.
#[test]
fn sim_adversary_json_unicode_escape_digits_are_hex_only() {
    every_byte_at_every_position(*b"\"\\u0041\"", |input, position, candidate| {
        if !(3..7).contains(&position) {
            return;
        }
        let read = from_slice::<String>(&input).ok();
        let Some(digit) = char::from(candidate).to_digit(16) else {
            assert_eq!(read, None, "{candidate:#04x} at digit {position}");
            return;
        };
        let mut digits = [0_u32, 0, 4, 1];
        digits[position.saturating_sub(3)] = digit;
        let code = digits.iter().fold(0_u32, |sum, &next| {
            sum.saturating_mul(16).saturating_add(next)
        });
        assert_eq!(
            read,
            char::from_u32(code).map(String::from),
            "{candidate:#04x} at digit {position}"
        );
    });
}

/// All 65,536 code units, spelled in lower and upper case: a unit that is a
/// scalar reads as that scalar, a surrogate on its own is refused (RFC 8259
/// section 8.2), and case never matters.
#[test]
fn sim_adversary_json_every_single_unicode_escape() {
    for code in 0..=0xffff_u32 {
        let lower = format!("\"\\u{code:04x}\"");
        let upper = format!("\"\\u{code:04X}\"");
        let expected = char::from_u32(code).map(String::from);
        assert_eq!(read_string(&lower), expected, "{lower}");
        assert_eq!(read_string(&upper), expected, "{upper}");
    }
}

/// All 1,048,576 surrogate pairs decode to the scalar Unicode section 3.9
/// assigns them, which is a character beyond the BMP.
#[test]
fn sim_adversary_json_every_surrogate_pair_decodes_to_its_scalar() {
    for high in 0xd800..=0xdbff_u32 {
        for low in 0xdc00..=0xdfff_u32 {
            let text = format!("\"\\u{high:04x}\\u{low:04x}\"");
            let expected = char::from_u32(pair_scalar(high, low)).map(String::from);
            assert_eq!(read_string(&text), expected, "{text}");
        }
    }
}

/// A surrogate half is refused alone, in the wrong order, before a unit that is
/// not its partner, before a plain character, before an escape, and at the end
/// of the string, for every high half and a spread of low halves.
#[test]
fn sim_adversary_json_lone_and_misordered_surrogates_are_refused() {
    for high in 0xd800..=0xdbff_u32 {
        for follower in ["", "a", "\\n", "\\u0041", "\\ud800", "\\uffff", "\\u0000"] {
            let text = format!("\"\\u{high:04x}{follower}\"");
            assert_eq!(read_string(&text), None, "high half then {follower:?}");
        }
    }
    for low in (0xdc00..=0xdfff_u32).step_by(7) {
        for high in (0xd800..=0xdbff_u32).step_by(31) {
            let reversed = format!("\"\\u{low:04x}\\u{high:04x}\"");
            assert_eq!(
                read_string(&reversed),
                None,
                "low half {low:04x} before high half {high:04x}"
            );
        }
    }
}

/// Raw bytes in a string are accepted as exactly the UTF-8 the standard library
/// accepts: every two-byte sequence, and mutated multi-byte text, with overlong
/// forms, surrogate encodings, and bytes past U+10FFFF all refused.
#[test]
fn sim_adversary_json_invalid_utf8_matches_the_standard_library() -> TestResult {
    let verdict = |raw: &[u8]| {
        if raw
            .iter()
            .any(|&byte| byte < 0x20 || byte == b'"' || byte == b'\\')
        {
            return None;
        }
        let mut document = vec![b'"'];
        document.extend_from_slice(raw);
        document.push(b'"');
        let expected = std::str::from_utf8(raw).ok().map(String::from);
        Some((from_slice::<String>(&document).ok(), expected))
    };
    for high in 0..=u8::MAX {
        for low in 0..=u8::MAX {
            if let Some((read, expected)) = verdict(&[high, low]) {
                assert_eq!(read, expected, "bytes {high:#04x} {low:#04x}");
            }
        }
    }
    sweep(2_048, |seed, round, attacker| {
        let base = draw_text(attacker, 8).replace(['\0', '\u{1}', '\u{1f}', '"', '\\'], "x");
        let hostile = attacker.mutate_n(base.as_bytes(), 3);
        if let Some((read, expected)) = verdict(&hostile) {
            assert_eq!(
                read, expected,
                "seed {seed:#x} round {round}: {hostile:02x?}"
            );
        }
        Ok(())
    })
}
