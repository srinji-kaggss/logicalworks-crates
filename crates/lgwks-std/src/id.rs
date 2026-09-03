//! `id` owns UUID version 4 generation and parsing, and enforces
//! INV-UUID-RFC4122: the version nibble is always `4` and the variant bits are
//! always `10`, so a value that this module produces is indistinguishable from
//! one the `uuid` crate produces.

use std::error::Error;
use std::fmt;

use crate::hex::DecodeError;
use crate::random::{self, EntropyError};

/// A 128-bit RFC 4122 identifier, stored as raw bytes in network order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Uuid([u8; 16]);

impl Uuid {
    /// Generates a version 4 UUID from OS entropy.
    pub fn new_v4() -> Result<Self, EntropyError> {
        let mut raw: [u8; 16] = random::bytes()?;
        raw[6] = (raw[6] & 0x0f) | 0x40;
        raw[8] = (raw[8] & 0x3f) | 0x80;
        Ok(Self(raw))
    }

    /// The identifier's raw bytes in network order.
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Builds an identifier from raw bytes without imposing version bits. Use
    /// this only to rehydrate a value that was generated elsewhere.
    pub fn from_bytes(raw: [u8; 16]) -> Self {
        Self(raw)
    }

    /// The RFC 4122 version nibble, or `None` for a value that carries no
    /// recognisable variant.
    pub fn version(&self) -> Option<u8> {
        if self.0[8] & 0xc0 == 0x80 {
            Some(self.0[6] >> 4)
        } else {
            None
        }
    }

    /// Parses the canonical hyphenated 8-4-4-4-12 lowercase or uppercase form.
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        let bytes = text.as_bytes();
        check_uuid_length(bytes.len())?;
        let mut raw = [0u8; 16];
        parse_uuid_groups(bytes, &mut raw)?;
        Ok(Self(raw))
    }
}

fn check_uuid_length(len: usize) -> Result<(), ParseError> {
    if len != 36 {
        Err(ParseError::WrongLength { len, at: len })
    } else {
        Ok(())
    }
}

fn check_hyphen(bytes: &[u8], cursor: usize) -> Result<(), ParseError> {
    if bytes.get(cursor) != Some(&b'-') {
        Err(ParseError::MissingHyphen { at: cursor })
    } else {
        Ok(())
    }
}

fn parse_uuid_group(
    bytes: &[u8],
    cursor: usize,
    width: usize,
    out: &mut [u8],
) -> Result<(), ParseError> {
    let group = &bytes[cursor..cursor + width];
    let decoded = crate::hex::decode(group).map_err(|err| match err {
        DecodeError::NotHexDigit { at, .. } => ParseError::NotHexDigit { at: cursor + at },
        DecodeError::OddLength { at, .. } => ParseError::NotHexDigit { at: cursor + at },
    })?;
    out[..decoded.len()].copy_from_slice(&decoded);
    Ok(())
}

fn parse_uuid_groups(bytes: &[u8], raw: &mut [u8; 16]) -> Result<(), ParseError> {
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    let mut out_offset = 0usize;
    let mut cursor = 0usize;
    for (group_index, &width) in GROUPS.iter().enumerate() {
        if group_index > 0 {
            check_hyphen(bytes, cursor)?;
            cursor += 1;
        }
        let byte_count = width / 2;
        parse_uuid_group(
            bytes,
            cursor,
            width,
            &mut raw[out_offset..out_offset + byte_count],
        )?;
        out_offset += byte_count;
        cursor += width;
    }
    Ok(())
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex_str = crate::hex::encode(self.0);
        write!(
            f,
            "{}-{}-{}-{}-{}",
            &hex_str[0..8],
            &hex_str[8..12],
            &hex_str[12..16],
            &hex_str[16..20],
            &hex_str[20..32]
        )
    }
}

/// Why a string is not a canonical UUID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The canonical form is exactly 36 characters.
    WrongLength {
        /// Length of the offending input, in bytes.
        len: usize,
        /// Offset where error occurred.
        at: usize,
    },
    /// A group separator was expected at this offset.
    MissingHyphen {
        /// Zero-based offset where a `-` was required.
        at: usize,
    },
    /// A group contained a character outside `[0-9a-fA-F]`.
    NotHexDigit {
        /// Zero-based offset of the group that failed to decode.
        at: usize,
    },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongLength { len, at: _ } => {
                write!(f, "UUID text has length {len}; expected exactly 36 bytes")
            }
            Self::MissingHyphen { at } => {
                write!(f, "expected '-' at offset {at}")
            }
            Self::NotHexDigit { at } => {
                write!(f, "non-hex character in UUID group starting at offset {at}")
            }
        }
    }
}

impl Error for ParseError {}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_value_carries_version_four_and_the_rfc_variant() {
        let id = Uuid::new_v4().expect("entropy available");
        assert_eq!(id.version(), Some(4));
        assert_eq!(id.as_bytes()[6] >> 4, 4);
        assert_eq!(id.as_bytes()[8] & 0xc0, 0x80);
    }

    #[test]
    fn successive_identifiers_differ() {
        let a = Uuid::new_v4().unwrap();
        let b = Uuid::new_v4().unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn display_is_hyphenated_lowercase_in_eight_four_four_four_twelve() {
        let id = Uuid::new_v4().unwrap();
        let s = id.to_string();
        assert_eq!(s.len(), 36);
        assert_eq!(&s[8..9], "-");
        assert_eq!(&s[13..14], "-");
        assert_eq!(&s[18..19], "-");
        assert_eq!(&s[23..24], "-");
        assert!(s.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
    }

    #[test]
    fn parse_reverses_display() {
        let id = Uuid::new_v4().unwrap();
        let parsed = Uuid::parse(&id.to_string()).expect("valid UUID");
        assert_eq!(id, parsed);
    }

    #[test]
    fn parse_accepts_uppercase() {
        let id = Uuid::new_v4().unwrap();
        let upper = id.to_string().to_ascii_uppercase();
        let parsed = Uuid::parse(&upper).expect("uppercase accepted");
        assert_eq!(id, parsed);
    }

    #[test]
    fn parse_refuses_the_wrong_length() {
        assert_eq!(
            Uuid::parse(""),
            Err(ParseError::WrongLength { len: 0, at: 0 })
        );
        assert_eq!(
            Uuid::parse("not-a-uuid"),
            Err(ParseError::WrongLength { len: 10, at: 10 })
        );
    }

    #[test]
    fn parse_refuses_a_missing_hyphen() {
        assert_eq!(
            Uuid::parse("12345678x1234-1234-1234-123456789abc"),
            Err(ParseError::MissingHyphen { at: 8 })
        );
    }

    #[test]
    fn parse_refuses_a_non_hex_group() {
        assert_eq!(
            Uuid::parse("12345678-123z-1234-1234-123456789abc"),
            Err(ParseError::NotHexDigit { at: 12 })
        );
    }
}
