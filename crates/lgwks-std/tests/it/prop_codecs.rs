//! Property tests over the byte codecs, with shrinking (#273).
//!
//! The `sim_*` sweeps find a failing input; these reduce it to the smallest one
//! that still fails. Every property runs from one fixed seed, so a run replays
//! exactly, and a failure's seed is written under `proptest-regressions/` and
//! replayed first on every later run with no environment variable.
//!
//! Each property is a function over the codec under test, so the same property
//! runs twice: against the real codec, where it must hold, and against a
//! deliberately broken one, where it must fail and shrink to the input named
//! in the test. A property that cannot catch its mutant proves nothing.

#[path = "../support/prop.rs"]
mod prop;

use lgwks_std::encoding::{base64, percent};
use lgwks_std::{hex, leb128};
use prop::{Outcome, check};
use proptest::collection::vec;
use proptest::prelude::{Just, Strategy, any, prop_oneof};
use proptest::test_runner::{TestCaseError, TestRunner};
use std::fmt::Debug;

/// The seed every run starts from. Changing it is a new corpus, not a retry.
const SEED: u64 = 0x2730_c0de_c5ee_d001;

/// Cases per property: proptest's default, stated so the count is reviewable.
const CASES: u32 = 256;

/// A runner for this target's properties.
fn runner() -> TestRunner {
    prop::runner(SEED, CASES, file!())
}

/// The minimal input a mutant fails on, from this target's seed.
fn shrunk<S>(
    strategy: &S,
    property: impl Fn(S::Value) -> Result<(), TestCaseError>,
) -> Result<S::Value, Box<dyn std::error::Error>>
where
    S: Strategy,
    S::Value: Debug + 'static,
{
    prop::shrunk(SEED, strategy, property)
}

/// Bytes biased toward the characters a codec treats specially, so a refusal
/// path is reached as often as the accepting one.
fn text_from(alphabet: &'static [u8]) -> impl Strategy<Value = Vec<u8>> {
    let special = proptest::sample::select(alphabet);
    vec(prop_oneof![4 => special, 1 => any::<u8>()], 0..48)
}

// ── hex ─────────────────────────────────────────────────────────────────────

/// A hex decoder's signature.
type HexDecode = fn(&[u8]) -> Result<Vec<u8>, hex::DecodeError>;

/// The real decoder, as a [`HexDecode`].
fn hex_decode(input: &[u8]) -> Result<Vec<u8>, hex::DecodeError> {
    hex::decode(input)
}

/// What a strict hex decoder must answer, computed independently: an odd
/// length is refused as a whole, otherwise the first non-digit is named.
fn hex_oracle(input: &[u8]) -> Result<Vec<u8>, hex::DecodeError> {
    let first_alien = input
        .iter()
        .enumerate()
        .find(|&(_, byte)| !byte.is_ascii_hexdigit());
    match (input.len().is_multiple_of(2), first_alien) {
        (false, _) => Err(hex::DecodeError::OddLength {
            len: input.len(),
            at: input.len(),
        }),
        (true, Some((at, &byte))) => Err(hex::DecodeError::NotHexDigit { at, byte }),
        (true, None) => {
            let digits: Vec<u32> = input
                .iter()
                .filter_map(|&byte| char::from(byte).to_digit(16))
                .collect();
            let (pairs, _) = digits.as_chunks::<2>();
            Ok(pairs
                .iter()
                .filter_map(|&[high, low]| {
                    u8::try_from(high.saturating_mul(16).saturating_add(low)).ok()
                })
                .collect())
        }
    }
}

/// Agrees with [`hex_oracle`] on every input, refusals included, and an
/// accepted input re-encodes to its own lowercase form.
fn hex_property(decode: HexDecode, input: &[u8]) -> Result<(), TestCaseError> {
    let answer = decode(input);
    check(answer == hex_oracle(input), || {
        format!("{input:?}: {answer:?}, oracle {:?}", hex_oracle(input))
    })?;
    if let Ok(bytes) = answer {
        check(
            hex::encode(&bytes).into_bytes() == input.to_ascii_lowercase(),
            || format!("{input:?} does not re-encode to itself"),
        )?;
    }
    Ok(())
}

#[test]
fn hex_round_trips_every_byte_string() -> Outcome {
    runner().run(&vec(any::<u8>(), 0..256), |bytes| {
        let decoded = hex::decode(hex::encode(&bytes));
        check(decoded.as_ref() == Ok(&bytes), || {
            format!("{bytes:?} came back as {decoded:?}")
        })
    })?;
    Ok(())
}

#[test]
fn hex_refuses_exactly_what_the_oracle_refuses_at_the_same_offset() -> Outcome {
    runner().run(&text_from(b"0123456789abcdefABCDEF"), |input| {
        hex_property(hex_decode, &input)
    })?;
    Ok(())
}

#[test]
fn hex_property_catches_a_decoder_that_drops_a_half_byte() -> Outcome {
    /// Decodes the even prefix and silently drops a trailing nibble.
    fn lenient(input: &[u8]) -> Result<Vec<u8>, hex::DecodeError> {
        let even = input.len().saturating_sub(input.len() & 1);
        // `even` drops at most the trailing nibble, so the even prefix is a
        // slice of the input; an input with no even prefix is an odd-length
        // refusal, which is what the codec itself would answer.
        match input.get(..even) {
            Some(even) => hex::decode(even),
            None => Err(hex::DecodeError::OddLength {
                len: input.len(),
                at: input.len(),
            }),
        }
    }
    let minimal = shrunk(&text_from(b"0123456789abcdefABCDEF"), |input| {
        hex_property(lenient, &input)
    })?;
    // The shrinker removes one element at a time, and any one removal from an
    // odd input makes it even, which the mutant decodes correctly; so the
    // minimal is an odd run of the smallest digit, not necessarily `b"0"`.
    assert!(
        minimal.len() & 1 == 1 && minimal.iter().all(|&digit| digit == b'0'),
        "an odd run of the smallest digit, got {minimal:?}"
    );
    Ok(())
}

// ── base64 ──────────────────────────────────────────────────────────────────

/// A base64 decoder's signature.
type Base64Decode = fn(&[u8]) -> Result<Vec<u8>, base64::DecodeError>;

/// The real decoder, as a [`Base64Decode`].
fn base64_decode(input: &[u8]) -> Result<Vec<u8>, base64::DecodeError> {
    base64::decode(input)
}

/// A strict decoder is canonical: whatever it accepts re-encodes to exactly
/// the input, so no two strings decode to one value. A refusal names an
/// offset inside the input or just past it.
fn base64_property(decode: Base64Decode, input: &[u8]) -> Result<(), TestCaseError> {
    match decode(input) {
        Ok(bytes) => check(base64::encode(&bytes).as_bytes() == input, || {
            format!("{input:?} decoded to {bytes:?}, which encodes differently")
        }),
        Err(
            base64::DecodeError::BadLength { at, .. }
            | base64::DecodeError::NotInAlphabet { at, .. }
            | base64::DecodeError::MisplacedPadding { at },
        ) => check(at <= input.len(), || {
            format!("{input:?}: offset {at} is outside")
        }),
        Err(other) => Err(TestCaseError::fail(format!("unknown refusal {other:?}"))),
    }
}

/// The base64 alphabet, padding, and the URL-safe pair a strict decoder refuses.
const BASE64_TEXT: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=-_";

#[test]
fn base64_round_trips_every_byte_string() -> Outcome {
    runner().run(&vec(any::<u8>(), 0..256), |bytes| {
        let decoded = base64::decode(base64::encode(&bytes));
        check(decoded.as_ref() == Ok(&bytes), || {
            format!("{bytes:?} came back as {decoded:?}")
        })
    })?;
    Ok(())
}

#[test]
fn base64_accepts_only_canonical_text() -> Outcome {
    // Whole quanta, so most inputs reach the body decoder rather than stopping
    // at the length check.
    let quanta =
        vec(vec(proptest::sample::select(BASE64_TEXT), 4), 0..6).prop_map(|quanta| quanta.concat());
    runner().run(&quanta, |input| base64_property(base64_decode, &input))?;
    runner().run(&text_from(BASE64_TEXT), |input| {
        base64_property(base64_decode, &input)
    })?;
    Ok(())
}

#[test]
fn base64_property_catches_a_decoder_that_accepts_the_url_safe_alphabet() -> Outcome {
    /// Reads `-` and `_` as `+` and `/`, so two strings name one value.
    fn lenient(input: &[u8]) -> Result<Vec<u8>, base64::DecodeError> {
        let standard: Vec<u8> = input
            .iter()
            .map(|&byte| match byte {
                b'-' => b'+',
                b'_' => b'/',
                other => other,
            })
            .collect();
        base64::decode(standard)
    }
    let minimal = shrunk(&vec(proptest::sample::select(BASE64_TEXT), 4), |input| {
        base64_property(lenient, &input)
    })?;
    assert_eq!(
        minimal, b"AAA-",
        "the smallest quantum using the URL-safe alphabet"
    );
    Ok(())
}

// ── percent ─────────────────────────────────────────────────────────────────

/// A percent decoder's signature.
type PercentDecode = fn(&str) -> Result<String, percent::DecodeError>;

/// The real decoder, as a [`PercentDecode`].
fn percent_decode(input: &str) -> Result<String, percent::DecodeError> {
    percent::decode(input)
}

/// What a strict percent decoder must answer, computed independently.
fn percent_oracle(input: &str) -> Result<String, percent::DecodeError> {
    let mut bytes = input.bytes().enumerate();
    let mut out = Vec::with_capacity(input.len());
    while let Some((at, byte)) = bytes.next() {
        if byte != b'%' {
            out.push(byte);
            continue;
        }
        let high = bytes.next();
        let low = bytes.next();
        out.push(percent_escape(at, high, low)?);
    }
    String::from_utf8(out).map_err(|error| percent::DecodeError::NotUtf8 {
        decoded_at: error.utf8_error().valid_up_to(),
    })
}

/// The byte one `%XY` escape at `at` stands for, from the two (offset, byte)
/// pairs after the `%`, either of which may be missing.
fn percent_escape(
    at: usize,
    high: Option<(usize, u8)>,
    low: Option<(usize, u8)>,
) -> Result<u8, percent::DecodeError> {
    match (high, low) {
        (Some((high_at, high)), Some((low_at, low))) => {
            let high = char::from(high)
                .to_digit(16)
                .ok_or(percent::DecodeError::NotHexDigit { at: high_at })?;
            let low = char::from(low)
                .to_digit(16)
                .ok_or(percent::DecodeError::NotHexDigit { at: low_at })?;
            // Two hex digits are at most 0xff. The narrowing is checked anyway,
            // and its refusal names the digit rather than substituting a byte,
            // because a value above 0xff is not a byte this reference produced.
            match u8::try_from(high.saturating_mul(16).saturating_add(low)) {
                Ok(packed) => Ok(packed),
                Err(_) => Err(percent::DecodeError::NotHexDigit { at: high_at }),
            }
        }
        _ => Err(percent::DecodeError::TruncatedEscape { at }),
    }
}

/// Agrees with [`percent_oracle`] on every input, refusals included.
fn percent_property(decode: PercentDecode, input: &str) -> Result<(), TestCaseError> {
    let answer = decode(input);
    check(answer == percent_oracle(input), || {
        format!("{input:?}: {answer:?}, oracle {:?}", percent_oracle(input))
    })
}

/// Text dense in escapes, digits and the bytes after them.
fn percent_text() -> impl Strategy<Value = String> {
    let piece = prop_oneof![
        3 => proptest::sample::select(&["%", "%2", "%41", "%e9", "%C3%A9", "%zz", "%%"][..]).prop_map(str::to_owned),
        2 => any::<char>().prop_map(String::from),
        1 => Just(String::from("a/b?c")),
    ];
    vec(piece, 0..16).prop_map(|pieces| pieces.concat())
}

#[test]
fn percent_round_trips_every_string() -> Outcome {
    runner().run(&any::<String>(), |text| {
        let encoded = percent::encode_component(&text);
        let decoded = percent::decode(&encoded);
        check(decoded.as_ref() == Ok(&text), || {
            format!("{text:?} came back as {decoded:?}")
        })?;
        check(
            encoded
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.~%".contains(&byte)),
            || format!("{encoded:?} carries a reserved byte"),
        )
    })?;
    Ok(())
}

#[test]
fn percent_refuses_exactly_what_the_oracle_refuses_at_the_same_offset() -> Outcome {
    runner().run(&percent_text(), |input| {
        percent_property(percent_decode, &input)
    })?;
    Ok(())
}

#[test]
fn percent_property_catches_a_decoder_that_keeps_a_truncated_escape() -> Outcome {
    /// Copies a `%` without two characters after it as a literal.
    fn lenient(input: &str) -> Result<String, percent::DecodeError> {
        match percent::decode(input) {
            Err(percent::DecodeError::TruncatedEscape { .. }) => Ok(input.to_owned()),
            other => other,
        }
    }
    let minimal = shrunk(&percent_text(), |input| percent_property(lenient, &input))?;
    assert_eq!(minimal, "%", "the smallest truncated escape");
    Ok(())
}

// ── leb128 ──────────────────────────────────────────────────────────────────

/// An LEB128 encoder and decoder for one integer width.
#[derive(Clone, Copy)]
struct Width<T: 'static> {
    /// Appends the encoding of a value.
    encode: fn(T, &mut Vec<u8>),
    /// Decodes a prefix, answering the value and the bytes it used.
    decode: Leb128Decode<T>,
}

/// An LEB128 prefix decoder: the value and the bytes it used.
type Leb128Decode<T> = fn(&[u8]) -> Result<(T, usize), leb128::DecodeError>;

/// Decoding an encoding answers the value and exactly its length, and leaves
/// whatever follows unread.
fn leb128_round_trip<T: Copy + PartialEq + Debug>(
    width: Width<T>,
    value: T,
    trailing: &[u8],
) -> Result<(), TestCaseError> {
    let mut bytes = Vec::new();
    (width.encode)(value, &mut bytes);
    let length = bytes.len();
    bytes.extend_from_slice(trailing);
    let decoded = (width.decode)(&bytes);
    check(decoded == Ok((value, length)), || {
        format!("{value:?} with {trailing:?} after it decoded as {decoded:?}")
    })
}

/// Minimal encoding is unique: an accepted prefix is exactly the encoding of
/// the value it names.
fn leb128_canonical<T: Copy + PartialEq + Debug>(
    width: Width<T>,
    input: &[u8],
) -> Result<(), TestCaseError> {
    let Ok((value, used)) = (width.decode)(input) else {
        return Ok(());
    };
    let mut again = Vec::new();
    (width.encode)(value, &mut again);
    check(input.get(..used) == Some(again.as_slice()), || {
        format!("{input:?} was accepted as {value:?}, whose encoding is {again:?}")
    })
}

/// Continuation-heavy bytes, so long and padded encodings are common.
fn leb128_text() -> impl Strategy<Value = Vec<u8>> {
    let byte = prop_oneof![
        3 => proptest::sample::select(&[0x80u8, 0xff, 0x7f, 0x00, 0x01, 0x40, 0x3f, 0x0f, 0x78][..]),
        1 => any::<u8>(),
    ];
    vec(byte, 0..12)
}

#[test]
fn leb128_round_trips_every_value_of_every_width() -> Outcome {
    let trailing = || vec(any::<u8>(), 0..4);
    let u64s = Width {
        encode: leb128::encode_u64,
        decode: leb128::decode_u64,
    };
    let u32s = Width {
        encode: leb128::encode_u32,
        decode: leb128::decode_u32,
    };
    let i64s = Width {
        encode: leb128::encode_i64,
        decode: leb128::decode_i64,
    };
    let i32s = Width {
        encode: leb128::encode_i32,
        decode: leb128::decode_i32,
    };
    runner().run(&(any::<u64>(), trailing()), |(value, rest)| {
        leb128_round_trip(u64s, value, &rest)
    })?;
    runner().run(&(any::<u32>(), trailing()), |(value, rest)| {
        leb128_round_trip(u32s, value, &rest)
    })?;
    runner().run(&(any::<i64>(), trailing()), |(value, rest)| {
        leb128_round_trip(i64s, value, &rest)
    })?;
    runner().run(&(any::<i32>(), trailing()), |(value, rest)| {
        leb128_round_trip(i32s, value, &rest)
    })?;
    Ok(())
}

#[test]
fn leb128_accepts_only_the_minimal_encoding() -> Outcome {
    runner().run(&leb128_text(), |input| {
        leb128_canonical(
            Width {
                encode: leb128::encode_u64,
                decode: leb128::decode_u64,
            },
            &input,
        )?;
        leb128_canonical(
            Width {
                encode: leb128::encode_u32,
                decode: leb128::decode_u32,
            },
            &input,
        )?;
        leb128_canonical(
            Width {
                encode: leb128::encode_i64,
                decode: leb128::decode_i64,
            },
            &input,
        )?;
        leb128_canonical(
            Width {
                encode: leb128::encode_i32,
                decode: leb128::decode_i32,
            },
            &input,
        )
    })?;
    Ok(())
}

#[test]
fn leb128_property_catches_a_decoder_that_accepts_padding() -> Outcome {
    /// A textbook unsigned decoder with no minimality check.
    fn lenient(input: &[u8]) -> Result<(u32, usize), leb128::DecodeError> {
        let mut value = 0u32;
        // Five groups is the whole 32-bit width, so the shift is counted in
        // groups and never reaches the width a shift by itself could overflow.
        let mut shift = 0u32;
        for (index, &byte) in input.iter().enumerate().take(5) {
            value |= u32::from(byte & 0x7f) << shift;
            shift = shift.saturating_add(7);
            if byte & 0x80 == 0 {
                return Ok((value, index.saturating_add(1)));
            }
        }
        Err(leb128::DecodeError::UnexpectedEnd { at: input.len() })
    }
    let width = Width {
        encode: leb128::encode_u32,
        decode: lenient,
    };
    let minimal = shrunk(&leb128_text(), |input| leb128_canonical(width, &input))?;
    assert_eq!(
        minimal,
        [0x80, 0x00],
        "the smallest padded encoding of zero"
    );
    Ok(())
}

// ── time ────────────────────────────────────────────────────────────────────

/// Seconds from 0000-01-01T00:00:00Z through 9999-12-31T23:59:59Z, the span
/// a four-digit RFC 3339 year can name.
const RFC3339_SPAN: std::ops::RangeInclusive<i64> = -62_167_219_200..=253_402_300_799;

/// A renderer's signature.
type Render = fn(std::time::SystemTime) -> Result<String, lgwks_std::time::FormatError>;

/// Every instant in the span renders, and the rendering parses back to the
/// same instant to the nanosecond.
fn time_property(render: Render, secs: i64, nanos: u32) -> Result<(), TestCaseError> {
    use lgwks_std::time::format::from_unix_parts;
    let instant = from_unix_parts(secs, nanos)
        .map_err(|error| TestCaseError::fail(format!("{secs}.{nanos}: {error}")))?;
    let text =
        render(instant).map_err(|error| TestCaseError::fail(format!("{secs}.{nanos}: {error}")))?;
    let back = lgwks_std::time::parse_rfc3339(&text);
    check(back.as_ref() == Ok(&instant), || {
        format!("{secs}.{nanos:09} rendered {text:?}, which parsed as {back:?}")
    })
}

#[test]
fn rfc3339_round_trips_every_instant_with_a_four_digit_year() -> Outcome {
    runner().run(&(RFC3339_SPAN, 0u32..1_000_000_000), |(secs, nanos)| {
        time_property(lgwks_std::time::format::to_rfc3339, secs, nanos)
    })?;
    Ok(())
}

#[test]
fn rfc3339_property_catches_a_renderer_that_rounds_to_milliseconds() -> Outcome {
    /// Renders at millisecond precision, the common shortcut.
    fn coarse(at: std::time::SystemTime) -> Result<String, lgwks_std::time::FormatError> {
        use lgwks_std::time::format::{from_unix_parts, to_rfc3339, unix_parts};
        let Ok((secs, nanos)) = unix_parts(at) else {
            return to_rfc3339(at);
        };
        let sub_milli = nanos.rem_euclid(1_000_000);
        from_unix_parts(secs, nanos.saturating_sub(sub_milli))
            .map_or_else(|_| to_rfc3339(at), to_rfc3339)
    }
    let minimal = shrunk(&(RFC3339_SPAN, 0u32..1_000_000_000), |(secs, nanos)| {
        time_property(coarse, secs, nanos)
    })?;
    assert_eq!(minimal.1, 1, "the smallest sub-millisecond fraction");
    Ok(())
}

#[test]
fn rfc3339_text_that_parses_renders_back_to_an_instant_that_parses_identically() -> Outcome {
    // Arbitrary edits of valid timestamps: offsets, fractions, lowercase
    // separators, leap seconds, out-of-range fields. Whatever is accepted must
    // be a fixed point of render-then-parse.
    let piece = prop_oneof![
        Just("1970-01-01T00:00:00Z"),
        Just("2000-02-29T23:59:60Z"),
        Just("1999-12-31t23:59:59.999999999+14:00"),
        Just("0000-01-01T00:00:00-23:59"),
        Just("9999-12-31T23:59:59.1Z"),
        Just("2024-13-01T00:00:00Z"),
    ];
    let edits = (piece, vec((0usize..40, any::<u8>()), 0..3));
    runner().run(&edits, |(base, edits)| {
        let mut bytes = base.as_bytes().to_vec();
        for (at, byte) in edits {
            if let Some(slot) = bytes.get_mut(at) {
                *slot = byte;
            }
        }
        let Ok(text) = String::from_utf8(bytes) else {
            return Ok(());
        };
        let Ok(instant) = lgwks_std::time::parse_rfc3339(&text) else {
            return Ok(());
        };
        let rendered = lgwks_std::time::format::to_rfc3339(instant).map_err(|error| {
            TestCaseError::fail(format!("{text:?} parsed but cannot render: {error}"))
        })?;
        let again = lgwks_std::time::parse_rfc3339(&rendered);
        check(again.as_ref() == Ok(&instant), || {
            format!("{text:?} -> {rendered:?} -> {again:?}")
        })
    })?;
    Ok(())
}

// ── wire ────────────────────────────────────────────────────────────────────

/// A record shape with variable-length parts, archived through the facade.
#[cfg(feature = "wire")]
type Record = Vec<(u64, String, Vec<u8>)>;

/// An archive round trip's signature.
#[cfg(feature = "wire")]
type RoundTrip = fn(&Record) -> Result<Record, lgwks_std::wire::WireError>;

/// The facade's own round trip.
#[cfg(feature = "wire")]
fn wire_round_trip(record: &Record) -> Result<Record, lgwks_std::wire::WireError> {
    use lgwks_std::wire::{WireError, from_bytes, to_bytes};
    let bytes = to_bytes::<WireError>(record)?;
    from_bytes::<Record, WireError>(&bytes)
}

/// Archiving and restoring gives the value back.
#[cfg(feature = "wire")]
fn wire_property(round_trip: RoundTrip, record: &Record) -> Result<(), TestCaseError> {
    let back = round_trip(record);
    check(matches!(back, Ok(ref value) if value == record), || {
        format!("{record:?} came back as {back:?}")
    })
}

/// Records up to sixteen rows.
#[cfg(feature = "wire")]
fn records() -> impl Strategy<Value = Record> {
    vec(
        (any::<u64>(), any::<String>(), vec(any::<u8>(), 0..32)),
        0..16,
    )
}

#[cfg(feature = "wire")]
#[test]
fn wire_round_trips_every_record() -> Outcome {
    runner().run(&records(), |record| wire_property(wire_round_trip, &record))?;
    Ok(())
}

#[cfg(feature = "wire")]
#[test]
fn wire_checked_access_refuses_or_accepts_corrupt_bytes_without_crashing() -> Outcome {
    use lgwks_std::wire::{AlignedVec, Archive, WireError, access, from_bytes, to_bytes};
    runner().run(
        &(records(), vec((any::<usize>(), any::<u8>()), 1..4)),
        |(record, flips)| {
            let bytes = to_bytes::<WireError>(&record).map_err(|error| {
                TestCaseError::fail(format!("{record:?} did not archive: {error}"))
            })?;
            let mut corrupt: AlignedVec = AlignedVec::new();
            corrupt.extend_from_slice(&bytes);
            let len = corrupt.len().max(1);
            for (at, byte) in flips {
                if let Some(slot) = corrupt.get_mut(at.rem_euclid(len)) {
                    *slot ^= byte;
                }
            }
            // Checked access either refuses or yields an archive that restores.
            if access::<<Record as Archive>::Archived, WireError>(&corrupt).is_ok() {
                check(from_bytes::<Record, WireError>(&corrupt).is_ok(), || {
                    String::from("checked access accepted bytes that do not restore")
                })?;
            }
            Ok(())
        },
    )?;
    Ok(())
}

#[cfg(feature = "wire")]
#[test]
fn wire_property_catches_an_archive_that_drops_the_last_row() -> Outcome {
    /// Archives every row but the last.
    fn lossy(record: &Record) -> Result<Record, lgwks_std::wire::WireError> {
        let kept: Record = record
            .iter()
            .take(record.len().saturating_sub(1))
            .cloned()
            .collect();
        wire_round_trip(&kept)
    }
    let minimal = shrunk(&records(), |record| wire_property(lossy, &record))?;
    assert_eq!(
        minimal,
        vec![(0, String::new(), Vec::new())],
        "the smallest non-empty record"
    );
    Ok(())
}

// ── glob ────────────────────────────────────────────────────────────────────

/// One token of the strict glob grammar, as the oracle reads it.
#[cfg(feature = "pattern")]
#[derive(Debug, Clone)]
enum Atom {
    /// A literal scalar.
    Literal(char),
    /// `?`.
    Question,
    /// `*`.
    Star,
    /// `[ab]`, `[!ab]` or `[a-b]`, as written.
    Class(&'static str),
}

/// A strict-dialect pattern: components joined by `/`, each either `**` or a
/// run of atoms.
#[cfg(feature = "pattern")]
fn glob_patterns() -> impl Strategy<Value = String> {
    let atom = prop_oneof![
        3 => proptest::sample::select(&['a', 'b', '.', 'é'][..]).prop_map(Atom::Literal),
        1 => Just(Atom::Question),
        2 => Just(Atom::Star),
        1 => proptest::sample::select(&["[ab]", "[!a]", "[a-b]", "[^b]"][..]).prop_map(Atom::Class),
    ];
    let component = prop_oneof![
        1 => Just(String::from("**")),
        4 => vec(atom, 0..4).prop_map(|atoms| {
            atoms
                .iter()
                .map(|atom| match *atom {
                    Atom::Literal(literal) => String::from(literal),
                    Atom::Question => String::from("?"),
                    Atom::Star => String::from("*"),
                    Atom::Class(class) => String::from(class),
                })
                .collect::<String>()
        }),
    ];
    vec(component, 1..5)
        .prop_map(|components| components.join("/"))
        .prop_filter("a strict pattern", |pattern| {
            lgwks_std::glob::GlobPattern::compile(pattern).is_ok()
        })
}

/// Paths over the same scalars, `/` included.
#[cfg(feature = "pattern")]
fn glob_paths() -> impl Strategy<Value = String> {
    vec(
        proptest::sample::select(&['a', 'b', '.', 'é', '/'][..]),
        0..12,
    )
    .prop_map(|scalars| scalars.into_iter().collect())
}

/// The glob as a regular expression for the `pattern` engine, which shares no
/// code with the glob automaton. The token split follows the grammar (the
/// longest `/**/`-family token first); the matching is the regex engine's.
#[cfg(feature = "pattern")]
fn glob_as_regex(pattern: &str) -> String {
    let mut out = String::from(r"(?s)\A");
    let mut rest = pattern;
    while !rest.is_empty() {
        let (piece, used): (String, usize) = if rest.starts_with("/**/") {
            (String::from("/(?:.*/)?"), 4)
        } else if rest.starts_with("/**") {
            (String::from("(?:/.*)?"), 3)
        } else if rest.starts_with("**/") {
            (String::from("(?:.*/)?"), 3)
        } else if rest.starts_with("**") {
            (String::from(".*"), 2)
        } else if rest.starts_with('*') {
            (String::from("[^/]*"), 1)
        } else if rest.starts_with('?') {
            (String::from("[^/]"), 1)
        } else if rest.starts_with('[') {
            // A close bracket at or after the opener ends the class; a token
            // with none runs to its end, which is how a glob spells one.
            let close = match rest.find(']') {
                Some(close) => close,
                None => rest.len(),
            };
            // The opener is at index 0 and a `]` is at index 1 or later, so
            // `close` is at least 1 and the body is the span between them: `[]`
            // and a bare `[` both spell the empty class, which this is.
            let body = &rest[1..close];
            let (negated, members) = match body.strip_prefix(['!', '^']) {
                Some(members) => (true, members),
                None => (false, body),
            };
            let members: String = members
                .chars()
                .map(|member| match member {
                    '-' => String::from("-"),
                    other => format!(r"\x{{{:x}}}", u32::from(other)),
                })
                .collect();
            let class = if negated {
                format!("[^/{members}]")
            } else {
                format!("[{members}&&[^/]]")
            };
            (class, close.saturating_add(1))
        } else {
            // The loop condition leaves a non-empty remainder, so the token has
            // a first scalar to escape; an empty remainder ends the pattern.
            let Some(literal) = rest.chars().next() else {
                break;
            };
            (
                format!(r"\x{{{:x}}}", u32::from(literal)),
                literal.len_utf8(),
            )
        };
        out.push_str(&piece);
        // A translator that consumed past the end of the token leaves no
        // remainder, and the pattern ends there rather than restarting.
        let Some(tail) = rest.get(used..) else {
            break;
        };
        rest = tail;
    }
    out.push_str(r"\z");
    out
}

/// A glob matcher under test: `None` when it refuses the pattern.
#[cfg(feature = "pattern")]
type Matcher = fn(&str, &str) -> Option<bool>;

/// The shipped strict matcher, through a reused scratch.
#[cfg(feature = "pattern")]
fn strict_glob(pattern: &str, path: &str) -> Option<bool> {
    let compiled = lgwks_std::glob::GlobPattern::compile(pattern).ok()?;
    let mut scratch = lgwks_std::glob::GlobScratch::new();
    Some(compiled.is_match_with(path, &mut scratch))
}

/// The matcher agrees with the regex oracle on every generated pair, and a
/// match's scratch stays linear in the path whatever the pattern.
#[cfg(feature = "pattern")]
fn glob_property(matcher: Matcher, pattern: &str, path: &str) -> Result<(), TestCaseError> {
    let oracle = lgwks_std::pattern::Regex::new(&glob_as_regex(pattern))
        .map_err(|error| TestCaseError::fail(format!("oracle for {pattern:?}: {error}")))?;
    let expected = oracle.is_match(path);
    let answer = matcher(pattern, path);
    check(answer == Some(expected), || {
        format!("{pattern:?} on {path:?}: {answer:?}, oracle {expected}")
    })?;
    if let Ok(compiled) = lgwks_std::glob::GlobPattern::compile(pattern) {
        let mut scratch = lgwks_std::glob::GlobScratch::new();
        let rerun = compiled.is_match_with(path, &mut scratch);
        check(rerun == expected, || {
            format!("{pattern:?} on {path:?}: a reused scratch answered {rerun}")
        })?;
        let scalars = path.chars().count().saturating_add(1);
        check(
            scratch.row_capacity() <= scalars.saturating_mul(2).saturating_add(8),
            || {
                format!(
                    "{pattern:?} on {path:?}: rows grew to {}",
                    scratch.row_capacity()
                )
            },
        )?;
    }
    Ok(())
}

#[cfg(feature = "pattern")]
#[test]
fn glob_agrees_with_a_regex_oracle_on_generated_patterns() -> Outcome {
    runner().run(&(glob_patterns(), glob_paths()), |(pattern, path)| {
        glob_property(strict_glob, &pattern, &path)
    })?;
    Ok(())
}

#[cfg(feature = "pattern")]
#[test]
fn glob_property_catches_a_matcher_that_lets_star_cross_a_slash() -> Outcome {
    /// Compiles every lone `*` as `**`'s reach: the classic glob bug.
    fn crossing(pattern: &str, path: &str) -> Option<bool> {
        let star_free = pattern.replace("**", "\u{0}").replace('*', "\u{1}");
        let regex = glob_as_regex(&star_free.replace('\u{0}', "**")).replace(r"\x{1}", ".*");
        let oracle = lgwks_std::pattern::Regex::new(&regex).ok()?;
        Some(oracle.is_match(path))
    }
    let minimal = shrunk(&(glob_patterns(), glob_paths()), |(pattern, path)| {
        glob_property(crossing, &pattern, &path)
    })?;
    assert_eq!(
        minimal,
        (String::from("*"), String::from("/")),
        "the smallest crossing"
    );
    Ok(())
}
