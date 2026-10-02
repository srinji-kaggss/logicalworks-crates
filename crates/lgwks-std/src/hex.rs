//! `hex` owns lowercase base-16 transcoding and enforces INV-HEX-ROUNDTRIP:
//! `decode(encode(b)) == b` for every byte slice, and `decode` refuses any input
//! that is not an even-length run of ASCII hex digits.

use std::error::Error;
use std::fmt;

// ── Encoding ────────────────────────────────────────────────────────────────

/// The lowercase base-16 digit set, indexed by nibble value: the byte at index
/// `nibble` is the ASCII character that represents it (`LOWER[0x0a] == b'a'`).
/// Indexing is therefore always in bounds for `nibble <= 0x0f`, which is what
/// the masked nibbles below guarantee.
const LOWER: &[u8; 16] = b"0123456789abcdef";

/// Encodes bytes as lowercase hex, two characters per input byte.
pub fn encode(bytes: impl AsRef<[u8]>) -> String {
    let bytes = bytes.as_ref();
    // `len * 2` cannot overflow on a 64-bit target (a slice length is at most
    // `isize::MAX`), and on a 32-bit target it can only be reached by a slice
    // within one byte of `isize::MAX`. This value is a capacity hint, never
    // observable behaviour, so saturating is the correct choice: the worst case
    // is a reservation that either succeeds early or is corrected by `push`.
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for &byte in bytes {
        // The high nibble is the top four bits; `char::from` on a `u8` is the
        // lossless, panic-free widening that the lint-required `as` removal
        // demands, and every byte in `LOWER` is ASCII.
        out.push(char::from(LOWER[usize::from(byte >> 4)]));
        out.push(char::from(LOWER[usize::from(byte & 0x0f)]));
    }
    out
}

// ── Decoding ────────────────────────────────────────────────────────────────

/// Why a hex string could not be decoded. Variant names are stable and
/// machine-readable; callers match on them rather than on message text.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeError {
    /// The input had an odd number of characters, so some byte is half-written.
    OddLength {
        /// Length of the offending input, in bytes.
        len: usize,
        /// Offset where error occurred.
        at: usize,
    },
    /// A character outside `[0-9a-fA-F]` appeared at this byte offset.
    NotHexDigit {
        /// Zero-based offset of the offending character.
        at: usize,
        /// The offending byte, reported verbatim for diagnosis.
        byte: u8,
    },
    /// The destination slice did not have exactly one byte per input pair.
    OutputLength {
        /// Number of decoded bytes required by the input.
        expected: usize,
        /// Number of bytes in the caller's destination.
        actual: usize,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::OddLength { len, .. } => {
                write!(
                    f,
                    "hex input has odd length {len}; every byte needs two characters"
                )
            }
            Self::NotHexDigit { at, byte } => {
                write!(f, "byte {byte:#04x} at offset {at} is not a hex digit")
            }
            Self::OutputLength { expected, actual } => write!(
                f,
                "hex output has length {actual}; exactly {expected} bytes are required"
            ),
        }
    }
}

impl Error for DecodeError {}

/// Rejects an odd input length before any decoding work begins.
///
/// INV-HEX-ROUNDTRIP depends on this: a half-written final byte must be an
/// error rather than a silently dropped nibble. The error reports `len` as both
/// the offending length and the offset, because the failure is a property of
/// the whole input rather than of one character.
///
/// Returns [`DecodeError::OddLength`] for an odd `len`, otherwise `Ok(())`.
fn check_even_length(len: usize) -> Result<(), DecodeError> {
    if !len.is_multiple_of(2) {
        Err(DecodeError::OddLength { len, at: len })
    } else {
        Ok(())
    }
}

/// Decodes one two-character hex pair into the byte it represents.
///
/// # Panics
///
/// Panics if `pair` has fewer than two bytes. Every caller passes a
/// `chunks(2)` slice of an even-length input, so both elements exist.
///
/// Returns [`DecodeError::NotHexDigit`] naming the first of the two characters
/// that is not a hex digit; the offset is absolute, not pair-relative, so a
/// caller can point at the offending character in the original input.
fn decode_pair(pair: &[u8], offset: usize) -> Result<u8, DecodeError> {
    let hi = decode_nibble(pair[0]).ok_or(DecodeError::NotHexDigit {
        at: offset,
        byte: pair[0],
    })?;
    let lo = decode_nibble(pair[1]).ok_or(DecodeError::NotHexDigit {
        at: offset.saturating_add(1),
        byte: pair[1],
    })?;
    Ok((hi << 4) | lo)
}

/// Decodes a hex string into bytes. Uppercase digits are accepted, matching the
/// upstream `hex` crate this module retires.
pub fn decode(input: impl AsRef<[u8]>) -> Result<Vec<u8>, DecodeError> {
    let input = input.as_ref();
    check_even_length(input.len())?;
    // `check_even_length` proved the length even, so the halving is exact.
    // `checked_div` is used only because this crate forbids the `/` operator on
    // integers; the `None` arm (a zero divisor) is unreachable for a constant 2.
    let pair_count = input.len().checked_div(2).unwrap_or(0);
    let mut out = Vec::with_capacity(pair_count);
    let (pairs, _) = input.as_chunks::<2>();
    for (pair_index, pair) in pairs.iter().enumerate() {
        let offset = pair_index.saturating_mul(2);
        out.push(decode_pair(pair, offset)?);
    }
    Ok(out)
}

/// Decodes an even-length hex string into an exactly sized caller-owned slice.
///
/// The input length is checked first, then the destination length, then every
/// digit. The destination is written only after all validation succeeds, so any
/// error leaves every destination byte unchanged. Uppercase and lowercase
/// hexadecimal digits are accepted.
///
/// ```rust
/// let mut bytes = [0; 2];
/// lgwks_std::hex::decode_into("Af09", &mut bytes)?;
/// assert_eq!(bytes, [0xaf, 0x09]);
/// # Ok::<(), lgwks_std::hex::DecodeError>(())
/// ```
pub fn decode_into(input: impl AsRef<[u8]>, output: &mut [u8]) -> Result<(), DecodeError> {
    // The zero-dependency build has no logging stack, and the crate
    // doc says so; the emission is the same refusal either way.
    #[cfg(feature = "trace")]
    crate::trace::warn!(
        operation = "decode_into",
        "operation refused its request; the typed error carries the facts"
    );
    let input = input.as_ref();
    check_even_length(input.len())?;
    let expected = input.len().checked_div(2).unwrap_or(0);
    if output.len() != expected {
        return Err(DecodeError::OutputLength {
            expected,
            actual: output.len(),
        });
    }
    let (pairs, _) = input.as_chunks::<2>();
    for (pair_index, pair) in pairs.iter().enumerate() {
        let offset = pair_index.saturating_mul(2);
        decode_pair(pair, offset)?;
    }
    for (pair_index, (pair, destination)) in pairs.iter().zip(output.iter_mut()).enumerate() {
        let offset = pair_index.saturating_mul(2);
        *destination = decode_pair(pair, offset)?;
    }
    Ok(())
}

/// Writes lowercase hexadecimal bytes to an existing formatter without
/// allocating an intermediate string.
#[cfg(any(feature = "random", feature = "hash"))]
pub(crate) fn write_lowercase(bytes: &[u8], output: &mut impl fmt::Write) -> fmt::Result {
    for &byte in bytes {
        output.write_char(char::from(LOWER[usize::from(byte >> 4)]))?;
        output.write_char(char::from(LOWER[usize::from(byte & 0x0f)]))?;
    }
    Ok(())
}

/// Maps one ASCII character to the nibble it denotes, in `0..=0x0f`.
///
/// Accepts `0-9`, `a-f` and `A-F`; every other byte (including the non-ASCII
/// range) returns `None` so the caller can report the exact offset and byte.
/// The three subtractions are guarded by their own match arm, so each is a
/// saturating subtraction from a value known to be at or above its base.
pub(crate) fn decode_nibble(ascii_byte: u8) -> Option<u8> {
    match ascii_byte {
        b'0'..=b'9' => Some(ascii_byte.saturating_sub(b'0')),
        b'a'..=b'f' => Some(ascii_byte.saturating_sub(b'a').saturating_add(10)),
        b'A'..=b'F' => Some(ascii_byte.saturating_sub(b'A').saturating_add(10)),
        _ => None,
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_emits_two_lowercase_characters_per_byte() {
        assert_eq!(encode([]), "");
        assert_eq!(encode([0x00]), "00");
        assert_eq!(encode([0x0f]), "0f");
        assert_eq!(encode([0xf0]), "f0");
        assert_eq!(encode([0xff]), "ff");
        assert_eq!(encode(b"hello"), "68656c6c6f");
    }

    // These tests return `Result` rather than unwrapping: a hex refusal reports
    // its own `Debug` on failure, which is the same report `.unwrap` would have
    // panicked with, without an `unwrap` in the tree.
    #[test]
    fn decode_accepts_uppercase_digits() -> Result<(), DecodeError> {
        assert_eq!(decode("48454C4C4F")?, b"HELLO");
        assert_eq!(decode("48454c4c6f")?, b"HELLo");
        Ok(())
    }

    #[test]
    fn decode_refuses_odd_length() {
        assert_eq!(decode("abc"), Err(DecodeError::OddLength { len: 3, at: 3 }));
        assert_eq!(decode("a"), Err(DecodeError::OddLength { len: 1, at: 1 }));
    }

    #[test]
    fn decode_names_the_offending_offset() {
        assert_eq!(
            decode("00zz"),
            Err(DecodeError::NotHexDigit { at: 2, byte: b'z' })
        );
        assert_eq!(
            decode("000z"),
            Err(DecodeError::NotHexDigit { at: 3, byte: b'z' })
        );
        assert_eq!(
            decode("g0"),
            Err(DecodeError::NotHexDigit { at: 0, byte: b'g' })
        );
    }

    #[test]
    fn decode_into_validates_exact_length_and_preserves_output_on_failure() {
        let mut short = [0xa5; 1];
        assert_eq!(
            decode_into("aabb", &mut short),
            Err(DecodeError::OutputLength {
                expected: 2,
                actual: 1
            }),
            "short destination is refused"
        );
        assert_eq!(short, [0xa5], "short destination remains untouched");

        let mut long = [0xa5; 3];
        assert_eq!(
            decode_into("aabb", &mut long),
            Err(DecodeError::OutputLength {
                expected: 2,
                actual: 3
            }),
            "long destination is refused"
        );
        assert_eq!(long, [0xa5; 3], "long destination remains untouched");

        let mut invalid = [0xa5; 2];
        assert_eq!(
            decode_into("aabz", &mut invalid),
            Err(DecodeError::NotHexDigit { at: 3, byte: b'z' }),
            "invalid digit identifies its source byte"
        );
        assert_eq!(invalid, [0xa5; 2], "invalid input writes no prefix");

        assert_eq!(decode_into("", &mut []), Ok(()), "empty input decodes");
        assert_eq!(
            decode_into("a", &mut []),
            Err(DecodeError::OddLength { len: 1, at: 1 }),
            "odd input is rejected before destination validation"
        );
        let mut uppercase = [0; 2];
        assert_eq!(decode_into("Af09", &mut uppercase), Ok(()));
        assert_eq!(uppercase, [0xaf, 0x09], "uppercase input is preserved");
    }

    #[test]
    fn roundtrip_holds_for_every_single_byte() -> Result<(), DecodeError> {
        for byte_value in 0u8..=255 {
            let encoded = encode([byte_value]);
            let decoded = decode(&encoded)?;
            assert_eq!(decoded, vec![byte_value]);
        }
        Ok(())
    }

    #[test]
    fn matches_the_braid_cid_encoding() -> Result<(), DecodeError> {
        let sample = [
            0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54,
            0x32, 0x10, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x11, 0x22, 0x33, 0x44,
            0x55, 0x66, 0x77, 0x88,
        ];
        let expected: String = sample
            .iter()
            .map(|byte_value| format!("{byte_value:02x}"))
            .collect();
        assert_eq!(encode(sample), expected);
        assert_eq!(decode(&expected)?, sample);
        Ok(())
    }
}
