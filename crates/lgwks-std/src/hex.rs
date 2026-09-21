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
    let hi = nibble(pair[0]).ok_or(DecodeError::NotHexDigit {
        at: offset,
        byte: pair[0],
    })?;
    let lo = nibble(pair[1]).ok_or(DecodeError::NotHexDigit {
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
    // `checked_div` is used only because the estate forbids the `/` operator on
    // integers; the `None` arm (a zero divisor) is unreachable for a constant 2.
    let pair_count = input.len().checked_div(2).unwrap_or(0);
    let mut out = Vec::with_capacity(pair_count);
    for (pair_index, pair) in input.chunks(2).enumerate() {
        // Both factors are bounded by the input length, which is at most
        // `isize::MAX`, so the product cannot overflow; saturating keeps the
        // offset arithmetic total if a caller ever presents a length that large.
        let offset = pair_index.saturating_mul(2);
        let decoded_byte = decode_pair(pair, offset)?;
        out.push(decoded_byte);
    }
    Ok(out)
}

/// Maps one ASCII character to the nibble it denotes, in `0..=0x0f`.
///
/// Accepts `0-9`, `a-f` and `A-F`; every other byte (including the non-ASCII
/// range) returns `None` so the caller can report the exact offset and byte.
/// The three subtractions are guarded by their own match arm, so each is a
/// saturating subtraction from a value known to be at or above its base.
fn nibble(ascii_byte: u8) -> Option<u8> {
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
