//! `hex` owns lowercase base-16 transcoding and enforces INV-HEX-ROUNDTRIP:
//! `decode(encode(b)) == b` for every byte slice, and `decode` refuses any input
//! that is not an even-length run of ASCII hex digits.

use std::error::Error;
use std::fmt;

// ── Encoding ────────────────────────────────────────────────────────────────

const LOWER: &[u8; 16] = b"0123456789abcdef";

/// Encodes bytes as lowercase hex, two characters per input byte.
pub fn encode(bytes: impl AsRef<[u8]>) -> String {
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(LOWER[(byte >> 4) as usize] as char);
        out.push(LOWER[(byte & 0x0f) as usize] as char);
    }
    out
}

// ── Decoding ────────────────────────────────────────────────────────────────

/// Why a hex string could not be decoded. Variant names are stable and
/// machine-readable; callers match on them rather than on message text.
#[derive(Debug, Clone, PartialEq, Eq)]
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
        match self {
            Self::OddLength { len, at: _ } => {
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

fn check_even_length(len: usize) -> Result<(), DecodeError> {
    if !len.is_multiple_of(2) {
        Err(DecodeError::OddLength { len, at: len })
    } else {
        Ok(())
    }
}

fn decode_pair(pair: &[u8], offset: usize) -> Result<u8, DecodeError> {
    let hi = nibble(pair[0]).ok_or(DecodeError::NotHexDigit {
        at: offset,
        byte: pair[0],
    })?;
    let lo = nibble(pair[1]).ok_or(DecodeError::NotHexDigit {
        at: offset + 1,
        byte: pair[1],
    })?;
    Ok((hi << 4) | lo)
}

/// Decodes a hex string into bytes. Uppercase digits are accepted, matching the
/// upstream `hex` crate this module retires.
pub fn decode(input: impl AsRef<[u8]>) -> Result<Vec<u8>, DecodeError> {
    let input = input.as_ref();
    check_even_length(input.len())?;
    let mut out = Vec::with_capacity(input.len() / 2);
    for (pair_index, pair) in input.chunks(2).enumerate() {
        let offset = pair_index * 2;
        let decoded_byte = decode_pair(pair, offset)?;
        out.push(decoded_byte);
    }
    Ok(out)
}

fn nibble(ascii_byte: u8) -> Option<u8> {
    match ascii_byte {
        b'0'..=b'9' => Some(ascii_byte - b'0'),
        b'a'..=b'f' => Some(ascii_byte - b'a' + 10),
        b'A'..=b'F' => Some(ascii_byte - b'A' + 10),
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

    #[test]
    fn decode_accepts_uppercase_digits() {
        assert_eq!(decode("48454C4C4F").unwrap(), b"HELLO");
        assert_eq!(decode("48454c4c6f").unwrap(), b"HELLo");
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
    fn roundtrip_holds_for_every_single_byte() {
        for b in 0u8..=255 {
            let encoded = encode([b]);
            let decoded = decode(&encoded).expect("valid hex");
            assert_eq!(decoded, vec![b]);
        }
    }

    #[test]
    fn matches_the_braid_cid_encoding() {
        let sample = [
            0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54,
            0x32, 0x10, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x11, 0x22, 0x33, 0x44,
            0x55, 0x66, 0x77, 0x88,
        ];
        let expected: String = sample.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(encode(sample), expected);
        assert_eq!(decode(&expected).unwrap(), sample);
    }
}
