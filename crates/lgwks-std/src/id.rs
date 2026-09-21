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
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Builds an identifier from raw bytes without imposing version bits. Use
    /// this only to rehydrate a value that was generated elsewhere.
    #[must_use]
    pub fn from_bytes(raw: [u8; 16]) -> Self {
        Self(raw)
    }

    /// The RFC 4122 version nibble, or `None` for a value that carries no
    /// recognisable variant.
    #[must_use]
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

/// Refuses any text that is not exactly the 36 bytes of the canonical form.
///
/// Both `len` and `at` carry the observed length: there is no meaningful
/// interior offset for a length violation, and reporting the length twice keeps
/// the error's two fields self-describing for a machine consumer.
fn check_uuid_length(len: usize) -> Result<(), ParseError> {
    if len != 36 {
        Err(ParseError::WrongLength { len, at: len })
    } else {
        Ok(())
    }
}

/// Requires the group separator at `cursor`.
///
/// `bytes.get` is used deliberately: a truncated input reports
/// [`ParseError::MissingHyphen`] at the exact offset instead of indexing past
/// the end.
fn check_hyphen(bytes: &[u8], cursor: usize) -> Result<(), ParseError> {
    if bytes.get(cursor) != Some(&b'-') {
        Err(ParseError::MissingHyphen { at: cursor })
    } else {
        Ok(())
    }
}

/// Decodes one hyphen-delimited hex group into `out`.
///
/// `bytes` must hold at least `cursor + width` bytes: the caller walks the
/// fixed 8-4-4-4-12 layout of an input that `check_uuid_length` already proved
/// to be 36 bytes, so the slice is in range. `out` is the destination for this
/// group's bytes and must be exactly `width / 2` bytes wide; both offsets are
/// inside the same input, so their sum cannot saturate.
fn parse_uuid_group(
    bytes: &[u8],
    cursor: usize,
    width: usize,
    out: &mut [u8],
) -> Result<(), ParseError> {
    let group = &bytes[cursor..cursor.saturating_add(width)];
    let decoded = crate::hex::decode(group).map_err(|err| match err {
        DecodeError::NotHexDigit { at, .. } => ParseError::NotHexDigit {
            at: cursor.saturating_add(at),
        },
        DecodeError::OddLength { at, .. } => ParseError::NotHexDigit {
            at: cursor.saturating_add(at),
        },
    })?;
    out[..decoded.len()].copy_from_slice(&decoded);
    Ok(())
}

/// Decodes the five canonical groups of the hyphenated form into `raw`.
///
/// `GROUPS` pairs each group's hex-character width with the number of bytes it
/// decodes to (8-4-4-4-12 characters become 4-2-2-2-6 bytes), so no width is
/// ever divided at run time. A hyphen is required before every group after the
/// first. The cursors stay inside a 36-byte input and the offsets stay inside
/// the 16-byte output, so none of the accumulations can saturate.
fn parse_uuid_groups(bytes: &[u8], raw: &mut [u8; 16]) -> Result<(), ParseError> {
    const GROUPS: [(usize, usize); 5] = [(8, 4), (4, 2), (4, 2), (4, 2), (12, 6)];
    let mut out_offset = 0usize;
    let mut cursor = 0usize;
    for (group_index, &(width, byte_count)) in GROUPS.iter().enumerate() {
        if group_index > 0 {
            check_hyphen(bytes, cursor)?;
            cursor = cursor.saturating_add(1);
        }
        parse_uuid_group(
            bytes,
            cursor,
            width,
            &mut raw[out_offset..out_offset.saturating_add(byte_count)],
        )?;
        out_offset = out_offset.saturating_add(byte_count);
        cursor = cursor.saturating_add(width);
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
///
/// Variants are stable and machine-readable; callers match on them rather than
/// on the message text, and `#[non_exhaustive]` keeps a future rejection reason
/// an additive change.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
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
        match *self {
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

    // These tests return `Result` rather than unwrapping: an entropy or parse
    // refusal reports its own `Debug` on failure, which is the same report
    // `.unwrap` would have panicked with, without an `unwrap` in the tree.
    #[test]
    fn generated_value_carries_version_four_and_the_rfc_variant()
    -> Result<(), Box<dyn std::error::Error>> {
        let id = Uuid::new_v4()?;
        assert_eq!(id.version(), Some(4));
        assert_eq!(id.as_bytes()[6] >> 4, 4);
        assert_eq!(id.as_bytes()[8] & 0xc0, 0x80);
        Ok(())
    }

    #[test]
    fn successive_identifiers_differ() -> Result<(), Box<dyn std::error::Error>> {
        let first = Uuid::new_v4()?;
        let second = Uuid::new_v4()?;
        assert_ne!(first, second);
        Ok(())
    }

    #[test]
    fn display_is_hyphenated_lowercase_in_eight_four_four_four_twelve()
    -> Result<(), Box<dyn std::error::Error>> {
        let id = Uuid::new_v4()?;
        let text = id.to_string();
        assert_eq!(text.len(), 36);
        assert_eq!(&text[8..9], "-");
        assert_eq!(&text[13..14], "-");
        assert_eq!(&text[18..19], "-");
        assert_eq!(&text[23..24], "-");
        assert!(
            text.chars()
                .all(|char_value| char_value.is_ascii_hexdigit() || char_value == '-')
        );
        Ok(())
    }

    #[test]
    fn parse_reverses_display() -> Result<(), Box<dyn std::error::Error>> {
        let id = Uuid::new_v4()?;
        let parsed = Uuid::parse(&id.to_string())?;
        assert_eq!(id, parsed);
        Ok(())
    }

    #[test]
    fn parse_accepts_uppercase() -> Result<(), Box<dyn std::error::Error>> {
        let id = Uuid::new_v4()?;
        let upper = id.to_string().to_ascii_uppercase();
        let parsed = Uuid::parse(&upper)?;
        assert_eq!(id, parsed);
        Ok(())
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
